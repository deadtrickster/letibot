//! The daemon watching its own memory - `TODO.md`. A harnessd serving qwen-3.8-flash-next
//! reached 108 GB resident over 47 hours and was killed by the kernel at 2026-10-11 00:47
//! and its log had not one line about memory in it

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

/// The system allocator, counted. See the module docs. One `fetch_add` and one `fetch_max`
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
                // doubling above it is its own line - rare by construction.
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
    let mut before: Option<BTreeMap<String, i64>> = None;
    let mut creeping = 0u32;
    let mut creep_alarm = Creep::default();
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
        // What MOVED since the last line, because a total that is the same every minute says a leak
        // is not happening and a total that climbs says nothing about which term climbed.
        let now = terms(rss, live, threads.values().sum(), &census);
        if let Some(was) = &before {
            let grew = creep(was, &now);
            if !grew.is_empty() {
                eprintln!("memwatch: grew {grew}");
            }
            creeping =
                if now.get("rss").copied().unwrap_or(0) > was.get("rss").copied().unwrap_or(0) {
                    creeping + 1
                } else {
                    0
                };
            // Three lines running with nothing in the census to account for the resident growth:
            // the shape that took a daemon to 113 GB and left no line in its log about it.
            if creep_alarm.says(creeping, unaccounted(was, &now)) {
                eprintln!(
                    "memwatch: MEMORY CREEP — resident has grown on {creeping} lines running and \
                     nothing the census holds has grown with it; the heap's own count is {} and \
                     the daemon is holding {} of it",
                    human_bytes(live as u64),
                    human_bytes(rss)
                );
                registry.broadcast(warning(
                    "memory_creep",
                    format!(
                        "this daemon's resident memory has grown on {creeping} readings running \
                         and nothing it can account for has grown with it (rss {} against heap {}). \
                         The census is in its log. Restarting the daemon frees it",
                        human_bytes(rss),
                        human_bytes(live as u64)
                    ),
                ));
            }
        }
        before = Some(now);
        // 's number over time : every fifth line - five minutes at the default -
        // and at every census step below.
        if tick % 5 == 1 || rss >= next_step {
            crate::instruments::mem_sample(rss, live, peak, threads.values().sum(), &census);
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

/// The terms one reading watches, by name, so two readings can be subtracted. A name is what the log
/// has to say to be actionable: `s-1 log` and not `logs`.
pub fn terms(
    rss: u64,
    live: usize,
    threads: usize,
    census: &[(HubCensus, usize)],
) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    out.insert("rss".into(), rss as i64);
    out.insert("heap".into(), live as i64);
    out.insert("threads".into(), threads as i64);
    for (c, jobs) in census {
        let s = &c.session_id;
        for (what, n) in [
            ("log", c.log_bytes),
            ("events", c.log_events),
            ("view", c.view.items),
            ("settled", c.view.settled),
            ("warnings", c.view.warnings),
            ("subagents", c.view.subagents),
            ("jobs", *jobs),
            ("harness", c.footprint.total()),
        ] {
            out.insert(format!("{s} {what}"), n as i64);
        }
    }
    out
}

/// What grew between two readings, biggest first, as the sentence the log prints. Empty when nothing
/// moved: a line that says `grew 0` every minute is one nobody reads.
pub fn creep(before: &BTreeMap<String, i64>, now: &BTreeMap<String, i64>) -> String {
    /// Below this a delta is noise: a page, a refill, one event's worth of a hub.
    const FLOOR: i64 = 128 << 10;
    /// Counts are not bytes, and one thread or one event is a fact at any size.
    const COUNT_FLOOR: i64 = 1;
    let counts = [
        "threads",
        "events",
        "view",
        "settled",
        "warnings",
        "subagents",
        "jobs",
    ];
    let is_count = |name: &str| {
        counts
            .iter()
            .any(|c| name == *c || name.ends_with(&format!(" {c}")))
    };
    let mut grew: Vec<(&String, i64)> = now
        .iter()
        .filter_map(|(name, v)| {
            let was = before.get(name).copied().unwrap_or(0);
            let delta = v - was;
            let floor = if is_count(name) { COUNT_FLOOR } else { FLOOR };
            (delta >= floor).then_some((name, delta))
        })
        .collect();
    grew.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
    grew.iter()
        .take(6)
        .map(|(name, d)| {
            if is_count(name) {
                format!("{name} +{d}")
            } else {
                format!("{name} +{}", human_bytes(*d as u64))
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// When a creep is news, kept by the watchdog: the first reading of an episode, and then each
/// time the run of growing readings has doubled. A notice per tick is a distinct note on every
/// head, and the head's list is short - the older diagnostics it holds go out with them.
#[derive(Default)]
pub struct Creep {
    /// The reading this episode was last said at; zero until it is said.
    said_at: u32,
}

impl Creep {
    /// Whether `creeping` consecutive growing readings, with the census unable to account for
    /// them, are worth another notice. A reading that does not grow ends the episode.
    pub fn says(&mut self, creeping: u32, unaccounted: bool) -> bool {
        if creeping == 0 {
            self.said_at = 0;
            return false;
        }
        if creeping < 3 || !unaccounted {
            return false;
        }
        // Doubling, the rung `memory_high` climbs on: the second notice says the leak outlived the
        // first by as much again, which is a new fact, where "one more reading" is not.
        if self.said_at > 0 && creeping < self.said_at.saturating_mul(2) {
            return false;
        }
        self.said_at = creeping;
        true
    }
}

/// Whether the resident growth is bigger than everything the census holds put together: the fact that
/// says the leak is in a structure no term names yet.
pub fn unaccounted(before: &BTreeMap<String, i64>, now: &BTreeMap<String, i64>) -> bool {
    /// Below this between two readings, growth is a page here and a refill there, and a watch that
    /// cried over it would be one nobody reads.
    const WORTH_SAYING: i64 = 4 << 20;
    let delta = |m: &BTreeMap<String, i64>, k: &str| m.get(k).copied().unwrap_or(0);
    let rss = delta(now, "rss") - delta(before, "rss");
    let held: i64 = now
        .keys()
        .filter(|k| !matches!(k.as_str(), "rss" | "threads" | "events"))
        .map(|k| delta(now, k) - delta(before, k))
        .filter(|d| *d > 0)
        .sum();
    // A quarter of the growth is enough: the accounted terms are text and text is copied, so a term
    // that grew by X can be worth several X of resident memory without anything being lost.
    rss > WORTH_SAYING && held * 4 < rss
}

/// Every thread, grouped by its name with the per-instance part taken off -
/// `letibot-job-j12` and `letibot-job-j13` are one row - because a leak of threads is a
/// count that climbs under one name, and a list of 300 names hides it.
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
    let harness: usize = census.iter().map(|(c, _)| c.footprint.total()).sum();
    format!(
        "rss {} · heap live {} (peak {}) · {large_n} large alloc(s) · {threads} threads · {} \
         sessions · logs {} / {log_events} events · view {items} items · {heads} heads · {jobs} \
         job rows · harness {}",
        human_bytes(rss),
        human_bytes(live as u64),
        human_bytes(peak as u64),
        census.len(),
        human_bytes(log_bytes as u64),
        human_bytes(harness as u64),
    )
}

/// The bounds a hub claims, checked. Each entry is `(which bound, the sentence)`. The slack
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
    // What the session's HARNESS holds - the one term a growing daemon could not
    // name before this
    if c.footprint.over_ceiling() {
        let f = &c.footprint;
        out.push((
            "footprint",
            format!(
                "this session's harness holds {} against its {} ceiling — transcript {} ({} \
                 rows), token region {} ({} tokens), trail {} ({} utterances)",
                human_bytes(f.total() as u64),
                human_bytes(f.ceiling_bytes as u64),
                human_bytes(f.items_text_bytes as u64),
                f.items,
                human_bytes(f.ledger_region_bytes as u64),
                f.ledger_tokens,
                human_bytes(f.trail_bytes as u64),
                f.trail_utterances,
            ),
        ));
    }
    // The view's lists with no bound of their own: a count this large is a list that only
    // grows. Not a bound the code claims - a tripwire for one it should.
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
    s.push_str(
        "  hubs holding most (log bytes / events · view items, settled, warnings, subagents · \
         heads, longest queue · job rows · harness: transcript, region, trail / ceiling):\n",
    );
    for (c, jobs) in by_log.iter().take(10) {
        let f = &c.footprint;
        s.push_str(&format!(
            "    {} — log {} / {} · view {} {} {} {} · heads {} q{} · jobs {} · harness {} \
             ({} + {} + {}) / {}\n",
            c.session_id,
            human_bytes(c.log_bytes as u64),
            c.log_events,
            c.view.items,
            c.view.settled,
            c.view.warnings,
            c.view.subagents,
            c.heads,
            c.head_queue_max,
            jobs,
            human_bytes(f.total() as u64),
            human_bytes(f.items_text_bytes as u64),
            human_bytes(f.ledger_region_bytes as u64),
            human_bytes(f.trail_bytes as u64),
            human_bytes(f.ceiling_bytes as u64),
        ));
    }
    // And the same list by what the HARNESS holds, because the two orders are different
    // questions: a hub with the longest log is not the session that grew.
    let mut by_footprint: Vec<&(HubCensus, usize)> = census.iter().collect();
    by_footprint.sort_by_key(|(c, _)| std::cmp::Reverse(c.footprint.total()));
    if by_footprint
        .first()
        .is_some_and(|(c, _)| c.footprint.total() > 0)
    {
        s.push_str("  sessions whose harness holds most (of its own ceiling):\n");
        for (c, _) in by_footprint.iter().take(10) {
            let f = &c.footprint;
            if f.total() == 0 {
                break;
            }
            s.push_str(&format!(
                "    {} — {} / {} · transcript {} ({} rows) · region {} ({} tokens) · trail {} \
                 ({} utterances)\n",
                c.session_id,
                human_bytes(f.total() as u64),
                human_bytes(f.ceiling_bytes as u64),
                human_bytes(f.items_text_bytes as u64),
                f.items,
                human_bytes(f.ledger_region_bytes as u64),
                f.ledger_tokens,
                human_bytes(f.trail_bytes as u64),
                f.trail_utterances,
            ));
        }
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

    /// A leak that runs for twenty readings is a handful of notices, not twenty: every tick would
    /// be a new note on every head (`note_key` is code, time and the detail's hash), and the older
    /// diagnostics - this one's reason among them - are what the head's list drops to hold them.
    #[test]
    fn a_creep_is_said_when_it_starts_and_again_only_when_it_doubles() {
        let mut c = Creep::default();
        let said: Vec<u32> = (1..=20).filter(|n| c.says(*n, true)).collect();
        assert_eq!(
            said,
            vec![3, 6, 12],
            "a notice on every reading churns every head's list"
        );

        // A reading that did not grow ends the episode, so the next one is news from its start.
        assert!(!c.says(0, true));
        let again: Vec<u32> = (1..=3).filter(|n| c.says(*n, true)).collect();
        assert_eq!(again, vec![3], "a new episode is said at once");

        // And a creep the census accounts for is not a leak of anything unnamed: nothing is said.
        let mut quiet = Creep::default();
        assert!((1..=10).all(|n| !quiet.says(n, false)));
    }

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

    /// The census a person reads names the session AND the part that grew - 's whole
    /// ask. A total is not actionable
    #[test]
    fn the_census_names_the_session_and_the_part_that_grew() {
        let c = HubCensus {
            session_id: "s-the-one-that-grew".into(),
            footprint: letibot_sessionlog::hub::Footprint {
                items: 7,
                items_text_bytes: 2 << 20,
                ledger_region_bytes: 3 << 20,
                ledger_tokens: 786_432,
                trail_bytes: 1024,
                trail_utterances: 2,
                ceiling_bytes: 8 << 20,
            },
            ..Default::default()
        };
        let out = full_census(4 << 30, 1 << 30, 2 << 30, &[(c, 0)], &BTreeMap::new());
        for want in [
            "s-the-one-that-grew",
            "harness 5 MiB",
            "transcript 2 MiB",
            "region 3 MiB",
            "trail 1 KiB",
            "sessions whose harness holds most",
        ] {
            assert!(out.contains(want), "the census must say {want}:\n{out}");
        }
        // And a daemon whose hubs have no footprint prints no empty block.
        let bare = full_census(
            4 << 30,
            1 << 30,
            2 << 30,
            &[(
                HubCensus {
                    session_id: "s-plain".into(),
                    ..Default::default()
                },
                0,
            )],
            &BTreeMap::new(),
        );
        assert!(
            !bare.contains("sessions whose harness holds most"),
            "{bare}"
        );
    }

    /// The term could not name - a session's harness past the ceiling it states for
    /// itself is reported, and the sentence says which part grew. Three shapes, because the
    /// three have to be told apart
    #[test]
    fn a_harness_past_its_ceiling_is_named_and_one_inside_it_is_not() {
        let ceiling = 4 << 20;
        let inside = HubCensus {
            session_id: "s".into(),
            footprint: letibot_sessionlog::hub::Footprint {
                items: 12,
                items_text_bytes: 1 << 20,
                ledger_region_bytes: 1 << 20,
                ledger_tokens: 262_144,
                trail_bytes: 1024,
                trail_utterances: 3,
                ceiling_bytes: ceiling,
            },
            ..Default::default()
        };
        assert!(broken_bounds(&inside).is_empty());

        // A hub nobody registered a footprint on: no ceiling, so nothing to be over. The census
        // of every test hub and every head's hub takes this arm.
        let unregistered = HubCensus {
            session_id: "s".into(),
            footprint: letibot_sessionlog::hub::Footprint {
                items_text_bytes: 1 << 30,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(broken_bounds(&unregistered).is_empty());

        let over = HubCensus {
            footprint: letibot_sessionlog::hub::Footprint {
                items_text_bytes: 3 << 20,
                ledger_region_bytes: 2 << 20,
                ..inside.footprint
            },
            ..inside
        };
        let broken = broken_bounds(&over);
        let kinds: Vec<&str> = broken.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, vec!["footprint"]);
        let sentence = &broken[0].1;
        // `human_bytes` rounds whole MiB, so these are the words the operator would read.
        for named in ["5 MiB", "4 MiB", "transcript 3 MiB", "token region 2 MiB"] {
            assert!(
                sentence.contains(named),
                "the sentence must name {named}: {sentence}"
            );
        }
    }

    fn a_hub(session: &str, log: usize, view: usize) -> (HubCensus, usize) {
        (
            HubCensus {
                session_id: session.into(),
                log_bytes: log,
                log_events: log / 1024,
                view: letibot_sessionlog::view::ViewCensus {
                    items: view,
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        )
    }

    /// A flat daemon prints no grew line at all, and one whose log doubled names the log and the
    /// session it belongs to - a total cannot say either.
    #[test]
    fn the_grew_line_names_the_term_that_moved_and_not_the_total() {
        let still = terms(200 << 20, 10 << 20, 14, &[a_hub("s-1", 1 << 20, 40)]);
        assert_eq!(creep(&still, &still.clone()), "");

        let later = terms(206 << 20, 11 << 20, 14, &[a_hub("s-1", 3 << 20, 41)]);
        let said = creep(&still, &later);
        for want in ["rss +", "heap +", "s-1 log +2 MiB", "s-1 events +"] {
            assert!(said.contains(want), "the line must say {want}: {said}");
        }
        assert!(!said.contains("threads"), "nothing moved there: {said}");

        // A session that went away is not a negative in a list of what grew.
        let gone = terms(200 << 20, 10 << 20, 14, &[]);
        let said = creep(&later, &gone);
        assert!(
            !said.contains("s-1"),
            "a closed session is not news: {said}"
        );
    }

    /// The sentence that says the growth is nowhere in the census: resident climbing with every term
    /// flat is the shape that took a daemon to 113 GB, and a term climbing with it is not.
    #[test]
    fn growth_with_nothing_accounted_for_it_is_what_the_watch_calls_creep() {
        let before = terms(200 << 20, 10 << 20, 14, &[a_hub("s-1", 1 << 20, 40)]);
        // Nothing the census holds moved, and the resident count rose by 40 MiB.
        let flat = terms(240 << 20, 10 << 20, 14, &[a_hub("s-1", 1 << 20, 40)]);
        assert!(
            unaccounted(&before, &flat),
            "20 MiB of growth and no term moved is the whole of the defect"
        );
        assert!(creep(&before, &flat).contains("rss +"));

        // The same growth with a log that grew by more than half of it is accounted for.
        let held = terms(240 << 20, 10 << 20, 14, &[a_hub("s-1", 12 << 20, 40)]);
        assert!(
            !unaccounted(&before, &held),
            "a log that grew by more than a quarter of the resident growth explains it"
        );

        // And a daemon that is not growing at all is never a creep.
        assert!(!unaccounted(&before, &before.clone()));
    }
}
