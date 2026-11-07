//! The lines an input has sent, newest last, and the walk Up and Down take over them. Keyed by
//! the input's `key` rather than by a channel: a plugin's chat pane keeps its own history with
//! the same code, and this module cannot tell the two apart.

use std::collections::BTreeMap;

/// How many lines one input key keeps. A recall is for the thing just said; this list is in
/// memory, and a longer one is a file.
pub(crate) const CAP: usize = 50;

#[derive(Debug, Default)]
pub(crate) struct History {
    /// By input key, oldest first.
    lines: BTreeMap<String, Vec<String>>,
    /// The key being walked and how far back it is: `None` when no walk is live.
    walk: Option<(String, usize)>,
}

impl History {
    /// Remember a line that went out. A repeat of the newest is not stored twice: Up twice on one
    /// command is a person who is sure, not two lines.
    pub(crate) fn record(&mut self, key: &str, line: &str) {
        self.walk = None;
        let list = self.lines.entry(key.to_string()).or_default();
        if list.last().map(String::as_str) == Some(line) {
            return;
        }
        list.push(line.to_string());
        while list.len() > CAP {
            list.remove(0);
        }
    }

    /// The lines one key holds, newest last.
    pub(crate) fn lines(&self, key: &str) -> &[String] {
        self.lines.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Whether Up and Down belong to this walk rather than to the editor.
    pub(crate) fn walking(&self, key: &str) -> bool {
        matches!(&self.walk, Some((k, _)) if k == key)
    }

    /// One step of the walk: `Some(line)` is what the composer now holds. An empty string is the
    /// step past the newest line, which is the line the person was typing. `None` is nothing to
    /// walk - no history for this key, or a Down with no walk to go back to.
    pub(crate) fn step(&mut self, key: &str, back: bool) -> Option<String> {
        let len = self.lines.get(key).map(Vec::len)?;
        if len == 0 {
            return None;
        }
        let at = match (&self.walk, back) {
            (Some((k, i)), true) if k == key => (*i + 1).min(len - 1),
            (Some((k, 0)), false) if k == key => {
                self.walk = None;
                return Some(String::new());
            }
            (Some((k, i)), false) if k == key => i - 1,
            _ if back => 0,
            _ => return None,
        };
        self.walk = Some((key.to_string(), at));
        self.lines.get(key)?.get(len - 1 - at).cloned()
    }

    /// The walk is over: the line was edited, sent, or another input was bound.
    pub(crate) fn stop(&mut self) {
        self.walk = None;
    }
}
