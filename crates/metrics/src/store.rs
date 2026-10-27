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
//!
//! # The two caps — a rate and a size (`docs/metrics.md` §2.9)
//!
//! Age alone does not bound this file. On 2026-10-27 an old build's session spun for 20 hours
//! handing `interrupt` back to its own queue, and each pass emitted a `worker.cmd` row: **1,400
//! rows a second, 270,214,514 of them**, and an `metrics.db` of **89 GB**. Retention was never
//! going to catch that — 90 days of it is 90 days of disk.
//!
//! So the writer holds two limits, and neither can gain anything by moving to the emitting site:
//! coalescing means *holding events back until the interval closes*, which a fire-and-forget
//! `try_send` cannot do, and the writer is the one place that sees every event one daemon writes
//! and is off the hot path (the emitter's contract is "never blocks, never touches SQL").
//!
//! - **A rate** ([`RateCap`]): more than [`RATE_BUDGET`] identical events a second — one key per
//!   `(session_id, kind, name, outcome)` — and the rest of that second becomes ONE row carrying
//!   the count it held back (`attrs.suppressed`), plus a `metrics.suppressed` report per key per
//!   minute that also goes to the daemon's log. Under the budget nothing changes: the rows are
//!   byte-identical to what this writer wrote before the cap existed.
//! - **A size** ([`Writer::enforce_size`]): over `[metrics] max_mb` the oldest whole days are
//!   rolled up and deleted until the file is under a low-water mark, in bounded steps; then
//!   `PRAGMA incremental_vacuum` gives the pages back. Checked at start and every
//!   [`SIZE_EVERY`] — once a day would be ~24 GB behind a storm like the one above.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use rusqlite::params;
pub use rusqlite::{self, Connection};

use crate::{CHANNEL, Emitter, Event, Msg, Value, kind, now_ms};

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
    /// The most identical events one key may write per second (§2.9). `0` is no cap — the
    /// honest case for it is a session that is *debugging* a defect and wants every row.
    pub rate_per_sec: u32,
    /// The file's ceiling in MiB (§2.9). `0` is no ceiling.
    pub max_mb: u64,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            enabled: true,
            keep_days: 90,
            snippets: vec![kind::CONTEXT_CLAIM.to_string()],
            rate_per_sec: RATE_BUDGET,
            max_mb: MAX_MB,
        }
    }
}

impl Settings {
    /// From `[metrics]`'s raw strings: `enabled = false|off|no|0` switches off, `keep_days` a
    /// number, `snippets` kinds separated by spaces or commas, `rate_per_sec` and `max_mb`
    /// numbers (`0` is "no cap"). Anything unreadable is the default — a metrics setting is
    /// never a reason a daemon does not start.
    pub fn from_raw(
        enabled: Option<&str>,
        keep_days: Option<&str>,
        snippets: Option<&str>,
        rate_per_sec: Option<&str>,
        max_mb: Option<&str>,
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
            rate_per_sec: rate_per_sec
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(d.rate_per_sec),
            max_mb: max_mb
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(d.max_mb),
        }
    }

    /// The size cap in bytes; `0` is no cap (§2.9).
    pub fn max_bytes(&self) -> u64 {
        self.max_mb.saturating_mul(1024 * 1024)
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

/// **The rate cap's budget: identical events per key per second** (§2.9).
///
/// Chosen from what the emit sites can honestly produce, because the file that produced the
/// 2026-10-27 measurement is on the operator's Mac and not visible from this box. Per key —
/// `(session_id, kind, name, outcome)` — the fastest *honest* key in this tree is `worker.pass`,
/// which has a 250 ms floor of its own (`instruments::worker_pass`) and so cannot exceed **4/s**;
/// then `round` and `tool.call` at ~0.2/s (a turn's 18 rounds over two minutes), `worker.cmd` at a
/// command a second at most (each one takes a round), and `mem.sample` at 1/300 s. A budget of
/// **20/s** is five times the worst honest key and cuts the loop that was measured — 1,400
/// identical rows a second — to 20 rows and one summary, which is what makes the file's growth a
/// function of the cap rather than of the defect.
pub const RATE_BUDGET: u32 = 20;

/// One rate window. A second, because a per-second rate is the number the incident is described
/// in and the granularity the rest of this file's timestamps already have (`ts_ms`).
pub const WINDOW_MS: u64 = 1000;

/// How often the same key may report that it is being suppressed — and the log line that goes
/// with it. A key that is over budget for a minute has already said everything it can.
pub const ALERT_EVERY_MS: u64 = 60_000;

/// The rate cap's key map's ceiling. Stale keys are dropped at every window boundary, so a full
/// map means one second really did hold this many distinct keys; at that point the map is written
/// out and cleared rather than grown (bounded memory on a writer thread that must never wedge).
pub const MAX_KEYS: usize = 4096;

/// **The size cap's default: 2048 MiB** (§2.9).
///
/// The age rule is normally the binding one, and at the design's ~10–12 MB/day (§2.4) 90 days is
/// ~1.1 GB — so 2 GiB is a little over twice a full retention window, which is the point: the size
/// cap must not fire on a healthy file, and it must fire long before a disk is gone. The storm
/// that produced this cap wrote ~1 GB an hour, so 2 GiB is about two hours of it against the 89 GB
/// it actually reached.
pub const MAX_MB: u64 = 2048;

/// The size cap's low-water mark, as a percentage of the ceiling: once over the cap the writer
/// deletes down to this, so the next pass is not a delete of one row.
pub const SIZE_LOW_PCT: u64 = 80;

/// How often the writer looks at the file's size. Not the prune's day: the measured storm grew
/// the file ~1 GB an hour, so a day's check would be ~24 GB behind it. Reading the size is two
/// pragmas off the header, and a pass under the cap does that and returns.
pub const SIZE_EVERY: Duration = Duration::from_secs(10);

/// The most rows one size pass deletes. A pass that hits it stops and the next one continues, so
/// a 270M-row file shrinks in steps rather than wedging the daemon for an hour.
pub const SIZE_ROWS_PER_PASS: usize = 250_000;

/// Rows one delete statement removes — one rowid range, so the delete is a rowid walk and not a
/// scan of the table per chunk.
pub const SIZE_CHUNK: i64 = 50_000;

/// How many whole chunks a pass may skip (finding only forever rows) before it decides the oldest
/// days it can reach are gone. Bounded, so a file whose oldest rows are all marks still returns.
pub const SIZE_SKIP_CHUNKS: usize = 4;

/// Pages `PRAGMA incremental_vacuum` may give back per call — 1,024 pages is 4 MiB at this file's
/// page size, which is a bounded wait on a thread that also writes the batch.
pub const SIZE_PAGES_PER_PASS: i64 = 1024;

/// The cap's key: what makes two events "the same event". `kind` is `&'static str` because a
/// summary row is an [`Event`], and an event's kind is always one of [`kind`]'s constants.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    session: Option<String>,
    kind: &'static str,
    name: Option<String>,
    outcome: Option<String>,
}

impl Key {
    fn of(e: &Event) -> Key {
        Key {
            session: e.session_id.clone(),
            kind: e.kind,
            name: e.name.clone(),
            outcome: e.outcome.clone(),
        }
    }

    /// `session|kind|name|outcome` — one string a log line, a `metrics.suppressed` row and a
    /// reader can all answer *which key?* from.
    fn text(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.session.as_deref().unwrap_or("-"),
            self.kind,
            self.name.as_deref().unwrap_or("-"),
            self.outcome.as_deref().unwrap_or("-")
        )
    }
}

/// One key's state in one window: how many rows it already has, and what it held back.
#[derive(Debug)]
struct Slot {
    window_ms: u64,
    written: u32,
    suppressed: u64,
    /// Every event of this window, written or held back — the rate, for the alert.
    total: u64,
    /// When this key last said it was suppressing (`0` = never, so the first window reports).
    last_alert_ms: u64,
}

impl Slot {
    fn new(window_ms: u64) -> Slot {
        Slot {
            window_ms,
            written: 0,
            suppressed: 0,
            total: 0,
            last_alert_ms: 0,
        }
    }
}

/// **The rate cap** — see the module header and `docs/metrics.md` §2.9.
///
/// `admit` answers whether the event is a row of its own. When it is not, it has been counted
/// into the key's window, and the window's end writes ONE row of the same kind carrying the
/// count (`attrs.suppressed`, with `n1..n3` left NULL: a count of events is not one observation
/// of the kind, and a `SUM(n1)` over `worker.cmd` must not average it in).
#[derive(Debug)]
pub struct RateCap {
    budget: u32,
    keys: HashMap<Key, Slot>,
}

impl Default for RateCap {
    fn default() -> RateCap {
        RateCap::new(RATE_BUDGET)
    }
}

impl RateCap {
    pub fn new(budget: u32) -> RateCap {
        RateCap {
            budget,
            keys: HashMap::new(),
        }
    }

    /// How many keys are held — a test's window on the map's ceiling.
    pub fn keys(&self) -> usize {
        self.keys.len()
    }

    /// **Is this event a row of its own?** Rows the interval boundary owes are pushed into
    /// `rows`, and the one line per key per minute into `log` — neither is the caller's to make,
    /// and both are empty for an event below the budget.
    pub fn admit(&mut self, e: &Event, rows: &mut Vec<Event>, log: &mut Vec<String>) -> bool {
        if self.budget == 0 {
            return true;
        }
        let key = Key::of(e);
        let window = e.ts_ms - e.ts_ms % WINDOW_MS;
        if !self.keys.contains_key(&key) && self.keys.len() >= MAX_KEYS {
            // A second with this many distinct keys is itself the anomaly, but the map is bounded
            // work either way: the stale windows go first, and everything goes if that was all.
            self.forget_stale(window, rows, log);
            if self.keys.len() >= MAX_KEYS {
                self.drain(rows, log);
            }
        }
        let slot = self.keys.entry(key.clone()).or_insert(Slot::new(window));
        if slot.window_ms != window {
            roll(&key, slot, self.budget, rows, log);
            slot.window_ms = window;
        }
        slot.total += 1;
        if slot.written < self.budget {
            slot.written += 1;
            return true;
        }
        slot.suppressed += 1;
        false
    }

    /// Write out what every window still holds — at shutdown, and for a test that cannot wait
    /// for the next event to close the interval.
    pub fn drain(&mut self, rows: &mut Vec<Event>, log: &mut Vec<String>) {
        for (key, mut slot) in std::mem::take(&mut self.keys) {
            roll(&key, &mut slot, self.budget, rows, log);
        }
    }

    /// Forget the keys whose window is not this one, writing what each owes first.
    fn forget_stale(&mut self, window: u64, rows: &mut Vec<Event>, log: &mut Vec<String>) {
        let stale: Vec<Key> = self
            .keys
            .iter()
            .filter(|(_, s)| s.window_ms != window)
            .map(|(k, _)| k.clone())
            .collect();
        for key in stale {
            if let Some(mut slot) = self.keys.remove(&key) {
                roll(&key, &mut slot, self.budget, rows, log);
            }
        }
    }
}

/// **Close one window of one key**: its summary row, and — at most once a minute — the report that
/// this key is being suppressed. Both are absent for a window that wrote everything it saw, which
/// is why a session below the budget is unchanged.
fn roll(key: &Key, slot: &mut Slot, budget: u32, rows: &mut Vec<Event>, log: &mut Vec<String>) {
    if slot.suppressed == 0 {
        slot.written = 0;
        slot.total = 0;
        return;
    }
    let (held, seen) = (slot.suppressed, slot.total);
    let mut summary = Event::new(key.kind);
    summary.ts_ms = slot.window_ms;
    summary.session_id = key.session.clone();
    summary.name = key.name.clone();
    summary.outcome = key.outcome.clone();
    summary.attrs.push(("suppressed", Value::from(held)));
    summary.attrs.push(("window_ms", Value::from(WINDOW_MS)));
    rows.push(summary);
    let due = slot.last_alert_ms == 0
        || slot.window_ms.saturating_sub(slot.last_alert_ms) >= ALERT_EVERY_MS;
    if due {
        slot.last_alert_ms = slot.window_ms;
        let text = key.text();
        let mut alert = Event::new(kind::SUPPRESSED);
        alert.ts_ms = slot.window_ms;
        alert.session_id = key.session.clone();
        alert.name = Some(key.kind.to_string());
        alert.n[0] = Some(held as i64);
        alert.n[1] = Some(seen as i64);
        alert.attrs.push(("key", Value::from(text.clone())));
        alert.attrs.push(("budget", Value::from(budget)));
        rows.push(alert);
        log.push(format!(
            "metrics: suppressing {text} — {seen} in one second over a budget of {budget}/s, \
             {held} held back; identical events at this rate are a defect, not activity"
        ));
    }
    slot.written = 0;
    slot.suppressed = 0;
    slot.total = 0;
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
    /// The rate cap (§2.9) — see [`RateCap`].
    cap: RateCap,
    /// Where a `metrics.suppressed` line goes: the daemon's log, or a test's `Vec`.
    log: Box<dyn FnMut(&str) + Send>,
    /// Does this FILE give freed pages back (§2.9)? A file created before this build set
    /// `auto_vacuum` cannot, until the operator runs one `VACUUM` — see [`Writer::shrinks`].
    shrinks: bool,
    last_size_check: Option<u64>,
}

/// §2.7: a batch a second, or sooner at this many rows.
pub const BATCH_ROWS: usize = 1000;
pub const BATCH_EVERY: Duration = Duration::from_secs(1);
const DAY_MS: u64 = 86_400_000;

/// **The rollout's `WHERE`** — an id range and a time bound, and the two kinds of row the
/// operator's decision 2 keeps forever: `operator.feedback`, and every row of a turn a mark
/// points at. Shared by the rollup and the delete so that what goes into `daily` is exactly what
/// leaves `event`, and a kept row is never counted in both.
const FOREVER: &str = " kind <> 'operator.feedback' \
     AND (turn_id IS NULL OR turn_id NOT IN \
          (SELECT turn_id FROM event WHERE kind = 'operator.feedback' AND turn_id IS NOT NULL))";

/// **A day's rows summed into `daily`** — shared by the age prune and the size cap.
///
/// It ADDS to a group rather than ignoring it: the size cap deletes a day in chunks, so the same
/// `(day, kind, name, model, outcome)` arrives more than once for one day, and what keeps a row
/// counted exactly once is that the rollup and the delete are one transaction over one range —
/// not the deduplication of the key. Percentiles stay `NULL` in phase 1: the `daily` columns are
/// the design's, the arithmetic is phase 2's.
const ROLLUP: &str = "INSERT INTO daily (day, kind, name, model, outcome, n, sum_dur_ms, \
     max_dur_ms, sum_n1, sum_n2, sum_n3) \
     SELECT date(ts_ms / 1000, 'unixepoch'), kind, COALESCE(name, ''), \
            COALESCE(attrs->>'model', CASE WHEN kind IN ('round','turn.end') THEN name END, ''), \
            COALESCE(outcome, ''), COUNT(*), SUM(dur_ms), MAX(dur_ms), \
            SUM(n1), SUM(n2), SUM(n3) \
     FROM event WHERE id > ?1 AND id <= ?2 AND ts_ms < ?3 AND";

impl Writer {
    pub fn new(conn: Connection, id: Identity, settings: Settings) -> Writer {
        // Read back what `open` asked for. A file whose tables predate this build answers 0: the
        // pragma is a no-op once the file has tables, so its pages are never given back until the
        // operator runs one `VACUUM` (§2.9, [`Writer::shrinks`]).
        let auto_vacuum: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
            .unwrap_or(0);
        let cap = RateCap::new(settings.rate_per_sec);
        Writer {
            conn,
            id,
            settings,
            parents: HashMap::new(),
            lost: 0,
            last_prune: None,
            cap,
            log: Box::new(|line: &str| eprintln!("{line}")),
            shrinks: auto_vacuum == 2,
            last_size_check: None,
        }
    }

    /// Where a suppression report goes. The daemon leaves the default — stderr, which is the log
    /// it is started with — and a test takes the lines into a `Vec`.
    pub fn log_with(&mut self, f: impl FnMut(&str) + Send + 'static) {
        self.log = Box::new(f);
    }

    /// **Can this file give freed pages back?** `false` for a file created before this build set
    /// `auto_vacuum`: the size cap still deletes rows and rolls them up, but `incremental_vacuum`
    /// moves nothing, so the file stays its size until the operator runs
    /// `sqlite3 metrics.db 'PRAGMA auto_vacuum=INCREMENTAL; VACUUM;'` once (§2.9).
    pub fn shrinks(&self) -> bool {
        self.shrinks
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
    ///
    /// `n` is ROWS, not events: the rate cap (§2.9) turns a storm into its first [`RATE_BUDGET`]
    /// events plus one summary of the rest, and it does that here — before the transaction,
    /// because coalescing is arithmetic on what this daemon is doing, not SQL.
    pub fn write_batch(&mut self, batch: &[Event]) -> rusqlite::Result<usize> {
        if batch.is_empty() {
            return Ok(0);
        }
        match self.try_write(batch) {
            Ok(n) => Ok(n),
            Err(_) => self.try_write(batch).inspect_err(|_| self.lost += 1),
        }
    }

    /// **Write out what the rate cap still holds** — the partial window of every key. The writer's
    /// loop calls it when the channel closes; a test calls it to see the summary of a storm that
    /// never spanned two seconds.
    pub fn flush_cap(&mut self) -> rusqlite::Result<usize> {
        let mut extra: Vec<Event> = Vec::new();
        let mut lines: Vec<String> = Vec::new();
        self.cap.drain(&mut extra, &mut lines);
        self.report(&lines);
        let rows: Vec<&Event> = extra.iter().collect();
        self.insert_rows(&rows)
    }

    fn report(&mut self, lines: &[String]) {
        for line in lines {
            (self.log)(line);
        }
    }

    fn try_write(&mut self, batch: &[Event]) -> rusqlite::Result<usize> {
        let mut keep: Vec<bool> = Vec::with_capacity(batch.len());
        let mut extra: Vec<Event> = Vec::new();
        let mut lines: Vec<String> = Vec::new();
        for e in batch {
            keep.push(self.cap.admit(e, &mut extra, &mut lines));
        }
        self.report(&lines);
        let mut rows: Vec<&Event> = batch
            .iter()
            .zip(keep)
            .filter_map(|(e, kept)| kept.then_some(e))
            .collect();
        rows.extend(extra.iter());
        self.insert_rows(&rows)
    }

    /// One transaction, one `INSERT` per row, the tree's root filled from what the parent map
    /// knows and the snippet stripped from every kind but those `[metrics] snippets` names.
    fn insert_rows(&mut self, rows: &[&Event]) -> rusqlite::Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let roots: Vec<Option<String>> = rows
            .iter()
            .map(|e| {
                e.root_id
                    .clone()
                    .or_else(|| e.session_id.as_deref().map(|s| self.root_of(s)))
            })
            .collect();
        // IMMEDIATE: a deferred transaction that ever reads before it writes is refused at once
        // by a concurrent writer, busy timeout or not (see tokencore's `store/busy.rs`).
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO event (ts_ms, dur_ms, daemon, build, session_id, root_id, \
                 transcript_id, turn_id, round, kind, name, outcome, n1, n2, n3, attrs) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )?;
            for (e, root) in rows.iter().zip(roots) {
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
        Ok(rows.len())
    }

    /// **Roll up, then prune, the rows older than `keep_days`** — except the marks and every
    /// row of a marked turn, which are the labelled data and kept forever (decision 2).
    ///
    /// The rollup adds to `daily` and covers exactly the rows the delete takes ([`ROLLUP`],
    /// [`FOREVER`]), so a row that survives — a mark, or a row of a marked turn — is not counted
    /// in `daily` as well as being still in `event`. Percentiles are left `NULL` in phase 1.
    /// Returns the rows deleted.
    pub fn prune(&mut self, now_ms: u64) -> rusqlite::Result<usize> {
        let cutoff = now_ms.saturating_sub(self.settings.keep_days.saturating_mul(DAY_MS));
        // Whole days only: a day is rolled up when every row of it is past the cutoff, so a
        // rollup never counts half a day and then misses the rest.
        let cutoff = cutoff - cutoff % DAY_MS;
        let gone = self.roll_up_and_delete(0, i64::MAX, cutoff as i64)?;
        self.last_prune = Some(now_ms);
        if gone > 0 {
            // Bounded: `incremental_vacuum` with no argument gives back EVERY free page, and on
            // the 89 GB file this cap exists for that is hours on the writer thread.
            let _ = self.shrink(SIZE_PAGES_PER_PASS);
        }
        Ok(gone)
    }

    fn prune_due(&self, now_ms: u64) -> bool {
        self.last_prune
            .is_none_or(|t| now_ms.saturating_sub(t) >= DAY_MS)
    }

    /// **The size cap** (`docs/metrics.md` §2.9): over `[metrics] max_mb`, roll the oldest whole
    /// days up and delete them until the file is under [`SIZE_LOW_PCT`] of the ceiling, then give
    /// the pages back. Returns the rows deleted.
    ///
    /// **Bounded**, because this runs on the thread that also writes the batch: at most
    /// [`SIZE_ROWS_PER_PASS`] rows go, in [`SIZE_CHUNK`]-row id ranges, and at most
    /// [`SIZE_PAGES_PER_PASS`] pages are vacuumed. A file far over the cap therefore shrinks over
    /// several passes rather than stalling the daemon for an hour; each pass reads the size from
    /// the database header, which is O(1).
    ///
    /// **The cap beats the age rule**, deliberately: the rows it deletes may be minutes old. A
    /// file that is losing its disk is not one to keep a retention promise to, and `daily` —
    /// permanent — still holds the counts.
    pub fn enforce_size(&mut self) -> rusqlite::Result<usize> {
        self.last_size_check = Some(now_ms());
        let cap = self.settings.max_bytes();
        if cap == 0 {
            return Ok(0);
        }
        let low = cap * SIZE_LOW_PCT / 100;
        // A previous pass's deletes are free pages until this runs, so the measurement below is
        // the LIVE size and not the file's. Both calls are bounded, and skipped entirely when
        // there is nothing to give back — eleven daemons run this every ten seconds.
        if self.free_pages() > 0 {
            self.shrink(SIZE_PAGES_PER_PASS)?;
        }
        if self.db_bytes() <= cap {
            return Ok(0);
        }
        // The oldest row the cap may actually take. NOT the oldest row: a mark, or a row of a
        // marked turn, is kept forever, and a cap whose ceiling is the oldest MARK can never pass
        // it — measured here as a file stuck at 3.7 MB against a 1 MiB cap, every pass, forever.
        let Some((first, oldest)) = self.first_deletable()? else {
            return Ok(0);
        };
        // The end of that row's whole day: one pass takes a day at a time, so a rollup never
        // counts half a day. Days are only ever the past here.
        let cutoff = oldest - oldest.rem_euclid(DAY_MS as i64) + DAY_MS as i64;
        // Start the walk ON that row: it costs nothing and every chunk after it is productive.
        let mut after = first - 1;
        let mut gone = 0usize;
        let mut skipped = 0usize;
        while gone < SIZE_ROWS_PER_PASS && self.live_bytes() > low {
            let upto = after.saturating_add(SIZE_CHUNK);
            let n = self.roll_up_and_delete(after, upto, cutoff)?;
            after = upto;
            if n == 0 {
                // Only the forever rows here. A bounded number of such chunks, so a file whose
                // oldest rows are all marks still returns.
                skipped += 1;
                if skipped >= SIZE_SKIP_CHUNKS {
                    break;
                }
            } else {
                skipped = 0;
                gone += n;
            }
        }
        if self.free_pages() > 0 {
            self.shrink(SIZE_PAGES_PER_PASS)?;
        }
        Ok(gone)
    }

    fn size_due(&self, now: u64) -> bool {
        self.last_size_check
            .is_none_or(|t| now.saturating_sub(t) >= SIZE_EVERY.as_millis() as u64)
    }

    /// Roll `id > ?1 AND id <= ?2 AND ts_ms < ?3` up into `daily` and delete it — one transaction,
    /// so a row is either in both or in neither. Returns the rows deleted.
    fn roll_up_and_delete(
        &mut self,
        after: i64,
        upto: i64,
        cutoff: i64,
    ) -> rusqlite::Result<usize> {
        // IMMEDIATE: a deferred transaction that ever reads before it writes is refused at once
        // by a concurrent writer, busy timeout or not (see tokencore's `store/busy.rs`).
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            &format!(
                "{ROLLUP} {FOREVER} GROUP BY 1, 2, 3, 4, 5 \
                 ON CONFLICT(day, kind, name, model, outcome) DO UPDATE SET \
                 n = n + excluded.n, \
                 sum_dur_ms = COALESCE(sum_dur_ms, 0) + COALESCE(excluded.sum_dur_ms, 0), \
                 max_dur_ms = MAX(COALESCE(max_dur_ms, excluded.max_dur_ms), \
                                  COALESCE(excluded.max_dur_ms, max_dur_ms)), \
                 sum_n1 = COALESCE(sum_n1, 0) + COALESCE(excluded.sum_n1, 0), \
                 sum_n2 = COALESCE(sum_n2, 0) + COALESCE(excluded.sum_n2, 0), \
                 sum_n3 = COALESCE(sum_n3, 0) + COALESCE(excluded.sum_n3, 0)"
            ),
            params![after, upto, cutoff],
        )?;
        let gone = tx.execute(
            &format!("DELETE FROM event WHERE id > ?1 AND id <= ?2 AND ts_ms < ?3 AND {FOREVER}"),
            params![after, upto, cutoff],
        )?;
        tx.commit()?;
        Ok(gone)
    }

    /// **The first row the cap may delete** — its `id` and `ts_ms`. `ORDER BY id LIMIT 1` over the
    /// table's own order, so it stops at the first match rather than scanning; the forever rows it
    /// steps over are the marks, which are few.
    fn first_deletable(&self) -> rusqlite::Result<Option<(i64, i64)>> {
        self.conn
            .query_row(
                &format!("SELECT id, ts_ms FROM event WHERE {FOREVER} ORDER BY id LIMIT 1"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    /// `PRAGMA page_size`, once — it cannot change under a live file.
    fn page_size(&self) -> u64 {
        self.conn
            .query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as u64)
            .unwrap_or(4096)
    }

    /// **The file's size as SQLite counts it**, off the header — two pragmas, no I/O of the table.
    pub fn db_bytes(&self) -> u64 {
        let pages: i64 = self
            .conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap_or(0);
        pages.max(0) as u64 * self.page_size()
    }

    /// The parts of it that are rows rather than free pages — what a delete is measured against.
    fn live_bytes(&self) -> u64 {
        self.db_bytes()
            .saturating_sub(self.free_pages().max(0) as u64 * self.page_size())
    }

    /// Pages on the freelist, waiting for a vacuum — the header's own count.
    fn free_pages(&self) -> i64 {
        self.conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Where this file is, as SQLite knows it — `PRAGMA database_list`'s `main` row. Empty for an
    /// in-memory database, which is not a file the size cap can say anything about.
    pub fn path(&self) -> PathBuf {
        self.conn
            .query_row(
                "SELECT file FROM pragma_database_list WHERE name = 'main'",
                [],
                |r| r.get::<_, String>(0),
            )
            .map(PathBuf::from)
            .unwrap_or_default()
    }

    /// **Give back at most `pages` pages**, then let the WAL carry the truncation out to the file.
    /// A file created without `auto_vacuum` gives back none — [`Writer::shrinks`] is how the
    /// daemon says so once, and the operator's one-time `VACUUM` is what makes it work from then on.
    ///
    /// **The rows must be drained.** `PRAGMA incremental_vacuum` yields ONE ROW PER PAGE IT FREES,
    /// so a statement that is stepped once — which is what `execute_batch` and `execute` do — gives
    /// back exactly one page per call. MEASURED in this crate on a 955-page file with 923 free:
    /// `execute_batch("PRAGMA incremental_vacuum(100000)")` removed 1 page; the same statement read
    /// to its last row took the file to 26 pages. Which means the day prune's
    /// `execute_batch("PRAGMA incremental_vacuum;")` had never shrunk anything either.
    ///
    /// `wal_checkpoint(PASSIVE)` after it: in WAL mode the truncation lives in the write-ahead log
    /// until a checkpoint, so without this the file on disk keeps its size even though `page_count`
    /// has fallen. PASSIVE never blocks a reader, and eleven daemons read this file.
    fn shrink(&mut self, pages: i64) -> rusqlite::Result<()> {
        {
            let mut st = self
                .conn
                .prepare(&format!("PRAGMA incremental_vacuum({pages})"))?;
            let mut rows = st.query([])?;
            while rows.next()?.is_some() {}
        }
        self.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")
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
///
/// The two caps (§2.9) run on this thread and nowhere else: the rate cap is inside
/// [`Writer::write_batch`], and the size cap is checked here — once before the loop (a daemon that
/// starts on an oversized file starts shrinking it) and then every [`SIZE_EVERY`].
pub fn serve(w: &mut Writer, rx: Receiver<Msg>, dropped: impl Fn() -> u64) {
    let mut reported = 0u64;
    // Said once, at start: without this the size cap deletes rows nobody can see the effect of.
    if !w.shrinks() {
        eprintln!(
            "metrics: {} was created before this build set auto_vacuum, so a deleted page is never \
             given back; run `sqlite3 <this file> 'PRAGMA auto_vacuum=INCREMENTAL; VACUUM;'` once \
             to let the size cap shrink it",
            w.path().display()
        );
    }
    let _ = w.enforce_size();
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
        if closed {
            // The last, partial window of every key is still held — a storm that ended before the
            // second did should still say how many events it was.
            let _ = w.flush_cap();
            return;
        }
        let t = now_ms();
        if w.prune_due(t) {
            let _ = w.prune(t);
        }
        if w.size_due(t) {
            let _ = w.enforce_size();
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
        writer_with(path, Settings::default())
    }

    fn writer_with(path: &Path, settings: Settings) -> Writer {
        Writer::new(
            open(path).unwrap(),
            Identity {
                daemon: "box/1/0".into(),
                build: "test".into(),
            },
            settings,
        )
    }

    /// The defaults with the two caps chosen per test.
    fn capped(rate_per_sec: u32, max_mb: u64) -> Settings {
        Settings {
            rate_per_sec,
            max_mb,
            ..Settings::default()
        }
    }

    /// Every row, column by column — what "byte-identical" is asserted on.
    fn dump(path: &Path) -> Vec<String> {
        let c = Connection::open(path).unwrap();
        let mut st = c
            .prepare(
                "SELECT ts_ms, dur_ms, session_id, root_id, transcript_id, turn_id, round, \
                 kind, name, outcome, n1, n2, n3, attrs FROM event ORDER BY id",
            )
            .unwrap();
        let rows = st
            .query_map([], |r| {
                let mut out: Vec<String> = Vec::new();
                for i in 0..14 {
                    let v: rusqlite::types::Value = r.get(i)?;
                    out.push(format!("{v:?}"));
                }
                Ok(out.join("|"))
            })
            .unwrap();
        rows.map(Result::unwrap).collect()
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
    ///
    /// The rate cap is OFF here on purpose: this test is about the channel and the drop counter,
    /// and forty events under one key would otherwise be coalesced into a summary — which is the
    /// next test's subject.
    #[test]
    fn the_writer_drains_the_channel_in_batches_and_reports_its_drops() {
        let p = temp("serve");
        let mut w = writer_with(&p, capped(0, MAX_MB));
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
            1,
            "the old day was rolled up before its rows went — and only what went, so a row \
             that is still in `event` is not counted in `daily` as well"
        );
        // A second prune the same day is a no-op, and the kept row is not rolled up again.
        w.prune(now).unwrap();
        assert_eq!(
            count(&p, "SELECT SUM(n) FROM daily WHERE kind = 'round'"),
            1,
            "a row kept forever is not added to `daily` on every pass"
        );
    }

    #[test]
    fn settings_read_the_switch_and_default_on() {
        let d = |e, k, s, r, m| Settings::from_raw(e, k, s, r, m);
        assert!(d(None, None, None, None, None).enabled);
        assert!(!d(Some("false"), None, None, None, None).enabled);
        assert!(!d(Some(" Off "), None, None, None, None).enabled);
        assert_eq!(d(None, Some("30"), None, None, None).keep_days, 30);
        assert_eq!(d(None, Some("junk"), None, None, None).keep_days, 90);
        assert_eq!(
            d(None, None, Some("warning, tool.call"), None, None).snippets,
            vec!["warning", "tool.call"]
        );
        assert_eq!(
            Settings::default().snippets,
            vec!["context.claim"],
            "decision 5: the claim sentence is kept by default"
        );
        // The two caps (§2.9): the defaults, read as numbers, and a junk value is the default
        // because a metrics setting is never a reason a daemon does not start.
        assert_eq!(
            d(None, None, None, None, None).rate_per_sec,
            RATE_BUDGET,
            "20 identical events a second is the budget"
        );
        assert_eq!(d(None, None, None, None, None).max_bytes(), 2048 << 20);
        assert_eq!(
            d(None, None, None, Some("5"), Some("512")).max_bytes(),
            512 << 20
        );
        assert_eq!(d(None, None, None, Some("5"), None).rate_per_sec, 5);
        assert_eq!(
            d(None, None, None, Some("junk"), Some("junk")).rate_per_sec,
            RATE_BUDGET
        );
        assert_eq!(
            d(None, None, None, Some("junk"), Some("junk")).max_mb,
            MAX_MB
        );
        assert_eq!(
            d(None, None, None, Some("0"), Some("0")).max_bytes(),
            0,
            "an explicit 0 is the documented `no cap`"
        );
    }

    /// **A storm becomes a handful of rows** (§2.9) — the 2026-10-27 shape: one key, a hundred
    /// thousand events, one second. The budget's worth is written as itself, the rest is ONE row
    /// carrying the count, and the key says once that it is suppressing.
    #[test]
    fn a_storm_of_identical_events_becomes_a_handful_of_rows_and_one_report() {
        let p = temp("storm");
        let mut w = writer(&p);
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = std::sync::Arc::clone(&lines);
        w.log_with(move |l| sink.lock().unwrap().push(l.to_string()));
        let second = 1_800_000_000_000u64;
        let storm: Vec<Event> = (0..100_000)
            .map(|_| {
                Event::new(kind::WORKER_CMD)
                    .at(second + 400)
                    .session("s-storm")
                    .name("interrupt")
                    .outcome("ignored")
                    .n1(3)
            })
            .collect();
        assert_eq!(
            w.write_batch(&storm).unwrap(),
            RATE_BUDGET as usize,
            "the budget's worth of rows, and nothing else, in the batch itself"
        );
        w.flush_cap().unwrap();
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE kind = 'worker.cmd' AND attrs->>'suppressed' \
                 IS NULL"
            ),
            RATE_BUDGET as i64,
            "the events that fit are themselves, unchanged"
        );
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE kind = 'worker.cmd' AND \
                 attrs->>'suppressed' IS NOT NULL"
            ),
            1,
            "one row for the interval, not one per held-back event"
        );
        assert_eq!(
            count(
                &p,
                "SELECT SUM(attrs->>'suppressed') FROM event WHERE kind = 'worker.cmd'"
            ),
            100_000 - RATE_BUDGET as i64,
            "the summary says how many it held back, so the count is still the truth"
        );
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE kind = 'metrics.suppressed' AND \
                 name = 'worker.cmd' AND session_id = 's-storm' AND n1 = ?1 AND n2 = 100000"
                    .replace("?1", &(100_000 - RATE_BUDGET as i64).to_string())
                    .as_str()
            ),
            1,
            "one report for the key, naming the rate it was doing"
        );
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE kind = 'worker.cmd' AND \
                 attrs->>'suppressed' IS NOT NULL AND n1 IS NULL AND n2 IS NULL \
                 AND name = 'interrupt' AND outcome = 'ignored' AND session_id = 's-storm'"
            ),
            1,
            "a summary keeps the key's axes (so a reader can group it) and leaves the slots \
             NULL — a count of events is not one observation of the kind"
        );
        let said = lines.lock().unwrap();
        assert_eq!(said.len(), 1, "one line to the daemon's log: {said:?}");
        assert!(
            said[0].contains("s-storm|worker.cmd|interrupt|ignored")
                && said[0].contains("100000")
                && said[0].contains("20/s"),
            "the line names the key and the rate: {}",
            said[0]
        );
    }

    /// **Below the budget nothing changes** — the same events through a capped and an uncapped
    /// writer are the same rows, to the column (§2.9).
    #[test]
    fn a_mix_below_the_budget_is_written_row_for_row_as_it_was_before_the_cap() {
        let (a, b) = (temp("uncapped"), temp("capped"));
        let mut plain = writer_with(&a, capped(0, MAX_MB));
        let mut capped_w = writer_with(&b, Settings::default());
        let t = 1_800_000_000_000u64;
        let mut mix: Vec<Event> = Vec::new();
        for i in 0..18u64 {
            mix.push(
                Event::new(kind::ROUND)
                    .at(t + i)
                    .dur(900)
                    .session("s-1")
                    .turn("t-1", Some(i as u32))
                    .name("glm/glm-5")
                    .outcome("eos")
                    .n1(1000 + i)
                    .attr("ttfb_ms", 300)
                    .attr("snippet", "a tool's output"),
            );
            mix.push(
                Event::new(kind::TOOL_CALL)
                    .at(t + i)
                    .session("s-1")
                    .name("bash")
                    .outcome("ok")
                    .n1(10),
            );
        }
        for (verb, q) in [("prompt", 10u64), ("prompt", 20), ("slash", 5)] {
            mix.push(
                Event::new(kind::WORKER_CMD)
                    .at(t)
                    .name(verb)
                    .outcome("ok")
                    .n1(q),
            );
        }
        // A `context.claim` keeps its snippet; the others lose theirs — the cap must not touch
        // either decision.
        mix.push(
            Event::new(kind::CONTEXT_CLAIM)
                .at(t)
                .session("s-1")
                .attr("snippet", "my context is full"),
        );
        plain.write_batch(&mix).unwrap();
        capped_w.write_batch(&mix).unwrap();
        capped_w.flush_cap().unwrap();
        assert_eq!(
            dump(&a),
            dump(&b),
            "not one column differs for a session under the budget"
        );
    }

    /// **The map is bounded**: thousands of distinct keys in one second is not a memory leak — the
    /// stale ones are written out and forgotten, and every event still gets its row.
    #[test]
    fn the_key_map_is_bounded_and_a_burst_of_distinct_keys_still_writes() {
        let mut cap = RateCap::new(RATE_BUDGET);
        let mut rows: Vec<Event> = Vec::new();
        let mut log: Vec<String> = Vec::new();
        let mut kept = 0usize;
        for i in 0..(MAX_KEYS as u64 * 2) {
            let e = Event::new(kind::WARNING)
                .at(1_800_000_000_000 + i)
                .session(&format!("s-{i}"))
                .name("repetition_collapse");
            if cap.admit(&e, &mut rows, &mut log) {
                kept += 1;
            }
        }
        assert_eq!(kept, MAX_KEYS * 2, "every one is under its own budget");
        assert!(
            cap.keys() <= MAX_KEYS,
            "the map stayed bounded: {}",
            cap.keys()
        );
        assert!(rows.is_empty() && log.is_empty(), "nothing was suppressed");
    }

    /// **A key reports at most once a minute** — one line and one `metrics.suppressed` per key per
    /// minute, however long the storm lasts, and the summaries keep counting meanwhile.
    #[test]
    fn a_suppressing_key_reports_once_a_minute_and_summarises_every_second() {
        let p = temp("alerts");
        let mut w = writer(&p);
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = std::sync::Arc::clone(&lines);
        w.log_with(move |l| sink.lock().unwrap().push(l.to_string()));
        let second = 1_800_000_000_000u64;
        let storm = |at: u64, n: usize| -> Vec<Event> {
            (0..n)
                .map(|_| {
                    Event::new(kind::WORKER_CMD)
                        .at(at)
                        .session("s-loop")
                        .name("interrupt")
                        .outcome("ignored")
                })
                .collect()
        };
        for s in 0..5u64 {
            w.write_batch(&storm(second + s * WINDOW_MS + 5, 100))
                .unwrap();
        }
        // A second's worth of new events closes the previous window; the fifth window is closed
        // by the drain.
        w.flush_cap().unwrap();
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE kind = 'metrics.suppressed'"
            ),
            1,
            "five seconds of the same key is still one report"
        );
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE kind = 'worker.cmd' AND \
                 attrs->>'suppressed' IS NOT NULL"
            ),
            5,
            "one summary per second, and none lost"
        );
        assert_eq!(
            count(
                &p,
                "SELECT SUM(attrs->>'suppressed') FROM event WHERE kind = 'worker.cmd'"
            ),
            5 * (100 - RATE_BUDGET as i64),
            "the five summaries add up to what was held back"
        );
        // A minute later the key may say it again.
        w.write_batch(&storm(second + 61_000, 100)).unwrap();
        w.flush_cap().unwrap();
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE kind = 'metrics.suppressed'"
            ),
            2,
            "a minute on, the same key reports again"
        );
        assert_eq!(lines.lock().unwrap().len(), 2, "one log line per report");
    }

    /// **The size cap**: over the cap the oldest days are rolled up and deleted until the file is
    /// under the low-water mark, the rows decision 2 keeps forever survive, and the pages come
    /// back — so `page_count` falls (§2.9).
    #[test]
    fn the_size_cap_prunes_under_the_low_water_mark_keeps_the_forever_rows_and_shrinks_the_file() {
        let p = temp("size");
        let day = 86_400_000u64;
        let start = 1_800_000_000_000u64 - 1_800_000_000_000u64 % day;
        // Two old days of fat rows, plus a mark on a turn in the oldest day: that turn's rows
        // are the labelled data and must outlive the cap while everything around them goes.
        {
            let mut w = writer_with(&p, capped(0, 0));
            let filler = "x".repeat(500);
            let mut rows: Vec<Event> = Vec::new();
            for i in 0..6000u64 {
                rows.push(
                    Event::new(kind::ROUND)
                        // Days in id order, as a real file's are: the cap deletes from the
                        // oldest end, and a whole page only goes when every row on it does.
                        .at(start + if i < 3000 { 0 } else { day } + i)
                        .session("s-old")
                        .turn(if i < 100 { "t-marked" } else { "t-plain" }, Some(0))
                        .name("glm")
                        .outcome("eos")
                        .n1(10)
                        .attr("filler", filler.clone()),
                );
            }
            rows.push(
                Event::new(kind::FEEDBACK)
                    .at(start + 1)
                    .session("s-old")
                    .turn("t-marked", Some(0))
                    .name("bad"),
            );
            w.write_batch(&rows).unwrap();
        }
        let mut w = writer_with(&p, capped(0, 1));
        assert!(
            w.shrinks(),
            "a file this build created gives its pages back"
        );
        let before = w.db_bytes();
        assert!(before > 1 << 20, "the fixture is over the cap: {before}");
        let marked: i64 = count(&p, "SELECT COUNT(*) FROM event WHERE turn_id = 't-marked'");
        assert_eq!(marked, 101, "a hundred rows and the mark itself");
        let mut passes = 0;
        while w.db_bytes() > w.settings.max_bytes() && passes < 40 {
            w.enforce_size().unwrap();
            passes += 1;
        }
        assert!(
            w.db_bytes() <= w.settings.max_bytes(),
            "over the cap after {passes} passes: {} bytes",
            w.db_bytes()
        );
        // Under the LOW-WATER mark, not merely under the cap: the pass deletes until 80 % and
        // the vacuum gives the rest back, so the next pass is not a delete of one row.
        let low = w.settings.max_bytes() * SIZE_LOW_PCT / 100;
        for _ in 0..40 {
            if w.db_bytes() <= low {
                break;
            }
            w.enforce_size().unwrap();
        }
        assert!(
            w.db_bytes() <= low,
            "{} bytes is above the {low}-byte low-water mark",
            w.db_bytes()
        );
        let pages_after: i64 = count(&p, "SELECT * FROM pragma_page_count");
        assert!(
            (pages_after as u64) * w.page_size() < before,
            "the file shrank: {pages_after} pages now, {} bytes before",
            before
        );
        assert_eq!(
            count(&p, "SELECT COUNT(*) FROM event WHERE turn_id = 't-marked'"),
            marked,
            "every row of a marked turn, and the mark, are still here"
        );
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE kind = 'operator.feedback'"
            ),
            1
        );
        assert!(
            count(&p, "SELECT SUM(n) FROM daily") > 0,
            "and what went was rolled up before it went"
        );
    }

    /// **A pass is bounded**: a file far over the cap loses at most [`SIZE_ROWS_PER_PASS`] rows a
    /// pass, keeps working after it, and converges over the following ones (§2.9).
    #[test]
    fn a_size_pass_deletes_a_bounded_number_of_rows_and_the_writer_keeps_writing() {
        let p = temp("bounded");
        let total = SIZE_ROWS_PER_PASS + 30_000;
        {
            let mut w = writer_with(&p, capped(0, 0));
            let rows: Vec<Event> = (0..total)
                .map(|i| {
                    Event::new(kind::ROUND)
                        .at(1_800_000_000_000 + i as u64)
                        .name("glm")
                        .outcome("eos")
                        .n1(7)
                })
                .collect();
            w.write_batch(&rows).unwrap();
        }
        let mut w = writer_with(&p, capped(0, 1));
        assert_eq!(
            w.enforce_size().unwrap(),
            SIZE_ROWS_PER_PASS,
            "one pass spends its whole budget and not a row more"
        );
        assert!(
            count(&p, "SELECT COUNT(*) FROM event") > 0,
            "and the file still has rows — the next pass continues where this one stopped"
        );
        // Never wedged: the same writer takes a batch straight after a pass that deleted 250k rows.
        w.write_batch(&[Event::new(kind::WAKE).at(1_800_000_100_000).name("job")])
            .unwrap();
        assert_eq!(
            count(&p, "SELECT COUNT(*) FROM event WHERE kind = 'wake'"),
            1
        );
    }

    /// **A file created before this build set `auto_vacuum` cannot give a page back**, and the
    /// writer says so instead of pretending: the rows still go and the rollup still happens.
    #[test]
    fn a_file_created_without_auto_vacuum_deletes_and_rolls_up_but_does_not_shrink() {
        let p = temp("noauto");
        {
            // The old shape: a connection that never set the pragma before its first table.
            let c = Connection::open(&p).unwrap();
            c.execute_batch(DDL).unwrap();
            c.execute_batch("BEGIN").unwrap();
            {
                let mut st = c
                    .prepare(
                        "INSERT INTO event (ts_ms, daemon, build, kind, name, n1, attrs) \
                         VALUES (?1, 'box/1/0', 'old', 'round', 'glm', 1, ?2)",
                    )
                    .unwrap();
                let filler = format!("{{\"filler\":\"{}\"}}", "x".repeat(400));
                for i in 0..4000u64 {
                    st.execute(params![1_800_000_000_000i64 + i as i64, filler])
                        .unwrap();
                }
            }
            c.execute_batch("COMMIT").unwrap();
            c.execute("INSERT INTO meta (key, value) VALUES ('schema', '1')", [])
                .unwrap();
        }
        let mut w = writer_with(&p, capped(0, 1));
        assert!(
            !w.shrinks(),
            "a file whose tables predate the pragma cannot be shrunk without one VACUUM"
        );
        let before = w.db_bytes();
        let gone = w.enforce_size().unwrap();
        assert!(gone > 0, "the cap still bounds what GROWS");
        assert_eq!(
            w.db_bytes(),
            before,
            "but no page comes back until the operator runs the one-time VACUUM"
        );
        assert!(count(&p, "SELECT SUM(n) FROM daily") > 0, "rolled up first");
    }

    /// **The loop itself does both**: a daemon that starts on an oversized file starts shrinking it
    /// before it has written a row, and a storm that ends when the channel closes still gets its
    /// summary (§2.9). This is `serve` rather than `Writer`, because those two are its own.
    #[test]
    fn the_loop_checks_the_size_at_start_and_flushes_the_cap_when_the_channel_closes() {
        let p = temp("serve-caps");
        {
            let mut w = writer_with(&p, capped(0, 0));
            let filler = "x".repeat(500);
            let rows: Vec<Event> = (0..4000u64)
                .map(|i| {
                    Event::new(kind::ROUND)
                        .at(1_800_000_000_000 + i)
                        .name("glm")
                        .attr("filler", filler.clone())
                })
                .collect();
            w.write_batch(&rows).unwrap();
            assert!(w.db_bytes() > 1 << 20, "the fixture is over the cap");
        }
        let over = {
            let w = writer_with(&p, capped(0, 1));
            w.db_bytes()
        };
        let mut w = writer_with(&p, capped(RATE_BUDGET, 1));
        let (e, rx) = Emitter::new(1024);
        // One second on purpose: a hundred events straddling a second boundary would be two
        // windows and two summaries, which is right and not what this test is asking.
        for _ in 0..100 {
            e.send(Msg::Event(Box::new(
                Event::new(kind::WORKER_CMD)
                    .at(1_800_000_000_000)
                    .session("s-loop")
                    .name("interrupt")
                    .outcome("ignored"),
            )));
        }
        drop(e);
        std::thread::spawn(move || serve(&mut w, rx, || 0))
            .join()
            .unwrap();
        let after = writer_with(&p, capped(0, 1));
        assert!(
            after.db_bytes() < over,
            "the pass the loop runs at start shrank the file: {} bytes of {over}",
            after.db_bytes()
        );
        assert_eq!(
            count(
                &p,
                "SELECT COUNT(*) FROM event WHERE kind = 'worker.cmd' AND \
                 attrs->>'suppressed' IS NOT NULL"
            ),
            1,
            "and the storm that ended with the channel is one summary, not a hundred rows"
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
