//! `/search TEXT`: a substring over the messages this channel has loaded, and a request to the
//! room for the ones it has not - the ring is only the window, and the log behind it is the
//! history. The hits are what `n` and `N` step through, and the needle is what the renderer picks
//! them out with.

use serde_json::json;

use super::*;
use crate::app::rooms::{Chan, SEARCH};

/// The most hits one search asks the room for, the most one read returns.
const HITS: u32 = 200;

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

/// The room key the selected channel's messages live in: a room's own id, or the one dm channel.
fn room_of(a: &App) -> Option<String> {
    match a.rooms.selected() {
        Some(Chan::Room(id)) => Some(id),
        Some(Chan::Dm(_) | Chan::Session(_)) => Some("dm".to_string()),
        None => None,
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
    // The ring is not the history: ask the room for the hits its log holds as well, and the
    // answer arrives as an ordinary history frame this head merges into the channel.
    if let Some(room) = room_of(a) {
        a.queued.push(Action::RoomChat {
            op: json!({"type": "search", "room": room, "text": needle, "last": HITS}),
            id: Some(format!("{SEARCH}{room}")),
        });
    }
    if let Some(first) = hits.first() {
        a.rooms_cursor_set(Some(*first));
        a.say(&format!(
            "{n} hit(s) for `{needle}` — n and N step through them"
        ));
    } else {
        a.say(&format!(
            "nothing loaded in this channel matches `{needle}`"
        ));
    }
}

/// The room answered a `/search` with rows out of its log: they are loaded now, so the hits are
/// recomputed over what is loaded and the cursor moves to the first one when the ring had none.
pub(super) fn merged(a: &mut App) {
    let Some(needle) = a.nav.search.as_ref().map(|s| s.needle.clone()) else {
        return;
    };
    let hits = Search::find(a, &needle);
    let had = a.nav.search.as_ref().is_some_and(|s| !s.hits.is_empty());
    let n = hits.len();
    let first = hits.first().copied();
    if let Some(s) = a.nav.search.as_mut() {
        s.hits = hits;
        s.at = s.at.min(n.saturating_sub(1));
    }
    if !had && let Some(seq) = first {
        a.rooms_cursor_set(Some(seq));
        a.say(&format!(
            "{n} hit(s) for `{needle}`, the room's log included — n and N step through them"
        ));
    }
}

pub(super) fn step(a: &mut App, forward: bool) -> std::ops::ControlFlow<Option<Action>> {
    let Some(s) = a.nav.search.as_mut() else {
        return std::ops::ControlFlow::Continue(());
    };
    if s.hits.is_empty() {
        let needle = s.needle.clone();
        a.say(&format!("nothing in this room matches `{needle}`"));
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
