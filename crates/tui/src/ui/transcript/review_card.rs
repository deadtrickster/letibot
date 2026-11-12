//! A review's outcome, drawn as ONE line - and the whole of what the daemon said one click
//! away, in the popup the job card opens.
//!
//! A session notice drew as dozens of lines; the ask was one line plus a popup, as for the red target.
//!
//! # The line
//!
//! ```text
//! review · agent/panic-groupingsets-join · could not decide · BUT the ask's acceptance cannot be met on this base… · click for details
//! ```
//!
//! The branch, the verdict's word, and the first clause of the decisive reason ([`decisive`]: the sentence opening with `BUT`, else the first),
//! cut to the width. The verdict takes the role the queue pane gives the row's state (`panes::queue::mark_of`): refusal and could-not-decide
//! in the attention register, blocked or cannot-run in the failure register.
//!
//! # The shapes
//!
//! Every text a review's outcome reaches a session or a hub in (`mergequeue::notices`, each pinned by `harnessd`'s `the_tuis_review_card_fixtures_are_what_the_queue_writes`):
//!
//! | text | where | verdict |
//! |---|---|---|
//! | `parked`, a branch whose reviewer could not decide | the branch's root session | could not decide |
//! | `parked`, a branch whose review is blocked | the branch's root session | blocked |
//! | `parked`, the red tip's reviewer could not decide | the repository's root sessions | could not decide |
//! | `parked`, the red tip's review is blocked | the repository's root sessions | blocked |
//! | `refused`, the refusal | the child that wrote the branch, as a parent's message | refused |
//! | `refused_to_root`, the refusal a child could not be told | its root, as a parent's message | refused |
//! | `vetoed`, the operator's veto | the child, as a parent's message | vetoed |
//! | `handed_over`, a review handed to the project | the project's main session | handed to you |
//! | `queue_needs_human` | every hub | could not decide |
//! | `queue_blocked`, attempts spent | every hub | blocked |
//! | `queue_blocked`, a reviewer that could not start | every hub | reviewer could not start |
//! | `queue_blocked`, a review that cannot run | every hub | cannot run |
//! | `refusal_undelivered` | every hub | refused, told nobody |
//! | `queue_waiting_for_vm`, no free VM slot for the reviewer | every hub | waiting for a VM slot |
//! | `queue_vm_slots_exhausted`, that wait past 30 minutes | every hub | VM slots exhausted |
//!
//! `queue_needs_human` and `queue_blocked` (attempts spent) have two shapes each: the daemon's current one and the older one an older
//! daemon still writes. An APPROVED, superseded or withdrawn review is a `QueueMoved` event, which the conversation does not draw. The red tip's
//! reviewer finding the target broken is a `target_red` warning, and `red_card`'s.
//!
//! The reader keys on the opening, the backticked branch and the words that carry the verdict; anything else is `None` and the row
//! is drawn as before. The reason is a summary, and the popup is the text verbatim.

use rano::style::Role;

use crate::ui::panes::queue::mark_of;
use crate::ui::transcript::line_cards::{
    CLICK, Card, SessionKind, WarningKind, first_clause, flat, sentences,
};

/// What a line opens with.
const REVIEW: &str = "review";

/// What the review came to, in the line's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The reviewer could not decide; a person is asked.
    CouldNotDecide,
    /// The review is blocked: it could not be run, or its attempts are spent.
    Blocked,
    /// The reviewer refused the branch.
    Refused,
    /// The operator vetoed it.
    Vetoed,
    /// The queue could not review it and handed it to the project's main session.
    HandedOver,
    /// The review cannot run at all.
    CannotRun,
    /// No reviewer could be started.
    CouldNotStart,
    /// Refused, and the complaints reached nobody.
    RefusedToldNobody,
    /// Waiting for a free VM slot - firecode had none for the reviewer; not blocked, no
    /// attempt counted (`queue_waiting_for_vm`).
    WaitingForVm,
    /// A VM-slot wait gone long - slots are not freeing on their own
    /// (`queue_vm_slots_exhausted`).
    SlotsExhausted,
}

impl Verdict {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Verdict::CouldNotDecide => "could not decide",
            Verdict::Blocked => "blocked",
            Verdict::Refused => "refused",
            Verdict::Vetoed => "vetoed",
            Verdict::HandedOver => "handed to you",
            Verdict::CannotRun => "cannot run",
            Verdict::CouldNotStart => "reviewer could not start",
            Verdict::RefusedToldNobody => "refused, told nobody",
            Verdict::WaitingForVm => "waiting for a VM slot",
            Verdict::SlotsExhausted => "VM slots exhausted",
        }
    }

    /// The role the queue pane draws the row's state in - one register for one state, on
    /// the pane and here.
    pub(crate) fn role(self) -> Role {
        let state = match self {
            Verdict::CouldNotDecide => "cannot_decide",
            Verdict::Refused | Verdict::Vetoed | Verdict::RefusedToldNobody => "rejected",
            Verdict::WaitingForVm => "waiting",
            Verdict::Blocked
            | Verdict::SlotsExhausted
            | Verdict::HandedOver
            | Verdict::CannotRun
            | Verdict::CouldNotStart => "blocked",
        };
        mark_of(state).1
    }
}

/// One review's outcome, as the daemon's text says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Review {
    pub(crate) verdict: Verdict,
    /// The branch, or `main's red tip 60bf819c` for the review of a red target.
    pub(crate) subject: String,
    /// Who judged it - the reviewer's session when the text names one, `you` for the
    /// operator's own veto. `None` when the daemon's words do not say, and the line then says
    /// nothing rather than inventing a name.
    pub(crate) by: Option<String>,
    /// The first clause of the decisive reason, when there is one.
    pub(crate) reason: Option<String>,
}

impl Review {
    fn new(verdict: Verdict, subject: String, reason: Option<String>) -> Review {
        Review {
            verdict,
            subject,
            by: None,
            reason: reason.filter(|r| !r.is_empty()),
        }
    }

    /// The same, naming who judged it.
    fn by(mut self, who: Option<&str>) -> Review {
        self.by = who
            .map(|w| w.trim_start_matches("reviewer:").trim().to_string())
            .filter(|w| !w.is_empty());
        self
    }

    /// The verdict's whole phrase - `refused by s-gatekeeper-0123`, so the outcome and who
    /// judged it are read together and cannot be parted by a cut.
    pub(crate) fn verdict_words(&self) -> String {
        match &self.by {
            Some(who) => format!("{} by {who}", self.verdict.word()),
            None => self.verdict.word().to_string(),
        }
    }

    /// The whole line, uncut.
    pub(crate) fn line(&self) -> String {
        let mut parts = vec![
            REVIEW.to_string(),
            self.subject.clone(),
            self.verdict_words(),
        ];
        if let Some(r) = &self.reason {
            parts.push(r.clone());
        }
        parts.push(CLICK.into());
        parts.join(" · ")
    }

    /// The popup's top edge: the branch, the verdict, and who judged it.
    pub(crate) fn title(&self) -> String {
        format!("{REVIEW} · {} · {}", self.subject, self.verdict_words())
    }

    /// The card: the verdict's word in its role.
    pub(crate) fn card(&self) -> Card {
        let from = REVIEW.len() + " · ".len() + self.subject.len() + " · ".len();
        Card::new(
            self.line(),
            from,
            self.verdict.word(),
            self.verdict.role(),
            self.title(),
        )
    }
}

/// The review in the card registry (`line_cards`).
pub(crate) const SESSION: SessionKind = SessionKind {
    opens: opens_notice,
    ends: &[
        // `parked`, a branch that could not be decided.
        "a new head goes back to review by itself.",
        // `parked`, a blocked branch.
        "(the merge gate still runs).",
        // `parked`, the red tip.
        "gates the tip again.",
        // `refused`.
        "(the verdict is bound to the head it judged).",
        // `handed_over`.
        "which lands it through the queue's own gate.",
    ],
    read: notice_card,
    carries: &[],
};

/// And its hub warnings.
pub(crate) const WARNING: WarningKind = WarningKind {
    codes: &[
        "queue_needs_human",
        "queue_blocked",
        "refusal_undelivered",
        "queue_waiting_for_vm",
        "queue_vm_slots_exhausted",
    ],
    read: warning_card,
    from_code: true,
};

fn notice_card(text: &str) -> Option<Card> {
    notice_review(text).map(|r| r.card())
}

fn warning_card(detail: &str) -> Option<Card> {
    warning_review(detail).map(|r| r.card())
}

/// A message between sessions, unwrapped: the two openings `message_from_a_session` writes say which way the edge
/// runs. Knowing only the parent one would draw every refusal with its envelope on. Answers the SENDER too - for a
/// refusal that is the reviewer, and the line says who judged the branch.
fn unwrap_message(text: &str) -> (Option<&str>, &str) {
    [
        "message from your parent session `",
        "message from session `",
    ]
    .into_iter()
    .find_map(|open| {
        text.strip_prefix(open)
            .and_then(|r| r.split_once("`: "))
            .map(|(who, body)| (Some(who), body))
    })
    .unwrap_or((None, text))
}

/// A branch's name, up to its closing backtick - `(name, the rest)`; `None` for an empty
/// name or one with a space or a line break in it.
fn ticked(s: &str) -> Option<(&str, &str)> {
    let (name, rest) = s.split_once('`')?;
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    Some((name, rest))
}

/// What a review is OF, as `parked` opens it: `` BRANCH` (`ID`), a branch of your tree, ``
/// or `` TARGET`'s red tip SHA (`ID`) ``. Answers the subject and the text after it.
fn parked_subject(s: &str) -> Option<(String, &str)> {
    let (name, rest) = ticked(s)?;
    if let Some(r) = rest.strip_prefix("'s red tip ") {
        let (sha, r) = r.split_once(" (`")?;
        if sha.len() < 7 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let (_, r) = r.split_once("`) ")?;
        let short: String = sha.chars().take(8).collect();
        return Some((format!("{name}'s red tip {short}"), r));
    }
    let r = rest.strip_prefix(" (`")?;
    let (_, r) = r.split_once("`), a branch of your tree, ")?;
    Some((name.to_string(), r))
}

/// The opening of a notice this module reads, and what follows it.
fn opening(text: &str) -> Option<(&'static str, &str)> {
    let (_, body) = unwrap_message(text);
    [
        "The reviewer of `",
        "The review of `",
        "Your branch `",
        "The operator VETOED `",
        "The review queue could not review `",
    ]
    .into_iter()
    .find_map(|o| body.strip_prefix(o).map(|r| (o, r)))
}

/// Whether `text` opens with a review's notice - read as far as the verdict, so a row is
/// split only before a notice this head would draw as a card.
fn opens_notice(text: &str) -> bool {
    let Some((open, r)) = opening(text) else {
        return false;
    };
    match open {
        "The reviewer of `" => {
            parked_subject(r).is_some_and(|(_, r)| r.starts_with("could not decide: "))
        }
        "The review of `" => {
            parked_subject(r).is_some_and(|(_, r)| r.starts_with("is BLOCKED"))
                || ticked(r)
                    .is_some_and(|(_, r)| r.starts_with(" — the branch of a child you started (`"))
        }
        "Your branch `" => {
            ticked(r).is_some_and(|(_, r)| r.starts_with(" was REFUSED by the review at "))
        }
        "The operator VETOED `" => ticked(r).is_some_and(|(_, r)| r.starts_with(": ")),
        "The review queue could not review `" => {
            ticked(r).is_some_and(|(_, r)| r.starts_with(" (row `"))
        }
        _ => false,
    }
}

/// A session notice, read - `None` for a shape this head does not know.
pub(crate) fn notice_review(text: &str) -> Option<Review> {
    if !opens_notice(text) {
        return None;
    }
    let (open, r) = opening(text)?;
    match open {
        // `parked`: `could not decide: REASONS. The operator is asked`.
        "The reviewer of `" => {
            let (subject, r) = parked_subject(r)?;
            let reasons = r.strip_prefix("could not decide: ")?;
            let (reasons, _) = reasons.rsplit_once(". The operator is asked")?;
            Some(Review::new(
                Verdict::CouldNotDecide,
                subject,
                decisive(reasons),
            ))
        }
        "The review of `" => {
            if let Some((subject, r)) = parked_subject(r) {
                // `parked`: a branch `is BLOCKED and nothing lands it: FAILURE\n\n…`, the red tip
                // `is BLOCKED: FAILURE\n\n…`.
                let failure = r
                    .strip_prefix("is BLOCKED and nothing lands it: ")
                    .or_else(|| r.strip_prefix("is BLOCKED: "))?;
                let failure = failure.split("\n\n").next().unwrap_or(failure);
                return Some(Review::new(Verdict::Blocked, subject, decisive(failure)));
            }
            // `refused_to_root`.
            let (branch, r) = ticked(r)?;
            if !r.contains("REFUSED it at ") {
                return None;
            }
            Some(
                Review::new(Verdict::Refused, branch.to_string(), reasons_listed(r))
                    .by(unwrap_message(text).0),
            )
        }
        // `refused`: the sender of the message IS the reviewer that judged the branch.
        "Your branch `" => {
            let (branch, r) = ticked(r)?;
            Some(
                Review::new(Verdict::Refused, branch.to_string(), reasons_listed(r))
                    .by(unwrap_message(text).0),
            )
        }
        // `vetoed`: `: WORDS. It does not land.`
        "The operator VETOED `" => {
            let (branch, r) = ticked(r)?;
            let words = r.strip_prefix(": ")?;
            let (words, _) = words.rsplit_once(". It does not land.")?;
            Some(Review::new(Verdict::Vetoed, branch.to_string(), decisive(words)).by(Some("you")))
        }
        // `handed_over`: `(row `ID`): FAILURE This project merges such a branch`.
        "The review queue could not review `" => {
            let (branch, r) = ticked(r)?;
            let r = r.strip_prefix(" (row `")?;
            let (_, failure) = r.split_once("`): ")?;
            let (failure, _) = failure.rsplit_once(" This project merges such a branch")?;
            Some(Review::new(
                Verdict::HandedOver,
                branch.to_string(),
                decisive(failure),
            ))
        }
        _ => None,
    }
}

/// A refusal's reasons, the bullets under `The reviewer's reasons:`, as one decisive clause.
fn reasons_listed(r: &str) -> Option<String> {
    let (_, list) = r.split_once("The reviewer's reasons:\n\n")?;
    let reasons: Vec<&str> = list
        .lines()
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.strip_prefix("  - "))
        .collect();
    decisive(&reasons.join("; "))
}

/// A hub warning's detail, read - `None` for a shape this head does not know (a
/// `queue_blocked` about a landing, for one: that is the merge queue's, not a review's).
pub(crate) fn warning_review(detail: &str) -> Option<Review> {
    if let Some(r) = detail.strip_prefix("a reviewer for `") {
        // `a reviewer for `B` could not start: WHY. The review is BLOCKED (`ID`): …`
        let (branch, r) = ticked(r)?;
        let why = r.strip_prefix(" could not start: ")?;
        let (why, _) = why.rsplit_once(". The review is BLOCKED (`")?;
        return Some(Review::new(
            Verdict::CouldNotStart,
            branch.to_string(),
            decisive(why),
        ));
    }
    if let Some(r) = detail.strip_prefix("VM SLOTS EXHAUSTED: the review of `") {
        // `queue_vm_slots_exhausted`: `… (`ID`) has waited N min for a free VM slot (BUSY). …`
        let (branch, r) = ticked(r)?;
        let r = r.strip_prefix(" (`")?;
        let (_, r) = r.split_once("`) has waited ")?;
        let (mins, r) = r.split_once(" min for a free VM slot (")?;
        let (busy, _) = r.split_once("). ")?;
        return Some(Review::new(
            Verdict::SlotsExhausted,
            branch.to_string(),
            Some(format!("waited {mins} min, {}", busy_words(busy))),
        ));
    }
    let r = detail.strip_prefix("the review of `")?;
    let (name, r) = ticked(r)?;
    // The red tip's `queue_needs_human`: `` main`'s red tip (`ID`) stops for a person… ``.
    if let Some(r) = r.strip_prefix("'s red tip (`") {
        let (_, r) = r.split_once("`) ")?;
        let (head, by, reason) = stops_for_a_person(r)?;
        return Some(
            Review::new(
                Verdict::CouldNotDecide,
                format!("{name}'s red tip {head}"),
                reason,
            )
            .by(by),
        );
    }
    let r = r.strip_prefix(" (`")?;
    let (_, r) = r.split_once("`) ")?;
    let branch = name.to_string();
    if let Some((_, by, reason)) = stops_for_a_person(r) {
        return Some(Review::new(Verdict::CouldNotDecide, branch, reason).by(by));
    }
    if let Some(r) = r.strip_prefix("cannot decide — ") {
        // The older `queue_needs_human`: `BY could not decide at SHA: REASONS. `/queue approve …`
        let (by, r) = r.split_once(" could not decide at ")?;
        let (_, reasons) = r.split_once(": ")?;
        let (reasons, _) = reasons.rsplit_once(". `/queue approve ")?;
        return Some(Review::new(Verdict::CouldNotDecide, branch, decisive(reasons)).by(Some(by)));
    }
    if let Some(r) = r.strip_prefix("is BLOCKED: ") {
        // `queue_blocked`, attempts spent: `N attempts to review it at HEAD ended without a
        // verdict - no reviewer judged the branch: (1) …; (n) LAST.[ Each was a reviewer that
        // could not be PLACED …] `/queue restart …`
        let (n, r) = r.split_once(" attempts to review it at ")?;
        let (head, r) = r.split_once(" ended without a verdict — ")?;
        let spent = format!("{n} attempts at {head} ended without a verdict");
        let cause = if r.contains("could not be PLACED") {
            Some("the reviewer's VM did not start".to_string())
        } else {
            let (list, _) = r.split_once(" `/queue restart ")?;
            let last = list.rsplit_once("; (").map_or(list, |(_, l)| l);
            let last = last.split_once(") ").map_or(last, |(_, l)| l);
            decisive(last.trim_end_matches('.'))
        };
        return Some(Review::new(
            Verdict::Blocked,
            branch,
            Some(match cause {
                Some(c) => format!("{spent} — {c}"),
                None => spent,
            }),
        ));
    }
    if let Some(r) = r.strip_prefix("is BLOCKED after ") {
        // The older form: `N attempts without a verdict: WHY `/queue restart …`
        let (_, why) = r.split_once(" attempts without a verdict: ")?;
        let (why, _) = why.rsplit_once(" `/queue restart ")?;
        return Some(Review::new(Verdict::Blocked, branch, decisive(why)));
    }
    if let Some(r) = r.strip_prefix("is waiting for a free VM slot: ") {
        // `queue_waiting_for_vm`: `BUSY. It is not blocked and no attempt is counted; …`
        let (busy, _) = r.split_once(". It is not blocked")?;
        return Some(Review::new(
            Verdict::WaitingForVm,
            branch,
            Some(busy_words(busy)),
        ));
    }
    if let Some(why) = r.strip_prefix("cannot run and is BLOCKED: ") {
        let (why, _) = why
            .rsplit_once(" This project merges such a branch via its main session")
            .or_else(|| why.rsplit_once(" Nothing lands it until a person acts"))?;
        return Some(Review::new(Verdict::CannotRun, branch, decisive(why)));
    }
    if r.starts_with("refused it, and the complaints reached nobody (") {
        return Some(Review::new(Verdict::RefusedToldNobody, branch, None));
    }
    None
}

/// `stops for a person: the reviewer could not decide at HEAD - "DECISIVE" (BY; …` - the
/// head, the reviewer that could not decide, and the decisive sentence's first clause.
fn stops_for_a_person(r: &str) -> Option<(String, Option<&str>, Option<String>)> {
    let r = r.strip_prefix("stops for a person: the reviewer could not decide at ")?;
    let (head, r) = r.split_once(" — \"")?;
    let (said, _) = r.split_once("; every reason is on the row)")?;
    let (said, by) = said.rsplit_once("\" (")?;
    Some((head.to_string(), Some(by), decisive(said)))
}

/// *"4 of 4 VM slots are busy"* as the line says it: *"4 of 4 busy"*.
fn busy_words(busy: &str) -> String {
    busy.replace(" VM slots are busy", " busy")
        .replace("every VM slot is busy", "every slot busy")
}

/// The decisive reason's first clause: a sentence starting with BUT wins, else the first
/// sentence, and the clause ends at the first `, and` / `, so` / `, which` / `: ` / ` - ` far
/// enough in to count. A cut clause ends in an ellipsis rather than being left to guess.
pub(crate) fn decisive(reasons: &str) -> Option<String> {
    let text = flat(reasons);
    let sentences = sentences(&text);
    let pick = sentences
        .iter()
        .find(|s| s.starts_with("BUT ") || s.starts_with("BUT,"))
        .or_else(|| sentences.first())?;
    Some(first_clause(pick))
}
