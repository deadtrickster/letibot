//! **The daemon's half of `!term`** — who owns a pane's pty, and how its bytes reach the head.
//!
//! # What this module is
//!
//! [`letibot_sessionlog::TerminalDriver`] is the seam: a trait the *server* can call on the
//! connection's thread, implemented here because this is the crate with the daemon in it.
//! [`Terminals`] is the implementation, and it is small on purpose — the mechanism is
//! [`letibot_tools::exec::term`], which owns the pty, the `setsid`/`TIOCSCTTY`, the reader
//! thread and the cgroup. What is here is the three things that are *the daemon's*:
//!
//! | | |
//! |---|---|
//! | **the panes** | one per session, held by session id, so a second `TermOpen` is refused rather than replacing a running program |
//! | **the workspace** | the session's own, read from the registry — a pane runs where the conversation runs, not where the daemon does |
//! | **the screen** | [`Pane::log`] — what the program has drawn, kept here and replayed to a head that asks for it (a bare `!term`) |
//! | **the sink** | [`Sink`], which turns the pty's reader thread's bytes into [`ServerFrame::TermOutput`] on the session's heads |
//!
//! # Who holds the screen, and why it is the daemon
//!
//! A head draws the pane, but **the daemon holds it**, and the operator's own defect is what
//! settles the question: *"i typed `!term mc` … it flashed and was gone … a second `!term`
//! then said 'term pane exists'"*, and the code admitted the same thing in its own words
//! (*"a head that switches back does not find its pane again, it finds the transcript"*).
//! A head is one view of a session and there may be several; a program on a pty is the
//! session's and there is exactly one. So the screen belongs to the half that outlives any
//! one head — and putting it here buys three things a head cannot:
//!
//! * **any head that asks can be given it back** — `attach`, the answer to a bare `!term`;
//! * **it survives a session switch**, because nothing about it was ever in a head;
//! * **and it can be handed to the model as text later** (the `capture-pane` idea): the bytes
//!   are here, and `letibot_vt::Screen` is what turns them into rows — a decision that needs
//!   no wire change because the crate that would do it is already below both halves.
//!
//! **What is held is the byte log, and the screen is what those bytes are.** [`Pane::log`] is
//! the last [`SCREEN_LOG`] bytes the program wrote; a replay is one [`ServerFrame::TermOutput`]
//! and the head's own `letibot_vt::Screen` reconstructs the screen from it — the same crate,
//! the same parser, the same cells, so the replay is not a rendering of the screen but the
//! screen's own input. **No new frame is needed to carry a screen**, and that is not a
//! saving: a frame carrying *cells* would have to invent a serialisation for a screen that
//! the wire already has one for, and a head would have to learn a second way to be told what
//! a program drew.
//!
//! **What a cap costs, said rather than discovered.** The log is bounded, so an attach to a
//! pane that has been drawing for hours replays its *tail* — and the tail is trimmed to the
//! next `ESC` so a cut mid-sequence cannot be painted as text. A program that drew its frame
//! before the tail began and has not repainted since would come back incomplete; a program
//! that repaints (which is what a screen program is) comes back whole, and the resize to the
//! attaching head's rectangle gives it one more reason to redraw.
//!
//! **The nudge is not the mechanism, and it was measured.** Resizing the pty to the attaching
//! head's rectangle raises `SIGWINCH` for a program that redraws on a resize — and
//! `TIOCSWINSZ` **with the same size raises nothing at all**: the kernel compares the new
//! `winsize` with the current one and returns before it signals (measured on this box,
//! `crates/tools/src/exec/term.rs`'s `a_same_size_resize_is_not_a_nudge`). So an attach from
//! the *same* head, at the same rectangle, would be nudged into nothing. **The replay is what
//! the attach is proved by**; the resize is what makes a head that switched sessions get a
//! program laid out for its own screen.
//!
//! # The refusal asks whether a program is *running*, and not whether anybody closed it
//!
//! `open` refuses while a pane is live, and the question it asks is
//! [`TermSession::live`] — not `closed`. That distinction is the **ghost** the operator hit:
//! `!term mc` printed one line and exited, nothing had *closed* the pane, so `closed` was
//! false, so the daemon kept the slot — and the next `!term` in that session was refused
//! with *"a pane is already open in this session"* about a pane that had been gone for a
//! minute, with nothing on the screen to leave and no way to clear it. A program that
//! exits on its own ends the pane, and the pane now says so: `TermSession::ended` is set by
//! the reader thread before it reports, and a dead pane's slot is dropped on the next
//! `open` (and by [`Terminals::close`] and [`Terminals`]' own `Drop`).
//!
//! # Why the sink pushes through the hub and not a channel of its own
//!
//! A pane's bytes arrive on a thread this module owns, and they have to reach a socket the
//! server owns. The obvious shape — a `Sender<ServerFrame>` per pane, handed to the
//! connection — is wrong for a reason the tree has already paid for once: **one socket needs
//! one ordering**, and a second writer on it would interleave a repaint into the middle of an
//! event batch. So the sink calls [`Hub::push_frame`], which is the one door for a frame that
//! is not the record, and the seat's own pump writes it in the same order as everything else.
//!
//! **It never blocks.** `push_frame` takes the hub's lock, appends and wakes; a head that is
//! not reading has its oldest pane frame dropped rather than the pty's reader thread parked —
//! see `Hub`'s `MAX_SIDE`. That is the difference between a stale repaint and a program that
//! has stopped drawing, and only one of those is survivable.
//!
//! # The lifetime is a scope, and it is this module that opens it
//!
//! A pane's program must not outlive the pane, and the answer is the tree's own: the command
//! joins a [`ScopeKind::Session`] cgroup through `join_script` before it `exec`s, and the pane
//! ends by [`ScopeTree::end`]. **The scope is this module's and not the session's**, and that
//! is deliberate: the session's cgroup is the harness's and holds the turn's processes, so a
//! pane that ended the session's scope would kill the turn that opened it. One scope per pane,
//! named for the session it belongs to, ended when the operator closes it and again by
//! [`Terminals`]' own `Drop` — so a daemon that stops takes its panes with it.
//!
//! # Leaving is not ending, and this module is where the difference lives
//!
//! **`ctrl-\` detaches.** The head hides the rectangle, returns the conversation and **sends
//! nothing at all** — so this module is not told, the program keeps running on the pty, the
//! screen stays in [`Pane::log`], and the slot stays occupied. That is the whole point of the
//! attach work one version earlier: a pane that ends when you look away is a pane that cannot
//! hold anything you care about. The operator's words for the correction are the 34 note's:
//! *"but i dont want it to exit"*.
//!
//! **`!term close` ends it**, and that is the only act that reaches [`Terminals::close`]: the
//! head asks the operator to confirm it first (its own card, its own key — see
//! `PROTOCOL_VERSION`'s 34 section for why the two questions must not be confusable) and then
//! sends the `TermClose` that was already on the wire. A program that exits on its own is the
//! third ending, and it is nobody's act: the reader thread reports it and the slot is freed on
//! the next `open`.
//!
//! **And a head that is not drawing the pane can still ask what is running in it** —
//! [`Terminals::status`], the answer to `ClientFrame::TermStatus`. A detach is not an event, so
//! there is no row for it; the head draws the fact while it is true, which is what
//! [`TermSession::live`] decides and what this module answers with.
//!
//! **A box with no cgroups degrades and says so**: [`letibot_tools::host_tree`] failing gives
//! [`NoScopes`], `open` fails, the pane runs with no scope, and the ending falls back to
//! `SIGHUP` to the pane's process group — which reaches the program and not what it
//! daemonised away. That is [`letibot_tools::exec::term::TermSession::close`]'s own documented
//! fallback and not a second mechanism invented here.
//!
//! # What is deliberately not here
//!
//! - **TODO: the pane's bytes are not recorded anywhere.** No transcript row, no corpus entry.
//!   The operator's `!` line becomes two rows because it is a *command with a result*; a pane is
//!   the conversation's rectangle given to a program, and its repaints are not conversation. The
//!   log [`Pane::log`] keeps is **not a record** — it is the screen, capped at [`SCREEN_LOG`],
//!   never written to the store and gone when the daemon stops. A *scrollback* (what a program
//!   drew and then scrolled off) is still not kept, and it is a rendering question (the ring
//!   would be `letibot_vt`'s) before it is a storage one.
//! - **TODO: one pane per session, and no second one.** `open` refuses while a pane is live
//!   rather than replacing it: a program killed by the next keystroke is a program that loses
//!   work. Two panes on one screen is a layout question this head does not have an answer for.
//! - **TODO: the pane is not confined.** It runs in the session's workspace with the session's
//!   terminal environment, and it is **not** put inside the namespace boundary a `bash` call
//!   gets. `letibot_tools::exec::confine` is the seam and the decision is not made here — see
//!   the module's own note in `docs/` for what a confined pane would have to keep working
//!   (`/dev/tty`, the cgroup, the workspace mount) before it is a change and not a regression.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::{Registry, TerminalDriver};
use letibot_tools::exec::{
    NoScopes, ScopeId, ScopeKind, ScopeTree, TermConfig, TermError, TermSession, TermSink,
};

/// **What the operator is told when they end the pane deliberately.**
///
/// The sentence comes from here rather than from the signal the program died of, because the
/// operator's act is the fact worth reporting: `nano` killed by `SIGKILL` reads as a crash, and
/// *"you closed the terminal"* reads as what happened. `TermSession::close` takes it for exactly
/// that reason — see its own doc.
///
/// **`closed` and not `left`, and the word is this version's correction.** `ctrl-\` used to end
/// the pane and this sentence used to say *"you left the terminal"*; leaving is now a **detach**
/// that ends nothing, so the only act that reaches this constant is the deliberate one — the
/// head's `!term close`, after it has asked the operator to confirm it (see
/// `PROTOCOL_VERSION`'s 34 section). A row saying *left* about an ending would now read as the
/// opposite of what happened.
const CLOSED: &str = "you closed the terminal";

/// **How much of what a program drew the daemon keeps.**
///
/// The cap is what makes a log a screen rather than a leak: a program redrawing as fast as it
/// likes would otherwise grow this without bound, and the only reader is an attach, which
/// wants the screen as it is *now* — which is the tail. 256 KiB is chosen against the same
/// measurement the pane's own frames were (`top` repaints about 2 KB, a 200×50 full repaint
/// about 4 KB): it is **several hundred repaints**, so the tail of any pane that is drawing
/// contains the current screen many times over, and a pane that has been idle for hours still
/// has everything it drew in the last hour of it.
///
/// See the module header for what the cap costs a program that has not repainted since before
/// the tail began, and for the trim that keeps a cut from being painted as text.
const SCREEN_LOG: usize = 256 * 1024;

/// **One pane per session, and the pty behind each.** See the module header.
pub struct Terminals {
    /// **A `Weak` and not an `Arc`, because the registry holds this driver.** The cycle is
    /// real and would be a leak: `Registry::set_terminal` stores an `Arc<dyn TerminalDriver>`
    /// on the registry, so a driver holding an `Arc<Registry>` keeps every session's log,
    /// view and hub alive for the life of the process. Upgraded once per `open`, which is an
    /// operator's act and not a hot path.
    registry: Weak<Registry>,
    /// The cgroup tree the panes' scopes are opened in, or [`NoScopes`] on a box without
    /// cgroup v2 — see the module header for what degrades and what does not.
    tree: Arc<dyn ScopeTree>,
    panes: Mutex<HashMap<String, Pane>>,
}

/// A pane: the pty session, what it is running, and **what it has drawn**.
///
/// Its scope is the session's own ([`TermSession`] holds both ends of it) so that closing the
/// pane and dropping the pane are the same act.
///
/// **The command and the log are the daemon's answer to a bare `!term`** — see the module
/// header for why the screen is the daemon's at all, and [`Terminals::attach`] for what it does
/// with the two.
///
/// **A pane whose program has ended is kept here until the next `open`**, deliberately: its
/// scope is still the cgroup that has to be ended (a program that forked something away
/// leaves that something in it — see [`TermSession::close`]), and a slot that is dropped the
/// instant a program exits would be a second path ending scopes. What must not happen is a
/// *refusal*, and that is [`TermSession::live`]'s job.
struct Pane {
    session: TermSession,
    /// The line the operator typed after the verb, as the daemon was handed it — what a head
    /// that attaches is told is running. The verb is not part of it: see
    /// [`ServerFrame::TermAttached`].
    command: String,
    /// **The last [`SCREEN_LOG`] bytes the program wrote**, shared with the reader thread's
    /// sink. See the module header.
    log: Arc<Mutex<Vec<u8>>>,
}

impl Pane {
    /// **What the program has drawn**, as one block of bytes for a head that asked.
    ///
    /// Cloned rather than lent: the caller pushes it through the hub, which takes its own lock,
    /// and a borrow held across that would be a pane's log lock held while another head's
    /// queue is walked. One clone per attach, which is an operator's act and not a hot path.
    fn screen(&self) -> Vec<u8> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Terminals {
    /// **A driver for this daemon.** `registry` is weak — see the field.
    pub fn new(registry: Weak<Registry>) -> Terminals {
        let tree: Arc<dyn ScopeTree> = match letibot_tools::host_tree() {
            Ok(c) => Arc::from(c),
            Err(e) => Arc::new(NoScopes::new(format!(
                "a pane has no cgroup to live in, so it will be ended by a signal to its own \
                 process group and not by the tree: {e}"
            ))),
        };
        Terminals {
            registry,
            tree,
            panes: Mutex::new(HashMap::new()),
        }
    }

    fn panes(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pane>> {
        self.panes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **Where this session runs.** The registry's own wiring, and the daemon's directory as
    /// the fallback — which is what a session the registry has never heard of gets, and is a
    /// starting directory rather than a report about one.
    fn workspace(&self, session_id: &str) -> PathBuf {
        let said = self
            .registry
            .upgrade()
            .map(|r| r.wiring(session_id).workspace)
            .unwrap_or_default();
        if said.trim().is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            PathBuf::from(said)
        }
    }
}

/// **Where a pane's bytes go.** See the module header for why this is the hub and not a
/// channel: one socket, one ordering.
///
/// **And it is where the daemon's copy of the screen is kept**, which is the second half of
/// the same job: the bytes go up to the session's heads *and* into the pane's log, from the one
/// place they already pass through. A log written by a second reader — a thread of its own,
/// a tee on the pty — would be a second place the same bytes could be lost, delayed or
/// reordered, and the replay would then disagree with what the heads saw.
struct Sink {
    hub: Arc<Hub>,
    log: Arc<Mutex<Vec<u8>>>,
}

impl TermSink for Sink {
    fn output(&self, bytes: &[u8]) {
        // **Kept first, and never allowed to block**: this runs on the pty's reader thread,
        // and the lock is held only for an append and a trim.
        if let Ok(mut log) = self.log.lock() {
            log.extend_from_slice(bytes);
            trim(&mut log);
        }
        self.hub.push_frame(ServerFrame::TermOutput {
            bytes: bytes.to_vec(),
        });
    }

    fn ended(&self, reason: &str) {
        self.hub.push_frame(ServerFrame::TermEnded {
            reason: reason.to_string(),
        });
    }
}

/// **Keep the tail of what a program drew, and start it where a parser can read it.**
///
/// A cut lands wherever the program happened to be — inside an escape sequence, most of the
/// time — and the head that receives the tail parses it from its ground state, so `[2J` would
/// be painted as three characters at the cursor. Dropping to the next `ESC` costs at most the
/// escape-free run before it, which a repainting program replaces on its next frame. **A tail
/// with no `ESC` in it at all is left alone**: that is a program writing text (`!term cat
/// file`), and text is what it wrote.
fn trim(log: &mut Vec<u8>) {
    if log.len() <= SCREEN_LOG {
        return;
    }
    let over = log.len() - SCREEN_LOG;
    log.drain(..over);
    if let Some(at) = log.iter().position(|b| *b == 0x1b) {
        log.drain(..at);
    }
}

impl TerminalDriver for Terminals {
    fn open(
        &self,
        session_id: &str,
        hub: &Arc<Hub>,
        command: &str,
        cols: usize,
        rows: usize,
    ) -> Result<(), String> {
        let mut panes = self.panes();
        // **A live pane is refused by name, not replaced.** Killing a running program to make
        // room for the next keystroke is how an operator loses an edited file; the refusal
        // names the way back and the way out instead — and neither of them is `ctrl-\`, which
        // now detaches and ends nothing. See `PROTOCOL_VERSION`'s 34 section.
        //
        // **`live` and not `closed`** — see the module header's *the ghost*: a program that
        // exits on its own ends the pane without anybody closing it, and refusing the next
        // `!term` on that pane is refusing it on nothing.
        if let Some(p) = panes.get(session_id)
            && p.session.live()
        {
            return Err(format!(
                "a pane is already open in this session and `{command}` was not started. \
                 `!term` comes back to it and `!term close` ends it — a pane that replaced a \
                 running program would lose whatever that program had not saved."
            ));
        }
        // **A pane that has already ended leaves nothing to keep**, and this is where the
        // slot is freed: dropping it ends its scope (which the cgroup has usually already
        // emptied).
        panes.remove(session_id);

        // **The scope, before the process.** `join_script` writes the pid into `cgroup.procs`
        // and *then* `exec`s, so there is no window in which the program or its children could
        // be forked outside it. A box with no cgroups fails here and the pane runs unscoped —
        // which `TermSession::close` names as its own fallback rather than pretending to own
        // what it cannot reach.
        let scope: Option<ScopeId> = self
            .tree
            .open(ScopeKind::Session, &format!("term-{session_id}"), None)
            .ok();

        let cfg = TermConfig {
            command: command.to_string(),
            // The pane's environment, and **not** the capture's: `console::env_from` forces
            // `PAGER=cat` because a capture has nobody at the keyboard, and a pane has
            // somebody. See `letibot_tools::exec::term::env_from`.
            env: letibot_tools::exec::term::env(),
            cwd: self.workspace(session_id),
            cols,
            rows,
            scope,
            tree: Some(self.tree.clone()),
        };
        // **The log is made before the program is**, because the reader thread starts writing
        // into it the moment the pty has a byte — and because the pane that is inserted below
        // and the sink the thread holds must be the *same* `Arc`.
        let log: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::new(Sink {
            hub: hub.clone(),
            log: Arc::clone(&log),
        });
        let session = TermSession::start(&cfg, sink).map_err(|e: TermError| e.to_string())?;
        panes.insert(
            session_id.to_string(),
            Pane {
                session,
                command: command.to_string(),
                log,
            },
        );
        Ok(())
    }

    /// **Give this head the pane the session already has** — a bare `!term`.
    ///
    /// Three things, in this order, and each of them is a decision:
    ///
    /// 1. **The attaching head's rectangle.** `TermSession::resize` is `TIOCSWINSZ`, so a
    ///    program that lays out for the screen it is drawn in gets the *new* one — a head that
    ///    switched sessions has a different rectangle from the one the pane opened at. It is
    ///    **not** the mechanism the attach is proved by: `TIOCSWINSZ` with an unchanged size
    ///    raises no `SIGWINCH` at all (the kernel compares before it signals — see
    ///    `letibot_tools::exec::term::tests::a_same_size_resize_is_not_a_nudge`), so for the
    ///    same head at the same size this is a no-op.
    /// 2. **What is running**, as [`ServerFrame::TermAttached`] — *before* the bytes, because a
    ///    head that drew the screen first and learned what it was afterwards would flash a
    ///    rectangle it could not name.
    /// 3. **The screen**: the whole log as **one** [`ServerFrame::TermOutput`]. One frame and
    ///    not several, and that is not tidiness — `Hub::push_frame` drops the oldest pane frame
    ///    for a head that is behind, so a replay split across frames could be delivered in part
    ///    and a *partial* screen is a corrupt screen where no screen is an honest empty one.
    ///
    /// **A session with no pane is a sentence, not a silence.** The head opened the rectangle
    /// the moment the operator pressed enter (it cannot wait for the daemon without showing the
    /// transcript for as long as the round trip takes), so *there is nothing to attach to* has
    /// to arrive as the `TermEnded` that closes it — the same frame a refusal uses, for the same
    /// reason: the head's act is identical either way.
    fn attach(
        &self,
        session_id: &str,
        hub: &Arc<Hub>,
        cols: usize,
        rows: usize,
    ) -> Result<(), String> {
        let panes = self.panes();
        let Some(p) = panes.get(session_id) else {
            return Err(
                "this session has no pane to attach to — `!term COMMAND` starts one. Nothing \
                 was attached."
                    .to_string(),
            );
        };
        if !p.session.live() {
            return Err(
                "the pane in this session is over — its program has exited, so there is \
                 nothing to attach to. `!term COMMAND` starts a new one."
                    .to_string(),
            );
        }
        p.session.resize(cols, rows);
        hub.push_frame(ServerFrame::TermAttached {
            command: p.command.clone(),
        });
        let screen = p.screen();
        if !screen.is_empty() {
            hub.push_frame(ServerFrame::TermOutput { bytes: screen });
        }
        Ok(())
    }

    /// **What this session's pane is running, or nothing** — the read behind a head's own line
    /// about a program it is not drawing.
    ///
    /// **`live` and not merely *present*, exactly as `open`'s refusal is.** A pane whose program
    /// has exited is kept until the next `open` (its scope is still the cgroup that has to be
    /// ended — see [`Pane`]), and reporting it as *running* would put a head's *a pane is
    /// running `!term mc`* line on the screen for a program that has been gone for a minute. A
    /// head that asks this question is asking *is something running here*, which is the question
    /// [`TermSession::live`] answers.
    ///
    /// The command is the daemon's own string, the one it was handed at `open` — the same one
    /// [`TerminalDriver::attach`] puts on [`ServerFrame::TermAttached`], so the two answers
    /// cannot disagree.
    fn status(&self, session_id: &str) -> Option<String> {
        let panes = self.panes();
        panes
            .get(session_id)
            .filter(|p| p.session.live())
            .map(|p| p.command.clone())
    }

    fn input(&self, session_id: &str, bytes: &[u8]) -> Result<(), String> {
        let panes = self.panes();
        match panes.get(session_id) {
            Some(p) => p.session.input(bytes).map_err(|e| e.to_string()),
            // No pane: a keystroke nobody is listening for. Quiet, like `close` — a key
            // that arrived a moment after the pane ended is not an error anybody can act on.
            None => Ok(()),
        }
    }

    fn resize(&self, session_id: &str, cols: usize, rows: usize) -> Result<(), String> {
        let panes = self.panes();
        if let Some(p) = panes.get(session_id) {
            p.session.resize(cols, rows);
        }
        Ok(())
    }

    fn close(&self, session_id: &str) -> Result<(), String> {
        let mut panes = self.panes();
        if let Some(mut p) = panes.remove(session_id) {
            // **The operator's sentence, set before the kill.** The pty's reader thread
            // composes the `TermEnded` the head will show, and it reads this — so the pane
            // that was closed says *"you closed the terminal"* rather than the signal number
            // the program died of. See `TermSession::close`.
            //
            // **This is the deliberate act, and the only one that reaches here.** `ctrl-\`
            // detaches and sends nothing at all; the frame that lands on this method is sent
            // by a head whose `!term close` the operator confirmed.
            p.session.close(CLOSED);
        }
        Ok(())
    }
}

impl Drop for Terminals {
    /// **A daemon that stops takes its panes with it.** Every live pane is closed, which is
    /// `ScopeTree::end` on its cgroup — the same act a confirmed `!term close` performs, and for
    /// the same reason: a screen program left running with no pane to draw in is a process
    /// nobody can see and nobody can end.
    fn drop(&mut self) {
        let mut panes = self.panes();
        for (_, mut p) in panes.drain() {
            p.session.close("the daemon stopped");
        }
    }
}
