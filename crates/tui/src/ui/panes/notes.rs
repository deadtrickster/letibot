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

/// One note, as the transcript draws it: at most `NOTE_LINES` lines and a seam — a pane's
/// ending whole. See `rano::agent::notes::Note::lines`.
///
/// **And a compaction is ONE line**, whatever `NOTE_LINES` says: see [`COMPACTION_ROWS`].
pub(crate) fn note_lines(cfg: &RenderConfig, n: &Note) -> Vec<String> {
    match compaction_row(cfg, n) {
        Some(rows) => rows,
        None => row_strings(&note_view(n).lines(cfg.width), cfg.palette()),
    }
}

/// **The compaction rows this head draws as ONE line** — the operator's ask, and the one
/// place it is decided.
///
/// > *"compaction leads to too much noise in the conversation for me. in conversation and
/// > show edits i want a one line - compacted blabla"*
///
/// The `Conversation` rung is *"the conversation alone — the operator's messages and the
/// model's answers, and nothing the head did to produce them"*, and a compaction is the one
/// thing in it that arrives as prose rather than as an answer. Worse, `compacted` is the one
/// sentence the daemon writes the model's own record INTO: `sessions::compaction_said`
/// appends the whole summary to it whenever nobody watched the summary turn being written
/// (`!summary_was_streamed` — every fold, whose summary runs over a scratch transcript), so
/// the quiet rungs drew **the model's continuation material** in the middle of the
/// conversation. A person reading the conversation was being shown the prompt.
///
/// **`/notes` is where the whole of it is**, unchanged, and the row says so: the seam is
/// rano's own `… · /notes`, the unfold this head already has for every other note. Nothing is
/// dropped from the record, and the sentence is one `/notes` away — which is the difference
/// between a fold and a cut.
///
/// # Which codes, and why not the two that are missing
///
/// **A compaction that RAN**, which is the four `Class::Routine` codes that report one:
/// `compacted` (the report), `auto_compact` (the wall announcing itself), `compact_half` (the
/// fold saying what it is doing while it does it) and `reseated` (a compaction that lands on
/// a different prompt). leticl's `+compaction-row-codes+` is the same list by the same
/// argument, and this is the shape of it: *a code that reports a compaction somebody TRIED*
/// becomes a row; a code whose fact is *nothing was attempted* stays a note.
///
/// **`auto_compact_no_progress` and `auto_compact_failed` are deliberately NOT here.** They
/// are `Class::Failure`, and a failure's sentence is the reason it failed — `the summary is
/// still within the headroom`, `the automatic compaction did not run` — which is exactly what
/// a fold must not take away. They are the two compaction codes where the detail IS the
/// finding, and they are drawn whole. `warning.rs` asserts both are failures;
/// `the_folded_codes_are_all_routine` asserts the same thing from this side, so a code that
/// changed register cannot be folded by accident.
///
/// **And the fold is a property of the note, not of a rung.** A warning whose appearance
/// changed as the ladder was cycled would be the revision `Verbosity::Loud`'s docstring
/// refuses: a reader who has read a sentence must not have it re-rendered because they
/// pressed a key.
const COMPACTION_ROWS: &[&str] = &["compacted", "auto_compact", "compact_half", "reseated"];

/// What a compaction's one line says instead of what it left out — rano's seam word, on the
/// same row.
///
/// rano writes `… +N lines · /notes` under a folded note, and a compaction cannot: the seam
/// would be the second row the operator asked to be rid of. So it goes on the line, and the
/// `…` goes with it — an elision whose denominator is missing is a silent cut.
const COMPACTION_SEAM: &str = " … · /notes";

/// The same seam where the row has already cut the account itself: `rano::width::text::truncate`
/// wrote an `…` of its own at the cut, and two of them read as a stutter.
const COMPACTION_CUT: &str = " · /notes";

/// The compaction's row: the mark, the code, and the daemon's own ACCOUNT of it — never the
/// model's record. `None` when this is not a compaction note, which is every other note.
///
/// **The account is the sentence, not a parsed figure.** The daemon writes
/// `compacted: 958397 → 100005 tokens, on transcript s#t25.` and everything after the first
/// full stop is either the verbatim-tail clause or the record itself; cutting at that stop is
/// a rule about *the daemon's sentences*, not a reading of English for numbers, and a reword
/// fails visibly (the row gets longer) rather than silently (a number invented). The same
/// discipline `folded_notice` keeps: what this head does not recognise it does not fold.
fn compaction_row(cfg: &RenderConfig, n: &Note) -> Option<Vec<String>> {
    let Note::Warned(w) = n else { return None };
    if !COMPACTION_ROWS.contains(&w.code.as_str()) {
        return None;
    }
    let view::Note::Warned { class, .. } = note_view(n) else {
        return None;
    };
    let whole = w.detail.trim();
    let account = first_sentence(whole);
    // The room the account has: `· ` + the code + ` — `, and the longer of the two seams. The
    // mark is one column in every register rano draws (`·`, `×`, `!`), which is why it is a
    // constant here rather than a second copy of that table.
    let room = cfg
        .width
        .saturating_sub(w.code.chars().count() + 5 + COMPACTION_SEAM.chars().count());
    let kept = trim_to(account, room);
    let seam = if kept.len() < account.len() {
        // The cut is already visible on the row, so the seam only says where the rest is.
        COMPACTION_CUT
    } else if account.len() < whole.len() {
        COMPACTION_SEAM
    } else {
        // The whole detail is on this row: there is nothing for the seam to point at.
        ""
    };
    // **Handed back to rano as a note with a short detail**, rather than composed here: the
    // mark, the register's colour and the wrapping are rano's, and a second table of the three
    // registers is the drift `the_three_registers_are_three_marks` exists to catch.
    let folded = view::Note::Warned {
        code: w.code.clone(),
        detail: format!("{kept}{seam}"),
        class,
    };
    // Every line, not the first: the arithmetic above is meant to leave exactly one, and a
    // bug in it must read as a two-row note rather than as a silently dropped one.
    Some(row_strings(&folded.lines(cfg.width), cfg.palette()))
}

/// The daemon's account of a compaction: its sentence up to the first full stop, and the whole
/// of it when it has none.
///
/// `. ` and not `.`: a detail holds figures (`1.5M`), and a full stop with nothing after it is
/// the end of the sentence already.
fn first_sentence(s: &str) -> &str {
    match s.find(". ") {
        Some(at) => &s[..=at],
        None => s,
    }
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

    /// **A code that changed register cannot be folded by accident.**
    ///
    /// [`COMPACTION_ROWS`] is the list of codes this head draws as one line, and every one of
    /// them is a compaction that RAN. A `Failure` in it would be a reason nobody reads — the
    /// one direction a fold must never go — so the register is asserted rather than assumed,
    /// against the daemon's own table and not a copy of it.
    #[test]
    fn the_folded_codes_are_all_routine() {
        for code in COMPACTION_ROWS {
            assert!(
                letibot_sessionlog::warning::is_routine(code),
                "`{code}` is folded to one line and is not Routine — a failure's sentence is \
                 the reason it failed, which a fold must not take away"
            );
        }
        // And the two the family leaves out are the two that would fail the check above.
        for code in ["auto_compact_no_progress", "auto_compact_failed"] {
            assert!(
                !letibot_sessionlog::warning::is_routine(code),
                "`{code}` is not folded because its detail IS the finding"
            );
        }
    }

    /// **The account is the daemon's sentence, cut at its first full stop.**
    #[test]
    fn the_account_is_the_first_sentence_and_the_record_is_not_in_it() {
        let detail = "compacted: 958397 → 100005 tokens, on transcript s#t25. Nothing of the \
                      summary turn reached this screen — it ran over a scratch transcript — so \
                      here is what the model now reads in place of everything before it:\n\n## \
                      Goal\n\nTHE MODEL'S OWN RECORD";
        assert_eq!(
            first_sentence(detail.trim()),
            "compacted: 958397 → 100005 tokens, on transcript s#t25."
        );
        // No full stop, no cut: the daemon's whole sentence is the account.
        assert_eq!(
            first_sentence("at the wall: summarising the first 12 item(s)"),
            "at the wall: summarising the first 12 item(s)"
        );
        // A figure is not a sentence boundary.
        assert_eq!(first_sentence("carried 1.5M tokens"), "carried 1.5M tokens");
    }

    /// **A compaction is one row and the model's record is not on it** — and the row says
    /// where the whole sentence is.
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
            row.contains("958397 → 100005 tokens"),
            "the numbers: {row:?}"
        );
        assert!(
            row.contains("/notes"),
            "the seam names where the rest is: {row:?}"
        );
        assert!(
            !row.contains("THE MODEL'S OWN RECORD"),
            "the model's record is not the conversation's row: {row:?}"
        );
        // **And nothing was swallowed.** The whole sentence, the record in it and all, is on
        // the listing — which is the unfold the seam points at.
        let listed = row_strings(
            &[view::Note::Warned {
                code: "compacted".into(),
                detail: detail.into(),
                class: NoteClass::Routine,
            }]
            .iter()
            .flat_map(|v| v.unfolded(cfg.width))
            .collect::<Vec<_>>(),
            cfg.palette(),
        );
        assert!(
            listed.iter().any(|l| l.contains("THE MODEL'S OWN RECORD")),
            "the listing keeps the whole sentence: {listed:?}"
        );
        // A note the family does not name is untouched: the register's own fold, three lines
        // and rano's seam.
        let other = warned(
            "ledger_chain_mismatch",
            &"a sentence long enough to fold. ".repeat(12),
        );
        assert!(
            note_lines(&cfg, &other).len() > 1,
            "a warning that is not a compaction keeps its own fold"
        );
    }

    /// **A long account is cut to the row, and the cut is visible.**
    #[test]
    fn a_long_account_is_trimmed_and_says_so() {
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
            rows[0].contains("/notes"),
            "and it says where the rest is: {rows:?}"
        );
    }
}
