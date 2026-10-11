//! **A busy store is waited for, never died of.**
//!
//! The report this module answers, 2026-10-12 ~01:20, from the model of the pg-noop project
//! session: *"the child died on sqlite: database is locked in the harness's own store, which is
//! the second infrastructure ending tonight … Neither is something I can fix from inside a
//! session, and both cost an agent's worth of work."* The child was
//! `s-1789919514688401228-sub-1791757785761179646`, and its last words were
//! `store: row 878 (…#t0.878): sqlite: database is locked` — 1352 uncommitted insertions across
//! eight files left behind in its worktree. The same minute the operator's rano head showed
//! `! merge_queue_store_error — the queues could not read or write their store while serving
//! the queues: sqlite: database is locked`.
//!
//! # What was measured
//!
//! * **A commit costs ~400 ms on this box tonight.** A fresh 4 MB WAL file on the same ext4
//!   (LUKS) volume as `sessions.db`, `synchronous = FULL`, one INSERT per autocommit: 500 rows
//!   took 205.7 s, 411 ms each, because every commit is an `fsync` and ext4's ordered mode
//!   makes that fsync wait for every other process's dirty data — and the box was running
//!   several `cargo test --workspace` at once. The same 500 rows in ONE transaction: 7.8 s.
//! * **The reseat copy of the rano session** (`s-1789668727263064994#t8` → `#t9`, ~971 rows)
//!   wrote one autocommit per row from 01:17:46 to ~01:24: the row timestamps are ~400 ms
//!   apart, i.e. the copy took the write lock 971 times back to back for six and a half
//!   minutes. The child died at 01:19:2x, inside that window; the queue errors are in it too.
//! * SQLite's busy handler is a **sleep-and-poll** (1, 2, 5 … 100 ms, then 100 ms forever),
//!   not a queue. With the lock held ~400 ms per commit by a writer that re-takes it at once,
//!   a poller can lose every race for the whole thirty seconds of `Store::BUSY_TIMEOUT`; then
//!   `append_item` returned `SQLITE_BUSY` and the child's turn ended on it.
//!
//! So the busy timeout is not a wait-forever: it is a single attempt of a lottery. This module
//! is the second half of the answer — **a write that comes back busy is taken again** with
//! backoff, for up to [`BUSY_RETRY_TOTAL`] — and `Store::append_items` is the first half: the
//! writer that holds the lock 971 times holds it once.
//!
//! # Why a retry is safe
//!
//! Every call this wraps is either one autocommit statement or a whole transaction that is
//! begun inside the closure. A statement or a transaction that failed with `SQLITE_BUSY` has
//! committed nothing — SQLite rolls the statement back, and rusqlite's `Transaction` rolls the
//! transaction back on drop — so running it again writes it once.
//!
//! What is **not** wrapped is a single statement inside a caller's open transaction: retrying
//! one statement of a transaction whose snapshot is stale can never succeed (that is
//! `SQLITE_BUSY_SNAPSHOT`), which is why every write transaction here begins `IMMEDIATE`.

use std::time::{Duration, Instant};

use super::{Result, StoreError};

/// **How long a write keeps trying after the first `SQLITE_BUSY`**, on top of the busy
/// timeout each attempt already waits. A minute: long enough to outlast any write this store
/// makes (the longest measured, a 971-row reseat copy, now commits in one transaction), short
/// enough that a store that is wedged — a process stopped holding the lock — is reported
/// rather than hung on.
pub const BUSY_RETRY_TOTAL: Duration = Duration::from_secs(60);

/// The first pause between attempts; it doubles up to [`BUSY_RETRY_MAX_STEP`].
const BUSY_RETRY_FIRST: Duration = Duration::from_millis(50);
const BUSY_RETRY_MAX_STEP: Duration = Duration::from_secs(2);

/// **Is this the database being busy, rather than the operation being wrong?**
///
/// `SQLITE_BUSY` (another connection holds the lock, including the `_SNAPSHOT` and
/// `_RECOVERY` extended codes) and `SQLITE_LOCKED` (a conflict inside one shared cache). Both
/// print as *"database is locked"* / *"database table is locked"*, and both mean the same thing
/// to a caller: nothing was written, and the same write later is the right move.
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
    let jitter = (std::process::id() as u64 ^ attempt as u64 * 7919) % 50;
    base + Duration::from_millis(jitter)
}

/// **Run `f` until it is not busy, or until `budget` has passed since the first attempt.**
///
/// Generic over the error so the store's `rusqlite::Error` and `StoreError` paths share one
/// ladder. Any error that is not busy is returned at once: retrying a constraint violation
/// would only fail the same way more slowly.
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
            other => return other,
        }
    }
}

/// [`retry_busy`] for one store operation with the default [`BUSY_RETRY_TOTAL`].
pub(super) fn retry_store<T>(f: impl FnMut() -> Result<T>) -> Result<T> {
    retry_busy(BUSY_RETRY_TOTAL, StoreError::is_busy, f)
}

/// [`retry_busy`] for one rusqlite call with the default [`BUSY_RETRY_TOTAL`].
pub(super) fn retry_sql<T>(f: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    retry_busy(BUSY_RETRY_TOTAL, is_busy_sql, f)
}

#[cfg(test)]
mod tests;
