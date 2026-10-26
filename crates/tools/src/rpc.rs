//! **A remote is an RPC, and the client follows the practices** — rules 5 to 8 of
//! `docs/remote-hosts.md`.
//!
//! The operator, 2026-10-26: *"the tooling shouldnt hang, that is important"* — *"like it is
//! classic RPC architecture right"* — *"so we should follow practicies"*. Rules 1–4 (a deadline on
//! every call, failure detection, retries that follow idempotency, an unknown outcome said as
//! unknown) were built by package HT and live in [`crate::remote`] beside the ssh policy they are
//! about. This module is the other four, and they are four answers to one question: **what does
//! the client do about a machine that is slow, down, refusing or busy?**
//!
//! * **A host that is down is not dialled.** [`before`] and [`after`] keep a per-host circuit
//!   breaker: three connection failures in a row inside a minute open it for
//!   [`BREAKER_COOLDOWN`], and every call to that host then fails at once with the reason and the
//!   time it was last reached, instead of each waiting out its own timeout. One call after the
//!   cooldown is let through as a **probe**; if it answers, the breaker closes.
//! * **Every call is recorded.** [`record`] appends to a bounded ring — host, step, duration,
//!   outcome — which [`report`] renders for `hosts {probe: true}` and which is printed, one line
//!   per call and carrying no payload, when `$LETIBOT_RPC_DEBUG` is set. A slow or flaky host is
//!   then visible rather than guessed at.
//! * **A host takes a bounded number of sessions.** [`enter`] is a per-host gate: at most
//!   [`MAX_SESSIONS_PER_HOST`] ssh invocations are in flight, and a call beyond that **queues
//!   under its own deadline** rather than opening an unbounded number of connections.
//! * **One bounded runner for everybody.** [`run_bounded`] is the single place a local process
//!   this client starts for its own purposes is given a wall clock, so the backend, the `hosts`
//!   probe and `letibot --migrate` cannot drift apart on what "bounded" means.
//!
//! # Why the state is process-wide
//!
//! The contract's rule 5 says the breaker is *"shared by every caller in the process (the
//! backend, hosts, migrate)"*, and that is not a convenience: the three callers reach the same
//! machine over the same `ControlMaster`, so three private breakers would mean a host that is
//! down costs each caller its own three timeouts — which is the cost the breaker exists to
//! remove. Everything here is therefore a `static`, keyed by the host's name.
//!
//! The consequence for a test is worth stating plainly: **a test that opens a breaker opens it
//! for the whole test binary**, so each test here uses a host name of its own rather than a reset
//! door. A reset door would be a switch production does not have, and a suite that needed it
//! would be a suite whose tests were sharing state by accident.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use std::os::unix::process::CommandExt;

// ---------------------------------------------------------------------------
// Rule 5 — the circuit breaker
// ---------------------------------------------------------------------------

/// **How many connection failures in a row open the breaker.**
///
/// Three, because of what one failure already costs: a step retries once (rule 3), so a single
/// step against a dead host is two failed attempts and two timeouts. Waiting for two *steps* to
/// fail would mean a session sat through four timeouts before anything decided the host was down,
/// which is exactly the complaint. Three attempts is also the point at which "it was unlucky" has
/// stopped being the likelier story — a machine that misses three dials in a minute is down, not
/// slow.
pub const BREAKER_THRESHOLD: u32 = 3;

/// **The window the failures must fall inside to count as consecutive.**
///
/// A minute. "Consecutive" alone is not enough: a host that failed once at nine and once at noon
/// has not been flaky, and a streak that never expires would open the breaker on three unrelated
/// bad minutes in a day. A minute is also comfortably longer than the failures of one hung turn
/// take to accumulate (three attempts at [`crate::remote::STEP_BOUND`] is under 60 s), so the
/// case the breaker exists for — a machine that goes away mid-session — always accumulates.
pub const BREAKER_WINDOW: Duration = Duration::from_secs(60);

/// **How long an open breaker refuses before it lets one probe through.**
///
/// Thirty seconds, and the two bounds it sits between are the argument. It must be long enough
/// that the calls it refuses are actually saved a timeout (a shorter window would just stagger
/// the same waits), and short enough that a machine which rebooted in ten seconds is usable again
/// without the operator restarting a daemon. `ControlPersist` is 120 s and `ServerAliveInterval`
/// with `ServerAliveCountMax` gives up on a dead link in about 15 s, so 30 s is the shortest
/// window in which "did it come back" is a real question rather than a race.
pub const BREAKER_COOLDOWN: Duration = Duration::from_secs(30);

/// What one call is admitted as, before it is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    /// **The one call let through after a cooldown.** It is the probe that may close the breaker,
    /// and it is the reason a closed breaker is not a decision nobody revisits.
    pub probe: bool,
}

/// What a host's breaker and its record of the last good moment say at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    Closed,
    /// **Refusing**, with the sentence a caller is handed and the facts behind it.
    Open {
        until: Instant,
        failures: u32,
        last_reached: Option<Instant>,
        opened: Instant,
    },
}

#[derive(Debug, Default)]
struct Health {
    /// Connection failures since the last successful call — reset by success, and restarted when
    /// the previous failure is older than [`BREAKER_WINDOW`].
    streak: u32,
    last_failure: Option<Instant>,
    open_until: Option<Instant>,
    opened: Option<Instant>,
    /// The last moment this host answered **anything**, including a refusal: a host that says
    /// `Permission denied` is up, and saying "last reached never" about it would be false.
    last_reached: Option<Instant>,
    /// A probe is in flight. Exactly one call at a time is let through after a cooldown: if every
    /// queued call were let through, the breaker would open the floodgates at the moment it knows
    /// least.
    probing: bool,
}

static HEALTH: LazyLock<Mutex<BTreeMap<String, Health>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn health_lock() -> std::sync::MutexGuard<'static, BTreeMap<String, Health>> {
    HEALTH.lock().unwrap_or_else(|e| e.into_inner())
}

/// **Is this host worth dialling right now?** — rule 5's question, asked before every call.
///
/// `Err` is the sentence a caller is refused with, and it says the three things a person needs:
/// why the host is unavailable, when it was last reached, and when letibot will try it again. A
/// conversation that retried and then said only "ssh failed" is what this replaces.
pub fn before(host: &str) -> Result<Pass, String> {
    let mut map = health_lock();
    let h = map.entry(host.to_string()).or_default();
    match h.open_until {
        None => Ok(Pass { probe: false }),
        Some(until) if Instant::now() < until => Err(refusal(host, h, until)),
        Some(_) => {
            if h.probing {
                // One probe at a time: the call that was let through is still asking.
                return Err(format!(
                    "{host} is being re-probed after its cooldown — the one call that was let \
                     through has not answered yet, so this one fails at once rather than joining \
                     a second attempt at a host that was unavailable a moment ago."
                ));
            }
            h.probing = true;
            Ok(Pass { probe: true })
        }
    }
}

/// The sentence an open breaker refuses with.
fn refusal(host: &str, h: &Health, until: Instant) -> String {
    let left = until.saturating_duration_since(Instant::now());
    let failures = h.streak;
    let reached = match h.last_reached {
        Some(t) => format!(
            "{} s ago",
            Instant::now().saturating_duration_since(t).as_secs()
        ),
        None => "not once since this process started".to_string(),
    };
    let since = match h.opened {
        Some(t) => format!(
            "{} s ago",
            Instant::now().saturating_duration_since(t).as_secs()
        ),
        None => "a moment ago".to_string(),
    };
    format!(
        "{host} is unavailable: {failures} connection failures in a row, the breaker opened \
         {since}, and it was last reached {reached}. Every call to it fails at once for another \
         {left} s rather than each waiting out its own timeout; the next call after that is a \
         probe, and one that answers closes this. Nothing was sent and nothing on {host} changed.",
        left = left.as_secs() + 1
    )
}

/// **Record how a call that was admitted ended** — and the breaker's whole state machine.
///
/// `connected` is *the far side said something*, which includes a refusal: `Permission denied`
/// is a machine that is up and will not have us, and counting it as a connection failure would
/// open the breaker on a host whose only problem is a key somebody has not installed.
pub fn after(host: &str, pass: Pass, connected: bool) {
    let now = Instant::now();
    let mut map = health_lock();
    let h = map.entry(host.to_string()).or_default();
    if connected {
        h.streak = 0;
        h.last_failure = None;
        h.last_reached = Some(now);
        h.open_until = None;
        h.opened = None;
        h.probing = false;
        return;
    }
    h.probing = false;
    if pass.probe {
        // The probe failed: back to refusing, with a fresh cooldown. The streak is whatever it
        // was — the failures that opened it have not been retracted by one more.
        h.streak = h.streak.max(1);
        h.open_until = Some(now + BREAKER_COOLDOWN);
        h.opened = Some(now);
        return;
    }
    let fresh = match h.last_failure {
        Some(t) => t.elapsed() <= BREAKER_WINDOW,
        None => false,
    };
    h.streak = if fresh { h.streak + 1 } else { 1 };
    h.last_failure = Some(now);
    if h.streak >= BREAKER_THRESHOLD && h.open_until.is_none() {
        h.open_until = Some(now + BREAKER_COOLDOWN);
        h.opened = Some(now);
    }
}

/// What the breaker says about one host, for a report or a test.
pub fn standing(host: &str) -> Standing {
    let map = health_lock();
    match map.get(host) {
        Some(h) => match h.open_until {
            Some(until) if Instant::now() < until => Standing::Open {
                until,
                failures: h.streak,
                last_reached: h.last_reached,
                opened: h.opened.unwrap_or(until),
            },
            _ => Standing::Closed,
        },
        None => Standing::Closed,
    }
}

// ---------------------------------------------------------------------------
// Rule 8 — back-pressure
// ---------------------------------------------------------------------------

/// **How many ssh invocations this process will have in flight to one host.**
///
/// Eight, and the number is OpenSSH's rather than ours: `MaxSessions` defaults to 10 per
/// connection, which is what a `ControlMaster` is, and the two this leaves are the master's own
/// channel and one `-O check`. Going to 10 exactly would have the *server* refuse the eleventh
/// with `Session open refused`, which reads like a fault on the host rather than a queue we
/// forgot to keep — so the client keeps a queue, and the server never has to.
pub const MAX_SESSIONS_PER_HOST: usize = 8;

struct Gate {
    in_use: Mutex<usize>,
    freed: Condvar,
}

static GATES: LazyLock<Mutex<BTreeMap<String, Arc<Gate>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn gate(host: &str) -> Arc<Gate> {
    let mut map = GATES.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(host.to_string())
        .or_insert_with(|| {
            Arc::new(Gate {
                in_use: Mutex::new(0),
                freed: Condvar::new(),
            })
        })
        .clone()
}

/// **A place in a host's queue, held for as long as the call it was taken for.**
///
/// Dropping it is what lets the next call through, so the only way to leak a slot is to leak the
/// value — which a `?` inside the call would do, hence this is a guard rather than a pair of
/// functions a caller has to remember to pair.
pub struct Entered {
    host: String,
    gate: Arc<Gate>,
}

impl std::fmt::Debug for Entered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entered").field("host", &self.host).finish()
    }
}

impl Drop for Entered {
    fn drop(&mut self) {
        let mut n = self.gate.in_use.lock().unwrap_or_else(|e| e.into_inner());
        *n = n.saturating_sub(1);
        drop(n);
        self.gate.freed.notify_one();
    }
}

impl Entered {
    pub fn host(&self) -> &str {
        &self.host
    }
}

/// **Rule 8: wait for a slot, but never past the call's own deadline.**
///
/// The queue is under the caller's clock and not beside it: a call that could not get a slot
/// inside `within` is refused with a sentence that says so, because a queue that outlives the
/// deadline it was queued under is the hang coming back through the door marked "back-pressure".
pub fn enter(host: &str, within: Duration) -> Result<Entered, String> {
    let gate = gate(host);
    let began = Instant::now();
    let mut n = gate.in_use.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if *n < MAX_SESSIONS_PER_HOST {
            *n += 1;
            // The guard is dropped before the slot is handed over: a `MutexGuard` held inside the
            // value that is returned would be the lock being kept for the whole call.
            drop(n);
            return Ok(Entered {
                host: host.to_string(),
                gate,
            });
        }
        let left = within.saturating_sub(began.elapsed());
        if left.is_zero() {
            return Err(busy(host, within));
        }
        let (guard, timeout) = gate
            .freed
            .wait_timeout(n, left)
            .unwrap_or_else(|e| e.into_inner());
        n = guard;
        if timeout.timed_out() && *n >= MAX_SESSIONS_PER_HOST {
            return Err(busy(host, within));
        }
    }
}

fn busy(host: &str, within: Duration) -> String {
    format!(
        "{host} already has {MAX_SESSIONS_PER_HOST} ssh calls in flight from this process — the \
         bound is a session below OpenSSH's own `MaxSessions` of 10 — and this one waited {} s for \
         a slot without getting one. Nothing was sent. The calls already queued hold their own \
         deadlines, so they will not hold this one: ask again once the host has answers to give, \
         or place less work on it at once.",
        within.as_secs_f32().max(0.0)
    )
}

// ---------------------------------------------------------------------------
// Rule 7 — the call records
// ---------------------------------------------------------------------------

/// **What one call came to**, in the closed vocabulary the contract names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It answered — including an answer we did not like (a non-zero exit, a refusal of the file
    /// asked for). The transport worked.
    Ok,
    /// The far side would not have us: `Permission denied (publickey)`, a host key that does not
    /// verify. Definite, and never retried.
    Refused,
    /// The wall clock on the local ssh fired.
    TimedOut,
    /// ssh failed before the command could say anything — a connection that went away.
    Dropped,
    /// Nobody knows: the local process could not even be started.
    Unknown,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Refused => "refused",
            Outcome::TimedOut => "timed_out",
            Outcome::Dropped => "dropped",
            Outcome::Unknown => "unknown",
        }
    }
}

/// One call, as it is remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRecord {
    pub host: String,
    /// Which step asked — *reading a path*, *a probe of lab2x1*, *this command ran*. A name, never
    /// the command: a record is read by a head and printed to a log, and neither is a place to
    /// put a payload.
    pub step: String,
    pub millis: u128,
    pub outcome: Outcome,
    pub at: SystemTime,
}

/// How many records are kept. A ring rather than a counter, because the question this answers is
/// *what has it been doing lately*, and a total would answer a different one at unbounded cost.
pub const RECORD_RING: usize = 256;

static RING: LazyLock<Mutex<VecDeque<CallRecord>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(RECORD_RING)));

/// **Rule 7: record one call**, and — when `$LETIBOT_RPC_DEBUG` is set — print its line.
///
/// The line goes to stderr, which is the daemon's log, at what the contract calls *debug-ish*:
/// one line per call, no payload, and off unless somebody asked. A remote session that is slow
/// then has a record instead of a feeling.
pub fn record(host: &str, step: &str, took: Duration, outcome: Outcome) {
    let record = CallRecord {
        host: host.to_string(),
        step: step.to_string(),
        millis: took.as_millis(),
        outcome,
        at: SystemTime::now(),
    };
    {
        let mut ring = RING.lock().unwrap_or_else(|e| e.into_inner());
        if ring.len() == RECORD_RING {
            ring.pop_front();
        }
        ring.push_back(record.clone());
    }
    if std::env::var_os("LETIBOT_RPC_DEBUG").is_some() {
        eprintln!(
            "rpc: {host} {} {} ms {}",
            record.step,
            record.millis,
            record.outcome.as_str()
        );
    }
}

/// The newest records, newest first.
pub fn recent(host: &str, n: usize) -> Vec<CallRecord> {
    let ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    ring.iter()
        .rev()
        .filter(|r| r.host == host)
        .take(n)
        .cloned()
        .collect()
}

/// The slowest records for a host, slowest first.
pub fn slowest(host: &str, n: usize) -> Vec<CallRecord> {
    let ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    let mut mine: Vec<CallRecord> = ring.iter().filter(|r| r.host == host).cloned().collect();
    mine.sort_by_key(|r| std::cmp::Reverse(r.millis));
    mine.truncate(n);
    mine
}

/// Every record, newest first — the daemon's own view, and a test's.
pub fn all(n: usize) -> Vec<CallRecord> {
    let ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    ring.iter().rev().take(n).cloned().collect()
}

/// **What a head should see about one host's calls** — the last few, and the slowest.
///
/// One line each, because this is a row in `hosts {probe: true}` beside the load average: a model
/// deciding where to place work needs *flaky or fine*, not a trace.
pub fn report(host: &str) -> String {
    let last = recent(host, 4);
    if last.is_empty() {
        return "  calls:    none from this process yet\n".to_string();
    }
    let slow = slowest(host, 3);
    let mut line = String::from("  calls:    last ");
    line.push_str(
        &last
            .iter()
            .map(|r| format!("{} {} ms {}", r.step, r.millis, r.outcome.as_str()))
            .collect::<Vec<_>>()
            .join(" · "),
    );
    line.push_str("\n            slowest ");
    line.push_str(
        &slow
            .iter()
            .map(|r| format!("{} {} ms", r.step, r.millis))
            .collect::<Vec<_>>()
            .join(" · "),
    );
    line.push('\n');
    line
}

// ---------------------------------------------------------------------------
// One bounded runner
// ---------------------------------------------------------------------------

/// **What a caller may do when a call does not answer at all** — the retry rule, as a value a
/// second caller can carry.
///
/// The same rule the backend follows (rule 3): an operation that cannot have changed anything is
/// asked once more, and one that may have run is never asked twice. It lives in this module as
/// well as in [`crate::remote`] because `letibot --migrate` is the second caller of an ssh call
/// and its steps are spelled by its own module — a retry rule spelled twice is a retry rule that
/// drifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retried {
    /// Asking again is the same as asking once.
    Once,
    /// Never asked twice: the far side may have acted, partly acted, or be acting still.
    Never,
}

/// **How one bounded local process ended.**
pub enum Ended {
    /// It finished, whichever way: the exit status is the answer's business, not the clock's.
    Answered(std::process::Output),
    /// The wall clock passed and everything the process started was killed.
    TimedOut,
    /// It could not be started at all.
    Unstartable(String),
}

/// **Run a program to completion, or kill everything it started at the deadline.**
///
/// The one place a local process this client starts **for its own purposes** — never a model's
/// command, which keeps the tool's own deadline — is given a wall clock. Both streams are drained
/// on threads of their own: a pipe holds a buffer's worth and no more, so a child that fills it
/// blocks for ever and the wait below would never see it finish.
///
/// **The whole process group is killed, not the child.** A killed child leaves whatever it
/// spawned holding the pipes it inherited, and this side's readers then block for as long as that
/// grandchild lives — measured once as a 2 s bound costing 120 s, because a fake `ssh` that ran
/// `sleep 30` made the sleeps outlive every kill.
pub fn run_bounded(program: &Path, args: &[String], within: Duration) -> Ended {
    let mut child = match std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Ended::Unstartable(format!("running `{}`: {e}", program.display())),
    };
    let pid = child.id();
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let began = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if began.elapsed() >= within => {
                kill_group(pid);
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                kill_group(pid);
                let _ = child.kill();
                let _ = child.wait();
                let _ = (out.join(), err.join());
                return Ended::Unstartable(format!("waiting for `{}`: {e}", program.display()));
            }
        }
    };
    let Some(status) = status else {
        // The readers belong to a group that is now dead, so they end on their own; a timed-out
        // call has no use for what it did not finish reading.
        let _ = (out, err);
        return Ended::TimedOut;
    };
    Ended::Answered(std::process::Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

/// Drain a pipe on its own thread, so a child that writes more than a buffer does not block.
fn drain<R: std::io::Read + Send + 'static>(
    mut pipe: Option<R>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = pipe.as_mut() {
            let _ = std::io::Read::read_to_end(p, &mut buf);
        }
        buf
    })
}

/// **End everything one call started**, by the process group it leads.
///
/// `kill` rather than a libc call because this crate has no `libc` dependency, and a `/bin/kill`
/// that is missing is a box with worse problems than this one.
fn kill_group(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique(tag: &str) -> String {
        format!(
            "{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )
    }

    /// **Three failures in a row open it; a success closes it.**
    #[test]
    fn three_failures_open_the_breaker_and_one_answer_closes_it() {
        let host = unique("opens");
        assert_eq!(before(&host), Ok(Pass { probe: false }));
        for _ in 0..2 {
            let pass = before(&host).expect("still dialled");
            after(&host, pass, false);
            assert_eq!(
                standing(&host),
                Standing::Closed,
                "two is under the threshold"
            );
        }
        let pass = before(&host).expect("the third attempt is made");
        after(&host, pass, false);
        let Standing::Open { failures, .. } = standing(&host) else {
            panic!("three connection failures in a row open the breaker");
        };
        assert_eq!(failures, 3);

        // And the refusal says the three things a person needs.
        let said = before(&host).expect_err("an open breaker refuses");
        assert!(said.contains(&host), "{said}");
        assert!(said.contains("3 connection failures"), "{said}");
        assert!(said.contains("last reached"), "{said}");
        assert!(said.contains("probe"), "{said}");

        // A connected call closes it, which is the half that keeps a recovered host usable.
        after(&host, Pass { probe: false }, true);
        assert_eq!(standing(&host), Standing::Closed);
    }

    /// **A refusal from the far side is not a connection failure.** `Permission denied` is a
    /// machine that is up; counting it would open the breaker on a key nobody installed.
    #[test]
    fn a_definite_refusal_never_opens_the_breaker() {
        let host = unique("refused");
        for _ in 0..5 {
            let pass = before(&host).expect("a refusal does not refuse");
            after(&host, pass, true);
        }
        assert_eq!(standing(&host), Standing::Closed);
    }

    /// **The cooldown lets exactly one probe through, and a failed probe re-opens.**
    #[test]
    fn the_cooldown_lets_one_probe_through() {
        let host = unique("probe");
        for _ in 0..BREAKER_THRESHOLD {
            let pass = before(&host).expect("dialled");
            after(&host, pass, false);
        }
        assert!(matches!(standing(&host), Standing::Open { .. }));

        // Move the clock: the breaker's own record is brought forward by hand, because a test
        // that slept out a 30 s cooldown would be a test nobody runs.
        {
            let mut map = health_lock();
            let h = map.get_mut(&host).expect("it is there");
            h.open_until = Some(Instant::now() - Duration::from_millis(1));
        }
        let probe = before(&host).expect("one probe is let through");
        assert!(
            probe.probe,
            "the first call after the cooldown is the probe"
        );
        // A second one is refused while the first is still asking.
        assert!(before(&host).is_err(), "one probe at a time");

        after(&host, probe, false);
        let Standing::Open { failures, .. } = standing(&host) else {
            panic!("a probe that failed re-opens the breaker");
        };
        assert!(failures >= BREAKER_THRESHOLD);
    }

    /// **Failures spread wider than the window are not consecutive.**
    #[test]
    fn failures_outside_the_window_start_a_new_streak() {
        let host = unique("window");
        for _ in 0..2 {
            let pass = before(&host).expect("dialled");
            after(&host, pass, false);
        }
        {
            let mut map = health_lock();
            let h = map.get_mut(&host).expect("it is there");
            // The last failure is older than the window, so the next one starts a new streak —
            // one, not three, and the breaker stays closed.
            h.last_failure = Some(Instant::now() - BREAKER_WINDOW - Duration::from_secs(1));
        }
        let pass = before(&host).expect("dialled");
        after(&host, pass, false);
        assert_eq!(
            standing(&host),
            Standing::Closed,
            "a failure an hour apart is not a flaky host"
        );
    }

    /// **The gate lets `MAX_SESSIONS_PER_HOST` calls in and queues the next one**, which is
    /// refused at its own deadline rather than waiting unbounded.
    #[test]
    fn the_gate_queues_under_the_callers_deadline() {
        let host = unique("gate");
        let held: Vec<Entered> = (0..MAX_SESSIONS_PER_HOST)
            .map(|_| enter(&host, Duration::from_secs(1)).expect("a slot"))
            .collect();
        let began = Instant::now();
        let refused = enter(&host, Duration::from_millis(150))
            .expect_err("the bound is reached, so this one queues");
        assert!(
            began.elapsed() >= Duration::from_millis(140),
            "it waited its own deadline before refusing: {:?}",
            began.elapsed()
        );
        assert!(refused.contains(&host), "{refused}");
        assert!(refused.contains("Nothing was sent"), "{refused}");
        // A slot freed is a slot given: the next call lands at once.
        drop(held);
        let _back = enter(&host, Duration::from_millis(200)).expect("the queue moved");
    }

    /// **The records are per host, newest first, and the slowest are sorted by time.**
    #[test]
    fn records_are_kept_per_host_and_the_slowest_is_the_slowest() {
        let host = unique("records");
        let other = unique("other");
        record(&host, "a read", Duration::from_millis(120), Outcome::Ok);
        record(&host, "a probe", Duration::from_millis(8), Outcome::Ok);
        record(
            &host,
            "this command ran",
            Duration::from_secs(3),
            Outcome::TimedOut,
        );
        record(&other, "a read", Duration::from_millis(1), Outcome::Ok);

        let last = recent(&host, 2);
        assert_eq!(last[0].step, "this command ran", "newest first");
        assert_eq!(last[1].step, "a probe");
        assert_eq!(last[0].outcome, Outcome::TimedOut);
        assert_eq!(last[0].millis, 3000);
        assert_eq!(
            recent(&other, 5).len(),
            1,
            "another host's records are not here"
        );

        let slow = slowest(&host, 2);
        assert_eq!(slow[0].step, "this command ran");
        assert_eq!(slow[1].step, "a read");

        let said = report(&host);
        assert!(
            said.contains("last this command ran 3000 ms timed_out"),
            "{said}"
        );
        assert!(said.contains("slowest"), "{said}");
    }

    /// **The bounded runner kills the group, not just the child.** A `sh -c 'sleep 30'` under a
    /// 300 ms clock comes back as `TimedOut` when it is due, not when the sleep ends.
    #[test]
    fn the_bounded_runner_ends_at_its_clock() {
        let began = Instant::now();
        let ended = run_bounded(
            Path::new("sh"),
            &["-c".to_string(), "sleep 30 & wait".to_string()],
            Duration::from_millis(300),
        );
        assert!(matches!(ended, Ended::TimedOut));
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "the group was killed with the child: {:?}",
            began.elapsed()
        );
    }
}
