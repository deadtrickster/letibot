//! **The composer**: the box the person types in, and the completion lines over it.

use crate::app::*;
use crate::render::{trim_to, visible_width};
use crate::ui::*;
use letibot_ui::editor::Editor;
use letibot_ui::style::Role;
use letibot_ui::width;

impl App {
    /// The live completion row shown above the composer while a `/command` or a
    /// `!` line is being typed. A `/` line lists its matches, name plus hint; a
    /// `!` line lists the history's candidates and the model's, with their
    /// provenance. A prefix nothing matches leaves the row empty rather than absent — the
    /// row is a slot and its height does not follow its content (see [`App::completion_slot`]
    /// and the composer block in `compose_screen`) — and Tab will say what went wrong when
    /// it is asked.
    pub(crate) fn completions_line(&mut self, w: usize) -> Option<String> {
        // **One gate, and it is the slot's.** The `/` arm below used to spell the shape
        // out itself, which is the second rule about one row the reservation cannot
        // afford: it would be free to disagree with the height.
        if !self.completion_slot() {
            return None;
        }
        // **The first character decides, and it is read without holding the borrow**:
        // the `!` path needs `&mut self` for the candidate memo, and `editor.text()`
        // hands back a `&str` borrowed from this same head.
        if self.editor.text().starts_with('!') {
            return self.shell_completions_line(w);
        }
        // A `/` line with no whitespace in it, which is what the slot just said.
        let text = self.editor.text();
        let needle = text[1..].replace('_', "-");
        let parts: Vec<String> = self
            .command_names()
            .into_iter()
            .filter(|(name, _)| name.replace('_', "-").starts_with(&needle))
            .map(|(name, hint)| {
                if hint.is_empty() {
                    format!("/{name}")
                } else {
                    format!("/{name} {hint}")
                }
            })
            .collect();
        if parts.is_empty() {
            return None;
        }
        let cfg = &self.cfg;
        Some(dim(cfg, &trim_to(&format!("  {}", parts.join("  ·  ")), w)))
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
        let text = self.editor.text().to_string();
        if !text.starts_with('!') {
            return None;
        }
        // **Only as many candidates as fit on the row.** The line is trimmed to the
        // width at the end anyway, so collecting every match and joining them into a
        // string that is then thrown away is work for nothing — and it is not a small
        // amount of it: measured at **10 ms a frame** on a session whose history holds
        // two thousand commands that all match the prefix, because each one was cloned
        // and the join built the whole of it. The cut is the one `trim_to` would make
        // at the end, made here instead, and it is the same rule the `/` row keeps.
        let mut parts: Vec<String> = Vec::new();
        let mut used = 2usize; // the row's own leading indent
        // **The file names a Tab found**, while the line is still the one they were found
        // for: what a shell lists on a second Tab, shown at once because the row is here.
        if let Some((line, names)) = &self.path_matches
            && *line == text
        {
            for n in names {
                if used >= w {
                    break;
                }
                used += visible_width(n) + SEPARATOR_COLS;
                parts.push(n.clone());
            }
            let cfg = &self.cfg;
            return Some(dim(cfg, &trim_to(&format!("  {}", parts.join("  ·  ")), w)));
        }
        // The history's candidates, plain: a command this session ran is a fact.
        for line in self
            .shell_candidates()
            .iter()
            .filter(|l| l.starts_with(&text))
        {
            if used >= w {
                break;
            }
            used += visible_width(line) + SEPARATOR_COLS;
            parts.push(line.clone());
        }
        // The model's candidates, marked: a proposal is not a fact, and the mark
        // is the provenance. Only the ones cached for this prefix at this
        // position, so a suggestion about a conversation that moved is not drawn.
        let position = self.items.len() as u64;
        if let Some(lines) = self.shell_suggestions.get(&(text.to_string(), position)) {
            for line in lines.iter().filter(|l| l.starts_with(&text)) {
                if used >= w {
                    break;
                }
                let marked = format!("~{line}");
                used += visible_width(&marked) + SEPARATOR_COLS;
                parts.push(marked);
            }
        }
        if parts.is_empty() {
            return None;
        }
        let cfg = &self.cfg;
        Some(dim(cfg, &trim_to(&format!("  {}", parts.join("  ·  ")), w)))
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
        let wall = boxed.then(|| self.cfg.palette().paint(Role::Faint, "│"));
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

    /// One edge of the box, with a legend inlaid at the left and one pinned to
    /// the right.
    ///
    /// `╰────────── ⚠ · ⠹ Responding · 4.2s ─╯`. A legend rather than a
    /// decoration **when there is something to say**: an edge with nothing to
    /// say renders plain, because a row of attention paid for ever for a fact
    /// read once is the mistake the composer's top border already made once.
    /// The right legend yields room to the left one, yields itself by
    /// truncation next, and is dropped before the border is allowed to wrap.
    pub(crate) fn box_edge(
        &self,
        w: usize,
        open: char,
        close: char,
        left: &str,
        right: &str,
    ) -> String {
        let w = w.max(4);
        let inner = w - 2;
        // A legend may arrive already painted — the alarm is in the attention
        // role, the turn's spinner in pending — and `Palette::paint` closes with
        // a plain reset, which restores the *terminal default* and not the grey
        // of the border it is inlaid into. So the border reopens itself on the
        // far side of each legend. Same defect and same fix as
        // `style::Painter::inside`, one layer up: a reset is not a restore.
        let reopen = self.cfg.palette().open(Role::Faint);
        let mut left_text = String::new();
        if !left.is_empty() && inner >= 10 {
            left_text = format!("─ {}{reopen} ", trim_to(left, inner - 4));
        }
        let left_cols = visible_width(&left_text);
        let mut right_text = String::new();
        if !right.is_empty() && inner >= 10 {
            let room = inner.saturating_sub(left_cols + 2);
            if room >= 4 {
                right_text = format!(" {}{reopen} ─", trim_to(right, room));
            }
        }
        let fill = inner.saturating_sub(left_cols + visible_width(&right_text));
        self.cfg.palette().paint(
            Role::Faint,
            &format!("{open}{left_text}{}{right_text}{close}", "─".repeat(fill)),
        )
    }
}
