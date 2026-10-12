//! **The reseat card and its pinned strip** — what a stale prompt looks like on this head. The
//! state and the keys are `app::reseat`'s; this is only the drawing.

use crate::app::reseat::{NOT_NOW, RESEAT_NOW, RESEAT_SUMMARISE};
use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::text::{one, trim_to, wrap};
use rano::style::Role;

/// **How many lines the short form of the sentence may take** before it says `tab shows all`.
const SHORT_LINES: usize = 3;

/// **The short form of the daemon's sentence**: what changed, without the explanation.
///
/// The daemon's sentence is several: *when the session was seated and what has changed since*,
/// a paragraph on why a frozen prompt matters, the tools gained, gone and grown, and the remedy
/// (`/reseat …`). On the card the remedy IS the card and the explanation is for whoever asks,
/// so the short form keeps the first sentence and the lists — the sentences that name tools —
/// and the whole is one `tab` away. A sentence this head does not recognise is shown by its
/// first sentence alone, which is never wrong, only shorter.
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
    /// **The open card**, split as R20 splits a decision: the content (headline and sentence)
    /// may be windowed by the fit ladder, the choices (the two rows and the keys) never are.
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
        let first = if c.summarise {
            RESEAT_SUMMARISE
        } else {
            RESEAT_NOW
        };
        for (i, name) in [first, NOT_NOW].iter().enumerate() {
            let picked = i == c.sel.min(1);
            choices.push(one(
                trim_to(
                    &format!("{} {:>2}  {name}", if picked { "▸" } else { " " }, i + 1),
                    w,
                ),
                if picked { Role::Strong } else { Role::Plain },
            ));
        }
        let more = if c.whole {
            " · tab shows less".to_string()
        } else if hidden {
            format!(" · tab shows all {whole_len} lines")
        } else {
            String::new()
        };
        choices.push(rano::agent::pane::faint(trim_to(
            &format!("  ↑↓ · enter chooses · esc not now{more}"),
            w,
        )));
        (
            row_strings(&content, self.cfg.palette()),
            row_strings(&choices, self.cfg.palette()),
        )
    }

    /// **The pinned strip**: the card, abstained from, still on the screen. Drawn while the
    /// prompt is stale and the open card is not the one in the slot — pinned, or open behind a
    /// decision or a picker that holds the slot. Empty when the prompt is current.
    pub(crate) fn reseat_strip(&self, w: usize) -> Vec<String> {
        let Some(c) = self.reseat.as_ref() else {
            return Vec::new();
        };
        if self.reseat_card_up() {
            return Vec::new();
        }
        let said = if c.asked {
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
