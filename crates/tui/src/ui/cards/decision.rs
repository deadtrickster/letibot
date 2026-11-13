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
/// card and on the bar, one spelling. Digits and arrows are the only selectors: nothing typed here
/// picks a rung, it is the marked rung's material.
pub(crate) const LADDER_HINT: &str = "↑↓ or a row number marks · enter answers";

/// A question's ladder: its digits are the answer being typed, so only the arrows mark a row, and
/// typing is the other way to answer. Its rows carry no number, since a drawn number is the promise
/// that the digit marks the row.
pub(crate) const QUESTION_LADDER_HINT: &str = "↑↓ marks · enter answers · or type your own answer";

/// A question with nothing to choose from: its line is the answer, and Enter is the only key.
pub(crate) const NO_LADDER_HINT: &str = "type your answer · enter sends it";

/// What a rung does with the words in the composer, typed alongside the mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Words {
    /// The rung writes a rule, and the words are the glob it is written with.
    Glob,
    /// The rung refuses, and the words are what the model is told.
    Reason,
    /// The rung answers on its own; words typed here have nowhere to go.
    None,
}

/// How long a permission card ignores the key that answers it: `MODE_ARM_MS`'s window one card over.
/// Since a typed line now means something to the marked rung, an Enter meant for the composer would
/// deny a call with the operator's own words as its reason. Digits and arrows only mark, so they are
/// not held.
pub(crate) const DEC_ARM_MS: u64 = 500;

/// What a card that has just appeared says when it refuses the key that would answer it.
pub(crate) const ARMING_SAID: &str = "the card just came up — press enter again to answer it";

/// What a typed line becomes for a rung of the marked kind, or the sentence that says it has
/// nowhere to go here. One reading for Enter on the card and Enter in the composer, so the two
/// cannot disagree about what a typed line means.
pub(crate) fn marked_words(
    kind: letibot_sessionlog::event::OptionKind,
    label: &str,
    typed: &str,
) -> Result<MarkedWords, String> {
    let words = typed.trim();
    match words_for_kind(kind) {
        Words::None if !words.is_empty() => Err(format!(
            "`{label}` takes no words — ↑↓ marks a rung that does (a deny, an always-allow), or \
             ctrl-c clears the line"
        )),
        _ if words.is_empty() => Ok(MarkedWords::Bare),
        Words::Glob => Ok(MarkedWords::Glob(words.to_string())),
        Words::Reason => Ok(MarkedWords::Reason(words.to_string())),
        Words::None => Ok(MarkedWords::Bare),
    }
}

/// A typed line spent on the marked rung: the two fields an `Action::Answer` carries besides the id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MarkedWords {
    /// Nothing typed, so nothing to carry.
    Bare,
    /// An always-allow's glob.
    Glob(String),
    /// A refusal's reason, which the model is told.
    Reason(String),
}

impl MarkedWords {
    /// The `(pattern, note)` pair, in the order `Action::Answer` takes them.
    pub(crate) fn into_answer(self) -> (Option<String>, Option<String>) {
        match self {
            MarkedWords::Bare => (None, None),
            MarkedWords::Glob(p) => (Some(p), None),
            MarkedWords::Reason(why) => (None, Some(why)),
        }
    }
}

/// What the marked rung does with a typed line. The kind is the daemon's own, so the card and the
/// gate cannot disagree about which rung takes a reason and which takes a rule.
pub(crate) fn words_for(d: &OpenDecision, at: usize) -> Words {
    d.options
        .get(at)
        .map_or(Words::None, |o| words_for_kind(o.kind))
}

fn words_for_kind(kind: letibot_sessionlog::event::OptionKind) -> Words {
    use letibot_sessionlog::event::OptionKind as K;
    match kind {
        K::AllowAlways => Words::Glob,
        K::RejectOnce | K::RejectAlways => Words::Reason,
        _ => Words::None,
    }
}

/// The sentence a permission card owes about its two standing rungs, drawn in the content half:
/// what a session grant lasts, and where a durable rule is written. In the gate's words it was a
/// paragraph inside the Deny button, which is a choice that explains itself instead of being one.
/// The mode is named when this head knows it, since the rungs it narrows are the mode's.
fn grant_sentence(d: &OpenDecision, mode: &str) -> Option<String> {
    use letibot_sessionlog::event::OptionKind as K;
    let mut said: Vec<&str> = Vec::new();
    if d.options.iter().any(|o| o.kind == K::AllowSession) {
        said.push("a session grant lasts until the daemon stops");
    }
    if d.options.iter().any(|o| o.kind == K::AllowAlways) {
        said.push("a durable rule is written to ~/.config/letibot/permission.json");
    }
    let mode = match mode.trim() {
        "" => String::new(),
        m => format!("mode `{m}` · "),
    };
    (!said.is_empty()).then(|| format!("{mode}{}", said.join(" · ")))
}

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
    /// the answer, and a head that printed both would promise a key nobody has. The ranges come out
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

    /// Whether the front card is still in its first moments, so the Enter on its way belongs to the
    /// composer rather than to the card. A permission's only, and only where this head has a clock:
    /// `now_ms` zero is a test or a replay, and a window nobody can measure is not a window.
    pub(crate) fn card_arming(&self) -> bool {
        self.open.first().is_some_and(|d| d.kind != "question")
            && self.now_ms > 0
            && self.now_ms.saturating_sub(self.card_arrived) < DEC_ARM_MS
    }

    /// How long there is and what silence does: the one line of rano's answer half this head keeps,
    /// because it is a fact about the ask rather than a description of a card.
    fn deadline_rows(&self, d: &OpenDecision, w: usize) -> Vec<rano::render::Line> {
        let Some(deadline) = d.deadline else {
            return Vec::new();
        };
        let left = match deadline.checked_sub(self.now_ms) {
            Some(left) => rano::agent::text::countdown(left),
            // Past its deadline: the daemon settled it without telling this head, and a count into
            // negative seconds reads as a rendering fault.
            None => format!(
                "past its deadline by {}s; the daemon has not said what became of it",
                (self.now_ms - deadline) / 1000
            ),
        };
        let clause = match d.on_timeout {
            letibot_sessionlog::event::OnTimeout::Deny => "if nobody answers, nothing runs",
            letibot_sessionlog::event::OnTimeout::Allow => "if nobody answers, it RUNS anyway",
            letibot_sessionlog::event::OnTimeout::Ask => {
                "if nobody answers, the guard model decides"
            }
        };
        rano::agent::text::wrap(&format!("  {left} · {clause}"), w)
            .into_iter()
            .map(|l| rano::agent::text::one(l, rano::style::Role::Faint))
            .collect()
    }

    /// What the marked rung will do with the words in the composer, or with the silence: the rule
    /// that will be written, the sentence the model will be handed, or that this rung has no use
    /// for them. Nothing when there is nothing typed.
    fn words_preview(&self, d: &OpenDecision, w: usize) -> Option<rano::render::Line> {
        let typed = self.editor.text();
        let words = typed.trim();
        if words.is_empty() {
            return None;
        }
        let at = self.sel.min(d.options.len().saturating_sub(1));
        let said = match words_for(d, at) {
            Words::Glob => format!("  writes the rule `{words}` to permission.json"),
            Words::Reason => format!("  tells the model: {words}"),
            Words::None => {
                "  words go nowhere on this rung — deny and an always-allow take them, ↑↓ marks one"
                    .to_string()
            }
        };
        Some(rano::agent::text::one(
            rano::agent::text::trim_to(&said, w),
            rano::style::Role::Pending,
        ))
    }

    /// The ladder's hint: digits and arrows mark, Enter answers, and - where the marked rung has a
    /// use for them - what the words typed beside it become. One spelling with the bar.
    pub(crate) fn ladder_hint(&self, d: &OpenDecision) -> String {
        if d.kind == "question" {
            return if decision_rows(d) == 0 {
                format!("  {NO_LADDER_HINT}")
            } else {
                format!("  {QUESTION_LADDER_HINT}")
            };
        }
        let at = self.sel.min(d.options.len().saturating_sub(1));
        match words_for(d, at) {
            Words::Glob => format!("  {LADDER_HINT} · type the rule"),
            Words::Reason => format!("  {LADDER_HINT} · type the reason"),
            Words::None => format!("  {LADDER_HINT}"),
        }
    }

    /// The card, split where the split is: `content` is the question, what it is about and the
    /// evidence, a viewport that shrinks and scrolls; `choices` is the ladder and what pressing it
    /// means, never trimmed and never scrolled, because that half is the answer.
    ///
    /// The answer half is this head's, whole: rano's rungs are replaced where the head has its own
    /// words for them, and everything rano draws under the ladder - the id to type, the `<why>`
    /// suffix, a glob hint with a run of spaces in it - described a card that no longer exists.
    pub(crate) fn decision_card(&self, d: &OpenDecision, w: usize) -> (Vec<String>, Vec<String>) {
        let view = self.decision_view(d);
        let (mut content, rano) = view.split(w);
        let p = self.cfg.palette();
        let question = d.kind == "question";
        // rano's rungs stay for a question, whose answers are the model's prose; a permission's are
        // this head's, so a digit and a click cannot disagree about what row two is.
        let cut = view.choice_rows(w).last().map_or(0, |r| r.end);
        let rungs = if question {
            rano[..cut].to_vec()
        } else {
            self.ladder_rows(d, w).0
        };
        // What a grant lasts, where the button cannot say it: the label is one line, and the
        // sentence a person needs to weigh the rungs is read in the half that scrolls.
        if let Some(said) = grant_sentence(d, &self.mode_current()) {
            for l in rano::agent::text::wrap(&format!("  {said}"), w) {
                content.push(rano::agent::text::one(l, rano::style::Role::Faint));
            }
        }
        let mut choices = rungs;
        // What silence does, below the ladder because that is the order a person reads.
        if d.deadline.is_some() {
            for l in self.deadline_rows(d, w) {
                choices.push(l);
            }
        }
        // What the words in the composer will do, above the hint: the rule that will be saved, or
        // the sentence the model will be given. It is the answer's own meaning, so it sits in the
        // half that never trims.
        if !question && let Some(l) = self.words_preview(d, w) {
            choices.push(l);
        }
        choices.push(rano::agent::text::one(
            rano::agent::text::trim_to(&self.ladder_hint(d), w),
            rano::style::Role::Pending,
        ));
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
