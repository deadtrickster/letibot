//! **The merge queue**: what is waiting to land, and a review's output — drawn by
//! `rano::agent::queue` from the queue and the reviews this head holds.
//!
//! # The merge half is this file's
//!
//! rano draws the review half of an entry — what the reviewer said, and the queue's own words
//! for why it is where it is. The merge half is here: the gate's steps, in the order `main`
//! declared them, spliced under the entry they belong to ([`with_gate_rows`]) and drawn whole in
//! the overlay ([`gate_section`]). The design note's argument for why this half can be drawn
//! well at all is that *"the merge queue is a machine. Rebase, then the gate's steps, then land.
//! Nothing is being decided; something is being run. And a thing that is being run can be drawn
//! well."* A head handed only `failed` can only ever draw `failed`, which is why the landing
//! records one row per step rather than a verdict.
//!
//! # Two views of one list
//!
//! The operator asked for the same queue twice, in their own words: *"it doesnt show me review
//! queue state transitions"* and *"nor there are separate views for review and merge queues"*.
//! The design note's argument for why the two are two things rather than one list with two kinds
//! of row: *"The review queue is a judgement surface. The tutor, its refusals, the conversations
//! with the kids, the question it raises to the father, the human's override. It is irreducibly
//! prose. The merge queue is a machine. Rebase, then the gate's steps, then land. Nothing is
//! being decided; something is being run."*
//!
//! So the LIST is one — rano's rows, the daemon's order, one cursor — and the row under each
//! entry is the view's ([`QueueView`], [`splice`]): the reviewer's verdict, the ask it was asked
//! against and the verbs a person has in [`QueueView::Review`], the gate's steps in
//! [`QueueView::Merge`]. The strip the pane draws under its title is the switch, and `tab` moves
//! between them.
//!
//! # What each view may promise
//!
//! A view draws what the wire carries and no more. The review view's verdict is rano's row; the
//! ask is `MergeEntry::brief`; the verbs are the head's own, and the states each is refused in
//! are the states the daemon refuses them in ([`verb_moves`]). **The reviewer's QUESTION is not
//! on the wire** — `decision` is one of three words, `reasons`/`files`/`commands` are the record,
//! and the argument itself is the reviewer's session, which the overlay names so a person can
//! attach to it. So the review view draws no question it cannot get; it draws the verdict, the
//! ask, and what a person can do about them.

use crate::app::*;
use crate::ui::render::{dur_human, row_strings, trim_to, wrap};
use letibot_sessionlog::event::{MergeEntry, MergeGateOutcome, MergeGateStep, MergeState};
use rano::agent::pane::PaneLines;
use rano::agent::queue::{
    MergeMark, OpenedEntry, QueueEntry, QueueEntryView, QueuePane, ReviewRecord, ReviewState,
};
use rano::agent::text::{clean_line, one};
use rano::render::{Line, Span};
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
    /// **And the row under it is the VIEW's** ([`QueueView`]): rano's own detail — the
    /// reviewer's verdict, and the queue's words for why the entry is where it is — in the
    /// review view, and the entry's merge half in the merge view ([`with_gate_rows`]): the gate's
    /// steps in the order `main` declared them, the red step's own output, and where a landed
    /// branch ended up. **The list, its order and its stop rows are the same in both**, which is
    /// what keeps the cursor on the same entry when the view changes.
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
        // **The body under each entry is the view's.** rano draws the list in both views, and the
        // review half with it; the merge view is the same list with that row replaced by the
        // gate's steps ([`splice`]).
        let (rows, tabs) = match self.queue_view {
            QueueView::Review => review_rows(content, &self.merge, self.queue_sel, w),
            QueueView::Merge => with_gate_rows(content, &self.merge, w),
        };
        // **The records, rebuilt rather than patched** — the arrows, a click and the strip read
        // the rows the last draw wrote, so they move with the rows that were inserted.
        self.queue_stop_rows = rows.stop_rows;
        self.queue_tabs = tabs;
        row_strings(&rows.lines, self.cfg.palette())
    }

    /// **One entry, whole** — the overlay the queue pane's Enter opens.
    ///
    /// Everything here is a row the head already holds: the entry's own fields, the evidence
    /// (which is the gate's own captured words when the gate failed, and the reviewer's
    /// rendered verdict when the review refused it), and the review record. Nothing is read
    /// again on the keypress, which is why a `MergeEntryMoved` arriving while the overlay is
    /// open shows the NEW state rather than the state at the moment Enter was pressed.
    ///
    /// **The overlay is NOT split by the view** ([`QueueView`]): the two views are two ways of
    /// scanning the LIST, and an entry that has been opened is one entry — the ask, the reviewer's
    /// verdict and the gate's steps are all of it, and a reader who pressed Enter asked for all of
    /// it. Which view the list happened to be on is not a fact about the entry.
    ///
    /// The entry is looked up by id every draw, and an id the queue no longer holds — a
    /// `recover` moved it, or a head switched and took a fresh snapshot — says so rather than
    /// drawing an empty overlay.
    pub(crate) fn queue_out_lines(&self, w: usize) -> Vec<String> {
        let Some(id) = self.queue_open.as_deref() else {
            return Vec::new();
        };
        let held = self.merge.iter().find(|e| e.id == id);
        let entry = held.map(|e| OpenedEntry {
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
        let mut lines = view.lines(w);
        // **The merge half goes under rano's overlay** — the entry's fields, the ask and the
        // reviewer's verdict are rano's, and the gate's steps are appended where a reader who
        // opened the entry scrolls to. Nothing is read again here, so a `MergeEntryMoved`
        // arriving under an open overlay shows the new state (see this method's docstring).
        if let Some(e) = held {
            lines.extend(gate_section(e, w));
        }
        row_strings(&lines, self.cfg.palette())
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

// ===== Two views of one list: the review queue and the merge queue =====

/// **Which half of the one queue the pane is showing** — the operator's second ask, verbatim:
/// *"nor there are separate views for review and merge queues"*.
///
/// **The review view is where the pane opens**, because it is where the pane already opened: the
/// list of entries and the reviewer's row beside each is rano's, and the merge half is one `tab`
/// away with the strip that says so drawn on the pane. Both views draw the same entries, in the
/// daemon's order, off one cursor — so moving between them cannot move the reader to another
/// entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum QueueView {
    /// The judgement surface: rano's verdict row, the ask the verdict was asked against, and
    /// the four verbs a person has on the entry under the cursor.
    #[default]
    Review,
    /// The machine: the gate's steps in order, the red step's own output, and where the branch
    /// landed.
    Merge,
}

impl QueueView {
    /// The view's own word, as the strip spells it.
    pub(crate) fn word(self) -> &'static str {
        match self {
            QueueView::Review => "review",
            QueueView::Merge => "merge",
        }
    }

    /// **The other one** — what `tab` moves to.
    pub(crate) fn other(self) -> QueueView {
        match self {
            QueueView::Review => QueueView::Merge,
            QueueView::Merge => QueueView::Review,
        }
    }
}

/// **Where the two tabs were drawn** — the pane CONTENT row the strip landed on, and each label's
/// `(first column, one past the last)` in the row's own columns.
///
/// A record rather than arithmetic at the click, for the reason `PaneLines::stop_rows` is: only
/// the frame knows where a label landed, and a click handler that recomputed it would be a second
/// opinion about the row it drew (`App::box_top_hits`'s rule). The gutter comes off at the click,
/// because a click's `x` is a terminal cell and the record is in the pane's own columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueTabs {
    pub(crate) row: usize,
    pub(crate) review: (usize, usize),
    pub(crate) merge: (usize, usize),
}

/// The column the strip starts at, so the tabs sit under the pane's title with the entries.
const TAB_AT: &str = "  ";

/// **The sentence beside the tabs that names the key** — on the pane, because the switch is the
/// screen's: a reader who has never opened `/help` has to be able to see that there is a second
/// view and what moves to it.
const TAB_KEY: &str = " · tab switches";

/// **The strip: the two views, the active one bracketed, and the key that moves between them.**
///
/// **A bracket and not a highlight.** The palette is not always there (`Palette::None` draws no
/// attributes at all) and *which view am I looking at* is not a fact the pane may draw only on a
/// terminal with colour. Both labels are padded to the ACTIVE spelling's width, so the strip is
/// the same width whichever view is on and the row does not reflow when `tab` is pressed.
///
/// The `row` in the answer is the caller's to fill in: only it knows where the strip landed.
fn tab_row(view: QueueView) -> (Line, QueueTabs) {
    let label = |v: QueueView| {
        if v == view {
            (format!("[{}]", v.word()), Role::Strong)
        } else {
            (format!(" {} ", v.word()), Role::Faint)
        }
    };
    let (review, review_role) = label(QueueView::Review);
    let (merge, merge_role) = label(QueueView::Merge);
    let review_from = TAB_AT.len();
    let merge_from = review_from + review.len();
    let merge_to = merge_from + merge.len();
    let line = Line::new(vec![
        Span::raw(TAB_AT),
        Span::role(review, review_role),
        Span::role(merge, merge_role),
        Span::role(TAB_KEY, Role::Faint),
    ]);
    (
        line,
        QueueTabs {
            row: 0,
            review: (review_from, merge_from),
            merge: (merge_from, merge_to),
        },
    )
}

/// **The list both views share**: rano's rows, the strip under the title, and the stop rows
/// REBUILT.
///
/// The stop rows are the record the arrows and a click read, and they are indices into the rows
/// that were drawn — so inserting rows without moving the record would leave the cursor on a row
/// belonging to somebody else's entry, the defect the record exists to prevent
/// (`rano::agent::pane`'s own header, and the operator's *"mouse doesnt click"*).
///
/// `body` is handed each entry's own row — rano draws two rows an entry, and the second is the
/// entry's — and returns the rows that stand in its place: rano's own row in the review view, the
/// gate's rows in the merge view. The list itself, its order and its stop rows are the same in
/// both, which is what keeps the cursor on the same entry across a switch.
fn splice(
    content: PaneLines,
    view: QueueView,
    body: impl Fn(usize, Line) -> Vec<Line>,
) -> (PaneLines, Option<QueueTabs>) {
    let mut out = PaneLines::new();
    let mut tabs = None;
    // Which entry's own row is next: the row after stop `k` is entry `k`'s, so the counter
    // advances with the stop rows rather than with the entries.
    let mut k = 0usize;
    for (i, line) in content.lines.into_iter().enumerate() {
        if i == 0 {
            // **The title, and under it the switch.** rano's own header row is left alone; the
            // strip is the pane's, drawn where a reader looks for it rather than in a help text.
            out.push(line);
            let (strip, at) = tab_row(view);
            tabs = Some(QueueTabs {
                row: out.lines.len(),
                ..at
            });
            out.push(strip);
            continue;
        }
        if content.stop_rows.get(k) == Some(&(i - 1)) {
            for l in body(k, line) {
                out.push(l);
            }
            k += 1;
        } else if content.stop_rows.get(k) == Some(&i) {
            out.push_stop(line);
        } else {
            out.push(line);
        }
    }
    (out, tabs)
}

/// **The merge view's rows**: rano's list with each entry's review row REPLACED by that entry's
/// gate rows.
///
/// It was the splice that put the merge half under a list drawing both halves; the split made it
/// the merge view's body, and the change is that rano's review row goes rather than staying. The
/// gate's own renderer is untouched ([`gate_lines`]): the step, its mark, the machine's clock, the
/// red one's own output tail, and where a landed branch ended up.
///
/// **An entry with no gate rows says so rather than drawing an empty column**, which is
/// [`gate_lines`]'s own first case and not something this function invents.
fn with_gate_rows(
    content: PaneLines,
    entries: &[MergeEntry],
    w: usize,
) -> (PaneLines, Option<QueueTabs>) {
    splice(content, QueueView::Merge, |k, _rano| match entries.get(k) {
        Some(e) => gate_lines(e, w, GateRows::Pane),
        None => Vec::new(),
    })
}

/// **The review view's rows**: rano's own rows, with the ask and the verbs under the entry the
/// cursor is on.
///
/// The verdict is rano's row (`reviewer: accept`, `no verdict`, `asked and not answered`, and the
/// queue's reason beside it); what this adds is the ask that verdict was asked against and the
/// four verbs a person has — see [`review_lines`] for why they are drawn under the cursor's entry
/// and not under every one.
fn review_rows(
    content: PaneLines,
    entries: &[MergeEntry],
    cursor: usize,
    w: usize,
) -> (PaneLines, Option<QueueTabs>) {
    let at = cursor.min(entries.len().saturating_sub(1));
    splice(content, QueueView::Review, |k, rano| {
        let mut out = vec![rano];
        if let Some(e) = entries.get(k) {
            out.extend(review_lines(e, k == at, w));
        }
        out
    })
}

/// The label an entry's ask is drawn under, and the one its verbs are — the same width, so the
/// two rows start together.
const ASK_LABEL: &str = "ask   ";
const VERBS_LABEL: &str = "verbs ";

/// **The judgement surface of one entry: the ask, and the verbs** — drawn under the entry the
/// CURSOR is on, and not under every entry, for two reasons that are one: the ask is prose (rano's
/// overlay draws it whole and wrapped, and this is the glance a list can afford), and the verbs
/// act on the row under the cursor — a verb row under an entry the cursor is not on would be a
/// sentence about a key that moves a different row.
///
/// **The verdict itself is rano's row above this one**, and the reviewer's reasons, files and
/// commands are the overlay's: they are prose too, and an overlay is where prose is read.
fn review_lines(e: &MergeEntry, selected: bool, w: usize) -> Vec<Line> {
    if !selected {
        return Vec::new();
    }
    // **An empty brief is a fact and is said.** The gatekeeper's protocol is brief-first, so an
    // entry nobody recorded an ask for is an entry the reviewer had nothing to review against —
    // rano's own sentence in the overlay, and never a blank row a reader reads as *nothing to
    // say*.
    let ask = if e.brief.trim().is_empty() {
        "(nobody recorded one — the reviewer has nothing to review against)".to_string()
    } else {
        trim_to(
            &clean_line(&e.brief),
            w.saturating_sub(STEP_AT.len() + ASK_LABEL.len()),
        )
    };
    let mut out = vec![Line::new(vec![
        Span::raw(STEP_AT),
        Span::role(ASK_LABEL, Role::Faint),
        Span::raw(ask),
    ])];
    out.extend(verb_rows(e.state, w));
    out
}

/// **What each of the person's four verbs would do to an entry in this state** — one paragraph,
/// wrapped, because the four are one vocabulary and a reader comparing *what may I do* with *what
/// would be refused* should not have to line up two lists.
///
/// The liveness is [`verb_moves`], which is also what the keys read, so the row cannot promise a
/// verb the key would refuse. **The daemon still arbitrates**: a live review attempt and a verdict
/// that already accepts are refusals the head does not know from the state alone, and the row says
/// what the state permits rather than what the next keypress will be answered with.
fn verb_rows(state: MergeState, w: usize) -> Vec<Line> {
    let mut said = String::new();
    for (i, verb) in [Verb::Approve, Verb::Veto, Verb::Restart, Verb::Rm]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            said.push_str(" · ");
        }
        if verb_moves(verb, state) {
            said.push_str(&format!("{} {} ({})", verb.key(), verb.word(), verb.does()));
        } else {
            said.push_str(&format!(
                "{} {} (nothing: {})",
                verb.key(),
                verb.word(),
                verb.why_not(state)
            ));
        }
    }
    let room = w.saturating_sub(STEP_AT.len() + VERBS_LABEL.len());
    wrap(&clean_line(&said), room)
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            // The label on the first row and blanks under it, so the wrapped tail stays in its
            // own column rather than running back under the label.
            let lead = if i == 0 { VERBS_LABEL } else { "      " };
            Line::new(vec![
                Span::raw(STEP_AT),
                Span::role(lead, Role::Faint),
                Span::raw(l),
            ])
        })
        .collect()
}

/// **One of the person's four verbs**, as the keys and the row that describes them spell it.
///
/// One type rather than four strings so the key's letter, the word the typed spelling uses and
/// the effect are one entry — the drift `Show::chord` exists to prevent, one pane over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Approve,
    Veto,
    Restart,
    Rm,
}

impl Verb {
    /// The key that sends it, as `key_queue_pane` spells it.
    pub(crate) fn key(self) -> char {
        match self {
            Verb::Approve => 'a',
            Verb::Veto => 'v',
            Verb::Restart => 'r',
            Verb::Rm => 'd',
        }
    }

    /// The word, as `/queue <word> ID` spells it.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Verb::Approve => "approve",
            Verb::Veto => "veto",
            Verb::Restart => "restart",
            Verb::Rm => "rm",
        }
    }

    /// **What it would do to an entry it may move** — the daemon's own effect, in the head's
    /// words (`harnessd::mergequeue`'s three verb docs are the source).
    fn does(self) -> &'static str {
        match self {
            // **The gate still runs.** An approval replaces the reviewer's JUDGEMENT and never
            // the landing machinery — the one thing the row must not let a reader conclude
            // otherwise (`mergequeue::approve`'s own rule).
            Verb::Approve => {
                "the queue takes it — your verdict replaces the review's, and the gate still runs"
            }
            Verb::Veto => "the child is sent back with the refusal and the entry parks `vetoed`",
            Verb::Restart => "the reviewer is asked again about the branch as it is now",
            Verb::Rm => "the entry leaves the queue — the branch and its worktree stay",
        }
    }

    /// **Why it would move nothing**, in a state it is refused in.
    fn why_not(self, state: MergeState) -> &'static str {
        use MergeState as S;
        match (self, state) {
            (_, S::Taken) => "the queue has claimed it and is merging it right now",
            (_, S::Landed) => "it has landed",
            // A `waiting` entry is what the queue's OWN retry is for; a restart there would be a
            // second ask about a branch nobody has touched yet.
            (Verb::Restart, _) => {
                "only a parked entry (failed, conflict, stale, vetoed) is restarted by hand"
            }
            (_, _) => "nothing moves it",
        }
    }
}

/// **Which verbs move an entry out of a state** — ONE predicate for the keys that send them
/// (`key_queue_pane`) and for the row that says what they would do, so the two cannot drift.
///
/// The sets are the daemon's (`mergequeue::movable` for the three person verbs, `restartable` for
/// the fourth), copied here deliberately: a head that sent a verb it knows the daemon refuses
/// would make the operator wait for a round trip to be told no, which is the rule
/// `key_queue_pane` already states.
pub(crate) fn verb_moves(verb: Verb, state: MergeState) -> bool {
    use MergeState as S;
    match verb {
        Verb::Restart => matches!(state, S::Failed | S::Conflict | S::Stale | S::Vetoed),
        Verb::Approve | Verb::Veto | Verb::Rm => matches!(
            state,
            S::Waiting | S::Failed | S::Conflict | S::Stale | S::Vetoed
        ),
    }
}

// ===== The merge half: the gate's steps, drawn =====

/// **How the gate's rows are drawn in the two places they appear.** A pane row is a glance at a
/// fixed width; an overlay is read and scrolled, so it gets every line the row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateRows {
    /// The pane: one row a step, and a failing step's last [`FAILURE_LINES`] output lines, each
    /// cut to the width.
    Pane,
    /// The overlay: every line of the output, wrapped.
    Whole,
}

/// The column an entry's own detail row is indented to — rano draws two rows an entry, the second
/// under the first — so a step lines up under the entry it belongs to.
const STEP_AT: &str = "         ";

/// A step's output, one level further in than the step.
const OUTPUT_AT: &str = "             ";

/// **How much of a failing step's output the PANE draws.** The row carries up to four kilobytes
/// of it (`mergequeue::EVIDENCE_BYTES`) and a list is a glance, so the pane draws the last few
/// lines — the failure is at the end, which is the end `mergequeue::tail` keeps. The overlay
/// draws all of it.
const FAILURE_LINES: usize = 4;

/// **The merge half of one entry: the gate's steps, in the order `main` declared them.**
///
/// The design note's argument, and the reason `af2144b` records a row per step rather than a
/// verdict: *"the merge queue is a machine. Rebase, then the gate's steps, then land. Nothing is
/// being decided; something is being run. And a thing that is being run can be drawn well."*
///
/// # The two edges the rows were built with, and why neither reads as *all steps passed*
///
/// **An empty list is *the gate has not run on this entry*** — what a daemon from before
/// `gate_steps` sends (`#[serde(default)]`), what an entry still waiting for its reviewer carries,
/// and what a vetoed or conflicted entry carries. It is drawn as that sentence and never as
/// nothing: a blank checklist under an entry reads as *the gate declared no steps*, and a gate
/// that declared no steps reads as green. Neither is what an empty list says.
///
/// **A [`MergeGateOutcome::NoGate`] row is a repository whose `main` declares no gate at all** —
/// one row, an empty command, the queue's own sentence in `output`. It is the honest and
/// actionable case (the queue holds the branch until somebody writes the section), so it is drawn
/// as that sentence with `⚠` beside it, and never as a step with a tick.
///
/// # An entry the queue has taken
///
/// The rows are written by the move that ENDS a run, so a `taken` entry holds none of this run's
/// — and it keeps the previous run's, because a move with nothing to say about the gate leaves
/// them alone (`mergequeue::Daemon::move_to`; the no-gate hold is the reachable case). So a
/// `taken` entry is drawn as *running*, and rows under it are labelled as the last run's, rather
/// than a previous run's outcome being drawn as the state of this one.
fn gate_lines(e: &MergeEntry, w: usize, how: GateRows) -> Vec<Line> {
    use letibot_sessionlog::event::MergeState as S;
    let running = e.state == S::Taken;
    let mut out = Vec::new();
    if e.gate_steps.is_empty() {
        out.push(if running {
            note(
                "…",
                Role::Pending,
                "the gate has not run on this entry yet — the merge is running",
                w,
            )
        } else {
            note("·", Role::Faint, "the gate has not run on this entry", w)
        });
        return out;
    }
    if running {
        out.push(note(
            "…",
            Role::Pending,
            "the merge is running — the rows below are the last run's",
            w,
        ));
    }
    for step in &e.gate_steps {
        out.extend(step_lines(step, w, how));
    }
    // **Where the branch ended up**, on the row the merge half is drawn on. The overlay draws
    // `landed <tip>` among the entry's own fields already, so this is the pane's.
    if how == GateRows::Pane
        && let Some(tip) = &e.landed_sha
    {
        out.push(note("→", Role::Success, &format!("landed {tip}"), w));
    }
    out
}

/// **One step's rows: the command, its mark, and — when it is the red one — what it printed.**
fn step_lines(step: &MergeGateStep, w: usize, how: GateRows) -> Vec<Line> {
    use MergeGateOutcome as O;
    // **A `no_gate` row is not a step.** It has no command because there was none to run, and it
    // is the repository's fact rather than a step's outcome — see [`gate_lines`].
    if step.outcome == O::NoGate {
        return vec![note(
            "⚠",
            Role::Attention,
            &format!("no gate declared — {}", step.output),
            w,
        )];
    }
    let (mark, role) = match step.outcome {
        O::Passed => ("✓", Role::Success),
        O::Failed => ("✗", Role::Failure),
        O::NotRun => ("·", Role::Faint),
        O::NoGate => unreachable!("the no-gate row is drawn above"),
    };
    let mut said = clean_line(&step.command);
    match step.outcome {
        // **The machine's own clock**, which is the row's other half: a step that took a minute
        // and a step that took a millisecond are the same `✓` without it.
        O::Passed | O::Failed => said.push_str(&format!(" · {}", dur_human(step.elapsed_ms))),
        // **Not `passed`.** A step the gate never reached because an earlier one was red is not a
        // step that was green, and drawing the two the same way is how a gate that stopped at the
        // first failure comes to look like a gate that ran everything.
        O::NotRun => said.push_str(" · not run"),
        O::NoGate => {}
    }
    let mut out = vec![Line::new(vec![
        Span::raw(STEP_AT),
        Span::role(mark, role),
        Span::raw(" "),
        Span::raw(trim_to(&said, w.saturating_sub(STEP_AT.len() + 2))),
    ])];
    // **What it printed, and where it stopped** — the step's own words rather than a sentence
    // about the failure, and the END of them, which is where a failure is.
    if step.outcome == O::Failed {
        out.extend(output_lines(&step.output, w, how));
    }
    out
}

/// **A failing step's own output**, drawn under the step that printed it.
///
/// The pane draws the last [`FAILURE_LINES`] of it, one row a line and cut to the width, because
/// a row in a list is a glance and the last lines are the failure. The overlay wraps every line
/// and draws all of them, because that is the only place the run is readable in full — rano's
/// overlay says the same about the entry's evidence, which is why it wraps rather than elides.
fn output_lines(output: &str, w: usize, how: GateRows) -> Vec<Line> {
    let lines: Vec<&str> = output.lines().collect();
    let from = match how {
        GateRows::Pane => lines.len().saturating_sub(FAILURE_LINES),
        GateRows::Whole => 0,
    };
    let room = w.saturating_sub(OUTPUT_AT.len());
    let mut out = Vec::new();
    for l in &lines[from..] {
        let text = clean_line(l);
        match how {
            GateRows::Pane => out.push(Line::new(vec![
                Span::raw(OUTPUT_AT),
                Span::raw(trim_to(&text, room)),
            ])),
            GateRows::Whole => {
                for wrapped in wrap(&text, room) {
                    out.push(Line::new(vec![Span::raw(OUTPUT_AT), Span::raw(wrapped)]));
                }
            }
        }
    }
    out
}

/// **A sentence row under an entry** — the gate's state when there are no steps to draw, and
/// where a landed branch ended up. `mark` is the one-column reading beside it and the role is the
/// mark's; the sentence is cut to the width like the entry's own detail row.
fn note(mark: &str, role: Role, text: &str, w: usize) -> Line {
    Line::new(vec![
        Span::raw(STEP_AT),
        Span::role(mark, role),
        Span::raw(" "),
        Span::raw(trim_to(
            &clean_line(text),
            w.saturating_sub(STEP_AT.len() + 2),
        )),
    ])
}

/// **The gate's steps, as the overlay's last section** — the pane's rows one width up: every line
/// of a failing step's output, wrapped rather than cut, because an overlay is read and scrolled
/// rather than glanced at.
fn gate_section(e: &MergeEntry, w: usize) -> Vec<Line> {
    let mut out = vec![
        Line::default(),
        one("  the gate's steps", Role::Strong),
        Line::default(),
    ];
    out.extend(gate_lines(e, w, GateRows::Whole));
    out
}
