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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    /// The person at the keyboard. Every row written before this field existed reads as
    /// this, which is what a head already drew for them.
    #[default]
    Operator,
    /// This session appended it - a job completion, a salvage notice, a steering line,
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

/// What a [`TranscriptItem::Notice`] is about. A closed set rather than a string so a head that
/// draws one cannot be handed a word it has never seen and draw nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// A monitor fired, expired or was retired: the condition somebody asked to be told about.
    Monitor,
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
        /// Both sides of the file an edit changed, bounded to what differs: the raw material
        /// of the two-panel diff, on the row because a head attaching after a restart is
        /// handed rows rather than events. Display-only - the prompt builders read `payload`.
        #[serde(default)]
        edit: Option<ToolEditExcerpt>,
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
    },
    /// A delivery, not words: something learned while nobody was typing (a monitor's firing),
    /// as its own kind. A `User` row defaults `speaker` to the operator and would read as their instruction.
    Notice {
        /// What sort of news, so a head draws it and a reader counts it.
        kind: NoticeKind,
        text: String,
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
                edit,
                media,
                ..
            } => {
                // The excerpt is what makes a `read` or an `edit` row large, and it is
                // carried to the head, so it counts - and so does a picture a tool read, whose
                // bytes are inline on the row for the next round.
                payload.len()
                    + edit
                        .as_ref()
                        .map(|e| e.before.len() + e.after.len())
                        .unwrap_or(0)
                    + media.as_ref().map(|m| m.data_ref.len()).unwrap_or(0)
            }
            TranscriptItem::Notice { text, .. } => text.len(),
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

/// Which wire field the model expects its own prior reasoning replayed into. Getting this
/// wrong does not error: every prompt diverges from its cache entry at the first assistant
/// turn, so the prefix cache never hits and the entry count grows without bound.
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
        // Rows the store already holds predate the field, and `#[serde(default)]` is what lets
        // them still load - so a resumed daemon does not refuse a session it served yesterday.
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

    /// A `User` row with no `speaker` reads as the operator, and an `agent` row does not. The
    /// absent case is the honest reading, and a third unknown value would put a new word on
    /// every old row for no gain.
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
            edit: None,
            origin: None,
            media: Some(media::Media {
                mime: "image/png".into(),
                bytes: 1024,
                width: Some(8),
                height: Some(8),
                data_ref: base64.clone(),
                delivered: true,
            }),
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
}
