//! The failure mode, its remedy, the retry ladder and the long copy, each against a real WAL
//! file with two connections, because an in-memory store has no second connection to race.

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
            effort: None,
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

/// Another connection to the same file, holding the write lock until told to let go  -
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

/// The failure: a deferred transaction that reads, then writes, is refused AT ONCE while another
/// connection holds the write lock - the busy timeout is never consulted, because a connection
/// already inside a (read) transaction is not offered the busy handler. So every write begins IMMEDIATE.
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

/// The remedy: `Store::write_tx` takes the lock before it reads, so the same read-then-write
/// WAITS for the other writer and then succeeds.
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

/// A store write outlasts its own busy timeout. The connection's timeout is cut to 100 ms and
/// another connection holds the lock for 700 ms. `append_item` must retry, not return
/// `database is locked` and end a child's turn.
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

/// A queue write waiting on another process's lock does not hold this process's connection.
/// `acquire_queue_lease` begins the transaction inside the retry closure; taking the connection
/// first would lock the whole process out for every wait.
#[test]
fn a_queue_write_waiting_on_another_process_leaves_the_store_readable() {
    let db = TempDb::new("queue-wait");
    let s = Store::open(&db.0).unwrap();
    seed(&s);
    // The production busy handler, not a short `busy_timeout`: the handler is what sleeps holding
    // the connection, so a test that swaps it out cannot see the stall.
    let (release, holder) = hold_write_lock(&db.0);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(HELD);
        release.send(()).unwrap();
    });

    let (done, is_done) = mpsc::channel::<()>();
    let mut finished = false;
    let mut worst = Duration::ZERO;
    let mut reads = 0usize;
    let deadline = Instant::now() + Duration::from_secs(10);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let out = s.acquire_queue_lease(
                "/w",
                &LeaseClaim {
                    owner: "test-daemon".into(),
                    pid: std::process::id(),
                    host: "this box".into(),
                    build: "test".into(),
                },
                1_000,
                60_000,
            );
            assert!(
                matches!(out, Ok(LeaseOutcome::Held { .. })),
                "the claim waits out another process's commit: {out:?}"
            );
            done.send(()).unwrap();
        });
        while !finished && Instant::now() < deadline {
            match is_done.try_recv() {
                Ok(()) => {
                    finished = true;
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let t = Instant::now();
            let read = s.title("sess-busy");
            worst = worst.max(t.elapsed());
            reads += 1;
            assert!(
                read.is_ok(),
                "a read while another process writes: {read:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    releaser.join().unwrap();
    holder.join().unwrap();
    assert!(finished, "the claim finished inside ten seconds");
    assert!(reads > 0, "the read loop ran");
    eprintln!(
        "queue write: {reads} reads while the claim waited on another process, worst {worst:?}"
    );
    assert!(
        worst < HELD / 2,
        "a read in this daemon waited {worst:?} for another process's write lock, over {reads} \
         reads: the queue's write held the process's one connection while it waited, so every \
         reader in the daemon waited with it"
    );
}

/// How long [`hold_write_lock`] holds in the tests that measure a reader beside a waiting write:
/// long against [`super::LADDER_ATTEMPT`], so a loaded box cannot blur the two.
const HELD: Duration = Duration::from_secs(2);

/// The daemon-start stall: `life_started` waited on another daemon's write lock with the process's
/// connection held, so the boot thread's next store call sat behind it. An `exec` write waits with
/// the connection released, like a `write_tx`.
#[test]
fn an_exec_write_waiting_on_another_process_leaves_the_store_readable() {
    let db = TempDb::new("exec-wait");
    let s = Store::open(&db.0).unwrap();
    seed(&s);
    let (release, holder) = hold_write_lock(&db.0);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(HELD);
        release.send(()).unwrap();
    });
    let (done, is_done) = mpsc::channel::<()>();
    let mut worst = Duration::ZERO;
    let mut reads = 0usize;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            s.life_started("i-wait", 1)
                .expect("the start waits out another process's commit");
            done.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while is_done.try_recv().is_err() && Instant::now() < deadline {
            let t = Instant::now();
            s.title("sess-busy")
                .expect("a read beside the waiting write");
            worst = worst.max(t.elapsed());
            reads += 1;
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    releaser.join().unwrap();
    holder.join().unwrap();
    eprintln!("exec write: {reads} reads while it waited on another process, worst {worst:?}");
    assert!(
        s.life("i-wait").unwrap().is_some(),
        "the write landed after the lock went"
    );
    assert!(
        worst < HELD / 2,
        "a read waited {worst:?} behind an `exec` write that was waiting on another process: \
         the busy handler slept with this process's one connection held"
    );
}

/// A call that waits for this process's connection while another thread holds it says so, rather
/// than `waited 0 ms` on a line that then reads as a slow query.
#[test]
fn a_wait_for_this_process_connection_is_on_the_line_as_queued() {
    let db = TempDb::new("queued");
    let s = Store::open(&db.0).unwrap();
    seed(&s);
    let (held, is_held) = mpsc::channel::<()>();
    let lines = std::thread::scope(|scope| {
        scope.spawn(|| {
            let guard = s.connection();
            held.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(1200));
            drop(guard);
        });
        is_held.recv().unwrap();
        let (out, lines) = super::collecting(|| s.life_started("i-queued", 1));
        out.expect("the write ran once the connection was free");
        lines
    });
    assert_eq!(lines.len(), 1, "one slow call, one line: {lines:?}");
    let line = &lines[0];
    assert!(
        line.contains("(waited 0 ms, 0 busy retries, queued "),
        "{line}"
    );
    let queued = number_after(line, "queued ");
    assert!(
        (900..=number_after(line, "took ")).contains(&queued),
        "the 1.2 s the other thread held the connection is the queued part: {line}"
    );
}

/// The ladder: a busy failure is taken again, anything else is returned at once, and a
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

/// The long copy commits in chunks, and lands the same rows as the per-row path. The temp dir
/// here is tmpfs, so the wall clock cannot show what the change is for; the assertions are on what
/// does not depend on the disk: every row reads back identically and the batched copy is not slower.
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

/// A chunk that breaks the append-only rule is refused, and the chunks before it stay.
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

/// A store call over a second is one line naming where it was called from; a quick one is none.
#[test]
fn a_slow_store_call_is_one_line_naming_its_caller() {
    #[track_caller]
    fn here() -> &'static std::panic::Location<'static> {
        std::panic::Location::caller()
    }
    let at = here();
    assert_eq!(
        super::slow_line(Duration::from_millis(999), super::Waits::default(), 0, at),
        None,
        "a call under a second is not logged"
    );
    let waits = super::Waits {
        lock: Duration::from_millis(1200),
        queued: Duration::ZERO,
    };
    let line = super::slow_line(Duration::from_millis(1834), waits, 12, at).expect("a slow call");
    assert!(line.contains("tests.rs:"), "{line}");
    assert!(
        line.contains("took 1834 ms (waited 1200 ms, 12 busy retries)"),
        "{line}"
    );
    let waits = super::Waits {
        lock: Duration::ZERO,
        queued: Duration::from_millis(31_900),
    };
    let line = super::slow_line(Duration::from_millis(32_080), waits, 0, at).expect("a slow call");
    assert!(
        line.contains(
            "took 32080 ms (waited 0 ms, 0 busy retries, queued 31900 ms behind another thread)"
        ),
        "the in-process wait is on the line, so `waited 0` no longer reads as a slow query: {line}"
    );
}

/// The number after `label` in a line: a measurement asserted, not a sentence spelt out.
fn number_after(line: &str, label: &str) -> u64 {
    line.split(label)
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no number after {label:?} in {line:?}"))
}

/// The number just before `label` in a line, for a count that closes a phrase.
fn number_before(line: &str, label: &str) -> u64 {
    line.split(label)
        .next()
        .and_then(|rest| rest.split_whitespace().next_back())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no number before {label:?} in {line:?}"))
}

/// P0's done-test: a write serialised behind another connection's transaction reports the WAIT.
/// The lock is held 300 ms and the body then spends a second of its own, so the line says which
/// of the two the call spent its time on, the handler's sleep and the ladder's pauses together.
#[test]
fn a_lock_wait_is_on_the_line_as_waited_and_the_body_is_not() {
    let db = TempDb::new("waited");
    let s = Store::open(&db.0).unwrap();
    seed(&s);
    let (release, holder) = hold_write_lock(&db.0);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        release.send(()).unwrap();
    });
    let (out, lines) = super::collecting(|| {
        s.write_tx(|tx| {
            tx.execute(
                "UPDATE session SET title = 'waited' WHERE id = 'sess-busy'",
                [],
            )?;
            std::thread::sleep(Duration::from_millis(1000));
            Ok(())
        })
    });
    out.expect("the write waited for the other connection and then ran");
    releaser.join().unwrap();
    holder.join().unwrap();

    assert_eq!(lines.len(), 1, "one slow call, one line: {lines:?}");
    let line = &lines[0];
    eprintln!("lock wait: {line}");
    let took = number_after(line, "took ");
    let waited = number_after(line, "waited ");
    // The 300 ms is counted from the releaser's spawn, so a loaded box starts the write late and
    // sees less of it. 100 still tells the wait from nothing, 700 from the second-long body.
    assert!(
        waited >= 100,
        "the 300 ms lock wait is on the line as waited {waited} ms: {line}"
    );
    assert!(
        (100..=700).contains(&waited),
        "and it is the wait, not the whole call: waited {waited} ms of {took} ms: {line}"
    );
    assert!(
        took >= 1200,
        "the body's own second is in the call: took {took} ms: {line}"
    );
    assert!(
        line.contains("busy/tests.rs:"),
        "the line names the caller's file:line: {line}"
    );
}

/// The other half: a call that is slow with nobody to wait for says `waited 0 ms`, which is how
/// a log tells the disk from a lock.
#[test]
fn an_uncontended_slow_call_reports_no_wait() {
    let db = TempDb::new("uncontended");
    let s = Store::open(&db.0).unwrap();
    seed(&s);
    let (out, lines) = super::collecting(|| {
        s.write_tx(|tx| {
            tx.execute(
                "UPDATE session SET title = 'slow' WHERE id = 'sess-busy'",
                [],
            )?;
            std::thread::sleep(Duration::from_millis(1100));
            Ok(())
        })
    });
    out.expect("the write ran");
    assert_eq!(lines.len(), 1, "one slow call, one line: {lines:?}");
    let line = &lines[0];
    assert!(number_after(line, "took ") >= 1000, "{line}");
    assert!(
        line.contains("(waited 0 ms, 0 busy retries)"),
        "nothing held the lock, so nothing waited: {line}"
    );
}

/// A call under [`super::SLOW_CALL`] writes nothing at all.
#[test]
fn a_call_under_the_slow_threshold_writes_nothing() {
    let db = TempDb::new("quiet");
    let s = Store::open(&db.0).unwrap();
    seed(&s);
    let (out, lines) = super::collecting(|| s.title("sess-busy"));
    assert!(out.unwrap().is_none(), "the row the read went after");
    let (out, more) = super::collecting(|| s.list_sessions());
    assert_eq!(out.unwrap().len(), 1, "and a whole session list");
    assert!(lines.is_empty() && more.is_empty(), "{lines:?} {more:?}");
}

/// The other failure the store counts: a write that lost the lock and a filesystem that refused
/// one are each named on a line of their own, with how many of that kind this site has seen.
#[test]
fn a_refused_write_is_counted_by_kind_and_call_site() {
    fn attempt(code: i32, said: &'static str) -> rusqlite::Result<()> {
        super::retry_busy(Duration::from_millis(20), is_busy_sql, || {
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                Some(said.into()),
            ))
        })
    }
    let (out, first) =
        super::collecting(|| attempt(rusqlite::ffi::SQLITE_BUSY, "database is locked"));
    assert!(out.is_err_and(|e| is_busy_sql(&e)), "it reached the caller");
    assert_eq!(first.len(), 1, "one line per failed call: {first:?}");
    let locked = &first[0];
    assert!(locked.contains("failed after "), "{locked}");
    assert!(locked.contains("tests.rs:"), "{locked}");
    assert!(locked.ends_with(": database is locked"), "{locked}");
    let once = number_before(locked, " at this site");
    assert_eq!(once, 1, "the first of its kind here: {locked}");

    let (_, second) =
        super::collecting(|| attempt(rusqlite::ffi::SQLITE_BUSY, "database is locked"));
    assert_eq!(
        number_before(&second[0], " at this site"),
        once + 1,
        "the second failure at that site counts up: {second:?}"
    );

    let (out, full) =
        super::collecting(|| attempt(rusqlite::ffi::SQLITE_FULL, "database or disk is full"));
    assert!(out.is_err(), "a full disk reaches the caller too");
    assert_eq!(full.len(), 1, "{full:?}");
    assert!(
        full[0].ends_with(": database or disk is full"),
        "{}",
        full[0]
    );
    assert!(
        number_before(&full[0], " at this site") == 1,
        "its own count, apart from the locked one: {}",
        full[0]
    );
}
