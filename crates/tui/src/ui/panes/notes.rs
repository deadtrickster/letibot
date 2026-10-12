//! **The notes**: the disclosures this head has shown, folded and unfolded (`/notes`) —
//! drawn by `rano::agent::notes` from the notes this head holds.

use crate::app::*;
use crate::ui::render::{RenderConfig, row_strings, trim_to};
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
        // **A question's answer, drawn in the faint register rano already has for a
        // line that is nobody's verdict.** `view::Note::Decided` would force one of
        // four permission words onto it; `view::Note::NotRun` is *"· {detail}"*, which
        // is what a settled thing nobody is waiting on should look like.
        Note::Answered {
            summary, by, said, ..
        } => view::Note::NotRun {
            detail: format!("{summary} — {by}: {said}"),
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

/// One note, as the transcript draws it: ONE line a note (R89) — a card's ([`card_row`]) — and a
/// pane's ending whole. See `rano::agent::notes::Note::lines` for the pane's.
pub(crate) fn note_lines(cfg: &RenderConfig, n: &Note) -> Vec<String> {
    match card_row(cfg, n) {
        Some(rows) => rows,
        None => row_strings(&note_view(n).lines(cfg.width), cfg.palette()),
    }
}

/// **The mark and the role a warning's lead is drawn in** — its code's register in the census
/// (`letibot_sessionlog::warning`), rano's three marks for the three.
pub(crate) fn class_mark(code: &str) -> (&'static str, rano::style::Role) {
    use letibot_sessionlog::warning::Class;
    use rano::style::Role;
    match letibot_sessionlog::warning::class(code) {
        Class::Routine => ("·", Role::Faint),
        Class::Refused => ("×", Role::Pending),
        Class::Failure => ("!", Role::Failure),
    }
}

/// **A note's card**: its one line, and the text its popup shows whole — `None` for a pane's
/// ending, which is drawn whole on purpose (see [`PANE_LAST_LINES`]) and for a settled
/// permission, which rano draws as its outcome.
///
/// A hub warning a card reads is that card (`line_cards::WARNING_KINDS`): a red target's, a
/// review's, a restart's, a stale prompt's. **Every other warning is a card too** (R89) — the
/// operator's *"make them the usual one liners"*: `<code> · <first clause> · click for details`,
/// the code in its register (`notice_card::warning_notice`). Before, such a note was three wrapped
/// lines and `… +N lines · /notes` that could not be clicked, and a compaction was one line ending
/// `· /notes` that could not be clicked either and said its code twice.
pub(crate) fn note_card(
    n: &Note,
) -> Option<(crate::ui::transcript::line_cards::Card, String, u64)> {
    use crate::ui::transcript::line_cards::warning_card;
    use crate::ui::transcript::notice_card::{warning_notice, whole_parts};
    use rano::style::Role;
    match n {
        Note::Warned(w) => {
            let card = warning_card(&w.code, &w.detail).unwrap_or_else(|| {
                let (mark, role) = class_mark(&w.code);
                warning_notice(&w.code, &w.detail, mark, role)
            });
            Some((card, w.detail.clone(), w.ts))
        }
        // **A refusal nobody made**, in the faint register and with the mark it has always had:
        // what was not run, in the head's own one sentence.
        Note::NotRun(w) => Some((
            whole_parts("·", Role::Faint, &w.detail),
            w.detail.clone(),
            w.ts,
        )),
        // An answer carries no clock of its own.
        Note::Answered {
            summary, by, said, ..
        } => {
            let text = format!("{summary} — {by}: {said}");
            Some((whole_parts("·", Role::Faint, &text), text, 0))
        }
        Note::Pane { .. } | Note::Decided(_) => None,
    }
}

/// **A note as a card's one line** — framed the way a session row's card is (`session ·`, the
/// faint text, the clock; `blocks::session_card_block`), so one card looks the same whichever
/// way it arrived: a `‼ main RED` from a `target_red` warning and from an `EMERGENCY —` row were
/// two framings of one fact (R89).
///
/// A click opens the text whole (`App::note_cards`), and `/notes` lists it unfolded as before.
pub(crate) fn card_row(cfg: &RenderConfig, n: &Note) -> Option<Vec<String>> {
    let (card, _, ts) = note_card(n)?;
    let line = card.cut(crate::ui::session_text_cols(ts, cfg));
    Some(crate::ui::session_card_block(&[line], ts, cfg))
}

/// The whole note, with no fold — what `/notes` lists and what the transcript shows the head
/// of. One renderer for both, so the listing cannot disagree with the screen about the text.
#[cfg(test)]
pub(crate) fn note_lines_unfolded(cfg: &RenderConfig, n: &Note) -> Vec<String> {
    row_strings(&note_view(n).unfolded(cfg.width), cfg.palette())
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::view::Warned;

    fn warned(code: &str, detail: &str) -> Note {
        Note::Warned(Warned {
            code: code.into(),
            detail: detail.into(),
            ts: 1,
        })
    }

    /// **A compaction is one row and the model's record is not on it** — the figures, the code
    /// once, and the hint; a click opens the whole sentence (R89: it was `· /notes`, unclickable,
    /// and said its code twice).
    #[test]
    fn a_compaction_is_one_line_whatever_note_lines_says() {
        let cfg = RenderConfig {
            width: 100,
            color: false,
            ..RenderConfig::default()
        };
        let detail = "compacted: 958397 → 100005 tokens, on transcript s#t25. Nothing of the \
                      summary turn reached this screen — it ran over a scratch transcript — so \
                      here is what the model now reads in place of everything before it:\n\n## \
                      Goal\n\nTHE MODEL'S OWN RECORD";
        let n = warned("compacted", detail);
        let rows = note_lines(&cfg, &n);
        assert_eq!(rows.len(), 1, "a compaction's row is one line: {rows:?}");
        let row = &rows[0];
        assert!(
            row.contains("session · compacted · 958k → 100k tokens · click for details"),
            "{row:?}"
        );
        assert_eq!(
            row.matches("compacted").count(),
            1,
            "the code once: {row:?}"
        );
        assert!(
            !row.contains("THE MODEL'S OWN RECORD"),
            "the model's record is not the conversation's row: {row:?}"
        );
        // **And nothing was swallowed**: the popup's text is the whole detail.
        let (_, text, _) = note_card(&n).expect("a card");
        assert_eq!(text, detail);
        // **Every warning is one line now** — a code no card reads included, and a failure:
        // its reason's first clause, the rest a click away.
        for code in [
            "ledger_chain_mismatch",
            "auto_compact_failed",
            "a_code_from_next_week",
        ] {
            let other = warned(code, &"a sentence long enough to fold. ".repeat(12));
            let rows = note_lines(&cfg, &other);
            assert_eq!(rows.len(), 1, "{code}: {rows:?}");
            assert!(
                rows[0].contains(&format!(
                    "{code} · a sentence long enough to fold · click for details"
                )),
                "{rows:?}"
            );
        }
    }

    /// **A long line is cut in its facts, and the hint survives** (R89: the hint at the end of
    /// a line was the first thing a narrow terminal cut).
    #[test]
    fn a_long_account_is_trimmed_and_the_hint_is_kept() {
        let cfg = RenderConfig {
            width: 80,
            color: false,
            ..RenderConfig::default()
        };
        let detail = format!(
            "auto_compact: {}. {}",
            "leaving less than the headroom the next turn needs — compacting now".repeat(3),
            "This is the wall, not a judgement about the conversation."
        );
        let rows = note_lines(&cfg, &warned("auto_compact", &detail));
        assert_eq!(rows.len(), 1, "still one row at a narrow width: {rows:?}");
        assert!(rows[0].contains('…'), "the elision is visible: {rows:?}");
        assert!(
            rows[0].contains("· click for details"),
            "and it says how to read the rest: {rows:?}"
        );
    }
}
