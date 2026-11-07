//! The unsent text of each input key: switching a channel keeps the line, and coming back finds
//! it as it was left. Only the words are kept - a chip's attachment belongs to the composer's
//! editor and travels with it.

use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub(crate) struct Drafts {
    text: BTreeMap<String, String>,
}

impl Drafts {
    /// Keep the words of the input being left. Empty is not a draft, so a channel left clean comes
    /// back clean rather than restoring a blank line over something typed since.
    pub(crate) fn keep(&mut self, key: &str, text: &str) {
        if text.is_empty() {
            self.text.remove(key);
        } else {
            self.text.insert(key.to_string(), text.to_string());
        }
    }

    /// The words of the input being arrived at, taken: a draft that was restored and then sent
    /// must not come back on the next visit.
    pub(crate) fn take(&mut self, key: &str) -> Option<String> {
        self.text.remove(key)
    }

    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.text.get(key).map(String::as_str)
    }
}
