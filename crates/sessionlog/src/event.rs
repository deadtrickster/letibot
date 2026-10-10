//! §4.5's event enum, verbatim, plus the envelope that carries it.
//!
//! ```text
//! TurnStarted{turn_id, model, ledger_head}
//! PromptProgress{turn_id, total, cache, processed, time_ms}
//! Delta{turn_id, target: Text|Reasoning, text}
//! ToolCallProposed{turn_id, call_id, name, args_digest}
//! DecisionRequested{req_id, kind, call_id, summary, options, deadline, on_timeout}
//! DecisionAnswered{req_id, outcome, by, basis, late: bool}
//! ToolStarted / ToolProgress / ToolFinished{call_id, outcome, ...}
//! TurnFinished{turn_id, finish_reason, usage, timings}
//! TurnInterrupted{turn_id, reason, partial_kept: bool}
//! TurnFailed{turn_id, error, partial_kept: bool}   — not §4.5's; see the variant
//! TranscriptAppended{item_id, kind, ledger_head}
//! TranscriptContent{item_id, item}          — not §4.5's; see the variant
//! HeadAttached / HeadDetached{head_id, kind, identity}
//! Warning{code, detail}
//! Explain{turn_id, plan}
//! ```
//!
//! Three notes on where this is *not* a transcription, and why.
//!
//! 1. **`ToolStarted` / `ToolProgress` are given by name only** in §4.5 (the `…`
//!    covers their fields). Their shapes were this crate's invention and W9 was
//!    the strand that would find out whether they were right. It has, by building
//!    the runtime that emits them, and the answer was: two of the three were
//!    short. `ToolStarted` gained `turn_id` and `access`; `ToolFinished` gained
//!    `turn_id`, the `inline`/`full` byte split, the spill locator and the repair
//!    count; `ToolProgress`'s free-text `note` was right as it stood. See
//!    `letibot_tools::events` for the argument in full.
//! 2. **`Explain{plan}` is a `serde_json::Value`.** W14 owes `ExplainPlan` as a
//!    type (§6.2 gives a rendered example and no field list). Typing it here would
//!    be inventing W14's contract from a screenshot.
//! 3. **`usage` carries `cached_tokens`.** §4.5 says "usage" without a field list;
//!    a head that cannot show cache reuse cannot show the one number this whole
//!    harness exists to move.
//!
//! **`Delta` carries only the increment.** Enforced the same way `letibot-turn`
//! enforces it: the variant has a `text` and there is nowhere to put a `full`.

use serde::{Deserialize, Serialize};

/// Which channel a delta belongs to.
///
/// `ToolCall` is the third, and it is the reason `PROTOCOL_VERSION` is 3. It
/// carries the raw `<function=…>` markup the model writes inside a `<tool_call>`
/// block, which used to travel as `Text` because there was nowhere else to put it
/// — the T13.5 gap, which a head could only work around by guessing. See
/// `letibot_turn::DeltaTarget` for why guessing is not available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaTarget {
    Text,
    Reasoning,
    ToolCall,
}

/// §5.6's prefill progress, as it reaches a head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PromptProgress {
    pub total: u64,
    pub cache: u64,
    pub processed: u64,
    pub time_ms: u64,
}

/// Both sides of a file-editing call, bounded to what differs, as a
/// [`SessionEvent::ToolFinished`] carries it.
///
/// The log's own name for the excerpt, which now lives in
/// [`letibot_transcript::ToolEditExcerpt`] — the one crate this crate and the
/// runtime can both see. It used to be defined here and lifted field by field
/// from `letibot_tools` in `lift_tools.rs`; two copies of a nine-field struct
/// with a lift between them is how copies drift, and the transcript row now
/// carries the excerpt too, so a third copy was about to exist. The wire shape
/// is unchanged: same fields, same names.
pub use letibot_transcript::ToolEditExcerpt as ToolEdit;

/// A todo's state, on the wire. The same five words the store spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    /// **The operator set the row aside** — see
    /// `letibot_tokencore::store::TodoStatus::Postponed` for the whole of what the state means: the
    /// row persists, the model still sees it, and the idle check stops asking about it. A copy of
    /// that type and not a re-export, for the reason [`TodoBy`] gives, so the word is spelled on
    /// both sides and a variant added to one fails to compile on the other.
    ///
    /// **Added at `PROTOCOL_VERSION` 36**, and it is the bump case this file's own history
    /// section names: no frame is added, and the number still moves, because a head built before
    /// it cannot decode the word — and the failure is the whole frame, not the row.
    Postponed,
    /// **The operator STRUCK THE ROW OFF, and the row stays** — see
    /// `letibot_tokencore::store::TodoStatus::Cancelled`: `/todo rm N` writes this instead of
    /// removing the row, because *"only i should be able to delete todo items. as a rule everything
    /// that ever created stays in history"*.
    ///
    /// **Added at `PROTOCOL_VERSION` 41**, on `Postponed`'s own argument one word over: no frame is
    /// added, and the number still moves, because `serde` has no catch-all on this enum and the
    /// word travels inside `TodosUpdated`/`ServerFrame::Todos`, which carry the whole list — a
    /// version-40 head cannot DECODE `"cancelled"`, and the failure takes every row beside it down
    /// with it, mid-session.
    Cancelled,
}

/// **Who wrote a todo**, on the wire — the whole of the difference between the operator's items and
/// the model's, which share one list, one format and one tool. The operator's ruling: *"the existing
/// getter should return mine and yours, and the rest is also the same. the only difference is who
/// created and that is it."* A third author arrived with parents writing their children's boards:
/// *"yes - i want parent agents to be able to create todos for subagents. throught tree author -
/// (Parent <session-id-of-parent>)"*.
///
/// A copy of `letibot_tokencore::store::TodoBy` rather than a re-export, for the reason
/// `TodoStatus` above is already a copy: this crate is the WIRE, and a wire type that aliases a
/// storage type changes when the storage does. The two are converted at the one seam that owns the
/// board.
///
/// **`Parent` carries the author string, so `by` is a bare string on the wire** — `"model"`,
/// `"operator"`, `"Parent s-…"` — spelled by custom impls on both copies, and **adding the variant
/// moved `PROTOCOL_VERSION` to 37** for the reason `TodoStatus::Postponed`'s own doc gives: no frame
/// is added, and the number still moves, because a head built before it cannot decode the word —
/// and the failure is the whole `TodosUpdated`/`Todos` frame, not the row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TodoBy {
    #[default]
    Model,
    Operator,
    /// **A parent session wrote it on a child's board.** The string is the author exactly as the
    /// operator specified — `Parent <full parent session id>` — so a child reading its own board
    /// can tell what it decided from what it was told, and by whom.
    Parent(String),
}

impl TodoBy {
    /// The author string for a parent's row: `Parent <session-id>`, verbatim and in full.
    pub fn parent_of(session_id: &str) -> TodoBy {
        TodoBy::Parent(format!("Parent {session_id}"))
    }
}

impl Serialize for TodoBy {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            TodoBy::Model => s.serialize_str("model"),
            TodoBy::Operator => s.serialize_str("operator"),
            // The variant CARRIES the author string, so the wire form is the string itself.
            TodoBy::Parent(author) => s.serialize_str(author),
        }
    }
}

impl<'de> Deserialize<'de> for TodoBy {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<TodoBy, D::Error> {
        let word = String::deserialize(d)?;
        match word.as_str() {
            "model" => Ok(TodoBy::Model),
            "operator" => Ok(TodoBy::Operator),
            // `Parent …` is the one open spelling; anything else is a word this reader does not
            // know, refused by name rather than read as the model's the way a catch-all would.
            other if other.starts_with("Parent ") => Ok(TodoBy::Parent(other.to_string())),
            other => Err(serde::de::Error::custom(format!(
                "`{other}` is not a todo author: model, operator, or `Parent <session-id>`"
            ))),
        }
    }
}

/// **A path a gated action opens for writing** — R35's field, and the shape the other head's
/// renderer was written against before there was ever a value for it.
///
/// ```text
/// write_targets: [{"path": "src/syntax.rs", "unresolved": false},
///                 {"path": "Path.home() / name", "unresolved": true}]
/// ```
///
/// **`unresolved` is not `path: ""`, and the difference is the requirement's own sentence.** A write
/// whose target could not be read — `open(sys.argv[1], 'w')`, a path built at run time — is *"the one
/// the operator most needs to see"*: it is the fact a person cannot get any other way, and a head
/// that folded it into *no write* would be throwing it away. So it is a flag on an entry rather than
/// an absent entry, and the renderer draws it in the attention register because a sentence that looks
/// like a path is a sentence that gets skimmed past.
///
/// **An EMPTY list is not a claim that the call writes nothing** — it is the honest reading of *no
/// write the scanner could place*, which is why a script it could not read yields an entry here
/// rather than silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteTarget {
    /// The path, or the text that names a path the classifier could not place.
    pub path: String,
    /// **Could the classifier place it?** `true` draws in the attention register.
    #[serde(default)]
    pub unresolved: bool,
}

/// One line of a session's todo list, on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoEntry {
    pub content: String,
    pub status: TodoStatus,
    /// **Who wrote the row** — the whole of the difference between the operator's items and the
    /// model's, which share this list, this format and this tool. See `TodoItem::by`.
    #[serde(default)]
    pub by: TodoBy,
    /// **What the row waits on**, when it waits for something rather than simply being undone —
    /// see `TodoItem::when` for the shape, for why a condition nobody can evaluate must never
    /// read as *met*, and for why it is EVALUATED rather than observed (so a restart of either
    /// side does not lose the firing).
    ///
    /// `serde(default)`, so a daemon older than this field and every row already written read
    /// back as *unconditional*, which is what they were.
    #[serde(default)]
    pub when: Option<TodoCondition>,
    /// **The rows and children this one waits for** — see `TodoItem::needs` for what an edge
    /// means, for why an unresolvable one is never *met*, and for why a child is named by its
    /// session id rather than by its words.
    ///
    /// **An ADDED, DEFAULTED FIELD on an existing struct, so no `PROTOCOL_VERSION` bump** —
    /// this file's own history section names the case: a head built before it ignores the key
    /// and draws exactly the row it drew before, and a head built after it reading an older
    /// daemon sees `[]` and draws that same row, because a row with no edges is what it was
    /// given. Nothing here is a word an older peer cannot DECODE, which is the one thing the
    /// bumps in this file are for.
    ///
    /// **It is here because the pane has to be able to CARRY an edge, not only draw one.**
    /// `SetOperatorTodos` replaces the operator's whole half of the board, and until this field
    /// existed every edge on an operator's row was dropped at that door — the daemon's own
    /// comment on that conversion said so (*"the wire carries no edges yet"*). Now the head
    /// echoes back exactly what it was given, so the round trip is lossless.
    #[serde(default)]
    pub needs: Vec<TodoNeed>,
}

/// **One row's state, named by its words** — what an operator's `/todo postpone|resume` carries
/// when the row it names is not one of theirs.
///
/// **Why it is not a `TodoEntry`.** A `TodoEntry` says who WROTE a row (`by`), what it waits on
/// (`when`) and what it waits for (`needs`) — none of which this carries, and all of which would be
/// a lie if it did: the operator moving a row's state changes none of them, and the author is the
/// one thing about the row that must not be restated by a head. So it is two fields, and the wire
/// says exactly what the act is.
///
/// **The words are the key**, which is the board's own identity rule: `TodoEntry` has no id (the
/// operator ruled out a bump for one), `TodoBoard::set_operator_states` resolves the operator's
/// rows by their exact trimmed text, and a plan's edges are a row's words for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoState {
    /// The row's own words, exactly as the board shows them.
    pub content: String,
    pub status: TodoStatus,
}

/// **What a row waits on**, as the WIRE spells it — **a copy of
/// `letibot_tokencore::store::TodoNeed`**, not a re-export, for the reason [`TodoCondition`]
/// below is already a copy of the store's: this crate is the wire, and a wire type that aliases
/// a store type makes one crate's rename a protocol change. The conversion lives where the wire
/// meets the store (harnessd's `todo_entry` and the `SetOperatorTodos` path), and it is a
/// `match`, so a variant added on one side fails to compile on the other rather than arriving as
/// a need nobody can evaluate.
///
/// **The tag is the contract**, exactly as it is for [`TodoCondition`]: `kind` and the snake_case
/// names are what an older head reads past and what a newer one reads by, so a new variant is
/// additive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoNeed {
    /// Another row on this board, by its exact `content`.
    Row { content: String },
    /// **A CHILD session, by its session id** — the handle `task` handed back.
    Child { id: String },
    /// A kind this build does not know. See `TodoNeed::Unknown` in the store for why an unknown
    /// edge is kept rather than refused: a plan is what would be lost.
    #[serde(other)]
    Unknown,
}

/// **What a row waits on**, as the WIRE spells it — **a copy of
/// `letibot_tokencore::store::TodoCondition`**, not a re-export, for the reason [`TodoBy`]
/// above is already a copy: this crate is the wire, and a wire type that aliases a store type
/// makes one crate's rename a protocol change. The conversion lives where the wire meets the
/// store (harnessd's `todo_entry` and the head's `SetOperatorTodos` path), and it is a `match`,
/// so a variant added on one side fails to compile on the other rather than arriving as a
/// condition nobody can evaluate.
///
/// **The tag is the contract.** `kind` and the snake_case names are what an older head reads
/// past and what a newer one reads by, so a new variant is additive: a reader that does not
/// know it can say so instead of taking it for *met*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoCondition {
    /// **Due when this job is no longer running** — the handle `bash background: true` and
    /// `task` hand back, and the one `job_list` prints.
    Job { handle: String },
}

/// **The merge queue's priority, as the wire spells it** — a copy of
/// `letibot_tokencore::store::MergePriority`, not a re-export, for the reason [`TodoCondition`]
/// is already a copy: this crate is the wire, and a wire type that aliases a store type makes
/// one crate's rename a protocol change. The conversion lives where the wire meets the store
/// (harnessd's merge-queue daemon), and it is a `match`, so a rung added on one side fails to
/// compile on the other rather than arriving as a priority nobody can order.
///
/// The tag is the contract: the snake_case names are what an older head reads past and what a
/// newer one reads by, so a rung added here is additive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergePriority {
    /// The operator's entry: it jumps every subagent entry, whatever the age.
    Urgent,
    /// A subagent's entry: it waits behind an operator's urgent entry; inside the rung it is
    /// oldest-first.
    Subagent,
}

/// **Where a merge-queue entry is, as the wire spells it** — a copy of
/// `letibot_tokencore::store::MergeState`, not a re-export, for the reason [`MergePriority`]
/// is already a copy. The states are the queue's own vocabulary, and a state that is not on
/// this list is a state the pane cannot draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    /// In the queue, not yet taken; ready once its `needs` have all `Landed`.
    Waiting,
    /// The daemon has it and is working — rebasing at the tip and running the gate.
    Taken,
    /// Merged to main; the worktree is removed and the branch deleted.
    Landed,
    /// The gate failed; the worktree stays, with the reason on the row.
    Failed,
    /// A rebase conflict; the worktree stays, with the reason on the row.
    Conflict,
    /// The gate job died with the daemon; it is listed with its reason, not dropped.
    Stale,
    /// **A PERSON rejected it** (`PROTOCOL_VERSION` 38) — the operator's verdict, and not a
    /// gatekeeper's. A state of its own rather than a sentence on a `failed` row because a
    /// person's rejection and a machine's must not draw the same way: the head draws `failed`,
    /// `conflict` and `stale` as the loud, red ones, and the operator's requirement is that a
    /// veto does not (`"a vetoed entry must not draw as red"`). It is terminal: the queue only
    /// ever takes a `waiting` entry, so a vetoed one is never reviewed or gated again. The way
    /// back is a person's — `approve` reverses the decision it was made by, `rm` forgets it.
    Vetoed,
}

/// **One entry in the merge queue, on the wire** — a copy of
/// `letibot_tokencore::store::MergeEntry`, not a re-export, for the reason [`MergeState`] is
/// already a copy. The snapshot carries the whole queue, every state, and an entry the queue
/// cannot act on is listed with its reason — the `evidence` on the row says what it is waiting
/// on, why it failed, or why it is stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeEntry {
    /// The id the enqueuer minted, and the one the queue's events name.
    pub id: String,
    /// The session that enqueued it.
    pub session_id: String,
    /// The branch to merge, and the one checked out in the entry's worktree.
    pub branch: String,
    /// The SHA the entry was written against — the tip of main when the branch was cut.
    pub base_sha: String,
    /// The queue's priority, out of [`MergePriority`]'s closed set.
    pub priority: MergePriority,
    /// The entries this one depends on, by id.
    pub needs: Vec<String>,
    /// Where the entry is, out of [`MergeState`]'s closed set.
    pub state: MergeState,
    /// **The ask the branch was produced under, verbatim** — the brief the child was given,
    /// carried on the entry because the reviewer reads it.
    ///
    /// `serde(default)` and additive: a head older than the field reads past it, and a daemon
    /// older than it sends none — an empty brief is *nobody recorded one*, which is a real
    /// state and not an error. The gatekeeper's protocol is brief-first (it is given the ask
    /// and NOT the child's report), so this is the field that makes a review possible at all
    /// rather than a nice-to-have the pane draws.
    #[serde(default)]
    pub brief: String,
    /// The reason for the state, in the queue's own words.
    pub evidence: String,
    /// When the entry was enqueued, Unix ms.
    pub created_ms: u64,
    /// When the entry last moved, Unix ms.
    pub updated_ms: u64,
    /// Where the branch is checked out, when it is.
    #[serde(default)]
    pub worktree: Option<String>,
    /// The tip the entry landed at, set when it moves to `Landed`.
    #[serde(default)]
    pub landed_sha: Option<String>,
    /// **The landing half's rows: one per gate step, in the order `main` declared them** — a
    /// copy of `letibot_tokencore::store::MergeGateStep`, for the reason [`MergeEntry`] is a copy
    /// of its own row.
    ///
    /// **`serde(default)`, so no `PROTOCOL_VERSION` bump** — this file's own test for whether a
    /// bump is owed: an older head ignores an unknown key and draws exactly the row it drew
    /// before, and a newer head reading an older daemon sees `[]` and draws the same row,
    /// because *the gate has not run on this entry* is what a daemon from before this field can
    /// honestly say. Nothing here is a word an older peer cannot DECODE, which is the one thing
    /// the bumps in this file are for.
    ///
    /// **It is on the wire because the note's argument for the merge half is that a thing being
    /// RUN can be drawn well**, and a drawing needs the steps rather than a verdict: a head
    /// handed only `failed` can only ever draw `failed`. Empty is a fact and not a missing one —
    /// see the store's own doc on the field — and a repository with no gate sends the one
    /// [`MergeGateOutcome::NoGate`] row rather than nothing.
    ///
    /// **The live half of it does not travel.** [`SessionEvent::MergeEntryMoved`] carries a
    /// state and an evidence sentence and not a whole entry, so a head folding a move keeps the
    /// rows from the snapshot it last read; the rows are written by the move that ENDS a
    /// landing, which is exactly the move a head re-reads the queue after.
    #[serde(default)]
    pub gate_steps: Vec<MergeGateStep>,
}

/// **One gate step, on the wire** — a copy of `letibot_tokencore::store::MergeGateStep`, for the
/// reason [`MergeEntry`] is a copy of its own row: the head draws it and must not be able to
/// write one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeGateStep {
    /// The command as `main`'s `AGENTS.md` spells it. Empty for exactly one row — the
    /// [`MergeGateOutcome::NoGate`] one, where there was no command to run.
    pub command: String,
    /// What became of it, out of [`MergeGateOutcome`]'s closed set.
    pub outcome: MergeGateOutcome,
    /// The tail of what the step wrote, with the bytes dropped from the front counted.
    #[serde(default)]
    pub output: String,
    /// When the step started, Unix ms. `0` on a step that never ran.
    #[serde(default)]
    pub started_ms: u64,
    /// How long it took, ms. `0` on a step that never ran.
    #[serde(default)]
    pub elapsed_ms: u64,
}

/// **What became of one gate step**, on the wire — a copy of
/// `letibot_tokencore::store::MergeGateOutcome`, for the reason [`MergeState`] is a copy.
///
/// `passed`, `failed`, `not_run` and `no_gate` are four different facts and a head must not
/// collapse them: a step that never ran is not a step that was green, and a repository with no
/// gate is not a gate that passed everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeGateOutcome {
    /// It ran, and it was green.
    Passed,
    /// It ran, and it was red; `output` is the tail that says why.
    Failed,
    /// An earlier step was red, so the gate stopped before this one.
    NotRun,
    /// The repository's `main` declares no gate at all.
    NoGate,
}

/// **The reviewer's verdict on one entry, on the wire** — a copy of
/// `letibot_tokencore::store::ReviewRecord`, for the reason [`MergeEntry`] is a copy of its own
/// row: the head draws it and must not be able to write one.
///
/// **It travels beside the entries rather than inside one**, and the two are the two questions
/// a reader asks: *what is the queue* (the entries) and *what did the reviewer say about it*
/// (this). An entry can have no review at all — nobody has asked — which is a different fact
/// from a review with no verdict yet, and `decision: None` is what tells them apart.
///
/// The pane's Enter opens this: a verdict without its reasons and the evidence it was based on
/// is an opinion, so `reasons`, `files` and `commands` travel with the word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeReview {
    /// The entry this verdict is about.
    pub entry_id: String,
    /// The session that reviewed it — the one a person attaches to when they want to read the
    /// argument rather than the verdict.
    pub session_id: String,
    /// The branch judged.
    pub branch: String,
    /// The base SHA judged.
    pub base_sha: String,
    /// When the reviewer was asked, Unix ms.
    pub asked_ms: u64,
    /// When the verdict came back, Unix ms. `None` while the review is outstanding.
    #[serde(default)]
    pub answered_ms: Option<u64>,
    /// `accept`, `reject` or `needs_human`, or `None` for a review that has not answered.
    #[serde(default)]
    pub decision: Option<String>,
    /// **The last attempt's failure, verbatim, or empty** — an added, defaulted field, so no
    /// `PROTOCOL_VERSION` bump: an older head reads past it and draws what it drew before.
    ///
    /// **It is not a `decision` and it must not be read as one.** A reviewer whose turn failed
    /// reached no judgement, so `decision` is `None`; without this field the head had only
    /// `decision: None` to go on and drew *the reviewer has been asked and has not answered*
    /// over an attempt that had already died — the failure was invisible on the pane unless
    /// somebody opened the store. The entry's own `evidence` carries the same sentence (that is
    /// where the row a person scans reads it); this is the field that lets the head say *no
    /// verdict* rather than *still waiting*, which is the difference the restart is a decision
    /// about.
    #[serde(default)]
    pub failure: String,
    /// The reviewer's reasons, in its own words.
    #[serde(default)]
    pub reasons: Vec<String>,
    /// The files the reviewer read.
    #[serde(default)]
    pub files: Vec<String>,
    /// The commands the reviewer ran.
    #[serde(default)]
    pub commands: Vec<String>,
}

/// Why generation stopped.
///
/// Shaped to match `letibot_turn::FinishReason` exactly, `Other` and its string
/// included. The engine's own note is the reason: *"a `finish_reason` nobody
/// recognises is exactly the thing that must not be silently normalised"* — and
/// normalising it on the way to a head is the same defect, one hop later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Eos,
    Word,
    Length,
    Aborted,
    Other(String),
}

impl FinishReason {
    pub fn as_str(&self) -> &str {
        match self {
            FinishReason::Eos => "eos",
            FinishReason::Word => "word",
            FinishReason::Length => "length",
            FinishReason::Aborted => "aborted",
            FinishReason::Other(s) => s,
        }
    }
}

/// §4.5's `usage`.
///
/// `cached_tokens` is prompt tokens **reused**, not slot occupancy — the
/// distinction `turn_metrics` paid for (`timings.cache_n`, not `tokens_cached`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub predicted_tokens: u64,
    /// **What this turn cost, in micro-USD.** `None` on the local server, which
    /// is free, and on a metered model nothing prices — unpriced and free are
    /// different and only one of them is a number.
    ///
    /// The daemon computed this all along and kept it: `micros_usd` was read in
    /// exactly one place, the one-shot `--prompt` printer in `harnessd.rs`, so a
    /// session driven from a head never saw it. The operator, on a conversation
    /// answered by deepseek: *"still no money"* — correctly, because the meter
    /// existed only for a surface they were not using.
    ///
    /// `#[serde(default)]` so an older daemon's frame still reads: absent is
    /// `None`, which renders as nothing rather than as zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros_usd: Option<u64>,
}

impl Usage {
    /// **`f_sim`** — the fraction of *this* prompt that did not have to be
    /// prefilled. Deliberately **not** called `f_keep`.
    ///
    /// `usage` carries this turn's three numbers and nothing about the previous
    /// turn, so `f_keep` — `cached(N+1) / (prompt(N) + committed_generated(N))`,
    /// D11's settled C4 — is **not computable from this struct at all**. Naming
    /// this one `f_keep` is exactly the confusion T22 records: a head would then
    /// display a number that falls whenever the conversation grows and call it the
    /// cache metric with the 0.99 bar on it. `letibot_turn::TurnMetrics` has both,
    /// because it has the witness this struct does not.
    ///
    /// `None` for an empty prompt, for the same reason `TurnMetrics::f_sim` is:
    /// neither 0.0 nor 1.0 is true and both get averaged into a session figure.
    pub fn f_sim(&self) -> Option<f64> {
        if self.prompt_tokens == 0 {
            None
        } else {
            Some(self.cached_tokens as f64 / self.prompt_tokens as f64)
        }
    }
}

/// §4.5's `timings`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Timings {
    pub prompt_ms: f64,
    pub predicted_ms: f64,
    pub wall_ms: u64,
}

/// ACP's vocabulary, as §13.4 requires: the adapter is then a mapping and not a
/// translation. `PermissionOption{option_id, label, kind}` with
/// `PermissionOptionKind in AllowOnce | AllowAlways | RejectOnce | RejectAlways`.
/// The model's verdict on a permission, carried to the person answering it.
///
/// The wire twin of `letibot_tools::adjudicate::ModelAdvice`. Separate because this
/// crate is the protocol and must not depend on the tool crate — the same split
/// `DecisionOption` and `OptionKind` already have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAdvice {
    /// **Whether an oracle was actually consulted** (R11).
    ///
    /// No `PROTOCOL_VERSION` bump: an added, defaulted field on an existing variant.
    /// An older head ignores it and renders exactly what it rendered before.
    ///
    /// `false` covers every case where the model adjudicator produced a verdict
    /// *without asking a model*: an unresolved action, an always-ask entry, a blocked
    /// one, an uncollected trail, an intent outside the oracle's earned scope. Those
    /// are layer A's answers arriving through layer B's door, and they are not
    /// verdicts — so `would: "unavailable"` with `consulted: false` is *nobody spoke*,
    /// which a head must not render as *a model said unavailable*.
    ///
    /// **Load-bearing for the corpus, which is why it is on the wire at all.**
    /// `letibot_tools`' own twin carries it and warned that without it a row whose
    /// "verdict" was `always-ask short-circuit` would be labelled as the operator
    /// agreeing or disagreeing with an opinion nothing held — manufactured signal, and
    /// the worst kind, because it looks like data. The wire copy had nowhere to put it,
    /// so a head could only guess from the prose. Defaulted, because a daemon from
    /// before this field sent none and `false` is the honest reading of silence.
    #[serde(default)]
    pub consulted: bool,
    /// `admit`, `refuse`, `ask` or `unavailable` — what the model's answer would
    /// have done on its own.
    pub would: String,
    /// Which oracle, in its own words.
    pub by: String,
    /// Why, in one or two sentences. Shown to the person; never parsed.
    pub basis: String,
    /// Which of the operator's own utterances it relied on. **Empty is loud**: an
    /// authorisation that cites nothing is one the oracle could not ground, and a
    /// head should render the emptiness rather than the absence of a list.
    #[serde(default)]
    pub cites: Vec<String>,
    /// **Whether the answer was a NON-answer, and which kind** (R12) — leticl's ask, and it
    /// closes a five-facts-one-line defect rather than adding a field for its own sake.
    ///
    /// Without it, `consulted: true` + `would: "ask"` + `cites: []` is what **five** distinct
    /// facts arrive as, and leticl measured that against three real frames on a scratch head:
    ///
    /// ```text
    ///     the model answered UNSURE                     UnsureKind::CouldNotDecide
    ///     two scores landed between the thresholds      UnsureKind::BetweenThresholds
    ///     the bytes are not a verdict at all            UnsureKind::Unreadable
    ///     the generation stopped at its ceiling         UnsureKind::OutOfRoom
    ///     the model found nothing that authorises it    OracleAnswer::NotAuthorised
    /// ```
    ///
    /// The last is a real ANSWER — the oracle looked and said no — and the four above are
    /// four different ways of not answering. A head that renders all five as "the guard
    /// asks" is rendering one line for facts whose remedies differ: raise the ceiling, read
    /// the bytes, re-ask, or accept the refusal.
    ///
    /// **The token, not the prose.** `basis` carries a sentence for each of the five and both
    /// trees' own docs forbid parsing it (*re-parsing prose to recover a label is how a corpus
    /// rots*). This is `UnsureKind::as_str()` — the same string the corpus column
    /// `oracle_reading` already stores, so one vocabulary has two destinations rather than two
    /// vocabularies that drift.
    ///
    /// **`None` with `would: "ask"` is the fifth fact**: a consulted oracle that answered, and
    /// answered *no*. `None` with `would: "unavailable"` is nobody spoke. `#[serde(default)]`
    /// and no version bump — an older head ignores it and renders what it rendered before, and
    /// a daemon from before the field sends none, which reads as `None`.
    #[serde(default)]
    pub unsure: Option<String>,
    /// How long it took, in milliseconds. A verdict that spent its whole budget is
    /// a different fact from one that came back in 40 ms.
    pub latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionOption {
    pub option_id: String,
    pub label: String,
    pub kind: OptionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionKind {
    AllowOnce,
    /// This class, for the rest of **this session** (`PROTOCOL_VERSION` 7).
    ///
    /// The widest an *answer* goes. Anything standing beyond one session is a
    /// **mode**, not a grant: the operator moves this project to a named point and
    /// the disclosure says which one. A grant table keyed by tool and class is a
    /// thing nobody audits; a per-project point is one value a person can hold in
    /// their head.
    AllowSession,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// What happens when nobody answers. Ours, not ACP's — ACP has no timeout at the
/// protocol layer at all (§13.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnTimeout {
    Deny,
    Allow,
    Ask,
}

/// How a decision settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DecisionOutcome {
    Selected { option_id: String },
    Cancelled,
    TimedOut,
}

/// Who settled it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decider {
    /// `human`, `policy`, `boundary`, `model`, `timeout` — §11's three adjudicators
    /// plus the two that are not adjudicators.
    pub kind: String,
    /// The identity, where there is one. A head id, a policy rule name, "".
    pub identity: String,
}

/// **How many bytes of a tool call's arguments may travel to a head — a wire-safety
/// limit, not a display decision (R25).**
///
/// `crates/ui/DESIGN.md` §4.1 asked for *"a short, tool-supplied, already-truncated display
/// string … capped at something like 120 bytes"*, and 120 is what this was. **That number was
/// a viewport guess made in a layer that cannot see a viewport**, and the cost is exactly what
/// the operator measured on 2026-09-22: a 227-column pane showing a headline cut at 121
/// characters, so **~100 columns went unused on every tool row** — *"some commands head lines
/// like 'Ran blabla' truncate too early, they dont use the whole conversation history viewport,
/// unlike say thinking."* Thinking rows carry no such cap, which is why they look right beside
/// it. The cap is **here**, where `TurnEvent` becomes `SessionEvent`, because here is where the
/// fan-out starts: below this line an argument is one in-process string, above it it is a copy
/// per attached head.
///
/// **Why the number moved and the *unit* did not.** §3.3 ruled *bytes in a daemon, columns in
/// a head, move neither*, which is right about the unit and was wrong about the number, and
/// its own text named this cost: moving it means the daemon sends the field untrimmed and each
/// head cuts it, "a protocol decision rather than a rounding one". The argument that settles
/// it is not aesthetics: **more than one head may be attached to one session, at different
/// widths, at the same time — so any single number this layer picks is wrong for all but one
/// of them.** The only layer that knows a viewport is the one that owns it.
///
/// **What it is sized against.** A viewport, with headroom: the widest terminal this box has
/// attached is 227 columns; an 8K display at a small monospace font is ~960. 2048 covers the
/// latter with 2× to spare, so **no plausible head is starved**, and it stays a bound — an
/// untrimmed command line can be arbitrarily long, and a field that fans out per head per live
/// call has to have one. Both ends are still bounded; only the layer that does the display
/// cut has changed.
///
/// **The elision is still disclosed, and still where it happens.** [`truncate_target`] keeps
/// appending `…` for the wire cut, and the head keeps appending its own when it cuts to its own
/// width (`rano::width::text::truncate`, which has done exactly that all along). Two cuts,
/// two marks, each at the layer that made it.
///
/// **A stored corpus row is unaffected, and that is worth stating rather than assuming.**
/// There is no `target` column: `display_target` is derived at lift time from
/// `arguments_json`, which the store holds **whole**. So raising this number changes what a
/// *future* session sends and therefore what a future row's head shows; it cannot make a
/// replay richer than the session it replays, because the replay derives the same field from
/// the same stored bytes under the same rule. The corpus is measured across time and this is
/// the half of it that does not move.
pub const TARGET_MAX_BYTES: usize = 2048;

/// The keys that name a call's subject, in preference order.
///
/// Key names, not tools: the rule below stays derived, and a tool whose
/// subject has another name keeps the written order.
const SUBJECT_KEYS: [&str; 3] = ["path", "file_path", "file"];

/// The keys that carry a file's new **body**, which never belong in a label beside a named file.
///
/// The operator, on a write row titled `Wrote "Mechanism confirmed — thank you, this one was
/// worth checking, and it is now pinned by tests: …` with the file name pushed off the end:
/// *"i already see what was written from the diff. we dont need the line in quotes - just put
/// the file name here please"*. A body under the cap never tripped the budget, so the prepend
/// below did not fire and the content led; the head then shortened the row from the end, which
/// is where the path was. Key names again, not tools — and only dropped when a subject key is
/// present, so a call with nothing else to name still says something.
const BODY_KEYS: [&str; 3] = ["content", "old_string", "new_string"];

/// The display target for a tool call: the one argument a person reads.
///
/// # Why it is derived and not tool-supplied
///
/// §4.1 proposes a *tool-supplied* string, on the argument that the tool knows
/// which of its arguments a person reads. It does — and `ToolCallProposed` is
/// emitted from the parsed call, before anything has been dispatched to a
/// registry, so a tool-supplied string would have to arrive on a later event, and
/// having it on the *first* one is the whole point.
///
/// The rule used instead has no per-tool table in it: **the scalar argument
/// values, in the order the model wrote them**, which `preserve_order` keeps. That
/// order is not arbitrary — a schema puts the subject first, and every tool in this
/// tree does (`read{path}`, `grep{pattern, path}`, `list{path}`). So
/// `{"pattern":"home.*button","path":"src"}` reads `home.*button src`, which is
/// what a person scans for; a tool the rule reads badly gets a slightly wrong
/// *label*, never a wrong fact.
///
/// **Except when the written order hides the subject altogether.** A model
/// composing an edit writes the content before the file it is going into, and the
/// budget then breaks before `path` lands — the operator's card read
/// `Edited "        let view = sidediff::edit_view…` and could not answer *which
/// file*. So a subject key ([`SUBJECT_KEYS`]) the loop never reached is prepended, and a
/// body ([`BODY_KEYS`]) beside a named file is not in the label at all:
/// the file leads, alone. An order that already shows every
/// argument — grep's pattern, then its path — does not move.
///
/// Nested values are **elided, not flattened**: `{…}` and `[…]` say there is more
/// without pretending a JSON dump is a label.
///
/// # Where that rule was drawn narrower, and why (R15)
///
/// **A nested value is elided only when the map has no scalar to show at all.** An
/// edit's `edits` array was landing in front of the filename —
/// `▸ Edited […] letibot/crates/harnessd/src/answers.rs · ok · 3ms · 10 lines` — so the
/// most valuable position on the row was occupied by a placeholder whose "more" is the
/// **diff sitting directly underneath it**. The operator's ruling: *"`[…]` should be
/// gone for Edit"*, not moved.
///
/// The line is drawn on the *arguments* and not on the tool name, because the whole rule
/// above is derived — a per-tool table is what this function exists to avoid. The general
/// point survives intact: `[…]` is still what a map of nothing but nested values says,
/// which is the case where a label would otherwise be empty. **No seated tool in this
/// tree has that shape today** — every one carries a scalar `path`, `pattern` or `cmd` —
/// so that arm is reachable by a tool added later, and a card with no label at all falls
/// back to the call id rather than to a blank (`subject` in the head's `targets`).
///
/// **What was deliberately not changed:** the `SUBJECT_KEYS` prepend below. It answers the
/// other failure — the budget breaking *before* `path`, leaving a card that says nothing
/// about which file — and reordering it was proposed and rejected in favour of removal:
/// on an edit nothing else belongs first, so the row should not need a rule to put the
/// file there.
///
/// # It is public because a head needs it twice
///
/// Once on the wire, for a call that is *running*. And once locally, for a settled
/// `TranscriptItem::Assistant { tool_calls }` whose proposal this head never saw —
/// a head that attached after the turn, or one that switched into the session.
/// Without a shared function that second case is either a second implementation of
/// this rule in the head (two spellings of one display, drifting) or a tool list
/// that says `Read (call_0)` for every row older than the attach.
pub fn display_target(arguments: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments) else {
        // Not JSON at all. The model wrote it, so it is still the most
        // informative thing available; it is trimmed and shown as it is.
        return truncate_target(arguments.trim());
    };
    let serde_json::Value::Object(map) = v else {
        return truncate_target(&scalar(&v).unwrap_or_default());
    };
    let mut parts: Vec<String> = Vec::new();
    // The placeholders of the nested values the walk passed, kept apart so that they can
    // be dropped if any scalar turned up (R15). In written order, so the fallback below
    // reads like the arguments the model wrote.
    let mut elided: Vec<&'static str> = Vec::new();
    let mut subject_seen = false;
    let names_a_file = SUBJECT_KEYS.iter().any(|k| map.contains_key(*k));
    for (_k, val) in &map {
        if names_a_file && BODY_KEYS.contains(&_k.as_str()) {
            continue;
        }
        if SUBJECT_KEYS.contains(&_k.as_str()) {
            subject_seen = true;
        }
        match scalar(val) {
            Some(s) => parts.push(s),
            None if val.is_array() => elided.push("[…]"),
            None => elided.push("{…}"),
        }
        if parts.iter().map(|p| p.len() + 1).sum::<usize>() > TARGET_MAX_BYTES {
            break;
        }
    }
    if parts.is_empty() {
        parts = elided.into_iter().map(str::to_string).collect();
    }
    if !subject_seen && let Some(val) = SUBJECT_KEYS.iter().find_map(|k| map.get(*k)) {
        parts.insert(
            0,
            match scalar(val) {
                Some(s) => s,
                None if val.is_array() => "[…]".into(),
                None => "{…}".into(),
            },
        );
    }
    truncate_target(parts.join(" ").trim())
}

/// One scalar argument as a label. A string containing whitespace is quoted, so
/// `grep "two words" src` cannot be misread as three arguments.
fn scalar(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(if s.chars().any(char::is_whitespace) {
            format!("{s:?}")
        } else {
            s.clone()
        }),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Null => Some("null".into()),
        _ => None,
    }
}

/// Cut to [`TARGET_MAX_BYTES`] on a character boundary, and say that it was cut.
///
/// **What a display target is, and why it is guarded here.** It is composed from the
/// model's own arguments — a path, a command line, a pattern — so it is content this
/// daemon did **not** write, and it ends up on a head's row. A display target that carried
/// a live escape would put an instruction to the operator's terminal on a card that names
/// a tool call (§3.1).
///
/// **And it must go as a whole sequence.** This mapped every control character to a space,
/// which for the `ESC` of an escape is the worst of both: the introducer became a space and
/// the `[31m` stayed, so a row read ` [31m` — five columns of visible garbage where the
/// terminal measured none, which also breaks the width this function is truncating
/// against. The sequence parser is `letibot_transcript::sanitize`'s, shared with
/// `letibot-ui` rather than copied: this crate cannot see that one and must not start.
fn truncate_target(s: &str) -> String {
    let s = letibot_transcript::sanitize::without_control(s);
    if s.len() <= TARGET_MAX_BYTES {
        return s;
    }
    // The ellipsis counts. `…` is three bytes, and a cap that forgets that is a cap
    // the output is allowed to exceed — which is exactly the class of off-by-a-few
    // that puts a status line one column past the terminal and scrolls the frame.
    const ELLIPSIS: &str = "…";
    let mut end = TARGET_MAX_BYTES - ELLIPSIS.len();
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ELLIPSIS}", &s[..end])
}

/// **One section of a compaction's record, as the daemon read it back.**
///
/// `name` is one of the template's headings (`letibot_turn::SUMMARY_SECTIONS`), spelled by
/// the daemon and not by the head (see [`COMPACTION_SECTIONS_KEY`]).
///
/// **Present with an empty body and absent are two different facts**, and the whole
/// reason the sections are a list rather than one string: an empty body is the model
/// saying *there is nothing here*, and a name missing from [`CompactionReport::sections`]
/// is the model having written no such heading at all — *nobody said*. Collapsing them
/// is how *nothing is blocked* becomes indistinguishable from *nobody asked*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionSection {
    pub name: String,
    /// The text between this heading and the next, trimmed. Empty when the model
    /// wrote the heading and nothing under it.
    pub body: String,
}

/// One exchange carried through a compaction verbatim rather than summarised.
///
/// `role` is `operator` or `agent`, the vocabulary [`Speaker`] already uses — a head
/// that draws it has a word for each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionTurn {
    pub role: String,
    pub text: String,
}

/// **What was kept verbatim, and why not more** — R27's ruled tail.
///
/// Present on every compaction, including a local one, where it is an object with
/// zero turns. That is deliberate and it is the requirement: *the local artefact is
/// the remote one with an empty tail*, so a session compacted locally and resumed
/// against a remote model (or the reverse) does not meet a record its reader cannot
/// read. One shape, one reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionTail {
    pub turns: Vec<CompactionTurn>,
    /// **How many items were carried.** Items, not turns: the tail is made of
    /// transcript items, and a turn is however many of them happened between two
    /// operator messages. `turns.len()` is the count of turns and is a different
    /// number on purpose.
    pub carried: u64,
    /// **Why this much and no more**, one of `"local_model"` (the ruled split: a local
    /// model is bounded by the KV cache in VRAM, where a tail competes with the
    /// pressure the compaction was called to relieve), `"nothing_fits"` (one item was
    /// larger than the whole tail budget), or `"no_turns"` (there was nothing to
    /// carry).
    ///
    /// **A string rather than an enum, and an unknown value is printed raw.** A head
    /// that met an unrecognised reason would otherwise have to choose between dropping
    /// the fact and inventing one, and R27's whole point is that a head does not hold
    /// the daemon's vocabulary — the same argument as `head-run.tools`.
    pub because: String,
    /// **Items of the newest exchange left out of the front of the tail**, when the tail
    /// could not start at an exchange boundary — one big file read is larger than the
    /// whole budget, so the tail begins inside the exchange still in progress. Non-zero
    /// means its first turn answers something that is no longer here, which the reader
    /// has to be told.
    #[serde(default)]
    pub dropped: u64,
}

impl CompactionTail {
    /// **WHY this compaction's tail is what it is, in one sentence** — or `None` when
    /// there is nothing to explain.
    ///
    /// The operator, after an automatic compaction of leticl's wrote no tail at all:
    /// *"the head should be able to say which treatment a compaction got. A reader who cannot
    /// tell whether the tail was omitted by policy or by accident is in the position R41's job
    /// pane was in: an absence with two causes and one appearance."*
    ///
    /// **Zero has three causes and they were one appearance.** `local_model` is R27's ruling
    /// working; `nothing_fits` is the budget losing to a single item; `no_turns` is an empty
    /// history. Nothing on any screen or in any row distinguished them, and this is the one
    /// place both readers meet: `harnessd` writes it into the transcript's own note (which is
    /// durable) and into the `compacted` sentence a head draws (which is not), so the two
    /// cannot come to say different things about one compaction.
    ///
    /// **`budget` is silent on purpose.** The tail exists, the count beside it says how much,
    /// and a sentence explaining that what fitted was what fitted is the furniture this file
    /// keeps deleting.
    ///
    /// **An unknown value is printed raw**, per this struct's own rule: a head does not hold
    /// the daemon's vocabulary, and dropping the fact or inventing one are the two things it
    /// must not do.
    pub fn why_line(&self) -> Option<String> {
        match self.because.as_str() {
            // R27's ruled split, and the reason has to be in the sentence: a reader who does
            // not know the rule reads this as the head having lost their history.
            "local_model" => Some(
                "No verbatim tail: this conversation's turns go to the LOCAL model, and R27's \
                 split gives the tail to a remote one only — a local model is bounded by the \
                 KV cache, where a tail competes with the pressure that called this compaction."
                    .into(),
            ),
            "nothing_fits" => Some(format!(
                "No verbatim tail: the newest exchange by itself is larger than the whole tail \
                 budget, and this engine never cuts an item in half. {carried} item(s) were in \
                 the history it was chosen from.",
                carried = self.carried
            )),
            "no_turns" => {
                Some("No verbatim tail: there was nothing to carry — the history was empty.".into())
            }
            // A reason this build does not know, shown rather than swallowed.
            other if !other.is_empty() => Some(format!(
                "No verbatim tail, and the daemon gave a reason this build does not know: \
                 `{other}`."
            )),
            _ => None,
        }
    }
}

/// **A compaction's account of itself, beside its sentence rather than inside it.**
///
/// R27. The `Warning` that already announces a compaction carries a `detail` written for
/// a person; a head that wants to draw a *row* — the section list, the numbers, the
/// verbatim tail — otherwise has to parse English, which is the same defect R24's frame 2
/// had one layer down: a prose description of a shape is not the shape.
///
/// **Inline on the warning, not an R11-style locator.** leticl's reason, and it is the
/// right one: two events can drift about which compaction they describe, and a locator
/// would have to be reachable from the snapshot too or a head attaching after a
/// compaction draws a row it can never fill. Nothing here is large — a record is a few
/// thousand tokens and this is a fraction of it, published once per compaction rather
/// than once per turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionReport {
    /// **Which kind of compaction this was**, in the vocabulary the warning code already
    /// uses: `"compacted"`, `"reseated"`, `"overrun"`. Carried here as well as in
    /// `Warning::code` so a head holding only the report can still name it.
    pub kind: String,
    /// The conversation's size before, in LEDGER tokens — the daemon's own count, the
    /// same number the detail sentence was rendered from.
    pub tokens_before: u64,
    /// And after: the new base, which is the prefix plus the record plus any tail.
    pub tokens_after: u64,
    /// The transcript the fork opened, e.g. `s-123#t2`.
    pub transcript: String,
    /// `tokens_before` plus the prompt, as the daemon measured it when it decided — the
    /// number that was compared against `window`.
    pub resident: u64,
    /// The window it was compared against.
    pub window: u64,
    /// The reserve that comparison subtracted, so a head can reproduce the decision
    /// rather than only read its outcome.
    pub headroom: u64,
    /// **The record ran out of room before it finished.** A bool on the structure and not
    /// a phrase parsed out of a body, because a templated record is exactly where
    /// *truncated* stops being readable out of any one section: the cut falls wherever the
    /// model ran out, which with eight sections can be inside `Relevant Files`.
    pub cut_off: bool,
    /// **Which wording of the instruction produced this record**, the way a corpus row
    /// carries `brief_sha`. Bump it when `letibot_turn::SUMMARY_INSTRUCTION` changes what it
    /// asks for: records produced under different templates are two datasets, and a
    /// head that saw the tag move knows the sections may differ.
    pub template: String,
    /// The headings the daemon found, in `letibot_turn::SUMMARY_SECTIONS`'s order and spelling. A heading it did not find is absent — see [`CompactionSection`].
    pub sections: Vec<CompactionSection>,
    pub tail: CompactionTail,
}

/// **The template tag a `CompactionReport::template` carries.**
///
/// A date, like `BRIEF_FORMAT`, and bumped by hand when `letibot_turn::SUMMARY_INSTRUCTION`
/// changes what it asks for — the same rule for the same reason, one layer over.
pub const COMPACTION_TEMPLATE: &str = "compaction/2026-09-23";

/// **The sections a compaction's record has, from the daemon that asks for them.**
///
/// A `SettingRow` key on the existing `ServerFrame::Settings`, exactly as
/// [`crate::protocol::HEAD_RUN_TOOLS_KEY`] is, and for the reason R24 decided that one:
/// **a head that held its own copy of a list would drift.** This key's `value` is
/// comma-joined with no spaces, and an absent row means a daemon older than this one —
/// which a head reads as *no structure to draw*, so the sentence in `detail` is all there
/// is and the head draws exactly what it drew before.
pub const COMPACTION_SECTIONS_KEY: &str = "compaction.sections";

/// **A subagent's call, named, on a card its own session cannot draw.**
///
/// The operator, on who a subagent's ask belongs to: *"who asks subagents
/// permissions? i think they should surface to the parent head all the way to the
/// root obviously"*. A child has no head attached and cannot be driven, so its
/// gate's card is posted to the session that does have one — the tree's root — and
/// this is the fact that says so on the card.
///
/// **A card that arrived at the root unlabelled would be answered for the wrong
/// thing**: a head draws `? \`write\` wants write access to \`src/main.rs\`` with the
/// options it always draws, and an operator with no way to tell whether that is the
/// session in front of them or the child it spawned is approving on the wrong
/// question.
///
/// The fields are exactly what the operator has to tell two children apart: the
/// **handle** (which is the child's session id — what `task_result` collects by and
/// what a head attaches to), the **first line of its task**, and the **root session
/// the card belongs to**, carried so a refusal can name where it went rather than
/// say "somewhere".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentAsk {
    /// The child's handle, which is its session id.
    pub handle: String,
    /// The first line of the child's task, so two children are told apart.
    pub task: String,
    /// The session this card is posted to: the tree's root, the only session in a
    /// subagent tree that has a head.
    pub root: String,
}

/// **Which of the two facts a prompt card is raised on** — the reading, or the absence of
/// one. The card's raise reason is not "the run is asking" but **"a run of the operator's
/// own is in flight"** (the operator, 2026-10-09: *"sudo can get input from here so can
/// others"*), so the card has to say which of the two it knows, and this is that word.
///
/// The distinction is the one `letibot_tools::exec::ask::Waiting` keeps between `Yes` and
/// `Unreadable`, carried one layer up: [`PromptReading::Blocked`] is *the daemon read the
/// process and found it blocked on the input this daemon holds*, and
/// [`PromptReading::Unreadable`] is *the daemon could not look at the run at all* — the
/// `sudo` case, where a card that claimed a reading would be a guess. A head that draws one
/// card for both would draw a card that claims the program asked; the whole point of the
/// field is that it never has to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptReading {
    /// The signal was read: a process of the run is blocked reading the input this daemon
    /// holds. The card may say the command is asking, and the request's `question` is the
    /// program's own last line, to show.
    #[default]
    Blocked,
    /// **The signal could not be read** — a process of the run belongs to another uid, or a
    /// `/proc` this daemon may not open. The card may NOT say the command is asking: its
    /// text is the honest sentence (*this daemon could not look at the run, so it cannot say
    /// whether it is asking; a line you send goes into it either way*), and `question` is
    /// `None` because there is no reading to quote from.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    TurnStarted {
        turn_id: String,
        model: String,
        ledger_head: String,
        /// **The WHOLE turn's start, Unix ms, or `None` when nobody measured it.**
        ///
        /// A `TurnStarted` is emitted once per ROUND — the engine's `run_turn_steered` is called
        /// inside the daemon's round loop — so a head that times from the event it receives
        /// restarts its clock at every round. The operator, watching the composer: *"it should be
        /// still responding even while you do tools calls and such, and not reset, currently it
        /// resets."*
        ///
        /// A turn is one prompt however many rounds it takes, so the start belongs to the prompt
        /// and the emitter stamps it here. `None` is a real answer (a snapshot turn, a test) and
        /// the head draws it as *started before this head attached* rather than inventing one.
        began_ms: Option<u64>,
    },
    /// Nothing surveyed reads this one, and §8.5 requires it to count as liveness.
    /// It is also the archetype of an *interactive* frame: see [`crate::scrub`].
    PromptProgress {
        turn_id: String,
        #[serde(flatten)]
        progress: PromptProgress,
    },
    /// The server's own generation counter, one per frame that advanced it.
    ///
    /// The generation half of liveness, paired with [`Self::PromptProgress`]: a
    /// head tells a hang from a model that is still emitting by whether this
    /// moves, which is exactly the case a long tool-call write is, where no
    /// visible text moves at all. Its durable residue is `TurnFinished`'s
    /// `usage.predicted_tokens`, so it is interactive — see [`crate::scrub`].
    TokensGenerated { turn_id: String, tokens: u64 },
    Delta {
        turn_id: String,
        target: DeltaTarget,
        text: String,
    },
    ToolCallProposed {
        turn_id: String,
        call_id: String,
        name: String,
        args_digest: String,
        /// The one argument a person reads: a path, a pattern, a command line.
        ///
        /// `crates/ui/DESIGN.md` §4.1, fixed. Before this field a head watching a
        /// call could render `Running bash` and never `Running cargo test
        /// --workspace`, because the arguments reached it only with the transcript
        /// — after the call had finished.
        ///
        /// `#[serde(default)]` so a log recorded before the field existed still
        /// replays; an old row then renders the verb with no target, which is what
        /// it always did.
        #[serde(default)]
        target: String,
    },
    DecisionRequested {
        req_id: String,
        /// **Another session's call, when this card is not this session's own.**
        ///
        /// `Some` for exactly one case: a subagent's gate reached an `ask`, and the
        /// card was posted to the tree's ROOT, because a child has no head of its
        /// own (R58 — the one session in a subagent tree that is in `Sessions::open`
        /// and therefore the only one a head can be attached to). See [`SubagentAsk`].
        ///
        /// `None` is *this session's own call*, which is every card before this
        /// existed — so an older head, which ignores the field, draws exactly what it
        /// drew before and answers the same one question. No `PROTOCOL_VERSION` bump:
        /// the precedent [`ModelAdvice::consulted`] and `write_targets` set, and the
        /// bumps in this file are for new frames and new variants, which an old peer
        /// cannot parse at all.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentAsk>,
        /// `permission` or `question`. §11.6: *"a permission and a question are one
        /// mechanism, differing in `kind`"* — and the two payload fields below are
        /// that difference made concrete rather than left to a head to infer.
        kind: String,
        call_id: Option<String>,
        /// **The tool's declared access** — `read | write | exec | network`, from the
        /// tool's schema. Empty from a daemon older than this field.
        ///
        /// **Added because the card printed two statements that looked like a
        /// contradiction and nothing joined them** (§11.7). The *headline* is what the
        /// tool **declares** — `wants exec access` — and [`Self::detail`] is layer A's
        /// reading of the **action** — `auto — a read inside the boundary` — so *"the
        /// classifier decided this needed no asking"* read as being argued with by the
        /// card going up anyway. The sentence that joins them is the head's (A's
        /// wording); the fact it needs is this one, which the request has carried as
        /// `ActionClass::access` all along and simply never put on the wire.
        ///
        /// **No `PROTOCOL_VERSION` bump**: an added, defaulted field on an existing
        /// variant, the precedent `ModelAdvice::consulted` set. An older head ignores
        /// it and renders exactly what it rendered before, and empty says *nobody told
        /// me* rather than *read*, which is the difference between a card that draws no
        /// clause and a card that draws the wrong one.
        #[serde(default)]
        access: String,
        summary: String,
        /// **The one thing being decided about**, on its own: the command a `bash`
        /// call would run, the path a write would take, the URL a fetch would reach.
        ///
        /// Separate from `summary` because a head has to be able to put it where the
        /// eye lands. It used to be interpolated into the sentence and the sentence
        /// was then joined to layer A's reading, so the line an operator decided
        /// from read *"`bash` wants exec access to `<no target argument>` — ask —
        /// intents [read_file write_file execute_code] over [host_other]"* — a
        /// taxonomy wrapped around a blank where the command should have been.
        ///
        /// `#[serde(default)]` for logs recorded before this existed; empty means
        /// the call named nothing, which is a fact about `web_search` and a bug
        /// about `bash`.
        #[serde(default)]
        target: String,
        /// **What this action would WRITE, by path** — R35, and the field that was computed,
        /// renderable on the other head, and reaching nobody.
        ///
        /// The classifier has found these since the day it was written: `write_targets` resolves an
        /// assigned name, `open(p, 'w')`, a mode spelling, and a method whose receiver is the path —
        /// built from the operator's own card (*"`p = Path(\"src/syntax.rs\")` …
        /// `open(p,'w').write(s)`"*). What was missing is that the names stopped inside the
        /// classifier: a `bash` call running a script that rewrites a file drew a card that did not
        /// say which file, and the operator's report is exactly that — *"it cant catch those pesky
        /// python edits"*. It can. Nothing showed them.
        ///
        /// **Empty means no write was FOUND**, which is not *this writes nothing* — see
        /// [`WriteTarget`]. A card that said *writes nothing* on an empty list would be claiming a
        /// negative the scanner cannot support, which is R35's own subject one layer over.
        ///
        /// **Display-only, and it does NOT gate.** A detected write already inserts
        /// `Intent::WriteFile` and already resolves regions — that is the gate's decision and it is
        /// unchanged by this field existing. So an operator seeing more does not mean anything being
        /// admitted more, and that is deliberate rather than incidental: changing a TIER on a
        /// scanner's inference is a separate ruling with its own measurement.
        ///
        /// `#[serde(default)]` so a log recorded before it replays: an old row has no targets, and a
        /// head drawing none is what every head drew before this.
        #[serde(default)]
        write_targets: Vec<WriteTarget>,
        /// Layer A's deterministic reading — verdict, intents, regions, tier.
        ///
        /// Evidence, not the question. It belongs under the question in the dim
        /// register rather than appended to it: an operator reads this line SECOND,
        /// if at all, and it was pushing the command off the first one.
        #[serde(default)]
        detail: String,
        /// **The adjudication ladder**, for a permission: allow once, allow for the
        /// session, allow for the project, deny. Empty for a question, which does not
        /// have a ladder — it has choices.
        options: Vec<DecisionOption>,
        /// **The model's own options**, for a question (T25/D10). Plain text in the
        /// person's vocabulary, not a policy vocabulary.
        ///
        /// This exists because it had nowhere to sit. `options` carries
        /// [`OptionKind`], which answers *may this run*; a question answers *which way
        /// should I go*, and squeezing "rebuild first" into an `AllowOnce` would put a
        /// free-form sentence where a policy engine reads a grant. Two fields, one
        /// event, and `kind` says which is populated.
        ///
        /// The index into this list is what
        /// [`crate::question::QuestionAnswer::option`] names, so a head must not
        /// reorder it.
        ///
        /// `#[serde(default)]` so a log recorded before `PROTOCOL_VERSION` 7 replays:
        /// an old row has no choices and was a permission, which is what an empty list
        /// says.
        #[serde(default)]
        choices: Vec<String>,
        /// Why the model is stuck, in one line, for a question. Shown to the person
        /// and never used to derive an answer.
        ///
        /// Carried verbatim rather than folded into `summary`, because a head that had
        /// to split one string back into a question and its reason would be
        /// reconstructing what it was sent — the same defect
        /// [`crate::hub::Hub`]'s `shown` field exists to avoid one layer down.
        #[serde(default)]
        because: String,
        /// **What the model already said about this**, at `/mode supervised`.
        ///
        /// The payload of *"ask model and ask me if i agree or not"*: a head renders
        /// it above the options so the person is agreeing or disagreeing rather than
        /// deciding cold. Empty at every other point — at `automode` the model is the
        /// decider and there is nobody to advise, at `always-ask` no model was asked.
        ///
        /// Advice and never an answer. A head must not preselect an option from it,
        /// and must not shorten the deadline because it arrived: the whole value of
        /// the point is a judgement the person actually made.
        ///
        /// No `PROTOCOL_VERSION` bump. It is an added, defaulted field on an existing
        /// event — serde ignores it on an older head, which then renders exactly what
        /// it rendered before and answers exactly as well. The bumps in this file are
        /// for new frames and new variants, which an old peer cannot parse at all.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        advice: Option<ModelAdvice>,
        /// Unix millis. `None` means §11.5's "wait forever", which is a policy a
        /// human head may choose and an automated one may not.
        deadline: Option<u64>,
        on_timeout: OnTimeout,
    },
    DecisionAnswered {
        req_id: String,
        outcome: DecisionOutcome,
        by: Decider,
        basis: String,
        /// The answer arrived after the deadline had already settled it. Recorded
        /// rather than dropped: "an answer that arrived too late" and "no answer"
        /// are different facts.
        late: bool,
    },
    /// **Shape revised by W9**, which is what T13.4 said would settle it: `turn_id`
    /// because every other turn-scoped event carries one and a head cannot
    /// attribute a call without it once §8.4's subagents run concurrently, and
    /// `access` because §8.1 clause 4 declares it in the schema and it is the fact
    /// that explains why a call did or did not stop for a decision.
    ToolStarted {
        turn_id: String,
        call_id: String,
        name: String,
        /// `read | write | exec | network`, from the tool's schema.
        access: String,
    },
    /// Interactive. Partial tool output, which a late head must never be replayed —
    /// see [`crate::scrub`].
    ///
    /// `note` survived contact with W9 unchanged: `grep` and `glob` do not know a
    /// total until they have finished walking, so a `done/total` pair would be a
    /// denominator invented for the display.
    ToolProgress {
        turn_id: String,
        call_id: String,
        note: String,
    },
    /// **Shape revised by W9.** `bytes` alone could not answer the question a
    /// spilling runtime raises — how much the model got versus how much there was —
    /// and the locator has to reach a head or "there is more" is a dead end.
    ToolFinished {
        turn_id: String,
        call_id: String,
        outcome: letibot_transcript::ToolOutcome,
        /// A digest, for the same reason `ToolCallProposed` carries one: the payload
        /// is in the transcript, and an event fans out to every head.
        payload_digest: String,
        /// What the model received.
        inline_bytes: u64,
        /// What the tool produced. Larger than `inline_bytes` exactly when the
        /// output spilled (§8.3).
        full_bytes: u64,
        /// The spill locator, when there is one: a head can offer the rest.
        spill: Option<String>,
        /// How many of §8.1 clause 2's repairs the call needed. A head that cannot
        /// see this cannot see a model steadily emitting malformed calls.
        repairs: u32,
        /// Both sides of the file a file-editing call changed, bounded to the
        /// region that differs. The one payload an event carries, because it
        /// is the one payload the transcript does not have: the model-facing
        /// result numbers only the after lines, and once the write has landed
        /// the before side exists nowhere else. `#[serde(default)]` so a log
        /// recorded before the field existed replays with `None`, and a head
        /// without the two-panel view renders exactly what it always did. No
        /// `PROTOCOL_VERSION` bump: an added, defaulted field on an existing
        /// event is the version-4 argument, not the version-10 one.
        #[serde(default)]
        edit: Option<ToolEdit>,
    },
    TurnFinished {
        turn_id: String,
        finish_reason: FinishReason,
        usage: Usage,
        timings: Timings,
    },
    TurnInterrupted {
        turn_id: String,
        reason: String,
        partial_kept: bool,
    },
    /// **The turn ended in a failure.** `crates/ui/DESIGN.md` §4.5, paid.
    ///
    /// Before this existed a turn that failed published a `Warning` and *no*
    /// terminal event, so a head's `TurnState` stayed `Running` for the rest of the
    /// session. Observed live: `! turn_failed — a frame advanced tokens_predicted
    /// 142 -> 144 but carried 1 id(s)`, after which the head sat at `Responding`
    /// forever. The head covered for it with *"nothing received for 17.0s"*, and a
    /// head saying that cannot tell a failed turn from a slow one — only the party
    /// that saw the error can, which is the party that publishes this.
    ///
    /// It is a **separate variant** rather than a `TurnInterrupted` with a reason
    /// or a `TurnFinished { finish_reason: Other }`, because both of those are
    /// endings a turn is allowed to have: an interrupt is something a person did,
    /// and a finish is a turn that produced an answer. A failure is neither, and
    /// folding it into either is the same move as recording a `length` as a
    /// completed turn.
    TurnFailed {
        turn_id: String,
        /// What went wrong, in the words the daemon has. Never abbreviated: this is
        /// the only place the reason exists once the process moves on.
        error: String,
        /// Whether anything the model wrote before the failure was committed. False
        /// for every §5.7 failure — the engine commits nothing — and the field is
        /// present so a future failure that *does* keep a partial cannot arrive
        /// looking like one that does not.
        partial_kept: bool,
    },
    TranscriptAppended {
        item_id: String,
        kind: String,
        ledger_head: String,
    },
    /// **The second addition to §4.5, and it is T13.1 being paid rather than
    /// deferred.**
    ///
    /// §4.5's `TranscriptAppended` announces a row and carries no body, and until
    /// now the body reached a head *only* through the snapshot
    /// ([`crate::view::SessionView::record_item`]). That works for a head that
    /// attaches after the row exists and fails permanently for one that was already
    /// attached: it is told a row landed, no later frame ever carries the content,
    /// and its placeholder is not a loading state — it is the final state. Measured
    /// on a live session: the operator's own prompt renders as
    /// `[user … — content not loaded]` forever.
    ///
    /// T13.1 also argues the log should be a **sufficient record of a session**, and
    /// it is not one today: `letibot-tui --replay recorded.jsonl` shows a placeholder
    /// for every row, because the only copy of the content was in a snapshot that
    /// was never written down. Assistant text is *nearly* recoverable from `Delta`,
    /// but the item boundaries are not, and a `ToolResult` payload is not on the
    /// wire at all — `ToolFinished` carries a digest by design.
    ///
    /// So the content goes in the log. **The right shape is a body on
    /// `TranscriptAppended` itself** — one row, one event, and no
    /// announced-but-empty state for anyone to render. That shape needs
    /// `letibot_turn::TurnEvent::TranscriptAppended` to carry the item too, because
    /// the engine is what emits the announcement and the daemon does not hold the
    /// item until the engine's call returns. Until that seam is widened, this
    /// variant carries the body a moment later, on the same log, and a head handles
    /// it with the same code it will use afterwards: fill the row this names.
    /// Deleting this variant is then the whole of the migration.
    ///
    /// Boxed for the reason `Hello.snapshot` is: a `TranscriptItem` is two orders
    /// of magnitude larger than a `Delta`, and an unboxed one would make every
    /// event on the hot path pay for it.
    TranscriptContent {
        item_id: String,
        item: Box<letibot_transcript::TranscriptItem>,
    },
    /// **The transcript this session speaks is not the one the rows you hold belong to.**
    ///
    /// A fork — a `/reseat`, a `/compact`, a re-seat onto a rebuilt prompt — opens a NEW
    /// transcript and carries the conversation into it, so every carried row is published
    /// under the new transcript's ids (`{transcript_id}.{n}`, `engine.rs`).
    /// [`SessionEvent::TranscriptAppended`] alone cannot say that: a row arriving under an id
    /// no reader has seen is indistinguishable from one arriving under an id it has, and
    /// **nothing in the events themselves clears the rows already held** — so every reader
    /// folded both conversations into one list.
    ///
    /// MEASURED, in the head's own fixtures: a 40-row conversation plus a 60-row carry leaves
    /// `items.len() == 100`, the reader still anchored on a row of the transcript that went,
    /// and the banner reading `144 line(s) below`. The operator, scrolled up and reading, typed
    /// `/reseat` and was left with *"thousands of lines 'below'"* — the anchor half of that is
    /// `agent/carry-reanchors-the-view`'s, and this is the other half.
    ///
    /// # Why the daemon states it, and not the reader
    ///
    /// The daemon is the one that forked: `fork_to_summary` is the single place that opens the
    /// new transcript and swaps the session for it. A reader can only *infer* it — from an id
    /// whose ordinal restarts at zero, which is the engine's minting rule read backwards, and
    /// which would then be a second copy of a rule that has one owner. `Filling`'s ruling is the
    /// precedent, one carry earlier: *"only the daemon knows which operation is running, because
    /// it is the one running it"* — and the fix there was the same: **let the layer that owns
    /// the fact state it.**
    ///
    /// # What a reader of this event does
    ///
    /// Drops the rows it holds **that are numbered in `parent_id`** — the transcript that went —
    /// and only those. Two readers, one rule: the daemon's own [`crate::view::SessionView`],
    /// which would otherwise hand a late-attaching head *both* conversations (the same defect one
    /// attach later), and a head.
    ///
    /// **The rows of any other transcript stay, and that is not a detail.** A reader can hold
    /// rows of more than one transcript on purpose: a resume republishes the tail of the
    /// transcripts a compaction put behind the current one, so that a reader can scroll above the
    /// summary they resumed onto. A fork replaces the transcript the session is speaking and
    /// nothing else, so the event names it and a reader drops exactly that — a rule that dropped
    /// everything would take the restored history with it.
    ///
    /// **And the rows are REPLACED, not trimmed.** They are not missing from a window, so a
    /// reader must not count them as `items_dropped` and go looking for them: the transcript they
    /// belong to is gone from the session, not from this reader's view of it.
    ///
    /// # Where it is published, and why the order is free
    ///
    /// After the fork's rows, because it names the transcript it replaces rather than a count —
    /// so a reader drops exactly those rows whenever it hears the statement, whether it arrives
    /// before them or after. That leaves the order to be chosen for what it costs: a statement
    /// published first would strand a head that reconnected into the one-seq window between the
    /// two (it would replay the rows and not the boundary), and would leave every reader emptied
    /// by a fork whose `append_items` then failed, while the session still speaks the transcript
    /// it was told it had left.
    ///
    /// # Why this moves the number
    ///
    /// A new VARIANT, so `PROTOCOL_VERSION` moves: `serde` has no catch-all on this enum,
    /// deliberately, so a head built before it cannot decode the frame at all — and the two sides
    /// refuse the mismatch by name at ATTACH rather than a head meeting one mid-session. An
    /// added, defaulted *field* would have been the zero-bump route and it is the wrong one here:
    /// an older head ignores an unknown field and renders exactly what it rendered before, which
    /// for this fact is **the conversation drawn twice** — the defect itself, kept in silence,
    /// which is precisely what the no-catch-all rule exists to refuse.
    TranscriptForked {
        /// The transcript that replaced it — the one every row that follows is numbered in.
        transcript_id: String,
        /// The transcript it replaced: the one the rows a reader is holding belong to. Carried
        /// for the log, which is the durable record of where a session's conversation went —
        /// `ForkReport::parent_id` keeps it for the same reason — rather than for a reader that
        /// acts on it.
        parent_id: String,
    },
    HeadAttached {
        head_id: String,
        kind: String,
        identity: String,
    },
    HeadDetached {
        head_id: String,
        kind: String,
        identity: String,
    },
    /// §18's post-flight assertions land here, and so do §8.5's guards.
    ///
    /// **`compaction` is the structure behind the sentence** (R27). The sentence stays and
    /// is unchanged — it is the fallback every unreadable path lands on — and this is what
    /// a head draws a row from instead of parsing English out of `detail`.
    ///
    /// **No `PROTOCOL_VERSION` bump**: an added, defaulted field on an existing variant,
    /// the precedent `ModelAdvice::consulted` and `DecisionRequested::access` set. An older
    /// head ignores an unknown field and renders exactly what it rendered before; a newer
    /// head reading an older daemon sees `None` and falls back to the sentence. Both
    /// directions are safe, which is the test for whether a bump is owed.
    Warning {
        code: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compaction: Option<Box<CompactionReport>>,
    },
    /// **A tool asked what the operator is looking at.** Every attached head that
    /// can render should answer with [`crate::protocol::ClientFrame::Screen`];
    /// the first answer wins and the rest are ignored. Carries nothing but the
    /// id — the request is "draw yourself", and the head decides its own size.
    ScreenRequested { req_id: String },
    /// `sudo` inside a session's command wants a password. The head shows the
    /// command and sudo's prompt, takes the password in a masked field, and
    /// answers with [`crate::protocol::ClientFrame::Secret`]. The event carries
    /// no secret and is safe to persist; the answer is never an event.
    SecretRequested {
        req_id: String,
        prompt: String,
        command: String,
        /// Unix millis; after it the helper gives up and `sudo` fails.
        deadline: u64,
    },
    /// **A run of the operator's own is waiting for an answer.**
    ///
    /// The operator's `!` line is the one run whose input this daemon holds
    /// (`letibot_tools::exec::Stdin`) — its own terminal, and a pipe only on a box where no pty
    /// opens — so *"the program is waiting for a line"* is a fact
    /// about the process and not a guess about its words — `letibot_tools::exec::ask` reads
    /// `/proc/<pid>/fd/0` against the device this daemon holds and
    /// `/proc/<pid>/task/*/wchan` plus `/proc/<tid>/syscall` for a read on it, and **that** is
    /// what raises this. The operator's own correction is why it
    /// is not a text match: *"i think `Continue?` is an overfit"* — question wording is
    /// per-program, per-locale and per-version, and a matcher for it fails silently on the
    /// next program.
    ///
    /// A head draws a card with `question` shown and the line it takes sent back as
    /// [`crate::protocol::ClientFrame::PromptAnswer`]. **Not a secret and never one**: the
    /// field is drawn in the open, and a password has its own path (`SUDO_ASKPASS`, an
    /// `askpass` head, `SecretRequested`) whose rules this must not be able to borrow.
    ///
    /// **The card is a convenience and not the way in.** The same line can be sent at any
    /// moment with `!send` ([`crate::protocol::ClientFrame::SendLine`]), which needs no
    /// signal at all — because the detection above has misses it names (a program blocked on
    /// another fd, one that asks and keeps drawing, a `/proc` a confined session's daemon may
    /// not read), and a person watching the stream can always answer.
    ///
    /// **Ephemeral**, like `SecretRequested` and for its reason: the request names a live
    /// run, and a head that attached after the run began has not seen the output the question
    /// is about. Its settlement ([`SessionEvent::PromptSettled`]) is the record and stays.
    PromptRequested {
        /// The handle [`crate::protocol::ClientFrame::PromptAnswer`] comes back under.
        req_id: String,
        /// The job's handle, as `job_list` spells it. For the record and for a head that
        /// wants to name it; the answer does not travel by it, so a stale card cannot
        /// address a later command.
        job: String,
        /// The operator's own command, verbatim, as they typed it after the `!`.
        ///
        /// **The command and not a program name**, deliberately: the daemon cannot know
        /// which process in a pipeline asked (`sudo apt install mc` is three programs and
        /// the question is the third one's), so it names the one thing that is certain —
        /// what the person typed.
        command: String,
        /// **The last line the program wrote, for the card to SHOW.** `None` when it has
        /// written nothing at all, which is a real case (`! cat`, blocked before its first
        /// byte). Nothing anywhere decides anything by this string.
        ///
        /// `None` also when `reading` is [`PromptReading::Unreadable`]: there the daemon has
        /// no reading of the process to quote from, and the card's own words are the honest
        /// sentence instead.
        question: Option<String>,
        /// **Which of the two facts the card is raised on** — the reading, or the absence of
        /// one. See [`PromptReading`]: the card's text says which it is, and a head that
        /// draws one card for both would be claiming a reading it was never given.
        ///
        /// **No `PROTOCOL_VERSION` bump**: an added, defaulted field on an existing variant,
        /// the precedent `Warning::compaction` states — *an older head ignores an unknown
        /// field and renders exactly what it rendered before; a newer head reading an older
        /// daemon sees the default and falls back*. The default is [`PromptReading::Blocked`],
        /// which is the only kind of card a daemon older than this field ever raised, so both
        /// directions are safe — which is this file's own test for whether a bump is owed.
        ///
        /// What a head built before the field does with an `Unreadable` card: it ignores
        /// `reading`, draws its asking card with no question line, and the warning notice —
        /// which still names `!send` — lands beside it. An overclaiming headline on an old
        /// head is the skew an added field buys; a wrong card on every head was the
        /// alternative.
        #[serde(default)]
        reading: PromptReading,
    },
    /// Whether a line was sent to the waiting run, and by whom — the record, without the
    /// line.
    ///
    /// **The line itself is not here**, for the same reason a password is not: it is a
    /// `ClientFrame` that reaches a program's stdin and nothing else — not the log, not the
    /// view, not a `CommandIssued`. What a corpus may need to know is *that* a person
    /// answered and who, which is this.
    ///
    /// `sent: false` is the run ending with the card still up, or the send failing — the
    /// two are told apart by `by`, which is a person's identity in the first case and a
    /// sentence in the second.
    PromptSettled {
        req_id: String,
        sent: bool,
        by: String,
    },
    /// Whether a password was given for `req_id`, and by which head — the
    /// record, without the secret.
    SecretSettled {
        req_id: String,
        given: bool,
        by: String,
    },
    /// §6. `plan` is untyped until W14 says what `ExplainPlan` is.
    Explain {
        turn_id: String,
        plan: serde_json::Value,
    },

    /// **The one addition to §4.5, and it is required by §13.2.**
    ///
    /// > "a second head's prompt is **queued as a follow-up user item** rather than
    /// > rejected … and the queuing is **announced as an event so both heads see it
    /// > and who did it**. Interrupt/abort is idempotent, any attached head may
    /// > issue it, and it is **announced with the issuer's identity**."
    ///
    /// Nothing in §4.5 can carry that. `TranscriptAppended` has no issuer;
    /// `HeadAttached` has an identity but no command; `Warning` is for §18's
    /// assertions and using it here would make a normal multi-head action look like
    /// a defect. Two heads sharing a session and being unable to see which of them
    /// interrupted the turn is precisely the failure §13.2 is written against, so
    /// the honest fix is a variant rather than an abuse of one.
    ///
    /// Written up in the W7 report as a proposed amendment to §4.5, not as a silent
    /// widening.
    CommandIssued {
        head_id: String,
        identity: String,
        /// `prompt`, `interrupt`, `answer`.
        command: String,
        client_request_id: String,
        /// What the daemon did with it, in the words the issuing head was given.
        note: String,
    },

    /// **The second addition required by §13.2, for the same reason as the first.**
    ///
    /// A session's title reaches a head in `SessionBrief`, which travels only in
    /// `Hello` and in `Sessions` — both of which a head receives when it *asks*. So a
    /// title set during a session (by `/rename`, or by the daemon naming an unnamed
    /// session from its first message) was invisible until the operator opened the
    /// picker, and the header went on showing the raw id over a session that had a
    /// name. Two heads sharing a session saw two different names for it, which is the
    /// same failure `CommandIssued` exists to prevent one variant along.
    ///
    /// It carries no `session_id`: an event is already addressed to one, by the
    /// envelope it travels in and by the log it is appended to.
    SessionRenamed {
        /// Empty means the name was cleared, and a head goes back to showing the id.
        /// Not `Option`: "renamed to nothing" and "no rename" must not be one shape.
        title: String,
    },

    /// **The session's todo list, as the model last wrote it.** `PROTOCOL_VERSION` 9.
    ///
    /// The whole list every time, not a delta — a delta the model got wrong is a
    /// delta nobody can audit, and a head that missed one event would render a
    /// list that never happened. A head keeps the latest one; the todos pane
    /// renders it next to the repo's own `TODO.md`, and the two are different
    /// lists — an agent's plan and the operator's queue — which is why this is an
    /// event rather than the head polling the store.
    ///
    /// Defined here rather than borrowed from the store crate: a head parses this
    /// without a database, and the protocol does not grow a storage dependency to
    /// save one `struct`.
    TodosUpdated {
        /// The list, in the order the model wrote it. Empty clears the pane's
        /// first section; that is a real state, not a missing one.
        todos: Vec<TodoEntry>,
    },

    /// **A refusal, delivered the moment it is decided.** `PROTOCOL_VERSION` 6.
    ///
    /// `docs/boundary-and-adjudication.md` §4b is the requirement, and it names the
    /// chain rather than the complaint: the classifier denies, the operator is not
    /// told, the model sees an unexplained failure and infers *the approach was
    /// wrong* rather than *the action was forbidden*, so it tries a variant — the
    /// routing-around every rule in this repo forbids, **induced by the design** —
    /// and the task dies with the operator seeing only a dead task.
    ///
    /// Three parties and three visibilities, and conflating any two is a defect. The
    /// model already had its half (`ToolOutcome::NotRun`, and the prose
    /// `letibot_tools::refusal_text` builds); the durable log already had its half
    /// (§11.5's rows). **The operator had nothing**, and this variant is that.
    ///
    /// # Why not a `Warning`
    ///
    /// A `Warning` is for §18's post-flight assertions. A denial is not a defect —
    /// it is a decision taken on the operator's behalf, and they alone can lift it.
    /// Rendering the two the same way is how a decision becomes noise. Same
    /// argument as [`SessionEvent::CommandIssued`], one variant along.
    ///
    /// # Why the fields are flat scalars
    ///
    /// `letibot-tools` is an **optional** dependency of this crate — the log, the
    /// protocol and the scrub must build with no path into a filesystem a tool can
    /// read — so `letibot_tools::DenialNotice` cannot appear here. The fields are
    /// its fields, and the two that are facts rather than prose (`repeat_count`,
    /// `breaker_open`) travel as facts, so a head can collapse repeats without
    /// parsing a sentence.
    DenialRaised {
        /// The adjudication request id. What an operator grants **by name**.
        request_id: String,
        turn_id: String,
        call_id: String,
        tool: String,
        /// The one argument a person reads: a path, a pattern, a command line.
        summary: String,
        /// Layer A's deterministic one-line reading of the call.
        baseline: String,
        /// Who decided: `boundary:host`, `model:…`, `breaker`, or `none`.
        by: String,
        basis: String,
        /// `blocked`, `always_ask`, `adjudicable`, `auto`.
        tier: String,
        /// **`denied` (somebody decided) or `not_run` (nobody did).** Never
        /// collapsed: `not_run` is not a denial, and it is not permission either.
        outcome: String,
        /// How many consecutive refusals this task direction has had, this one
        /// counted. A head seeing more than one is looking at the operator's own
        /// complaint — *"a second attempt currently looks like a fresh request"* —
        /// and has what it needs to collapse them.
        repeat_count: u32,
        /// The consecutive-denial circuit breaker is open for this direction: no
        /// adjudicator will be consulted again in this session, and only the
        /// operator can lift it.
        breaker_open: bool,
        /// **What the operator can do about it right now.** A refusal whose grant
        /// path arrives after the task has died is a refusal that can only be
        /// routed around, with an extra step.
        grant: String,
    },
    /// A subagent this session spawned changed state. Carried on the **parent**'s
    /// hub, so a head attached to the parent sees the subagent spawn and finish
    /// without subscribing to the subagent's own hub.
    ///
    /// `state` is `opening`, `running`, `done` or `failed`.
    Subagent {
        /// The subagent's own session id. Named `subagent_id` rather than
        /// `session_id` because [`Envelope`] already carries the *parent*'s
        /// `session_id`, and a flattened duplicate field would fail to parse.
        subagent_id: String,
        state: String,
        /// **The subtask's first line on the opening states, and the child's answer's
        /// first line on the finish** — a field with two meanings, kept exactly as it
        /// was so a head older than `task` below is unchanged.
        ///
        /// **A new head must not read the ROW from this.** Its meaning depends on
        /// `state`, and on the finish it is no longer what the child was asked. Read
        /// [`SessionEvent::Subagent::task`] for the row and
        /// [`SessionEvent::Subagent::answer`] for the subtitle. The operator, reading
        /// this pane: *"the first prompt is truncated too early"*, and the measured
        /// case was a row of 122 characters which were the ANSWER, with the task — two
        /// lines, ~250 characters — nowhere on the wire at all.
        prompt: String,
        role: String,
        /// **The subtask this child was asked to do, in full, and the same string on
        /// every state.**
        ///
        /// Carried whole — never truncated by the daemon — because it is the only copy
        /// a head gets and the head truncates for a row as it does for everything else
        /// it draws. `#[serde(default)]` so a daemon built before this field answers
        /// with an empty string and a head falls back to `prompt`, which is the
        /// pre-field behaviour rather than a blank row.
        #[serde(default)]
        task: String,
        /// **The model this child runs on, as `PROVIDER/MODEL` or `local`** — the operator's
        /// ask, 2026-10-05: *"I want to be able to have subagents using different models.
        /// say you deepseek should be able to run local model"*, and the other direction
        /// too: *"local qwen in main session should be able to run cloud glm"*.
        ///
        /// Empty when the child inherited its parent's model, which is the default and the
        /// behaviour every earlier build had — so the pane draws no model clause on such a
        /// row, and a reader sees exactly what they saw before. `#[serde(default)]` for the
        /// same reason this event's `task` has it: a daemon older than this field answers
        /// with an empty string and the row simply says nothing about a model.
        #[serde(default)]
        model: String,
        /// **The child's answer's first line, on the finishing state and nowhere else.**
        ///
        /// `None` while the child is opening, running, or has failed. The same string
        /// `prompt` carries on the finish, given its own name so a reader does not have
        /// to know the state to interpret it.
        #[serde(default)]
        answer: Option<String>,
    },

    /// A background job this session started has stopped running.
    /// `PROTOCOL_VERSION` 15.
    ///
    /// The **start** of a background job needs no event of its own: a backgrounded
    /// `bash` call finishes as `ToolOutcome::Backgrounded`, whose `handle` *is* the
    /// job id, and a head folding tool events already knows the job exists. What
    /// nothing carried was the **end**. A session-scoped job outlives the turn that
    /// started it — usually by design; that is what background is for — so its
    /// settlement happens when no turn is running, and the only events a hub
    /// publishes between turns are the ones the daemon publishes itself. Without
    /// this variant every head's picture of a background job was frozen at
    /// "running" forever: a panel built from the tool events alone would still show
    /// a build as running an hour after it exited, which is the exact lie
    /// `scrub::is_interactive` keeps off the wire — a progress frame from four
    /// minutes ago is a lie about now.
    ///
    /// # Why the daemon publishes it, and not the runtime
    ///
    /// The runtime reaps a job when somebody asks — `job_wait`, or turn-end scope
    /// reaping — and nobody asks between turns. The daemon runs a watcher per
    /// session that notices the transition and publishes here. The event is the
    /// record; the watcher is only how it comes to be published the moment it
    /// becomes true.
    ///
    /// # Why the fields are flat scalars
    ///
    /// Same reason as [`SessionEvent::DenialRaised`]: `letibot-tools` is an
    /// **optional** dependency of this crate, so `JobState` cannot appear here even
    /// by reference. `state` is the word a job listing shows — `exited 0`,
    /// `signalled 15`, `killed by job_kill` — deliberately not "ok"/"error": what
    /// happened to the *process* is the fact, and success is a property of the
    /// command's own exit code. `produced` is the bytes the job wrote, the same
    /// number a `job_output` denominator counts; `elapsed_ms` is wall time from
    /// spawn to settlement.
    /// **The daemon has admitted an operator's own call, and the head may run it now.**
    /// — R24 part two, decision 4.
    ///
    /// The answer to [`crate::protocol::ClientFrame::OperatorCall`], and it is on the log
    /// rather than a reply to the frame because the admission is written by the worker: the
    /// server thread's `Accepted` says *queued*, and what the head needs to know before it
    /// runs anything is *recorded*. Putting it here also means both heads see it, which is
    /// what the operator's act deserves — it happened in the session, not on one socket.
    ///
    /// `who` is the identity the admission is recorded under (`human:<who>` in `verdict_by`,
    /// and the `who` in the row's `CallOrigin`), so a head draws the card and a corpus query
    /// names the actor from one string.
    OperatorCallAllowed {
        /// The head's own handle for the call — the key its
        /// [`crate::protocol::ClientFrame::OperatorResult`] comes back under.
        call_id: String,
        /// One of [`crate::protocol::HEAD_RUN_TOOLS`]. Carried so every head can draw the
        /// call without asking the one that made it.
        name: String,
        /// Who is running it.
        who: String,
        /// The arguments, verbatim, so a second head draws the same call.
        arguments: String,
    },
    JobSettled {
        /// The job's handle, as the backgrounded result already printed it.
        job: String,
        /// What happened to the process, as a listing words it.
        state: String,
        /// Bytes the job produced, all streams together.
        produced: u64,
        /// Wall time from spawn to settlement.
        elapsed_ms: u64,
    },
    /// **A merge-queue entry was added** — the whole entry, so a head that missed the
    /// snapshot sees it rather than only later changes.
    ///
    /// The snapshot-plus-events pattern the todos and jobs use: the snapshot carries the whole
    /// queue as of now, and from then on the events carry every change. A head attaching
    /// mid-flight gets the snapshot, and a head that was attached gets this event, so neither
    /// sees a shorter queue than the other.
    MergeEntryAdded {
        /// The entry, whole: the id, the branch, the priority, the `needs`, the state and the
        /// evidence.
        entry: MergeEntry,
    },
    /// **A merge-queue entry moved** — its new state and its evidence, so a head that was
    /// attached sees the move rather than only the next snapshot.
    ///
    /// The evidence is the reason for the state, in the queue's own words: the unmet
    /// dependencies while `Waiting` with some, the gate's failure while `Failed`, the conflict
    /// while `Conflict`, the dead job while `Stale`, the landed tip while `Landed`. A move
    /// without its reason is a row the pane draws and the operator cannot read.
    MergeEntryMoved {
        /// The id of the entry that moved.
        id: String,
        /// The state it moved to.
        state: MergeState,
        /// The reason for the state, in the queue's own words.
        evidence: String,
    },
    /// **A merge-queue entry was REMOVED** — the operator's `rm`, and the only way a row leaves
    /// the queue without a state.
    ///
    /// The queue's other events carry a MOVE; this one carries an absence, and a head that
    /// folded [`SessionEvent::MergeEntryAdded`] needs it for the reason it needs
    /// [`SessionEvent::MergeEntryMoved`]: without it an open pane would go on drawing a row the
    /// queue no longer holds, which is exactly the lie the module is written to refuse and the
    /// one an operator who has just deleted an entry would be looking straight at. The head
    /// drops the row and its verdict; the `evidence` is here for the log, which is the durable
    /// record of what the queue did and why, rather than for a row there is no longer.
    MergeEntryRemoved {
        /// The id of the entry that was removed.
        id: String,
        /// Why, in the queue's own words — the act's own sentence.
        evidence: String,
    },
    /// **A window of one background job's output, for a pane that draws it.**
    ///
    /// The jobs pane drew `N out` for every row and had no way to show the bytes it counted. Enter
    /// sent `/job ID`, whose reply is a `Warning` on the session log — so the pane closed and the
    /// operator read a build log scrolling past in the chat. The operator, 2026-09-20: *"when i
    /// press enter on jobs pane im not shown the job output im brought back to the main
    /// conversation with /job <id> posted - this is not what i want"*.
    ///
    /// # Why this is an event and not a request-answered frame
    ///
    /// `Peek`, `Settings` and `Jobs` are answered on the asking connection because what they read
    /// is reachable from the **server** — the hub's view, a registry mailbox. Job output is not:
    /// it lives in the exec host, which is the daemon worker's, so the server would have to
    /// register a reply channel, queue a command and block its own read loop on the answer,
    /// stalling that connection's live events for the duration.
    ///
    /// It is also **the same thing a slash reply already is**, so publishing it keeps one rule
    /// rather than two: a verb reads, and what it found lands on the log where every head sees it.
    /// What this variant adds over the `Warning` prose is the **offsets beside the text** — a pane
    /// can draw its own header and its own paging instead of parsing a footer sentence.
    ///
    /// Additive, like [`Self::TranscriptContent`], so no protocol version moves.
    JobOutput {
        /// The job's handle, as the pane lists it.
        job: String,
        /// The offset this window actually starts at — the request is clamped, not refused.
        from: u64,
        /// One past the window's last byte.
        to: u64,
        /// Everything the job has written, all streams together.
        produced: u64,
        /// Bytes that fell off the ring before this window: a head must be able to say
        /// "there was more and it is gone" rather than showing a window whose start looks
        /// like the job's start.
        dropped: u64,
        /// The job's own word — `exited 0`, `running`, `killed by …`.
        state: String,
        /// **Whether anything was ever executed for this job.**
        ///
        /// `false` on every ending of a process that ran — including one that ran and
        /// wrote nothing, which is the ordinary empty case. `true` for the one state
        /// where the wrapper could not join the process's cgroup and **nothing started**
        /// (`JobState::NotScoped` in the tool crate's exec layer — this crate is the
        /// protocol and must not depend on that one, so the name is prose).
        ///
        /// **No `PROTOCOL_VERSION` bump**: an added, defaulted field on an existing
        /// variant, exactly like [`ModelAdvice::consulted`]. An older head ignores it and
        /// renders what it rendered before, and `false` is the honest reading of silence —
        /// a daemon from before this field had only one answer for an empty window.
        ///
        /// It is on the wire because *"the window is empty"* is two different facts and
        /// the head had one sentence for both: `it wrote nothing at all.` under a header
        /// reading `not run (could not join its scope)` — the operator's own R17 rule
        /// inverted, *a row with no output must not look like a row whose output is
        /// empty*. The words each head writes for it are A's ruling (§11.6); this is the
        /// fact that rules out choosing them by `lines.is_empty()` alone.
        #[serde(default)]
        never_ran: bool,
        /// The window, split into lines by the daemon so two heads cannot disagree about
        /// where a line ends.
        lines: Vec<String>,
        /// Where to ask next when there is more that is still readable, and `None` when the
        /// end is here. The daemon decides rather than the head computing `to`, because only
        /// the daemon knows how much of the ring survives.
        next: Option<u64>,
    },
    /// **A named operation the daemon is filling rows for**, with its own counter.
    ///
    /// R6, generalized. Four things look identical from a head — an opencode import, a
    /// reseat, a compaction, and an ordinary turn — because all four announce rows before
    /// their bodies (that is `TranscriptAppended`/`TranscriptContent`, §4.5). **Only the
    /// daemon knows which operation is running**, because it is the one running it; a head
    /// that tried to tell them apart could only infer a cause from the symptom *"rows have
    /// no bodies yet"*, and that inference is the defect it was written to avoid: the head
    /// drew *"carrying the conversation onto the new prompt"* over every ordinary reply,
    /// announcing a carry that was not happening.
    ///
    /// So the daemon names the operation (`what`), names the unit it counts (`unit`), and
    /// reports how far it has got (`done` of `total`). The head draws exactly that — it no
    /// longer derives a progress bar from body-less rows at all. This is the same rule the
    /// day's diagnosis produced three times over: **an indicator must be the fact, not a
    /// rendering of the fact** — the monitor firing, R2's queued prompt, and now this. In
    /// each case the fix was identical: *let the layer that owns the fact state it.*
    ///
    /// `unit` is a string rather than an enum because the units honestly differ and are the
    /// daemon's to name: `parts` for an import (opencode parts read), `rows` for a carry
    /// (transcript rows announced). A head renders `{done} of {total} {unit}`.
    ///
    /// **Ephemeral**, like [`SessionEvent::JobOutput`]: a progress frame from four minutes
    /// ago is a lie about now. The operation's durable residue is the rows themselves and
    /// the note that says it finished; this is the line that walks while it fills.
    Filling {
        /// The operation, in the daemon's own words — `importing an opencode
        /// conversation`, `carrying the conversation onto the new prompt`.
        what: String,
        /// What `done` and `total` count: `parts`, `rows`.
        unit: String,
        /// How much of it is done.
        done: u64,
        /// How much there is, known before the first row where it can be.
        total: u64,
    },
    /// **A fold's long wait, named as the compaction's own** — see
    /// [`letibot_turn::events::TurnEvent::CompactionProgress`] for the event this is
    /// the wire form of, and `summarise_one` for the sink that produces it.
    ///
    /// The overrun compaction summarises a SCRATCH transcript, and forwarding its raw
    /// `PromptProgress` put the scratch prompt's token count in the head's
    /// `turn.progress` — the SESSION's turn — so the operator watched `69k` sit over a
    /// 240k conversation that had not changed (2026-09-20). The number was never wrong;
    /// its label was. This is that number under its own name, which is why a head that
    /// draws these fields cannot confuse them with session context however similar the
    /// figures are.
    ///
    /// **A new variant is a `PROTOCOL_VERSION` bump**, unlike the defaulted field on
    /// [`SessionEvent::Warning`] above: an older head has no arm for this and would
    /// fail to decode the frame, so the attach-time refusal is what tells it why rather
    /// than a head that dies mid-stream. The two are different classes of change and the
    /// version note says which this is.
    ///
    /// **Ephemeral, like [`SessionEvent::Filling`]**: a progress frame from four minutes
    /// ago is a lie about now. What survives the compaction is the fork, the `compacted`
    /// warning and its [`CompactionReport`].
    CompactionProgress {
        /// Which half is running, 1-based, in the order they RUN — the recent tail
        /// first on the local plan, since its cold prompt is the cheap one to warm the
        /// server's cache with.
        half: u64,
        /// How many halves: 1 for the cloud fold, 2 for the local two-half plan.
        halves: u64,
        /// The half's own prompt, in the box's ledger tokens.
        prompt_tokens: u64,
        /// How much of it the server has read. `0` on a messages transport, which
        /// reports no prefill — a real answer, not a missing one.
        processed: u64,
        /// What the half has produced so far.
        written: u64,
        /// What `written` counts: `tokens` where the transport reports the server's own
        /// count, `chars` where it reports only text. Named, because the two transports
        /// do not count the same thing and one name for both would be a lie about one.
        unit: String,
    },
}

impl SessionEvent {
    /// The variant name, for counters and for a head's status line.
    pub fn kind(&self) -> &'static str {
        match self {
            SessionEvent::TurnStarted { .. } => "TurnStarted",
            SessionEvent::PromptProgress { .. } => "PromptProgress",
            SessionEvent::PromptRequested { .. } => "PromptRequested",
            SessionEvent::PromptSettled { .. } => "PromptSettled",
            SessionEvent::TokensGenerated { .. } => "TokensGenerated",
            SessionEvent::Delta { .. } => "Delta",
            SessionEvent::ToolCallProposed { .. } => "ToolCallProposed",
            SessionEvent::DecisionRequested { .. } => "DecisionRequested",
            SessionEvent::DecisionAnswered { .. } => "DecisionAnswered",
            SessionEvent::ToolStarted { .. } => "ToolStarted",
            SessionEvent::ToolProgress { .. } => "ToolProgress",
            SessionEvent::ToolFinished { .. } => "ToolFinished",
            SessionEvent::TurnFinished { .. } => "TurnFinished",
            SessionEvent::TurnInterrupted { .. } => "TurnInterrupted",
            SessionEvent::TurnFailed { .. } => "TurnFailed",
            SessionEvent::TranscriptAppended { .. } => "TranscriptAppended",
            SessionEvent::TranscriptContent { .. } => "TranscriptContent",
            SessionEvent::TranscriptForked { .. } => "TranscriptForked",
            SessionEvent::HeadAttached { .. } => "HeadAttached",
            SessionEvent::HeadDetached { .. } => "HeadDetached",
            SessionEvent::SessionRenamed { .. } => "SessionRenamed",
            SessionEvent::TodosUpdated { .. } => "TodosUpdated",
            SessionEvent::Warning { .. } => "Warning",
            SessionEvent::ScreenRequested { .. } => "ScreenRequested",
            SessionEvent::SecretRequested { .. } => "SecretRequested",
            SessionEvent::SecretSettled { .. } => "SecretSettled",
            SessionEvent::Explain { .. } => "Explain",
            SessionEvent::CommandIssued { .. } => "CommandIssued",
            SessionEvent::DenialRaised { .. } => "DenialRaised",
            SessionEvent::Subagent { .. } => "Subagent",
            SessionEvent::JobSettled { .. } => "JobSettled",
            SessionEvent::MergeEntryAdded { .. } => "MergeEntryAdded",
            SessionEvent::MergeEntryMoved { .. } => "MergeEntryMoved",
            SessionEvent::MergeEntryRemoved { .. } => "MergeEntryRemoved",
            SessionEvent::OperatorCallAllowed { .. } => "OperatorCallAllowed",
            SessionEvent::JobOutput { .. } => "JobOutput",
            SessionEvent::Filling { .. } => "Filling",
            SessionEvent::CompactionProgress { .. } => "CompactionProgress",
        }
    }
}

/// An event with its `(session_id, seq, ts)`.
///
/// `seq` is monotonic and gap-free **per session**, and it is the only thing a
/// head's read mark or a command's `expected_seq` ever refers to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub session_id: String,
    pub seq: u64,
    /// Unix millis.
    pub ts: u64,
    #[serde(flatten)]
    pub event: SessionEvent,
}

/// Unix millis now. Fails backwards to 0 rather than panicking: a clock before the
/// epoch is a broken clock, not a reason to lose a session.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delta_has_nowhere_to_put_accumulated_text() {
        // Same assertion `letibot-turn` carries, at the other end of the seam:
        // adding a `full` field has to delete a test that says why not.
        let json = serde_json::to_string(&SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Text,
            text: "abc".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"event":"delta","turn_id":"t1","target":"text","text":"abc"}"#
        );
    }

    #[test]
    fn every_variant_round_trips_through_the_wire() {
        for e in crate::testing::one_of_each() {
            let s = serde_json::to_string(&e).unwrap();
            let back: SessionEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(e, back, "{s}");
        }
    }

    /// **The `reading` a prompt card is raised on is safe in both directions**, which is the
    /// test for whether a `PROTOCOL_VERSION` bump is owed — and here it is not (the
    /// `Warning::compaction` precedent: *an older head ignores an unknown field and renders
    /// exactly what it rendered before; a newer head reading an older daemon sees the default
    /// and falls back*).
    ///
    /// The default is `Blocked` because a daemon older than the field only ever raised cards
    /// on a reading — so an absent `reading` is not a missing fact but the one fact there was.
    #[test]
    fn a_prompt_request_without_a_reading_reads_as_blocked_and_one_with_it_round_trips() {
        // An older daemon's frame: no `reading` at all.
        let old = r#"{"event":"prompt_requested","req_id":"r1","job":"j1",
                     "command":"sudo apt install mc","question":"Continue? [Y/n]"}"#;
        let e: SessionEvent = serde_json::from_str(old).expect("an absent reading decodes");
        assert_eq!(
            e,
            SessionEvent::PromptRequested {
                req_id: "r1".into(),
                job: "j1".into(),
                command: "sudo apt install mc".into(),
                question: Some("Continue? [Y/n]".into()),
                reading: PromptReading::Blocked,
            },
            "the default is the only kind of card an older daemon ever raised"
        );
        // This daemon's frame: `unreadable`, spelled once on the wire and read back.
        let unreadable = SessionEvent::PromptRequested {
            req_id: "r2".into(),
            job: "j2".into(),
            command: "sudo apt install mc".into(),
            question: None,
            reading: PromptReading::Unreadable,
        };
        let s = serde_json::to_string(&unreadable).unwrap();
        assert_eq!(
            serde_json::from_str::<SessionEvent>(&s).unwrap(),
            unreadable,
            "{s}"
        );
        assert!(s.contains("\"unreadable\""), "the word travels: {s}");
    }

    #[test]
    fn an_edit_excerpt_round_trips_and_an_old_log_reads_as_none() {
        let e = SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "fnv1a:1".into(),
            inline_bytes: 12,
            full_bytes: 12,
            spill: None,
            repairs: 0,
            edit: Some(ToolEdit {
                path: "crates/ui/src/diff.rs".into(),
                created: false,
                before_start: 22,
                after_start: 22,
                before_lines: 790,
                after_lines: 793,
                truncated: false,
                before: "pub fn render(old: &[&str]) -> Vec<String> {\n".into(),
                after: "pub fn render(old: &[&str]) -> Vec<String> {\n    let d = diff_lines(old, new);\n".into(),
            }),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"before_start\":22"), "{s}");
        let back: SessionEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);

        // A log recorded before the field existed has no `edit` key. It must
        // parse, and read as None — the head then renders exactly what it
        // always rendered, which is the whole compatibility argument.
        let old = r#"{"event":"tool_finished","turn_id":"t1","call_id":"c1",
            "outcome":{"outcome":"ok"},"payload_digest":"fnv1a:1",
            "inline_bytes":12,"full_bytes":12,"repairs":0}"#;
        let back: SessionEvent = serde_json::from_str(old).unwrap();
        assert!(matches!(
            back,
            SessionEvent::ToolFinished { edit: None, .. }
        ));
    }

    /// The flatten that puts an event inside an [`Envelope`] is where a field-name
    /// collision shows up, not in the event alone: an event that names a field the
    /// envelope already has (`session_id`) serialises to a duplicate key and a head
    /// fails to parse it. `Subagent` was exactly that, and this is the test that
    /// would have caught it — every variant, wrapped, must survive the wire.
    #[test]
    fn every_variant_round_trips_inside_an_envelope() {
        for (i, e) in crate::testing::one_of_each().into_iter().enumerate() {
            let env = Envelope {
                session_id: "s-parent".into(),
                seq: i as u64,
                ts: 1000,
                event: e.clone(),
            };
            let s = serde_json::to_string(&env).unwrap();
            let back: Envelope = serde_json::from_str(&s)
                .unwrap_or_else(|err| panic!("envelope round-trip failed: {err}; json: {s}"));
            assert_eq!(env, back, "{s}");
        }
    }

    #[test]
    fn a_display_target_is_the_argument_a_person_scans_for() {
        assert_eq!(
            display_target(r#"{"path":"crates/tui/src/app.rs"}"#),
            "crates/tui/src/app.rs"
        );
        // Key order is the model's, and `preserve_order` keeps it.
        assert_eq!(
            display_target(r#"{"pattern":"home.*button","path":"src"}"#),
            "home.*button src"
        );
        // A command line keeps its spaces and is quoted, so it cannot be read as
        // three arguments.
        assert_eq!(
            display_target(r#"{"cmd":"cargo test --workspace"}"#),
            "\"cargo test --workspace\""
        );
    }

    /// **R15: an edit's row is the file, and the `edits` array says nothing.**
    ///
    /// The operator's screen carried
    /// `▸ Edited […] letibot/crates/harnessd/src/answers.rs · ok · 3ms · 10 lines`, and the
    /// ruling was *"`[…]` should be gone for Edit"* — **not moved**. The placeholder sat in
    /// the most valuable position on the row while the "more" it pointed at was the diff
    /// directly underneath it.
    ///
    /// The line, and it is on the *arguments* rather than on the tool: **a nested value is
    /// elided only when the map has no scalar at all.** The elision rule therefore keeps
    /// every case where it earns its keep — a label that would otherwise be *empty* — and
    /// stops spending a token on a row that already names something.
    #[test]
    fn a_nested_argument_is_elided_only_when_nothing_else_can_be_shown() {
        // The batch edit, in the order the model writes it (`edits` first). The row is the
        // file: no placeholder before it, and none after it either.
        assert_eq!(
            display_target(r#"{"edits":[{"old":"x"}],"path":"letibot/src/answers.rs"}"#),
            "letibot/src/answers.rs"
        );
        // The other order. The previous cut of this test asserted `"a.rs […]"` — and that
        // string was the defect, not the expectation.
        assert_eq!(
            display_target(r#"{"path":"a.rs","edits":[{"old":"x"}]}"#),
            "a.rs"
        );
        // An object nested beside a scalar: the scalar is the label.
        assert_eq!(
            display_target(r#"{"cmd":"cargo test","env":{"RUST_LOG":"x"}}"#),
            "\"cargo test\""
        );

        // **And the rule keeps its other half.** A map with nothing scalar in it still
        // says there is more rather than rendering an empty label; in this tree that is
        // reachable only by a tool added later, because every seated one carries a scalar
        // `path`, `pattern` or `cmd`.
        assert_eq!(display_target(r#"{"rows":[1,2,3]}"#), "[…]");
        assert_eq!(display_target(r#"{"nested":{"a":1}}"#), "{…}");
        assert_eq!(display_target(r#"{"rows":[1],"opts":{"a":1}}"#), "[…] {…}");
        // An argument object with nothing in it at all is the empty label, which is what
        // makes the head fall back to the call id instead of drawing a blank row.
        assert_eq!(display_target("{}"), "");
    }

    #[test]
    fn the_subject_the_written_order_left_past_the_budget_comes_back_in_front() {
        // **The card the operator photographed.** A model composing an edit
        // writes the content before the file it is going into, and the budget
        // broke before `path` landed — the card opened with eighty bytes of
        // old_string and could not answer *which file*. The subject the order
        // left out is prepended; the content head follows it.
        //
        // **The content is sized off the cap, not off a literal (R25).** This used 200-byte
        // strings, which broke a 120-byte budget and stopped breaking anything the moment the
        // wire limit moved — a test whose premise is a number the code no longer has. The rule
        // it tests is unchanged; what it takes to reach it is now a genuinely long argument.
        let args = format!(
            r#"{{"old_string":"{}","new_string":"{}","path":"crates/tui/src/app.rs"}}"#,
            "x".repeat(TARGET_MAX_BYTES + 100),
            "y".repeat(TARGET_MAX_BYTES + 100),
        );
        let t = display_target(&args);
        assert!(t.starts_with("crates/tui/src/app.rs"), "{t}");
        // **And the body is not in the label at all** — under the cap or over it.
        assert_eq!(t, "crates/tui/src/app.rs");
        assert_eq!(
            display_target(
                r#"{"content":"Mechanism confirmed — thank you, this one was worth checking","path":"/tmp/reply-thread-1.txt"}"#
            ),
            "/tmp/reply-thread-1.txt",
            "a write's row is its file, whatever order the model wrote the arguments in"
        );
        assert_eq!(
            display_target(r#"{"path":"a.rs","old_string":"x y","new_string":"z w"}"#),
            "a.rs"
        );
        // With no file named, a body is still the only thing to show, and is shown.
        assert_eq!(display_target(r#"{"content":"hello"}"#), "hello");
        // An order that already shows every argument — grep's pattern, then
        // its path — does not move.
        assert_eq!(
            display_target(r#"{"pattern":"home.*button","path":"src"}"#),
            "home.*button src"
        );
    }

    #[test]
    fn a_target_never_exceeds_the_cap_and_says_when_it_was_cut() {
        // **Sized off the constant, not off 400** (R25). The old literal was written when the
        // cap was 120 and quietly stopped testing anything the moment the cap moved past it —
        // a test whose premise is a number the code no longer has is a green test about
        // nothing.
        let long = "x".repeat(TARGET_MAX_BYTES + 100);
        let t = display_target(&format!(r#"{{"path":"{long}"}}"#));
        assert!(t.len() <= TARGET_MAX_BYTES, "{} bytes", t.len());
        assert!(t.ends_with('…'), "{t}");
    }

    /// **A realistic long command reaches a head whole, and it is the head that cuts it** (R25).
    ///
    /// The operator's measurement was a 227-column pane showing a headline cut at 121
    /// characters, so ~100 columns went unused on every tool row. The wire limit is not a
    /// viewport, so a command of a few hundred characters must arrive intact and be cut — with
    /// its own `…` — by the layer that knows the width.
    #[test]
    fn a_command_of_a_few_hundred_characters_survives_the_wire_whole() {
        let cmd = format!(
            "cd /opt/secure_auth && gcc -o test_auth {} -Wl,-rpath,/opt/secure_auth/lib && ./test_auth --selftest",
            "-Iinclude ".repeat(10)
        );
        assert!(
            cmd.len() > 120,
            "the premise is a command the OLD cap would have cut"
        );
        assert!(cmd.len() < TARGET_MAX_BYTES, "and one the new cap may not");
        let t = display_target(&format!(
            r#"{{"command":{}}}"#,
            serde_json::to_string(&cmd).unwrap()
        ));
        // A string with whitespace is quoted, so `grep "two words" src` cannot be misread as
        // three arguments — that is `scalar`'s rule and the quoted form is the right one.
        assert_eq!(
            t,
            format!("{cmd:?}"),
            "the wire cut a command it had no reason to touch"
        );
        assert!(!t.ends_with('…'), "and it did not claim to: {t}");
        // The whole tail survives, which is the operator's complaint in one assertion: the
        // end of a long command is what a 120-byte cut used to take.
        assert!(t.contains("--selftest"), "{t}");
    }

    #[test]
    fn a_target_never_carries_a_control_character_into_a_one_line_header() {
        // A newline here would put a row on the screen the head did not count,
        // which scrolls the frame it has just painted — the same fault
        // `rano::width::text::break_cells` had, one layer up.
        let t = display_target("{\"cmd\":\"a\\nb\"}");
        assert!(!t.contains('\n'), "{t:?}");
    }

    #[test]
    fn arguments_that_are_not_json_still_produce_something_readable() {
        // The model wrote them; refusing to show them would hide the evidence of
        // exactly the malformed call this case is.
        assert_eq!(display_target("  path=src/main.rs  "), "path=src/main.rs");
    }

    #[test]
    fn f_sim_is_absent_rather_than_invented_for_an_empty_prompt() {
        assert_eq!(Usage::default().f_sim(), None);
        let u = Usage {
            prompt_tokens: 100,
            cached_tokens: 90,
            predicted_tokens: 5,
            cost_micros_usd: None,
        };
        assert_eq!(u.f_sim(), Some(0.9));
    }
}
