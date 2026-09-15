//! The daemon's answer to [`letibot_tools::builtins::harness_view::HarnessFacts`]:
//! what a session may know about itself.
//!
//! The trait lives in `letibot-tools` so that crate keeps depending on nothing
//! above it; the implementation lives here because this is the layer that holds
//! the hub and the config. Same split as every other seam in this tree.
//!
//! **Every field is read at CALL time, not at open.** A session that reported the
//! warnings it had when it started would be answering a question about the past
//! with the confidence of the present — and warnings are exactly what a model
//! asks about because something just happened.

use std::sync::Arc;

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::view::TurnState;
use letibot_tools::builtins::harness_view::HarnessFacts;

/// The disclosures, filled once the session is wired.
///
/// A slot rather than a value because the tool is registered BEFORE the gate,
/// the backend and the seated role exist — they are what the disclosures
/// describe. Same shape as the flowy seat's slot, and for the same reason: the
/// tool has to exist early enough to be in the prompt, and its subject arrives
/// later.
///
/// Filled once and not refreshed, which is honest here: the disclosures describe
/// how the session was WIRED, and that is fixed for its life by construction —
/// `/mode` says so in as many words, "from the NEXT session in this project".
pub type DisclosureSlot = Arc<std::sync::Mutex<Vec<(String, String, String)>>>;

pub struct DaemonFacts {
    hub: Arc<Hub>,
    disclosures: DisclosureSlot,
}

impl DaemonFacts {
    pub fn new(hub: Arc<Hub>, disclosures: DisclosureSlot) -> Self {
        DaemonFacts { hub, disclosures }
    }
}

impl HarnessFacts for DaemonFacts {
    fn disclosures(&self) -> Vec<(String, String, String)> {
        self.disclosures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn warnings(&self) -> Vec<(String, String, u64)> {
        let now = letibot_sessionlog::event::now_ms();
        self.hub
            .snapshot()
            .warnings
            .into_iter()
            .map(|w| {
                (
                    w.code,
                    w.detail,
                    // Age rather than a timestamp: a session's clock is hours old
                    // by the time it reads this, and "four seconds ago" is the
                    // fact a model is actually asking for.
                    now.saturating_sub(w.ts) / 1000,
                )
            })
            .collect()
    }

    fn turn(&self) -> Option<(bool, usize, String)> {
        let snap = self.hub.snapshot();
        let t = snap.turn?;
        Some((
            matches!(t.state, TurnState::Running),
            t.calls.len(),
            t.model,
        ))
    }

    fn heads(&self) -> Vec<(String, String)> {
        self.hub
            .snapshot()
            .heads
            .into_iter()
            .map(|h| (h.kind, h.identity))
            .collect()
    }
}
