//! The reseat card and its pinned strip: what a stale prompt looks like on this head. The state and
//! the keys are `app::reseat`'s; this is only the drawing.

use crate::app::reseat::{NOT_NOW, RESEAT_NOW, RESEAT_SUMMARISE};
use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::text::{one, trim_to, wrap};
use rano::style::Role;

/// How many lines the short form of the sentence may take before it says `tab shows all`.
const SHORT_LINES: usize = 3;

/// The short form of the daemon's sentence: what changed, without the explanation. On the card the
/// remedy IS the card and the explanation is one `tab` away, so the short form keeps the first
/// sentence and the lists that name tools; an unrecognised sentence is shown by its first alone.
pub(crate) fn short_form(said: &str) -> String {
    let sentences: Vec<&str> = said
        .split(". ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let mut kept: Vec<String> = Vec::new();
    for (i, s) in sentences.iter().enumerate() {
        let lists = s.starts_with("Seated here")
            || s.starts_with("In the prompt")
            || s.starts_with("Seated on both sides");
        if i == 0 || lists {
            kept.push(s.trim_end_matches('.').to_string());
        }
    }
    format!("{}.", kept.join(". "))
}

impl App {
    /// The open card, split as a decision card is: the content (headline and sentence) may be
    /// windowed by the fit ladder, the choices (the two rows and the keys) never are.
    pub(crate) fn reseat_card_lines(&self, w: usize) -> (Vec<String>, Vec<String>) {
        let Some(c) = self.reseat.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let mut content = vec![one(
            trim_to(
                "PROMPT IS STALE — this session runs on an older prompt than the daemon seats now",
                w,
            ),
            Role::Attention,
        )];
        let short = short_form(&c.said);
        let body = if c.whole {
            c.said.as_str()
        } else {
            short.as_str()
        };
        let mut lines = wrap(body, w.saturating_sub(2).max(1));
        let whole_len = wrap(&c.said, w.saturating_sub(2).max(1)).len();
        // Something is out of view when the short form dropped sentences or was cut to
        // `SHORT_LINES`; the hint says how long the whole is, so a reader knows whether
        // `tab` is one more line or twenty.
        let hidden = !c.whole && (lines.len() > SHORT_LINES || short != c.said.trim());
        if !c.whole {
            lines.truncate(SHORT_LINES);
        }
        content.extend(
            lines
                .into_iter()
                .map(|l| one(format!("  {l}"), Role::Plain)),
        );
        let mut choices = Vec::new();
        for (i, rows) in self.reseat_choice_lines(w).into_iter().enumerate() {
            let picked = i == c.sel.min(1);
            for l in rows {
                choices.push(one(l, if picked { Role::Strong } else { Role::Plain }));
            }
        }
        let more = if c.whole {
            " · tab shows less".to_string()
        } else if hidden {
            format!(" · tab shows all {whole_len} lines")
        } else {
            String::new()
        };
        choices.push(rano::agent::pane::faint(trim_to(
            &format!("  ↑↓ or a row number · enter chooses · esc not now{more}"),
            w,
        )));
        (
            row_strings(&content, self.cfg.palette()),
            row_strings(&choices, self.cfg.palette()),
        )
    }

    /// The card's two rows, wrapped rather than trimmed, because the summarise row is long enough
    /// to lose its reason on an ordinary terminal. The continuation lines sit under the row's words,
    /// and the screen's click stops read the same lines, so a wrapped row is one button.
    pub(crate) fn reseat_choice_lines(&self, w: usize) -> [Vec<String>; 2] {
        let Some(c) = self.reseat.as_ref() else {
            return [Vec::new(), Vec::new()];
        };
        let first = if c.summarise {
            RESEAT_SUMMARISE
        } else {
            RESEAT_NOW
        };
        let row = |i: usize, name: &str| {
            let picked = i == c.sel.min(1);
            let lead = format!("{} {:>2}  ", if picked { "▸" } else { " " }, i + 1);
            let mut out = Vec::new();
            for (k, l) in wrap(name, w.saturating_sub(6).max(1))
                .into_iter()
                .enumerate()
            {
                out.push(trim_to(
                    &if k == 0 {
                        format!("{lead}{l}")
                    } else {
                        format!("      {l}")
                    },
                    w,
                ));
            }
            out
        };
        [row(0, first), row(1, NOT_NOW)]
    }

    /// The pinned strip: the card, abstained from, still on the screen. Drawn while the prompt is
    /// stale and the open card is not the one in the slot - pinned, or open behind a decision or a
    /// picker that holds the slot. Empty when the prompt is current.
    pub(crate) fn reseat_strip(&self, w: usize) -> Vec<String> {
        let Some(c) = self.reseat.as_ref() else {
            return Vec::new();
        };
        if self.reseat_card_up() {
            return Vec::new();
        }
        // Asked for during a turn, it waits for the turn: the daemon runs a reseat on its queue,
        // between turns and never inside one, so the strip stays up and says when it will go.
        let said = if c.asked && self.turn.is_some() {
            "PROMPT IS STALE — reseat queued: it runs when this turn ends · alt-p shows the card"
                .to_string()
        } else if c.asked {
            "PROMPT IS STALE — reseat asked for, waiting for the daemon · alt-p shows the card"
                .to_string()
        } else if c.summarise {
            "PROMPT IS STALE — too big to carry verbatim · alt-r reseats with a summary · alt-p \
             shows the card"
                .to_string()
        } else {
            "PROMPT IS STALE — reseat now? · alt-r reseats now · alt-p shows the card".to_string()
        };
        row_strings(&rano::agent::status::warning(&said, w), self.cfg.palette())
    }
}
