//! The three tool-lifecycle events, and the sink they go to. No event carries the payload:
//! an event fans out to every attached head, so a 200 KB result sent to four heads is four
//! copies. Events carry a digest and a byte count; the bytes live in the transcript or a spill.

use letibot_transcript::ToolOutcome;

use crate::edit::ToolEditExcerpt;
use crate::schema::Access;

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // the payload rides the Finished event once per call; boxing it spreads over every constructor
pub enum ToolEvent {
    Started {
        /// Every other turn-scoped event carries one, and without it a head cannot
        /// attribute a call once subagents run concurrently.
        turn_id: String,
        call_id: String,
        name: String,
        /// The declared access class: the fact a head needs to show why a call did or did
        /// not stop for a decision, and it is known the moment the call starts.
        access: Access,
    },
    /// Liveness, and it counts as such. Interactive: a late head is never replayed
    /// partial tool output.
    Progress {
        turn_id: String,
        call_id: String,
        /// Free text, and this is the right shape: `grep` and `glob` do not know a
        /// total until they have finished walking, so a `done/total` pair would be a
        /// denominator invented for the display's benefit.
        note: String,
    },
    Finished {
        turn_id: String,
        call_id: String,
        outcome: ToolOutcome,
        payload_digest: String,
        /// What the model actually received.
        inline_bytes: u64,
        /// What the tool produced, as against what the model received: under spill the two
        /// differ, and that gap is the number that says spilling is working.
        full_bytes: u64,
        /// The spill locator, when the output spilled. Durable, so a head can
        /// offer the rest of it rather than only mentioning that there is more.
        spill: Option<String>,
        /// How many argument repairs this call needed. A head that cannot see this cannot
        /// see a model steadily emitting malformed calls, the signal a dialect is wrong.
        repairs: u32,
        /// Both sides of every file this call changed, bounded to the region that differs
        /// ([`ToolEditExcerpt`]). The one exception to the no-payload rule: the before side
        /// is in neither the transcript nor the model's payload, and is gone once it lands.
        edits: Vec<ToolEditExcerpt>,
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
