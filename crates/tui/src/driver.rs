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
        let (client, reader) =
            HeadClient::start_attach(path, session, since_seq, kind, identity, Caps::default())?;
        Ok(Link::spawn(
            client,
            reader,
            session.to_string(),
            kind.to_string(),
            identity.to_string(),
        ))
    }

    fn spawn(
        client: HeadClient,
        reader: FrameReader<UnixStream>,
        session: String,
        kind: String,
        identity: String,
    ) -> Link {
        let (tx, rx) = std::sync::mpsc::channel();
        Link {
            client,
            rx,
            reader: Some(std::thread::spawn(move || pump(reader, tx))),
            session,
            kind,
            identity,
        }
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
        *self = Link::spawn(client, reader, session, kind, identity);
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
                    Action::Secret { req_id, secret } => {
                        self.client.secret(&req_id, secret)?;
                    }
                    // Leaving on purpose: a detach that fails is the socket that was
                    // already gone, which the loop is about to notice anyway.
                    Action::Quit => {
                        let _ = self.client.detach();
                    }
                    // **Ask, then leave.** The daemon announces the stop to every other
                    // head before it goes, so the request has to reach it while this
                    // head is still attached — a detach first would close the socket
                    // the notice travels on.
                    Action::StopDaemon => {
                        // The identity is the client's own — the name it attached
                        // under — rather than anything the head could make up.
                        let who = self.client.identity().to_string();
                        let _ = self.client.stop(app.seq, &who);
                        let _ = self.client.detach();
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
