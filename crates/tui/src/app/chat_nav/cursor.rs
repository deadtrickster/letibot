//! The message cursor: one stop per message, so an arrow moves by message and never by the screen
//! row a message happened to wrap onto. Tab puts it on the newest; Up and Down then walk the
//! channel's messages; Esc gives it back to the composer.

use super::*;
use crate::app::rooms::{Chan, Msg};

/// The `seq`s of the selected channel's entries, oldest first - the list the cursor walks. One
/// stop is one message: the rows a message is drawn on are the renderer's business, not the
/// cursor's.
pub(crate) fn stops(a: &App) -> Vec<u64> {
    match a.rooms.selected() {
        Some(c) => a.rooms.messages(&c).into_iter().map(|m| m.seq).collect(),
        None => Vec::new(),
    }
}

pub(super) fn r#move(a: &mut App, up: bool) -> std::ops::ControlFlow<Option<Action>> {
    let stops = stops(a);
    // No cursor is the arrows' own meaning, which is the channel switch: only Tab puts a cursor on
    // a message, and only then do the arrows walk messages.
    let Some(at) = a
        .rooms_cursor()
        .and_then(|at| stops.iter().position(|s| *s == at))
    else {
        return std::ops::ControlFlow::Continue(());
    };
    let last = stops.len() - 1;
    // A cursor stays on its stop at the ends: wrapping a chat would jump the reader from the oldest
    // message to the newest, which is what `End` is for and says so.
    let next = match (up, at) {
        (true, 0) => 0,
        (true, i) => i - 1,
        (false, i) => (i + 1).min(last),
    };
    a.rooms_cursor_set(Some(stops[next]));
    std::ops::ControlFlow::Break(None)
}

/// Tab: the cursor onto the newest message, so the arrows walk messages from then on. `false` for a
/// channel with nothing in it - there is no message to put it on.
pub(super) fn newest(a: &mut App) -> bool {
    match stops(a).last() {
        Some(top) => {
            a.rooms_cursor_set(Some(*top));
            true
        }
        None => false,
    }
}

/// The message under the cursor, with its text as it was said.
fn selected(a: &App) -> Option<(u64, Msg)> {
    let at = a.rooms_cursor()?;
    let c = a.rooms.selected()?;
    let m = a.rooms.messages(&c).into_iter().find(|m| m.seq == at)?;
    Some((m.seq, m.clone()))
}

pub(super) fn copy(a: &mut App) -> std::ops::ControlFlow<Option<Action>> {
    let Some((seq, m)) = selected(a) else {
        return std::ops::ControlFlow::Continue(());
    };
    // OSC 52: the head hands the text to the terminal, as the conversation's `/copy` does.
    a.clipboard_out = Some(m.text.clone());
    let lines = m.text.lines().count();
    a.say(&format!("copied #{seq} — {lines} line(s)"));
    std::ops::ControlFlow::Break(None)
}

pub(super) fn thread(a: &mut App) -> std::ops::ControlFlow<Option<Action>> {
    let Some((seq, _)) = selected(a) else {
        return std::ops::ControlFlow::Continue(());
    };
    let Some(Chan::Room(room)) = a.rooms.selected() else {
        // A DM has no `read{thread}`: the two principals' lines are already the whole exchange.
        a.say(&format!("#{seq} is a direct message, so it has no thread"));
        return std::ops::ControlFlow::Break(None);
    };
    // The root of a reply is the `thread` its own row carries - `t` on a reply shows the thread it
    // belongs to rather than an empty one under the reply.
    let root = a
        .rooms
        .messages(&Chan::Room(room.clone()))
        .into_iter()
        .find(|m| m.seq == seq)
        .and_then(|m| m.thread)
        .unwrap_or(seq);
    a.nav.thread = Some(root);
    a.queued.push(Action::RoomChat {
        op: serde_json::json!({"type": "read", "room": room, "thread": root}),
        id: None,
    });
    let replies = a
        .rooms
        .messages(&Chan::Room(room))
        .into_iter()
        .filter(|m| m.thread == Some(root))
        .count();
    a.say(&format!(
        "thread of #{root}: {replies} repl{}",
        if replies == 1 { "y" } else { "ies" }
    ));
    a.rooms_cursor_set(Some(root));
    std::ops::ControlFlow::Break(None)
}

pub(super) fn enter(a: &mut App) -> std::ops::ControlFlow<Option<Action>> {
    let Some((seq, m)) = selected(a) else {
        return std::ops::ControlFlow::Continue(());
    };
    // A picture first: the overlay is what the round trip is for. The first picture the message
    // carries, in the order the coordinator listed them.
    if let Some(att) = m.attachments.iter().find(|a| a.mime.starts_with("image/"))
        && a.room_open_picture(&att.id)
    {
        return std::ops::ControlFlow::Break(None);
    }
    // Otherwise the first link the body names, opened the way a click on one is.
    if let Some(url) = first_link(&m.text) {
        let act = a.open_link(&url);
        return std::ops::ControlFlow::Break(act);
    }
    let _ = seq;
    a.say("this message carries nothing to open — `R` replies, `y` copies it");
    std::ops::ControlFlow::Break(None)
}

/// The first `http` URL in a body. A room's text is markdown, so a link is written either bare or
/// as `[label](url)`; both spell the URL the same way, and one reader serves both.
fn first_link(text: &str) -> Option<String> {
    let at = ["https://", "http://"]
        .iter()
        .filter_map(|p| text.find(p))
        .min()?;
    let rest = &text[at..];
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, ')' | '>' | '"'))
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}
