//! `Ctrl+K`'s list, drawn: the query line and the channels it leaves, unread first, with the row
//! under the cursor marked. The model is `app::chat_nav`'s - what is here is only the shape of the
//! rows, so the pane that drops them over the chat spends no arithmetic on them.

#![allow(dead_code)] // The pane that draws it over the chat is P2's; the model it reads is live.

use crate::app::chat_nav::{Row, shown};
use crate::app::*;
use crate::ui::render::{trim_to, visible_width};
use rano::render::{Line, Span};
use rano::style::Role;

/// The rows of an open switcher, at most `height` of them, `width` columns wide. A line each: the
/// query, then one per channel with its unread count at the right, and the row under the cursor in
/// the strong role - the same mark the lists use for `where I am`.
pub(crate) fn lines(a: &App, width: usize, height: usize) -> Vec<Line> {
    let Some(s) = a.nav.switcher.as_ref() else {
        return Vec::new();
    };
    let rows = shown(a);
    if height == 0 {
        return Vec::new();
    }
    let mut out = vec![Line::new(vec![
        Span::role("switch: ", Role::Faint),
        Span::raw(trim_to(&s.query, width.saturating_sub(8))),
    ])];
    let room = height.saturating_sub(1);
    // The window follows the cursor, as every list here does.
    let first = (s.at + 1).saturating_sub(room);
    for (i, r) in rows.iter().enumerate().skip(first).take(room) {
        out.push(row(r, i == s.at, width));
    }
    if rows.is_empty() {
        out.push(Line::new(vec![Span::role(
            "  nothing that name",
            Role::Faint,
        )]));
    }
    out
}

fn row(r: &Row, at: bool, width: usize) -> Line {
    let unread = if r.unread == 0 {
        String::new()
    } else {
        format!(" {}", r.unread)
    };
    let mark = if at { "▸ " } else { "  " };
    let label = trim_to(
        &format!("{mark}{}", r.label),
        width.saturating_sub(visible_width(&unread)),
    );
    let role = if at { Role::Strong } else { Role::Plain };
    Line::new(vec![
        Span::role(label, role),
        Span::role(unread, Role::Attention),
    ])
}
