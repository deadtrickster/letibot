//! One entry's rows: the header with its facts at the right edge, the quote of what it answers, the
//! body under both, and the facts a narrow column could not hold beside the header.

use letibot_sessionlog::pane::Cell;
use rano::agent::text::clean_line;
use rano::render::{Line, Span, Style, truncate};
use rano::style::Role as Paint;
use rano::width::text::width;

use crate::ui::chat::{Draw, body};

/// One fact a layer put on an entry: its text, and the paint it means.
type Fact = (String, Paint);

/// The rows one entry takes. A continuation - an entry the layer gave no head - keeps its time in
/// the gutter of its first row, which is where the header it did not get would have put it.
pub(super) fn lines(d: &Draw<'_>) -> Vec<Line> {
    let facts: Vec<Fact> =
        d.e.facts
            .iter()
            .map(|c| (clean_line(&c.text), crate::ui::severity::role(c.role)))
            .collect();
    let (beside, under) = parted(d, &facts);
    let mut rows = Vec::new();
    if !d.e.head.is_empty() {
        rows.push(header(d, &beside));
    }
    // The quote sits above the answer: what it answers, then the answer.
    if let Some(quote) = d.e.quote.as_deref() {
        rows.push(under_gutter(d, |spans| {
            spans.push(Span::role(clean_line(quote), Paint::Faint));
        }));
    }
    rows.extend(body::lines(d));
    if !under.is_empty() {
        rows.push(under_gutter(d, |spans| {
            for (i, (text, paint)) in under.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::role(text.clone(), *paint));
            }
        }));
    }
    // A continuation's time lands in the gutter of its first row, which is the quote it answers or
    // the first line of its body - so it is stamped once those rows exist.
    if d.e.head.is_empty()
        && let (Some(time), Some(first)) = (d.e.time.as_deref(), rows.first_mut())
    {
        stamp(first, time, d);
    }
    if d.ctx.selected == Some(d.e.id())
        && let Some(first) = rows.first_mut()
    {
        first.style = Style::of(Paint::Selected);
    }
    rows
}

/// A row under the gutter, cut to the column.
fn under_gutter(d: &Draw<'_>, fill: impl FnOnce(&mut Vec<Span>)) -> Line {
    let mut spans = vec![Span::raw(" ".repeat(d.gutter()))];
    fill(&mut spans);
    truncate(&Line::new(spans), d.cols)
}

/// The header: the time, the speaker in its hue with the marks after it, and the facts at the
/// right edge - the `#seq` a reply names, and the rows this message is about.
fn header(d: &Draw<'_>, facts: &[Fact]) -> Line {
    let gutter = d.gutter();
    let held = facts_width(facts);
    // A fact never takes the last column of the head: a header with no word in it is a seq on its
    // own, which is not a header.
    let room = d.cols.saturating_sub(gutter + held + usize::from(held > 0));
    let head = if room >= 2 {
        truncate(&Line::new(cells(&d.e.head)), room)
    } else {
        Line::default()
    };
    let mut spans = vec![Span::raw(" ".repeat(gutter))];
    if let Some(time) = d.e.time.as_deref() {
        spans[0] = Span::role(format!("{:<gutter$}", clean_line(time)), Paint::Faint);
    }
    let used = gutter + head.width();
    spans.extend(head.spans);
    spans.push(Span::raw(" ".repeat(d.cols.saturating_sub(used + held))));
    for (i, (text, paint)) in facts.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::role(text.clone(), *paint));
    }
    truncate(&Line::new(spans), d.cols)
}

/// The head's cells as spans, one space apart. A cell that named no role takes the plain one.
fn cells(list: &[Cell]) -> Vec<Span> {
    let mut out = Vec::new();
    for (i, c) in list.iter().enumerate() {
        if i > 0 {
            out.push(Span::raw(" "));
        }
        out.push(Span::styled(
            clean_line(&c.text),
            Style::of(crate::ui::severity::role(c.role)),
        ));
    }
    out
}

/// The facts the header can hold with a word before them, and the ones it cannot. A ref cut in
/// half says nothing, so the rest go under the body rather than into the margin.
fn parted(d: &Draw<'_>, facts: &[Fact]) -> (Vec<Fact>, Vec<Fact>) {
    let (mut beside, mut under) = (Vec::new(), Vec::new());
    let mut held = 0usize;
    for (i, (text, paint)) in facts.iter().enumerate() {
        let gap = usize::from(i > 0);
        let need = d.gutter() + held + gap + width(text) + HEAD_FLOOR;
        if beside.is_empty() || need <= d.cols {
            held += gap + width(text);
            beside.push((text.clone(), *paint));
        } else {
            under.push((text.clone(), *paint));
        }
    }
    (beside, under)
}

/// The columns a header keeps for its speaker, whatever its facts want: a name cut to three letters
/// is not a name, and the ref it was cut for is under the body anyway.
const HEAD_FLOOR: usize = 4;

fn facts_width(facts: &[Fact]) -> usize {
    facts
        .iter()
        .map(|(text, _)| width(text))
        .sum::<usize>()
        .saturating_add(facts.len().saturating_sub(1))
}

/// A continuation's time in the gutter of its first row. A run that kept no time at all would be a
/// paragraph with no place in the day.
fn stamp(row: &mut Line, time: &str, d: &Draw<'_>) {
    let gutter = d.gutter();
    if gutter == 0 || row.spans.is_empty() {
        return;
    }
    row.spans[0] = Span::role(format!("{:<gutter$}", clean_line(time)), Paint::Faint);
}
