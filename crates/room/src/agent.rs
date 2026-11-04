//! A tag's own agent: its derived session id, how many times it was reseated, and when the current one was seated.
//! Whether it is up is a fact about a socket and the room already answers it.
//! The ledger is not a room field because an agent outlives every item under its tag.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// One tag's agent: the derived session id, how many times it was brought back, and when the one now
/// seated began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagAgent {
    /// The queue it owns, normalised (`crate::normalise_tag`).
    pub tag: String,
    /// `crate::tag_session`'s answer - never a name the daemon invented.
    pub session: String,
    /// How many times this tag's agent had to be reseated. A number that keeps climbing is a tag
    /// something keeps killing, and it is said rather than hidden.
    pub reseats: u32,
    /// When the agent now seated was seated, so `reseats: 2` can be read as *twice in four minutes*
    /// rather than *twice, ever*.
    pub since_ms: u64,
}

/// The counts, shared by the daemon that reseats and the server that answers the pane: `Arc`-shared
/// and interior-mutable because neither of those is the room's one writer.
#[derive(Debug, Default)]
pub struct AgentLedger {
    agents: Mutex<BTreeMap<String, TagAgent>>,
}

impl AgentLedger {
    pub fn new() -> Arc<AgentLedger> {
        Arc::new(AgentLedger::default())
    }

    /// This tag's agent is seated - the first time, or the same one found alive. Idempotent: the count
    /// and the clock do not move for an agent that was already there, because nothing happened to it.
    pub fn seat(&self, tag: &str, session: &str, now: u64) -> TagAgent {
        let tag = key(tag);
        let mut g = self.lock();
        let entry = g.entry(tag.clone()).or_insert_with(|| TagAgent {
            tag: tag.clone(),
            session: session.to_string(),
            reseats: 0,
            since_ms: now,
        });
        // A ledger that was told a different session for one tag is following a derivation that
        // moved; the count belongs to the TAG, so it is kept and the name is corrected.
        entry.session = session.to_string();
        entry.clone()
    }

    /// This tag's agent is back - it was gone, and the daemon has just seated it again. That is the
    /// one event the count exists for, and why `seat` and `reseat` are two calls: a caller that has
    /// not checked liveness must not be able to inflate the number.
    pub fn reseat(&self, tag: &str, session: &str, now: u64) -> TagAgent {
        let tag = key(tag);
        let mut g = self.lock();
        let entry = g.entry(tag.clone()).or_insert_with(|| TagAgent {
            tag: tag.clone(),
            session: session.to_string(),
            reseats: 0,
            since_ms: now,
        });
        entry.session = session.to_string();
        entry.reseats = entry.reseats.saturating_add(1);
        entry.since_ms = now;
        entry.clone()
    }

    /// Every tag that has an agent, in tag order - what the pane is handed.
    pub fn all(&self) -> Vec<TagAgent> {
        self.lock().values().cloned().collect()
    }

    pub fn of(&self, tag: &str) -> Option<TagAgent> {
        self.lock().get(&key(tag)).cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, TagAgent>> {
        self.agents.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The room's own spelling of a tag, as a key - `normalise_tag` and nothing else, so `RAM` and `ram`
/// are one agent rather than two entries pointing at one session. A private copy of that rule would be
/// the drift this module exists to avoid.
fn key(tag: &str) -> String {
    crate::normalise_tag(tag).unwrap_or_else(|_| tag.trim().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seat_is_idempotent_and_a_reseat_is_counted() {
        let ledger = AgentLedger::new();
        let first = ledger.seat("ram", "s-room-k-ram", 1_000);
        assert_eq!((first.reseats, first.since_ms), (0, 1_000));
        // Found alive again: nothing happened, so nothing moves.
        let again = ledger.seat("ram", "s-room-k-ram", 9_000);
        assert_eq!((again.reseats, again.since_ms), (0, 1_000));
        // Gone and brought back: that is the fact the count is for.
        let back = ledger.reseat("ram", "s-room-k-ram", 10_000);
        assert_eq!((back.reseats, back.since_ms), (1, 10_000));
        assert_eq!(ledger.reseat("ram", "s-room-k-ram", 11_000).reseats, 2);
    }

    /// `RAM` and `ram` are one queue, so they are one agent: two entries for one session would be a
    /// pane drawing two agents for one tag.
    #[test]
    fn the_ledger_folds_two_spellings_of_one_tag() {
        let ledger = AgentLedger::new();
        ledger.seat("RAM", "s-room-k-ram", 1_000);
        ledger.seat(" ram ", "s-room-k-ram", 2_000);
        assert_eq!(ledger.all().len(), 1, "{:?}", ledger.all());
        assert_eq!(ledger.of("ram").map(|a| a.tag), Some("ram".to_string()));
        assert_eq!(ledger.of("RAM").map(|a| a.since_ms), Some(1_000));
    }

    #[test]
    fn the_ledger_is_per_tag_and_answers_in_tag_order() {
        let ledger = AgentLedger::new();
        ledger.seat("ram", "s-room-k-ram", 1);
        ledger.reseat("ram", "s-room-k-ram", 2);
        ledger.seat("disk", "s-room-k-disk", 3);
        assert_eq!(
            ledger
                .all()
                .iter()
                .map(|a| a.tag.as_str())
                .collect::<Vec<_>>(),
            vec!["disk", "ram"],
            "a pane reads them in one order"
        );
        assert_eq!(ledger.of("ram").map(|a| a.reseats), Some(1));
        assert_eq!(ledger.of("disk").map(|a| a.reseats), Some(0));
        assert_eq!(ledger.of("nothing"), None);
    }
}
