//! The permission card: a call waiting on the person, its ladder of options, and what the card says
//! about how it was decided once it has been - drawn by `rano::agent::decision` from the open
//! decision this head holds.

use crate::app::*;
use crate::ui::render::{row, row_strings};
use crate::ui::*;
use letibot_sessionlog::view::OpenDecision;
#[cfg(test)]
use letibot_sessionlog::view::SettledDecision;
use rano::agent::decision as card;

/// How a question card says Esc and alt-a - on the card's last row and on the hint bar under it,
/// one spelling for both. Esc puts the card away and answers nothing; alt-a brings it back.
pub(crate) const PUT_AWAY_HINT: &str = "esc puts it away — alt-a brings it back";

/// How a ladder says its keys: a row number or the arrows marks a row, Enter answers it - on the
/// card and on the bar, one spelling.
pub(crate) const LADDER_HINT: &str = "↑↓ or a row number marks · enter answers";

/// A question's ladder: its digits are the answer being typed, so only the arrows mark a row, and
/// typing is the other way to answer. Its rows carry no number, since a drawn number is the promise
/// that the digit marks the row.
pub(crate) const QUESTION_LADDER_HINT: &str = "↑↓ marks · enter answers · or type your own";

/// The head of rano's ladder hint, which [`LADDER_HINT`] replaces.
const RANO_LADDER_HINT: &str = "  ↑↓ to choose · Enter to answer";

/// A ladder row with its number: the digit that marks it is drawn on it, `1`-`9`; a row past the
/// ninth has no digit and is padded to line up.
pub(crate) fn numbered(i: usize, body: &str) -> String {
    let body = row_body(body);
    if i < 9 {
        format!("{}  {body}", i + 1)
    } else {
        format!("   {body}")
    }
}

/// A row's body alone, for a card whose rows carry no number: an empty choice reads as one rather
/// than as a blank highlighted row.
fn row_body(body: &str) -> String {
    if body.trim().is_empty() {
        "(an empty answer)".to_string()
    } else {
        body.to_string()
    }
}

impl App {
    /// The rungs, in this head's words: rano puts each option's id beside a label that already says
    /// the answer, and the id is only read where an answer takes a word after it. The ranges come out
    /// of the same walk as the rows, so a click and the drawing cannot disagree.
    pub(crate) fn ladder_rows(
        &self,
        d: &OpenDecision,
        w: usize,
    ) -> (Vec<rano::render::Line>, Vec<std::ops::Range<usize>>) {
        let mut rows = Vec::new();
        let mut spans = Vec::new();
        let sel = self.sel.min(d.options.len().saturating_sub(1));
        for (i, o) in d.options.iter().enumerate() {
            let first = rows.len();
            let picked = i == sel;
            let marker = if picked { "▸" } else { " " };
            for l in rano::agent::text::wrap(&format!("  {marker} {}", numbered(i, &o.label)), w) {
                rows.push(if picked {
                    rano::agent::text::styled(l, rano::render::Style::new().reverse())
                } else {
                    rano::agent::text::one(l, rano::style::Role::Pending)
                });
            }
            spans.push(first..rows.len());
        }
        (rows, spans)
    }

    /// Where the answer half's rungs were drawn, for the click record: rano's own rows for a
    /// question, whose answers are the model's prose, and this head's for a permission's ladder.
    pub(crate) fn choice_rows(&self, d: &OpenDecision, w: usize) -> Vec<std::ops::Range<usize>> {
        if d.kind == "question" {
            self.decision_view(d).choice_rows(w)
        } else {
            self.ladder_rows(d, w).1
        }
    }

    /// The card, split where the split is: `content` is the question, what it is about and the
    /// evidence, a viewport that shrinks and scrolls; `choices` is the ladder and what pressing it
    /// means, never trimmed and never scrolled, because that half is the answer.
    pub(crate) fn decision_card(&self, d: &OpenDecision, w: usize) -> (Vec<String>, Vec<String>) {
        let view = self.decision_view(d);
        let (content, mut choices) = view.split(w);
        let p = self.cfg.palette();
        // The ladder's hint, in this head's words: a permission's row number marks a row and Enter answers it, a
        // question's digits are its answer so only the arrows mark, and a question with nothing to choose from has no
        // ladder - its line is typed. One casing with the bar.
        let question = d.kind == "question";
        let rows = decision_rows(d);
        // rano's rungs go, this head's take their place: what it draws after them - the deadline,
        // the hints - stays, so only the answer's own rows are this head's words.
        if !question && rows > 0 {
            let cut = view.choice_rows(w).last().map_or(0, |r| r.end);
            choices.splice(..cut, self.ladder_rows(d, w).0);
        }
        for l in choices.iter_mut() {
            let plain = l.plain();
            let Some(rest) = plain.strip_prefix(RANO_LADDER_HINT) else {
                continue;
            };
            let said = if question && rows == 0 {
                "  type your answer · enter sends it".to_string()
            } else if question {
                format!("  {QUESTION_LADDER_HINT}{rest}")
            } else {
                format!("  {LADDER_HINT}{rest}")
            };
            *l = rano::render::Line::styled(
                rano::agent::text::trim_to(&said, w),
                rano::style::Role::Pending,
            );
        }
        // A question's two spellings the model cannot offer: rano draws the hint for a question and
        // is pinned, so a note on a choice and an abstention are this head's row under it. Without
        // it they exist and are undiscoverable, which for an affordance is the same as absent.
        if d.kind == "question" {
            choices.push(rano::agent::pane::faint(rano::agent::text::trim_to(
                &format!(
                    "  type `{ABSTAIN}` to answer nothing · `<choice> {NOTE_SEP} <note>` puts your note on that choice"
                ),
                w,
            )));
        }
        // Why a rung a person would look for is not on the ladder: the gate leaves a rung off rather
        // than showing a button whose effect it would decline to honour, and a rung that vanished in
        // silence reads as `maybe I misremembered`. In the `choices` half, which is never trimmed.
        if let Some(note) = Some(d.options_note.as_str()).filter(|n| !n.trim().is_empty()) {
            choices.push(rano::agent::pane::faint(rano::agent::text::trim_to(
                &format!("  {note}"),
                w,
            )));
        }
        // What waits behind this card: only the front card is drawn, so a model's question with a
        // deadline behind a todo card could time out unseen.
        let behind = self.open.iter().skip(1).count();
        if behind > 0 && self.open.first().is_some_and(|f| f.req_id == d.req_id) {
            let clock = if self.open.iter().skip(1).any(|o| o.deadline.is_some()) {
                " — one has a deadline"
            } else {
                ""
            };
            choices.push(rano::agent::pane::faint(rano::agent::text::trim_to(
                &format!("  +{behind} more waiting behind this card{clock}"),
                w,
            )));
        }
        // And the way to step away from it without answering: its own row, so a narrow card trims
        // the line above and not this, and on every decision card, because Esc means the same on
        // every one.
        choices.push(rano::agent::pane::faint(rano::agent::text::trim_to(
            &format!("  {PUT_AWAY_HINT}"),
            w,
        )));
        (row_strings(&content, p), row_strings(&choices, p))
    }

    /// The card's facts, in rano's words: the ask and target, whose call it is, the files it would write, layer A's
    /// reading, the model's reason, the guard's verdict, the ladder with this head's highlighted row, and the deadline.
    /// `decision_card` drops rano's ladder rows for this head's; nothing here preselects an option.
    pub(crate) fn decision_view(&self, d: &OpenDecision) -> card::DecisionCard {
        use letibot_sessionlog::event::{OnTimeout as T, OptionKind as K};
        card::DecisionCard {
            kind: d.kind.clone(),
            summary: d.summary.clone(),
            target: d.target.clone(),
            subagent: d.subagent.as_ref().map(|s| card::SubagentAsk {
                // The child by its name when this head knows one (`fix-login · …tail`).
                handle: match self.child_label(&s.handle) {
                    named if named.contains(" · ") => named,
                    _ => s.handle.clone(),
                },
                task: s.task.clone(),
            }),
            write_targets: d
                .write_targets
                .iter()
                .map(|t| card::WriteTarget {
                    path: t.path.clone(),
                    unresolved: t.unresolved,
                })
                .collect(),
            detail: d.detail.clone(),
            access: d.access.clone(),
            because: d.because.clone(),
            advice: d.advice.as_ref().map(model_advice),
            options: d
                .options
                .iter()
                .enumerate()
                .map(|(i, o)| card::DecisionOption {
                    label: numbered(i, &o.label),
                    option_id: o.option_id.clone(),
                    kind: match o.kind {
                        K::AllowOnce => card::OptionKind::AllowOnce,
                        K::AllowSession => card::OptionKind::AllowSession,
                        K::AllowAlways => card::OptionKind::AllowAlways,
                        K::RejectOnce => card::OptionKind::RejectOnce,
                        K::RejectAlways => card::OptionKind::RejectAlways,
                    },
                })
                .collect(),
            choices: d.choices.iter().map(|c| row_body(c)).collect(),
            selected: self.sel,
            deadline_ms: d.deadline,
            now_ms: self.now_ms,
            on_timeout: match d.on_timeout {
                T::Deny => card::OnTimeout::Deny,
                T::Allow => card::OnTimeout::Allow,
                T::Ask => card::OnTimeout::Ask,
            },
        }
    }

    /// The whole card as one list, for the callers that want it whole: the fit loop in
    /// [`App::screen`] does not, because the split happens there, so this is the transcript's shape
    /// and the tests' entry point rather than the drawing path.
    #[cfg(test)]
    pub(crate) fn decision_lines(&self, d: &OpenDecision, w: usize) -> Vec<String> {
        let (mut content, choices) = self.decision_card(d, w);
        content.extend(choices);
        content
    }

    /// Put the card's window where a value's rows are, when the cursor has moved and left it. `room`
    /// is the whole viewport the fit ladder granted, one row of which the seam spends - read the same
    /// way `card_window` reads it, so a value near the end can still be brought fully into view.
    pub(crate) fn card_scroll_into(&mut self, span: std::ops::Range<usize>, room: usize) {
        let shown = room.saturating_sub(1).max(1);
        if span.start < self.dec_scroll {
            self.dec_scroll = span.start;
        } else if span.end > self.dec_scroll + shown {
            self.dec_scroll = span.end.saturating_sub(shown);
        }
    }

    /// The card's content as a window: `room` rows, seam included; returns the lines to draw and the
    /// scroll used. The seam says how many lines are out of view - never "there is more" - and the
    /// scroll is clamped here, since the drawn length is a function of the width and the fold.
    pub(crate) fn card_window(
        &mut self,
        _w: usize,
        content: &[String],
        room: usize,
    ) -> (Vec<String>, usize) {
        self.dec_content_len = content.len();
        if content.is_empty() || room == 0 {
            self.dec_content_room = 0;
            self.dec_scroll = 0;
            return (Vec::new(), 0);
        }
        if content.len() <= room {
            // `room` counts the content lines the viewport actually showed, seam excluded,
            // so "nothing is out of view" is `len <= room` in both places that ask.
            self.dec_content_room = content.len();
            self.dec_scroll = 0;
            return (content.to_vec(), 0);
        }
        // One row of the viewport is the seam, and it is spent even at `room == 1` - a
        // viewport whose whole height is the sentence saying how much is missing is the
        // honest shape of a screen with nowhere to put the content.
        let shown = room - 1;
        self.dec_content_room = shown;
        let max = content.len() - shown;
        let at = self.dec_scroll.min(max);
        self.dec_scroll = at;
        let above = at;
        let below = content.len() - (at + shown);
        let seam = if below == 0 {
            row(
                &rano::agent::pane::faint(format!(
                    "  … {above} line(s) out of view · pgup scrolls"
                )),
                self.cfg.palette(),
            )
        } else if above == 0 {
            row(
                &rano::agent::pane::faint(format!(
                    "  … {below} line(s) out of view · pgdn scrolls"
                )),
                self.cfg.palette(),
            )
        } else {
            row(
                &rano::agent::pane::faint(format!(
                    "  … {above} above, {below} below · pgup/pgdn scrolls"
                )),
                self.cfg.palette(),
            )
        };
        let mut out: Vec<String> = Vec::with_capacity(room);
        if below == 0 {
            out.push(seam.clone());
        }
        out.extend(content[at..at + shown].iter().cloned());
        if below != 0 {
            out.push(seam);
        }
        (out, at)
    }
}

/// The ask with its target taken off the end - rano's `ask_without_target`, which the card lays its
/// headline out with; here for the tests that pin it.
#[cfg(test)]
pub(crate) use rano::agent::decision::ask_without_target;

/// The two reasons a settled decision carries, labelled as whose they are - rano's
/// `decision_detail`, which the tool card and the settled row both draw through, asked of this
/// head's own decision. Here for the tests that pin what it says.
#[cfg(test)]
pub(crate) fn decision_detail(d: &SettledDecision, w: usize) -> Vec<String> {
    rano::agent::decision::decision_detail(&crate::ui::settled_decision(d), w)
}
