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

/// A todo's state, on the wire. The same three words the store spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// One line of a session's todo list, on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoEntry {
    pub content: String,
    pub status: TodoStatus,
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

/// How much of a tool call's arguments may travel to a head, in bytes.
///
/// `crates/ui/DESIGN.md` §4.1 asks for *"a short, tool-supplied, already-truncated
/// display string … capped at something like 120 bytes"*. The cap is **here**,
/// where `TurnEvent` becomes `SessionEvent`, because here is where the fan-out
/// starts: below this line an argument is one in-process string, above it it is a
/// copy per attached head. A head that truncated for itself would also give two
/// heads two different renderings of one call.
pub const TARGET_MAX_BYTES: usize = 120;

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
/// Nested values are **elided, not flattened**: `{…}` and `[…]` say there is more
/// without pretending a JSON dump is a label.
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
    for (_k, val) in map {
        parts.push(match scalar(&val) {
            Some(s) => s,
            None if val.is_array() => "[…]".into(),
            None => "{…}".into(),
        });
        if parts.iter().map(|p| p.len() + 1).sum::<usize>() > TARGET_MAX_BYTES {
            break;
        }
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
/// Control characters go first: a newline inside a header would put a row on the
/// screen the head did not count, which scrolls the frame it has just painted.
/// That is the same fault `letibot_ui::width::break_cells` had, one layer up.
fn truncate_target(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
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


/// One event. `(session_id, seq, ts)` live on [`Envelope`], not here, because an
/// event that has not been appended yet has none of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    TurnStarted {
        turn_id: String,
        model: String,
        ledger_head: String,
    },
    /// Nothing surveyed reads this one, and §8.5 requires it to count as liveness.
    /// It is also the archetype of an *interactive* frame: see [`crate::scrub`].
    PromptProgress {
        turn_id: String,
        #[serde(flatten)]
        progress: PromptProgress,
    },
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
        /// `permission` or `question`. §11.6: *"a permission and a question are one
        /// mechanism, differing in `kind`"* — and the two payload fields below are
        /// that difference made concrete rather than left to a head to infer.
        kind: String,
        call_id: Option<String>,
        summary: String,
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
    Warning {
        code: String,
        detail: String,
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
        /// `inexpressible`, `always_ask`, `adjudicable`, `auto`.
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
    /// `state` is `running`, `done` or `failed`; `prompt` is the subtask's first
    /// line (the same derivation the subagent's title uses), so a head shows what
    /// the subagent was for without parsing the `task` call's arguments.
    Subagent {
        session_id: String,
        state: String,
        prompt: String,
        role: String,
    },
}

impl SessionEvent {
    /// The variant name, for counters and for a head's status line.
    pub fn kind(&self) -> &'static str {
        match self {
            SessionEvent::TurnStarted { .. } => "TurnStarted",
            SessionEvent::PromptProgress { .. } => "PromptProgress",
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
            SessionEvent::HeadAttached { .. } => "HeadAttached",
            SessionEvent::HeadDetached { .. } => "HeadDetached",
            SessionEvent::SessionRenamed { .. } => "SessionRenamed",
            SessionEvent::TodosUpdated { .. } => "TodosUpdated",
            SessionEvent::Warning { .. } => "Warning",
            SessionEvent::Explain { .. } => "Explain",
            SessionEvent::CommandIssued { .. } => "CommandIssued",
            SessionEvent::DenialRaised { .. } => "DenialRaised",
            SessionEvent::Subagent { .. } => "Subagent",
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

    #[test]
    fn a_nested_argument_is_elided_rather_than_dumped() {
        assert_eq!(
            display_target(r#"{"path":"a.rs","edits":[{"old":"x"}]}"#),
            "a.rs […]"
        );
    }

    #[test]
    fn a_target_never_exceeds_the_cap_and_says_when_it_was_cut() {
        let long = "x".repeat(400);
        let t = display_target(&format!(r#"{{"path":"{long}"}}"#));
        assert!(t.len() <= TARGET_MAX_BYTES, "{} bytes", t.len());
        assert!(t.ends_with('…'), "{t}");
    }

    #[test]
    fn a_target_never_carries_a_control_character_into_a_one_line_header() {
        // A newline here would put a row on the screen the head did not count,
        // which scrolls the frame it has just painted — the same fault
        // `letibot_ui::width::break_cells` had, one layer up.
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
        };
        assert_eq!(u.f_sim(), Some(0.9));
    }
}