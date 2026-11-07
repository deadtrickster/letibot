//! `Ctrl+K`: one list of the channels and the DM peers, fuzzy over their names, unread first, and
//! Enter switches to the row under the cursor. A channel is a row here, not a stream entry, because
//! the list is the HEAD's: it is the same list the channel column draws, and a plugin's stream is
//! what one of these rows holds.

use super::*;
use crate::app::rooms::Chan;

/// The open switcher: what has been typed, and the row under the cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Switcher {
    pub(crate) query: String,
    pub(crate) at: usize,
}

/// One row of the list: the channel, the words the switcher matches it by, and how much is unread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) chan: Chan,
    pub(crate) label: String,
    pub(crate) unread: u64,
}

/// How well a label answers what was typed: `0` for a prefix, `1` for a substring, `2` for the
/// letters in order somewhere, `None` for no match. A fuzzy list that ranked a scattering above a
/// prefix would put `#host` under `host-status-broadcast` for the word `host`.
fn rank(query: &str, label: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let (q, l) = (query.to_lowercase(), label.to_lowercase());
    if l.starts_with(&q) {
        return Some(0);
    }
    if l.contains(&q) {
        return Some(1);
    }
    let mut chars = l.chars();
    q.chars().all(|c| chars.any(|x| x == c)).then_some(2)
}

/// The rows the switcher shows for `query`, unread first and then by how well the label answers it.
pub(crate) fn rows(a: &App, query: &str) -> Vec<Row> {
    let mut out: Vec<(Row, u8)> = Vec::new();
    for c in a.rooms.channels() {
        let label = a.rooms_name(&c);
        // A DM's peer is matched by its own handle: the row is titled `dm with X`, and someone
        // looking for X should not have to type the words this head wraps it in.
        let peer = match &c {
            Chan::Dm(p) => Some(p.as_str()),
            Chan::Room(_) => None,
        };
        let Some(rank) = rank(query, &label).or_else(|| peer.and_then(|p| rank(query, p))) else {
            continue;
        };
        let unread = a.rooms.unread(&c);
        out.push((
            Row {
                chan: c,
                label,
                unread,
            },
            rank,
        ));
    }
    // Unread first, because the list is opened to find what wants answering; then the better
    // match; then the label, so the order is stable between two frames of the same list.
    out.sort_by(|(x, xr), (y, yr)| {
        (x.unread == 0, *xr, x.label.to_lowercase()).cmp(&(
            y.unread == 0,
            *yr,
            y.label.to_lowercase(),
        ))
    });
    out.into_iter().map(|(r, _)| r).collect()
}

/// The rows of the switcher as it stands, in the order `↑` and `↓` walk them.
pub(crate) fn shown(a: &App) -> Vec<Row> {
    match &a.nav.switcher {
        Some(s) => rows(a, &s.query),
        None => Vec::new(),
    }
}

pub(super) fn open(a: &mut App) -> std::ops::ControlFlow<Option<Action>> {
    a.nav.switcher = Some(Switcher::default());
    std::ops::ControlFlow::Break(None)
}

/// The keys of an open switcher. A letter is its query and never the composer's: with a line
/// typed, `Ctrl+K` is readline's kill-to-end and never reaches here (`Guard::EmptyComposer`).
pub(super) fn key(a: &mut App, k: &Key) -> std::ops::ControlFlow<Option<Action>> {
    use std::ops::ControlFlow;
    let Some(s) = a.nav.switcher.as_ref() else {
        return ControlFlow::Continue(());
    };
    let (query, at) = (s.query.clone(), s.at);
    let listed = rows(a, &query);
    match k {
        Key::Esc => {
            a.nav.switcher = None;
            return ControlFlow::Break(None);
        }
        Key::Enter => {
            a.nav.switcher = None;
            let Some(row) = listed.into_iter().nth(at) else {
                return ControlFlow::Break(None);
            };
            a.rooms.sel = Some(row.chan);
            a.rooms.gone = None;
            // A channel is a different stream: the cursor, the search and the tail mark all belong
            // to the one being left.
            a.rooms_cursor_set(None);
            a.nav.search = None;
            a.rooms_to_tail();
            a.rooms.next_poll_ms = 0;
            a.rooms_shown();
            return ControlFlow::Break(None);
        }
        // Every other key is spent on the list being open: a `q` that fell through would start a
        // line nobody could see under the switcher.
        _ => {}
    }
    let last = listed.len().saturating_sub(1);
    if let Some(s) = a.nav.switcher.as_mut() {
        match k {
            Key::Char(c) => {
                s.query.push(*c);
                // A new query re-ranks the list, so the cursor starts at its first row again.
                s.at = 0;
            }
            Key::Backspace => {
                s.query.pop();
                s.at = 0;
            }
            Key::Up => s.at = s.at.min(last).saturating_sub(1),
            Key::Down => s.at = (s.at.min(last) + 1).min(last),
            _ => {}
        }
    }
    ControlFlow::Break(None)
}
