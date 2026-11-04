//! The events one turn emits, and the sink they go to. `HeadAttached` and tool-lifecycle events
//! belong elsewhere. `Delta` carries only the increment: it has a `text` and no `full`, so
//! accumulated text has nowhere to go.

use letibot_tokencore::TokenId;

use crate::completion::{FinishReason, PromptProgress};
use crate::metrics::TurnMetrics;

/// Which channel a delta belongs to. `ToolCall` exists because a call's body is ordinary text
/// to a renderer: the engine, walking the ids, is the only layer that can say it is not the
/// answer, and by the time it is a string in a `Delta` the evidence is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaTarget {
    Text,
    Reasoning,
    /// The body of a tool call, still being written. A head must never render this as prose:
    /// it is the raw `<function=…><parameter=…>` markup, and its settled form is the
    /// `ToolCallProposed` that follows.
    ToolCall,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    TurnStarted {
        turn_id: String,
        model: String,
        ledger_head: String,
        /// The WHOLE turn's start in Unix ms, not this round's: a `TurnStarted` fires once per
        /// round, so a head timing from the event restarts its clock every round. `None` means
        /// nobody measured it, which a head draws as *started before this head attached*.
        began_ms: Option<u64>,
    },
    /// Nothing surveyed reads this one. It is what makes a long prefill visible,
    /// and the guards require it to count as liveness.
    PromptProgress {
        turn_id: String,
        progress: PromptProgress,
    },
    /// The server's own generation counter, one event per frame that advanced it: the
    /// generation half of liveness, so a head watching a turn tells a hang from a model still
    /// emitting. It is the server's `tokens_predicted`, not our accumulated count.
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
        /// The call's raw arguments, in process only. A running call reached a head as a name
        /// and a digest, so it could never render `Running "cargo test"`; a `TurnEvent` goes to
        /// an in-process sink, and the lift cuts the string to a bounded label.
        arguments: String,
    },
    TranscriptAppended {
        item_id: String,
        kind: &'static str,
        ledger_head: String,
        tokens: u32,
        /// The row itself, so a log of these events can be read back as a conversation: the
        /// lift decides whether it fits an announcement, and the engine has no opinion on that
        /// number. Boxed because every other variant here is small.
        item: Box<letibot_transcript::TranscriptItem>,
    },
    TurnFinished {
        turn_id: String,
        finish_reason: FinishReason,
        /// Whether this round ended with calls for the caller to run. A round cannot be the
        /// prompt's end while calls it has just made are unanswered, and only the caller knows
        /// what follows them: this is the fact, not a verdict on whether a next round comes.
        proposed_calls: bool,
        metrics: Box<TurnMetrics>,
    },
    TurnInterrupted {
        turn_id: String,
        reason: String,
        partial_kept: bool,
    },
    /// The post-flight assertions land here, and so do the guards.
    Warning { code: &'static str, detail: String },
    /// A fold's long wait, named as the compaction's own: a scratch turn's `PromptProgress`
    /// lifted as itself lands in the head's `turn.progress`, which is the SESSION's, so the
    /// scratch prompt's count was drawn as the session's context.
    CompactionProgress {
        /// Which half is running, 1-based, in the order they RUN - which for the local
        /// plan is the recent tail first, since its cold prompt is the cheap one to warm
        /// the cache with.
        half: u64,
        /// How many halves this compaction has: 1 or 2.
        halves: u64,
        /// The half's own prompt, in the box's ledger tokens. The same unit every other
        /// count in this tree is in, so a reader can compare it with a `--status` figure
        /// without a conversion nobody named.
        prompt_tokens: u64,
        /// How much of that prompt the server has read. Zero on a transport that reports
        /// no prefill - a messages stream says nothing until it answers - which is a
        /// real answer here and not a failure to report one.
        processed: u64,
        /// What the half has produced so far, counted in `unit`.
        written: u64,
        /// What `written` counts, because the transports do not count the same thing: a token
        /// stream reports `tokens_predicted`, a messages stream reports only text, so one name
        /// for both would be a lie about one of them.
        unit: &'static str,
    },
}

/// Where events go. A trait rather than a channel so the engine has no opinion about the
/// transport, and so a test can assert on the exact sequence a turn produced.
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
        TurnEvent::TokensGenerated { .. } => "TokensGenerated",
        TurnEvent::Delta { .. } => "Delta",
        TurnEvent::ToolCallProposed { .. } => "ToolCallProposed",
        TurnEvent::TranscriptAppended { .. } => "TranscriptAppended",
        TurnEvent::TurnFinished { .. } => "TurnFinished",
        TurnEvent::TurnInterrupted { .. } => "TurnInterrupted",
        TurnEvent::Warning { .. } => "Warning",
        TurnEvent::CompactionProgress { .. } => "CompactionProgress",
    }
}

/// A short, stable digest of a tool call's arguments, for `ToolCallProposed`: the event
/// carries a digest rather than the arguments because it fans out to every attached head,
/// and the arguments are in the transcript, where a head can ask for them.
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

/// A sink that forwards progress and swallows rows.
///
/// For a session that is real work but not part of the operator's conversation:
/// the overrun summariser's scratchpads, which restage history a model has
/// already seen so it can be read in one piece, then are thrown away.
///
/// Two things went wrong on the way to this, in opposite directions, and both
/// were visible on the operator's screen. Passing the live sink announced every
/// restaged row as `TranscriptAppended`, and a head shows a placeholder for an
/// item id and waits for a body that never comes:
///
/// ```text
/// [tool_result — waiting for the body of …#compact-batch-0.66]
/// ```
///
/// Passing [`NullSink`] instead fixed that and broke the other half: a scratchpad
/// is a large prompt and prefilling it takes minutes, during which the head had
/// nothing to show - "tui doesnt show any prefill tho".
///
/// So `PromptProgress` goes through, because it is the only honest answer to
/// "what is it doing", and everything else is dropped, because everything else is
/// about a transcript nobody will read.
pub struct ProgressOnly<'a>(pub &'a mut dyn EventSink);

impl EventSink for ProgressOnly<'_> {
    fn emit(&mut self, event: TurnEvent) {
        if matches!(event, TurnEvent::PromptProgress { .. }) {
            self.0.emit(event);
        }
    }
}

#[cfg(test)]
mod progress_only {
    use super::*;

    #[test]
    fn rows_are_dropped_and_progress_is_not() {
        let mut inner = RecordingSink::new();
        {
            let mut s = ProgressOnly(&mut inner);
            s.emit(TurnEvent::TranscriptAppended {
                item_id: "scratch.0".into(),
                kind: "tool_result",
                ledger_head: "abc".into(),
                tokens: 10,
                item: Box::new(letibot_transcript::TranscriptItem::System {
                    text: "restaged history".into(),
                    origin: letibot_transcript::SystemOrigin::Update,
                }),
            });
        }
        assert!(
            inner.kinds().is_empty(),
            "a throwaway transcript must not reach the head: {:?}",
            inner.kinds()
        );
    }
}
