//! The map a compaction leaves, as the `recall` tool reads it back: which units were evicted and the
//! window a read of one is bounded by. The map's text is written once by the harness; the ids it
//! names are kept here, findable without a model.

use std::sync::Mutex;

/// Characters per token, the ratio the ledger counts in, so a window in tokens is a size in bytes.
const CHARS_PER_TOKEN: u64 = 4;

/// A read may spend an eighth of the window. A body recalled is part of the conversation from then
/// on, so one line of the map must not fill the room the turn after it needs, and its bound has to
/// follow the window rather than be a constant that happens to fit one model.
const WINDOW_SHARE: u64 = 8;

/// The least one read returns whatever the window is: below this a body arrives in pieces too small
/// to be worth the call, and a small window would make `recall` useless rather than bounded.
pub const MIN_BYTES: usize = 2_000;

/// One unit the map names: what it was, and what it was about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evicted {
    /// The tool whose body went, which is the map line's own word for what this is.
    pub kind: String,
    /// What the call named: the file and its window, the command, or the child session.
    pub named: String,
    /// The body's size, so a reader can weigh a read before making it.
    pub bytes: usize,
}

/// Every unit a session's compactions evicted, oldest first, and the window this session has.
#[derive(Debug, Default)]
pub struct Recall {
    window: Option<u64>,
    units: Mutex<Vec<(String, Evicted)>>,
}

impl Recall {
    /// `window` is the session's own, in the model's tokens; `None` is a session that has no
    /// window to speak of, and then only the floor applies.
    pub fn new(window: Option<u64>) -> Self {
        Recall {
            window,
            units: Mutex::new(Vec::new()),
        }
    }

    /// The most one read returns, in bytes.
    pub fn bound(&self) -> usize {
        let share = self.window.unwrap_or(0) * CHARS_PER_TOKEN / WINDOW_SHARE;
        (share as usize).max(MIN_BYTES)
    }

    /// Add the units one compaction evicted. An id already held keeps its place: a body the store
    /// keys by content is one body, however many compactions evicted it.
    pub fn remember(&self, units: impl IntoIterator<Item = (String, Evicted)>) {
        let mut held = self.units.lock().unwrap_or_else(|e| e.into_inner());
        for (id, unit) in units {
            if held.iter().any(|(had, _)| *had == id) {
                continue;
            }
            held.push((id, unit));
        }
    }

    /// What the map says about one id, or `None` when the map never named it.
    pub fn named(&self, id: &str) -> Option<Evicted> {
        self.units
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(had, _)| had == id)
            .map(|(_, unit)| unit.clone())
    }

    /// Every unit the map names, oldest first: what a refusal lists so the next call can be right.
    pub fn held(&self) -> Vec<(String, Evicted)> {
        self.units.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(named: &str) -> Evicted {
        Evicted {
            kind: "read".into(),
            named: named.into(),
            bytes: 4_096,
        }
    }

    /// The bound follows the window: an eighth of it, floored, and a session with no window gets
    /// the floor. Two windows, two answers, so a constant cannot have replaced this.
    #[test]
    fn the_read_bound_follows_the_window() {
        assert_eq!(
            Recall::new(Some(262_144)).bound(),
            262_144 / 8 * 4,
            "an eighth of a 262k window, in bytes"
        );
        assert_eq!(Recall::new(Some(8_192)).bound(), 8_192 / 8 * 4);
        assert_eq!(
            Recall::new(Some(2_048)).bound(),
            MIN_BYTES,
            "a small window floors at the least a read is worth"
        );
        assert_eq!(
            Recall::new(None).bound(),
            MIN_BYTES,
            "a session with no window has only the floor"
        );
    }

    /// The map is a list and not a set: order is the eviction's, and an id seen twice is one body.
    #[test]
    fn a_unit_is_remembered_once_and_in_the_order_it_was_evicted() {
        let recall = Recall::new(Some(8_192));
        recall.remember([
            ("b".to_string(), unit("src/b.rs")),
            ("a".to_string(), unit("src/a.rs")),
        ]);
        recall.remember([("b".to_string(), unit("src/b.rs"))]);
        let held = recall.held();
        assert_eq!(
            held.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert_eq!(recall.named("a").map(|u| u.named), Some("src/a.rs".into()));
        assert_eq!(recall.named("nope"), None);
    }
}
