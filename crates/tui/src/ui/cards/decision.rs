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
/// one spelling for both. Esc puts the card away and answers nothing; alt-a brings it back. The
/// words are the catalog's, so a card and the bar under it cannot be drawn in two languages.
pub(crate) fn put_away_hint() -> &'static str {
    crate::i18n::t("card.put_away")
}

/// What a card says when it arrived over a line the operator is typing: the line is theirs, and
/// this is the key that hands the card the keyboard without it. On the card and on the bar, one
/// spelling, and the ladder's own keys are not promised until the card has them.
pub(crate) const DRAFT_HINT: &str = "your line is yours · tab answers this card";

/// How a ladder says its keys: a row number or the arrows marks a row, Enter answers it - on the
/// card and on the bar, one spelling. Digits and arrows are the only selectors: nothing typed here
/// picks a rung, it is the marked rung's material.
pub(crate) fn ladder_hint() -> &'static str {
    crate::i18n::t("card.ladder_hint")
}

/// A question's ladder: its digits are the answer being typed, so only the arrows mark a row, and
/// typing is the other way to answer. Its rows carry no number, since a drawn number is the promise
/// that the digit marks the row.
pub(crate) fn question_ladder_hint() -> &'static str {
    crate::i18n::t("card.question_ladder_hint")
}

/// A question with nothing to choose from: its line is the answer, and Enter is the only key.
pub(crate) fn no_ladder_hint() -> &'static str {
    crate::i18n::t("card.no_ladder_hint")
}

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

/// How long a permission card ignores the key that answers it, `MODE_ARM_MS`'s window one card over:
/// a typed line means something to the marked rung, so an Enter meant for the composer would deny
/// with the operator's words as reason. Digits and arrows only mark, so they are not held.
pub(crate) const DEC_ARM_MS: u64 = 500;

/// What a card that has just appeared says when it refuses the key that would answer it.
pub(crate) fn arming_said() -> &'static str {
    crate::i18n::t("card.arm_waiting")
}

/// How many diff rows a permission card draws before it folds the rest: a small edit fits whole,
/// and a big one cannot push the ladder off the screen.
pub(crate) const CARD_DIFF_ROWS: usize = 12;

/// The key that opens the card's diff whole, and folds it again.
pub(crate) const CARD_DIFF_KEY: &str = "alt-d";

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
        Words::None if !words.is_empty() => {
            Err(crate::i18n::tf("card.words.refused", &[("label", label)]))
        }
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

/// What a permission card says about its two standing rungs, in the content half: what a session
/// grant lasts and where a durable rule is written, not inside the Deny button. The mode is named
/// when this head knows it, since the rungs narrow the mode's.
fn grant_sentence(d: &OpenDecision, mode: &str) -> Option<String> {
    use letibot_sessionlog::event::OptionKind as K;
    let mut said: Vec<&str> = Vec::new();
    // A relayed ask's grant is the asking session's gate's, and this head's mode is not its mode.
    let relayed = relayed_role(d).is_some();
    if d.options.iter().any(|o| o.kind == K::AllowSession) {
        said.push(match relayed {
            true => crate::i18n::t("card.grant.session_relayed"),
            false => crate::i18n::t("card.grant.session"),
        });
    }
    if d.options.iter().any(|o| o.kind == K::AllowAlways) {
        said.push(crate::i18n::t("card.grant.durable"));
    }
    let mode = match mode.trim() {
        _ if relayed => String::new(),
        "" => String::new(),
        m => crate::i18n::tf("card.grant.mode", &[("mode", m)]),
    };
    (!said.is_empty()).then(|| format!("{mode}{}", said.join(" · ")))
}

/// Who asks, by role and work, when the card is relayed from a review queue.
pub(crate) fn relayed_role(d: &OpenDecision) -> Option<&str> {
    d.subagent
        .as_ref()
        .map(|s| s.role.as_str())
        .filter(|r| !r.is_empty())
}

/// Layer A's line when it read nothing about the action: `ask — intents [unknown] over []`. It
/// says no more than the headline, so the card leaves it off.
pub(crate) fn reading_is_empty(detail: &str) -> bool {
    let Some((_, rest)) = detail.split_once(" — intents [") else {
        return false;
    };
    let Some((intents, rest)) = rest.split_once(']') else {
        return false;
    };
    let rest = rest.strip_prefix(" over []").unwrap_or(rest);
    rest.trim().is_empty() && intents.split_whitespace().all(|i| i == "unknown")
}

/// A rung's label in the language this head draws in.
///
/// In English the daemon's own label is drawn unchanged: those words ARE the English source, and a
/// daemon that words a rung differently must not be relabelled by a head that thinks it knows
/// better. A translated language has to choose, and this is the choice: the rung's id is the
/// daemon's stable name, so the words come from the catalog - except where two rungs share an id,
/// and except for the two that carry a VALUE (the rule an always-allow writes, the program a
/// session grant covers), whose value is the daemon's data and is taken out of its sentence.
fn option_label(o: &letibot_sessionlog::event::DecisionOption) -> String {
    use letibot_sessionlog::event::OptionKind as K;
    if crate::i18n::active() == crate::i18n::Lang::En {
        return o.label.clone();
    }
    const MEMORY_DENY: &str = "No — it does not start";
    if o.label.trim() == MEMORY_DENY {
        return crate::i18n::t("card.option.memory_deny").to_string();
    }
    match o.option_id.as_str() {
        "allow_once" => crate::i18n::t("card.option.allow_once").to_string(),
        "deny" => crate::i18n::t("card.option.deny").to_string(),
        "deny_and_tell" => crate::i18n::t("card.option.deny_and_tell").to_string(),
        "grant_memory" => crate::i18n::t("card.option.grant_memory").to_string(),
        "grant_memory_session" => crate::i18n::t("card.option.grant_memory_session").to_string(),
        _ => match (o.kind, backticked(&o.label)) {
            (K::AllowAlways, Some(rule)) => {
                crate::i18n::tf("card.option.allow_always", &[("rule", &rule)])
            }
            (K::AllowSession, Some(program)) => crate::i18n::tf(
                "card.option.allow_program_session",
                &[("program", &program)],
            ),
            (K::AllowSession, None) => {
                crate::i18n::t("card.option.allow_class_session").to_string()
            }
            _ => o.label.clone(),
        },
    }
}

/// The first `…` span of a label - the rule or the program the daemon put in it.
fn backticked(label: &str) -> Option<String> {
    let open = label.find('`')? + 1;
    let close = label[open..].find('`')? + open;
    (!label[open..close].trim().is_empty()).then(|| label[open..close].to_string())
}

/// rano's sentence for a card no oracle saw, in rano's own words: it is DRAWN by rano's card, and
/// this head only counts its rows and finds where it landed, so translating it here would look for
/// a sentence that is not on the screen.
const NO_ORACLE: &str = "  no oracle was consulted for this one — the judgement is yours alone";

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
        crate::i18n::t("card.empty_answer").to_string()
    } else {
        body.to_string()
    }
}

/// What a card says over an excerpt that is not a change to the file: the tool's own reason first,
/// then what allowing the call actually does. A proposal is drawn under it when the call carried
/// one, and the sentence has to change with it - "the lines below" over nothing is a promise the
/// card would not keep.
pub(crate) fn card_refusal_line(note: &str, has_proposal: bool) -> String {
    match has_proposal {
        true => {
            format!("  {note} — nothing would be written; below is what this call asked to write")
        }
        false => format!("  {note} — so this call would write nothing"),
    }
}

impl App {
    /// The diff the call would apply, drawn by the tool row's renderer and folded at
    /// [`CARD_DIFF_ROWS`] with a count, unless alt-d opened it on this card.
    ///
    /// An excerpt that is not a change to the file says so first, in the tool's own words: an `edit`
    /// whose text is not there would write nothing, and a card that drew its proposal as though it
    /// were the change would be asking about a file that is not on disk.
    pub(crate) fn card_diff_rows(&self, d: &OpenDecision, w: usize) -> Vec<String> {
        let p = self.cfg.palette();
        let open = self.dec_diff_open == d.req_id;
        let mut out = Vec::new();
        for e in &d.edits {
            if !e.note.is_empty() {
                let why = card_refusal_line(&e.note, !e.after.trim().is_empty());
                for l in rano::agent::text::wrap(&why, w) {
                    out.push(row(&rano::agent::pane::faint(&l), p));
                }
            }
            let diff = edit_diff_rows(
                e,
                w.saturating_sub(2),
                &self.cfg,
                self.diff_split,
                usize::MAX,
            );
            let rows = diff.rows_under(&d.target);
            let keep = if open {
                rows.len()
            } else {
                CARD_DIFF_ROWS.min(rows.len())
            };
            out.extend(
                row_strings(&rows[..keep], p)
                    .into_iter()
                    .map(|l| format!("  {l}")),
            );
            let said = match rows.len() - keep {
                0 if open && rows.len() > CARD_DIFF_ROWS => {
                    format!(
                        "  {}",
                        crate::i18n::tf("card.diff.folded", &[("key", CARD_DIFF_KEY)])
                    )
                }
                0 => continue,
                hidden => format!(
                    "  {}",
                    crate::i18n::tf(
                        "card.diff.more",
                        &[("n", &hidden.to_string()), ("key", CARD_DIFF_KEY)],
                    )
                ),
            };
            out.push(row(
                &rano::agent::pane::faint(rano::agent::text::trim_to(&said, w)),
                p,
            ));
        }
        out
    }

    /// alt-d: the front card's diff whole, or folded again.
    pub(crate) fn toggle_card_diff(&mut self) {
        let Some(d) = self.open.first().filter(|d| !d.edits.is_empty()) else {
            self.say(crate::i18n::t("card.no_card"));
            return;
        };
        if self.dec_diff_open == d.req_id {
            self.dec_diff_open.clear();
        } else {
            self.dec_diff_open = d.req_id.clone();
        }
    }

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
            for l in
                rano::agent::text::wrap(&format!("  {marker} {}", numbered(i, &option_label(o))), w)
            {
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
            None => crate::i18n::tf(
                "card.deadline_past",
                &[("s", &((self.now_ms - deadline) / 1000).to_string())],
            ),
        };
        let clause = match d.on_timeout {
            letibot_sessionlog::event::OnTimeout::Deny => crate::i18n::t("card.timeout.deny"),
            letibot_sessionlog::event::OnTimeout::Allow => crate::i18n::t("card.timeout.allow"),
            letibot_sessionlog::event::OnTimeout::Ask => crate::i18n::t("card.timeout.ask"),
        };
        rano::agent::text::wrap(&format!("  {left} · {clause}"), w)
            .into_iter()
            .map(|l| rano::agent::text::one(l, rano::style::Role::Faint))
            .collect()
    }

    /// What the marked rung will do with the words in the composer, or with the silence: the rule
    /// that will be written, the sentence the model will be handed, or that this rung has no use
    /// for them. Nothing when there is nothing typed, and nothing while the card has no keys: those
    /// words are the operator's line, not this card's, and saying what the rung would do with them
    /// would promise a spend that never happens (see [`App::card_focus`]).
    fn words_preview(&self, d: &OpenDecision, w: usize) -> Option<rano::render::Line> {
        if !self.card_focus {
            return None;
        }
        let typed = self.editor.text();
        let words = typed.trim();
        if words.is_empty() {
            return None;
        }
        let at = self.sel.min(d.options.len().saturating_sub(1));
        let said = match words_for(d, at) {
            Words::Glob => format!(
                "  {}",
                crate::i18n::tf("card.words.glob", &[("rule", words)])
            ),
            Words::Reason => {
                format!(
                    "  {}",
                    crate::i18n::tf("card.words.reason", &[("words", words)])
                )
            }
            Words::None => format!("  {}", crate::i18n::t("card.words.none")),
        };
        Some(rano::agent::text::one(
            rano::agent::text::trim_to(&said, w),
            rano::style::Role::Pending,
        ))
    }

    /// The ladder's hint: digits and arrows mark, Enter answers, and - where the marked rung has a
    /// use for them - what the words typed beside it become. One spelling with the bar. A card that
    /// does not hold the keyboard says the key that gives it to them instead: every other promise
    /// would name a key that is the composer's.
    pub(crate) fn ladder_hint(&self, d: &OpenDecision) -> String {
        if !self.card_focus {
            return format!("  {DRAFT_HINT}");
        }
        if d.kind == "question" {
            return if decision_rows(d) == 0 {
                format!("  {}", no_ladder_hint())
            } else {
                format!("  {}", question_ladder_hint())
            };
        }
        let at = self.sel.min(d.options.len().saturating_sub(1));
        match words_for(d, at) {
            Words::Glob => format!("  {} · {}", ladder_hint(), crate::i18n::t("card.hint.rule")),
            Words::Reason => {
                format!(
                    "  {} · {}",
                    ladder_hint(),
                    crate::i18n::t("card.hint.reason")
                )
            }
            Words::None => format!("  {}", ladder_hint()),
        }
    }

    /// The card split in two: `content` is the question and its evidence, a viewport that shrinks
    /// and scrolls; `choices` is the ladder, never trimmed or scrolled, because it is the answer.
    /// The answer half is wholly this head's, so rano's rungs take its words.
    pub(crate) fn decision_card(&self, d: &OpenDecision, w: usize) -> (Vec<String>, Vec<String>) {
        let view = self.decision_view(d);
        let (mut content, rano) = view.split(w);
        let p = self.cfg.palette();
        let question = d.kind == "question";
        // A relayed card names its asker by role, under the question, where rano names a child.
        if let Some(role) = relayed_role(d) {
            let at = 1.min(content.len());
            let said = crate::i18n::tf("card.from_role", &[("role", role)]);
            for (i, l) in rano::agent::text::wrap(&format!("    {said}"), w)
                .into_iter()
                .enumerate()
            {
                content.insert(at + i, rano::agent::text::one(l, rano::style::Role::Faint));
            }
        }
        if !question && d.advice.is_none() && !self.oracle_matters(d) {
            let n = rano::agent::text::wrap(NO_ORACLE, w).len();
            if let Some(at) = content
                .iter()
                .position(|l| l.plain().starts_with("  no oracle was consulted"))
            {
                content.drain(at..(at + n).min(content.len()));
            }
        }
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
        // The shadow's verdict on a relayed card: observe-only, so it sits with the facts.
        if let Some(line) = d.shadow.as_deref().filter(|l| !l.trim().is_empty()) {
            content.push(rano::agent::text::one(
                rano::agent::text::trim_to(&format!("  {line}"), w),
                rano::style::Role::Faint,
            ));
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
                    "  {}",
                    crate::i18n::tf("card.abstain", &[("abstain", ABSTAIN), ("sep", NOTE_SEP)],)
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
                crate::i18n::t("card.behind.clock")
            } else {
                ""
            };
            let said = crate::i18n::tf(
                "card.behind",
                &[("n", &behind.to_string()), ("clock", clock)],
            );
            choices.push(rano::agent::pane::faint(rano::agent::text::trim_to(
                &format!("  {said}"),
                w,
            )));
        }
        // And the way to step away from it without answering: its own row, so a narrow card trims
        // the line above and not this, and on every decision card, because Esc means the same on
        // every one.
        choices.push(rano::agent::pane::faint(rano::agent::text::trim_to(
            &format!("  {}", put_away_hint()),
            w,
        )));
        // The diff last in the content half, so the facts above it stay on the first screenful.
        let mut content = row_strings(&content, p);
        content.extend(self.card_diff_rows(d, w));
        (content, row_strings(&choices, p))
    }

    /// Whether *no oracle was consulted* tells the operator anything: only where this session's
    /// mode has a model decide (`automode`), or the mode is not known. A relayed card's asker runs
    /// under its own mode, which this head does not know.
    fn oracle_matters(&self, d: &OpenDecision) -> bool {
        if relayed_role(d).is_some() {
            return false;
        }
        let mode = self.mode_current();
        let mode = mode.trim();
        mode.is_empty() || mode.starts_with("automode")
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
            // A relayed card's asker is drawn by `decision_card`, by role and not by id and brief.
            subagent: d
                .subagent
                .as_ref()
                .filter(|_| relayed_role(d).is_none())
                .map(|s| card::SubagentAsk {
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
            // A reading that found nothing is left off, and so is the clause that explains it.
            detail: match reading_is_empty(&d.detail) {
                true => String::new(),
                false => d.detail.clone(),
            },
            access: match reading_is_empty(&d.detail) {
                true => String::new(),
                false => d.access.clone(),
            },
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
