//! The daemon's half of `!term`: the panes a session owns, and how their bytes reach a
//! head. [`letibot_sessionlog::TerminalDriver`] is the seam and [`Terminals`] implements
//! it; the pty itself is [`letibot_tools::exec::term`]. What lives here is what the daemon
//! has to hold The screen is the daemon's because a program on a pty belongs to the session
//! while a head is one view of it: any head that asks can be given the screen back, and a
//! session switch does not lose it. The log is bytes and the head rebuilds cells with
//! `letibot_vt::Screen`, so no frame has to carry a screen.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{PaneBrief, ServerFrame};
use letibot_sessionlog::{Registry, TerminalDriver};
use letibot_tools::exec::{
    NoScopes, ScopeId, ScopeKind, ScopeTree, TermConfig, TermError, TermSession, TermSink,
};

/// What the operator is told when they end a pane deliberately. The operator's act is the
/// fact worth reporting: a program killed by `SIGKILL` reads as a crash, while "you closed
/// the terminal" reads as what happened.
const CLOSED: &str = "you closed the terminal";

/// How much of what a program drew the daemon keeps. A cap makes this a screen rather than
/// a leak, and the only reader wants the screen as it is now. Hundreds of repaints fit, so
/// a pane that is drawing always holds its current screen.
const SCREEN_LOG: usize = 256 * 1024;

/// How many live panes one session may hold, so a stuck key cannot fill the box with ptys.
/// The refusal names the panes, so the way out is in the sentence.
const MAX_PANES: usize = 8;

/// One session's panes, by number, and the bookkeeping that numbers them.
#[derive(Default)]
struct Session {
    panes: std::collections::BTreeMap<u32, Pane>,
    /// The number the next pane gets. Starts at 1 and only grows.
    next: u32,
    /// The pane last opened or attached to, which is what a bare `!term` comes back to.
    last: Option<u32>,
}

impl Session {
    /// Drop every pane whose program has ended, ending its scope. [`Pane`] says why an ended
    /// pane is kept until here rather than dropped the instant its program exits.
    fn sweep(&mut self) {
        self.panes.retain(|_, p| p.session.live());
    }

    fn live(&self) -> Vec<(u32, &Pane)> {
        self.panes
            .iter()
            .filter(|(_, p)| p.session.live())
            .map(|(n, p)| (*n, p))
            .collect()
    }
}

/// Every session's panes, and the pty behind each. See the module header.
pub struct Terminals {
    /// A `Weak`, because `Registry::set_terminal` holds this driver: an `Arc` back would keep
    /// every session's log, view and hub alive for the life of the process.
    registry: Weak<Registry>,
    /// The cgroup tree the panes' scopes are opened in, or [`NoScopes`] on a box without
    /// cgroup v2.
    tree: Arc<dyn ScopeTree>,
    panes: Mutex<HashMap<String, Session>>,
}

/// A pane: the pty session, what it is running, and what it has drawn. [`TermSession`]
/// holds both ends of the scope, so closing the pane and dropping it are one act.
struct Pane {
    session: TermSession,
    /// The line the operator typed after the verb, as the daemon was handed it. The verb is
    /// not part of it: see [`ServerFrame::TermAttached`].
    command: String,
    /// The last [`SCREEN_LOG`] bytes the program wrote, shared with the reader thread's sink.
    log: Arc<Mutex<Vec<u8>>>,
}

impl Pane {
    /// What the program has drawn, as one block of bytes for a head that asked. Cloned rather
    /// than lent: the caller pushes it through the hub, which takes its own lock and a borrow
    /// across that would hold the log lock while a head's queue is walked.
    fn screen(&self) -> Vec<u8> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Terminals {
    /// A driver for this daemon. `registry` is weak, for the reason the field gives.
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

    fn panes(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.panes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where this session runs: its wiring, else the daemon's directory.
    ///
    /// An unknown session gets the daemon's directory as a starting place, not as a report.
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

/// Where a pane's bytes go: the hub, and the daemon's copy of the screen. One socket needs
/// one ordering, so a frame of its own would interleave into an event batch. The log is
/// filled here for the same reason
struct Sink {
    hub: Arc<Hub>,
    log: Arc<Mutex<Vec<u8>>>,
    /// The pane's number, on every frame.
    pane: u32,
}

impl TermSink for Sink {
    fn output(&self, bytes: &[u8]) {
        // Kept first and never allowed to block: this runs on the pty's reader thread, and
        // the lock is held only for an append and a trim.
        if let Ok(mut log) = self.log.lock() {
            log.extend_from_slice(bytes);
            trim(&mut log);
        }
        self.hub.push_frame(ServerFrame::TermOutput {
            pane: self.pane,
            bytes: bytes.to_vec(),
        });
    }

    fn ended(&self, reason: &str) {
        self.hub.push_frame(ServerFrame::TermEnded {
            pane: Some(self.pane),
            reason: reason.to_string(),
        });
    }
}

/// Keep the tail of what a program drew, and start it where a parser can read it. A cut
/// usually lands inside an escape sequence, and a head parses the tail from ground state so
/// `[2J` would be painted as three characters. A tail with no `ESC` in it is left alone
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
    ) -> Result<u32, String> {
        let mut all = self.panes();
        let session = all.entry(session_id.to_string()).or_default();
        // Ended panes leave nothing to keep, and dropping one ends its scope.
        session.sweep();
        // A live pane is not a refusal: the new program runs beside it. Only [`MAX_PANES`] is
        // refused, by name, with the numbers that would free one.
        if session.panes.len() >= MAX_PANES {
            let held: Vec<String> = session
                .live()
                .iter()
                .map(|(n, p)| format!("{n} `{}`", p.command))
                .collect();
            return Err(format!(
                "this session already has {MAX_PANES} panes running ({}), so `{command}` was \
                 not started. `!term close N` ends one.",
                held.join(", ")
            ));
        }

        let cwd = self.workspace(session_id);
        let env = letibot_tools::exec::term::env();
        // The session's boundary, before anything is opened: a refusal here starts nothing that
        // would then have to be ended. A session that installed none runs its pane on the host.
        let boundary = self
            .registry
            .upgrade()
            .and_then(|r| r.pane_boundary(session_id));
        let confine = match &boundary {
            None => Vec::new(),
            Some(wrap) => {
                wrap(&cwd, &letibot_tools::exec::term::confined_env(&env)).map_err(|why| {
                    format!(
                        "`{command}` was not started: this session's boundary refused it — {why}"
                    )
                })?
            }
        };

        // The scope before the process: `join_script` writes the pid into `cgroup.procs` and
        // then `exec`s, so nothing can be forked outside it. No cgroups means no scope, and the
        // pane runs unscoped.
        session.next = session.next.max(1);
        let pane = session.next;
        let scope: Option<ScopeId> = self
            .tree
            .open(
                ScopeKind::Session,
                &format!("term-{session_id}-{pane}"),
                None,
            )
            .ok();

        let cfg = TermConfig {
            command: command.to_string(),
            // The pane's environment and not the capture's: `env_from` forces `PAGER=cat`
            // because a capture has nobody at the keyboard, and a pane has somebody.
            env,
            cwd,
            cols,
            rows,
            scope,
            tree: Some(self.tree.clone()),
            confine,
        };
        // The log before the program: the reader thread writes into it from the first byte, and
        // the pane inserted below and the sink the thread holds must be the same `Arc`.
        let log: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::new(Sink {
            hub: hub.clone(),
            log: Arc::clone(&log),
            pane,
        });
        // The number before the first byte, on the same hub, so no head sees a `TermOutput`
        // for a pane it has not been told about.
        hub.push_frame(ServerFrame::TermAttached {
            pane,
            command: command.to_string(),
            opened: true,
        });
        let started = TermSession::start(&cfg, sink).map_err(|e: TermError| e.to_string());
        let started = match started {
            Ok(s) => s,
            Err(why) => {
                // The head was told the number; it is told the ending too, under the same one.
                hub.push_frame(ServerFrame::TermEnded {
                    pane: Some(pane),
                    reason: why.clone(),
                });
                return Err(why);
            }
        };
        session.next = pane + 1;
        session.last = Some(pane);
        session.panes.insert(
            pane,
            Pane {
                session: started,
                command: command.to_string(),
                log,
            },
        );
        Ok(pane)
    }

    /// Give this head a pane the session already has: a bare `!term`, or `!term N`. The
    /// rectangle first, so a program lays out for the screen it is now drawn in; a `TIOCSWINSZ`
    /// at an unchanged size raises no `SIGWINCH`, so this is not what proves the attach.
    fn attach(
        &self,
        session_id: &str,
        pane: Option<u32>,
        hub: &Arc<Hub>,
        cols: usize,
        rows: usize,
    ) -> Result<u32, String> {
        let mut all = self.panes();
        let Some(session) = all.get_mut(session_id).filter(|s| !s.live().is_empty()) else {
            return Err(
                "this session has no pane to attach to — `!term COMMAND` starts one. Nothing \
                 was attached."
                    .to_string(),
            );
        };
        let live: Vec<u32> = session.live().iter().map(|(n, _)| *n).collect();
        let n = match pane {
            Some(n) if live.contains(&n) => n,
            Some(n) => {
                return Err(format!(
                    "there is no pane {n} running in this session — the live ones are {}. \
                     Nothing was attached.",
                    live.iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            None => session
                .last
                .filter(|n| live.contains(n))
                .unwrap_or(*live.last().expect("checked non-empty")),
        };
        session.last = Some(n);
        let p = &session.panes[&n];
        p.session.resize(cols, rows);
        hub.push_frame(ServerFrame::TermAttached {
            pane: n,
            command: p.command.clone(),
            opened: false,
        });
        let screen = p.screen();
        if !screen.is_empty() {
            hub.push_frame(ServerFrame::TermOutput {
                pane: n,
                bytes: screen,
            });
        }
        Ok(n)
    }

    /// What this session's panes are running: the read behind a head's line about programs it
    /// is not drawing.
    fn status(&self, session_id: &str) -> Vec<PaneBrief> {
        self.panes()
            .get(session_id)
            .map(|s| {
                s.live()
                    .into_iter()
                    .map(|(pane, p)| PaneBrief {
                        pane,
                        command: p.command.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn input(&self, session_id: &str, pane: u32, bytes: &[u8]) -> Result<(), String> {
        match self
            .panes()
            .get(session_id)
            .and_then(|s| s.panes.get(&pane))
        {
            Some(p) => p.session.input(bytes).map_err(|e| e.to_string()),
            // No pane: a keystroke nobody is listening for. A key that arrived a moment
            // after the pane ended is not an error anybody can act on.
            None => Ok(()),
        }
    }

    fn resize(&self, session_id: &str, pane: u32, cols: usize, rows: usize) -> Result<(), String> {
        if let Some(p) = self
            .panes()
            .get(session_id)
            .and_then(|s| s.panes.get(&pane))
        {
            p.session.resize(cols, rows);
        }
        Ok(())
    }

    fn close(&self, session_id: &str, pane: u32) -> Result<(), String> {
        let taken = self
            .panes()
            .get_mut(session_id)
            .and_then(|s| s.panes.remove(&pane));
        if let Some(mut p) = taken {
            // Set before the kill: the reader thread composes the `TermEnded` from it, so the
            // pane says "you closed the terminal" and not the signal it died of.
            p.session.close(CLOSED);
        }
        Ok(())
    }
}

impl Drop for Terminals {
    /// A daemon that stops takes its panes with it: a screen program left running with no pane
    /// to draw in is a process nobody can see and nobody can end.
    fn drop(&mut self) {
        let mut all = self.panes();
        for (_, session) in all.drain() {
            for (_, mut p) in session.panes {
                p.session.close("the daemon stopped");
            }
        }
    }
}
