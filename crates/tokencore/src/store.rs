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

/// **16** since the **merge queue** is a table of its own — `merge_queue`, one row per
/// entry, described at its migration arm below and in [`MergeEntry`]. **15** since the
/// session's **job history** is a table of its own — `job`, one row per
/// handle, described at its migration arm below and in [`JobRecord`]. **14** since a session's
/// resolved **context window** is on its row — `context_window`,
/// additive, described at its migration arm below. A child's window belongs to the model
/// that answers it, and the number was otherwise knowable only at the moment it was used.
/// **12** since the session row pairs its provider count with the LEDGER that count
/// was measured against — `context_ledger`, additive, described at its migration arm
/// below. **11** since the corpus records **which of the four `Unsure`s** an oracle's answer was
/// (R12) — `oracle_reading`, additive, described at its migration arm below. **10** since it
/// records whether an oracle was consulted and what it answered (R11), and **9** added
/// `oracle_reply` for the same requirement.
///
/// **19** since the session row carries the pair that stood its automatic compaction
/// down — `auto_compact_resident`/`auto_compact_after`, additive, described at its
/// migration arm below. The pair is written once, when the no-progress guard fires, and
/// nothing clears it but a fresh session or a raised window: a reader asking *why does
/// this session not compact* can only answer it from here once the daemon that decided
/// it is gone.
pub const SCHEMA_VERSION: i64 = 20;

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
    auto_compact_after INTEGER       -- v19; LEDGER tokens coming out of it, still within
                                     -- a headroom of the window. NULL with its pair.
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
    PRIMARY KEY (session_id, handle)
);

-- **The merge queue.** One row per entry the daemon is serving toward main, durable in
-- this file for the reason the job history is: the queue the daemon serves is the queue
-- that must survive the daemon, because the daemon is the one thing that is restarted and
-- a merge that dies mid-flight must come back as a row that says what happened, not as an
-- empty queue that says nothing.
--
-- The daemon is the only writer; a head never writes one. The store cannot tell who a
-- connection is, so the guarantee is that the daemon is the only caller, which is the same
-- shape as the job history's.
--
-- `session_id` is NOT a foreign key, and that is the difference from `job`: a job belongs
-- to a session and dies with it, but an entry is about a BRANCH, and a session that is
-- deleted does not un-merge a branch that still needs merging. It is the entry's origin —
-- the subagent that finished it, or the operator's session for an urgent entry — and it
-- is metadata the queue reads, not a referent the queue depends on.
--
-- `needs_json` is a JSON array of entry ids, the dependencies the entry is not taken
-- until have `landed`. `state` and `priority` are the closed sets' own words. `evidence`
-- is the reason for the state, in the queue's own words: a state without its reason is a
-- row the pane draws and the operator cannot read.
--
-- `brief` (v17) is the ask the branch was produced under, verbatim, and it is here rather
-- than in a session row because the reviewer reads it: the gatekeeper's protocol is
-- brief-first, so an entry that cannot carry its ask is an entry nobody can review against
-- anything. Empty means nobody recorded one.
--
-- **`vetoed` is a WORD, not a column, and it costs no migration.** The operator's ask is
-- *"i want to be able to approve / veto / delete"*, and the veto needs a state a person's
-- decision can be read in: a rejection by a person is not a gate's failure and must not
-- draw as one. `state` is `TEXT NOT NULL` with no `CHECK` — the closed set is
-- `MergeState`'s, parsed on read (`merge_entry_from_raw`) so a word this build does not
-- know is a named refusal rather than a silent misread — so the seventh word travels in
-- the same column as the six. What DOES have to move for it is the wire: see
-- `letibot_sessionlog::event::MergeState` and `PROTOCOL_VERSION` 38, because a head that
-- cannot decode the word loses the whole frame it arrives in.
CREATE TABLE IF NOT EXISTS merge_queue (
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
    landed_sha  TEXT
);

-- The read the daemon does on every pass: the entries in a state, in priority order,
-- oldest first. The queue is small and the pure core sorts in memory, but the index
-- documents the read the way flowy's `tasks(to_user, state)` does its own, and it is the
-- one a SQL-side read would use.
CREATE INDEX IF NOT EXISTS merge_queue_state_idx
    ON merge_queue (state, priority, created_ms);

-- **The gatekeeper's verdict on one entry (v18).** One row per entry, keyed by the entry's
-- own id, written when the review is ASKED FOR and updated once when the verdict comes back —
-- the `job` row's shape, for the `job` row's reason: *asked* is not the same fact as
-- *answered*, and a queue that could not tell them apart would either re-ask for ever or wait
-- for a verdict nobody was ever asked for.
--
-- A table of its own rather than columns on `merge_queue`, because the two have different
-- writers: the queue owns the entry's state and the REVIEWER owns the verdict. The rule that
-- a head never writes a `merge_queue` row is about the merge — a head that could mark its own
-- branch `Landed` is the thing the queue exists to prevent — and it says nothing about the
-- review, which is a different act by a different seat.
--
-- `decision` is `accept`, `reject` or `needs_human` (the gatekeeper's own closed set), and
-- NULL while no verdict has come back. `reasons_json` is the reviewer's reasons and
-- `files_json`/`commands_json` are what it looked at — a verdict with no evidence is an
-- opinion, which is why they are columns and not prose.
--
-- **`attempts`, `failed_ms` and `failure` (v20) are the ATTEMPT, and the attempt is not the
-- verdict.** A reviewer whose turn failed — a provider that answered `429`, a child that could
-- not be started, a reply with no readable verdict in it — has reached no judgement at all, and
-- writing one into `decision` was the defect these columns exist to end: `mergequeue::review_gate`
-- reads a word outside the closed set as a REFUSAL, so a failed attempt was parked on the entry
-- as though somebody had judged it, and `answered_ms` being set made that park terminal. So a
-- failure goes in `failure` (the provider's own words, verbatim), `failed_ms` is when it
-- happened, and `attempts` counts how many attempts have failed since the last restart.
-- `decision` stays NULL, which is what lets the queue ask again — boundedly, and restartable
-- by hand (`mergequeue::restart`).
CREATE TABLE IF NOT EXISTS merge_review (
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
    ///
    /// **So is the vocabulary, when it is the byte one**, for the same reason: the byte
    /// vocabulary and a GGUF tokenize one rendering to different ids, and without this a
    /// session moved between them would reuse a row whose tokens are the other's. It is
    /// hashed in only for a `bytes:` source so that every id written before the byte
    /// vocabulary existed — all of them GGUF rows — is the id it always was.
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

/// A session as the store holds it: enough to list it, pick it and resume it.
///
/// Separate from [`SessionRecord`], which is the *write* shape. This one carries
/// three things that are computed rather than stored — the transcript id, the row
/// count and the last activity — because every caller that lists sessions needs all
/// three and would otherwise write the same three subqueries slightly differently.
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
    /// **The current transcript's last row**, or `None` for a session with no rows.
    ///
    /// What a list can say about how a session's last turn ended without loading the
    /// transcript: an answer with no calls after it is a finished turn, anything else is
    /// a turn that was still going when the rows stopped. The subagents pane reads it for
    /// a child its head did not watch end — after a daemon restart, every one of them.
    /// `None` too for a row this build cannot parse, which is a fact it has not got.
    pub last_item: Option<TranscriptItem>,
    /// The last turn's prompt tokens, or `None` before a turn has finished (or on
    /// a row that predates the column). A head that attaches after a restart shows
    /// the context from this rather than waiting for a turn.
    pub context_tokens: Option<u64>,
    /// The last turn's cached tokens, for the cache %. `None` with `context_tokens`.
    pub context_cached: Option<u64>,
    /// **The ledger this box counted for the same prompt** — the other half of the
    /// measurement [`StoredSession::context_tokens`] is one side of.
    ///
    /// The ratio between the two is what turns a provider's window into a ledger
    /// size, and a ratio needs BOTH numbers from the same prompt. Recovering it from
    /// a provider count alone silently pairs that count with today's ledger, which is
    /// only the same conversation if nothing has been appended since — and at a
    /// resume it usually is not. `None` for a row written before this column existed,
    /// which is *unverifiable* rather than wrong: such a pair is refused rather than
    /// guessed at, and the next turn measures afresh.
    pub context_ledger: Option<u64>,
    /// **The provider this session was switched to**, `name` or `name/model`, or
    /// `None` for a session that never chose one — which resumes on the daemon's
    /// own default, as it always did. `Some("local")` is the deliberate switch
    /// BACK to the local server, kept distinct from `None` because the two answer
    /// different questions at a resume: *no opinion* against *the local server,
    /// by name*.
    pub provider_choice: Option<String>,
    /// **The window this session plans its compaction against**, in tokens — resolved when
    /// it opened, or when it was spawned.
    ///
    /// Written down because the number was otherwise knowable only at the moment it was
    /// used: `/props` on the daemon's own server, or a catalogue row for a cloud model. A
    /// child's window belongs to the model that ANSWERS it rather than to its parent's, and
    /// the measurement behind that rule is this column's reason: three children of one
    /// session ran to 911,522 / 910,486 / 911,708 tokens **with no compaction item in their
    /// logs at all**, because their wall was computed from the parent's cloud window. A
    /// reader asking *why did this session run past its wall* can only answer it from here
    /// once the process that resolved the number is gone.
    ///
    /// `None` is **nobody recorded one**, and it is not the same fact as a large window:
    /// every check that would compact reads `let Some(window) = …` and is skipped, so a
    /// `None` here says the record cannot answer rather than that the session had room.
    pub context_window: Option<u64>,
    /// **The pair that stood this session's automatic compaction down** —
    /// `(resident, after)`, both LEDGER tokens — or `None` when the no-progress guard
    /// has never fired for it.
    ///
    /// On the row because the guard's old fact was `auto_compact = false` in the
    /// daemon's memory: it died with the process, so a restart silently re-armed the
    /// looping the guard existed to stop — and, worse, the same restart was the ONLY
    /// thing that cleared it, so a wedged session came back wedged with nothing saying
    /// why. MEASURED 2026-10-09, session `s-1789919514688401228`: one `compacting:`
    /// line in the whole log, then thirty provider 400s (1,048,624 tokens against a
    /// 1,048,576 limit, attributed first to `monitor` and last to `todo check`), none
    /// of them recovered. The pair says what the guard saw, in the units it decided
    /// in, so the next daemon — and the operator, on `/status` — read the finding
    /// rather than inferring it from a silence.
    ///
    /// `None` is *the guard has not fired*, which is not the same fact as
    /// `auto_compact` being on: a `--no-auto-compact` session has the flag off and
    /// this pair absent, and the two readers that care (the settings row, the
    /// resident line a head draws) keep them apart on purpose.
    pub auto_compact_stood_down: Option<(u64, u64)>,
}

/// **One job's row, as the session store keeps it** — the durable half of the process table.
///
/// The daemon's job table lives in the process host and dies with it, so a handle the pane
/// listed a second ago answered `no job ... here` to the next daemon — *about a job whose
/// ending was written down*. What the store holds is what the daemon cannot re-derive: that a
/// job ran, what it was, and how it ended.
///
/// **Two writes, one row.** A job is on disk from the moment it is backgrounded (`state` is
/// the listing's own word, `running` while it has not ended), and the settlement updates that
/// same row; so a job still running when the daemon dies is on disk, and one that ended is not
/// lost because nobody was watching.
///
/// **A `running` row here is a job the daemon was watching when it died — not a claim that it
/// runs now.** Only the live process host can say that. This row answers what *happened*, which
/// is why a reader that needs *is it running* asks the host first and reads this second.
///
/// `redirect` is here for R41's reason: a job whose output went to a file has a window that is
/// empty by construction, and the path is the only thing that can say where to read instead.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JobRecord {
    /// The handle `bash background: true` handed back, and the one `job_list` prints.
    pub handle: String,
    /// What was run, verbatim — the row the model asked for and the operator reads.
    pub command: String,
    /// Who backgrounded it: the listing's own phrase for it — *you asked for this to run in the
    /// background*, *the RUNTIME moved this to the background*. **Empty means *not recorded
    /// here***, which is why it is not overwritten by a write that does not know it; see
    /// [`Store::put_job`].
    pub how: String,
    /// The listing's own word for how it ended, or `running` while it has not.
    pub state: String,
    /// Bytes produced, all streams together.
    pub produced: u64,
    /// Wall time from spawn to settlement.
    pub elapsed_ms: u64,
    /// Where its output went, when that was not this daemon's window (R41).
    pub redirect: Option<String>,
}

/// **What kind of merge an entry is, out of a closed set** — the queue's priority.
///
/// The operator's ask, in their words: *"we need a gated merge to main, and worktree
/// cleanup. for this we might need a merge queue. look how flowy does it - it has a nice
/// queue with priorities and dependencies."*
///
/// A closed set rather than a number, for the reason flowy's `category` is one and the
/// reason [`Verbosity`]'s ladder is one list: a priority that is a free integer is a
/// priority nobody can count or route on, and `3` and `high` and `urgent` are three
/// populations that each look like a confident answer. The set is held closed by the
/// column's own vocabulary, and a word outside it is a refusal rather than a guess.
///
/// **Two rungs, and the order is the rule.** An operator's urgent entry jumps subagent
/// work — that is the whole of the scheduling, and it is why there are exactly two: a
/// third rung would need a sentence about what the queue does differently with it, and
/// there is not one. Ties inside a rung break by age, which is the queue's own `created_ms`
/// and not a second number to keep in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergePriority {
    /// **The operator's entry.** It jumps every subagent entry, whatever the age.
    Urgent,
    /// **A subagent's entry.** It waits behind an operator's urgent entry and ahead of
    /// nothing else; inside the rung it is oldest-first.
    Subagent,
}

impl MergePriority {
    /// **Every priority, in the order the queue climbs** — the one list, the one answer.
    ///
    /// The seeding, the parse and the ordering all ask *which of the values is this*, and a
    /// second list is a second answer. `ALL[0]` is the highest priority: the queue sorts by
    /// position in this list, so a rung added here is reachable by every reader and a rung
    /// outside it is reachable by none.
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

/// **Where an entry is in its life, out of a closed set** — the queue's state.
///
/// The states are the queue's own vocabulary, and the moves between them are the state
/// machine the daemon serves. A state that is not on this list is a state the queue cannot
/// draw, and a row that says `waiting` while its gate job is dead is the lie the pane would
/// draw — which is why `Stale` exists rather than a `Waiting` that means two things.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    /// **In the queue, not yet taken.** Ready to be taken once its `needs` have all
    /// `Landed`; until then it is listed with the dependencies it is waiting on, not
    /// dropped.
    Waiting,
    /// **The daemon has it and is working** — rebasing at the tip and running the gate.
    /// A `Taken` row on disk is a job the daemon was running when it died, exactly as a
    /// `running` job row is: the live process answers *is it running now*, and this row
    /// answers *what happened*.
    Taken,
    /// **Merged to main.** The worktree is removed and the branch deleted; the row keeps
    /// the tip it landed at, which is the base its dependents rebase onto.
    Landed,
    /// **The gate failed.** The worktree STAYS, with the reason on the row: removing a
    /// tree after a failed test destroys the evidence.
    Failed,
    /// **A rebase conflict.** The worktree STAYS, with the reason on the row. A conflict
    /// is reported, never auto-resolved: the queue's job is to notice, not to guess.
    Conflict,
    /// **The gate job died with the daemon.** It must not come back as `Waiting` — a row
    /// that says `waiting` while its job is dead is a lie the pane would draw. It is
    /// listed with its reason and waits for a re-enqueue rather than a silent retry.
    Stale,
    /// **A PERSON rejected it** — the operator's verdict, not a gatekeeper's.
    ///
    /// The operator's ask, in their words: *"i want to be able to approve / veto / delete"*.
    /// A refusal by a reviewer and a rejection by the operator are two different facts and
    /// the pane must not draw them the same way: `Failed` is *the gate or the reviewer said
    /// no*, and this is *somebody decided*, which is why it is a state of its own rather
    /// than a `Failed` with a sentence on it. A veto is never red (the operator's ask:
    /// *"a vetoed entry must not draw as red"*) — nothing about the branch is broken; it
    /// was judged, by the one seat whose judgement ends the question.
    ///
    /// **It is terminal, and it is not `Failed`'s terminal.** The queue never takes it (its
    /// scheduling only ever looks at `Waiting`), so no gatekeeper is asked again and no gate
    /// runs. The way back is a person's, one state over: `mergequeue::approve` reverses the
    /// decision it was made by, and `mergequeue::remove_merge_entry` forgets the entry.
    Vetoed,
}

impl MergeState {
    /// **Every state, in the order the life runs** — the one list.
    pub const ALL: [MergeState; 7] = [
        MergeState::Waiting,
        MergeState::Taken,
        MergeState::Landed,
        MergeState::Failed,
        MergeState::Conflict,
        MergeState::Stale,
        MergeState::Vetoed,
    ];

    /// A state the queue will never leave on its own: `Landed` is merged and cleaned up,
    /// and `Failed`/`Conflict`/`Stale`/`Vetoed` are parked with their reason for a person to
    /// answer. Only `Waiting` and `Taken` still move.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            MergeState::Landed
                | MergeState::Failed
                | MergeState::Conflict
                | MergeState::Stale
                | MergeState::Vetoed
        )
    }

    /// The state a stored word names, if it names one.
    pub fn parse(stored: &str) -> Option<MergeState> {
        Self::ALL.into_iter().find(|s| s.as_str() == stored)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MergeState::Waiting => "waiting",
            MergeState::Taken => "taken",
            MergeState::Landed => "landed",
            MergeState::Failed => "failed",
            MergeState::Conflict => "conflict",
            MergeState::Stale => "stale",
            MergeState::Vetoed => "vetoed",
        }
    }
}

/// **One entry in the merge queue, as the session store keeps it** — the durable half of
/// the queue the daemon serves.
///
/// The operator's ask, in their words: *"merge queue is 2 - together with persistence and
/// harnessd thread that serves it"* and *"it has to be durable so live in the session db."*
///
/// One row per entry, in `sessions.db` beside the job history and the todo list, because a
/// queue that lives in the daemon's memory is a queue that dies with the daemon — and the
/// daemon is the one thing that is restarted. The row is written when the entry is
/// enqueued and updated as it moves, so a daemon that dies mid-merge comes back to a queue
/// that says what happened rather than an empty one that says nothing.
///
/// **The daemon is the only writer; a head never writes one.** The row is the daemon's
/// account of the merge, the way the `job` row is the daemon's account of a process: a head
/// that could write it could mark its own branch `Landed` without a gate, which is the whole
/// thing the queue exists to prevent. The store does not enforce the writer — it cannot
/// know who a connection is — and the daemon is the only caller, which is the guarantee.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MergeEntry {
    /// The id the enqueuer minted, and the one the queue's events name.
    pub id: String,
    /// **The session that enqueued it** — the subagent that finished the branch, or the
    /// operator's session for an urgent entry. Metadata about the entry's origin, not a
    /// foreign key: the entry is about the branch, and a session that is deleted does not
    /// un-merge a branch that still needs merging.
    pub session_id: String,
    /// The branch to merge, and the one checked out in the entry's worktree.
    pub branch: String,
    /// **The SHA the entry was written against** — the tip of main when the branch was cut.
    /// It goes stale the moment anything else lands, which is why the queue rebases onto
    /// the *current* tip rather than this: two branches green in isolation are not green
    /// together, and this column is the record of where the entry started, not where it
    /// lands.
    pub base_sha: String,
    /// The queue's priority, out of [`MergePriority`]'s closed set.
    pub priority: MergePriority,
    /// **The entries this one depends on, by id.** It is not taken until every one of them
    /// has `Landed`, and when they have, its base becomes the landed tip rather than the
    /// stale `base_sha` it was written against.
    pub needs: Vec<String>,
    /// Where the entry is, out of [`MergeState`]'s closed set.
    pub state: MergeState,
    /// **The brief the child was given, verbatim** — the prompt the branch was produced
    /// under, carried with the entry because the reviewer needs it and nothing else can
    /// supply it.
    ///
    /// The gatekeeper's whole protocol is brief-first: its `ReviewRequest` has no field for
    /// the child's own report, so the reviewer starts from the ask and reads the artifact
    /// against it. That makes the ask part of the entry rather than a fact about a session
    /// that may be gone by the time the review happens — a reviewer handed *"review branch
    /// `agent/x`"* with no ask would be reviewing the code against nothing.
    ///
    /// Empty is a real value and it means *nobody recorded one*: an entry enqueued by a
    /// door that had no brief (an operator's urgent entry, one day) says so rather than
    /// inventing a sentence, and the reviewer refuses by name rather than reviewing against
    /// a blank.
    pub brief: String,
    /// **The reason for the state, in the queue's own words.** Empty while `Waiting` with
    /// no unmet dependencies; the unmet dependencies while `Waiting` with some; the gate's
    /// failure while `Failed`; the conflict while `Conflict`; the dead job while `Stale`;
    /// the landed tip while `Landed`. A state without its reason is a row the pane draws
    /// and the operator cannot read.
    pub evidence: String,
    /// When the entry was enqueued, Unix ms. Ties inside a priority break by this, oldest
    /// first.
    pub created_ms: u64,
    /// When the entry last moved, Unix ms.
    pub updated_ms: u64,
    /// **Where the branch is checked out**, when it is — the worktree the daemon rebases in
    /// and the one it removes on `Landed`. `None` while the worktree does not exist yet
    /// (the enqueuer's half, which is `task_start`'s to create) and after it is removed.
    pub worktree: Option<String>,
    /// **The tip the entry landed at**, set when it moves to `Landed`. It is the base its
    /// dependents rebase onto: a dependent's `effective_base` is the `landed_sha` of the
    /// last of its dependencies to land, which is the current tip of main because the queue
    /// is serial.
    pub landed_sha: Option<String>,
}

/// **One row of the merge queue, before the closed sets are parsed** — the shape the
/// `SELECT` hands back and the shape [`merge_entry_from_raw`] turns into a [`MergeEntry`].
///
/// The two-step read exists for one reason: `priority` and `state` are the closed sets' own
/// words, and a word that is not on the list is a row written by a build this one does not
/// know. Parsing inside the `query_map` closure would force that refusal into a
/// `rusqlite::Error`, which is the wrong error for a row the store can read fine; parsing
/// after the read lets it be a [`StoreError::Corrupt`] that says which word was not on the
/// list, which is the difference between *the file is damaged* and *a guess was made*.
#[derive(Debug)]
struct RawMergeEntry {
    id: String,
    session_id: String,
    branch: String,
    base_sha: String,
    priority: String,
    needs_json: String,
    state: String,
    brief: String,
    evidence: String,
    created_ms: i64,
    updated_ms: i64,
    worktree: Option<String>,
    landed_sha: Option<String>,
}

/// The column order the merge-queue `SELECT`s use, read into a [`RawMergeEntry`].
fn merge_entry_raw_from_row(r: &rusqlite::Row) -> rusqlite::Result<RawMergeEntry> {
    Ok(RawMergeEntry {
        id: r.get(0)?,
        session_id: r.get(1)?,
        branch: r.get(2)?,
        base_sha: r.get(3)?,
        priority: r.get(4)?,
        needs_json: r.get(5)?,
        state: r.get(6)?,
        brief: r.get(7)?,
        evidence: r.get(8)?,
        created_ms: r.get(9)?,
        updated_ms: r.get(10)?,
        worktree: r.get(11)?,
        landed_sha: r.get(12)?,
    })
}

/// Turn a [`RawMergeEntry`] into a [`MergeEntry`], parsing the closed sets.
///
/// A `priority` or `state` word that is not on the list is a [`StoreError::Corrupt`] that
/// names the word, for the reason [`RawMergeEntry`] exists: a row that names a priority this
/// build does not know is a row written by a newer build, and reading it as the lowest rung
/// would be a silent misread of the queue's own vocabulary.
fn merge_entry_from_raw(raw: RawMergeEntry) -> Result<MergeEntry> {
    // **The closed sets are parsed first, and the word is named.** A refusal that printed the
    // whole row would make the reader find the word; the word is the whole of the answer, and
    // parsing before the moves below is also what lets the moves be plain moves.
    let priority = MergePriority::parse(&raw.priority).ok_or_else(|| {
        StoreError::Corrupt(format!(
            "merge_queue priority {:?} is not in the closed set {:?}",
            raw.priority,
            MergePriority::ALL
        ))
    })?;
    let state = MergeState::parse(&raw.state).ok_or_else(|| {
        StoreError::Corrupt(format!(
            "merge_queue state {:?} is not in the closed set {:?}",
            raw.state,
            MergeState::ALL
        ))
    })?;
    let needs = serde_json::from_str(&raw.needs_json)?;
    Ok(MergeEntry {
        id: raw.id,
        session_id: raw.session_id,
        branch: raw.branch,
        base_sha: raw.base_sha,
        priority,
        needs,
        state,
        brief: raw.brief,
        evidence: raw.evidence,
        created_ms: raw.created_ms as u64,
        updated_ms: raw.updated_ms as u64,
        worktree: raw.worktree,
        landed_sha: raw.landed_sha,
    })
}

/// **The reviewer's verdict on one entry, as the session store keeps it** — the durable half
/// of the gate the queue is under.
///
/// The operator's ask, in their words: *"we need a gatekeeper - a subagent that does code
/// review on merge queue"*. The verdict is what stops the fast-forward, so it has to outlive
/// both the reviewer's turn and the daemon: a verdict that lived in the daemon's memory would
/// make a restarted daemon either re-review everything or land something nobody reviewed.
///
/// **Two writes, one row**, the [`JobRecord`] shape and for the same reason: the row is written
/// when the review is ASKED FOR — `decision` is `None` — and updated once when the verdict comes
/// back. The two are different facts, and a queue that could not tell them apart would either
/// re-ask for ever or wait for a verdict nobody was ever asked for.
///
/// **`decision` is a word and not an enum here.** The closed set (`accept`, `reject`,
/// `needs_human`) belongs to the gatekeeper, which lives above this crate; the store keeps the
/// word the way it keeps `merge_queue.state` and `merge_queue.priority`, and the reader that
/// needs the closed set parses it where the two vocabularies meet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRecord {
    /// The entry this verdict is about — the queue's own id, so the two tables are joined by
    /// the one thing they share.
    pub entry_id: String,
    /// **The session that reviewed it**, which is the reviewer's own — a session the daemon
    /// serves, and the one a person attaches to when they want to read the argument.
    pub session_id: String,
    /// The branch judged, echoed from the request so a verdict names what it is about even if
    /// the entry row is gone.
    pub branch: String,
    /// The base SHA judged, echoed for the same reason.
    pub base_sha: String,
    /// When the reviewer was asked, Unix ms.
    pub asked_ms: u64,
    /// When the verdict came back, Unix ms. `None` while the review is outstanding.
    pub answered_ms: Option<u64>,
    /// **The decision, in the gatekeeper's own words** — `accept`, `reject` or `needs_human`.
    /// `None` is *no verdict yet*, which is not the same fact as a rejection: the queue waits
    /// for the first and refuses to land on the second.
    pub decision: Option<String>,
    /// **How many attempts have FAILED since the last restart** (v20), and the whole of the
    /// queue's bound: the queue asks again while this is under its own ceiling, and stops when
    /// it is not. `0` on a fresh ask, and `0` again after a person restarts one.
    pub attempts: u32,
    /// **When the last attempt failed** (v20), Unix ms, or `None` when no attempt has failed
    /// since the last restart. This is the clock the queue's backoff reads, and it is the fact
    /// that tells *an attempt is in flight* from *an attempt came back badly* — the two are the
    /// same `decision: None` otherwise, and a restart has to tell them apart.
    pub failed_ms: Option<u64>,
    /// **The last attempt's failure, verbatim** (v20) — the provider's own words, or the reason
    /// a child could not be started. **Not a verdict**: `decision` stays `None`, and this is what
    /// a person reads on the entry's row so the restart is a decision about something legible
    /// rather than about the word *failed*.
    pub failure: String,
    /// The reviewer's reasons, in its own words.
    pub reasons: Vec<String>,
    /// The files the reviewer read.
    pub files: Vec<String>,
    /// The commands the reviewer ran.
    pub commands: Vec<String>,
}

/// **One row of `merge_review`, before the JSON columns are parsed.** See [`RawMergeEntry`]
/// for why the read is two steps: there is nothing to refuse here yet, and the shape exists
/// because the `SELECT` column order should be written down once.
#[derive(Debug)]
struct RawReviewRecord {
    entry_id: String,
    session_id: String,
    branch: String,
    base_sha: String,
    asked_ms: i64,
    answered_ms: Option<i64>,
    decision: Option<String>,
    attempts: i64,
    failed_ms: Option<i64>,
    failure: String,
    reasons_json: String,
    files_json: String,
    commands_json: String,
}

/// The column order the `merge_review` `SELECT`s use, read into a [`RawReviewRecord`].
fn review_raw_from_row(r: &rusqlite::Row) -> rusqlite::Result<RawReviewRecord> {
    Ok(RawReviewRecord {
        entry_id: r.get(0)?,
        session_id: r.get(1)?,
        branch: r.get(2)?,
        base_sha: r.get(3)?,
        asked_ms: r.get(4)?,
        answered_ms: r.get(5)?,
        decision: r.get(6)?,
        attempts: r.get(7)?,
        failed_ms: r.get(8)?,
        failure: r.get(9)?,
        reasons_json: r.get(10)?,
        files_json: r.get(11)?,
        commands_json: r.get(12)?,
    })
}

fn review_from_raw(raw: RawReviewRecord) -> Result<ReviewRecord> {
    Ok(ReviewRecord {
        entry_id: raw.entry_id,
        session_id: raw.session_id,
        branch: raw.branch,
        base_sha: raw.base_sha,
        asked_ms: raw.asked_ms.max(0) as u64,
        answered_ms: raw.answered_ms.map(|v| v.max(0) as u64),
        decision: raw.decision,
        attempts: raw.attempts.max(0) as u32,
        failed_ms: raw.failed_ms.map(|v| v.max(0) as u64),
        failure: raw.failure,
        reasons: serde_json::from_str(&raw.reasons_json)?,
        files: serde_json::from_str(&raw.files_json)?,
        commands: serde_json::from_str(&raw.commands_json)?,
    })
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
    /// **WHO PUT IT ON THE BOARD.** The operator's ruling is that there is ONE list — *"the existing
    /// getter should return mine and yours, and the rest is also the same. the only difference is who
    /// created and that is it"* — so the two authors share a list and a format, and this field is the
    /// whole of the difference between them.
    ///
    /// `serde(default)` and `Model`, so every list already in a store — and every list a head sends
    /// that predates this field — reads back as the model's, which is what it was.
    #[serde(default)]
    pub by: TodoBy,
    /// **What this row is WAITING FOR**, when it is waiting for something rather than simply
    /// being undone.
    ///
    /// The operator's own framing, 2026-10-06: *"if you are telling me 'job ends and i do this
    /// and that' then 'this and that' is a todo item, which is conditioned by job status
    /// (end)"*. So the condition is not a note ABOUT the row — it is what makes the row *due*,
    /// and the row is the intent.
    ///
    /// **An enum rather than a string, and tagged**, so a kind can be added without a new
    /// field on every row and a reader that does not know one can SAY SO rather than misread
    /// it: a condition nobody can evaluate must never read as *met*, because a row that fires
    /// immediately makes the model act on something that did not happen — worse than a row
    /// that never fires, since the second is visible and the first is not.
    ///
    /// **It is EVALUATED, not observed** — the operator, 2026-10-06: *"a todo conditioned on
    /// job end, and then i restart head and harnessd. once server is back it should fire - job
    /// is gone"*. A firing that needed a live `JobSettled` would be lost by exactly that
    /// restart; a firing that asks *is this handle still running here* survives it, because a
    /// job that is not running — **including one this session has never heard of, which is what
    /// a handle looks like after a restart** — is a job that ended. The two are reported apart
    /// (ended, with its word and its output; or unknown, so a reader knows the result is not
    /// here) because *"if you want to distinguish - you either follow the job result up
    /// manually (which is more robust) if next steps depend on it or just do your things if it
    /// was just a timeline"*.
    ///
    /// **A POSTPONED row KEEPS its condition and does not fire while it is postponed.** Nothing
    /// here is cleared and nothing is guessed at: the condition is left exactly as it is, the
    /// evaluator skips the row while the operator has it set aside, and lifting the postponement
    /// hands the same question back to the evaluator. That is what makes the state reversible
    /// rather than a quiet way to drop a firing — a postponed row that had lost its handle would
    /// be a condition nobody could ever answer.
    ///
    /// `None` is the ordinary row — a thing to do, not a thing to do *when* — and
    /// `serde(default)` makes every row already in a store read back that way.
    #[serde(default)]
    pub when: Option<TodoCondition>,
}

/// **What a row waits on.** See [`TodoItem::when`] for what evaluating one means.
///
/// Internally tagged (`kind`), so the wire is self-describing, a new kind is a new variant
/// rather than a new field on every row, and a reader can tell *a condition I do not know*
/// from *no condition at all*.
///
/// **One variant, and the next is named when something can EVALUATE it.** A condition kind is
/// only as good as the thing that can answer it: a `time` variant with nothing holding a clock,
/// or a `port` variant in a layer that cannot see the network, is a row that waits for ever
/// while looking like a row that is waiting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoCondition {
    /// **Due when this job is no longer running** — the handle `bash background: true` and
    /// `task` hand back, and the one `job_list` prints.
    ///
    /// **Absence is the condition, and that is what makes it survive a restart.** A handle the
    /// session knows nothing about reads as *gone*, not as *not yet* — the job ended before the
    /// daemon came back, or the handle is wrong, and both are reported rather than guessed at.
    Job { handle: String },
}

/// Who authored a todo. See `TodoItem::by`.
///
/// Three authors, and the third is a STRING: the operator's ruling — *"yes - i want parent
/// agents to be able to create todos for subagents. throught tree author - (Parent
/// <session-id-of-parent>)"* — names the author as `Parent <session-id>`, the FULL session id and
/// not a short form or a display name, because a child reading its own board has to be able to
/// tell what it decided from what it was told and by whom.
///
/// **Serialised as one bare string, and that is deliberate.** `by` reads in `sqlite3` as
/// `"model"` and `"operator"` today, and the parent's rows sit beside those as
/// `"Parent s-…"` — a `#[serde(rename_all)]` tuple variant would spell `{"parent": "…"}`
/// instead, putting an object where every other author is a word. Custom impls on purpose, in
/// both places that own a copy of this vocabulary (here and `letibot_sessionlog::event::TodoBy`):
/// an unknown word is refused NAMING the word, the house rule, rather than read as the model's
/// (`TodoItem::by`'s `#[serde(default)]` still covers a list that predates the field entirely).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TodoBy {
    /// The model wrote it with the `todo` tool.
    #[default]
    Model,
    /// **The operator wrote it in the pane.** It is the same list, the same statuses and the same
    /// tool — the model can mark the operator's item done, and the nag in `harness.rs` picks it up
    /// like any other, because it reads the board and the board no longer cares who wrote a row.
    Operator,
    /// **A PARENT session wrote it on a CHILD's board** — the string is the author exactly as the
    /// operator specified it, `Parent <full parent session id>`, built by [`TodoBy::parent_of`]
    /// and never taken from a tool call: the daemon knows which session is calling, so it stamps
    /// the author itself and a model cannot claim another one.
    ///
    /// Same board, same statuses, same nag as the other authors. What differs is the WRITE: a
    /// parent's `todo_write` with a `target` is an UPSERT scoped to this authorship (rows matched
    /// by exact text), never the whole-list replace the model's own half takes — see
    /// `TodoBoard::upsert_parent`.
    Parent(String),
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
        }
    }
}

impl<'de> serde::Deserialize<'de> for TodoBy {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<TodoBy, D::Error> {
        let word = String::deserialize(d)?;
        match word.as_str() {
            "model" => Ok(TodoBy::Model),
            "operator" => Ok(TodoBy::Operator),
            // `Parent …` is the one open spelling, and the prefix is the author's own marker:
            // anything else is a word this reader does not know, refused by name rather than
            // read as the model's the way a catch-all would.
            other if other.starts_with("Parent ") => Ok(TodoBy::Parent(other.to_string())),
            other => Err(serde::de::Error::custom(format!(
                "`{other}` is not a todo author: model, operator, or `Parent <session-id>`"
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
    /// **Set aside by the OPERATOR — the row persists, and it stops asking.**
    ///
    /// The operator's ask: *"can we handle postponed todo item properly? i.e. they persist but
    /// without nag and with some counter visible to me"*. So this is a fourth word and not a
    /// quieter spelling of one of the three: `Completed` is work that is answered, `Postponed` is
    /// work still owed and deliberately not being asked for. The row stays on the board and the
    /// model still sees it (marked), while the idle check stays silent about it — the `[todo
    /// check]` nudge, the firing of a condition it carries, and the `[todo]` notice.
    ///
    /// **It is the operator's act and not the model's.** `todo_write` still takes the three other
    /// words: a model that could postpone its own row would have a way to silence the check that
    /// exists to stop it abandoning a plan, and that is the one thing the check must not offer.
    /// Lifting it is the same act, spelled the other way — see the head's `/todo resume N`.
    Postponed,
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
    /// **Where this store lives**, so a second connection can be opened to it — see
    /// [`Store::path`]. `None` for `open_in_memory`, which is a store with no file to reach.
    path: Option<std::path::PathBuf>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_connection(Connection::open(path)?, Some(path.to_path_buf()))
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, None)
    }

    /// **The file this store is**, when it has one.
    ///
    /// A second connection is a normal thing to want here and not a workaround: the watcher
    /// threads settle jobs on their own thread, a `rusqlite::Connection` is `Send` and not
    /// `Sync`, and re-opening the file is the only way to write from a thread that must not
    /// be handed the harness's own connection. `from_connection`'s own comment already says
    /// two connections exist by design — the worker writes rows and a second answers a head —
    /// and WAL plus the five-second `busy_timeout` are what make a third harmless.
    ///
    /// `None` for an in-memory store, which nobody else can reach. That is an honest
    /// absence rather than an error: a store with no file has no path to give, and a caller
    /// that needs one must say so.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// **How long a connection waits for the write lock before giving up** — MEASURED, not chosen.
    ///
    /// Five seconds was the right number when a box held a "third connection": the doc above
    /// says WAL plus this timeout is what made a third harmless. On 2026-10-10 the same box
    /// held **478 sessions across 48 projects in one 2.1 GB store, ~11 daemons and dozens of
    /// connections** wanting the single write lock SQLite allows — and five seconds stopped
    /// being enough. Four CHILDREN of one session died mid-work on
    /// `store: row N (…#t0.N): sqlite: database is locked`, and one of them was the child sent
    /// to fix exactly this; the same sentence appears for the queue (*"the queue could not be
    /// served"*) and for a live session's own row.
    ///
    /// A transcript append that waits twenty seconds and then succeeds is strictly better than
    /// one that fails and takes the turn with it: this timeout is what BOUNDS the wait, and the
    /// alternative to waiting is losing the work.
    ///
    /// **This is the stopgap, not the answer.** The structural fix is the shared store splitting
    /// the transcript tables out per session, leaving the queue, the corpus and the session index
    /// in the one file; and the retry discipline for the writes that cannot simply wait is its own
    /// change (`ed07d98` is its first half). Neither is a reason to leave children dying in the
    /// meantime.
    pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    fn from_connection(conn: Connection, path: Option<std::path::PathBuf>) -> Result<Self> {
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
        conn.busy_timeout(Self::BUSY_TIMEOUT)?;
        let store = Store { conn, path };
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
                self.conn
                    .execute_batch("ALTER TABLE adjudication ADD COLUMN oracle_reading TEXT")?;
            }
        }
        if from < 12 {
            // v12: **the ledger that `context_tokens` was measured against.**
            //
            // `Config::ledger_scale` is recovered at startup by pairing the stored
            // provider count with the ledger the daemon has just rebuilt, which is
            // the same conversation only if nothing was appended between the measure
            // and the restart — and at a resume it usually is not. Measured
            // 2026-10-02: a store holding `context_tokens = 940211`, measured on a
            // ~950k-token conversation, was paired with a rebuilt ledger of
            // 1,486,369, giving a ratio of 0.63 where the truth was 1.016. The
            // window came out 1,580,888 instead of ~1,015,628 — so a session 1.46M
            // tokens deep planned against a window it was already far past, never
            // compacted, and died at the provider's 400 on every attempt.
            //
            // **NULL on every existing row, and not backfilled.** A backfill would
            // have to assert that a count from whenever that row was written
            // measures the transcript as it stands now, and that assertion IS the
            // bug being fixed. NULL reads as "unverifiable", the recovery refuses
            // such a pair instead of guessing at it, and the next turn writes a real
            // one. The cost is bounded and lands the safe way: an unverified session
            // falls back to the unscaled window, which compacts EARLY rather than
            // late (see `Config::tokens_are_unscaled`).
            //
            // Idempotent for the same reason v6 is: the fixtures build a current
            // store and walk the version back, so the column can already be there.
            let has: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('session') WHERE name = 'context_ledger'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn
                    .execute_batch("ALTER TABLE session ADD COLUMN context_ledger INTEGER")?;
            }
        }
        if from < 13 {
            // v13: the provider this session was switched to. The operator switched a session
            // to a cloud provider, restarted the daemon, and the session came back on the CLI
            // default — `set_provider` was in-memory and the session row had nowhere to record
            // the choice. NULL in every existing row is exactly "never switched": such a
            // session resumes on the daemon's own default, which is what it always did.
            // Idempotent for the same reason v6 and v12 are: a fixture can walk the version
            // back over a store that already has the column.
            let has: bool = self
                .conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('session') WHERE name = 'provider_choice'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn
                    .execute_batch("ALTER TABLE session ADD COLUMN provider_choice TEXT")?;
            }
        }
        if from < 14 {
            // v14: **the window a session plans its compaction against.**
            //
            // The rule that produced it — a child's window belongs to the model that answers
            // it and never to its parent's — was fixed at the spawn (`child_window_refusal`,
            // `subagent_model`), and the number itself lived only in the spawn line, which is
            // not durable. So a child that ran past its wall left nothing behind that said
            // which wall it had been planning against. See `StoredSession::context_window`
            // for the measurement.
            //
            // **NULL in every existing row, and deliberately not backfilled.** A closed-form
            // guess is available — a daemon's own `/props`, or the catalogue row for the model
            // the session records — and every one of them would write TODAY's number onto a
            // row describing a session that ran then, which is the same class of lie the
            // column exists to stop, and in the direction that looks authoritative. *Nobody
            // recorded one* is the true state of a pre-v14 row.
            //
            // Idempotent for the same reason v6, v12 and v13 are: a fixture walks a current
            // store backwards, so the column can already be here.
            let has: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('session') WHERE name = 'context_window'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn
                    .execute_batch("ALTER TABLE session ADD COLUMN context_window INTEGER")?;
            }
        }
        if from < 15 {
            // v15: **the job history** — see [`JobRecord`] for what it is for.
            //
            // A table rather than a column, because a session has many jobs and each is its own
            // row: the `todo` shape (one JSON blob per session) would make *one handle's fate* a
            // whole-list read, and the reader that wants it is answering a question about one
            // handle.
            //
            // `IF NOT EXISTS` for the same reason v6, v12, v13 and v14 are idempotent: a fixture
            // walks a current store backwards, so the table can already be here.
            self.conn.execute_batch(
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
            // v16: **the merge queue** — see [`MergeEntry`] for what it is for.
            //
            // A table rather than a column, for the reason v15's job history is one: the
            // daemon has many entries and each is its own row, and the reader that wants one
            // entry's fate is not reading the whole queue.
            //
            // `session_id` is a plain column here and not a foreign key, and that is the
            // deliberate difference from `job`: an entry is about a branch, and a session that
            // is deleted does not un-merge a branch that still needs merging.
            //
            // `IF NOT EXISTS` for the same reason v6, v12, v13, v14 and v15 are idempotent: a
            // fixture walks a current store backwards, so the table can already be here.
            self.conn.execute_batch(
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
            // v17: **the ask a branch was produced under.**
            //
            // `merge_queue.brief` — the ask the child was given, carried on the entry because
            // the gatekeeper reviews the artifact AGAINST the ask and nothing else can supply
            // it once the child's session is gone. Added with a guard rather than a bare
            // `ALTER TABLE`: a fixture walks a current store backwards by dropping columns and
            // lowering the version, and a fixture that dropped only some of them would
            // otherwise fail here with "duplicate column name" — which reads as corruption
            // rather than as the idempotence every other step in this function has.
            let has_brief: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('merge_queue') WHERE name = 'brief'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has_brief {
                self.conn.execute_batch(
                    "ALTER TABLE merge_queue ADD COLUMN brief TEXT NOT NULL DEFAULT ''",
                )?;
            }
        }
        if from < 18 {
            // v18: **the reviewer's verdict** — see [`ReviewRecord`] and the table's own
            // comment in [`SCHEMA_SQL`]. A table rather than a column for the reason v16's
            // entry table is one: the queue and the review have different writers, and the
            // question a reader asks ("what did the gatekeeper say about this entry") is about
            // one row and not about the entry's state.
            //
            // `IF NOT EXISTS` for the same reason v6, v12, v13, v14, v15 and v16 are
            // idempotent: a fixture walks a current store backwards, so the table can already
            // be here.
            self.conn.execute_batch(
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
        if from < 20 {
            // v20: **the failed attempt, which is not a verdict** — see [`ReviewRecord`]'s
            // `attempts`/`failed_ms`/`failure` and the `merge_review` comment in [`SCHEMA_SQL`].
            //
            // Three columns rather than one, because the three answer different questions:
            // *may the queue ask again* (`attempts`), *has it waited long enough* (`failed_ms`),
            // and *what should a person read on the row* (`failure`, verbatim).
            //
            // Guarded rather than a bare `ALTER TABLE`, for the reason v17's `brief` is: a
            // fixture walks a current store backwards by dropping columns and lowering the
            // version, and a fixture that dropped only some of them would otherwise fail here
            // with "duplicate column name" — which reads as corruption rather than as the
            // idempotence every other step in this function has.
            // **The table first, in its v20 shape, `IF NOT EXISTS`** — the v5 and v16 arms'
            // posture, for their reason: a migrated store never runs `SCHEMA_SQL`, and a fixture
            // may arrive at this version with no table at all (the stood-down fixture builds a
            // bare v18 store and jumps straight here). On a real v18 store this is a no-op and
            // the guarded `ALTER`s below do the work; on a bare one it IS the work.
            self.conn.execute_batch(
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
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('merge_review') WHERE name = 'failure'")
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                // **Every existing row reads as *no failed attempt*.** A row written before
                // these columns existed has a `decision` and an `answered_ms` — it is a verdict,
                // and a verdict is not a failure. `attempts = 0` says the queue has not spent an
                // attempt on it, which is the true state of a row whose ask was answered.
                self.conn.execute_batch(
                    "ALTER TABLE merge_review ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
                     ALTER TABLE merge_review ADD COLUMN failed_ms INTEGER;
                     ALTER TABLE merge_review ADD COLUMN failure TEXT NOT NULL DEFAULT '';",
                )?;
            }
        }
        if from < 19 {
            // v19: **the pair that stood automatic compaction down** — see
            // [`StoredSession::auto_compact_stood_down`] for the wedge it answers.
            //
            // Two columns rather than one JSON blob, because the pair is two numbers a
            // reader compares (`did it shrink?`), and a reader comparing them should not
            // have to parse first. NULL in every existing row, and deliberately not
            // backfilled: a session that predates v19 never had the guard fire on it, and
            // *the guard has not fired* is the true state of such a row.
            //
            // Idempotent for the same reason v6, v12, v13 and v14 are: a fixture walks a
            // current store backwards, so the columns can already be here.
            let has: bool = self
                .conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('session') WHERE name = 'auto_compact_resident'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn.execute_batch(
                    "ALTER TABLE session ADD COLUMN auto_compact_resident INTEGER;
                     ALTER TABLE session ADD COLUMN auto_compact_after INTEGER;",
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

    /// **The transcript this one was forked from**, or `None` for a session's first.
    ///
    /// One indexed lookup rather than `load_transcript`, which reads every row and every
    /// token of a transcript to answer this one column. A resume walks the chain to put the
    /// conversation back on the screen, and the walk asks this once per step before it
    /// decides whether the parent's rows are needed at all.
    pub fn parent_of(&self, transcript_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT parent_transcript_id FROM transcript WHERE id = ?1",
                params![transcript_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
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
                      ORDER BY i.seq DESC LIMIT 1)
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
                    // **Index 19, after the stood-down pair at 17 and 18.** This list is
                    // read BY POSITION and both changes appended to it: the pair came from
                    // the compaction work, the last item from the pane that says how a
                    // child ended. The pair goes first because it is one fact in two
                    // columns — splitting it around an unrelated subquery would be a
                    // reading waiting to be shifted by the next person who appends here.
                    last_item: r
                        .get::<_, Option<String>>(19)?
                        .and_then(|j| serde_json::from_str(&j).ok()),
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
    ///
    /// **`ledger` is not decoration and must be passed whenever `tokens` is.**
    /// `tokens` is the PROVIDER's count and `ledger` is this box's for the same
    /// prompt; only the two together give a ratio, and a ratio recovered from the
    /// provider's half alone is one measured against a different conversation. Pass
    /// `None` for both when clearing. See [`StoredSession::context_ledger`].
    pub fn set_context(
        &self,
        id: &str,
        tokens: Option<u64>,
        cached: Option<u64>,
        ledger: Option<u64>,
    ) -> Result<()> {
        // `i64` on the wire: rusqlite's `ToSql` has no `u64`, and a token count
        // that does not fit an `i64` is not a prompt this box will ever send.
        let n = self.conn.execute(
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

    /// **Write the window this session plans its compaction against.** See
    /// [`StoredSession::context_window`].
    ///
    /// A setter of its own rather than a fifth parameter on [`Store::set_context`], because
    /// the two facts have different lifetimes and folding them together would say they were
    /// measured together: the context counts are replaced by **every turn**, and a window is
    /// a property of the session that changes only when the model underneath it moves.
    ///
    /// `None` clears it — what a session whose window nobody can name records.
    pub fn set_window(&self, id: &str, window: Option<u64>) -> Result<()> {
        // `i64` on the wire for the same reason `set_context` gives: rusqlite's `ToSql` has
        // no `u64`, and a window that does not fit an `i64` is not a window any server has.
        let n = self.conn.execute(
            "UPDATE session SET context_window = ?2 WHERE id = ?1",
            params![id, window.map(|w| w as i64)],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// **Write the pair that stood automatic compaction down.** See
    /// [`StoredSession::auto_compact_stood_down`].
    ///
    /// A setter of its own for the same reason [`Store::set_window`] has one: the two
    /// facts have different lifetimes, and folding them into another setter's
    /// parameter list would say they were written together. This pair is written
    /// ONCE — when the no-progress guard fires — and `None` clears it for the
    /// session-naming tests, not for the daemon: nothing in the daemon re-arms a
    /// guard that has fired, because the finding it recorded does not expire.
    pub fn set_auto_compact_stood_down(&self, id: &str, pair: Option<(u64, u64)>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE session SET auto_compact_resident = ?2, auto_compact_after = ?3 \
             WHERE id = ?1",
            params![id, pair.map(|(r, _)| r as i64), pair.map(|(_, a)| a as i64)],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// The provider this session was switched to, or `None` for one that never
    /// chose. Read rather than remembered for the reason [`Store::title`] is: a
    /// second connection can write the row while a harness holds an older idea.
    pub fn provider_choice(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT provider_choice FROM session WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .filter(|s: &String| !s.is_empty()))
    }

    /// Record the provider this session was switched to, or clear it with `None`.
    ///
    /// **The choice is a session's fact, not the daemon process's.** Before this
    /// column a switch lived in `Harness::provider` alone, so a restart brought the
    /// session back on the CLI default and the first turn quietly went to a model
    /// nobody chose for that conversation.
    pub fn set_provider_choice(&self, id: &str, choice: Option<&str>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE session SET provider_choice = ?2 WHERE id = ?1",
            params![id, choice],
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

    /// **Every job this session has recorded**, ordered by when its row last moved — see
    /// [`JobRecord`].
    ///
    /// `handle` breaks the tie, so two jobs written in the same millisecond do not come back in
    /// an order that changes between calls: a listing that reorders itself costs the model its
    /// prefix cache, which is the same reason the monitor registry orders by name.
    pub fn jobs(&self, session_id: &str) -> Result<Vec<JobRecord>> {
        let mut st = self.conn.prepare(
            "SELECT handle, command, how, state, produced, elapsed_ms, redirect
               FROM job WHERE session_id = ?1 ORDER BY updated_ms, handle",
        )?;
        let rows = st.query_map(params![session_id], |r| {
            Ok(JobRecord {
                handle: r.get(0)?,
                command: r.get(1)?,
                how: r.get(2)?,
                state: r.get(3)?,
                // `i64` out of SQLite and back into the `u64` the wire spells: rusqlite's
                // `FromSql` has no `u64`, and a byte count that does not fit an `i64` is not
                // a job this box will ever run — the same reading `Store::set_context` takes
                // for a token count.
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

    /// **Write one job's row, replacing that handle's last one — field by field.**
    ///
    /// An upsert rather than an append: the row is a job and not a log line, so a job has one
    /// state at a time and the settlement is the same job as the start. *The append-only record
    /// of every ending* is the session's own log, which is a different question from *where is
    /// this handle's row now* — and the reader that wants the second one is the pane.
    ///
    /// **An EMPTY text field is *not recorded here*, so the row keeps what it had.** The two
    /// writers know different things and neither knows both: the START is told who backgrounded
    /// the job (`how`) and never its command, and the SETTLEMENT is told the command — read from
    /// the live view, which is the last moment it exists — and not `how`. Without this the second
    /// write erased the first and no row could ever hold both, which is the whole reason a row
    /// would be written twice.
    ///
    /// The numbers are not merged: `state`, `produced`, `elapsed_ms` and `redirect` are facts of
    /// the write that is happening, and the later write's are the true ones.
    pub fn put_job(&self, session_id: &str, job: &JobRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO job
               (session_id, handle, command, how, state, produced, elapsed_ms, redirect, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
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

    /// **Write one merge-queue entry, replacing that id's last one — whole.**
    ///
    /// An upsert rather than an append: the row is an entry and not a log line, so an entry
    /// has one state at a time and the move is the same entry as the enqueue. The
    /// append-only record of every move is the queue's own events (`MergeEntryMoved`), which
    /// is a different question from *where is this entry's row now* — and the reader that
    /// wants the second one is the pane.
    ///
    /// `updated_ms` is stamped here rather than taken from the entry, for the reason
    /// [`Store::put_job`] stamps it: the store is the clock, and a caller that supplies its
    /// own would be a caller that could backdate a move. `created_ms` is the entry's, because
    /// it is the enqueue time and the enqueue is the entry's own act.
    /// **Take a waiting entry, or learn that somebody else has** — one `UPDATE … WHERE state =
    /// 'waiting'`, so two daemons over one store cannot both take the same branch: exactly one
    /// of them sees `true`. The row moves to `taken`; the caller writes the rest of the move.
    pub fn claim_merge_entry(&self, id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE merge_queue SET state = 'taken', updated_ms = ?2
              WHERE id = ?1 AND state = 'waiting'",
            params![id, now_ms()],
        )?;
        Ok(n == 1)
    }

    pub fn put_merge_entry(&self, entry: &MergeEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO merge_queue
                (id, session_id, branch, base_sha, priority, needs_json, state, brief, evidence,
                 created_ms, updated_ms, worktree, landed_sha)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
              ON CONFLICT(id) DO UPDATE SET
                session_id = ?2, branch = ?3, base_sha = ?4, priority = ?5, needs_json = ?6,
                state = ?7, brief = ?8, evidence = ?9, updated_ms = ?11, worktree = ?12,
                landed_sha = ?13",
            params![
                entry.id,
                entry.session_id,
                entry.branch,
                entry.base_sha,
                entry.priority.as_str(),
                serde_json::to_string(&entry.needs)?,
                entry.state.as_str(),
                entry.brief,
                entry.evidence,
                entry.created_ms as i64,
                now_ms(),
                entry.worktree,
                entry.landed_sha,
            ],
        )?;
        Ok(())
    }

    /// **Every merge-queue entry**, in the order the queue was filled — `created_ms`, then
    /// `id`, so two entries enqueued in the same millisecond do not come back in an order
    /// that changes between calls.
    ///
    /// The whole queue, every state: the read that answers *what is the queue* is the read
    /// that must not drop a row it cannot act on, and the state and the evidence on each row
    /// are what say why a row is where it is. The pure core sorts by priority on top of this;
    /// this read is the durable order, which is the enqueue order.
    pub fn merge_entries(&self) -> Result<Vec<MergeEntry>> {
        let mut st = self.conn.prepare(
            "SELECT id, session_id, branch, base_sha, priority, needs_json, state, brief,
                    evidence, created_ms, updated_ms, worktree, landed_sha
               FROM merge_queue ORDER BY created_ms, id",
        )?;
        let rows = st.query_map([], merge_entry_raw_from_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(merge_entry_from_raw(r?)?);
        }
        Ok(out)
    }

    /// **One merge-queue entry by id**, or `None` when the queue has no such entry.
    pub fn merge_entry(&self, id: &str) -> Result<Option<MergeEntry>> {
        let mut st = self.conn.prepare(
            "SELECT id, session_id, branch, base_sha, priority, needs_json, state, brief,
                    evidence, created_ms, updated_ms, worktree, landed_sha
               FROM merge_queue WHERE id = ?1",
        )?;
        let raw: Option<RawMergeEntry> = st
            .query_row(params![id], merge_entry_raw_from_row)
            .optional()?;
        // `transpose` and no `Ok(…?)`: the `Result` is already the answer, and wrapping it in
        // another one to unwrap it again is the shape clippy's `needless_question_mark` names.
        raw.map(merge_entry_from_raw).transpose()
    }

    /// **Write one review row, replacing that entry's last one — whole.**
    ///
    /// An upsert on the entry's id, for the reason [`Store::put_merge_entry`] is one: the row
    /// is a review and not a log line, so an entry has one review at a time and the verdict is
    /// the same row as the request. The append-only record of a review's life would be an
    /// event, and there is not one — the queue's events carry the ENTRY's moves, which is the
    /// question a head asks.
    ///
    /// **`asked_ms` is the caller's and `answered_ms` is the caller's too**, which is the
    /// opposite of [`Store::put_job`]'s rule and deliberate: the ask and the answer are two
    /// different turns by two different writers (the daemon asks; the reviewer's session
    /// answers), so the times are facts those writers hold rather than something a single
    /// clock can stamp. `answered_ms` is `None` while the review is outstanding, and a
    /// verdict write sets it.
    pub fn put_review(&self, rec: &ReviewRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO merge_review
                (entry_id, session_id, branch, base_sha, asked_ms, answered_ms, decision,
                 attempts, failed_ms, failure, reasons_json, files_json, commands_json)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
              ON CONFLICT(entry_id) DO UPDATE SET
                session_id = ?2, branch = ?3, base_sha = ?4, asked_ms = ?5,
                answered_ms = ?6, decision = ?7, attempts = ?8, failed_ms = ?9,
                failure = ?10, reasons_json = ?11, files_json = ?12,
                commands_json = ?13",
            params![
                rec.entry_id,
                rec.session_id,
                rec.branch,
                rec.base_sha,
                rec.asked_ms as i64,
                rec.answered_ms.map(|v| v as i64),
                rec.decision,
                rec.attempts as i64,
                rec.failed_ms.map(|v| v as i64),
                rec.failure,
                serde_json::to_string(&rec.reasons)?,
                serde_json::to_string(&rec.files)?,
                serde_json::to_string(&rec.commands)?,
            ],
        )?;
        Ok(())
    }

    /// **Re-attempt one entry's review** — the person's act, as the two writes it is, in one
    /// transaction: the entry goes back to `waiting`, and its review row goes back to *nobody
    /// has answered*.
    ///
    /// The operator's report, verbatim: *"so merge queue has 4 failed items, we need a way to
    /// restart them"*. The queue could not: `Failed` is terminal, and a review row with a
    /// `decision` on it is never re-asked. So the restart is the one move that un-parks an
    /// entry, and it is the QUEUE's own retry asked for by hand rather than a second rule — see
    /// `mergequeue::restart`, which is where the decision to allow it lives and which is the
    /// only caller.
    ///
    /// **Conditional, and the condition is the double-press guard.** The `UPDATE` moves an
    /// entry only while it is parked (`failed`, `conflict`, `stale`), so a second press finds
    /// it `waiting` and moves nothing — `false` — and no second reviewer is ever asked for.
    /// That is the same shape [`Store::claim_merge_entry`] has, one state over: the row is the
    /// arbiter, not the caller's memory of what it just did.
    ///
    /// **The review row is cleared only when the entry actually moved.** Clearing it for an
    /// entry that did not move would throw away a live attempt's request — the second-writer
    /// shape the whole queue is written to refuse.
    ///
    /// `asked_ms` is restamped to `now_ms`, because the row is a NEW ask: the old `asked_ms` was
    /// the first attempt's, and a person reading *asked 4h ago* about an attempt that started a
    /// second ago would be reading the wrong fact.
    pub fn restart_review(&self, entry_id: &str, evidence: &str, now_ms: u64) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let moved = tx.execute(
            "UPDATE merge_queue SET state = 'waiting', evidence = ?2, updated_ms = ?3
              WHERE id = ?1 AND state IN ('failed', 'conflict', 'stale', 'vetoed')",
            params![entry_id, evidence, now_ms as i64],
        )?;
        if moved == 1 {
            tx.execute(
                "UPDATE merge_review SET asked_ms = ?2, answered_ms = NULL, decision = NULL,
                        attempts = 0, failed_ms = NULL, failure = '',
                        reasons_json = '[]', files_json = '[]', commands_json = '[]'
                  WHERE entry_id = ?1",
                params![entry_id, now_ms as i64],
            )?;
        }
        tx.commit()?;
        Ok(moved == 1)
    }

    /// **The sentence `/queue reset` writes on every row it moves** — the store's own, because
    /// the write is the store's, and `pub` because the daemon ANNOUNCES the move with these same
    /// words: a head that folded `MergeEntryMoved` and a reader who opens the row afterwards must
    /// not be told two different things about one move.
    ///
    /// What it says is the whole of what the verb does — the review starts over and nothing else
    /// changes — because that is the one thing a person reading a row has to be able to tell apart
    /// from a `restart` (which re-asks the same review for one entry) and from a move that lost
    /// work.
    pub const RESET_EVIDENCE: &str = "reset by the operator: the review is re-asked, and the branch and its worktree are \
         untouched.";

    /// **Reset the review of every parked entry** — `/queue reset [ENTRY-ID]`, the operator's
    /// verb, as the two writes it is, in one transaction.
    ///
    /// The operator's ruling, in their own words: *"as for reset - no reset resets review states.
    /// and /queue clean deletes"*. So this is [`Store::restart_review`] with the `entry_id` filter
    /// removed and nothing else changed: every entry parked in `failed`, `conflict`, `stale` or
    /// `vetoed` — the four words `mergequeue::parked` names — goes back to `waiting` with
    /// [`RESET_EVIDENCE`] on its row, and its review row goes back to *nobody has answered*:
    /// `asked_ms` restamped to `now_ms`, `answered_ms` and `decision` NULL, `attempts` 0,
    /// `failed_ms` NULL, `failure` empty and the three `*_json` cleared. The branches, the
    /// worktrees and every other fact about the entries are untouched: the tutor re-reviews, and
    /// the queue then does what it does with a waiting entry whose verdict accepts.
    ///
    /// **`entry: None` is the whole queue**, which is the sweep the verb is for; `Some(id)` is the
    /// same write for one row. An entry that is not parked — `waiting` (already in the queue),
    /// `taken` (the daemon is mid-merge on it) or `landed` (done) — is not moved, and that is the
    /// ROW arbitrating rather than the caller: the `UPDATE`'s `WHERE` is the guard, exactly as it
    /// is in [`Store::restart_review`], so a second press moves nothing and a live landing is
    /// never written over.
    ///
    /// **A review row is cleared only where the entry actually moved**, `restart_review`'s own
    /// rule and its reason: clearing the review of an entry that did not move would throw away a
    /// live attempt's request.
    ///
    /// Returns how many entries moved. Nothing else about the queue changes, and nothing is
    /// deleted — that is [`Store::remove_merge_entry`]'s verb.
    pub fn reset_reviews(&self, now_ms: u64, entry: Option<&str>) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        // **The rows the write will move, read first**, because the review rows are cleared per
        // entry and the count the caller gets is the count of these. Read inside the transaction,
        // so what is counted is what the `UPDATE`s below are about to see.
        let mut st = tx.prepare(
            "SELECT id FROM merge_queue
              WHERE state IN ('failed', 'conflict', 'stale', 'vetoed')
                AND (?1 IS NULL OR id = ?1)",
        )?;
        let ids: Vec<String> = st
            .query_map(params![entry], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // The statement's borrow of `tx` ends here: `commit` takes the transaction by value.
        drop(st);
        let mut moved = 0usize;
        for id in &ids {
            let n = tx.execute(
                "UPDATE merge_queue SET state = 'waiting', evidence = ?2, updated_ms = ?3
                  WHERE id = ?1 AND state IN ('failed', 'conflict', 'stale', 'vetoed')",
                params![id, Self::RESET_EVIDENCE, now_ms as i64],
            )?;
            if n == 1 {
                tx.execute(
                    "UPDATE merge_review SET asked_ms = ?2, answered_ms = NULL, decision = NULL,
                            attempts = 0, failed_ms = NULL, failure = '',
                            reasons_json = '[]', files_json = '[]', commands_json = '[]'
                      WHERE entry_id = ?1",
                    params![id, now_ms as i64],
                )?;
                moved += 1;
            }
        }
        tx.commit()?;
        Ok(moved)
    }

    /// **The person's verdict REPLACES the review's** — the entry back in the queue with an
    /// accepting verdict on it, in one transaction.
    ///
    /// The operator's ask, in their words: *"i want to be able to approve / veto / delete"*.
    /// A review can be `needs_human` or a refusal, and the person who reads the branch may
    /// disagree with it; this is where they say so. What it is NOT is a way past the gate: the
    /// entry goes back to `waiting`, and the queue then does what it does with a waiting entry
    /// whose verdict accepts — rebase at the tip, run the gate, land. An approval overrides a
    /// JUDGEMENT; it never touches the landing machinery. `mergequeue::approve` is the only
    /// caller and holds the rule; what is here is the two writes.
    ///
    /// **The verdict row is the arbiter, and it has to be.** Every other conditional write in
    /// this table keys on the entry's state changing, which is what makes a second press move
    /// nothing ([`Store::restart_review`], [`Store::veto_entry`]). An approval's post-state is
    /// `waiting`, which is also a pre-state — the entry a person approves may be one nobody has
    /// asked about yet — so the state cannot be the guard. The row that CHANGES is the verdict,
    /// so the guard is there: a verdict that already accepts is not overwritten, the upsert
    /// changes nothing, and a second press is refused rather than reported as done. The entry's
    /// own move is conditional too, and it is the second half of the same transaction: an entry
    /// the daemon has claimed (`taken`) or that has `landed` is not moved, and the verdict
    /// written for it is rolled back with it.
    ///
    /// **The decider is recorded on the review row**: `session_id` becomes `by` — the session
    /// the verb came from — because the pane draws that field as *attach to it to read the
    /// argument*, and after an approval the argument that matters is the person's, in their own
    /// session. `asked_ms` is `now_ms` for a row that did not exist: there was no ask, and the
    /// moment somebody decided is the only true stamp the column can carry.
    ///
    /// **What the old verdict was is not here but on the entry's `evidence`**, in the row's own
    /// words — `mergequeue::approve` composes that sentence, for the reason `restart` does: a
    /// person who presses the verb on the wrong entry has to be able to see that they did.
    pub fn approve_entry(
        &self,
        entry: &MergeEntry,
        by: &str,
        evidence: &str,
        now_ms: u64,
    ) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let ruled = tx.execute(
            "INSERT INTO merge_review
                 (entry_id, session_id, branch, base_sha, asked_ms, answered_ms, decision,
                  attempts, failed_ms, failure, reasons_json, files_json, commands_json)
               VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'accept', 0, NULL, '', '[]', '[]', '[]')
               ON CONFLICT(entry_id) DO UPDATE SET
                 session_id = ?2, answered_ms = ?5, decision = 'accept', attempts = 0,
                 failed_ms = NULL, failure = '', reasons_json = '[]', files_json = '[]',
                 commands_json = '[]'
                 WHERE merge_review.decision IS NULL OR merge_review.decision <> 'accept'",
            params![entry.id, by, entry.branch, entry.base_sha, now_ms as i64],
        )?;
        // Nothing written and nothing to roll back — the drop of `tx` is the rollback, and the
        // early return is what keeps the entry's move from happening on its own.
        if ruled != 1 {
            return Ok(false);
        }
        let moved = tx.execute(
            "UPDATE merge_queue SET state = 'waiting', evidence = ?2, updated_ms = ?3
              WHERE id = ?1 AND state IN ('waiting', 'failed', 'conflict', 'stale', 'vetoed')",
            params![entry.id, evidence, now_ms as i64],
        )?;
        if moved != 1 {
            // The entry moved under us between the read and the write — the daemon claimed it,
            // or it landed. The verdict written a statement ago goes with it: a verdict for an
            // entry that did not move is a verdict nobody asked for.
            return Ok(false);
        }
        tx.commit()?;
        Ok(true)
    }

    /// **The person's rejection** — the entry parked in `vetoed`, with their own words on it.
    ///
    /// The other half of *"i want to be able to approve / veto / delete"*, and the half that
    /// must never read as a machine's: the state word says a person decided, the `evidence`
    /// says who and in what words, and **the review row is not touched at all**. That last part
    /// is the design rather than an omission — the reviewer's verdict is a fact about the
    /// branch that the person overrode, and overwriting it with a person's word would put a
    /// human decision in the column the pane reads as *the reviewer's* and lose the judgement
    /// that was overridden.
    ///
    /// **Conditional on the entry being movable, and the post-state is excluded**, which is
    /// what makes a second press move nothing: `vetoed` is not in the list, so the second
    /// `UPDATE` matches no row and answers `false` — the row is the arbiter, not the caller's
    /// memory of what it just did ([`Store::restart_review`]'s shape, one state over).
    /// `taken` and `landed` are excluded for the reason every person's verb excludes them: a
    /// veto written over an entry the daemon is mid-merge on would be overwritten by the
    /// landing, and a landed entry is merged.
    pub fn veto_entry(&self, entry_id: &str, evidence: &str, now_ms: u64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE merge_queue SET state = 'vetoed', evidence = ?2, updated_ms = ?3
              WHERE id = ?1 AND state IN ('waiting', 'failed', 'conflict', 'stale')",
            params![entry_id, evidence, now_ms as i64],
        )?;
        Ok(n == 1)
    }

    /// **Drop one entry from the queue** — the row and its review, in one transaction.
    ///
    /// The third of the person's three verbs. The branch and the worktree are untouched: the
    /// queue forgets the entry, and what happens to the tree is not the queue's business any
    /// more (`mergequeue::remove` says so in the sentence it answers with). The review row goes
    /// with the entry — a verdict about an entry the queue no longer holds is a row nothing
    /// reads, and leaving it would make [`Store::reviews`] a table that grows with rows no
    /// entry can be joined against.
    ///
    /// **Conditional on the entry being movable.** `taken` is excluded because a gatekeeper or
    /// a daemon mid-merge writing to a row that is gone is the second-writer shape this whole
    /// queue is written to refuse; `landed` is excluded because a landed row is the queue's
    /// record that main moved and the base its dependents rebase onto (`mergequeue::effective_base`),
    /// so deleting one would silently change what a pending dependent rebases onto. The
    /// decision and its sentences are `mergequeue::removable`'s; what is here is the write.
    ///
    /// A second press finds no row and answers `false`: the id that resolved a moment ago does
    /// not resolve now, and the caller says exactly that rather than reporting a deletion twice.
    pub fn remove_merge_entry(&self, entry_id: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let gone = tx.execute(
            "DELETE FROM merge_queue
              WHERE id = ?1 AND state IN ('waiting', 'failed', 'conflict', 'stale', 'vetoed')",
            params![entry_id],
        )?;
        if gone != 1 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM merge_review WHERE entry_id = ?1",
            params![entry_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// **Every review, oldest ask first** — the whole table, which is what the queue's pass
    /// reads once and joins in memory against the entries it is about to take.
    ///
    /// The whole table rather than a per-entry lookup because the queue's pass is about the
    /// whole queue anyway (`Store::merge_entries`), and a second read per entry would make one
    /// pass N+1 queries for a table that is one row per entry.
    pub fn reviews(&self) -> Result<Vec<ReviewRecord>> {
        let mut st = self.conn.prepare(
            "SELECT entry_id, session_id, branch, base_sha, asked_ms, answered_ms, decision,
                    attempts, failed_ms, failure, reasons_json, files_json, commands_json
               FROM merge_review ORDER BY asked_ms, entry_id",
        )?;
        let rows = st.query_map([], review_raw_from_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(review_from_raw(r?)?);
        }
        Ok(out)
    }

    /// **One entry's review**, or `None` when nobody has asked — which is not the same fact as
    /// a row with no verdict, and the two are what the queue's gate is built on.
    pub fn merge_review(&self, entry_id: &str) -> Result<Option<ReviewRecord>> {
        let mut st = self.conn.prepare(
            "SELECT entry_id, session_id, branch, base_sha, asked_ms, answered_ms, decision,
                    attempts, failed_ms, failure, reasons_json, files_json, commands_json
               FROM merge_review WHERE entry_id = ?1",
        )?;
        let raw: Option<RawReviewRecord> = st
            .query_row(params![entry_id], review_raw_from_row)
            .optional()?;
        raw.map(review_from_raw).transpose()
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
            other => {
                return Err(StoreError::Refused(format!(
                    "`{other}` is not a diagnostic field"
                )));
            }
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
            measured: one(&format!(
                "SELECT COUNT(*) FROM adjudication WHERE {MEASURED_SQL}"
            ))?,
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

    /// **A listed session carries its current transcript's last row** — what the subagents
    /// pane reads to say how a child it did not watch ended.
    #[test]
    fn a_listed_session_carries_its_last_row() {
        let s = store();
        let (tr, _) = seeded(&s);
        assert_eq!(
            s.list_sessions().unwrap()[0].last_item,
            None,
            "no rows, no last row"
        );
        let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
        let answer = TranscriptItem::Assistant {
            text: "the answer".into(),
            tool_calls: vec![],
            truncated: false,
        };
        let items = [
            TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: "the question".into(),
                }],
            },
            answer.clone(),
        ];
        for (seq, item) in items.iter().enumerate() {
            let toks = vec![10 + seq as u32];
            let row = ledger.append(&format!("it-{seq}"), &toks).unwrap().clone();
            s.append_item(&tr, seq as u32, item, &row, &toks).unwrap();
        }
        assert_eq!(s.list_sessions().unwrap()[0].last_item, Some(answer));
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
                speaker: Default::default(),
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
            speaker: Default::default(),
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
                speaker: Default::default(),
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
            media: None,
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
            "letibot-migrate-v1-{}-{}-{}.db",
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
            "letibot-migrate-v2-{}-{}-{}.db",
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
                by: TodoBy::Model,
                when: None,
            }],
        )
        .unwrap();
        assert_eq!(s.todos("s-todo").unwrap().len(), 1);
    }

    /// **A job survives the daemon that ran it** — the whole point of the table, and the half
    /// nothing could answer before it existed.
    ///
    /// The measured shape: the process table is memory, so a handle the pane listed a moment ago
    /// answered `no job ... here` to the next daemon, *about a job whose ending was written
    /// down*. This closes the store and reopens it, which is the only thing that proves the row
    /// is on disk rather than in a cache the next daemon would not have.
    #[test]
    fn a_job_survives_the_daemon_that_ran_it() {
        let dir = std::env::temp_dir().join(format!("letibot-jobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.db");

        let started = JobRecord {
            handle: "j7".into(),
            command: "cargo test --release".into(),
            how: "asked".into(),
            state: "running".into(),
            produced: 0,
            elapsed_ms: 12_000,
            redirect: None,
        };
        {
            let s = Store::open(&path).expect("a store");
            // **The session row first**, because `job.session_id` is a foreign key and
            // `foreign_keys` is on — a job belongs to a session the same way a transcript
            // does. The daemon's own order gives this for free: the session row is written at
            // open, and a job is only ever backgrounded inside a session.
            s.put_session(&SessionRecord {
                id: "s-jobs".into(),
                title: Some("jobs".into()),
                model_id: "qwen".into(),
                dialect_sha: "sha".into(),
                workspace_root: "/w".into(),
                owner: "dead".into(),
                role: None,
                approvers: vec![],
                parent_session_id: None,
            })
            .expect("the session row");
            s.put_job("s-jobs", &started).expect("the job row");
        }
        {
            let s = Store::open(&path).expect("the same store, a second daemon");
            let back = s.jobs("s-jobs").expect("the job table reads");
            assert_eq!(back.len(), 1, "the job did not come back: {back:?}");
            assert_eq!(back[0], started, "the row came back changed");

            // **And the settlement is the SAME row.** A job has one state at a time, so the
            // second write updates it — a table that appended would make `jobs()` a history of
            // states, when its caller wants one handle's fate.
            let settled = JobRecord {
                state: "exited 0".into(),
                produced: 4_096,
                ..started.clone()
            };
            s.put_job("s-jobs", &settled).expect("the settlement");
            assert_eq!(
                s.jobs("s-jobs").expect("reads"),
                vec![settled],
                "the settlement made a second row"
            );
            // **And a session that never ran a job reads empty** — "no jobs" and "the table
            // is missing" have to be different answers or the second shows up as the first.
            assert!(s.jobs("s-other").expect("reads").is_empty());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A merge-queue entry survives the daemon that enqueued it** — the whole point of the
    /// table, and the half nothing could answer before it existed.
    ///
    /// The measured shape the job history pins, one level up: the queue the daemon serves is
    /// the queue that must survive the daemon, because the daemon is the one thing that is
    /// restarted and a merge that dies mid-flight must come back as a row that says what
    /// happened. This closes the store and reopens it, which is the only thing that proves the
    /// row is on disk rather than in a cache the next daemon would not have.
    #[test]
    fn a_merge_entry_survives_the_daemon_that_enqueued_it() {
        let dir = std::env::temp_dir().join(format!("letibot-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.db");

        let enqueued = MergeEntry {
            id: "m-1".into(),
            session_id: "s-merge".into(),
            branch: "agent/merge-queue".into(),
            base_sha: "abc123".into(),
            priority: MergePriority::Subagent,
            needs: vec!["m-0".into()],
            state: MergeState::Waiting,
            // **The ask, and it is asserted byte-for-byte below** — the reviewer's only
            // framing, so a store that mangled it (a trim, a re-encoding) would hand the
            // gatekeeper a brief the child never saw.
            brief: "build the merge queue, and make it durable\nsecond line".into(),
            evidence: "waiting on m-0".into(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some("/wt/agent-merge-queue".into()),
            landed_sha: None,
        };
        {
            let s = Store::open(&path).expect("a store");
            s.put_merge_entry(&enqueued).expect("the entry row");
        }
        {
            let s = Store::open(&path).expect("the same store, a second daemon");
            let back = s.merge_entries().expect("the queue reads");
            assert_eq!(back.len(), 1, "the entry did not come back: {back:?}");
            // `updated_ms` is the store's clock, so it is not asserted equal to the entry's;
            // everything else is the row the enqueuer wrote.
            assert_eq!(back[0].id, enqueued.id);
            assert_eq!(back[0].branch, enqueued.branch);
            assert_eq!(back[0].base_sha, enqueued.base_sha);
            assert_eq!(back[0].priority, enqueued.priority);
            assert_eq!(back[0].needs, enqueued.needs);
            assert_eq!(back[0].state, enqueued.state);
            assert_eq!(
                back[0].brief, enqueued.brief,
                "the brief is the reviewer's only framing and must survive verbatim"
            );
            assert_eq!(back[0].evidence, enqueued.evidence);
            assert_eq!(back[0].created_ms, enqueued.created_ms);
            assert_eq!(back[0].worktree, enqueued.worktree);
            assert_eq!(back[0].landed_sha, enqueued.landed_sha);

            // **And the move is the SAME row.** An entry has one state at a time, so the
            // second write updates it — a table that appended would make `merge_entries()` a
            // history of states, when its caller wants one entry's fate.
            let landed = MergeEntry {
                state: MergeState::Landed,
                evidence: "landed at def456".into(),
                landed_sha: Some("def456".into()),
                worktree: None,
                ..enqueued.clone()
            };
            s.put_merge_entry(&landed).expect("the move");
            let moved = s.merge_entry("m-1").expect("reads").expect("the entry");
            // `updated_ms` is the store's clock — see the doc on `put_merge_entry` — so it is
            // the one field that is not the caller's, and it is normalized away rather than
            // asserted. Everything else is the row the caller wrote.
            assert_eq!(
                MergeEntry {
                    updated_ms: moved.updated_ms,
                    ..landed.clone()
                },
                moved,
                "the move made a second row"
            );
            assert_eq!(
                s.merge_entries().expect("reads").len(),
                1,
                "the move made a second row"
            );

            // **And an id the queue has never held reads `None`** — "no such entry" and "the
            // table is missing" have to be different answers or the second shows up as the
            // first.
            assert!(s.merge_entry("m-nope").expect("reads").is_none());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A failed attempt is a row of its own, and a restart clears it.**
    ///
    /// The operator's report, verbatim: *"so merge queue has 4 failed items, we need a way to
    /// restart them"*. Three columns carry the attempt (`attempts`, `failed_ms`, `failure`) and
    /// `decision` stays NULL — a failure is not a judgement — which is what lets the queue ask
    /// again and a person start it by hand.
    #[test]
    fn a_failed_attempt_is_its_own_row_and_a_restart_clears_it() {
        let dir = std::env::temp_dir().join(format!("letibot-restart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.db");
        let s = Store::open(&path).expect("a store");
        s.put_merge_entry(&MergeEntry {
            id: "m-1".into(),
            session_id: "s-1".into(),
            branch: "agent/x".into(),
            base_sha: "abc".into(),
            priority: MergePriority::Subagent,
            needs: vec![],
            // Parked, which is the state a restart moves.
            state: MergeState::Failed,
            brief: "do the work".into(),
            evidence: "http 429: Weekly/Monthly Limit Exhausted".into(),
            created_ms: 1,
            updated_ms: 1,
            worktree: None,
            landed_sha: None,
        })
        .expect("the entry");
        s.put_review(&ReviewRecord {
            entry_id: "m-1".into(),
            session_id: "s-host".into(),
            branch: "agent/x".into(),
            base_sha: "abc".into(),
            asked_ms: 1_000,
            answered_ms: None,
            decision: None,
            attempts: 3,
            failed_ms: Some(1_500),
            failure: "http 429: Weekly/Monthly Limit Exhausted".into(),
            reasons: vec![],
            files: vec![],
            commands: vec![],
        })
        .expect("the failed-attempt row");
        // **Read back whole.** A column that exists and refuses a value is not a column the
        // queue can use.
        let back = s.merge_review("m-1").unwrap().unwrap();
        assert_eq!(back.attempts, 3);
        assert_eq!(back.failed_ms, Some(1_500));
        assert_eq!(back.failure, "http 429: Weekly/Monthly Limit Exhausted");
        assert!(back.decision.is_none(), "a failure is not a verdict");

        // **The restart is conditional on the entry being parked**, and it is the row that
        // arbitrates: the first ask moves it, the second moves nothing.
        assert!(
            s.restart_review("m-1", "restarted by the operator", 9_000)
                .expect("the first ask")
        );
        let entry = s.merge_entry("m-1").unwrap().unwrap();
        assert_eq!(entry.state, MergeState::Waiting, "back in the queue");
        assert_eq!(entry.evidence, "restarted by the operator");
        let cleared = s.merge_review("m-1").unwrap().unwrap();
        assert_eq!(cleared.attempts, 0, "the bound is reset");
        assert_eq!(cleared.failed_ms, None);
        assert_eq!(cleared.failure, "");
        assert_eq!(cleared.asked_ms, 9_000, "the ask is a NEW ask");
        assert!(
            !s.restart_review("m-1", "again", 9_001).unwrap(),
            "a second ask moves nothing, because the entry is not parked any more"
        );
        assert_eq!(
            s.merge_review("m-1").unwrap().unwrap().asked_ms,
            9_000,
            "and it did not restamp the live attempt's ask"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A whole-queue reset moves the parked rows, clears their reviews, and touches nothing
    /// else.**
    ///
    /// The operator's ruling, in their words: *"as for reset - no reset resets review states. and
    /// /queue clean deletes"*. The two halves of that are exactly what this asserts. Every parked
    /// entry — `failed`, `conflict`, `stale`, `vetoed` — goes back to `waiting` with
    /// [`Store::RESET_EVIDENCE`] on its row and its review row cleared to *nobody has answered*: a
    /// NEW ask, stamped `now_ms`, with the old verdict, its reasons and its failure gone. The
    /// three states a reset is not about — `waiting` (already in the queue), `taken` (the daemon
    /// is mid-merge on it) and `landed` (done) — are not moved, and their review rows are left
    /// exactly as they were.
    ///
    /// The second half is the one that keeps the verb from being a way around a live landing: the
    /// arbiter is the row, so a second press moves nothing and restamps nothing.
    #[test]
    fn a_whole_queue_reset_moves_the_parked_rows_and_clears_their_reviews() {
        let dir = std::env::temp_dir().join(format!("letibot-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.db");
        let s = Store::open(&path).expect("a store");
        let entry = |id: &str, state: MergeState| MergeEntry {
            id: id.into(),
            session_id: "s-child".into(),
            branch: format!("agent/{id}"),
            base_sha: "abc".into(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state,
            brief: "do the work".into(),
            evidence: "the gate is red".into(),
            created_ms: 1,
            updated_ms: 1,
            worktree: Some("/wt".into()),
            landed_sha: None,
        };
        // **A verdict on every one of the seven**, so "cleared" and "left alone" are both
        // claims about a row that had something to clear.
        for (id, state) in [
            ("m-failed", MergeState::Failed),
            ("m-conflict", MergeState::Conflict),
            ("m-stale", MergeState::Stale),
            ("m-vetoed", MergeState::Vetoed),
            ("m-waiting", MergeState::Waiting),
            ("m-taken", MergeState::Taken),
            ("m-landed", MergeState::Landed),
        ] {
            s.put_merge_entry(&entry(id, state)).expect("the entry");
            s.put_review(&ReviewRecord {
                entry_id: id.into(),
                session_id: "s-reviewer".into(),
                branch: format!("agent/{id}"),
                base_sha: "abc".into(),
                asked_ms: 1_000,
                answered_ms: Some(1_500),
                decision: Some("reject".into()),
                attempts: 2,
                failed_ms: Some(1_600),
                failure: "http 429: Weekly/Monthly Limit Exhausted".into(),
                reasons: vec!["the branch is 0 commits over its base".into()],
                files: vec!["crates/widget.rs".into()],
                commands: vec!["git diff base...branch".into()],
            })
            .expect("the verdict");
        }

        assert_eq!(
            s.reset_reviews(9_000, None).expect("the reset"),
            4,
            "the four parked entries moved, and only those"
        );
        for id in ["m-failed", "m-conflict", "m-stale", "m-vetoed"] {
            let back = s.merge_entry(id).unwrap().unwrap();
            assert_eq!(back.state, MergeState::Waiting, "{id} is back in the queue");
            assert_eq!(
                back.evidence,
                Store::RESET_EVIDENCE,
                "{id}: the row says what moved it"
            );
            assert_eq!(
                back.worktree.as_deref(),
                Some("/wt"),
                "{id}: the tree is its own"
            );
            let cleared = s.merge_review(id).unwrap().unwrap();
            assert_eq!(cleared.asked_ms, 9_000, "{id}: the ask is a NEW ask");
            assert!(
                cleared.answered_ms.is_none() && cleared.decision.is_none(),
                "{id}: the verdict is cleared"
            );
            assert_eq!(cleared.attempts, 0, "{id}: the bound is reset");
            assert_eq!(cleared.failed_ms, None, "{id}: the failure goes with it");
            assert_eq!(cleared.failure, "", "{id}");
            assert!(
                cleared.reasons.is_empty()
                    && cleared.files.is_empty()
                    && cleared.commands.is_empty(),
                "{id}: the argument goes with the verdict"
            );
        }
        // **The three a reset is not about are untouched** — the row AND its review.
        for (id, state) in [
            ("m-waiting", MergeState::Waiting),
            ("m-taken", MergeState::Taken),
            ("m-landed", MergeState::Landed),
        ] {
            let still = s.merge_entry(id).unwrap().unwrap();
            assert_eq!(still.state, state, "{id} did not move");
            assert_eq!(
                still.evidence, "the gate is red",
                "{id}: its row is its own"
            );
            let kept = s.merge_review(id).unwrap().unwrap();
            assert_eq!(kept.asked_ms, 1_000, "{id}: its review was not restamped");
            assert_eq!(kept.decision.as_deref(), Some("reject"), "{id}");
            assert_eq!(kept.attempts, 2, "{id}: nor was its bound");
        }

        // **One id is the same write for one row**, and a second press moves nothing: the four
        // that moved are `waiting` now, so the `WHERE` finds none of them, and an id the queue
        // has never held is not an error — it is a row that did not move.
        assert_eq!(s.reset_reviews(9_100, Some("m-conflict")).unwrap(), 0);
        assert_eq!(s.reset_reviews(9_200, Some("m-taken")).unwrap(), 0);
        assert_eq!(s.reset_reviews(9_300, Some("m-nope")).unwrap(), 0);
        assert_eq!(
            s.merge_review("m-conflict").unwrap().unwrap().asked_ms,
            9_000,
            "and the second press did not restamp the live ask"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **An approval writes the verdict the queue reads, and the second press writes
    /// nothing.**
    ///
    /// The operator's ask, in their words: *"i want to be able to approve / veto / delete"*.
    /// The verdict row is the arbiter here rather than the entry's state — the post-state
    /// (`waiting`) is also a pre-state, because the entry a person approves may be one nobody
    /// has asked about — so this asserts the two halves of that: the row is written, and the
    /// same press twice writes it once.
    #[test]
    fn an_approval_writes_the_verdict_and_a_second_press_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("letibot-approve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.db");
        let s = Store::open(&path).expect("a store");
        let entry = |id: &str, state: MergeState| MergeEntry {
            id: id.into(),
            session_id: "s-child".into(),
            branch: format!("agent/{id}"),
            base_sha: "abc".into(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state,
            brief: "do the work".into(),
            evidence: "landing the branch lands nothing".into(),
            created_ms: 1,
            updated_ms: 1,
            worktree: Some("/wt".into()),
            landed_sha: None,
        };
        let failed = entry("m-1", MergeState::Failed);
        s.put_merge_entry(&failed).expect("the entry");
        // The verdict the person is overriding, with the reasons and the files it was based
        // on: what an approval replaces, and what must not survive into the new verdict.
        s.put_review(&ReviewRecord {
            entry_id: "m-1".into(),
            session_id: "s-reviewer".into(),
            branch: failed.branch.clone(),
            base_sha: failed.base_sha.clone(),
            asked_ms: 1_000,
            answered_ms: Some(1_500),
            decision: Some("reject".into()),
            attempts: 0,
            failed_ms: None,
            failure: String::new(),
            reasons: vec!["the branch is 0 commits over its base".into()],
            files: vec!["crates/widget.rs".into()],
            commands: vec!["git diff base...branch".into()],
        })
        .expect("the reviewer's verdict");

        assert!(
            s.approve_entry(&failed, "s-operator", "approved by the operator", 9_000)
                .expect("the approval")
        );
        let back = s.merge_entry("m-1").unwrap().unwrap();
        assert_eq!(back.state, MergeState::Waiting, "back in the queue");
        assert_eq!(back.evidence, "approved by the operator");
        let ruled = s.merge_review("m-1").unwrap().unwrap();
        assert_eq!(
            ruled.decision.as_deref(),
            Some("accept"),
            "the verdict the queue reads"
        );
        assert_eq!(ruled.answered_ms, Some(9_000));
        assert_eq!(
            ruled.session_id, "s-operator",
            "the decider is on the row, where the pane says *attach to read the argument*"
        );
        assert_eq!(ruled.attempts, 0);
        assert!(ruled.failure.is_empty());
        assert!(
            ruled.reasons.is_empty() && ruled.files.is_empty(),
            "the verdict is the person's now, not the reviewer's: {ruled:?}"
        );

        // **The second press moves nothing**, and it does not restamp the verdict either.
        assert!(
            !s.approve_entry(&failed, "s-operator", "approved again", 9_001)
                .unwrap(),
            "a verdict that already accepts is not overwritten"
        );
        assert_eq!(
            s.merge_entry("m-1").unwrap().unwrap().evidence,
            "approved by the operator"
        );
        assert_eq!(
            s.merge_review("m-1").unwrap().unwrap().answered_ms,
            Some(9_000)
        );

        // **An entry nobody has asked about gets the row the approval needs** — the upsert's
        // insert half, and the reason `asked_ms` is the moment somebody decided.
        let unreviewed = entry("m-2", MergeState::Waiting);
        s.put_merge_entry(&unreviewed).expect("the entry");
        assert!(s.merge_review("m-2").unwrap().is_none(), "nobody has asked");
        assert!(
            s.approve_entry(&unreviewed, "s-operator", "approved by the operator", 9_100)
                .expect("the approval")
        );
        let written = s.merge_review("m-2").unwrap().unwrap();
        assert_eq!(written.decision.as_deref(), Some("accept"));
        assert_eq!(written.asked_ms, 9_100);
        assert_eq!(
            written.branch, "agent/m-2",
            "the row carries the entry's own branch"
        );

        // **An entry the queue has taken is not moved, and the verdict goes with it.** The
        // daemon is mid-merge on a `taken` row, and a verdict written under it would be a
        // second writer on a row the landing is about to overwrite.
        let taken = entry("m-3", MergeState::Taken);
        s.put_merge_entry(&taken).expect("the entry");
        assert!(
            !s.approve_entry(&taken, "s-operator", "approved by the operator", 9_200)
                .unwrap()
        );
        assert_eq!(
            s.merge_entry("m-3").unwrap().unwrap().state,
            MergeState::Taken
        );
        assert!(
            s.merge_review("m-3").unwrap().is_none(),
            "the verdict written for an entry that did not move was rolled back with it"
        );
        // And a landed entry is refused the same way.
        let landed = entry("m-4", MergeState::Landed);
        s.put_merge_entry(&landed).expect("the entry");
        assert!(
            !s.approve_entry(&landed, "s-operator", "approved by the operator", 9_300)
                .unwrap()
        );
        // **And an id the queue has never held writes nothing at all.**
        assert!(
            !s.approve_entry(
                &entry("m-nope", MergeState::Waiting),
                "s-operator",
                "x",
                9_400
            )
            .unwrap()
        );
        assert!(s.merge_review("m-nope").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A veto parks the row as a person's decision; a remove forgets the entry and its
    /// review.**
    ///
    /// The two acts whose post-state is not a pre-state, so the entry row is the arbiter and
    /// the second press moves nothing. The veto must not touch the review — the judgement it
    /// overrode is a fact about the branch, and the pane reads that column as the reviewer's —
    /// and the removal must take the review row with the entry rather than leaving a verdict
    /// nothing can be joined against.
    #[test]
    fn a_veto_parks_the_row_and_a_remove_forgets_it() {
        let dir = std::env::temp_dir().join(format!("letibot-veto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.db");
        let s = Store::open(&path).expect("a store");
        let entry = |id: &str, state: MergeState| MergeEntry {
            id: id.into(),
            session_id: "s-child".into(),
            branch: format!("agent/{id}"),
            base_sha: "abc".into(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state,
            brief: "do the work".into(),
            evidence: "landing the branch lands nothing".into(),
            created_ms: 1,
            updated_ms: 1,
            worktree: Some("/wt".into()),
            landed_sha: None,
        };
        let review = |id: &str| ReviewRecord {
            entry_id: id.into(),
            session_id: "s-reviewer".into(),
            branch: format!("agent/{id}"),
            base_sha: "abc".into(),
            asked_ms: 1_000,
            answered_ms: Some(1_500),
            decision: Some("reject".into()),
            attempts: 0,
            failed_ms: None,
            failure: String::new(),
            reasons: vec!["the branch is 0 commits over its base".into()],
            files: vec![],
            commands: vec![],
        };
        let waiting = entry("m-1", MergeState::Waiting);
        s.put_merge_entry(&waiting).expect("the entry");
        s.put_review(&review("m-1")).expect("the verdict");

        assert!(
            s.veto_entry("m-1", "vetoed by the operator: it lands nothing", 9_000)
                .expect("the veto")
        );
        let parked = s.merge_entry("m-1").unwrap().unwrap();
        assert_eq!(
            parked.state,
            MergeState::Vetoed,
            "parked as a person's decision"
        );
        assert_eq!(parked.evidence, "vetoed by the operator: it lands nothing");
        assert_eq!(
            s.merge_review("m-1").unwrap().unwrap(),
            review("m-1"),
            "the veto does not touch the reviewer's verdict"
        );
        // **The second press moves nothing** — `vetoed` is not a state a veto leaves.
        assert!(!s.veto_entry("m-1", "vetoed again", 9_001).unwrap());
        assert_eq!(
            s.merge_entry("m-1").unwrap().unwrap().evidence,
            "vetoed by the operator: it lands nothing"
        );

        // **A taken entry is not vetoed and a landed one is not removed** — the daemon is
        // mid-merge on the first and the second is merged, branch deleted and all.
        let taken = entry("m-2", MergeState::Taken);
        s.put_merge_entry(&taken).expect("the entry");
        assert!(
            !s.veto_entry("m-2", "vetoed by the operator", 9_100)
                .unwrap()
        );
        assert!(!s.remove_merge_entry("m-2").unwrap());
        assert_eq!(
            s.merge_entry("m-2").unwrap().unwrap().state,
            MergeState::Taken
        );
        let landed = entry("m-3", MergeState::Landed);
        s.put_merge_entry(&landed).expect("the entry");
        assert!(
            !s.veto_entry("m-3", "vetoed by the operator", 9_200)
                .unwrap()
        );
        assert!(!s.remove_merge_entry("m-3").unwrap());
        assert_eq!(
            s.merge_entry("m-3").unwrap().unwrap().state,
            MergeState::Landed
        );

        // **And the remove forgets the entry and its review together.**
        assert!(s.remove_merge_entry("m-1").unwrap());
        assert!(s.merge_entry("m-1").unwrap().is_none());
        assert!(s.merge_review("m-1").unwrap().is_none());
        assert!(
            s.merge_entries().unwrap().len() == 2,
            "only the two parked rows are left"
        );
        // A second press finds no row, and an id that never existed finds none either.
        assert!(!s.remove_merge_entry("m-1").unwrap());
        assert!(!s.remove_merge_entry("m-nope").unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A v19 store gains the three columns, and its existing rows read as *no failed
    /// attempt*.** A verdict is not a failure, and `attempts = 0` is the true state of a row
    /// whose ask was answered.
    #[test]
    fn a_v19_store_gains_the_attempt_columns() {
        let dir = std::env::temp_dir().join(format!("letibot-migrate-v20-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.db");
        let verdict = ReviewRecord {
            entry_id: "m-old".into(),
            session_id: "s-host".into(),
            branch: "agent/x".into(),
            base_sha: "abc".into(),
            asked_ms: 1_000,
            answered_ms: Some(2_000),
            decision: Some("accept".into()),
            attempts: 0,
            failed_ms: None,
            failure: String::new(),
            reasons: vec!["it does what the ask said".into()],
            files: vec![],
            commands: vec![],
        };
        {
            let s = Store::open(&path).unwrap();
            s.put_review(&verdict).unwrap();
        }
        {
            // Walk the store back to v19: the three columns go, and the version with them —
            // the same shape the v15 fixture uses, one version along.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(
                "ALTER TABLE merge_review DROP COLUMN attempts;
                 ALTER TABLE merge_review DROP COLUMN failed_ms;
                 ALTER TABLE merge_review DROP COLUMN failure;
                 DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (19);",
            )
            .unwrap();
        }
        let s = Store::open(&path).expect("the migration runs");
        let back = s.merge_review("m-old").unwrap().unwrap();
        assert_eq!(back, verdict, "a v19 verdict reads back unchanged");
        assert_eq!(back.attempts, 0, "and reads as no failed attempt");
        assert_eq!(back.failure, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A review survives the daemon that asked for it, and the two writes are one row** — the
    /// half of the gate that has to outlive everything: the ask is durable, the verdict is
    /// durable, and the row is the same row.
    ///
    /// The queue reads this table on every pass and refuses to land anything it cannot find an
    /// accepting verdict for, so a review held in the daemon's memory would make a restart
    /// either re-review everything or land something nobody reviewed.
    #[test]
    fn a_review_survives_the_daemon_that_asked_for_it() {
        let dir = std::env::temp_dir().join(format!("letibot-review-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.db");

        let asked = ReviewRecord {
            entry_id: "m-1".into(),
            session_id: "gatekeeper".into(),
            branch: "agent/merge-queue".into(),
            base_sha: "abc123".into(),
            asked_ms: 1_000,
            answered_ms: None,
            decision: None,
            attempts: 0,
            failed_ms: None,
            failure: String::new(),
            reasons: vec![],
            files: vec![],
            commands: vec![],
        };
        {
            let s = Store::open(&path).expect("a store");
            s.put_review(&asked).expect("the request row");
        }
        {
            let s = Store::open(&path).expect("the same store, a second daemon");
            let back = s.merge_review("m-1").expect("reads").expect("the review");
            assert_eq!(
                back, asked,
                "the request came back changed — and `decision: None` is what makes the queue \
                 WAIT rather than land"
            );

            // **The verdict is the SAME row.** A review has one verdict at a time, so the
            // second write updates it; a table that appended would make *what did the reviewer
            // say about this entry* a history the queue would have to pick a winner from.
            let answered = ReviewRecord {
                asked_ms: back.asked_ms,
                answered_ms: Some(2_000),
                decision: Some("accept".into()),
                reasons: vec!["the artifact does what the brief asked".into()],
                files: vec!["crates/harnessd/src/mergequeue.rs".into()],
                commands: vec!["git diff abc123...agent/merge-queue".into()],
                ..asked.clone()
            };
            s.put_review(&answered).expect("the verdict");
            assert_eq!(
                s.merge_review("m-1").expect("reads"),
                Some(answered),
                "the verdict made a second row"
            );
            assert_eq!(s.reviews().expect("reads").len(), 1);

            // **And an entry nobody asked about reads `None`** — *nobody asked* and *asked and
            // unanswered* are different answers, and the queue does something different with
            // each (it asks, or it waits).
            assert!(s.merge_review("m-nope").expect("reads").is_none());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A store written before the merge-queue table gains one** — the migration arm, guarded
    /// the way the job table's is: build a real store, reverse the step by hand with
    /// `DROP TABLE`, walk the version back, and assert the migration puts back exactly what
    /// `SCHEMA_SQL` would have.
    #[test]
    fn a_v15_store_is_migrated_and_gains_a_merge_queue_table() {
        let path = std::env::temp_dir().join(format!(
            "letibot-migrate-v15-{}-{}-{}.db",
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

        {
            let s = Store::open(&path).unwrap();
            s.put_session(&SessionRecord {
                id: "s-v15".into(),
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
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("DROP INDEX IF EXISTS merge_queue_state_idx", [])
                .unwrap();
            c.execute("DROP TABLE merge_queue", []).unwrap();
            c.execute("DELETE FROM schema_version", []).unwrap();
            c.execute("INSERT INTO schema_version (version) VALUES (15)", [])
                .unwrap();
        }
        {
            let s = Store::open(&path).expect("the migration runs");
            // The table is back, and a row written through it reads back.
            let entry = MergeEntry {
                id: "m-mig".into(),
                session_id: "s-v15".into(),
                branch: "b".into(),
                base_sha: "sha".into(),
                priority: MergePriority::Urgent,
                needs: vec![],
                state: MergeState::Waiting,
                brief: String::new(),
                evidence: String::new(),
                created_ms: 1,
                updated_ms: 1,
                worktree: None,
                landed_sha: None,
            };
            s.put_merge_entry(&entry)
                .expect("a row through the migrated table");
            assert_eq!(s.merge_entries().expect("reads").len(), 1);
            // **And v17 ran on the way here**: a v15 store is two steps behind, so the `brief`
            // column is added by the same open. Asserted through a write, because a column that
            // exists and refuses a value is not a column the queue can use.
            let mut with_brief = entry.clone();
            with_brief.brief = "the ask".into();
            s.put_merge_entry(&with_brief)
                .expect("a row carrying a brief");
            assert_eq!(
                s.merge_entry("m-mig").expect("reads").unwrap().brief,
                "the ask"
            );
        }
    }

    /// **A store written before the job table gains one** — and this is the test that guards a
    /// live box, which is at v14 today.
    ///
    /// Same fixture rule as the v1 and v2 tests: build a real store, reverse the step by hand with
    /// `DROP TABLE`, walk the version back, and assert the migration puts back exactly what
    /// `SCHEMA_SQL` would have. **A migration that only ever ran on an empty file would reach a
    /// fresh checkout and no existing store** — which is the failure those tests were written for
    /// and the reason this one exists rather than trusting `SCHEMA_SQL` alone.
    #[test]
    fn a_v14_store_is_migrated_and_gains_a_job_table() {
        let path = std::env::temp_dir().join(format!(
            "letibot-migrate-v14-{}-{}-{}.db",
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

        {
            let s = Store::open(&path).unwrap();
            s.put_session(&SessionRecord {
                id: "s-v14".into(),
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
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("DROP TABLE job", []).unwrap();
            c.execute("UPDATE schema_version SET version = 14", [])
                .unwrap();
            // Prove the fixture really is v14: the table is gone.
            assert!(
                c.query_row("SELECT 1 FROM job", [], |r| r.get::<_, i64>(0))
                    .is_err(),
                "the fixture still has a job table, so it is not a v14 store"
            );
        }

        // Opening it runs the migration, and the table it added is a working one — not merely
        // present: a `CREATE TABLE` that disagreed with `SCHEMA_SQL` would pass an existence check
        // and fail on the first insert.
        let s = Store::open(&path).unwrap();
        let v: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert!(s.jobs("s-v14").unwrap().is_empty());
        s.put_job(
            "s-v14",
            &JobRecord {
                handle: "j1".into(),
                command: "cargo build".into(),
                how: "asked".into(),
                state: "running".into(),
                produced: 0,
                elapsed_ms: 10,
                redirect: None,
            },
        )
        .unwrap();
        assert_eq!(s.jobs("s-v14").unwrap().len(), 1);
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
                by: TodoBy::Model,
                when: None,
            },
            TodoItem {
                content: "seat the tool".into(),
                status: TodoStatus::InProgress,
                by: TodoBy::Model,
                when: None,
            },
            TodoItem {
                content: "render the pane".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
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
            by: TodoBy::Model,
            when: None,
        }];
        s.put_todos("sess-1", &second).unwrap();
        assert_eq!(s.todos("sess-1").unwrap(), second);

        // An empty write clears; the row remains and answers empty.
        s.put_todos("sess-1", &[]).unwrap();
        assert!(s.todos("sess-1").unwrap().is_empty());
    }

    /// **A postponed row is a row the store keeps, and the LIFT is the same round trip.**
    ///
    /// The operator's ask is that a postponed item *"persists"* — so the state has to survive the
    /// one thing persistence means here, which is a write and a read. Asserted in both directions
    /// because the two acts are one word apart and a store that read the lift back as a fresh
    /// pending row would be the same defect wearing a shrug: the row's own words, its author and
    /// its condition have to come back with it, or lifting a row would quietly lose the handle it
    /// was waiting on.
    ///
    /// The spelling is asserted too: `postponed` is what a `sqlite3` reader sees in the row, the
    /// same way `pending` and `completed` are.
    #[test]
    fn a_postponed_row_round_trips_through_the_store_and_so_does_lifting_it() {
        let s = store();
        let _seeded = seeded(&s);
        let set_aside = TodoItem {
            content: "push once CI lands".into(),
            status: TodoStatus::Postponed,
            by: TodoBy::Operator,
            when: Some(TodoCondition::Job {
                handle: "j121".into(),
            }),
        };
        s.put_todos("sess-1", &[set_aside.clone()]).unwrap();
        let back = s.todos("sess-1").unwrap();
        assert_eq!(
            back,
            vec![set_aside],
            "the postponed row, its author and its handle all survive the store"
        );
        // The word on the wire between this crate and `sqlite3`.
        let raw: String = s
            .conn
            .query_row(
                "SELECT todos_json FROM todo WHERE session_id = ?1",
                params!["sess-1"],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            raw.contains(r#""status":"postponed""#),
            "a store reader spells it the way every other status is spelled: {raw}"
        );

        // **And the lift.** Back to open work, which is a different row to the evaluator and to
        // the idle check, and it must be a different row after a restart too.
        let lifted = TodoItem {
            status: TodoStatus::Pending,
            ..s.todos("sess-1").unwrap().remove(0)
        };
        s.put_todos("sess-1", &[lifted.clone()]).unwrap();
        assert_eq!(s.todos("sess-1").unwrap(), vec![lifted]);
    }

    /// **A parent's row round trips with the operator's own spelling for its author.**
    ///
    /// The ruling names the author string exactly — *"throught tree author - (Parent
    /// <session-id-of-parent>)"* — and the store is where that string has to survive, because a
    /// resumed child rebuilds its board from here and a child that could not tell `Parent s-…`
    /// from `model` could not tell what it decided from what it was told. The raw JSON is asserted
    /// for the same reason the postponed test asserts its own word: `"by":"Parent s-…"` beside
    /// `"by":"model"` is the whole of the third author as a `sqlite3` reader sees it.
    ///
    /// And the refusal is asserted on the word: an author this reader does not know is an error
    /// naming the word, not a silent `model` — the same rule every other vocabulary in this tree
    /// keeps.
    #[test]
    fn a_parents_row_round_trips_with_the_authors_own_string() {
        let s = store();
        let _seeded = seeded(&s);
        let told = TodoItem {
            content: "land the parity row".into(),
            status: TodoStatus::Pending,
            by: TodoBy::parent_of("s-1789462738453908838"),
            when: None,
        };
        s.put_todos("sess-1", &[told.clone()]).unwrap();
        assert_eq!(s.todos("sess-1").unwrap(), vec![told]);

        let raw: String = s
            .conn
            .query_row(
                "SELECT todos_json FROM todo WHERE session_id = ?1",
                params!["sess-1"],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            raw.contains(r#""by":"Parent s-1789462738453908838""#),
            "the author is the operator's string, in full, beside the other two words: {raw}"
        );

        // **An unknown author is refused by name.** Not read back as the model's — a list whose
        // `by` this reader cannot parse is a list it says so about, taking nothing with it. (The
        // `at line … column …` tail is serde_json's own and not part of the sentence.)
        let bad = r#"[{"content":"x","status":"pending","by":"the cat"}]"#;
        let err = serde_json::from_str::<Vec<TodoItem>>(bad)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with(
                "`the cat` is not a todo author: model, operator, or `Parent <session-id>`"
            ),
            "the refusal names the word it did not understand: {err}"
        );
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
        assert_eq!(got.context_ledger, None);

        s.set_context("sess-1", Some(44_700), Some(40_000), Some(43_900))
            .unwrap();
        let got = s.session("sess-1").unwrap().unwrap();
        assert_eq!(got.context_tokens, Some(44_700));
        assert_eq!(got.context_cached, Some(40_000));
        // **The other half of the measurement, and the reason both moved together.**
        // A provider count with no ledger beside it cannot be turned into a ratio:
        // whoever reads it back has to supply the ledger, and supplying TODAY's is
        // the bug this column exists to stop.
        assert_eq!(got.context_ledger, Some(43_900));

        // A later turn replaces the number: it is the LAST turn's prompt, not a
        // sum and not a maximum.
        s.set_context("sess-1", Some(51_000), Some(44_700), Some(50_200))
            .unwrap();
        let got = s.session("sess-1").unwrap().unwrap();
        assert_eq!(got.context_tokens, Some(51_000));
        assert_eq!(got.context_cached, Some(44_700));
        assert_eq!(got.context_ledger, Some(50_200));

        // Clearing takes the pair with it. A row that kept the ledger while the
        // count went away would read as a measurement of nothing.
        s.set_context("sess-1", None, None, None).unwrap();
        let got = s.session("sess-1").unwrap().unwrap();
        assert_eq!(got.context_tokens, None);
        assert_eq!(got.context_ledger, None);

        // **The window, which has its own setter and its own lifetime.** A session opens
        // with one and keeps it until the model underneath it moves; nothing about a turn
        // touches it, which is exactly why it is not a fifth parameter on `set_context`.
        s.set_window("sess-1", Some(262_144)).unwrap();
        assert_eq!(
            s.session("sess-1").unwrap().unwrap().context_window,
            Some(262_144)
        );
        // **A turn's write does not disturb it** — the two facts are measured at different
        // times and folding them together would say they were measured together.
        s.set_context("sess-1", Some(9), Some(8), Some(7)).unwrap();
        assert_eq!(
            s.session("sess-1").unwrap().unwrap().context_window,
            Some(262_144)
        );
        // A session whose window nobody can name CLEARS it rather than keeping a stale
        // one: `None` is the record saying it cannot answer, not a large window.
        s.set_window("sess-1", None).unwrap();
        assert_eq!(s.session("sess-1").unwrap().unwrap().context_window, None);
        assert!(matches!(
            s.set_window("nope", Some(1)),
            Err(StoreError::NotFound(_))
        ));

        // A session nobody has written is not a row to update.
        assert!(matches!(
            s.set_context("nope", Some(1), None, None),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn a_v7_store_is_migrated_and_gains_the_context_columns() {
        // Same fixture rule as the v1/v2 tests: a real store, one step reversed,
        // and the migration has to put back exactly what `SCHEMA_SQL` would have.
        let path = std::env::temp_dir().join(format!(
            "letibot-migrate-v7-{}-{}-{}.db",
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
            // **And v12's, because a genuine v7 store never had it either.** The
            // fixture is built by walking a CURRENT store backwards, so leaving this
            // column in place would mean the v12 arm below hit its idempotence check
            // (`has = true`) and its `ALTER` — the one line a real pre-v12 store
            // needs — was never run by any test. Same reasoning as the v6 arm's own
            // note about the fixtures, pointed the other way.
            // **v14's column is NOT dropped here, because this table will not give it up.**
            // sqlite refuses `ALTER TABLE session DROP COLUMN context_window` on it —
            // MEASURED: *"error in table session after drop column: incomplete input"*, the
            // statement-text re-splice tripping over this table's own comment blocks (the
            // same class of constraint the note below records for the other drop order). So
            // this walk stops at v13, and the v14 arm has a fixture of its own — the honest
            // shape for an ADDITIVE arm, which needs a `session` table rather than a walk.
            // **v13's first, and the order is not taste.** The store is walked backwards,
            // so both columns are here and neither arm's `ALTER` would run without this —
            // and dropping provider_choice while context_ledger still follows is the drop
            // sqlite accepts; the other order dies reconstructing the table's CREATE text
            // ("incomplete input"), which is also why the column sits before v12's in
            // SCHEMA_SQL rather than at the end.
            c.execute("ALTER TABLE session DROP COLUMN provider_choice", [])
                .unwrap();
            c.execute("ALTER TABLE session DROP COLUMN context_ledger", [])
                .unwrap();
            c.execute("UPDATE schema_version SET version = 7", [])
                .unwrap();
            assert!(
                c.query_row("SELECT context_tokens FROM session", [], |r| r
                    .get::<_, Option<i64>>(0))
                    .is_err(),
                "the fixture still has a context_tokens column, so it is not a v7 store"
            );
            assert!(
                c.query_row("SELECT context_ledger FROM session", [], |r| r
                    .get::<_, Option<i64>>(0))
                    .is_err(),
                "the fixture still has a context_ledger column, so the v12 ALTER is untested"
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
        // v12's column arrived with the same migration run, and is `None` on this
        // pre-existing row: unverifiable rather than zero, so the recovery refuses
        // the pair. See the v12 arm.
        assert_eq!(got.context_ledger, None);
        // v14's column arrived with the same run, and is `None` on a pre-existing row:
        // *nobody recorded one* — never the parent's window and never a guess. See the
        // v14 arm for why it is not backfilled.
        assert_eq!(got.context_window, None);
        s.set_window("s-ctx", Some(262_144)).unwrap();
        assert_eq!(
            s.session("s-ctx").unwrap().unwrap().context_window,
            Some(262_144)
        );

        s.set_context("s-ctx", Some(12_000), Some(11_000), Some(11_800))
            .unwrap();
        let got = s.session("s-ctx").unwrap().unwrap();
        assert_eq!(got.context_tokens, Some(12_000));
        assert_eq!(got.context_cached, Some(11_000));
        assert_eq!(got.context_ledger, Some(11_800));
    }

    /// **A v13 store gains the window column** — v14's arm, with a fixture of its own.
    ///
    /// Every other arm here is tested by walking a CURRENT store backwards, and this one
    /// cannot be: sqlite refuses to drop `context_window` from this table (see the v7
    /// fixture's note — measured, `incomplete input`). A hand-written v13 `session` is the
    /// right fixture for an **additive** arm, and it is the one place in this file where
    /// hand-written DDL is not drift: the arm only needs the table to exist, and what is
    /// asserted is what the arm DOES — the column appears, a row written before it reads
    /// `NULL` rather than a guess, and a value written through the API comes back.
    ///
    /// The other tables are deliberately absent, so this reads through `connection()`
    /// rather than `session()`: `list_sessions` joins `transcript` and `transcript_item`,
    /// and building those by hand is the drift this fixture is trying to avoid.
    #[test]
    fn a_v13_store_gains_the_context_window_column() {
        let path = std::env::temp_dir().join(format!(
            "letibot-migrate-v13-{}-{}-{}.db",
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

        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE session (
                     id    TEXT PRIMARY KEY,
                     title TEXT,
                     role  TEXT
                 );
                 INSERT INTO session (id, title, role) VALUES ('s-window', 'old', 'coder');
                 CREATE TABLE schema_version (version INTEGER);
                 INSERT INTO schema_version (version) VALUES (13);",
            )
            .unwrap();
            // Prove the fixture really is pre-v14, or the arm below is tested by nothing.
            assert!(
                c.query_row("SELECT context_window FROM session", [], |r| r
                    .get::<_, Option<i64>>(0))
                    .is_err(),
                "the fixture already has a context_window column, so it is not a v13 store"
            );
        }

        let s = Store::open(&path).unwrap();
        let v: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "the migration stamped the new version");

        // The column is there, and the row that predates it says *nobody recorded one* —
        // never the parent window, and never a catalogue guess. See the v14 arm.
        let got: Option<i64> = s
            .connection()
            .query_row(
                "SELECT context_window FROM session WHERE id = 's-window'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(got, None);

        // And it is writeable through the API this column exists for.
        s.set_window("s-window", Some(262_144)).unwrap();
        let got: Option<i64> = s
            .connection()
            .query_row(
                "SELECT context_window FROM session WHERE id = 's-window'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(got, Some(262_144));

        // Reopening is a no-op rather than a second migration.
        drop(s);
        let s = Store::open(&path).unwrap();
        assert!(
            s.set_window("s-window", Some(1)).is_ok(),
            "a store already at the current version still answers a write"
        );
    }

    /// **A v18 store gains the stood-down pair** — v19's arm, in the v13
    /// fixture's shape (additive arms take hand-written DDL; see that test's
    /// note for why this is the one place hand-written DDL is not drift).
    ///
    /// What is asserted is what the arm DOES: the two columns appear, a row
    /// written before them reads as *the guard has not fired* rather than as a
    /// guess, and a pair written through the API comes back.
    #[test]
    fn a_v18_store_gains_the_stood_down_pair() {
        let path = std::env::temp_dir().join(format!(
            "letibot-migrate-v18-{}-{}-{}.db",
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

        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE session (
                     id    TEXT PRIMARY KEY,
                     title TEXT,
                     role  TEXT
                 );
                 INSERT INTO session (id, title, role) VALUES ('s-stood', 'old', 'coder');
                 CREATE TABLE schema_version (version INTEGER);
                 INSERT INTO schema_version (version) VALUES (18);",
            )
            .unwrap();
            // Prove the fixture really is pre-v19, or the arm below is tested
            // by nothing.
            assert!(
                c.query_row("SELECT auto_compact_resident FROM session", [], |r| r
                    .get::<_, Option<i64>>(0))
                    .is_err(),
                "the fixture already has an auto_compact_resident column, so it is not a v18 store"
            );
        }

        let s = Store::open(&path).unwrap();
        let v: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "the migration stamped the new version");

        // Both columns are there, and the row that predates them says *the
        // guard has not fired* — which is not the same fact as `auto_compact`
        // being on, and is deliberately not backfilled.
        let (resident, after): (Option<i64>, Option<i64>) = s
            .connection()
            .query_row(
                "SELECT auto_compact_resident, auto_compact_after FROM session \
                 WHERE id = 's-stood'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((resident, after), (None, None));

        // And the pair is writeable through the API this column exists for.
        s.set_auto_compact_stood_down("s-stood", Some((1_849_499, 1_850_000)))
            .unwrap();
        let (resident, after): (Option<i64>, Option<i64>) = s
            .connection()
            .query_row(
                "SELECT auto_compact_resident, auto_compact_after FROM session \
                 WHERE id = 's-stood'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((resident, after), (Some(1_849_499), Some(1_850_000)));

        // Reopening is a no-op rather than a second migration.
        drop(s);
        let s = Store::open(&path).unwrap();
        assert!(
            s.set_auto_compact_stood_down("s-stood", None).is_ok(),
            "a store already at the current version still answers a write"
        );
    }

    /// **The guard's pair survives the daemon that decided it** — the whole
    /// point of the columns, and the half nothing could answer before they
    /// existed.
    ///
    /// The guard's old write was `auto_compact = false` in the daemon's
    /// memory: it died with the process, so a restart silently re-armed the
    /// looping the guard existed to stop AND silently cleared the one fact
    /// that explained a session that would not tidy itself. This closes and
    /// reopens the store, which is the only thing that proves the row is on
    /// disk rather than in a cache the next daemon would not have.
    #[test]
    fn a_stood_down_compaction_survives_the_daemon_that_decided_it() {
        let dir = std::env::temp_dir().join(format!("letibot-stood-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _clean = Clean(dir.clone());
        let path = dir.join("sessions.db");

        // The pair at the wedge's own scale: the 2026-10-09 session compacted
        // 1,849,499 ledger tokens and the summary still stood within a
        // headroom of the 1,000,000-token plan.
        {
            let s = Store::open(&path).unwrap();
            s.put_session(&SessionRecord {
                id: "s-wedge".into(),
                title: Some("the wedged session".into()),
                model_id: "m".into(),
                dialect_sha: "sha".into(),
                workspace_root: "/w".into(),
                owner: "dead".into(),
                role: None,
                approvers: vec![],
                parent_session_id: None,
            })
            .unwrap();
            s.set_auto_compact_stood_down("s-wedge", Some((1_849_499, 1_530_411)))
                .unwrap();
        }

        // The next daemon opens the same file and reads the finding.
        let s = Store::open(&path).unwrap();
        let got = s
            .session("s-wedge")
            .unwrap()
            .expect("the row survived the reopen");
        assert_eq!(
            got.auto_compact_stood_down,
            Some((1_849_499, 1_530_411)),
            "the pair comes back in the units it was taken in — LEDGER tokens, \
             because that is what the guard decided in"
        );

        // And a session the guard never fired on stays absent, which is a
        // different fact from a pair of zeros: `None` is *the guard has not
        // fired*, and the settings row keeps it apart from `--no-auto-compact`.
        s.put_session(&SessionRecord {
            id: "s-fresh".into(),
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
        assert_eq!(
            s.session("s-fresh")
                .unwrap()
                .unwrap()
                .auto_compact_stood_down,
            None
        );
    }

    /// **The provider a session was SWITCHED TO is a fact on the session row, and it
    /// round-trips** — `name`, `name/model`, `local`, and NULL for one that never chose.
    ///
    /// The four spellings are four different claims at a resume, and the `local` one is
    /// the easy to get wrong: `None` says *no opinion, the daemon's default stands*, while
    /// `Some("local")` says *the local server, deliberately* — and a daemon whose own default
    /// is a provider would hand a switched-back session straight back to it if the two were
    /// collapsed.
    #[test]
    fn the_provider_choice_round_trips_with_its_four_spellings() {
        let s = store();
        let _ = seeded(&s);
        // Never switched: NULL, not the empty string — `set_title`'s own rule, so an
        // unset choice reads as an absence and not as a name that is blank.
        assert_eq!(s.provider_choice("sess-1").unwrap(), None);
        assert_eq!(s.session("sess-1").unwrap().unwrap().provider_choice, None);

        s.set_provider_choice("sess-1", Some("deepseek")).unwrap();
        assert_eq!(
            s.provider_choice("sess-1").unwrap().as_deref(),
            Some("deepseek")
        );
        s.set_provider_choice("sess-1", Some("glm-coding/glm-5.3"))
            .unwrap();
        // Also on the listing, which is what a resume reads.
        assert_eq!(
            s.session("sess-1")
                .unwrap()
                .unwrap()
                .provider_choice
                .as_deref(),
            Some("glm-coding/glm-5.3")
        );
        // The deliberate switch back is BY NAME, not NULL.
        s.set_provider_choice("sess-1", Some("local")).unwrap();
        assert_eq!(
            s.provider_choice("sess-1").unwrap().as_deref(),
            Some("local")
        );
        s.set_provider_choice("sess-1", None).unwrap();
        assert_eq!(s.provider_choice("sess-1").unwrap(), None);
        // And a session that does not exist is refused, not invented.
        assert!(s.set_provider_choice("no-such", Some("deepseek")).is_err());
    }

    /// **A v12 store gains the column as NULL**, which is exactly "never switched": a
    /// session recorded before this column existed resumes on the daemon's own default,
    /// which is what it always did. Walked back from a current store, for the same reason
    /// the v12 fixture walks back from one: the conversation's rows stay, so the migration
    /// is exercised against rows that already exist rather than against an empty file.
    #[test]
    fn a_v12_store_gains_the_provider_column_as_never_switched() {
        let path = std::env::temp_dir().join(format!(
            "letibot-v13-{}-{}-{}.db",
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

        {
            let s = Store::open(&path).unwrap();
            let _ = seeded(&s);
        }
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("ALTER TABLE session DROP COLUMN provider_choice", [])
                .unwrap();
            c.execute("UPDATE schema_version SET version = 12", [])
                .unwrap();
        }

        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.provider_choice("sess-1").unwrap(),
            None,
            "a session recorded before the column existed has no choice, and NULL says so"
        );
        // And the column is there to be written: the migration is not a no-op that
        // merely bumps the version.
        s.set_provider_choice("sess-1", Some("glm-coding/glm-5.3"))
            .unwrap();
        assert_eq!(
            s.provider_choice("sess-1").unwrap().as_deref(),
            Some("glm-coding/glm-5.3")
        );
    }

    #[test]
    fn the_v8_migration_backfills_the_context_of_a_conversation_already_on_disk() {
        // The case that matters: a store being upgraded holds a conversation whose
        // last turn finished before the column existed. Without the backfill its
        // row reads `NULL` — "no turn has finished" — and an attaching head shows
        // no context until the next turn. The backfill sums the prefix and the
        // current transcript's items, the same rows the encoder reads.
        let path = std::env::temp_dir().join(format!(
            "letibot-backfill-v8-{}-{}-{}.db",
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

        {
            let s = Store::open(&path).unwrap();
            let (tr, _) = seeded(&s);
            // Two items: 2 tokens and 3 tokens. The prefix carries 4.
            let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
            let item = TranscriptItem::User {
                speaker: Default::default(),
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
        let fresh = rows
            .iter()
            .find(|r| r.request_id == "adj-after-v11")
            .unwrap();
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
        // `None` for the path: this store was opened in memory, and what the test is
        // re-running is the MIGRATION, which does not care where the file is.
        let s = Store::from_connection(s.conn, None).expect("migrate");

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

    /// **Every id written before the byte vocabulary is the id it always was**: a GGUF
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
