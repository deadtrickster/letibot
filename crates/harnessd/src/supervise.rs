//! **The daemon's process supervisor** — RH7: one record per started process, one liveness check
//! per HOST per tick, deadlines enforced, teardown by ownership, and a restart that finds what is
//! still running.
//!
//! # Why this is not the job watcher
//!
//! [`crate::jobwatch`] is about ONE job and ONE session: a thread blocked in `wait_job`, which is
//! woken by the child the daemon itself spawned. That works exactly as long as the daemon is the
//! process that started the job. It answers nothing about a job on another machine (there is no
//! `Child` to wait on — there is a pid over there), and nothing at all after a restart (the thread
//! is gone, the `Child` is gone, the job is not).
//!
//! So the supervisor asks the questions that survive both, of the record rather than of a child
//! handle: **is the process with this pid still the process I recorded, on this host**, and
//! **what did it say it ended with**. The store row is the truth ([`letibot_tokencore::store::
//! ProcessRecord`]), which is what makes a restarted daemon able to pick the work back up.
//!
//! # One check per host per tick, and why the number is the HOST's
//!
//! A check is a round trip. Asking per job is how a machine with eight running builds spends eight
//! ssh connections a second to learn one fact, and it is the shape of a busy loop that looks like
//! supervision. [`Probe::check`] is therefore handed **every** open record of one host at once and
//! answers for all of them: locally that is a pass over `/proc`, remotely that is ONE script over
//! ONE connection that reports every job directory on the host.
//!
//! # The four things a check can answer, and the one rule they are read by
//!
//! | the answer | the state | why not the other |
//! |---|---|---|
//! | the same process is there | [`ProcState::Running`] | — |
//! | it exited, and a status was recorded | [`ProcState::Done`] | an exit code is the command's own answer |
//! | it is gone with no status | [`ProcState::Dead`] | *not* `done 0`: nothing said it succeeded |
//! | the host did not answer | [`ProcState::Lost`] | the job **may still run**; `dead` would be a claim nobody checked |
//!
//! `lost` is retried on the next tick and is not a verdict. `dead` is a verdict, and the only one
//! that is made by absence — which is why a job that was never seen on its HOST cannot reach it:
//! the host's own silence is `lost`.
//!
//! # Deadlines are wall clocks, and the kill is a group
//!
//! A deadline is stored as an absolute unix millisecond rather than as a duration, because the
//! thread that armed it may not be the thread that enforces it — a restart is exactly the case this
//! supervisor exists for. The kill is `TERM`, then `KILL` a second later, against the **process
//! group** when the record says the pid leads one: `kill -TERM -<pid>` on a pid that is not a
//! leader signals the caller's own group, which is a way to kill the daemon instead of the job.
//!
//! # Teardown follows OWNERSHIP, and what that means here
//!
//! * A **session closing** kills every open record that session owns, on every host.
//! * The **daemon stopping** is every session closing.
//! * A **turn ending is NOT a teardown**, and a job started to outlive its turn keeps running
//!   across turns — that is what `background: true` means, and a supervisor that killed at turn end
//!   would end every build the moment the model stopped waiting for it.
//! * An **interrupt** is not a teardown either: it ends the round, and the job it started is still
//!   the session's until the session goes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_tokencore::store::{Check, ProcState, ProcessRecord, Store};

/// **How often the daemon looks at every open process** — one second.
///
/// A check is cheap on the box that runs the job and one round trip on any other, and the number
/// this trades against is *how late can a deadline kill be*. A second late on a build's own limit is
/// nothing; a second between checks on a job that ended is the same second the settlement was
/// already going to take to reach the model.
pub const TICK: Duration = Duration::from_secs(1);

/// **One process, as a check is asked about it** — the fields a probe needs and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub handle: String,
    pub pid: u32,
    /// What makes a reused pid not this process. See the module header.
    pub identity: String,
    /// **Where this job's exit would be written down**, for a host whose jobs record one in a file
    /// (a remote host's job directory). `None` for a local job, whose exit is the kernel's.
    pub exit_path: Option<String>,
}

/// **What one check answered about one process.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// The same process, still there.
    Alive,
    /// It ended: `Done` with a status, or `Dead`/`Stopped` without one.
    Ended { state: ProcState, exit: Option<i32> },
}

/// **What one check of one host answered**, with its own clock and its own failure.
///
/// `error` is the whole host's, not one job's: a connection that did not answer says nothing about
/// any of the processes behind it, and reporting each of them individually would be the same
/// sentence eight times.
#[derive(Debug, Clone)]
pub struct Checked {
    /// A short word for how it was answered — `proc`, `ps`, `ssh`, `jobdir`.
    pub how: &'static str,
    pub took: Duration,
    /// By handle.
    pub answers: Vec<(String, Answer)>,
    pub error: Option<String>,
}

impl Checked {
    pub fn failed(how: &'static str, took: Duration, why: impl Into<String>) -> Checked {
        Checked {
            how,
            took,
            answers: Vec::new(),
            error: Some(why.into()),
        }
    }

    pub fn answer(&self, handle: &str) -> Option<Answer> {
        self.answers
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, a)| *a)
    }
}

/// **What the supervisor can ask of the world** — the seam, so a test is a state machine and not a
/// farm of sleeping processes.
///
/// One implementation serves every host: [`Probe::check`] is handed the host name and decides (this
/// box, or one script over one connection), because a caller that had to pick the implementation
/// would be a caller that knows how a host is reached — which is `rpc`'s business.
pub trait Probe: Send + Sync {
    /// **Every open process of ONE host, in one call.** See the module header for why this is the
    /// unit rather than the job.
    fn check(&self, host: Option<&str>, wants: &[Want]) -> Checked;

    /// End one process group. `by` is why, and it is what the record's reader is told.
    fn kill(&self, host: Option<&str>, rec: &ProcessRecord, by: &str) -> Result<(), String>;
}

/// **What one tick did**, for a test to assert and the daemon to log — because "it ran" and "it
/// found three processes and ended one" are different facts and only the second is a measurement.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tick {
    /// One per host that was checked.
    pub hosts: usize,
    /// Records a check found running (or still starting) — the transition to `running` counted here.
    pub running: usize,
    /// Records a check found ended, and the state it ended in.
    pub ended: Vec<(String, ProcState)>,
    /// Records whose deadline passed, whose group was killed.
    pub expired: Vec<String>,
    /// Records whose host could not be reached.
    pub lost: Vec<String>,
    /// **Checks and writes that could not be done**, said rather than dropped: a record nobody
    /// could update is a record the next tick checks again, which is the safe half — but a
    /// supervisor whose every write fails is doing nothing, and a tick that reported it as quiet
    /// would be the silence this tree keeps paying for.
    pub errors: Vec<String>,
}

/// **One host's group of open records**, as a tick hands them to a probe.
fn group(open: Vec<ProcessRecord>) -> BTreeMap<Option<String>, Vec<ProcessRecord>> {
    let mut out: BTreeMap<Option<String>, Vec<ProcessRecord>> = BTreeMap::new();
    for rec in open {
        out.entry(rec.host.clone()).or_default().push(rec);
    }
    out
}

/// **The supervisor itself** — the record, the probe, and the two verbs a tick needs.
///
/// Not a thread: [`Supervisor::tick`] is called by whatever loop owns the clock, which is what makes
/// a restart, a deadline and a teardown assertable without a sleep. The daemon's own tick is
/// `daemon.rs`.
pub struct Supervisor {
    /// **Behind a `Mutex`, and not a bare `Store`.** A `rusqlite::Connection` is `Send` and not
    /// `Sync`, so `Arc<Store>` is not `Send` either — and a daemon component that cannot be moved onto
    /// the thread that ticks it is a component the daemon cannot run. `Arc<Mutex<Store>>` is both, and
    /// the lock is uncontended: one supervisor, one tick, one thread.
    store: Arc<std::sync::Mutex<Store>>,
    probe: Arc<dyn Probe>,
    /// The clock, injected so a deadline is tested by moving the clock rather than by waiting.
    now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl Supervisor {
    pub fn new(store: Arc<std::sync::Mutex<Store>>, probe: Arc<dyn Probe>) -> Supervisor {
        Supervisor {
            store,
            probe,
            now_ms: Box::new(now_ms),
        }
    }

    /// A supervisor whose clock the test moves.
    pub fn with_clock(
        store: Arc<std::sync::Mutex<Store>>,
        probe: Arc<dyn Probe>,
        now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Supervisor {
        Supervisor {
            store,
            probe,
            now_ms,
        }
    }

    /// **Check every open process, once per host, and write down what was found.**
    ///
    /// The order inside one record is the rule's order: an answer first (is it there at all), then
    /// the deadline (it is there and it is out of time), and nothing else. A job that vanished is
    /// `dead` even if its deadline had also passed — killing a process that is not there is not
    /// what happened, and `stopped by its deadline` would be a false claim about a job that died on
    /// its own.
    pub fn tick(&self) -> Tick {
        let mut report = Tick::default();
        let open = match self
            .store
            .lock()
            .expect("the supervisor's store")
            .open_processes()
        {
            Ok(v) => v,
            Err(e) => {
                report.errors.push(format!("reading the open records: {e}"));
                return report;
            }
        };
        for (host, recs) in group(open) {
            report.hosts += 1;
            let wants: Vec<Want> = recs
                .iter()
                .map(|r| Want {
                    handle: r.handle.clone(),
                    pid: r.pid,
                    identity: r.identity.clone(),
                    exit_path: r.log_path.as_ref().and_then(|p| exit_path_of(p)),
                })
                .collect();
            let checked = self.probe.check(host.as_deref(), &wants);
            let at = (self.now_ms)();
            let took = checked.took.as_millis() as u64;
            for rec in recs {
                if let Some(err) = &checked.error {
                    // **The host said nothing, so nothing is claimed about the job.** `lost` and a
                    // retry: a breaker-open host is this arm, and so is a connection that dropped.
                    report.lost.push(rec.handle.clone());
                    self.write(
                        &mut report,
                        &rec,
                        Check::new(ProcState::Lost, at, took, checked.how),
                        Some(err.as_str()),
                    );
                    continue;
                }
                match checked.answer(&rec.handle) {
                    Some(Answer::Alive) => {
                        if let Some(deadline) = rec.deadline_ms
                            && at >= deadline
                        {
                            match self.probe.kill(host.as_deref(), &rec, expired_by()) {
                                Ok(()) => {
                                    report.expired.push(rec.handle.clone());
                                    report.ended.push((rec.handle.clone(), ProcState::Stopped));
                                    self.write(
                                        &mut report,
                                        &rec,
                                        Check::new(ProcState::Stopped, at, took, "expired"),
                                        None,
                                    );
                                }
                                Err(e) => {
                                    // **A kill that could not be delivered is not a kill.** The
                                    // record stays open and says what went wrong, so the next tick
                                    // tries again rather than reporting a job as ended while it
                                    // runs.
                                    report.lost.push(rec.handle.clone());
                                    self.write(
                                        &mut report,
                                        &rec,
                                        Check::new(ProcState::Lost, at, took, "expired"),
                                        Some(&format!("ending it: {e}")),
                                    );
                                }
                            }
                            continue;
                        }
                        report.running += 1;
                        self.write(
                            &mut report,
                            &rec,
                            Check::new(ProcState::Running, at, took, checked.how),
                            None,
                        );
                    }
                    Some(Answer::Ended { state, exit }) => {
                        report.ended.push((rec.handle.clone(), state));
                        let mut check = Check::new(state, at, took, checked.how);
                        if let Some(code) = exit {
                            check = check.with_exit(code);
                        }
                        self.write(&mut report, &rec, check, None);
                    }
                    // **A process this check did not report on is not a process that ended.** The
                    // probe's own silence about one handle is a bug in the probe or a host that
                    // answered a different list; either way the record is left as it was, and the
                    // next tick asks again.
                    None => {
                        report.lost.push(rec.handle.clone());
                        self.write(
                            &mut report,
                            &rec,
                            Check::new(ProcState::Lost, at, took, checked.how),
                            Some("the check did not report on it"),
                        );
                    }
                }
            }
        }
        report
    }

    /// Write one answer down, and say so rather than dropping it: a record nobody updated is a
    /// record the next tick checks again, which is the safe half — but a write that fails on every
    /// tick is a supervisor that is doing nothing and a log that would not say so.
    fn write(&self, report: &mut Tick, rec: &ProcessRecord, check: Check, why: Option<&str>) {
        if let Err(e) = self
            .store
            .lock()
            .expect("the supervisor's store")
            .check_process(&rec.session, &rec.handle, &check)
        {
            let said = match why {
                Some(w) => format!("{} ({}): {w}", rec.handle, e),
                None => format!("{}: {e}", rec.handle),
            };
            report.errors.push(said);
        }
    }

    /// **A session is going: end every process it owns, on every host.**
    ///
    /// What this is called for, and what it is not, is the module header's list. The one rule worth
    /// repeating at the call site: **a turn ending does not call this**. A job started to outlive its
    /// turn is the whole of `background: true`.
    ///
    /// A kill that could not be delivered leaves the record `lost` — the job may still run — rather
    /// than `stopped`, because a teardown that claims to have ended something it never reached is
    /// the failure this whole module is against.
    pub fn teardown_session(&self, session: &str, by: &str) -> Vec<ProcessRecord> {
        let open: Vec<ProcessRecord> = self
            .store
            .lock()
            .expect("the supervisor's store")
            .processes_of(session)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.state.is_open())
            .collect();
        let at = (self.now_ms)();
        for rec in &open {
            let written = match self.probe.kill(rec.host.as_deref(), rec, by) {
                Ok(()) => Check::new(ProcState::Stopped, at, 0, "teardown"),
                Err(_) => Check::new(ProcState::Lost, at, 0, "teardown"),
            };
            let _ = self
                .store
                .lock()
                .expect("the supervisor's store")
                .check_process(session, &rec.handle, &written);
        }
        open
    }

    /// **A daemon is starting: find out which of yesterday's processes are still there.**
    ///
    /// One [`Supervisor::tick`], and the records it leaves open are the ones a session has to be
    /// re-attached to — which is the caller's half, because re-arming a watcher is the harness's.
    /// The store is the whole of the input: nothing here remembers a `Child`, a thread or a
    /// connection, which is exactly why it works after a restart.
    pub fn recover(&self) -> Tick {
        self.tick()
    }
}

/// **The daemon's half of the tool crate's process book** — where a `fanout` branch or a
/// `scenario` role is written into the supervisor's table. See [`book`].
pub use book::StoreBook;

/// The exit file a remote job's log implies, or `None` for a local job.
///
/// A remote job's record keeps its output file — `<jobs>/<id>.out` — and the exit beside it is
/// `<jobs>/<id>.exit`, the job directory's own convention (`RemoteProcesses`). Reading it out of the
/// path rather than storing a second column keeps one field where the shape is already written down.
fn exit_path_of(log_path: &str) -> Option<String> {
    let stem = log_path.strip_suffix(".out")?;
    Some(format!("{stem}.exit"))
}

/// What [`Supervisor::tick`] records as the cause of a deadline kill. The same string the tools'
/// own deadline watchdog writes (`exec::jobs::DEADLINE_KILL`), so a row read after either says one
/// thing.
pub fn expired_by() -> &'static str {
    "its deadline"
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// **Which process has this pid, and is it the one we recorded?**
///
/// **There is one implementation of this answer and it is not this file's.**
/// `letibot_tools::exec::procs` reads a process from `/proc` on Linux and from `libproc` on macOS,
/// and its `start` field is *the* half of a pid's identity that survives reuse — measured, portable
/// and already carrying a `Z` (zombie) rule that a hand-rolled read gets wrong. So the supervisor's
/// notion of identity is that number, spelled `start:<n>`, and this function is the spelling.
pub fn identity_of(pid: u32) -> Option<String> {
    let p = letibot_tools::exec::procs::read(pid)?;
    Some(format!("start:{}", p.start))
}

/// **Is `pid` still the process whose identity is `identity`?**
///
/// `None` when the question cannot be asked of this identity at all — a remote record's
/// `cmd:<marker>` is answered on the far side by [`SshProbe`], never here, and a pid nobody can
/// read is not evidence either way.
pub fn alive_with_identity(pid: u32, identity: &str) -> Option<bool> {
    let start: u64 = identity.strip_prefix("start:")?.parse().ok()?;
    Some(letibot_tools::exec::procs::alive(pid, start))
}

/// **A pid is a number, and this is the box that can tell you which process has it** — the local
/// half of the probe seam.
///
/// One pass, no subprocess on Linux at all: a host with eight running builds is eight reads of
/// `/proc/<pid>/stat`, not eight forks.
#[derive(Debug, Default)]
pub struct LocalProbe;

impl LocalProbe {
    pub fn new() -> LocalProbe {
        LocalProbe
    }
}

impl Probe for LocalProbe {
    fn check(&self, host: Option<&str>, wants: &[Want]) -> Checked {
        let started = Instant::now();
        if host.is_some() {
            // Asked of the wrong probe: `check` on a name is the dispatcher's business, and a local
            // probe told about a remote host must not answer `gone` for every job on it.
            return Checked::failed(
                "local",
                started.elapsed(),
                format!(
                    "a check of `{}` was handed to the local probe",
                    host.unwrap_or_default()
                ),
            );
        }
        let mut answers = Vec::with_capacity(wants.len());
        for w in wants {
            let state = match alive_with_identity(w.pid, &w.identity) {
                // **The same process.** Not "a process with this pid".
                Some(true) => Answer::Alive,
                // The pid is somebody else's now, or nothing is there. Either way the job this
                // record is about is no longer running, and this box has no exit status to give:
                // the child that could have reported one died with the daemon.
                _ => Answer::Ended {
                    state: ProcState::Dead,
                    exit: None,
                },
            };
            answers.push((w.handle.clone(), state));
        }
        Checked {
            how: "proc",
            took: started.elapsed(),
            answers,
            error: None,
        }
    }

    /// **`TERM`, a second of grace, then `KILL`** — the group when the record says the pid leads
    /// one, the process alone otherwise. See the module header for why the group test is not
    /// optional.
    fn kill(&self, host: Option<&str>, rec: &ProcessRecord, by: &str) -> Result<(), String> {
        if host.is_some() {
            return Err(format!(
                "a kill of `{}` was handed to the local probe",
                host.unwrap_or_default()
            ));
        }
        let _ = by;
        signal(rec, "-TERM")?;
        let until = Instant::now() + Duration::from_secs(1);
        while Instant::now() < until {
            if alive_with_identity(rec.pid, &rec.identity) != Some(true) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        signal(rec, "-KILL")?;
        std::thread::sleep(Duration::from_millis(50));
        if alive_with_identity(rec.pid, &rec.identity) == Some(true) {
            return Err(format!(
                "pid {} is still there after TERM and KILL",
                rec.pid
            ));
        }
        Ok(())
    }
}

/// The one `kill` invocation, with the group rule.
fn signal(rec: &ProcessRecord, sig: &str) -> Result<(), String> {
    let target = match (rec.pgid, rec.pid) {
        // **A group only when the pid really leads it.** `kill -TERM -<pid>` on a pid that is not a
        // leader signals the caller's own group — the daemon's — which is a way to end the harness
        // instead of the job.
        (Some(pgid), pid) if pgid == pid && pgid != 0 => format!("-{pgid}"),
        _ => rec.pid.to_string(),
    };
    let out = std::process::Command::new("kill")
        .args([sig, &target])
        .output()
        .map_err(|e| format!("kill {sig} {target}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    // A kill that said `no such process` is a job that is already gone, which is what the caller
    // wanted; anything else is a refusal to report.
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if said.contains("No such process") || said.contains("no such process") {
        return Ok(());
    }
    Err(format!("kill {sig} {target}: {said}"))
}

/// **One script, one connection, every job of one host** — the remote half of the probe seam.
///
/// The runner is injected rather than built here: reaching a host is `rpc`'s business (the
/// `ControlMaster`, the deadlines, the breaker, the records), and a second spelling of ssh in this
/// file would be a second policy. What this owns is the SCRIPT — the one place that says how a job
/// directory answers *is it still running* and *what did it exit with*, and the thing a fake ssh in
/// a test runs for real against a temporary "remote root".
pub struct SshProbe {
    run: Run,
}

/// **How one host is reached** — `(host, script) -> (what it said, its exit status)`, or why it could
/// not be reached at all. A named alias so the field, the constructor and the doc say one thing.
pub type Run = Arc<dyn Fn(&str, &str) -> Result<(String, i32), String> + Send + Sync>;

impl SshProbe {
    pub fn new(run: Run) -> SshProbe {
        SshProbe { run }
    }

    /// **The script that answers for every job of one host**, in one pass.
    ///
    /// Per job: **the exit file first** (its existence IS the settlement, and a job whose pid has
    /// been reused must not be read as still running), then the process itself, matched by the
    /// command marker the record holds. `ps -p … -o command=` is the portable spelling on the
    /// operator's hosts (`/proc` is Linux-only and a Mac's `ps` answers a different flag set).
    pub fn script(wants: &[Want]) -> String {
        let mut s = String::from("set +e\n");
        for w in wants {
            s.push_str(&format!("printf '%s\\t' {}\n", shell_word(&w.handle)));
            match &w.exit_path {
                Some(exit) => {
                    let e = shell_word(exit);
                    s.push_str(&format!(
                        "if [ -s {e} ]; then printf 'exit\\t%s\\n' \"$(cat {e})\"\n"
                    ));
                }
                None => {
                    // A record with no exit file is not a job directory's; the pid is all there is.
                    s.push_str("if false; then :\n");
                }
            }
            s.push_str(&format!(
                "elif ps -p {pid} -o command= 2>/dev/null | grep -qF -- {marker}; then \
                 printf 'alive\\n'\nelse printf 'gone\\n'; fi\n",
                pid = w.pid,
                marker = shell_word(&marker_of(&w.identity)),
            ));
        }
        s
    }

    /// The answers out of the script's output. A line this build cannot read is skipped rather than
    /// guessed at — the handle simply has no answer, and the tick says so for that one record.
    pub fn parse(said: &str) -> Vec<(String, Answer)> {
        let mut out = Vec::new();
        for line in said.lines() {
            let mut f = line.split('\t');
            let (Some(handle), Some(word)) = (f.next(), f.next()) else {
                continue;
            };
            let handle = handle.trim();
            if handle.is_empty() {
                continue;
            }
            let answer = match word.trim() {
                "alive" => Answer::Alive,
                "gone" => Answer::Ended {
                    state: ProcState::Dead,
                    exit: None,
                },
                "exit" => {
                    let code = f.next().map(str::trim).and_then(|c| c.parse::<i32>().ok());
                    Answer::Ended {
                        state: ProcState::Done,
                        exit: code,
                    }
                }
                _ => continue,
            };
            out.push((handle.to_string(), answer));
        }
        out
    }
}

impl Probe for SshProbe {
    fn check(&self, host: Option<&str>, wants: &[Want]) -> Checked {
        let started = Instant::now();
        let Some(host) = host else {
            return Checked::failed(
                "ssh",
                started.elapsed(),
                "the ssh probe was handed a local job",
            );
        };
        if wants.is_empty() {
            return Checked {
                how: "ssh",
                took: started.elapsed(),
                answers: Vec::new(),
                error: None,
            };
        }
        match (self.run)(host, &SshProbe::script(wants)) {
            // **A non-zero exit is not an error here.** The script's own `ps`/`grep` fail on
            // purpose for a job that is gone, and the last of them decides the shell's status —
            // which is why the answers are read out of the output and never out of the status.
            Ok((said, _)) => Checked {
                how: "ssh",
                took: started.elapsed(),
                answers: SshProbe::parse(&said),
                error: None,
            },
            Err(e) => Checked::failed(
                "ssh",
                started.elapsed(),
                format!("`{host}` could not be reached: {e}"),
            ),
        }
    }

    fn kill(&self, host: Option<&str>, rec: &ProcessRecord, by: &str) -> Result<(), String> {
        let Some(host) = host else {
            return Err("the ssh probe was handed a local job".to_string());
        };
        let pid = rec.pid;
        let _ = by;
        // The same group rule as the local path, asked on the far side: `ps -o pgid=`.
        let script = format!(
            "g=$(ps -p {pid} -o pgid= 2>/dev/null | tr -d ' ')\n\
             if [ \"$g\" = \"{pid}\" ]; then kill -TERM -{pid}; else kill -TERM {pid}; fi\n\
             sleep 1\n\
             if ps -p {pid} -o command= >/dev/null 2>&1; then \
             if [ \"$g\" = \"{pid}\" ]; then kill -KILL -{pid}; else kill -KILL {pid}; fi; fi\n\
             echo done\n"
        );
        match (self.run)(host, &script) {
            // The script says the kill was attempted; whether the process is gone is the next
            // tick's question, and reading it here would be a second, slower check.
            Ok((_, 0)) => Ok(()),
            Ok((_, code)) => Err(format!("ending it on `{host}` exited {code}")),
            Err(e) => Err(format!("`{host}` could not be reached: {e}")),
        }
    }
}

/// **The marker inside a remote record's identity** — `cmd:<the command's own words>`, or the whole
/// string for a record written before the marker convention, which is the honest reading of a field
/// this build did not write.
fn marker_of(identity: &str) -> String {
    identity
        .strip_prefix("cmd:")
        .unwrap_or(identity)
        .to_string()
}

static SUPERVISOR: std::sync::OnceLock<Arc<Supervisor>> = std::sync::OnceLock::new();

/// **End every process a session owns, on every host** — what a session's close and the daemon's
/// own stop do, in one place, and `0` when this process has no supervisor (a one-shot run with no
/// store, a test).
///
/// A free function over a `static` rather than a field on `Sessions`: the audiences are far apart —
/// the tick thread sets it, a session's close and the registry's stop read it — and threading an
/// `Arc` through `Sessions`, `Harness` and `Config` would touch every constructor in every test to
/// carry one value that a single producer sets. `rpc`'s breakers are the tree's own precedent for
/// the shape, and a `OnceLock` says the thing that matters: **one supervisor per process**, so a
/// second `spawn` cannot become a second tick over the same records.
pub fn teardown_session(session: &str, by: &str) -> usize {
    match SUPERVISOR.get() {
        Some(sup) => sup.teardown_session(session, by).len(),
        None => 0,
    }
}

/// The supervisor this process has, if it has one — read by a test and by nothing in production.
pub fn running() -> Option<Arc<Supervisor>> {
    SUPERVISOR.get().cloned()
}

/// One shell word, single-quoted, with the `'\''` escape — a command is arbitrary text and half of
/// what it contains is quotes.
fn shell_word(s: &str) -> String {
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

/// **The probe a daemon runs: this box's processes here, a host's over the one ssh policy the tree
/// has.**
///
/// The dispatcher exists because [`Probe::check`] is asked once per HOST and the answer decides
/// which of two entirely different mechanisms is used — a pass over `/proc` here, one script over
/// one connection there. A caller that had to choose would be a caller that knows how a host is
/// reached, which is `letibot_tools::remote`'s business and not the supervisor's.
pub struct HostProbe {
    local: LocalProbe,
    /// One connection per host, opened on first use and cached: `Ssh::connect` is where the identity
    /// file and the accepted host key are REFUSED, and paying that per tick would be paying a
    /// refusal that has not changed.
    ssh: std::sync::Mutex<std::collections::HashMap<String, Arc<SshProbe>>>,
}

impl HostProbe {
    pub fn new() -> HostProbe {
        HostProbe {
            local: LocalProbe,
            ssh: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// The ssh probe for one host, or the refusal that says why this machine cannot be reached —
    /// which the tick turns into `lost` for every job of it, rather than into eight dead ones.
    fn for_host(&self, host: &str) -> Result<Arc<SshProbe>, String> {
        if let Some(p) = self.ssh.lock().expect("ssh probes").get(host) {
            return Ok(Arc::clone(p));
        }
        let hosts = letibot_tools::remote::Hosts::load();
        let named = hosts
            .get(host)
            .ok_or_else(|| hosts.refusal(host))
            .cloned()?;
        let ssh = letibot_tools::remote::Ssh::connect(
            named,
            None,
            letibot_tools::remote::Paths::from_env(),
        )?;
        let probe = Arc::new(SshProbe::new(Arc::new(
            move |_host: &str, script: &str| match ssh.sh_once("checking a job", script) {
                Ok((said, _, code)) => Ok((said, code)),
                Err(e) => Err(e.to_string()),
            },
        )));
        self.ssh
            .lock()
            .expect("ssh probes")
            .insert(host.to_string(), Arc::clone(&probe));
        Ok(probe)
    }
}

impl Default for HostProbe {
    fn default() -> HostProbe {
        HostProbe::new()
    }
}

impl Probe for HostProbe {
    fn check(&self, host: Option<&str>, wants: &[Want]) -> Checked {
        match host {
            None => self.local.check(None, wants),
            Some(h) => match self.for_host(h) {
                Ok(probe) => probe.check(Some(h), wants),
                Err(why) => Checked::failed("ssh", Duration::ZERO, why),
            },
        }
    }

    fn kill(&self, host: Option<&str>, rec: &ProcessRecord, by: &str) -> Result<(), String> {
        match host {
            None => self.local.kill(None, rec, by),
            Some(h) => self.for_host(h)?.kill(Some(h), rec, by),
        }
    }
}

/// **Start the supervisor as the daemon's own thread** — after [`crate::vm_reap::spawn`] and for
/// its reason: its own connection, its own clock, and no session's work waiting on it.
///
/// **The first tick IS the recovery.** A daemon that starts reads every open record the previous
/// one left (`Store::open_processes`), checks the hosts once, and writes down what it found — so a
/// job still running is `running` again, one that vanished while the daemon was down is `dead`,
/// and a host that cannot be reached is `lost` and tried again a second later. Nothing here
/// remembers a `Child` or a thread, which is exactly why it works after a restart.
///
/// The thread stops when the registry does: a closed registry is a daemon that has been told to
/// go, and a check that outlived it would be a check whose result nobody would read.
pub fn spawn(store_path: Option<std::path::PathBuf>, registry: Arc<letibot_sessionlog::Registry>) {
    let Some(path) = store_path else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("supervisor".into())
        .spawn(move || {
            let Ok(store) = Store::open(&path) else {
                eprintln!("  supervisor: the store did not open; no process is supervised");
                return;
            };
            let sup = Arc::new(Supervisor::new(
                Arc::new(std::sync::Mutex::new(store)),
                Arc::new(HostProbe::new()),
            ));
            let _ = SUPERVISOR.set(Arc::clone(&sup));
            loop {
                let tick = sup.tick();
                // **Said only when something happened**: a tick that found eight jobs still running
                // is silence, and a log that said so every second would bury the one line that
                // matters.
                if !tick.ended.is_empty()
                    || !tick.expired.is_empty()
                    || !tick.lost.is_empty()
                    || !tick.errors.is_empty()
                {
                    eprintln!(
                        "  supervisor: {} host(s), {} running, ended {:?}, expired {:?}, lost {:?}{}",
                        tick.hosts,
                        tick.running,
                        tick.ended,
                        tick.expired,
                        tick.lost,
                        if tick.errors.is_empty() {
                            String::new()
                        } else {
                            format!(", errors {:?}", tick.errors)
                        }
                    );
                }
                let until = Instant::now() + TICK;
                while Instant::now() < until {
                    if registry.is_stopping() || registry.is_closed() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
}

#[cfg(test)]
mod tests;

/// **Where a fanout's and a scenario's processes are written down** — the daemon's half of
/// `letibot_tools::fleet::ProcessBook`. A module of this file's own because it is the same table
/// and the same supervisor, one writer further out.
mod book;
