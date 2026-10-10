//! **The standing notes**: the notes the harness reads into the system prompt, one row each,
//! in the order the section carries them — `/standing`.
//!
//! # What a row is, and which half of it is whose
//!
//! A row is the **index's own**: the path the section's heading carries, the abstract it shows
//! for the note, and the **form** the budget gave the file — `verbatim` when the prompt
//! carries the note whole, `indexed` when it did not fit what was left of the budget and the
//! prompt carries its headings and line ranges instead. Those three come from the daemon, and
//! deliberately: the form is decided with the session's token counter, by the one walk that
//! assembles the section, and a pane that re-derived it would be a second opinion about the
//! very thing it exists to show.
//!
//! The **size, the age and whether the file is still there** are this head's, read from the
//! disk at DRAW time — the todos pane's rule (`refresh_repo_todos`), for the same reason: the
//! operator edits and deletes their own notes while a pane is open, and *the index names a
//! note the disk no longer has* is a comparison that only a fresh `stat` can make. It is the
//! one row that must never be drawn as a row that looks fine, because the prompt is still
//! carrying that note's entry.
//!
//! # The words
//!
//! `verbatim` and `indexed` are the section's own — a file the budget could not fit arrives
//! under a heading that says it is `indexed` — so the pane says what the prompt says. Calling
//! it *digested* here would be a second name for one fact, and a second name is how a reader
//! comes to think there are two things.

use crate::app::standing::{Corpus, OnDisk};
use crate::app::*;
use crate::ui::render::{row_strings, wrap};
use letibot_sessionlog::protocol::NoteForm;
use rano::agent::pane::{self, PaneLines};
use rano::agent::text::{bytes_human, clean_line};
use rano::render::{Line, Span};
use rano::style::Role;

impl App {
    /// The standing-notes pane's rows, and the record of where each stop landed — which is
    /// what the arrows scroll by, never arithmetic over the list they drew from.
    ///
    /// **And the corpus's own watch.** The form on a row is the daemon's answer — decided with
    /// the session's token counter, which this head does not have — while the size beside it is
    /// read here, now; so a note edited with the pane open would otherwise draw a new size next
    /// to the form it had before the edit. `repo_todos_at`'s rule for `TODO.md`, one pane over:
    /// one `stat` per note (already taken for the row) plus a `read_dir` per directory, and an
    /// ask when any of it moved.
    pub(crate) fn standing_lines(&mut self, w: usize) -> Vec<String> {
        let (content, corpus) = self.standing_view(w);
        self.standing_stop_rows = content.stop_rows.clone();
        let moved = match &self.standing_corpus {
            Some(prev) => *prev != corpus,
            // **The first draw after the rows landed is the baseline, not a change.** The ask
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

    /// **The list: one stop per note, and one faint line under it for the abstract** — with the
    /// corpus's own signature alongside, folded from the `stat` each row's draw already takes.
    ///
    /// Two lines an entry, like the queue pane's rows and for the same reason: the facts a
    /// reader needs are *what is it* (the path, and the index's own line about it) and *what
    /// state is it in* (the form the budget gave it, its size, its age) — and a single row
    /// would spend the second on the first.
    fn standing_view(&self, w: usize) -> (PaneLines, Corpus) {
        let mut out = PaneLines::new();
        let mut corpus = Corpus::default();
        // **The workspace's own notes directory, watched whether or not it has anything in
        // it**: with no rows to name it, a first note written while the pane is open would
        // otherwise be a change nothing could see, and the pane would sit there saying there
        // are none. The box-wide directory is not named here — this head has no path to it
        // until a row carries one — so a FIRST note there is the one change it cannot notice.
        if !self.wiring.workspace.is_empty() {
            corpus.watch_dir(&std::path::Path::new(&self.wiring.workspace).join(".letibot/notes"));
        }
        let n = self.standing.len();
        out.push(pane::title(format!(
            "standing notes — {n} note(s), in the order the prompt carries them"
        )));
        out.blank();
        if self.standing.is_empty() {
            out.push(pane::faint(
                "    none. The harness reads AGENTS.md, the box-wide notes and this project's \
                 `.letibot/notes/`; it found nothing in any of them. A note is written with \
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
            let left = Line::new(vec![
                Span::raw(format!("{} ", pane::mark(picked))),
                Span::raw(clean_line(&e.path)),
            ]);
            // **The disk's half, read now** — see the module doc. `None` is the file being
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
            // **THE KEEPER'S REPORT GOES HERE, ONE FAINT LINE UNDER THE ABSTRACT.** The
            // notes keeper (`agent/notes-keeper`, a sibling in flight) reports stale
            // references, what changed, and proposals to remove — and this is the row it
            // lands on, exactly as a queue verdict lands on the queue's row: the reader is
            // already here when the question *is this note still true* comes up, and the two
            // lines above are the fields a report would be about. **Its shape is deliberately
            // NOT invented here**: it arrives with the keeper, on its own wire, and this is
            // the place rather than a guess at it.
        }
        // **The legend, only when it has something to say.** The whole point of the form
        // column is that the budget's effect was invisible; one line saying what `indexed`
        // means is what turns a word into a fact a reader can act on.
        if indexed > 0 {
            out.blank();
            out.push(pane::faint(format!(
                "    {indexed} of {n} indexed: they did not fit what was left of the notes \
                 budget, so the prompt carries their headings and line ranges rather than \
                 their text — `read` the path for the rest"
            )));
        }
        (out, corpus)
    }

    /// **One note, open: the text the row's Enter read.** Until Esc, which goes back to the
    /// list rather than out of the pane — the jobs pane's overlay rule, one pane along.
    pub(crate) fn note_open_lines(&mut self, w: usize) -> Vec<String> {
        let Some(v) = self.note_open.as_ref() else {
            return Vec::new();
        };
        let mut lines = vec![
            pane::title(format!("standing note — {}", clean_line(&v.path))),
            // The same disk facts the row carried, read again: a note deleted while it is
            // open says so on the next frame rather than at the next key.
            pane::faint(match OnDisk::read(&v.path) {
                Some(d) => format!(
                    "    {} · last written {}",
                    bytes_human(d.bytes),
                    age_of(d.mtime_ms, self.now_ms)
                ),
                None => "    gone — the file is not there".to_string(),
            }),
            Line::default(),
        ];
        match &v.body {
            Ok(text) => {
                for l in text.lines() {
                    // Wrapped, not truncated: a note is prose, and the reader opened it to
                    // read it. `wrap` is the head's own column arithmetic, so the rows this
                    // produces are what the painter measures.
                    for one in wrap(&clean_line(l), w) {
                        lines.push(Line::raw(one));
                    }
                }
            }
            Err(why) => {
                for one in wrap(&clean_line(why), w) {
                    lines.push(Line::styled(one, Role::Attention));
                }
            }
        }
        lines.push(Line::default());
        lines.push(pane::faint("    esc goes back to the list"));
        row_strings(&lines, self.cfg.palette())
    }
}

/// The form, in the section's own words — `NoteForm` has exactly two, and this is the one
/// place they become text.
fn form_word(form: NoteForm) -> &'static str {
    match form {
        NoteForm::Verbatim => "verbatim",
        NoteForm::Indexed => "indexed",
    }
}

/// **How long ago a note was last written**, from its own mtime.
///
/// The FACT is the file's — `OnDisk` read it — and this is only its rendering, in the register
/// `ps` uses for ages: a reader asking *is this note from this week* wants a word, not an
/// epoch. `None` is an mtime the filesystem would not give, which is said rather than shown as
/// 1970.
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
