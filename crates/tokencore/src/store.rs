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

use std::collections::BTreeMap;
use std::path::Path;

use letibot_transcript::TranscriptItem;
use rusqlite::{Connection, OptionalExtension, Params, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::ledger::LedgerRow;
use crate::vocab::TokenId;

mod busy;
pub mod live_view;
mod queues;
pub use busy::{BUSY_RETRY_TOTAL, is_busy_sql};
pub use queues::*;
mod modes;
pub use modes::ProjectMode;
mod subagents;
pub use subagents::*;
mod citations;
mod told;
pub use told::Told;

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
/// entry, described at its migration arm below and in `MergeEntry` (v16–v23; archived at v24, see [`queues`]). **15** since the
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
///
/// **21** since the session row carries the FROZEN prefix's fingerprint —
/// `prefix_fingerprint`, additive, described at its migration arm below. See
/// [`StoredSession::prefix_fingerprint`] for what is in it and why the live half of the
/// prompt is not.
///
/// **22** since the merge-queue entry carries the **landing half's own rows** —
/// `merge_queue.gate_steps_json`, additive, described at its migration arm below and at the
/// column in [`SCHEMA_SQL`]. One row per gate step (the command, its outcome, the tail of its
/// output, when it started and how long it took) so that a landing can be DRAWN and not merely
/// judged: a row that records only `failed` can only ever draw `failed`.
///
/// **23** since a store that predates the review/merge SPLIT has its queue **cleared once**,
/// on the first open of it by this build. This is the one arm that REMOVES rows rather than
/// adding a column or a table, and its arm is where the operator's ruling and the reason it is
/// the honest act are written down.
///
/// **It is 23 rather than 22 because the two migrations were in flight together.** `22` was
/// taken by `merge_queue.gate_steps_json` (`agent/per-step-landing-rows`, `af2144b`) before
/// this one reached main, and two migrations numbered 22 would each read the other's store as
/// *current* and never run. So this arm is written `from < 23` rather than against a pair of
/// numbers: a store left at 21 by the base, at 22 by that branch, or at anything older is
/// cleared on the first open by this build either way.
///
/// **24** since **review and merge are two queues** (R61): `review_queue`, a new `merge_queue`,
/// `queue_transition` (append-only), `queue_lease` and `target_health` — see [`queues`]. The
/// v23 `merge_queue`/`merge_review` are RENAMED to `merge_queue_v23`/`merge_review_v23`, every row
/// kept (the operator's decision 13: *"archive, don't delete"*), and the rows still waiting are
/// carried forward as reviews. The new `merge_queue` shares no column list with the old one, so a
/// daemon of an older build that was already running when the file was migrated fails its own
/// reads and writes of it rather than serving it.
///
/// **25** since the session row carries **the operator's pause of the idle plan-check** —
/// `nags_paused_until`, additive, described at its migration arm below and at
/// [`StoredSession::nags_paused_until`]. A pause is the operator's own act on one session, and
/// one that lived in the daemon's memory would come back as nags after the restart it was
/// typed to outlast.
///
/// **26** since a session's subagents are **rows of their own** — `subagent_run`, a new table,
/// additive, described at [`subagents`] and at its migration arm below. The operator: *"we
/// should bring subagents back - i cant afford to wait till they finish. we should record them
/// (session persist anyway) and restart automatically"*. A child's session was always here; what
/// was not was the fact that it was mid-task, what it had been asked and how it was spawned.
///
/// **27** since the permission modes are **rows of their own** — `project_mode` and
/// `session_mode`, new tables, additive, described at [`modes`]. They were `modes.tsv`, a file
/// each daemon rewrote whole from the copy it read at start, so one daemon's write undid
/// another's: *"and again peerdb allow-all lost after restart/reseat"*.
///
/// **28** since a background job's row says **whether its ending reached the model** —
/// `job.delivered`, additive, described at its migration arm below and at
/// [`Store::deliver_job`]. A job that settled and was HELD (R79) when the daemon stopped had
/// a row saying `exited 0` and nothing saying the model never heard it, so the next daemon
/// passed it over as finished business and the result was never delivered (TODO.md R91.5).
/// `subagent_run.delivered` (v26) is the same fact for a child.
///
/// **29** since what the queues have SAID is a row — `queue_told`, a new table, additive,
/// described at [`told`]. The rings and the notices were deduped in the daemon's memory, so every
/// restart re-rang the same red tip and resumed dormant sessions to re-tell it (TODO R92.7).
///
/// **30** since the lines a reply cited are a row — `citation`, a new table, additive, described
/// at [`citations`]. A citation is a snapshot taken when the reply lands, kept beside the reply so
/// the model's own text is never rewritten.
pub const SCHEMA_VERSION: i64 = 30;

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
    nags_paused_until INTEGER        -- v25; the operator paused this session's idle plan-check.
                                     -- NULL = not paused; 0 = until they resume it; any other
                                     -- value = unix ms the pause ends at. See
                                     -- `StoredSession::nags_paused_until`.
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
    /// **The fingerprint of the FROZEN prefix this session was seated with**, or `None`
    /// for a session seated before this column existed.
    ///
    /// **What it is:** one hash over the composed system instructions and the tool schemas
    /// this seat hands this session, plus the seat's own name — computed by
    /// `letibot_harnessd`'s `prefix_fingerprint`, which is the only thing that ever writes
    /// or reads it. Message 0 is frozen at creation, so a daemon that has since changed
    /// what it would seat composes a different one, and this is how that is detected
    /// without rendering either.
    ///
    /// **What it is not:** the standing notes. Those are re-read at session open, at every
    /// base rebuild and on every provider turn, so a fingerprint that covered them would
    /// cry stale every time a note was written. The frozen/live split is the whole reason
    /// this is a column rather than a hash of the stored prefix row — that row's `system`
    /// carries the notes as they were, and the section markers that would have to be
    /// stripped out of it belong to the daemon, not to this file.
    ///
    /// `None` means *nobody recorded one here*, which a reader must not render as *the
    /// prefix is unchanged*: the resume path falls back to the session's own stored
    /// `stable_prefix` row for exactly this case, and says nothing at all when neither is
    /// there.
    pub prefix_fingerprint: Option<String>,
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
    /// **The operator paused this session's idle plan-check** — `None` when they have not (or
    /// have resumed it), `Some(0)` for a pause that lasts until they resume it, and `Some(ms)`
    /// for one that ends by itself at that unix millisecond.
    ///
    /// On the row for `auto_compact_stood_down`'s reason: a pause in the daemon's memory alone
    /// would die with the process, and the nags the operator turned off would come back after a
    /// restart they never asked to be nagged across. A `Some(ms)` already in the past is a pause
    /// that ENDED while nobody held the session; the daemon that opens it reads that as resumed
    /// and says so, rather than as a pause that never ends.
    pub nags_paused_until: Option<i64>,
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

/// **One step of the gate, as the landing ran it** — the row the landing half records, one per
/// command `main`'s `AGENTS.md` declared.
///
/// The note's argument for the merge half is that *"the merge queue is a machine. Rebase, then
/// the gate's steps, then land. Nothing is being decided; something is being run. And a thing
/// that is being run can be drawn well"* — and a drawing needs the steps, not a verdict. A
/// landing that records only `failed` can only ever draw `failed`: the niceness is impossible by
/// construction, not by taste. The review half already carries `commands_json` and `evidence`;
/// this is the landing half's own.
///
/// **The rows live on the merge row** ([`MergeRow::gate_steps`]) and on the target's health row
/// ([`TargetHealth::gate_steps`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MergeGateStep {
    /// **The command as `main`'s `AGENTS.md` spells it** — the step's identity, and the string
    /// a person would paste into a shell to reproduce the row.
    ///
    /// Empty for exactly one row: the [`MergeGateOutcome::NoGate`] one, where there was no
    /// command to run. See that variant.
    pub command: String,
    /// What became of it, out of [`MergeGateOutcome`]'s closed set.
    pub outcome: MergeGateOutcome,
    /// **The tail of what the step wrote**, with the bytes dropped from the front counted —
    /// see `mergequeue::tail`, which is where the truncation and its admission come from. Empty
    /// on a step that never ran, which is what `not_run` says rather than an empty output.
    pub output: String,
    /// When the step started, Unix ms. `0` on a step that never ran.
    pub started_ms: u64,
    /// How long it took, ms. `0` on a step that never ran, and a real `0` for a command that
    /// took under a millisecond — the outcome is what tells the two apart, not this number.
    pub elapsed_ms: u64,
}

/// **What became of one gate step** — the closed set the landing's rows are written in.
///
/// Four words, and each is a different fact: a step that ran and was green, a step that ran and
/// was red, a step the gate never reached because an earlier one was red, and the one row a
/// repository with **no gate** records. The last is a variant rather than an empty list because
/// *"the gate declared nothing to run"* and *"the gate has not run yet"* are different facts,
/// and an empty checklist reads as a third one — *all steps passed* — which is the lie the
/// operator's own rule forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeGateOutcome {
    /// It ran, and it was green.
    Passed,
    /// It ran, and it was red. Its `output` is the tail that says why.
    Failed,
    /// **An earlier step was red, so the gate stopped before this one.** Not `passed`: a step
    /// that did not run is not a step that was green, and drawing the two the same way is how a
    /// gate that stopped at the first failure comes to look like a gate that ran everything.
    NotRun,
    /// **The repository's `main` declares no gate at all.** One row, with an empty `command` and
    /// the queue's own sentence in `output`, because there is no command to name — and a row
    /// rather than an empty list, because the empty list is the row's word for *the gate has not
    /// run on this entry*.
    NoGate,
    /// **It was still running at the gate's deadline and was killed** with its whole process
    /// group (R61 decision 9: thirty minutes, then `timed out` — an outcome of its own).
    TimedOut,
    /// **It was killed before it finished** because the daemon was stopping or lost the
    /// repository's lease — or because the kernel killed it at the gate's own memory cap
    /// (`[memory] gate`, R76 item 6; its `output` ends saying so, with the peak). It judged
    /// nothing.
    Killed,
}

/// **How long a row's TITLE may be** — 135, and it is the pane's own row rather than a round number.
///
/// A board row is drawn by rano's todos pane at the width the head hands it — `w = term_w - 2 *
/// GUTTER`, and `GUTTER` is 2 (`tui/src/ui/screen.rs`) — so what a row has to fit in is the
/// terminal less four columns. **The operator's terminal is 160 cells**: their own words, and the
/// number `SUBAGENT_TITLE_MAX` is written from (*"for example my terminal is 160 cells wide, make
/// the cap 256"*, 2026-10-03). The pane therefore draws a row at 156 columns, and a row spends 21
/// of them before its own words: 8 for the cursor mark and the `/todo` number (`"  ▸ 12  "`), 3 for
/// the checkbox and the space after it, 3 for the two spaces before the author's name, and 7 for
/// `— model` — read off rano's own row, whose test asserts `"        [~] write the port  — model"`.
/// 156 − 21 = **135**, and a title longer than that is a row the pane CUTS at its right edge, with
/// nothing anywhere saying it did.
///
/// **It is a bound on the TITLE, not on the row.** A row may carry a detail after
/// [`TODO_DETAIL_SEP`], and the detail is not capped: it is not what the operator scans the list
/// by, and a row whose title fits is a row that can still be told apart from its neighbours when
/// its detail runs off the edge. A row's `· waits on <handle>` tail is the same case — a condition
/// the operator set, drawn after the words, and the first thing the cut takes.
///
/// **Counted in `char`s, not in columns**, which is the honest limit of what a writer can be held
/// to: a title of wide characters can still overflow the pane, and the head cuts what it draws
/// either way. What this bounds is the WORDS, at the door they are written.
pub const TODO_TITLE_MAX: usize = 135;

/// **The separator between a row's title and its detail** — `title — detail`, the spelling this
/// tree already writes in two places: the head's new-todo card (`tui/src/app/keys/cards.rs`, *"the
/// card already made this head's choice for a typed detail"*) and the template seed
/// (`tui/src/app/todos.rs`). So this is one existing convention with two writers, named once here
/// rather than re-spelled by every reader of a row.
///
/// **The board has no second field for a detail and does not need one.** `TodoItem` is
/// `content`/`status`/`by`/`when`/`needs`, the wire's `TodoEntry` is the same five, and the words
/// are the row's one key — `needs` names a row by its exact `content`, `resolve` matches a quote by
/// its trimmed text, and `/todo rm N` addresses the operator's rows by number over that same list.
/// A detail field would be a second thing the key does not cover, on every row of every board
/// already in a store; and rano's pinned `TodoLine` — the board's row — has no body to draw it in
/// (`RepoRow`, the file's row, is the one row in that pane that has one). So the detail rides in
/// the same string, after this separator, and the pane draws it after the title. `panes/todos.rs`
/// says what would give it a line of its own.
pub const TODO_DETAIL_SEP: &str = " — ";

/// **A row's TITLE**: the words before [`TODO_DETAIL_SEP`], or the whole row when it has none.
///
/// This is the string [`TODO_TITLE_MAX`] bounds, and it is a function rather than a rule each
/// writer spells for itself because there are three writers — the model's `todo_write`, a parent's
/// `target` write, and the operator's own rows — and a second spelling is how one of them ends up
/// bounding a different string than the others.
///
/// **A newline is not a separator, and that is deliberate.** The pane flattens one — rano's
/// `clean_line` replaces it with a space before a row is drawn — so a second line is not a body the
/// row has: it is words the operator will read run together, and a reader that split a title at a
/// newline would be honouring a structure nothing else in the tree knows about. The row's words are
/// its title, newlines and all, up to the one separator the head actually writes.
pub fn todo_title(content: &str) -> &str {
    content
        .split_once(TODO_DETAIL_SEP)
        .map(|(title, _)| title)
        .unwrap_or(content)
        .trim()
}

/// **The rows that wait on the OPERATOR right now** — every open row (not `Completed`, not
/// `Cancelled`) with a [`TodoNeed::Operator`] edge, and the question it asks, in board order.
///
/// **One reading, used by every door that has to be LOUD about it** — the daemon's settings row
/// and its `waiting_on_operator` row, the picker's mark (through the store's
/// [`Store::sessions_waiting_on_operator`]), and the answer's own validation — so a row cannot be
/// waiting on one screen and not on another. A done or struck-off row asks nobody anything: its
/// question is moot, and a loud line about it would be a person asked to answer for work nobody
/// is going to do.
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

/// One line of a session's todo list, as the model wrote it.
///
/// Stored as mutable session metadata — the `set_title` class, not the
/// append-only class: a todo list the model revised three times is one list with
/// a history nobody asked to keep. **The whole list is sent on every write, and it
/// is an UPSERT**: a row whose text matches one already there moves that row's
/// state, a new text is added, and a row left out is NOT removed — the operator's
/// rule is *"only i should be able to delete todo items. as a rule everything that
/// ever created stays in history."*
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
    /// **The rows this one waits for** — the plan's edges, and the whole of why the idle check asks
    /// for a READY SET rather than the head of the list.
    ///
    /// A row is *ready* when every need is met, and the check offers the ready rows and says what is
    /// blocked on what — so an old blocked row no longer heads the line for ever. What it replaces
    /// is `open_priority(todos).first()`, under that function's own comment *"THE QUEUE, served one
    /// at a time"*: head-of-line blocking is now structural, and a row nobody can start is not
    /// asked for at all.
    ///
    /// **An unresolvable need is never met.** A name that matches no row, a name that matches two,
    /// or a kind this build cannot evaluate leaves the row BLOCKED and is SAID to be — see
    /// [`TodoNeed`]. The rule is `TodoCondition`'s, learned there first: *a condition nobody can
    /// evaluate must never read as met*, because a silently satisfied dependency makes the graph a
    /// lie and the check asks for work that cannot be started.
    ///
    /// `serde(default)`, the `by` precedent: every row already in a store, and every row a head
    /// older than this field sends, reads back as **having no edges** — which is what it was. A plan
    /// with no edges anywhere is therefore read exactly as it was before this field existed, which
    /// is what the compatibility test asserts.
    #[serde(default)]
    pub needs: Vec<TodoNeed>,
}

/// **What a row waits on** — one entry of [`TodoItem::needs`], and one edge of the plan's DAG.
///
/// **A tagged kind, exactly like [`TodoCondition`], and for that enum's own reason**: a kind can be
/// added without a new field on every row, and a reader that does not know one must *say so* rather
/// than misread it. The rule the two share is the load-bearing one — **an edge nobody can evaluate
/// must never read as met** — and it is why this is a kind rather than a bare string: a bare string
/// has no way to say *I do not know what this means*, and a dependency whose meaning is unknown
/// would then be the one dependency that is silently satisfied.
///
/// **The next kind is named when something can EVALUATE it.** The one still shaped for and not
/// built, so it does not have to be designed again:
///
/// * a **wasm predicate** — the host passes values and the module answers a verdict, and a trap, a
///   failed call or a fuelled-out one is unresolvable, which is this same rule a third time.
///
/// (Two others on that list are BUILT, and both are [`TodoNeed`]'s: a **child session**
/// ([`TodoNeed::Child`]) and a **background job** ([`TodoNeed::Job`]). What evaluates them is the
/// daemon — a board cannot know whether a session or a process is running, so the facts are handed
/// in from outside rather than read here.)
///
/// **The edge is a row's WORDS, and that is a decision rather than a shortcut.** The board has one
/// key and it is `content`: `TodoBoard::set_operator_states` resolves the operator's rows by their
/// exact trimmed text, `TodoBy`'s own doc says why (*"the words are the key, and that is not a
/// shortcut — there is no other key"*), and the wire's `TodoEntry` is `content`/`status`/`by`, so a
/// row the operator types into the pane has no id to type. An id field would be a second identity
/// rule for the same rows, and on every board already in a store it would be empty — an id that is
/// not a key at all until something synthesises one. What text costs is that a re-worded row breaks
/// every edge into it; what it buys is that the break is SAID, naming the need, and repaired by one
/// word in a plan the model is already looking at.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoNeed {
    /// **Another row on this board, named by its own words** — the exact `content`, trimmed, the
    /// way `set_operator_states` quotes one back.
    ///
    /// **The name must resolve to exactly ONE row.** A name that matches none, and a name that
    /// matches two, are both *unresolvable*: the row is blocked and the check says which need it
    /// could not place. That is the house rule for an ambiguous name — `set_operator_states`
    /// refuses one and names the candidates — read one step earlier, where the guess it refuses to
    /// make is a row silently starting on the strength of a name that meant something else.
    Row { content: String },
    /// **A CHILD session, named by its session id** — the dispatch case this enum's doc names first:
    /// a row that is a dispatch is not work, it waits on a session's completion, and that is what
    /// makes a parent's row for a running child silent *because it is not ready* rather than
    /// because the operator postponed it.
    ///
    /// **The id is the handle `task` handed back**, in FULL: `task_result` is what reads it, and
    /// the same rule `TodoBy::Parent` keeps for the author string applies — a shortened id is a
    /// name the model cannot quote back to the tool that has to act on it.
    ///
    /// **Whether that child is RUNNING is not on this board, and this variant does not pretend it
    /// is.** The daemon is the only thing that knows — its task journal records the spawn and the
    /// finish, and its registry holds when the session was opened — so the caller hands both facts
    /// in and `unmet_needs` reads them there. What a board alone can say about a child need is
    /// nothing, which is why a board with no daemon behind it (every test of the tool crate) treats
    /// the child as finished: there are no children there, and a need nobody can answer must not
    /// read as met *or* as a row that waits for ever — see the doc on [`TodoItem::needs`].
    Child { id: String },
    /// **A background JOB, named by the handle `bash background: true` and `job_list` print** —
    /// [`TodoNeed::Child`]'s sibling one layer down: the row cannot start until that job ENDS, any
    /// ending, and while it runs the idle check stays silent about the row rather than asking for
    /// work that is blocked on something the plan cannot move.
    ///
    /// **The distinction from a `when` is the whole of why this variant exists**, because the two
    /// read the SAME handle out of the same table and mean opposite things. `when: TodoCondition::Job`
    /// is the operator's clock — the row FIRES when the job ends, and the check is asked for it. A
    /// `needs` entry is an EDGE — the row cannot start until the job is over, and the check is
    /// silent about it while it is not. Measured, 2026-10-11: the operator's own long jobs each had
    /// a row nagging while they ran, and there was no way to say *this waits on that job*.
    ///
    /// **The handle is the full one** `bash background: true` and `job_list` print, the same rule
    /// [`TodoNeed::Child`] keeps for a session id and for the same reason: it is what `job_output`
    /// and `job_kill` are given, so a shortened handle is a name nothing answers to.
    ///
    /// **Whether that job is RUNNING is not on this board, and this variant does not pretend it
    /// is** — the process table is the daemon's and the store holds only what outlived it, so the
    /// caller hands a snapshot in and `unmet_needs` reads it there. What a board alone can say
    /// about a job need is nothing, and a caller with no daemon behind it treats the job as ended:
    /// **a handle that is not running — including one this session has never heard of, which is
    /// what a handle looks like after a restart — is a job that ended**, which is
    /// [`TodoCondition::Job`]'s own ruling one variant up and the reason the two cannot disagree.
    Job { handle: String },
    /// **The OPERATOR, asked a question** — the row cannot start until a person answers it.
    ///
    /// The operator's ask, verbatim: *"a todo can be dependent on todo, and todo can be dependend
    /// on ME. if model says todo is dependent on ME the other heads of the todo DAG keep firing,
    /// model told to try to figure it out anyways, and the heads show loud notification that im
    /// waited. i guess we will force models to do choice for todo - so when it says it waits for
    /// ME it give me that choice list with an optioin to enter my freetext. and then when i submit
    /// todo becomes unblocked with reply attached"*.
    ///
    /// **`options` are the model's offered choices, and there are always at least two** — *"we
    /// will force models to do choice"*: `todo_write` refuses fewer, so the operator is never
    /// handed an open-ended question where a choice would do. Free text and an abstention are
    /// offered by the head's card IN ADDITION, which is `QuestionAnswer`'s vocabulary and not
    /// this row's: the card is the same one `ask_user_question` draws.
    ///
    /// **Whether it is answered is ON THE BOARD, unlike a child or a job** — nothing outside the
    /// row knows, so nothing has to be handed in: the edge is unmet while it is here, and the
    /// operator's answer REPLACES it with [`TodoNeed::Answered`]. That is also what makes it
    /// survive a restart: the store holds the row, the row holds the question.
    Operator {
        question: String,
        options: Vec<String>,
    },
    /// **An [`TodoNeed::Operator`] edge the operator has ANSWERED** — met, always, and kept on the
    /// row as the reply: *"when i submit todo becomes unblocked with reply attached"*.
    ///
    /// **An edge rather than words appended to the row, and the reason is the key.** A row's
    /// words are its identity — `Row` edges, `update` and the operator's own pane all resolve a
    /// row by its exact `content` — so writing the reply into `content` would re-name the row
    /// under every edge into it and under the model's next restatement of its plan (which would
    /// then ADD the old words back as a new row, still waiting). Kept here, the title and the
    /// key are untouched and the reply rides with the row wherever the row goes.
    ///
    /// **The model cannot write one and cannot clear one** — `todo_write` refuses the kind, and a
    /// restatement of the row keeps the answers it already has — because an answer is the
    /// operator's word. A question the operator already answered on this row is therefore not
    /// asked again by restating it: the restatement's `operator` edge is dropped against the
    /// answer with the same question.
    ///
    /// `reply` is the answer as a sentence — the option chosen and the note on it, the words the
    /// operator typed, or that they abstained — the same words the model was told.
    Answered { question: String, reply: String },
    /// **A kind this build does not know** — a need written by a build that has one this one does
    /// not, and it is here because the alternative is worse in a way that is MEASURED, not imagined:
    /// `Harness::new` restores a resumed session's board with `store.todos(id).unwrap_or_default()`,
    /// so a `TodoNeed` that refused to deserialize would not fail loudly — it would empty the whole
    /// PLAN, silently, on the first resume after a newer daemon wrote a kind this one lacks. That is
    /// the one outcome the fail-loud rule exists to prevent, and it is a bigger loss than this one.
    ///
    /// **What is lost is the kind's own payload** — `#[serde(other)]` catches the tag and nothing
    /// else, so a round trip through this build spells the need `unknown`. What is NOT lost is the
    /// ANSWER: the row is blocked, and the check says the kind cannot be evaluated. A need nobody
    /// can answer must never read as met, and this is how a reader that does not know one SAYS SO
    /// rather than misreading it — which is exactly the promise [`TodoCondition`] makes in its own
    /// words, kept here where the cost of breaking it is a plan rather than a row.
    #[serde(other)]
    Unknown,
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
    /// by exact text), and it reaches neither the child's rows nor the operator's — see
    /// `TodoBoard::upsert_parent`.
    Parent(String),
    /// **The merge queue wrote it on the GATEKEEPER SESSION's board.** The fourth author, and it
    /// is not a session at all: the daemon's queue is the object the operator's merge-queue pane
    /// already draws, and its rows on a session's board are that queue seen as work.
    ///
    /// # The design this comes from, in the operator's words
    ///
    /// *"reviews are subagents managed by the main gatekeeper session which is hardcoded … it is a
    /// normal auto-driving session that treats review queue as todo list - so nags and all"*, and
    /// *"parents with children sending their homework to the teachers. School administration doesnt
    /// teach or checks homework. it just makes sure pencils are here and rooms are warm"*. So the
    /// daemon is the ADMINISTRATION: it fills the queue, keeps the worktrees and runs the gates —
    /// and the teacher is a session, whose board is the queue.
    ///
    /// # Why it is its own word and not one of the three
    ///
    /// * **Not the model's.** A row of the model's is a row the model wrote about its own plan, and
    ///   `todo_write` disposes of it. A queue row is not the session's to strike off: the VERDICT
    ///   takes it off, and a session that could close its own queue row could close a review nobody
    ///   made.
    /// * **Not the operator's.** That half is replaced wholesale by the head on every push
    ///   (`TodoBoard::set_operator`), so a daemon writing queue rows there would have them erased by
    ///   the next `HELLO` and would erase the operator's own rows in between.
    /// * **Not a `Parent`'s.** `Parent <session-id>` is a claim about a session that wrote a child's
    ///   board. The daemon is not that session and does not write on its behalf, and the house rule
    ///   for an author string is that it says what is TRUE.
    ///
    /// # The rows are a VIEW, and the STATE is not here
    ///
    /// A queue row is written from the store's own queue tables every pass and the half is
    /// REPLACED, not accumulated — so a row leaves the board when its entry leaves the queue, and
    /// any write to it is corrected by the next pass. **The state of an entry is `ReviewRow::state`
    /// and `ReviewRow::verdict` in the store and nowhere else**: the board is a projection a
    /// session can be nagged about, never a second place a state can be read from. What counts the
    /// queue — a status line, a pane, a head that is not this daemon — counts the store.
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
    /// **Struck off by the OPERATOR — and the row STAYS.**
    ///
    /// The operator's two rulings, and this word is the one that makes them both true: *"only i
    /// should be able to delete todo items. as a rule everything that ever created stays in
    /// history"*, and *"so done items or canceled items should be kept"*. So `/todo rm N` is not a
    /// removal any more — it is this status. The row keeps its words, its author and its place on
    /// the board, the model still sees it (marked, with the mark's meaning spelled out under the
    /// list), and the idle check never asks about it again.
    ///
    /// **A fifth word rather than a quieter `Postponed`.** `Postponed` is work still owed and
    /// deliberately not being asked for — the operator intends to come back to it; `Cancelled` is
    /// work the operator has decided against. They draw differently, they read differently to the
    /// model, and one is the other's undo: `/todo resume N` lifts either, which is what makes a
    /// `rm` a decision rather than an accident nobody can take back.
    ///
    /// **It is the operator's act and not the model's**, on `Postponed`'s own argument: a model
    /// that could strike off a row would have a way to retire work it was asked for, and the whole
    /// point of the row staying on the board is that a person can see it was there. `todo_write`
    /// still takes the three words it took before.
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
    /// in the one file; and the retry discipline is `store/busy.rs` — every write retried on
    /// busy for [`BUSY_RETRY_TOTAL`], write transactions `BEGIN IMMEDIATE`, long copies chunked
    /// by [`Store::append_items`] — because on 2026-10-12 thirty seconds was not enough either:
    /// a commit cost ~411 ms of fsync and the busy handler is a poll, not a queue.
    pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    fn from_connection(conn: Connection, path: Option<std::path::PathBuf>) -> Result<Self> {
        // WAL lets readers and the writer run at once, but two *writers* still
        // serialise, and rusqlite's default is to fail immediately rather than wait.
        // Two connections exist by design — the worker writes rows, a second one
        // answers a head's list and writes a title — so a rename landing during a
        // turn's `append_item` must wait a moment rather than come back as
        // SQLITE_BUSY on a database that is working exactly as intended.
        //
        // The timeout FIRST: `journal_mode` and `migrate` below already touch the file, and
        // an open that lands while another daemon commits must wait like any other statement
        // rather than run on rusqlite's default.
        conn.busy_timeout(Self::BUSY_TIMEOUT)?;
        busy::retry_sql(|| conn.pragma_update(None, "journal_mode", "WAL"))?;
        // The region is volatile by design, so this file is the only copy of the
        // tokens. A lost commit is a lost turn.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let store = Store { conn, path };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        // **Only a missing table is a fresh store.** This was `.unwrap_or(None)`, which read
        // EVERY failure as "no schema yet" — including `database is locked` from an open that
        // landed on a busy store, after which the arm below stamped the current version over
        // a store whose migrations had not run. A busy read is waited out; anything else that
        // is not "no such table" is reported.
        let found: Option<i64> = match busy::retry_sql(|| {
            self.conn
                .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
                .optional()
        }) {
            Ok(v) => v,
            Err(e) if e.to_string().contains("no such table") => None,
            Err(e) => return Err(e.into()),
        };
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
        self.conn.execute_batch(queues::QUEUES_SQL)?;
        self.conn.execute_batch(subagents::SUBAGENTS_SQL)?;
        self.conn.execute_batch(modes::MODES_SQL)?;
        self.conn.execute_batch(told::TOLD_SQL)?;
        self.conn.execute_batch(citations::CITATIONS_SQL)?;
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
            // v16: **the merge queue** — see `MergeEntry` (v16–v23; archived at v24, see [`queues`]) for what it is for.
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
        if from < 21 {
            // v21: **the frozen prefix's fingerprint** — see
            // [`StoredSession::prefix_fingerprint`] for what is in it and why the standing
            // notes are not. Written where a session's transcript row is written (its
            // creation, and every fork onto a new prompt), so that a session opened by a
            // later build can be told its message 0 is not what this daemon would seat now.
            //
            // **NULL on every existing row, and deliberately not backfilled here.** A
            // session seated before this column existed has the same fact on disk already —
            // its `stable_prefix` row holds the system text and the tool schemas it was
            // seated with — and the reader falls back to that rather than a migration
            // guessing at a hash it would have to re-derive from text this file cannot see
            // the markers of. *Nobody recorded one here* is the true state of such a row.
            //
            // Idempotent for the reason v6, v12, v13, v14 and v19 are: a fixture walks a
            // current store backwards, so the column can already be here.
            let has: bool = self
                .conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('session') \
                     WHERE name = 'prefix_fingerprint'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn
                    .execute_batch("ALTER TABLE session ADD COLUMN prefix_fingerprint TEXT")?;
            }
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
        if from < 22 {
            // v22: **the landing half's own rows** — see the `merge_queue` comment in
            // [`SCHEMA_SQL`] for where they live and why, and [`MergeGateStep`] for what one row
            // is.
            //
            // **`'[]'` on every existing row, and that is the true state of such a row.** A
            // queue entry written before this column existed has no gate rows because nothing
            // recorded any: `[]` is the entry's own word for *the gate has not run on this
            // entry* (`MergeEntry::gate_steps`), which is exactly what a row from an older
            // build can honestly say. Backfilling a `no_gate` row or a row per step would be
            // this migration inventing a landing it did not witness.
            //
            // Guarded rather than a bare `ALTER TABLE`, for the reason v17's `brief` and v20's
            // `failure` are: a fixture walks a current store backwards by dropping columns and
            // lowering the version, and a fixture that dropped only some of them would otherwise
            // fail here with "duplicate column name" — which reads as corruption rather than as
            // the idempotence every other step in this function has.
            //
            // **The table first, in its v22 shape, `IF NOT EXISTS`** — the v5, v16 and v20 arms'
            // posture, for their reason: a migrated store never runs `SCHEMA_SQL`, and a fixture
            // may arrive at this version with no `merge_queue` table at all (the stood-down
            // fixture builds a bare v18 store and jumps straight here). On a real v21 store this
            // is a no-op and the guarded `ALTER` below does the work; on a bare one it IS the
            // work.
            self.conn.execute_batch(
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
                .conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('merge_queue') WHERE name = 'gate_steps_json'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn.execute_batch(
                    "ALTER TABLE merge_queue ADD COLUMN gate_steps_json TEXT NOT NULL DEFAULT '[]'",
                )?;
            }
        }
        if from < 23 {
            // v23: **the queue a pre-split store was holding is CLEARED — once, on the first
            // open of it by this build.**
            //
            // **`from < 23`, and deliberately not a pair of numbers.** The arm above (v22,
            // `merge_queue.gate_steps_json`) was written while this one was in flight and took
            // the number 22. The newest a store this build opens can be is therefore 22 — one
            // the v22 arm has just migrated — or older still, and it is the RANGE that clears
            // it either way: a pair of numbers would have to be re-derived every time another
            // migration lands between them. See [`SCHEMA_VERSION`].
            //
            // The operator's ruling, verbatim: *"after it all built we need a way out for the
            // pg-noop session. basically i suggest on the first start to reset the whole
            // queue, we are splitting it to reviews and merges anyway."* The split is what
            // makes those rows unreadable rather than merely stale: once an entry is a REVIEW
            // that hands off to a merge, the single-queue states this build wrote
            // (`waiting`/`taken`/`failed`/`conflict`/`stale`/`landed`/`vetoed`) describe a
            // machine it no longer has, and a pane has no honest way to draw one.
            //
            // **Clearing is the honest act here, and it is the operator's own decision rather
            // than a tidy-up nobody asked for.** Those rows belonged to a DIFFERENT MACHINE;
            // clearing them says exactly that, and does not say *this work never happened*.
            // The work is in git — this arm deletes from two tables and touches no branch and
            // no worktree — and that is what makes the clear safe to run on a real store's
            // first open.
            //
            // **`DELETE FROM` is the route taken, and the trigger list is why.** The doc above
            // says a `BEFORE DELETE` trigger raises, so that a migration cannot quietly rewrite
            // history. VERIFIED against [`SCHEMA_SQL`]: the only triggers in this store are
            // `transcript_item_no_update`, `transcript_item_no_delete` and
            // `transcript_item_append_only_insert`, all three on `transcript_item`. Neither
            // `merge_queue` nor `merge_review` carries one, so the drop-and-recreate route (and
            // the drop-the-trigger, delete, restore-it one) is NOT needed and is deliberately
            // not taken: recreating a table means restating its column list, and a restated
            // list drifts from [`SCHEMA_SQL`] the first time one of those columns changes.
            //
            // **Guarded on the table being there at all.** A migrated store never runs
            // [`SCHEMA_SQL`], and a fixture may arrive at this version with no queue in it —
            // the v1, v4 and v20 fixtures build bare stores and jump straight here. `DELETE
            // FROM` a table that does not exist is an error, and a migration that refused an
            // empty file would be a worse bug than the one this arm exists to fix.
            for table in ["merge_queue", "merge_review"] {
                let there: bool = self
                    .conn
                    .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
                    .and_then(|mut st| st.exists(params![table]))
                    .unwrap_or(false);
                if there {
                    self.conn.execute(&format!("DELETE FROM {table}"), [])?;
                }
            }
        }
        if from < 24 {
            // v24: **review and merge are two queues** (R61) — the v23 pair archived under `_v23`
            // names, the new tables created, and the rows decision 13 carries forward re-enqueued
            // for review. The whole of the argument is at [`queues`]'s module docs and at
            // `Store::migrate_queues_to_v24`; nothing is deleted.
            self.migrate_queues_to_v24()?;
        }
        if from < 25 {
            // v25: **the operator's pause of the idle plan-check** — see
            // [`StoredSession::nags_paused_until`] for the three spellings and why the pause is the
            // session's fact rather than the daemon's.
            //
            // NULL in every existing row, and that is the true state of such a row: nobody has
            // paused a session that predates the verb. Guarded rather than a bare `ALTER TABLE`,
            // for the reason v19 and v22 give — a fixture walks a current store backwards — and
            // because the guard is what makes this arm safe to RENUMBER if another migration takes
            // 25 first: it asks whether the column is there, not which number the file carries.
            let has: bool = self
                .conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('session') WHERE name = 'nags_paused_until'",
                )
                .and_then(|mut st| st.exists([]))
                .unwrap_or(false);
            if !has {
                self.conn
                    .execute_batch("ALTER TABLE session ADD COLUMN nags_paused_until INTEGER")?;
            }
        }
        if from < 26 {
            // v26: **a session's subagents, as rows that outlive the daemon** — see
            // [`subagents`]. A new table and an index, both `IF NOT EXISTS`, so the arm is safe to
            // renumber if another migration takes 26 first and safe on a store that somehow already
            // has them. Nothing existing is read or rewritten: a store from before this build has
            // no record of its children, and its running children are not resumed — there is
            // nothing to resume them FROM.
            self.conn.execute_batch(subagents::SUBAGENTS_SQL)?;
        }
        if from < 27 {
            // v27: **the permission modes, as rows** — see [`modes`]. Two new tables, both
            // `IF NOT EXISTS`; nothing existing is read or rewritten. The rows `modes.tsv` held
            // are imported by the daemon (`harnessd::modes`), which is the one that knows the file.
            self.conn.execute_batch(modes::MODES_SQL)?;
        }
        if from < 28 {
            // v28: **whether a job's ending reached the model** — `job.delivered`, see
            // [`Store::deliver_job`]. Every EXISTING row is stamped delivered (`DEFAULT 1`), and
            // that is the conservative reading: this build cannot know which of them were told,
            // and a restart that re-announced every job a store ever ran would bury the one that
            // matters. Rows written from here on are inserted undelivered by `put_job`. Guarded on
            // the column rather than the number, for the reason v25 gives — and on the table: a
            // fixture that is only a `session` table has no `job` to add it to.
            let has = |sql: &str| -> bool {
                self.conn
                    .prepare(sql)
                    .and_then(|mut st| st.exists([]))
                    .unwrap_or(false)
            };
            let table = has("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'job'");
            let column = has("SELECT 1 FROM pragma_table_info('job') WHERE name = 'delivered'");
            if table && !column {
                self.conn.execute_batch(
                    "ALTER TABLE job ADD COLUMN delivered INTEGER NOT NULL DEFAULT 1",
                )?;
            }
        }
        if from < 29 {
            // v29: **what the queues have said, as rows** — see [`told`]. One new table,
            // `IF NOT EXISTS`; nothing existing is read or rewritten. A store from before it has
            // no record of what was rung, so the first daemon on this build may say each
            // standing alert once more — and then never again across a restart.
            self.conn.execute_batch(told::TOLD_SQL)?;
        }
        if from < 30 {
            // v30: **the lines a reply cited** — see [`citations`]. One new table, `IF NOT
            // EXISTS`; a reply from before it has no snapshot, and one is taken the first time
            // it is shown.
            self.conn.execute_batch(citations::CITATIONS_SQL)?;
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

    /// **A fork that is not the session's conversation until it is sealed** (R91.2).
    ///
    /// [`Store::put_fork`] made the new transcript the session's newest — the one a resume
    /// opens ([`Store::current_transcript_id`], `created_at DESC, rowid DESC`) — before a single
    /// row of it was written, so a daemon that died during a long carry (a `/reseat` of 971 rows,
    /// or a wait on a busy store) came back onto a truncated or empty conversation. This writes the
    /// row with `created_at = 0`, which no live transcript has, so every "newest" query keeps
    /// answering with the parent until [`Store::seal_transcript`] stamps it once its rows are all
    /// stored. A fork that is never sealed is a dead branch, never the conversation.
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

    /// **The fork's rows are all stored: it is the session's conversation from now on.** See
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

    /// **Many rows, appended in as few commits as possible** — the same rows, the same
    /// triggers and the same order [`Store::append_item`] writes them in, `first_seq` onward.
    ///
    /// `rows` is `(item, ledger row, tokens)` per row. Committed in chunks of
    /// [`Store::APPEND_CHUNK`] rows, each its own `BEGIN IMMEDIATE` transaction, so the
    /// write lock is taken once per chunk instead of once per row and is never held for a
    /// whole long copy. MEASURED on this box, 2026-10-12, with several workspace test runs
    /// going: an autocommit costs ~411 ms of fsync under `synchronous = FULL`, so the
    /// rano reseat's 971 per-row commits held the lock back to back for six and a half
    /// minutes, which is the window a child died in on `database is locked`.
    ///
    /// A chunk that fails busy is retried whole (nothing of it was committed); a chunk that
    /// fails otherwise stops the append with the rows before it committed, and the error
    /// names the row's `seq`, the way a per-row loop's would.
    ///
    /// **Each chunk lands whole or not at all**, and the insert trigger checks every row of it
    /// exactly as it checks [`Store::append_item`]'s: a chunk that half-landed would leave the
    /// harness's `persisted` count disagreeing with the store, which is the one state a resume
    /// cannot rebuild honestly. A caller that wants a run to be one transaction hands it at most
    /// [`Store::APPEND_CHUNK`] rows (`Harness::persist` does, so that each of its chunks is one
    /// commit and one progress tick).
    ///
    /// The same report made the case a second time, from the head's side: the rano session's
    /// bare `/reseat`, 2026-10-12 01:17, carried 971 rows whose render took two seconds and
    /// whose per-row write took from 01:17:48 to 01:24:07, while the head showed the old
    /// conversation and a `queued` prompt and nothing else.
    pub fn append_items<'a, I>(&self, transcript_id: &str, first_seq: u32, rows: I) -> Result<()>
    where
        I: IntoIterator<Item = (&'a TranscriptItem, &'a LedgerRow, &'a [TokenId])>,
    {
        let rows: Vec<_> = rows.into_iter().collect();
        for (c, chunk) in rows.chunks(Self::APPEND_CHUNK).enumerate() {
            let base = first_seq + (c * Self::APPEND_CHUNK) as u32;
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
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Rows per transaction in [`Store::append_items`]. Small enough that one chunk's commit
    /// holds the lock for one fsync and a few milliseconds of inserts, large enough that a
    /// thousand-row copy is sixteen commits rather than a thousand.
    pub const APPEND_CHUNK: usize = 64;

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
        self.sessions_matching("", &[])
    }

    /// **The one query behind both [`Store::list_sessions`] and [`Store::session`]**,
    /// with an optional `WHERE` over `s` appended — so the 20-column positional reader
    /// below exists once. Two copies of a reader that is read BY POSITION would drift the
    /// first time somebody appended a column to one of them.
    fn sessions_matching(
        &self,
        filter: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<StoredSession>> {
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
        let mut stmt = self.conn.prepare(&format!(
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
                      ORDER BY i.seq DESC LIMIT 1),
                    s.prefix_fingerprint,
                    s.nags_paused_until
               FROM session s{filter}",
        ))?;
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
                    // **Index 19, after the stood-down pair at 17 and 18.** This list is
                    // read BY POSITION and both changes appended to it: the pair came from
                    // the compaction work, the last item from the pane that says how a
                    // child ended. The pair goes first because it is one fact in two
                    // columns — splitting it around an unrelated subquery would be a
                    // reading waiting to be shifted by the next person who appends here.
                    last_item: r
                        .get::<_, Option<String>>(19)?
                        .and_then(|j| serde_json::from_str(&j).ok()),
                    // **Index 20, appended for the same reason 19 was.** The list is read BY
                    // POSITION and this is the newest fact on the row; the fingerprint goes
                    // last so nothing above it shifts.
                    prefix_fingerprint: r.get(20)?,
                    // **Index 21**, appended last for the reason 20 was.
                    nags_paused_until: r.get(21)?,
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

    /// One session, or `None` — **keyed**, so it reads one session's rows and no others.
    ///
    /// It was `list_sessions()` and a `find`, which made every lookup of ONE session a
    /// walk of EVERY session's transcripts: the listing computes a `MAX(created_at)` per
    /// session over its items, and nothing indexes `created_at`, so it reads the whole
    /// `transcript_item` table. MEASURED 2026-10-11 on the operator's 3.3 GB store: 0.54 s
    /// per call with the file hot in the page cache — and the merge queue's review pass made
    /// dozens of them every 30 s (see [`Store::session_parent`]), which held a daemon's only
    /// worker at 100% of a core and left an operator's prompt queued for up to a minute with
    /// nothing on the screen to say why.
    pub fn session(&self, id: &str) -> Result<Option<StoredSession>> {
        Ok(self
            .sessions_matching(" WHERE s.id = ?1", &[&id])?
            .into_iter()
            .next())
    }

    /// **A session's parent, and nothing else** — `Some(None)` for a root, `None` for an
    /// id this store does not hold.
    ///
    /// For the callers that walk a parent chain. They were calling [`Store::session`] at
    /// every step, which built a whole [`StoredSession`] — the transcript joins, the last
    /// item, the activity time — to read one column of the `session` row. This reads that
    /// column off the primary key and nothing else.
    pub fn session_parent(&self, id: &str) -> Result<Option<Option<String>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT parent_session_id FROM session WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?)
    }

    /// **A session's workspace, and nothing else** — `None` for an id this store does not
    /// hold. The same reasoning as [`Store::session_parent`], for the caller that only asks
    /// which folder a session belongs to.
    pub fn session_workspace(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT workspace_root FROM session WHERE id = ?1",
                params![id],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Name a session, or clear its name with an empty string.
    ///
    /// `session` carries no append-only trigger and is not part of the chain: a
    /// title is metadata *about* a conversation and changing it changes no prompt
    /// byte. That is the whole reason renaming is allowed here while
    /// [`Store::append_item`]'s rows cannot be touched at all.
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
        let n = self.exec(
            "UPDATE session SET context_window = ?2 WHERE id = ?1",
            params![id, window.map(|w| w as i64)],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// **Write the fingerprint of the frozen prefix this session is seated with.** See
    /// [`StoredSession::prefix_fingerprint`] for what it is.
    ///
    /// **Written where a session's transcript row is written, and nowhere else.** That is
    /// the whole rule, and it is what makes the column mean one thing: *the prefix the
    /// session's CURRENT transcript was opened under*. Two callers obey it — the open that
    /// creates a session, and the fork that re-seats one onto a new prompt — and a caller
    /// that wrote it anywhere else would be recording a prefix no transcript speaks.
    ///
    /// A setter of its own rather than a field on [`SessionRecord`], for the reason
    /// [`Store::set_window`] is one: a `SessionRecord` is the session's creation and this
    /// fact outlives it — a re-seat writes it again, long after the row exists. Keeping it
    /// out of `put_session` also keeps it out of the twenty-odd fixtures that build a
    /// `SessionRecord` to test something else.
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

    /// **Write the pair that stood automatic compaction down.** See
    /// [`StoredSession::auto_compact_stood_down`].
    ///
    /// A setter of its own for the same reason [`Store::set_window`] has one: the two
    /// facts have different lifetimes, and folding them into another setter's
    /// parameter list would say they were written together. This pair is written
    /// when the wall controller goes `Stuck` (every fold MEASURED still at the wall),
    /// and `None` clears it when the controller is released — by a measured count
    /// comfortably below the wall, `/compact` or `/auto-compact on`. It used to be
    /// written once and never cleared, and on 2026-10-11 it was written on an
    /// estimate (pg-noop: ≈ 1,039,055, measured 507,355): a finding that cannot
    /// expire must not be made on a number nobody measured.
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

    /// **Pause this session's idle plan-check, or end the pause** —
    /// [`StoredSession::nags_paused_until`]'s setter, in its spelling: `None` clears it, `Some(0)`
    /// is until the operator resumes, `Some(ms)` ends at that unix millisecond.
    ///
    /// A setter of its own for [`Store::set_auto_compact_stood_down`]'s reason: it is written by
    /// one act (the operator's `/nag`) and cleared by its two ends (the verb, or the pause's own
    /// deadline), and nothing else on the row moves with it.
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
        let n = self.exec(
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

    /// **Every session with a row waiting on the operator, and how many questions are open** —
    /// the picker's mark, read for all sessions in one pass.
    ///
    /// **Narrowed in SQL, decided in Rust.** The `LIKE` is only the cheap half: it keeps the rows
    /// the JSON could hold an `operator` edge in, and [`operator_asks`] — the one reading every
    /// other door uses — decides, so a done row's leftover question or a string that merely
    /// contains the tag is not counted. A list that cannot be parsed counts nothing rather than
    /// failing the listing: a picker that refused to draw over one bad row would hide every
    /// session to report one.
    pub fn sessions_waiting_on_operator(&self) -> Result<BTreeMap<String, usize>> {
        let mut st = self.conn.prepare(
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

    /// Replace the session's todo list wholesale. The whole list every time —
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

    /// **Every job this session has recorded**, ordered by when its row last moved — see
    /// [`JobRecord`].
    ///
    /// `handle` breaks the tie, so two jobs written in the same millisecond do not come back in
    /// an order that changes between calls: a listing that reorders itself costs the model its
    /// prefix cache, which is the same reason the monitor registry orders by name.
    pub fn jobs(&self, session_id: &str) -> Result<Vec<JobRecord>> {
        self.jobs_where(session_id, "")
    }

    /// [`Store::jobs`], narrowed by a fixed SQL condition (never a caller's text).
    fn jobs_where(&self, session_id: &str, and: &str) -> Result<Vec<JobRecord>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT handle, command, how, state, produced, elapsed_ms, redirect
               FROM job WHERE session_id = ?1 {and} ORDER BY updated_ms, handle"
        ))?;
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

    /// **This job's ending has reached the model** (v28) — set where that is true
    /// (`JobWatchers::read_now`, beside `deliver_subagent_run`), and by a restart that said it.
    /// `put_job` never touches the flag after the insert, so a settlement written after the
    /// delivery does not undo it. A handle with no row writes nothing.
    pub fn deliver_job(&self, session_id: &str, handle: &str) -> Result<()> {
        self.exec(
            "UPDATE job SET delivered = 1 WHERE session_id = ?1 AND handle = ?2",
            params![session_id, handle],
        )?;
        Ok(())
    }

    /// **The jobs of this session that ENDED and were never delivered** — what a daemon that
    /// stopped while holding a result (R79) left for the next one to say (TODO.md R91.5). A
    /// `running` row is not here: that is a job the daemon was watching when it died, which
    /// `jobs` already answers.
    pub fn undelivered_jobs(&self, session_id: &str) -> Result<Vec<JobRecord>> {
        self.jobs_where(session_id, "AND state <> 'running' AND delivered = 0")
    }

    /// **The highest `jN` handle any job in this store was given**, across every session — what a
    /// daemon seeds its handle counter above at start, so a handle is never minted twice for one
    /// session across restarts (TODO.md R91.3). `0` for a store with no `jN` handle.
    pub fn max_job_number(&self) -> Result<u64> {
        let mut st = self
            .conn
            .prepare("SELECT handle FROM job WHERE handle GLOB 'j[0-9]*'")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut max = 0u64;
        for h in rows {
            if let Some(n) = h?.strip_prefix('j').and_then(|n| n.parse::<u64>().ok()) {
                max = max.max(n);
            }
        }
        Ok(max)
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

    /// **One autocommit write statement, taken again while the store is busy** — see
    /// [`busy`](self::busy) for the report this answers and why a retry writes once.
    ///
    /// Every write method of this store that is a single statement goes through here; a
    /// write that is several statements goes through [`Store::write_tx`] instead.
    fn exec<P: Params + Clone>(&self, sql: &str, p: P) -> rusqlite::Result<usize> {
        busy::retry_sql(|| self.conn.execute(sql, p.clone()))
    }

    /// **A write transaction: `BEGIN IMMEDIATE`, the body, `COMMIT` — the whole of it taken
    /// again while the store is busy.**
    ///
    /// IMMEDIATE because a deferred transaction that READS first holds a snapshot, and its
    /// first write must then promote that snapshot to the write lock. SQLite refuses that
    /// promotion at once when another connection holds the lock — the busy handler is not
    /// consulted, because the connection is already inside a transaction — so the thirty
    /// seconds of `BUSY_TIMEOUT` never apply to it. `BEGIN IMMEDIATE` takes the write lock
    /// before the first read, and waiting for it is what the busy handler does.
    ///
    /// The body is run again from the top on a busy failure, so it must be a pure function of
    /// the store (it is: it is given only the transaction).
    pub fn write_tx<T>(
        &self,
        mut body: impl FnMut(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        busy::retry_store(|| {
            let tx =
                rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            let out = body(&tx)?;
            tx.commit()?;
            Ok(out)
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
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
