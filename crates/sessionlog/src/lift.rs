//! The W6↔W7 seam: `letibot_turn::TurnEvent` → §4.5's [`SessionEvent`].
//!
//! `docs/workstreams.md` says of this pair: *"the engine emits events into a
//! channel; the log consumes them. Fix §4.5's enum (already exact) and these two
//! never meet until integration."* They met here, and this file is the whole of the
//! meeting — 80 lines, one direction, behind a feature flag.
//!
//! **Neither side reaches into the other.** The engine does not know a log exists:
//! it takes a `&mut dyn EventSink`, and [`LogSink`] is one. The log does not know
//! a turn engine exists unless this feature is on, which is what lets the TUI
//! depend on the log without linking `libllama`.
//!
//! # Two places the seam is lossy, and both are reported rather than papered over
//!
//! 1. **`TurnFinished` is `TurnMetrics` upstream and `{usage, timings}` here.**
//!    §4.5 says `usage` and does not say what is in it. What survives is what a
//!    head can show: prompt/cached/predicted, and three durations. What does *not*
//!    survive is `prefix_check`, `id_slot`, `n_busy_slots`, `cost` and
//!    `dialect_template_sha` — every one of which §18.2 says is needed to tell a
//!    scheduling fact from a prefix divergence. A head cannot show them, so
//!    `turn_metrics` must reach a head by some other route, or §4.5's `usage` must
//!    grow.
//! 2. **`TranscriptAppended` has no content**, and `EventSink` has no channel for
//!    it. See [`crate::view::SessionView::record_item`].

use letibot_turn::{DeltaTarget as TurnDelta, EventSink, TurnEvent};

use crate::event::{DeltaTarget, FinishReason, PromptProgress, SessionEvent, Timings, Usage};
use crate::hub::Hub;

/// Lift one engine event into §4.5's enum.
pub fn from_turn_event(e: TurnEvent) -> SessionEvent {
    match e {
        TurnEvent::TurnStarted {
            turn_id,
            model,
            ledger_head,
            began_ms,
        } => SessionEvent::TurnStarted {
            turn_id,
            model,
            ledger_head,
            began_ms,
        },
        TurnEvent::PromptProgress { turn_id, progress } => SessionEvent::PromptProgress {
            turn_id,
            progress: PromptProgress {
                total: progress.total,
                cache: progress.cache,
                processed: progress.processed,
                time_ms: progress.time_ms,
            },
        },
        TurnEvent::TokensGenerated { turn_id, tokens } => {
            SessionEvent::TokensGenerated { turn_id, tokens }
        }
        // **The compaction's own progress, never lifted as the session's.** This is the
        // one arm here that exists to NOT forward an event as itself; see
        // `TurnEvent::CompactionProgress`. Nothing about the wording would have shown
        // the difference — the head drew it under the wrong label, as the session's
        // context, because that is the only slot a `PromptProgress` has.
        TurnEvent::CompactionProgress {
            half,
            halves,
            prompt_tokens,
            processed,
            written,
            unit,
        } => SessionEvent::CompactionProgress {
            half,
            halves,
            prompt_tokens,
            processed,
            written,
            unit: unit.to_string(),
        },
        TurnEvent::Delta {
            turn_id,
            target,
            text,
        } => SessionEvent::Delta {
            turn_id,
            target: match target {
                TurnDelta::Text => DeltaTarget::Text,
                TurnDelta::Reasoning => DeltaTarget::Reasoning,
                TurnDelta::ToolCall => DeltaTarget::ToolCall,
            },
            text,
        },
        TurnEvent::ToolCallProposed {
            turn_id,
            call_id,
            name,
            args_digest,
            arguments,
        } => SessionEvent::ToolCallProposed {
            turn_id,
            call_id,
            name,
            args_digest,
            // §4.1's cap, applied at the boundary it is about: below this line the
            // arguments are one in-process string, above it they would be a copy
            // per attached head.
            target: crate::event::display_target(&arguments),
        },
        // `tokens` is dropped: §4.5's `TranscriptAppended` is
        // `{item_id, kind, ledger_head}`. The token count is a ledger fact and the
        // ledger is where a head asks for it.
        TurnEvent::TranscriptAppended {
            item_id,
            kind,
            ledger_head,
            ..
        } => SessionEvent::TranscriptAppended {
            item_id,
            kind: kind.to_string(),
            ledger_head,
        },
        TurnEvent::TurnFinished {
            turn_id,
            finish_reason,
            metrics,
        } => SessionEvent::TurnFinished {
            turn_id,
            finish_reason: lift_finish(finish_reason),
            usage: Usage {
                prompt_tokens: metrics.prompt_tokens,
                cached_tokens: metrics.cached_tokens,
                predicted_tokens: metrics.predicted_tokens,
                // The one line that was missing: the daemon had the number and
                // dropped it here, so only the one-shot printer ever showed it.
                cost_micros_usd: metrics.cost.micros_usd,
            },
            timings: Timings {
                prompt_ms: metrics.prompt_ms,
                predicted_ms: metrics.predicted_ms,
                wall_ms: metrics.wall_ms,
            },
        },
        TurnEvent::TurnInterrupted {
            turn_id,
            reason,
            partial_kept,
        } => SessionEvent::TurnInterrupted {
            turn_id,
            reason,
            partial_kept,
        },
        TurnEvent::Warning { code, detail } => SessionEvent::Warning {
            code: code.to_string(),
            detail,

            compaction: None,
        },
    }
}

fn lift_finish(f: letibot_turn::FinishReason) -> FinishReason {
    use letibot_turn::FinishReason as F;
    match f {
        F::Eos => FinishReason::Eos,
        F::Word => FinishReason::Word,
        F::Length => FinishReason::Length,
        F::Aborted => FinishReason::Aborted,
        // The string survives. Folding an unrecognised reason into a neighbour on
        // the way to a head is the same defect the engine refuses to commit.
        F::Other(s) => FinishReason::Other(s.to_string()),
    }
}

/// An [`EventSink`] that appends to a [`Hub`] and fans out.
///
/// This is the *only* object that knows both crates.
pub struct LogSink {
    hub: std::sync::Arc<Hub>,
}

impl LogSink {
    pub fn new(hub: std::sync::Arc<Hub>) -> Self {
        LogSink { hub }
    }

    pub fn hub(&self) -> &std::sync::Arc<Hub> {
        &self.hub
    }
}

impl EventSink for LogSink {
    fn emit(&mut self, event: TurnEvent) {
        // Never blocks: the hub's fan-out demotes a slow head rather than waiting
        // for it, so a turn cannot be stalled by a client.
        self.hub.publish(from_turn_event(event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_turn::RecordingSink;

    #[test]
    fn every_engine_event_has_a_home_in_section_4_5() {
        // `TurnEvent` is a strict subset of §4.5 by construction. The exhaustive
        // match in `from_turn_event` is the enforcement; this test is the reminder
        // that a new engine event must be given one.
        let e = TurnEvent::Warning {
            code: "guard.empty",
            detail: "nothing came back".into(),
        };
        assert_eq!(from_turn_event(e).kind(), "Warning");
    }

    #[test]
    fn the_sink_publishes_and_the_seq_advances() {
        let hub = Hub::new("s");
        let mut sink = LogSink::new(hub.clone());
        sink.emit(TurnEvent::TurnStarted {
            turn_id: "t1".into(),
            model: "m".into(),
            ledger_head: "ab".into(),
            began_ms: None,
        });
        sink.emit(TurnEvent::Delta {
            turn_id: "t1".into(),
            target: TurnDelta::Text,
            text: "hi".into(),
        });
        assert_eq!(hub.head_seq(), 2);
        assert_eq!(hub.snapshot().turn.unwrap().text, "hi");
    }

    #[test]
    fn a_recording_sink_and_a_log_sink_see_the_same_sequence() {
        // The seam is a pure function, so a test that recorded engine output can be
        // replayed into a log — which is what makes W8 a leaf.
        let mut rec = RecordingSink::new();
        rec.emit(TurnEvent::TurnStarted {
            turn_id: "t1".into(),
            model: "m".into(),
            ledger_head: "ab".into(),
            began_ms: None,
        });
        rec.emit(TurnEvent::Delta {
            turn_id: "t1".into(),
            target: TurnDelta::Text,
            text: "x".into(),
        });
        let hub = Hub::new("s");
        for e in rec.events {
            hub.publish(from_turn_event(e));
        }
        assert_eq!(hub.head_seq(), 2);
    }

    #[test]
    fn the_generation_counter_lifts_to_section_4_5() {
        let s = from_turn_event(TurnEvent::TokensGenerated {
            turn_id: "t1".into(),
            tokens: 42,
        });
        assert_eq!(s.kind(), "TokensGenerated");
        assert!(
            matches!(s, SessionEvent::TokensGenerated { tokens: 42, .. }),
            "the counter is the server's number, lifted verbatim: {s:?}"
        );
    }
}
