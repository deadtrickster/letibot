//! A busy store is waited for, never died of. `SQLITE_BUSY` is ordinary where sessions share a
//! file, so a write takes `BEGIN IMMEDIATE` and retries within a budget; only a non-busy
//! failure is an error.
//!
//! The wait itself is measured, not inferred: [`on_busy`] is a busy handler of the store's own
//! that sleeps the retry and adds the sleep up, so a lock wait shows as `waited N ms` on the
//! call's line. A laddered call's attempt sleeps at most [`LADDER_ATTEMPT`] there, because the
//! handler sleeps holding the process's one connection; the rest of the wait is between attempts.
//!
//! The line, on a daemon's stderr:
//!
//! ```text
//! store: a call at store/procs.rs:345 took 11405 ms (waited 11400 ms, 0 busy retries)
//! store: a call at store.rs:3150 failed after 87 ms (waited 0 ms, 2 busy retries, 3 at this site): database is locked
//! ```
//!
//! The first is a call over [`SLOW_CALL`]; `waited` is how much of it SQLite's busy handler slept.
//! A `queued N ms behind another thread` part, when present, is the wait for this process's
//! connection while another thread used it; the rest is I/O. The second is a [`Flavour`] failure - a write that lost the lock or a
//! filesystem that refused it - logged however fast it was, with how many of that kind have
//! reached a caller at that `file:line` since the process started.

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{Result, Store, StoreError};

/// How long a write keeps trying after the first `SQLITE_BUSY`. Ninety seconds: the minute
/// this used to be plus the 30 s one attempt used to sleep inside the handler.
pub const BUSY_RETRY_TOTAL: Duration = Duration::from_secs(90);

/// The first pause between attempts; it doubles up to [`BUSY_RETRY_MAX_STEP`]. The cap is short
/// because the ladder now does the polling a long in-handler wait used to do.
const BUSY_RETRY_FIRST: Duration = Duration::from_millis(50);
const BUSY_RETRY_MAX_STEP: Duration = Duration::from_millis(250);

/// How long one attempt of a laddered call sleeps in the busy handler. Short, because the handler
/// sleeps holding this process's only connection to the file, and every other thread waits too.
pub(super) const LADDER_ATTEMPT: Duration = Duration::from_millis(250);

/// How long [`on_busy`] sleeps for the lock before one attempt gives up. It is
/// [`Store::BUSY_TIMEOUT`] and not a number of its own, so the doc and the handler cannot drift.
const BUSY_TIMEOUT: Duration = Store::BUSY_TIMEOUT;

/// SQLite's own backoff, in milliseconds: what `sqlite3_busy_timeout` sleeps on its 1st, 2nd, ...
/// retry, and 100 ms for every one after the twelfth.
const DELAYS_MS: [u64; 12] = [1, 2, 5, 10, 15, 20, 25, 25, 25, 50, 50, 100];

// This thread's measurement and, in a test, the lines that would have gone to stderr. SQLite
// calls the handler on the thread running the statement, so a thread's own slots are right.
thread_local! {
    static MEASUREMENT: RefCell<Measure> = RefCell::new(Measure::default());
}

#[derive(Default)]
struct Measure {
    /// How many instrumented calls this thread is inside. Only the outermost one reports: a
    /// nested call's wait is part of the outer one's, and counting it twice would double it.
    depth: u32,
    /// What the busy handler has slept since the outermost call began.
    waited: Duration,
    /// The same for the attempt now running, which is what [`BUSY_TIMEOUT`] caps.
    epoch: Duration,
    /// What the outermost call spent waiting for this process's connection, which another thread
    /// held. Not `waited`: that one is this thread's own busy sleep.
    queued: Duration,
    /// `Some` while a test is collecting lines instead of writing them.
    lines: Option<Vec<String>>,
}

/// Is this the database being busy rather than the operation being wrong? Covers `SQLITE_BUSY`
/// and its `_SNAPSHOT` and `_RECOVERY` spellings.
pub fn is_busy_sql(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(
                f.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

impl StoreError {
    /// [`is_busy_sql`] for a store error: the one kind of failure a caller should wait out.
    pub fn is_busy(&self) -> bool {
        matches!(self, StoreError::Sql(e) if is_busy_sql(e))
    }
}

/// The name of a failure worth counting by call site: a write that lost the lock and a
/// filesystem that refused it. No retry fixes either, and the logs of 2026-11 counted them by
/// grep; this is the same count on the line that already names the site.
pub(super) trait Flavour {
    fn flavour(&self) -> Option<&'static str>;
}

impl Flavour for rusqlite::Error {
    fn flavour(&self) -> Option<&'static str> {
        match self {
            rusqlite::Error::SqliteFailure(f, _) => match f.code {
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                    Some("database is locked")
                }
                rusqlite::ErrorCode::DiskFull => Some("database or disk is full"),
                _ => None,
            },
            _ => None,
        }
    }
}

impl Flavour for StoreError {
    fn flavour(&self) -> Option<&'static str> {
        match self {
            StoreError::Sql(e) => e.flavour(),
            _ => None,
        }
    }
}

/// SQLite's busy handler: sleep the backoff, count the sleep, and give up at [`BUSY_TIMEOUT`],
/// or at [`LADDER_ATTEMPT`] inside a laddered call, which retries with the connection released.
/// Installed instead of `busy_timeout` because that one is a black box which reports nothing.
pub(super) fn on_busy(count: i32) -> bool {
    let step = DELAYS_MS[(count.max(0) as usize).min(DELAYS_MS.len() - 1)];
    let delay = Duration::from_millis(step);
    let go = MEASUREMENT.with(|m| {
        let mut m = m.borrow_mut();
        // SQLite resets the count at the end of every statement, so a `0` is the first call of
        // one wait: the cap's clock starts there.
        if count <= 0 {
            m.epoch = Duration::ZERO;
        }
        let cap = if m.depth > 0 {
            LADDER_ATTEMPT
        } else {
            BUSY_TIMEOUT
        };
        if m.epoch + delay > cap {
            return false;
        }
        m.epoch += delay;
        m.waited += delay;
        true
    });
    if go {
        std::thread::sleep(delay);
    }
    go
}

/// The pause before attempt `attempt + 1` (0-based): 50 ms doubling, capped at 2 s, with a
/// little per-process jitter so two daemons that collided do not collide again in lockstep.
pub(super) fn backoff(attempt: u32) -> Duration {
    let base = BUSY_RETRY_FIRST
        .saturating_mul(1u32 << attempt.min(16))
        .min(BUSY_RETRY_MAX_STEP);
    let jitter = (std::process::id() as u64 ^ (attempt as u64 * 7919)) % 50;
    base + Duration::from_millis(jitter)
}

/// Begin an instrumented call: the outermost one starts the clock, a nested one only counts.
fn enter() {
    MEASUREMENT.with(|m| {
        let mut m = m.borrow_mut();
        if m.depth == 0 {
            m.waited = Duration::ZERO;
            m.queued = Duration::ZERO;
        }
        m.depth += 1;
    });
}

/// End an instrumented call and say what its two waits were: zero for a nested call, whose waits
/// are the outer one's to report.
fn leave() -> Waits {
    MEASUREMENT.with(|m| {
        let mut m = m.borrow_mut();
        m.depth = m.depth.saturating_sub(1);
        if m.depth > 0 {
            Waits::default()
        } else {
            Waits {
                lock: std::mem::take(&mut m.waited),
                queued: std::mem::take(&mut m.queued),
            }
        }
    })
}

/// A call's waits: on another connection's write lock, and on this process's own connection.
#[derive(Clone, Copy, Default)]
pub(super) struct Waits {
    pub lock: Duration,
    pub queued: Duration,
}

/// Count `d` spent taking this process's connection against the instrumented call running on this
/// thread, if there is one.
pub(super) fn queued(d: Duration) {
    MEASUREMENT.with(|m| {
        let mut m = m.borrow_mut();
        if m.depth > 0 {
            m.queued += d;
        }
    });
}

/// Run `f` until it is not busy, or until `budget` has passed since the first attempt. Generic
/// over the error so the store's `rusqlite::Error` and `StoreError` share one ladder.
#[track_caller]
pub(super) fn retry_busy<T, E: Flavour>(
    budget: Duration,
    busy: impl Fn(&E) -> bool,
    mut f: impl FnMut() -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    let start = Instant::now();
    let mut attempt = 0u32;
    enter();
    loop {
        match f() {
            Err(e) if busy(&e) && start.elapsed() < budget => {
                let pause = backoff(attempt).min(budget.saturating_sub(start.elapsed()));
                std::thread::sleep(pause);
                // The pause is lock wait too, now that most of a long wait is spent between attempts.
                MEASUREMENT.with(|m| m.borrow_mut().waited += pause);
                attempt += 1;
            }
            other => {
                let waits = leave();
                let took = start.elapsed();
                note(took, waits, attempt, &other, std::panic::Location::caller());
                return other;
            }
        }
    }
}

/// A store call that took longer than this is logged: one slow writer stalls every daemon
/// sharing the file, and the store split is to be measured against the stalls actually seen.
pub const SLOW_CALL: Duration = Duration::from_secs(1);

/// What an instrumented call says about itself: a line if it was slow, and a line if it failed
/// with a [`Flavour`] whatever its speed, because the count of those is the other half of the
/// picture.
fn note<T, E: Flavour>(
    took: Duration,
    waits: Waits,
    retries: u32,
    out: &std::result::Result<T, E>,
    at: &'static std::panic::Location<'static>,
) {
    match out.as_ref().err().and_then(Flavour::flavour) {
        Some(what) => {
            let n = counted(at, what);
            emit(&failure_line(took, waits, retries, at, what, n));
        }
        None => {
            if let Some(line) = slow_line(took, waits, retries, at) {
                emit(&line);
            }
        }
    }
}

/// One call site and one kind of failure - the key a count is kept under.
type Site = ((&'static str, u32), &'static str);

/// How many failures of one kind have reached a caller at one site since the process started:
/// what the failure's line carries, so a log answers "how often, and where" without a second
/// grep. Few enough sites that a walk beats a map.
fn counted(at: &'static std::panic::Location<'static>, what: &'static str) -> u64 {
    static SITES: Mutex<Vec<(Site, u64)>> = Mutex::new(Vec::new());
    let mut sites = SITES.lock().unwrap_or_else(|e| e.into_inner());
    let key = ((at.file(), at.line()), what);
    match sites.iter_mut().find(|(k, _)| *k == key) {
        Some((_, n)) => {
            *n += 1;
            *n
        }
        None => {
            sites.push((key, 1));
            1
        }
    }
}

/// Where a line goes: a daemon's stderr, or the list a test is collecting into instead. Written,
/// not `eprintln!`: a daemon whose stderr closed must not panic in its store.
fn emit(line: &str) {
    let kept = MEASUREMENT.with(|m| match m.borrow_mut().lines.as_mut() {
        Some(list) => {
            list.push(line.to_string());
            true
        }
        None => false,
    });
    if !kept {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{line}");
    }
}

/// [`note`]'s line for a call over [`SLOW_CALL`], or `None` for one under it.
pub(super) fn slow_line(
    took: Duration,
    waits: Waits,
    retries: u32,
    at: &std::panic::Location<'_>,
) -> Option<String> {
    (took >= SLOW_CALL).then(|| {
        format!(
            "store: a call at {}:{} took {} ms (waited {} ms, {retries} busy retries{})",
            at.file(),
            at.line(),
            took.as_millis(),
            waits.lock.as_millis(),
            queued_words(waits.queued)
        )
    })
}

/// The in-process wait, said only when there was one, so an uncontended line keeps its shape.
fn queued_words(d: Duration) -> String {
    match d.as_millis() {
        0 => String::new(),
        ms => format!(", queued {ms} ms behind another thread"),
    }
}

/// [`note`]'s line for a [`Flavour`] failure: the slow-call shape plus what it was and how many
/// of those this site has seen. A failure is said however fast it was - a refused write is not
/// a slow one, and the count is the whole point.
fn failure_line(
    took: Duration,
    waits: Waits,
    retries: u32,
    at: &std::panic::Location<'_>,
    what: &str,
    count: u64,
) -> String {
    format!(
        "store: a call at {}:{} failed after {} ms (waited {} ms, {retries} busy retries{}, \
         {count} at this site): {what}",
        at.file(),
        at.line(),
        took.as_millis(),
        waits.lock.as_millis(),
        queued_words(waits.queued)
    )
}

/// Collect the lines this thread's instrument would write instead of writing them, so a test
/// asserts the measurement and not a log. Thread-local: tests run in parallel in one process.
#[cfg(test)]
pub(super) fn collecting<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    MEASUREMENT.with(|m| m.borrow_mut().lines = Some(Vec::new()));
    let out = f();
    let lines = MEASUREMENT.with(|m| m.borrow_mut().lines.take().unwrap_or_default());
    (out, lines)
}

/// [`retry_busy`] for one store operation with the default [`BUSY_RETRY_TOTAL`].
#[track_caller]
pub(super) fn retry_store<T>(f: impl FnMut() -> Result<T>) -> Result<T> {
    retry_busy(BUSY_RETRY_TOTAL, StoreError::is_busy, f)
}

/// [`retry_busy`] for one rusqlite call with the default [`BUSY_RETRY_TOTAL`].
#[track_caller]
pub(super) fn retry_sql<T>(f: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    retry_busy(BUSY_RETRY_TOTAL, is_busy_sql, f)
}

#[cfg(test)]
mod tests;
