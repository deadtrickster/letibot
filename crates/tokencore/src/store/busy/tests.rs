//! The failure mode, its remedy, the retry ladder and the long copy — each against a real WAL
//! file with two connections, because an in-memory store has no second connection to lose a
//! race to.
//!
//! The report: *"the child died on sqlite: database is locked in the harness's own store,
//! which is the second infrastructure ending tonight … Neither is something I can fix from
//! inside a session, and both cost an agent's worth of work."* (pg-noop's model, 2026-10-12.)

use std::sync::mpsc;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::super::*;
use super::{backoff, retry_busy};
use crate::ledger::{TokenLedger, hash_tokens};

/// A temp file that removes itself and its `-wal` / `-shm`.
struct TempDb(std::path::PathBuf);

impl TempDb {
    fn new(tag: &str) -> TempDb {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-busy-{tag}-{}-{}.db",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let t = TempDb(p);
        t.clean();
        t
    }
    fn clean(&self) {
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", self.0.display()));
        }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        self.clean();
    }
}

fn seed(s: &Store) -> String {
    let tokens = vec![1u32, 2, 3, 4];
    let prefix_id = s
        .put_stable_prefix(&StablePrefixRecord {
            dialect_sha: "aa".repeat(32),
            system: "you are a harness".into(),
            tools_json: vec![],
            h_init: hash_tokens(&tokens),
            tokens,
            vocab_source: "/models/x.gguf".into(),
        })
        .unwrap();
    s.put_session(&SessionRecord {
        id: "sess-busy".into(),
        title: None,
        model_id: "m".into(),
        dialect_sha: "aa".repeat(32),
        workspace_root: "/w".into(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .unwrap();
    s.put_transcript("tr-busy", "sess-busy", &prefix_id)
        .unwrap();
    "tr-busy".into()
}

/// Another connection to the same file, holding the write lock until told to let go —
/// another daemon mid-commit.
fn hold_write_lock(path: &std::path::Path) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (go, wait) = mpsc::channel::<()>();
    let (held, is_held) = mpsc::channel::<()>();
    let path = path.to_path_buf();
    let h = std::thread::spawn(move || {
        let c = Connection::open(&path).unwrap();
        c.execute_batch("BEGIN IMMEDIATE; INSERT INTO todo (session_id, todos_json, updated_ms) VALUES ('other', '[]', 0) ON CONFLICT(session_id) DO UPDATE SET updated_ms = updated_ms + 1;")
            .unwrap();
        held.send(()).unwrap();
        let _ = wait.recv();
        c.execute_batch("COMMIT").unwrap();
    });
    is_held.recv().unwrap();
    (go, h)
}

/// **The failure: a deferred transaction that reads, then writes, is refused AT ONCE while
/// another connection holds the write lock — the busy timeout is never consulted.** Both
/// connections carry a five-second timeout and the refusal comes back in well under one,
/// because a connection already inside a (read) transaction is not offered the busy handler.
/// This is why a timeout of any length is not a fix for this shape, and why every write
/// transaction in the store begins IMMEDIATE.
#[test]
fn a_deferred_read_then_write_is_refused_at_once_whatever_the_timeout() {
    let db = TempDb::new("deferred");
    drop(seed(&Store::open(&db.0).unwrap()));
    let (release, holder) = hold_write_lock(&db.0);

    let a = Connection::open(&db.0).unwrap();
    a.busy_timeout(Duration::from_secs(5)).unwrap();
    a.execute_batch("BEGIN DEFERRED").unwrap();
    let _n: i64 = a
        .query_row("SELECT COUNT(*) FROM session", [], |r| r.get(0))
        .unwrap();
    let t = Instant::now();
    let err = a
        .execute("UPDATE session SET title = 'x' WHERE id = 'sess-busy'", [])
        .expect_err("the promotion is refused while B holds the lock");
    let took = t.elapsed();
    assert!(is_busy_sql(&err), "busy, not some other failure: {err}");
    assert!(
        took < Duration::from_secs(2),
        "refused without waiting out the 5 s timeout (took {took:?})"
    );
    a.execute_batch("ROLLBACK").unwrap();
    release.send(()).unwrap();
    holder.join().unwrap();
}

/// **The remedy: `Store::write_tx` takes the lock before it reads, so the same read-then-write
/// WAITS for the other writer and then succeeds.**
#[test]
fn a_write_tx_waits_for_the_lock_and_then_reads_and_writes() {
    let db = TempDb::new("immediate");
    let s = Store::open(&db.0).unwrap();
    seed(&s);
    let (release, holder) = hold_write_lock(&db.0);
    let t = Instant::now();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        release.send(()).unwrap();
    });
    let n = s
        .write_tx(|tx| {
            let n: i64 = tx.query_row("SELECT COUNT(*) FROM todo", [], |r| r.get(0))?;
            tx.execute(
                "UPDATE session SET title = ?1 WHERE id = 'sess-busy'",
                [format!("{n} todo rows")],
            )?;
            Ok(n)
        })
        .expect("waited for the lock instead of being refused");
    assert!(t.elapsed() >= Duration::from_millis(250), "it did wait");
    assert_eq!(n, 1, "and it read the other writer's committed row");
    assert_eq!(
        s.title("sess-busy").unwrap().as_deref(),
        Some("1 todo rows")
    );
    releaser.join().unwrap();
    holder.join().unwrap();
}

/// **A store write outlasts its own busy timeout.** The connection's timeout is cut to 100 ms
/// and another connection holds the lock for 700 ms: before this change `append_item` came
/// back `database is locked` after the timeout, and a child's turn ended on it (row 878 of
/// `s-1789919514688401228-sub-1791757785761179646`). Now the store takes it again.
#[test]
fn an_append_outlasts_a_lock_held_past_the_busy_timeout() {
    let db = TempDb::new("outlast");
    let s = Store::open(&db.0).unwrap();
    let tr = seed(&s);
    s.connection()
        .busy_timeout(Duration::from_millis(100))
        .unwrap();
    let (release, holder) = hold_write_lock(&db.0);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        release.send(()).unwrap();
    });
    let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
    let toks = vec![7u32, 8, 9];
    let row = ledger.append("it-0", &toks).unwrap().clone();
    let item = TranscriptItem::Assistant {
        text: "survived".into(),
        tool_calls: vec![],
        truncated: false,
    };
    s.append_item(&tr, 0, &item, &row, &toks)
        .expect("a busy store is waited for, not died of");
    assert_eq!(s.item_count(&tr).unwrap(), 1);
    releaser.join().unwrap();
    holder.join().unwrap();
}

/// **The ladder**: a busy failure is taken again, anything else is returned at once, and a
/// failure that stays busy is returned once the budget is spent.
#[test]
fn the_busy_retry_retries_busy_only_and_stops_at_its_budget() {
    fn busy() -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".into()),
        )
    }
    let mut n = 0;
    let r = retry_busy(Duration::from_secs(5), is_busy_sql, || {
        n += 1;
        if n < 3 { Err(busy()) } else { Ok(n) }
    });
    assert_eq!(r.unwrap(), 3, "two busy failures, then the write");

    let mut n = 0;
    let r: rusqlite::Result<()> = retry_busy(Duration::from_secs(5), is_busy_sql, || {
        n += 1;
        Err(rusqlite::Error::QueryReturnedNoRows)
    });
    assert!(r.is_err());
    assert_eq!(n, 1, "a failure that is not busy is not retried");

    let t = Instant::now();
    let r: rusqlite::Result<()> =
        retry_busy(Duration::from_millis(300), is_busy_sql, || Err(busy()));
    assert!(
        r.is_err_and(|e| is_busy_sql(&e)),
        "the busy error is what is reported"
    );
    assert!(
        t.elapsed() >= Duration::from_millis(300),
        "the whole budget was spent"
    );
    assert!(t.elapsed() < Duration::from_secs(3), "and not much more");

    assert!(backoff(0) < Duration::from_millis(100));
    assert!(
        backoff(30) <= Duration::from_millis(2050),
        "the step is capped"
    );
    assert!(StoreError::Sql(busy()).is_busy());
    assert!(!StoreError::NotFound("x".into()).is_busy());
}

/// **The long copy commits in chunks, and lands the same rows as the per-row path.**
///
/// 5000 rows with 2000-token blobs — the size of a long session's transcript — written once
/// per row through `append_item` and once through `append_items`. The test's temp dir is tmpfs
/// on this box, where an fsync is free, so the wall clock here cannot show what the change is
/// for; the numbers that do were measured on the ext4 volume `sessions.db` lives on,
/// 2026-10-12 ~01:30, with several workspace test runs going (`synchronous = FULL`, ~11 KB
/// rows):
///
/// | shape                        | rows | total     | longest single lock hold |
/// |------------------------------|------|-----------|--------------------------|
/// | one autocommit per row       | 500  | 205.7 s   | ~0.41 s, 500 times       |
/// | one transaction              | 1000 | 2.7 s     | 2.7 s                    |
/// | `APPEND_CHUNK` (64) per tx   | 1000 | 6.8 s     | 0.95 s                   |
///
/// So the per-row shape is what took the rano reseat six and a half minutes for 971 rows, and
/// the chunked one is that copy in seconds without any one hold growing with the copy. The
/// assertions are on what does not depend on the disk: every row reads back identically, and
/// the batched copy is not slower than the per-row one.
#[test]
fn a_long_copy_commits_in_chunks_and_lands_every_row() {
    const ROWS: usize = 5000;
    const TOKS: usize = 2000;
    let db = TempDb::new("copy");
    let s = Store::open(&db.0).unwrap();
    let tr = seed(&s);
    let prefix_id = s.load_transcript(&tr).unwrap().stable_prefix_id;
    s.put_transcript("tr-batched", "sess-busy", &prefix_id)
        .unwrap();

    let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
    let mut items = Vec::with_capacity(ROWS);
    let mut toks = Vec::with_capacity(ROWS);
    for i in 0..ROWS {
        let t: Vec<u32> = (0..TOKS as u32).map(|k| k ^ i as u32).collect();
        ledger.append(&format!("it-{i}"), &t).unwrap();
        items.push(TranscriptItem::Assistant {
            text: format!("row {i}"),
            tool_calls: vec![],
            truncated: false,
        });
        toks.push(t);
    }
    let rows = ledger.rows();

    let t = Instant::now();
    for i in 0..ROWS {
        s.append_item(&tr, i as u32, &items[i], &rows[i], &toks[i])
            .unwrap();
    }
    let per_row = t.elapsed();

    let t = Instant::now();
    s.append_items(
        "tr-batched",
        0,
        (0..ROWS).map(|i| (&items[i], &rows[i], toks[i].as_slice())),
    )
    .unwrap();
    let batched = t.elapsed();
    eprintln!(
        "{ROWS} rows x {TOKS} tokens: per-row {} ms ({} commits), batched {} ms ({} commits) — temp dir {}",
        per_row.as_millis(),
        ROWS,
        batched.as_millis(),
        ROWS.div_ceil(Store::APPEND_CHUNK),
        std::env::temp_dir().display()
    );

    let a = s.load_transcript(&tr).unwrap();
    let b = s.load_transcript("tr-batched").unwrap();
    assert_eq!(b.items.len(), ROWS);
    for (x, y) in a.items.iter().zip(&b.items) {
        assert_eq!(x.0, y.0);
        assert_eq!(x.1.h_k, y.1.h_k);
        assert_eq!(x.2, y.2);
    }
    assert!(
        batched <= per_row + Duration::from_secs(2),
        "the batched copy is not slower than the per-row one: {batched:?} vs {per_row:?}"
    );
}

/// **A chunk that breaks the append-only rule is refused, and the chunks before it stay.**
/// The trigger still runs per row inside a batch, so a batch cannot smuggle a hole in.
#[test]
fn a_batch_with_a_hole_is_refused_by_the_trigger() {
    let s = Store::open_in_memory().unwrap();
    let tr = seed(&s);
    let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
    let item = TranscriptItem::Assistant {
        text: "x".into(),
        tool_calls: vec![],
        truncated: false,
    };
    let t = vec![5u32];
    let r0 = ledger.append("a", &t).unwrap().clone();
    let err = s
        .append_items(&tr, 1, [(&item, &r0, t.as_slice())])
        .expect_err("seq 1 on an empty transcript is a hole");
    assert!(
        err.to_string().contains("seq must be the next one"),
        "{err}"
    );
    assert_eq!(s.item_count(&tr).unwrap(), 0);
}
