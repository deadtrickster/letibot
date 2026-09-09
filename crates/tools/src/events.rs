//! The three tool-lifecycle events, and the sink they go to.
//!
//! §4.5 gives them by name only — `ToolStarted / ToolProgress / ToolFinished{call_id,
//! outcome, …}` — and `TODO.md` T13.4 records that their shapes are W8's invention
//! and that *"W9 will find out whether they are right"*. This is what W9 found,
//! stated as a type. `crates/sessionlog/src/lift_tools.rs` is the mapping into
//! §4.5's enum, and the differences are argued there.
//!
//! The rule the turn engine states about `Delta` applies here for the same reason:
//! **no event carries the payload**. An event fans out to every attached head, and
//! a 200 KB `grep` result sent to four heads is four copies of a thing that is
//! already in the transcript. Events carry a digest and a byte count; the bytes
//! live in the transcript and, when they are large, in the spill store.

use letibot_transcript::ToolOutcome;

use crate::schema::Access;

#[derive(Debug, Clone, PartialEq)]
pub enum ToolEvent {
    Started {
        /// **Not in W8's shape.** Every other turn-scoped event in §4.5 carries
        /// one, and without it a head cannot attribute a call once subagents run
        /// concurrently (§8.4's whole argument for them).
        turn_id: String,
        call_id: String,
        name: String,
        /// **Not in W8's shape.** Clause 4's declaration is the fact a head needs
        /// to show *why* a call did or did not stop for a decision, and it is
        /// known at the moment the call starts.
        access: Access,
    },
    /// Liveness, and §8.5 requires it to count as such. Interactive: a late head is
    /// never replayed partial tool output.
    Progress {
        turn_id: String,
        call_id: String,
        /// Free text, and this **is** the right shape: `grep` and `glob` do not
        /// know a total until they have finished walking, so a `done/total` pair
        /// would be a denominator invented for the display's benefit.
        note: String,
    },
    Finished {
        turn_id: String,
        call_id: String,
        outcome: ToolOutcome,
        payload_digest: String,
        /// What the model actually received.
        inline_bytes: u64,
        /// What the tool produced. **Not in W8's shape**, where a single `bytes`
        /// could not distinguish the two — and under spill they differ, which is
        /// the number that says spilling is working.
        full_bytes: u64,
        /// The spill locator, when the output spilled. Durable, so a head can
        /// offer the rest of it rather than only mentioning that there is more.
        spill: Option<String>,
        /// How many of clause 2's repairs this call needed. A head that cannot see
        /// this cannot see a model steadily emitting malformed calls, which is the
        /// signal that a dialect or a schema is wrong.
        repairs: u32,
    },
}

impl ToolEvent {
    pub fn call_id(&self) -> &str {
        match self {
            ToolEvent::Started { call_id, .. }
            | ToolEvent::Progress { call_id, .. }
            | ToolEvent::Finished { call_id, .. } => call_id,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ToolEvent::Started { .. } => "ToolStarted",
            ToolEvent::Progress { .. } => "ToolProgress",
            ToolEvent::Finished { .. } => "ToolFinished",
        }
    }
}

/// Where tool events go. A trait for the same reason `letibot-turn` uses one: the
/// runtime has no opinion about the transport, and a test can assert on the exact
/// sequence one call produced.
pub trait ToolEventSink {
    fn emit(&mut self, event: ToolEvent);
}

/// Drops everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullToolSink;

impl ToolEventSink for NullToolSink {
    fn emit(&mut self, _event: ToolEvent) {}
}

/// Keeps everything, for tests.
#[derive(Debug, Default)]
pub struct RecordingToolSink {
    pub events: Vec<ToolEvent>,
}

impl RecordingToolSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        self.events.iter().map(|e| e.kind()).collect()
    }
}

impl ToolEventSink for RecordingToolSink {
    fn emit(&mut self, event: ToolEvent) {
        self.events.push(event);
    }
}

/// A short, stable digest of a payload. FNV-1a, the same choice and the same
/// reasoning as `letibot_turn::args_digest`: a correlation aid, not identity.
pub fn payload_digest(payload: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in payload.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}
