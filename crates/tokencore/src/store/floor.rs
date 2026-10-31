//! **The reader floor** — the lowest schema a reader may know and still read this store.
//!
//! One `sessions.db` is shared by every daemon on the box (measured 2026-10-10: ~11 daemons and
//! dozens of connections on one 2.1 GB file), so a schema bump is not a private act. It changes a
//! file that a daemon somebody has not restarted, or a binary somebody has not rebuilt, opens the
//! next time it starts a session. Until this module the rule was one line in `migrate` — *a
//! version above [`SCHEMA_VERSION`] is refused* — and on 2026-10-26 a bump 32 → 33, which is one
//! nullable column and the most additive thing a migration can be, refused every build older than
//! it:
//!
//! ```text
//! store: opening "…/sessions.db": store schema is version 33; this build knows 32
//! ```
//!
//! The operator's ruling is the shape of this file: **an older binary keeps working when the newer
//! schema only ADDED things, and says so in red on the head's status line** — and is refused, with
//! the fix named, when the newer schema changed something it reads.
//!
//! # What a floor is, and why it is not the version
//!
//! The version says what the file HOLDS. The floor says what a reader must KNOW to read it, which
//! is a different number the moment any migration breaks an older reader: a store at 33 whose
//! migrations were all additive is readable by a build that knows 24, and a store at 25 whose
//! migration was not is readable by nothing below 25.
//!
//! So **every** migration declares itself once, in [`READER_ARMS`], and [`reader_floor_for`] is the
//! whole of the arithmetic: **the floor of a store at version V is the newest breaking migration at
//! or below V**, or [`FIRST_SCHEMA`] for a store that predates every one of them.
//!
//! # Where it is recorded, and why it is not a version bump of its own
//!
//! In one row of one table, [`READER_FLOOR_SQL`], created `IF NOT EXISTS` at every open of a store
//! this build may write. **Deliberately not a new `SCHEMA_VERSION`**: the binaries that need to be
//! tolerated are exactly the ones that would refuse a bump, so a floor that arrived as a version
//! bump could not do the job it exists for. A table a pre-floor build never reads is invisible to
//! it, which is what makes writing one into a shared store safe.
//!
//! The row is raised, never lowered ([`Store::ensure_reader_floor`]), and it is written only after
//! the version it belongs to — see the note at `migrate`'s call site.
//!
//! # The gap this cannot close, said plainly
//!
//! A store bumped by a build that predates this module carries no floor at all. [`Store::open`]
//! refuses such a store when it is ahead of this build rather than assuming the bump was additive:
//! *nobody said whether it was* is not *it was*, and the whole point of the floor is that a misread
//! of this file costs conversations. The window closes at the next migration applied by a build
//! that knows about floors, which writes the row for every store it touches.

use rusqlite::{Connection, OptionalExtension, params};

use super::{Result, SCHEMA_VERSION, Store, StoreError};

/// **The first schema** — the floor of a store nothing breaking has ever migrated. Version 1 is
/// not a migration here (`migrate`'s `None` arm creates the first schema), it is the ground.
pub const FIRST_SCHEMA: i64 = 1;

/// **The floor's one row.** See the module doc for why the floor is a table and not a version.
///
/// `only_row` is a `CHECK`ed constant rather than a bare row: a second floor row would be two
/// answers to one question, and the table exists so that there is exactly one.
pub const READER_FLOOR_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_reader_floor (
    only_row   INTEGER PRIMARY KEY CHECK (only_row = 1),
    min_reader INTEGER NOT NULL
);
"#;

/// **What a migration does to a reader younger than itself** — the two readings [`READER_ARMS`]
/// is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// **Only adds** — a new table, a nullable column, a new index, or a backfill of a column that
    /// did not exist for an older reader to read. An older reader's `SELECT` never names what
    /// arrived, and its `INSERT` fills the `NOT NULL … DEFAULT` ones with the default.
    Additive,
    /// **Changes or drops what older code READS** — see [`READER_ARMS`]'s own notes for the two
    /// arms this tree has, and the module doc for why a deleted ROW breaks a reader just as hard as
    /// a renamed table.
    Breaks,
}

/// **One row per arm of `migrate_from`, and what that arm does to an older reader** — every
/// version from 2 to [`super::SCHEMA_VERSION`], read one arm at a time on 2026-10-26.
///
/// **Declared per arm rather than defaulted to additive**, which is the whole of this list's
/// design: a migration that says nothing about readers would be a migration whose silence reads as
/// *safe*, and silence is exactly what a careless `DELETE FROM` looks like. So adding an arm is two
/// places — `migrate_from` and a row here — and `the_floor_names_every_arm` in `store/tests.rs`
/// reads `store.rs` and refuses an arm with no row.
///
/// A migration breaks a reader when it **changes or drops something older code reads**. That is a
/// narrower test than "it deletes something", and a wider one than "it changes the schema": a
/// migration that rewrites the ROWS of a table older code acts on breaks it just as thoroughly as
/// one that renames the table, and neither is visible in the column list.
///
/// **23 — it emptied the queue.** The arm `DELETE FROM merge_queue` (and its pair) threw away every
/// entry a pre-split store held, in a shape a v22 reader reads and acts on: such a daemon opening
/// this store finds an empty queue, re-enqueues nothing, and reports *nothing is queued* about work
/// that is on disk and gone. There is no marker anywhere that says the rows were cleared, so the
/// older reader cannot even be told what it is missing.
///
/// **24 — it took the tables away.** From [`super::queues`]'s own header: the v23 pair is archived
/// as `merge_queue_v23`/`merge_review_v23` and the new `merge_queue` shares no column list with the
/// old one, so a v23 reader's `SELECT … needs_json … FROM merge_queue` fails with `no such column`.
/// That was written as *"the second wall against old binaries"*, and it is exactly what a floor is
/// for, said before there was one.
///
/// **Everything else is additive**, and each row says which kind of additive it is rather than
/// leaning on the group argument: a new table (an older reader never names it), a new column (its
/// `SELECT` never mentions it), a new index, or v8's backfill — which is the one arm that writes
/// existing rows and still does not break a reader, because the columns it fills did not exist for
/// a v7 reader to read.
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
];

/// **The floor a store at `version` carries**: the newest arm at or below `version` that [`Arm::Breaks`],
/// or [`FIRST_SCHEMA`] when none does.
///
/// `const` because it is compiled into every open and there is nothing to compute at run time: a
/// short list and a comparison per entry.
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

/// **A store this build opened although a newer build wrote it.**
///
/// The fact a daemon has to say out loud, in the one place it can say it to everybody: the file is
/// ahead of the binary, this build can still read it, and the process that should be restarted is
/// this one. See [`Store::newer_store`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewerStore {
    /// The version the FILE carries.
    pub found: i64,
    /// The version this build knows.
    pub known: i64,
}

impl NewerStore {
    /// **The sentence**, in the operator's own terms — what is wrong, and the command that fixes
    /// it. The daemon appends which binary it is; the store cannot know that.
    ///
    /// Deliberately short, and the fix deliberately at the end of the first clause: this is drawn
    /// on the status row, which is one line and is trimmed from the right.
    pub fn sentence(&self) -> String {
        format!(
            "this daemon (schema {}) is older than its store ({}) — restart it: letibot --stop",
            self.known, self.found
        )
    }
}

/// **What the floor row says in `schema`** — `"main"` for the store's own file, `"live"` for the
/// one a [`super::live_view::LiveView`] attached read-only. `None` when the store has no floor row
/// at all, which is a store written by a build from before [`READER_FLOOR_SQL`].
///
/// A `&Connection` rather than `&Store` because the queues ask this inside an open transaction
/// (their claim reads the file's schema live, in the same transaction, so a daemon already running
/// when a newer build migrates the file finds out at its next claim).
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

/// **May a reader that knows [`SCHEMA_VERSION`] read a store at `stored`?** — the whole rule, in
/// one function, because three doors ask it: the open ([`Store::open_newer`]), the merge queue's
/// claim (which re-reads the version live, in its own transaction, so a daemon already running
/// when a newer build migrates the file finds out at its next claim) and anything else that
/// re-reads the version rather than trusting the one it opened at.
///
/// * **Below this build: no.** A store older than the code was migrated by somebody else's
///   migration and this build's schema does not match; what the caller does about that is its
///   own business.
/// * **At this build: yes**, and the floor is not consulted — the floor says what an OLDER reader
///   may know, and this is not one.
/// * **Above this build: only if the floor allows it**, and **no floor row means no** — see the
///   module doc's last section.
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
    /// **The store is written by a newer build and this one opened it anyway** — or `None`, which
    /// is every ordinary open and includes a store this build just migrated.
    ///
    /// The caller is the daemon, and what it owes this fact is a line in front of every head: a
    /// session served by a binary older than its store behaves correctly and is one restart away
    /// from being right, and the operator is the only one who can make that restart happen.
    pub fn newer_store(&self) -> Option<NewerStore> {
        *self.shared.newer.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **The floor this store records**, or `None` for one written before there were floors.
    pub(crate) fn reader_floor(&self) -> Result<Option<i64>> {
        read_floor(&self.conn(), "main")
    }

    /// **Record the floor a store at `at_version` carries, raising the row and never lowering
    /// it**, and answer what the row holds now.
    ///
    /// Called at every open of a store this build may write, so that a store created by a build
    /// from before this module gains a floor the first time a build that knows about them touches
    /// it — which is the only repair available for the gap the module doc describes.
    ///
    /// **Read first, write only when the row is behind.** The common open — an up-to-date store
    /// that already says 24 — then costs one `SELECT` and no write at all, which matters on the
    /// shared file this exists to protect: eleven daemons take the single write lock, and an open
    /// that queued behind one to write a row it already has would be a cost paid forever for a
    /// fact that changes twice in a schema's life.
    pub(crate) fn ensure_reader_floor(&self, at_version: i64) -> Result<i64> {
        let want = reader_floor_for(at_version);
        if let Some(have) = self.reader_floor()?
            && have >= want
        {
            return Ok(have);
        }
        self.conn().execute_batch(READER_FLOOR_SQL)?;
        // `INSERT OR IGNORE` then `max`, rather than an upsert: this is the spelling that says
        // *a floor moves up or not at all*, and it is the same statement whether the row is
        // absent (a store from before this table) or behind (a schema that grew a breaking
        // migration since).
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

    /// **`migrate`'s answer to a store AHEAD of this build** — open it when the floor says older
    /// code can read it, refuse it by name when it does not.
    ///
    /// **This arm writes nothing**: not the version (a lower build must never stamp its own over a
    /// higher one), not the floor (it is the newer build's declaration to make, and a lower build
    /// has nothing to say about a migration it has never seen).
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
