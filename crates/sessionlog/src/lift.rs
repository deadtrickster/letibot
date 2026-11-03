//! The engine-to-log seam: `letibot_turn::TurnEvent` lifted into [`SessionEvent`].
//!
//! Neither side reaches into the other. The engine does not know a log exists - it takes a
//! `&mut dyn EventSink`, and [`LogSink`] is one - which is what lets the TUI depend on the log
//! without linking `libllama`.
//!
//! The seam is lossy in two places, both reported rather than papered over. `TurnFinished` carries
//! `{usage, timings}` and not all of the engine's metrics: `prefix_check` has no wire twin yet, and
//! `dialect_template_sha` is a model fact a head already has from its wiring. The slot and the load
//! do reach a head, through `Usage::served`, because zero reuse means two different things without
//! them. And `TranscriptAppended` has no content: see [`crate::view::SessionView::record_item`].

use letibot_turn::{DeltaTarget as TurnDelta, EventSink, TurnEvent};

use crate::event::{
    DeltaTarget, FinishReason, PromptProgress, Served, SessionEvent, Timings, Usage,
};
use crate::hub::Hub;

/// Lift one engine event into [`SessionEvent`].
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
        // The compaction's own progress, never lifted as the session's: the head drew a
        // `PromptProgress` under the session's context label, because that is the only slot it has.
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
            // The display cap, applied at the boundary it is about: below this line the arguments
            // are one in-process string, above it they would be a copy per attached head.
            target: crate::event::display_target(&arguments),
        },
        // `tokens` is dropped: the token count is a ledger fact and the ledger is where a head asks
        // for it. The BODY rides, bounded here: one copy per retained event, and a head that can
        // draw a row from its announcement never waits for a second event to do it.
        TurnEvent::TranscriptAppended {
            item_id,
            kind,
            ledger_head,
            item,
            ..
        } => {
            let body_bytes = item.bytes() as u64;
            SessionEvent::TranscriptAppended {
                item_id,
                kind: kind.to_string(),
                ledger_head,
                item: crate::event::rides_announcement(&item).then_some(item),
                body_bytes,
            }
        }
        TurnEvent::TurnFinished {
            turn_id,
            finish_reason,
            proposed_calls,
            metrics,
        } => SessionEvent::TurnFinished {
            turn_id,
            finish_reason: lift_finish(finish_reason),
            // The round's own fact, translated: calls it just made are unanswered, so the prompt
            // cannot be over. What follows them is the daemon's, and this says nothing about it.
            // The round loop overrides it where it knows more: a round nudged into another.
            ends_turn: !proposed_calls,
            usage: Usage {
                prompt_tokens: metrics.prompt_tokens,
                cached_tokens: metrics.cached_tokens,
                predicted_tokens: metrics.predicted_tokens,
                // Carried so a turn's spend reaches the daemon and not only the one-shot printer.
                cost_micros_usd: metrics.cost.micros_usd,
                // Which slot served the turn and under what load. The engine's -1 means the server
                // did not say, and a head is told nothing rather than a number that is an absence.
                served: Served {
                    id_slot: (metrics.id_slot >= 0).then_some(metrics.id_slot),
                    n_busy_slots: metrics.n_busy_slots,
                },
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
        // `TurnEvent` is a strict subset of what a head can be told by construction. The
        // exhaustive match in `from_turn_event` is the enforcement; this test is the reminder that
        // a new engine event must be given a home.
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
        // The seam is a pure function, so a test that recorded engine output can be replayed into
        // a log - which is what makes the two crates land independently.
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

    /// A turn's slot and load reach a head, and the engine's -1 for an absent slot reaches it as
    /// nothing. Without them, zero reuse is one number for a slot that did not hold the history and
    /// for a prompt that diverged.
    #[test]
    fn a_turns_slot_and_load_reach_a_head_and_an_absent_slot_as_nothing() {
        let served = |id_slot: i64, n_busy_slots: Option<u32>| {
            let e = from_turn_event(TurnEvent::TurnFinished {
                turn_id: "t1".into(),
                finish_reason: letibot_turn::FinishReason::Eos,
                // A round with nothing for the caller to run is the prompt's end, and this
                // measures the usage a finished one carries.
                proposed_calls: false,
                metrics: Box::new(metrics(id_slot, n_busy_slots)),
            });
            match e {
                SessionEvent::TurnFinished { usage, .. } => usage.served,
                other => panic!("a finished turn lifts to TurnFinished, not {other:?}"),
            }
        };
        assert_eq!(
            served(3, Some(2)),
            Served {
                id_slot: Some(3),
                n_busy_slots: Some(2),
            }
        );
        assert_eq!(served(-1, None), Served::default(), "-1 is an absence");
        // One half measured and the other not is a real case: the slot came back from the server
        // and the harness never looked at the load.
        assert_eq!(
            served(0, None),
            Served {
                id_slot: Some(0),
                n_busy_slots: None,
            }
        );
    }

    /// One finished submission's metrics, with every field a head is not being told set to
    /// something obviously not the field under test.
    fn metrics(id_slot: i64, n_busy_slots: Option<u32>) -> letibot_turn::TurnMetrics {
        use letibot_backend::{Meter, TurnCost};
        letibot_turn::TurnMetrics {
            turn_id: "t1".into(),
            model: "m".into(),
            dialect_template_sha: "00".into(),
            ledger_head_sent: "ab".into(),
            prompt_tokens: 100,
            cached_tokens: 90,
            predicted_tokens: 5,
            prompt_tokens_server: 100,
            prompt_processed: 10,
            finish_reason: letibot_turn::FinishReason::Eos,
            prompt_ms: 40.0,
            predicted_ms: 900.0,
            draft_n: 0,
            draft_n_accepted: 0,
            id_slot,
            n_busy_slots,
            prefix_check: letibot_turn::PrefixCheck::FirstTurn,
            cost: TurnCost {
                meter: Meter::WallClock,
                prompt_tokens: 100,
                cached_tokens: 90,
                generated_tokens: 5,
                wall_ms: 950,
                micros_usd: None,
            },
            wall_ms: 950,
            first_byte_ms: Some(120),
        }
    }
}
