//! How many of a project's merge-queue rows are stuck, and how long the oldest has been.
//! The queue tells a project's session about a row ENTERING a bad state, once, as it
//! happens (`notices::merge_failed`, `notices::merge_conflict`).

use letibot_tokencore::store::{MergeRow, MergeState, Store};

use super::desk;

/// How long a row must have been stuck before the census says so at all. The transition's
/// own notice arrives the moment the row goes bad, so a census a minute later is that news
/// twice.
pub const STUCK_AFTER_MS: u64 = 15 * 60 * 1000;

/// How often the census is said again while the rows stay stuck - the rate this notice has
/// of its own. Two hours, in the queue's own units
pub const STUCK_REPEAT_MS: u64 = 2 * 60 * 60 * 1000;

/// A landing row that is stuck: nobody is on it and no pass will move it. The non-open half of
/// `desk::parked_merge`, which is the half `status` prints and a pass does not pick up.
pub fn stuck_merge(m: MergeState) -> bool {
    desk::parked_merge(m) && !m.is_open()
}

/// One project's stuck landing rows, counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stuck {
    /// `(state, count)`, only the states that have rows, in `desk::FLOW`'s order - the same order
    /// `status` prints, so the two lines read alike.
    pub states: Vec<(&'static str, usize)>,
    /// Every stuck row.
    pub total: usize,
    /// The `state_ms` of the oldest stuck row, `0` when there is none: when it entered the state
    /// it is stuck in, which is what the age is measured from.
    pub oldest_ms: u64,
}

impl Stuck {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// How long the oldest stuck row has been stuck, as of `now_ms`.
    pub fn oldest_age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.oldest_ms)
    }

    /// Whether the census is the idle pass's business: there are rows stuck, and the oldest has
    /// been stuck longer than [`STUCK_AFTER_MS`]. One row that went bad a minute ago is the
    /// transition's, and saying it again adds nothing.
    pub fn is_news(&self, now_ms: u64) -> bool {
        !self.is_empty() && self.oldest_age_ms(now_ms) >= STUCK_AFTER_MS
    }

    /// How long until it is: the deadline a clock arms for when rows are stuck but too young. The
    /// crossing is not an event anything rings, so without a deadline here the census would wait
    /// for a turn a quiet session may never take. `None` when nothing is stuck or it is news.
    pub fn news_in_ms(&self, now_ms: u64) -> Option<u64> {
        if self.is_empty() || self.is_news(now_ms) {
            return None;
        }
        Some(STUCK_AFTER_MS - self.oldest_age_ms(now_ms))
    }

    /// The counts as one clause - `3 failed · 5 conflict`.
    pub fn said(&self) -> String {
        self.states
            .iter()
            .map(|(s, n)| format!("{n} {s}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// The stuck rows of the repositories a session is the project of. `repositories` is what
/// `mergequeue::repositories_of_workspace_cached` answers for the session's workspace - one
/// repository, or each of a one-project folder's.
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

/// A duration in the words a person reads an alarm in - `40 s`, `18 min`, `4h 12m`. Hours are the
/// unit this exists for: `252 min` is a number a reader has to convert.
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
