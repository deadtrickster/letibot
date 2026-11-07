//! The SQLite store for the transcript and its ledger rows. Tokens are stored as ids, per item,
//! so a restart replays spans and an append is one insert. Triggers enforce append-only. The
//! prefix is addressed on the dialect too. WAL with `synchronous = FULL`: this is the only copy.

use std::collections::BTreeMap;
use std::path::Path;

/// A session's context row as [`Store::set_context`] takes it: `(session id, tokens, cached, ledger)`.
pub type ContextRow<'a> = (&'a str, Option<u64>, Option<u64>, Option<u64>);

use letibot_transcript::TranscriptItem;
use rusqlite::{Connection, OptionalExtension, Params, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::ledger::LedgerRow;
use crate::vocab::TokenId;

mod busy;
mod floor;
pub use floor::{Arm, FIRST_SCHEMA, NewerStore, READER_ARMS, READER_FLOOR_SQL, reader_floor_for};
pub mod live_view;
mod transfer;

/// Who may append to a session's transcript (schema 32) - the row and its compare-and-swap.
/// See [`writers`] for the daemon-shared store this exists in and the two writers it stops.
pub mod writers;
pub use transfer::{ImportInto, ImportReport, SessionTransfer, TRANSFER_FORMAT, TableExport};
mod queues;
pub use busy::{BUSY_RETRY_TOTAL, is_busy_sql};
pub use queues::*;
mod modes;
pub use modes::ProjectMode;
mod subagents;
pub use subagents::*;
mod citations;
mod leases;
mod marks;
mod procs;
mod prompts;
mod seat;
mod told;
pub use leases::{Ask, FIRST_PORT, LEASES_SQL, Lease};
pub use procs::{Check, PROCS_SQL, ProcState, ProcessRecord};
pub use prompts::{PROMPTS_SQL, QueuedPrompt};
pub use told::Told;

/// Why `session.role` is nullable: a role belongs to the SESSION and not to the process, and
/// `NULL` is *unrecorded* rather than `coder`, so a resume does not re-seat an old
/// conversation. The prose is here because a multi-line SQL comment breaks `ALTER TABLE`.
pub const ROLE_COLUMN: () = ();

/// One corpus row as stored. `trail_json` stays serialised: a reader that wants
/// the structure deserialises it, and one that wants to write a training file
/// does not pay for a parse it will not use.
#[derive(Debug, Clone)]
pub struct StoredAdjudication {
    pub request_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub decided_ms: i64,
    pub action: String,
    pub baseline: String,
    pub tier: String,
    pub trail_json: String,
    pub shown: Option<String>,
    pub tool: String,
    pub arguments_json: String,
    pub mode: String,
    pub options_json: String,
    pub agent: String,
    pub model_verdict: Option<String>,
    pub verdict: Option<String>,
    pub verdict_by: Option<String>,
    pub verdict_basis: Option<String>,
    pub p_allow: Option<f64>,
    pub oracle_ms: Option<i64>,
    pub oracle_model: Option<String>,
    pub brief_sha: Option<String>,
    /// Whether an oracle was consulted. `None` on rows written before the
    /// column existed - "not recorded" - which is a different statement from
    /// `Some(false)`, "recorded, and nobody asked a model".
    pub consulted: Option<bool>,
    /// Which of the four `Unsure`s the answer was, verbatim as stored. `None` means an answered
    /// verdict or a row from before the column. Read with `UnsureKind::parse`, which refuses a
    /// token this build does not know rather than guessing.
    pub oracle_reading: Option<String>,
    /// What the oracle answered, verbatim. `None` means either that
    /// no oracle was consulted - `verdict_by` says which - or that one was asked and
    /// nothing came back inside its budget.
    pub reply: Option<String>,
    pub effect: String,
    pub asked: bool,
    pub operator_kind: Option<String>,
    pub operator_note: Option<String>,
    pub operator_latency_ms: Option<i64>,
    pub corpus_version: i64,
}

/// The three predicates every count of the corpus is built from, in one place: a `WHERE` clause
/// copied into a second file is a count that drifts, and these did. A count measures what its
/// PREDICATE says and not what its name says, so the predicate is written once, by the schema.
pub const ASKED_SQL: &str = "asked = 1";
/// An oracle actually spoke: not "a model adjudicator was in the chain" and not "a model
/// decided", which are different facts and were both counted as this one. Written from
/// `ModelAdvice::consulted`, which is the flag the distinction exists for.
pub const MEASURED_SQL: &str = "consulted = 1";
/// The rows a fine-tune is for: the operator ruled *against* what the model would
/// have done. `granted` and `revoked` only - `upheld` is the operator agreeing, and
/// counting it here made the number mean "ruled on" while wearing the name of "differ".
pub const DISAGREEMENT_SQL: &str = "operator_kind IN ('granted', 'revoked')";
/// A model decided the call itself, which is neither of the two above and had no
/// name before this: `automode` lets the oracle's own verdict admit, and at `/supervised`
/// the oracle only advises while a person decides.
pub const MODEL_DECIDED_SQL: &str = "verdict_by LIKE 'model%'";

/// What the corpus holds, for the disclosure and `/gate corpus`. The split that matters is
/// who DECIDED against what it was MEASURED against, and they are independent: a row can be
/// a person's own decision, an oracle's verdict on it, and a disagreement between the two.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CorpusCounts {
    pub total: u64,
    /// `asked = 1`: a person was put in front of this call and answered it.
    pub decided_by_operator: u64,
    /// Whether an oracle was actually consulted, from `ModelAdvice::consulted`. `NULL` before
    /// the column existed, so those rows read `0` and `model_decided` is the count that
    /// speaks for them.
    pub measured: u64,
    /// The operator ruled against what the model would have done - [`DISAGREEMENT_SQL`].
    pub disagreements: u64,
    /// A model decided the call - [`MODEL_DECIDED_SQL`]. The one count that is exact on
    /// rows written before v10.
    pub model_decided: u64,
}

/// The write shape for one decision: a struct rather than twenty positional arguments, since
/// two adjacent `Option<String>`s passed positionally is a defect waiting for the next
/// field. Separate from [`StoredAdjudication`], whose read shape carries what it became.
#[derive(Debug, Clone, Default)]
pub struct NewAdjudication {
    pub request_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub action: String,
    pub baseline: String,
    pub tier: String,
    pub trail_json: String,
    pub shown: Option<String>,
    pub tool: String,
    pub arguments_json: String,
    pub mode: String,
    pub options_json: String,
    pub agent: String,
    pub model_verdict: Option<String>,
    pub verdict: Option<String>,
    pub verdict_by: Option<String>,
    pub verdict_basis: Option<String>,
    pub p_allow: Option<f64>,
    pub oracle_ms: Option<i64>,
    pub oracle_model: Option<String>,
    pub brief_sha: Option<String>,
    /// Whether an oracle was actually consulted, from `ModelAdvice::consulted`: the flag that
    /// separates *the oracle said ask* from *nobody asked a model*.
    pub consulted: Option<bool>,
    /// Which of the four `Unsure`s the answer was, or `None` for an answered
    /// verdict. `CorpusRow::oracle_reading`, which is `UnsureKind::as_str`.
    pub oracle_reading: Option<String>,
    /// What the oracle answered, verbatim. `None` when none was consulted,
    /// or when the one that was did not answer inside its budget - `verdict_by`
    /// tells those apart, and these are the bytes the verdict was read out of.
    pub reply: Option<String>,
    pub effect: String,
    /// Whether the operator was actually put in front of this decision.
    pub asked: bool,
    /// The command's shape - the parse with its literals holed. `None` for a
    /// call that is not a command. See `letibot_code::shell::shape`.
    pub shape: Option<String>,
    /// The effect class the decision was taken at, as `ActionClass`'s Display.
    /// Paired with `shape`: the warm start needs both or it has neither.
    pub shape_class: Option<String>,
}

/// One row the backfill may be able to fill in. See [`Store::shapeless_human_admits`].
#[derive(Debug, Clone)]
pub struct ShapelessAdmit {
    pub request_id: String,
    /// The workspace of the session this decision was taken in, from the `session`
    /// row - so a recomputed shape is filed under the project it was approved in.
    pub workspace_root: String,
    pub tool: String,
    pub arguments_json: String,
}

/// The schema version: what the FILE holds. [`floor`] says what a reader must know, and the
/// two are separate on purpose: a migration that only adds leaves the floor alone, and one
/// that rewrites what an older reader reads declares itself in [`READER_ARMS`].
pub const SCHEMA_VERSION: i64 = 42;

/// What this row's columns mean. Stamped on every corpus row, because a column can keep its
/// name and its type while changing what it records, and a trainer that pools two meanings
/// under one name fits a model to the seam.
pub const CORPUS_VERSION: i64 = 1;

/// The session list's indexes: every per-session subquery ends in `transcript_by_session`, and a
/// parent's children are `session_by_parent`. Run beside `SCHEMA_SQL` on a fresh store and by the
/// v38 arm, not inside it, so the migration tests that rebuild an old shape can still drop columns.
pub const SESSION_INDEXES_SQL: &str = "
CREATE INDEX IF NOT EXISTS transcript_by_session ON transcript (session_id, created_at);
CREATE INDEX IF NOT EXISTS session_by_parent ON session (parent_session_id);
";

/// [`corpus_counts`], index-only: the four columns the five counts read, so every open's
/// disclosure walks an index of a few megabytes instead of the whole `adjudication` table.
/// Run beside `SCHEMA_SQL` on a fresh store and by the v42 arm on a migrated one, not inside it.
pub const CORPUS_INDEXES_SQL: &str = "
CREATE INDEX IF NOT EXISTS adjudication_corpus_counts
    ON adjudication (asked, consulted, verdict_by, operator_kind);
";

pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version     INTEGER NOT NULL
);

-- Content-addressed and shared between sessions: a new session with the same
-- system text, the same tools and the same dialect inherits a warm prefix for
-- free. (Measured in oracle: sharing one prefix took an identical request from
-- 9,325 tokens processed to 4.)
CREATE TABLE IF NOT EXISTS stable_prefix (
    id           TEXT PRIMARY KEY,   -- hex sha256 over (dialect_sha, system, tools)
    dialect_sha  TEXT NOT NULL,      -- the dialect's template_sha; part of the address
    system       TEXT NOT NULL,
    tools_json   TEXT NOT NULL,      -- JSON array of schema strings, in prefix order
    n_tokens     INTEGER NOT NULL,
    tokens       BLOB NOT NULL,      -- little-endian u32 ids
    h_init       BLOB NOT NULL,      -- 32 bytes: h_{-1} = H(tokens)
    vocab_source TEXT NOT NULL,      -- the GGUF these ids came out of
    created_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS session (
    id             TEXT PRIMARY KEY,
    title          TEXT,
    model_id       TEXT NOT NULL,
    dialect_sha    TEXT NOT NULL,    -- §4.4: the dialect id *is* the template_sha
    workspace_root TEXT NOT NULL,
    owner          TEXT NOT NULL,
    approvers_json TEXT NOT NULL,    -- JSON array of identities
    created_at     INTEGER NOT NULL,
    role           TEXT,             -- v2; see ROLE_COLUMN below. NULL = unrecorded
    parent_session_id TEXT,          -- v4; the session that spawned this one as a
                                     -- subagent. NULL = a top-level session
    context_tokens INTEGER,          -- v8; the last turn's prompt_tokens. NULL = no
                                     -- turn has finished. Survives a restart so an
                                     -- attaching head can show the context at once.
    context_cached INTEGER,          -- v8; the last turn's cached_tokens, for the
                                     -- cache %. NULL with context_tokens.
    provider_choice TEXT,            -- v13; the provider THIS session was switched to,
                                     -- and before v12's column only because the store's
                                     -- own fixtures walk a current store BACKWARDS, and
                                     -- this is the drop order sqlite accepts — the v7
                                     -- fixture drops provider_choice before context_ledger.
                                     -- `name` or `name/model`. NULL = never switched, so
                                     -- the daemon's own default. Survives a restart for
                                     -- the same reason context_tokens does: a session's
                                     -- facts belong to the session, not to the process
                                     -- that happens to be holding it.
    context_ledger INTEGER,          -- v12; the LEDGER tokens that context_tokens was
                                     -- measured against. A provider count on its own is
                                     -- not a ratio: the daemon used to recover
                                     -- ledger_scale by pairing it with whatever ledger
                                     -- it had TODAY, which for a resumed session is a
                                     -- bigger conversation and so a wrong ratio in the
                                     -- direction that compacts too late. NULL = no
                                     -- measurement / one that predates this column.
    context_window INTEGER,          -- v14; the window this session plans its compaction against.
    auto_compact_resident INTEGER,   -- v19; LEDGER tokens going into the compaction that
                                     -- made no room. NULL = the no-progress guard has
                                     -- not fired for this session.
    auto_compact_after INTEGER,      -- v19; LEDGER tokens coming out of it, still within
                                     -- a headroom of the window. NULL with its pair.
    prefix_fingerprint TEXT,         -- v21; the hash of the FROZEN prefix this session was
                                     -- seated with — the composed instructions and the
                                     -- tool schemas this seat hands a session, with the
                                     -- standing notes excluded, because those are re-read
                                     -- at every base rebuild. NULL = seated before this
                                     -- column existed, which the resume path falls back to
                                     -- the stored prefix row for. See
                                     -- `StoredSession::prefix_fingerprint`.
    nags_paused_until INTEGER,        -- v25; the operator paused this session's idle plan-check.
                                     -- NULL = not paused; 0 = until they resume it; any other
                                     -- value = unix ms the pause ends at. See
                                     -- `StoredSession::nags_paused_until`.
    origin         TEXT,             -- v33; WHO the session is for — `operator`, `package`,
                                     -- `queue`, `one_shot`. NULL = written before the column
                                     -- existed, which is read as the operator's own unless the
                                     -- parent edge or the queue's id convention says otherwise.
                                     -- Written once, at creation; see
                                     -- `Store::set_session_origin` and
                                     -- `letibot_sessionlog::SessionOrigin`.
    lives_on       TEXT,             -- v31; the endpoint holding the LIVE copy of this
                                     -- session, when it is not in this store's own box —
                                     -- `local` or a host from `hosts.toml`. NULL = the copy
                                     -- here is the live one. See `store::transfer`.
    moved_ms       INTEGER           -- v31; when it moved there. NULL with `lives_on`.
                                                                                                                                                                                                                                                                                                                                                                                  );

CREATE TABLE IF NOT EXISTS transcript (
    id                   TEXT PRIMARY KEY,
    session_id           TEXT NOT NULL REFERENCES session(id),
    parent_transcript_id TEXT REFERENCES transcript(id),  -- non-null after a fork
    forked_at_seq        INTEGER,                         -- items carried over
    stable_prefix_id     TEXT NOT NULL REFERENCES stable_prefix(id),
    created_at           INTEGER NOT NULL,
    CHECK ((parent_transcript_id IS NULL) = (forked_at_seq IS NULL))
);

CREATE TABLE IF NOT EXISTS transcript_item (
    transcript_id TEXT NOT NULL REFERENCES transcript(id),
    seq           INTEGER NOT NULL,   -- 0-based, dense; also the ledger row index
    item_id       TEXT NOT NULL,
    kind          TEXT NOT NULL,      -- the serde tag, denormalised for filtering
    item_json     TEXT NOT NULL,      -- the TranscriptItem itself
    tok_offset    INTEGER NOT NULL,   -- ledger row
    tok_len       INTEGER NOT NULL,   -- ledger row
    h_k           BLOB NOT NULL,      -- ledger row, 32 bytes
    tokens        BLOB NOT NULL,      -- little-endian u32 ids, tok_len of them
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (transcript_id, seq)
);

-- The session's todo list, as the model last wrote it. Mutable metadata about a
-- conversation, in the same class as the title: it rides no chain and carries no
-- append-only trigger, because a todo list the model revised three times is one
-- list with a history nobody asked to keep. One row per session, replaced whole.
CREATE TABLE IF NOT EXISTS todo (
    session_id  TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
    todos_json  TEXT NOT NULL,   -- JSON array of {content, status}
    updated_ms  INTEGER NOT NULL
);

-- **The job history.** The daemon's process table is memory and dies with it, so a handle
-- the pane listed a moment ago answered `no job ... here` to the next daemon — about a job
-- whose ending was written down. One row per handle per session, UPSERTED: a job has one
-- state at a time and the settlement is the same job as the start. The append-only record of
-- endings is the session's own log (`JobSettled`), which is a different question.
--
-- `state` is the listing's own word, and a `running` row here is **a job the daemon was
-- watching when it died** — not a claim that it runs now. The live process host answers that;
-- this row answers what happened.
CREATE TABLE IF NOT EXISTS job (
    session_id  TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    handle      TEXT NOT NULL,
    command     TEXT NOT NULL,
    how         TEXT NOT NULL,
    state       TEXT NOT NULL,
    produced    INTEGER NOT NULL,
    elapsed_ms  INTEGER NOT NULL,
    redirect    TEXT,
    updated_ms  INTEGER NOT NULL,
    delivered   INTEGER NOT NULL DEFAULT 0,  -- v28; 1 once its ending reached the model
    PRIMARY KEY (session_id, handle)
);

-- **The review queue, the merge queue, their history, the lease and the target's health**
-- (R61, schema 24) are `queues::QUEUES_SQL`, run beside this text on a fresh store and by the
-- v24 arm on a migrated one. The single v16-v23 `merge_queue`/`merge_review` pair is archived
-- under `_v23` names by that arm; a fresh store never had it.


-- **The adjudication corpus.** Every decision this harness makes, and every
-- decision the operator makes about it, as one row.
--
-- It exists because a fine-tune needs labelled disagreements and they can only
-- be collected as a side effect of working. `AdjudicationRow` was already built
-- for this -- it keeps `shown` verbatim rather than reconstructed, and keeps the
-- model's verdict and the operator's answer in SEPARATE columns so the
-- disagreement survives as the label -- but the rows lived in a Vec on the gate
-- and died with the process. Every decision made before this table existed is
-- gone.
--
-- NOT append-only by trigger, unlike transcript_item, and the reason is
-- `operator_kind`: the operator answers AFTER the gate acted, sometimes turns
-- later, so a row is written when the decision is made and updated once when the
-- human rules on it. That is the only mutation, and it only ever fills columns
-- that were NULL.
CREATE TABLE IF NOT EXISTS adjudication (
    request_id     TEXT PRIMARY KEY,
    session_id     TEXT NOT NULL,
    turn_id        TEXT NOT NULL,
    decided_ms     INTEGER NOT NULL,
    -- Layer A. `action` is the normalised summary plus stages, never raw command
    -- text, for the same reason the oracle is not shown it.
    action         TEXT NOT NULL,
    baseline       TEXT NOT NULL,
    tier           TEXT NOT NULL,
    -- The trail as rendered into the brief, and the exact bytes the oracle saw.
    -- `shown` is NULL when no oracle was consulted -- a human-only decision is
    -- still a corpus row, and "nobody asked a model" is a fact about it.
    --
    -- **R11: this column is the BRIEF, and until 2026-09-21 it never held one.**
    -- It was set on 147 of 5785 rows and NULL on every row an oracle decided,
    -- because the gate asked its decider for `last_brief()` and the decider was a
    -- person's card (`req.brief()`, whose first line is `decision — …`) while the
    -- adjudicator that had rendered the real brief kept it in a cell it never
    -- reported. Both halves are fixed in the same commit; rows written before it
    -- keep their cards, and are recognisable as such because they are exactly the
    -- rows a human decided.
    trail_json     TEXT NOT NULL,
    shown          TEXT,
    -- **R11: what came back, verbatim, before anything parses it.** NULL means no
    -- oracle answered -- either because none was consulted (`verdict_by`) or
    -- because the one that was did not reply inside its budget. The parsed reading
    -- is `verdict_basis`; this is the bytes it was read out of, and a corpus whose
    -- labels cannot be traced back to them is a corpus nobody can check.
    oracle_reply   TEXT,
    -- **The input, unnormalised.** `action` above is layer A's reading of it, and
    -- a corpus that kept only the reading can never be re-featurised when layer A
    -- changes -- which it will, because the whole point of collecting this is to
    -- change it. Stored as the gate received it.
    tool           TEXT NOT NULL DEFAULT '',
    arguments_json TEXT NOT NULL DEFAULT '{}',
    -- Where the gate was standing. A decision is not interpretable without it:
    -- the same call admits under allow-all and asks under always-ask, and a row
    -- that dropped this teaches the model to ignore the mode.
    mode           TEXT NOT NULL DEFAULT '',
    options_json   TEXT NOT NULL DEFAULT '[]',
    agent          TEXT NOT NULL DEFAULT '',
    -- Layer B, in parts. `model_verdict` is the formatted line the disclosure
    -- shows; the three columns beside it are what a trainer actually reads, and
    -- re-parsing prose to recover them is how a corpus rots.
    model_verdict  TEXT,
    verdict        TEXT,
    verdict_by     TEXT,
    verdict_basis  TEXT,
    -- Calibrated P(allow) where the oracle returned logprobs, NULL where it did
    -- not. A threshold cannot be fitted from hard labels alone, and "the model
    -- was 0.51 sure" and "the model was 0.99 sure" are the same ALLOW string.
    p_allow        REAL,
    oracle_ms      INTEGER,
    oracle_model   TEXT,
    shape          TEXT,
    -- The effect class the operator approved that shape AT, spelled as
    -- `ActionClass`'s Display. The shape alone is not an approval: `cat <arg>`
    -- inside the project and `cat <arg>` over `~/.ssh` are one shape and two
    -- different things, and the class is the half that tells them apart. Written
    -- only where `shape` is, and read back only by the warm start.
    shape_class    TEXT,
    -- Which brief format produced `shown`. A corpus spanning a prompt change is
    -- two datasets, and without this nobody can tell where the seam is.
    brief_sha      TEXT,
    -- **R11: whether an oracle was actually consulted.** `NULL` on rows written before
    -- schema v10 — "recorded before this was kept" — and `0`/`1` after. `ModelAdvice::consulted`
    -- is the flag; this is its durable form, and without it the only way to ask the
    -- question was a regex over `model_verdict` prose, which this file's own comments
    -- warn against (`re-parsing prose to recover a label is how a corpus rots`).
    consulted      INTEGER,
    -- **R12: which of the four `Unsure`s the answer was** — `could_not_decide`,
    -- `between_thresholds`, `unreadable`, `out_of_room` (`UnsureKind::as_str`), and NULL
    -- for an answered verdict and for every row where no model spoke. The distinction the
    -- operator asked for in as many words: *an oracle that ran out of budget is not an
    -- oracle that could not be read.* Same reasoning as the column above, one requirement
    -- later — the sentence that carries it is prose, and "the rate of each" is a question
    -- only a token can be counted in.
    oracle_reading TEXT,
    effect         TEXT NOT NULL,
    -- **Was a human actually asked?** The operator named this case directly: an
    -- UNSURE the gate surfaced and the operator answered anyway is a corpus row,
    -- and it is a DIFFERENT row from one the gate settled alone. Derivable from
    -- no other column here.
    asked          INTEGER NOT NULL DEFAULT 0,
    -- The label. Filled later, never overwriting model_verdict.
    operator_kind  TEXT,
    operator_note  TEXT,
    operator_ms    INTEGER,
    -- How long the human took. A ruling given in half a second and one given
    -- after forty are not the same label, and the second is the one worth most.
    operator_latency_ms INTEGER,
    -- The meaning of this row's columns. Rows written before a semantic change
    -- stay readable because they say which semantics they were written under.
    corpus_version INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS adjudication_by_session
    ON adjudication (session_id, decided_ms);

-- The rows a fine-tune is for: the operator ruled, and ruled against the gate.
CREATE INDEX IF NOT EXISTS adjudication_disagreements
    ON adjudication (operator_kind) WHERE operator_kind IS NOT NULL;

-- v35's `host_lease` table is `leases::LEASES_SQL`, run beside this text on a fresh store and by
-- the v35 arm on a migrated one — the shape every table added since v24 takes, so the DDL lives
-- in one place and a comment here cannot drift from it.

CREATE UNIQUE INDEX IF NOT EXISTS transcript_item_by_item_id
    ON transcript_item (transcript_id, item_id);

-- §4.1: "Immutable, with exactly one mutation: append." Said in SQL.
CREATE TRIGGER IF NOT EXISTS transcript_item_no_update
BEFORE UPDATE ON transcript_item BEGIN
    SELECT RAISE(ABORT, 'transcript_item is append-only: no UPDATE');
END;

CREATE TRIGGER IF NOT EXISTS transcript_item_no_delete
BEFORE DELETE ON transcript_item BEGIN
    SELECT RAISE(ABORT, 'transcript_item is append-only: no DELETE');
END;

-- The prefix invariant as a constraint. A hole, an overlap, a reordering or a
-- re-insert at an old index is refused by the database.
CREATE TRIGGER IF NOT EXISTS transcript_item_append_only_insert
BEFORE INSERT ON transcript_item BEGIN
    SELECT RAISE(ABORT, 'transcript_item seq must be the next one')
    WHERE NEW.seq <> (
        SELECT COALESCE(MAX(seq), -1) + 1
          FROM transcript_item WHERE transcript_id = NEW.transcript_id
    );
    SELECT RAISE(ABORT, 'transcript_item tok_offset must continue the previous row')
    WHERE NEW.seq > 0 AND NEW.tok_offset <> (
        SELECT tok_offset + tok_len FROM transcript_item
         WHERE transcript_id = NEW.transcript_id AND seq = NEW.seq - 1
    );
    SELECT RAISE(ABORT, 'transcript_item tok_len must match the token blob')
    WHERE NEW.tok_len <> LENGTH(NEW.tokens) / 4;
END;
"#;

/// The one statement a transcript row is written with, by [`Store::append_item`] and
/// [`Store::append_items`] alike.
const APPEND_SQL: &str = "INSERT INTO transcript_item
   (transcript_id, seq, item_id, kind, item_json,
    tok_offset, tok_len, h_k, tokens, created_at)
 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

/// The resume's own read: one transcript's rows in order, with the ledger facts they carry.
const TRANSCRIPT_ITEMS_SQL: &str = "SELECT item_id, item_json, tok_offset, tok_len, h_k, tokens
               FROM transcript_item WHERE transcript_id = ?1 ORDER BY seq ASC";

/// [`Store::corpus_counts`]'s one statement: five counts as one pass over the covering index v42
/// adds, because a `SELECT` per count was five passes and three of them read the whole table.
/// A function so a test can ask SQLite for its plan, the way [`sessions_sql`] is.
pub(crate) fn corpus_counts_sql() -> String {
    format!(
        "SELECT COUNT(*),
                COALESCE(SUM({ASKED_SQL}), 0),
                COALESCE(SUM({MEASURED_SQL}), 0),
                COALESCE(SUM({DISAGREEMENT_SQL}), 0),
                COALESCE(SUM({MODEL_DECIDED_SQL}), 0)
           FROM adjudication"
    )
}

#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    /// A blob whose byte length is not a whole number of token ids, or a hash
    /// that is not 32 bytes. Means the file was written by something else.
    Corrupt(String),
    /// The store is fine and the operation is not allowed. Separate from [`StoreError::Corrupt`]
    /// because they send a reader to opposite places, and printing a refused delete as
    /// corruption is how an operator runs `PRAGMA integrity_check` on a healthy file.
    Refused(String),
    NotFound(String),
    /// The file was written by a newer build. `floor` is the lowest schema a reader may know and
    /// still read this file, so the sentence a person needs depends on both numbers: a store at
    /// 34 whose floor is 24 is fine here. `None` predates floors, and this build cannot tell.
    SchemaTooNew {
        found: i64,
        known: i64,
        floor: Option<i64>,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sql(e) => write!(f, "sqlite: {e}"),
            StoreError::Json(e) => write!(f, "json: {e}"),
            StoreError::Corrupt(m) => write!(f, "store is corrupt: {m}"),
            StoreError::Refused(m) => write!(f, "{m}"),
            StoreError::NotFound(m) => write!(f, "not found: {m}"),
            // Both numbers, then the fix, because the reader of this sentence is a person
            // whose daemon just refused to start and whose one question is *what do I do*.
            StoreError::SchemaTooNew {
                found,
                known,
                floor,
            } => {
                write!(
                    f,
                    "store schema is version {found}; this build knows {known}"
                )?;
                match floor {
                    Some(floor) => write!(f, ", and the store needs a reader of at least {floor}")?,
                    None => write!(
                        f,
                        ", and the store records no reader floor to compare against"
                    )?,
                }
                write!(
                    f,
                    " — restart this daemon from a current build: letibot --stop"
                )
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sql(e)
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// The store's stamps are `i64` columns; the wall clock itself is the tree's one.
fn now_ms() -> i64 {
    letibot_metrics::now_ms() as i64
}

/// Token ids as a little-endian blob. Explicit, not a pointer cast: this blob
/// outlives the machine that wrote it.
pub fn tokens_to_blob(tokens: &[TokenId]) -> Vec<u8> {
    let mut v = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        v.extend_from_slice(&t.to_le_bytes());
    }
    v
}

pub fn blob_to_tokens(blob: &[u8]) -> Result<Vec<TokenId>> {
    if !blob.len().is_multiple_of(4) {
        return Err(StoreError::Corrupt(format!(
            "token blob of {} bytes is not a whole number of ids",
            blob.len()
        )));
    }
    Ok(blob
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect())
}

fn blob_to_hash(blob: &[u8]) -> Result<[u8; 32]> {
    <[u8; 32]>::try_from(blob)
        .map_err(|_| StoreError::Corrupt(format!("hash of {} bytes, want 32", blob.len())))
}

/// The `serde` tag of an item, denormalised into its own column so a query can
/// filter by kind without parsing every row's JSON.
pub fn item_kind(item: &TranscriptItem) -> &'static str {
    match item {
        TranscriptItem::System { .. } => "system",
        TranscriptItem::User { .. } => "user",
        TranscriptItem::Reasoning { .. } => "reasoning",
        TranscriptItem::Assistant { .. } => "assistant",
        TranscriptItem::ToolResult { .. } => "tool_result",
        TranscriptItem::Notice { .. } => "notice",
        TranscriptItem::SegmentMark { .. } => "segment_mark",
        TranscriptItem::Offer { .. } => "offer",
    }
}

/// A stable prefix as it goes into the store.
#[derive(Debug, Clone, PartialEq)]
pub struct StablePrefixRecord {
    pub dialect_sha: String,
    pub system: String,
    pub tools_json: Vec<String>,
    pub tokens: Vec<TokenId>,
    pub h_init: [u8; 32],
    pub vocab_source: String,
}

impl StablePrefixRecord {
    /// The content address: a hash over dialect, system, tools and the rest. The dialect is in
    /// it because the row stores TOKENS, and two dialects render one text to different ones;
    /// the byte vocabulary is hashed in only for a `bytes:` source, so GGUF ids never move.
    pub fn id(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.dialect_sha.as_bytes());
        h.update([0]);
        h.update(self.system.as_bytes());
        h.update([0]);
        for tool in &self.tools_json {
            h.update(tool.as_bytes());
            h.update([0]);
        }
        if self.vocab_source.starts_with(crate::vocab::BYTES_SOURCE) {
            h.update(b"vocab\0");
            h.update(self.vocab_source.as_bytes());
            h.update([0]);
        }
        crate::ledger::hex(&h.finalize().into())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: String,
    pub title: Option<String>,
    pub model_id: String,
    pub dialect_sha: String,
    pub workspace_root: String,
    pub owner: String,
    pub approvers: Vec<String>,
    /// The role to seat, or `None` for the daemon's own.
    pub role: Option<String>,
    /// The session that spawned this one as a subagent, or `None` for a top-level
    /// session. Recorded so a subagent tree can be drawn from the store rather than
    /// reconstructed from id conventions.
    pub parent_session_id: Option<String>,
}

/// A session name as an identifier: lowercase letters and digits, every other run of characters
/// one `-`, none at the ends. `Stroppy Conformity` and `stroppy-conformity` are one name.
pub fn slug_of(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Do two titles name one session: equal ignoring case, or equal as slugs when they have one.
pub fn same_name(a: &str, b: &str) -> bool {
    if a.to_lowercase() == b.to_lowercase() {
        return true;
    }
    let slug = slug_of(a);
    !slug.is_empty() && slug == slug_of(b)
}

/// A session as the store holds it: enough to list, pick and resume. Separate from
/// [`SessionRecord`], the write shape, and carrying three computed things (transcript id,
/// row count, last activity) that every list would otherwise re-derive differently.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSession {
    pub id: String,
    /// `None` when nobody has named it. Distinct from `Some("")`, which
    /// [`Store::set_title`] does not write.
    pub title: Option<String>,
    pub model_id: String,
    pub dialect_sha: String,
    pub workspace_root: String,
    pub owner: String,
    /// The role this session was opened with, or `None` if it predates v2 or was
    /// opened without one. `None` means the daemon's `--role`, not `coder`.
    pub role: Option<String>,
    /// The session that spawned this one as a subagent, or `None` for a top-level
    /// session.
    pub parent_session_id: Option<String>,
    pub created_ms: i64,
    /// Who this session is for, written once at creation. `None` predates the column, so a
    /// reader falls back to the parent edge and the id convention and calls the rest the
    /// operator's own.
    pub origin: Option<String>,
    /// The newest transcript for this session, or `None` if it has none at all.
    pub transcript_id: Option<String>,
    /// Rows in the session's current (newest) transcript. Not every transcript's
    /// rows summed: with forks, that would count the history a compaction just
    /// stopped carrying.
    pub items: u32,
    /// When the last row was written, falling back to `created_ms` for a session
    /// nothing ran in. Never `Option`: "never used" is a time, not an absence, and
    /// an `Option` here would make every caller invent the same fallback.
    pub last_activity_ms: i64,
    /// The current transcript's last row, or `None` for a session with no rows: what a list can
    /// say about how the last turn ended without loading the transcript. `None` too for a row
    /// this build cannot parse, which is a fact it has not got.
    pub last_item: Option<TranscriptItem>,
    /// The last turn's prompt tokens, or `None` before a turn has finished (or on
    /// a row that predates the column). A head that attaches after a restart shows
    /// the context from this rather than waiting for a turn.
    pub context_tokens: Option<u64>,
    /// The last turn's cached tokens, for the cache %. `None` with `context_tokens`.
    pub context_cached: Option<u64>,
    /// The fingerprint of the FROZEN prefix this session was seated with: the system
    /// instructions, the tool schemas and the seat's name. The standing notes are NOT in it,
    /// because they are re-read at every open and every turn and would always read stale.
    pub prefix_fingerprint: Option<String>,
    /// The ledger this box counted for the same prompt, the other half of
    /// [`StoredSession::context_tokens`]: a ratio needs both numbers from one prompt, and
    /// pairing a provider count with today's ledger is only right if nothing was appended.
    pub context_ledger: Option<u64>,
    /// The provider this session was switched to, `name` or `name/model`. `None` means it never
    /// chose one and resumes on the daemon's default; `Some("local")` is the deliberate switch
    /// back, kept apart because the two answer different questions at a resume.
    pub provider_choice: Option<String>,
    /// The window this session plans its compaction against, resolved when it opened: a child's
    /// window belongs to the model that ANSWERS it, not to its parent's. `None` is nobody
    /// recorded one, and the checks skip on it rather than reading it as a large window.
    pub context_window: Option<u64>,
    /// The pair that stood this session's automatic compaction down, `(resident, after)` in
    /// ledger tokens, or `None` when the no-progress guard never fired. On the row, not in daemon
    /// memory, so a restart does not re-arm the loop.
    pub auto_compact_stood_down: Option<(u64, u64)>,
    /// The operator's pause of this session's idle plan-check: `None` for no pause or a resumed
    /// one, `Some(0)` for a pause until they resume it, `Some(ms)` for one that ends by itself.
    /// On the row so a restart does not bring back the nags they turned off.
    pub nags_paused_until: Option<i64>,
    /// Where this session's live copy is, when it is not in this store's box: `None` means the
    /// copy in front of you is the live one. A migration marks the rows it leaves behind rather
    /// than deleting them, and `Some("local")` is a real value for a session that came home.
    pub lives_on: Option<String>,
    /// When it moved there, unix ms. `None` with [`Self::lives_on`].
    pub moved_ms: Option<i64>,
}

/// One job's row as the store keeps it: the durable half of the process table, which dies with
/// the daemon. A job is on disk from the moment it is backgrounded and its settlement updates
/// that one row, so `running` means the daemon was watching when it died, not that it runs now.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JobRecord {
    /// The handle `bash background: true` handed back, and the one `job_list` prints.
    pub handle: String,
    /// What was run, verbatim - the row the model asked for and the operator reads.
    pub command: String,
    /// Who backgrounded it, in the listing's own phrase. Empty means not recorded here, which is
    /// why a write that does not know it does not overwrite it: see [`Store::put_job`].
    pub how: String,
    /// The listing's own word for how it ended, or `running` while it has not.
    pub state: String,
    /// Bytes produced, all streams together.
    pub produced: u64,
    /// Wall time from spawn to settlement.
    pub elapsed_ms: u64,
    /// Where its output went, when that was not this daemon's window.
    pub redirect: Option<String>,
}

/// What kind of merge an entry is, out of a closed set: the queue's priority. A closed set and
/// not a free integer, because `3`, `high` and `urgent` are three populations that each look
/// like an answer. Two rungs; an urgent entry jumps subagent work, and ties break by age.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergePriority {
    /// The operator's entry. It jumps every subagent entry, whatever the age.
    Urgent,
    /// A subagent's entry. It waits behind an operator's urgent entry and ahead of
    /// nothing else; inside the rung it is oldest-first.
    Subagent,
}

impl MergePriority {
    /// Every priority, in the order the queue climbs: the one list, the one answer. `ALL[0]` is
    /// the highest and the queue sorts by position in it, so a rung outside this list is
    /// reachable by none.
    pub const ALL: [MergePriority; 2] = [MergePriority::Urgent, MergePriority::Subagent];

    /// The queue's rank: lower is taken first. A priority nobody recorded reads as the
    /// lowest rung rather than the highest, because a row that jumps the queue on the
    /// strength of a missing field is a row the queue cannot vouch for.
    pub fn rank(self) -> usize {
        Self::ALL
            .iter()
            .position(|p| *p == self)
            .unwrap_or(Self::ALL.len())
    }

    /// The rung a stored word names, if it names one.
    pub fn parse(stored: &str) -> Option<MergePriority> {
        Self::ALL.into_iter().find(|p| p.as_str() == stored)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MergePriority::Urgent => "urgent",
            MergePriority::Subagent => "subagent",
        }
    }
}

/// One step of the gate, as the landing ran it: one row per command `main`'s `AGENTS.md`
/// declared. A landing that records only `failed` can only ever draw `failed`, and the drawing
/// is the point. The rows live on the merge row and on the target's health row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MergeGateStep {
    /// The command as `main`'s `AGENTS.md` spells it: the step's identity, and the string a person
    /// would paste into a shell to reproduce the row. Empty for exactly one row, the `NoGate`
    /// one, where there was no command to run.
    pub command: String,
    /// What became of it, out of [`MergeGateOutcome`]'s closed set.
    pub outcome: MergeGateOutcome,
    /// What the step wrote, bounded by the gate's excerpt: the failing target first, then its
    /// first failing test and panic, then as much of the end as the cap allows. Empty on a step
    /// that never ran, which `not_run` says rather than an empty output.
    pub output: String,
    /// When the step started, Unix ms. `0` on a step that never ran.
    pub started_ms: u64,
    /// How long it took, ms. `0` on a step that never ran, and a real `0` for a command that
    /// took under a millisecond - the outcome is what tells the two apart, not this number.
    pub elapsed_ms: u64,
}

/// What became of one gate step, as a closed set: ran green, ran red, never reached because an
/// earlier step was red, or the one row a repository with no gate records. The last is a
/// variant because an empty checklist reads as *all steps passed*, which is the lie here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeGateOutcome {
    /// It ran, and it was green.
    Passed,
    /// It ran, and it was red. Its `output` is the tail that says why.
    Failed,
    /// An earlier step was red, so the gate stopped before this one. Not `passed`: a step
    /// that did not run is not a step that was green, and drawing the two the same way is how a
    /// gate that stopped at the first failure comes to look like a gate that ran everything.
    NotRun,
    /// The repository's `main` declares no gate at all: one row, an empty `command` and the
    /// queue's own sentence in `output`. A row and not an empty list, because the empty list is
    /// the row's word for *the gate has not run on this entry*.
    NoGate,
    /// It was still running at the gate's deadline and was killed with its whole process
    /// group (thirty minutes, then `timed out` - an outcome of its own).
    TimedOut,
    /// It was killed before it finished, because the daemon was stopping or lost the lease, or
    /// because the kernel killed it at the gate's memory cap (its `output` ends saying so, with
    /// the peak). It judged nothing.
    Killed,
}

/// What one row's words may cost, in bytes: a guard for the store's `todos_json` blob, which every
/// write rewrites whole, and for the rendered list that rides each `todo_write` answer. Not a UI
/// rule - the pane cuts a row at the terminal's width - and a row under it is stored as written.
pub const TODO_ROW_MAX_BYTES: usize = 4 * 1024;

/// The other half of the pair above: a board is a plan, and more rows than this is a runaway
/// write rather than a plan anybody reads. Bounds the rows one write adds to a board.
pub const TODO_ROWS_MAX: usize = 256;

/// The separator between a row's title and its detail. The board has no second field and needs
/// none: `content` is the row's one key, since a need names a row by its exact text, so the
/// detail rides after this separator and the pane draws it after the title.
pub const TODO_DETAIL_SEP: &str = " — ";

/// A row's TITLE: the words before [`TODO_DETAIL_SEP`], or the whole row. One function rather than a
/// rule each reader spells, because every reader must split the row in the same place. A newline is
/// NOT a separator: the pane flattens it, so a second line is not a body the row has.
pub fn todo_title(content: &str) -> &str {
    content
        .split_once(TODO_DETAIL_SEP)
        .map(|(title, _)| title)
        .unwrap_or(content)
        .trim()
}

/// The rows that wait on the OPERATOR right now: every open row with a [`TodoNeed::Operator`]
/// edge and the question it asks, in board order. One reading for every door that has to be
/// loud about it, so a row cannot wait on one screen and not another.
pub fn operator_asks(todos: &[TodoItem]) -> Vec<(&TodoItem, &str, &[String])> {
    let mut out = Vec::new();
    for row in todos {
        if matches!(row.status, TodoStatus::Completed | TodoStatus::Cancelled) {
            continue;
        }
        for need in &row.needs {
            if let TodoNeed::Operator { question, options } = need {
                out.push((row, question.as_str(), options.as_slice()));
            }
        }
    }
    out
}

/// One line of a session's todo list, as the model wrote it: mutable session metadata, the
/// `set_title` class rather than append-only. The whole list is sent on every write and it is
/// an UPSERT, and a row left out is NOT removed: only the operator deletes rows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    /// Who put it on the board. There is ONE list and the two authors share it, so this field is
    /// the whole of the difference between them. `serde(default)` and `Model`, so every list
    /// already in a store reads back as the model's, which is what it was.
    #[serde(default)]
    pub by: TodoBy,
    /// What this row is WAITING FOR: the condition that makes it due, EVALUATED and not observed
    /// so a firing survives a restart. An unresolvable condition must never read as met. `None`
    /// is the ordinary row, a thing to do rather than a thing to do when.
    #[serde(default)]
    pub when: Option<TodoCondition>,
    /// The rows this one waits for: the plan's edges, and why the idle check asks for a READY
    /// SET rather than the head of the list. A need that resolves to nothing, or to two, leaves
    /// the row BLOCKED and is SAID to be, because a silent edge makes the graph a lie.
    #[serde(default)]
    pub needs: Vec<TodoNeed>,
}

/// What a row waits on: one entry of [`TodoItem::needs`], one edge of the plan's DAG. A tagged
/// kind like [`TodoCondition`], for its reason: an edge nobody can evaluate must never read as
/// met, and a kind can be added without a new field on every row. The name is the row's WORDS.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoNeed {
    /// Another row on this board, named by its own exact `content`, the way
    /// `set_operator_states` quotes one back. A name matching none or two is unresolvable: the
    /// row is blocked and the check names the need it could not place, rather than a guess.
    Row { content: String },
    /// A CHILD session, by its session id: a row that is a dispatch waits on a session's
    /// completion, which makes a parent's row silent because it is not ready. The id is the
    /// handle `task` handed back, in full; whether it runs is the daemon's fact, handed in.
    Child { id: String },
    /// A background JOB, by the handle `bash background: true` prints: the row cannot start until
    /// that job ends, and while it runs the check is silent. The distinction from a `when` is
    /// the whole of why this exists: the two read one handle and mean opposite things.
    Job { handle: String },
    /// A MONITOR, by the name it was armed under: the row cannot start until it ends, fires,
    /// expires or is retired. While it watches the check is silent, because its firing wakes the
    /// session by itself. A name that is not watching is a monitor that ended.
    Monitor { name: String },
    /// A ROOM ITEM the holder has on its board, so the reminder arrives through the idle check and
    /// needs no model call. `working` is the whole of quiet-or-loud and it is on the ROW, because
    /// the daemon that wrote it is the one that asked the room. The number is the item's id.
    Room { item: u64, working: bool },
    /// The OPERATOR, asked a question: the row cannot start until a person answers it. An edge and
    /// not words appended to the row, because the answer has to arrive as the reply the row
    /// carries and unblock it.
    Operator {
        question: String,
        options: Vec<String>,
    },
    /// An [`TodoNeed::Operator`] edge the operator has ANSWERED: met, always, and kept on the row
    /// as the reply, so a submitted row becomes unblocked with its answer attached. `ask` is the
    /// room's id when the reply is a peer's rather than the operator's - it is what a settle is
    /// matched on, and `default` so every edge written before asks existed reads back.
    Answered {
        question: String,
        reply: String,
        #[serde(default)]
        ask: Option<u64>,
    },
    /// A question to another session, carried by the room as ask `ask`: the edge is A's side of
    /// an ask, and only the room settles it. `of` is the target's handle and `room` the room it
    /// was asked in, so the row says who is being waited on without asking the room again.
    Ask {
        ask: u64,
        of: String,
        room: String,
        question: String,
    },
    /// The other side: the ask `ask` somebody made of THIS session, waiting for its answer. Met,
    /// always - the asker waits, not this board - and it is the room pass's key for its own row,
    /// the way [`TodoNeed::Room`] keys a reminder row.
    Owed { ask: u64 },
    /// A kind this build does not know: a need written by a newer build. It is a variant because
    /// the alternative is worse - a resumed session's board would lose the row - and a need
    /// nobody can evaluate must never read as met.
    #[serde(other)]
    Unknown,
}

/// What a row waits on: see [`TodoItem::when`] for what evaluating one means. Internally tagged,
/// so the wire is self-describing, a new kind is a variant rather than a new field, and a reader
/// can tell *a condition I do not know* from *no condition at all*.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoCondition {
    /// Due when this job is no longer running: the handle `bash background: true` and `task` hand
    /// back. Absence is the condition, which is what survives a restart: a handle this session
    /// knows nothing about reads as gone, not as not yet.
    Job { handle: String },
}

/// Who authored a todo. Three authors, and the third is a STRING (`Parent <session-id>`), which is
/// how a parent writes on a child's board without a second kind of author.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TodoBy {
    /// The model wrote it with the `todo` tool.
    #[default]
    Model,
    /// The operator wrote it in the pane. It is the same list, the same statuses and the same
    /// tool - the model can mark the operator's item done, and the nag in `harness.rs` picks it up
    /// like any other, because it reads the board and the board no longer cares who wrote a row.
    Operator,
    /// A PARENT session wrote it on a CHILD's board: the author is `Parent <full parent session
    /// id>`, built by [`TodoBy::parent_of`] and never taken from a tool call, because the daemon
    /// knows which session is calling.
    Parent(String),
    /// The merge queue wrote it on the GATEKEEPER SESSION's board: the fourth author, and not a
    /// session at all. Its rows on a session's board are the operator's queue seen as work, so one
    /// queue is not a second board.
    Queue,
}

impl TodoBy {
    /// The author string for a parent's row: `Parent <session-id>`, verbatim and in full.
    pub fn parent_of(session_id: &str) -> TodoBy {
        TodoBy::Parent(format!("Parent {session_id}"))
    }
}

impl serde::Serialize for TodoBy {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            TodoBy::Model => s.serialize_str("model"),
            TodoBy::Operator => s.serialize_str("operator"),
            // The variant CARRIES the author string, so the wire form is the string itself.
            TodoBy::Parent(author) => s.serialize_str(author),
            TodoBy::Queue => s.serialize_str("queue"),
        }
    }
}

impl<'de> serde::Deserialize<'de> for TodoBy {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<TodoBy, D::Error> {
        let word = String::deserialize(d)?;
        match word.as_str() {
            "model" => Ok(TodoBy::Model),
            "operator" => Ok(TodoBy::Operator),
            "queue" => Ok(TodoBy::Queue),
            // `Parent …` is the one open spelling, and the prefix is the author's own marker:
            // anything else is a word this reader does not know, refused by name rather than
            // read as the model's the way a catch-all would.
            other if other.starts_with("Parent ") => Ok(TodoBy::Parent(other.to_string())),
            other => Err(serde::de::Error::custom(format!(
                "`{other}` is not a todo author: model, operator, queue, or `Parent <session-id>`"
            ))),
        }
    }
}

/// A todo's state. Serde as the lower-case words, so a stored list reads the
/// same in `sqlite3` as it does here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    /// Set aside by the OPERATOR: the row persists and stops asking, with a counter they can see. A
    /// fourth word rather than a quieter spelling of one of the three, because the state has to be
    /// reversible: lifting it hands the same question back.
    Postponed,
    /// Struck off by the OPERATOR, and the row STAYS: only the operator deletes rows, and everything
    /// that was ever created stays in history, so a struck-off row is a word and not a removal.
    Cancelled,
}

/// What a stable prefix was rendered from and by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StablePrefixMeta {
    pub dialect_sha: String,
    pub vocab_source: String,
    pub n_tokens: u32,
}

/// Everything needed to rebuild a ledger and a transcript after a restart.
#[derive(Debug, Clone)]
pub struct LoadedTranscript {
    pub transcript_id: String,
    pub session_id: String,
    pub parent_transcript_id: Option<String>,
    pub forked_at_seq: Option<u32>,
    pub stable_prefix_id: String,
    pub prefix_tokens: Vec<TokenId>,
    pub h_init: [u8; 32],
    /// Item, its ledger row, and its tokens -- in `seq` order.
    pub items: Vec<(TranscriptItem, LedgerRow, Vec<TokenId>)>,
}

impl LoadedTranscript {
    /// The `(row, tokens)` pairs `TokenLedger::restore` wants.
    pub fn ledger_input(&self) -> Vec<(LedgerRow, Vec<TokenId>)> {
        self.items
            .iter()
            .map(|(_, row, toks)| (row.clone(), toks.clone()))
            .collect()
    }
}

/// The one connection this process holds to a store file, and everything that reaches it: a
/// `Store` is a handle onto this, and [`Store::open`] hands every caller the same one. It is a
/// `Mutex` because `Connection` is `Send` and not `Sync`, so two threads need a lock.
struct Shared {
    /// `ManuallyDrop`, so the close is OURS and its failure can be said: `rusqlite`'s own `Drop`
    /// calls `sqlite3_close` and discards the answer, and `SQLITE_BUSY` then leaves the connection
    /// and its descriptors open with nobody told.
    conn: std::sync::Mutex<std::mem::ManuallyDrop<Connection>>,
    /// Where this store lives, so a second caller can be handed this connection instead of
    /// opening a new one - see [`Store::path`]. `None` for `open_in_memory`, which is a store
    /// with no file to reach and so nothing to share.
    path: Option<std::path::PathBuf>,
    /// `(device, inode)` of the file this connection is open on. A store file removed and
    /// written again at the same path is a DIFFERENT store, and a caller that opens that path
    /// must not be handed the connection to the one that is gone.
    file: Option<(u64, u64)>,
    /// The store is written by a build newer than this one, and this build opened it
    /// anyway - see [`floor`]. A `Mutex` rather than the `Cell` this was: a `Shared` is
    /// reached from every thread in the daemon, and a `Cell` is not `Sync`.
    newer: std::sync::Mutex<Option<NewerStore>>,
}

pub struct Store {
    shared: std::sync::Arc<Shared>,
}

/// The process's store lock, held: what [`Store::connection`] answers and what every method takes
/// for one statement or transaction. A newtype rather than the guard, so a caller reaches a
/// `Connection` in one `Deref` step.
pub struct Conn<'a>(std::sync::MutexGuard<'a, std::mem::ManuallyDrop<Connection>>);

impl std::ops::Deref for Conn<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.0
    }
}

/// How many connections to a store file this process holds open: [`Store::live`]'s counter, and a
/// daemon's own question - does this grow with the number of sessions? Process-wide because one
/// file is one connection.
static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The connection goes with the last handle, and the close is this impl's: see the field's doc
/// for the `SQLITE_BUSY` `rusqlite` swallows. A close that fails is said on stderr, because
/// there is no caller to return it to.
impl Drop for Shared {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: the connection is initialised in `Shared::new` and nothing else takes it; this
        // is the one place it is moved out, and the struct is being dropped so it is not read
        // again. The lock is taken so a close never races a statement on another thread.
        let conn = {
            let mut held = lock_shared(&self.conn);
            unsafe { std::mem::ManuallyDrop::take(&mut *held) }
        };
        if let Err((_conn, e)) = conn.close() {
            eprintln!(
                "store: the connection to {} did not close ({e}) — a statement was still open, so \
                 its descriptors are held until this process exits",
                self.path
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "an in-memory store".into())
            );
        }
    }
}

impl Shared {
    /// The connection, held for as long as the guard lives: the one place this process's access is
    /// serialised. A poisoned lock is a thread that panicked inside a statement, and SQLite's own
    /// state is fine because that statement was rolled back.
    fn conn(&self) -> std::sync::MutexGuard<'_, std::mem::ManuallyDrop<Connection>> {
        lock_shared(&self.conn)
    }

    /// Is the file this connection is open on still the file at this path? See [`Shared::file`].
    /// A path that cannot be stat'ed, or one that did not exist when the connection was made, says
    /// nothing - and refusing to share over that would open a second connection on every call.
    fn still_the_same_file(&self) -> bool {
        match (self.file, self.path.as_deref().and_then(file_id)) {
            (Some(open), Some(now)) => open == now,
            _ => true,
        }
    }
}

/// The lock, taken without minding a poison - see [`Shared::conn`] for why a poisoned lock is
/// taken back rather than propagated.
fn lock_shared(
    conn: &std::sync::Mutex<std::mem::ManuallyDrop<Connection>>,
) -> std::sync::MutexGuard<'_, std::mem::ManuallyDrop<Connection>> {
    conn.lock().unwrap_or_else(|e| e.into_inner())
}

/// Every store file this process has open, by canonical path. A `Weak` and not an `Arc`: the
/// connection lives exactly as long as something holds a `Store` for it, and the entry is
/// dropped when the next open finds it dead.
fn shared_stores()
-> &'static std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, std::sync::Weak<Shared>>>
{
    static OPEN: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, std::sync::Weak<Shared>>>,
    > = std::sync::OnceLock::new();
    OPEN.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// The key a store file is shared under - the path with its directory resolved, so two
/// spellings of one file (`./x/sessions.db` and `x/sessions.db`) meet. The FILE itself is not
/// canonicalised: on the first open it does not exist yet.
fn canonical_key(path: &Path) -> std::path::PathBuf {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    match (std::fs::canonicalize(parent), path.file_name()) {
        (Ok(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

/// `(device, inode)` of `path`, or `None` when there is no such file to ask about.
fn file_id(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// One row of a page: its `seq`, id, kind, JSON and when it was written - the five facts a head
/// draws a paged row from. A named type so the page's shape is said once.
pub type RowJson = (u32, String, String, String, u64);

/// The session list's one SELECT, with an optional `WHERE` appended. A free function so a test
/// can ask SQLite for its plan.
pub(crate) fn sessions_sql(filter: &str) -> String {
    format!(
        "SELECT s.id, s.title, s.model_id, s.dialect_sha, s.workspace_root, s.owner,
                    s.created_at,
                    (SELECT t.id FROM transcript t
                      WHERE t.session_id = s.id
                      ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1),
                    (SELECT COALESCE(MAX(i.seq) + 1, 0) FROM transcript_item i
                      WHERE i.transcript_id = (SELECT t.id FROM transcript t
                          WHERE t.session_id = s.id
                          ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1)),
                    (SELECT MAX((SELECT i.created_at FROM transcript_item i
                                  WHERE i.transcript_id = t.id
                                  ORDER BY i.seq DESC LIMIT 1))
                       FROM transcript t WHERE t.session_id = s.id),
                    s.role,
                    s.parent_session_id,
                    s.context_tokens,
                    s.context_cached,
                    s.context_ledger,
                    s.provider_choice,
                    s.context_window,
                    s.auto_compact_resident,
                    s.auto_compact_after,
                    (SELECT i.item_json FROM transcript_item i
                      WHERE i.transcript_id = (SELECT t.id FROM transcript t
                          WHERE t.session_id = s.id
                          ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1)
                        AND i.kind <> 'offer'
                      ORDER BY i.seq DESC LIMIT 1),
                    s.prefix_fingerprint,
                    s.nags_paused_until,
                    s.lives_on,
                    s.moved_ms,
                    s.origin
               FROM session s{filter}"
    )
}

impl Store {
    /// The process's one connection to this store file: SQLite caches a descriptor per inode inside
    /// the process and releases it only when the last connection closes, so a connection per caller
    /// is a descriptor leak wearing a pool's clothes.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_sharing(path, true)
    }

    /// A connection of its own to a store file, never shared - for the tests that measure two
    /// connections to one file (WAL, `BEGIN IMMEDIATE`, the busy handler). Production code wants
    /// [`Store::open`]: a second connection here is a descriptor the process cannot give back.
    pub fn open_private(path: &Path) -> Result<Self> {
        Self::open_sharing(path, false)
    }

    fn open_sharing(path: &Path, share: bool) -> Result<Self> {
        let key = if share {
            Some(canonical_key(path))
        } else {
            None
        };
        // The registry is held across the open so two threads cannot each create a connection to
        // one file. Nothing inside this window opens another store, so it cannot deadlock.
        let mut open = shared_stores().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(key) = &key
            && let Some(shared) = open.get(key).and_then(std::sync::Weak::upgrade)
            && shared.still_the_same_file()
        {
            return Ok(Store { shared });
        }
        let store = Self::from_connection(Connection::open(path)?, Some(path.to_path_buf()))?;
        if let Some(key) = key {
            open.insert(key, std::sync::Arc::downgrade(&store.shared));
        }
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, None)
    }

    /// The file this store is, when it has one: a PATH because the caller that needs one is on
    /// another thread, and a `Connection` is `Send` and not `Sync`.
    pub fn path(&self) -> Option<&Path> {
        self.shared.path.as_deref()
    }

    /// The connection, held for the duration of one statement or one transaction - the
    /// private door every method of this type goes through. Public callers get the same thing
    /// through [`Store::connection`].
    fn conn(&self) -> Conn<'_> {
        Conn(self.shared.conn())
    }

    /// How many connections to store files this process holds open right now: the census a test
    /// measures a session's lifetime by, because a descriptor count is a proxy and this is the
    /// thing.
    pub fn live() -> usize {
        LIVE.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How long a connection waits for the write lock before giving up: MEASURED, not chosen. On a
    /// box holding hundreds of sessions across many daemons the wait is real, and failing at once
    /// turns a busy file into a failed turn. Enforced by [`busy::on_busy`], not `busy_timeout`.
    pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// The one place a `Shared` is built, and therefore the one place [`LIVE`] is counted:
    /// every `Store` is a handle onto a `Shared`, so this is the count of connections.
    fn from_connection(conn: Connection, path: Option<std::path::PathBuf>) -> Result<Self> {
        // WAL lets readers and the writer run at once, but two WRITERS still serialise - this
        // process's behind the store's lock, another daemon's inside SQLite - and rusqlite's
        // default is to fail at once rather than wait. The handler sleeps the same schedule
        // `busy_timeout` does and stops at the same 30 s; it counts the sleep on the way.
        conn.busy_handler(Some(busy::on_busy))?;
        busy::retry_sql(|| conn.pragma_update(None, "journal_mode", "WAL"))?;
        // The region is volatile by design, so this file is the only copy of the
        // tokens. A lost commit is a lost turn.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let store = Store {
            shared: std::sync::Arc::new(Shared {
                conn: std::sync::Mutex::new(std::mem::ManuallyDrop::new(conn)),
                file: path.as_deref().and_then(file_id),
                path,
                newer: std::sync::Mutex::new(None),
            }),
        };
        // Taken here and not on success: `Drop` gives it back on every path, and `migrate` may
        // refuse and drop this store, so an add after the `?` would leave that drop subtracting
        // from a counter nobody added to.
        LIVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        store.migrate()?;
        Ok(store)
    }

    /// Run the migrations again on this store's own connection, for the one test that unwinds a
    /// schema and checks the migration carries it forward.
    #[cfg(test)]
    fn remigrate(self) -> Result<Self> {
        self.migrate()?;
        Ok(self)
    }

    fn migrate(&self) -> Result<()> {
        // Only a missing table is a fresh store. `.unwrap_or(None)` read EVERY failure as "no
        // schema yet", including `database is locked`, and then stamped the current version over
        // a store whose migrations had not run.
        let found: Option<i64> = match busy::retry_sql(|| {
            self.conn()
                .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
                .optional()
        }) {
            Ok(v) => v,
            Err(e) if e.to_string().contains("no such table") => None,
            Err(e) => return Err(e.into()),
        };
        match found {
            // A store AHEAD of this build is opened when its reader floor allows it: one nullable
            // column in a newer build must not stop every daemon on the box.
            Some(v) if v > SCHEMA_VERSION => return self.open_newer(v),
            // An older store is MIGRATED, not left alone and not rebuilt: the rows are append-only
            // and cannot be recreated, and before v2 `SCHEMA_SQL` only ran on an empty file.
            Some(v) if v < SCHEMA_VERSION => {
                self.migrate_from(v)?;
                self.conn().execute("DELETE FROM schema_version", [])?;
                self.conn().execute(
                    "INSERT INTO schema_version (version) VALUES (?1)",
                    params![SCHEMA_VERSION],
                )?;
                // The floor AFTER the version, never before: a floor above the version beside it
                // would refuse an older build a store that has not been migrated yet, and the two
                // writes cannot share a transaction without holding the write lock across it.
                let floor = self.ensure_reader_floor(SCHEMA_VERSION)?;
                debug_assert!(
                    floor <= SCHEMA_VERSION,
                    "a floor above the version it describes"
                );
                return Ok(());
            }
            // A store this build knows records its floor too: the repair for one created before the
            // floor existed, which a newer build could not tell from a store whose schema broke it.
            Some(_) => {
                self.ensure_reader_floor(SCHEMA_VERSION)?;
                return Ok(());
            }
            None => {}
        }
        self.conn().execute_batch(SCHEMA_SQL)?;
        self.conn().execute_batch(SESSION_INDEXES_SQL)?;
        self.conn().execute_batch(CORPUS_INDEXES_SQL)?;
        self.conn().execute_batch(queues::QUEUES_SQL)?;
        self.conn().execute_batch(subagents::SUBAGENTS_SQL)?;
        self.conn().execute_batch(modes::MODES_SQL)?;
        self.conn().execute_batch(told::TOLD_SQL)?;
        self.conn().execute_batch(marks::MARKS_SQL)?;
        self.conn().execute_batch(seat::SEAT_TODO_SQL)?;
        self.conn().execute_batch(citations::CITATIONS_SQL)?;
        self.conn().execute_batch(writers::WRITERS_SQL)?;
        self.conn().execute_batch(procs::PROCS_SQL)?;
        self.conn().execute_batch(leases::LEASES_SQL)?;
        self.conn().execute_batch(prompts::PROMPTS_SQL)?;
        self.conn().execute("DELETE FROM schema_version", [])?;
        self.conn().execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )?;
        // A fresh store's floor is what its migrations WOULD have raised: it was created by this
        // build, so it holds everything this build knows, and *nothing older than the newest
        // breaking migration is a reader of it* - see [`reader_floor_for`].
        self.ensure_reader_floor(SCHEMA_VERSION)?;
        Ok(())
    }

    /// Apply the steps from `from` up to [`SCHEMA_VERSION`]: one arm per version, each idempotent on
    /// its own, so a store two versions behind is carried forward by running both. An arm that
    /// breaks an older READER declares itself in [`READER_ARMS`].
    fn migrate_from(&self, from: i64) -> Result<()> {
        if from < 2 {
            // v2: a session records its own role. `ALTER TABLE ... ADD COLUMN` with
            // no default writes NULL into every existing row, which is exactly the
            // "unrecorded, use the daemon default" case the column documents.
            self.conn()
                .execute_batch("ALTER TABLE session ADD COLUMN role TEXT")?;
        }
        if from < 3 {
            // v3: the session todo list. A migrated store never runs `SCHEMA_SQL`
            // - `migrate` returns early - so the table is created here, and
            // `IF NOT EXISTS` keeps a store that somehow already has one honest.
            self.conn().execute_batch(
                "CREATE TABLE IF NOT EXISTS todo (
                     session_id  TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
                     todos_json  TEXT NOT NULL,
                     updated_ms  INTEGER NOT NULL
                 );",
            )?;
        }
        if from < 4 {
            // v4: a session records the session that spawned it as a subagent, so a
            // subagent tree is a fact on disk rather than an id convention. NULL in
            // every existing row: a session that predates v4 is a top-level one.
            self.conn()
                .execute_batch("ALTER TABLE session ADD COLUMN parent_session_id TEXT")?;
        }
        if from < 5 {
            // v5: the adjudication corpus. Created here for a migrated store,
            // which never runs SCHEMA_SQL.
            self.conn().execute_batch(
                "CREATE TABLE IF NOT EXISTS adjudication (
    request_id     TEXT PRIMARY KEY,
    session_id     TEXT NOT NULL,
    turn_id        TEXT NOT NULL,
    decided_ms     INTEGER NOT NULL,
    action         TEXT NOT NULL,
    baseline       TEXT NOT NULL,
    tier           TEXT NOT NULL,
    trail_json     TEXT NOT NULL,
    shown          TEXT,
    tool           TEXT NOT NULL DEFAULT '',
    arguments_json TEXT NOT NULL DEFAULT '{}',
    mode           TEXT NOT NULL DEFAULT '',
    options_json   TEXT NOT NULL DEFAULT '[]',
    agent          TEXT NOT NULL DEFAULT '',
    model_verdict  TEXT,
    verdict        TEXT,
    verdict_by     TEXT,
    verdict_basis  TEXT,
    p_allow        REAL,
    oracle_ms      INTEGER,
    oracle_model   TEXT,
    brief_sha      TEXT,
    effect         TEXT NOT NULL,
    asked          INTEGER NOT NULL DEFAULT 0,
    operator_kind  TEXT,
    operator_note  TEXT,
    operator_ms    INTEGER,
    operator_latency_ms INTEGER,
    corpus_version INTEGER NOT NULL DEFAULT 1
);

                 CREATE INDEX IF NOT EXISTS adjudication_by_session
                     ON adjudication (session_id, decided_ms);
                 CREATE INDEX IF NOT EXISTS adjudication_disagreements
                     ON adjudication (operator_kind) WHERE operator_kind IS NOT NULL;",
            )?;
        }
        if from < 6 {
            // v6: the SHAPE of the command a decision was about - the parse with its literals
            // replaced by holes, so two `grep`s differing only in pattern and range are one row.
            let has: bool = self
                .conn()
                .prepare("SELECT 1 FROM pragma_table_info('adjudication') WHERE name = 'shape'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE adjudication ADD COLUMN shape TEXT")?;
            }
        }
        if from < 7 {
            // v7: the effect class a shape was approved at, so the shape cache can
            // survive a restart without widening. Same idempotence argument as v6.
            let has: bool = self
                .conn()
                .prepare(
                    "SELECT 1 FROM pragma_table_info('adjudication') WHERE name = 'shape_class'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE adjudication ADD COLUMN shape_class TEXT")?;
            }
        }
        if from < 8 {
            // v8: the last turn's context on the session row, so a head that attaches after a
            // restart can say how big the prompt is without waiting for a turn.
            let has: bool = self
                .conn()
                .prepare("SELECT 1 FROM pragma_table_info('session') WHERE name = 'context_tokens'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn().execute_batch(
                    "ALTER TABLE session ADD COLUMN context_tokens INTEGER;
                     ALTER TABLE session ADD COLUMN context_cached INTEGER;",
                )?;
                // Backfill it for the sessions that already exist: `NULL` reads as "no turn has
                // finished", which is false for a conversation upgraded mid-life.
                self.conn().execute(
                    "UPDATE session
                     SET context_tokens = (
                         SELECT COALESCE(SUM(ti.tok_len), 0) + sp.n_tokens
                         FROM transcript t
                         JOIN stable_prefix sp ON sp.id = t.stable_prefix_id
                         LEFT JOIN transcript_item ti ON ti.transcript_id = t.id
                         WHERE t.id = (
                             SELECT t2.id FROM transcript t2
                             WHERE t2.session_id = session.id
                             ORDER BY t2.created_at DESC, t2.rowid DESC
                             LIMIT 1
                         )
                     )
                     WHERE context_tokens IS NULL
                       AND EXISTS (
                           SELECT 1 FROM transcript t3
                           WHERE t3.session_id = session.id
                       )",
                    [],
                )?;
            }
        }
        if from < 9 {
            // v9: what the oracle answered, verbatim. `shown` was meant to be the other half and
            // held the operator's card instead, so the reply needed a column of its own.
            let has: bool = self
                .conn()
                .prepare(
                    "SELECT 1 FROM pragma_table_info('adjudication') \
                     WHERE name = 'oracle_reply'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE adjudication ADD COLUMN oracle_reply TEXT")?;
            }
        }
        if from < 10 {
            // v10: whether an oracle was consulted, as a column rather than a reading of the
            // verdict's prose. NULL-able on purpose: "we did not keep it" must not look like
            // "nobody asked".
            let has: bool = self
                .conn()
                .prepare(
                    "SELECT 1 FROM pragma_table_info('adjudication') \
                     WHERE name = 'consulted'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE adjudication ADD COLUMN consulted INTEGER")?;
            }
        }
        if from < 11 {
            // v11: which of the four `Unsure`s the answer was, as a token. NULL on everything before
            // it, because those rows recorded the reason in a sentence and the two are not
            // interchangeable.
            let has: bool = self
                .conn()
                .prepare(
                    "SELECT 1 FROM pragma_table_info('adjudication') \
                     WHERE name = 'oracle_reading'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE adjudication ADD COLUMN oracle_reading TEXT")?;
            }
        }
        if from < 12 {
            // v12: the ledger `context_tokens` was measured against, because a provider count paired
            // with today's ledger is only the same conversation if nothing was appended since.
            let has: bool = self
                .conn()
                .prepare("SELECT 1 FROM pragma_table_info('session') WHERE name = 'context_ledger'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE session ADD COLUMN context_ledger INTEGER")?;
            }
        }
        if from < 13 {
            // v13: the provider this session was switched to. `set_provider` was in-memory and the
            // row had nowhere to record the choice, so a restart came back on the CLI default.
            let has: bool = self
                .conn()
                .prepare(
                    "SELECT 1 FROM pragma_table_info('session') WHERE name = 'provider_choice'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE session ADD COLUMN provider_choice TEXT")?;
            }
        }
        if from < 14 {
            // v14: the window a session plans its compaction against. The number lived only in the
            // spawn line, and a child's window belongs to the model that answers it.
            let has: bool = self
                .conn()
                .prepare("SELECT 1 FROM pragma_table_info('session') WHERE name = 'context_window'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE session ADD COLUMN context_window INTEGER")?;
            }
        }
        if from < 15 {
            // v15: the job history, a table because a session has many jobs and each is its own
            // row: one JSON blob per session would make a single handle's fate a whole-list read.
            self.conn().execute_batch(
                "CREATE TABLE IF NOT EXISTS job (
                     session_id  TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
                     handle      TEXT NOT NULL,
                     command     TEXT NOT NULL,
                     how         TEXT NOT NULL,
                     state       TEXT NOT NULL,
                     produced    INTEGER NOT NULL,
                     elapsed_ms  INTEGER NOT NULL,
                     redirect    TEXT,
                     updated_ms  INTEGER NOT NULL,
                     PRIMARY KEY (session_id, handle)
                 );",
            )?;
        }
        if from < 16 {
            // v16: the merge queue, a table for v15's reason: the daemon has many entries, and the
            // reader that wants one entry's fate is not reading all of them.
            self.conn().execute_batch(
                "CREATE TABLE IF NOT EXISTS merge_queue (
                     id          TEXT PRIMARY KEY,
                     session_id  TEXT NOT NULL,
                     branch      TEXT NOT NULL,
                     base_sha    TEXT NOT NULL,
                     priority    TEXT NOT NULL,
                     needs_json  TEXT NOT NULL,
                     state       TEXT NOT NULL,
                     evidence    TEXT NOT NULL,
                     created_ms  INTEGER NOT NULL,
                     updated_ms  INTEGER NOT NULL,
                     worktree    TEXT,
                     landed_sha  TEXT
                 );
                 CREATE INDEX IF NOT EXISTS merge_queue_state_idx
                     ON merge_queue (state, priority, created_ms);",
            )?;
        }
        if from < 17 {
            // v17: the ask a branch was produced under, on the entry, because the gatekeeper reviews
            // the artifact AGAINST the ask and nothing else can supply it once the child is gone.
            let has_brief: bool = self
                .conn()
                .prepare("SELECT 1 FROM pragma_table_info('merge_queue') WHERE name = 'brief'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has_brief {
                self.conn().execute_batch(
                    "ALTER TABLE merge_queue ADD COLUMN brief TEXT NOT NULL DEFAULT ''",
                )?;
            }
        }
        if from < 18 {
            // v18: the reviewer's verdict, a table for v16's reason: the queue and the review have
            // different writers and different readers.
            self.conn().execute_batch(
                "CREATE TABLE IF NOT EXISTS merge_review (
                     entry_id       TEXT PRIMARY KEY,
                     session_id     TEXT NOT NULL,
                     branch         TEXT NOT NULL,
                     base_sha       TEXT NOT NULL,
                     asked_ms       INTEGER NOT NULL,
                     answered_ms    INTEGER,
                     decision       TEXT,
                     reasons_json   TEXT NOT NULL DEFAULT '[]',
                     files_json     TEXT NOT NULL DEFAULT '[]',
                     commands_json  TEXT NOT NULL DEFAULT '[]'
                 );",
            )?;
        }
        if from < 21 {
            // v21: the frozen prefix's fingerprint, written where a session's transcript row is
            // written, so a later build can detect a prompt it would compose differently.
            let has: bool = self
                .conn()
                .prepare(
                    "SELECT 1 FROM pragma_table_info('session') \
                     WHERE name = 'prefix_fingerprint'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE session ADD COLUMN prefix_fingerprint TEXT")?;
            }
        }
        if from < 20 {
            // v20: the failed attempt, which is not a verdict. Three columns because they answer
            // three questions: may the queue ask again, when did it fail, and what did it say.
            self.conn().execute_batch(
                "CREATE TABLE IF NOT EXISTS merge_review (
                     entry_id       TEXT PRIMARY KEY,
                     session_id     TEXT NOT NULL,
                     branch         TEXT NOT NULL,
                     base_sha       TEXT NOT NULL,
                     asked_ms       INTEGER NOT NULL,
                     answered_ms    INTEGER,
                     decision       TEXT,
                     attempts       INTEGER NOT NULL DEFAULT 0,
                     failed_ms      INTEGER,
                     failure        TEXT NOT NULL DEFAULT '',
                     reasons_json   TEXT NOT NULL DEFAULT '[]',
                     files_json     TEXT NOT NULL DEFAULT '[]',
                     commands_json  TEXT NOT NULL DEFAULT '[]'
                 );",
            )?;
            let has: bool = self
                .conn()
                .prepare("SELECT 1 FROM pragma_table_info('merge_review') WHERE name = 'failure'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                // Every existing row reads as no failed attempt: it has a decision and an answered
                // stamp, and a verdict is not a failure.
                self.conn().execute_batch(
                    "ALTER TABLE merge_review ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
                     ALTER TABLE merge_review ADD COLUMN failed_ms INTEGER;
                     ALTER TABLE merge_review ADD COLUMN failure TEXT NOT NULL DEFAULT '';",
                )?;
            }
        }
        if from < 19 {
            // v19: the pair that stood automatic compaction down, two columns because a reader
            // compares them and should not parse a blob to do it.
            let has: bool = self
                .conn()
                .prepare(
                    "SELECT 1 FROM pragma_table_info('session') WHERE name = 'auto_compact_resident'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn().execute_batch(
                    "ALTER TABLE session ADD COLUMN auto_compact_resident INTEGER;
                     ALTER TABLE session ADD COLUMN auto_compact_after INTEGER;",
                )?;
            }
        }
        if from < 22 {
            // v22: the landing half's own rows, so a landing can be DRAWN and not merely judged.
            // `'[]'` on every existing row is the true state of one that ran no gate.
            self.conn().execute_batch(
                "CREATE TABLE IF NOT EXISTS merge_queue (
                      id          TEXT PRIMARY KEY,
                      session_id  TEXT NOT NULL,
                      branch      TEXT NOT NULL,
                      base_sha    TEXT NOT NULL,
                      priority    TEXT NOT NULL,
                      needs_json  TEXT NOT NULL,
                      state       TEXT NOT NULL,
                      brief       TEXT NOT NULL DEFAULT '',
                      evidence    TEXT NOT NULL,
                      created_ms  INTEGER NOT NULL,
                      updated_ms  INTEGER NOT NULL,
                      worktree    TEXT,
                      landed_sha  TEXT,
                      gate_steps_json TEXT NOT NULL DEFAULT '[]'
                  );
                  CREATE INDEX IF NOT EXISTS merge_queue_state_idx
                      ON merge_queue (state, priority, created_ms);",
            )?;
            let has: bool = self
                .conn()
                .prepare(
                    "SELECT 1 FROM pragma_table_info('merge_queue') WHERE name = 'gate_steps_json'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn().execute_batch(
                    "ALTER TABLE merge_queue ADD COLUMN gate_steps_json TEXT NOT NULL DEFAULT '[]'",
                )?;
            }
        }
        if from < 23 {
            // v23: the queue a pre-split store held is CLEARED once, on the first open by this
            // build. `from < 23` and not a pair of numbers, because v22 took that number while this
            // was in flight, and the arm must run for a store left at 21 or 22 either way.
            for table in ["merge_queue", "merge_review"] {
                let there: bool = self
                    .conn()
                    .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
                    .and_then(|mut st| st.exists(params![table]))
                    .unwrap_or(false);
                if there {
                    self.conn().execute(&format!("DELETE FROM {table}"), [])?;
                }
            }
        }
        if from < 24 {
            // v24: review and merge are two queues: the v23 pair archived under `_v23` names, the
            // new tables created, and the waiting rows re-enqueued for review. Nothing is deleted.
            self.migrate_queues_to_v24()?;
        }
        if from < 25 {
            // v25: the operator's pause of the idle plan-check. NULL in every existing row, because
            // nobody has paused a session yet.
            let has: bool = self
                .conn()
                .prepare(
                    "SELECT 1 FROM pragma_table_info('session') WHERE name = 'nags_paused_until'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE session ADD COLUMN nags_paused_until INTEGER")?;
            }
        }
        if from < 26 {
            // v26: a session's subagents as rows that outlive the daemon. A new table and an index,
            // both `IF NOT EXISTS`; nothing existing is read or rewritten.
            self.conn().execute_batch(subagents::SUBAGENTS_SQL)?;
        }
        if from < 27 {
            // v27: the permission modes, as rows - see [`modes`]. Two new tables, both
            // `IF NOT EXISTS`; nothing existing is read or rewritten. The rows `modes.tsv` held
            // are imported by the daemon (`harnessd::modes`), which is the one that knows the file.
            self.conn().execute_batch(modes::MODES_SQL)?;
        }
        if from < 28 {
            // v28: whether a job's ending reached the model. Every existing row is stamped
            // delivered, the conservative reading: this build cannot know, and a restart must not
            // re-announce every job a store ever held.
            let has = |sql: &str| -> bool {
                self.conn()
                    .prepare(sql)
                    .and_then(|mut st| st.exists([]))
                    .unwrap_or(false)
            };
            let table = has("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'job'");
            let column = has("SELECT 1 FROM pragma_table_info('job') WHERE name = 'delivered'");
            if table && !column {
                self.conn().execute_batch(
                    "ALTER TABLE job ADD COLUMN delivered INTEGER NOT NULL DEFAULT 1",
                )?;
            }
        }
        if from < 29 {
            // v29: what the queues have said, as rows. One new table; a store from before it may
            // say each standing alert once more, and then never again across restarts.
            self.conn().execute_batch(told::TOLD_SQL)?;
        }
        if from < 30 {
            // v30: the lines a reply cited - see [`citations`]. One new table, `IF NOT
            // EXISTS`; a reply from before it has no snapshot, and one is taken the first time
            // it is shown.
            self.conn().execute_batch(citations::CITATIONS_SQL)?;
        }
        if from < 31 {
            // v31: a session can live somewhere else. Two additive columns, NULL in every existing
            // row, which is true: no session that predates this verb was ever migrated.
            let has = |sql: &str| -> bool {
                self.conn()
                    .prepare(sql)
                    .and_then(|mut st| st.exists([]))
                    .unwrap_or(false)
            };
            if !has("SELECT 1 FROM pragma_table_info('session') WHERE name = 'lives_on'") {
                self.conn()
                    .execute_batch("ALTER TABLE session ADD COLUMN lives_on TEXT")?;
            }
            if !has("SELECT 1 FROM pragma_table_info('session') WHERE name = 'moved_ms'") {
                self.conn()
                    .execute_batch("ALTER TABLE session ADD COLUMN moved_ms INTEGER")?;
            }
        }
        if from < 32 {
            // v32: one writer per session, as a row. A migrated store never runs `SCHEMA_SQL`, so
            // the table is created here.
            self.conn().execute_batch(writers::WRITERS_SQL)?;
        }
        if from < 33 {
            // v33: who a session is for. `NULL` in every existing row is the honest value, and a
            // reader falls back to the parent edge and the queue's id convention.
            let has: bool = self
                .conn()
                .prepare("SELECT 1 FROM pragma_table_info('session') WHERE name = 'origin'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE session ADD COLUMN origin TEXT")?;
            }
        }
        if from < 34 {
            // v34: every process the daemon supervises, as a row. One new table; a store from before
            // it has no record of a process, which is the honest state.
            self.conn().execute_batch(procs::PROCS_SQL)?;
        }
        if from < 35 {
            // v35: a block of a shared host's ports, held by one session. One new table; a store from
            // before it has no lease.
            self.conn().execute_batch(leases::LEASES_SQL)?;
        }
        if from < 36 {
            // v36: the answer of a turn the child ran AFTER the one that settled it - a settlement
            // of its own, kept beside `outcome`, which stays the first answer.
            let has: bool = self
                .conn()
                .prepare(
                    "SELECT 1 FROM pragma_table_info('subagent_run') \
                     WHERE name = 'later_answer'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn()
                    .execute_batch("ALTER TABLE subagent_run ADD COLUMN later_answer TEXT")?;
            }
        }
        if from < 37 {
            // v37: nothing in the SCHEMA changes - the new thing is a transcript row KIND (`notice`),
            // which a younger reader meets as an unknown variant and refuses by name. The floor
            // exists for exactly this, so it moves and this arm does nothing else.
        }
        if from < 38 {
            // v38: accepted prompts the hub has not started - one new table, `IF NOT EXISTS`.
            self.conn().execute_batch(prompts::PROMPTS_SQL)?;
        }
        if from < 39 {
            // v39: two indexes so a session's transcripts and a parent's children are lookups, not scans.
            // A store with no `transcript` table cannot be listed and has nothing to index.
            let has_transcripts: bool = self.conn().query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'transcript')",
                [],
                |r| r.get(0),
            )?;
            if has_transcripts {
                let began = std::time::Instant::now();
                self.conn().execute_batch(SESSION_INDEXES_SQL)?;
                // Said after the fact: the build is one pass over two small tables, and a
                // store large enough to make it noticeable should not look hung.
                if began.elapsed() > std::time::Duration::from_millis(300) {
                    eprintln!(
                        "letibot: indexed the session store for faster listing ({} ms, one time)",
                        began.elapsed().as_millis()
                    );
                }
            }
        }
        if from < 40 {
            // v40: what a session's requests have learned about pictures. One new table,
            // `IF NOT EXISTS`; a store from before it re-earns each refusal once, which is the
            // honest state of a session nobody had taught yet.
            self.conn().execute_batch(marks::MARKS_SQL)?;
        }
        if from < 41 {
            // v41: a Claude Code seat's todo board - `todo` keyed by the seat's handle instead of a
            // session id, because a seat is not a session and its table carries no foreign key.
            self.conn().execute_batch(seat::SEAT_TODO_SQL)?;
        }
        if from < 42 {
            // v42: the corpus counts are read at every open, and without this index they scan the
            // adjudication table - on this box's store, 990 MB per count. One build, at one start.
            let has_corpus: bool = self.conn().query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'adjudication')",
                [],
                |r| r.get(0),
            )?;
            if has_corpus {
                let began = std::time::Instant::now();
                self.conn().execute_batch(CORPUS_INDEXES_SQL)?;
                if began.elapsed() > std::time::Duration::from_millis(300) {
                    eprintln!(
                        "letibot: indexed the decision corpus for its counts ({} ms, one time)",
                        began.elapsed().as_millis()
                    );
                }
            }
        }
        Ok(())
    }

    /// Insert a stable prefix, or return the id of the identical one already
    /// there. Idempotent because the id is the content.
    pub fn put_stable_prefix(&self, rec: &StablePrefixRecord) -> Result<String> {
        let id = rec.id();
        self.exec(
            "INSERT OR IGNORE INTO stable_prefix
               (id, dialect_sha, system, tools_json, n_tokens, tokens, h_init, vocab_source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                rec.dialect_sha,
                rec.system,
                serde_json::to_string(&rec.tools_json)?,
                rec.tokens.len() as i64,
                tokens_to_blob(&rec.tokens),
                rec.h_init.as_slice(),
                rec.vocab_source,
                now_ms(),
            ],
        )?;
        // A row written with no tokens is repairable, and only that row. `INSERT OR IGNORE` is
        // right for a content-addressed table: the id is the hash of the system text and the
        // tools, so a second write of it is the same prefix and not a repair.
        if !rec.tokens.is_empty() {
            self.exec(
                "UPDATE stable_prefix SET n_tokens = ?2, tokens = ?3, h_init = ?4
                   WHERE id = ?1 AND n_tokens = 0",
                params![
                    id,
                    rec.tokens.len() as i64,
                    tokens_to_blob(&rec.tokens),
                    rec.h_init.as_slice(),
                ],
            )?;
        }
        Ok(id)
    }

    pub fn get_stable_prefix(&self, id: &str) -> Result<(Vec<TokenId>, [u8; 32])> {
        Self::stable_prefix_on(&self.conn(), id)
    }

    /// [`Store::get_stable_prefix`] on a connection the caller already holds - see
    /// [`Store::load_transcript_on`].
    fn stable_prefix_on(conn: &Connection, id: &str) -> Result<(Vec<TokenId>, [u8; 32])> {
        let (blob, hash): (Vec<u8>, Vec<u8>) = conn
            .query_row(
                "SELECT tokens, h_init FROM stable_prefix WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("stable_prefix {id}")))?;
        Ok((blob_to_tokens(&blob)?, blob_to_hash(&hash)?))
    }

    /// The whole stable-prefix row, for a caller rebuilding the prefix a session was created with:
    /// a compaction fork keeps the session's own prefix rather than the one the running daemon
    /// would render now, the same rule a resume follows.
    pub fn stable_prefix_record(&self, id: &str) -> Result<StablePrefixRecord> {
        let (dialect_sha, system, tools_json, tokens, h_init, vocab_source): (
            String,
            String,
            String,
            Vec<u8>,
            Vec<u8>,
            String,
        ) = self
            .conn()
            .query_row(
                "SELECT dialect_sha, system, tools_json, tokens, h_init, vocab_source
                   FROM stable_prefix WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("stable_prefix {id}")))?;
        Ok(StablePrefixRecord {
            dialect_sha,
            system,
            tools_json: serde_json::from_str(&tools_json)?,
            tokens: blob_to_tokens(&tokens)?,
            h_init: blob_to_hash(&h_init)?,
            vocab_source,
        })
    }

    pub fn put_session(&self, rec: &SessionRecord) -> Result<()> {
        self.exec(
            "INSERT INTO session
               (id, title, model_id, dialect_sha, workspace_root, owner, approvers_json,
                created_at, role, parent_session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                rec.id,
                rec.title,
                rec.model_id,
                rec.dialect_sha,
                rec.workspace_root,
                rec.owner,
                serde_json::to_string(&rec.approvers)?,
                now_ms(),
                rec.role,
                rec.parent_session_id,
            ],
        )?;
        Ok(())
    }

    /// A fresh transcript. `parent` is `None`; use [`Store::put_fork`] otherwise.
    pub fn put_transcript(
        &self,
        transcript_id: &str,
        session_id: &str,
        stable_prefix_id: &str,
    ) -> Result<()> {
        self.insert_transcript(transcript_id, session_id, stable_prefix_id, None)
    }

    /// A transcript that came from a fork. A separate entry point rather than an `Option`
    /// argument, so recording the divergence is not something a caller can forget.
    pub fn put_fork(
        &self,
        transcript_id: &str,
        session_id: &str,
        stable_prefix_id: &str,
        parent_transcript_id: &str,
        forked_at_seq: u32,
    ) -> Result<()> {
        self.insert_transcript(
            transcript_id,
            session_id,
            stable_prefix_id,
            Some((parent_transcript_id, forked_at_seq)),
        )
    }

    /// A fork that is not the session's conversation until it is sealed: `put_fork` made it the
    /// newest transcript, which a resume opens, before its first row was written. Sealing is what
    /// says the fork is complete.
    pub fn put_fork_unsealed(
        &self,
        transcript_id: &str,
        session_id: &str,
        stable_prefix_id: &str,
        parent_transcript_id: &str,
        forked_at_seq: u32,
    ) -> Result<()> {
        self.exec(
            "INSERT INTO transcript
               (id, session_id, parent_transcript_id, forked_at_seq, stable_prefix_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            params![
                transcript_id,
                session_id,
                parent_transcript_id,
                forked_at_seq as i64,
                stable_prefix_id,
            ],
        )?;
        Ok(())
    }

    /// The fork's rows are all stored: it is the session's conversation from now on. See
    /// [`Store::put_fork_unsealed`]. One statement, so a resume sees the parent or the whole
    /// fork and never a part of one.
    pub fn seal_transcript(&self, transcript_id: &str) -> Result<()> {
        let n = self.exec(
            "UPDATE transcript SET created_at = ?2 WHERE id = ?1",
            params![transcript_id, now_ms()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("transcript {transcript_id}")));
        }
        Ok(())
    }

    /// The prefix a transcript was seated with, or `None` when the transcript is not there. The id
    /// and not the record: it is content-addressed, so two ids that differ are two different
    /// prompts, and that is the whole comparison a daemon needs.
    pub fn transcript_prefix_id(&self, transcript_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT stable_prefix_id FROM transcript WHERE id = ?1",
                params![transcript_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Re-seat a transcript that has no rows onto another prefix: the one prefix change that costs
    /// nothing. A session's message 0 is frozen once a row exists under it, so this refuses then.
    pub fn set_transcript_prefix(&self, transcript_id: &str, stable_prefix_id: &str) -> Result<()> {
        let n = self.exec(
            "UPDATE transcript SET stable_prefix_id = ?2
              WHERE id = ?1
                AND NOT EXISTS (SELECT 1 FROM transcript_item WHERE transcript_id = ?1)",
            params![transcript_id, stable_prefix_id],
        )?;
        if n > 0 {
            return Ok(());
        }
        let rows: Option<i64> = self
            .conn()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM transcript_item WHERE transcript_id = ?1)
                   FROM transcript WHERE id = ?1",
                params![transcript_id],
                |r| r.get(0),
            )
            .optional()?;
        match rows {
            None => Err(StoreError::NotFound(format!("transcript {transcript_id}"))),
            Some(rows) => Err(StoreError::Refused(format!(
                "transcript {transcript_id} holds {rows} row(s), and its steady prefix is what \
                 those rows were tokenized against: moving it would make every one of them \
                 unreadable. Nothing was changed. A conversation that must change its prompt \
                 forked onto a new transcript (`/reseat`, `/clear`, a compaction) rather than \
                 having message 0 swapped under it."
            ))),
        }
    }

    /// Every stored prefix exactly `n_tokens` long: the candidates a transcript is checked against
    /// when its rows do not rebuild under the prefix it names. A length and not an id, because
    /// the id is content-addressed on the prompt's text.
    pub fn stable_prefix_ids_of_len(&self, n_tokens: u32) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id FROM stable_prefix WHERE LENGTH(tokens) / 4 = ?1
              ORDER BY created_at DESC, rowid DESC",
        )?;
        let rows = stmt.query_map(params![n_tokens as i64], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn insert_transcript(
        &self,
        transcript_id: &str,
        session_id: &str,
        stable_prefix_id: &str,
        fork: Option<(&str, u32)>,
    ) -> Result<()> {
        self.exec(
            "INSERT INTO transcript
               (id, session_id, parent_transcript_id, forked_at_seq, stable_prefix_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                transcript_id,
                session_id,
                fork.map(|(p, _)| p),
                fork.map(|(_, s)| s as i64),
                stable_prefix_id,
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// Where row `seq` must start, from the store's own numbers: the transcript's prefix length
    /// plus the `tok_len` of every row before it. This is the one number `restore` checks a chain
    /// against.
    fn anchored_offset(&self, transcript_id: &str, seq: u32) -> Result<u32> {
        // `LENGTH` of the blob, not `n_tokens`: the bytes are what a restore replays, and
        // the two can only differ for a prefix row somebody wrote by hand.
        let prefix_len: i64 = self
            .conn()
            .query_row(
                "SELECT LENGTH(sp.tokens) / 4
                   FROM transcript t JOIN stable_prefix sp ON sp.id = t.stable_prefix_id
                  WHERE t.id = ?1",
                params![transcript_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("transcript {transcript_id}")))?;
        let before: i64 = self.conn().query_row(
            "SELECT COALESCE(SUM(tok_len), 0) FROM transcript_item
              WHERE transcript_id = ?1 AND seq < ?2",
            params![transcript_id, seq as i64],
            |r| r.get(0),
        )?;
        Ok((prefix_len + before) as u32)
    }

    /// The append refuses a run of rows that does not start where the transcript's prefix ends: a
    /// repair for the store an older build left, where a session seated under one prefix appended
    /// rows computed under another.
    fn assert_anchored(&self, transcript_id: &str, seq: u32, tok_offset: u32) -> Result<()> {
        let expected = self.anchored_offset(transcript_id, seq)?;
        if tok_offset == expected {
            return Ok(());
        }
        Err(StoreError::Refused(format!(
            "row {seq} of {transcript_id} would start at token {tok_offset}, but this \
             transcript's rows start where its stable prefix ends: {expected}. A resume \
             rebuilds the chain from the prefix, so a row written there would be refused \
             with `row {seq} starts at {tok_offset}, should start at {expected}` and the \
             conversation could never be loaded again. Nothing was written. This is the \
             shape a daemon makes when it appends to a transcript under a prompt the \
             transcript was not seated with — see `Store::set_transcript_prefix`."
        )))
    }

    /// Append one item and its ledger row together: one statement, so the row and the tokens it
    /// describes cannot disagree, and the insert trigger refuses it if `seq` or `tok_offset` does
    /// not continue the previous row.
    pub fn append_item(
        &self,
        transcript_id: &str,
        seq: u32,
        item: &TranscriptItem,
        row: &LedgerRow,
        tokens: &[TokenId],
    ) -> Result<()> {
        self.assert_anchored(transcript_id, seq, row.tok_offset)?;
        self.exec(
            APPEND_SQL,
            params![
                transcript_id,
                seq as i64,
                row.item_id,
                item_kind(item),
                serde_json::to_string(item)?,
                row.tok_offset as i64,
                row.tok_len as i64,
                row.h_k.as_slice(),
                tokens_to_blob(tokens),
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// Many rows, appended in as few commits as possible: the same rows, triggers and order
    /// [`Store::append_item`] writes, from `first_seq` on. Committed in chunks so one huge commit
    /// cannot hold the write lock for the whole batch.
    pub fn append_items<'a, I>(&self, transcript_id: &str, first_seq: u32, rows: I) -> Result<()>
    where
        I: IntoIterator<Item = (&'a TranscriptItem, &'a LedgerRow, &'a [TokenId])>,
    {
        self.append_items_with_context(transcript_id, first_seq, rows, None)
    }

    /// [`Store::append_items`], plus the session's context row in the last chunk's commit when
    /// `context` is given: `(session id, tokens, cached, ledger)`, as [`Store::set_context`] takes
    /// them. One fsync instead of two. A missing session row is not an error: context is a measurement.
    pub fn append_items_with_context<'a, I>(
        &self,
        transcript_id: &str,
        first_seq: u32,
        rows: I,
        context: Option<ContextRow<'_>>,
    ) -> Result<()>
    where
        I: IntoIterator<Item = (&'a TranscriptItem, &'a LedgerRow, &'a [TokenId])>,
    {
        let rows: Vec<_> = rows.into_iter().collect();
        // The same anchor [`Store::append_item`] checks, once for the run: the rows inside a
        // chunk continue each other by the trigger's rule, and every later chunk continues
        // the chunk before it, so the first row is the only one with nothing before it.
        if let Some((_, row, _)) = rows.first() {
            self.assert_anchored(transcript_id, first_seq, row.tok_offset)?;
        }
        let chunks = rows.chunks(Self::APPEND_CHUNK).count();
        for (c, chunk) in rows.chunks(Self::APPEND_CHUNK).enumerate() {
            let base = first_seq + (c * Self::APPEND_CHUNK) as u32;
            let last = c + 1 == chunks;
            // Serialised once, outside the retry: a busy retry re-runs only SQL.
            let mut encoded = Vec::with_capacity(chunk.len());
            for (item, row, tokens) in chunk {
                encoded.push((
                    item_kind(item),
                    serde_json::to_string(item)?,
                    *row,
                    tokens_to_blob(tokens),
                ));
            }
            self.write_tx(|tx| {
                let mut st = tx.prepare_cached(APPEND_SQL)?;
                for (i, (kind, json, row, blob)) in encoded.iter().enumerate() {
                    let seq = base + i as u32;
                    st.execute(params![
                        transcript_id,
                        seq as i64,
                        row.item_id,
                        kind,
                        json,
                        row.tok_offset as i64,
                        row.tok_len as i64,
                        row.h_k.as_slice(),
                        blob,
                        now_ms(),
                    ])
                    .map_err(|e| {
                        if is_busy_sql(&e) {
                            StoreError::Sql(e)
                        } else {
                            StoreError::Refused(format!(
                                "row {seq} of {transcript_id}: sqlite: {e}"
                            ))
                        }
                    })?;
                }
                if let (true, Some((id, tokens, cached, ledger))) = (last, context) {
                    tx.execute(
                        "UPDATE session SET context_tokens = ?2, context_cached = ?3, \
                         context_ledger = ?4 WHERE id = ?1",
                        params![
                            id,
                            tokens.map(|t| t as i64),
                            cached.map(|c| c as i64),
                            ledger.map(|l| l as i64)
                        ],
                    )?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Rows per transaction in [`Store::append_items`]. Small enough that one chunk's commit
    /// holds the lock for one fsync and a few milliseconds of inserts, large enough that a
    /// thousand-row copy is sixteen commits rather than a thousand.
    pub const APPEND_CHUNK: usize = 64;

    /// Everything needed to rebuild the ledger. Timed like a write: a reader never waits for a
    /// writer in WAL, but it holds the snapshot that keeps a checkpoint from moving the WAL, and
    /// this is the read that stalls when the disk is saturated.
    pub fn load_transcript(&self, transcript_id: &str) -> Result<LoadedTranscript> {
        let conn = self.conn();
        busy::retry_store(|| Self::load_transcript_on(&conn, transcript_id))
    }

    /// [`Store::load_transcript`] on a connection the caller already holds - the form
    /// [`live_view`] needs, because it reads one transcript out of a file it has ATTACHed and
    /// holds that connection for the whole copy.
    fn load_transcript_on(conn: &Connection, transcript_id: &str) -> Result<LoadedTranscript> {
        let (session_id, parent, forked_at, prefix_id): (
            String,
            Option<String>,
            Option<i64>,
            String,
        ) = conn
            .query_row(
                "SELECT session_id, parent_transcript_id, forked_at_seq, stable_prefix_id
                       FROM transcript WHERE id = ?1",
                params![transcript_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("transcript {transcript_id}")))?;

        let (prefix_tokens, h_init) = Self::stable_prefix_on(conn, &prefix_id)?;

        let mut stmt = conn.prepare(TRANSCRIPT_ITEMS_SQL)?;
        let rows = stmt.query_map(params![transcript_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Vec<u8>>(4)?,
                r.get::<_, Vec<u8>>(5)?,
            ))
        })?;

        let mut items = Vec::new();
        for row in rows {
            let (item_id, item_json, tok_offset, tok_len, h_k, blob) = row?;
            let item: TranscriptItem = serde_json::from_str(&item_json)?;
            let ledger_row = LedgerRow {
                item_id,
                tok_offset: tok_offset as u32,
                tok_len: tok_len as u32,
                h_k: blob_to_hash(&h_k)?,
            };
            items.push((item, ledger_row, blob_to_tokens(&blob)?));
        }

        Ok(LoadedTranscript {
            transcript_id: transcript_id.to_string(),
            session_id,
            parent_transcript_id: parent,
            forked_at_seq: forked_at.map(|s| s as u32),
            stable_prefix_id: prefix_id,
            prefix_tokens,
            h_init,
            items,
        })
    }

    pub fn item_count(&self, transcript_id: &str) -> Result<u32> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM transcript_item WHERE transcript_id = ?1",
            params![transcript_id],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    /// The transcript this one was forked from, or `None` for a session's first: one indexed lookup
    /// rather than a whole transcript load, because a resume walks the chain.
    pub fn parent_of(&self, transcript_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT parent_transcript_id FROM transcript WHERE id = ?1",
                params![transcript_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// One row's JSON by ordinal, the read behind `FetchRow`: one indexed lookup on
    /// `(transcript_id, seq)`, so it is neither a scan nor a transcript load, and that is the
    /// whole point of it.
    pub fn row_json_at(&self, transcript_id: &str, seq: u32) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT item_json FROM transcript_item\n                   WHERE transcript_id = ?1 AND seq = ?2",
                params![transcript_id, seq as i64],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// A page of rows by ordinal, in order, as [`RowJson`]. The read behind a head scrolling above
    /// its own window, and one query because a page asked for a row at a time is a page of round
    /// trips.
    pub fn rows_json_from(
        &self,
        transcript_id: &str,
        from: u32,
        count: u32,
    ) -> Result<Vec<RowJson>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT seq, item_id, kind, item_json, created_at FROM transcript_item\n              WHERE transcript_id = ?1 AND seq >= ?2 ORDER BY seq ASC LIMIT ?3",
        )?;
        let rows = st.query_map(params![transcript_id, from as i64, count as i64], |r| {
            Ok((
                r.get::<_, i64>(0)? as u32,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)? as u64,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The session's current transcript id, which is what a row ordinal is relative to: the NEWEST,
    /// because a fork replaces the conversation, so ordinal 3 after a compaction is a row of the
    /// new base.
    pub fn current_transcript_id(&self, session_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id FROM transcript WHERE session_id = ?1\n                   ORDER BY created_at DESC, rowid DESC LIMIT 1",
                params![session_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Every session in the store, newest activity first: what a picker or `letibot --sessions` is
    /// drawn from. Ordered by the last row written and not `created_at`, because the question is
    /// which one I was just in.
    pub fn list_sessions(&self) -> Result<Vec<StoredSession>> {
        self.sessions_matching("", &[])
    }

    /// The one query behind both [`Store::list_sessions`] and [`Store::session`], with an optional
    /// `WHERE` appended, so the 20-column positional reader exists once: two copies read BY
    /// POSITION would drift the first time a column was added.
    fn sessions_matching(
        &self,
        filter: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<StoredSession>> {
        // Count and last activity read the newest `seq` by key: `seq` is dense from 0 and rows are
        // written in time order, so neither needs a row scan. The "current transcript" subquery orders
        // by `created_at DESC, rowid DESC`, because a fork shares its parent's millisecond and needs it.
        // The session list, timed: it is what a picker, `--sessions` and the fleet view read,
        // and until this it was one of the reads no instrument could see.
        let conn = self.conn();
        busy::retry_store(|| {
            let mut stmt = conn.prepare(&sessions_sql(filter))?;
            let mut out: Vec<StoredSession> = stmt
                .query_map(args, |r| {
                    let created_ms: i64 = r.get(6)?;
                    let last: Option<i64> = r.get(9)?;
                    Ok(StoredSession {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        model_id: r.get(2)?,
                        dialect_sha: r.get(3)?,
                        workspace_root: r.get(4)?,
                        owner: r.get(5)?,
                        created_ms,
                        transcript_id: r.get(7)?,
                        items: r.get::<_, i64>(8)? as u32,
                        last_activity_ms: last.unwrap_or(created_ms),
                        role: r.get(10)?,
                        parent_session_id: r.get(11)?,
                        context_tokens: r.get::<_, Option<i64>>(12)?.map(|v| v as u64),
                        context_cached: r.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                        context_ledger: r.get::<_, Option<i64>>(14)?.map(|v| v as u64),
                        provider_choice: r.get(15)?,
                        context_window: r.get::<_, Option<i64>>(16)?.map(|v| v as u64),
                        auto_compact_stood_down: match (
                            r.get::<_, Option<i64>>(17)?,
                            r.get::<_, Option<i64>>(18)?,
                        ) {
                            // Half a pair is a store this file never wrote; reading it as
                            // `Some` would invent the other number, so it reads as no pair.
                            (Some(resident), Some(after)) => Some((resident as u64, after as u64)),
                            _ => None,
                        },
                        // Index 19, after the stood-down pair at 17 and 18. This list is read BY POSITION and
                        // both changes appended to it; the pair comes first because it is the older change.
                        last_item: r
                            .get::<_, Option<String>>(19)?
                            .and_then(|j| serde_json::from_str(&j).ok()),
                        // Index 20, appended for the same reason 19 was. The list is read BY
                        // POSITION and this is the newest fact on the row; the fingerprint goes
                        // last so nothing above it shifts.
                        prefix_fingerprint: r.get(20)?,
                        // Index 21, appended last for the reason 20 was.
                        nags_paused_until: r.get(21)?,
                        // Index 22 and 23, the two columns a migration writes. Appended
                        // for the reason every index above was: the list is read BY POSITION.
                        lives_on: r.get(22)?,
                        moved_ms: r.get(23)?,
                        // Index 24, appended for the reason 22 and 23 were, and read as a string because the
                        // vocabulary is `letibot_sessionlog::SessionOrigin`'s and this crate does not depend
                        // on it. A value this build does not know reads as unknown.
                        origin: r.get(24)?,
                    })
                })?
                .collect::<std::result::Result<_, _>>()?;
            out.sort_by_key(|s| std::cmp::Reverse(s.last_activity_ms));
            Ok(out)
        })
    }

    /// The name on disk right now, or `None` for a session nobody has named. Read rather than
    /// remembered: a head can set a title from its own thread while a harness holds an older idea,
    /// and trusting the cached copy would overwrite it.
    pub fn title(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT title FROM session WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .filter(|t: &String| !t.is_empty()))
    }

    /// One session, or `None`, KEYED: it reads one session's rows and no others. It was
    /// `list_sessions()` and a `find`, which made every lookup of one session a walk of every
    /// session's transcripts.
    pub fn session(&self, id: &str) -> Result<Option<StoredSession>> {
        Ok(self
            .sessions_matching(" WHERE s.id = ?1", &[&id])?
            .into_iter()
            .next())
    }

    /// The sessions whose parent is `parent_id`: one indexed lookup, for the caller that needs
    /// a session's kin and not the whole store.
    pub fn children_of(&self, parent_id: &str) -> Result<Vec<StoredSession>> {
        self.sessions_matching(" WHERE s.parent_session_id = ?1", &[&parent_id])
    }

    /// A session's parent, and nothing else: `Some(None)` for a root, `None` for an id this store
    /// does not hold. For the callers that walk a parent chain, which were loading a whole
    /// `StoredSession` at every step.
    pub fn session_parent(&self, id: &str) -> Result<Option<Option<String>>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT parent_session_id FROM session WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?)
    }

    /// A session's workspace, and nothing else - `None` for an id this store does not
    /// hold. The same reasoning as [`Store::session_parent`], for the caller that only asks
    /// which folder a session belongs to.
    pub fn session_workspace(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT workspace_root FROM session WHERE id = ?1",
                params![id],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Name a session, or clear its name with an empty string. `session` carries no append-only
    /// trigger and is not part of the chain: a title is metadata about a conversation, and
    /// changing it changes no prompt byte.
    pub fn set_title(&self, id: &str, title: &str) -> Result<()> {
        let n = self.exec(
            "UPDATE session SET title = ?2 WHERE id = ?1",
            params![id, (!title.is_empty()).then(|| title.to_string())],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// [`Store::set_title`] for a name a person typed: refused when another session in the same
    /// workspace already has it, by title (case-insensitive) or by [`slug_of`], because
    /// `--session NAME` and `@NAME` must pick one.
    pub fn rename_unique(&self, id: &str, title: &str) -> Result<()> {
        if !title.is_empty() {
            let me = self
                .session(id)?
                .ok_or_else(|| StoreError::NotFound(format!("session {id}")))?;
            if let Some(o) = self
                .sessions_matching(" WHERE s.workspace_root = ?1", &[&me.workspace_root])?
                .into_iter()
                .find(|o| o.id != id && o.title.as_deref().is_some_and(|t| same_name(t, title)))
            {
                return Err(StoreError::Refused(format!(
                    "the name {title:?} is taken in this project by {}",
                    o.id
                )));
            }
        }
        self.set_title(id, title)
    }

    /// Give an unnamed session `base` as its name, with `-2`, `-3`... appended until no other
    /// session in the workspace has it. `None` when the session already has a name, so a default
    /// never overwrites what a person typed.
    pub fn name_default(&self, id: &str, base: &str) -> Result<Option<String>> {
        let me = self
            .session(id)?
            .ok_or_else(|| StoreError::NotFound(format!("session {id}")))?;
        if me.title.as_deref().is_some_and(|t| !t.is_empty()) || base.is_empty() {
            return Ok(None);
        }
        let others =
            self.sessions_matching(" WHERE s.workspace_root = ?1", &[&me.workspace_root])?;
        let taken = |name: &str| {
            others
                .iter()
                .any(|o| o.id != id && o.title.as_deref().is_some_and(|t| same_name(t, name)))
        };
        let mut name = base.to_string();
        let mut n = 2u32;
        while taken(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        self.set_title(id, &name)?;
        Ok(Some(name))
    }

    /// The conversations of `workspace` that a typed `name` means exactly: the same title
    /// (case-insensitive) or the same [`slug_of`]. Subagents are not addressed by name.
    pub fn named_in(&self, workspace: &str, name: &str) -> Result<Vec<StoredSession>> {
        Ok(self
            .sessions_matching(" WHERE s.workspace_root = ?1", &[&workspace])?
            .into_iter()
            .filter(|s| {
                s.parent_session_id.is_none()
                    && s.title.as_deref().is_some_and(|t| same_name(t, name))
            })
            .collect())
    }

    /// Write down who this session is for. Set once, and the `AND origin IS NULL` is the guarantee:
    /// how a session came to exist does not change.
    pub fn set_session_origin(&self, id: &str, origin: &str) -> Result<()> {
        let there: bool = self
            .conn()
            .prepare("SELECT 1 FROM session WHERE id = ?1")
            .and_then(|mut st| st.exists(params![id]))?;
        if !there {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        self.exec(
            "UPDATE session SET origin = ?2 WHERE id = ?1 AND origin IS NULL",
            params![id, origin],
        )?;
        Ok(())
    }

    /// Record the last turn's context on the session row, so a head that attaches after a restart
    /// can show how big the prompt is without waiting for a turn. `ledger` is the other half the
    /// ratio needs; see [`StoredSession::context_ledger`].
    pub fn set_context(
        &self,
        id: &str,
        tokens: Option<u64>,
        cached: Option<u64>,
        ledger: Option<u64>,
    ) -> Result<()> {
        // `i64` on the wire: rusqlite's `ToSql` has no `u64`, and a token count
        // that does not fit an `i64` is not a prompt this box will ever send.
        let n = self.exec(
            "UPDATE session SET context_tokens = ?2, context_cached = ?3, context_ledger = ?4 \
             WHERE id = ?1",
            params![
                id,
                tokens.map(|t| t as i64),
                cached.map(|c| c as i64),
                ledger.map(|l| l as i64)
            ],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// Write the window this session plans its compaction against. A setter of its own rather than
    /// a parameter on [`Store::set_context`], because the two facts have different lifetimes.
    pub fn set_window(&self, id: &str, window: Option<u64>) -> Result<()> {
        // `i64` on the wire for the same reason `set_context` gives: rusqlite's `ToSql` has
        // no `u64`, and a window that does not fit an `i64` is not a window any server has.
        let n = self.exec(
            "UPDATE session SET context_window = ?2 WHERE id = ?1",
            params![id, window.map(|w| w as i64)],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// Write the fingerprint of the frozen prefix this session is seated with, where the transcript
    /// row is written and nowhere else: a later build compares it against what it would compose
    /// now.
    pub fn set_prefix_fingerprint(&self, id: &str, fingerprint: &str) -> Result<()> {
        let n = self.exec(
            "UPDATE session SET prefix_fingerprint = ?2 WHERE id = ?1",
            params![id, fingerprint],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// Write the pair that stood automatic compaction down. A setter of its own for
    /// [`Store::set_window`]'s reason: the two facts have different lifetimes.
    pub fn set_auto_compact_stood_down(&self, id: &str, pair: Option<(u64, u64)>) -> Result<()> {
        let n = self.exec(
            "UPDATE session SET auto_compact_resident = ?2, auto_compact_after = ?3 \
             WHERE id = ?1",
            params![id, pair.map(|(r, _)| r as i64), pair.map(|(_, a)| a as i64)],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// Pause this session's idle plan-check, or end the pause: `None` clears it, `Some(0)` is until
    /// the operator resumes, `Some(ms)` ends at that unix millisecond.
    pub fn set_nags_paused_until(&self, id: &str, until: Option<i64>) -> Result<()> {
        let n = self.exec(
            "UPDATE session SET nags_paused_until = ?2 WHERE id = ?1",
            params![id, until],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// The provider this session was switched to, or `None` for one that never
    /// chose. Read rather than remembered for the reason [`Store::title`] is: another writer can
    /// change the row while a harness holds an older idea.
    pub fn provider_choice(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT provider_choice FROM session WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .filter(|s: &String| !s.is_empty()))
    }

    /// Record the provider this session was switched to, or clear it with `None`. The choice is a
    /// session's fact and not the daemon's: a switch lived in a harness, and a restart lost it.
    pub fn set_provider_choice(&self, id: &str, choice: Option<&str>) -> Result<()> {
        let n = self.exec(
            "UPDATE session SET provider_choice = ?2 WHERE id = ?1",
            params![id, choice],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// The session's todo list, in the order the model last wrote it. Empty covers both "never
    /// wrote one" and "cleared it": from outside they are the same state.
    pub fn todos(&self, session_id: &str) -> Result<Vec<TodoItem>> {
        let json: Option<String> = self
            .conn()
            .query_row(
                "SELECT todos_json FROM todo WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json
            .map(|j| serde_json::from_str(&j))
            .transpose()?
            .unwrap_or_default())
    }

    /// Every session with a row waiting on the operator, and how many questions are open: the
    /// picker's mark, read for all sessions in one pass. Narrowed in SQL, decided in Rust.
    pub fn sessions_waiting_on_operator(&self) -> Result<BTreeMap<String, usize>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT session_id, todos_json FROM todo WHERE todos_json LIKE '%\"kind\":\"operator\"%'",
        )?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = BTreeMap::new();
        for row in rows {
            let (id, json) = row?;
            let Ok(todos) = serde_json::from_str::<Vec<TodoItem>>(&json) else {
                continue;
            };
            let n = operator_asks(&todos).len();
            if n > 0 {
                out.insert(id, n);
            }
        }
        Ok(out)
    }

    /// Replace the session's todo list wholesale. The whole list every time  -
    /// there is no append, no reorder-by-id, no delta: the model writes what the
    /// list now is, and that is what the store holds.
    pub fn put_todos(&self, session_id: &str, todos: &[TodoItem]) -> Result<()> {
        self.exec(
            "INSERT INTO todo (session_id, todos_json, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET todos_json = ?2, updated_ms = ?3",
            params![session_id, serde_json::to_string(todos)?, now_ms()],
        )?;
        Ok(())
    }

    /// Every job this session has recorded, ordered by when its row last moved. `handle` breaks
    /// the tie, so two jobs written in the same millisecond do not come back arbitrarily.
    pub fn jobs(&self, session_id: &str) -> Result<Vec<JobRecord>> {
        self.jobs_where(session_id, "")
    }

    /// [`Store::jobs`], narrowed by a fixed SQL condition (never a caller's text).
    fn jobs_where(&self, session_id: &str, and: &str) -> Result<Vec<JobRecord>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT handle, command, how, state, produced, elapsed_ms, redirect
               FROM job WHERE session_id = ?1 {and} ORDER BY updated_ms, handle"
        ))?;
        let rows = st.query_map(params![session_id], |r| {
            Ok(JobRecord {
                handle: r.get(0)?,
                command: r.get(1)?,
                how: r.get(2)?,
                state: r.get(3)?,
                // `i64` out of SQLite and back into the `u64` the wire spells: rusqlite's `FromSql`
                // has no `u64`, and a byte count that does not fit an `i64` is not a job this box
                // will ever run.
                produced: r.get::<_, i64>(4)? as u64,
                elapsed_ms: r.get::<_, i64>(5)? as u64,
                redirect: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Write one job's row, replacing that handle's last one, field by field: an upsert and not an
    /// append, because the row is a job and not a log line, and a job has one state at a time.
    pub fn put_job(&self, session_id: &str, job: &JobRecord) -> Result<()> {
        self.exec(
            "INSERT INTO job
               (session_id, handle, command, how, state, produced, elapsed_ms, redirect, updated_ms,
                delivered)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)
             ON CONFLICT(session_id, handle) DO UPDATE SET
               command = CASE WHEN ?3 = '' THEN command ELSE ?3 END,
               how = CASE WHEN ?4 = '' THEN how ELSE ?4 END,
               state = ?5, produced = ?6, elapsed_ms = ?7,
               redirect = ?8, updated_ms = ?9",
            params![
                session_id,
                job.handle,
                job.command,
                job.how,
                job.state,
                job.produced as i64,
                job.elapsed_ms as i64,
                job.redirect,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// This job's ending has reached the model: set where that is true, and by a restart that said
    /// so. `put_job` never touches it.
    pub fn deliver_job(&self, session_id: &str, handle: &str) -> Result<()> {
        self.exec(
            "UPDATE job SET delivered = 1 WHERE session_id = ?1 AND handle = ?2",
            params![session_id, handle],
        )?;
        Ok(())
    }

    /// The jobs of this session that ENDED and were never delivered: what a daemon that stopped
    /// while holding a result left for the next one to say. A `running` row is not one of them.
    pub fn undelivered_jobs(&self, session_id: &str) -> Result<Vec<JobRecord>> {
        self.jobs_where(session_id, "AND state <> 'running' AND delivered = 0")
    }

    /// The highest `jN` handle any job in this store was given, across every session - what a
    /// daemon seeds its handle counter above at start, so a handle is never minted twice for one
    /// session across restarts. `0` for a store with no `jN` handle.
    pub fn max_job_number(&self) -> Result<u64> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT handle FROM job WHERE handle GLOB 'j[0-9]*'")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut max = 0u64;
        for h in rows {
            if let Some(n) = h?.strip_prefix('j').and_then(|n| n.parse::<u64>().ok()) {
                max = max.max(n);
            }
        }
        Ok(max)
    }

    /// Remove a session that holds no transcript rows. A session with rows cannot be deleted, and
    /// that is not an omission: `transcript_item` refuses a delete by trigger, so the rows this
    /// would orphan cannot go.
    pub fn delete_empty_session(&self, id: &str) -> Result<()> {
        let Some(s) = self.session(id)? else {
            return Err(StoreError::NotFound(format!("session {id}")));
        };
        if s.items > 0 {
            return Err(StoreError::Refused(format!(
                "session {id} holds {} transcript row(s) and cannot be deleted: \
                 transcript_item carries a BEFORE DELETE trigger, because an \
                 append-only log whose history can be removed is not one. Rename it \
                 instead.",
                s.items
            )));
        }
        // Order matters: `transcript.session_id` is a foreign key and
        // `foreign_keys` is on. One IMMEDIATE transaction, so the two deletes land together
        // and a busy store is waited for rather than leaving a session with no transcript.
        self.write_tx(|tx| {
            tx.execute("DELETE FROM transcript WHERE session_id = ?1", params![id])?;
            tx.execute("DELETE FROM session WHERE id = ?1", params![id])?;
            Ok(())
        })
    }

    /// What a stable prefix was rendered from and by. `None` if it is not here.
    pub fn stable_prefix_meta(&self, id: &str) -> Result<Option<StablePrefixMeta>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT dialect_sha, vocab_source, n_tokens FROM stable_prefix WHERE id = ?1",
                params![id],
                |r| {
                    Ok(StablePrefixMeta {
                        dialect_sha: r.get(0)?,
                        vocab_source: r.get(1)?,
                        n_tokens: r.get::<_, i64>(2)? as u32,
                    })
                },
            )
            .optional()?)
    }

    // Escape hatch for the tests below and for `EXPLAIN`. Read-only by convention only, which
    // is why the append-only guarantees live in triggers and not in whoever holds this.
    pub fn record_adjudication(&self, a: &NewAdjudication) -> Result<bool> {
        let changed = self.exec(
            "INSERT OR IGNORE INTO adjudication
               (request_id, session_id, turn_id, decided_ms, action, baseline, tier,
                trail_json, shown, oracle_reply, tool, arguments_json, mode, options_json, agent,
                model_verdict, verdict, verdict_by, verdict_basis, p_allow, oracle_ms,
                oracle_model, shape, shape_class, brief_sha, consulted, oracle_reading,
                effect, asked, corpus_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29,
                     ?30)",
            rusqlite::params![
                a.request_id,
                a.session_id,
                a.turn_id,
                now_ms(),
                a.action,
                a.baseline,
                a.tier,
                a.trail_json,
                a.shown,
                a.reply,
                a.tool,
                a.arguments_json,
                a.mode,
                a.options_json,
                a.agent,
                a.model_verdict,
                a.verdict,
                a.verdict_by,
                a.verdict_basis,
                a.p_allow,
                a.oracle_ms,
                a.oracle_model,
                a.shape,
                a.shape_class,
                a.brief_sha,
                a.consulted.map(|c| c as i64),
                a.oracle_reading,
                a.effect,
                a.asked as i64,
                CORPUS_VERSION,
            ],
        )?;

        Ok(changed == 1)
    }

    /// The highest decision number this session has already stored: `AdjudicatedGate` numbers
    /// requests from an in-memory counter, so a resumed session re-emits ids that are taken.
    pub fn max_adjudication_seq(&self, session_id: &str) -> Result<u64> {
        let n: Option<i64> = self.conn().query_row(
            "SELECT MAX(CAST(substr(request_id, length(?1) + 6) AS INTEGER))
               FROM adjudication WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )?;
        Ok(n.unwrap_or(0).max(0) as u64)
    }

    /// The operator's ruling on a decision already recorded. This is the LABEL and it never
    /// touches `model_verdict`: a row where the two differ is the row a fine-tune is for.
    pub fn record_operator_ruling(&self, request_id: &str, kind: &str, note: &str) -> Result<bool> {
        let now = now_ms();
        let n = self.exec(
            "UPDATE adjudication
                SET operator_kind = ?2,
                    operator_note = ?3,
                    operator_ms = ?4,
                    -- Computed from the row's own `decided_ms` rather than passed
                    -- in: the caller does not reliably know when the gate decided,
                    -- and a latency measured against the wrong zero is worse than
                    -- none.
                    operator_latency_ms = ?4 - decided_ms
              WHERE request_id = ?1 AND operator_kind IS NULL",
            rusqlite::params![request_id, kind, note, now],
        )?;

        Ok(n > 0)
    }

    /// One half of one decision's oracle exchange. `column` is matched against a closed pair
    /// rather than interpolated, so a caller cannot name an arbitrary column.
    pub fn diagnostic(&self, request_id: &str, column: &str) -> Result<Option<String>> {
        let field = match column {
            "brief" => "shown",
            "reply" => "oracle_reply",
            // `Refused`, not `Corrupt`: the store is fine and the caller named a field that
            // is not one of the two this locator answers for.
            other => {
                return Err(StoreError::Refused(format!(
                    "`{other}` is not a diagnostic field"
                )));
            }
        };
        let sql = format!("SELECT {field} FROM adjudication WHERE request_id = ?1");
        let out: Option<Option<String>> = self
            .conn()
            .query_row(&sql, rusqlite::params![request_id], |r| r.get(0))
            .optional()?;
        // Nothing flattened: a row that is not there and a column that is `NULL` are the same
        // sentence to a reader ("not recorded"), and the caller cannot act differently on them.
        Ok(out.flatten())
    }

    /// The corpus, newest first. `only_labelled` narrows to rows the operator ruled on, because
    /// "every decision" and "every decision a human checked" are different datasets.
    pub fn corpus(&self, only_labelled: bool, limit: usize) -> Result<Vec<StoredAdjudication>> {
        const COLS: &str = "request_id, session_id, turn_id, decided_ms, action, baseline,
                    tier, trail_json, shown, oracle_reply, tool, arguments_json, mode,
                    options_json,
                    agent, model_verdict, verdict, verdict_by, verdict_basis, p_allow,
                    oracle_ms, oracle_model, brief_sha, consulted, oracle_reading, effect, asked,
                    operator_kind,
                    operator_note, operator_latency_ms, corpus_version";
        let sql = if only_labelled {
            format!(
                "SELECT {COLS} FROM adjudication WHERE operator_kind IS NOT NULL
                  ORDER BY decided_ms DESC LIMIT ?1"
            )
        } else {
            format!("SELECT {COLS} FROM adjudication ORDER BY decided_ms DESC LIMIT ?1")
        };
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([limit as i64], |r| {
                Ok(StoredAdjudication {
                    request_id: r.get(0)?,
                    session_id: r.get(1)?,
                    turn_id: r.get(2)?,
                    decided_ms: r.get(3)?,
                    action: r.get(4)?,
                    baseline: r.get(5)?,
                    tier: r.get(6)?,
                    trail_json: r.get(7)?,
                    shown: r.get(8)?,
                    reply: r.get(9)?,
                    tool: r.get(10)?,
                    arguments_json: r.get(11)?,
                    mode: r.get(12)?,
                    options_json: r.get(13)?,
                    agent: r.get(14)?,
                    model_verdict: r.get(15)?,
                    verdict: r.get(16)?,
                    verdict_by: r.get(17)?,
                    verdict_basis: r.get(18)?,
                    p_allow: r.get(19)?,
                    oracle_ms: r.get(20)?,
                    oracle_model: r.get(21)?,
                    brief_sha: r.get(22)?,
                    consulted: r.get::<_, Option<i64>>(23)?.map(|v| v != 0),
                    oracle_reading: r.get(24)?,
                    effect: r.get(25)?,
                    asked: r.get::<_, i64>(26)? != 0,
                    operator_kind: r.get(27)?,
                    operator_note: r.get(28)?,
                    operator_latency_ms: r.get(29)?,
                    corpus_version: r.get(30)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Shapes this operator approved by hand, under this workspace: the warm start for the shape
    /// cache, which was in-memory and died with the process, exactly when a restart needed it.
    pub fn approved_shapes(&self, workspace_root: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT a.shape, a.shape_class
               FROM adjudication a
               JOIN session s ON s.id = a.session_id
              WHERE a.shape IS NOT NULL
                AND a.shape_class IS NOT NULL
                AND a.tier = 'may_approve'
                AND a.effect = 'admit'
                AND a.verdict_by LIKE 'human%'
                AND a.baseline NOT LIKE '%destroy%'
                AND s.workspace_root = ?1
              ORDER BY a.decided_ms DESC",
        )?;
        let rows = stmt
            .query_map([workspace_root], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Human admits recorded before the shape column was filled in: the input to the backfill, and
    /// nothing else reads it. It carries the workspace from the session rather than assuming one.
    pub fn shapeless_human_admits(&self) -> Result<Vec<ShapelessAdmit>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT a.request_id, s.workspace_root, a.tool, a.arguments_json
               FROM adjudication a
               JOIN session s ON s.id = a.session_id
              WHERE a.shape IS NULL
                AND a.tier = 'may_approve'
                AND a.effect = 'admit'
                AND a.verdict_by LIKE 'human%'
              ORDER BY a.decided_ms ASC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ShapelessAdmit {
                    request_id: r.get(0)?,
                    workspace_root: r.get(1)?,
                    tool: r.get(2)?,
                    arguments_json: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Write a shape and its class onto a row that has neither. `WHERE shape IS NULL` is the whole
    /// safety property: this fills a hole and never changes an answer, and a second run finds
    /// nothing.
    pub fn backfill_shape(&self, request_id: &str, shape: &str, class: &str) -> Result<bool> {
        let n = self.exec(
            "UPDATE adjudication SET shape = ?2, shape_class = ?3
              WHERE request_id = ?1 AND shape IS NULL",
            rusqlite::params![request_id, shape, class],
        )?;
        Ok(n == 1)
    }

    /// Counts for the startup disclosure: a corpus nobody can see the size of is
    /// a corpus nobody maintains.
    pub fn corpus_counts(&self) -> Result<CorpusCounts> {
        let (total, asked, measured, disagreements, model_decided) =
            self.conn().query_row(&corpus_counts_sql(), [], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?;
        Ok(CorpusCounts {
            total: total as u64,
            decided_by_operator: asked as u64,
            // `measured` is `consulted`, not `verdict_by LIKE 'model%'`: that predicate was a
            // tautology which counted every row as measured, and this banner said so.
            measured: measured as u64,
            disagreements: disagreements as u64,
            model_decided: model_decided as u64,
        })
    }

    /// One autocommit write statement, taken again while the store is busy. Every write method
    /// takes this, and a retry writes once because the statement either ran or did not.
    #[track_caller]
    fn exec<P: Params + Clone>(&self, sql: &str, p: P) -> rusqlite::Result<usize> {
        busy::retry_sql(|| self.conn().execute(sql, p.clone()))
    }

    /// A write transaction: `BEGIN IMMEDIATE`, the body, `COMMIT`, taken again while busy.
    /// IMMEDIATE because a deferred transaction that reads first holds a lock it cannot upgrade.
    #[track_caller]
    pub fn write_tx<T>(
        &self,
        mut body: impl FnMut(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        busy::retry_store(|| {
            let conn = self.conn();
            let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
            let out = body(&tx)?;
            tx.commit()?;
            Ok(out)
        })
    }

    /// The connection, with the process's lock held for as long as the guard lives. A guard and
    /// not `&Connection`, because there is one connection per store file per process.
    pub fn connection(&self) -> Conn<'_> {
        self.conn()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod corpus_tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    fn a_session(s: &Store) -> String {
        let id = "s-corpus-1".to_string();
        s.put_session(&SessionRecord {
            id: id.clone(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: "/tmp".into(),
            owner: "op".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("session");
        id
    }

    /// A resumed session must not silently drop every decision it makes: the gate numbers requests
    /// from an in-memory counter, so the store is asked for the highest id already taken.
    #[test]
    fn a_repeated_request_id_is_reported_and_the_seq_can_be_resumed_from() {
        let s = store();
        let sid = "s-42";
        assert_eq!(s.max_adjudication_seq(sid).expect("seq"), 0, "nothing yet");

        let mut a = a_decision("adj-s-42-0001", sid);
        assert!(
            s.record_adjudication(&a).expect("write"),
            "the first write lands"
        );
        // The same id again - a restarted daemon's first decision.
        a.tool = "a different call entirely".into();
        assert!(
            !s.record_adjudication(&a).expect("write"),
            "a collision must report that nothing was written"
        );

        a.request_id = "adj-s-42-0002".into();
        assert!(s.record_adjudication(&a).expect("write"));
        assert_eq!(
            s.max_adjudication_seq(sid).expect("seq"),
            2,
            "a restart seeds from here and carries on at 3"
        );
        // Another session's rows are not this one's high-water mark.
        assert_eq!(s.max_adjudication_seq("s-99").expect("seq"), 0);
    }

    fn a_decision(request_id: &str, session_id: &str) -> NewAdjudication {
        NewAdjudication {
            request_id: request_id.into(),
            session_id: session_id.into(),
            turn_id: "t-1".into(),
            action: "destroy /p/target".into(),
            baseline: "ask — destruction".into(),
            tier: "AlwaysAsk".into(),
            trail_json: "{}".into(),
            tool: "bash".into(),
            arguments_json: r#"{"command":"rm -rf /p/target"}"#.into(),
            mode: "always-ask".into(),
            options_json: "[]".into(),
            agent: "coder".into(),
            effect: "refuse".into(),
            ..Default::default()
        }
    }

    /// the exchange is on the row, and an older row says so honestly. The table already had
    /// `shown` and it held the operator's decision instead, so the reply needed its own column.
    #[test]
    fn the_oracles_brief_and_reply_round_trip_and_an_older_row_says_null() {
        let s = store();
        let session = a_session(&s);
        let mut d = a_decision("adj-r11", &session);
        d.shown = Some("brief — a file was read\ntrail: 1 operator message(s)".into());
        d.reply = Some("It follows from what was asked.\nALLOW 0".into());
        assert!(s.record_adjudication(&d).unwrap());

        let row = s.corpus(false, 10).unwrap().remove(0);
        assert!(
            row.shown.as_deref().unwrap().starts_with("brief — "),
            "{:?}",
            row.shown
        );
        assert!(
            row.reply.as_deref().unwrap().contains("ALLOW 0"),
            "{:?}",
            row.reply
        );

        // The other two facts, which are not the same as a missing column: no oracle was
        // consulted at all, and one was asked and said nothing.
        let mut quiet = a_decision("adj-quiet", &session);
        quiet.shown = None;
        quiet.reply = None;
        assert!(s.record_adjudication(&quiet).unwrap());
        let row = s
            .corpus(false, 10)
            .unwrap()
            .into_iter()
            .find(|r| r.request_id == "adj-quiet")
            .unwrap();
        assert_eq!(row.shown, None);
        assert_eq!(row.reply, None);

        // A row from an older daemon: the column is there and empty, which is why
        // `verdict_by` is what tells a reader whether anybody was asked.
        s.conn()
            .execute(
                "UPDATE adjudication SET oracle_reply = NULL WHERE request_id = 'adj-r11'",
                [],
            )
            .unwrap();
        let row = s
            .corpus(false, 10)
            .unwrap()
            .into_iter()
            .find(|r| r.request_id == "adj-r11")
            .unwrap();
        assert_eq!(row.reply, None);
        assert!(
            row.shown.is_some(),
            "the brief survives a reply that was never recorded"
        );
    }

    /// which of the four `Unsure`s it was, as a column. An oracle that ran out of budget is
    /// not one that could not be read, and a sentence could not be counted.
    #[test]
    fn the_reading_of_an_unsure_is_a_column_and_the_rate_of_each_is_a_query() {
        let s = store();
        let session = a_session(&s);
        for (i, kind) in [
            "could_not_decide",
            "between_thresholds",
            "unreadable",
            "out_of_room",
        ]
        .into_iter()
        .enumerate()
        {
            let mut d = a_decision(&format!("adj-r12-{i}"), &session);
            d.consulted = Some(true);
            d.oracle_reading = Some(kind.into());
            assert!(s.record_adjudication(&d).unwrap());
        }
        // An answered verdict: consulted, and no reading - which is what a row looks like
        // when the oracle did its job.
        let mut answered = a_decision("adj-r12-answered", &session);
        answered.consulted = Some(true);
        assert!(s.record_adjudication(&answered).unwrap());

        let rows = s.corpus(false, 20).unwrap();
        let mut seen: Vec<&str> = rows
            .iter()
            .filter(|r| r.request_id.starts_with("adj-r12-") && r.request_id != "adj-r12-answered")
            .map(|r| r.oracle_reading.as_deref().expect("a reading"))
            .collect();
        seen.sort_unstable();
        assert_eq!(
            seen,
            [
                "between_thresholds",
                "could_not_decide",
                "out_of_room",
                "unreadable"
            ]
        );
        let answered = rows
            .iter()
            .find(|r| r.request_id == "adj-r12-answered")
            .expect("the answered row");
        assert_eq!(answered.consulted, Some(true));
        assert_eq!(
            answered.oracle_reading, None,
            "an answered verdict has no reading"
        );

        // The rate of each, which is what the column exists for: one query, and the answer is a
        // count per token rather than a regex over a sentence. The guard's scope is this block.
        let counts: Vec<(String, i64)> = {
            let conn = s.conn();
            let mut stmt = conn
                .prepare(
                    "SELECT oracle_reading, COUNT(*) FROM adjudication
                      WHERE oracle_reading IS NOT NULL GROUP BY 1 ORDER BY 1",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            counts,
            [
                ("between_thresholds".to_string(), 1),
                ("could_not_decide".to_string(), 1),
                ("out_of_room".to_string(), 1),
                ("unreadable".to_string(), 1),
            ]
        );

        // And a row written before the column - `NULL`, which is not any of the four. A
        // reader of the old ones goes to `model_verdict`/`verdict_basis`, and `corpus_version`
        // is what says the row is older than the question.
        s.conn()
            .execute(
                "UPDATE adjudication SET oracle_reading = NULL WHERE request_id = 'adj-r12-3'",
                [],
            )
            .unwrap();
        let row = s
            .corpus(false, 20)
            .unwrap()
            .into_iter()
            .find(|r| r.request_id == "adj-r12-3")
            .unwrap();
        assert_eq!(row.oracle_reading, None);
    }

    /// Who a session is for is written once, and a row from before the column says `NULL`, which
    /// is not "the operator's": it is nobody has said, and the reader does the falling back.
    #[test]
    fn a_sessions_origin_is_written_once_and_read_back() {
        let s = store();
        let id = a_session(&s);
        let row = |s: &Store| s.session(&id).unwrap().expect("the row");
        assert_eq!(row(&s).origin, None, "a row nobody has classified yet");

        s.set_session_origin(&id, "package").expect("the write");
        assert_eq!(row(&s).origin.as_deref(), Some("package"));
        assert_eq!(
            s.list_sessions().unwrap()[0].origin.as_deref(),
            Some("package"),
            "and the list a picker is drawn from carries it"
        );

        // Set once. How a session came to exist does not change, and a second write
        // overwriting the first would be a picker that re-groups a conversation under the person
        // using it - the row-renames-itself defect in a new column.
        s.set_session_origin(&id, "one_shot").expect("a no-op");
        assert_eq!(row(&s).origin.as_deref(), Some("package"));

        // An id this store does not hold is refused by name rather than silently written.
        assert!(s.set_session_origin("s-nobody", "operator").is_err());
    }

    /// A store from the version before gains the column and its rows read `NULL`: the migration's
    /// half of the test above, and the reason the arm is `pragma_table_info`-guarded.
    #[test]
    fn an_older_store_gains_the_origin_column_and_its_rows_say_nobody_has_said() {
        let path = std::env::temp_dir().join(format!(
            "letibot-store-v32-{}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        let _ = std::fs::remove_file(&path);
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _clean = Clean(path.clone());

        let id;
        {
            let s = Store::open(&path).unwrap();
            id = a_session(&s);
        }
        {
            // Back to a store that predates the column, the way one on disk from an older
            // daemon is.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("ALTER TABLE session DROP COLUMN origin", [])
                .unwrap();
            c.execute("UPDATE schema_version SET version = 32", [])
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let v: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);

        let row = s.session(&id).unwrap().expect("the old row survived");
        assert_eq!(
            row.origin, None,
            "a row written before the column is not the operator's by fiat — it is unclassified"
        );
        // And it can be classified now, in the same store.
        s.set_session_origin(&id, "queue").expect("the write");
        assert_eq!(
            s.session(&id).unwrap().unwrap().origin.as_deref(),
            Some("queue")
        );
    }

    /// A store written before the column gains the column and says so: the v11 arm is guarded like every
    /// other one, and what is worth asserting is the reading, not that the `ALTER` ran.
    #[test]
    fn an_older_store_gains_the_reading_and_its_rows_say_null() {
        let path = std::env::temp_dir().join(format!(
            "letibot-store-v11-{}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        let _ = std::fs::remove_file(&path);
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _clean = Clean(path.clone());

        let session;
        {
            let s = Store::open(&path).unwrap();
            session = a_session(&s);
            let mut d = a_decision("adj-before-v11", &session);
            d.consulted = Some(true);
            assert!(s.record_adjudication(&d).unwrap());
        }
        {
            // Back to a store that predates the column, the way one on disk from an older
            // daemon is.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("ALTER TABLE adjudication DROP COLUMN oracle_reading", [])
                .unwrap();
            c.execute("UPDATE schema_version SET version = 10", [])
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let v: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);

        // The column is there, the old row survived, and its reading is `NULL` - which is
        // what *"this row is older than the question"* looks like. It is still readable as
        // a row; what it cannot say is which of the four it was.
        let row = s.corpus(false, 10).unwrap().remove(0);
        assert_eq!(row.request_id, "adj-before-v11");
        assert_eq!(row.consulted, Some(true));
        assert_eq!(row.oracle_reading, None);

        // And a row written now can carry one, in the same store.
        let mut d = a_decision("adj-after-v11", &session);
        d.consulted = Some(true);
        d.oracle_reading = Some("out_of_room".into());
        assert!(s.record_adjudication(&d).unwrap());
        let rows = s.corpus(false, 10).unwrap();
        let fresh = rows
            .iter()
            .find(|r| r.request_id == "adj-after-v11")
            .unwrap();
        assert_eq!(fresh.oracle_reading.as_deref(), Some("out_of_room"));
    }

    /// The warm start is exactly as wide as the session that earned it: six rows, one per bound,
    /// and only the first may come back. Each other one is a way the cache could go wrong.
    #[test]
    fn a_warm_start_carries_only_what_the_operator_themselves_approved() {
        let s = store();
        let here = a_session(&s);

        // A second project, same store - a shape approved there is not approved here.
        let elsewhere = "s-corpus-elsewhere".to_string();
        s.put_session(&SessionRecord {
            id: elsewhere.clone(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: "/other".into(),
            owner: "op".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("session");

        let approved = |id: &str, session: &str, shape: &str| NewAdjudication {
            tier: "may_approve".into(),
            effect: "admit".into(),
            verdict_by: Some("human:op".into()),
            baseline: "ask — intents [inspect read_file]".into(),
            shape: Some(shape.into()),
            shape_class: Some("read,host_project,reversible,free".into()),
            ..a_decision(id, session)
        };

        // The one that counts.
        s.record_adjudication(&approved("a-keep", &here, "grep -n <arg> <arg>"))
            .expect("write");
        // The guard's own admit: never learned from.
        s.record_adjudication(&NewAdjudication {
            verdict_by: Some("model oracle `qwen`".into()),
            ..approved("a-model", &here, "sed -n <arg> <arg>")
        })
        .expect("write");
        // A refusal is not an approval.
        s.record_adjudication(&NewAdjudication {
            effect: "refuse".into(),
            ..approved("a-refused", &here, "curl <arg>")
        })
        .expect("write");
        // An always-ask a human admitted was admitted for that one call.
        s.record_adjudication(&NewAdjudication {
            tier: "always_ask".into(),
            ..approved("a-alwaysask", &here, "ssh <arg>")
        })
        .expect("write");
        // A shape holes its operands, so a destructive call is never settled by one.
        s.record_adjudication(&NewAdjudication {
            baseline: "ask — intents [destroy]".into(),
            ..approved("a-destroy", &here, "rm -rf <arg>")
        })
        .expect("write");
        // Approved by the same person, in a different project.
        s.record_adjudication(&approved("a-elsewhere", &elsewhere, "cat <arg>"))
            .expect("write");

        let got = s.approved_shapes("/tmp").expect("read");
        assert_eq!(
            got,
            vec![(
                "grep -n <arg> <arg>".to_string(),
                "read,host_project,reversible,free".to_string()
            )],
            "the warm start reached past one of its bounds"
        );

        // And the other project sees its own, not this one's.
        assert_eq!(
            s.approved_shapes("/other").expect("read"),
            vec![(
                "cat <arg>".to_string(),
                "read,host_project,reversible,free".to_string()
            )]
        );
    }

    /// A row written before the class column existed is not a usable approval: the
    /// shape alone does not say what it was approved at. Withheld rather than
    /// assumed, which costs one question and no more.
    #[test]
    fn a_shape_recorded_without_its_class_is_not_warm_started() {
        let s = store();
        let here = a_session(&s);
        s.record_adjudication(&NewAdjudication {
            tier: "may_approve".into(),
            effect: "admit".into(),
            verdict_by: Some("human:op".into()),
            baseline: "ask — intents [inspect]".into(),
            shape: Some("grep -n <arg> <arg>".into()),
            shape_class: None,
            ..a_decision("a-old", &here)
        })
        .expect("write");
        assert!(s.approved_shapes("/tmp").expect("read").is_empty());
    }

    /// The property the whole table exists for: the model's verdict and the
    /// operator's ruling are separate columns, and recording the ruling does not
    /// touch the verdict. A row where they differ is the training example.
    #[test]
    fn an_operator_ruling_never_overwrites_the_model_verdict() {
        let s = store();
        let sid = a_session(&s);

        s.record_adjudication(&NewAdjudication {
            shown: Some("<brief bytes>".into()),
            model_verdict: Some("authorised by model-oracle".into()),
            verdict: Some("allow".into()),
            verdict_by: Some("model-oracle".into()),
            p_allow: Some(0.87),
            oracle_ms: Some(310),
            asked: true,
            ..a_decision("req-1", &sid)
        })
        .expect("record");

        assert!(
            s.record_operator_ruling("req-1", "revoked", "no, not that one")
                .expect("rule")
        );

        let row = &s.corpus(false, 10).expect("corpus")[0];
        assert_eq!(
            row.model_verdict.as_deref(),
            Some("authorised by model-oracle")
        );
        assert_eq!(row.operator_kind.as_deref(), Some("revoked"));
        assert_eq!(row.operator_note.as_deref(), Some("no, not that one"));
        assert_eq!(row.shown.as_deref(), Some("<brief bytes>"));
        assert_eq!(row.p_allow, Some(0.87));
        assert!(row.asked);
        assert_eq!(row.corpus_version, CORPUS_VERSION);
        // The un-normalised input survives, so the row can be re-featurised when
        // layer A changes -- which is the reason for collecting it at all.
        assert_eq!(row.tool, "bash");
        assert!(row.arguments_json.contains("rm -rf"));
        // The latency is measured against the row's own decision time.
        assert!(row.operator_latency_ms.is_some_and(|ms| ms >= 0));
    }

    /// A decision with no oracle is still a corpus row. "Nobody asked a model"
    /// is a fact about the decision, not a reason to drop it -- those rows are
    /// what an unsure-then-human loop produces.
    #[test]
    fn a_human_only_decision_is_recorded_with_no_verdict() {
        let s = store();
        let sid = a_session(&s);

        s.record_adjudication(&NewAdjudication {
            asked: true,
            ..a_decision("req-2", &sid)
        })
        .expect("record");
        s.record_operator_ruling("req-2", "upheld", "correct to ask")
            .expect("rule");

        let row = &s.corpus(true, 10).expect("corpus")[0];
        assert!(row.shown.is_none());
        assert!(row.model_verdict.is_none());
        assert!(row.p_allow.is_none());
        assert!(row.asked);
        assert_eq!(row.operator_kind.as_deref(), Some("upheld"));
    }

    /// Writing twice for one request must not double-count, and must not erase a
    /// ruling that arrived between the two writes.
    #[test]
    fn a_repeated_write_neither_duplicates_nor_clobbers() {
        let s = store();
        let sid = a_session(&s);

        s.record_adjudication(&a_decision("req-3", &sid))
            .expect("first");
        s.record_operator_ruling("req-3", "granted", "yes")
            .expect("rule");
        s.record_adjudication(&a_decision("req-3", &sid))
            .expect("second");

        let rows = s.corpus(false, 10).expect("corpus");
        assert_eq!(rows.len(), 1, "one decision is one row");
        assert_eq!(rows[0].operator_kind.as_deref(), Some("granted"));

        // And a second ruling keeps the first: the session acted on the first.
        assert!(
            !s.record_operator_ruling("req-3", "revoked", "changed mind")
                .expect("again")
        );
        assert_eq!(
            s.corpus(false, 10).expect("corpus")[0]
                .operator_kind
                .as_deref(),
            Some("granted")
        );
    }

    /// Who decided and what it was measured against are independent counts. Reporting one number
    /// for both made a session at `always-ask` read as "412 decisions, 0 ruled on".
    #[test]
    fn counts_separate_who_decided_from_what_was_measured() {
        let s = store();
        let sid = a_session(&s);
        // `consulted` is the third axis and what `measured` reads. Here "the model said
        // something" and "an oracle was consulted" are one row shape, so the fixture sets both.
        let add = |id: &str, asked: bool, verdict: Option<&str>, rule: Option<&str>| {
            s.record_adjudication(&NewAdjudication {
                asked,
                model_verdict: verdict.map(str::to_string),
                verdict_by: verdict.map(|_| "model:test".to_string()),
                consulted: verdict.map(|_| true),
                ..a_decision(id, &sid)
            })
            .expect("record");
            if let Some(k) = rule {
                s.record_operator_ruling(id, k, "").expect("rule");
            }
        };

        // always-ask: the operator answered, no oracle existed.
        add("a1", true, None, None);
        add("a2", true, None, None);
        // automode: an oracle decided, nobody was asked.
        add("m1", false, Some("admit by model:test"), None);
        // supervised: both, and they agreed.
        add("s1", true, Some("admit by model:test"), Some("upheld"));
        // supervised: both, and they did not.
        add("s2", true, Some("admit by model:test"), Some("revoked"));
        // The row that caught the tautology: a mode admitted it, nothing was
        // asked, and `model_verdict` is still Some because `corpus()` fills it from
        // whatever decided. It must NOT count as measured.
        s.record_adjudication(&NewAdjudication {
            asked: false,
            model_verdict: Some("selected by gate:mode: the mode admits writes".into()),
            verdict_by: Some("gate:mode".into()),
            ..a_decision("m2", &sid)
        })
        .expect("record");
        // a rule settled it, nobody asked and no model was reachable.
        add("r1", false, None, None);

        let c = s.corpus_counts().expect("counts");
        assert_eq!(c.total, 7);
        assert_eq!(
            c.decided_by_operator, 4,
            "a1 a2 s1 s2 — every call a person answered"
        );
        assert_eq!(
            c.measured, 3,
            "m1 s1 s2 only — NOT m2, whose model_verdict was written by the gate"
        );
        assert_eq!(
            c.disagreements, 1,
            "s2 alone — an `upheld` is the operator AGREEING, and counting it here is the \
             defect this predicate was fixed for (R11: 1863 reported against a true 1294)"
        );
        // The count that speaks for rows written before v10. Every row here carries
        // `verdict_by: model:test`, so "a model decided" and "an oracle was consulted" agree.
        assert_eq!(c.model_decided, 3, "m1 s1 s2: a model decided these");
        // The two always-ask rows are in neither `measured` nor `disagreements` and
        // are still the primary dataset: input → the operator's decision.
        assert_eq!(
            c.total - c.decided_by_operator,
            3,
            "m1, m2 and r1: nobody was asked"
        );
    }

    /// A store written before this table existed is carried forward, not rebuilt.
    #[test]
    fn a_v4_store_migrates_and_accepts_rows() {
        let s = store();
        s.connection()
            .execute_batch("DROP TABLE adjudication; DELETE FROM schema_version;")
            .expect("unwind to v4");
        s.connection()
            .execute("INSERT INTO schema_version (version) VALUES (4)", [])
            .expect("stamp v4");
        // `None` for the path: this store was opened in memory, and what the test is
        // re-running is the MIGRATION, which does not care where the file is.
        let s = s.remigrate().expect("migrate");

        let sid = a_session(&s);
        s.record_adjudication(&a_decision("after-migration", &sid))
            .expect("record");
        assert_eq!(s.corpus_counts().expect("counts").total, 1);
    }
}

#[cfg(test)]
mod prefix_id_tests {
    use super::*;

    fn rec(vocab_source: &str) -> StablePrefixRecord {
        StablePrefixRecord {
            dialect_sha: "d".into(),
            system: "sys".into(),
            tools_json: vec!["t1".into(), "t2".into()],
            tokens: vec![1, 2, 3],
            h_init: [0; 32],
            vocab_source: vocab_source.into(),
        }
    }

    /// Every id written before the byte vocabulary is the id it always was: a GGUF
    /// row hashes exactly what the old formula hashed, recomputed here by hand.
    #[test]
    fn a_gguf_rows_id_is_unchanged_and_a_byte_rows_is_its_own() {
        let mut h = Sha256::new();
        for part in ["d", "sys", "t1", "t2"] {
            h.update(part.as_bytes());
            h.update([0]);
        }
        let old_formula = crate::ledger::hex(&h.finalize().into());
        assert_eq!(rec("/m/Qwen.gguf").id(), old_formula);
        assert_eq!(rec("").id(), old_formula);
        assert_ne!(rec("bytes:aaaaaaaaaaaa").id(), old_formula);
        assert_ne!(
            rec("bytes:aaaaaaaaaaaa").id(),
            rec("bytes:bbbbbbbbbbbb").id()
        );
    }
}
