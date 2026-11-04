//! The CPU clock a test uses to measure a child rather than the box. A sampled percentage
//! measures the machine; this reads the daemon's own `utime + stime` (`/proc/<pid>/stat` fields
//! 14 and 15, `getrusage(RUSAGE_SELF)`). The window opens when the process has gone quiet.
#![allow(dead_code)]

use std::time::{Duration, Instant};

/// This process's CPU time so far, user and system together - `getrusage(RUSAGE_SELF)`.
pub fn this_process() -> Duration {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `getrusage` writes into a `rusage` this frame owns, and `RUSAGE_SELF` cannot fail
    // for a live process.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    assert_eq!(rc, 0, "getrusage(RUSAGE_SELF)");
    let secs = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    secs(ru.ru_utime) + secs(ru.ru_stime)
}

/// Any process's own `utime + stime`, read off `/proc/<pid>/stat` - the one a test uses to
/// prove a *neighbour* was really burning. `None` where there is no `/proc` (macOS) or the pid is
/// gone, so a caller can say the reading is not available rather than failing on it.
pub fn of_pid(pid: u32) -> Option<Duration> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` (field 2) is parenthesised and may itself contain spaces and parentheses, so the
    // fields are counted from AFTER the closing parenthesis - the standard way to parse this file.
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    // After `)` the first field is 3 (`state`), so 14 and 15 are the 12th and 13th here.
    let ticks: u64 = fields.nth(11)?.parse().ok()?;
    let ticks = ticks + fields.next()?.parse::<u64>().ok()?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hz = if hz > 0 { hz as u64 } else { 100 };
    Some(Duration::from_micros(ticks.saturating_mul(1_000_000) / hz))
}

/// The same quantity for THIS process, read the other way - so a test can show the two agree.
pub fn procs_reading() -> Option<Duration> {
    of_pid(std::process::id())
}

/// The stretch a settle adds up - one second, added up as a TOTAL rather than as a run of
/// shorter slices: a bursty tail puts a few quarter-second slices under 5 ms each, so *two quiet
/// slices in a row* was satisfied while the work went on. Every settle costs at least this long.
pub const SPAN: Duration = Duration::from_secs(1);

/// The settle's line: 1% of a core - [`SPAN`] / 100, i.e. 10 ms of CPU in a second. Low enough
/// that the rate it admits cannot breach the window's budget (at 10%, a slow opening sat just under
/// the settle and just over the window: 144 ms against 300), high enough that waiting is far under it.
pub fn settle_budget() -> Duration {
    SPAN / 100
}

/// Wait until this process has nothing left to do, and say how long that took. The condition is
/// read off the child's own accounting, so a quiet box and a box running ninety other processes
/// settle at the same point. The ceiling is [`CEILING`], and only a real hang trips it.
pub const CEILING: Duration = Duration::from_secs(120);

pub fn wait_until_idle(what: &str) -> Duration {
    wait_until_idle_within(what, CEILING)
}

/// The same settle with the ceiling handed in, so a test can prove the ceiling fires without
/// spending [`CEILING`] on it - the failure path is the one that has to work, because a daemon that
/// spins never settles and this is the assertion that has to say so.
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

/// How much CPU this process spends over `span` - the one reading every window here is built
/// from.
pub fn over(span: Duration) -> Duration {
    let before = this_process();
    std::thread::sleep(span);
    this_process().saturating_sub(before)
}
