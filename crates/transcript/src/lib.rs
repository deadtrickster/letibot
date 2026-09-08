//! The conversation record: what the harness stores, and what a dialect renders.
//!
//! This crate is deliberately tiny and has no dependency on a vocab, a model, an
//! FFI or a running server. Both the dialect crate (which renders) and the token
//! core (which tokenizes and persists) depend on it, and they must be buildable
//! independently of each other.
//!
//! See `docs/implementation-plan.md` §4.2.

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptItem {
    System {
        text: String,
        origin: SystemOrigin,
    },
    User {
        parts: Vec<UserPart>,
    },
    Reasoning {
        text: String,
        field: ReasoningField,
    },
    Assistant {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    ToolResult {
        call_id: String,
        name: String,
        outcome: ToolOutcome,
        payload: String,
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

/// A **closed** vocabulary. Adding a variant is a deliberate act.
///
/// `Abstained` is not a flavour of `Ok`, and that distinction is the whole reason
/// this is an enum rather than a bool. It is the case the oracle project measured:
/// retrieval honestly reported that the corpus did not cover the question, and the
/// model wrote a confident answer on top of it anyway. A tool runtime that collapses
/// "no answer" into "success with empty payload" makes that failure invisible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolOutcome {
    Ok,
    Abstained { reason: String },
    Failed { reason: String },
    Denied { req_id: String },
    Timeout,
    NotRun { why: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserPart {
    Text { text: String },
    Image { media_type: String, data_ref: String },
    FileRef { path: String, sha256: String },
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
            },
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"a"}"#.into(),
                }],
            },
            TranscriptItem::Reasoning {
                text: "second".into(),
                field: ReasoningField::ReasoningContent,
            },
            TranscriptItem::Assistant {
                text: "done".into(),
                tool_calls: vec![],
            },
        ];
        let json = serde_json::to_string(&turn).unwrap();
        let back: Vec<TranscriptItem> = serde_json::from_str(&json).unwrap();
        assert_eq!(turn, back, "interleaved order must survive serialisation");
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
}
