//! The reader floor: the lowest schema a reader may know and still read this store.
//!
//! One `sessions.db` is shared by every daemon on the box, so a newer build that only ADDS
//! leaves older daemons reading it, and one that rewrites what an older reader reads says so
//! here, by number. The floor is a row rather than a version because it moves up or not at
//! all, and a reader below it is refused by name.
//!
//! ```text
//! store: opening "…/sessions.db": store schema is version 33; this build knows 32
//! ```

use rusqlite::{Connection, OptionalExtension, params};

use super::{Result, SCHEMA_VERSION, Store, StoreError};

/// **The first schema** - the floor of a store nothing breaking has ever migrated. Version 1 is
/// not a migration here (`migrate`'s `None` arm creates the first schema), it is the ground.
pub const FIRST_SCHEMA: i64 = 1;

/// The floor's one row: `only_row` is a `CHECK`ed constant rather than a bare row, so a second
/// row cannot exist. See the module doc for why the floor is a table.
pub const READER_FLOOR_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_reader_floor (
    only_row   INTEGER PRIMARY KEY CHECK (only_row = 1),
    min_reader INTEGER NOT NULL
);
"#;

/// **What a migration does to a reader younger than itself** - the two readings [`READER_ARMS`]
/// is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// **Only adds** - a new table, a nullable column, a new index, or a backfill of a column that
    /// did not exist for an older reader to read. An older reader's `SELECT` never names what
    /// arrived, and its `INSERT` fills the `NOT NULL … DEFAULT` ones with the default.
    Additive,
    /// **Changes or drops what older code READS** - see [`READER_ARMS`]'s own notes for the two
    /// arms this tree has, and the module doc for why a deleted ROW breaks a reader just as hard as
    /// a renamed table.
    Breaks,
}

/// One row per arm of `migrate_from`, and what that arm does to an older reader: every version
/// from 2 to [`super::SCHEMA_VERSION`], read one arm at a time against the source, so an arm
/// that forgets to declare itself fails a test rather than a daemon.
pub const READER_ARMS: &[(i64, Arm)] = &[
    (2, Arm::Additive),  // role: a NULLed column
    (3, Arm::Additive),  // todo: a new table (and its index)
    (4, Arm::Additive),  // parent_session_id: a NULLed column
    (5, Arm::Additive),  // adjudication: a new table and two indexes
    (6, Arm::Additive),  // adjudication.shape: a NULLed column
    (7, Arm::Additive),  // adjudication.shape_class: a NULLed column
    (8, Arm::Additive),  // context_tokens/cached: new columns, and a backfill OF THEM
    (9, Arm::Additive),  // adjudication.oracle_reply: a NULLed column
    (10, Arm::Additive), // adjudication.consulted: a NULLed column
    (11, Arm::Additive), // adjudication.oracle_reading: a NULLed column
    (12, Arm::Additive), // session.context_ledger: a NULLed column
    (13, Arm::Additive), // session.provider_choice: a NULLed column
    (14, Arm::Additive), // session.context_window: a NULLed column
    (15, Arm::Additive), // job: a new table
    (16, Arm::Additive), // merge_queue: a new table and its index
    (17, Arm::Additive), // merge_queue.brief: a defaulted column
    (18, Arm::Additive), // merge_review: a new table
    (19, Arm::Additive), // session.auto_compact_*: NULLed columns
    (20, Arm::Additive), // merge_review.attempts/failed_ms/failure: defaulted columns
    (21, Arm::Additive), // session.prefix_fingerprint: a NULLed column
    (22, Arm::Additive), // merge_queue.gate_steps_json: a defaulted column
    (23, Arm::Breaks),   // the row this file's doc explains: the queue is CLEARED
    (24, Arm::Breaks),   // and this one: the v23 tables are archived, new ones in their place
    (25, Arm::Additive), // session.nags_paused_until: a NULLed column
    (26, Arm::Additive), // subagent_run: a new table and its index
    (27, Arm::Additive), // project_mode/session_mode: new tables
    (28, Arm::Additive), // job.delivered: a defaulted column
    (29, Arm::Additive), // queue_told: a new table
    (30, Arm::Additive), // citation: a new table
    (31, Arm::Additive), // session.lives_on/moved_ms: NULLed columns
    (32, Arm::Additive), // session_writer: a new table
    (33, Arm::Additive), // session.origin: a NULLed column
    (34, Arm::Additive), // process: a new table (the supervisor's records)
    (35, Arm::Additive), // host_lease: a new table (a block of a shared host's ports)
    (36, Arm::Additive), // subagent_run.later_answer: a NULLed column
    (37, Arm::Breaks),   // the `notice` transcript row: a younger reader cannot deserialize it
];

/// The floor a store at `version` carries: the newest arm at or below it that [`Arm::Breaks`],
/// or [`FIRST_SCHEMA`] when none does.
pub const fn reader_floor_for(version: i64) -> i64 {
    let mut floor = FIRST_SCHEMA;
    let mut i = 0;
    while i < READER_ARMS.len() {
        let (at, arm) = READER_ARMS[i];
        if at > version {
            break;
        }
        if let Arm::Breaks = arm {
            floor = at;
        }
        i += 1;
    }
    floor
}

/// A store this build opened although a newer build wrote it: the fact a daemon has to say out
/// loud, in the one place it can say it to everybody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewerStore {
    /// The version the FILE carries.
    pub found: i64,
    /// The version this build knows.
    pub known: i64,
}

impl NewerStore {
    /// The sentence in the operator's own terms: what is wrong, and the command that fixes it. The
    /// daemon appends which binary it is; the store cannot know that.
    pub fn sentence(&self) -> String {
        format!(
            "this daemon (schema {}) is older than its store ({}) — restart it: letibot --stop",
            self.known, self.found
        )
    }
}

/// What the floor row says in `schema`: `"main"` for the store's own file, `"live"` for the
/// one a [`super::live_view::LiveView`] attached read-only, `None` when neither.
pub(crate) fn read_floor(conn: &Connection, schema: &str) -> Result<Option<i64>> {
    let there: bool = conn
        .prepare(&format!(
            "SELECT 1 FROM {schema}.sqlite_master WHERE type = 'table' AND name = 'schema_reader_floor'"
        ))
        .and_then(|mut st| st.exists([]))
        .unwrap_or(false);
    if !there {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            &format!("SELECT min_reader FROM {schema}.schema_reader_floor WHERE only_row = 1"),
            [],
            |r| r.get(0),
        )
        .optional()?)
}

/// May a reader that knows [`SCHEMA_VERSION`] read a store at `stored`? The whole rule in one
/// function, because three doors ask it: the open, the queue lease and a read-only view.
pub(crate) fn readable_here(conn: &Connection, stored: i64) -> Result<bool> {
    if stored < SCHEMA_VERSION {
        return Ok(false);
    }
    if stored == SCHEMA_VERSION {
        return Ok(true);
    }
    Ok(read_floor(conn, "main")?.is_some_and(|floor| floor <= SCHEMA_VERSION))
}

impl Store {
    /// The store is written by a newer build and this one opened it anyway, or `None`, which is
    /// every ordinary open and includes a store this build just migrated.
    pub fn newer_store(&self) -> Option<NewerStore> {
        *self.shared.newer.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **The floor this store records**, or `None` for one written before there were floors.
    pub(crate) fn reader_floor(&self) -> Result<Option<i64>> {
        read_floor(&self.conn(), "main")
    }

    /// Record the floor a store at `at_version` carries, raising the row and never lowering it, and
    /// answer what the row holds now. Called at every open, so a store from an older build
    /// gains its floor on the first touch.
    pub(crate) fn ensure_reader_floor(&self, at_version: i64) -> Result<i64> {
        let want = reader_floor_for(at_version);
        if let Some(have) = self.reader_floor()?
            && have >= want
        {
            return Ok(have);
        }
        self.conn().execute_batch(READER_FLOOR_SQL)?;
        // `INSERT OR IGNORE` then `max`, rather than an upsert: this spelling says a floor moves up
        // or not at all, and the statement is the same whether the row exists.
        self.conn().execute(
            "INSERT OR IGNORE INTO schema_reader_floor (only_row, min_reader) VALUES (1, ?1)",
            params![want],
        )?;
        self.conn().execute(
            "UPDATE schema_reader_floor SET min_reader = max(min_reader, ?1) WHERE only_row = 1",
            params![want],
        )?;
        Ok(self.reader_floor()?.unwrap_or(want))
    }

    /// `migrate`'s answer to a store AHEAD of this build: open it when the floor says older code
    /// can read it, refuse it by name when it does not.
    pub(crate) fn open_newer(&self, found: i64) -> Result<()> {
        let floor = self.reader_floor()?;
        match floor {
            Some(floor) if floor <= SCHEMA_VERSION => {
                *self.shared.newer.lock().unwrap_or_else(|e| e.into_inner()) = Some(NewerStore {
                    found,
                    known: SCHEMA_VERSION,
                });
                Ok(())
            }
            floor => Err(StoreError::SchemaTooNew {
                found,
                known: SCHEMA_VERSION,
                floor,
            }),
        }
    }
}
