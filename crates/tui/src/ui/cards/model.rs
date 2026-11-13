//! The model card, drawn: a search field at the top, then the two levels - the model answering now,
//! the recent ones and a row per provider; or one provider's models, one line each with the window
//! and the two rates lined up in columns. A query replaces either level with one ranked list.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::render::{Line as RLine, Span, Style};
use rano::style::Role;

/// How wide a model's name may grow before it is cut: the columns the row exists for keep their
/// room on a narrow terminal, and a long name loses its tail rather than the numbers.
const NAME_CAP: usize = 52;

impl App {
    /// The model card's rows as this head's strings, and where each takeable row was drawn:
    /// `spans[k]` is the line of the k-th row of the cursor's list, which is what a click reads.
    pub(crate) fn model_card_layout(&self, w: usize) -> (Vec<String>, Vec<std::ops::Range<usize>>) {
        let (card, lines, takes) = self.model_card_rows();
        let widths = self.card_widths(&card, &lines, &takes);
        let mut out: Vec<RLine> = Vec::with_capacity(lines.len() + 1);
        out.push(query_row(
            self.model_query.trim(),
            self.card_note(&takes),
            w,
        ));
        // Why the card is up, when the head opened it (`app::reach`): the model a message was about
        // to go to could not be reached. Under the field and above the list, because it is the
        // reason to be reading it.
        if let Some(line) = &self.reach.reason {
            out.push(rano::agent::pane::faint(format!("  {line}")));
        }
        let mut spans: Vec<std::ops::Range<usize>> = vec![0..0; takes.len()];
        for line in &lines {
            match line {
                Line::Group(title) => out.push(rano::agent::pane::faint(format!("  {title}"))),
                Line::Columns => out.push(column_labels(widths)),
                Line::Model(k) => {
                    let at = out.len();
                    spans[*k] = at..at + 1;
                    match takes.get(*k) {
                        Some(take) => {
                            out.push(self.model_row(take, widths, *k == self.mode_sel, w, &card))
                        }
                        None => out.push(RLine::default()),
                    }
                }
                Line::Provider(k) => {
                    let at = out.len();
                    spans[*k] = at..at + 1;
                    let key = match takes.get(*k) {
                        Some(Take::Provider { key, .. }) => key.clone(),
                        _ => String::new(),
                    };
                    if key == crate::app::NOT_CONNECTED {
                        let n = card.providers.iter().filter(|p| !p.ready).count();
                        out.push(folded_row(n, *k == self.mode_sel, widths));
                        continue;
                    }
                    let p = card.providers.iter().find(|p| p.key == key);
                    out.push(provider_row(
                        p,
                        *k == self.mode_sel,
                        widths,
                        self.keys_row_present(),
                    ));
                }
            }
        }
        // What taking a row does, last: one fact per line, and the same sentence whatever level it is
        // read from, since taking a model is one act.
        for line in Pick::Model.consequence() {
            out.push(rano::agent::pane::faint(format!("  {line}")));
        }
        (row_strings(&out, self.cfg.palette()), spans)
    }

    /// The faint note at the right of the search field: what this level is, in the level's own
    /// words. One row with the query, so it is short.
    fn card_note(&self, takes: &[Take]) -> String {
        if !self.model_query.trim().is_empty() {
            return match takes.len() {
                1 => "1 match".to_string(),
                n => format!("{n} matches"),
            };
        }
        if let Some(p) = self.model_open_provider() {
            let at = self.model_door_at(&p);
            return match p.doors.len() {
                0 | 1 => String::new(),
                n => format!(
                    "{n} doors · tab switches to {}",
                    p.doors
                        .iter()
                        .enumerate()
                        .find(|(i, _)| *i != at)
                        .map(|(_, d)| d.as_str())
                        .unwrap_or_default()
                ),
            };
        }
        // Level one counts every provider, the folded ones too.
        let n = match self.model_open.is_none() {
            true => self.model_card().providers.len(),
            false => takes
                .iter()
                .filter(|t| matches!(t, Take::Provider { .. }))
                .count(),
        };
        match n {
            0 => String::new(),
            n => format!("{n} providers"),
        }
    }

    /// One model's line: its name, then the window and the two rates in the columns the labels
    /// name. A fact line the catalogue does not write that way is drawn whole and faint, because
    /// `local`'s "no key, no meter" has no window to put in a column.
    fn model_row(
        &self,
        take: &Take,
        widths: Widths,
        cursor: bool,
        w: usize,
        card: &ModelCard,
    ) -> RLine {
        let Take::Model { name, facts, at } = take else {
            return RLine::default();
        };
        let shown = self.shown_model(name, at);
        let mut spans = vec![
            Span::raw("  "),
            marks(cursor, is_now(card, name)),
            Span::raw(" "),
        ];
        spans.extend(named(shown, at, widths.name));
        match columns(facts) {
            Some((window, input, output)) => {
                spans.push(Span::raw("  "));
                spans.push(Span::raw(pad_left(&window, widths.window)));
                spans.push(Span::raw("  "));
                spans.push(Span::role(pad_left(&input, widths.input), Role::Faint));
                spans.push(Span::raw("  "));
                spans.push(Span::role(pad_left(&output, widths.output), Role::Faint));
            }
            None => {
                spans.push(Span::raw("  "));
                spans.push(Span::role(facts.clone(), Role::Faint));
            }
        }
        rano::render::text::truncate(&RLine::new(spans), w)
    }

    /// What a model's row says its name is. Under an open provider the heading already names the
    /// door, so the row is the model alone; the switch name it is taken by keeps its door. One rule
    /// for the drawing and the column widths, or the name's column would be sized for a name that is
    /// not on the screen.
    fn shown_model<'a>(&self, name: &'a str, at: &[usize]) -> &'a str {
        match self.model_open.is_some() && at.is_empty() {
            true => name.rsplit('/').next().unwrap_or(name),
            false => name,
        }
    }

    /// The columns of one frame: as wide as what is drawn, and the name capped so the numbers keep
    /// their room. Read off the same rows the frame draws, so a column cannot be a row behind.
    fn card_widths(&self, card: &ModelCard, lines: &[Line], takes: &[Take]) -> Widths {
        let mut w = Widths {
            name: "model".len(),
            label: "provider".len(),
            window: "window".len(),
            input: "in /M".len(),
            output: "out /M".len(),
        };
        for line in lines {
            let Some(Take::Model { name, facts, at }) = line.at(takes) else {
                continue;
            };
            w.name = w
                .name
                .max(self.shown_model(name, at).chars().count().min(NAME_CAP));
            if let Some((window, input, output)) = columns(facts) {
                w.window = w.window.max(window.chars().count());
                w.input = w.input.max(input.chars().count());
                w.output = w.output.max(output.chars().count());
            }
        }
        for p in &card.providers {
            w.label = w.label.max(p.label.chars().count());
        }
        w
    }
}

impl Line {
    /// The row this line draws, when it draws one.
    fn at<'a>(&self, takes: &'a [Take]) -> Option<&'a Take> {
        match self {
            Line::Model(k) | Line::Provider(k) => takes.get(*k),
            _ => None,
        }
    }
}

/// The mark in front of a row: where the cursor is, and whether this is the model answering now.
/// Two characters always, so the names under them line up whether or not either is set.
fn marks(cursor: bool, now: bool) -> Span {
    Span::role(
        format!(
            "{}{}",
            if cursor { "▸" } else { " " },
            if now { "●" } else { " " }
        ),
        if now { Role::Success } else { Role::Faint },
    )
}

/// The search field: the query, a caret, and the level's own words pinned to the right.
fn query_row(query: &str, note: String, w: usize) -> RLine {
    // The caret is the field's own: the composer's is elsewhere, and a card that owned the keys
    // without saying so would look like it took none.
    let mut spans = vec![
        Span::raw("  "),
        Span::role("⌕ ", Role::Faint),
        Span::styled(query.to_string(), Style::of(Role::Strong)),
        Span::role("▏", Role::Faint),
    ];
    let used = 2 + 2 + query.chars().count() + 1;
    if !note.is_empty() && used + note.chars().count() + 2 <= w {
        spans.push(Span::raw(" ".repeat(w - used - note.chars().count() - 2)));
        spans.push(Span::role(note, Role::Faint));
    }
    RLine::new(spans)
}

/// The labels over the model columns, in the columns themselves: which number is the window and
/// which is a rate, and `/M` for what the rates are per.
fn column_labels(w: Widths) -> RLine {
    rano::agent::pane::faint(format!(
        "  {} {}  {}  {}  {}",
        " ".repeat(3),
        pad_right("model", w.name),
        pad_left("window", w.window),
        pad_left("in /M", w.input),
        pad_left("out /M", w.output),
    ))
}

/// One provider: what it is called, whether this box can take it, how many models are under it and
/// what they cost - the four facts a choice between providers is made on. `keys` is whether the
/// daemon published the row that says which providers it holds a key for: without it the state is
/// not "not connected" but unknown, because the head has nothing to judge by.
fn provider_row(p: Option<&Provider>, cursor: bool, w: Widths, keys: bool) -> RLine {
    let Some(p) = p else {
        return RLine::default();
    };
    let state = if p.keyless {
        "no key needed"
    } else if p.ready {
        "keyed"
    } else if !keys {
        "cannot say"
    } else {
        "not connected"
    };
    let count = format!(
        "{} model{}",
        p.models.len(),
        if p.models.len() == 1 { "" } else { "s" }
    );
    RLine::new(vec![
        Span::raw("  "),
        marks(cursor, false),
        Span::raw(" "),
        Span::role(pad_right(&p.label, w.label), Role::Strong),
        Span::raw("  "),
        Span::role(
            pad_right(state, 13),
            if p.ready { Role::Success } else { Role::Faint },
        ),
        Span::raw("  "),
        Span::role(pad_left(&count, 9), Role::Faint),
        Span::raw("  "),
        Span::role(price_range(p), Role::Faint),
    ])
}

/// The one row the providers this box holds no key for fold into: Enter lists them.
fn folded_row(n: usize, cursor: bool, w: Widths) -> RLine {
    RLine::new(vec![
        Span::raw("  "),
        marks(cursor, false),
        Span::raw(" "),
        Span::role(pad_right("not connected", w.label), Role::Faint),
        Span::raw("  "),
        Span::role(pad_right("", 13), Role::Faint),
        Span::raw("  "),
        Span::role(
            pad_left(&format!("{n} provider{}", if n == 1 { "" } else { "s" }), 9),
            Role::Faint,
        ),
    ])
}

/// A model's name, cut to its column, with the characters the query matched tinted: the highlight
/// is why a row is in the list, and a card that could not say would be a lucky guess.
fn named(name: &str, at: &[usize], width: usize) -> Vec<Span> {
    let chars: Vec<char> = name.chars().collect();
    let take = chars.len().min(width);
    let mut spans: Vec<Span> = Vec::new();
    let mut run = String::new();
    let mut hot = false;
    for (i, c) in chars.iter().take(take).enumerate() {
        let lit = at.contains(&i);
        if lit != hot && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), look(hot)));
        }
        hot = lit;
        run.push(*c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, look(hot)));
    }
    if take < width {
        spans.push(Span::raw(" ".repeat(width - take)));
    }
    spans
}

/// The style of a run of a name: the query's own match tinted, the rest plain.
fn look(hot: bool) -> Style {
    match hot {
        true => Style::of(Role::Match),
        false => Style::default(),
    }
}

/// Whether a switch name is the model answering now: the card spells a row with the door in force
/// and the daemon spells `now` with the door it was switched by, so the model part is what is
/// compared - `deepseek-anthropic/x` and `deepseek/x` are one model.
fn is_now(card: &ModelCard, name: &str) -> bool {
    let Some(now) = card.now.as_deref() else {
        return false;
    };
    name == now || name.rsplit('/').next() == now.rsplit('/').next()
}

/// The column widths of one frame.
#[derive(Debug, Clone, Copy, Default)]
struct Widths {
    name: usize,
    label: usize,
    window: usize,
    input: usize,
    output: usize,
}

fn pad_right(s: &str, width: usize) -> String {
    let n = s.chars().count();
    match n < width {
        true => format!("{s}{}", " ".repeat(width - n)),
        false => s.to_string(),
    }
}

fn pad_left(s: &str, width: usize) -> String {
    let n = s.chars().count();
    match n < width {
        true => format!("{}{s}", " ".repeat(width - n)),
        false => s.to_string(),
    }
}
