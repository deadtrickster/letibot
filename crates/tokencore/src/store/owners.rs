//! Who an owner is, what it holds, and the journal of what was done to it (`docs/lifecycle.md` d.2).
//!
//! Three tables and no policy: an `owner` is the unit things belong to and are reaped as a whole, a
//! `holding` is one resource of one owner on one host, and `reap_event` is append-only. The proof
//! that a holding is the owner's lives on the RESOURCE - a cgroup, a directory root, a run file, a
//! label - and these rows are its index and its journal, never the proof itself.

use rusqlite::{OptionalExtension, params};

use super::{Result, Store};

/// The three tables, in the shared file of a split store so a box-level reaper opens one file.
pub const OWNERS_SQL: &str = r#"
-- **The unit things belong to, and its three lifetimes** (schema 48). One row per session id
-- where there is one, one per merge-queue run otherwise. Written before anything the owner
-- creates, because a row with no resource is harmless and a resource with no row is the leak.
CREATE TABLE IF NOT EXISTS owner (
    id            TEXT PRIMARY KEY,
    -- session | package | subagent | gate | review
    kind          TEXT NOT NULL,
    -- the owner tree; NULL at the top
    parent_id     TEXT,
    -- the main checkout this owner works against
    repo          TEXT,
    -- live | closing | closed | settling | settled | swept | stuck
    state         TEXT NOT NULL,
    -- the operator's hold: NULL none, 0 for ever
    held_until_ms INTEGER,
    -- the last renewal of activity, which is what an idle lease is measured from
    lease_ms      INTEGER NOT NULL,
    -- NULL = never closed for idleness
    idle_ttl_ms   INTEGER,
    -- NULL = no hard bound
    hard_ttl_ms   INTEGER,
    closed_ms     INTEGER,
    closed_why    TEXT,
    -- set when the queue proves the work is on the target
    landed_sha    TEXT,
    settled_ms    INTEGER,
    created_ms    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS owner_by_state ON owner (state);
CREATE INDEX IF NOT EXISTS owner_by_parent ON owner (parent_id);

-- **One resource of one owner on one host** (schema 48). `handle` is the path, ref, run id, unit
-- or container id; `stamp` is what on the resource proves it, as last checked. `bytes` is the last
-- measured size, so a plan can add up without touching the disk.
CREATE TABLE IF NOT EXISTS holding (
    owner_id   TEXT NOT NULL,
    -- see d.5: scope, vm, daemon, copy, copy-out, worktree, branch, target, tmp, home, ...
    kind       TEXT NOT NULL,
    -- `''` = this box; otherwise a host from hosts.toml. Empty rather than NULL because a NULL
    -- in a primary key is distinct in SQLite, so every write of one resource would be a new row
    host       TEXT NOT NULL DEFAULT '',
    handle     TEXT NOT NULL,
    -- compute | work | cache | record
    class      TEXT NOT NULL,
    -- intended | held | reaping | gone | kept | stuck | lost
    state      TEXT NOT NULL,
    stamp      TEXT NOT NULL,
    bytes      INTEGER,
    -- where its content was preserved, once it was
    archive    TEXT,
    -- the last verdict sentence
    why        TEXT,
    created_ms INTEGER NOT NULL,
    checked_ms INTEGER,
    attempts   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (owner_id, kind, host, handle)
);
CREATE INDEX IF NOT EXISTS holding_by_owner ON holding (owner_id);

-- **What was done to a holding, in order, for ever** (schema 48). Never deleted: a removal that
-- cannot be read back afterwards is a removal nobody can argue with.
CREATE TABLE IF NOT EXISTS reap_event (
    at_ms    INTEGER NOT NULL,
    owner_id TEXT NOT NULL,
    kind     TEXT,
    host     TEXT,
    handle   TEXT,
    act      TEXT NOT NULL,
    said     TEXT NOT NULL,
    by       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS reap_event_by_owner ON reap_event (owner_id, at_ms);
"#;

/// The five kinds of owner (`docs/lifecycle.md` d.1). An owner is what is reaped as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    /// An interactive session: a person holds it and nothing closes it for being quiet.
    Session,
    /// A session the bridge started from a brief: idle and hard TTLs apply.
    Package,
    /// A child session.
    Subagent,
    /// A merge queue run.
    Gate,
    /// A reviewer's session.
    Review,
}

impl OwnerKind {
    pub fn word(self) -> &'static str {
        match self {
            OwnerKind::Session => "session",
            OwnerKind::Package => "package",
            OwnerKind::Subagent => "subagent",
            OwnerKind::Gate => "gate",
            OwnerKind::Review => "review",
        }
    }

    /// The row's word back into the value. `None` for a word this build does not know, which is a
    /// row written by a newer build rather than corruption.
    pub fn of(word: &str) -> Option<OwnerKind> {
        Some(match word {
            "session" => OwnerKind::Session,
            "package" => OwnerKind::Package,
            "subagent" => OwnerKind::Subagent,
            "gate" => OwnerKind::Gate,
            "review" => OwnerKind::Review,
            _ => return None,
        })
    }

    /// How the operator's lines name it: *a package*, *a subagent*.
    pub fn noun(self) -> &'static str {
        match self {
            OwnerKind::Session => "session",
            OwnerKind::Package => "package",
            OwnerKind::Subagent => "subagent",
            OwnerKind::Gate => "gate run",
            OwnerKind::Review => "review",
        }
    }
}

/// Where an owner is in its life (`docs/lifecycle.md` d.3): compute ends at `closed`, work at
/// `settled`, and `stuck` is a step that failed twice and is told rather than retried in silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerState {
    /// It may hold anything.
    Live,
    /// Its compute is being reaped.
    Closing,
    /// No compute left; work, caches and records remain, and it can be reopened.
    Closed,
    /// Work is being archived and removed.
    Settling,
    /// Swept down to records.
    Settled,
    /// Nothing left that is not `gone`; the row and the journal stay.
    Swept,
    /// A reap step failed twice.
    Stuck,
}

impl OwnerState {
    pub fn word(self) -> &'static str {
        match self {
            OwnerState::Live => "live",
            OwnerState::Closing => "closing",
            OwnerState::Closed => "closed",
            OwnerState::Settling => "settling",
            OwnerState::Settled => "settled",
            OwnerState::Swept => "swept",
            OwnerState::Stuck => "stuck",
        }
    }

    pub fn of(word: &str) -> Option<OwnerState> {
        Some(match word {
            "live" => OwnerState::Live,
            "closing" => OwnerState::Closing,
            "closed" => OwnerState::Closed,
            "settling" => OwnerState::Settling,
            "settled" => OwnerState::Settled,
            "swept" => OwnerState::Swept,
            "stuck" => OwnerState::Stuck,
            _ => return None,
        })
    }

    /// Has its compute been given up? A closed owner is resumable but holds no process.
    pub fn compute_gone(self) -> bool {
        matches!(
            self,
            OwnerState::Closed | OwnerState::Settling | OwnerState::Settled | OwnerState::Swept
        )
    }
}

/// The class of a holding: what it costs and what its loss would be (`docs/lifecycle.md` d.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldingClass {
    /// RAM or a slot while it exists.
    Compute,
    /// May hold the only copy of something a person wants.
    Work,
    /// Can be rebuilt.
    Cache,
    /// Logs, run directories, session rows.
    Record,
}

impl HoldingClass {
    pub fn word(self) -> &'static str {
        match self {
            HoldingClass::Compute => "compute",
            HoldingClass::Work => "work",
            HoldingClass::Cache => "cache",
            HoldingClass::Record => "record",
        }
    }

    pub fn of(word: &str) -> Option<HoldingClass> {
        Some(match word {
            "compute" => HoldingClass::Compute,
            "work" => HoldingClass::Work,
            "cache" => HoldingClass::Cache,
            "record" => HoldingClass::Record,
            _ => return None,
        })
    }
}

/// Where one holding stands (`docs/lifecycle.md` d.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldingState {
    /// Written before the act; the resource may not exist yet.
    Intended,
    /// Seen and the owner's.
    Held,
    /// Being ended.
    Reaping,
    /// The row is here and the resource is not.
    Gone,
    /// Looked at, and left, with the reason in `why`.
    Kept,
    /// A step failed twice.
    Stuck,
    /// On a host this box could not reach.
    Lost,
}

impl HoldingState {
    pub fn word(self) -> &'static str {
        match self {
            HoldingState::Intended => "intended",
            HoldingState::Held => "held",
            HoldingState::Reaping => "reaping",
            HoldingState::Gone => "gone",
            HoldingState::Kept => "kept",
            HoldingState::Stuck => "stuck",
            HoldingState::Lost => "lost",
        }
    }

    pub fn of(word: &str) -> Option<HoldingState> {
        Some(match word {
            "intended" => HoldingState::Intended,
            "held" => HoldingState::Held,
            "reaping" => HoldingState::Reaping,
            "gone" => HoldingState::Gone,
            "kept" => HoldingState::Kept,
            "stuck" => HoldingState::Stuck,
            "lost" => HoldingState::Lost,
            _ => return None,
        })
    }

    /// Is the resource still said to be there?
    pub fn present(self) -> bool {
        matches!(
            self,
            HoldingState::Intended
                | HoldingState::Held
                | HoldingState::Reaping
                | HoldingState::Kept
        )
    }
}

/// One owner, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRow {
    pub id: String,
    pub kind: OwnerKind,
    pub parent_id: Option<String>,
    /// The main checkout this owner works against: what a directory stamp is resolved under.
    pub repo: Option<String>,
    pub state: OwnerState,
    /// The operator's hold: `None` none, `Some(0)` for ever.
    pub held_until_ms: Option<i64>,
    pub lease_ms: i64,
    pub idle_ttl_ms: Option<i64>,
    pub hard_ttl_ms: Option<i64>,
    pub closed_ms: Option<i64>,
    pub closed_why: Option<String>,
    pub landed_sha: Option<String>,
    pub settled_ms: Option<i64>,
    pub created_ms: i64,
}

impl OwnerRow {
    /// A live owner of `kind`, with no TTL and no children - what a session's open writes before
    /// anything it creates.
    pub fn live(id: impl Into<String>, kind: OwnerKind, repo: Option<String>, now_ms: i64) -> Self {
        OwnerRow {
            id: id.into(),
            kind,
            parent_id: None,
            repo,
            state: OwnerState::Live,
            held_until_ms: None,
            lease_ms: now_ms,
            idle_ttl_ms: None,
            hard_ttl_ms: None,
            closed_ms: None,
            closed_why: None,
            landed_sha: None,
            settled_ms: None,
            created_ms: now_ms,
        }
    }

    /// Is the operator holding it? `Some(0)` never expires.
    pub fn held(&self, now_ms: i64) -> bool {
        match self.held_until_ms {
            Some(0) => true,
            Some(until) => until > now_ms,
            None => false,
        }
    }
}

/// One holding, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldingRow {
    pub owner_id: String,
    pub kind: String,
    pub host: Option<String>,
    pub handle: String,
    pub class: HoldingClass,
    pub state: HoldingState,
    /// What on the resource proves it is this owner's, as last checked.
    pub stamp: String,
    pub bytes: Option<u64>,
    pub archive: Option<String>,
    pub why: Option<String>,
    pub created_ms: i64,
    pub checked_ms: Option<i64>,
    pub attempts: i64,
}

impl HoldingRow {
    pub fn new(
        owner_id: impl Into<String>,
        kind: impl Into<String>,
        class: HoldingClass,
        handle: impl Into<String>,
        stamp: impl Into<String>,
        now_ms: i64,
    ) -> Self {
        HoldingRow {
            owner_id: owner_id.into(),
            kind: kind.into(),
            host: None,
            handle: handle.into(),
            class,
            state: HoldingState::Held,
            stamp: stamp.into(),
            bytes: None,
            archive: None,
            why: None,
            created_ms: now_ms,
            checked_ms: Some(now_ms),
            attempts: 0,
        }
    }

    pub fn on_host(mut self, host: Option<String>) -> Self {
        self.host = host;
        self
    }

    pub fn measured(mut self, bytes: Option<u64>) -> Self {
        self.bytes = bytes;
        self
    }
}

/// One act in the journal (`docs/lifecycle.md` d.2's `reap_event.act`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapAct {
    Plan,
    Signal,
    Kill,
    Down,
    Archive,
    Remove,
    Keep,
    Stuck,
    Restore,
}

impl ReapAct {
    pub fn word(self) -> &'static str {
        match self {
            ReapAct::Plan => "plan",
            ReapAct::Signal => "signal",
            ReapAct::Kill => "kill",
            ReapAct::Down => "down",
            ReapAct::Archive => "archive",
            ReapAct::Remove => "remove",
            ReapAct::Keep => "keep",
            ReapAct::Stuck => "stuck",
            ReapAct::Restore => "restore",
        }
    }
}

/// One journal line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapEvent {
    pub at_ms: i64,
    pub owner_id: String,
    pub kind: Option<String>,
    pub host: Option<String>,
    pub handle: Option<String>,
    pub act: String,
    pub said: String,
    pub by: String,
}

const OWNER_COLUMNS: &str = "id, kind, parent_id, repo, state, held_until_ms, lease_ms, idle_ttl_ms, \
                             hard_ttl_ms, closed_ms, closed_why, landed_sha, settled_ms, created_ms";

fn owner_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<OwnerRow> {
    let kind: String = r.get(1)?;
    let state: String = r.get(4)?;
    Ok(OwnerRow {
        id: r.get(0)?,
        // A word this build does not know is not evidence of anything an owner can be reaped on:
        // `session` is the kind nothing closes for idleness, so it is the conservative answer.
        kind: OwnerKind::of(&kind).unwrap_or(OwnerKind::Session),
        parent_id: r.get(2)?,
        repo: r.get(3)?,
        state: OwnerState::of(&state).unwrap_or(OwnerState::Live),
        held_until_ms: r.get(5)?,
        lease_ms: r.get(6)?,
        idle_ttl_ms: r.get(7)?,
        hard_ttl_ms: r.get(8)?,
        closed_ms: r.get(9)?,
        closed_why: r.get(10)?,
        landed_sha: r.get(11)?,
        settled_ms: r.get(12)?,
        created_ms: r.get(13)?,
    })
}

const HOLDING_COLUMNS: &str = "owner_id, kind, host, handle, class, state, stamp, bytes, archive, \
                               why, created_ms, checked_ms, attempts";

fn holding_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<HoldingRow> {
    let class: String = r.get(4)?;
    let state: String = r.get(5)?;
    Ok(HoldingRow {
        owner_id: r.get(0)?,
        kind: r.get(1)?,
        host: r.get::<_, Option<String>>(2)?.filter(|h| !h.is_empty()),
        handle: r.get(3)?,
        // `work` is the class nothing removes without an archive: the safe reading of a word from
        // a newer build.
        class: HoldingClass::of(&class).unwrap_or(HoldingClass::Work),
        // Likewise the state: a row this build cannot read is one it must not reap.
        state: HoldingState::of(&state).unwrap_or(HoldingState::Kept),
        stamp: r.get(6)?,
        bytes: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        archive: r.get(8)?,
        why: r.get(9)?,
        created_ms: r.get(10)?,
        checked_ms: r.get(11)?,
        attempts: r.get(12)?,
    })
}

impl Store {
    /// Write one owner, replacing that id's row. An upsert rather than an append: an owner has one
    /// state at a time, and a re-open moves it back to `live` on the same row.
    pub fn put_owner(&self, o: &OwnerRow) -> Result<()> {
        self.exec(
            "INSERT INTO owner (id, kind, parent_id, repo, state, held_until_ms, lease_ms,
                                idle_ttl_ms, hard_ttl_ms, closed_ms, closed_why, landed_sha,
                                settled_ms, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET
               kind = excluded.kind, parent_id = excluded.parent_id, repo = excluded.repo,
               state = excluded.state, held_until_ms = excluded.held_until_ms,
               lease_ms = excluded.lease_ms, idle_ttl_ms = excluded.idle_ttl_ms,
               hard_ttl_ms = excluded.hard_ttl_ms, closed_ms = excluded.closed_ms,
               closed_why = excluded.closed_why, landed_sha = excluded.landed_sha,
               settled_ms = excluded.settled_ms",
            params![
                o.id,
                o.kind.word(),
                o.parent_id,
                o.repo,
                o.state.word(),
                o.held_until_ms,
                o.lease_ms,
                o.idle_ttl_ms,
                o.hard_ttl_ms,
                o.closed_ms,
                o.closed_why,
                o.landed_sha,
                o.settled_ms,
                o.created_ms,
            ],
        )?;
        Ok(())
    }

    /// One owner by id. `None` for an id this store never wrote - which is a holding that names no
    /// owner, and the one case a reaper must report rather than act on.
    pub fn owner(&self, id: &str) -> Result<Option<OwnerRow>> {
        Ok(self
            .conn()
            .prepare(&format!("SELECT {OWNER_COLUMNS} FROM owner WHERE id = ?1"))?
            .query_row([id], owner_row)
            .optional()?)
    }

    /// Every owner, oldest first.
    pub fn owners(&self) -> Result<Vec<OwnerRow>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {OWNER_COLUMNS} FROM owner ORDER BY created_ms, id"
        ))?;
        let rows = st.query_map([], owner_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Move an owner to `state`, keeping the two clocks that date a close.
    pub fn set_owner_state(
        &self,
        id: &str,
        state: OwnerState,
        why: &str,
        now_ms: i64,
    ) -> Result<()> {
        self.exec(
            "UPDATE owner SET state = ?2, closed_ms = COALESCE(closed_ms, ?3),
                              closed_why = COALESCE(closed_why, ?4)
               WHERE id = ?1",
            params![id, state.word(), now_ms, why],
        )?;
        Ok(())
    }

    /// Write one holding, replacing the row its three keys name. An upsert, so a discovery pass
    /// re-inserts a row somebody deleted by hand instead of colliding with nothing. `host` is
    /// written as `''` for this box and read back as `None`: SQLite treats NULLs in a primary key
    /// as distinct, so a NULL there would make every write a new row.
    pub fn put_holding(&self, h: &HoldingRow) -> Result<()> {
        self.exec(
            "INSERT INTO holding (owner_id, kind, host, handle, class, state, stamp, bytes,
                                  archive, why, created_ms, checked_ms, attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(owner_id, kind, host, handle) DO UPDATE SET
               class = excluded.class, state = excluded.state, stamp = excluded.stamp,
               bytes = COALESCE(excluded.bytes, holding.bytes),
               archive = COALESCE(excluded.archive, holding.archive),
               why = COALESCE(excluded.why, holding.why),
               checked_ms = excluded.checked_ms",
            params![
                h.owner_id,
                h.kind,
                h.host.clone().unwrap_or_default(),
                h.handle,
                h.class.word(),
                h.state.word(),
                h.stamp,
                h.bytes.map(|b| b as i64),
                h.archive,
                h.why,
                h.created_ms,
                h.checked_ms,
                h.attempts,
            ],
        )?;
        Ok(())
    }

    /// Every holding of one owner, by kind then handle.
    pub fn holdings_of(&self, owner_id: &str) -> Result<Vec<HoldingRow>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {HOLDING_COLUMNS} FROM holding WHERE owner_id = ?1
              ORDER BY class, kind, handle"
        ))?;
        let rows = st.query_map([owner_id], holding_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every holding in the store, by owner - the plan's whole input.
    pub fn holdings(&self) -> Result<Vec<HoldingRow>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {HOLDING_COLUMNS} FROM holding ORDER BY owner_id, class, kind, handle"
        ))?;
        let rows = st.query_map([], holding_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Append one journal line. There is no update and no delete: what was done to a holding is a
    /// fact, and a removal nobody can read back afterwards is one nobody can argue with.
    pub fn put_reap_event(&self, e: &ReapEvent) -> Result<()> {
        self.exec(
            "INSERT INTO reap_event (at_ms, owner_id, kind, host, handle, act, said, by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.at_ms, e.owner_id, e.kind, e.host, e.handle, e.act, e.said, e.by
            ],
        )?;
        Ok(())
    }

    /// The journal of one owner, oldest first.
    pub fn reap_events_of(&self, owner_id: &str) -> Result<Vec<ReapEvent>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT at_ms, owner_id, kind, host, handle, act, said, by
               FROM reap_event WHERE owner_id = ?1 ORDER BY at_ms, rowid",
        )?;
        let rows = st.query_map([owner_id], |r| {
            Ok(ReapEvent {
                at_ms: r.get(0)?,
                owner_id: r.get(1)?,
                kind: r.get(2)?,
                host: r.get(3)?,
                handle: r.get(4)?,
                act: r.get(5)?,
                said: r.get(6)?,
                by: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().expect("an in-memory store")
    }

    /// An owner written before anything it creates is there, and reads back as what it was. The
    /// three lifetimes are three columns, so reopening after a close does not lose the close.
    #[test]
    fn an_owner_is_a_row_and_its_close_is_a_clock() {
        let s = store();
        let mut o = OwnerRow::live("s-1", OwnerKind::Package, Some("/w".into()), 100);
        o.parent_id = None;
        o.idle_ttl_ms = Some(30 * 60 * 1000);
        s.put_owner(&o).expect("the owner");
        let back = s.owner("s-1").expect("a read").expect("a row");
        assert_eq!(back, o);

        s.set_owner_state("s-1", OwnerState::Closed, "idle 30m", 500)
            .expect("closed");
        let back = s.owner("s-1").unwrap().unwrap();
        assert_eq!(back.state, OwnerState::Closed);
        assert_eq!(back.closed_ms, Some(500));
        assert_eq!(back.closed_why.as_deref(), Some("idle 30m"));
        // Re-opening moves it back and does not rewrite the clock that dates the first close.
        s.set_owner_state("s-1", OwnerState::Live, "", 900)
            .expect("live");
        let back = s.owner("s-1").unwrap().unwrap();
        assert_eq!(back.state, OwnerState::Live);
        assert_eq!(back.closed_ms, Some(500), "the close is a fact");
    }

    /// A holding's key is `(owner, kind, host, handle)`: writing it twice is one row, and a row
    /// deleted by hand comes back with its size and archive when it is written again.
    #[test]
    fn a_holding_is_one_row_per_resource_and_an_upsert() {
        let s = store();
        let h = HoldingRow::new(
            "s-1",
            "scope",
            HoldingClass::Compute,
            "session.s-1",
            "cgroup",
            10,
        )
        .measured(Some(4096));
        s.put_holding(&h).expect("the holding");
        s.put_holding(&HoldingRow {
            state: HoldingState::Gone,
            ..h.clone()
        })
        .expect("the same holding, gone");
        let rows = s.holdings_of("s-1").expect("a read");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, HoldingState::Gone);
        assert_eq!(
            rows[0].bytes,
            Some(4096),
            "a measurement survives a rewrite"
        );
        // No host and a host are two resources, not one.
        s.put_holding(
            &HoldingRow::new(
                "s-1",
                "home",
                HoldingClass::Cache,
                "/root/.letibot-home/s-1",
                "owner.json",
                10,
            )
            .on_host(Some("lubuntu2".into())),
        )
        .expect("the remote one");
        assert_eq!(s.holdings_of("s-1").unwrap().len(), 2);
    }

    /// The journal is append-only and answers one owner's story in order.
    #[test]
    fn the_journal_keeps_every_act_in_order() {
        let s = store();
        for (at, act) in [(30, ReapAct::Remove), (10, ReapAct::Plan)] {
            s.put_reap_event(&ReapEvent {
                at_ms: at,
                owner_id: "s-1".into(),
                kind: Some("worktree".into()),
                host: None,
                handle: Some("/w/wt".into()),
                act: act.word().into(),
                said: format!("{act:?}"),
                by: "gc timer".into(),
            })
            .expect("a line");
        }
        let log = s.reap_events_of("s-1").expect("the journal");
        assert_eq!(
            log.iter().map(|e| e.act.as_str()).collect::<Vec<_>>(),
            ["plan", "remove"]
        );
    }

    /// A word this build does not know is read as the conservative value, not as corruption: the
    /// kind nothing closes for idleness, the class nothing removes without an archive, the state
    /// nothing reaps.
    #[test]
    fn a_word_from_a_newer_build_reads_conservatively() {
        let s = store();
        s.exec(
            "INSERT INTO owner (id, kind, state, lease_ms, created_ms)
             VALUES ('s-9', 'future-kind', 'future-state', 1, 1)",
            [],
        )
        .expect("a row a newer build wrote");
        let o = s.owner("s-9").unwrap().unwrap();
        assert_eq!(o.kind, OwnerKind::Session);
        assert_eq!(o.state, OwnerState::Live);
        s.exec(
            "INSERT INTO holding (owner_id, kind, handle, class, state, stamp, created_ms)
             VALUES ('s-9', 'k', 'h', 'future-class', 'future-state', 's', 1)",
            [],
        )
        .expect("a holding a newer build wrote");
        let h = &s.holdings_of("s-9").unwrap()[0];
        assert_eq!(h.class, HoldingClass::Work);
        assert_eq!(h.state, HoldingState::Kept);
    }
}
