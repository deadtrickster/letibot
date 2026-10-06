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

/// **Where the process this head is watching came from** — R30, and the two cases are not
/// the same test.
///
/// The operator, twice: *"they always tell me daemon not stopped after waiting for 5 sec,
/// then `letibot --stop` tells nothing runs."* Both true, and the daemon really had stopped —
/// **the check was wrong, and the relationship is why.**
///
/// The daemon is a **child of the head**. A child that exits is not reaped by init: it becomes
/// a **zombie**, and a zombie keeps its `/proc/<pid>` entry until its parent waits for it. So
/// `process_alive` — which was `fs::metadata("/proc/{pid}")` — answered *alive* about a
/// process that had already exited, the head sat out the whole deadline, printed `NOT
/// stopped`, and exited; init reaped the zombie at that moment, which is why the operator's
/// next command correctly said nothing was running.
///
/// **The docstring named the trap and did not see it.** It said this was *"the same test the
/// wrapper makes"* — and it is, and that is the defect. In the wrapper the daemon is not its
/// child, so a dead daemon is reaped at once and `/proc` vanishes. In the head it is,
/// so `/proc` persists until the head reaps it — and the head is the process sitting in the
/// loop not reaping. **Same syscall, different meaning, because the relationship differs**,
/// and a test copied with its reason intact can still be wrong in the new place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parentage {
    /// **A direct child of this head.** The head owes it a wait, and until that wait happens
    /// the child is a zombie that `/proc` still lists. A `waitpid` is the only test that
    /// answers correctly here.
    Ours,
    /// **Somebody else's process.** Its own parent or init reaps it, so a dead one is gone
    /// from `/proc` and the wrapper's test is the right one — which is what an attached head
    /// that did not spawn the daemon is looking at.
    NotOurs,
}

/// Which of the two it is, read from `/proc` rather than assumed from how this head was
/// launched: a head that inherited its daemon through an `exec` (which is how `~/bin/letibot`
/// produces one) is a parent without ever having called `spawn`, so *"did I start it"* is not
/// the question — *"is it mine"* is, and the kernel answers it.
fn parentage(pid: i32) -> Parentage {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => match stat_ppid(&stat) {
            Some(ppid) if ppid == std::process::id() as i32 => Parentage::Ours,
            _ => Parentage::NotOurs,
        },
        // Gone, or unreadable: not something this head is the parent of, and the `/proc`
        // test below is the honest one either way.
        Err(_) => Parentage::NotOurs,
    }
}

/// The `ppid` out of one `/proc/<pid>/stat` line.
///
/// **Field 4, found from the LAST `)`.** Field 2 is the executable's name in parentheses and
/// may itself contain spaces and brackets — `(my ) weird (program)` — so splitting on
/// whitespace from the left reads the wrong field for exactly the processes somebody would
/// bother to name that way. Everything after the last `)` is whitespace-separated fields
/// from field 3 on, which is what both this and [`stat_state`] rely on.
fn stat_ppid(stat: &str) -> Option<i32> {
    let after = &stat[stat.rfind(')')? + 1..];
    let mut fields = after.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok()
}

/// The state char out of one `/proc/<pid>/stat` line — `R` running, `S` sleeping, **`Z` a
/// zombie**, `X` dead.
fn stat_state(stat: &str) -> Option<char> {
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().next()?.chars().next()
}

/// **Has that process gone** — R30's one question, asked the way the relationship makes true.
///
/// `Some(true)` is *gone*, and it is the only answer that lets the head leave early.
/// `Some(false)` is *still there*. `None` is *cannot tell*, and the deadline decides — the
/// direction to be wrong in is the one that keeps looking.
///
/// **The four facts stay four.** This is what `gone` means: nothing here touches `sent`,
/// `acked` or `closed`, and the sentence the operator reads still names which of the four it
/// observed.
fn process_gone(pid: i32) -> Option<bool> {
    match parentage(pid) {
        Parentage::Ours => match reap_own_child(pid) {
            Some(gone) => Some(gone),
            // `ECHILD`: the kernel says this is not our child after all — or somebody has
            // already reaped it, which is the same answer as reaping it here. Ask `/proc`.
            None => process_gone_by_proc(pid),
        },
        Parentage::NotOurs => process_gone_by_proc(pid),
    }
}

/// **Reap our own child, if it has exited** — and answer whether it is gone.
///
/// `waitpid(pid, WNOHANG)` and **never `waitpid(-1, …)`**: the targeted form cannot collect
/// another child's status, and a head that reaped something else's exit would be a head
/// corrupting a process table it does not own.
///
/// **This is what a parent owes a child anyway.** An unreaped zombie is a leak whether or not
/// anything is watching: it holds a pid, a task slot and an entry in `/proc`, and the only
/// process that can clear it is this one. So the head reaps on every tick, not only while a
/// stop is in flight — see [`Link::tick`].
///
/// `None` when the kernel will not answer (`ECHILD`, or an interrupted wait), which the caller
/// resolves through `/proc`.
fn reap_own_child(pid: i32) -> Option<bool> {
    let mut status: libc::c_int = 0;
    let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if r == pid {
        // It had exited, and it is reaped now — the zombie this whole function exists for.
        Some(true)
    } else if r == 0 {
        // Our child, and still running. `0` is the only answer that means that.
        Some(false)
    } else {
        None
    }
}

/// The `/proc` test — for a process this head is not the parent of, and for the fallback.
///
/// **A zombie counts as gone**, which is the second half of the fix: a `Z` is a process that
/// has exited and is waiting to be reaped, so it is not a daemon that can still be running.
/// Without the parent's `waitpid` this is the only test available, and with it this is what
/// answers when the kernel declines to.
fn process_gone_by_proc(pid: i32) -> Option<bool> {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => match stat_state(&stat) {
            Some('Z') | Some('X') => Some(true),
            Some(_) => Some(false),
            None => None,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(true),
        // PermissionDenied means the entry IS there; anything else is a `/proc` that cannot
        // answer, which is not the same fact.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(false),
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
    pub fn tick(
        &mut self,
        app: &mut App,
        size: (usize, usize),
        keys: &[Key],
        raw: &[u8],
        draw: &mut Draw<'_>,
    ) {
        app.clock(now_ms());
        // **The workspace's branch, read from the LOOP and never from a paint.** See
        // `gitfield`: it spawns a process, and the rule is the dash collectors' — a
        // measurement never runs where the frame is drawn. Two seconds between readings
        // (`GIT_REFRESH_MS`) is leticl's own interval, kept so the process rate stays low.
        app.refresh_git();

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
        // **The pane owns the keyboard while it is open**, and this is where that is decided.
        //
        // `raw` is the bytes the reader consumed, verbatim, and it is what the program gets:
        // the `Key`s beside it are this head's *reading* of those bytes, and a program fed a
        // reading is a program that never sees the byte its own terminal sent. So the two
        // paths are exclusive, not layered — while a pane is open, `App::key` is not called
        // at all, and the way out is found in the byte stream by `App::pane_keys` before
        // anything is forwarded.
        if app.pane_open() {
            actions.extend(app.pane_keys(raw));
        } else {
            for k in keys {
                // Cloned rather than copied: `Key::Paste` carries the paste, because the
                // point of bracketed paste is that a 3 KB stack trace is one key.
                if let Some(a) = app.key(k.clone()) {
                    actions.push(a);
                }
            }
        }
        // **And the pane's rectangle is not asked for here.** The pane is resized by
        // `compose_screen`, which is the only layer that knows how many rows the pane actually
        // got — `room` is the terminal's height minus the chrome and the header — and the frame
        // it queues is sent on the next tick. Asking here would send the *terminal's* size as
        // though it were the pane's, which is a program laying out for a rectangle nobody drew
        // it in.

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
                // **A pane whose open never left is not a pane.** The submit refused a line on
                // a link that was already down, but the link can go down between the
                // keystroke and this send — inside one tick — and what is left behind is a
                // rectangle with no program in it and no ending coming. See
                // [`App::drop_pane`].
                app.drop_pane();
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
                    // **The operator's half of the board, on its way to the daemon.** The head
                    // sends the WHOLE list: the daemon files it in the operator's half by the
                    // `by` tag on each row, persists it, and publishes it to every head. There is
                    // no per-row frame and no delta, so a head cannot accumulate a difference
                    // between its copy and the store.
                    Action::SetOperatorTodos(items) => {
                        self.client.set_operator_todos(app.seq, items)?;
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
                    //
                    // **Rows, and the shape is the head's own decision.** A child is a
                    // session, so this pane is drawn by the one renderer that draws rows
                    // (`sub_out_from_rows`), and asking for them here is what makes that the
                    // ordinary path rather than the exception. The daemon's answer is still
                    // `Peeked` with a `snapshot` that may be `None` — a daemon built before
                    // the field ignores the shape and answers with its ring — and *that* is
                    // the fallback `SubOut::degraded` puts on the screen, so the asking head
                    // does not have to be the one that says which it got.
                    Action::Peek(id) => {
                        self.client
                            .peek(&id, letibot_sessionlog::protocol::PeekShape::Rows)?;
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
                    // **The operator's own shell line.** No `call_id` to mint and no
                    // result frame to wait for: the daemon owns the run AND the rows,
                    // so this head's job ends at handing the typed line over. The
                    // echo in `pending_prompts` is retired by the `User` row the
                    // daemon appends, which is the same text by construction.
                    Action::OperatorShell { line } => {
                        self.client.operator_shell(app.seq, &line)?;
                    }
                    // **`!term <command>` — the pane, opened with the terminal's own
                    // rectangle.**
                    //
                    // This is the one action that carries a size, and it takes it from `tick`'s
                    // own argument rather than from `App`: the rectangle is the *terminal's*,
                    // and the driver is the layer that read it. Sending a pane at 80×24 and
                    // resizing it a moment later would make every program lay out twice, and
                    // the first layout is the one a full-screen program caches.
                    Action::TermOpen { line } => {
                        self.client.term_open(&line, size.0, size.1)?;
                    }
                    // **The keys, verbatim.** No `client_request_id` and nothing to wait for:
                    // a keystroke has no answer, and the pane's next `TermOutput` is the
                    // program's reply to whatever it did with it.
                    Action::TermInput { bytes } => {
                        self.client.term_input(&bytes)?;
                    }
                    Action::TermResize { cols, rows } => {
                        self.client.term_resize(cols, rows)?;
                    }
                    Action::TermClose => {
                        self.client.term_close()?;
                    }
                    // **The model's half of the `!` completion.** The history is the
                    // head's own and was already tried; this is the fallback, asked when
                    // the history has no match for the prefix or its cycle is exhausted.
                    //
                    // **`client_request_id` is the head's, and it is passed through.**
                    // The answer comes back on the pump as a `ShellSuggestions`, and the
                    // head has to match it to the (prefix, transcript position) it asked
                    // about — so the key it filed the ask under is the key that has to
                    // travel, and a second id minted by this writer would leave the head
                    // holding one the daemon never echoes. The head only asks once per
                    // (prefix, position); the cache and that rule are its own.
                    //
                    // **Nothing here submits.** The answer is a list of candidate lines
                    // for the composer, drawn as candidates with their provenance, and
                    // Enter is still the operator's.
                    Action::SuggestShell {
                        prefix,
                        client_request_id,
                    } => {
                        self.client
                            .suggest_shell(app.seq, &client_request_id, &prefix)?;
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
        // **A child this head is the parent of is reaped every tick**, and not only while a
        // stop is in flight. That is what a parent owes a child: an unreaped zombie holds a
        // pid, a task slot and a `/proc` entry, and the only process that can clear it is this
        // one. It also means the daemon's own exit is collected promptly when it goes without
        // being asked — a crash, or another head's `--stop` — rather than sitting as a zombie
        // until this head exits.
        //
        // One `waitpid(WNOHANG)` per tick, on a pid the head already holds. The result is
        // discarded here on purpose: this is the leak, not the question. The question is
        // [`Link::watch_stop`]'s, and it asks the same function again at the moment it
        // matters.
        if let Some(p) = app.daemon_pid()
            && parentage(p) == Parentage::Ours
        {
            let _ = reap_own_child(p);
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
    /// * **the process** — *the daemon has gone*, and **which test says so depends on whose
    ///   child it is**: a `waitpid` for this head's own (a zombie is still in `/proc`), the
    ///   `/proc` entry — zombie-aware — for anybody else's. See [`Parentage`];
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
        // The process, and only while the pid is known. `Some(true)` is *gone* and is the
        // only answer that ends the wait early; an unanswerable one leaves the deadline to
        // decide, which is the direction to be wrong in — a head that left on an unknown
        // would be the defect again with better manners.
        //
        // **Which test this is depends on the relationship** — see [`Parentage`]. A daemon
        // that is this head's own child is a zombie the moment it exits, and `/proc` goes on
        // listing it until this head waits; the wrapper's `/proc` test is right for a daemon
        // this head merely attached to.
        if let Some(p) = pid
            && process_gone(p) == Some(true)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **`ppid` and the state char come off the LAST `)`** — the field-2 trap, on the
    /// processes somebody would actually name that way.
    ///
    /// `/proc/<pid>/stat` puts the executable's name in parentheses as field 2, unbounded in
    /// length and free to contain spaces and brackets. Splitting on whitespace from the left
    /// reads the wrong field for any process called `(my ) weird (program)`, and reading the
    /// wrong field here means believing a nested BashArena-style `)` is the end of the name.
    #[test]
    fn the_stat_line_is_parsed_from_the_last_bracket() {
        // A plain one: pid (comm) state ppid …
        assert_eq!(stat_state("42 (harnessd) S 7 42 42 0"), Some('S'));
        assert_eq!(stat_ppid("42 (harnessd) S 7 42 42 0"), Some(7));
        // A comm with a space, a `)`, and more parentheses inside it.
        let nasty = "9067 (my ) weird (program)) Z 1 9067 0";
        assert_eq!(stat_state(nasty), Some('Z'));
        assert_eq!(stat_ppid(nasty), Some(1));
        // Truncated or nonsense lines answer nothing rather than guessing.
        assert_eq!(stat_state("nonsense with no bracket"), None);
        assert_eq!(stat_ppid("42 (x) S"), None);
        assert_eq!(stat_ppid("42 (x) S not-a-number 1"), None);
    }

    /// **A zombie is GONE** — the defect, reproduced against a real one.
    ///
    /// The operator: *"they always tell me daemon not stopped after waiting for 5 sec, then
    /// `letibot --stop` tells nothing runs."* Both statements true; the daemon had stopped and
    /// `/proc` still listed it, because it was this process's own unreaped child.
    ///
    /// So this **spawns a real child, lets it exit without waiting, and asks the question** —
    /// the zombie is making the same `/proc` entry the daemon made, and this is the only test
    /// that reproduces the mechanism rather than describing it. `Child` is held and never
    /// waited on: dropping it does not reap it either, which is the point.
    #[test]
    fn an_unreaped_child_that_has_exited_is_gone() {
        use std::process::Command;
        let mut child = Command::new("/bin/true")
            .spawn()
            .expect("`/bin/true` starts");
        let pid = child.id() as i32;
        // It is ours, and the kernel says so — which is what decides which test runs.
        assert_eq!(
            parentage(pid),
            Parentage::Ours,
            "a process this test spawned is not seen as this process's child"
        );
        // Let it exit without waiting for it. A short sleep is the only synchronisation
        // available: there is no event for "it has exited but not been reaped", which is
        // exactly the state under test.
        std::thread::sleep(std::time::Duration::from_millis(200));
        // **It is a zombie, and `/proc` says so** — the observation the old test read as
        // `alive`. Asserted directly, so this test fails loudly rather than silently turning
        // into a test of something else if the timing below ever changes.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        assert_eq!(
            stat_state(&stat),
            Some('Z'),
            "the child exited but is not a zombie — this test is not testing what it says: {stat:?}"
        );
        // **And the answer is GONE**, which the old `fs::metadata` could not give: the
        // directory is there for a zombie.
        assert_eq!(
            process_gone(pid),
            Some(true),
            "an exited child was reported as still running, which is the defect"
        );
        // And the reap really happened: a second ask finds no `/proc` entry at all, because
        // this process has now collected it.
        assert_eq!(process_gone(pid), Some(true));
        let _ = child.wait();
    }

    /// **A live child is still there** — the negative, without which the test above would pass
    /// on a function that answered `gone` to everything.
    #[test]
    fn a_running_child_is_not_gone() {
        use std::process::Command;
        let mut child = Command::new("/bin/sleep")
            .arg("5")
            .spawn()
            .expect("`/bin/sleep` starts");
        let pid = child.id() as i32;
        assert_eq!(parentage(pid), Parentage::Ours);
        assert_eq!(
            process_gone(pid),
            Some(false),
            "a running child read as gone"
        );
        // **And asking did not kill it** — `WNOHANG` returns without waiting, and a version
        // that blocked here would hang the turn for five seconds while looking healthy.
        assert_eq!(process_gone(pid), Some(false));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            process_gone(pid),
            Some(true),
            "a killed child is gone once reaped"
        );
    }

    /// **A process this head is not the parent of is answered by `/proc`, and a pid that never
    /// existed is gone.** The second is the wrapper's own case, and it must not have been
    /// broken by routeing the child case through `waitpid`.
    #[test]
    fn a_pid_that_is_not_this_processs_child_is_answered_from_proc() {
        // **PID 1 is nobody's child**, so this takes the `/proc` path — and it is running, so
        // the answer is *not gone*.
        assert_eq!(parentage(1), Parentage::NotOurs);
        assert_eq!(process_gone(1), Some(false), "init read as gone");
        // A pid that cannot exist. `i32::MAX` is above every `pid_max` on Linux, so this asks
        // the question about a process that was never there — which is what the operator's
        // `letibot --stop` saw a moment after the head exited.
        assert_eq!(parentage(i32::MAX), Parentage::NotOurs);
        assert_eq!(process_gone(i32::MAX), Some(true), "an absent pid is gone");
    }
}
