//! **How many of a project's merge-queue rows are stuck, and how long the oldest has been.**
//!
//! The queue tells a project's session about a row ENTERING a bad state — once, as it happens
//! (`notices::merge_failed`, `notices::merge_conflict`). Nothing anywhere told it the TOTAL, and
//! nothing told it the AGE. MEASURED 2026-11-01 on the operator's daemon: eight rows sat stuck for
//! hours — three `failed`, five `conflict` — and every one of those transitions *was* notified,
//! once, as it happened. What was missing is the census.
//!
//! **One counting rule.** These are the landing rows `desk::status` counts as parked, minus the
//! ones still the queue's own business (`is_open`): a `queued` landing is a pass's work, while a
//! `failed`, `timed_out` or `conflict` landing is nobody's until the project or a person acts. The
//! counts are built by the same `desk::counts_of` over the same `desk::parked_merge`, so they are
//! a sub-line of `status`'s own line rather than a second opinion — and a test holds the two
//! together, because a count nobody checks is a count that drifts.
//!
//! **The review queue is deliberately NOT counted**, and that is the difference between a sentence
//! that is true and one that is not. `serve.rs` re-seats a `rejected` or `cannot_decide` review to
//! `waiting` by itself when the branch moved past the verdict it gave, so *no pass will move them*
//! — which is what this census promises — is false for exactly those rows. It would also keep the
//! census non-empty for ever: one lingering rejection would hold the alarm open over a queue that
//! is otherwise clean. A verdict a person owes is `notices::parked`'s subject, said once when the
//! row parks, and the pane's.
//!
//! **And a clock, because a row STAYING bad is not an event.** `is_news` is what makes the census
//! the idle pass's business: a row that went bad a minute ago was the transition's to announce,
//! and a row that has been bad for a quarter of an hour is nobody's but this.

use letibot_tokencore::store::{MergeRow, MergeState, Store};

use super::desk;

/// **How long a row must have been stuck before the census says so at all.**
///
/// The transition's own notice arrives the moment the row goes bad, so a census a minute later is
/// that news twice. A quarter of an hour is longer than anything the queue does by itself — its
/// pass is `REVIEW_PASS_SECS` (30 s), a reviewer's start grace is `REVIEWER_START_GRACE_MS`
/// (5 min), and a landing's gate is bounded by the project's own `[queue] gate_deadline` — so a row
/// still bad at fifteen minutes is one no pass is going to move. It is also short enough that the
/// operator's *"eight rows sat stuck for hours"* would have been said in the first quarter of it.
pub const STUCK_AFTER_MS: u64 = 15 * 60 * 1000;

/// **How often the census is said again while the rows stay stuck** — the rate this notice has of
/// its own, and the number the brief asked to be picked rather than inherited.
///
/// Two hours, and the reasons are the queue's own units: a review's deadline is
/// `REVIEW_DEADLINE_MS` (60 min), so **two full deadlines pass between two notices** and a repeat
/// can never arrive before the queue has had a real chance to move the rows; and it bounds the
/// wall — eight rows stuck for a night (10 h) produce at most five notices, where the idle check's
/// own ladder would have produced twenty.
///
/// It does not decay, because it does not have to: the sentence carries the AGE, so every repeat
/// is a different fact — the number grew by two hours — rather than the same news said again. A
/// ladder on top of a growing number would be two mechanisms doing one job. This is a number to
/// tune against a real queue rather than a property of the mechanism.
pub const STUCK_REPEAT_MS: u64 = 2 * 60 * 60 * 1000;

/// **A landing row that is stuck** — nobody is on it and no pass will move it. The non-open half of
/// `desk::parked_merge`, which is the half `status` prints and a pass does not pick up.
pub fn stuck_merge(m: MergeState) -> bool {
    desk::parked_merge(m) && !m.is_open()
}

/// **One project's stuck landing rows, counted.**
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stuck {
    /// `(state, count)`, only the states that have rows, in `desk::FLOW`'s order — the same order
    /// `status` prints, so the two lines read alike.
    pub states: Vec<(&'static str, usize)>,
    /// Every stuck row.
    pub total: usize,
    /// The `state_ms` of the oldest stuck row, `0` when there is none — when it entered the state
    /// it is stuck in, which is what the age is measured from.
    pub oldest_ms: u64,
}

impl Stuck {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// **How long the oldest stuck row has been stuck**, as of `now_ms`.
    pub fn oldest_age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.oldest_ms)
    }

    /// **Whether the census is the idle pass's business** — there are rows stuck, and the oldest
    /// has been stuck longer than [`STUCK_AFTER_MS`]. One row that went bad a minute ago is the
    /// transition's, and saying it again adds nothing.
    pub fn is_news(&self, now_ms: u64) -> bool {
        !self.is_empty() && self.oldest_age_ms(now_ms) >= STUCK_AFTER_MS
    }

    /// **How long until it is** — the deadline a clock arms for when rows are stuck but too young,
    /// for the reason `sessions::when_a_young_child_grows_up` arms one: the crossing is not an
    /// event anything rings, so without a deadline here the census would wait for a turn that a
    /// quiet session may never take. `None` when there is nothing stuck or it is already news.
    pub fn news_in_ms(&self, now_ms: u64) -> Option<u64> {
        if self.is_empty() || self.is_news(now_ms) {
            return None;
        }
        Some(STUCK_AFTER_MS - self.oldest_age_ms(now_ms))
    }

    /// **The counts as one clause** — `3 failed · 5 conflict`.
    pub fn said(&self) -> String {
        self.states
            .iter()
            .map(|(s, n)| format!("{n} {s}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// **The stuck rows of the repositories a session is the project of.**
///
/// `repositories` is what `mergequeue::repositories_of_workspace_cached` answers for the session's
/// workspace — one repository, or each of a one-project folder's. An empty list is a session in no
/// repository, and it has no queue to be told about.
///
/// **Scoped, because this runs on a door that every turn ends at.** `merge_rows(Some(repo))` is
/// what the queue's own readers use; the unscoped form materialises every row of every repository
/// on the box — evidence blobs and gate-step output included — to then throw all but one
/// project's away. The rows read here are one repository's, and the age is a `min` over them.
pub fn of(store: &Store, repositories: &[String]) -> Result<Stuck, String> {
    let mut rows: Vec<MergeRow> = Vec::new();
    for repo in repositories {
        rows.extend(
            store
                .merge_rows(Some(repo))
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|m| stuck_merge(m.state)),
        );
    }
    let oldest_ms = rows.iter().map(|m| m.state_ms).min().unwrap_or(0);
    Ok(Stuck {
        states: desk::counts_of(rows.iter().map(|m| m.state.as_str())),
        total: rows.len(),
        oldest_ms,
    })
}

/// **A duration in the words a person reads an alarm in** — `40 s`, `18 min`, `4h 12m`. Hours are
/// the unit this exists for: the incident it was written from ran for most of a night, and `252
/// min` is a number a reader has to convert.
pub fn age_words(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests;
