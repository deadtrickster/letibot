//! The standing notes, one row each in the order the section carries them. The path, abstract and
//! form come from the daemon, because the form is decided with the session's token counter; size,
//! age and presence are read from disk at draw time, since an open pane outlives an edit.

use crate::app::standing::{Corpus, OnDisk};
use crate::app::*;
use crate::ui::render::{path_span, row_strings};
use letibot_sessionlog::protocol::NoteForm;
use rano::agent::pane::{self, PaneLines};
use rano::agent::text::{bytes_human, clean_line};
use rano::render::{Line, Span, Style};
use rano::style::Role;

impl App {
    /// The pane's rows, and where each stop landed - what the arrows scroll by, never
    /// arithmetic over the list. The corpus watch compares the `stat` each row already takes,
    /// so a note edited under the pane draws a new size beside a form the daemon decided.
    pub(crate) fn standing_lines(&mut self, w: usize) -> Vec<String> {
        let (content, corpus) = self.standing_view(w);
        self.standing_stop_rows = content.stop_rows.clone();
        let moved = match &self.standing_corpus {
            Some(prev) => *prev != corpus,
            // The first draw after the rows landed is the baseline, not a change. The ask
            // that produced them was a moment ago, and re-asking here would be a round trip
            // per pane-open for nothing.
            None => false,
        };
        self.standing_corpus = Some(corpus);
        if moved {
            self.queued.push(Action::ListNotes);
        }
        row_strings(&content.lines, self.cfg.palette())
    }

    /// The list: one stop per note, one faint line under it for the abstract, and the corpus
    /// signature folded from the `stat` each row already takes. Two lines an entry, so that
    /// what it is and what state it is in stay two facts a reader can take apart.
    fn standing_view(&self, w: usize) -> (PaneLines, Corpus) {
        let mut out = PaneLines::new();
        let mut corpus = Corpus::default();
        // The project's notes directory is watched even when empty: with no rows to name it, a
        // first note written under an open pane would be a change nothing sees.
        if let Some(dir) = &self.standing_dir {
            corpus.watch_dir(std::path::Path::new(dir));
        }
        let n = self.standing.len();
        out.push(pane::title(format!(
            "standing notes — {n} note(s), in the order the prompt carries them"
        )));
        out.blank();
        if self.standing.is_empty() {
            out.push(pane::faint(
                "    none. The harness reads AGENTS.md, the box-wide notes and this project's \
                 notes repository; it found nothing in any of them. A note is written with \
                 the `notes` tool.",
            ));
            return (out, corpus);
        }
        let cursor = self.standing_sel.min(n.saturating_sub(1));
        let mut indexed = 0usize;
        for (i, e) in self.standing.iter().enumerate() {
            if e.form == NoteForm::Indexed {
                indexed += 1;
            }
            let picked = i == cursor;
            let disk = OnDisk::read(&e.path);
            corpus.note(&e.path, disk.as_ref());
            // The printed path as a link where the terminal speaks OSC 8; `file_url` decides
            // which URL opens it. `Style::new()` rather than a role, because a terminal
            // without links must get the same bytes as one with them.
            let left = Line::new(vec![
                Span::raw(format!("{} ", pane::mark(picked))),
                path_span(
                    self.cfg.links.as_deref(),
                    &clean_line(&e.path),
                    Style::new(),
                ),
            ]);
            // The disk's half, read now - see the module doc. `None` is the file being
            // gone, and it is said in the same column the size and the age would be, so a row
            // that cannot answer them does not look like one that answered them badly.
            let right = match &disk {
                Some(d) => Line::styled(
                    format!(
                        "{} · {} · {}",
                        bytes_human(d.bytes),
                        age_of(d.mtime_ms, self.now_ms),
                        form_word(e.form)
                    ),
                    Role::Faint,
                ),
                None => Line::styled("gone — the file is not there", Role::Attention),
            };
            let row = pane::split_row(left, right, w);
            out.push_stop(pane::picked(row, picked));
            // The abstract, marked as derived when it is: the index says so for the same
            // reason, and a reader who cannot tell the harness's reading of a note from the
            // author's own line is taking a guess for a statement.
            match &e.abstract_line {
                Some(a) => out.push(pane::faint(format!(
                    "    {}{}",
                    clean_line(a),
                    if e.abstract_written {
                        ""
                    } else {
                        "  (derived)"
                    }
                ))),
                None => out.push(pane::faint(
                    "    (no prose in this note — the index has no abstract for it)",
                )),
            }
            // The keeper's report lands here, one faint line under the abstract, as a queue
            // verdict lands on the queue's row: the reader is already asking whether this
            // note is still true. Its shape arrives with the keeper, not invented here.
        }
        // The legend only when it has something to say: the form column exists because the
        // budget's effect was invisible, and one line on what `indexed` means turns the word
        // into a fact a reader can act on with Enter.
        if indexed > 0 {
            out.blank();
            out.push(pane::faint(format!(
                "    {indexed} of {n} indexed: they did not fit what was left of the notes \
                 budget, so the prompt carries their headings and line ranges rather than \
                 their text — enter opens the file, which has all of it"
            )));
        }
        (out, corpus)
    }
}

/// The form, in the section's own words - `NoteForm` has exactly two, and this is the one
/// place they become text.
fn form_word(form: NoteForm) -> &'static str {
    match form {
        NoteForm::Verbatim => "verbatim",
        NoteForm::Indexed => "indexed",
    }
}

/// How long ago a note was last written, from its own mtime. The fact is `OnDisk`'s and this
/// is only its rendering, in `ps`'s register for ages. `None` is an mtime the filesystem would
/// not give, said rather than shown as 1970.
fn age_of(mtime_ms: Option<u64>, now_ms: u64) -> String {
    let Some(at) = mtime_ms else {
        return "at an unknown time".into();
    };
    let secs = now_ms.saturating_sub(at) / 1000;
    match secs {
        0..=59 => "under a minute ago".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=2_591_999 => format!("{} d ago", secs / 86_400),
        _ => format!("{} mo ago", secs / 2_592_000),
    }
}
