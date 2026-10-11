//! **`metrics.db`** — the schema, the writer thread, and the 90-day prune.
//!
//! One writer per daemon (`metrics-writer`), owning its own connection to a file nobody else
//! locks: WAL, `synchronous=NORMAL`, `busy_timeout` 2 s. It drains the channel and commits **one
//! transaction per second or per 1,000 rows**, whichever comes first (§2.7). A commit that fails
//! is tried once more and then the batch is dropped and counted — metrics must never wedge the
//! daemon, and eleven daemons write this file side by side.
//!
//! **Retention** (the operator's decision 2): raw rows 90 days; every `operator.feedback` row,
//! and every row of a turn a mark points at, forever; and before a day's rows go, they are rolled
//! up into `daily`, which is kept forever. The prune runs on this thread, once at start and then
//! once a day.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use rusqlite::params;
pub use rusqlite::{self, Connection};

use crate::{CHANNEL, Emitter, Event, Msg, kind, now_ms};

/// The schema this build writes. In `meta` as `schema`; a reader that meets a larger number
/// still reads — the table is kind-tolerant by construction (§2.2).
pub const SCHEMA: u32 = 1;

/// `docs/metrics.md` §2.2, as written. `IF NOT EXISTS` throughout: eleven daemons of mixed
/// builds open this file, and the first one creates it.
pub const DDL: &str = "
CREATE TABLE IF NOT EXISTS event (
    id            INTEGER PRIMARY KEY,
    ts_ms         INTEGER NOT NULL,
    dur_ms        INTEGER,
    daemon        TEXT NOT NULL,
    build         TEXT NOT NULL,
    session_id    TEXT,
    root_id       TEXT,
    transcript_id TEXT,
    turn_id       TEXT,
    round         INTEGER,
    kind          TEXT NOT NULL,
    name          TEXT,
    outcome       TEXT,
    n1 INTEGER, n2 INTEGER, n3 INTEGER,
    attrs         TEXT
);
CREATE INDEX IF NOT EXISTS event_kind_ts    ON event (kind, ts_ms);
CREATE INDEX IF NOT EXISTS event_session_ts ON event (session_id, ts_ms);
CREATE INDEX IF NOT EXISTS event_root_ts    ON event (root_id, ts_ms);
CREATE INDEX IF NOT EXISTS event_turn       ON event (turn_id) WHERE turn_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS daily (
    day TEXT, kind TEXT, name TEXT, model TEXT, outcome TEXT,
    n INTEGER, sum_dur_ms INTEGER, p50_dur_ms INTEGER, p95_dur_ms INTEGER, max_dur_ms INTEGER,
    sum_n1 INTEGER, sum_n2 INTEGER, sum_n3 INTEGER,
    PRIMARY KEY (day, kind, name, model, outcome)
);
";

/// **One view per kind, naming the slots** — shipped by the reader rather than migrated
/// (§2.2), so a new kind is a new line here and never an `ALTER`. `TEMP`, so a read-only
/// connection can make them.
pub const VIEWS: &str = "
CREATE TEMP VIEW IF NOT EXISTS turn_start AS
  SELECT id, ts_ms, daemon, build, session_id, root_id, turn_id, name AS door,
         n1 AS fill_tokens, n2 AS window, n3 AS rounds,
         attrs->>'model' AS model, attrs->>'cause' AS cause, attrs->>'queued_ms' AS queued_ms
  FROM event WHERE kind = 'turn.start';
CREATE TEMP VIEW IF NOT EXISTS turn_end AS
  SELECT id, ts_ms, dur_ms, daemon, build, session_id, root_id, turn_id, name AS model,
         outcome, n1 AS prompt_tokens, n2 AS cached_tokens, n3 AS output_tokens,
         attrs->>'rounds' AS rounds, attrs->>'tool_calls' AS tool_calls,
         attrs->>'tool_errors' AS tool_errors, attrs->>'ttfb_ms' AS ttfb_ms,
         attrs->>'cost_micros_usd' AS cost_micros_usd, attrs->>'door' AS door
  FROM event WHERE kind = 'turn.end';
CREATE TEMP VIEW IF NOT EXISTS round_event AS
  SELECT id, ts_ms, dur_ms, session_id, turn_id, round, name AS model, outcome AS finish,
         n1 AS prompt_tokens, n2 AS cached_tokens, n3 AS output_tokens,
         attrs->>'ttfb_ms' AS ttfb_ms, attrs->>'prompt_ms' AS prompt_ms,
         attrs->>'predicted_ms' AS predicted_ms, attrs->>'ledger_tokens' AS ledger_tokens,
         attrs->>'provider' AS provider
  FROM event WHERE kind = 'round';
CREATE TEMP VIEW IF NOT EXISTS tool_call AS
  SELECT id, ts_ms, dur_ms, session_id, turn_id, round, name AS tool, outcome,
         n1 AS inline_bytes, n2 AS full_bytes, n3 AS peak_kib, attrs->>'cap_kib' AS cap_kib
  FROM event WHERE kind = 'tool.call';
CREATE TEMP VIEW IF NOT EXISTS delivery AS
  SELECT id, ts_ms, dur_ms AS held_ms, session_id, turn_id, round, name AS what,
         outcome AS how, n1 AS batch, attrs->>'rule' AS rule, attrs->>'door' AS door,
         attrs->>'mid_turn' AS mid_turn, attrs->>'handle' AS handle
  FROM event WHERE kind = 'delivery';
CREATE TEMP VIEW IF NOT EXISTS wake AS
  SELECT id, ts_ms, session_id, name AS cause, outcome, n1 AS items, attrs->>'causes' AS causes
  FROM event WHERE kind = 'wake';
CREATE TEMP VIEW IF NOT EXISTS worker_cmd AS
  SELECT id, ts_ms, dur_ms, daemon, build, session_id, name AS verb, outcome,
         n1 AS queued_ms, n2 AS depth
  FROM event WHERE kind = 'worker.cmd';
CREATE TEMP VIEW IF NOT EXISTS feedback AS
  SELECT id, ts_ms, session_id, root_id, turn_id, round, name AS polarity, outcome,
         attrs->>'note' AS note, attrs->>'tags' AS tags, attrs->>'item_id' AS item_id,
         attrs->>'head' AS head, attrs->>'identity' AS identity, attrs->>'at_state' AS at_state
  FROM event WHERE kind = 'operator.feedback';
";

/// `[metrics]`, resolved — what the daemon read from `providers.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    /// Raw rows older than this many days are rolled up and pruned (decision 2: 90).
    pub keep_days: u64,
    /// The kinds that may keep `attrs.snippet` (§2.8). Decision 5: `context.claim` keeps its
    /// sentence by default while R71's floor is tuned.
    pub snippets: Vec<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            enabled: true,
            keep_days: 90,
            snippets: vec![kind::CONTEXT_CLAIM.to_string()],
        }
    }
}

impl Settings {
    /// From `[metrics]`'s raw strings: `enabled = false|off|no|0` switches off, `keep_days` a
    /// number, `snippets` kinds separated by spaces or commas. Anything unreadable is the
    /// default — a metrics setting is never a reason a daemon does not start.
    pub fn from_raw(
        enabled: Option<&str>,
        keep_days: Option<&str>,
        snippets: Option<&str>,
    ) -> Settings {
        let d = Settings::default();
        Settings {
            enabled: match enabled.map(|s| s.trim().to_ascii_lowercase()) {
                Some(s) if ["false", "off", "no", "0"].contains(&s.as_str()) => false,
                _ => d.enabled,
            },
            keep_days: keep_days
                .and_then(|s| s.trim().parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(d.keep_days),
            snippets: snippets
                .map(|s| {
                    s.split([' ', ',', '\t'])
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or(d.snippets),
        }
    }
}

/// **Where the file is**: `$LETIBOT_METRICS_DB`, else beside the store the daemon was given —
/// which on the operator's box is `~/.local/share/letibot/metrics.db` — so a test's temp store
/// gets a temp metrics file and never the operator's.
pub fn path_beside(store: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("LETIBOT_METRICS_DB") {
        return PathBuf::from(p);
    }
    store
        .parent()
        .map(|d| d.join("metrics.db"))
        .unwrap_or_else(|| PathBuf::from("metrics.db"))
}

/// The default file, for a reader with no store in hand (`letibot stats`).
pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os("LETIBOT_METRICS_DB") {
        return PathBuf::from(p);
    }
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_default()
        .join("letibot")
        .join("metrics.db")
}

/// Open (creating) the file for writing: the pragmas, the schema, the mode.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let conn = Connection::open(path)?;
    // 0600: §2.8 — the file holds the operator's own notes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    conn.busy_timeout(Duration::from_millis(2000))?;
    // Before the first table, or it does nothing: the prune's `incremental_vacuum` needs it.
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(DDL)?;
    conn.execute(
        "INSERT OR IGNORE INTO meta (key, value) VALUES ('schema', ?1)",
        params![SCHEMA.to_string()],
    )?;
    Ok(conn)
}

/// **Who is writing** — the daemon (`host/pid/start_ms`, the form `queue_transition.by` uses)
/// and its build, stamped on every row because mixed builds are normal here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub daemon: String,
    pub build: String,
}

/// **The writer's state**, apart from its thread so a test can drive a batch by hand.
pub struct Writer {
    conn: Connection,
    id: Identity,
    settings: Settings,
    /// session → parent, taught by [`crate::session_parent`]; `root_id` is the walk's end.
    parents: HashMap<String, Option<String>>,
    /// Batches lost to a commit that failed twice.
    pub lost: u64,
    last_prune: Option<u64>,
}

/// §2.7: a batch a second, or sooner at this many rows.
pub const BATCH_ROWS: usize = 1000;
pub const BATCH_EVERY: Duration = Duration::from_secs(1);
const DAY_MS: u64 = 86_400_000;

impl Writer {
    pub fn new(conn: Connection, id: Identity, settings: Settings) -> Writer {
        Writer {
            conn,
            id,
            settings,
            parents: HashMap::new(),
            lost: 0,
            last_prune: None,
        }
    }

    pub fn learn_parent(&mut self, session: String, parent: Option<String>) {
        self.parents.insert(session, parent);
    }

    /// The root of `session`'s tree, as far as this writer was told. Bounded, so a cycle a
    /// bug made cannot hold the thread.
    pub fn root_of(&self, session: &str) -> String {
        let mut at = session;
        for _ in 0..64 {
            match self.parents.get(at) {
                Some(Some(p)) => at = p,
                _ => break,
            }
        }
        at.to_string()
    }

    /// **One transaction for the batch**, tried twice. `Ok(n)` rows written.
    pub fn write_batch(&mut self, batch: &[Event]) -> rusqlite::Result<usize> {
        if batch.is_empty() {
            return Ok(0);
        }
        match self.try_write(batch) {
            Ok(n) => Ok(n),
            Err(_) => self.try_write(batch).inspect_err(|_| self.lost += 1),
        }
    }

    fn try_write(&mut self, batch: &[Event]) -> rusqlite::Result<usize> {
        let roots: Vec<Option<String>> = batch
            .iter()
            .map(|e| {
                e.root_id
                    .clone()
                    .or_else(|| e.session_id.as_deref().map(|s| self.root_of(s)))
            })
            .collect();
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO event (ts_ms, dur_ms, daemon, build, session_id, root_id, \
                 transcript_id, turn_id, round, kind, name, outcome, n1, n2, n3, attrs) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )?;
            for (e, root) in batch.iter().zip(roots) {
                let keep_snippet = self.settings.snippets.iter().any(|k| k == e.kind);
                let attrs: serde_json::Map<String, serde_json::Value> = e
                    .attrs
                    .iter()
                    .filter(|(k, _)| keep_snippet || *k != "snippet")
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect();
                let attrs =
                    (!attrs.is_empty()).then(|| serde_json::Value::Object(attrs).to_string());
                st.execute(params![
                    e.ts_ms as i64,
                    e.dur_ms.map(|d| d as i64),
                    self.id.daemon,
                    self.id.build,
                    e.session_id,
                    root,
                    e.transcript_id,
                    e.turn_id,
                    e.round,
                    e.kind,
                    e.name,
                    e.outcome,
                    e.n[0],
                    e.n[1],
                    e.n[2],
                    attrs,
                ])?;
            }
        }
        tx.commit()?;
        Ok(batch.len())
    }

    /// **Roll up, then prune, the rows older than `keep_days`** — except the marks and every
    /// row of a marked turn, which are the labelled data and kept forever (decision 2).
    ///
    /// The rollup is `INSERT OR IGNORE` per day, so a day two daemons both prune is summed once
    /// — by whichever got there first, from the rows that were all still there. Percentiles
    /// are left `NULL` in phase 1: the `daily` columns are the design's, the arithmetic is
    /// phase 2's. Returns the rows deleted.
    pub fn prune(&mut self, now_ms: u64) -> rusqlite::Result<usize> {
        let cutoff = now_ms.saturating_sub(self.settings.keep_days.saturating_mul(DAY_MS));
        // Whole days only: a day is rolled up when every row of it is past the cutoff, so a
        // rollup never counts half a day and then misses the rest.
        let cutoff = cutoff - cutoff % DAY_MS;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO daily (day, kind, name, model, outcome, n, sum_dur_ms, \
             max_dur_ms, sum_n1, sum_n2, sum_n3) \
             SELECT date(ts_ms / 1000, 'unixepoch'), kind, COALESCE(name, ''), \
                    COALESCE(attrs->>'model', CASE WHEN kind IN ('round','turn.end') THEN name END, ''), \
                    COALESCE(outcome, ''), COUNT(*), SUM(dur_ms), MAX(dur_ms), \
                    SUM(n1), SUM(n2), SUM(n3) \
             FROM event WHERE ts_ms < ?1 GROUP BY 1, 2, 3, 4, 5",
            params![cutoff as i64],
        )?;
        let gone = tx.execute(
            "DELETE FROM event WHERE ts_ms < ?1 AND kind <> 'operator.feedback' \
             AND (turn_id IS NULL OR turn_id NOT IN \
                  (SELECT turn_id FROM event WHERE kind = 'operator.feedback' \
                   AND turn_id IS NOT NULL))",
            params![cutoff as i64],
        )?;
        tx.commit()?;
        self.last_prune = Some(now_ms);
        if gone > 0 {
            let _ = self.conn.execute_batch("PRAGMA incremental_vacuum;");
        }
        Ok(gone)
    }

    fn prune_due(&self, now_ms: u64) -> bool {
        self.last_prune
            .is_none_or(|t| now_ms.saturating_sub(t) >= DAY_MS)
    }
}

/// **What [`start`] hands back** — the thread, for a clean join at shutdown.
pub struct Running {
    pub path: PathBuf,
    pub thread: std::thread::JoinHandle<()>,
}

/// **Open the file, start the writer, install the emitter.** `Ok(None)` when `[metrics]` is off
/// or another writer is already installed in this process; an error when the file could not be
/// opened — which the daemon reports and starts without, never the reverse.
pub fn start(path: &Path, id: Identity, settings: Settings) -> rusqlite::Result<Option<Running>> {
    if !settings.enabled {
        return Ok(None);
    }
    let conn = open(path)?;
    let (emitter, rx) = Emitter::new(CHANNEL);
    // Installed before the thread starts: an emit between the two queues instead of vanishing.
    if !crate::install(emitter) {
        return Ok(None);
    }
    let mut w = Writer::new(conn, id, settings);
    let thread = std::thread::Builder::new()
        .name("metrics-writer".into())
        .spawn(move || serve(&mut w, rx, crate::dropped))
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    Ok(Some(Running {
        path: path.to_path_buf(),
        thread,
    }))
}

/// **The writer's loop.** `dropped` reads the emitter's counter; a rise since the last batch is
/// written as a `metrics.dropped` row, which is the writer reporting on its own health.
pub fn serve(w: &mut Writer, rx: Receiver<Msg>, dropped: impl Fn() -> u64) {
    let mut reported = 0u64;
    loop {
        let deadline = Instant::now() + BATCH_EVERY;
        let mut batch: Vec<Event> = Vec::new();
        let mut flushers: Vec<SyncSender<()>> = Vec::new();
        let mut closed = false;
        while batch.len() < BATCH_ROWS {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(Msg::Event(e)) => batch.push(*e),
                Ok(Msg::Parent { session, parent }) => w.learn_parent(session, parent),
                Ok(Msg::Flush(tx)) => {
                    flushers.push(tx);
                    break;
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    closed = true;
                    break;
                }
            }
        }
        let now = dropped();
        if now > reported {
            batch.push(Event::new(kind::DROPPED).n1(now - reported));
            reported = now;
        }
        let _ = w.write_batch(&batch);
        for f in flushers {
            let _ = f.try_send(());
        }
        let t = now_ms();
        if w.prune_due(t) {
            let _ = w.prune(t);
        }
        if closed {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "letibot-metrics-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d.join("metrics.db")
    }

    fn writer(path: &Path) -> Writer {
        Writer::new(
            open(path).unwrap(),
            Identity {
                daemon: "box/1/0".into(),
                build: "test".into(),
            },
            Settings::default(),
        )
    }

    fn count(path: &Path, sql: &str) -> i64 {
        Connection::open(path)
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_batch_is_one_transaction_with_the_root_filled_and_snippets_kept_only_where_allowed() {
        let p = temp("batch");
        let mut w = writer(&p);
        w.learn_parent("child".into(), Some("root".into()));
        w.learn_parent("grandchild".into(), Some("child".into()));
        let batch = vec![
            Event::new(kind::TOOL_CALL)
                .session("grandchild")
                .name("bash")
                .n1(10)
                .attr("snippet", "rm -rf"),
            Event::new(kind::CONTEXT_CLAIM)
                .session("root")
                .attr("snippet", "my context is full"),
        ];
        assert_eq!(w.write_batch(&batch).unwrap(), 2);
        assert_eq!(
            count(&p, "SELECT COUNT(*) FROM event WHERE root_id = 'root'"),
            2
        );
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE attrs->>'snippet' IS NOT NULL"
            ),
            1,
            "a tool call's snippet is stripped; a context claim's is kept (decision 5)"
        );
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE daemon = 'box/1/0' AND build = 'test'"
            ),
            2
        );
    }

    /// **The writer batches**: many emits within a second are one commit, and a flush answers
    /// only after they are on disk.
    #[test]
    fn the_writer_drains_the_channel_in_batches_and_reports_its_drops() {
        let p = temp("serve");
        let mut w = writer(&p);
        let (e, rx) = Emitter::new(16);
        let e = std::sync::Arc::new(e);
        // Weak: the writer must see the channel close when the test drops its emitter.
        let e2 = std::sync::Arc::downgrade(&e);
        let t = std::thread::spawn(move || {
            serve(&mut w, rx, move || e2.upgrade().map_or(0, |e| e.dropped()))
        });
        for i in 0..40u64 {
            // Some of these will not fit: the writer is draining a 16-slot channel.
            e.send(Msg::Event(Box::new(Event::new(kind::ROUND).n1(i))));
        }
        assert!(e.flush(Duration::from_secs(5)));
        let rounds = count(&p, "SELECT COUNT(*) FROM event WHERE kind = 'round'");
        let dropped: i64 = count(
            &p,
            "SELECT COALESCE(SUM(n1), 0) FROM event WHERE kind = 'metrics.dropped'",
        );
        assert_eq!(
            rounds + dropped,
            40,
            "every event is either written or counted as dropped"
        );
        drop(e);
        t.join().unwrap();
    }

    #[test]
    fn retention_prunes_old_rows_but_keeps_marks_their_turns_and_a_rollup() {
        let p = temp("prune");
        let mut w = writer(&p);
        let now = 200 * DAY_MS;
        let old = now - 100 * DAY_MS;
        let batch = vec![
            Event::new(kind::ROUND)
                .at(old)
                .turn("t-old", Some(0))
                .name("glm"),
            Event::new(kind::ROUND)
                .at(old)
                .turn("t-marked", Some(0))
                .name("glm"),
            Event::new(kind::FEEDBACK)
                .at(old)
                .turn("t-marked", Some(0))
                .name("bad"),
            Event::new(kind::ROUND)
                .at(now - DAY_MS)
                .turn("t-new", Some(0))
                .name("glm"),
        ];
        w.write_batch(&batch).unwrap();
        let gone = w.prune(now).unwrap();
        assert_eq!(gone, 1, "only the unmarked old round goes");
        assert_eq!(
            count(&p, "SELECT COUNT(*) FROM event WHERE turn_id = 't-marked'"),
            2,
            "a marked turn is kept whole, forever"
        );
        assert_eq!(
            count(
                &p,
                "SELECT SUM(n) FROM daily WHERE kind = 'round' AND model = 'glm'"
            ),
            2,
            "the old day was rolled up before its rows went"
        );
        // A second prune the same day is a no-op, and the rollup is not counted twice.
        w.prune(now).unwrap();
        assert_eq!(
            count(&p, "SELECT SUM(n) FROM daily WHERE kind = 'round'"),
            2
        );
    }

    #[test]
    fn settings_read_the_switch_and_default_on() {
        assert!(Settings::from_raw(None, None, None).enabled);
        assert!(!Settings::from_raw(Some("false"), None, None).enabled);
        assert!(!Settings::from_raw(Some(" Off "), None, None).enabled);
        assert_eq!(Settings::from_raw(None, Some("30"), None).keep_days, 30);
        assert_eq!(Settings::from_raw(None, Some("junk"), None).keep_days, 90);
        assert_eq!(
            Settings::from_raw(None, None, Some("warning, tool.call")).snippets,
            vec!["warning", "tool.call"]
        );
        assert_eq!(
            Settings::default().snippets,
            vec!["context.claim"],
            "decision 5: the claim sentence is kept by default"
        );
    }

    #[test]
    fn the_views_load_on_a_read_only_connection() {
        let p = temp("views");
        writer(&p);
        let c =
            Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        c.execute_batch(VIEWS).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM turn_end", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
