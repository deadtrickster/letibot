//! The two strips the room composer draws above its field: the row of what a completion answered,
//! and the message the next line answers. Both are rows of the edge, so neither moves the
//! conversation when a key changes what it says.

use letibot_plugin::complete::Completion;
use rano::render::{Line, Span, Style};
use rano::style::Role;
use rano::width::text::{truncate as trim_to, width};

/// The completion row: each candidate as the row shows it - its label, or its insert when a
/// `Complete` named no label - with the faint note that says why or where, and the candidate that
/// is in the line drawn strong. `None` is an answer with nothing in it, which draws no row at all.
pub(crate) fn completion_row(answer: &Completion, live: usize, w: usize) -> Option<Line> {
    if answer.candidates.is_empty() || w < 4 {
        return None;
    }
    let faint = Style::of(Role::Faint);
    let mut spans = vec![Span::styled("  ", faint.clone())];
    let mut used = 2usize;
    for (i, c) in answer.candidates.iter().enumerate() {
        let shown = c.label.clone().unwrap_or_else(|| c.insert.clone());
        let note = c.note.clone().unwrap_or_default();
        let separator = if i > 0 { 3 } else { 0 };
        if used + separator + 4 > w {
            break;
        }
        if separator > 0 {
            spans.push(Span::styled(" · ", faint.clone()));
            used += separator;
        }
        // The note is dropped before the name is: a row that cut the handle of its best candidate
        // has cut the thing the person is choosing between.
        let mut body = shown;
        if !note.is_empty() && used + width(&body) + width(&note) < w {
            body.push(' ');
            body.push_str(&note);
        }
        if used + width(&body) > w {
            body = trim_to(&body, w - used);
        }
        used += width(&body);
        // P2 maps a hue to a colour that reads on dark and light; until then the candidate in the
        // line is the strong one and the rest are faint.
        let role = if i == live { Role::Strong } else { Role::Faint };
        spans.push(Span::styled(body, Style::of(role)));
    }
    if spans.len() == 1 {
        return None;
    }
    let mut line = Line::new(spans);
    line.style = faint;
    Some(line)
}

/// The reply strip: the message the next line answers, one faint row under a mark, trimmed to the
/// width. The words are the caller's - this only places them.
pub(crate) fn reply_strip(quote: &str, w: usize) -> Option<Line> {
    let quote = quote.trim();
    if quote.is_empty() || w < 6 {
        return None;
    }
    let text = trim_to(&format!("↳ {quote}"), w);
    let mut line = Line::new(vec![Span::styled(text, Style::of(Role::Faint))]);
    line.style = Style::of(Role::Faint);
    Some(line)
}
