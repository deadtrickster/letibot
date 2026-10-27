//! **The permission card**: a call waiting on the person, its ladder of options, and what
//! the card says about how it was decided once it has been — drawn by
//! `rano::agent::decision`, which carries the card's rules and the reports behind them, from
//! the open decision this head holds.

use crate::app::*;
use crate::ui::render::{row, row_strings};
use crate::ui::*;
use letibot_sessionlog::view::OpenDecision;
#[cfg(test)]
use letibot_sessionlog::view::SettledDecision;
use rano::agent::decision as card;

/// **How a question card says Esc and alt-a** — on the card's last row and on the hint bar under
/// it, one spelling for both. Esc puts the card away and answers nothing; alt-a brings it back.
pub(crate) const PUT_AWAY_HINT: &str = "esc puts it away — alt-a brings it back";

/// **How a ladder says its keys** (R90.2): a row number or ↑↓ marks a row, Enter answers it —
/// on the card and on the bar, one spelling.
pub(crate) const LADDER_HINT: &str = "↑↓ or a row number marks · enter answers";

/// The head of rano's ladder hint, which [`LADDER_HINT`] replaces.
const RANO_LADDER_HINT: &str = "  ↑↓ to choose · Enter to answer";

/// **A ladder row with its number** (R90.2): the digit that marks it is drawn on it, `1`–`9`;
/// a row past the ninth has no digit and is padded to line up. An empty choice reads as one
/// rather than as a blank highlighted row (R90.23).
pub(crate) fn numbered(i: usize, body: &str) -> String {
    let body = if body.trim().is_empty() {
        "(an empty answer)"
    } else {
        body
    };
    if i < 9 {
        format!("{}  {body}", i + 1)
    } else {
        format!("   {body}")
    }
}

impl App {
    /// **The card, split where R20 says the split is.**
    ///
    /// `head-parity-2026-09-21.md` **R20**, ruled 2026-09-22 on a permission card carrying a
    /// giant replace or a commit message: *"I'm shown a permission prompt and I just cant
    /// see the selector."* The screen-fit loop in [`App::screen`] shrinks the card with
    /// `dec_rows -= 1`, which trims **from the end**, and the card was built headline,
    /// target, intents, because, advice, options, hint — so the loop ate the hint, then the
    /// options bottom-up, and kept the wall. **A card that has dropped its choices is a
    /// question with no way to answer it**, and the operator was left reading a wall at full
    /// length while the four rows they had to act on were gone.
    ///
    /// So the card has two halves and only one of them gives way:
    ///
    /// * **`content`** — the question, what it is about, and the evidence. A viewport over
    ///   this shrinks to the room that is left, and **scrolls** (`pgup`/`pgdn`), so the whole
    ///   diff or message can still be read;
    /// * **`choices`** — the ladder and what pressing it means: the deadline, the hint, the
    ///   `deny_and_tell` line. **Never trimmed and never scrolled**, because this half is the
    ///   answer.
    ///
    /// The order the card is read in does not change: content above, choices below, which is
    /// the bottom of the card and therefore the row nearest the composer.
    pub(crate) fn decision_card(&self, d: &OpenDecision, w: usize) -> (Vec<String>, Vec<String>) {
        let (content, mut choices) = self.decision_view(d).split(w);
        let p = self.cfg.palette();
        // **The ladder's hint, in this head's words** (R90.2, R90.23, R90.32). rano's says
        // *"↑↓ to choose · Enter to answer"*: a row number now marks a row (the numbers are drawn,
        // see `decision_view`) and Enter answers it, and a question with nothing to choose from
        // has no ladder for the arrows to walk — its line is typed. One casing with the bar.
        let question = d.kind == "question";
        let rows = decision_rows(d);
        for l in choices.iter_mut() {
            let plain = l.plain();
            let Some(rest) = plain.strip_prefix(RANO_LADDER_HINT) else {
                continue;
            };
            let said = if question && rows == 0 {
                "  type your answer · enter sends it".to_string()
            } else {
                format!("  {LADDER_HINT}{rest}")
            };
            *l = rano::render::Line::styled(
                rano::agent::text::trim_to(&said, w),
                rano::style::Role::Pending,
            );
        }
        // **A question's two spellings the model cannot offer.** rano draws the hint for
        // a question (*"↑↓ to choose · Enter to answer · or type your own answer"*), and
        // rano is pinned, so the other two shapes — a note on a choice, and an abstention
        // — are this head's row under it. Without it they exist and are undiscoverable,
        // which for an affordance is the same as absent.
        //
        // In the `choices` half rather than the content: this is how to answer, and R20's
        // rule is that the answer half is never trimmed and never scrolled.
        if d.kind == "question" {
            choices.push(rano::agent::pane::faint(rano::agent::text::trim_to(
                &format!(
                    "  type `{ABSTAIN}` to answer nothing · `<choice> {NOTE_SEP} <note>` puts your note on that choice"
                ),
                w,
            )));
        }
        // **Why a rung a person would look for is not on the ladder.** The gate leaves a
        // rung off rather than showing a button whose effect it would decline to honour
        // (R18's rule), and the operator's report is the other half of it: a rung that
        // vanished in silence reads as *maybe I misremembered*. One line, under the ladder
        // it belongs to, in the attention register — it is a fact about what the person
        // CAN answer, not a footnote.
        //
        // In the `choices` half for the reason the question hints above are: the rungs are
        // the answer, and R20 says the answer half is never trimmed away.
        if let Some(note) = Some(d.options_note.as_str()).filter(|n| !n.trim().is_empty()) {
            choices.push(rano::agent::pane::faint(rano::agent::text::trim_to(
                &format!("  {note}"),
                w,
            )));
        }
        // **What waits behind this card** (R90.9): only the front card is drawn, so a model's
        // question with a deadline behind a todo card could time out unseen.
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
        // **And the way to step away from it without answering** — the operator: *"i want to
        // be able to escape the selection card. sometimes i need time to think and just let the
        // card stay"*. Its own row so a narrow card trims the line above, not this. On every
        // decision card, because Esc means the same on every one (R90.5).
        choices.push(rano::agent::pane::faint(rano::agent::text::trim_to(
            &format!("  {PUT_AWAY_HINT}"),
            w,
        )));
        (row_strings(&content, p), row_strings(&choices, p))
    }

    /// **The card's facts, in rano's words**: the ask and its target, whose call it is, the
    /// files it would write, layer A's reading and the model's reason, the guard's verdict,
    /// the ladder with this head's highlighted row, and the deadline against this head's
    /// clock. Nothing here preselects an option: `self.sel` is the row the person moved to.
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
            choices: d
                .choices
                .iter()
                .enumerate()
                .map(|(i, c)| numbered(i, c))
                .collect(),
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
    /// [`App::screen`] does not, because R20 splits it there, so this is the transcript's
    /// shape and the tests' entry point rather than the drawing path.
    pub(crate) fn decision_lines(&self, d: &OpenDecision, w: usize) -> Vec<String> {
        let (mut content, choices) = self.decision_card(d, w);
        content.extend(choices);
        content
    }

    /// **The card's content as a window** (R20).
    ///
    /// `room` is how many rows the viewport may occupy, seam included; returns the lines to
    /// draw — the seam and the window — and the scroll actually used.
    ///
    /// The seam is the whole of the disclosure, so it is written as one: **how many lines
    /// are out of view** and which key moves toward them. It is never "there is more" — a
    /// reader who cannot see the rest has to know whether one line or four hundred are
    /// missing before deciding whether to scroll at all, which is the same rule
    /// `OutputSlice::denominator` keeps for a job's output.
    ///
    /// **The window starts at the head**, because that is where a card is read from: the
    /// question and what it is about are the first lines, and `pgdn` walks down into the
    /// wall. The seam changes ends with the scroll — above the window once there is nothing
    /// left below it — so the sentence is always the boundary the reader is looking at.
    ///
    /// The scroll is clamped here rather than in the key handler, for the reason the panes
    /// clamp in `pane_window`: the drawn length is a function of the width and the fold, and
    /// the key handler knows neither.
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
        // One row of the viewport is the seam, and it is spent even at `room == 1` — a
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

/// The ask with its target taken off the end — rano's `ask_without_target`, which the card
/// lays its headline out with; here for the tests that pin it.
#[cfg(test)]
pub(crate) use rano::agent::decision::ask_without_target;

/// **The two reasons a settled decision carries, labelled as whose they are** — rano's
/// `decision_detail`, which the tool card and the settled row both draw through, asked of
/// this head's own decision. Here for the tests that pin what it says.
#[cfg(test)]
pub(crate) fn decision_detail(d: &SettledDecision, w: usize) -> Vec<String> {
    rano::agent::decision::decision_detail(&crate::ui::settled_decision(d), w)
}
