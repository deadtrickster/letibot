//! The session event enum every head and every log row reads, plus the envelope that
//! addresses one event to one session.
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
//! A new VARIANT, or a new word in a closed enum, moves `PROTOCOL_VERSION`: serde has no
//! catch-all, so an older peer cannot decode the frame and the two sides refuse the mismatch by
//! name at attach. An added, defaulted FIELD does not move it.

use serde::{Deserialize, Serialize};

/// Which channel a delta belongs to. `ToolCall` carries the model's raw `<function=` markup from
/// inside a tool call, so a head draws it as code rather than as prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaTarget {
    Text,
    Reasoning,
    ToolCall,
}

/// Prefill progress for one prompt, as it reaches a head: cached, processed, and how long it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PromptProgress {
    pub total: u64,
    pub cache: u64,
    pub processed: u64,
    pub time_ms: u64,
}

/// Both sides of a file-editing call, bounded to what differs, as a `ToolFinished` carries it:
/// the edit a person wants to read is the diff, not two whole files.
pub use letibot_transcript::ToolEditExcerpt as ToolEdit;

/// A todo's state, on the wire. The same five words the store spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    /// The operator set the row aside: it persists and the model still sees it, but the idle
    /// check stops asking. A copy rather than a re-export for [`TodoBy`]'s reason, and the word
    /// moves `PROTOCOL_VERSION` because a head built before it cannot decode it.
    Postponed,
    /// The operator struck the row off, and the row stays: `/todo rm N` writes this rather than
    /// deleting, because everything that was ever created stays in history.
    Cancelled,
}

/// Who wrote a todo, on the wire: the whole of the difference between the operator's items and
/// the model's, which share one list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TodoBy {
    #[default]
    Model,
    Operator,
    /// A parent session wrote it on a child's board. The string is the author verbatim -
    /// `Parent <full parent session id>` - so a child can tell what it decided from what it was
    /// told, and by whom.
    Parent(String),
    /// The merge queue wrote it on the gatekeeper's board - the one author that is not a session.
    /// The daemon refills this half from the store every pass and replaces it, so a row leaves when
    /// its entry's verdict arrives; an entry's STATE lives on the store's row, never here.
    Queue,
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
            // The variant carries the author string, so the wire form is the string itself.
            TodoBy::Parent(author) => s.serialize_str(author),
            TodoBy::Queue => s.serialize_str("queue"),
        }
    }
}

impl<'de> Deserialize<'de> for TodoBy {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<TodoBy, D::Error> {
        let word = String::deserialize(d)?;
        match word.as_str() {
            "model" => Ok(TodoBy::Model),
            "operator" => Ok(TodoBy::Operator),
            "queue" => Ok(TodoBy::Queue),
            // `Parent ...` is the one open spelling; any other word is refused by name rather
            // than read as the model's, which is what a catch-all would do.
            other if other.starts_with("Parent ") => Ok(TodoBy::Parent(other.to_string())),
            other => Err(serde::de::Error::custom(format!(
                "`{other}` is not a todo author: model, operator, queue, or `Parent <session-id>`"
            ))),
        }
    }
}

/// A path a gated action opens for writing.
///
/// ```text
/// write_targets: [{"path": "src/syntax.rs", "unresolved": false},
///                 {"path": "Path.home() / name", "unresolved": true}]
/// ```
///
/// `unresolved` is a path the parser could not place, so a grant made over it covers a shape and
/// not a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteTarget {
    /// The path, or the text that names a path the classifier could not place.
    pub path: String,
    /// Could the classifier place it? `true` draws in the attention register, because a sentence
    /// that looks like a path is a sentence that gets skimmed past.
    #[serde(default)]
    pub unresolved: bool,
}

/// One line of a session's todo list, on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoEntry {
    pub content: String,
    pub status: TodoStatus,
    /// Who wrote the row - the whole of the difference between the operator's items and the
    /// model's, which share this list, this format and this tool.
    #[serde(default)]
    pub by: TodoBy,
    /// What the row waits on, when it waits rather than simply being undone - see
    /// `TodoItem::when` for the shape and for why a condition nobody can evaluate must never read
    /// as *met*. `serde(default)`, so a row written before the field reads back as unconditional.
    #[serde(default)]
    pub when: Option<TodoCondition>,
    /// The rows and children this one waits for: see `TodoItem::needs` for what an edge means,
    /// and why an unresolvable one must never read as met.
    #[serde(default)]
    pub needs: Vec<TodoNeed>,
}

/// How long a row's TITLE may be, and what a title is: the wire's copy of the store's bound,
/// because a head and the daemon must cut at the same place.
pub const TODO_TITLE_MAX: usize = 135;

/// The separator between a row's title and its detail - an em dash between the two, which is what
/// the head's card and template seed already write. See [`TODO_TITLE_MAX`] for why this crate has
/// its own copy of the store's constant.
pub const TODO_DETAIL_SEP: &str = " — ";

/// A row's TITLE: the words before [`TODO_DETAIL_SEP`], or the whole row when it has none - the
/// string [`TODO_TITLE_MAX`] bounds. The store's `todo_title`, copied for [`TODO_TITLE_MAX`]'s
/// reason.
pub fn todo_title(content: &str) -> &str {
    content
        .split_once(TODO_DETAIL_SEP)
        .map(|(title, _)| title)
        .unwrap_or(content)
        .trim()
}

/// One row's state, named by its words: what an operator's `/todo postpone|resume` carries when
/// the row it names is not on their board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoState {
    /// The row's own words, exactly as the board shows them.
    pub content: String,
    pub status: TodoStatus,
}

/// What a row waits on, as the WIRE spells it: a copy of the store's `TodoNeed`, not a re-export,
/// for `TodoCondition`'s reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoNeed {
    /// Another row on this board, by its exact `content`.
    Row { content: String },
    /// A child session, by its session id - the handle `task` handed back.
    Child { id: String },
    /// A background job, by its handle - the one `bash background: true` and `job_list` print.
    /// [`TodoNeed::Child`]'s sibling one layer down: an EDGE, where a `when` of the same handle is
    /// the operator's clock.
    Job { handle: String },
    /// A monitor, by the name it was armed under.
    Monitor { name: String },
    /// A room item the holder has on its board. The number is the room's item id - the one `room`'s
    /// `done`/`beat`/`abandon` take - and `working` is whether the room is carrying what the holder
    /// said it waits for.
    Room { item: u64, working: bool },
    /// The operator, asked a question with these choices: the row waits until a person answers. A
    /// head draws it as `waits on you` and opens the question card from it.
    Operator {
        question: String,
        options: Vec<String>,
    },
    /// An operator edge the operator has answered - met, and kept as the reply so the row still
    /// records what it waited for.
    Answered { question: String, reply: String },
    /// A kind this build does not know. Kept rather than refused, because a plan is what would be
    /// lost.
    #[serde(other)]
    Unknown,
}

/// What a condition is, as the WIRE spells it: a copy of the store's `TodoCondition`, not a
/// re-export, for `TodoBy`'s reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoCondition {
    /// Due when this job is no longer running - the handle `bash background: true` and `task` hand
    /// back, and the one `job_list` prints.
    Job { handle: String },
}

/// The review queue and the merge queue as a head reads them: one snapshot of both, with who
/// serves each repository and how fresh that owner's beat is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueSnapshot {
    /// When the daemon read it, Unix ms - the clock every age in the pane is measured against.
    pub at_ms: u64,
    #[serde(default)]
    pub reviews: Vec<QueueReview>,
    #[serde(default)]
    pub merges: Vec<QueueMerge>,
    /// Who holds each repository's lease, and the heartbeat's time.
    #[serde(default)]
    pub leases: Vec<QueueLeaseView>,
    /// The target branches' health - a `red` one is the emergency the pane leads with.
    #[serde(default)]
    pub targets: Vec<TargetHealthView>,
    /// The repositories that are no queue target at all: their target declares no merge gate, so
    /// the queue was never theirs and nothing about them is counted or alarmed on. The pane draws
    /// them in one muted line and nothing more.
    #[serde(default)]
    pub not_queued: Vec<String>,
    /// The last moves across both queues, oldest first.
    #[serde(default)]
    pub recent: Vec<QueueMove>,
    /// How many rows of the queue that predates the review/merge split are archived.
    #[serde(default)]
    pub archived: usize,
    /// Why the snapshot is incomplete, when it is: the store could not be read. `None` is a whole
    /// read.
    #[serde(default)]
    pub unreadable: Option<String>,
    /// Which repository the ASKING session is a session of, filled per connection by the server,
    /// so a head scopes its pane to the repository it is actually in.
    #[serde(default)]
    pub here: Option<QueueHere>,
}

/// The asking session's place in the queues - see [`QueueSnapshot::here`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueHere {
    /// The session's workspace, as the store has it.
    pub workspace: String,
    /// The queues' key for the repository that workspace is a session of - `None` when it is a
    /// session of no repository.
    #[serde(default)]
    pub repository: Option<String>,
    /// Every repository the workspace is a session of - one, or each project of a folder that is
    /// several. Defaulted, so an older daemon's snapshot reads as empty and the head falls back to
    /// `repository`, which is still set when there is exactly one so an older head reads this one.
    #[serde(default)]
    pub repositories: Vec<String>,
}

/// One review row on the wire - see `letibot_tokencore::store::ReviewRow`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueReview {
    pub id: String,
    pub repository: String,
    pub target: String,
    /// `branch`, or `target` - the target's own red tip.
    pub kind: String,
    pub session_id: String,
    pub branch: String,
    /// `waiting`, `reviewing`, `approved`, `rejected`, `cannot_decide`, `blocked`, `withdrawn`,
    /// `archived`.
    pub state: String,
    /// The lease holder working it, `""` when nobody is.
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub head_sha: String,
    #[serde(default)]
    pub reviewed_sha: Option<String>,
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub verdict_by: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub failure: String,
    #[serde(default)]
    pub reasons: Vec<String>,
    pub evidence: String,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    #[serde(default)]
    pub fixes: Option<String>,
    pub created_ms: u64,
    /// When the row entered its current state.
    pub state_ms: u64,
    #[serde(default)]
    pub brief: String,
}

/// One merge row on the wire - see `letibot_tokencore::store::MergeRow`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueMerge {
    pub id: String,
    pub review_id: String,
    pub repository: String,
    pub target: String,
    pub branch: String,
    /// `queued`, `gating`, `landed`, `failed`, `timed_out`, `conflict`, `moved`, `blocked`,
    /// `withdrawn`.
    pub state: String,
    #[serde(default)]
    pub owner: String,
    pub approved_sha: String,
    #[serde(default)]
    pub approved_by: String,
    #[serde(default)]
    pub rebased_sha: Option<String>,
    #[serde(default)]
    pub landed_sha: Option<String>,
    #[serde(default)]
    pub pushed: String,
    pub evidence: String,
    #[serde(default)]
    pub gate_steps: Vec<MergeGateStep>,
    #[serde(default)]
    pub fixes: Option<String>,
    /// The step its gate is running RIGHT NOW - `Some` only while the row is `gating`, and the one
    /// thing that answers *is it stuck?*: which step of how many, since when, and the end of what it
    /// has written so far.
    #[serde(default)]
    pub running_gate: Option<QueueRunningGate>,
    pub created_ms: u64,
    pub state_ms: u64,
}

/// Who serves a repository's queues - and the heartbeat a person reads liveness from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueLeaseView {
    pub repository: String,
    pub owner: String,
    pub pid: u32,
    pub host: String,
    pub build: String,
    pub heartbeat_ms: u64,
    pub acquired_ms: u64,
}

/// A target branch's health - `gating`, `green`, `red`, `timed_out` or `blocked`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetHealthView {
    pub repository: String,
    pub target: String,
    pub sha: String,
    pub state: String,
    pub checked_ms: u64,
    pub evidence: String,
    #[serde(default)]
    pub gate_steps: Vec<MergeGateStep>,
    /// The step the target's own gate is running, while it is `gating` - see
    /// [`QueueMerge::running_gate`].
    #[serde(default)]
    pub running_gate: Option<QueueRunningGate>,
}

/// The step a gate is running now: what a `gating` row's detail reads to answer *is it stuck?*
/// without opening the gate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueRunningGate {
    /// The step's command, as the project declared it.
    pub command: String,
    /// Which step, 1-based - the `2` of `2 of 4`.
    #[serde(default)]
    pub step: usize,
    /// How many steps the gate has.
    #[serde(default)]
    pub of: usize,
    /// When the step started, Unix ms - the clock the pane's `running 6m` is measured from.
    #[serde(default)]
    pub started_ms: u64,
    /// The end of the step's output so far: at most 40 lines of the last 4 KiB it has written.
    #[serde(default)]
    pub tail: String,
    /// Every byte the step has written - a number that only grows while it works, so a reader can
    /// tell *the tail has not moved* from *the step is quiet between two lines*.
    #[serde(default)]
    pub log_bytes: u64,
}

/// One move of one row - the transition history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueMove {
    /// `review`, `merge`, `target` or `lease`.
    pub queue: String,
    pub entry_id: String,
    pub repository: String,
    #[serde(default)]
    pub from: Option<String>,
    pub to: String,
    pub at_ms: u64,
    pub by: String,
    pub why: String,
}

/// One step of a gate, on the wire - a copy of `letibot_tokencore::store::MergeGateStep`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeGateStep {
    pub command: String,
    pub outcome: MergeGateOutcome,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub started_ms: u64,
    #[serde(default)]
    pub elapsed_ms: u64,
}

/// What became of one gate step - a copy of the store's `MergeGateOutcome`. `timed_out` is a step
/// past the gate's deadline and `killed` is one cut off because the daemon stopped or lost its
/// lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeGateOutcome {
    Passed,
    Failed,
    NotRun,
    NoGate,
    TimedOut,
    Killed,
}

/// Why generation stopped. Shaped to match `letibot_turn::FinishReason` exactly, `Other` and its
/// string included, so a head never has to guess at a reason this build does not know.
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

/// What one turn cost in tokens. `cached_tokens` is prompt tokens reused, not slot occupancy,
/// which is a different number the KV cache would report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub predicted_tokens: u64,
    /// What this turn cost, in micro-USD. `None` on the local server, which is free, and on a
    /// metered model nothing prices, because unpriced is not free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros_usd: Option<u64>,
}

impl Usage {
    /// The fraction of THIS prompt that did not have to be prefilled. Deliberately not called
    /// `f_keep`: the prefix cache's hit rate and the slot's occupancy are different facts.
    pub fn f_sim(&self) -> Option<f64> {
        if self.prompt_tokens == 0 {
            None
        } else {
            Some(self.cached_tokens as f64 / self.prompt_tokens as f64)
        }
    }
}

/// One turn's timings: prefill, generation, and wall clock.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Timings {
    pub prompt_ms: f64,
    pub predicted_ms: f64,
    pub wall_ms: u64,
}

/// The model's verdict on a permission, carried to the person answering it: the wire twin of
/// the tools crate's adjudication vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAdvice {
    /// Whether an oracle was actually consulted. `false` covers every verdict layer A produced
    /// without asking a model, which must not be counted as a model's answer.
    #[serde(default)]
    pub consulted: bool,
    /// `admit`, `refuse`, `ask` or `unavailable` - what the model's answer would have done on its
    /// own.
    pub would: String,
    /// Which oracle, in its own words.
    pub by: String,
    /// Why, in one or two sentences. Shown to the person; never parsed.
    pub basis: String,
    /// Which of the operator's own utterances it relied on. Empty is loud: an authorisation that
    /// cites nothing is one the oracle could not ground, and a head should render the emptiness
    /// rather than the absence of a list.
    #[serde(default)]
    pub cites: Vec<String>,
    /// Whether the answer was a NON-answer, and which kind.
    ///
    /// ```text
    ///     the model answered UNSURE                     UnsureKind::CouldNotDecide
    ///     two scores landed between the thresholds      UnsureKind::BetweenThresholds
    ///     the bytes are not a verdict at all            UnsureKind::Unreadable
    ///     the generation stopped at its ceiling         UnsureKind::OutOfRoom
    ///     the model found nothing that authorises it    OracleAnswer::NotAuthorised
    /// ```
    ///
    /// Their remedies differ, and a token rather than prose because parsing `basis` back into a
    /// label is how a corpus rots. Defaulted, so no version bump.
    #[serde(default)]
    pub unsure: Option<String>,
    /// How long it took, in milliseconds. A verdict that spent its whole budget is a different fact
    /// from one that came back in 40 ms.
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
    /// This class, for the rest of this session: the widest an answer goes, because anything
    /// standing beyond one session is a mode and not a grant.
    AllowSession,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// What happens when nobody answers. Ours, not ACP's - ACP has no timeout at the protocol layer at
/// all.
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
    /// `human`, `policy`, `boundary`, `model`, `timeout` - the three adjudicators plus the two that
    /// are not adjudicators.
    pub kind: String,
    /// The identity, where there is one. A head id, a policy rule name, "".
    pub identity: String,
}

/// How many bytes of a tool call's arguments may travel to a head: a wire-safety limit, not a
/// display decision, so a huge argument cannot stall a head's socket.
pub const TARGET_MAX_BYTES: usize = 2048;

/// The keys that name a call's subject, in preference order. Key names and not tools, so the
/// rule stays derived from the arguments rather than a table of tools.
const SUBJECT_KEYS: [&str; 3] = ["path", "file_path", "file"];

/// The keys that carry a file's new body, which never belong in a label beside a named file: a
/// body under the cap is still a body.
const BODY_KEYS: [&str; 3] = ["content", "old_string", "new_string"];

/// The display target for a tool call: the one argument a person reads. Derived and not
/// tool-supplied, because a tool that named its own label could hide what it acted on.
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
    // The placeholders of the nested values the walk passed, kept apart so they can be dropped if
    // any scalar turns up. In written order, so the fallback reads like the model's arguments.
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

/// Cut to the cap on a character boundary, and say that it was cut: a head shows the marker, so
/// a person knows the label is short of the argument.
fn truncate_target(s: &str) -> String {
    let s = letibot_transcript::sanitize::without_control(s);
    if s.len() <= TARGET_MAX_BYTES {
        return s;
    }
    // The ellipsis counts. `...` is three bytes here, and a cap that forgets that is a cap the
    // output is allowed to exceed - the class of off-by-a-few that puts a status line one column
    // past the terminal and scrolls the frame.
    const ELLIPSIS: &str = "…";
    format!(
        "{}{ELLIPSIS}",
        letibot_transcript::text::bytes_head(&s, TARGET_MAX_BYTES - ELLIPSIS.len())
    )
}

/// One section of a compaction's record, as the daemon read it back. `name` is one of the
/// template's headings, spelled as the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionSection {
    pub name: String,
    /// The text between this heading and the next, trimmed. Empty when the model wrote the heading
    /// and nothing under it.
    pub body: String,
}

/// One exchange carried through a compaction verbatim rather than summarised. `role` is the
/// vocabulary of the template, not of a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionTurn {
    pub role: String,
    pub text: String,
}

/// What was kept verbatim, and why not more: present on every compaction, including a local one,
/// where it holds zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionTail {
    pub turns: Vec<CompactionTurn>,
    /// How many items were carried. Items, not turns: the tail is made of transcript items, and a
    /// turn is however many of them happened between two operator messages. `turns.len()` is the
    /// count of turns and is a different number on purpose.
    pub carried: u64,
    /// Why this much and no more: a local model is bounded by the KV cache in VRAM, where a tail
    /// competes with the space a turn needs to answer.
    pub because: String,
    /// Items of the newest exchange left out of the front of the tail, when the tail could not
    /// start at an exchange boundary.
    #[serde(default)]
    pub dropped: u64,
}

impl CompactionTail {
    /// Why this compaction's tail is what it is, in one sentence, or `None` when there is nothing
    /// to explain: a tail that lost nothing needs no account.
    pub fn why_line(&self) -> Option<String> {
        match self.because.as_str() {
            // The ruled split, and the reason has to be in the sentence: a reader who does not know
            // the rule reads this as the head having lost their history.
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

/// A compaction's account of itself, beside its sentence rather than inside it: the `Warning`
/// that announces a compaction stays one line, and this carries the numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionReport {
    /// Which kind of compaction this was, in the vocabulary the warning code already uses:
    /// `compacted`, `reseated`, `overrun`. Carried here as well as in `Warning::code` so a head
    /// holding only the report can still name it.
    pub kind: String,
    /// The conversation's size before, in LEDGER tokens - the daemon's own count, the same number
    /// the detail sentence was rendered from.
    pub tokens_before: u64,
    /// And after: the new base, which is the prefix plus the record plus any tail.
    pub tokens_after: u64,
    /// The transcript the fork opened, e.g. `s-123#t2`.
    pub transcript: String,
    /// `tokens_before` plus the prompt, as the daemon measured it when it decided - the number that
    /// was compared against `window`.
    pub resident: u64,
    /// The window it was compared against.
    pub window: u64,
    /// The reserve that comparison subtracted, so a head can reproduce the decision rather than only
    /// read its outcome.
    pub headroom: u64,
    /// The record ran out of room before it finished. A bool on the structure and not a phrase parsed
    /// out of a body, because a templated record is exactly where *truncated* stops being readable
    /// out of any one section: the cut falls wherever the model ran out.
    pub cut_off: bool,
    /// Which wording of the instruction produced this record, the way a corpus row carries a brief
    /// hash. Bump it when `letibot_turn::SUMMARY_INSTRUCTION` changes what it asks for: records under
    /// different templates are two datasets.
    pub template: String,
    /// The headings the daemon found, in `letibot_turn::SUMMARY_SECTIONS`'s order and spelling. A
    /// heading it did not find is absent - see [`CompactionSection`].
    pub sections: Vec<CompactionSection>,
    pub tail: CompactionTail,
}

/// The template tag a compaction report carries: a date, bumped by hand when the summary
/// instruction changes, so a report can be read against the template that produced it.
pub const COMPACTION_TEMPLATE: &str = "compaction/2026-09-23";

/// The sections a compaction's record has, from the daemon that asks for them: a settings row
/// on the existing key, so a head can draw what it did not witness.
pub const COMPACTION_SECTIONS_KEY: &str = "compaction.sections";

/// The fill a session created from the store announces, and the one its restore completes:
/// `Filling` is ephemeral, so a head that arrives late is told the state separately.
pub const RESTORING_ROWS: &str = "restoring the stored conversation";

/// A subagent's call, named, on a card its own session cannot draw: a child has no head
/// attached, so the card is raised where a person can see it.
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

/// Which of the two facts a prompt card is raised on: the reading, or the absence of one.
/// `Blocked` is a program waiting; the absence is a program that may be waiting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptReading {
    /// The signal was read: a process of the run is blocked reading the input this daemon holds. The
    /// card may say the command is asking, and the request's `question` is the program's own last
    /// line, to show.
    #[default]
    Blocked,
    /// The signal could not be read - a process of the run belongs to another uid, or a `/proc` this
    /// daemon may not open. The card may NOT say the command is asking: its text is the honest
    /// sentence, and `question` is `None` because there is no reading to quote from.
    Unreadable,
}

/// One cited range - what [`SessionEvent::Citations`] carries for each empty fence labelled
/// `path:start-end` (or `path:line`) in a reply, in the order they appear.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    /// The fence's label as the model wrote it - what a head matches the fence by.
    pub label: String,
    /// The path as cited, relative to the session's workspace or absolute.
    pub path: String,
    /// The first and last line, 1-based and inclusive, as cited.
    pub start: u32,
    pub end: u32,
    /// The lines themselves, `start..=end` (fewer when the file is shorter; see `error`).
    pub lines: Vec<String>,
    /// Why the lines are not there, or not all there: the file could not be read, the range starts
    /// past its end. `None` when every cited line was read.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // events are built once and matched by every head and sink; boxing payloads would touch them all
pub enum SessionEvent {
    TurnStarted {
        turn_id: String,
        model: String,
        ledger_head: String,
        /// The WHOLE turn's start, Unix ms, or `None` when nobody measured it. A `TurnStarted`
        /// is emitted once per round, so the first of a turn is the turn's own start.
        began_ms: Option<u64>,
    },
    /// Prefill progress. Nothing downstream reads the numbers, but it counts as liveness - and it is
    /// the archetype of an *interactive* frame, which a late head is never replayed: see
    /// [`crate::scrub`].
    PromptProgress {
        turn_id: String,
        #[serde(flatten)]
        progress: PromptProgress,
    },
    /// The server's own generation counter, one per frame that advanced it: the generation half
    /// of liveness, paired with the sequence a head has already read.
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
        /// The one argument a person reads: a path, a pattern, a command line. Without it a head
        /// watching a call could only show the tool's name.
        #[serde(default)]
        target: String,
    },
    DecisionRequested {
        req_id: String,
        /// Another session's call, when this card is not this session's own. `Some` for exactly
        /// one case: a subagent's call, shown on its parent's card.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentAsk>,
        /// `permission` or `question`: a permission and a question are one mechanism, differing in
        /// `kind`, and the two payload fields below are that difference made concrete rather than
        /// left to a head to infer.
        kind: String,
        call_id: Option<String>,
        /// The tool's declared access, from its schema: read, write, exec or network. Empty from a
        /// daemon older than the field.
        #[serde(default)]
        access: String,
        summary: String,
        /// The one thing being decided about, on its own: the command a `bash` call would run, the
        /// path a write would take.
        #[serde(default)]
        target: String,
        /// What this action would WRITE, by path. The classifier finds these from the call itself,
        /// so a grant can name a file rather than a shape.
        #[serde(default)]
        write_targets: Vec<WriteTarget>,
        /// Layer A's deterministic reading: verdict, intents, regions, tier. Evidence, not the
        /// question: it belongs under the card rather than on it.
        #[serde(default)]
        detail: String,
        /// The adjudication ladder, for a permission: allow once, allow for the session, allow for
        /// the project, deny. Empty for a question, which has no ladder - it has choices.
        options: Vec<DecisionOption>,
        /// Why a rung a person would look for is not on the ladder, in one line, or empty when the
        /// ladder is complete: an option that is missing without a reason reads as a bug.
        #[serde(default)]
        options_note: String,
        /// The model's own options, for a question: plain text in the person's vocabulary, not a
        /// policy vocabulary.
        #[serde(default)]
        choices: Vec<String>,
        /// Why the model is stuck, in one line, for a question: shown to the person and never used
        /// to derive an answer.
        #[serde(default)]
        because: String,
        /// What the model already said about this, at `/mode supervised`: a head renders it above
        /// the options so the person sees it before choosing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        advice: Option<ModelAdvice>,
        /// Unix millis. `None` means *wait forever*, which is a policy a human head may choose and an
        /// automated one may not.
        deadline: Option<u64>,
        on_timeout: OnTimeout,
    },
    DecisionAnswered {
        req_id: String,
        outcome: DecisionOutcome,
        by: Decider,
        basis: String,
        /// The answer arrived after the deadline had already settled it. Recorded rather than
        /// dropped: "an answer that arrived too late" and "no answer" are different facts.
        late: bool,
    },
    /// A tool call starting. `turn_id` because every other turn-scoped event carries one and a head
    /// cannot attribute a call without it once subagents run concurrently; `access` because the
    /// schema declares it and it explains why a call did or did not stop for a decision.
    ToolStarted {
        turn_id: String,
        call_id: String,
        name: String,
        /// `read | write | exec | network`, from the tool's schema.
        access: String,
    },
    /// Partial tool output, which a late head must never be replayed: see [`crate::scrub`].
    /// `note` is free text the tool wrote.
    ToolProgress {
        turn_id: String,
        call_id: String,
        note: String,
    },
    /// A tool call ending. `bytes` alone cannot answer the question a spilling runtime raises - how
    /// much the model got versus how much there was - and the locator has to reach a head or "there
    /// is more" is a dead end.
    ToolFinished {
        turn_id: String,
        call_id: String,
        outcome: letibot_transcript::ToolOutcome,
        /// A digest, for the same reason `ToolCallProposed` carries one: the payload is in the
        /// transcript, and an event fans out to every head.
        payload_digest: String,
        /// What the model received.
        inline_bytes: u64,
        /// What the tool produced. Larger than `inline_bytes` exactly when the output spilled.
        full_bytes: u64,
        /// The spill locator, when there is one: a head can offer the rest.
        spill: Option<String>,
        /// How many repairs the call needed. A head that cannot see this cannot see a model steadily
        /// emitting malformed calls.
        repairs: u32,
        /// Both sides of the file a file-editing call changed, bounded to the region that differs:
        /// the one payload an event can carry a diff in.
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
    /// The turn ended in a failure. Without it a failed turn published a warning and no terminal
    /// event, so a head drew a turn that never ended.
    TurnFailed {
        turn_id: String,
        /// What went wrong, in the words the daemon has. Never abbreviated: this is the only place
        /// the reason exists once the process moves on.
        error: String,
        /// Whether anything the model wrote before the failure was committed. False for every engine
        /// failure - it commits nothing - and the field is present so a future failure that does keep
        /// a partial cannot arrive looking like one that does not.
        partial_kept: bool,
    },
    TranscriptAppended {
        item_id: String,
        kind: String,
        ledger_head: String,
    },
    /// A row's body, published after its announcement: `TranscriptAppended` announces a row and
    /// carries no body, so this is what a head draws from.
    TranscriptContent {
        item_id: String,
        item: Box<letibot_transcript::TranscriptItem>,
    },
    /// The lines a reply cited, as the file had them when the reply landed. A model citing code
    /// writes an empty fence labelled `path:start-end`, and this is the text that goes in it.
    Citations {
        item_id: String,
        citations: Vec<Citation>,
    },
    /// The transcript this session speaks is not the one the rows a reader holds belong to. A fork
    /// replaces the conversation, so a head must drop what it drew and start from the new base.
    TranscriptForked {
        /// The transcript that replaced it - the one every row that follows is numbered in.
        transcript_id: String,
        /// The transcript it replaced: the one the rows a reader is holding belong to. Carried for
        /// the log, which is the durable record of where a session's conversation went, rather than
        /// for a reader that acts on it.
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
    /// A post-flight assertion or guard that failed, in words a person reads. `compaction` is the
    /// structure behind it when the warning was a compaction's.
    Warning {
        code: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compaction: Option<Box<CompactionReport>>,
    },
    /// A tool asked what the operator is looking at. Every attached head that can render answers
    /// with the pane it is showing.
    ScreenRequested { req_id: String },
    /// `sudo` inside a session's command wants a password: the head shows the command and sudo's
    /// prompt, takes the password and answers down the same connection.
    SecretRequested {
        req_id: String,
        prompt: String,
        command: String,
        /// Unix millis; after it the helper gives up and `sudo` fails.
        deadline: u64,
    },
    /// A run of the operator's own is waiting for an answer. The `!` line is the one run whose
    /// input the daemon cannot supply, so a person is asked and the answer goes to the program.
    PromptRequested {
        /// The handle [`crate::protocol::ClientFrame::PromptAnswer`] comes back under.
        req_id: String,
        /// The job's handle, as `job_list` spells it. For the record and for a head that wants to name
        /// it; the answer does not travel by it, so a stale card cannot address a later command.
        job: String,
        /// The operator's own command, verbatim, as they typed it after the `!`. The command and
        /// not a program name, because that is what the card must show.
        command: String,
        /// The last line the program wrote, for the card to SHOW. `None` when it has written
        /// nothing at all, which is a reason to ask rather than a reason to wait.
        question: Option<String>,
        /// Which of the two facts the card is raised on: the reading, or the absence of one. See
        /// [`PromptReading`].
        #[serde(default)]
        reading: PromptReading,
    },
    /// Whether a line was sent to the waiting run, and by whom: the record, without the line.
    /// The line itself is never recorded, because a password is a line.
    PromptSettled {
        req_id: String,
        sent: bool,
        by: String,
    },
    /// Whether a password was given for `req_id`, and by which head - the record, without the secret.
    SecretSettled {
        req_id: String,
        given: bool,
        by: String,
    },
    /// The turn's plan. `plan` is untyped until `ExplainPlan` has a field list; typing it here would
    /// invent that contract.
    Explain {
        turn_id: String,
        plan: serde_json::Value,
    },

    /// A head's command on the session, announced so both heads see it and who issued it.
    /// Nothing else in this enum is a person's act on the session itself.
    CommandIssued {
        head_id: String,
        identity: String,
        /// `prompt`, `interrupt`, `answer`.
        command: String,
        client_request_id: String,
        /// What the daemon did with it, in the words the issuing head was given.
        note: String,
    },

    /// The session's title changed. A title reaches a head in `SessionBrief`, which travels only
    /// in the snapshot, so this is how a live head learns of a rename.
    SessionRenamed {
        /// Empty means the name was cleared, and a head goes back to showing the id.
        /// Not `Option`: "renamed to nothing" and "no rename" must not be one shape.
        title: String,
    },

    /// The session's todo list, as the model last wrote it: the whole list every time and not a
    /// delta, because a delta the model got wrong would leave a board nobody can repair.
    TodosUpdated {
        /// The list, in the order the model wrote it. Empty clears the pane's first section; that is a
        /// real state, not a missing one.
        todos: Vec<TodoEntry>,
    },

    /// A refusal, delivered the moment it is decided. The classifier denies, the operator is
    /// never asked, and the model is told before it tries a variant.
    DenialRaised {
        /// The adjudication request id. What an operator grants by name.
        request_id: String,
        turn_id: String,
        call_id: String,
        tool: String,
        /// The one argument a person reads: a path, a pattern, a command line.
        summary: String,
        /// Layer A's deterministic one-line reading of the call.
        baseline: String,
        /// Who decided: `boundary:host`, `model:...`, `breaker`, or `none`.
        by: String,
        basis: String,
        /// `blocked`, `always_ask`, `adjudicable`, `auto`.
        tier: String,
        /// `denied` (somebody decided) or `not_run` (nobody did). Never collapsed: `not_run` is not a
        /// denial, and it is not permission either.
        outcome: String,
        /// How many consecutive refusals this task direction has had, this one counted. A head seeing
        /// more than one has what it needs to collapse them into one row.
        repeat_count: u32,
        /// The consecutive-denial circuit breaker is open for this direction: no adjudicator will be
        /// consulted again in this session, and only the operator can lift it.
        breaker_open: bool,
        /// What the operator can do about it right now. A refusal whose grant path arrives after the
        /// task has died is a refusal that can only be routed around, with an extra step.
        grant: String,
    },
    /// A subagent this session spawned changed state. Carried on the PARENT's hub, so a head
    /// attached to the parent sees its children without attaching to them.
    Subagent {
        /// The subagent's own session id. Named `subagent_id` rather than `session_id` because
        /// [`Envelope`] already carries the parent's `session_id`, and a flattened duplicate field
        /// would fail to parse.
        subagent_id: String,
        state: String,
        /// The subtask's first line on the opening states, and the child's answer's first line on
        /// the finish: one field, because a card draws one line.
        prompt: String,
        role: String,
        /// The subtask this child was asked to do, in full, and the same string on every state.
        /// Carried whole and never truncated, because it is what the child was told.
        #[serde(default)]
        task: String,
        /// The model this child runs on, as `PROVIDER/MODEL` or `local`. Empty when the child
        /// inherited its parent's.
        #[serde(default)]
        model: String,
        /// The child's answer's first line, on the finishing state and nowhere else: `None` while
        /// the child is still opening or working.
        #[serde(default)]
        answer: Option<String>,
        /// The child's name - its slug - the same on every state: the one `task_start` cut its
        /// worktree's branch `agent/<slug>` from, or the one `task` derived from its prompt. A head
        /// draws the child by it where it drew the session id. Empty from an older daemon.
        #[serde(default)]
        slug: String,
    },

    /// The daemon has admitted an operator's own call, and the head may run it now: the answer to
    /// the door's admission, carrying the call id the result must quote.
    OperatorCallAllowed {
        /// The head's own handle for the call - the key its
        /// [`crate::protocol::ClientFrame::OperatorResult`] comes back under.
        call_id: String,
        /// One of [`crate::protocol::HEAD_RUN_TOOLS`]. Carried so every head can draw the call without
        /// asking the one that made it.
        name: String,
        /// Who is running it.
        who: String,
        /// The arguments, verbatim, so a second head draws the same call.
        arguments: String,
    },
    /// A background job this session started has stopped running. The start needs no event: a
    /// backgrounded `bash` call is the job starting, and its finishing is the news.
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
    /// A row of the review queue or the merge queue moved: the queue, the row, the state it moved to
    /// and why. The pane re-reads the snapshot on it; the log keeps the sentence. Broadcast to every
    /// session's log, because the queues are daemon-level and a pane may be open on any session.
    QueueMoved {
        /// `review`, `merge` or `target`.
        queue: String,
        id: String,
        to: String,
        why: String,
    },
    /// A window of one background job's output, for a pane that draws it. An event rather than a
    /// reply, because the pane asks again and again as the job runs.
    JobOutput {
        /// The job's handle, as the pane lists it.
        job: String,
        /// The offset this window actually starts at - the request is clamped, not refused.
        from: u64,
        /// One past the window's last byte.
        to: u64,
        /// Everything the job has written, all streams together.
        produced: u64,
        /// Bytes that fell off the ring before this window: a head must be able to say "there was more
        /// and it is gone" rather than showing a window whose start looks like the job's start.
        dropped: u64,
        /// The job's own word - `exited 0`, `running`, `killed by ...`.
        state: String,
        /// Whether anything was ever executed for this job. `false` on every ending of a process
        /// that ran, including one killed before it wrote anything.
        #[serde(default)]
        never_ran: bool,
        /// The window, split into lines by the daemon so two heads cannot disagree about where a line
        /// ends.
        lines: Vec<String>,
        /// Where to ask next when there is more that is still readable, and `None` when the end is
        /// here. The daemon decides rather than the head computing `to`, because only the daemon knows
        /// how much of the ring survives.
        next: Option<u64>,
    },
    /// A named operation the daemon is filling rows for, with its own counter. Four things look
    /// identical from a head, and this is what tells them apart.
    Filling {
        /// The operation, in the daemon's own words - `importing an opencode conversation`,
        /// `carrying the conversation onto the new prompt`.
        what: String,
        /// What `done` and `total` count: `parts`, `rows`.
        unit: String,
        /// How much of it is done.
        done: u64,
        /// How much there is, known before the first row where it can be.
        total: u64,
    },
    /// A fold's long wait, named as the compaction's own: the overrun compaction summarises a
    /// scratch transcript, so a head that drew it as the session's would draw the wrong one.
    CompactionProgress {
        /// Which half is running, 1-based, in the order they RUN - the recent tail first on the local
        /// plan, since its cold prompt is the cheap one to warm the server's cache with.
        half: u64,
        /// How many halves: 1 for the cloud fold, 2 for the local two-half plan.
        halves: u64,
        /// The half's own prompt, in the box's ledger tokens.
        prompt_tokens: u64,
        /// How much of it the server has read. `0` on a messages transport, which reports no prefill -
        /// a real answer, not a missing one.
        processed: u64,
        /// What the half has produced so far.
        written: u64,
        /// What `written` counts: `tokens` where the transport reports the server's own count, `chars`
        /// where it reports only text. Named, because one name for both would be a lie about one.
        unit: String,
    },
    /// What this session is holding back from its model right now: the results nothing in the
    /// plan waits on, held so a turn that does not need them is not spent.
    HeldResults {
        /// Background jobs (`bash` background, a promoted call) held.
        jobs: u64,
        /// Subagents (`task`) whose ending is held.
        subagents: u64,
        /// How many of those, of either kind, FAILED - a non-zero exit, a signal, a child that answered
        /// `failed`.
        failed: u64,
    },
    /// A plugin pane's view: the document a `pane_<name>` hook answered, published so every head
    /// draws the same dashboard.
    PluginPane {
        /// Which pane, as `<layer name>/<hook name>`: `plan-dashboard/dashboard`. The layer's name is
        /// the manifest's, so two layers cannot collide, and the hook's keeps one layer's two panes
        /// apart.
        pane: String,
        /// The C3 `View`, as the plugin wrote it.
        view: serde_json::Value,
    },
    /// That pane is gone: its layer was disabled, removed or hot-reloaded into a failure, or the
    /// session it was scoped to has closed.
    PluginPaneGone {
        /// See [`SessionEvent::PluginPane::pane`].
        pane: String,
    },
    /// What this session is waiting on, as the daemon computed it: the daemon's own answer to
    /// whether this session will act again without being asked.
    ActivityChanged {
        /// Quiet, or the list of what it waits on - [`crate::activity::Activity`] carries the definition
        /// of which entries mean *this session moves on its own* and which one means *a person must*.
        activity: crate::activity::Activity,
    },
}

impl SessionEvent {
    /// Could publishing this have moved what the session is waiting on? The set `Hub::publish`
    /// re-derives on, so an event that cannot move it costs nothing.
    pub fn touches_activity(&self) -> bool {
        matches!(
            self,
            SessionEvent::TurnStarted { .. }
                | SessionEvent::TurnFinished { .. }
                | SessionEvent::TurnInterrupted { .. }
                | SessionEvent::TurnFailed { .. }
                | SessionEvent::ToolCallProposed { .. }
                | SessionEvent::ToolFinished { .. }
                | SessionEvent::DecisionRequested { .. }
                | SessionEvent::DecisionAnswered { .. }
                | SessionEvent::JobSettled { .. }
                | SessionEvent::Subagent { .. }
                | SessionEvent::HeldResults { .. }
                // The board, because it is what says a job is awaited. A job counts once a row `needs`
                // it, and the model usually backgrounds the job FIRST and writes the row after: without
                // this the session read quiet from the job's start until something else moved.
                | SessionEvent::TodosUpdated { .. }
                // A warning, because it is a verb's answer: a command the worker took off the queue is
                // `Delivery` until something says otherwise, and every command that runs no turn ends by
                // saying why on the log.
                | SessionEvent::Warning { .. }
        )
    }

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
            SessionEvent::Citations { .. } => "Citations",
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
            SessionEvent::QueueMoved { .. } => "QueueMoved",
            SessionEvent::OperatorCallAllowed { .. } => "OperatorCallAllowed",
            SessionEvent::JobOutput { .. } => "JobOutput",
            SessionEvent::Filling { .. } => "Filling",
            SessionEvent::CompactionProgress { .. } => "CompactionProgress",
            SessionEvent::HeldResults { .. } => "HeldResults",
            SessionEvent::PluginPane { .. } => "PluginPane",
            SessionEvent::PluginPaneGone { .. } => "PluginPaneGone",
            SessionEvent::ActivityChanged { .. } => "ActivityChanged",
        }
    }
}

/// An event with its `(session_id, seq, ts)`. `seq` is monotonic and gap-free per session, and
/// it is the only thing a reattaching head can resume from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub session_id: String,
    pub seq: u64,
    /// Unix millis.
    pub ts: u64,
    #[serde(flatten)]
    pub event: SessionEvent,
}

/// Unix millis now, from the tree's one clock: re-exported because this path is what the head and the
/// daemon already call. Fails backwards to 0 rather than panicking: a clock before the epoch is a
/// broken clock, not a reason to lose a session.
pub use letibot_metrics::now_ms;

#[cfg(test)]
mod tests {
    /// A board change recomputes the activity: a job counts once a row needs it, and the row is
    /// usually written after the job starts.
    #[test]
    fn a_todo_board_change_touches_the_activity() {
        assert!(SessionEvent::TodosUpdated { todos: Vec::new() }.touches_activity());
    }

    use super::*;

    /// The wire's title rule is the STORE's title rule: the test that makes a copy safe. The enum
    /// copies get their bound from the store's own.
    #[test]
    fn the_wires_title_rule_is_the_stores() {
        use letibot_tokencore::store;
        assert_eq!(TODO_TITLE_MAX, store::TODO_TITLE_MAX, "one cap, not two");
        assert_eq!(
            TODO_DETAIL_SEP,
            store::TODO_DETAIL_SEP,
            "one separator, or the two doors would split a row in two places"
        );
        for row in [
            "plain row",
            "fix the test — it fails when the store is busy",
            "  padded  ",
            "a — b — c",
            "",
            " — leading separator",
        ] {
            assert_eq!(
                todo_title(row),
                store::todo_title(row),
                "the same row's title, whichever copy is asked: {row:?}"
            );
        }
    }

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

    /// The `reading` a prompt card is raised on is safe in both directions, which is the test for
    /// whether a protocol bump is needed at all.
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

        // A log recorded before the field existed has no `edit` key. It must parse, and read as
        // None - the head then renders exactly what it always rendered.
        let old = r#"{"event":"tool_finished","turn_id":"t1","call_id":"c1",
            "outcome":{"outcome":"ok"},"payload_digest":"fnv1a:1",
            "inline_bytes":12,"full_bytes":12,"repairs":0}"#;
        let back: SessionEvent = serde_json::from_str(old).unwrap();
        assert!(matches!(
            back,
            SessionEvent::ToolFinished { edit: None, .. }
        ));
    }

    /// The flatten that puts an event inside an [`Envelope`] is where a field-name collision shows
    /// up, and not in the event itself.
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

    /// An edit's row is the file, and the `edits` array says nothing: the placeholder sat in the most
    /// valuable position a head has.
    #[test]
    fn a_nested_argument_is_elided_only_when_nothing_else_can_be_shown() {
        // The batch edit, in the order the model writes it (`edits` first). The row is the
        // file: no placeholder before it, and none after it either.
        assert_eq!(
            display_target(r#"{"edits":[{"old":"x"}],"path":"letibot/src/answers.rs"}"#),
            "letibot/src/answers.rs"
        );
        // The other order: the placeholder must not appear before the file either.
        assert_eq!(
            display_target(r#"{"path":"a.rs","edits":[{"old":"x"}]}"#),
            "a.rs"
        );
        // An object nested beside a scalar: the scalar is the label.
        assert_eq!(
            display_target(r#"{"cmd":"cargo test","env":{"RUST_LOG":"x"}}"#),
            "\"cargo test\""
        );

        // And the rule keeps its other half. A map with nothing scalar in it still says there is more
        // rather than rendering an empty label; in this tree that is reachable only by a tool added
        // later, because every seated one carries a scalar `path`, `pattern` or `cmd`.
        assert_eq!(display_target(r#"{"rows":[1,2,3]}"#), "[…]");
        assert_eq!(display_target(r#"{"nested":{"a":1}}"#), "{…}");
        assert_eq!(display_target(r#"{"rows":[1],"opts":{"a":1}}"#), "[…] {…}");
        // An argument object with nothing in it at all is the empty label, which is what
        // makes the head fall back to the call id instead of drawing a blank row.
        assert_eq!(display_target("{}"), "");
    }

    #[test]
    fn the_subject_the_written_order_left_past_the_budget_comes_back_in_front() {
        // A model composing an edit writes the content before the file it is going into, and the
        // budget breaks before `path` does: the label has to survive that order.
        let args = format!(
            r#"{{"old_string":"{}","new_string":"{}","path":"crates/tui/src/app.rs"}}"#,
            "x".repeat(TARGET_MAX_BYTES + 100),
            "y".repeat(TARGET_MAX_BYTES + 100),
        );
        let t = display_target(&args);
        assert!(t.starts_with("crates/tui/src/app.rs"), "{t}");
        // And the body is not in the label at all - under the cap or over it.
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
        // An order that already shows every argument - grep's pattern, then its path - does not move.
        assert_eq!(
            display_target(r#"{"pattern":"home.*button","path":"src"}"#),
            "home.*button src"
        );
    }

    #[test]
    fn a_target_never_exceeds_the_cap_and_says_when_it_was_cut() {
        // Sized off the constant, not off a literal: a test whose premise is a number the code no
        // longer has is a green test about nothing.
        let long = "x".repeat(TARGET_MAX_BYTES + 100);
        let t = display_target(&format!(r#"{{"path":"{long}"}}"#));
        assert!(t.len() <= TARGET_MAX_BYTES, "{} bytes", t.len());
        assert!(t.ends_with('…'), "{t}");
    }

    /// A realistic long command reaches a head whole, and it is the head that cuts it: the wire
    /// limit is not a view limit.
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
        // A string with whitespace is quoted, so `grep "two words" src` cannot be misread as three
        // arguments - that is `scalar`'s rule and the quoted form is the right one.
        assert_eq!(
            t,
            format!("{cmd:?}"),
            "the wire cut a command it had no reason to touch"
        );
        assert!(!t.ends_with('…'), "and it did not claim to: {t}");
        // The whole tail survives, which is the complaint in one assertion: the end of a long command
        // is what a short cut takes.
        assert!(t.contains("--selftest"), "{t}");
    }

    #[test]
    fn a_target_never_carries_a_control_character_into_a_one_line_header() {
        // A newline here would put a row on the screen the head did not count, which scrolls the
        // frame it has just painted.
        let t = display_target("{\"cmd\":\"a\\nb\"}");
        assert!(!t.contains('\n'), "{t:?}");
    }

    #[test]
    fn arguments_that_are_not_json_still_produce_something_readable() {
        // The model wrote them; refusing to show them would hide the evidence of exactly the
        // malformed call this case is.
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
