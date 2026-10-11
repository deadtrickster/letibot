//! **The daemon watching its own memory** — `TODO.md` R66.
//!
//! A harnessd serving qwen-3.8-flash-next reached 108 GB resident over 47 hours and was killed
//! by the kernel at 2026-10-11 00:47, and its log had not one line about memory in it: nothing
//! said it was growing, what was growing, or when it started. The operator: *"can we have more
//! logs and more instrumentation? maybe some asserts? can rust runtime do asserts or log large
//! memory allocations?"* Three answers, one module:
//!
//! 1. **[`Counting`], the global allocator** (installed in `bin/harnessd.rs`). The system
//!    allocator with two atomics around it — bytes live and the high-water mark — and a line
//!    with a backtrace for any ONE allocation at or above [`set_large_threshold`] (64 MiB by
//!    default, `LETIBOT_LARGE_ALLOC`). That is the single runaway buffer: a `Vec` doubling into
//!    gigabytes, a whole file read into memory. It cannot see a million small allocations;
//!    the census can.
//! 2. **[`spawn`], the watchdog thread.** One line a minute (`LETIBOT_MEMWATCH_SECS`): resident
//!    memory, heap live and peak, threads, sessions, and what the hubs hold. When resident
//!    memory crosses the next step (1 GiB, then doubling), a **census**: the hubs holding most,
//!    the threads by name, and a warning published to every session — so the person sees it in
//!    their session and not only in a log they would have to go and read.
//! 3. **The bounds, asserted.** Every number in the census is something a bound in sessionlog
//!    claims to keep. A census that finds one broken says `BOUND BROKEN` in the log and on that
//!    session, once per session and bound. A log line and a warning, not a panic: a panic takes
//!    every session down with it, and the point is to see the leak while the daemon still runs.
//!
//! The fourth half lives in `cli.rs`: the daemon's own leaf gets `memory.high`, so past it the
//! kernel throttles the daemon instead of letting it take the box. This module reads that
//! leaf's `memory.events` and says when the throttle has started.
//!
//! # Not a delivery clock
//!
//! §18.1-I12 says the daemon spawns no timer or poll loop **for message delivery**. This is
//! neither: it delivers nothing, and a watchdog that only ran when something else woke the
//! daemon would be silent exactly while the daemon is growing with nobody talking to it —
//! which is how the 47 hours went.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::HubCensus;
use letibot_sessionlog::registry::Registry;
use letibot_tools::exec::human_bytes;

// ===== 1. The allocator =====

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static LARGE: AtomicU64 = AtomicU64::new(0);
static LARGE_THRESHOLD: AtomicUsize = AtomicUsize::new(64 << 20);

thread_local! {
    /// Set while this thread is logging a large allocation: the log line and its backtrace
    /// allocate, and an allocator that logged its own logging would not stop.
    static LOGGING: Cell<bool> = const { Cell::new(false) };
}

/// **The system allocator, counted.** See the module docs. One `fetch_add` and one `fetch_max`
/// per allocation; the large-allocation path is taken only above the threshold.
pub struct Counting;

/// Below this many bytes nothing is logged, whatever the setting says: a threshold of a few
/// bytes would log every allocation and the daemon would do nothing else.
const LARGE_FLOOR: usize = 1 << 20;

/// Log any single allocation of `bytes` or more. Read from `LETIBOT_LARGE_ALLOC` at start.
pub fn set_large_threshold(bytes: usize) {
    LARGE_THRESHOLD.store(bytes.max(LARGE_FLOOR), Ordering::Relaxed);
}

/// Bytes the program holds from the allocator right now, the most it ever held, and how many
/// single allocations reached the threshold.
pub fn heap() -> (usize, usize, u64) {
    (
        LIVE.load(Ordering::Relaxed),
        PEAK.load(Ordering::Relaxed),
        LARGE.load(Ordering::Relaxed),
    )
}

fn grew(n: usize) {
    let live = LIVE.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

fn large(size: usize, how: &str) {
    if size < LARGE_THRESHOLD.load(Ordering::Relaxed) {
        return;
    }
    LARGE.fetch_add(1, Ordering::Relaxed);
    // `try_with`: during a thread's teardown its locals may be gone, and an allocation then is
    // counted and not logged rather than a panic inside the allocator.
    let _ = LOGGING.try_with(|busy| {
        if busy.get() {
            return;
        }
        busy.set(true);
        let bt = std::backtrace::Backtrace::force_capture();
        eprintln!(
            "memwatch: LARGE {how} of {} ({size} bytes); heap live {}\n{bt}",
            human_bytes(size as u64),
            human_bytes(LIVE.load(Ordering::Relaxed) as u64)
        );
        busy.set(false);
    });
}

// SAFETY: every call is forwarded to `System` with the same arguments; the counters and the log
// line are side effects that neither touch the returned memory nor change its layout.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grew(l.size());
            large(l.size(), "alloc");
        }
        p
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            grew(l.size());
            large(l.size(), "alloc");
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, l: Layout) {
        unsafe { System.dealloc(ptr, l) };
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, l, new) };
        if !p.is_null() {
            let old = l.size();
            if new >= old {
                grew(new - old);
                // A `Vec` doubling past the threshold is exactly what this is for, and each
                // doubling above it is its own line — rare by construction.
                large(new, "realloc");
            } else {
                LIVE.fetch_sub(old - new, Ordering::Relaxed);
            }
        }
        p
    }
}

// ===== 2. The watchdog =====

/// The first resident-memory step that triggers a census; each after it is double the last.
const FIRST_STEP: u64 = 1 << 30;

/// One minute, unless `LETIBOT_MEMWATCH_SECS` says otherwise.
pub const DEFAULT_EVERY: Duration = Duration::from_secs(60);

/// Start the watchdog. `leaf` is the daemon's own cgroup when it runs in a delegated scope
/// (`exec::delegate_memory`), for the `memory.high` throttle count.
pub fn spawn(registry: Arc<Registry>, every: Duration, leaf: Option<PathBuf>) {
    let spawned = std::thread::Builder::new()
        .name("memwatch".into())
        .spawn(move || watch(registry, every, leaf));
    if let Err(e) = spawned {
        eprintln!(
            "memwatch: the watchdog did not start ({e}); this daemon's memory goes unwatched"
        );
    }
}

fn watch(registry: Arc<Registry>, every: Duration, leaf: Option<PathBuf>) {
    let mut next_step = FIRST_STEP;
    let mut said: HashSet<(String, &'static str)> = HashSet::new();
    let mut throttled_seen = 0u64;
    let mut tick = 0u64;
    loop {
        std::thread::sleep(every);
        tick += 1;
        let rss = resident().unwrap_or(0);
        let (live, peak, large_n) = heap();
        let census = registry.census();
        let threads = thread_names();
        eprintln!(
            "memwatch: {}",
            summary(rss, live, peak, large_n, threads.values().sum(), &census)
        );
        // R66's number over time (TODO R81): every fifth line — five minutes at the default —
        // and at every census step below.
        if tick % 5 == 1 || rss >= next_step {
            crate::instruments::mem_sample(rss, live, peak, threads.values().sum(), census.len(), census.iter().map(|(c, _)| c.heads).sum());
        }

        // The bounds, asserted.
        for (c, _) in &census {
            for (kind, broken) in broken_bounds(c) {
                if said.insert((c.session_id.clone(), kind)) {
                    eprintln!("memwatch: BOUND BROKEN in {}: {broken}", c.session_id);
                    if let Some(h) = registry.get(&c.session_id) {
                        h.publish(warning(
                            "memory_bound_broken",
                            format!(
                                "this session's {broken}. A bound the daemon relies on to keep \
                                 its memory flat did not hold; the daemon's log has the census."
                            ),
                        ));
                    }
                }
            }
        }

        // The throttle, when the daemon is past its own `memory.high`.
        if let Some(n) = leaf.as_ref().and_then(|l| throttle_events(l))
            && n > throttled_seen
        {
            eprintln!(
                "memwatch: THROTTLED — the daemon is past its memory.high ({} times so far); the \
                 kernel is reclaiming from it and it will be slow",
                n
            );
            throttled_seen = n;
        }

        // A step crossed: the census, and a word to every session.
        if rss >= next_step {
            while rss >= next_step {
                next_step = next_step.saturating_mul(2);
            }
            let report = full_census(rss, live, peak, &census, &threads);
            eprintln!("memwatch: CENSUS at {}\n{report}", human_bytes(rss));
            registry.broadcast(warning(
                "memory_high",
                format!(
                    "the daemon now holds {} resident (heap {}). That is not normal for a \
                     daemon; the census is in its log. Restarting the daemon frees it; saying \
                     so to the operator is the right move if you see this.",
                    human_bytes(rss),
                    human_bytes(live as u64)
                ),
            ));
        }
    }
}

fn warning(code: &str, detail: String) -> SessionEvent {
    SessionEvent::Warning {
        code: code.into(),
        detail,
        compaction: None,
    }
}

/// `VmRSS` from `/proc/self/status`, bytes.
fn resident() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

/// `memory.events`' `high` count for the daemon's leaf: how often it has been throttled.
fn throttle_events(leaf: &std::path::Path) -> Option<u64> {
    let s = std::fs::read_to_string(leaf.join("memory.events")).ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("high "))
        .and_then(|n| n.trim().parse().ok())
}

/// Every thread, grouped by its name with the per-instance part taken off — `letibot-job-j12`
/// and `letibot-job-j13` are one row — because a leak of threads is a count that climbs under
/// one name, and a list of 300 names hides it. Public so a test can take the same census the
/// watchdog prints (R68's `tests/session_lifecycle.rs`: threads flat across 50 subagents).
pub fn thread_names() -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
        for t in dir.flatten() {
            let name = std::fs::read_to_string(t.path().join("comm")).unwrap_or_default();
            *out.entry(thread_group(name.trim())).or_insert(0) += 1;
        }
    }
    out
}

/// `letibot-job-j12` → `letibot-job-#`; `monitor-wake-s-17914` → `monitor-wake-#`.
pub fn thread_group(name: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for part in name.split('-') {
        let digits = part.chars().filter(|c| c.is_ascii_digit()).count();
        if digits > 0 && digits * 2 >= part.len().max(1) {
            if parts.last().map(String::as_str) != Some("#") {
                parts.push("#".into());
            }
        } else {
            parts.push(part.to_string());
        }
    }
    parts.join("-")
}

/// The once-a-minute line.
pub fn summary(
    rss: u64,
    live: usize,
    peak: usize,
    large_n: u64,
    threads: usize,
    census: &[(HubCensus, usize)],
) -> String {
    let log_bytes: usize = census.iter().map(|(c, _)| c.log_bytes).sum();
    let log_events: usize = census.iter().map(|(c, _)| c.log_events).sum();
    let items: usize = census.iter().map(|(c, _)| c.view.items).sum();
    let heads: usize = census.iter().map(|(c, _)| c.heads).sum();
    let jobs: usize = census.iter().map(|(_, j)| j).sum();
    format!(
        "rss {} · heap live {} (peak {}) · {large_n} large alloc(s) · {threads} threads · {} \
         sessions · logs {} / {log_events} events · view {items} items · {heads} heads · {jobs} \
         job rows",
        human_bytes(rss),
        human_bytes(live as u64),
        human_bytes(peak as u64),
        census.len(),
        human_bytes(log_bytes as u64),
    )
}

/// **The bounds a hub claims, checked.** Each entry is `(which bound, the sentence)`. The slack
/// is the one event a log may hold over its bound between the push and the eviction, and a
/// head's queue may sit at its gap bound until the demotion that clears it.
pub fn broken_bounds(c: &HubCensus) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let log_slack = 1 << 20;
    if c.log_bytes > c.log_bound_bytes + log_slack {
        out.push((
            "log",
            format!(
                "event log holds {} against its {} bound",
                human_bytes(c.log_bytes as u64),
                human_bytes(c.log_bound_bytes as u64)
            ),
        ));
    }
    if c.head_queue_bound > 0 && c.head_queue_max > c.head_queue_bound + 1 {
        out.push((
            "head_queue",
            format!(
                "a head's queue holds {} events against its {} bound",
                c.head_queue_max, c.head_queue_bound
            ),
        ));
    }
    // The view's lists with no bound of their own: a count this large is a list that only
    // grows. Not a bound the code claims — a tripwire for one it should.
    const UNBOUNDED_TRIP: usize = 10_000;
    for (kind, n) in [
        ("view_settled", c.view.settled),
        ("view_warnings", c.view.warnings),
        ("view_subagents", c.view.subagents),
        ("operator_calls", c.operator_calls),
    ] {
        if n > UNBOUNDED_TRIP {
            out.push((
                kind,
                format!("{kind} holds {n} entries — a list with no bound that only grew"),
            ));
        }
    }
    out
}

fn full_census(
    rss: u64,
    live: usize,
    peak: usize,
    census: &[(HubCensus, usize)],
    threads: &BTreeMap<String, usize>,
) -> String {
    let mut s = format!(
        "  rss {} · heap live {} · heap peak {} · resident not on the heap {}\n",
        human_bytes(rss),
        human_bytes(live as u64),
        human_bytes(peak as u64),
        human_bytes(rss.saturating_sub(live as u64)),
    );
    let mut by_log: Vec<&(HubCensus, usize)> = census.iter().collect();
    by_log.sort_by_key(|(c, _)| std::cmp::Reverse(c.log_bytes));
    s.push_str("  hubs holding most (log bytes / events · view items, settled, warnings, subagents · heads, longest queue · job rows):\n");
    for (c, jobs) in by_log.iter().take(10) {
        s.push_str(&format!(
            "    {} — log {} / {} · view {} {} {} {} · heads {} q{} · jobs {}\n",
            c.session_id,
            human_bytes(c.log_bytes as u64),
            c.log_events,
            c.view.items,
            c.view.settled,
            c.view.warnings,
            c.view.subagents,
            c.heads,
            c.head_queue_max,
            jobs
        ));
    }
    let mut ts: Vec<(&String, &usize)> = threads.iter().collect();
    ts.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    s.push_str("  threads by name:\n");
    for (name, n) in ts.iter().take(15) {
        s.push_str(&format!("    {n:>5}  {name}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_names_group_by_what_they_are_not_by_which_one() {
        assert_eq!(thread_group("letibot-job-j12"), "letibot-job-#");
        assert_eq!(thread_group("letibot-job-j1234"), "letibot-job-#");
        assert_eq!(thread_group("monitor-wake-s-17914"), "monitor-wake-s-#");
        assert_eq!(thread_group("memwatch"), "memwatch");
        assert_eq!(thread_group("harnessd-signal"), "harnessd-signal");
    }

    /// The asserts have to be able to fail, or they assert nothing: a log over its bound and a
    /// queue over its gap bound are each reported, and a hub inside both is not.
    #[test]
    fn a_broken_bound_is_named_and_a_kept_one_is_not() {
        let ok = HubCensus {
            session_id: "s".into(),
            log_bytes: 10 << 20,
            log_bound_bytes: 16 << 20,
            head_queue_max: 10,
            head_queue_bound: 5000,
            ..Default::default()
        };
        assert!(broken_bounds(&ok).is_empty());
        let bad = HubCensus {
            log_bytes: 40 << 20,
            head_queue_max: 9000,
            ..ok.clone()
        };
        let kinds: Vec<&str> = broken_bounds(&bad).iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, vec!["log", "head_queue"]);
    }

    #[test]
    fn the_counters_move_with_what_is_allocated() {
        // The test binary does not install `Counting`, so drive the hooks directly.
        let before = LIVE.load(Ordering::Relaxed);
        grew(1000);
        assert!(LIVE.load(Ordering::Relaxed) >= before + 1000);
        assert!(PEAK.load(Ordering::Relaxed) >= before + 1000);
        LIVE.fetch_sub(1000, Ordering::Relaxed);
    }
}
