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
    store: Arc<Store>,
    probe: Arc<dyn Probe>,
    /// The clock, injected so a deadline is tested by moving the clock rather than by waiting.
    now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl Supervisor {
    pub fn new(store: Arc<Store>, probe: Arc<dyn Probe>) -> Supervisor {
        Supervisor {
            store,
            probe,
            now_ms: Box::new(now_ms),
        }
    }

    /// A supervisor whose clock the test moves.
    pub fn with_clock(
        store: Arc<Store>,
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
        let open = match self.store.open_processes() {
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
        if let Err(e) = self.store.check_process(&rec.session, &rec.handle, &check) {
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
            .processes_of(session)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.state.is_open())
            .collect();
        let at = (self.now_ms)();
        for rec in &open {
            match self.probe.kill(rec.host.as_deref(), rec, by) {
                Ok(()) => {
                    let _ = self.store.check_process(
                        session,
                        &rec.handle,
                        &Check::new(ProcState::Stopped, at, 0, "teardown"),
                    );
                }
                Err(_) => {
                    let _ = self.store.check_process(
                        session,
                        &rec.handle,
                        &Check::new(ProcState::Lost, at, 0, "teardown"),
                    );
                }
            }
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

/// **The exit file a remote job's log implies**, or `None` for a local job.
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// **Which process has this pid, and is it the one we recorded?**
///
/// Two implementations and one rule: the answer is about the PROCESS, not about the number. On Linux
/// that is `/proc/<pid>/stat`'s field 22 — the start time in clock ticks since boot, which no
/// reuse of the pid can reproduce. On macOS it is `ps -o lstart=`, which is the same fact at a lower
/// resolution (seconds), and a job started and replaced inside one second on a Mac is the one case
/// the two cannot tell apart — said here rather than discovered.
pub fn identity_of(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // **`comm` is in parentheses and may contain anything, including spaces and `)`.** So the
        // fields are read from after the LAST `)`, never by splitting the whole line — a command
        // named `(sh)` would otherwise shift every field by one and match a stranger's identity.
        let after = &stat[stat.rfind(')')? + 1..];
        let fields: Vec<&str> = after.split_whitespace().collect();
        // field 3 is `fields[0]` (state), so field 22 is `fields[19]`.
        let start = fields.get(19)?;
        Some(format!("starttime:{start}"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
            .ok()?;
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!said.is_empty()).then(|| format!("lstart:{said}"))
    }
}

/// **Is `pid` still the process whose identity is `identity`?**
///
/// `None` when the pid cannot be asked about at all — a pid nobody can introspect is not evidence
/// either way, and the caller turns that into `lost`.
pub fn alive_with_identity(pid: u32, identity: &str) -> Option<bool> {
    let now = identity_of(pid)?;
    Some(now == identity)
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
            let state = match identity_of(w.pid) {
                // **The same process.** Not "a process with this pid".
                Some(seen) if seen == w.identity => Answer::Alive,
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
            if identity_of(rec.pid).as_deref() != Some(rec.identity.as_str()) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        signal(rec, "-KILL")?;
        std::thread::sleep(Duration::from_millis(50));
        if identity_of(rec.pid).as_deref() == Some(rec.identity.as_str()) {
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
    run: Arc<dyn Fn(&str, &str) -> Result<(String, i32), String> + Send + Sync>,
}

impl SshProbe {
    pub fn new(
        run: Arc<dyn Fn(&str, &str) -> Result<(String, i32), String> + Send + Sync>,
    ) -> SshProbe {
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

#[cfg(test)]
mod tests;
