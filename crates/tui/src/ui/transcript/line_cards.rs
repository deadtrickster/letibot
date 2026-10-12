//! **A notice the daemon writes as a wall of text, drawn as ONE line** — and the whole of it one
//! click away, in the popup the job card opens. The one mechanism behind every such card; the
//! readers are the cards' own modules.
//!
//! The operator asked for it twice. First of a red `main`, drawn as two big blocks: *"please make
//! all this: … a one-line similar to job - with a popup"* (`red_card`). Then, pasting a review
//! that could not decide — dozens of lines of the reviewer's reasons — *"same story about reviews
//! - … one line + popup"* (`review_card`).
//!
//! # The registry
//!
//! A card kind is a reader of the daemon's text: whether a text OPENS with one of its notices,
//! the last words its notices END with (where a row of several notices is split), and the full
//! reading, which hands back a [`Card`] — the line, the part of it drawn in a role, and the
//! popup's title. A session row (a notice spoken in the operator's place) is read by
//! [`SESSION_KINDS`]; a hub warning by [`WARNING_KINDS`], each under the codes it is raised with.
//!
//! # Only what this head can read is folded
//!
//! A reader answers `None` for any shape it does not know, and the row is then drawn as it was
//! before cards existed. **A row must OPEN with a card**: one that opens with anything else is
//! drawn as before, whole. A row that opens with a card and goes on is split after the card's
//! last words only where what follows is another notice this head can open — another card, or a
//! labelled `[job]`/`[task]` group — and the rest is drawn the way it is drawn without a card
//! (the job card's lines, or the fold); a rest that is neither falls the WHOLE row back.
//! Anything after the last words that does not open a notice stays inside the card: a sentence
//! the daemon learned to say is the notice's, and the popup shows it.
//!
//! The record is untouched: the model reads the daemon's words, every one of them.

use std::collections::HashMap;
use std::ops::Range;

use rano::style::Role;

use crate::app::SubagentState;
use crate::ui::render::trim_to;
use crate::ui::transcript::job_card::{JobTail, compact_notice};
use crate::ui::transcript::{asked_card, red_card, review_card};
use crate::ui::*;

/// What every card's line ends with: the row is a summary, and the popup is the text.
pub(crate) const CLICK: &str = "click for details";

/// **One notice, read** — what a card draws for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Card {
    /// The whole line, uncut: `review · agent/x · could not decide · BUT … · click for details`.
    pub(crate) line: String,
    /// The part of the line drawn in [`Card::role`] — the alarm's head, the verdict's word.
    pub(crate) accent: Range<usize>,
    pub(crate) role: Role,
    /// The popup's top edge.
    pub(crate) title: String,
}

/// **A line of a compact row**, cut to its width: a card's (with its accent) or a job card's or
/// a fold's (with none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CardLine {
    pub(crate) text: String,
    pub(crate) accent: Option<(Range<usize>, Role)>,
}

impl CardLine {
    pub(crate) fn plain(text: String) -> CardLine {
        CardLine { text, accent: None }
    }
}

impl Card {
    /// Build a card from its parts, the accent being the first occurrence of `accented` in
    /// `line` at or after byte `from`.
    pub(crate) fn new(
        line: String,
        from: usize,
        accented: &str,
        role: Role,
        title: String,
    ) -> Card {
        let start = line[from..].find(accented).map_or(from, |i| from + i);
        let accent = start..(start + accented.len()).min(line.len());
        Card {
            line,
            accent,
            role,
            title,
        }
    }

    /// **The line cut to `cols`** with the head's `…`, the accent kept to what survived the cut.
    pub(crate) fn cut(&self, cols: usize) -> CardLine {
        let text = trim_to(&self.line, cols);
        let kept = text
            .char_indices()
            .zip(self.line.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((i, c), _)| i + c.len_utf8());
        let end = self.accent.end.min(kept);
        let start = self.accent.start.min(end);
        CardLine {
            text,
            accent: (start < end).then_some((start..end, self.role)),
        }
    }
}

/// **A kind of session notice this head draws as a card.**
pub(crate) struct SessionKind {
    /// Whether `text` opens with one of this kind's notices — read from its first words only.
    pub(crate) opens: fn(&str) -> bool,
    /// The last words this kind's notices end with: where a row of several is split.
    pub(crate) ends: &'static [&'static str],
    /// The whole notice, read; `None` for a shape this head does not know.
    pub(crate) read: fn(&str) -> Option<Card>,
}

/// **A kind of hub warning this head draws as a card**: the codes it is raised under, and the
/// reader of its detail.
pub(crate) struct WarningKind {
    pub(crate) codes: &'static [&'static str],
    pub(crate) read: fn(&str) -> Option<Card>,
}

/// Every session notice drawn as a card, in the order they are tried.
pub(crate) const SESSION_KINDS: &[SessionKind] =
    &[red_card::SESSION, review_card::SESSION, asked_card::SESSION];

/// Every hub warning drawn as a card.
pub(crate) const WARNING_KINDS: &[WarningKind] = &[
    red_card::WARNING,
    red_card::UNJUDGED_WARNING,
    review_card::WARNING,
];

fn kind_of(text: &str) -> Option<&'static SessionKind> {
    SESSION_KINDS.iter().find(|k| (k.opens)(text))
}

/// **Where a row's first notice ends**, when the row opens with a card: `(the notice, the rest,
/// its kind)`. See the module docs for the rule.
pub(crate) fn split_card(text: &str) -> Option<(&str, &str, &'static SessionKind)> {
    let kind = kind_of(text)?;
    let mut split: Option<usize> = None;
    for end in kind.ends {
        for (at, _) in text.match_indices(end) {
            let e = at + end.len();
            if let Some(next) = text[e..].strip_prefix("\n\n")
                && (next.starts_with('[') || kind_of(next).is_some())
            {
                split = Some(split.map_or(e, |s| s.min(e)));
                break;
            }
        }
    }
    Some(match split {
        Some(e) => (&text[..e], &text[e + 2..], kind),
        None => (text, "", kind),
    })
}

/// **The card a session row opens with**, read.
pub(crate) fn session_card(text: &str) -> Option<Card> {
    let (notice, _, kind) = split_card(text)?;
    (kind.read)(notice)
}

/// **Whether a row is one a card draws** — the click map's question, which needs no width.
pub(crate) fn is_card_row(text: &str) -> bool {
    session_card(text).is_some()
}

/// **The card a hub warning is drawn as**, or `None` for a code or a detail no card reads (the
/// note is then drawn as it always was).
pub(crate) fn warning_card(code: &str, detail: &str) -> Option<Card> {
    WARNING_KINDS
        .iter()
        .filter(|k| k.codes.contains(&code))
        .find_map(|k| (k.read)(detail))
}

/// **The popup's top edge** for a card's text — a session row's or a warning's detail (whose
/// code the popup no longer has, so every warning reader is asked; each reads only its own
/// shape) — and the role it is drawn in.
pub(crate) fn popup_title(text: &str) -> Option<(String, Role)> {
    session_card(text)
        .or_else(|| WARNING_KINDS.iter().find_map(|k| (k.read)(text)))
        .map(|c| (c.title, c.role))
}

/// **The compact row**: one line per card, and every other notice of the row drawn the way it
/// is drawn without one (the job card, or the fold). `None` when the row does not open with a
/// card, or when any part of it is a shape this head does not know — the row is then drawn as
/// it was.
pub(crate) fn compact_cards(
    text: &str,
    subagents: &[SubagentState],
    tails: &HashMap<String, JobTail>,
    cols: usize,
) -> Option<Vec<CardLine>> {
    let (notice, rest, kind) = split_card(text)?;
    let card = (kind.read)(notice)?;
    let mut out = vec![card.cut(cols)];
    if !rest.is_empty() {
        if let Some(more) = compact_cards(rest, subagents, tails, cols) {
            out.extend(more);
        } else if let Some(more) = compact_notice(rest, subagents, tails, cols) {
            out.extend(more.into_iter().map(CardLine::plain));
        } else {
            for l in folded_notice(rest, subagents)?.lines() {
                out.push(CardLine::plain(trim_to(l, cols)));
            }
        }
    }
    Some(out)
}

/// Whitespace runs as one space: a line is one line.
pub(crate) fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// **The sentences of a flattened text**: ended by `. `, `; `, `! ` or `? `, trimmed.
pub(crate) fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = text.as_bytes();
    for i in 0..b.len() {
        if matches!(b[i], b'.' | b';' | b'!' | b'?') && b.get(i + 1) == Some(&b' ') {
            let s = text[start..i].trim();
            if !s.is_empty() {
                out.push(s);
            }
            start = i + 2;
        }
    }
    let s = text[start.min(text.len())..]
        .trim()
        .trim_end_matches(['.', ';']);
    if !s.is_empty() {
        out.push(s);
    }
    out
}

/// **A sentence's first clause** — cut at the first `, and` / `, so` / `, which` / `: ` / ` — `
/// far enough in to be a clause, and a cut clause says so with `…`.
pub(crate) fn first_clause(sentence: &str) -> String {
    /// A clause shorter than this is not the reason yet (`BUT, as`, `So: `).
    const AT_LEAST: usize = 12;
    const MARKS: &[&str] = &[", and ", ", so ", ", which ", ": ", " — "];
    let cut = MARKS
        .iter()
        .flat_map(|m| sentence.match_indices(m).map(|(i, _)| i))
        .filter(|&i| i >= AT_LEAST)
        .min();
    match cut {
        Some(i) => format!("{}…", &sentence[..i]),
        None => sentence.to_string(),
    }
}
