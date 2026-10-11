//! **The review queue and the merge queue — two queues, one store, per repository (R61).**
//!
//! The operator: *"review is review and merge is merge. separate steps. separate queues."* and
//! *"the merge/review queue is a center-piece and it was never reliable so far."* This module is
//! the durable half of both, written against the review of 2026-10-11 (TODO.md R61), whose five
//! requirements are the reason for every shape here:
//!
//! 1. **One owner per queue, with a live heartbeat** — [`QueueLease`], one row per repository,
//!    claimed by compare-and-swap ([`Store::acquire_queue_lease`]) and renewed by a heartbeat that
//!    names its owner ([`Store::heartbeat_queue_lease`]). A daemon whose binary does not know the
//!    store's schema never takes one: the claim reads `schema_version` LIVE, in the same
//!    transaction, so a daemon that was running before a newer build migrated the file finds out
//!    at its next claim rather than serving a table it no longer understands.
//! 2. **Every state change is a conditional move, with history.** No whole-row upserts:
//!    [`Store::move_review`] and [`Store::move_merge`] are `UPDATE … WHERE id = ? AND state = ?
//!    [AND owner = ?]`, and a move that matched a row appends one [`QueueTransition`] (from, to,
//!    at, by, why) in the same transaction. The transition table is append-only by trigger, the
//!    way `transcript_item` is, so *time in state* and *why stuck* are read and never guessed.
//! 3. **The verdict is bound to a commit.** A review row records the head it put in front of the
//!    reviewer (`head_sha`) and the head the verdict judged (`reviewed_sha`); the hand-off writes
//!    `approved_sha` onto the merge row in the same transaction as the verdict
//!    ([`Store::approve_review`]), and the merge queue lands only that head.
//! 4. **Every outcome is its own word** — `timed_out`, `blocked` (infrastructure) and `failed`
//!    (the branch is bad) are three states, not one with three sentences.
//! 5. **Red target is first-class** — [`TargetHealth`], one row per (repository, target branch),
//!    and a review row of kind [`ReviewKind::Target`] for the red tip.
//!
//! # Why the old tables are RENAMED and not reused (schema 24)
//!
//! The v23 `merge_queue`/`merge_review` pair is archived as `merge_queue_v23`/`merge_review_v23`
//! — every row kept, nothing deleted (the operator's decision 13: *"archive, don't delete"*) — and
//! the new `merge_queue` deliberately shares no column list with the old one: it has a
//! `repository NOT NULL` with no default and no `needs_json`. That is the second wall against old
//! binaries, and the one that matters for a daemon ALREADY RUNNING when this build migrates the
//! file: such a daemon never re-reads `schema_version` (the open-time check stops only a binary
//! that opens the file afresh), so its `INSERT INTO merge_queue (…, needs_json, …)` and its
//! `SELECT … needs_json …` now fail with `no such column` — it can neither take an entry nor
//! write one, which is the whole of what it must not do. Measured on 2026-10-11: nine daemons on
//! one `sessions.db`, all on deleted binaries of different builds.

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{MergeGateStep, MergePriority, Result, SCHEMA_VERSION, Store, StoreError};

/// **The two queues' tables, the transition history, the lease and the target's health** — the
/// DDL both a fresh store ([`super::SCHEMA_SQL`] runs it) and the v24 migration run.
///
/// Every table is `IF NOT EXISTS` and every index is named for the NEW tables
/// (`*_repo_state`), because the archived v23 table keeps its own index names
/// (`merge_queue_state_idx`) through the rename, and a new index under an old name would be
/// silently skipped by `IF NOT EXISTS`.
pub const QUEUES_SQL: &str = r#"
-- **The review queue (R61, schema 24).** One row per branch put up for review, per repository.
-- `kind` is `branch` (a branch somebody wants landed) or `target` (the target branch's own RED
-- tip, reviewed first — the operator's *"when main becomes red - main itself goes to review"*).
-- `head_sha` is the head put in front of the reviewer; `reviewed_sha` the head the verdict
-- judged. `owner` is the lease holder that claimed the row while it is `reviewing`, and `''`
-- otherwise. `fixes` is the red target sha a fix branch was opened for, so the merge queue may
-- land that one branch while the target is red.
CREATE TABLE IF NOT EXISTS review_queue (
    id            TEXT PRIMARY KEY,
    repository    TEXT NOT NULL,
    target        TEXT NOT NULL,
    kind          TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    branch        TEXT NOT NULL,
    worktree      TEXT,
    base_sha      TEXT NOT NULL,
    head_sha      TEXT NOT NULL,
    brief         TEXT NOT NULL,
    priority      TEXT NOT NULL,
    fixes         TEXT,
    state         TEXT NOT NULL,
    owner         TEXT NOT NULL DEFAULT '',
    reviewer      TEXT,
    deadline_ms   INTEGER,
    attempts      INTEGER NOT NULL DEFAULT 0,
    failure       TEXT NOT NULL DEFAULT '',
    verdict       TEXT,
    verdict_by    TEXT,
    reviewed_sha  TEXT,
    reasons_json  TEXT NOT NULL DEFAULT '[]',
    files_json    TEXT NOT NULL DEFAULT '[]',
    commands_json TEXT NOT NULL DEFAULT '[]',
    evidence      TEXT NOT NULL,
    created_ms    INTEGER NOT NULL,
    state_ms      INTEGER NOT NULL,
    updated_ms    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS review_queue_repo_state ON review_queue (repository, state);

-- **The merge queue (R61, schema 24).** Approved branches only, each row born in the same
-- transaction as the verdict that approved it (`review_id`), carrying the head that verdict
-- judged (`approved_sha`). `onto_sha` is the target tip the row was last rebased onto,
-- `rebased_sha` the commit the gate judged, `landed_sha` the target's tip after the landing.
-- `pushed` is the push's own sentence (empty until one was tried).
CREATE TABLE IF NOT EXISTS merge_queue (
    id              TEXT PRIMARY KEY,
    review_id       TEXT NOT NULL,
    repository      TEXT NOT NULL,
    target          TEXT NOT NULL,
    branch          TEXT NOT NULL,
    worktree        TEXT,
    approved_sha    TEXT NOT NULL,
    approved_by     TEXT NOT NULL,
    priority        TEXT NOT NULL,
    fixes           TEXT,
    state           TEXT NOT NULL,
    owner           TEXT NOT NULL DEFAULT '',
    onto_sha        TEXT,
    rebased_sha     TEXT,
    landed_sha      TEXT,
    pushed          TEXT NOT NULL DEFAULT '',
    gate_steps_json TEXT NOT NULL DEFAULT '[]',
    evidence        TEXT NOT NULL,
    created_ms      INTEGER NOT NULL,
    state_ms        INTEGER NOT NULL,
    updated_ms      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS merge_queue_repo_state ON merge_queue (repository, state);

-- **Every move of every row, append-only** (requirement 2). `queue` is `review`, `merge` or
-- `target`; `from_state` is NULL on the row's birth. Nothing updates or deletes a transition.
CREATE TABLE IF NOT EXISTS queue_transition (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    queue       TEXT NOT NULL,
    entry_id    TEXT NOT NULL,
    repository  TEXT NOT NULL,
    from_state  TEXT,
    to_state    TEXT NOT NULL,
    at_ms       INTEGER NOT NULL,
    by          TEXT NOT NULL,
    why         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS queue_transition_entry ON queue_transition (queue, entry_id, seq);
CREATE TRIGGER IF NOT EXISTS queue_transition_no_update
BEFORE UPDATE ON queue_transition BEGIN
    SELECT RAISE(ABORT, 'queue_transition is append-only: no UPDATE');
END;
CREATE TRIGGER IF NOT EXISTS queue_transition_no_delete
BEFORE DELETE ON queue_transition BEGIN
    SELECT RAISE(ABORT, 'queue_transition is append-only: no DELETE');
END;

-- **Who serves a repository's queues** (requirement 1, decision 1). One row per repository.
CREATE TABLE IF NOT EXISTS queue_lease (
    repository   TEXT PRIMARY KEY,
    owner        TEXT NOT NULL,
    pid          INTEGER NOT NULL,
    host         TEXT NOT NULL,
    build        TEXT NOT NULL,
    schema       INTEGER NOT NULL,
    heartbeat_ms INTEGER NOT NULL,
    acquired_ms  INTEGER NOT NULL
);

-- **The target branch's own health** (decisions 10, 11): the last tip gated, and what came of it.
CREATE TABLE IF NOT EXISTS target_health (
    repository      TEXT NOT NULL,
    target          TEXT NOT NULL,
    sha             TEXT NOT NULL,
    state           TEXT NOT NULL,
    checked_ms      INTEGER NOT NULL,
    evidence        TEXT NOT NULL,
    gate_steps_json TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (repository, target)
);
"#;

/// **What a review row is about** — a branch somebody wants landed, or the target's own tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKind {
    /// A branch put up to land on the target.
    Branch,
    /// **The target branch's RED tip** — *"when main becomes red - main itself goes to review"*.
    /// Taken first, as soon as a reviewer is free.
    Target,
}

impl ReviewKind {
    pub const ALL: [ReviewKind; 2] = [ReviewKind::Branch, ReviewKind::Target];
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewKind::Branch => "branch",
            ReviewKind::Target => "target",
        }
    }
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == word)
    }
}

/// **Where a review is** — the review queue's closed set. Its outcome is a verdict; it never
/// touches git.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    /// Waiting for a reviewer.
    Waiting,
    /// **A reviewer is on it** — claimed by the lease holder (`owner`), with a deadline.
    Reviewing,
    /// **Accepted, bound to `reviewed_sha`, and handed to the merge queue** in the same
    /// transaction. Terminal for this queue.
    Approved,
    /// The reviewer said no. Terminal; the complaints go back to the branch's session.
    Rejected,
    /// The reviewer could not decide (`needs_human`). Terminal until a person rules.
    CannotDecide,
    /// **The review could not be RUN** — a reviewer that could not start, or attempts spent on
    /// deadlines. Raised at high severity (decision 4); not terminal: a person's `restart` or
    /// `approve` moves it, and a project that opted in merges it via its main session.
    Blocked,
    /// Taken out of the queue — by a person, or because the branch is already on the target.
    Withdrawn,
    /// Carried over from a store that predates the split, as a record and nothing else.
    Archived,
}

impl ReviewState {
    pub const ALL: [ReviewState; 8] = [
        ReviewState::Waiting,
        ReviewState::Reviewing,
        ReviewState::Approved,
        ReviewState::Rejected,
        ReviewState::CannotDecide,
        ReviewState::Blocked,
        ReviewState::Withdrawn,
        ReviewState::Archived,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewState::Waiting => "waiting",
            ReviewState::Reviewing => "reviewing",
            ReviewState::Approved => "approved",
            ReviewState::Rejected => "rejected",
            ReviewState::CannotDecide => "cannot_decide",
            ReviewState::Blocked => "blocked",
            ReviewState::Withdrawn => "withdrawn",
            ReviewState::Archived => "archived",
        }
    }
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == word)
    }
    /// Still the review queue's business: waiting, under review, or blocked and raised.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            ReviewState::Waiting | ReviewState::Reviewing | ReviewState::Blocked
        )
    }
}

/// **Where a landing is** — the merge queue's closed set. No model touches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    /// Approved and waiting its turn.
    Queued,
    /// **The lease holder (`owner`) is rebasing it and running the gate.**
    Gating,
    /// On the target, pushed when there was somewhere to push.
    Landed,
    /// **The gate was red** — the branch is bad. Terminal.
    Failed,
    /// **The gate ran past its deadline and was killed** (decision 9) — not `failed`.
    TimedOut,
    /// The rebase onto the target's tip conflicted. Terminal; reported, never resolved.
    Conflict,
    /// **The branch moved after its approval** (decision 6): the review row went back to
    /// `waiting`, and this row ends here.
    Moved,
    /// **Infrastructure**: the queue could not run the landing (no gate declared, a worktree
    /// that could not be made, a branch git cannot name). Raised; nothing about the branch is
    /// judged.
    Blocked,
    /// Taken out by a person.
    Withdrawn,
}

impl MergeState {
    pub const ALL: [MergeState; 9] = [
        MergeState::Queued,
        MergeState::Gating,
        MergeState::Landed,
        MergeState::Failed,
        MergeState::TimedOut,
        MergeState::Conflict,
        MergeState::Moved,
        MergeState::Blocked,
        MergeState::Withdrawn,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            MergeState::Queued => "queued",
            MergeState::Gating => "gating",
            MergeState::Landed => "landed",
            MergeState::Failed => "failed",
            MergeState::TimedOut => "timed_out",
            MergeState::Conflict => "conflict",
            MergeState::Moved => "moved",
            MergeState::Blocked => "blocked",
            MergeState::Withdrawn => "withdrawn",
        }
    }
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == word)
    }
    /// Still the merge queue's business.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            MergeState::Queued | MergeState::Gating | MergeState::Blocked
        )
    }
}

/// **The target branch's health** — what the last gate of its tip said (decision 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    /// The tip is being gated now.
    Gating,
    /// The gate passed at `sha`.
    Green,
    /// **The gate failed at `sha`** — an emergency: the merge queue lands nothing but the fix.
    Red,
    /// The gate ran past its deadline at `sha`. Nothing lands on a tip nobody could judge.
    TimedOut,
    /// The gate could not run at `sha` (none declared, a worktree that could not be made).
    Blocked,
}

impl HealthState {
    pub const ALL: [HealthState; 5] = [
        HealthState::Gating,
        HealthState::Green,
        HealthState::Red,
        HealthState::TimedOut,
        HealthState::Blocked,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            HealthState::Gating => "gating",
            HealthState::Green => "green",
            HealthState::Red => "red",
            HealthState::TimedOut => "timed_out",
            HealthState::Blocked => "blocked",
        }
    }
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == word)
    }
}

/// **One review row** — see the `review_queue` table in [`QUEUES_SQL`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRow {
    pub id: String,
    /// The repository, canonical (the main checkout's path). `''` on a row carried over from a
    /// v23 store before a daemon resolved it.
    pub repository: String,
    /// The target branch the work is for (decision 12). `''` until resolved, as above.
    pub target: String,
    pub kind: ReviewKind,
    /// The session that enqueued it (the child that finished the branch).
    pub session_id: String,
    pub branch: String,
    pub worktree: Option<String>,
    pub base_sha: String,
    /// The head put in front of the reviewer, `''` when nobody has read it yet.
    pub head_sha: String,
    pub brief: String,
    pub priority: MergePriority,
    /// The red target sha this branch was opened to fix, when it was.
    pub fixes: Option<String>,
    pub state: ReviewState,
    /// The lease holder that claimed it while `reviewing`; `''` otherwise.
    pub owner: String,
    /// The reviewer child's handle, once one was started.
    pub reviewer: Option<String>,
    /// When the reviewer's time is up, Unix ms.
    pub deadline_ms: Option<u64>,
    /// Attempts that ended without a verdict since the last restart.
    pub attempts: u32,
    /// The last attempt's failure, verbatim.
    pub failure: String,
    /// `accept`, `reject` or `needs_human` — the gatekeeper's words — once judged.
    pub verdict: Option<String>,
    /// `reviewer:<handle>` or `operator:<session>`.
    pub verdict_by: Option<String>,
    /// **The head the verdict judged** (decision 6).
    pub reviewed_sha: Option<String>,
    pub reasons: Vec<String>,
    pub files: Vec<String>,
    pub commands: Vec<String>,
    /// The reason for the state, in the queue's words.
    pub evidence: String,
    pub created_ms: u64,
    /// **When the row entered its current state** — what the pane's age is measured from.
    pub state_ms: u64,
    pub updated_ms: u64,
}

/// **One merge row** — see the `merge_queue` table in [`QUEUES_SQL`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRow {
    pub id: String,
    pub review_id: String,
    pub repository: String,
    pub target: String,
    pub branch: String,
    pub worktree: Option<String>,
    /// **The head the approving verdict judged** — the only head this row may land.
    pub approved_sha: String,
    pub approved_by: String,
    pub priority: MergePriority,
    pub fixes: Option<String>,
    pub state: MergeState,
    pub owner: String,
    pub onto_sha: Option<String>,
    pub rebased_sha: Option<String>,
    pub landed_sha: Option<String>,
    pub pushed: String,
    pub gate_steps: Vec<MergeGateStep>,
    pub evidence: String,
    pub created_ms: u64,
    pub state_ms: u64,
    pub updated_ms: u64,
}

/// **One move of one row**, as the transition table keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueTransition {
    pub seq: i64,
    /// `review`, `merge` or `target`.
    pub queue: String,
    pub entry_id: String,
    pub repository: String,
    /// `None` on the row's birth.
    pub from_state: Option<String>,
    pub to_state: String,
    pub at_ms: u64,
    pub by: String,
    pub why: String,
}

/// **The lease on a repository's queues** — who serves them, and how recently it said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueLease {
    pub repository: String,
    /// The owner's identity: host, pid and start time, so a reused pid is not the same owner.
    pub owner: String,
    pub pid: u32,
    pub host: String,
    /// What the owner's binary is, for the pane: a person reading *who serves this* needs to
    /// know which build it is.
    pub build: String,
    /// The schema the owner's binary knows.
    pub schema: i64,
    pub heartbeat_ms: u64,
    pub acquired_ms: u64,
}

/// **Who is asking for a lease** — the claimant's half of [`QueueLease`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseClaim {
    pub owner: String,
    pub pid: u32,
    pub host: String,
    pub build: String,
}

/// **What a claim came to.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseOutcome {
    /// This claimant holds it now. `previous` is the lease it replaced — an EXPIRED one, never
    /// a live one — or `None` when nobody held it or the claimant already did. Recovery reads
    /// it: rows the previous owner held mid-work are the ones it may move.
    Held { previous: Option<QueueLease> },
    /// **Somebody else holds it, and its heartbeat is live.**
    HeldBy(QueueLease),
    /// **This build does not match the store** — the file's schema is not the one this binary
    /// knows. Such a daemon serves no queue (requirement 1).
    WrongSchema { store: i64, mine: i64 },
}

/// **The target branch's health row.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetHealth {
    pub repository: String,
    pub target: String,
    pub sha: String,
    pub state: HealthState,
    pub checked_ms: u64,
    pub evidence: String,
    pub gate_steps: Vec<MergeGateStep>,
}

/// **A conditional move's guard and what it writes.** `expect_owner` is the second half of
/// `WHERE state = ? AND owner = ?`; `sets` are further columns the move writes, by name.
#[derive(Debug, Clone, Default)]
pub struct Move {
    pub by: String,
    pub why: String,
    pub expect_owner: Option<String>,
    pub sets: Vec<(&'static str, Value)>,
}

impl Move {
    pub fn new(by: impl Into<String>, why: impl Into<String>) -> Self {
        Move {
            by: by.into(),
            why: why.into(),
            ..Default::default()
        }
    }
    /// Only if the row's `owner` is this one.
    pub fn owned_by(mut self, owner: impl Into<String>) -> Self {
        self.expect_owner = Some(owner.into());
        self
    }
    /// Write `column` too. The column names are this module's own constants at every call site;
    /// nothing a person types reaches here.
    pub fn set(mut self, column: &'static str, value: impl IntoColumn) -> Self {
        self.sets.push((column, value.into_column()));
        self
    }
    /// Write `column` NULL.
    pub fn clear(mut self, column: &'static str) -> Self {
        self.sets.push((column, Value::Null));
        self
    }
}

/// **What a [`Move`] may write into a column** — text and integers, which is every column a move
/// writes.
pub trait IntoColumn {
    fn into_column(self) -> Value;
}
impl IntoColumn for &str {
    fn into_column(self) -> Value {
        Value::Text(self.to_string())
    }
}
impl IntoColumn for String {
    fn into_column(self) -> Value {
        Value::Text(self)
    }
}
impl IntoColumn for &String {
    fn into_column(self) -> Value {
        Value::Text(self.clone())
    }
}
impl IntoColumn for u64 {
    fn into_column(self) -> Value {
        Value::Integer(self as i64)
    }
}
impl IntoColumn for u32 {
    fn into_column(self) -> Value {
        Value::Integer(self as i64)
    }
}
impl IntoColumn for i64 {
    fn into_column(self) -> Value {
        Value::Integer(self)
    }
}

/// A new review row's fields — the enqueuer's half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReview {
    pub id: String,
    pub repository: String,
    pub target: String,
    pub kind: ReviewKind,
    pub session_id: String,
    pub branch: String,
    pub worktree: Option<String>,
    pub base_sha: String,
    pub head_sha: String,
    pub brief: String,
    pub priority: MergePriority,
    pub fixes: Option<String>,
    pub evidence: String,
}

/// The verdict that approves a review, as [`Store::approve_review`] writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    /// The state the row is expected to be in (`reviewing` for a reviewer's verdict; whatever a
    /// person saw for an approve-over).
    pub expect: ReviewState,
    pub expect_owner: Option<String>,
    /// The head the verdict judged — becomes `approved_sha` on the merge row.
    pub reviewed_sha: String,
    /// `reviewer:<handle>` or `operator:<session>`.
    pub by: String,
    pub reasons: Vec<String>,
    pub files: Vec<String>,
    pub commands: Vec<String>,
    pub why: String,
    /// The merge row's id.
    pub merge_id: String,
}

const REVIEW_COLUMNS: &str = "id, repository, target, kind, session_id, branch, worktree, \
     base_sha, head_sha, brief, priority, fixes, state, owner, reviewer, deadline_ms, attempts, \
     failure, verdict, verdict_by, reviewed_sha, reasons_json, files_json, commands_json, \
     evidence, created_ms, state_ms, updated_ms";

const MERGE_COLUMNS: &str = "id, review_id, repository, target, branch, worktree, approved_sha, \
     approved_by, priority, fixes, state, owner, onto_sha, rebased_sha, landed_sha, pushed, \
     gate_steps_json, evidence, created_ms, state_ms, updated_ms";

/// The columns a [`Move`] may write on `review_queue` — the guard that keeps a typo from
/// becoming SQL.
const REVIEW_SETTABLE: &[&str] = &[
    "repository",
    "target",
    "head_sha",
    "owner",
    "reviewer",
    "deadline_ms",
    "attempts",
    "failure",
    "verdict",
    "verdict_by",
    "reviewed_sha",
    "reasons_json",
    "files_json",
    "commands_json",
    "fixes",
    "priority",
];

const MERGE_SETTABLE: &[&str] = &[
    "owner",
    "onto_sha",
    "rebased_sha",
    "landed_sha",
    "pushed",
    "gate_steps_json",
    "approved_sha",
];

fn ms(v: i64) -> u64 {
    v.max(0) as u64
}

fn corrupt(table: &str, column: &str, word: &str) -> StoreError {
    StoreError::Corrupt(format!(
        "{table}.{column} {word:?} is not in this build's closed set — a row written by a build \
         this one does not know"
    ))
}

fn review_from_row(r: &rusqlite::Row) -> rusqlite::Result<Result<ReviewRow>> {
    let kind: String = r.get(3)?;
    let priority: String = r.get(10)?;
    let state: String = r.get(12)?;
    let reasons: String = r.get(21)?;
    let files: String = r.get(22)?;
    let commands: String = r.get(23)?;
    let deadline: Option<i64> = r.get(15)?;
    let attempts: i64 = r.get(16)?;
    let row = (|| -> Result<ReviewRow> {
        Ok(ReviewRow {
            id: r.get(0)?,
            repository: r.get(1)?,
            target: r.get(2)?,
            kind: ReviewKind::parse(&kind).ok_or_else(|| corrupt("review_queue", "kind", &kind))?,
            session_id: r.get(4)?,
            branch: r.get(5)?,
            worktree: r.get(6)?,
            base_sha: r.get(7)?,
            head_sha: r.get(8)?,
            brief: r.get(9)?,
            priority: MergePriority::parse(&priority)
                .ok_or_else(|| corrupt("review_queue", "priority", &priority))?,
            fixes: r.get(11)?,
            state: ReviewState::parse(&state)
                .ok_or_else(|| corrupt("review_queue", "state", &state))?,
            owner: r.get(13)?,
            reviewer: r.get(14)?,
            deadline_ms: deadline.map(ms),
            attempts: attempts.max(0) as u32,
            failure: r.get(17)?,
            verdict: r.get(18)?,
            verdict_by: r.get(19)?,
            reviewed_sha: r.get(20)?,
            reasons: serde_json::from_str(&reasons)?,
            files: serde_json::from_str(&files)?,
            commands: serde_json::from_str(&commands)?,
            evidence: r.get(24)?,
            created_ms: ms(r.get(25)?),
            state_ms: ms(r.get(26)?),
            updated_ms: ms(r.get(27)?),
        })
    })();
    Ok(row)
}

fn merge_from_row(r: &rusqlite::Row) -> rusqlite::Result<Result<MergeRow>> {
    let priority: String = r.get(8)?;
    let state: String = r.get(10)?;
    let steps: String = r.get(16)?;
    let row = (|| -> Result<MergeRow> {
        Ok(MergeRow {
            id: r.get(0)?,
            review_id: r.get(1)?,
            repository: r.get(2)?,
            target: r.get(3)?,
            branch: r.get(4)?,
            worktree: r.get(5)?,
            approved_sha: r.get(6)?,
            approved_by: r.get(7)?,
            priority: MergePriority::parse(&priority)
                .ok_or_else(|| corrupt("merge_queue", "priority", &priority))?,
            fixes: r.get(9)?,
            state: MergeState::parse(&state)
                .ok_or_else(|| corrupt("merge_queue", "state", &state))?,
            owner: r.get(11)?,
            onto_sha: r.get(12)?,
            rebased_sha: r.get(13)?,
            landed_sha: r.get(14)?,
            pushed: r.get(15)?,
            gate_steps: serde_json::from_str(&steps)?,
            evidence: r.get(17)?,
            created_ms: ms(r.get(18)?),
            state_ms: ms(r.get(19)?),
            updated_ms: ms(r.get(20)?),
        })
    })();
    Ok(row)
}

fn lease_from_row(r: &rusqlite::Row) -> rusqlite::Result<QueueLease> {
    Ok(QueueLease {
        repository: r.get(0)?,
        owner: r.get(1)?,
        pid: r.get::<_, i64>(2)?.max(0) as u32,
        host: r.get(3)?,
        build: r.get(4)?,
        schema: r.get(5)?,
        heartbeat_ms: ms(r.get(6)?),
        acquired_ms: ms(r.get(7)?),
    })
}

const LEASE_COLUMNS: &str =
    "repository, owner, pid, host, build, schema, heartbeat_ms, acquired_ms";

/// Append one transition. Called inside the transaction of the move it records.
#[allow(clippy::too_many_arguments)]
fn record(
    conn: &rusqlite::Connection,
    queue: &str,
    entry_id: &str,
    repository: &str,
    from: Option<&str>,
    to: &str,
    at_ms: u64,
    by: &str,
    why: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO queue_transition (queue, entry_id, repository, from_state, to_state, at_ms, by, why)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![queue, entry_id, repository, from, to, at_ms as i64, by, why],
    )?;
    Ok(())
}

impl Store {
    /// **The schema the FILE says it is, read now** — not the one this connection opened at.
    /// The lease claim reads it in its own transaction (requirement 1).
    pub fn stored_schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))?)
    }

    /// **`BEGIN IMMEDIATE`, taken again while the store is busy.** Only the BEGIN is retried,
    /// and that is enough: once it returns, this connection holds the write lock, and nothing
    /// the transaction does after it can come back busy. A BEGIN that failed began nothing.
    /// See `store/busy.rs` for the night this was for: `! merge_queue_store_error — the queues
    /// could not read or write their store while serving the queues: sqlite: database is
    /// locked`.
    fn immediate(&self) -> Result<rusqlite::Transaction<'_>> {
        Ok(super::busy::retry_sql(|| {
            rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)
        })?)
    }

    // ===== The review queue =====

    /// **Put a branch up for review.** `false` when a row with this id is already there (the
    /// same child enqueued twice is one row), and nothing is written then.
    pub fn enqueue_review(&self, new: &NewReview, by: &str, now_ms: u64) -> Result<bool> {
        let tx = self.immediate()?;
        let n = tx.execute(
            "INSERT OR IGNORE INTO review_queue
               (id, repository, target, kind, session_id, branch, worktree, base_sha, head_sha,
                brief, priority, fixes, state, owner, evidence, created_ms, state_ms, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'waiting', '', ?13, ?14, ?14, ?14)",
            params![
                new.id,
                new.repository,
                new.target,
                new.kind.as_str(),
                new.session_id,
                new.branch,
                new.worktree,
                new.base_sha,
                new.head_sha,
                new.brief,
                new.priority.as_str(),
                new.fixes,
                new.evidence,
                now_ms as i64,
            ],
        )?;
        if n == 1 {
            record(
                &tx,
                "review",
                &new.id,
                &new.repository,
                None,
                ReviewState::Waiting.as_str(),
                now_ms,
                by,
                &new.evidence,
            )?;
        }
        tx.commit()?;
        Ok(n == 1)
    }

    /// Every review row, every state — or one repository's. A row this build cannot read is an
    /// error naming the word, never a shorter list.
    pub fn review_rows(&self, repository: Option<&str>) -> Result<Vec<ReviewRow>> {
        let sql = match repository {
            Some(_) => format!(
                "SELECT {REVIEW_COLUMNS} FROM review_queue WHERE repository = ?1 ORDER BY created_ms, id"
            ),
            None => format!("SELECT {REVIEW_COLUMNS} FROM review_queue ORDER BY created_ms, id"),
        };
        let mut st = self.conn.prepare(&sql)?;
        let rows: Vec<Result<ReviewRow>> = match repository {
            Some(r) => st
                .query_map(params![r], review_from_row)?
                .collect::<rusqlite::Result<_>>()?,
            None => st
                .query_map([], review_from_row)?
                .collect::<rusqlite::Result<_>>()?,
        };
        rows.into_iter().collect()
    }

    pub fn review_row(&self, id: &str) -> Result<Option<ReviewRow>> {
        let sql = format!("SELECT {REVIEW_COLUMNS} FROM review_queue WHERE id = ?1");
        let row = self
            .conn
            .query_row(&sql, params![id], review_from_row)
            .optional()?;
        row.transpose()
    }

    /// **Move a review row from `from` to `to`, only if it is still in `from`** (and owned by
    /// `mv.expect_owner` when one is named), appending the transition in the same transaction.
    /// `Ok(false)` is *somebody else moved it first* — a fact, not an error.
    pub fn move_review(
        &self,
        id: &str,
        from: ReviewState,
        to: ReviewState,
        mv: &Move,
        now_ms: u64,
    ) -> Result<bool> {
        self.cas(
            "review_queue",
            "review",
            REVIEW_SETTABLE,
            id,
            from.as_str(),
            to.as_str(),
            mv,
            now_ms,
        )
    }

    /// **The verdict that approves, and the hand-off, in ONE transaction** (decision 6): the
    /// review row moves to `approved` with `reviewed_sha`, and the merge row is born carrying
    /// that sha as `approved_sha`. Either both happen or neither does — a verdict with no merge
    /// row is an approval nobody acts on, and a merge row with no verdict is a landing nobody
    /// approved.
    pub fn approve_review(&self, id: &str, a: &Approval, now_ms: u64) -> Result<bool> {
        let tx = self.immediate()?;
        let Some(row) = tx
            .query_row(
                &format!("SELECT {REVIEW_COLUMNS} FROM review_queue WHERE id = ?1"),
                params![id],
                review_from_row,
            )
            .optional()?
            .transpose()?
        else {
            return Ok(false);
        };
        if row.state != a.expect || a.expect_owner.as_deref().is_some_and(|o| o != row.owner) {
            return Ok(false);
        }
        let n = tx.execute(
            "UPDATE review_queue SET state = 'approved', verdict = 'accept', verdict_by = ?3,
                 reviewed_sha = ?4, reasons_json = ?5, files_json = ?6, commands_json = ?7,
                 owner = '', deadline_ms = NULL, failure = '', evidence = ?8,
                 state_ms = ?9, updated_ms = ?9
              WHERE id = ?1 AND state = ?2",
            params![
                id,
                a.expect.as_str(),
                a.by,
                a.reviewed_sha,
                serde_json::to_string(&a.reasons)?,
                serde_json::to_string(&a.files)?,
                serde_json::to_string(&a.commands)?,
                a.why,
                now_ms as i64,
            ],
        )?;
        if n != 1 {
            return Ok(false);
        }
        record(
            &tx,
            "review",
            id,
            &row.repository,
            Some(a.expect.as_str()),
            ReviewState::Approved.as_str(),
            now_ms,
            &a.by,
            &a.why,
        )?;
        let born = format!("approved at {} by {}", a.reviewed_sha, a.by);
        tx.execute(
            "INSERT INTO merge_queue
               (id, review_id, repository, target, branch, worktree, approved_sha, approved_by,
                priority, fixes, state, owner, evidence, created_ms, state_ms, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'queued', '', ?11, ?12, ?12, ?12)",
            params![
                a.merge_id,
                id,
                row.repository,
                row.target,
                row.branch,
                row.worktree,
                a.reviewed_sha,
                a.by,
                row.priority.as_str(),
                row.fixes,
                born,
                now_ms as i64,
            ],
        )?;
        record(
            &tx,
            "merge",
            &a.merge_id,
            &row.repository,
            None,
            MergeState::Queued.as_str(),
            now_ms,
            &a.by,
            &born,
        )?;
        tx.commit()?;
        Ok(true)
    }

    // ===== The merge queue =====

    pub fn merge_rows(&self, repository: Option<&str>) -> Result<Vec<MergeRow>> {
        let sql = match repository {
            Some(_) => format!(
                "SELECT {MERGE_COLUMNS} FROM merge_queue WHERE repository = ?1 ORDER BY created_ms, id"
            ),
            None => format!("SELECT {MERGE_COLUMNS} FROM merge_queue ORDER BY created_ms, id"),
        };
        let mut st = self.conn.prepare(&sql)?;
        let rows: Vec<Result<MergeRow>> = match repository {
            Some(r) => st
                .query_map(params![r], merge_from_row)?
                .collect::<rusqlite::Result<_>>()?,
            None => st
                .query_map([], merge_from_row)?
                .collect::<rusqlite::Result<_>>()?,
        };
        rows.into_iter().collect()
    }

    pub fn merge_row(&self, id: &str) -> Result<Option<MergeRow>> {
        let sql = format!("SELECT {MERGE_COLUMNS} FROM merge_queue WHERE id = ?1");
        let row = self
            .conn
            .query_row(&sql, params![id], merge_from_row)
            .optional()?;
        row.transpose()
    }

    /// [`Store::move_review`]'s twin for the merge queue.
    pub fn move_merge(
        &self,
        id: &str,
        from: MergeState,
        to: MergeState,
        mv: &Move,
        now_ms: u64,
    ) -> Result<bool> {
        self.cas(
            "merge_queue",
            "merge",
            MERGE_SETTABLE,
            id,
            from.as_str(),
            to.as_str(),
            mv,
            now_ms,
        )
    }

    /// **A branch that moved after its approval goes back to review** (decision 6): the merge
    /// row ends `moved` and its review row goes `approved → waiting` with the new head, in one
    /// transaction, so the branch is never in neither queue.
    pub fn send_back_to_review(
        &self,
        merge_id: &str,
        owner: &str,
        new_head: &str,
        by: &str,
        why: &str,
        now_ms: u64,
    ) -> Result<bool> {
        let tx = self.immediate()?;
        let Some(m) = tx
            .query_row(
                &format!("SELECT {MERGE_COLUMNS} FROM merge_queue WHERE id = ?1"),
                params![merge_id],
                merge_from_row,
            )
            .optional()?
            .transpose()?
        else {
            return Ok(false);
        };
        let n = tx.execute(
            "UPDATE merge_queue SET state = 'moved', owner = '', evidence = ?3, state_ms = ?4,
                 updated_ms = ?4
              WHERE id = ?1 AND state = 'gating' AND owner = ?2",
            params![merge_id, owner, why, now_ms as i64],
        )?;
        if n != 1 {
            return Ok(false);
        }
        record(
            &tx,
            "merge",
            merge_id,
            &m.repository,
            Some("gating"),
            "moved",
            now_ms,
            by,
            why,
        )?;
        let r = tx.execute(
            "UPDATE review_queue SET state = 'waiting', head_sha = ?2, verdict = NULL,
                 verdict_by = NULL, reviewed_sha = NULL, reviewer = NULL, attempts = 0,
                 failure = '', evidence = ?3, state_ms = ?4, updated_ms = ?4
              WHERE id = ?1 AND state = 'approved'",
            params![m.review_id, new_head, why, now_ms as i64],
        )?;
        if r == 1 {
            record(
                &tx,
                "review",
                &m.review_id,
                &m.repository,
                Some("approved"),
                "waiting",
                now_ms,
                by,
                why,
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    // ===== History =====

    /// One row's moves, oldest first.
    pub fn transitions(&self, queue: &str, entry_id: &str) -> Result<Vec<QueueTransition>> {
        let mut st = self.conn.prepare(
            "SELECT seq, queue, entry_id, repository, from_state, to_state, at_ms, by, why
               FROM queue_transition WHERE queue = ?1 AND entry_id = ?2 ORDER BY seq",
        )?;
        let rows = st
            .query_map(params![queue, entry_id], transition_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The last `limit` moves across every queue, newest LAST.
    pub fn recent_transitions(&self, limit: usize) -> Result<Vec<QueueTransition>> {
        let mut st = self.conn.prepare(
            "SELECT seq, queue, entry_id, repository, from_state, to_state, at_ms, by, why
               FROM queue_transition ORDER BY seq DESC LIMIT ?1",
        )?;
        let mut rows = st
            .query_map(params![limit as i64], transition_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// Record a move that is not a row's — the target's health, a lease changing hands.
    #[allow(clippy::too_many_arguments)]
    pub fn record_transition(
        &self,
        queue: &str,
        entry_id: &str,
        repository: &str,
        from: Option<&str>,
        to: &str,
        by: &str,
        why: &str,
        now_ms: u64,
    ) -> Result<()> {
        record(
            &self.conn, queue, entry_id, repository, from, to, now_ms, by, why,
        )
    }

    // ===== The lease =====

    /// **Claim, or renew, the lease on `repository`'s queues** — by compare-and-swap.
    ///
    /// In ONE immediate transaction: the file's schema is read and compared to this build's
    /// (a mismatch claims nothing — requirement 1's *"a daemon whose build does not match the
    /// store serves no queue"*); then the row is taken only if nobody holds it, the claimant
    /// already does, or the holder's heartbeat is older than `ttl_ms`. A live holder is never
    /// displaced.
    pub fn acquire_queue_lease(
        &self,
        repository: &str,
        me: &LeaseClaim,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<LeaseOutcome> {
        let tx = self.immediate()?;
        let stored: i64 = tx.query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;
        if stored != SCHEMA_VERSION {
            return Ok(LeaseOutcome::WrongSchema {
                store: stored,
                mine: SCHEMA_VERSION,
            });
        }
        let held: Option<QueueLease> = tx
            .query_row(
                &format!("SELECT {LEASE_COLUMNS} FROM queue_lease WHERE repository = ?1"),
                params![repository],
                lease_from_row,
            )
            .optional()?;
        let previous = match held {
            Some(l) if l.owner == me.owner => None,
            Some(l) if now_ms.saturating_sub(l.heartbeat_ms) <= ttl_ms => {
                return Ok(LeaseOutcome::HeldBy(l));
            }
            other => other,
        };
        let renewing = previous.is_none()
            && tx
                .query_row(
                    "SELECT 1 FROM queue_lease WHERE repository = ?1 AND owner = ?2",
                    params![repository, me.owner],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
        if renewing {
            tx.execute(
                "UPDATE queue_lease SET heartbeat_ms = ?3 WHERE repository = ?1 AND owner = ?2",
                params![repository, me.owner, now_ms as i64],
            )?;
        } else {
            tx.execute(
                "INSERT INTO queue_lease (repository, owner, pid, host, build, schema, heartbeat_ms, acquired_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT(repository) DO UPDATE SET owner = excluded.owner, pid = excluded.pid,
                     host = excluded.host, build = excluded.build, schema = excluded.schema,
                     heartbeat_ms = excluded.heartbeat_ms, acquired_ms = excluded.acquired_ms",
                params![
                    repository,
                    me.owner,
                    me.pid as i64,
                    me.host,
                    me.build,
                    SCHEMA_VERSION,
                    now_ms as i64
                ],
            )?;
            let why = match &previous {
                Some(p) => format!(
                    "took the lease over from {} (pid {}, {}), whose heartbeat was {} s old",
                    p.owner,
                    p.pid,
                    p.build,
                    now_ms.saturating_sub(p.heartbeat_ms) / 1000
                ),
                None => "took the free lease".to_string(),
            };
            record(
                &tx,
                "lease",
                repository,
                repository,
                previous.as_ref().map(|p| p.owner.as_str()),
                &me.owner,
                now_ms,
                &me.owner,
                &why,
            )?;
        }
        tx.commit()?;
        Ok(LeaseOutcome::Held { previous })
    }

    /// **Say this owner is alive.** `Ok(false)` is *the lease is not yours any more* — the
    /// caller stops serving at once. The schema is re-read too, so a store migrated under a
    /// running daemon stops it at its next heartbeat.
    pub fn heartbeat_queue_lease(
        &self,
        repository: &str,
        owner: &str,
        now_ms: u64,
    ) -> Result<bool> {
        let stored = self.stored_schema_version()?;
        if stored != SCHEMA_VERSION {
            return Ok(false);
        }
        let n = self.exec(
            "UPDATE queue_lease SET heartbeat_ms = ?3 WHERE repository = ?1 AND owner = ?2",
            params![repository, owner, now_ms as i64],
        )?;
        Ok(n == 1)
    }

    /// Give the lease up — on shutdown, so a successor need not wait out the heartbeat.
    pub fn release_queue_lease(&self, repository: &str, owner: &str, now_ms: u64) -> Result<bool> {
        let tx = self.immediate()?;
        let n = tx.execute(
            "DELETE FROM queue_lease WHERE repository = ?1 AND owner = ?2",
            params![repository, owner],
        )?;
        if n == 1 {
            record(
                &tx,
                "lease",
                repository,
                repository,
                Some(owner),
                "released",
                now_ms,
                owner,
                "the daemon serving it stopped and gave the lease up",
            )?;
        }
        tx.commit()?;
        Ok(n == 1)
    }

    pub fn queue_leases(&self) -> Result<Vec<QueueLease>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {LEASE_COLUMNS} FROM queue_lease ORDER BY repository"
        ))?;
        let rows = st
            .query_map([], lease_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// **Recovery, and only the recovery a lease entitles** (requirement 1): rows of
    /// `repository` that an owner OTHER than `me` held mid-work — a `gating` merge row, a
    /// `reviewing` review row. Called right after `me` took the lease, so every such owner's
    /// lease is expired or released by construction; rows of other repositories are never read.
    /// Returns how many rows moved.
    pub fn recover_queue(&self, repository: &str, me: &str, now_ms: u64) -> Result<usize> {
        let mut moved = 0;
        for m in self.merge_rows(Some(repository))? {
            if m.state == MergeState::Gating && m.owner != me {
                let why = format!(
                    "the daemon that was gating it ({}) is gone — its lease expired or was \
                     released — so it goes back in line and is rebased and gated again",
                    m.owner
                );
                if self.move_merge(
                    &m.id,
                    MergeState::Gating,
                    MergeState::Queued,
                    &Move::new(me, why)
                        .owned_by(m.owner.clone())
                        .set("owner", ""),
                    now_ms,
                )? {
                    moved += 1;
                }
            }
        }
        for r in self.review_rows(Some(repository))? {
            if r.state == ReviewState::Reviewing && r.owner != me {
                let why = format!(
                    "the daemon whose reviewer was on it ({}) is gone, so it waits for a reviewer \
                     again",
                    r.owner
                );
                if self.move_review(
                    &r.id,
                    ReviewState::Reviewing,
                    ReviewState::Waiting,
                    &Move::new(me, why)
                        .owned_by(r.owner.clone())
                        .set("owner", "")
                        .clear("reviewer")
                        .clear("deadline_ms"),
                    now_ms,
                )? {
                    moved += 1;
                }
            }
        }
        Ok(moved)
    }

    // ===== The target's health =====

    pub fn target_health(&self, repository: &str, target: &str) -> Result<Option<TargetHealth>> {
        let row = self
            .conn
            .query_row(
                "SELECT repository, target, sha, state, checked_ms, evidence, gate_steps_json
                   FROM target_health WHERE repository = ?1 AND target = ?2",
                params![repository, target],
                health_from_row,
            )
            .optional()?;
        row.transpose()
    }

    pub fn target_healths(&self) -> Result<Vec<TargetHealth>> {
        let mut st = self.conn.prepare(
            "SELECT repository, target, sha, state, checked_ms, evidence, gate_steps_json
               FROM target_health ORDER BY repository, target",
        )?;
        let rows: Vec<Result<TargetHealth>> = st
            .query_map([], health_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter().collect()
    }

    /// Write the target's health, and the move in the history when the state changed.
    pub fn set_target_health(&self, h: &TargetHealth, by: &str) -> Result<()> {
        let tx = self.immediate()?;
        let before: Option<String> = tx
            .query_row(
                "SELECT state FROM target_health WHERE repository = ?1 AND target = ?2",
                params![h.repository, h.target],
                |r| r.get(0),
            )
            .optional()?;
        tx.execute(
            "INSERT INTO target_health (repository, target, sha, state, checked_ms, evidence, gate_steps_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(repository, target) DO UPDATE SET sha = excluded.sha, state = excluded.state,
                 checked_ms = excluded.checked_ms, evidence = excluded.evidence,
                 gate_steps_json = excluded.gate_steps_json",
            params![
                h.repository,
                h.target,
                h.sha,
                h.state.as_str(),
                h.checked_ms as i64,
                h.evidence,
                serde_json::to_string(&h.gate_steps)?,
            ],
        )?;
        if before.as_deref() != Some(h.state.as_str()) {
            record(
                &tx,
                "target",
                &h.target,
                &h.repository,
                before.as_deref(),
                h.state.as_str(),
                h.checked_ms,
                by,
                &format!("{} at {}", h.evidence, h.sha),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// **The v23 queue as it was archived** — `(id, branch, state, evidence)` per row, for the
    /// pane and for a person deciding about rows the operator has not ruled on yet.
    pub fn archived_v23_queue(&self) -> Result<Vec<(String, String, String, String)>> {
        let there: bool = self
            .conn
            .prepare(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'merge_queue_v23'",
            )?
            .exists([])?;
        if !there {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare(
            "SELECT id, branch, state, evidence FROM merge_queue_v23 ORDER BY created_ms, id",
        )?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The conditional move both queues share. The guard is the WHERE; the history row is
    /// written only when the guard matched, in the same transaction.
    #[allow(clippy::too_many_arguments)]
    fn cas(
        &self,
        table: &str,
        queue: &str,
        settable: &[&str],
        id: &str,
        from: &str,
        to: &str,
        mv: &Move,
        now_ms: u64,
    ) -> Result<bool> {
        for (col, _) in &mv.sets {
            if !settable.contains(col) {
                return Err(StoreError::Refused(format!(
                    "`{col}` is not a column a move may write on {table}"
                )));
            }
        }
        let tx = self.immediate()?;
        let repository: Option<String> = tx
            .query_row(
                &format!("SELECT repository FROM {table} WHERE id = ?1"),
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(repository) = repository else {
            return Ok(false);
        };
        let mut sql =
            format!("UPDATE {table} SET state = ?1, evidence = ?2, state_ms = ?3, updated_ms = ?3");
        let mut values: Vec<Value> = vec![
            Value::Text(to.to_string()),
            Value::Text(mv.why.clone()),
            Value::Integer(now_ms as i64),
        ];
        for (col, v) in &mv.sets {
            values.push(v.clone());
            sql.push_str(&format!(", {col} = ?{}", values.len()));
        }
        values.push(Value::Text(id.to_string()));
        sql.push_str(&format!(" WHERE id = ?{}", values.len()));
        values.push(Value::Text(from.to_string()));
        sql.push_str(&format!(" AND state = ?{}", values.len()));
        if let Some(owner) = &mv.expect_owner {
            values.push(Value::Text(owner.clone()));
            sql.push_str(&format!(" AND owner = ?{}", values.len()));
        }
        let n = tx.execute(&sql, rusqlite::params_from_iter(values.iter()))?;
        if n != 1 {
            return Ok(false);
        }
        // `repository` may itself be one of the sets (a carried-over row being resolved); the
        // history names the repository the row belongs to after the move.
        let repository = mv
            .sets
            .iter()
            .find(|(c, _)| *c == "repository")
            .and_then(|(_, v)| match v {
                Value::Text(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap_or(repository);
        record(
            &tx,
            queue,
            id,
            &repository,
            Some(from),
            to,
            now_ms,
            &mv.by,
            &mv.why,
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// **The v24 migration's queue half** — archive the v23 tables by renaming them, create the
    /// new ones, and carry forward the rows decision 13 says go on.
    ///
    /// Decision 13, as a rule rather than a list of names (the store cannot ask git anything):
    ///
    /// * **Every v23 row is kept**, in `merge_queue_v23`/`merge_review_v23`, untouched.
    /// * `landed`, `failed`, `conflict` and `vetoed` rows stay archived and nothing else — the 9
    ///   landed are archived, and the 10 failed are archived (verified 2026-10-11: none of them
    ///   holds work missing anywhere).
    /// * **A `waiting`, `taken` or `stale` row is carried forward** as a review row in
    ///   `waiting` — `a-pass-reads-one-row` re-enqueued; `group-the-queue-messages` (stale)
    ///   carried forward, and the review queue's first pass checks whether its branch is already
    ///   on the target and withdraws it if so (the *"checked and re-enqueued if unmerged"*).
    /// * **Except a waiting row whose review said `reject`** — `queue-spin`, dropped: its work
    ///   reached main by hand. It stays in the archive.
    ///
    /// A carried row's `repository`, `target` and `head_sha` are `''`: they are git facts, and a
    /// daemon resolves them (`mergequeue::review::resolve_carried`) before anything serves the
    /// row.
    pub(super) fn migrate_queues_to_v24(&self) -> Result<()> {
        let exists = |name: &str| -> Result<bool> {
            Ok(self
                .conn
                .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
                .exists(params![name])?)
        };
        // An old `merge_queue` is recognised by its own column, not by its name: a store already
        // at 24 has a `merge_queue` too.
        let old_merge = exists("merge_queue")?
            && self
                .conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('merge_queue') WHERE name = 'needs_json'",
                )?
                .exists([])?;
        if old_merge && !exists("merge_queue_v23")? {
            self.conn
                .execute_batch("ALTER TABLE merge_queue RENAME TO merge_queue_v23")?;
        }
        if exists("merge_review")? && !exists("merge_review_v23")? {
            self.conn
                .execute_batch("ALTER TABLE merge_review RENAME TO merge_review_v23")?;
        }
        self.conn.execute_batch(QUEUES_SQL)?;
        if !exists("merge_queue_v23")? {
            return Ok(());
        }
        let has_reviews = exists("merge_review_v23")?;
        let now = super::now_ms() as u64;
        let carried: Vec<CarriedRow> = {
            let sql = if has_reviews {
                "SELECT q.id, q.session_id, q.branch, q.base_sha, q.priority, q.worktree, q.brief,
                        q.state, q.evidence, COALESCE(r.decision, '')
                   FROM merge_queue_v23 q LEFT JOIN merge_review_v23 r ON r.entry_id = q.id
                  WHERE q.state IN ('waiting', 'taken', 'stale')"
            } else {
                "SELECT id, session_id, branch, base_sha, priority, worktree, brief, state, evidence, ''
                   FROM merge_queue_v23 WHERE state IN ('waiting', 'taken', 'stale')"
            };
            let mut st = self.conn.prepare(sql)?;
            st.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, session, branch, base, priority, worktree, brief, state, evidence, decision) in
            carried
        {
            if decision == "reject" {
                continue;
            }
            let priority = MergePriority::parse(&priority).unwrap_or(MergePriority::Subagent);
            let why = format!(
                "carried forward from the v23 queue (schema 24), where it was `{state}`: {}",
                if evidence.is_empty() {
                    "no reason on the row"
                } else {
                    evidence.as_str()
                }
            );
            self.enqueue_review(
                &NewReview {
                    id,
                    repository: String::new(),
                    target: String::new(),
                    kind: ReviewKind::Branch,
                    session_id: session,
                    branch,
                    worktree,
                    base_sha: base,
                    head_sha: String::new(),
                    brief,
                    priority,
                    fixes: None,
                    evidence: why,
                },
                "migration v24",
                now,
            )?;
        }
        Ok(())
    }
}

/// A v23 row the v24 migration carries forward: id, session, branch, base, priority, worktree,
/// brief, state, evidence, and its review's decision (`''` when it had none).
type CarriedRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
);

fn transition_from_row(r: &rusqlite::Row) -> rusqlite::Result<QueueTransition> {
    Ok(QueueTransition {
        seq: r.get(0)?,
        queue: r.get(1)?,
        entry_id: r.get(2)?,
        repository: r.get(3)?,
        from_state: r.get(4)?,
        to_state: r.get(5)?,
        at_ms: ms(r.get(6)?),
        by: r.get(7)?,
        why: r.get(8)?,
    })
}

fn health_from_row(r: &rusqlite::Row) -> rusqlite::Result<Result<TargetHealth>> {
    let state: String = r.get(3)?;
    let steps: String = r.get(6)?;
    let row = (|| -> Result<TargetHealth> {
        Ok(TargetHealth {
            repository: r.get(0)?,
            target: r.get(1)?,
            sha: r.get(2)?,
            state: HealthState::parse(&state)
                .ok_or_else(|| corrupt("target_health", "state", &state))?,
            checked_ms: ms(r.get(4)?),
            evidence: r.get(5)?,
            gate_steps: serde_json::from_str(&steps)?,
        })
    })();
    Ok(row)
}

#[cfg(test)]
mod tests;
