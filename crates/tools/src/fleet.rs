//! **Work on many hosts at once** — the engine under `fanout` and `scenario` (RH8).
//!
//! `docs/remote-hosts.md` § *The supervisor, and work across hosts* is the contract: *"one
//! supervised job per host, in parallel … the result is one row per host"*, and *"start roles in
//! order, wait each ready; then the run; on the run's end, any role failing, a ready deadline
//! passing, or an interrupt: tear everything down on every host"*.
//!
//! # Why this is not a loop over `ssh`
//!
//! The Stroppy session this package answers hand-rolled its own fleet runner — `fleet.sh`, 641
//! lines, rewritten several times — out of `ssh … &`, `ps -p` polls and log tails. Its own audit
//! found cells of the plan that never ran and were never mentioned, and that is what a hand-rolled
//! loop does: a host that never answered is a `for`-body that did not execute, and nothing counts
//! it.
//!
//! So three rules are structural here rather than remembered:
//!
//! 1. **Every leg is a job of a [`ProcessHost`]** — [`crate::remote::RemoteProcesses`] on a
//!    declared host, the session's own host for [`LOCAL`]. Not a shell pipeline this module
//!    threads by hand: the launch, the pid file, the exit file, the `setsid` process group and the
//!    group kill are RH1's code, which is the code a placement already runs.
//! 2. **Every planned cell ends with a [`Cell`]** — [`cells`] is what both tools answer with, and a
//!    leg with no result is a row saying so ([`Fate::Never`]). *Never a silent omission* is a
//!    count, not an intention.
//! 3. **Every started process is written down** ([`ProcessBook`]), so the daemon's supervisor —
//!    not this module — owns its deadline, its liveness and its teardown. A fanout branch is a
//!    `process` record like any background job: a restarted daemon finds it, and a session's close
//!    kills it on the host it runs on.
//!
//! # What a cell is allowed to say
//!
//! The three fates never collapse, for the reason [`crate::exec::Waited`] gives one layer down:
//! [`Fate::Ended`] is the command's own status; [`Fate::Stopped`] is *something ended it on
//! purpose* and names what; [`Fate::Never`] is *it did not run* and carries the reason — an
//! unknown host, a key that is not installed, a breaker that refuses. **An unreachable host is a
//! row, not a failure of the whole**, because a fanout that answers nothing when one of five
//! machines is off has thrown away four results.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_tokencore::store::ProcState;

use crate::exec::{JobId, JobState, JobView, ProcessHost, ScopeKind, SpawnRequest, Waited};
use crate::remote::{self, Hosts, Paths, Ssh};

/// **The name a host list may use for *the box this session's tools run on*.** Not a host in
/// `hosts.toml`: it is the session's own backend — where a `scenario`'s client usually wants to
/// run, and the one fanout cell that needs no ssh at all.
pub const LOCAL: &str = "local";

/// **What a branch is stopped by when its deadline passes** — the same words the supervisor's own
/// tick writes (`harnessd::supervise::expired_by`), so a row read after either says one thing.
pub const EXPIRED: &str = "its deadline";

/// **How much of a command's output a row carries.** A tail is what a table holds; the whole of it
/// is still in the host's job directory, and the row names that path ([`Cell::log`]).
pub const TAIL: usize = 1200;

/// **One command to run on one host** — a fanout branch, a scenario role, a scenario's run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leg {
    /// What the caller called it — a role's name, or the host's. A row is keyed by it.
    pub label: String,
    /// A `hosts.toml` name, or [`LOCAL`].
    pub host: String,
    pub command: String,
    /// Relative to the host's work directory, or absolute. `None` is that directory.
    pub cwd: Option<String>,
    /// When this leg must be gone. Decided by the caller **before** the start, so the record and
    /// the loop cannot disagree about it.
    pub deadline: Option<Instant>,
}

/// **What became of a leg.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fate {
    /// It ran and ended. `state` is the host's own answer, and `took` is start to settle.
    Ended { state: JobState, took: Duration },
    /// Something ended it on purpose — this module at its deadline, or a teardown — and `why`
    /// says which.
    Stopped { why: String, took: Duration },
    /// It never ran, and `why` is the sentence: no such host, a key that is not installed, a
    /// breaker that refuses, a backend that cannot start processes.
    Never { why: String },
}

impl Fate {
    /// The one word a row leads with — [`JobState::word`]'s own words, so a row and a jobs listing
    /// cannot say the same ending two ways.
    pub fn word(&self) -> String {
        match self {
            Fate::Ended { state, .. } => state.word(),
            Fate::Stopped { .. } => "stopped".to_string(),
            Fate::Never { .. } => "never ran".to_string(),
        }
    }

    /// Did this leg do what a leg is normally asking? **Exited 0, and nothing else** — a stopped
    /// leg and one that never ran are both not-yet-answers, and a kill is not a status.
    pub fn ok(&self) -> bool {
        matches!(
            self,
            Fate::Ended {
                state: JobState::Exited { code: 0 },
                ..
            }
        )
    }
}

/// **One planned cell's answer** — the row a fanout or a scenario prints, and what a test asserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub label: String,
    pub host: String,
    pub command: String,
    /// **How long until its readiness check first answered**, or `None` when nothing asked it to
    /// be ready (a fanout branch, a scenario's run) or it never was.
    pub ready_after: Option<Duration>,
    pub fate: Fate,
    /// The end of what it wrote, as the host gave it. Empty is a fact: *it wrote nothing*.
    pub tail: String,
    /// **Where the whole of its output is** — the remote job directory's `<id>.out`, on the host
    /// that ran it. `None` where the output is not a file this row can name.
    pub log: Option<String>,
}

impl Cell {
    /// A cell for a leg that was never started, so a plan and its answer are the same length.
    pub fn never(leg: &Leg, why: impl Into<String>) -> Cell {
        Cell {
            label: leg.label.clone(),
            host: leg.host.clone(),
            command: leg.command.clone(),
            ready_after: None,
            fate: Fate::Never { why: why.into() },
            tail: String::new(),
            log: None,
        }
    }

    /// The row, as one line: `server on lab2x1 · exited 0 · 2.1s · ready after 0.4s · tail: …`.
    pub fn line(&self) -> String {
        let mut s = if self.host == LOCAL {
            format!("{} (local)", self.label)
        } else {
            format!("{} on {}", self.label, self.host)
        };
        match &self.fate {
            Fate::Ended { state, took } => {
                s.push_str(&format!(" · {} · {:.1}s", state.word(), took.as_secs_f32()))
            }
            Fate::Stopped { why, took } => {
                s.push_str(&format!(" · stopped · {:.1}s · {why}", took.as_secs_f32()))
            }
            Fate::Never { why } => s.push_str(&format!(" · never ran — {why}")),
        }
        if let Some(r) = self.ready_after {
            s.push_str(&format!(" · ready after {:.1}s", r.as_secs_f32()));
        }
        if !self.tail.trim().is_empty() {
            s.push_str(&format!(" · tail: {}", one_line(&self.tail, 240)));
        }
        if let Some(log) = &self.log {
            s.push_str(&format!(" · output {log}"));
        }
        s
    }
}

/// A leg as this module holds it after a successful start.
#[derive(Debug, Clone)]
pub struct Running {
    pub leg: Leg,
    /// The handle the host's own job table knows it by.
    pub job: JobId,
    /// `None` for [`LOCAL`], the `hosts.toml` name otherwise.
    pub place: Option<String>,
    pub started: Instant,
    pub log: Option<String>,
}

/// **Where a started process is written down** — `letibot_tokencore::store::ProcessRecord`, the
/// daemon's supervisor's own table (schema 34).
///
/// A seam and not a `Store`: this crate is below the daemon and a tool has no store path. The
/// daemon registers a book bound to its own session
/// ([`crate::builtins::fanout::Fanout::with_book`]); everywhere else [`NoBook`] writes nothing,
/// which is the truth for a run with no store.
///
/// **Why it is written at all, rather than left to the job table**: a fanout branch is not a job of
/// the session's process host — it is a job of a host the session is not placed on — so nothing
/// else would write it down. A daemon that restarted mid-fanout would find no process to adopt,
/// and a session close would kill nothing on that machine.
pub trait ProcessBook: Send + Sync {
    /// A process has started, on `p.host`.
    fn started(&self, p: &ProcessNote);
    /// A process ended. `handle` is the job id this book was told about.
    fn ended(&self, handle: &str, state: ProcState, exit: Option<i32>);
}

/// What a book is told about a start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessNote {
    pub handle: String,
    /// `None` for this box, a `hosts.toml` name otherwise.
    pub host: Option<String>,
    pub pid: u32,
    /// **What makes a reused pid not this process** — `start:<n>` here, `cmd:<command>` there: the
    /// two spellings `crate::supervise` already reads.
    pub identity: String,
    pub command: String,
    pub cwd: String,
    pub scope: String,
    pub started_ms: i64,
    pub deadline_ms: Option<i64>,
    /// Where the whole of its output goes, when it goes to a file.
    pub log: Option<String>,
}

/// **A book that writes nothing** — a run with no store, and every test that does not ask for one.
/// Silent on purpose: this is not a failure, it is a fact about a process with no daemon behind it.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoBook;

impl ProcessBook for NoBook {
    fn started(&self, _p: &ProcessNote) {}
    fn ended(&self, _handle: &str, _state: ProcState, _exit: Option<i32>) {}
}

/// **A host's connection, or the refusal that says why it cannot be reached** — one entry per name,
/// cached, so a machine that refuses is asked once and not once per leg. A named alias rather than
/// the spelling twice, which is also what clippy asks for.
type Opened = Result<Arc<dyn ProcessHost>, String>;

/// **The hosts a fleet reaches, and the jobs it has started on them.**
pub struct Fleet {
    hosts: Hosts,
    paths: Paths,
    /// A test's fake `ssh`. `None` is the box's own.
    ssh: Option<PathBuf>,
    /// The bound on this module's own steps (a `mkdir`, a `pwd`), never on a command.
    bound: Duration,
    book: Arc<dyn ProcessBook>,
    /// **The session's own backend**, for [`LOCAL`]. `None` when it cannot start processes.
    local: Option<Arc<dyn ProcessHost>>,
    /// The directory this call works in, on every host — `<root>/<work>`. One per call, so two
    /// fanouts in one session cannot write over each other's job directories.
    work: String,
    /// One connection's worth of processes per host, opened once. `Err` is the refusal, cached so
    /// a machine that refuses is asked once and not once per leg.
    remote: Mutex<BTreeMap<String, Opened>>,
    /// Where each host's work directory is, absolute on that host.
    workdirs: Mutex<BTreeMap<String, String>>,
}

impl std::fmt::Debug for Fleet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fleet").field("work", &self.work).finish()
    }
}

impl Fleet {
    /// A fleet for one call.
    pub fn new(work: impl Into<String>) -> Fleet {
        Fleet {
            hosts: Hosts::load(),
            paths: Paths::from_env(),
            ssh: None,
            bound: remote::STEP_BOUND,
            book: Arc::new(NoBook),
            local: None,
            work: work.into(),
            remote: Mutex::new(BTreeMap::new()),
            workdirs: Mutex::new(BTreeMap::new()),
        }
    }

    /// Read the hosts from this file instead of the operator's — a test's door, and the one
    /// [`crate::builtins::hosts::HostsTool::with_file`] opens.
    pub fn with_file(mut self, file: impl AsRef<std::path::Path>) -> Fleet {
        self.hosts = Hosts::load_from(file);
        self
    }

    /// This ssh material instead of this box's.
    pub fn with_paths(mut self, paths: Paths) -> Fleet {
        self.paths = paths;
        self
    }

    /// Run this program instead of `ssh` — a test's fake, which never touches a network.
    pub fn with_ssh(mut self, program: impl Into<PathBuf>) -> Fleet {
        self.ssh = Some(program.into());
        self
    }

    /// Shorten the wall clock on this module's own steps. A fake ssh answers in milliseconds, and
    /// a test that measured [`remote::STEP_BOUND`] would be measuring a constant.
    pub fn with_bound(mut self, bound: Duration) -> Fleet {
        self.bound = bound;
        self
    }

    pub fn with_book(mut self, book: Arc<dyn ProcessBook>) -> Fleet {
        self.book = book;
        self
    }

    /// **The session's own backend**, for [`LOCAL`] legs. `None` is a session that cannot start
    /// processes, and then a `local` leg is a row saying so rather than a panic.
    pub fn with_local(mut self, local: Option<Arc<dyn ProcessHost>>) -> Fleet {
        self.local = local;
        self
    }

    pub fn work(&self) -> &str {
        &self.work
    }

    /// **The hosts this file declares** — what a refusal lists, so a name that is not there is
    /// answered with the names that are.
    pub fn declared(&self) -> Vec<String> {
        self.hosts.names()
    }

    /// **The host a leg runs on, or the sentence why it cannot.** One lookup, so a name is
    /// resolved the same way for a start, a readiness check and a kill.
    fn host(&self, name: &str) -> Result<Arc<dyn ProcessHost>, String> {
        if name == LOCAL {
            return self.local.clone().ok_or_else(|| {
                "`local` is this session's own backend, and it cannot start processes — so this \
                 leg would have had nowhere to run. A session whose backend is executable (a host \
                 session, a VM, a placed one) has one."
                    .to_string()
            });
        }
        let mut map = self.remote.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = map.get(name) {
            return hit.clone();
        }
        let opened = self.open(name);
        map.insert(name.to_string(), opened.clone());
        opened
    }

    /// Open one host: resolve it, make its work directory, then connect the process host there.
    ///
    /// The order is the one RH6 measured: **`mise exec -C` refuses a directory that does not
    /// exist**, so the work directory is made with a plain step *before* the connection is told to
    /// use it as its `-C`, and the toolchain wrapper is never pointed at a path that is not there.
    fn open(&self, name: &str) -> Result<Arc<dyn ProcessHost>, String> {
        let host = self
            .hosts
            .get(name)
            .ok_or_else(|| self.hosts.refusal(name))?
            .clone();
        let ssh = Ssh::connect(host.clone(), self.ssh.clone(), self.paths.clone())?
            .with_bound(self.bound);
        // **`root` is expanded on the far side, once** — `~` is that machine's home, and quoting
        // it would make a directory literally named `~` (RH1). `pwd -P` is the same answer
        // `SshBackend::place` uses, so a fanout's directory and a placement's checkout cannot
        // disagree about what `~/letibot-remote` means on that box.
        let (said, _, code) = ssh
            .sh_once(
                &format!("finding {name}'s root"),
                &format!(
                    "mkdir -p {r} && cd {r} && pwd -P",
                    // **The root is the one path that wants its `~` expanded THERE** — see
                    // [`remote::expand_root`]. A quoted `~` is a directory named `~`.
                    r = remote::expand_root(&host.root)
                ),
            )
            .map_err(|e| format!("`{name}` could not be reached: {e}"))?;
        if code != 0 {
            return Err(format!(
                "`{name}` would not give its root `{}` (exit {code}): {}",
                host.root,
                said.trim()
            ));
        }
        let root = said
            .lines()
            .last()
            .map(str::trim)
            .filter(|l| l.starts_with('/'))
            .ok_or_else(|| {
                format!(
                    "`{name}`'s root `{}` is not an absolute path there — it said `{}`",
                    host.root,
                    said.trim()
                )
            })?
            .to_string();
        let dir = format!("{root}/{}", self.work);
        let (_, _, made) = ssh
            .sh_once(
                &format!("making the work directory on {name}"),
                &format!("mkdir -p {}", shell(&dir)),
            )
            .map_err(|e| format!("`{name}` could not be reached: {e}"))?;
        if made != 0 {
            return Err(format!("`{name}` would not make `{dir}` (exit {made})"));
        }
        self.workdirs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), dir.clone());
        // **The toolchain's `-C` is the work directory**, not the home: a project pins its
        // versions in its own tree, and a command that ran without that tree's `mise.toml` would
        // see the wrong `cargo` — the failure RH6 measured on every one of the operator's hosts.
        let ssh = ssh.with_dir(dir.clone());
        Ok(Arc::new(remote::RemoteProcesses::new(
            ssh,
            name.to_string(),
            dir,
        )))
    }

    /// The directory a leg's command runs in, on the host that runs it.
    fn cwd_on(&self, name: &str, cwd: Option<&str>) -> String {
        let Some(cwd) = cwd.map(str::trim).filter(|c| !c.is_empty()) else {
            return String::new();
        };
        if cwd.starts_with('/') {
            return cwd.to_string();
        }
        match self
            .workdirs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            Some(dir) => format!("{dir}/{cwd}"),
            // `local`'s work directory is the backend's own root, and the empty string is how a
            // spawn request says *there* — so a relative `cwd` on the local box stays relative.
            None => cwd.to_string(),
        }
    }

    /// **Start one leg**, or say why it never ran.
    ///
    /// The process is a job of the host's own table (`background: true`), which is what gives it a
    /// pid, an exit file and a process group this module can end. It is then *written down*
    /// ([`ProcessBook`]) so the daemon's supervisor owns it from that moment: the record's deadline
    /// is the leg's, and a session's close kills it.
    pub fn start(&self, leg: &Leg) -> Result<Running, String> {
        let host = self.host(&leg.host)?;
        let req = SpawnRequest {
            command: leg.command.clone(),
            // **The name the caller gave it** — `server`, `db`, or the host's own name for a
            // fanout. Never a parse of the command: the caller named the cell.
            slug: Some(leg.label.clone()),
            cwd: self.cwd_on(&leg.host, leg.cwd.as_deref()),
            // **The session owns it, not the turn.** A scenario role or a branch that outlived
            // its turn is what `background: true` means, and a turn-scoped role would be killed by
            // the round's own end.
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: Vec::new(),
            tty: false,
            memory: None,
        };
        let started = Instant::now();
        let job = host
            .spawn(&req)
            .map_err(|e| format!("starting it on `{}`: {e}", leg.host))?;
        let view: Option<JobView> = host.job(&job);
        let place = if leg.host == LOCAL {
            None
        } else {
            Some(leg.host.clone())
        };
        let log = host.job_log(&job);
        if let Some(view) = &view {
            self.book.started(&ProcessNote {
                handle: job.0.clone(),
                host: place.clone(),
                pid: view.pid,
                identity: match place {
                    // **The two spellings the supervisor reads.** On a host, the far side answers
                    // *is this still the process* from the job directory's pid and the command's
                    // own words; here, the pid and `/proc`'s start time are the pair that makes a
                    // reused pid not this process.
                    Some(_) => format!("cmd:{}", leg.command),
                    None => match crate::exec::procs::read(view.pid) {
                        Some(p) => format!("start:{}", p.start),
                        None => String::new(),
                    },
                },
                command: leg.command.clone(),
                cwd: view.cwd.clone(),
                // **No scope name here**: the daemon's book knows which session owns this and
                // writes `session.<id>` itself. A name this crate invented would be a second
                // answer to a question only the daemon can answer.
                scope: String::new(),
                started_ms: unix_ms(),
                deadline_ms: leg
                    .deadline
                    .map(|d| unix_ms() + d.saturating_duration_since(started).as_millis() as i64),
                log: log.clone(),
            });
        }
        Ok(Running {
            leg: leg.clone(),
            job,
            place,
            started,
            log,
        })
    }

    /// **Has this leg settled?** — one status question, and never a verdict from silence.
    pub fn settled(&self, run: &Running) -> Option<(JobState, Duration)> {
        let host = self.host(&run.leg.host).ok()?;
        let view = host.job(&run.job)?;
        if view.state.is_running() {
            return None;
        }
        Some((view.state.clone(), run.started.elapsed()))
    }

    /// **Wait one leg out**, bounded. [`Waited::Deadline`] means *still running* and is never read
    /// as a finish — the caller decides what its own deadline does about that.
    pub fn wait(&self, run: &Running, within: Duration) -> Waited {
        match self.host(&run.leg.host) {
            Ok(host) => host
                .wait_job(&run.job, within)
                .unwrap_or(Waited::NeverStarted {
                    waited_for_presence: Duration::ZERO,
                }),
            Err(_) => Waited::NeverStarted {
                waited_for_presence: Duration::ZERO,
            },
        }
    }

    /// **End one leg on its own host** — the group, by the host's own rule. `by` is why, and it is
    /// what the record says.
    pub fn stop(&self, run: &Running, by: &str) {
        if let Ok(host) = self.host(&run.leg.host) {
            let _ = host.kill_job_as(&run.job, by);
        }
    }

    /// The end of a leg's output, as the host gave it. The row does the flattening.
    pub fn tail(&self, run: &Running) -> String {
        read_output(self, run, TAIL as u64)
    }

    /// **Write a leg's ending down** — the record's state, so the supervisor stops looking and the
    /// row and the store agree about how it ended.
    pub fn record_end(&self, run: &Running, fate: &Fate) {
        let (state, exit) = match fate {
            Fate::Ended {
                state: JobState::Exited { code },
                ..
            } => (ProcState::Done, Some(*code)),
            Fate::Ended {
                state: JobState::Signalled { .. } | JobState::Killed { .. },
                ..
            } => (ProcState::Stopped, None),
            Fate::Ended { .. } => (ProcState::Dead, None),
            Fate::Stopped { .. } => (ProcState::Stopped, None),
            Fate::Never { .. } => (ProcState::Dead, None),
        };
        self.book.ended(&run.job.0, state, exit);
    }

    /// **The cell one running leg answers with**, ready-time included.
    pub fn cell(&self, run: &Running, ready_after: Option<Duration>, fate: Fate) -> Cell {
        Cell {
            label: run.leg.label.clone(),
            host: run.leg.host.clone(),
            command: run.leg.command.clone(),
            ready_after,
            fate,
            tail: self.tail(run),
            log: run.log.clone(),
        }
    }

    /// **Poll one leg until it is ready, or the deadline passes.** Each check is asked **on the
    /// host that runs the role** — a port that accepts *there* is the fact a client on another
    /// machine is about to depend on, and asking this box's `127.0.0.1` would be answering about
    /// the wrong machine.
    pub fn await_ready(
        &self,
        run: &Running,
        check: &Ready,
        until: Instant,
    ) -> Result<Duration, String> {
        let began = Instant::now();
        let mut last = String::new();
        loop {
            match self.ask_ready(run, check) {
                Ok(true) => return Ok(began.elapsed()),
                Ok(false) => last.clear(),
                Err(e) => last = e,
            }
            if Instant::now() >= until {
                return match self.ask_ready(run, check) {
                    Ok(true) => Ok(began.elapsed()),
                    Ok(false) => Err(format!(
                        "its readiness check ({}) did not answer within {:.1}s — it never did",
                        check.describe(),
                        began.elapsed().as_secs_f32()
                    )),
                    Err(e) => Err(format!(
                        "its readiness check ({}) did not answer within {:.1}s — {e}",
                        check.describe(),
                        began.elapsed().as_secs_f32()
                    )),
                };
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// One readiness question, answered now.
    fn ask_ready(&self, run: &Running, check: &Ready) -> Result<bool, String> {
        // **A leg that has already died is not going to become ready**, and saying so now is what
        // turns a thirty-second wait into a sentence. A service whose start failed is the case.
        if let Some((state, _)) = self.settled(run) {
            return Err(format!(
                "the role ended before it was ready ({}) — it will not become ready",
                state.word()
            ));
        }
        match check {
            Ready::Port(port) => {
                if run.leg.host == LOCAL {
                    return Ok(crate::exec::host::port_is_listening(*port));
                }
                let host = self
                    .hosts
                    .get(&run.leg.host)
                    .ok_or_else(|| self.hosts.refusal(&run.leg.host))?
                    .clone();
                let ssh = Ssh::connect(host, self.ssh.clone(), self.paths.clone())?
                    .with_bound(self.bound);
                let script = format!(
                    "if (exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null; then exec 3>&-; echo yes; \
                     else echo no; fi"
                );
                let (said, _, _) = ssh
                    .sh_once(&format!("asking whether port {port} accepts"), &script)
                    .map_err(|e| e.to_string())?;
                Ok(said.trim() == "yes")
            }
            Ready::Log(pattern) => {
                let text = read_output(self, run, 64 * 1024);
                Ok(regex_hits(pattern, &text)?)
            }
            Ready::Check(command) => {
                // **One short job, on the role's host**, because a check is a command and the
                // host's own table is what runs commands: `pg_isready`, `curl -sf`, a `SELECT 1`.
                let leg = Leg {
                    label: format!("{}-ready", run.leg.label),
                    host: run.leg.host.clone(),
                    command: command.clone(),
                    cwd: run.leg.cwd.clone(),
                    deadline: Some(Instant::now() + Duration::from_secs(10)),
                };
                let probe = self.start(&leg)?;
                let fate = match self.wait(&probe, Duration::from_secs(10)) {
                    Waited::Happened {
                        state: Some(state), ..
                    } => Fate::Ended {
                        state,
                        took: probe.started.elapsed(),
                    },
                    _ => {
                        self.stop(&probe, "its readiness check took too long");
                        Fate::Stopped {
                            why: "its readiness check took too long".to_string(),
                            took: probe.started.elapsed(),
                        }
                    }
                };
                self.record_end(&probe, &fate);
                Ok(fate.ok())
            }
        }
    }
}

/// The last `bytes` of a leg's output, from whichever side it lives on.
fn read_output(fleet: &Fleet, run: &Running, bytes: u64) -> String {
    let Ok(host) = fleet.host(&run.leg.host) else {
        return String::new();
    };
    let Some(view) = host.job(&run.job) else {
        return String::new();
    };
    let from = view.produced.saturating_sub(bytes);
    host.output(&run.job, from, bytes as usize)
        .map(|s| s.text())
        .unwrap_or_default()
}

/// **How a role says it is ready.** Three spellings, because three are what a service fixture
/// actually has: a port that accepts, a line in its own log, and a client program that exits 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ready {
    Port(u16),
    Log(String),
    Check(String),
}

impl Ready {
    /// What the row calls it.
    pub fn describe(&self) -> String {
        match self {
            Ready::Port(p) => format!("port {p}"),
            Ready::Log(p) => format!("log /{p}/"),
            Ready::Check(c) => format!("check `{}`", one_line(c, 60)),
        }
    }

    /// The port this check is about, if it is about one — what `${role.port}` resolves to.
    pub fn port(&self) -> Option<u16> {
        match self {
            Ready::Port(p) => Some(*p),
            // **A log line that mentions a port is not a port this build knows.** A `log` check is
            // a pattern, and reading a number out of a role's own output would be guessing.
            _ => None,
        }
    }

    /// The check as the tool's arguments spell it, or the refusal naming the shape.
    pub fn parse(v: &serde_json::Value) -> Result<Ready, String> {
        let Some(obj) = v.as_object() else {
            return Err(format!(
                "a readiness check is an object with one of `port`, `log` or `check`, not `{v}`"
            ));
        };
        let mut found = Vec::new();
        if let Some(p) = obj.get("port") {
            let port = p
                .as_u64()
                .filter(|p| *p > 0 && *p < 65536)
                .ok_or_else(|| format!("`port` must be between 1 and 65535, not `{p}`"))?;
            found.push(Ready::Port(port as u16));
        }
        if let Some(l) = obj.get("log") {
            let pattern = l
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "`log` must be a non-empty regular expression".to_string())?;
            // **Compiled once, here**, so a pattern that does not compile is refused before a
            // single role is started rather than thirty seconds into a wait.
            regex::Regex::new(pattern)
                .map_err(|e| format!("`log` is not a regular expression: {e}"))?;
            found.push(Ready::Log(pattern.to_string()));
        }
        if let Some(c) = obj.get("check") {
            let command = c
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "`check` must be a command".to_string())?;
            found.push(Ready::Check(command.to_string()));
        }
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(
                "a readiness check names one of `port`, `log` or `check` — this one names none"
                    .to_string(),
            ),
            _ => Err(
                "a readiness check names ONE of `port`, `log` or `check`; this one names several, \
                 and a check that is three checks cannot say which one passed"
                    .to_string(),
            ),
        }
    }
}

/// Does `pattern` match anywhere in `text`?
fn regex_hits(pattern: &str, text: &str) -> Result<bool, String> {
    regex::Regex::new(pattern)
        .map(|re| re.is_match(text))
        .map_err(|e| format!("`{pattern}` is not a regular expression: {e}"))
}

/// **One shell word**, single-quoted with the `'\''` escape — a root is the operator's to spell and
/// half of what a path contains is quotes.
pub fn shell(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// A few lines of text on one line, cut — what a row can hold.
pub fn one_line(s: &str, max: usize) -> String {
    let flat: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let joined = flat.join(" ⏎ ");
    if joined.chars().count() <= max {
        return joined;
    }
    format!("{}…", joined.chars().take(max).collect::<String>())
}

pub fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// **A plan and its answer, the same length by construction.**
///
/// The Stroppy session's own fleet runner skipped cells — a host that never answered was a body
/// that never executed, and nothing counted it. So an answer here is never a list of *results*: it
/// is the plan, with a [`Cell`] for every leg, and a leg with no result is a row that says it never
/// ran. *Every planned cell is accounted for* as a type rather than as a habit — and a duplicate
/// label in the plan is a refusal one layer up, because two legs called `server` would be one cell
/// answering for two commands.
pub fn cells(plan: &[Leg], done: &[Cell]) -> Vec<Cell> {
    let mut out = Vec::with_capacity(plan.len());
    for leg in plan {
        match done.iter().find(|c| c.label == leg.label) {
            Some(cell) => out.push(cell.clone()),
            None => out.push(Cell::never(
                leg,
                "it was in the plan and no result reached the table — nothing was started for it",
            )),
        }
    }
    out
}
