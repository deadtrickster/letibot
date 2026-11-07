//! A rule across the column with a word in it: the day the talk under it begins on, or the first
//! message the reader had not seen when they opened the channel.

use letibot_sessionlog::pane::Divider;
use rano::agent::text::clean_line;
use rano::render::{Line, Span, truncate};
use rano::style::Role as Paint;
use rano::width::text::width;

use crate::ui::chat::Draw;

pub(super) fn lines(d: &Draw<'_>) -> Vec<Line> {
    let word = clean_line(&d.e.text);
    let word = word.trim();
    // The unread divider is the one thing here that is about the reader rather than about the talk
    // under it, so it is drawn the way this head draws *this one is for you*.
    let paint = match d.e.divider {
        Some(Divider::Unread) => Paint::Attention,
        _ => Paint::Faint,
    };
    let head = if word.is_empty() {
        "───".to_string()
    } else {
        format!("── {word} ")
    };
    let rest = d.cols.saturating_sub(width(&head));
    let row = Line::new(vec![Span::role(
        format!("{head}{}", "─".repeat(rest)),
        paint,
    )]);
    vec![truncate(&row, d.cols)]
}
