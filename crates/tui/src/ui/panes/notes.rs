//! The notes: the disclosures this head has shown, folded and unfolded (`/notes`)  -
//! drawn by `rano::agent::notes` from the notes this head holds.

use crate::app::*;
use crate::ui::render::{RenderConfig, row_strings};
use rano::agent::notes::{self as view, Listed, NoteClass, NotesList, Settled};

impl App {
    /// The `/notes` listing: every note this head holds, in the order the conversation has them,
    /// numbered for `/notes dismiss N` - including the ones it is not drawing, marked, because two
    /// different reasons for an absence must not look like one.
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

/// How many of a pane's last rows its ending carries - the cap that replaces the fold, since a
/// pane's ending is not folded to `rano::agent::notes::NOTE_LINES`. What is past it is not kept
/// anywhere, so this is a disclosure decision and not a fold over a record.
pub(crate) const PANE_LAST_LINES: usize = 4;

/// A note, in rano's words. The register a warning is drawn in is its code's class, and
/// the class is `letibot_sessionlog::warning`'s - the log's vocabulary, not this head's
/// - so it is decided here and handed over.
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
        // A question's answer, drawn in the faint register rano already has for a line that is
        // nobody's verdict: `view::Note::Decided` would force one of four permission words onto it,
        // while `view::Note::NotRun` is the dot and the detail a settled thing should look like.
        Note::Answered {
            summary, by, said, ..
        } => view::Note::NotRun {
            detail: format!("{summary} — {by}: {said}"),
        },
        // The register is the operator's own act, and it is a fact this head holds:
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
        // The settled row and the one still trying are one register in `/notes`, the dot and
        // the sentence: the listing is what a reader scrolls for the words, and the card is what
        // they see in the conversation.
        Note::Retrying(r) => view::Note::NotRun {
            detail: r.line().to_string(),
        },
        Note::Decided(d) => {
            use letibot_sessionlog::event::DecisionOutcome as O;
            view::Note::Decided {
                summary: d.summary.clone(),
                outcome: match &d.outcome {
                    O::Selected { option_id } if is_allow(option_id) => {
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

/// One note, as the transcript draws it: ONE line a note - a card's ([`card_row`]) - and a
/// pane's ending whole. See `rano::agent::notes::Note::lines` for the pane's.
pub(crate) fn note_lines(cfg: &RenderConfig, n: &Note) -> Vec<String> {
    match card_row(cfg, n) {
        Some(rows) => rows,
        None => row_strings(&note_view(n).lines(cfg.width), cfg.palette()),
    }
}

/// The mark and the role a warning's lead is drawn in, both from the log's own class: the mark is
/// the register's glyph and the role is `Class::role`'s, through the head's one mapping.
pub(crate) fn class_mark(code: &str) -> (&'static str, rano::style::Role) {
    use letibot_sessionlog::warning::Class;
    let class = letibot_sessionlog::warning::class(code);
    let mark = match class {
        Class::Routine => "·",
        Class::Refused => "×",
        Class::Failure => "!",
    };
    (mark, crate::ui::severity::role(Some(class.role())))
}

/// A note's card: its one line, and the text its popup shows whole - `None` for a pane's ending,
/// drawn whole on purpose because the rows a program dies saying are the useful ones. Every other
/// note is a card, so none of them is a `... +N lines` seam that cannot be clicked.
pub(crate) fn note_card(
    n: &Note,
) -> Option<(crate::ui::transcript::line_cards::Card, String, u64)> {
    use crate::ui::transcript::line_cards::{CLICK, Card, warning_card};
    use crate::ui::transcript::notice_card::{short, warning_notice, whole_parts};
    use rano::style::Role;
    match n {
        Note::Warned(w) => {
            // Both halves of the register come from `warning::class`: `warning_card` sets a card's
            // role from it, and `class_mark` the fallback line's.
            let card = warning_card(&w.code, &w.detail).unwrap_or_else(|| {
                let (mark, role) = class_mark(&w.code);
                warning_notice(&w.code, &w.detail, mark, role)
            });
            Some((card, w.detail.clone(), w.ts))
        }
        // A refusal nobody made, in the faint register and with the mark it has always had:
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
        // A settled decision is a notice too. Its words are rano's, read back from the renderer rather than written a
        // second time, so the row and the listing cannot disagree. Every line of them, because a summary that carries
        // a newline puts the verdict on the last one.
        Note::Decided(d) => {
            let said = note_view(n)
                .unfolded(UNBOUNDED)
                .into_iter()
                .map(|l| l.plain())
                .collect::<Vec<_>>()
                .join(" ");
            if said.trim().is_empty() {
                return None;
            }
            // The row leads with the verdict. rano writes it last, at the end of a sentence
            // that begins with the summary, and a card cuts from its right end - so at eighty
            // columns an allow and a refuse drew alike.
            let (word, role) = settled(&d.outcome);
            let card = Card::new(
                format!("{word} · {said} · {CLICK}"),
                0,
                &word,
                role,
                format!("decided · {}", short(&d.summary, 60)),
            );
            Some((card, said, 0))
        }
        // The model not answering, while it is still being handled: ONE line in the Warning role -
        // something went wrong and is being retried, which is neither the red of a failure nor the
        // quiet of a routine row. No mark, because the code is not on the line.
        Note::Retrying(r) => {
            let card = Card::new(
                format!("{} · {CLICK}", r.line()),
                0,
                r.line(),
                Role::Warning,
                format!("model retry · {}", r.host),
            );
            Some((card, retry_popup(r), r.ts))
        }
        Note::Pane { .. } => None,
    }
}

/// The answers that let the call or the grant through: `allow_*` on a permission card,
/// `grant_*` on a filesystem-view or memory card.
fn is_allow(option_id: &str) -> bool {
    option_id.starts_with("allow") || option_id.starts_with("grant")
}

/// A width no note wraps at, so [`rano::agent::notes::Note::unfolded`] answers its one sentence.
const UNBOUNDED: usize = 1_000_000;

/// The word and the register a settled decision is drawn in, named together because the row leads
/// with the one and is painted in the other.
fn settled(outcome: &letibot_sessionlog::event::DecisionOutcome) -> (String, rano::style::Role) {
    use letibot_sessionlog::event::DecisionOutcome as O;
    use rano::style::Role;
    match outcome {
        O::Selected { option_id } if is_allow(option_id) => {
            (format!("allowed ({option_id})"), Role::Success)
        }
        O::Selected { option_id } => (format!("REFUSED ({option_id})"), Role::Failure),
        O::Cancelled => ("cancelled".to_string(), Role::Pending),
        // A deadline is not an answer, and must not read like one.
        O::TimedOut => (
            "NOT ANSWERED — the deadline decided it".to_string(),
            Role::Failure,
        ),
    }
}

/// The popup a retry row opens: the row's own line whole - the frame cut it - and every failed
/// attempt with its clock, which is the shape of the outage a reader asks about and the one thing a
/// row cannot say.
fn retry_popup(r: &Retry) -> String {
    // Nothing to list: a header with no attempts under it is worse than the line alone.
    if r.log.is_empty() {
        return r.line().to_string();
    }
    let mut text = format!(
        "{}\n\n{} · every failed attempt, oldest first:",
        r.line(),
        r.host
    );
    for attempt in &r.log {
        text.push('\n');
        text.push_str(attempt);
    }
    text
}

/// A note as a card's one line, framed the way a session row's card is, so one card looks the same
/// whichever way it arrived: the same fact from a warning and from a transcript row was two
/// framings of it. A click opens the text whole; `/notes` lists it unfolded as before.
pub(crate) fn card_row(cfg: &RenderConfig, n: &Note) -> Option<Vec<String>> {
    let (card, _, ts) = note_card(n)?;
    let line = card.cut(crate::ui::session_text_cols(ts, cfg));
    Some(crate::ui::session_card_block(&[line], ts, cfg))
}

/// The whole note, with no fold - what `/notes` lists and what the transcript shows the head
/// of. One renderer for both, so the listing cannot disagree with the screen about the text.
#[cfg(test)]
pub(crate) fn note_lines_unfolded(cfg: &RenderConfig, n: &Note) -> Vec<String> {
    row_strings(&note_view(n).unfolded(cfg.width), cfg.palette())
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::view::Warned;

    /// A view or memory grant the operator chose is an allow, not a refusal.
    #[test]
    fn a_chosen_grant_is_drawn_as_allowed_and_a_deny_as_refused() {
        use letibot_sessionlog::event::DecisionOutcome as O;
        let pick = |id: &str| {
            settled(&O::Selected {
                option_id: id.into(),
            })
            .0
        };
        for id in [
            "grant_ro",
            "grant_rw",
            "grant_memory",
            "grant_memory_session",
        ] {
            assert_eq!(pick(id), format!("allowed ({id})"));
        }
        assert_eq!(pick("deny"), "REFUSED (deny)");
    }

    fn warned(code: &str, detail: &str) -> Note {
        Note::Warned(Warned {
            code: code.into(),
            detail: detail.into(),
            ts: 1,
        })
    }

    /// A compaction is one row and the model's record is not on it - the figures, the code
    /// once, and the hint; a click opens the whole sentence, which must not be an unclickable `· /notes`
    /// or say its code twice.
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
        // And nothing was swallowed: the popup's text is the whole detail.
        let (_, text, _) = note_card(&n).expect("a card");
        assert_eq!(text, detail);
        // Every warning is one line now - a code no card reads included, and a failure:
        // its reason's first clause, the rest a click away. Codes with no action clause, because
        // this row is cut in its facts and what survives the cut is the width's business.
        for code in [
            "ledger_chain_mismatch",
            "reseat_unchecked",
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

    /// A long line is cut in its facts, and the hint survives: the hint at the end of
    /// a line is the first thing a narrow terminal cuts.
    #[test]
    fn a_long_account_is_trimmed_and_the_hint_is_kept() {
        let cfg = RenderConfig {
            width: 80,
            color: false,
            ..RenderConfig::default()
        };
        let detail = format!(
            "compacting: {}. {}",
            "leaving less than the headroom the next turn needs — compacting now".repeat(3),
            "This is the wall, not a judgement about the conversation."
        );
        let rows = note_lines(&cfg, &warned("compacting", &detail));
        assert_eq!(rows.len(), 1, "still one row at a narrow width: {rows:?}");
        assert!(rows[0].contains('…'), "the elision is visible: {rows:?}");
        assert!(
            rows[0].contains("· click for details"),
            "and it says how to read the rest: {rows:?}"
        );
    }
}
