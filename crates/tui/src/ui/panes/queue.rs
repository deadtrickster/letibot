//! **The merge queue**: what is waiting to land, and a review's output — drawn by
//! `rano::agent::queue` from the queue and the reviews this head holds.

use crate::app::*;
use crate::ui::render::{dur_human, row_strings};
use rano::agent::queue::{
    MergeMark, OpenedEntry, QueueEntry, QueueEntryView, QueuePane, ReviewRecord, ReviewState,
};

impl App {
    /// **The merge queue, as rows** — one entry per row plus its state and reason beneath it.
    ///
    /// Two lines an entry, like the jobs pane's rows and for the same reason: the facts a
    /// reader needs are (what is it) and (why is it where it is), and a single line would
    /// truncate the second to make room for the first.
    ///
    /// **The order is the daemon's** and is not sorted here. The queue is the queue's own
    /// scheduling (`created_ms`, then the priority), and a head that re-sorted it would be a
    /// second opinion about what should land next.
    ///
    /// Takes `&mut self` only to record where each row was drawn, which is what the arrows and
    /// a click scroll by.
    pub(crate) fn queue_lines(&mut self, w: usize) -> Vec<String> {
        use letibot_sessionlog::event::MergeState as S;
        let pane = QueuePane {
            entries: self
                .merge
                .iter()
                .map(|e| QueueEntry {
                    id: e.id.clone(),
                    branch: e.branch.clone(),
                    state: merge_state_word(e.state).to_string(),
                    // **The state word is the daemon's**, and the mark is this head's reading of
                    // it — the same split the jobs pane keeps.
                    mark: match e.state {
                        S::Waiting => MergeMark::Waiting,
                        S::Taken => MergeMark::Taken,
                        S::Landed => MergeMark::Landed,
                        S::Failed | S::Conflict | S::Stale => MergeMark::Parked,
                    },
                    age: dur_human(self.now_ms.saturating_sub(e.created_ms)),
                    // **The reason, and the review.** *No verdict yet* and *nobody has asked*
                    // are different facts — see [`App::review_of`].
                    review: match self.review_of(&e.id) {
                        None => ReviewState::NotAsked,
                        Some(r) => match &r.decision {
                            None => ReviewState::Asked,
                            Some(d) => ReviewState::Decided(d.clone()),
                        },
                    },
                    evidence: e.evidence.clone(),
                })
                .collect(),
            selected: self.queue_sel,
        };
        let content = pane.content(w);
        self.queue_stop_rows = content.stop_rows;
        row_strings(&content.lines, self.cfg.palette())
    }

    /// **One entry, whole** — the overlay the queue pane's Enter opens.
    ///
    /// Everything here is a row the head already holds: the entry's own fields, the evidence
    /// (which is the gate's own captured words when the gate failed, and the reviewer's
    /// rendered verdict when the review refused it), and the review record. Nothing is read
    /// again on the keypress, which is why a `MergeEntryMoved` arriving while the overlay is
    /// open shows the NEW state rather than the state at the moment Enter was pressed.
    ///
    /// The entry is looked up by id every draw, and an id the queue no longer holds — a
    /// `recover` moved it, or a head switched and took a fresh snapshot — says so rather than
    /// drawing an empty overlay.
    pub(crate) fn queue_out_lines(&self, w: usize) -> Vec<String> {
        let Some(id) = self.queue_open.as_deref() else {
            return Vec::new();
        };
        let entry = self.merge.iter().find(|e| e.id == id).map(|e| OpenedEntry {
            branch: e.branch.clone(),
            state: merge_state_word(e.state).to_string(),
            priority: merge_priority_word(e.priority).to_string(),
            base_sha: e.base_sha.clone(),
            age: dur_human(self.now_ms.saturating_sub(e.created_ms)),
            worktree: e.worktree.clone(),
            landed_sha: e.landed_sha.clone(),
            needs: e.needs.clone(),
            brief: e.brief.clone(),
            evidence: e.evidence.clone(),
            review: self.review_of(&e.id).map(|r| ReviewRecord {
                decision: r.decision.clone(),
                asked: dur_human(self.now_ms.saturating_sub(r.asked_ms)),
                session_id: r.session_id.clone(),
                reasons: r.reasons.clone(),
                files: r.files.clone(),
                commands: r.commands.clone(),
            }),
        });
        let view = QueueEntryView {
            id: id.to_string(),
            entry,
        };
        row_strings(&view.lines(w), self.cfg.palette())
    }
}

/// **Where a merge-queue entry is, as the head spells it** — the wire's word, and the one the
/// pane draws. A free function rather than a method on the wire type, which this crate does not
/// own, and a `match` rather than a `Debug` format so a state added to the closed set fails to
/// compile here rather than arriving as a word nobody recognises.
pub(crate) fn merge_state_word(state: letibot_sessionlog::event::MergeState) -> &'static str {
    use letibot_sessionlog::event::MergeState as S;
    match state {
        S::Waiting => "waiting",
        S::Taken => "taken",
        S::Landed => "landed",
        S::Failed => "failed",
        S::Conflict => "conflict",
        S::Stale => "stale",
    }
}

/// **An entry's rung, as the head spells it** — [`merge_state_word`]'s sibling, one field over.
pub(crate) fn merge_priority_word(
    priority: letibot_sessionlog::event::MergePriority,
) -> &'static str {
    use letibot_sessionlog::event::MergePriority as P;
    match priority {
        P::Urgent => "urgent",
        P::Subagent => "subagent",
    }
}
