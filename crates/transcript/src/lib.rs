//! The conversation record: what the harness stores, and what a dialect renders.
//!
//! This crate is deliberately tiny and has no dependency on a vocab, a model, an
//! FFI or a running server. Both the dialect crate (which renders) and the token
//! core (which tokenizes and persists) depend on it, and they must be buildable
//! independently of each other.
//!
//! See `docs/implementation-plan.md` §4.2.

use serde::{Deserialize, Serialize};

/// **Text a head did not author, made safe for a terminal** (§3.1) — here because both
/// sides of the log/wire boundary need it and neither can see the other.
pub mod media;
pub mod sanitize;

/// **Who asked for a tool call** — R24 part two, decision 1.
///
/// One variant today, and the enum exists rather than a `bool` or an `Option<String>` for the
/// reason this tree reaches for an enum on a fact that can grow: a second origin (a peer agent,
/// a flowy seat, a scheduled job) is a variant rather than a second field on a struct that
/// already has one. `SystemOrigin` is the precedent for the naming.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOrigin {
    /// A person at this console ran it. `who` is the identity the gate records in
    /// `verdict_by` (`human:dead`, `human:leticl`), so the row and its own adjudication name
    /// the actor the same way.
    Operator { who: String },
}

/// One entry in the conversation, in the order the model produced or consumed it.
///
/// **Reasoning is a sibling of `Assistant`, not a field on it.** This is the single
/// most consequential shape decision in the type, and it is borrowed from Grok
/// Build's `ConversationItem` along with its rationale:
///
/// > "The interleaved order of `[reasoning, tool_call, reasoning, …, message]`
/// > produced by the model stays byte-stable across turns. That stability is what
/// > lets the server-side prefix KV-cache hit."
///
/// Reasoning-as-a-field forces last-write-wins when one turn emits several
/// reasoning blocks around several tool calls, and loses the interleaving. GLM and
/// Qwen both interleave, so a field would be wrong for every model we target.
/// **Who a `User` row's words came from** — R42.
///
/// Two values, and they are the two the requirement names as needing to be distinguishable:
/// *the operator typed this* and *this session appended it*. The names are
/// [`letibot_tools::authorise::Speaker`]'s own — `operator` and `agent` — so a head has one
/// vocabulary for the fact wherever it meets it, and the authorisation trail's record and the
/// transcript row cannot spell the same speaker two ways.
///
/// **Why not the tools' type.** This crate is what the wire is made of and it must not depend
/// on the classifier that judges it (`letibot-tools` depends on *this*, not the reverse), so
/// the vocabulary is repeated here rather than imported. Two strings, one meaning, and both
/// endpoints of the same sentence about who spoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    /// **The person at the keyboard.** Every row written before this field existed reads as
    /// this, which is what a head already drew for them.
    #[default]
    Operator,
    /// **This session appended it** — a job completion, a salvage notice, a steering line,
    /// the intent check. Never an authorisation, and never to be drawn as the operator's
    /// words.
    Agent,
}

impl Speaker {
    pub fn as_str(self) -> &'static str {
        match self {
            Speaker::Operator => "operator",
            Speaker::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]

pub enum TranscriptItem {
    System {
        text: String,
        origin: SystemOrigin,
    },
    User {
        parts: Vec<UserPart>,
        /// **Who these words came from** — R42.
        ///
        /// Everything this session appends is a `User` item: the operator's prompt, a head's
        /// steering, §5.7's salvage notice, the intent check's *"you said you would X"*, and a
        /// job settlement (`harnessd::harness`). On the wire they were indistinguishable, so a
        /// head drew a job's completion exactly as it drew the person typing — the operator's
        /// own report: *"why job completion events arrive as my messages?"*
        ///
        /// **The daemon has known all along.** It records the same fact on its authorisation
        /// trail, with its own words for why it matters (`harness.rs`): *"Walking the
        /// transcript and calling all of them the operator's words would let the harness
        /// authorise itself, and an agent whose own text can authorise an action is the shape
        /// a prompt injection would most like to take."* That record was built for the oracle
        /// and stopped there; this carries it to the head. The provenance is known only at the
        /// moment of appending, so it is recorded there.
        ///
        /// **And the reader is the other party the gate serves.** The oracle already refuses
        /// to let agent text authorise an action; a head that draws agent text as the
        /// operator's is showing the reader the exact lie the oracle is defended against.
        ///
        /// # Absent means `Operator`, and that is the same reading `TrailMirror::seed` takes
        ///
        /// A row written before this field existed cannot say who spoke, and the honest
        /// reading available is the operator — which is what a head drew for it anyway, so
        /// nothing regresses and no version bumps. The alternative (a third *unknown* value)
        /// would put a new word on every old row for no gain: what the field is FOR is telling
        /// the two apart going forward, and *absent* is not a fact about the future.
        ///
        /// `None` and `Some(Operator)` are therefore the same rendering, while
        /// `Some(Agent)` is a different one — the distinction the requirement asks a head to
        /// be able to draw.
        #[serde(default)]
        speaker: Speaker,
    },
    Reasoning {
        text: String,
        field: ReasoningField,
        /// **The operator stopped this thought, or it ran out of room.**
        ///
        /// Not the same claim as `Assistant::truncated`, which marks an answer
        /// that was cut. This marks a DRAFT that was abandoned -- and a draft is
        /// the one thing in a transcript that nothing downstream depends on, so
        /// it is the one thing a renderer may decline to replay.
        ///
        /// Why it has to be marked at all, measured 2026-09-18: a session where
        /// the model began counting parentheses by hand produced 25464 tokens of
        /// `+ 0 + 0 + 0 + 0` before the operator stopped it. That block was
        /// committed and replayed on every later turn -- a tenth of the window,
        /// forever -- and the model, reading its own abandoned loop as history,
        /// started counting by hand again. The operator: "it counts them again by
        /// hand lol".
        ///
        /// Absent on the wire means `false`, so rows written before the field
        /// existed still read.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    Assistant {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
        /// The turn was cut short rather than finished naturally: §5.7's
        /// `length` with usable text, or §5.8's kept partial. Absent on the wire
        /// means `false`, so rows written before the field existed still read.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    ToolResult {
        call_id: String,
        name: String,
        outcome: ToolOutcome,
        payload: String,
        /// Both sides of the file a file-editing call changed, bounded to what
        /// differs — the raw material of the two-panel diff a head draws.
        ///
        /// It lives on the row and not only on the `ToolFinished` event because
        /// the row is the durable artifact: the event log is the daemon's
        /// memory, and a head that attaches after a restart is handed rows, not
        /// events. Without it the card rendered from history and the change it
        /// made did not (operator, 2026-09-18: *"it renders history, why can it
        /// render the edit card and not the diff"*). Display-only: the prompt
        /// builders read `payload` and never this, so the tokens a row renders
        /// to — and the hash chain over them — do not move. Absent on rows
        /// written before the field existed, and what lets those still load is that a
        /// **missing key for an `Option` field deserialises as `None`** — not the
        /// `#[serde(default)]` beside it, which is explicit rather than load-bearing.
        /// (Measured 2026-09-23: deleting the attribute, every test still passes.)
        #[serde(default)]
        edit: Option<ToolEditExcerpt>,
        /// **Who asked for this call** — R24 part two, decision 1.
        ///
        /// A head labels a tool card from the nearest preceding `Assistant { tool_calls }`,
        /// which by construction cannot see a call nobody proposed. So a call the OPERATOR
        /// ran — a `web_fetch` they typed themselves — would be drawn as the model's, which is
        /// a lie about who acted on the one row whose whole subject is what happened.
        ///
        /// **It cannot be inferred, which is why it is a field.** The absence of a proposing
        /// row is *also* what a row from a resumed daemon looks like, and this tree already
        /// renders that case deliberately (*"a row this head did not watch shows no
        /// duration"*) — so inferring would make the two indistinguishable, which is the
        /// defect R17 is about.
        ///
        /// **`who`, and not a unit variant.** The store already separates one person from
        /// another (`verdict_by` holds `human:dead` 121 and `human:leticl` 37), so a bare
        /// `Operator` would name the actor at a different granularity from the adjudication
        /// for the same call — and one of the two would be wrong.
        ///
        /// `None` is **a call the model proposed**, which is every row written before this
        /// field existed — hence `#[serde(default)]` and no version bump, the shape `never_ran`
        /// and `edit` on this same variant already set.
        /// No version bump, and the reason is serde's own rule rather than an attribute: a
        /// missing key for an `Option` field deserialises as `None`. The `#[serde(default)]`
        /// is kept only because the three fields around it have one.
        #[serde(default)]
        origin: Option<CallOrigin>,
        /// **Bytes this call read that are not text** — an image, today; see
        /// [`crate::ToolOutcome`]'s sibling in `letibot_tools::media` for the argument.
        ///
        /// It lives on the row for the reason `edit` does and one more: the row is the durable
        /// artifact a prompt is rebuilt FROM, and an image that reached the model once and not on
        /// the next round would be a picture the model saw and then forgot — the failure that looks
        /// like a model ignoring a picture rather than a head dropping it.
        ///
        /// **The data URI is stored inline and that is a real cost, stated here rather than
        /// discovered.** Base64 is 4/3 of the file, it rides every JSONL line and every store write,
        /// and it is re-sent on every prompt rebuild — which is exactly opencode's situation and why
        /// it strips media on compaction (`compaction.ts`, `stripMedia`). R24/R27 own what this tree
        /// does about that; this field is what makes the question askable.
        ///
        /// Absent on every row written before it existed, and on every call that read no picture.
        #[serde(default)]
        media: Option<media::Media>,
    },
    /// A zero-width delimiter. **Renders to nothing.**
    ///
    /// It exists in v1 only so that per-segment decay, user-assisted compaction and
    /// agentic memory have something to address later. If those never arrive it has
    /// cost one row and zero rendered bytes.
    SegmentMark {
        segment_id: String,
        label: String,
        kind: String,
        edge: SegmentEdge,
    },
}

impl TranscriptItem {
    /// Roughly how many bytes of text this row carries — the measure a **size bound**
    /// needs.
    ///
    /// Counts the strings a reader would see and ignores structure, ids and enums: it is
    /// a bound's input, not a serialisation, and it has to be cheap enough to call per
    /// row per append. A row whose text this cannot reach (`None` bodies, a mark) counts
    /// as zero, which is safe because it errs toward keeping the row.
    ///
    /// **One definition, and it is here rather than on either side of the wire.** The
    /// daemon bounds its view by this and the head decides whether a transcript is too
    /// big to walk by this, and two copies of "what counts as size" would drift into two
    /// different answers to one question.
    pub fn bytes(&self) -> usize {
        let parts = |parts: &[UserPart]| -> usize {
            parts
                .iter()
                .map(|p| match p {
                    UserPart::Text { text } => text.len(),
                    // An image or a file reference is a pointer, not a body: the
                    // payload is elsewhere and counting it here would bound the wrong
                    // thing.
                    UserPart::Image { .. } | UserPart::FileRef { .. } => 32,
                })
                .sum()
        };
        match self {
            TranscriptItem::System { text, .. }
            | TranscriptItem::Reasoning { text, .. }
            | TranscriptItem::Assistant { text, .. } => text.len(),
            TranscriptItem::User { parts: p, .. } => parts(p),
            TranscriptItem::ToolResult { payload, edit, .. } => {
                // The excerpt is what makes a `read` or an `edit` row large, and it is
                // carried to the head, so it counts.
                payload.len()
                    + edit
                        .as_ref()
                        .map(|e| e.before.len() + e.after.len())
                        .unwrap_or(0)
            }
            TranscriptItem::SegmentMark { label, .. } => label.len(),
        }
    }
}

/// The before/after of a file-editing call, bounded to what differs — the raw
/// material of the two-panel diff a head draws.
///
/// One definition, here, because this is the one crate both sides of the old
/// lift can see: `letibot-tools` (which builds it at the call site) and
/// `letibot-sessionlog` (which carries it on the wire) cannot name each other,
/// and two copies of a nine-field struct with a field-by-field lift between
/// them is how copies drift. The runtime and the log re-export it; the wire
/// shape is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolEditExcerpt {
    /// Relative to the session root, as the head should label it.
    pub path: String,
    /// The tool created the file: `before` is empty and a two-panel view
    /// renders the left side as nothing rather than as deleted content.
    pub created: bool,
    /// 1-based line of the old file that `before` starts at, so a gutter
    /// numbers the left panel exactly as `read` would.
    pub before_start: usize,
    /// 1-based line of the new file that `after` starts at.
    pub after_start: usize,
    /// Line counts of each **whole** file, so "… N unchanged lines" is a
    /// fact rather than a guess.
    pub before_lines: usize,
    pub after_lines: usize,
    /// The cap cut the excerpt: there is more change than this carries.
    pub truncated: bool,
    /// The excerpt lines, LF-joined, no trailing newline. Empty when the
    /// side has no lines in the range (a pure insertion has no `before`).
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemOrigin {
    /// The system prompt the session opened with; part of the stable prefix.
    Bootstrap,
    /// A later change. Appended after the cached history rather than rewriting
    /// message 0 — rewriting cost a full cold re-prefill of a 179k conversation
    /// when measured. See `SystemUpdateMode` in the dialect crate.
    Update,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentEdge {
    Open,
    Close,
}

/// Which wire field the model expects its own prior reasoning replayed into.
///
/// Getting this wrong does not error. It makes every prompt diverge from its own
/// cache entry at the first assistant turn, so the prefix cache never hits and the
/// entry count grows without bound — measured at 291 entries / 612 GB and four OOM
/// kills before the cause was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningField {
    /// GLM: replayed as `reasoning_content`.
    ReasoningContent,
    /// Qwen: replayed inside the rendered turn, gated by `preserve_thinking`.
    Inline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON text as the model emitted it. Not parsed here: the exact bytes are
    /// what must be replayed to keep the prefix stable.
    pub arguments: String,
}

/// How a command came to be running in the background rather than inline.
///
/// Three ways in and no fourth, because a fourth would be a promotion nobody
/// decided the rule for. The distinction is not bookkeeping: *"the model asked"*
/// and *"the runtime decided"* differ in whether the model already knows, and
/// *"a person decided"* differs from both in that there is somebody to attribute
/// it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "how", rename_all = "snake_case")]
pub enum Backgrounding {
    /// The model asked for it: `background: true`.
    Asked,
    /// **The runtime promoted it**, because it outlived the foreground threshold.
    /// Reactive on elapsed time, never predicted from the command text.
    Promoted,
    /// A person promoted it mid-flight from a head, and this is who.
    Operator { identity: String },
}

impl Backgrounding {
    /// The clause a result puts in front of the model. Never "backgrounded",
    /// which does not say who did it.
    pub fn phrasing(&self) -> String {
        match self {
            Backgrounding::Asked => "you asked for this to run in the background".into(),
            Backgrounding::Promoted => {
                "the RUNTIME moved this to the background — you did not ask for it".into()
            }
            Backgrounding::Operator { identity } => {
                format!("`{identity}` moved this to the background from the head")
            }
        }
    }
}

/// A **closed** vocabulary. Adding a variant is a deliberate act.
///
/// `Abstained` is not a flavour of `Ok`, and that distinction is the whole reason
/// this is an enum rather than a bool. It is the case the oracle project measured:
/// retrieval honestly reported that the corpus did not cover the question, and the
/// model wrote a confident answer on top of it anyway. A tool runtime that collapses
/// "no answer" into "success with empty payload" makes that failure invisible.
///
/// # `Backgrounded` is here for the same reason `Abstained` is
///
/// A command that was moved to the background is **still running and still
/// recoverable**, and every outcome that already existed says something false
/// about it:
///
/// | reported as | what the model concludes |
/// |---|---|
/// | `Ok` with an empty payload | the command produced nothing |
/// | `Failed` | retry — and now there are two of them |
/// | `Timeout` | it was abandoned; `Timeout` means exactly that |
///
/// So it is its own variant rather than a sentence in a payload, and it carries
/// the handle and the recovery verb, because a fact the model has to guess the
/// next action from is a fact it will guess wrong.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolOutcome {
    Ok,
    Abstained {
        reason: String,
    },
    Failed {
        reason: String,
    },
    Denied {
        req_id: String,
    },
    Timeout,
    NotRun {
        why: String,
    },
    /// **Still running.** Not finished, not failed, not abandoned.
    Backgrounded {
        /// The job id, spelled as the recovery verbs take it.
        handle: String,
        /// How long it ran in the foreground before it went to the background.
        /// The number that makes a promotion legible rather than mysterious.
        ran_for_ms: u64,
        how: Backgrounding,
        /// The call that gets its output, ready to make. "Errors carry the fix",
        /// applied to something that is not an error.
        next: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserPart {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        data_ref: String,
    },
    FileRef {
        path: String,
        sha256: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_is_a_sibling_not_a_field() {
        // The interleaving must survive a round trip, because the byte-stability of
        // that order is what makes the server's prefix cache hit.
        let turn = vec![
            TranscriptItem::Reasoning {
                text: "first".into(),
                field: ReasoningField::ReasoningContent,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"a"}"#.into(),
                }],
                truncated: false,
            },
            TranscriptItem::Reasoning {
                text: "second".into(),
                field: ReasoningField::ReasoningContent,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: "done".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ];
        let json = serde_json::to_string(&turn).unwrap();
        let back: Vec<TranscriptItem> = serde_json::from_str(&json).unwrap();
        assert_eq!(turn, back, "interleaved order must survive serialisation");
    }

    #[test]
    fn truncated_survives_a_round_trip() {
        // The flag is the transcript's own record that §5.7's kept text or §5.8's
        // kept partial did not finish naturally. If it were dropped on the way out,
        // the row would read back as a completed turn — the exact defect §5.7
        // exists to abolish.
        let item = TranscriptItem::Assistant {
            text: "the part that made it out".into(),
            tool_calls: vec![],
            truncated: true,
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"truncated\":true"), "{json}");
        let back: TranscriptItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, item);
    }

    #[test]
    fn a_row_written_before_the_field_existed_reads_as_not_truncated() {
        let old = r#"{"type":"assistant","text":"done"}"#;
        let back: TranscriptItem = serde_json::from_str(old).unwrap();
        assert_eq!(
            back,
            TranscriptItem::Assistant {
                text: "done".into(),
                tool_calls: vec![],
                truncated: false,
            }
        );
    }

    #[test]
    fn an_edit_excerpt_survives_a_round_trip_on_its_row() {
        // The row is the durable artifact: the store persists it and a resumed
        // head replays it, so the excerpt has to come back byte-equal or the
        // panels a restart draws are not the panels the edit made.
        let item = TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "edit".into(),
            outcome: ToolOutcome::Ok,
            payload: "done".into(),
            edit: Some(ToolEditExcerpt {
                path: "crates/ui/src/style.rs".into(),
                created: false,
                before_start: 3,
                after_start: 3,
                before_lines: 64,
                after_lines: 65,
                truncated: false,
                before: "fn a() {".into(),
                after: "fn a() {\n    x();".into(),
            }),
            origin: None,
            media: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: TranscriptItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, item, "the excerpt rides the row both ways");
    }

    #[test]
    fn a_row_written_before_the_edit_existed_reads_as_none() {
        // Rows the store already holds predate the field. `#[serde(default)]`
        // is what lets them still load — and what keeps a resumed daemon from
        // refusing a session it served yesterday. The old row is built by
        // stripping the key off a real serialisation, not by hand-typing the
        // wire shape and hoping.
        let item = TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "edit".into(),
            outcome: ToolOutcome::Ok,
            payload: "done".into(),
            edit: None,
            origin: None,
            media: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        let old = json.replace(",\"edit\":null", "");
        assert_ne!(json, old, "the fixture must actually strip the key");
        let back: TranscriptItem = serde_json::from_str(&old).unwrap();
        assert_eq!(back, item);
    }

    /// **Who asked for the call, and that an old row says `None`** — R24 part two, decision 1.
    ///
    /// Two properties, and the second is the one a head's card depends on:
    ///
    ///  * an operator-run call round-trips **with the identity**, so the row and the
    ///    adjudication for the same call name the actor the same way (`human:dead` is what the
    ///    gate records);
    ///  * a row written **before the field existed** reads as `None`, which is
    ///    *the model proposed it* — not *nobody did*. That is what keeps a resumed daemon from
    ///    refusing a session it served yesterday, and it is why the field is `default` rather
    ///    than versioned, the same shape `edit` and `never_ran` already set on this variant.
    #[test]
    fn an_operator_run_call_carries_its_identity_and_an_old_row_reads_as_the_models() {
        let mine = TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "web_fetch".into(),
            outcome: ToolOutcome::Ok,
            payload: "the page".into(),
            edit: None,
            origin: Some(CallOrigin::Operator { who: "dead".into() }),
            media: None,
        };
        let json = serde_json::to_string(&mine).unwrap();
        assert!(
            json.contains("\"operator\"") && json.contains("\"dead\""),
            "the identity must be on the wire, not inferred by a head: {json}"
        );
        let back: TranscriptItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, mine);

        // And the row the store already holds, built by stripping the key off a real
        // serialisation rather than hand-typing the wire shape and hoping.
        let old = json.replace(",\"origin\":{\"operator\":{\"who\":\"dead\"}}", "");
        assert_ne!(json, old, "the fixture must actually strip the key");
        let back: TranscriptItem = serde_json::from_str(&old).unwrap();
        match back {
            TranscriptItem::ToolResult { origin, .. } => assert_eq!(
                origin, None,
                "a row from before the field is a row the MODEL proposed"
            ),
            other => panic!("not a tool result: {other:?}"),
        }
    }

    #[test]
    fn abstained_does_not_deserialise_as_ok() {
        let a = serde_json::to_string(&ToolOutcome::Abstained {
            reason: "corpus does not cover it".into(),
        })
        .unwrap();
        assert_ne!(
            serde_json::from_str::<ToolOutcome>(&a).unwrap(),
            ToolOutcome::Ok
        );
    }

    #[test]
    fn backgrounded_is_none_of_the_three_it_would_otherwise_be_mistaken_for() {
        // `backgrounded != finished != failed`, as a round trip rather than as a
        // convention: a transcript re-rendered a turn later must still be able to
        // tell "still running" from "produced nothing" and from "abandoned".
        let b = ToolOutcome::Backgrounded {
            handle: "j4".into(),
            ran_for_ms: 15_000,
            how: Backgrounding::Promoted,
            next: "job_output with job=\"j4\"".into(),
        };
        let back: ToolOutcome = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert_eq!(back, b);
        assert_ne!(back, ToolOutcome::Ok);
        assert_ne!(back, ToolOutcome::Timeout);
        assert!(!matches!(back, ToolOutcome::Failed { .. }));
        // And the handle survives, because a result whose handle was lost is a
        // process nobody can reach.
        assert!(serde_json::to_string(&b).unwrap().contains("\"j4\""));
    }

    #[test]
    fn the_three_ways_into_the_background_are_three_different_sentences() {
        // A promotion the model reads as its own request is a promotion it did not
        // notice, which is the defect this variant exists to make impossible.
        let asked = Backgrounding::Asked.phrasing();
        let promoted = Backgrounding::Promoted.phrasing();
        let operator = Backgrounding::Operator {
            identity: "deadtrickster".into(),
        }
        .phrasing();
        assert_ne!(asked, promoted);
        assert_ne!(promoted, operator);
        assert!(promoted.contains("did not ask"), "{promoted}");
        assert!(operator.contains("deadtrickster"), "{operator}");
    }

    /// **A `User` row with no `speaker` reads as the operator, and an `agent` row does not** —
    /// R42's wire half, and the two facts the requirement asks to be distinguishable.
    ///
    /// The absent case is the one leticl named as the reason a head cannot infer this: a row
    /// written before the field existed says nothing about who spoke, so the honest reading is
    /// the one a head already drew — and the alternative, a third *unknown* value, would put a
    /// new word on every old row for no gain.
    #[test]
    fn a_user_row_says_who_spoke_and_an_old_row_reads_as_the_operator() {
        let wire = r#"{"type":"user","parts":[{"kind":"text","text":"hi"}]}"#;
        let item: TranscriptItem = serde_json::from_str(wire).unwrap();
        let TranscriptItem::User { speaker, .. } = item else {
            panic!("a user row");
        };
        assert_eq!(
            speaker,
            Speaker::Operator,
            "an absent speaker is the operator"
        );
        assert_eq!(speaker.as_str(), "operator");

        let wire = r#"{"type":"user","speaker":"agent","parts":[{"kind":"text","text":"hi"}]}"#;
        let item: TranscriptItem = serde_json::from_str(wire).unwrap();
        let TranscriptItem::User { speaker, .. } = item else {
            panic!("a user row");
        };
        // **The word is the authorisation trail's own** — `agent` — so the row and the record
        // the oracle reads cannot spell the same speaker two ways.
        assert_eq!(speaker, Speaker::Agent);
        assert_eq!(speaker.as_str(), "agent");

        // And a row the daemon writes says which, so the two are never the same bytes.
        let operator = serde_json::to_string(&TranscriptItem::User {
            speaker: Speaker::Operator,
            parts: vec![UserPart::Text { text: "hi".into() }],
        })
        .unwrap();
        let agent = serde_json::to_string(&TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text { text: "hi".into() }],
        })
        .unwrap();
        assert_ne!(operator, agent);
        assert!(agent.contains(r#""speaker":"agent""#), "{agent}");
    }
}
