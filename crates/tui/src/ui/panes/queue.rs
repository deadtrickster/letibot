//! **The merge queue**: what is waiting to land, and a review's output — drawn by
//! `rano::agent::queue` from the queue and the reviews this head holds.

use crate::app::*;
use crate::ui::render::{dur_human, row_strings};
use rano::agent::queue::{
    MergeMark, OpenedEntry, QueueEntry, QueueEntryView, QueuePane, ReviewRecord, ReviewState,
};
use rano::style::Role;

impl App {
    /// **The queue's standings** — how many entries stand in each of the three states a
    /// reader reads the queue for, folded from the same snapshot the pane draws.
    ///
    /// # Where the words come from, and the one state with none
    ///
    /// `waiting` is the daemon's word for *not taken yet* — the gatekeeper's review is what
    /// it is waiting for, and *in review* is the operator's own phrase for it. `taken` is a
    /// merge in flight. `failed`, `conflict`, `stale` and `vetoed` are the four the queue has
    /// stopped moving by itself, and **`parked` is the one word for all four** — what a person
    /// has to act on, which is why it is the count the edge is loudest about. Those four are
    /// not flattened into one state anywhere else (the pane draws `vetoed` in a different
    /// mark from the machine's three, deliberately), so the word is this line's, and the pane
    /// is where a reader goes to tell them apart.
    ///
    /// **`landed` has no standing word and none is invented.** A landed entry is finished —
    /// it stays in the queue only because a later entry names it in `needs` — so counting it
    /// as *in review* or *parked* would be a lie about where the work is, and calling it
    /// anything of its own would invent a fourth standing nothing acts on. The standings
    /// count what stands; a landed entry is drawn by the pane and by nothing here.
    ///
    /// # Why the edge carries it at all
    ///
    /// It is the fact a `merge_queued` note carries, and the reason that note may expire
    /// (`letibot_sessionlog::warning::FLEETING`): the queue is on the screen for as long as it
    /// holds anything, so the sentence going does not take the fact with it.
    pub(crate) fn queue_standings(&self) -> Standings {
        use letibot_sessionlog::event::MergeState as S;
        let mut s = Standings::default();
        for e in &self.merge {
            // A `match` rather than a `Debug` format, so a state added to the closed set fails
            // to compile here rather than arriving in a bucket nobody chose — the rule
            // `merge_state_word` states one function down.
            match e.state {
                S::Waiting => s.review += 1,
                S::Taken => s.merging += 1,
                S::Failed | S::Conflict | S::Stale | S::Vetoed => s.parked += 1,
                S::Landed => {}
            }
        }
        s
    }

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
                    // **The SHORT id, because the line under the row is where the reason goes.**
                    // The full id of a `task_start` entry is forty-odd columns of
                    // `s-…-sub-…`, and rano draws this row as `{id}{review}{evidence}`
                    // truncated at the pane's width — so a full id spent the whole line on
                    // itself and the failure a person needed to read was cut off the end. The
                    // tail is the part that differs (`registry::short_id` is the same spelling
                    // the subagents pane uses), the overlay still prints the id in full, and
                    // Enter looks the entry up by the row's own `merge` entry rather than by
                    // this string.
                    id: letibot_sessionlog::registry::short_id(&e.id),
                    branch: e.branch.clone(),
                    state: merge_state_word(e.state).to_string(),
                    // **The state word is the daemon's**, and the mark is this head's reading of
                    // it — the same split the jobs pane keeps.
                    //
                    // **A veto is NOT the red one.** `Parked` is the loud mark for the three
                    // states a machine parks an entry in (failed, conflict, stale), and the
                    // operator's requirement for the person's own rejection is that it does not
                    // draw the same way: *"a vetoed entry must not draw as red"*. rano's four
                    // marks are the whole vocabulary this head has (the pane is rano's, pinned by
                    // tag) and none of them means *a person decided*, so a vetoed row takes
                    // `Waiting` — the one mark that is neither a failure nor a success nor work
                    // in progress — and the word beside it (`· vetoed ·`) plus the evidence are
                    // what say which of the two it is.
                    mark: match e.state {
                        S::Waiting => MergeMark::Waiting,
                        S::Taken => MergeMark::Taken,
                        S::Landed => MergeMark::Landed,
                        S::Vetoed => MergeMark::Waiting,
                        S::Failed | S::Conflict | S::Stale => MergeMark::Parked,
                    },
                    age: dur_human(self.now_ms.saturating_sub(e.created_ms)),
                    // **The reason, and the review.** *No verdict yet* and *nobody has asked*
                    // are different facts — see [`App::review_of`].
                    review: match self.review_of(&e.id) {
                        None => ReviewState::NotAsked,
                        // **`asked and has not answered` is the wrong sentence for an attempt
                        // that has already died.** The operator's report is that the four
                        // entries were invisible as failures; the half of that which is the
                        // head's is here — a review with no verdict and a failure on it said
                        // *the reviewer has been asked and has not answered*, which reads as
                        // *still working*. `no verdict` is the true word, and the entry's own
                        // evidence (drawn after it on the same row) carries the reason.
                        Some(r) => match (&r.decision, r.failure.is_empty()) {
                            (None, false) => ReviewState::Decided("no verdict".into()),
                            (None, true) => ReviewState::Asked,
                            (Some(d), _) => ReviewState::Decided(d.clone()),
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

/// **The queue's three standings** — the counts the bottom edge draws, and nothing else.
///
/// Its own type rather than a tuple or a pre-rendered string for the reason the edge needs
/// both halves of it: the words go on the line and the **register** is read from the counts
/// (see [`Standings::role`]), so a caller cannot be handed the text without the count that
/// decides how loud it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Standings {
    /// `waiting`: in the queue, not taken — the gatekeeper's review is what it waits on.
    pub(crate) review: usize,
    /// `taken`: the queue is merging it now.
    pub(crate) merging: usize,
    /// `failed` / `conflict` / `stale` / `vetoed`: the queue has stopped, and a person moves it.
    pub(crate) parked: usize,
}

impl Standings {
    /// **The line, or `None` when nothing stands.**
    ///
    /// Nothing stands for an empty queue — and for a queue whose entries have all landed, which
    /// is the same fact about what is left to do. The edge then says nothing at all rather than
    /// `0 in review · 0 being merged · 0 parked`: a row of zeroes is a row of attention paid for
    /// ever for a fact nobody has, which is the mistake `BoxBottom`'s own docstring names. The
    /// disclosure is `/queue`, which says *none* in words and says what would land there.
    ///
    /// **All three counts, zeroes included, whenever anything stands** — §13.2b's rule read the
    /// way it is meant: the three are one field, and a count that is missing when it is zero is
    /// a count a reader has to remember the shape of the line to interpret. The words are fixed
    /// and only the digits move, so the line is the same width for a given queue and does not
    /// reflow when something else on the screen changes.
    pub(crate) fn words(&self) -> Option<String> {
        let standing = self.review + self.merging + self.parked;
        (standing > 0).then(|| {
            format!(
                "{} in review · {} being merged · {} parked",
                self.review, self.merging, self.parked
            )
        })
    }

    /// **The register the words are drawn in.** In flight is `Pending`; a parked entry is the
    /// one count only a person can move, so the whole line takes the `⚠`'s own `Attention` —
    /// bold yellow against plain, which is the pair the palette already separates by weight
    /// for exactly this distinction. The words do not change with it, so neither does the
    /// width.
    pub(crate) fn role(&self) -> Role {
        if self.parked > 0 {
            Role::Attention
        } else {
            Role::Pending
        }
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
        // **A person's decision, and not a machine's** — the operator's own verb. It is a state
        // of its own rather than a `failed` with a sentence on it because the pane draws the two
        // differently: this one is not red.
        S::Vetoed => "vetoed",
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
