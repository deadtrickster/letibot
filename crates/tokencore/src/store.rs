//! The SQLite store for the transcript and its ledger rows.
//!
//! §4.4 names the tables and says what each holds, in prose. It does not give
//! columns, and `docs/workstreams.md` §7 item 11 flags that: *"whoever writes W5
//! writes the schema; anyone else who touches it will guess columns."* This
//! module is the schema, and this comment is the reasoning.
//!
//! # Scope: four tables, not eight
//!
//! §4.4 lists eight persisted tables. Four of them belong to this strand --
//! `stable_prefix`, `session`, `transcript`, `transcript_item` -- and four
//! belong to strands that are not written yet: `event` (W7), `turn_metrics`
//! (W6), `adjudication_request` / `adjudication_decision` (W11), `tool_spill`
//! (W9), `segment` (W13). Those are **deliberately absent**. Inventing their
//! columns here would be the exact failure the workstreams document warns about,
//! one strand guessing another's contract, except worse, because a guess that is
//! already in a migration is harder to correct than one that is not. They arrive
//! as `migrate()` steps 2, 3, ... written by the people who know what goes in
//! them.
//!
//! # The decisions worth arguing about
//!
//! **The tokens are stored.** `transcript_item.tokens` holds the item's token
//! ids as a blob, and it is not redundant with the rendered text. §4.3 clause 6
//! says a restart rebuilds the region *by replaying the ledger's spans*, not by
//! re-rendering, and that is only possible if the ids are durable. Storing only
//! the text and re-tokenizing on restart would reintroduce the renderer
//! non-determinism the hash chain exists to detect, during recovery, when nobody
//! is watching. The cost is 4 bytes per token, about 800 KB for a 200 k-token
//! conversation, which is nothing next to the failure it prevents.
//!
//! **Per item, not one blob per transcript.** An append is one row insert; a
//! restart is one ordered scan. The ledger row and the tokens it describes are
//! written in the *same statement*, so they cannot disagree -- which is the one
//! way `restore` could be fed a lie it is unable to detect.
//!
//! **The stable prefix is content-addressed on `(dialect_sha, system, tools)`,
//! not on `(system, tools)`.** The tokens in that row were produced by a
//! particular renderer. Two dialects rendering the same system text and the same
//! tool schemas produce different tokens, so keying on the text alone would let
//! a GLM session inherit a Qwen session's prefix tokens and be structurally
//! unable to notice. §4.4 says the dialect id *is* the `template_sha`; this puts
//! it where it changes an answer.
//!
//! **`transcript.forked_at_seq` is mine and is not in the plan.** §5.5 says a
//! truncation produces a new transcript with a `parent_transcript_id`, but
//! nothing records *where* the fork happened. Without it a fork is
//! indistinguishable from a fresh transcript that happens to name a parent, and
//! the question "which items did these two share?" -- which is the question the
//! prefix cache is answering -- can only be recovered by comparing token blobs.
//! The `CHECK` makes the two fields live or die together.
//!
//! **Append-only is enforced in SQL, not only in Rust.** Three triggers:
//! `UPDATE` and `DELETE` on `transcript_item` `RAISE(ABORT)`, and an insert
//! trigger refuses any row whose `seq` is not the previous `seq + 1` or whose
//! `tok_offset` is not the previous `tok_offset + tok_len`. The last one is the
//! prefix invariant written as a constraint: a hole, an overlap, a reordering or
//! a re-insert at an old index is rejected by the database, whatever the caller
//! believed it was doing. `crate::ledger`'s type makes a rewrite blocked
//! in this process; the triggers make it blocked in `sqlite3` on the
//! command line too.
//!
//! **WAL, `synchronous = FULL`.** §4.4 says WAL. `FULL` rather than `NORMAL`
//! because the durable copy of the tokens is the *only* copy -- the region is
//! volatile by design -- so a lost commit is a lost turn, not a re-derivable
//! cache.
//!
//! # Times
//!
//! `created_at` is milliseconds since the Unix epoch, `INTEGER`. Not a string:
//! this column is only ever ordered and differenced.

use std::path::Path;

use letibot_transcript::TranscriptItem;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::ledger::LedgerRow;
use crate::vocab::TokenId;

/// Why `session.role` is nullable, and why the comment is here and not in the DDL.
///
/// A role belongs to the SESSION and not to the process. The runtime was already
/// shaped for that: `Sessions` holds one `Harness` per session and a `ToolRuntime`
/// is per-`Harness`, so two sessions in one daemon can already carry different tool
/// sets — they all resolved from the same `--role` flag only because there was
/// nowhere to record anything else. `stable_prefix` has keyed on `tools_json` since
/// v1, so two roles in one daemon get two content-addressed prefix rows with no
/// coordination needed.
///
/// **NULL is not `coder`.** It means *unrecorded*, which is what every session
/// written before v2 is, and it has to keep meaning "the daemon's own role" or a
/// resume would silently re-seat an old conversation.
///
/// The prose lives in Rust because SQLite's `ALTER TABLE ... DROP COLUMN` rewrites
/// the stored `CREATE TABLE` text and fails with `incomplete input` when a
/// multi-line comment sits inside the parens. That is not hypothetical: it broke
/// the migration test's fixture, which builds a v1 store by dropping this column.
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
    /// **Whether an oracle was consulted** (R11, v10). `None` on rows written before the
    /// column existed — "not recorded" — which is a different statement from
    /// `Some(false)`, "recorded, and nobody asked a model".
    pub consulted: Option<bool>,
    /// **Which of the four `Unsure`s the answer was** (R12, v11), verbatim as stored.
    /// `None` means either an answered verdict or a row from before the column — the first
    /// is `consulted = 1` with a `verdict_basis` that is not an unsure sentence, and the
    /// second says so in `corpus_version`. Read with
    /// [`letibot_tools::authorise::UnsureKind::parse`], which refuses a token this build
    /// does not know rather than guessing.
    pub oracle_reading: Option<String>,
    /// **What the oracle answered, verbatim** (R11, v9). `None` means either that
    /// no oracle was consulted — `verdict_by` says which — or that one was asked and
    /// nothing came back inside its budget.
    pub reply: Option<String>,
    pub effect: String,
    pub asked: bool,
    pub operator_kind: Option<String>,
    pub operator_note: Option<String>,
    pub operator_latency_ms: Option<i64>,
    pub corpus_version: i64,
}

/// **The three predicates every count of the corpus is built from, in one place.**
///
/// A `WHERE` clause copied into a second file is a count that drifts, and these did:
/// `harnessd/src/decision_source.rs` kept its own copies and two of them measured
/// something other than what they were named after — `measured` was
/// `oracle_ms IS NOT NULL`, which matches **every** row because `oracle_ms` is `0` and
/// never NULL, and `disagreements` was `operator_kind IS NOT NULL`, which counts a
/// ruling the operator *agreed* with. Measured on this box 2026-09-21: the scoped count
/// read `1863` ruled-against where the true figure was `1294`.
///
/// The store's own comments already say the thing that generalises: **a count is not
/// measuring what its NAME says, it is measuring what its PREDICATE says.** So the
/// predicate is written once, next to the schema it reads, and both callers interpolate
/// it.
pub const ASKED_SQL: &str = "asked = 1";
/// An oracle **actually spoke**. Not "a model adjudicator was in the chain" and not "a
/// model decided" — the two are different facts and both were counted as this one.
/// `consulted` is written from `ModelAdvice::consulted`, which is the flag the whole
/// distinction exists for.
pub const MEASURED_SQL: &str = "consulted = 1";
/// **The rows a fine-tune is for**: the operator ruled *against* what the model would
/// have done. `granted` and `revoked` only — `upheld` is the operator agreeing, and
/// counting it here made the number mean "ruled on" while wearing the name of "differ".
pub const DISAGREEMENT_SQL: &str = "operator_kind IN ('granted', 'revoked')";
/// **A model decided the call itself**, which is neither of the two above and had no
/// name before this: `automode` lets the oracle's own verdict admit, and at `/supervised`
/// the oracle only advises while a person decides.
pub const MODEL_DECIDED_SQL: &str = "verdict_by LIKE 'model%'";

/// What the corpus holds, for the disclosure and for `/gate corpus`.
///
/// Four numbers rather than three, because the earlier three answered the wrong
/// question. `labelled` counted `operator_kind IS NOT NULL`, which only fills when
/// there is a model verdict to agree or disagree with — so a session at `always-ask`
/// where the operator personally answered four hundred calls read as
/// *"412 decisions, 0 ruled on"*. Every one of those was a decision a human made.
///
/// The split that matters is **who decided** against **what it was measured against**,
/// and they are independent:
///
/// | | |
/// |---|---|
/// | `decided_by_operator` | a person answered THIS call. The primary dataset: input → decision |
/// | `measured` | an oracle also gave a verdict on it. What calibration needs |
/// | `disagreements` | the two differ. What a fine-tune is for |
///
/// A row can be in all three, in `decided_by_operator` alone (`always-ask`, no
/// oracle), or in `measured` alone (`automode`, nobody watching).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CorpusCounts {
    pub total: u64,
    /// `asked = 1`: a person was put in front of this call and answered it.
    pub decided_by_operator: u64,
    /// **An oracle was actually consulted** — [`MEASURED_SQL`], from
    /// `ModelAdvice::consulted`, which is what that flag is for. **`NULL` before schema
    /// v10**, so rows written earlier read `0` here and `model_decided` below is the
    /// count that speaks for them; a reader who sees the two apart is seeing the flag's
    /// arrival date, which is a fact and not a defect.
    pub measured: u64,
    /// The operator ruled against what the model would have done — [`DISAGREEMENT_SQL`].
    pub disagreements: u64,
    /// A model decided the call — [`MODEL_DECIDED_SQL`]. The one count that is exact on
    /// rows written before v10.
    pub model_decided: u64,
}

/// The write shape for one decision. A struct rather than an argument list
/// because there are twenty of them and two adjacent `Option<String>`s passed
/// positionally is a defect waiting for the day somebody adds a twenty-first.
///
/// Separate from [`StoredAdjudication`] for [`SessionRecord`]'s reason: the read
/// shape carries what the row became, including the operator's ruling, which by
/// construction cannot exist yet at the moment this one is built.
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
    /// **Whether an oracle was actually consulted** (R11), from
    /// `ModelAdvice::consulted` — the flag that separates *the oracle said ask* from
    /// *nobody asked a model*. Without it the only way to ask was a regex over
    /// `model_verdict`'s prose.
    pub consulted: Option<bool>,
    /// **Which of the four `Unsure`s the answer was** (R12), or `None` for an answered
    /// verdict. `CorpusRow::oracle_reading`, which is `UnsureKind::as_str`.
    pub oracle_reading: Option<String>,
    /// **What the oracle answered, verbatim** (R11). `None` when none was consulted,
    /// or when the one that was did not answer inside its budget — `verdict_by`
    /// tells those apart, and these are the bytes the verdict was read out of.
    pub reply: Option<String>,
    pub effect: String,
    /// Whether the operator was actually put in front of this decision.
    pub asked: bool,
    /// The command's shape — the parse with its literals holed. `None` for a
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
    /// row — so a recomputed shape is filed under the project it was approved in.
    pub workspace_root: String,
    pub tool: String,
    pub arguments_json: String,
}

/// **11** since the corpus records **which of the four `Unsure`s** an oracle's answer was
/// (R12) — `oracle_reading`, additive, described at its migration arm below. **10** since it
/// records whether an oracle was consulted and what it answered (R11), and **9** added
/// `oracle_reply` for the same requirement.
pub const SCHEMA_VERSION: i64 = 11;

/// **What this row's columns mean.** Stamped on every corpus row.
///
/// Distinct from [`SCHEMA_VERSION`], which says what the *file* holds. A column
/// can keep its name and its type and change what it records — a `p_allow` that
/// switched from a raw softmax to a calibrated one is the same schema and a
/// different dataset. A trainer that pools two meanings under one name produces a
/// model fitted to the seam.
pub const CORPUS_VERSION: i64 = 1;

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
    context_cached INTEGER           -- v8; the last turn's cached_tokens, for the
                                     -- cache %. NULL with context_tokens.
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

#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    /// A blob whose byte length is not a whole number of token ids, or a hash
    /// that is not 32 bytes. Means the file was written by something else.
    Corrupt(String),
    /// The store is fine and the operation is not allowed. Separate from
    /// [`StoreError::Corrupt`] because they send a reader to opposite places: one is
    /// "your database is damaged", the other is "the database is doing its job".
    /// Printing a refused delete as corruption is how an operator ends up running
    /// `PRAGMA integrity_check` on a healthy file.
    Refused(String),
    NotFound(String),
    /// The file was written by a newer build.
    SchemaTooNew {
        found: i64,
        known: i64,
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
            StoreError::SchemaTooNew { found, known } => write!(
                f,
                "store schema is version {found}; this build knows {known}"
            ),
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
        TranscriptItem::SegmentMark { .. } => "segment_mark",
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
    /// The content address: `H(dialect_sha || 0 || system || 0 || tool || 0 ...)`.
    ///
    /// The dialect is in the address because the *tokens* are what this row
    /// stores, and two dialects render the same text to different tokens.
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

/// A session as the store holds it: enough to list it, pick it and resume it.
///
/// Separate from [`SessionRecord`], which is the *write* shape. This one carries
/// three things that are computed rather than stored — the transcript id, the row
/// count and the last activity — because every caller that lists sessions needs all
/// three and would otherwise write the same three subqueries slightly differently.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The last turn's prompt tokens, or `None` before a turn has finished (or on
    /// a row that predates the column). A head that attaches after a restart shows
    /// the context from this rather than waiting for a turn.
    pub context_tokens: Option<u64>,
    /// The last turn's cached tokens, for the cache %. `None` with `context_tokens`.
    pub context_cached: Option<u64>,
}

/// One line of a session's todo list, as the model wrote it.
///
/// Stored as mutable session metadata — the `set_title` class, not the
/// append-only class: a todo list the model revised three times is one list with
/// a history nobody asked to keep. The whole list is replaced on every write,
/// because a delta the model got wrong is a delta nobody can audit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// A todo's state. Serde as the lower-case words, so a stored list reads the
/// same in `sqlite3` as it does here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
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

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // The region is volatile by design, so this file is the only copy of the
        // tokens. A lost commit is a lost turn.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        // WAL lets readers and the writer run at once, but two *writers* still
        // serialise, and rusqlite's default is to fail immediately rather than wait.
        // Two connections exist by design — the worker writes rows, a second one
        // answers a head's list and writes a title — so a rename landing during a
        // turn's `append_item` must wait a moment rather than come back as
        // SQLITE_BUSY on a database that is working exactly as intended.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let found: Option<i64> = self
            .conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .optional()
            .unwrap_or(None);
        match found {
            Some(v) if v > SCHEMA_VERSION => {
                return Err(StoreError::SchemaTooNew {
                    found: v,
                    known: SCHEMA_VERSION,
                });
            }
            // An older store is MIGRATED, not left alone and not rebuilt. Before v2
            // this arm was `Some(_) => return Ok(())`, so `SCHEMA_SQL` only ever ran
            // on an empty file and there was no way to add a column to a store that
            // already held conversations. The rows are append-only and cannot be
            // recreated, so a migration is the only shape this can take.
            Some(v) if v < SCHEMA_VERSION => {
                self.migrate_from(v)?;
                self.conn.execute("DELETE FROM schema_version", [])?;
                self.conn.execute(
                    "INSERT INTO schema_version (version) VALUES (?1)",
                    params![SCHEMA_VERSION],
                )?;
                return Ok(());
            }
            Some(_) => return Ok(()),
            None => {}
        }
        self.conn.execute_batch(SCHEMA_SQL)?;
        self.conn.execute("DELETE FROM schema_version", [])?;
        self.conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )?;
        Ok(())
    }

    /// Apply the steps from `from` up to [`SCHEMA_VERSION`].
    ///
    /// One arm per version, each idempotent on its own, so a store two versions
    /// behind is carried forward by running both rather than by a special case.
    ///
    /// **A step may add and it may not destroy.** `transcript_item` carries a
    /// `BEFORE DELETE` trigger that raises, so a migration that wanted to rewrite
    /// rows could not, and that is the guarantee rather than a convention here.
    fn migrate_from(&self, from: i64) -> Result<()> {
        if from < 2 {
            // v2: a session records its own role. `ALTER TABLE ... ADD COLUMN` with
            // no default writes NULL into every existing row, which is exactly the
            // "unrecorded, use the daemon default" case the column documents.
            self.conn
                .execute_batch("ALTER TABLE session ADD COLUMN role TEXT")?;
        }
        if from < 3 {
            // v3: the session todo list. A migrated store never runs `SCHEMA_SQL`
            // — `migrate` returns early — so the table is created here, and
            // `IF NOT EXISTS` keeps a store that somehow already has one honest.
            self.conn.execute_batch(
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
            self.conn
                .execute_batch("ALTER TABLE session ADD COLUMN parent_session_id TEXT")?;
        }
        if from < 5 {
            // v5: the adjudication corpus. Created here for a migrated store,
            // which never runs SCHEMA_SQL.
            self.conn.execute_batch(
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
            // v6: the SHAPE of the command a decision was about — the parse with its
            // literals replaced by holes, so `grep -n P -A 22 a.rs` and `grep -n Q
            // -A 3 b.rs` are one row and two calls. Collected before anything is
            // built on it: the operator's question was whether the same question is
            // being asked repeatedly, and that is a thing to count rather than
            // assume. NULL on every existing row, which is "recorded before this
            // column existed" and not "no shape".
            // **Idempotent, because a migration is not always run on a file that
            // has never seen this column.** The v1/v2 fixtures build a CURRENT
            // store, reverse two steps by hand and set the version back — so the
            // table already carries `shape` and the plain `ALTER` fails with
            // "duplicate column name". Asking the file what it has is cheap and is
            // the only thing that is true for both paths.
            let has: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('adjudication') WHERE name = 'shape'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn
                    .execute_batch("ALTER TABLE adjudication ADD COLUMN shape TEXT")?;
            }
        }
        if from < 7 {
            // v7: the effect class a shape was approved at, so the shape cache can
            // survive a restart without widening. Same idempotence argument as v6.
            let has: bool = self
                .conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('adjudication') WHERE name = 'shape_class'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn
                    .execute_batch("ALTER TABLE adjudication ADD COLUMN shape_class TEXT")?;
            }
        }
        if from < 8 {
            // v8: the last turn's context, on the session row. A head that attaches
            // after a restart must be able to say how big the prompt is without
            // waiting for a turn; the in-memory view loses it, so the row keeps it.
            // NULL in every existing row: "no turn has finished since the column
            // existed", which a head renders as no number rather than zero.
            let has: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('session') WHERE name = 'context_tokens'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn.execute_batch(
                    "ALTER TABLE session ADD COLUMN context_tokens INTEGER;
                     ALTER TABLE session ADD COLUMN context_cached INTEGER;",
                )?;
                // **Backfill the column for the sessions that already exist.** A
                // store being upgraded has conversations whose last turn finished
                // before the column existed, so their row reads `NULL` — "no turn
                // has finished" — which is false and would leave an attaching head
                // with no context to show until the next turn. The prompt the next
                // round would send is the stable prefix plus the current
                // transcript's items, and both are on disk: the prefix's own
                // `n_tokens` and each item's `tok_len`. That sum is the context,
                // measured from the same rows the encoder reads, and the next
                // turn's real `prompt_tokens` replaces it.
                //
                // Only the current transcript, by the same `created_at DESC, rowid
                // DESC` rule `list_sessions` uses: a compaction fork is written in
                // the same millisecond as its parent, and counting both would put
                // the history a compaction stopped carrying back into the number.
                // `context_cached` is left `NULL`: the cache fraction of a prompt
                // that was never sent is not a measurement, and a head renders
                // `NULL` as no percentage rather than a zero.
                // The `EXISTS` is load-bearing, not belt-and-braces: an aggregate
                // without a GROUP BY returns one row even over an empty input, so
                // the subquery alone would answer `0` for a session with no
                // transcript and a head would show `0 ctx` for a conversation that
                // never had one. The gate keeps such a row `NULL` — "no turn has
                // finished" — which is the true state.
                self.conn.execute(
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
            // v9: **what the oracle answered, verbatim** (R11). `shown` was meant to be
            // the other half and held the operator's card instead; the brief now goes
            // where that column's own doc says it goes, so what needed a column was the
            // reply.
            //
            // NULL on every existing row: "recorded before this column existed", and
            // indistinguishable from "the oracle did not answer" — which `verdict_by`
            // separates, because that names whether an oracle was consulted at all.
            let has: bool = self
                .conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('adjudication') \
                     WHERE name = 'oracle_reply'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn
                    .execute_batch("ALTER TABLE adjudication ADD COLUMN oracle_reply TEXT")?;
            }
        }
        if from < 10 {
            // v10: **whether an oracle was consulted** (R11), as a column rather than a
            // reading of `model_verdict`'s prose. NULL-able on purpose: a row written
            // before this cannot say, and "we did not keep it" must not look like "no
            // model was asked". `CorpusCounts::measured` counts `= 1`, so an old store
            // reports 0 there and `model_decided` is the count that speaks for its rows.
            let has: bool = self
                .conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('adjudication') \
                     WHERE name = 'consulted'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn
                    .execute_batch("ALTER TABLE adjudication ADD COLUMN consulted INTEGER")?;
            }
        }
        if from < 11 {
            // v11: **which of the four `Unsure`s** (R12), as a token. NULL-able and NULL on
            // everything written before it, which is the honest reading: those rows recorded
            // the reason in a sentence and not in a column, and the two are not
            // interchangeable — that is the whole finding. A row with `consulted = 1` and no
            // `oracle_reading` is an answered verdict or an old row, and
            // `verdict_basis`/`model_verdict` is where a reader of the old ones goes.
            let has: bool = self
                .conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('adjudication') \
                     WHERE name = 'oracle_reading'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn.execute_batch(
                    "ALTER TABLE adjudication ADD COLUMN oracle_reading TEXT",
                )?;
            }
        }
        Ok(())
    }

    /// Insert a stable prefix, or return the id of the identical one already
    /// there. Idempotent because the id is the content.
    pub fn put_stable_prefix(&self, rec: &StablePrefixRecord) -> Result<String> {
        let id = rec.id();
        self.conn.execute(
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
        // **A row written with no tokens is repairable, and only that row is.**
        //
        // `INSERT OR IGNORE` is right for a content-addressed table: the id is the
        // hash of the system text and the tool schemas, so a second write of the
        // same id is the same prefix and there is nothing to change. It was NOT
        // right for one case, and that case happened: a caller that wrote the row
        // with an empty token list and a zero `h_init` produced a prefix a resume
        // verifies as a broken chain at row 0, and `IGNORE` meant no later, correct
        // write could ever fix it.
        //
        // So an existing row with `n_tokens = 0` is filled in. Nothing else is ever
        // overwritten: an id whose row already has tokens describes a prefix that
        // has been spoken, and rewriting that would change the history of every
        // transcript hanging off it.
        if !rec.tokens.is_empty() {
            self.conn.execute(
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
        let (blob, hash): (Vec<u8>, Vec<u8>) = self
            .conn
            .query_row(
                "SELECT tokens, h_init FROM stable_prefix WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("stable_prefix {id}")))?;
        Ok((blob_to_tokens(&blob)?, blob_to_hash(&hash)?))
    }

    /// The whole stable-prefix row, for a caller rebuilding the prefix a session
    /// was actually created with.
    ///
    /// A compaction fork keeps the session's own prefix rather than the one the
    /// running daemon would render now — the same rule a resume follows — and
    /// building that fork needs the record's `system` and `tools_json`, which the
    /// token-and-hash getter above does not carry.
    pub fn stable_prefix_record(&self, id: &str) -> Result<StablePrefixRecord> {
        let (dialect_sha, system, tools_json, tokens, h_init, vocab_source): (
            String,
            String,
            String,
            Vec<u8>,
            Vec<u8>,
            String,
        ) = self
            .conn
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
        self.conn.execute(
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

    /// A transcript that came from a fork (§5.5).
    ///
    /// Separate entry point rather than an `Option` argument, so that recording
    /// the divergence is not something a caller can forget by passing `None`.
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

    fn insert_transcript(
        &self,
        transcript_id: &str,
        session_id: &str,
        stable_prefix_id: &str,
        fork: Option<(&str, u32)>,
    ) -> Result<()> {
        self.conn.execute(
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

    /// Append one item and its ledger row, together.
    ///
    /// One statement, so the row and the tokens it describes cannot end up
    /// disagreeing; and the insert trigger refuses it if `seq` or `tok_offset`
    /// does not continue the previous row.
    pub fn append_item(
        &self,
        transcript_id: &str,
        seq: u32,
        item: &TranscriptItem,
        row: &LedgerRow,
        tokens: &[TokenId],
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO transcript_item
               (transcript_id, seq, item_id, kind, item_json,
                tok_offset, tok_len, h_k, tokens, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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

    /// Everything needed to rebuild the ledger.
    pub fn load_transcript(&self, transcript_id: &str) -> Result<LoadedTranscript> {
        let (session_id, parent, forked_at, prefix_id): (
            String,
            Option<String>,
            Option<i64>,
            String,
        ) = self
            .conn
            .query_row(
                "SELECT session_id, parent_transcript_id, forked_at_seq, stable_prefix_id
                       FROM transcript WHERE id = ?1",
                params![transcript_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("transcript {transcript_id}")))?;

        let (prefix_tokens, h_init) = self.get_stable_prefix(&prefix_id)?;

        let mut stmt = self.conn.prepare(
            "SELECT item_id, item_json, tok_offset, tok_len, h_k, tokens
               FROM transcript_item WHERE transcript_id = ?1 ORDER BY seq ASC",
        )?;
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
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM transcript_item WHERE transcript_id = ?1",
            params![transcript_id],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    /// **One row's JSON, by ordinal — the read behind `FetchRow`** (R19.2b).
    ///
    /// One indexed lookup: `transcript_item`'s primary key is
    /// `(transcript_id, seq)`, so this is not a scan and not a transcript load. That
    /// distinction is the whole point of the frame — the daemon's *view* is bounded
    /// (2,000 rows, 8 MB of bodies), so an ordinal it trimmed was previously
    /// unanswerable, and the two ways to answer it are `load_transcript` (every row
    /// of the session, to return one) or this.
    ///
    /// `None` for an ordinal this transcript does not have. **Not an empty string**: a
    /// row the store does not hold and a row whose body is empty must not look alike,
    /// which is the same rule `RowFetched`'s own doc states for the wire.
    pub fn row_json_at(&self, transcript_id: &str, seq: u32) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT item_json FROM transcript_item\n                   WHERE transcript_id = ?1 AND seq = ?2",
                params![transcript_id, seq as i64],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// **The session's current transcript id**, which is what a row ordinal is
    /// relative to.
    ///
    /// The **newest** transcript, because a fork replaces the conversation: after a
    /// compaction, ordinal 3 is a row of the new base and not of the history it
    /// summarised, so reading the old transcript's row 3 would answer with a row
    /// nobody asked about. Ordered the same way `list_sessions` orders its subquery —
    /// `created_at DESC, rowid DESC`, the rowid because a fork is written in the same
    /// millisecond as the row that caused it.
    pub fn current_transcript_id(&self, session_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM transcript WHERE session_id = ?1\n                   ORDER BY created_at DESC, rowid DESC LIMIT 1",
                params![session_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Every session in the store, newest activity first: what a picker or
    /// `letibot --sessions` is drawn from.
    ///
    /// Ordered by the **last row written**, not by `session.created_at`. "Which one
    /// was I just in" is the question this list is asked, and a session created on
    /// Monday and used ten minutes ago is the answer to it; ordering by creation
    /// puts it at the bottom.
    ///
    /// `LEFT JOIN`, so a session whose transcript row exists but holds nothing still
    /// appears — with `items: 0` and `last_activity_ms` falling back to its creation
    /// time. Those are exactly the sessions somebody wants to delete, and a list that
    /// hides them is a list that cannot be acted on.
    pub fn list_sessions(&self) -> Result<Vec<StoredSession>> {
        // The "current transcript" subquery orders by `created_at DESC, rowid
        // DESC`, and the rowid is not decoration: a compaction fork is written in
        // the same millisecond as the transcript it forked from, and a
        // created-at-only order breaks that tie arbitrarily — a resume could come
        // back on the parent, putting the full history behind the next prompt.
        // Insertion order is what "newest" means here.
        //
        // The item count is the **current transcript's** rows, not every
        // transcript's: since forks exist, a cross-transcript count double-counts
        // the history a compaction just stopped carrying, and a picker row that
        // grew because the conversation got shorter is a lie.
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.title, s.model_id, s.dialect_sha, s.workspace_root, s.owner,
                    s.created_at,
                    (SELECT t.id FROM transcript t
                      WHERE t.session_id = s.id
                      ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1),
                    (SELECT COUNT(*) FROM transcript_item i
                      WHERE i.transcript_id = (SELECT t.id FROM transcript t
                          WHERE t.session_id = s.id
                          ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1)),
                     (SELECT MAX(i.created_at) FROM transcript_item i
                       JOIN transcript t ON t.id = i.transcript_id
                      WHERE t.session_id = s.id),
                    s.role,
                    s.parent_session_id,
                    s.context_tokens,
                    s.context_cached
               FROM session s",
        )?;
        let mut out: Vec<StoredSession> = stmt
            .query_map([], |r| {
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
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        out.sort_by_key(|s| std::cmp::Reverse(s.last_activity_ms));
        Ok(out)
    }

    /// The name on disk right now, or `None` for a session nobody has named.
    ///
    /// Read rather than remembered: a title can be set by a head through the
    /// registry's own connection while a harness holds an older idea of it, and a
    /// derivation that trusted its cached copy would overwrite a name the operator
    /// had just typed.
    pub fn title(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT title FROM session WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .filter(|t: &String| !t.is_empty()))
    }

    /// One session, or `None`.
    pub fn session(&self, id: &str) -> Result<Option<StoredSession>> {
        Ok(self.list_sessions()?.into_iter().find(|s| s.id == id))
    }

    /// Name a session, or clear its name with an empty string.
    ///
    /// `session` carries no append-only trigger and is not part of the chain: a
    /// title is metadata *about* a conversation and changing it changes no prompt
    /// byte. That is the whole reason renaming is allowed here while
    /// [`Store::append_item`]'s rows cannot be touched at all.
    pub fn set_title(&self, id: &str, title: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE session SET title = ?2 WHERE id = ?1",
            params![id, (!title.is_empty()).then(|| title.to_string())],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// Record the last turn's context on the session row, so a head that attaches
    /// after a restart can show how big the prompt is without waiting for a turn.
    /// `None` clears it (a session that has run no turn since the column existed).
    pub fn set_context(&self, id: &str, tokens: Option<u64>, cached: Option<u64>) -> Result<()> {
        // `i64` on the wire: rusqlite's `ToSql` has no `u64`, and a token count
        // that does not fit an `i64` is not a prompt this box will ever send.
        let n = self.conn.execute(
            "UPDATE session SET context_tokens = ?2, context_cached = ?3 WHERE id = ?1",
            params![id, tokens.map(|t| t as i64), cached.map(|c| c as i64)],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// The session's todo list, in the order the model last wrote it.
    ///
    /// Empty when the session has none — "never wrote one" and "cleared it" are
    /// the same state from the outside, and pretending they differ is a pane
    /// showing a distinction nothing stands behind.
    pub fn todos(&self, session_id: &str) -> Result<Vec<TodoItem>> {
        let json: Option<String> = self
            .conn
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

    /// Replace the session's todo list wholesale. The whole list every time —
    /// there is no append, no reorder-by-id, no delta: the model writes what the
    /// list now is, and that is what the store holds.
    pub fn put_todos(&self, session_id: &str, todos: &[TodoItem]) -> Result<()> {
        self.conn.execute(
            "INSERT INTO todo (session_id, todos_json, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET todos_json = ?2, updated_ms = ?3",
            params![session_id, serde_json::to_string(todos)?, now_ms()],
        )?;
        Ok(())
    }

    /// Remove a session that holds no transcript rows.
    ///
    /// **A session with rows cannot be deleted, and that is not an omission.**
    /// `transcript_item` carries a `BEFORE DELETE` trigger that raises — §4.1's
    /// "immutable, with exactly one mutation: append", said in SQL — so there is no
    /// statement this method could run that would remove a conversation. Offering a
    /// `--delete` that quietly left the rows behind and dropped only the `session`
    /// row would be worse than refusing: the tokens would still be on disk, orphaned
    /// and unreachable, and the operator would believe they were gone.
    ///
    /// What *is* deletable is a session nothing ever ran in — the row a daemon
    /// writes at startup and a `/new` that was never used. Those accumulate, they
    /// are the ones a picker is cluttered by, and removing them destroys nothing.
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
        // `foreign_keys` is on.
        self.conn
            .execute("DELETE FROM transcript WHERE session_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM session WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// What a stable prefix was rendered from and by. `None` if it is not here.
    pub fn stable_prefix_meta(&self, id: &str) -> Result<Option<StablePrefixMeta>> {
        Ok(self
            .conn
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

    /// Escape hatch for the tests below and for `EXPLAIN`. Read-only by
    /// convention only, which is why the append-only guarantees live in triggers
    /// rather than in whoever holds this reference.
    // ----------------------------------------------------------- adjudication

    /// Record one decision. Called when the gate acts, not when the operator
    /// answers — the two are separate events and sometimes separate turns.
    ///
    /// Idempotent on `request_id`: a retried write must not produce a second row
    /// for one decision, and must not clobber an operator ruling that arrived in
    /// between, which is why this is INSERT OR IGNORE rather than REPLACE.
    ///
    /// **Returns whether a row was actually written.** `INSERT OR IGNORE` answers
    /// `Ok(0)` when the key is already there, and the caller read only `is_ok()`
    /// — so a decision that collided was counted as kept. That is the exact
    /// shape `lost` exists to make impossible, and it hid a total corpus outage
    /// for a whole session (see `AdjudicatedGate::next_id`). `false` here means
    /// the row is NOT in the table and never will be.
    pub fn record_adjudication(&self, a: &NewAdjudication) -> Result<bool> {
        let changed = self.conn.execute(
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

    /// **The highest decision number this session has already stored.**
    ///
    /// `AdjudicatedGate` numbers its requests `adj-<session_id>-<seq>` from an
    /// in-memory counter, so a resumed session restarts at 1 and re-emits ids
    /// that are already in the table. This is what a gate seeds that counter
    /// from, so a restart continues the sequence instead of colliding with it.
    ///
    /// The `+ 6` is `adj-` (4) + the session id + `-` (1), one-based: it reads
    /// the numeric tail of the id this crate's own writer produced. A row whose
    /// tail does not parse contributes 0, which only ever makes the seed lower
    /// and the next write collide once more — never silently wrong in the
    /// direction that loses data, because that collision is now reported.
    pub fn max_adjudication_seq(&self, session_id: &str) -> Result<u64> {
        let n: Option<i64> = self.conn.query_row(
            "SELECT MAX(CAST(substr(request_id, length(?1) + 6) AS INTEGER))
               FROM adjudication WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )?;
        Ok(n.unwrap_or(0).max(0) as u64)
    }

    /// The operator's ruling on a decision already recorded. **This is the
    /// label**, and it never touches `model_verdict`: a row where the two differ
    /// is the row a fine-tune is for, and overwriting one with the other would
    /// destroy exactly the signal being collected.
    ///
    /// Only fills columns that are NULL. An operator who rules twice on one
    /// request keeps the first ruling, because the first is the one the session
    /// acted on.
    pub fn record_operator_ruling(&self, request_id: &str, kind: &str, note: &str) -> Result<bool> {
        let now = now_ms();
        let n = self.conn.execute(
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

    /// **One half of one decision's oracle exchange** — R11's locator, leticl's ask.
    ///
    /// `column` is the field's own name and is matched against a closed pair rather than
    /// interpolated: a caller that could name any column would be a caller that could read the
    /// whole table through a locator, and this one is for two fields.
    ///
    /// **`None` is "not recorded" and not "empty".** The column is `NULL` on every row written
    /// before R11 kept it, and an oracle that never answered has no reply either — a reader has
    /// to be able to tell *"nobody kept this"* from *"here it is, and it is empty"*, which is
    /// why this answers `Option<String>` and the wire answers `Option<String>`.
    pub fn diagnostic(&self, request_id: &str, column: &str) -> Result<Option<String>> {
        let field = match column {
            "brief" => "shown",
            "reply" => "oracle_reply",
            // `Refused`, not `Corrupt`: the store is fine and the caller named a field that
            // is not one of the two this locator answers for.
            other => return Err(StoreError::Refused(format!("`{other}` is not a diagnostic field"))),
        };
        let sql = format!("SELECT {field} FROM adjudication WHERE request_id = ?1");
        let out: Option<Option<String>> = self
            .conn
            .query_row(&sql, rusqlite::params![request_id], |r| r.get(0))
            .optional()?;
        // Nothing flattened: a row that is not there and a column that is `NULL` are the same
        // sentence to a reader ("not recorded"), and the caller cannot act differently on them.
        Ok(out.flatten())
    }

    /// The corpus, newest first. `only_labelled` narrows to rows the operator
    /// ruled on — the labelled set — because "every decision" and "every
    /// decision a human checked" are different datasets and a caller must say
    /// which it wants.
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
        let mut stmt = self.conn.prepare(&sql)?;
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

    /// **Shapes this operator approved by hand, under this workspace** — the warm
    /// start for the shape cache.
    ///
    /// The cache was in-memory, so it died with the process. That made it useless
    /// to the person it was built for: the operator restarts a session precisely
    /// when a session has gone wrong, and was then asked again about every shape
    /// they had already approved. Persisting it is the feature; the bounds below
    /// are what keep persisting it honest.
    ///
    /// Every bound the live rule applies is applied HERE too, in SQL, so a warm
    /// start can never be wider than the session that earned it:
    ///
    /// * `verdict_by LIKE 'human%'` — only a person's approval is remembered. The
    ///   guard's own admits are excluded for the same reason they are kept out of
    ///   the brief: a mechanism that learns from itself drifts.
    /// * `tier = 'may_approve'` — an `always_ask` a human admitted was admitted for
    ///   that one call. It is re-checked live as well; this is the second lock.
    /// * `effect = 'admit'` — a refusal is not an approval.
    /// * `baseline NOT LIKE '%destroy%'` — a shape holes its operands, so a
    ///   destructive call is never settled by one.
    /// * **the same `workspace_root`** — this is the bound the old in-memory cache
    ///   got for free and a persisted one does not. `host_project` means *this*
    ///   project, so without the join a shape approved in one checkout would admit
    ///   the same shape in another. The class is compared for equality by the
    ///   caller, and `host_project` is equal to `host_project` across two different
    ///   projects — hence the join rather than trust in the class alone.
    ///
    /// Newest first, so a caller keeping the first answer per shape keeps the most
    /// recent ruling.
    pub fn approved_shapes(&self, workspace_root: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
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

    /// **Human admits recorded before the shape column was filled in.** The input
    /// to the backfill, and nothing else reads it.
    ///
    /// Carries the workspace from the session rather than assuming one: a shape is
    /// only ever an approval *within a project*, and these rows span more than one.
    /// The row's own arguments come back unparsed — deciding what a command means is
    /// the intent layer's job and this module does not have an opinion.
    pub fn shapeless_human_admits(&self) -> Result<Vec<ShapelessAdmit>> {
        let mut stmt = self.conn.prepare(
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

    /// Write a shape and its class onto a row that has neither.
    ///
    /// `WHERE shape IS NULL` is the whole safety property: this can only ever fill a
    /// hole, never change an answer. Running it twice fills nothing the second time,
    /// and a row whose shape was recorded live is untouchable by it. Returns whether
    /// a row was actually filled, so a backfill can report what it did rather than
    /// what it attempted.
    pub fn backfill_shape(&self, request_id: &str, shape: &str, class: &str) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE adjudication SET shape = ?2, shape_class = ?3
              WHERE request_id = ?1 AND shape IS NULL",
            rusqlite::params![request_id, shape, class],
        )?;
        Ok(n == 1)
    }

    /// Counts for the startup disclosure: a corpus nobody can see the size of is
    /// a corpus nobody maintains.
    pub fn corpus_counts(&self) -> Result<CorpusCounts> {
        let one = |sql: &str| -> Result<u64> {
            let n: i64 = self.conn.query_row(sql, [], |r| r.get(0))?;
            Ok(n as u64)
        };
        Ok(CorpusCounts {
            total: one("SELECT COUNT(*) FROM adjudication")?,
            decided_by_operator: one(&format!(
                "SELECT COUNT(*) FROM adjudication WHERE {ASKED_SQL}"
            ))?,
            // **`measured` is `consulted`, not `verdict_by LIKE 'model%'`.**
            //
            // That predicate was a tautology and this banner said so out loud:
            // "60 decisions recorded: 3 you answered yourself, 60 measured against
            // a model" on a box where `verdict_by LIKE 'model%'` matched ZERO rows.
            // `AdjudicationRow::corpus` fills `model_verdict` from whatever DECIDED
            // when there is no advice, so it is always `Some` and the count was the
            // total wearing another name.
            //
            // It was replaced by `verdict_by LIKE 'model%'`, which is exact for "a
            // model decided" and still not the thing this number is named after: at
            // `/supervised` the oracle is consulted and a PERSON decides, so that
            // predicate silently dropped every row where the guard spoke and was
            // overruled — the rows the operator calls *"the rows a fine-tune is for"*.
            // The fact is `ModelAdvice::consulted` and it is a column now (**v10**),
            // so this reads the fact. `model_decided` carries the other one.
            //
            // **A count is not measuring what its NAME says, it is measuring what its
            // PREDICATE says** — and the predicate lives in one place now, above.
            measured: one(&format!("SELECT COUNT(*) FROM adjudication WHERE {MEASURED_SQL}"))?,
            disagreements: one(&format!(
                "SELECT COUNT(*) FROM adjudication WHERE {DISAGREEMENT_SQL}"
            ))?,
            model_decided: one(&format!(
                "SELECT COUNT(*) FROM adjudication WHERE {MODEL_DECIDED_SQL}"
            ))?,
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::ledger::{TokenLedger, hash_tokens};
    use letibot_transcript::{ReasoningField, SystemOrigin};

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn prefix() -> StablePrefixRecord {
        let tokens = vec![1u32, 2, 3, 4];
        StablePrefixRecord {
            dialect_sha: "aa".repeat(32),
            system: "you are a harness".into(),
            tools_json: vec![r#"{"name":"read"}"#.into()],
            h_init: hash_tokens(&tokens),
            tokens,
            vocab_source: "/models/x.gguf".into(),
        }
    }

    fn seeded(s: &Store) -> (String, String) {
        let p = prefix();
        let prefix_id = s.put_stable_prefix(&p).unwrap();
        s.put_session(&SessionRecord {
            id: "sess-1".into(),
            title: Some("t".into()),
            model_id: "qwen".into(),
            dialect_sha: p.dialect_sha.clone(),
            workspace_root: "/w".into(),
            owner: "deadtrickster".into(),
            role: None,
            approvers: vec!["deadtrickster".into()],
            parent_session_id: None,
        })
        .unwrap();
        s.put_transcript("tr-1", "sess-1", &prefix_id).unwrap();
        ("tr-1".into(), prefix_id)
    }

    /// **R19.2b: one row, by ordinal, without loading the transcript it is in.**
    ///
    /// The read behind `FetchRow` for a row the daemon's **bounded view** has trimmed
    /// (2,000 rows, 8 MB of bodies). Its whole point is that it is **one indexed
    /// lookup** — `transcript_item`'s primary key is `(transcript_id, seq)` — so this
    /// asserts what the index is for, what it answers for an ordinal that is not there,
    /// and that an ordinal is relative to the session's **current** transcript.
    #[test]
    fn a_row_is_readable_by_ordinal_without_loading_its_transcript() {
        let s = store();
        let (tr, _) = seeded(&s);
        let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
        let items = [
            TranscriptItem::User {
                parts: vec![letibot_transcript::UserPart::Text {
                    text: "first".into(),
                }],
            },
            TranscriptItem::Assistant {
                text: "second".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ];
        for (seq, item) in items.iter().enumerate() {
            let toks = vec![10 + seq as u32];
            let row = ledger.append(&format!("it-{seq}"), &toks).unwrap().clone();
            s.append_item(&tr, seq as u32, item, &row, &toks).unwrap();
        }

        // **The ordinal is relative to the CURRENT transcript**, which is the one the
        // rows went into. This is the mapping the `FetchRow` glue depends on, and after
        // a fork it is the whole reason the lookup goes through the session rather than
        // a transcript id the head has never heard of.
        assert_eq!(
            s.current_transcript_id("sess-1").unwrap().as_deref(),
            Some("tr-1")
        );

        // The row by ordinal, read through the same serde form the wire carries — so
        // this asserts the shape `body_of` will be handed, not a private one.
        let json = s.row_json_at(&tr, 1).unwrap().expect("row 1 is there");
        let back: TranscriptItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, items[1]);

        // **An ordinal the transcript does not have is `None`, not an empty string** —
        // the rule `RowFetched` states for the wire: "nobody has it" and "it is empty"
        // must not look alike.
        assert_eq!(s.row_json_at(&tr, 2).unwrap(), None);
        assert_eq!(s.row_json_at(&tr, 9_999).unwrap(), None);

        // A session nothing was written for has no current transcript, so the same read
        // answers `None` rather than somebody else's row.
        assert_eq!(s.current_transcript_id("no-such-session").unwrap(), None);
    }

    /// **A fork's rows are the ones an ordinal names.** After a compaction the session
    /// has two transcripts and ordinal 3 belongs to the new base — reading the old one
    /// would answer with a row nobody asked about, which is the failure the "newest
    /// transcript" rule exists to prevent. Asserted against the store so the SQL that
    /// picks the leaf is what is under test, not a comment about it.
    #[test]
    fn an_ordinal_follows_the_fork_to_the_new_base() {
        let s = store();
        let (tr, prefix_id) = seeded(&s);
        s.put_fork("tr-2", "sess-1", &prefix_id, &tr, 1).unwrap();
        assert_eq!(
            s.current_transcript_id("sess-1").unwrap().as_deref(),
            Some("tr-2"),
            "the newest transcript is the one an ordinal is relative to"
        );
        // …and the new base has none of the old rows: ordinal 0 of a fork that carried
        // nothing is not the old transcript's row 0.
        assert_eq!(s.row_json_at("tr-2", 0).unwrap(), None);
    }

    #[test]
    fn a_stable_prefix_is_addressed_by_its_content_and_its_dialect() {
        let s = store();
        let p = prefix();
        let a = s.put_stable_prefix(&p).unwrap();
        let b = s.put_stable_prefix(&p).unwrap();
        assert_eq!(a, b, "the same prefix must not be stored twice");

        // Same text, different renderer, therefore different tokens: it must not
        // collide, or a GLM session would inherit Qwen's ids.
        let mut other = p.clone();
        other.dialect_sha = "bb".repeat(32);
        other.tokens = vec![9, 9, 9];
        other.h_init = hash_tokens(&other.tokens);
        let c = s.put_stable_prefix(&other).unwrap();
        assert_ne!(a, c);
        assert_eq!(s.get_stable_prefix(&a).unwrap().0, vec![1, 2, 3, 4]);
        assert_eq!(s.get_stable_prefix(&c).unwrap().0, vec![9, 9, 9]);
    }

    #[test]
    fn transcript_item_cannot_be_updated_or_deleted() {
        let s = store();
        let (tr, _) = seeded(&s);
        let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
        let item = TranscriptItem::User {
            parts: vec![letibot_transcript::UserPart::Text { text: "hi".into() }],
        };
        let row = ledger.append("it-0", &[10, 11]).unwrap().clone();
        s.append_item(&tr, 0, &item, &row, &[10, 11]).unwrap();

        let update = s.conn.execute(
            "UPDATE transcript_item SET item_json = '{}' WHERE transcript_id = ?1",
            params![tr],
        );
        assert!(update.is_err(), "UPDATE must be refused by the trigger");
        assert!(format!("{:?}", update.unwrap_err()).contains("append-only"));

        let delete = s.conn.execute(
            "DELETE FROM transcript_item WHERE transcript_id = ?1",
            params![tr],
        );
        assert!(delete.is_err(), "DELETE must be refused by the trigger");
        assert!(format!("{:?}", delete.unwrap_err()).contains("append-only"));

        assert_eq!(s.item_count(&tr).unwrap(), 1);
    }

    #[test]
    fn a_row_that_does_not_continue_the_previous_one_is_refused() {
        let s = store();
        let (tr, _) = seeded(&s);
        let item = TranscriptItem::Reasoning {
            text: "t".into(),
            field: ReasoningField::Inline,
            truncated: false,
        };
        let good = LedgerRow {
            item_id: "a".into(),
            tok_offset: 4,
            tok_len: 2,
            h_k: [1; 32],
        };
        s.append_item(&tr, 0, &item, &good, &[1, 2]).unwrap();

        // A hole in the token stream.
        let gap = LedgerRow {
            item_id: "b".into(),
            tok_offset: 9,
            tok_len: 1,
            h_k: [2; 32],
        };
        assert!(s.append_item(&tr, 1, &item, &gap, &[3]).is_err());

        // A seq that skips.
        let skip = LedgerRow {
            item_id: "c".into(),
            tok_offset: 6,
            tok_len: 1,
            h_k: [3; 32],
        };
        assert!(s.append_item(&tr, 5, &item, &skip, &[3]).is_err());

        // A re-insert at an index already used: the seq trigger catches it before
        // the primary key does, and either way it is refused.
        let reuse = LedgerRow {
            item_id: "d".into(),
            tok_offset: 4,
            tok_len: 2,
            h_k: [4; 32],
        };
        assert!(s.append_item(&tr, 0, &item, &reuse, &[1, 2]).is_err());

        // A blob that does not match the row's length.
        let lying = LedgerRow {
            item_id: "e".into(),
            tok_offset: 6,
            tok_len: 7,
            h_k: [5; 32],
        };
        assert!(s.append_item(&tr, 1, &item, &lying, &[3]).is_err());

        // And the good continuation still works.
        let next = LedgerRow {
            item_id: "f".into(),
            tok_offset: 6,
            tok_len: 1,
            h_k: [6; 32],
        };
        s.append_item(&tr, 1, &item, &next, &[3]).unwrap();
        assert_eq!(s.item_count(&tr).unwrap(), 2);
    }

    #[test]
    fn a_fork_records_where_it_forked() {
        let s = store();
        let (tr, prefix_id) = seeded(&s);
        s.put_fork("tr-2", "sess-1", &prefix_id, &tr, 3).unwrap();
        let loaded = s.load_transcript("tr-2").unwrap();
        assert_eq!(loaded.parent_transcript_id.as_deref(), Some("tr-1"));
        assert_eq!(loaded.forked_at_seq, Some(3));

        let fresh = s.load_transcript(&tr).unwrap();
        assert_eq!(fresh.parent_transcript_id, None);
        assert_eq!(fresh.forked_at_seq, None);

        // The CHECK keeps the two fields together.
        let half = s.conn.execute(
            "INSERT INTO transcript (id, session_id, parent_transcript_id, forked_at_seq,
                                     stable_prefix_id, created_at)
             VALUES ('tr-3', 'sess-1', 'tr-1', NULL, ?1, 0)",
            params![prefix_id],
        );
        assert!(
            half.is_err(),
            "a parent without a fork point must be refused"
        );
    }

    #[test]
    fn store_round_trip_rebuilds_the_same_ledger() {
        let s = store();
        let (tr, _) = seeded(&s);
        let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();

        let items: Vec<TranscriptItem> = vec![
            TranscriptItem::System {
                text: "boot".into(),
                origin: SystemOrigin::Bootstrap,
            },
            TranscriptItem::User {
                parts: vec![letibot_transcript::UserPart::Text {
                    text: "hello".into(),
                }],
            },
            TranscriptItem::Reasoning {
                text: "think".into(),
                field: ReasoningField::Inline,
                truncated: false,
            },
            TranscriptItem::SegmentMark {
                segment_id: "s0".into(),
                label: "l".into(),
                kind: "k".into(),
                edge: letibot_transcript::SegmentEdge::Open,
            },
            TranscriptItem::Assistant {
                text: "hi".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ];
        // The SegmentMark renders to nothing; every other item to something.
        let payloads: Vec<Vec<u32>> =
            vec![vec![20, 21], vec![22], vec![23, 24, 25], vec![], vec![26]];

        for (seq, (item, toks)) in items.iter().zip(&payloads).enumerate() {
            let row = ledger.append(&format!("it-{seq}"), toks).unwrap().clone();
            s.append_item(&tr, seq as u32, item, &row, toks).unwrap();
        }

        let loaded = s.load_transcript(&tr).unwrap();
        assert_eq!(loaded.prefix_tokens, vec![1, 2, 3, 4]);
        assert_eq!(loaded.h_init, ledger.h_init());
        assert_eq!(loaded.items.len(), 5);
        assert_eq!(
            loaded
                .items
                .iter()
                .map(|(i, ..)| i.clone())
                .collect::<Vec<_>>(),
            items
        );

        let restored = TokenLedger::restore(
            "tr-1-restored",
            &loaded.prefix_tokens,
            loaded.h_init,
            &loaded.ledger_input(),
        )
        .unwrap();
        assert_eq!(restored.tokens(), ledger.tokens());
        assert_eq!(restored.head(), ledger.head());
        assert_eq!(restored.rows(), ledger.rows());
        assert_eq!(restored.span(), ledger.span());
        restored.verify_chain().unwrap();
    }

    #[test]
    fn kinds_are_denormalised_correctly() {
        let s = store();
        let (tr, _) = seeded(&s);
        let mut ledger = TokenLedger::new(&tr, &[]).unwrap();
        let item = TranscriptItem::ToolResult {
            call_id: "c".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Abstained {
                reason: "no cover".into(),
            },
            payload: "{}".into(),
            edit: None,
            origin: None,
        };
        let row = ledger.append("it", &[1]).unwrap().clone();
        s.append_item(&tr, 0, &item, &row, &[1]).unwrap();
        let kind: String = s
            .conn
            .query_row("SELECT kind FROM transcript_item", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "tool_result");

        // Abstained must survive the round trip as itself, not as Ok.
        let back = s.load_transcript(&tr).unwrap();
        assert_eq!(back.items[0].0, item);
    }

    /// A v1 store — one written before a session could record its role — is carried
    /// forward, and every row in it reads back as "unrecorded".
    ///
    /// This is the test the operator's ten live sessions depend on. `migrate()` used
    /// to return `Ok(())` for any version it already knew, so a column added to
    /// `SCHEMA_SQL` reached a fresh file and nothing else; the failure would have been
    /// a `no such column: role` on the first list, after the daemon had already
    /// started.
    #[test]
    fn a_v1_store_is_migrated_and_its_rows_read_as_unrecorded() {
        // A file, not `open_in_memory`, because the point is a store that already
        // exists on disk at an older version. No dev-dependency for one test.
        let path = std::env::temp_dir().join(format!(
            "letibot-migrate-v1-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _clean = Clean(path.clone());

        // Make a REAL store, then reverse the v2 step on it. Hand-writing the v1
        // DDL was the first attempt and it was wrong: the fixture lacked
        // `transcript`, which `list_sessions` joins, so the test failed on a table
        // the migration never touches. Reversing a real store keeps every other
        // table exactly as v1 had it and cannot drift from `SCHEMA_SQL`.
        {
            let s = Store::open(&path).unwrap();
            s.put_session(&SessionRecord {
                id: "s-old".into(),
                title: Some("a title".into()),
                model_id: "m".into(),
                dialect_sha: "sha".into(),
                workspace_root: "/w".into(),
                owner: "dead".into(),
                role: None,
                approvers: vec![],
                parent_session_id: None,
            })
            .unwrap();
        }
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(
                "ALTER TABLE session DROP COLUMN role;
                 ALTER TABLE session DROP COLUMN parent_session_id;
                 DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (1);",
            )
            .unwrap();
            // Prove the fixture really is v1: the column is gone.
            assert!(
                c.query_row("SELECT role FROM session", [], |r| r
                    .get::<_, Option<String>>(0))
                    .is_err(),
                "the fixture still has a role column, so it is not a v1 store"
            );
            assert!(
                c.query_row("SELECT parent_session_id FROM session", [], |r| r
                    .get::<_, Option<String>>(0))
                    .is_err(),
                "the fixture still has a parent_session_id column, so it is not a v1 store"
            );
        }

        // Opening it runs the migration.
        let s = Store::open(&path).unwrap();
        let got = s.session("s-old").unwrap().expect("the v1 row survived");
        assert_eq!(got.title.as_deref(), Some("a title"));
        assert_eq!(
            got.role, None,
            "a session written before v2 must read as unrecorded, not as some default"
        );

        // The version moved, and opening again is a no-op rather than a re-migration.
        let v: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        drop(s);
        let s = Store::open(&path).unwrap();
        assert!(
            s.session("s-old").unwrap().is_some(),
            "reopen is idempotent"
        );

        // And a role written after the migration comes back.
        s.put_session(&SessionRecord {
            id: "s-new".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: Some("planner".into()),
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
        assert_eq!(
            s.session("s-new").unwrap().unwrap().role.as_deref(),
            Some("planner")
        );
    }

    #[test]
    fn a_v2_store_is_migrated_and_gains_a_todo_table() {
        // Same fixture rule as the v1 test: a real store, one step reversed, and
        // the migration has to put back exactly what `SCHEMA_SQL` would have.
        let path = std::env::temp_dir().join(format!(
            "letibot-migrate-v2-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _clean = Clean(path.clone());

        {
            let s = Store::open(&path).unwrap();
            s.put_session(&SessionRecord {
                id: "s-todo".into(),
                title: None,
                model_id: "m".into(),
                dialect_sha: "sha".into(),
                workspace_root: "/w".into(),
                owner: "dead".into(),
                role: Some("coder".into()),
                approvers: vec![],
                parent_session_id: None,
            })
            .unwrap();
        }
        {
            // Reverse the v3 step (the table goes away) and the v4 step (the column
            // goes away), and the version says 2 — so the migration has to put both
            // back.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("DROP TABLE todo", []).unwrap();
            c.execute("ALTER TABLE session DROP COLUMN parent_session_id", [])
                .unwrap();
            c.execute("UPDATE schema_version SET version = 2", [])
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let v: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        // The migrated table works, and the session's rows survived the trip.
        assert!(s.todos("s-todo").unwrap().is_empty());
        s.put_todos(
            "s-todo",
            &[TodoItem {
                content: "ship the pane".into(),
                status: TodoStatus::InProgress,
            }],
        )
        .unwrap();
        assert_eq!(s.todos("s-todo").unwrap().len(), 1);
    }

    #[test]
    fn todos_are_replaced_whole_and_read_back_in_order() {
        let s = store();
        let _seeded = seeded(&s);

        // No list yet: empty, not an error, not a distinction from "cleared".
        assert!(s.todos("sess-1").unwrap().is_empty());

        let first = vec![
            TodoItem {
                content: "read the harness".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                content: "seat the tool".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                content: "render the pane".into(),
                status: TodoStatus::Pending,
            },
        ];
        s.put_todos("sess-1", &first).unwrap();
        assert_eq!(
            s.todos("sess-1").unwrap(),
            first,
            "order survives the store"
        );

        // The second write is the list, not a patch on it: the revision the model
        // made is the only one the store holds.
        let second = vec![TodoItem {
            content: "render the pane".into(),
            status: TodoStatus::InProgress,
        }];
        s.put_todos("sess-1", &second).unwrap();
        assert_eq!(s.todos("sess-1").unwrap(), second);

        // An empty write clears; the row remains and answers empty.
        s.put_todos("sess-1", &[]).unwrap();
        assert!(s.todos("sess-1").unwrap().is_empty());
    }

    #[test]
    fn the_last_turns_context_lives_on_the_session_row() {
        let s = store();
        let _seeded = seeded(&s);

        // Before a turn has finished the row says so: `None`, not zero. A head
        // renders `None` as no number; a stored zero would be a number nobody took.
        let got = s.session("sess-1").unwrap().unwrap();
        assert_eq!(got.context_tokens, None);
        assert_eq!(got.context_cached, None);

        s.set_context("sess-1", Some(44_700), Some(40_000)).unwrap();
        let got = s.session("sess-1").unwrap().unwrap();
        assert_eq!(got.context_tokens, Some(44_700));
        assert_eq!(got.context_cached, Some(40_000));

        // A later turn replaces the number: it is the LAST turn's prompt, not a
        // sum and not a maximum.
        s.set_context("sess-1", Some(51_000), Some(44_700)).unwrap();
        let got = s.session("sess-1").unwrap().unwrap();
        assert_eq!(got.context_tokens, Some(51_000));
        assert_eq!(got.context_cached, Some(44_700));

        // A session nobody has written is not a row to update.
        assert!(matches!(
            s.set_context("nope", Some(1), None),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn a_v7_store_is_migrated_and_gains_the_context_columns() {
        // Same fixture rule as the v1/v2 tests: a real store, one step reversed,
        // and the migration has to put back exactly what `SCHEMA_SQL` would have.
        let path = std::env::temp_dir().join(format!(
            "letibot-migrate-v7-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _clean = Clean(path.clone());

        {
            let s = Store::open(&path).unwrap();
            s.put_session(&SessionRecord {
                id: "s-ctx".into(),
                title: None,
                model_id: "m".into(),
                dialect_sha: "sha".into(),
                workspace_root: "/w".into(),
                owner: "dead".into(),
                role: None,
                approvers: vec![],
                parent_session_id: None,
            })
            .unwrap();
        }
        {
            // Reverse the v8 step: the columns go away and the version says 7.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("ALTER TABLE session DROP COLUMN context_tokens", [])
                .unwrap();
            c.execute("ALTER TABLE session DROP COLUMN context_cached", [])
                .unwrap();
            c.execute("UPDATE schema_version SET version = 7", [])
                .unwrap();
            assert!(
                c.query_row("SELECT context_tokens FROM session", [], |r| r
                    .get::<_, Option<i64>>(0))
                    .is_err(),
                "the fixture still has a context_tokens column, so it is not a v7 store"
            );
        }

        let s = Store::open(&path).unwrap();
        let v: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        // The migrated row reads as "no turn has finished since the column
        // existed", and a write after the migration comes back.
        let got = s.session("s-ctx").unwrap().expect("the v7 row survived");
        assert_eq!(got.context_tokens, None);
        s.set_context("s-ctx", Some(12_000), Some(11_000)).unwrap();
        let got = s.session("s-ctx").unwrap().unwrap();
        assert_eq!(got.context_tokens, Some(12_000));
        assert_eq!(got.context_cached, Some(11_000));
    }

    #[test]
    fn the_v8_migration_backfills_the_context_of_a_conversation_already_on_disk() {
        // The case that matters: a store being upgraded holds a conversation whose
        // last turn finished before the column existed. Without the backfill its
        // row reads `NULL` — "no turn has finished" — and an attaching head shows
        // no context until the next turn. The backfill sums the prefix and the
        // current transcript's items, the same rows the encoder reads.
        let path = std::env::temp_dir().join(format!(
            "letibot-backfill-v8-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _clean = Clean(path.clone());

        {
            let s = Store::open(&path).unwrap();
            let (tr, _) = seeded(&s);
            // Two items: 2 tokens and 3 tokens. The prefix carries 4.
            let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
            let item = TranscriptItem::User {
                parts: vec![letibot_transcript::UserPart::Text { text: "hi".into() }],
            };
            let row = ledger.append("it-0", &[10, 11]).unwrap().clone();
            s.append_item(&tr, 0, &item, &row, &[10, 11]).unwrap();
            let row = ledger.append("it-1", &[12, 13, 14]).unwrap().clone();
            s.append_item(&tr, 1, &item, &row, &[12, 13, 14]).unwrap();
        }
        {
            // Reverse the v8 step: the columns go away and the version says 7. The
            // conversation's rows stay, which is what the migration has to read.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("ALTER TABLE session DROP COLUMN context_tokens", [])
                .unwrap();
            c.execute("ALTER TABLE session DROP COLUMN context_cached", [])
                .unwrap();
            c.execute("UPDATE schema_version SET version = 7", [])
                .unwrap();
        }

        let s = Store::open(&path).unwrap();
        let got = s.session("sess-1").unwrap().expect("the row survived");
        // 4 (prefix) + 2 + 3 (items) = 9: the prompt the next round would send.
        assert_eq!(
            got.context_tokens,
            Some(9),
            "the backfill is the prefix plus the current transcript's items"
        );
        // The fraction of a prompt that was never sent is not a measurement.
        assert_eq!(got.context_cached, None);

        // A session with no transcript is not invented a number: the subquery
        // finds nothing and the row stays `NULL`.
        s.put_session(&SessionRecord {
            id: "s-empty".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
        // Reopening must not backfill it: the column already exists, so the
        // `NULL` means "no turn has finished", not "needs a number".
        drop(s);
        let s = Store::open(&path).unwrap();
        let got = s.session("s-empty").unwrap().unwrap();
        assert_eq!(got.context_tokens, None, "no transcript, no number");
    }
}

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

    /// **A resumed session must not silently drop every decision it makes.**
    ///
    /// `AdjudicatedGate` numbers requests `adj-<session>-<seq>` from an in-memory
    /// counter, so a restart re-emits ids that are already stored, and
    /// `INSERT OR IGNORE` answers `Ok` for each one. Measured 2026-09-20 in the
    /// operator's store: a session with 646 rows recorded nothing at all after a
    /// restart, and `lost` stayed at zero the whole time.
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
        // The same id again — a restarted daemon's first decision.
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

    /// **R11: the exchange is on the row, and an older row says so honestly.**
    ///
    /// The corpus table already had a column for the brief — `shown` — and it was being
    /// filled with the operator's decision card, because the gate asked its *decider* for
    /// `last_brief` and the decider is a person at a supervised point. Measured on this box
    /// 2026-09-21: set on 147 of 5785 rows, `NULL` on every row an oracle decided, and all
    /// 147 were rows a human answered. The reply had no column at all.
    ///
    /// So there are two assertions here and they are different statements: a row written
    /// now carries both halves through the round trip, and a row written before the column
    /// existed reads `NULL` for the reply — *"recorded before this was kept"* — which is
    /// what a reader has to be able to tell apart from *"the oracle did not answer"*.
    #[test]
    fn the_oracles_brief_and_reply_round_trip_and_an_older_row_says_null() {
        let s = store();
        let session = a_session(&s);
        let mut d = a_decision("adj-r11", &session);
        d.shown = Some("brief — a file was read\ntrail: 1 operator message(s)".into());
        d.reply = Some("It follows from what was asked.\nALLOW 0".into());
        assert!(s.record_adjudication(&d).unwrap());

        let row = s.corpus(false, 10).unwrap().remove(0);
        assert!(row.shown.as_deref().unwrap().starts_with("brief — "), "{:?}", row.shown);
        assert!(row.reply.as_deref().unwrap().contains("ALLOW 0"), "{:?}", row.reply);

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
        s.conn
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

    /// **R12: which of the four `Unsure`s it was, as a column.**
    ///
    /// The operator: *an oracle that ran out of budget is not an oracle that could not be
    /// read.* The sentence that distinguished them was already on the row — in
    /// `model_verdict`, as prose — and a sentence is not something a `GROUP BY` can count.
    /// That is exactly the argument `verdict` has beside `model_verdict`, and this is the
    /// same move one requirement later.
    ///
    /// Three assertions, and the middle one is the requirement: the four values round trip;
    /// **the rate of each is a query**; and a row from before the column reads `NULL`, which
    /// is a different statement from any of the four.
    #[test]
    fn the_reading_of_an_unsure_is_a_column_and_the_rate_of_each_is_a_query() {
        let s = store();
        let session = a_session(&s);
        for (i, kind) in ["could_not_decide", "between_thresholds", "unreadable", "out_of_room"]
            .into_iter()
            .enumerate()
        {
            let mut d = a_decision(&format!("adj-r12-{i}"), &session);
            d.consulted = Some(true);
            d.oracle_reading = Some(kind.into());
            assert!(s.record_adjudication(&d).unwrap());
        }
        // An answered verdict: consulted, and no reading — which is what a row looks like
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
            ["between_thresholds", "could_not_decide", "out_of_room", "unreadable"]
        );
        let answered = rows
            .iter()
            .find(|r| r.request_id == "adj-r12-answered")
            .expect("the answered row");
        assert_eq!(answered.consulted, Some(true));
        assert_eq!(answered.oracle_reading, None, "an answered verdict has no reading");

        // **The rate of each**, which is the thing the column exists for: one query, and
        // the answer is a count per token rather than a regex over a sentence.
        let mut stmt = s
            .conn
            .prepare(
                "SELECT oracle_reading, COUNT(*) FROM adjudication
                  WHERE oracle_reading IS NOT NULL GROUP BY 1 ORDER BY 1",
            )
            .unwrap();
        let counts: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            counts,
            [
                ("between_thresholds".to_string(), 1),
                ("could_not_decide".to_string(), 1),
                ("out_of_room".to_string(), 1),
                ("unreadable".to_string(), 1),
            ]
        );

        // And a row written before the column — `NULL`, which is not any of the four. A
        // reader of the old ones goes to `model_verdict`/`verdict_basis`, and `corpus_version`
        // is what says the row is older than the question.
        s.conn
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

    /// **A store written before R12 gains the column and says so.**
    ///
    /// The v11 arm is `pragma_table_info`-guarded like every other one, and the thing worth
    /// asserting is not that the `ALTER` runs — it is that a store from the version before
    /// comes back with the column and with its old rows **`NULL`**, which is a different
    /// statement from any of the four readings. The old rows recorded their reason in a
    /// sentence (`model_verdict`), and that is where a reader of them goes.
    #[test]
    fn an_older_store_gains_the_reading_and_its_rows_say_null() {
        let path = std::env::temp_dir().join(format!(
            "letibot-store-v11-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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

        // The column is there, the old row survived, and its reading is `NULL` — which is
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
        let fresh = rows.iter().find(|r| r.request_id == "adj-after-v11").unwrap();
        assert_eq!(fresh.oracle_reading.as_deref(), Some("out_of_room"));
    }

    /// **The warm start is exactly as wide as the session that earned it.**
    ///
    /// Six rows, one per bound, and only the first may come back. Each of the other
    /// five is a way the shape cache could quietly grow across a restart into
    /// something nobody approved — which is the failure that matters here, because
    /// it is silent: a cache that is too wide does not ask, and not being asked is
    /// indistinguishable from things working.
    #[test]
    fn a_warm_start_carries_only_what_the_operator_themselves_approved() {
        let s = store();
        let here = a_session(&s);

        // A second project, same store — a shape approved there is not approved here.
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

    /// **Who decided and what it was measured against are independent counts.**
    ///
    /// The earlier version of this reported one number for both, so a session at
    /// `always-ask` where the operator personally answered every call read as
    /// "0 ruled on". Every one of those was a decision a human made.
    #[test]
    fn counts_separate_who_decided_from_what_was_measured() {
        let s = store();
        let sid = a_session(&s);
        // **`consulted` is the third axis, and it is what `measured` reads (R11).** Here
        // "the model said something" and "an oracle was consulted" are the same row
        // shape, so the fixture sets both together; `m2` below is the row where they
        // come apart — `model_verdict` written by the GATE, nobody asked.
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
        // **The row that caught the tautology**: a mode admitted it, nothing was
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
        // **The count that speaks for rows written before v10.** Every row here carries
        // `verdict_by: model:test`, so "a model decided" is 3 where "an oracle was
        // consulted" is 3 — the two agree in this fixture and are different fields in
        // the schema, which is the whole reason both are reported.
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
        let s = Store::from_connection(s.conn).expect("migrate");

        let sid = a_session(&s);
        s.record_adjudication(&a_decision("after-migration", &sid))
            .expect("record");
        assert_eq!(s.corpus_counts().expect("counts").total, 1);
    }
}
