//! **The CPU clock a test uses to measure a CHILD rather than the box.**
//!
//! A sampled percentage (`ps`, `top`, a delta of the whole machine) is a measurement of the
//! machine: on a box running six other daemons and two hundred test binaries it says the box is
//! busy, which nobody doubted. What a test of *this* daemon needs is the CPU the daemon spent,
//! and that is one number — the process's own `utime + stime` over an interval.
//!
//! # The two readings, and why this module takes the syscall
//!
//! `/proc/<pid>/stat`'s fields 14 and 15 ARE that number, and `getrusage(RUSAGE_SELF)`'s
//! `ru_utime + ru_stime` is the same two fields for the calling process. [`this_process`] takes
//! the syscall — one call, no `/proc` walk, no descriptor on a box where descriptors are exactly
//! the resource the tree keeps running out of — and [`procs_reading`] reads the `/proc` side so a
//! test can prove the two agree (`crates/bridge/tests/idle_clock.rs` does, to the 10 ms tick).
//!
//! Neither reading includes a child process's CPU: `RUSAGE_SELF` is this process and its threads,
//! which is exactly right when the daemon under test is in the test's own process
//! (`Daemon::serve`, as `no_spin.rs` and `end_to_end.rs` build it).
//!
//! # Why the interval is the second half of the instrument
//!
//! A rate is a total divided by a window, so the WINDOW decides what the number means. The defect
//! [`wait_until_idle`] exists for was measured on 2026-11-01: a daemon's opening is a fixed
//! ~260 ms of CPU, spent over 0.6 s on a quiet box and 4.7 s on a loaded one — so a 3 s window
//! opened half a second after the start read 1.3 ms of "idle" on the first and 178 ms on the
//! second, against a 300 ms budget. The window was measuring how fast the box got through the
//! opening. A window that opens when the process has *gone quiet* measures the daemon.
#![allow(dead_code)]

use std::time::{Duration, Instant};

/// This process's CPU time so far, user and system together — `getrusage(RUSAGE_SELF)`.
pub fn this_process() -> Duration {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `getrusage` writes into a `rusage` this frame owns, and `RUSAGE_SELF` cannot fail
    // for a live process.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    assert_eq!(rc, 0, "getrusage(RUSAGE_SELF)");
    let secs = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    secs(ru.ru_utime) + secs(ru.ru_stime)
}

/// **Any process's own `utime + stime`, read off `/proc/<pid>/stat`** — the reading the brief for
/// this work named, and the one a test uses to prove a *neighbour* was really burning.
///
/// `None` where there is no `/proc` (macOS) or the pid is gone, so a caller can say the reading
/// is not available on this box instead of failing on it. The fields are 1-based in the file and
/// 0-based here: field 14 is `utime`, field 15 `stime`.
pub fn of_pid(pid: u32) -> Option<Duration> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` (field 2) is parenthesised and may itself contain spaces and parentheses, so the
    // fields are counted from AFTER the closing parenthesis — the standard way to parse this file.
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    // After `)` the first field is 3 (`state`), so 14 and 15 are the 12th and 13th here.
    let ticks: u64 = fields.nth(11)?.parse().ok()?;
    let ticks = ticks + fields.next()?.parse::<u64>().ok()?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hz = if hz > 0 { hz as u64 } else { 100 };
    Some(Duration::from_micros(ticks.saturating_mul(1_000_000) / hz))
}

/// The same quantity for THIS process, read the other way — so a test can show the two agree.
pub fn procs_reading() -> Option<Duration> {
    of_pid(std::process::id())
}

/// **The stretch a settle adds up** — one second, added up as a TOTAL rather than as a run of
/// shorter slices. That is the whole of the design and it was measured: a bursty tail that spends
/// 22 ms a second puts a few quarter-second slices under 5 ms each, so *two quiet slices in a row*
/// was satisfied while the work went on — while 22 ms is 22 ms wherever the bursts land, so a
/// second's total cannot be fooled that way. [`settle_budget`] carries the two rejected cuts and
/// the numbers they read.
///
/// **It is not a slice.** A settle is one reading over this span and there is no shorter unit in
/// it: `wait_until_idle` calls [`over`] once and compares. Two things follow, and both are
/// somebody's wall clock — every call to a settle costs at least this long, and the window a test
/// opens after it is `no_spin.rs`'s 3 s on top of that, which is why that file's `WINDOW` doc
/// counts the settles and not only the windows.
pub const SPAN: Duration = Duration::from_secs(1);

/// **The settle's line: 1% of a core** — [`SPAN`] / 100, i.e. 10 ms of CPU in a second.
///
/// Two numbers decide it, and both were wrong in a first cut (MEASURED 2026-11-01):
///
/// * **It must be low enough that the rate it admits cannot breach the budget of the window that
///   follows.** The settle is not the assertion — it only says *start measuring here* — so anything
///   it lets through is added to the window. At 10% of a core (the window's own line) a daemon whose
///   opening is being spent slowly sits *just under* the settle and just over the window: measured
///   under four hundred busy processes, a **4 ms lull** in a 250 ms slice ended the settle and the
///   window then read **144 ms against a 300 ms budget**. At 2% the same thing happened more subtly
///   — a settle that ended after 5.5 s left **68 ms** in the window. At 1%, over a full second, the
///   most a settle can contribute to a 3 s window is 30 ms, a tenth of that budget.
/// * **It must be high enough that a genuinely waiting daemon is under it by a wide margin.**
///   Measured: **0.19 ms in a 250 ms slice** on a quiet box — a slice being the unit of the design
///   that was rejected above, kept because that is how the number was taken — and the six 3 s
///   windows of `no_spin.rs` read **0.76–3.72 ms** (0.03–0.12% of a core) across the four runs on
///   record: 2.02–2.38 ms on a quiet box, 0.76–2.12 ms under four hundred busy processes on the
///   gate's box, and 1.87–3.72 ms on a 32-core box at load 28 with another workspace's test
///   running beside it. That is the accept loop's 20 ms poll and nothing else: at the loudest of
///   them, two and a half times under this line and eighty times under the window's budget.
///
/// And the span is why this is a total and not a run of slices — see [`SPAN`] for the tail that
/// made two quiet slices in a row a lie.
pub fn settle_budget() -> Duration {
    SPAN / 100
}

/// **Wait until this process has nothing left to do, and say how long that took.**
///
/// The condition is the thing a test of idleness is about — *the daemon has finished what it was
/// doing* — and it is read off the child's own accounting, so a quiet box and a box running
/// ninety other processes settle at the same point: the same fixed work, done. How long that
/// takes is the box's business, which is exactly why it is waited for rather than assumed.
///
/// One [`SPAN`] under [`settle_budget`], and see that function for the two ways a shorter or
/// looser condition was measured failing.
///
/// The ceiling is [`CEILING`] and only a real hang (or a real spin) trips it — and a spin trips it
/// with the words for what it is, because a process that never spends under 1% of a core over a
/// second is not settling.
pub const CEILING: Duration = Duration::from_secs(120);

pub fn wait_until_idle(what: &str) -> Duration {
    wait_until_idle_within(what, CEILING)
}

/// The same settle with the ceiling handed in, so a test can prove **the ceiling fires** without
/// spending [`CEILING`] on it — the failure path is the one that has to work, because a daemon
/// that spins never settles and this is the assertion that has to say so.
pub fn wait_until_idle_within(what: &str, ceiling: Duration) -> Duration {
    let started = Instant::now();
    loop {
        let last = over(SPAN);
        if last < settle_budget() {
            return started.elapsed();
        }
        assert!(
            started.elapsed() < ceiling,
            "{what}: this process has not been quiet for one {SPAN:?} in {ceiling:?} — the last \
             one cost {last:?} of CPU (the line is {:?}). A daemon with nothing to do blocks; one \
             that keeps spending a share of a core is a thread looping rather than waiting, and no \
             window opened later would say anything different.",
            settle_budget()
        );
    }
}

/// **How much CPU this process spends over `span`** — the one reading every window here is built
/// from.
pub fn over(span: Duration) -> Duration {
    let before = this_process();
    std::thread::sleep(span);
    this_process().saturating_sub(before)
}
