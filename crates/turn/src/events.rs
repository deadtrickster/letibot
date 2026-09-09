//! The events one turn emits (§4.5), and the sink they go to.
//!
//! Only the subset a turn produces lives here. `HeadAttached`, `DecisionRequested`
//! and the tool-lifecycle events belong to W7, W11 and W9 — putting them in this
//! enum now would make the turn engine look like it owns them.
//!
//! **`Delta` carries only the increment.** No event here carries accumulated text.
//! That single rule is half of "render cost independent of output length"; §13.3 is
//! the other half. It is stated as a type: `Delta` has a `text` and no `full`, and
//! there is nowhere to put one.

use letibot_tokencore::TokenId;

use crate::completion::{FinishReason, PromptProgress};
use crate::metrics::TurnMetrics;

/// Which channel a delta belongs to. Reasoning is a sibling of assistant here for
/// the same reason it is in `TranscriptItem`: one turn interleaves several of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaTarget {
    Text,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    TurnStarted {
        turn_id: String,
        model: String,
        ledger_head: String,
    },
    /// Nothing surveyed reads this one. It is what makes a long prefill visible,
    /// and §8.5 requires it to count as liveness.
    PromptProgress {
        turn_id: String,
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
    },
    TranscriptAppended {
        item_id: String,
        kind: &'static str,
        ledger_head: String,
        tokens: u32,
    },
    TurnFinished {
        turn_id: String,
        finish_reason: FinishReason,
        metrics: Box<TurnMetrics>,
    },
    TurnInterrupted {
        turn_id: String,
        reason: String,
        partial_kept: bool,
    },
    /// §18's post-flight assertions land here, and so do §8.5's guards.
    Warning { code: &'static str, detail: String },
}

/// Where events go.
///
/// A trait rather than a channel so the engine has no opinion about the transport,
/// and so a test can assert on the exact sequence a turn produced. W7 will
/// implement it over the session log.
pub trait EventSink {
    fn emit(&mut self, event: TurnEvent);
}

/// Drops everything. For the paths that have no head attached yet.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&mut self, _event: TurnEvent) {}
}

/// Keeps everything, for tests.
#[derive(Debug, Default)]
pub struct RecordingSink {
    pub events: Vec<TurnEvent>,
}

impl RecordingSink {
    pub fn new() -> Self {
        RecordingSink::default()
    }

    pub fn warnings(&self) -> Vec<(&'static str, &str)> {
        self.events
            .iter()
            .filter_map(|e| match e {
                TurnEvent::Warning { code, detail } => Some((*code, detail.as_str())),
                _ => None,
            })
            .collect()
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        self.events.iter().map(kind_of).collect()
    }
}

impl EventSink for RecordingSink {
    fn emit(&mut self, event: TurnEvent) {
        self.events.push(event);
    }
}

fn kind_of(e: &TurnEvent) -> &'static str {
    match e {
        TurnEvent::TurnStarted { .. } => "TurnStarted",
        TurnEvent::PromptProgress { .. } => "PromptProgress",
        TurnEvent::Delta { .. } => "Delta",
        TurnEvent::ToolCallProposed { .. } => "ToolCallProposed",
        TurnEvent::TranscriptAppended { .. } => "TranscriptAppended",
        TurnEvent::TurnFinished { .. } => "TurnFinished",
        TurnEvent::TurnInterrupted { .. } => "TurnInterrupted",
        TurnEvent::Warning { .. } => "Warning",
    }
}

/// A short, stable digest of a tool call's arguments, for `ToolCallProposed`.
///
/// The event carries a digest rather than the arguments because an event stream
/// fans out to every attached head and a 200 KB argument would be sent to all of
/// them. The arguments are in the transcript, which is where a head can ask for
/// them.
pub fn args_digest(arguments: &str) -> String {
    // FNV-1a. Not a security hash and not pretending to be one: this is a display
    // and correlation aid, and the ledger's SHA-256 chain is what carries identity.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in arguments.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a:{h:016x}")
}

/// Hex for a ledger head, so events carry something a human can compare.
pub fn head_hex(head: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in head {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Token ids as a short debug string, for warning details.
pub fn ids_preview(ids: &[TokenId], max: usize) -> String {
    if ids.len() <= max {
        format!("{ids:?}")
    } else {
        format!("{:?}… ({} total)", &ids[..max], ids.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delta_has_nowhere_to_put_accumulated_text() {
        // The rule is enforced by the type. This test exists so that adding a
        // `full` field has to delete an assertion that says why not.
        let d = TurnEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Text,
            text: "abc".into(),
        };
        let TurnEvent::Delta { text, .. } = &d else {
            panic!()
        };
        assert_eq!(text, "abc");
    }

    #[test]
    fn the_digest_is_stable_and_differs_on_a_changed_argument() {
        assert_eq!(args_digest(r#"{"a":1}"#), args_digest(r#"{"a":1}"#));
        assert_ne!(args_digest(r#"{"a":1}"#), args_digest(r#"{"a":2}"#));
    }
}
