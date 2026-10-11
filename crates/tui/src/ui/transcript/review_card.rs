//! **A review's outcome, drawn as ONE line** — and the whole of what the daemon said one click
//! away, in the popup the job card opens.
//!
//! The operator, after the red-target card landed, pasting a session notice that drew as dozens
//! of lines — *"The reviewer of `agent/panic-groupingsets-join` (`s-…`), a branch of your tree,
//! could not decide: The two root causes are the right ones and both patches are correct in
//! themselves. …"* — and asking: *"same story about reviews - … one line + popup"*.
//!
//! # The line
//!
//! ```text
//! review · agent/panic-groupingsets-join · could not decide · BUT the ask's acceptance cannot be met on this base… · click for details
//! ```
//!
//! The branch, the verdict's word, and the first clause of the decisive reason when there is an
//! obvious one ([`decisive`]: the sentence that opens with `BUT` if the reviewer wrote one,
//! otherwise the first), cut to the width with the head's `…`. The verdict is drawn in the role
//! the queue pane gives the row's state (`panes::queue::mark_of`): a refusal and a reviewer that
//! could not decide in the attention register, a review that is blocked or cannot run in the
//! failure register.
//!
//! # The shapes
//!
//! Every text a review's outcome reaches a session or a hub in (`mergequeue::notices`; each
//! pinned as a fixture by `harnessd`'s `the_tuis_review_card_fixtures_are_what_the_queue_writes`):
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
//!
//! An APPROVED review, a superseded one and a withdrawn one say nothing to a session or a hub —
//! they are `QueueMoved` events, which the queue pane reads and the conversation does not draw —
//! so there is no line for them. The red tip's reviewer finding the target broken is a
//! `target_red` warning, and `red_card`'s.
//!
//! # Only what this head can read is folded
//!
//! The reader keys on the opening, the backticked branch, and the words that carry the verdict;
//! anything else is `None` and the row is drawn as before. The reason is a summary — the popup
//! is the text, verbatim — so a reason that reads oddly costs nothing but a clause.

use rano::style::Role;

use crate::ui::panes::queue::mark_of;
use crate::ui::transcript::line_cards::{CLICK, Card, SessionKind, WarningKind, flat};

/// What a line opens with.
const REVIEW: &str = "review";

/// **What the review came to**, in the line's words.
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
        }
    }

    /// **The role the queue pane draws the row's state in** — one register for one state, on
    /// the pane and here.
    pub(crate) fn role(self) -> Role {
        let state = match self {
            Verdict::CouldNotDecide => "cannot_decide",
            Verdict::Refused | Verdict::Vetoed | Verdict::RefusedToldNobody => "rejected",
            Verdict::Blocked
            | Verdict::HandedOver
            | Verdict::CannotRun
            | Verdict::CouldNotStart => "blocked",
        };
        mark_of(state).1
    }
}

/// **One review's outcome, as the daemon's text says it.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Review {
    pub(crate) verdict: Verdict,
    /// The branch, or `main's red tip 60bf819c` for the review of a red target.
    pub(crate) subject: String,
    /// The first clause of the decisive reason, when there is one.
    pub(crate) reason: Option<String>,
}

impl Review {
    fn new(verdict: Verdict, subject: String, reason: Option<String>) -> Review {
        Review {
            verdict,
            subject,
            reason: reason.filter(|r| !r.is_empty()),
        }
    }

    /// **The whole line, uncut.**
    pub(crate) fn line(&self) -> String {
        let mut parts = vec![
            REVIEW.to_string(),
            self.subject.clone(),
            self.verdict.word().to_string(),
        ];
        if let Some(r) = &self.reason {
            parts.push(r.clone());
        }
        parts.push(CLICK.into());
        parts.join(" · ")
    }

    /// The popup's top edge: the branch and the verdict.
    pub(crate) fn title(&self) -> String {
        format!("{REVIEW} · {} · {}", self.subject, self.verdict.word())
    }

    /// **The card**: the verdict's word in its role.
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

/// **The review in the card registry** (`line_cards`).
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
};

/// **And its hub warnings.**
pub(crate) const WARNING: WarningKind = WarningKind {
    codes: &["queue_needs_human", "queue_blocked", "refusal_undelivered"],
    read: warning_card,
};

fn notice_card(text: &str) -> Option<Card> {
    notice_review(text).map(|r| r.card())
}

fn warning_card(detail: &str) -> Option<Card> {
    warning_review(detail).map(|r| r.card())
}

/// **A parent's message, unwrapped** — `parent_message_text`'s `message from your parent
/// session `X`: ` — the door a refusal and a veto reach the branch's session through.
fn unwrap_message(text: &str) -> &str {
    text.strip_prefix("message from your parent session `")
        .and_then(|r| r.split_once("`: "))
        .map_or(text, |(_, body)| body)
}

/// **A branch's name, up to its closing backtick** — `(name, the rest)`; `None` for an empty
/// name or one with a space or a line break in it.
fn ticked(s: &str) -> Option<(&str, &str)> {
    let (name, rest) = s.split_once('`')?;
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    Some((name, rest))
}

/// **What a review is OF, as `parked` opens it**: `` BRANCH` (`ID`), a branch of your tree, ``
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
    let body = unwrap_message(text);
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

/// **Whether `text` opens with a review's notice** — read as far as the verdict, so a row is
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

/// **A session notice, read** — `None` for a shape this head does not know.
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
            Some(Review::new(
                Verdict::Refused,
                branch.to_string(),
                reasons_listed(r),
            ))
        }
        // `refused`.
        "Your branch `" => {
            let (branch, r) = ticked(r)?;
            Some(Review::new(
                Verdict::Refused,
                branch.to_string(),
                reasons_listed(r),
            ))
        }
        // `vetoed`: `: WORDS. It does not land.`
        "The operator VETOED `" => {
            let (branch, r) = ticked(r)?;
            let words = r.strip_prefix(": ")?;
            let (words, _) = words.rsplit_once(". It does not land.")?;
            Some(Review::new(
                Verdict::Vetoed,
                branch.to_string(),
                decisive(words),
            ))
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

/// **A refusal's reasons**, the bullets under `The reviewer's reasons:`, as one decisive clause.
fn reasons_listed(r: &str) -> Option<String> {
    let (_, list) = r.split_once("The reviewer's reasons:\n\n")?;
    let reasons: Vec<&str> = list
        .lines()
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.strip_prefix("  - "))
        .collect();
    decisive(&reasons.join("; "))
}

/// **A hub warning's detail, read** — `None` for a shape this head does not know (a
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
    let r = detail.strip_prefix("the review of `")?;
    let (branch, r) = ticked(r)?;
    let r = r.strip_prefix(" (`")?;
    let (_, r) = r.split_once("`) ")?;
    let branch = branch.to_string();
    if let Some(r) = r.strip_prefix("cannot decide — ") {
        // `queue_needs_human`: `BY could not decide at SHA: REASONS. `/queue approve …`
        let (_, r) = r.split_once(" could not decide at ")?;
        let (_, reasons) = r.split_once(": ")?;
        let (reasons, _) = reasons.rsplit_once(". `/queue approve ")?;
        return Some(Review::new(
            Verdict::CouldNotDecide,
            branch,
            decisive(reasons),
        ));
    }
    if let Some(r) = r.strip_prefix("is BLOCKED after ") {
        // `N attempts without a verdict: WHY `/queue restart …`
        let (_, why) = r.split_once(" attempts without a verdict: ")?;
        let (why, _) = why.rsplit_once(" `/queue restart ")?;
        return Some(Review::new(Verdict::Blocked, branch, decisive(why)));
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

/// **The decisive reason's first clause** — the operator's example: *"for "could not decide"
/// the sentence starting with BUT is the decisive one if present; otherwise the first
/// sentence"*. The reviewer's reasons arrive joined with `; `, so a sentence ends at `. `, `; `
/// or a line break; the clause ends at the first `, and` / `, so` / `, which` / `: ` / ` — ` far
/// enough in to be a clause, and a cut clause says so with `…`.
pub(crate) fn decisive(reasons: &str) -> Option<String> {
    let text = flat(reasons);
    let sentences = sentences(&text);
    let pick = sentences
        .iter()
        .find(|s| s.starts_with("BUT ") || s.starts_with("BUT,"))
        .or_else(|| sentences.first())?;
    Some(first_clause(pick))
}

fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = text.as_bytes();
    for i in 0..b.len() {
        if matches!(b[i], b'.' | b';' | b'!' | b'?') && b.get(i + 1) == Some(&b' ') {
            let s = text[start..i].trim();
            if !s.is_empty() {
                out.push(s);
            }
            start = i + 2;
        }
    }
    let s = text[start.min(text.len())..]
        .trim()
        .trim_end_matches(['.', ';']);
    if !s.is_empty() {
        out.push(s);
    }
    out
}

fn first_clause(sentence: &str) -> String {
    /// A clause shorter than this is not the reason yet (`BUT, as`, `So: `).
    const AT_LEAST: usize = 12;
    const MARKS: &[&str] = &[", and ", ", so ", ", which ", ": ", " — "];
    let cut = MARKS
        .iter()
        .flat_map(|m| sentence.match_indices(m).map(|(i, _)| i))
        .filter(|&i| i >= AT_LEAST)
        .min();
    match cut {
        Some(i) => format!("{}…", &sentence[..i]),
        None => sentence.to_string(),
    }
}
