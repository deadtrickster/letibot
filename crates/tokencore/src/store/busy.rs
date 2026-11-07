//! A busy store is waited for, never died of. `SQLITE_BUSY` is ordinary where sessions share a
//! file, so a write takes `BEGIN IMMEDIATE` and retries within a budget; only a non-busy
//! failure is an error.

use std::time::{Duration, Instant};

use super::{Result, StoreError};

/// How long a write keeps trying after the first `SQLITE_BUSY`, on top of the busy timeout each
/// attempt already waits. A minute, so it outlasts any write this box performs.
pub const BUSY_RETRY_TOTAL: Duration = Duration::from_secs(60);

/// The first pause between attempts; it doubles up to [`BUSY_RETRY_MAX_STEP`].
const BUSY_RETRY_FIRST: Duration = Duration::from_millis(50);
const BUSY_RETRY_MAX_STEP: Duration = Duration::from_secs(2);

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

/// The pause before attempt `attempt + 1` (0-based): 50 ms doubling, capped at 2 s, with a
/// little per-process jitter so two daemons that collided do not collide again in lockstep.
pub(super) fn backoff(attempt: u32) -> Duration {
    let base = BUSY_RETRY_FIRST
        .saturating_mul(1u32 << attempt.min(16))
        .min(BUSY_RETRY_MAX_STEP);
    let jitter = (std::process::id() as u64 ^ (attempt as u64 * 7919)) % 50;
    base + Duration::from_millis(jitter)
}

/// Run `f` until it is not busy, or until `budget` has passed since the first attempt. Generic
/// over the error so the store's `rusqlite::Error` and `StoreError` share one ladder.
#[track_caller]
pub(super) fn retry_busy<T, E>(
    budget: Duration,
    busy: impl Fn(&E) -> bool,
    mut f: impl FnMut() -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    let start = Instant::now();
    let mut attempt = 0u32;
    loop {
        match f() {
            Err(e) if busy(&e) && start.elapsed() < budget => {
                let pause = backoff(attempt).min(budget.saturating_sub(start.elapsed()));
                std::thread::sleep(pause);
                attempt += 1;
            }
            other => {
                said_if_slow(start.elapsed(), attempt, std::panic::Location::caller());
                return other;
            }
        }
    }
}

/// A store call that took longer than this is logged: one slow writer stalls every daemon
/// sharing the file, and the store split is to be measured against the stalls actually seen.
pub const SLOW_CALL: Duration = Duration::from_secs(1);

/// One line on stderr for a call over [`SLOW_CALL`], naming the store method by its line.
/// Written, not `eprintln!`: a daemon whose stderr closed must not panic in its store.
fn said_if_slow(took: Duration, retries: u32, at: &std::panic::Location<'_>) {
    if let Some(line) = slow_line(took, retries, at) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{line}");
    }
}

/// [`said_if_slow`]'s line, or `None` for a call under [`SLOW_CALL`].
pub(super) fn slow_line(
    took: Duration,
    retries: u32,
    at: &std::panic::Location<'_>,
) -> Option<String> {
    (took >= SLOW_CALL).then(|| {
        format!(
            "store: a call at {}:{} took {} ms ({retries} busy retries)",
            at.file(),
            at.line(),
            took.as_millis()
        )
    })
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
