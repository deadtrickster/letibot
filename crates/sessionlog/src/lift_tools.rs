//! The tool runtime's events, lifted into §4.5's enum.
//!
//! The same shape as [`crate::lift`] and for the same reason: `letibot-tools`
//! defines the three lifecycle events it emits, this crate defines what a head
//! sees, and exactly one object knows both. Keeping the runtime free of the log
//! is what lets `cargo build -p letibot-tools` prove that a tool cannot reach a
//! head, a socket or a model.
//!
//! **This mapping is total and lossless**, which was not true before W9: the
//! fields it needs are the fields §4.5's `…` did not name, and they were added to
//! [`crate::event::SessionEvent`] rather than dropped here. A lift that silently
//! discards a field is how a head ends up unable to show something the runtime
//! knew.

use letibot_tools::events::{ToolEvent, ToolEventSink};

use crate::event::SessionEvent;
use crate::hub::Hub;

pub fn from_tool_event(e: ToolEvent) -> SessionEvent {
    match e {
        ToolEvent::Started {
            turn_id,
            call_id,
            name,
            access,
        } => SessionEvent::ToolStarted {
            turn_id,
            call_id,
            name,
            // A string on the wire: the head protocol is versioned by content, and
            // an enum here would make adding an access class a protocol break.
            access: access.as_str().to_string(),
        },
        ToolEvent::Progress {
            turn_id,
            call_id,
            note,
        } => SessionEvent::ToolProgress {
            turn_id,
            call_id,
            note,
        },
        ToolEvent::Finished {
            turn_id,
            call_id,
            outcome,
            payload_digest,
            inline_bytes,
            full_bytes,
            spill,
            repairs,
            edit,
        } => SessionEvent::ToolFinished {
            turn_id,
            call_id,
            outcome,
            payload_digest,
            inline_bytes,
            full_bytes,
            spill,
            repairs,
            // Field by field, runtime shape into wire shape: the event enum
            // compiles with `tools` off, so it cannot carry the runtime's
            // type, and a lift that dropped the pair would send a head that
            // never saw the call live away unable to draw it.
            edit: edit.map(|e| crate::event::ToolEdit {
                path: e.path,
                created: e.created,
                before_start: e.before_start,
                after_start: e.after_start,
                before_lines: e.before_lines,
                after_lines: e.after_lines,
                truncated: e.truncated,
                before: e.before,
                after: e.after,
            }),
        },
    }
}

/// A [`ToolEventSink`] that appends to a [`Hub`] and fans out.
pub struct ToolLogSink {
    hub: std::sync::Arc<Hub>,
}

impl ToolLogSink {
    pub fn new(hub: std::sync::Arc<Hub>) -> Self {
        ToolLogSink { hub }
    }

    pub fn hub(&self) -> &std::sync::Arc<Hub> {
        &self.hub
    }
}

impl ToolEventSink for ToolLogSink {
    fn emit(&mut self, event: ToolEvent) {
        // Never blocks, for the same reason `LogSink` does not: a slow head is
        // demoted, not waited for, so a tool call cannot be stalled by a client.
        self.hub.publish(from_tool_event(event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tools::schema::Access;
    use letibot_transcript::ToolOutcome;

    #[test]
    fn a_real_call_reaches_a_head_with_everything_the_runtime_knew() {
        // Not a hand-built event: a real `read` of a large file, spilled, through
        // the real runtime — so the mapping is tested against what is actually
        // emitted rather than against what this file assumes is emitted.
        let hub = Hub::new("s");
        hub.publish(SessionEvent::TurnStarted {
            turn_id: "t1".into(),
            model: "m".into(),
            ledger_head: "ab".into(),
        });
        let mut sink = ToolLogSink::new(hub.clone());
        let mut h =
            letibot_tools::testing::harness_with_spiller(letibot_tools::spill::Spiller::new(
                Box::new(letibot_tools::spill::FixedBudget(1_000)),
                Box::new(letibot_tools::spill::MemoryStore::new()),
            ));
        h.rt.invoke(
            "t1",
            &letibot_transcript::ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: r#"{"path":"big.txt"}"#.into(),
            },
            &mut sink,
        );

        let snap = hub.snapshot();
        let call = &snap.turn.expect("a turn is open").calls[0];
        assert_eq!(call.call_id, "c1");
        match &call.state {
            crate::view::CallState::Finished {
                inline_bytes,
                full_bytes,
                spill,
                ..
            } => {
                assert!(inline_bytes < full_bytes, "{inline_bytes} {full_bytes}");
                assert!(spill.is_some(), "the head can offer the rest");
            }
            other => panic!("expected a finished call, got {other:?}"),
        }
    }

    #[test]
    fn the_mapping_is_total() {
        for e in [
            ToolEvent::Started {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                name: "read".into(),
                access: Access::Read,
            },
            ToolEvent::Progress {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                note: "half".into(),
            },
            ToolEvent::Finished {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                outcome: ToolOutcome::Ok,
                payload_digest: "fnv1a:1".into(),
                inline_bytes: 10,
                full_bytes: 90,
                spill: Some("abcd".into()),
                repairs: 2,
                edit: None,
            },
        ] {
            let kind = e.kind();
            assert_eq!(from_tool_event(e).kind(), kind);
        }
    }
}
