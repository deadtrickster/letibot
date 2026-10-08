//! **The notes**: the disclosures this head has shown, folded and unfolded (`/notes`) —
//! drawn by `rano::agent::notes` from the notes this head holds.

use crate::app::*;
use crate::ui::render::{RenderConfig, row_strings};
use rano::agent::notes::{self as view, Listed, NoteClass, NotesList, Settled};

impl App {
    /// The `/notes` listing: every note this head holds, in the order the
    /// conversation has them, numbered for `/notes dismiss N`.
    ///
    /// **Including the ones it is not drawing** (R19). A note from before this window is a
    /// fact this head holds and has chosen not to plant in the conversation, so the
    /// listing is where a reader finds it — marked, for the same reason a retired one is:
    /// two different reasons for an absence must not look like one.
    pub(crate) fn notes_lines(&self) -> Vec<String> {
        let list = NotesList {
            notes: self
                .notes
                .iter()
                .map(|(place, note)| Listed {
                    note: note_view(note),
                    retired: self.is_retired(note),
                    before: matches!(place, Placed::Before),
                })
                .collect(),
        };
        row_strings(&list.lines(self.cfg.width), self.cfg.palette())
    }
}

/// **How many of a pane's last rows its ending carries** — the cap that replaces the fold.
///
/// See [`note_lines`]: a pane's ending is not folded to `rano::agent::notes::NOTE_LINES`, so it needs a bound of
/// its own, and four is chosen the way `NOTE_LINES` was: the common case is one row (a program
/// that dies with a sentence about why), a full-screen program leaves a whole rectangle, and a
/// rectangle is not what a transcript row is for. What is past the cap is *not* kept anywhere —
/// a pane's bytes are not recorded, see `harnessd`'s own TODO — so this is a disclosure
/// decision and not a fold over a record.
pub(crate) const PANE_LAST_LINES: usize = 4;

/// **A note, in rano's words.** The register a warning is drawn in is its code's class, and
/// the class is `letibot_sessionlog::warning`'s — the log's vocabulary, not this head's
/// (R19, R29 part two) — so it is decided here and handed over.
pub(crate) fn note_view(n: &Note) -> view::Note {
    match n {
        Note::Warned(w) => {
            use letibot_sessionlog::warning::Class;
            view::Note::Warned {
                code: w.code.clone(),
                detail: w.detail.clone(),
                class: match letibot_sessionlog::warning::class(&w.code) {
                    Class::Routine => NoteClass::Routine,
                    Class::Refused => NoteClass::Refused,
                    Class::Failure => NoteClass::Failure,
                },
            }
        }
        Note::NotRun(w) => view::Note::NotRun {
            detail: w.detail.clone(),
        },
        // **The register is the operator's own act, and it is a fact this head holds**:
        // `closed` is the head's own record that it sent `Action::TermClose`, never a match
        // on the daemon's wording of the reason.
        Note::Pane {
            line,
            said,
            reason,
            closed,
        } => view::Note::Pane {
            line: line.clone(),
            said: said.clone(),
            reason: reason.clone(),
            closed: *closed,
        },
        Note::Decided(d) => {
            use letibot_sessionlog::event::DecisionOutcome as O;
            view::Note::Decided {
                summary: d.summary.clone(),
                outcome: match &d.outcome {
                    O::Selected { option_id } if option_id.starts_with("allow") => {
                        Settled::Allowed(option_id.clone())
                    }
                    O::Selected { option_id } => Settled::Refused(option_id.clone()),
                    O::Cancelled => Settled::Cancelled,
                    O::TimedOut => Settled::TimedOut,
                },
                by_kind: d.by.kind.clone(),
                by_identity: d.by.identity.clone(),
                basis: d.basis.clone(),
                late: d.late,
            }
        }
    }
}

/// One note, as the transcript draws it: at most `NOTE_LINES` lines and a seam — a pane's
/// ending whole. See `rano::agent::notes::Note::lines`.
pub(crate) fn note_lines(cfg: &RenderConfig, n: &Note) -> Vec<String> {
    row_strings(&note_view(n).lines(cfg.width), cfg.palette())
}

/// The whole note, with no fold — what `/notes` lists and what the transcript shows the head
/// of. One renderer for both, so the listing cannot disagree with the screen about the text.
#[cfg(test)]
pub(crate) fn note_lines_unfolded(cfg: &RenderConfig, n: &Note) -> Vec<String> {
    row_strings(&note_view(n).unfolded(cfg.width), cfg.palette())
}
