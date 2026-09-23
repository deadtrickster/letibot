//! The loop: frames in, screen out, ack after the screen is out.
//!
//! Small, because the order of three operations is the whole of it and burying
//! that order in a binary would make it unreviewable:
//!
//! ```text
//!   1. drain every frame that has arrived, classifying each
//!   2. draw
//!   3. ack `batch.last_seq()` with (rendered, filtered)
//! ```
//!
//! Step 3 comes after step 2, always. §13.2b: *"a crash then costs a duplicate,
//! never a silence"*. And the seq acked is the last one **read** in step 1, not the
//! last one drawn — a head at `Terse` renders almost nothing and must still
//! advance, or it rereads its own output forever.
//!
//! # The socket is not the session
//!
//! A daemon goes away for ordinary reasons — it is restarted, `--stop` lands, the box
//! has a moment — and a head whose loop treats that as the end of the program takes
//! the operator's view of a conversation that is still on disk with it. So the
//! connection lives in [`Link`], which owns the socket, the channel the frames arrive
//! on and the thread reading it, and whose failure mode is to **mark the link down**
//! rather than to return an error:
//!
//! ```text
//!   a write fails        -> Link::tick tells the head; the head draws the line
//!   the pump goes away   -> same, from Disconnected
//!   Link::reconnect      -> a new socket, ATTACH with the seq we had
//! ```
//!
//! The `since_seq` is the whole of the recovery: the daemon answers a resume with the
//! gap as events, or with a `Resync` when the gap is larger than it still holds. So
//! getting the socket back *is* getting the conversation back, out of the protocol
//! that was already there.

use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread::JoinHandle;

use letibot_sessionlog::client::{ClientError, HeadClient, Inbound, pump};
use letibot_sessionlog::protocol::{Ack, Caps, ServerFrame};
use letibot_sessionlog::wire::FrameReader;

use crate::app::{Action, App, Disposition, Key};

/// Where the terminal's caret belongs: `(row, column)`, zero-based, or nowhere.
pub type Caret = Option<(usize, usize)>;

/// What puts a frame on a screen. A closure, so the same loop serves a terminal
/// and a test that renders to a `Vec<String>`.
pub type Draw<'a> = dyn FnMut(&[String], Caret) + 'a;

/// **A connection that can die and come back.**
///
/// Owns the three things that make one: the client (its writer), the channel the
/// reader thread pushes frames into, and the thread itself. Keeping them together is
/// what makes the recovery a two-line act for the caller — `reconnect` drops the old
/// socket, connects, sends `ATTACH`, and starts a new reader — instead of four pieces
/// of state the binary has to swap in the right order.
pub struct Link {
    client: HeadClient,
    rx: Receiver<Inbound>,
    /// `None` only while [`Link::close`] is running. A test never sees it, and keeping
    /// it an `Option` is what lets `join` take the handle out of `self`.
    reader: Option<JoinHandle<()>>,
    /// The session this connection is for, kept so `reconnect` does not need the caller
    /// to remember it — a head that lost the name of the session it was in has lost
    /// more than its socket.
    session: String,
    kind: String,
    identity: String,
    /// **The socket this connection is on** (R30). Kept so a head that has asked the
    /// daemon to stop can see the file go: the daemon's `shutdown` unlinks it after it has
    /// joined its accept loop, which is the same shape as the wrapper's *"the record is
    /// removed only after the process is gone"*.
    socket_path: std::path::PathBuf,
    /// **The process at the other end of this socket**, from `SO_PEERCRED` — the daemon
    /// itself, not a number read out of a file that may be stale or belong to an older
    /// daemon with the same workspace. `None` when the kernel would not say.
    daemon_pid: Option<i32>,
}

/// **Which process is at the other end of this socket.**
///
/// `SO_PEERCRED`, which the kernel fills in from the `connect` — so this is the daemon
/// this head is actually attached to, and not a pid in a file that may have been written
/// by a predecessor. It asks the kernel a question; it sends no signal, and R30's *"not a
/// licence to kill by pid from a head"* is about the signal, not about knowing.
///
/// `None` on any failure at all, and the farewell says *pid unknown* rather than filling
/// in a zero: a head that printed a number it did not have would send the operator to `ps`
/// for a process that is not there.
fn peer_pid(s: &UnixStream) -> Option<i32> {
    use std::os::unix::io::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0 && cred.pid > 0).then_some(cred.pid)
}

/// **Is that process still there?**
///
/// `/proc/<pid>`, the same test the wrapper makes — *"for _ in 1 2 3 4 5 …; do [ -d
/// "/proc/$p" ] || break"* — and for the same reason: *"stopped" is said AFTER the
/// process is gone, not after the signal is sent.*
///
/// `None` when the answer cannot be had: a `/proc` that does not exist, or a pid
/// directory this user may not stat. `Some(false)` is *it is gone* and is the only answer
/// that lets the head leave early, so an unknown is treated as *still here* and the
/// deadline decides — the direction to be wrong in is the one that keeps looking.
fn process_alive(pid: i32) -> Option<bool> {
    match std::fs::metadata(format!("/proc/{pid}")) {
        Ok(_) => Some(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        // PermissionDenied means the directory IS there; anything else is a `/proc`
        // that cannot answer, which is not the same fact.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(true),
        Err(_) => None,
    }
}

impl Link {
    /// Connect and send the `ATTACH`. The `Hello` arrives on [`Link::frames`] like any
    /// other frame — which is also what makes a reconnect look like the first attach.
    pub fn open(
        path: impl AsRef<std::path::Path>,
        session: &str,
        since_seq: u64,
        kind: &str,
        identity: &str,
    ) -> Result<Link, ClientError> {
        let socket_path = path.as_ref().to_path_buf();
        let (client, reader) =
            HeadClient::start_attach(path, session, since_seq, kind, identity, Caps::default())?;
        Ok(Link::spawn(
            client,
            reader,
            session.to_string(),
            kind.to_string(),
            identity.to_string(),
            socket_path,
        ))
    }

    fn spawn(
        mut client: HeadClient,
        reader: FrameReader<UnixStream>,
        session: String,
        kind: String,
        identity: String,
        socket_path: std::path::PathBuf,
    ) -> Link {
        let (tx, rx) = std::sync::mpsc::channel();
        // **Read once, at connect.** `SO_PEERCRED` answers for the socket, so it cannot
        // go stale while the connection is up, and a reconnect is a new connection to
        // whatever daemon is there now — see `Link::reconnect`.
        let daemon_pid = peer_pid(client.socket());
        Link {
            client,
            rx,
            reader: Some(std::thread::spawn(move || pump(reader, tx))),
            session,
            kind,
            identity,
            socket_path,
            daemon_pid,
        }
    }

    /// **The process at the other end of this socket**, or `None`.
    ///
    /// Read once by the caller that opened the connection, which hands it to the head
    /// (`App::set_daemon_pid`) — that is where `/status` and the farewell read it from, so
    /// this is the seed and not a second home for the fact.
    pub fn daemon_pid(&self) -> Option<i32> {
        self.daemon_pid
    }

    /// The frames, in order, as the reader hands them over.
    pub fn frames(&self) -> &Receiver<Inbound> {
        &self.rx
    }

    /// Take the seat the daemon named in a `Hello` this caller read itself.
    pub fn seated_by(&mut self, hello: &ServerFrame) {
        self.client.seated_by(hello);
    }

    /// The session this connection is for.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// **Drop the dead socket and try for a new one, resuming where this head got to.**
    ///
    /// `since_seq` is passed in rather than taken from the link because the read mark
    /// belongs to the head that read the frames: it is the seq of the last event the
    /// head consumed, and the head is the only thing that knows it.
    ///
    /// The old connection goes first, deliberately: its reader thread is blocked in a
    /// `read` on a socket that may be in any state, and the only thing that ends it is
    /// its channel having no receiver. So this replaces the receiver (which is what the
    /// pump's `send` fails on) and joins the thread before a new socket exists — one
    /// reader at a time, and no chance of two threads racing to push frames from two
    /// sockets into one head.
    pub fn reconnect(
        &mut self,
        path: impl AsRef<std::path::Path>,
        since_seq: u64,
    ) -> Result<(), ClientError> {
        self.close();
        let (client, reader) = HeadClient::start_attach(
            path,
            &self.session,
            since_seq,
            &self.kind,
            &self.identity,
            Caps::default(),
        )?;
        let (session, kind, identity) = (
            self.session.clone(),
            self.kind.clone(),
            self.identity.clone(),
        );
        *self = Link::spawn(
            client,
            reader,
            session,
            kind,
            identity,
            self.socket_path.clone(),
        );
        Ok(())
    }

    /// **A `Bye` is final and a dropped socket is not**, and this is the caller's way of
    /// saying goodbye when *it* is leaving: send the `Detach` if the socket is still
    /// there, then stop the reader. Best effort throughout — a head on its way out has
    /// nothing to report a failure to.
    pub fn detach(&mut self) {
        let _ = self.client.detach();
        self.close();
    }

    /// Stop reading, and wait for the reader to actually stop.
    ///
    /// # Two things have to happen, and neither is enough alone
    ///
    /// **The channel** tells the pump not to bother: the receiver is replaced by a
    /// dropped one, so the next `send` fails and the loop returns. That is what ends a
    /// reader that is *awake* — one chewing through a burst of frames.
    ///
    /// **The socket** ends a reader that is *asleep*, which is the reader at rest: it is
    /// blocked in `read` on its own descriptor, and the only things that wake that are
    /// data, EOF, or a shutdown. A daemon that is merely idle sends none of them, so
    /// replacng the channel alone leaves the join waiting for a frame that may never
    /// come. Found by the end-to-end reconnect test, which deadlocked here — and it is
    /// not only a test's problem: a head that has to *wait* for its reader on a live
    /// connection has to hang up on it.
    fn close(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        drop(tx);
        drop(std::mem::replace(&mut self.rx, rx));
        let _ = self.client.shut_down();
        if let Some(t) = self.reader.take() {
            let _ = t.join();
        }
    }

    /// **One pass: drain, draw, ack — with every way the socket can fail turned into a
    /// fact the head draws rather than an error the head dies of.**
    ///
    /// It returns nothing, and that is the requirement stated as a type: there is no
    /// failure of the *connection* that is a failure of this *loop*. The three ways out
    /// were `client.ack(...)?` on the ack, `?` on each command, and the head's own
    /// `main` propagating either — measured: the process was gone and the session view
    /// with it, for a daemon that had merely restarted.
    pub fn tick(&mut self, app: &mut App, size: (usize, usize), keys: &[Key], draw: &mut Draw<'_>) {
        app.clock(now_ms());

        // 1. Drain. Nothing is sent in this phase.
        let mut rendered = 0u64;
        let mut filtered = 0u64;
        let mut last_seq = 0u64;
        loop {
            match self.rx.try_recv() {
                Ok(Inbound::Frame(frame)) => {
                    if let ServerFrame::Event(env) = &frame {
                        // The read mark, taken from what was *read*. There is no
                        // "last rendered seq" variable here, on purpose.
                        last_seq = env.seq;
                    }
                    match app.apply(frame) {
                        Disposition::Rendered => rendered += 1,
                        Disposition::Filtered => filtered += 1,
                        Disposition::Control => {}
                    }
                }
                // **A frame this head cannot read is said, counted, and stepped over.**
                // It carries no seq — nothing was parsed, so there is nothing to ack —
                // so it moves neither of the two counters the `Ack` carries and it does
                // not touch `last_seq`. What it does is reach the transcript and the
                // `unreadable` counter, so a daemon whose frames this head does not
                // understand is distinguishable from a quiet one. The connection is
                // still up; see `Inbound`.
                Ok(Inbound::Unreadable(u)) => {
                    app.unreadable(u);
                }
                Err(TryRecvError::Empty) => break,
                // **The reader is gone, and that is how a clean death is noticed.** A
                // daemon killed with a signal closes the socket, the pump's `read`
                // returns `Eof`, the thread ends and the sender is dropped — with
                // nothing failing to *write*, because at rest a head writes nothing at
                // all. Without this arm the loop would tick for ever over a dead
                // channel, drawing the last screen it was given, which is
                // indistinguishable from a quiet session.
                //
                // It is also the normal end of a `Bye`: the daemon writes one and
                // returns. `link_down` refuses to act on a head that is going away, so
                // that case still leaves as it should.
                Err(TryRecvError::Disconnected) => {
                    app.link_down("the daemon closed the connection");
                    break;
                }
            }
        }

        // A `Hello` in the drain above may have seated this connection as a different
        // head — that is what a session switch is — and a client still acking under the
        // old id would ack into a session it has left, which the hub ignores in silence.
        if let Some(id) = app.take_seated() {
            self.client.seated(&id);
        }

        // Actions a *frame* produced, not a key: the switch that follows a session
        // being created. Ahead of the key actions, because they are the answer to
        // something the operator already asked for.
        let mut actions = app.take_actions();
        for k in keys {
            // Cloned rather than copied: `Key::Paste` carries the paste, because the
            // point of bracketed paste is that a 3 KB stack trace is one key.
            if let Some(a) = app.key(k.clone()) {
                actions.push(a);
            }
        }

        // 2. Draw. Every frame is built; whether any of it reaches the terminal is
        //    `Terminal::draw`'s business, and for an unchanged frame the answer is no
        //    bytes at all.
        let screen = app.screen(size.0, size.1);
        draw(&screen, app.cursor());

        // **Answer with what was actually drawn.** A tool asked what the operator is
        // looking at; this is the only place in the system that knows, because it is
        // the place that put the bytes on the terminal — at this head's real size,
        // with its scroll position, its theme and its folds. A failure here is the
        // socket, so it is reported the same way every other write is.
        for req_id in app.take_screen_requests() {
            if let Err(e) = self.client.screen(&req_id, size.0, size.1, screen.clone()) {
                app.link_down(&e.to_string());
            }
        }

        // 3. Ack — after the frame is out, and only when a frame was read. An idle
        //    tick has no seq and must not invent one.
        if last_seq > 0 && !app.detached() {
            if let Err(e) = self.client.ack(Ack {
                seq: last_seq,
                rendered,
                filtered,
            }) {
                app.link_down(&e.to_string());
            }
        }

        for a in actions {
            // **Nothing leaves a head whose link is down**, and it says so once rather
            // than failing once per action. Every write below would fail the same way,
            // and a batch of them would bury the one sentence that matters.
            if app.detached() {
                app.refused_while_detached();
                break;
            }
            // The whole match is one `Result` so that the *only* thing a failed write
            // does is tell the head. Each arm used to end in `?`, which propagated out
            // of this loop and out of `main`.
            let sent = (|| -> Result<(), ClientError> {
                match a {
                    // `expected_seq` is what this head was looking at when the operator
                    // acted. The daemon decides what that means per command; the head's
                    // job is to report it honestly.
                    Action::Prompt(text) => {
                        self.client.prompt(app.seq, &text)?;
                    }
                    Action::WithdrawPrompts => {
                        self.client.withdraw_prompts(app.seq)?;
                    }
                    Action::Interrupt(reason) => {
                        self.client.interrupt(app.seq, &reason)?;
                    }
                    Action::Promote => {
                        self.client.promote(app.seq)?;
                    }
                    Action::Answer {
                        req_id,
                        option_id,
                        pattern,
                        note,
                    } => {
                        self.client.answer_with(
                            &req_id,
                            &option_id,
                            pattern.as_deref(),
                            note.as_deref(),
                        )?;
                    }
                    // **The frame this head had no way to send** (§1.7). `Answer`
                    // above grants a permission; this answers a question, and the
                    // client method has existed since protocol 5 with no caller.
                    Action::AnswerQuestion { req_id, answer } => {
                        self.client.answer_question(&req_id, answer)?;
                    }
                    Action::Resync => {
                        self.client.request_resync()?;
                    }
                    Action::ListSessions => {
                        self.client.list_sessions()?;
                    }
                    Action::ListJobs => {
                        self.client.list_jobs()?;
                    }
                    Action::ListTodos => {
                        self.client.list_todos()?;
                    }
                    Action::NewSession(title) => {
                        // The head's own working directory, read here rather than carried
                        // through `App`: the app is the same object under `--replay`, where
                        // there is no daemon and no session to make.
                        let cwd = std::env::current_dir()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        self.client.new_session(&title, &cwd)?;
                    }
                    // The answer is a second `Hello`, which arrives on the pump and goes
                    // through `App::apply` exactly like the first one. Nothing is torn down
                    // here: the switch happens inside the daemon, on this same socket, so
                    // there is no window in which this head is attached to nothing.
                    Action::Switch(id) => {
                        self.client.switch(&id, 0)?;
                    }
                    Action::Settings => {
                        self.client.settings()?;
                    }
                    // A read, not a move: the answer arrives as a `Peeked` frame on the
                    // pump and the output pane is built from it. Nothing here changes
                    // which session this connection is in, and nothing is read until the
                    // operator asked — the laziness is the point.
                    Action::Peek(id) => {
                        self.client.peek(&id)?;
                    }
                    // The same shape as `Peek`: a read that comes back as an event on the
                    // log rather than a frame on this connection, and the pane is built from
                    // it. The `client_request_id` is dropped here for the same reason the
                    // peek's is: the answer is addressed to the session, not to the ask.
                    Action::ReadJobOutput { job, offset } => {
                        self.client.read_job_output(&job, offset)?;
                    }
                    // Same shape as `NewSession`: the daemon answers with `Sessions` naming
                    // it as `created`, and `App::apply` turns that into the `Switch`. One
                    // path for "go to a session that was not here a moment ago", whether it
                    // was minted or restored.
                    Action::ResumeSession(id) => {
                        self.client.resume_session(&id)?;
                    }
                    Action::Rename { session_id, title } => {
                        self.client.rename_session(&session_id, &title)?;
                    }
                    // The compaction itself is disclosed on the session's own log: the
                    // summary turn streams like any turn, and the forked transcript's
                    // first item says what replaced the history. Nothing to apply here.
                    Action::Compact => {
                        self.client.compact(app.seq)?;
                    }
                    Action::Reseat { summarise } => {
                        self.client.reseat(app.seq, summarise)?;
                    }
                    Action::Mode { name, consented } => {
                        self.client.set_mode(app.seq, &name, consented)?;
                    }
                    Action::Slash { line } => {
                        self.client.slash(app.seq, &line)?;
                    }
                    // **The operator's own call through the door** (R34, R31).
                    //
                    // One frame, not two: `execute: true` means the daemon runs it and
                    // appends the row, so there is no result for this head to hand back.
                    // The admission is the same one a head-run call has always had, and
                    // the permission to go is still `OperatorCallAllowed` — a head that
                    // assumed its own request was admitted would be the R24 part two
                    // defect with a new caller.
                    Action::HeadRun { name, arguments } => {
                        let n = app.next_head_run();
                        let call_id = format!("{}-{n}", app.head_id());
                        let _ = self
                            .client
                            .operator_call(app.seq, &call_id, &name, &arguments, true)?;
                    }
                    Action::Secret { req_id, secret } => {
                        self.client.secret(&req_id, secret)?;
                    }
                    // Leaving on purpose: a detach that fails is the socket that was
                    // already gone, which the loop is about to notice anyway. Not sent
                    // while a stop is in flight — see `App::wants_detach`.
                    Action::Quit => {
                        if app.wants_detach() {
                            let _ = self.client.detach();
                        }
                    }
                    // **Ask, then STAY until you know.** R30, and the whole of it.
                    //
                    // The daemon announces the stop to every other head before it goes,
                    // so the request has to reach it while this head is still attached —
                    // a detach first would close the socket the notice travels on. That
                    // was already right. What was wrong is the next two lines: the old
                    // arm discarded both results and returned, so *whether the frame
                    // reached the socket was a race against this head's own shutdown*,
                    // and on 2026-09-23 the operator's head was gone while `harnessd` sat
                    // at `PPID 1` still holding the socket: nothing had asked it, or
                    // nothing had checked. **A request is not an outcome.**
                    //
                    // So the write's result is *kept* rather than discarded, and the head
                    // then waits — `watch_stop` below, once a tick, with the screen live —
                    // until the daemon's process is gone or the deadline passes. The
                    // identity is the client's own, the name it attached under, rather
                    // than anything the head could make up.
                    Action::StopDaemon => {
                        let who = self.client.identity().to_string();
                        // **The pid comes off the HEAD, not off this link.** One fact, one
                        // holder: `main` seeds `App::daemon_pid` from `SO_PEERCRED` when it
                        // opens the socket (and again on a reconnect, where the answer can
                        // be a different process), and the head is what `/status` reads and
                        // what the farewell names. A second copy here is how the two come
                        // to disagree about which process the operator is being sent to
                        // `ps` for — found by a test that injects a pid the link never saw.
                        let pid = app.daemon_pid();
                        let sent = self.client.stop(app.seq, &who);
                        // A failed write is not a failed stop — the socket may already be
                        // gone because the daemon is going — so it is recorded as *not
                        // sent* and the wait runs anyway. What it changes is the sentence.
                        app.stop_began(&who, sent.is_ok(), pid, now_ms());
                        // **And this head does not detach.** The connection is what the
                        // wait is reading: `detach()` closes the reader, and the daemon's
                        // socket closing is one of the facts being watched for.
                    }
                }
                Ok(())
            })();
            if let Err(e) = sent {
                // The write failed with the socket still open as far as this head knew:
                // the daemon is gone. Said, and the loop carries on to draw the line.
                app.link_down(&e.to_string());
            }
        }
        // **Last, every tick, and after the drawing.** R30. Nothing here sends anything:
        // it reads the four things that answer the question the operator asked.
        self.watch_stop(app);
    }

    /// **Has the daemon this head asked to stop actually gone?**
    ///
    /// Called once a tick from [`Link::tick`], and it is a *watcher* rather than a wait:
    /// the loop keeps drawing frames and taking keys while it runs, which is the
    /// requirement's second part — *a deadline, because a daemon mid-turn may legitimately
    /// take time, and the head SAYS what it is waiting for rather than freezing on a dead
    /// screen.* A `recv_timeout` loop here would be the freeze.
    ///
    /// Four observations, and they are kept apart because each is a different answer:
    ///
    /// * **the ack** — set in `App::apply` when the daemon's `Accepted` arrives, because
    ///   that is where frames are read. It is the only evidence the request was *read*;
    /// * **the socket file** — the daemon unlinks it in `shutdown`, after joining its
    ///   accept loop;
    /// * **the process** — `/proc/<pid>`, the wrapper's own test and the only one that is
    ///   literally *the daemon has gone*;
    /// * **the deadline** — so a head with no other answer still stops waiting.
    ///
    /// The exit condition is the process, or the deadline. The socket and the ack are
    /// evidence for the sentence, not gates: a daemon can unlink its socket and keep
    /// running (a wedged worker), and it can be asked by a head whose write was never
    /// flushed — which is the whole defect — so waiting on the ack alone would hang the
    /// head for the full deadline every time the daemon ignored it.
    pub fn watch_stop(&mut self, app: &mut App) {
        let now = now_ms();
        // **Read the state out before touching it.** `App::stopping` borrows the app, and
        // every write below is also on the app — a preview in one borrow and a write in
        // the next is what keeps this one function rather than three passes over `self`.
        let Some((closed, gone, pid, deadline)) = app
            .stopping()
            .map(|s| (s.closed, s.gone, s.pid, s.deadline_ms))
        else {
            return;
        };
        if gone {
            return;
        }
        let mut next_closed = closed;
        let mut next_gone = gone;
        // The socket file. `exists` on a unix socket is a `stat` on the directory entry,
        // which is exactly the fact `shutdown`'s `remove_file` publishes.
        if !closed && !self.socket_path.exists() {
            next_closed = true;
        }
        // The process, and only while the pid is known. `Some(false)` is *gone* and is the
        // only answer that ends the wait early; an unanswerable `/proc` leaves the deadline
        // to decide, which is the direction to be wrong in — a head that left on an unknown
        // would be the defect again with better manners.
        if let Some(p) = pid
            && process_alive(p) == Some(false)
        {
            next_gone = true;
        }
        if next_closed != closed || next_gone != gone {
            if let Some(s) = app.stopping_mut() {
                s.closed = next_closed;
                s.gone = next_gone;
            }
            app.mark_redraw();
        }
        // **The deadline crossing is a frame too.** The waiting line stops being drawn the
        // moment the question is answered, and without this the head would sit on the last
        // frame it drew for up to a tick with a sentence saying it was still waiting.
        if next_gone || now >= deadline {
            app.mark_redraw();
        }
    }

}

impl Drop for Link {
    /// Whatever the caller did, no reader thread outlives the connection it was reading:
    /// replacing or dropping the receiver is what ends it, and a head that exited with a
    /// thread still blocked in `read` on a socket nobody will close is a leak that shows
    /// up as a process that will not die.
    fn drop(&mut self) {
        if self.reader.is_some() {
            self.close();
        }
    }
}

/// Wall clock in milliseconds. The head's own, not the daemon's: it is used only
/// to notice that the daemon has gone quiet, and a clock taken from the party that
/// has stopped talking cannot notice that.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
