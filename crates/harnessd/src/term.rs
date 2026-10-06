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
//! | **the sink** | [`Sink`], which turns the pty's reader thread's bytes into [`ServerFrame::TermOutput`] on the session's heads |
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
//! named for the session it belongs to, ended when the operator leaves and again by
//! [`Terminals`]' own `Drop` — so a daemon that stops takes its panes with it.
//!
//! **A box with no cgroups degrades and says so**: [`Cgroup2::probe`] failing gives
//! [`NoScopes`], `open` fails, the pane runs with no scope, and the ending falls back to
//! `SIGHUP` to the pane's process group — which reaches the program and not what it
//! daemonised away. That is [`letibot_tools::exec::term::TermSession::close`]'s own documented
//! fallback and not a second mechanism invented here.
//!
//! # What is deliberately not here
//!
//! - **TODO: the pane's bytes are not recorded anywhere.** No transcript row, no corpus entry,
//!   no byte log. The operator's `!` line becomes two rows because it is a *command with a
//!   result*; a pane is the conversation's rectangle given to a program, and its repaints are
//!   not conversation. A scrollback of what a pane drew is a rendering question and a byte log
//!   is a storage question, and neither is answered by putting it in the transcript.
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
    Cgroup2, NoScopes, ScopeId, ScopeKind, ScopeTree, TermConfig, TermError, TermSession, TermSink,
};

/// **What the operator is told when they press the way out.**
///
/// The sentence comes from here rather than from the signal the program died of, because the
/// operator's act is the fact worth reporting: `nano` killed by `SIGKILL` reads as a crash, and
/// *"you left the terminal"* reads as what happened. `TermSession::close` takes it for exactly
/// that reason — see its own doc.
const LEFT: &str = "you left the terminal";

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

/// A live pane: the pty session, and nothing else. Its scope is the session's own
/// ([`TermSession`] holds both ends of it) so that closing the pane and dropping the pane are
/// the same act.
///
/// **A pane whose program has ended is kept here until the next `open`**, deliberately: its
/// scope is still the cgroup that has to be ended (a program that forked something away
/// leaves that something in it — see [`TermSession::close`]), and a slot that is dropped the
/// instant a program exits would be a second path ending scopes. What must not happen is a
/// *refusal*, and that is [`TermSession::live`]'s job.
struct Pane {
    session: TermSession,
}

impl Terminals {
    /// **A driver for this daemon.** `registry` is weak — see the field.
    pub fn new(registry: Weak<Registry>) -> Terminals {
        let tree: Arc<dyn ScopeTree> = match Cgroup2::probe() {
            Ok(c) => Arc::new(c),
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
struct Sink {
    hub: Arc<Hub>,
}

impl TermSink for Sink {
    fn output(&self, bytes: &[u8]) {
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
        // names the way out instead.
        //
        // **`live` and not `closed`** — see the module header's *the ghost*: a program that
        // exits on its own ends the pane without anybody closing it, and refusing the next
        // `!term` on that pane is refusing it on nothing.
        if let Some(p) = panes.get(session_id)
            && p.session.live()
        {
            return Err(format!(
                "a pane is already open in this session and `{command}` was not started. \
                 Leave it with ctrl-\\ first — a pane that replaced a running program would \
                 lose whatever that program had not saved."
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
        let session = TermSession::start(&cfg, Arc::new(Sink { hub: hub.clone() }))
            .map_err(|e: TermError| e.to_string())?;
        panes.insert(session_id.to_string(), Pane { session });
        Ok(())
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
            // that was closed says *"you left the terminal"* rather than the signal number
            // the program died of. See `TermSession::close`.
            p.session.close(LEFT);
        }
        Ok(())
    }
}

impl Drop for Terminals {
    /// **A daemon that stops takes its panes with it.** Every live pane is closed, which is
    /// `ScopeTree::end` on its cgroup — the same act the operator's way out performs, and for
    /// the same reason: a screen program left running with no pane to draw in is a process
    /// nobody can see and nobody can end.
    fn drop(&mut self) {
        let mut panes = self.panes();
        for (_, mut p) in panes.drain() {
            p.session.close("the daemon stopped");
        }
    }
}
