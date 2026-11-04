//! The daemon's half of `!term`: the panes a session owns and how their bytes reach a head. [`letibot_sessionlog::TerminalDriver`] is the seam, [`Terminals`] implements it, the pty is [`letibot_tools::exec::term`].
//!
//! A pane ends with its session, and a starting daemon sweeps panes a killed predecessor of the same workspace left ([`Terminals::begin`]): a program drawing into an unopenable pane cannot otherwise be ended.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{PaneBrief, ServerFrame};
use letibot_sessionlog::{Registry, TerminalDriver};
use letibot_tools::exec::memory::{Meminfo, MemoryPolicy};
use letibot_tools::exec::{
    NoScopes, ScopeId, ScopeKind, ScopeTree, TermConfig, TermError, TermSession, TermSink,
};

use crate::leticode_config::LeticodeConfig;

/// What the operator is told when they end a pane deliberately. The operator's act is the
/// fact worth reporting: a program killed by `SIGKILL` reads as a crash, while "you closed
/// the terminal" reads as what happened.
const CLOSED: &str = "you closed the terminal";

/// [`CLOSED`]'s counterpart for a pane a SESSION's end took with it. Same reason: the pane's
/// ending names the act that ended it, and an exit status says only that a program died.
const WITH_THE_SESSION: &str = "the session ended";

/// A pane's scope under the tree's root - `session.term-<session>-<pane>`, the name
/// [`TerminalDriver::open`] opens it under. It is the one thing in a tree a successor daemon
/// may end without asking, which is why the sweep reads it by name and not by anything else.
const PANE_DIR: &str = "session.term-";

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

/// What a pane's scope is capped by, resolved PER SESSION the way a `bash` call's cap is: the
/// box's `[memory]` under the user's and the session's own project's (R76 item 7). A pane is
/// somebody working at a keyboard, and it used to be the one way into the box that ran uncapped.
#[derive(Clone)]
pub struct PaneMemory {
    /// The box-wide numbers, `providers.toml`'s `[memory]`.
    box_wide: MemoryPolicy,
    /// The user's `leticode.toml`, whose `[memory]` is the level above the project's.
    user: LeticodeConfig,
    /// Whether the session's own workspace may speak - `--no-project-config` turns it off.
    read_project: bool,
}

impl PaneMemory {
    pub fn new(box_wide: MemoryPolicy, user: LeticodeConfig, read_project: bool) -> PaneMemory {
        PaneMemory {
            box_wide,
            user,
            read_project,
        }
    }

    /// The cap a pane in `workspace` runs under, or `None` on a box with no memory controller to
    /// hold one - the same answer `bash` gives there, so the two cannot disagree about what
    /// *capped* means on this machine.
    fn bytes(&self, workspace: &Path) -> Option<u64> {
        // One read of what is free, handed to the resolver: a fraction must mean the same bytes to
        // the policy and to the number written on the scope, and two reads are two moments.
        let free = Meminfo::read()?.available;
        let policy = crate::leticode_config::memory_policy_at(
            self.box_wide,
            &self.user,
            workspace,
            self.read_project,
            Some(free),
        );
        Some(policy.default.resolve(free))
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
    /// What a pane's scope is capped by, or `None` for a driver that caps nothing.
    memory: Option<PaneMemory>,
    /// Whether a cap the tree would not take has been said. Once: every pane would say it, and a
    /// warning that repeats reads as a new fault each time.
    said_uncapped: AtomicBool,
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
            memory: None,
            said_uncapped: AtomicBool::new(false),
        }
    }

    /// This driver with a memory policy: every pane opened from here is capped by it. The
    /// daemon's own answer, because a pane's cap is resolved against the workspace it runs in.
    pub fn with_memory(mut self, memory: PaneMemory) -> Terminals {
        self.memory = Some(memory);
        self
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

    /// Cap one pane's scope the way a `bash` call's is capped, and say so ONCE if the tree will
    /// not take it: a cap that is silently absent reads as protection, and every pane would say
    /// it if the sentence were per pane.
    fn cap(&self, scope: &ScopeId, workspace: &Path) {
        let Some(bytes) = self.memory.as_ref().and_then(|m| m.bytes(workspace)) else {
            return;
        };
        if let Err(why) = self.tree.cap_memory(scope, bytes)
            && !self.said_uncapped.swap(true, Ordering::SeqCst)
        {
            eprintln!("  panes run UNCAPPED: {why}");
        }
    }

    /// What a daemon does about panes its predecessor did not outlive: end what a DEAD daemon of
    /// this workspace left running, then record this tree so the next one can find this one's.
    /// `record` is [`record_path`]'s answer, and the reply is one sentence per tree swept.
    pub fn begin(&self, record: &Path) -> Vec<String> {
        let (mut keep, said) = sweep_record(record, self.tree.as_ref(), &owner_alive);
        if let Some(mine) = self.tree.root() {
            keep.push(mine.display().to_string());
        }
        write_record(record, &keep);
        said
    }
}

/// Where this workspace's daemons record the scope trees their panes live in: one root per line,
/// under the box's data directory and keyed by the workspace. The cgroup path alone cannot say
/// which workspace a tree belonged to, and another folder's panes are not this daemon's to end.
pub fn record_path(workspace: &Path) -> Option<PathBuf> {
    Some(
        crate::mergequeue::git::letibot_data_dir()?
            .join("panes")
            .join(format!(
                "{}.scopes",
                letibot_sessionlog::workspace_key(workspace)
            )),
    )
}

/// Whether a pid is a process that can still act. A zombie answers `kill -0` and owns nothing, so
/// it is dead here - the reading `mergequeue::lease` and `writer` both take.
fn owner_alive(pid: u32) -> bool {
    letibot_tools::exec::procs::read(pid).is_some_and(|p| p.state != 'Z')
}

/// The owner's pid out of a tree root's own name, `letibot.<pid>.<n>`, and `None` for a directory
/// this format did not write - nothing then says whose it is, so nothing in it is ended.
fn owner_pid(name: &str) -> Option<u32> {
    let mut parts = name.strip_prefix("letibot.")?.split('.');
    let pid = parts.next()?.parse().ok()?;
    parts.next()?;
    parts.next().is_none().then_some(pid)
}

/// End the panes under one dead daemon's tree root and say what happened: the sentence and whether anything would not die.
/// Only child directories named for a pane scope ([`PANE_DIR`]) are touched; job and turn scopes are not this sweep's.
fn end_panes_under(tree: &dyn ScopeTree, root: &Path) -> Option<(String, bool)> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(PANE_DIR))
        })
        .collect();
    dirs.sort();
    if dirs.is_empty() {
        return None;
    }
    let mut ended = 0;
    let mut left = Vec::new();
    for path in dirs {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .trim_start_matches("session.")
            .to_string();
        let scope = ScopeId {
            kind: ScopeKind::Session,
            name,
            path,
        };
        let reaping = tree.end(&scope);
        if reaping.survivors.is_empty() {
            ended += 1;
        } else {
            left.push(reaping.summary());
        }
    }
    let mut said = format!(
        "panes a dead daemon left in {}: {ended} ended",
        root.display()
    );
    if !left.is_empty() {
        said.push_str(&format!(
            "; {} still running ({})",
            left.len(),
            left.join("; ")
        ));
    }
    Some((said, !left.is_empty()))
}

/// The rule, with its two facts handed in so it runs without a cgroup: whether a pid is live, and the tree that ends a scope.
/// Only a root whose owner pid is dead is swept. Answers the lines to keep for the next daemon and one sentence per root swept.
fn sweep_record(
    record: &Path,
    tree: &dyn ScopeTree,
    alive: &dyn Fn(u32) -> bool,
) -> (Vec<String>, Vec<String>) {
    let mut keep = Vec::new();
    let mut said = Vec::new();
    let text = std::fs::read_to_string(record).unwrap_or_default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let root = PathBuf::from(line);
        let Some(owner) = root
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(owner_pid)
        else {
            continue;
        };
        if owner == std::process::id() || alive(owner) {
            keep.push(line.to_string());
            continue;
        }
        match end_panes_under(tree, &root) {
            Some((sentence, false)) => said.push(sentence),
            // Something in it would not die, so the root STAYS recorded: the next start tries
            // again rather than forgetting a process nobody owns.
            Some((sentence, true)) => {
                said.push(sentence);
                keep.push(line.to_string());
            }
            None => {}
        }
    }
    (keep, said)
}

/// Rewrite the record. A record that cannot be written is a tree a successor will not find, so the
/// failure is said out loud - and is not fatal: the panes work either way.
fn write_record(record: &Path, lines: &[String]) {
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    if let Some(dir) = record.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        eprintln!("  panes: no record at {}: {e}", record.display());
        return;
    }
    if let Err(e) = std::fs::write(record, body) {
        eprintln!("  panes: no record at {}: {e}", record.display());
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

        // The cap goes on before the program does, as a job's does: a pane that ran even for a
        // moment uncapped is a pane whose memory was the box's to lose.
        if let Some(scope) = &scope {
            self.cap(scope, &cwd);
        }

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

    /// Every pane this session holds, ended and forgotten. A pane is that session's work, so its
    /// close takes it: a program drawing into a pane no head can open is one nobody can end.
    fn end_session(&self, session_id: &str) -> usize {
        let Some(session) = self.panes().remove(session_id) else {
            return 0;
        };
        let mut ended = 0;
        for (_, mut pane) in session.panes {
            // Counted before the kill and only when there was still a program: a pane whose
            // program had exited has nothing left to end, and counting it would report a kill
            // nobody made.
            if pane.session.live() {
                ended += 1;
            }
            pane.session.close(WITH_THE_SESSION);
        }
        ended
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

#[cfg(test)]
mod tests {
    //! The driver's decisions on a tree whose scopes are directories: a pane's scope, its cap, and what a session's end does.
    //! The kernel half belongs to `letibot_tools::exec::scope`.
    use std::sync::Mutex as StdMutex;
    use std::time::{Duration, Instant};

    use letibot_sessionlog::registry::SessionWiring;
    use letibot_tools::exec::memory::{Amount, PolicySource};
    use letibot_tools::exec::{ExecError, Migration, Reaping};

    use super::*;

    /// How long a killed program gets to be gone before a test calls it alive. Generous, and finite.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// A tree whose scopes are plain directories, holding the two files a pane's wrapper and a cap
    /// touch. `end` is the real mechanism's shape - read the members, signal them, wait, remove -
    /// so a pane's program really ends and the test is about this module and not about signals.
    struct Faked {
        root: PathBuf,
        ended: StdMutex<Vec<String>>,
        capped: StdMutex<Vec<(String, u64)>>,
        killed: StdMutex<Vec<u32>>,
    }

    impl Faked {
        fn new(tag: &str) -> Faked {
            let root = std::env::temp_dir().join(format!(
                "letibot-panes-{tag}-{}-{}",
                std::process::id(),
                letibot_sessionlog::registry::now_ms()
            ));
            std::fs::create_dir_all(&root).expect("a root for the fake tree");
            Faked {
                root,
                ended: StdMutex::new(Vec::new()),
                capped: StdMutex::new(Vec::new()),
                killed: StdMutex::new(Vec::new()),
            }
        }

        /// A scope's directory, as `open` names it.
        fn at(&self, scope: &str) -> PathBuf {
            self.root.join(scope)
        }

        fn ended(&self) -> Vec<String> {
            self.ended.lock().expect("ended").clone()
        }

        fn capped(&self) -> Vec<(String, u64)> {
            self.capped.lock().expect("capped").clone()
        }

        fn killed(&self) -> Vec<u32> {
            self.killed.lock().expect("killed").clone()
        }
    }

    impl ScopeTree for Faked {
        fn describe(&self) -> String {
            format!("directories at {}", self.root.display())
        }

        fn root(&self) -> Option<PathBuf> {
            Some(self.root.clone())
        }

        fn open(
            &self,
            kind: ScopeKind,
            name: &str,
            parent: Option<&ScopeId>,
        ) -> Result<ScopeId, ExecError> {
            let base = parent
                .map(|p| p.path.clone())
                .unwrap_or_else(|| self.root.clone());
            let path = base.join(format!("{}.{name}", kind.as_str()));
            std::fs::create_dir_all(&path).expect("a scope directory");
            std::fs::write(path.join("cgroup.procs"), "").expect("an empty cgroup.procs");
            Ok(ScopeId {
                kind,
                name: name.to_string(),
                path,
            })
        }

        fn members(&self, scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
            let text = std::fs::read_to_string(scope.path.join("cgroup.procs"))
                .map_err(|e| ExecError::NoSuchScope(format!("{}: {e}", scope.path.display())))?;
            Ok(text.lines().filter_map(|l| l.trim().parse().ok()).collect())
        }

        fn end(&self, scope: &ScopeId) -> Reaping {
            let pids = self.members(scope).unwrap_or_default();
            let mut signalled = Vec::new();
            for pid in &pids {
                // `-1` is every process this user may signal, and this process is not a pane: a
                // test stands a pid that may not be signalled in with its own, and it survives.
                if *pid <= 1 || *pid == std::process::id() {
                    continue;
                }
                // The pane's program leads its own group (`setsid`), so the group is what ends it,
                // exactly as `TermSession::close` does for a pane with no scope.
                unsafe {
                    libc::kill(-(*pid as i32), libc::SIGHUP);
                    libc::kill(*pid as i32, libc::SIGHUP);
                }
                signalled.push(*pid);
            }
            // A dead process leaves its scope, as the kernel takes it out of `cgroup.procs`: wait
            // for that and read the scope back rather than assuming it. Only what was signalled is
            // waited on: a pid this tree refuses to signal is a survivor by definition.
            let deadline = Instant::now() + PATIENCE;
            let mut left = signalled.clone();
            while !left.is_empty() && Instant::now() < deadline {
                left.retain(|p| owner_alive(*p));
                if !left.is_empty() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            let mut survivors: Vec<u32> = pids
                .iter()
                .copied()
                .filter(|p| !signalled.contains(p))
                .collect();
            survivors.extend(left);
            survivors.sort_unstable();
            let body: String = survivors.iter().map(|p| format!("{p}\n")).collect();
            let _ = std::fs::write(scope.path.join("cgroup.procs"), body);
            self.ended.lock().expect("ended").push(scope.to_string());
            self.killed.lock().expect("killed").extend(signalled);
            let removed = survivors.is_empty() && std::fs::remove_dir_all(&scope.path).is_ok();
            Reaping {
                scope: scope.clone(),
                at: std::time::SystemTime::now(),
                mechanism: "SIGHUP to the pane's group",
                observed: vec![],
                survivors,
                waited: Duration::ZERO,
                removed,
                note: None,
            }
        }

        fn migrate(&self, from: &ScopeId, to: &ScopeId) -> Result<Migration, ExecError> {
            Err(ExecError::NoScopes(format!(
                "a faked tree does not migrate {} to {}",
                from.path.display(),
                to.path.display()
            )))
        }

        fn list(&self) -> Vec<ScopeId> {
            Vec::new()
        }

        fn memory_caps(&self) -> Result<(), String> {
            Ok(())
        }

        fn cap_memory(&self, scope: &ScopeId, bytes: u64) -> Result<(), String> {
            std::fs::write(scope.path.join("memory.max"), bytes.to_string())
                .map_err(|e| e.to_string())?;
            self.capped
                .lock()
                .expect("capped")
                .push((scope.to_string(), bytes));
            Ok(())
        }
    }

    /// A registry with one session in it, and a driver whose tree is the fake's. The struct is
    /// built here rather than through `new` so no cgroup root is made for a tree this throws away.
    fn fixture(tag: &str) -> (Arc<Registry>, Terminals, Arc<Faked>, PathBuf) {
        let tree = Arc::new(Faked::new(tag));
        let workspace = tree.root.join("ws");
        std::fs::create_dir_all(&workspace).expect("a workspace");
        let registry = Registry::new();
        registry
            .create(
                "s-pane",
                "one",
                SessionWiring {
                    model: "qwen3-4b".into(),
                    dialect: "qwen".into(),
                    endpoint: "127.0.0.1:9".into(),
                    workspace: workspace.display().to_string(),
                },
            )
            .expect("the session registers");
        let terminals = Terminals {
            registry: Arc::downgrade(&registry),
            tree: tree.clone(),
            panes: Mutex::new(HashMap::new()),
            memory: None,
            said_uncapped: AtomicBool::new(false),
        };
        (registry, terminals, tree, workspace)
    }

    /// Whether a pid is gone, waited for. The half a scope's name cannot prove: the scope was
    /// ended AND the program in it died.
    fn gone(pid: u32) -> bool {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if !owner_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The property the close is for: a session's panes end with it, the report says how many, and
    /// the programs are gone rather than merely forgotten.
    #[test]
    fn closing_a_session_ends_its_panes_and_counts_them() {
        let (registry, terminals, tree, _ws) = fixture("close");
        let hub = registry.get("s-pane").expect("the hub");
        let one = terminals
            .open("s-pane", &hub, "sleep 30", 80, 24)
            .expect("a pane");
        let two = terminals
            .open("s-pane", &hub, "sleep 30", 80, 24)
            .expect("a second pane");
        assert_eq!(
            terminals.status("s-pane").len(),
            2,
            "two programs are running"
        );

        let ended = terminals.end_session("s-pane");
        assert_eq!(
            ended, 2,
            "both programs were still running when the session ended"
        );
        assert!(
            terminals.status("s-pane").is_empty(),
            "a session that ended holds no pane, so a later `!term` starts rather than attaching"
        );
        let mut scopes = tree.ended();
        scopes.sort();
        assert_eq!(
            scopes,
            vec![
                format!("session.term-s-pane-{one}"),
                format!("session.term-s-pane-{two}")
            ],
            "the tree must be told to end those two scopes and nothing else"
        );
        let killed = tree.killed();
        assert_eq!(killed.len(), 2, "one program per pane: {killed:?}");
        for pid in killed {
            assert!(
                gone(pid),
                "pid {pid} outlived the session that owned its pane"
            );
        }
        assert_eq!(
            terminals.end_session("s-pane"),
            0,
            "a session with no panes ends none, and a second close says nothing"
        );
    }

    /// The sentence the close's report carries - and the one a PARK has, because nothing is told
    /// there. `None` is reserved for the case that has nothing to say.
    #[test]
    fn the_close_report_says_what_a_session_took_with_it() {
        assert_eq!(crate::sessions::close_report(None, 0), None);
        assert_eq!(
            crate::sessions::close_report(Some(String::new()), 2).as_deref(),
            Some("2 pane(s) ended with it")
        );
        assert_eq!(
            crate::sessions::close_report(Some("its branch came home".into()), 1).as_deref(),
            Some("its branch came home · 1 pane(s) ended with it")
        );
        assert_eq!(
            crate::sessions::close_report(None, 1).as_deref(),
            Some("1 pane(s) ended with it"),
            "a session that was not open here can still have held a pane"
        );
        assert_eq!(
            crate::sessions::close_report(Some("a branch".into()), 0).as_deref(),
            Some("a branch"),
            "no panes, no sentence"
        );
    }

    /// A pane is capped by the policy a `bash` call in its session gets, and the number is on the
    /// scope before the program starts.
    #[test]
    fn a_pane_is_capped_by_the_policy_its_session_gets() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a memory controller this box can cap with (`/proc/meminfo`)",
            Meminfo::read().is_some(),
        ) else {
            return;
        };
        let (registry, terminals, tree, workspace) = fixture("cap");
        let policy = MemoryPolicy {
            default: Amount::Bytes(96 << 20),
            ceiling: Amount::Fraction(0.5),
            default_from: PolicySource::Project,
        };
        let terminals =
            terminals.with_memory(PaneMemory::new(policy, LeticodeConfig::default(), false));
        let hub = registry.get("s-pane").expect("the hub");
        let pane = terminals
            .open("s-pane", &hub, "sleep 5", 80, 24)
            .expect("a pane");

        let scope = format!("session.term-s-pane-{pane}");
        assert_eq!(
            tree.capped(),
            vec![(scope.clone(), 96 << 20)],
            "the pane's own scope is the one capped, at the policy's default"
        );
        let set = std::fs::read_to_string(tree.at(&scope).join("memory.max")).expect("memory.max");
        assert_eq!(
            set.trim(),
            (96 << 20).to_string(),
            "and the cap is the number the kernel would read"
        );
        assert_eq!(
            PaneMemory::new(policy, LeticodeConfig::default(), false).bytes(&workspace),
            Some(96 << 20),
            "a whole number needs no free memory to resolve"
        );
    }

    /// A daemon that starts after one that was killed finds what the dead one left: the pane scopes
    /// under a tree root whose OWNER PID is dead, and nothing else - not the live daemon's panes, not
    /// the job scopes beside them, and not a directory this format never wrote.
    #[test]
    fn a_start_ends_the_panes_a_dead_daemon_of_the_same_workspace_left() {
        let tree = Faked::new("sweep");
        let parent = tree.root.join("cgroups");
        let dead = parent.join("letibot.999999.0");
        let live = parent.join(format!("letibot.{}.0", std::process::id()));
        let stranger = parent.join("not-a-tree");
        let panes = |root: &Path, names: &[&str]| {
            for &name in names {
                std::fs::create_dir_all(root.join(name)).expect("a pane scope");
                std::fs::write(root.join(name).join("cgroup.procs"), "").expect("procs");
            }
        };
        panes(&dead, &["session.term-s1-1", "session.term-s1-2"]);
        panes(&live, &["session.term-s2-1"]);
        panes(&stranger, &["session.term-s3-1"]);
        // A job scope in the dead daemon's tree: the same tree, and not a pane.
        std::fs::create_dir_all(dead.join("session.s1")).expect("a job scope");
        // A second dead root whose one pane holds a pid this process may not signal - the stand-in
        // for a program that outlives the sweep. Its root has to stay recorded.
        let stubborn = parent.join("letibot.999998.0");
        std::fs::create_dir_all(stubborn.join("session.term-s4-1")).expect("a pane scope");
        std::fs::write(
            stubborn.join("session.term-s4-1").join("cgroup.procs"),
            format!("{}\n", std::process::id()),
        )
        .expect("procs");

        let record = tree.root.join("panes.scopes");
        std::fs::write(
            &record,
            format!(
                "{}\n{}\n{}\n{}\nnot a path at all\n",
                dead.display(),
                live.display(),
                stranger.display(),
                stubborn.display()
            ),
        )
        .expect("a record");

        let alive = |pid: u32| pid == std::process::id();
        let (keep, said) = sweep_record(&record, &tree, &alive);

        let mut scopes = tree.ended();
        scopes.sort();
        assert_eq!(
            scopes,
            vec![
                "session.term-s1-1",
                "session.term-s1-2",
                "session.term-s4-1"
            ],
            "only the dead daemons' pane scopes are ended"
        );
        assert!(
            !dead.join("session.term-s1-1").exists() && !dead.join("session.term-s1-2").exists(),
            "and they are gone from the tree"
        );
        assert!(
            stubborn.join("session.term-s4-1").exists(),
            "a pane whose program would not die keeps its scope, and its root keeps its line"
        );
        assert!(
            dead.join("session.s1").exists(),
            "a job scope in the same tree is not a pane, so the sweep leaves it"
        );
        assert!(
            live.join("session.term-s2-1").exists(),
            "a LIVE daemon's pane is its own, whatever workspace the record names"
        );
        assert!(
            stranger.join("session.term-s3-1").exists(),
            "a directory whose name names no owner pid is never swept"
        );
        assert_eq!(
            keep,
            vec![live.display().to_string(), stubborn.display().to_string()],
            "the live root is kept for the next start, and so is one it could not finish"
        );
        assert_eq!(said.len(), 2, "one sentence per root swept: {said:?}");
        assert!(
            said.iter()
                .any(|s| s.contains("2 ended") && s.contains("999999")),
            "the sentence names the root and what it ended: {said:?}"
        );
        assert!(
            said.iter().any(|s| s.contains("1 still running")),
            "and a pane that would not die is said rather than counted: {said:?}"
        );
    }

    /// And the daemon's own door: `begin` sweeps, then records THIS tree, so the next daemon can
    /// find this one's panes. A pid that is not alive is the whole of the licence to sweep it.
    #[test]
    fn a_start_records_this_tree_after_sweeping_a_dead_one() {
        let tree = Faked::new("begin");
        let mine = tree.root.display().to_string();
        let dead = tree.root.join("letibot.650000.0");
        std::fs::create_dir_all(dead.join("session.term-s1-1")).expect("a pane scope");
        std::fs::write(dead.join("session.term-s1-1").join("cgroup.procs"), "").expect("procs");
        let record = tree.root.join("records");
        std::fs::create_dir_all(&record).expect("a directory of records");
        let file = record.join("w.scopes");
        std::fs::write(&file, format!("{}\n", dead.display())).expect("a record");

        let registry = Registry::new();
        let terminals = Terminals {
            registry: Arc::downgrade(&registry),
            tree: Arc::new(tree),
            panes: Mutex::new(HashMap::new()),
            memory: None,
            said_uncapped: AtomicBool::new(false),
        };
        let said = terminals.begin(&file);
        assert_eq!(said.len(), 1, "the dead root is swept: {said:?}");
        let written = std::fs::read_to_string(&file).expect("the record it wrote");
        assert_eq!(
            written.trim(),
            mine,
            "the tree records itself, so the daemon after this one can find its panes"
        );
        assert!(
            !written.contains("650000"),
            "and a swept root is not recorded again"
        );
    }
}
