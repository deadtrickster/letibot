//! `/search TEXT`: a substring over the messages this channel has loaded - the ring is the history,
//! so a search over the server's log waits for a request that does not exist yet. The hits are what
//! `n` and `N` step through, and the needle is what the renderer picks them out with.

use super::*;

/// What a search holds: the needle, the `seq`s it hits, and which hit the cursor is on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Search {
    pub(crate) needle: String,
    /// The hits, oldest first, as the `seq`s of the loaded messages that carry the needle.
    pub(crate) hits: Vec<u64>,
    /// The index into `hits` the cursor rests on.
    pub(crate) at: usize,
}

impl Search {
    /// The hits of `needle` among the selected channel's messages, in the order a reader reads.
    fn find(a: &App, needle: &str) -> Vec<u64> {
        let Some(c) = a.rooms.selected() else {
            return Vec::new();
        };
        let needle = needle.to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        a.rooms
            .messages(&c)
            .into_iter()
            .filter(|m| m.text.to_lowercase().contains(&needle))
            .map(|m| m.seq)
            .collect()
    }
}

pub(super) fn open(a: &mut App, needle: &str) {
    let needle = needle.trim();
    if needle.is_empty() {
        // `/search` with nothing after it closes the search, which is the way out of the
        // highlighting without leaving the view.
        a.nav.search = None;
        a.say("search closed");
        return;
    }
    let hits = Search::find(a, needle);
    let n = hits.len();
    a.nav.search = Some(Search {
        needle: needle.to_string(),
        hits: hits.clone(),
        at: 0,
    });
    if let Some(first) = hits.first() {
        a.rooms_cursor_set(Some(*first));
        a.say(&format!(
            "{n} hit(s) for `{needle}` — n and N step through them"
        ));
    } else {
        a.say(&format!("nothing in this channel matches `{needle}`"));
    }
}

pub(super) fn step(a: &mut App, forward: bool) -> std::ops::ControlFlow<Option<Action>> {
    let Some(s) = a.nav.search.as_mut() else {
        return std::ops::ControlFlow::Continue(());
    };
    if s.hits.is_empty() {
        let needle = s.needle.clone();
        a.say(&format!("nothing in this channel matches `{needle}`"));
        return std::ops::ControlFlow::Break(None);
    }
    let n = s.hits.len();
    s.at = if forward {
        (s.at + 1) % n
    } else {
        (s.at + n - 1) % n
    };
    let (at, seq) = (s.at, s.hits[s.at]);
    a.rooms_cursor_set(Some(seq));
    let needle = a.rooms_search_needle().unwrap_or("").to_string();
    a.say(&format!("hit {}/{n} for `{needle}`", at + 1));
    std::ops::ControlFlow::Break(None)
}
