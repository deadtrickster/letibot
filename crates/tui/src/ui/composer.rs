//! The composer: the box the person types in, and the completion lines over it.

use crate::app::*;
use crate::ui::render::{row, trim_to};
use letibot_ui::editor::Editor;
use rano::agent::composer::{Candidate, completions_line};
use rano::render::Line;
use rano::width::text as width;

impl App {
    /// The completions as a line, for the composer's bottom edge: the edge is always there, so
    /// typing `/` moves nothing, where a row above the box was a row the conversation gave up.
    pub(crate) fn completions_legend(&mut self, w: usize) -> Option<Line> {
        if !self.completion_slot() {
            return None;
        }
        if self.editor.text().starts_with('!') {
            return self.shell_completions_legend(w);
        }
        let text = self.editor.text();
        let needle = text[1..].replace('_', "-");
        let items: Vec<Candidate> = self
            .command_names()
            .into_iter()
            .filter(|(name, _)| name.replace('_', "-").starts_with(&needle))
            .map(|(name, hint)| Candidate {
                text: if hint.is_empty() {
                    format!("/{name}")
                } else {
                    format!("/{name} {hint}")
                },
                proposed: false,
            })
            .collect();
        completions_line(&items, w)
    }

    /// The live `!` completion row, with provenance: the session's own commands are drawn plain
    /// and the model's proposals carry a leading `~`, so a reader can tell a suggestion from a
    /// recollection. The mark is display-only - Tab fills the line itself, never the marked form.
    #[cfg(test)]
    pub(crate) fn shell_completions_line(&mut self, w: usize) -> Option<String> {
        let p = self.cfg.palette();
        self.shell_completions_legend(w).map(|l| row(&l, p))
    }

    /// [`App::shell_completions_line`] as a line, for the composer's bottom edge.
    pub(crate) fn shell_completions_legend(&mut self, w: usize) -> Option<Line> {
        let text = self.editor.text().to_string();
        if !text.starts_with('!') {
            return None;
        }
        let plain = |t: &String| Candidate {
            text: t.clone(),
            proposed: false,
        };
        // The paths under the word being typed, when the reader has them for this line, and
        // then only those: a path is what the word is, and a history line beside it would be an
        // answer to a different question.
        if let Some((line, names)) = &self.path_matches
            && *line == text
        {
            let items: Vec<Candidate> = names.iter().map(plain).collect();
            return Some(completions_line(&items, w).unwrap_or_default());
        }
        // The history's own lines first, then the model's proposals for this point of the
        // conversation, each marked `~` so a reader can tell a suggestion from a recollection.
        let mut items: Vec<Candidate> = self
            .shell_candidates()
            .iter()
            .filter(|l| l.starts_with(&text))
            .map(plain)
            .collect();
        let position = self.items.len() as u64;
        if let Some(lines) = self.shell_suggestions.get(&(text.to_string(), position)) {
            items.extend(
                lines
                    .iter()
                    .filter(|l| l.starts_with(&text))
                    .map(|l| Candidate {
                        text: l.clone(),
                        proposed: true,
                    }),
            );
        }
        completions_line(&items, w)
    }

    /// The composer's rows and the caret's `(row, column)`. Each row is padded to the full
    /// width because `term::paint` erases with `\x1b[K`: a row that stops early would leave the
    /// field's right wall hanging in space.
    pub(crate) fn composer_rows(
        &self,
        w: usize,
        max_rows: usize,
        boxed: bool,
    ) -> (Vec<String>, usize, usize) {
        let inner = self.composer_cols();
        let (lines, (crow, ccol)) = if self.secret.is_some() {
            // A dot per character, and the caret after the last one. The text
            // itself is never rendered, not even to compute a width.
            let n = self.secret_buf.chars().count();
            (vec!["•".repeat(n)], (0, n))
        } else if self.prompt.is_some() && !self.prompt_away {
            // The prompt card's field is drawn IN THE OPEN - the text as typed - and that
            // difference from a password's dots is what keeps the two channels apart: this is a
            // line for a program's stdin, on a card that says so. `prompt_buf` is its own field.
            let mut e = Editor::new();
            e.insert(&self.prompt_buf);
            e.render(inner, self.cfg.palette())
        } else if self.key_ask.is_some() {
            // A provider key is masked exactly as a password is - a dot per character, the text
            // never rendered, not even to compute a width. The card above the composer says what
            // the dots are for; this is the surface they are typed on.
            let n = self.key_buf.chars().count();
            (vec!["•".repeat(n)], (0, n))
        } else {
            self.editor.render(inner, self.cfg.palette())
        };
        let n = lines.len().max(1);
        let show = max_rows.clamp(1, n);
        // Scroll to the row being edited, never to the top: a composer taller than
        // the rows it was given must still show the caret, or the person is typing
        // somewhere they cannot see.
        let start = crow.saturating_sub(show - 1).min(n - show);
        let mut out = Vec::with_capacity(show);
        // The wall is drawn once, not once per row: it is the same bytes on every row and
        // `paint` allocates. `Role::Faint` rather than `sgr::GREY`, whose 90 sits within a hair
        // of the background on light themes; this de-emphasises the reader's own foreground.
        let wall = boxed.then(|| row(&rano::agent::pane::faint("│"), self.cfg.palette()));
        for i in start..start + show {
            let body = lines.get(i).cloned().unwrap_or_default();
            out.push(match &wall {
                Some(wall) => format!("{wall} {}{wall}", width::fit(&body, inner + 1)),
                None => trim_to(&body, w),
            });
        }
        let col = if boxed { ccol + 2 } else { ccol };
        (out, crow.saturating_sub(start), col)
    }
}
