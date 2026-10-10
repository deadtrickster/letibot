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
//! # What is not here
//!
//! **One view, not two.** The operator's other half of the complaint — *"nor there are separate
//! views for review and merge queues"* — is not built: this pane draws the review half and the
//! merge half of every entry in one list, which is where both halves were already drawn from.
//! The merge half being legible at all is this change; two views of one queue is a pane of its
//! own, and is not pretended at here.

use crate::app::*;
use crate::ui::render::{dur_human, row_strings, trim_to, wrap};
use letibot_sessionlog::event::{MergeEntry, MergeGateOutcome, MergeGateStep};
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
    /// **And then the entry's merge half**, spliced under it ([`with_gate_rows`]): the gate's
    /// steps in the order `main` declared them, the red step's own output, and where a landed
    /// branch ended up. rano draws the review half; this is the half that is a machine.
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
        // **The merge half, spliced in under each entry.** rano draws the review half; the
        // gate's steps belong between an entry's own two rows and the next entry's stop.
        let PaneLines { lines, stop_rows } = with_gate_rows(content, &self.merge, w);
        // **The record, rebuilt rather than patched** — the arrows and a click read the rows the
        // last draw wrote, so the stop rows move with the rows that were inserted.
        self.queue_stop_rows = stop_rows;
        row_strings(&lines, self.cfg.palette())
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

/// **The pane's rows with each entry's gate steps spliced in under it.**
///
/// rano draws an entry as two rows — the stop the cursor rests on, and the detail under it — and
/// the merge half belongs between one entry's detail and the next entry's stop. The stop rows are
/// REBUILT rather than patched: they are the record the arrows and a click read, and they are
/// indices into the rows that were drawn, so inserting rows without moving the record would leave
/// the cursor on a row belonging to somebody else's entry — the defect the record exists to
/// prevent (`rano::agent::pane`'s own header, and the operator's *"mouse doesnt click"*).
fn with_gate_rows(content: PaneLines, entries: &[MergeEntry], w: usize) -> PaneLines {
    let mut out = PaneLines::new();
    let mut k = 0usize;
    for (i, line) in content.lines.into_iter().enumerate() {
        if content.stop_rows.get(k) == Some(&i) {
            out.push_stop(line);
        } else {
            out.push(line);
        }
        // **The row after an entry's stop is the entry's own detail** (rano's shape: two rows an
        // entry), so the gate goes after that row and before the next entry's stop.
        if i > 0 && content.stop_rows.get(k) == Some(&(i - 1)) {
            if let Some(e) = entries.get(k) {
                for l in gate_lines(e, w, GateRows::Pane) {
                    out.push(l);
                }
            }
            k += 1;
        }
    }
    out
}

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
