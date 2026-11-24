//! The conversation record: what the harness stores and what a dialect renders.
//! Deliberately tiny, with no dependency on a vocab, model, FFI or server, so the dialect and token
//! core crates stay independently buildable.

use serde::{Deserialize, Serialize};

/// Text a head did not author, made safe for a terminal - here because both
/// sides of the log/wire boundary need it and neither can see the other.
pub mod media;
pub mod sanitize;
/// Fixtures that are real images - public for the reason `letibot_sessionlog::testing` is: the
/// crates that need a picture in a test are not this one. See the module for what it replaced.
pub mod testing;

/// What a fence that cites code looks like - the daemon reads the lines and the head fills
/// the fence, so the two spellings of that fence (an empty block, and the one-liner) and the
/// rule for a block that is never closed live here, once. Same reason as `media` above.
pub mod citation;

/// Cutting text to a bound, once: a payload, a queue row, a card and a status line all cut,
/// and a second copy of the arithmetic is a second place a character can be split.
pub mod text;

/// What a waiting session is owed, typed: the seat, the session list and the bridge all read one
/// shape, and the room and the session log cannot see each other.
pub mod pending;

/// The one line a spill leaves where it took the middle out. The tools write it and a head draws
/// it, so its spelling lives here once.
pub mod seam;

/// Who asked for a tool call. An enum rather than a `bool` because a second origin (a
/// peer agent, a seat, a scheduled job) is a variant rather than another field on a
/// struct that already has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOrigin {
    /// A person at this console ran it. `who` is the identity the gate records in
    /// `verdict_by` (`human:dead`, `human:leticl`), so the row and its own adjudication name
    /// the actor the same way.
    Operator { who: String },
}

/// Who a `User` row's words came from: the operator typed it, or this session appended
/// it. The vocabulary is `letibot_tools::authorise::Speaker`'s, repeated rather than
/// imported because `letibot-tools` depends on this crate and not the reverse.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    /// The person at the keyboard. Never the default: a row that did not say so may be anybody's,
    /// and reading a missing label as the operator's is what makes an agent's words authority.
    Operator,
    /// This session appended it - a job completion, a salvage notice, a steering line,
    /// the intent check. Never an authorisation, and never to be drawn as the operator's
    /// words. Every row written before the field existed reads as this: nothing said who it
    /// was, so nothing may read it as the person's.
    #[default]
    Agent,
    /// Another agent or program sent it into this session, e.g. a Claude Code seat through the
    /// bridge. It drives a turn like a prompt, but it is never the operator's words: nothing may
    /// read it as an authorisation, a correction or a label.
    Peer {
        /// The sender's handle, `cc@rano`.
        from: String,
    },
}

impl Speaker {
    pub fn as_str(&self) -> &'static str {
        match self {
            Speaker::Operator => "operator",
            Speaker::Agent => "agent",
            Speaker::Peer { .. } => "peer",
        }
    }

    /// The human at the keyboard said it. The one test every reader of "the operator's words" uses.
    pub fn is_operator(&self) -> bool {
        matches!(self, Speaker::Operator)
    }

    /// Who said it, as a reader names them: `operator`, `agent`, or the peer's handle.
    pub fn author(&self) -> &str {
        match self {
            Speaker::Peer { from } => from,
            other => other.as_str(),
        }
    }

    /// A peer's words as the model reads them: under the sender's handle, once. The model is not
    /// shown the speaker field, so the handle rides the text.
    pub fn signed(from: &str, text: &str) -> String {
        if text.trim_start().starts_with(&format!("{from}:")) {
            text.to_string()
        } else {
            format!("{from}: {text}")
        }
    }

    /// A peer's line, with an empty handle read as unknown rather than as the operator.
    pub fn peer(from: impl Into<String>) -> Self {
        let from = from.into();
        Speaker::Peer {
            from: if from.trim().is_empty() {
                "unknown".into()
            } else {
                from
            },
        }
    }
}

/// What a [`TranscriptItem::Notice`] is about. A closed set rather than a string so a head that
/// draws one cannot be handed a word it has never seen and draw nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// A monitor fired, expired or was retired: the condition somebody asked to be told about.
    Monitor,
}

/// What an [`TranscriptItem::Offer`] offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferKind {
    Skill,
    Note,
    /// A shadow's record: what a second model was shown, what it answered, and what the checks said.
    Shadow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
/// One entry in the conversation, in the order the model produced or consumed it.
/// Reasoning is a SIBLING of `Assistant`, not a field on it: a field forces
/// last-write-wins across several blocks and loses the interleaving a prefix cache needs.
#[allow(clippy::large_enum_variant)]
// The box would change the wire type's shape for every matcher in the tree, and that is
// the operator's decision rather than a tidy-up smuggled into a red gate.
pub enum TranscriptItem {
    System {
        text: String,
        origin: SystemOrigin,
    },
    User {
        parts: Vec<UserPart>,
        /// Who these words came from. Everything this session appends is a `User` item -
        /// steering, a salvage notice, a job settlement - so without this a head draws a
        /// job's completion exactly as it draws the person typing.
        #[serde(default)]
        speaker: Speaker,
    },
    Reasoning {
        text: String,
        field: ReasoningField,
        /// The operator stopped this thought, or it ran out of room. Not the same claim as
        /// `Assistant::truncated`, which marks an answer that was cut: a draft is the one
        /// thing a renderer may decline to replay, and replaying a loop teaches the model it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    Assistant {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
        /// The turn was cut short rather than finished naturally:
        /// `length` with usable text, or the kept partial. Absent on the wire
        /// means `false`, so rows written before the field existed still read.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    ToolResult {
        call_id: String,
        name: String,
        outcome: ToolOutcome,
        payload: String,
        /// Both sides of every file this call changed, bounded to what differs, for the two-panel
        /// diff. On the row because a head attaching after a restart gets rows, not events.
        /// Display-only; a list because one `bash` call can change several files.
        #[serde(
            default,
            alias = "edit",
            deserialize_with = "one_or_many_edits",
            skip_serializing_if = "Vec::is_empty"
        )]
        edits: Vec<ToolEditExcerpt>,
        /// Who asked for this call. A head labels a tool card from the nearest preceding
        /// assistant call, which cannot see a call nobody proposed, so an operator-run call
        /// would be drawn as the model's. `None` is a call the model proposed.
        #[serde(default)]
        origin: Option<CallOrigin>,
        /// Bytes this call read that are not text, stored inline so a prompt rebuilt from the
        /// row still carries the picture: one that reached the model once and not the next
        /// round would be a picture the model saw and then forgot.
        #[serde(default)]
        media: Option<media::Media>,
        /// How long the call ran, from its start to its result, written by the daemon. On the row so
        /// a head that restarts or resyncs still shows it. `None` on older rows and refused calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ran_ms: Option<u64>,
        /// The settled decision that gated this call, as `letibot_sessionlog::view::SettledDecision`
        /// serialises it. A value because this crate cannot name that type. Display-only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<serde_json::Value>,
    },
    /// A delivery, not words: something learned while nobody was typing (a monitor's firing),
    /// as its own kind. A `User` row defaults `speaker` to the operator and would read as their instruction.
    Notice {
        /// What sort of news, so a head draws it and a reader counts it.
        kind: NoticeKind,
        text: String,
        /// The facts the sentence was composed from, so a head draws the card from them and a
        /// `notice_` layer that rewords the sentence cannot take the card with it. Absent on older
        /// rows and where the site had nothing to hand over.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        facts: Option<serde_json::Value>,
    },
    /// What the model was offered in a round, for the operator. Display-only: every renderer skips
    /// it, so it adds no tokens and no request item, and heads draw it in place from the record.
    Offer {
        kind: OfferKind,
        /// The headline line a head draws.
        title: String,
        /// What a click opens.
        body: String,
    },
    /// A zero-width delimiter that renders to nothing, kept so per-segment decay and
    /// user-assisted compaction have something to address later. If those never arrive it
    /// has cost one row and zero rendered bytes.
    SegmentMark {
        segment_id: String,
        label: String,
        kind: String,
        edge: SegmentEdge,
    },
}

impl TranscriptItem {
    /// Whether this row is sent to the model, decided on the type so no renderer can drift from
    /// the store. `Offer` and `SegmentMark` never reach a request. Every arm is spelled, so a new
    /// kind must answer here.
    pub fn reaches_model(&self) -> bool {
        match self {
            // The operator's record of what a round offered, drawn in place by a head.
            TranscriptItem::Offer { .. } => false,
            // Renders to nothing in every dialect: structure for compaction, not words.
            TranscriptItem::SegmentMark { .. } => false,
            TranscriptItem::System { .. }
            | TranscriptItem::User { .. }
            | TranscriptItem::Reasoning { .. }
            | TranscriptItem::Assistant { .. }
            | TranscriptItem::ToolResult { .. }
            | TranscriptItem::Notice { .. } => true,
        }
    }

    /// Roughly how many bytes of text this row carries: a size bound's input, not a
    /// serialisation, so it counts the strings a reader would see and ignores structure.
    /// One definition, because two copies of "what counts as size" would drift.
    pub fn bytes(&self) -> usize {
        let parts = |parts: &[UserPart]| -> usize {
            parts
                .iter()
                .map(|p| match p {
                    UserPart::Text { text } => text.len(),
                    // A picture's bytes are ON this row, base64 and all: the reference is the body,
                    // not a pointer. Counting 32 for it left the census blind to the largest row a
                    // session can hold, and the hub's byte bound blind in the same place.
                    UserPart::Image { data_ref, .. } => data_ref.len(),
                    // A document's bytes are on the row for the same reason, and the same count.
                    UserPart::Document { data_ref, .. } => data_ref.len(),
                    UserPart::FileRef { .. } => 32,
                })
                .sum()
        };
        match self {
            TranscriptItem::System { text, .. } | TranscriptItem::Reasoning { text, .. } => {
                text.len()
            }
            // A `write` call carries the whole file body in its arguments, so a row that counted
            // only its text under-reports by the size of the file it wrote.
            TranscriptItem::Assistant {
                text, tool_calls, ..
            } => {
                text.len()
                    + tool_calls
                        .iter()
                        .map(|c| c.name.len() + c.arguments.len())
                        .sum::<usize>()
            }
            TranscriptItem::User { parts: p, .. } => parts(p),
            TranscriptItem::ToolResult {
                payload,
                edits,
                media,
                ..
            } => {
                // The excerpts are what makes a `read` or an `edit` row large, and they are
                // carried to the head, so they count - and so does a picture a tool read, whose
                // bytes are inline on the row for the next round.
                payload.len()
                    + edits
                        .iter()
                        .map(|e| e.before.len() + e.after.len())
                        .sum::<usize>()
                    + media.as_ref().map(|m| m.data_ref.len()).unwrap_or(0)
            }
            TranscriptItem::Notice { text, .. } => text.len(),
            TranscriptItem::Offer { title, body, .. } => title.len() + body.len(),
            TranscriptItem::SegmentMark { label, .. } => label.len(),
        }
    }
}

/// The before/after of a file-editing call, bounded to what differs. One definition, here,
/// because `letibot-tools` builds it and `letibot-sessionlog` carries it and neither can
/// name the other, so two copies of a nine-field struct is how copies drift.
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
    /// Line counts of each whole file, so "… N unchanged lines" is a
    /// fact rather than a guess.
    pub before_lines: usize,
    pub after_lines: usize,
    /// The cap cut the excerpt: there is more change than this carries.
    pub truncated: bool,
    /// The excerpt lines, LF-joined, no trailing newline. Empty when the
    /// side has no lines in the range (a pure insertion has no `before`).
    pub before: String,
    pub after: String,
    /// Why this is not a diff of the file, in the tool's own words, when the call it previews
    /// would not apply: an `edit` whose text is not in the file, or matches more than one place.
    /// `before` and `after` are then the call's PROPOSAL - what it asked to write - and not a
    /// change to the file, which is what a card says above them. Empty for every real excerpt, and
    /// absent from a row written before the field existed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// A stored row's excerpts: the list this tree writes, or the ONE object a row written before the
/// list existed carries, read as a one-item list - so the store never has to rewrite a row. A
/// `null` is the empty list, which is what the old `Option` wrote for a call that changed nothing.
pub fn one_or_many_edits<'de, D>(d: D) -> Result<Vec<ToolEditExcerpt>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{MapAccess, SeqAccess, Visitor};
    struct Edits;
    impl<'de> Visitor<'de> for Edits {
        type Value = Vec<ToolEditExcerpt>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a list of edit excerpts, or the one an older row carries")
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_any(self)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(e) = seq.next_element::<ToolEditExcerpt>()? {
                out.push(e);
            }
            Ok(out)
        }
        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
            let one =
                ToolEditExcerpt::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
            Ok(vec![one])
        }
    }
    d.deserialize_any(Edits)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemOrigin {
    /// The system prompt the session opened with; part of the stable prefix.
    Bootstrap,
    /// A later change. Appended after the cached history rather than rewriting
    /// message 0 - rewriting cost a full cold re-prefill of a 179k conversation
    /// when measured. See `SystemUpdateMode` in the dialect crate.
    Update,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentEdge {
    Open,
    Close,
}

/// Which wire field the model expects its prior reasoning replayed into. A wrong answer does not
/// error, it misses the prefix cache at every assistant turn. The signed variants carry the
/// provider's opaque bytes, since a signed block cannot be replayed without them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningField {
    /// GLM: replayed as `reasoning_content`.
    ReasoningContent,
    /// Qwen: replayed inside the rendered turn, gated by `preserve_thinking`.
    Inline,
    /// Anthropic: replayed as a `thinking` block carrying this block's `signature`, exactly as
    /// the API returned it. `text` is that block's `thinking` - a summary, when it summarizes.
    Signed { signature: String },
    /// Anthropic: replayed as a `redacted_thinking` block carrying its encrypted `data`, which is
    /// the whole of the block. `text` is empty: there was nothing to show.
    Redacted { data: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON text as the model emitted it. Not parsed here: the exact bytes are
    /// what must be replayed to keep the prefix stable.
    pub arguments: String,
}

/// An id for a call the model did not name. Positional `call_0` repeats every turn, so this is a
/// per-process nonce plus a counter, never reused across a restart. A model's own id is kept as
/// given, since the next request must echo it.
pub fn mint_call_id() -> String {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    static NONCE: OnceLock<String> = OnceLock::new();
    let nonce = NONCE.get_or_init(|| {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{:x}{t:x}", std::process::id())
    });
    format!("call_{nonce}_{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// How a command came to be running in the background rather than inline. Three ways in and
/// no fourth, because the distinction decides whether the model already knows and whether
/// there is somebody to attribute it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "how", rename_all = "snake_case")]
pub enum Backgrounding {
    /// The model asked for it: `background: true`.
    Asked,
    /// The runtime promoted it, because it outlived the foreground threshold.
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

/// A closed vocabulary: adding a variant is a deliberate act. `Abstained` is not a flavour of
/// `Ok`, and `Backgrounded` is not `Ok` with an empty payload either - each names a state the
/// model would otherwise conclude something false about.
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
    /// Still running. Not finished, not failed, not abandoned.
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
    /// A file the person attached or a tool read that the model is given as a DOCUMENT: the bytes,
    /// the type, and the name the door's request carries. The same channel as a picture, because the
    /// store row, the replay and the `data:` reference are one mechanism.
    Document {
        media_type: String,
        data_ref: String,
        filename: String,
    },
    FileRef {
        path: String,
        sha256: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two minted ids differ, and neither is a positional name: a `call_0` in every turn is one id
    /// for a whole session, which is what a reader correlating a call with its result cannot use.
    #[test]
    fn two_minted_call_ids_differ_and_are_not_positional() {
        let a = mint_call_id();
        let b = mint_call_id();
        assert_ne!(a, b);
        for id in [&a, &b] {
            assert!(id.starts_with("call_"), "{id}");
            assert!(
                id.trim_start_matches("call_").chars().any(|c| c == '_'),
                "no nonce in {id}"
            );
        }
    }

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
        // The transcript's own record that kept text or a kept partial did not finish
        // naturally. Dropped on the way out, the row would read back as a completed turn.
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
            edits: vec![ToolEditExcerpt {
                path: "crates/ui/src/style.rs".into(),
                created: false,
                before_start: 3,
                after_start: 3,
                before_lines: 64,
                after_lines: 65,
                truncated: false,
                before: "fn a() {".into(),
                after: "fn a() {\n    x();".into(),
                note: String::new(),
            }],
            origin: None,
            media: None,
            ran_ms: None,
            decision: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: TranscriptItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, item, "the excerpt rides the row both ways");
    }

    #[test]
    fn a_row_written_before_the_edit_existed_reads_as_none() {
        // Rows the store already holds predate the field, and `#[serde(default)]` is what lets
        // them still load - so a resumed daemon does not refuse a session it served yesterday.
        let old = serde_json::json!({"type":"tool_result","call_id":"c1","name":"edit",
            "outcome":{"outcome":"ok"},"payload":"done"});
        let back: TranscriptItem = serde_json::from_value(old).unwrap();
        match back {
            TranscriptItem::ToolResult { edits, .. } => {
                assert!(edits.is_empty(), "no key is no excerpt: {edits:?}")
            }
            other => panic!("not a tool result: {other:?}"),
        }
    }

    /// The store holds rows written when one excerpt was all a row could carry: `"edit": {…}` must
    /// read back as a one-item list, because rewriting stored rows is the migration this avoids.
    #[test]
    fn a_row_with_one_stored_excerpt_reads_as_a_one_item_list() {
        let one = serde_json::json!({
            "path": "a.txt",
            "created": false,
            "before_start": 1,
            "after_start": 1,
            "before_lines": 2,
            "after_lines": 2,
            "truncated": false,
            "before": "one\nTWO",
            "after": "one\nTHREE",
        });
        // Both spellings of an older row: the object, and the object beside a `null` key.
        for old in [
            serde_json::json!({"type":"tool_result","call_id":"c1","name":"edit",
                "outcome":{"outcome":"ok"},"payload":"done","edit":one}),
            serde_json::json!({"type":"tool_result","call_id":"c1","name":"bash",
                "outcome":{"outcome":"ok"},"payload":"done","edit":null}),
        ] {
            let back: TranscriptItem = serde_json::from_value(old.clone()).unwrap();
            match back {
                TranscriptItem::ToolResult { edits, .. } => {
                    assert_eq!(edits.len(), usize::from(old["edit"].is_object()), "{old}")
                }
                other => panic!("not a tool result: {other:?}"),
            }
        }
        // And a row this tree writes carries the list, not the single object.
        let json = serde_json::to_string(&TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            outcome: ToolOutcome::Ok,
            payload: "done".into(),
            edits: vec![ToolEditExcerpt {
                path: "a.txt".into(),
                created: false,
                before_start: 1,
                after_start: 1,
                before_lines: 2,
                after_lines: 2,
                truncated: false,
                before: "one\nTWO".into(),
                after: "one\nTHREE".into(),
                note: String::new(),
            }],
            origin: None,
            media: None,
            ran_ms: None,
            decision: None,
        })
        .unwrap();
        assert!(json.contains("\"edits\":["), "{json}");
    }

    /// The daemon's duration and decision ride the row both ways, and a row stored before the
    /// fields existed reads with neither and serialises to the same bytes.
    #[test]
    fn a_result_row_stores_its_duration_and_decision_and_an_old_row_still_reads() {
        let bare = TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            outcome: ToolOutcome::Ok,
            payload: "done".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
            ran_ms: None,
            decision: None,
        };
        let old = serde_json::to_string(&bare).unwrap();
        assert!(
            !old.contains("ran_ms") && !old.contains("decision"),
            "an absent fact changed the stored bytes: {old}"
        );
        let back: TranscriptItem = serde_json::from_str(&old).unwrap();
        assert_eq!(back, bare);

        let mut with = bare.clone();
        if let TranscriptItem::ToolResult {
            ran_ms, decision, ..
        } = &mut with
        {
            *ran_ms = Some(1_234);
            *decision = Some(serde_json::json!({"req_id": "r1"}));
        }
        let json = serde_json::to_string(&with).unwrap();
        let back: TranscriptItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, with, "the facts did not survive the store");
    }

    /// Who asked for the call, and that an old row says `None`. An operator-run call
    /// round-trips with the identity so the row and its adjudication name the actor the same
    /// way; a row written before the field reads as "the model proposed it", not "nobody did".
    #[test]
    fn an_operator_run_call_carries_its_identity_and_an_old_row_reads_as_the_models() {
        let mine = TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "web_fetch".into(),
            outcome: ToolOutcome::Ok,
            payload: "the page".into(),
            edits: Vec::new(),
            origin: Some(CallOrigin::Operator { who: "dead".into() }),
            media: None,
            ran_ms: None,
            decision: None,
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

    /// A `User` row with no `speaker` reads as the session's own, never as the person's: nothing
    /// said who it was, and a missing label is not consent. A third unknown word would put a new
    /// word on every old row for no gain.
    #[test]
    fn a_user_row_says_who_spoke_and_an_old_row_is_not_the_operator() {
        let wire = r#"{"type":"user","parts":[{"kind":"text","text":"hi"}]}"#;
        let item: TranscriptItem = serde_json::from_str(wire).unwrap();
        let TranscriptItem::User { speaker, .. } = item else {
            panic!("a user row");
        };
        assert_eq!(
            speaker,
            Speaker::Agent,
            "an absent speaker is not the operator's words"
        );
        assert_eq!(speaker.as_str(), "agent");

        let wire = r#"{"type":"user","speaker":"agent","parts":[{"kind":"text","text":"hi"}]}"#;
        let item: TranscriptItem = serde_json::from_str(wire).unwrap();
        let TranscriptItem::User { speaker, .. } = item else {
            panic!("a user row");
        };
        // The word is the authorisation trail's own - `agent` - so the row and the record
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

    /// A peer's line keeps its sender across the store and is not the operator's.
    #[test]
    fn a_peer_row_keeps_its_sender() {
        let peer = TranscriptItem::User {
            speaker: Speaker::peer("cc@rano"),
            parts: vec![UserPart::Text { text: "hi".into() }],
        };
        let wire = serde_json::to_string(&peer).unwrap();
        let back: TranscriptItem = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, peer);
        let TranscriptItem::User { speaker, .. } = back else {
            panic!("a user row");
        };
        assert!(!speaker.is_operator());
        assert_eq!(speaker.author(), "cc@rano");
        assert_eq!(Speaker::peer(" ").author(), "unknown");
    }

    /// The size bound counts what a row actually holds. A picture's base64 body, a tool's own
    /// picture and a `write` call's file body are the three rows that can be megabytes, and each
    /// counting as nothing left a census and a byte bound blind to the largest row there is.
    #[test]
    fn a_row_that_carries_a_picture_or_a_written_file_counts_its_bytes() {
        let base64 = "data:image/png;base64,".to_string() + &"A".repeat(4096);
        let picture = TranscriptItem::User {
            speaker: Speaker::Operator,
            parts: vec![
                UserPart::Text {
                    text: "look".into(),
                },
                UserPart::Image {
                    media_type: "image/png".into(),
                    data_ref: base64.clone(),
                },
            ],
        };
        assert_eq!(picture.bytes(), 4 + base64.len());

        let written = TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "write".into(),
                arguments: format!(r#"{{"path":"a","content":"{}"}}"#, "x".repeat(2048)),
            }],
            truncated: false,
        };
        assert!(
            written.bytes() > 2048,
            "a write call's body is on the row: {}",
            written.bytes()
        );

        let read_a_picture = TranscriptItem::ToolResult {
            call_id: "c2".into(),
            name: "read".into(),
            outcome: ToolOutcome::Ok,
            payload: "a picture".into(),
            edits: Vec::new(),
            origin: None,
            media: Some(media::Media {
                mime: "image/png".into(),
                bytes: 1024,
                width: Some(8),
                height: Some(8),
                data_ref: base64.clone(),
                delivered: true,
            }),
            ran_ms: None,
            decision: None,
        };
        assert_eq!(read_a_picture.bytes(), "a picture".len() + base64.len());

        // And a file REFERENCE really is a pointer: the row does not carry those bytes, so
        // counting the file it names would bound the wrong thing.
        let referenced = TranscriptItem::User {
            speaker: Speaker::Operator,
            parts: vec![UserPart::FileRef {
                path: "/tmp/huge.bin".into(),
                sha256: "0".repeat(64),
            }],
        };
        assert_eq!(referenced.bytes(), 32);
    }

    /// A document part is a row the store keeps like a picture's: bytes inline, a count that sees
    /// them, and a round trip that keeps the file's name (a door's request needs it).
    #[test]
    fn a_document_part_round_trips_with_its_filename_and_counts_its_bytes() {
        let base64 = media::encode_base64(&crate::testing::pdf("a known word"));
        let row = TranscriptItem::User {
            speaker: Speaker::Operator,
            parts: vec![
                UserPart::Text {
                    text: "what does it say".into(),
                },
                UserPart::Document {
                    media_type: "application/pdf".into(),
                    data_ref: format!("data:application/pdf;base64,{base64}"),
                    filename: "report.pdf".into(),
                },
            ],
        };
        let wire = serde_json::to_string(&row).unwrap();
        assert!(wire.contains(r#""kind":"document""#), "{wire}");
        assert_eq!(serde_json::from_str::<TranscriptItem>(&wire).unwrap(), row);
        assert_eq!(
            row.bytes(),
            "what does it say".len() + "data:application/pdf;base64,".len() + base64.len()
        );
    }

    /// Every row written before documents existed still reads: a picture part, a file reference and
    /// a tool result whose media is a picture. The new variant is an addition, not a version.
    #[test]
    fn a_row_written_before_documents_existed_still_reads() {
        let old = r#"{"type":"user","parts":[{"kind":"image","media_type":"image/png",
            "data_ref":"data:image/png;base64,AAAA"}],"speaker":"operator"}"#;
        let read: TranscriptItem = serde_json::from_str(old).expect("an old user row");
        assert!(matches!(
            read,
            TranscriptItem::User { parts, .. } if matches!(&parts[0], UserPart::Image { .. })
        ));
        let old = r#"{"type":"tool_result","call_id":"c1","name":"read",
            "outcome":{"outcome":"ok"},"payload":"a picture",
            "media":{"mime":"image/png","bytes":8,
            "width":null,"height":null,"data_ref":"data:image/png;base64,AAAA"}}"#;
        let read: TranscriptItem = serde_json::from_str(old).expect("an old tool result");
        let TranscriptItem::ToolResult { media, .. } = read else {
            panic!("a tool result");
        };
        let media = media.expect("the media an old row carries");
        assert_eq!(media.mime, "image/png");
        assert!(!media.is_document());
    }

    /// The one decision, pinned: every kind answers, and exactly the operator's card and the
    /// zero-width marker never reach a request. A new kind that forgets to answer breaks the
    /// match in `reaches_model` first, and this test holds the list it must be added to.
    #[test]
    fn every_transcript_item_kind_declares_whether_it_reaches_the_model() {
        let all = vec![
            TranscriptItem::System {
                text: "s".into(),
                origin: SystemOrigin::Bootstrap,
            },
            TranscriptItem::User {
                speaker: Speaker::Operator,
                parts: vec![UserPart::Text { text: "u".into() }],
            },
            TranscriptItem::Reasoning {
                text: "r".into(),
                field: ReasoningField::Inline,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: "a".into(),
                tool_calls: Vec::new(),
                truncated: false,
            },
            TranscriptItem::ToolResult {
                call_id: "c".into(),
                name: "read".into(),
                outcome: ToolOutcome::Ok,
                payload: "p".into(),
                edits: Vec::new(),
                origin: None,
                media: None,
                ran_ms: None,
                decision: None,
            },
            TranscriptItem::Notice {
                kind: NoticeKind::Monitor,
                text: "n".into(),
                facts: None,
            },
            TranscriptItem::Offer {
                kind: OfferKind::Note,
                title: "o".into(),
                body: "b".into(),
            },
            TranscriptItem::SegmentMark {
                segment_id: "1".into(),
                label: "m".into(),
                kind: "cut".into(),
                edge: SegmentEdge::Open,
            },
        ];
        assert_eq!(all.len(), 8, "a new kind must join this list");
        for item in &all {
            let reaches = item.reaches_model();
            let expected = !matches!(
                item,
                TranscriptItem::Offer { .. } | TranscriptItem::SegmentMark { .. }
            );
            assert_eq!(reaches, expected, "{item:?}");
        }
    }
}
