//! **The composer**: the box the person types in, and the completion lines over it.

use crate::app::*;
use crate::ui::render::{row, trim_to};
use letibot_ui::editor::Editor;
use rano::agent::composer::{Candidate, completions_line};
use rano::render::Line;
use rano::width::text as width;

impl App {
    /// The live completion row shown above the composer while a `/command` or a
    /// `!` line is being typed. A `/` line lists its matches, name plus hint; a
    /// `!` line lists the history's candidates and the model's, with their
    /// provenance. A prefix nothing matches leaves the row empty rather than absent — the
    /// row is a slot and its height does not follow its content (see [`App::completion_slot`]
    /// and the composer block in `compose_screen`) — and Tab will say what went wrong when
    /// it is asked.
    pub(crate) fn completions_line(&mut self, w: usize) -> Option<String> {
        let p = self.cfg.palette();
        self.completions_legend(w).map(|l| row(&l, p))
    }

    /// **The completions as a line, for the composer's bottom edge** — where they are drawn, so
    /// that typing `/` moves nothing: the operator, 2026-10-08, *"when i type / this gray hint
    /// line appears and conversation jumps one line. i hate that. leticl does this gray thing
    /// instead of the bottom input border line and nothing jumps"*. The edge is always there;
    /// a row above the box was a row the conversation gave up the moment a `/` was typed.
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

    /// **The live `!` completion row, with provenance.**
    ///
    /// The history's candidates — the commands this session actually ran — are
    /// facts, and are drawn plain. The model's candidates are proposals, not
    /// facts, and are drawn marked with a leading `~`, because **a line that
    /// looks like the operator typed it and did not is the same class of lie as
    /// an unattributed quote**: the operator has to be able to tell, at a glance,
    /// which candidates are the session's own and which the model invented.
    ///
    /// The mark is display-only: a Tab fills the composer with the line itself
    /// (`! git status`), never the marked form (`~! git status`). And nothing
    /// here is submitted — the row is a typing aid, and Enter is still the
    /// operator's.
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
        // **The paths under the word being typed, when the reader has them for this line** —
        // and then only those: a path is what the word is, and a history line beside it would
        // be an answer to a different question.
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

    /// The composer's rows, and the caret's `(row, column)` within them.
    ///
    /// The editor lays out the text; this puts a wall on each side of it and pads
    /// to the full width, so the row is a *field* and not a line of text that
    /// happens to be at the bottom. Padding matters for more than looks:
    /// `term::paint` erases each row it rewrites with `\x1b[K`, and a row that
    /// stops early leaves the field's right wall hanging in space.
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
        } else if self.prompt.is_some() {
            // **The prompt card's field is drawn IN THE OPEN** — the text as typed, the same
            // editor renderer the composer uses — and that difference from the password's
            // dots is the whole of what keeps the two channels apart. What this carries is a
            // line for a program's stdin, on a card that says so; a secret has its own card
            // and its own masked field. The text never reaches the composer's history either:
            // `prompt_buf` is a field of its own.
            let mut e = Editor::new();
            e.insert(&self.prompt_buf);
            e.render(inner, self.cfg.palette())
        } else if self.key_ask.is_some() {
            // **A provider key is masked exactly as a password is** — a dot per character,
            // the text never rendered, not even to compute a width. The card above the
            // composer says what the dots are for; this is the surface they are typed on.
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
        // **The wall is drawn once, not once per row.** It is the same two bytes of the same
        // register for every row of the composer — `paint` allocates a `String` — and this loop
        // used to rebuild it inside, which is a per-row allocation for a value that cannot vary.
        //
        // `Role::Faint`, not `sgr::GREY`. 90 is the theme's *bright black*, which `style.rs`
        // measured landing within a hair of the background on several light themes; the attribute
        // de-emphasises whatever foreground the reader has already chosen.
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
