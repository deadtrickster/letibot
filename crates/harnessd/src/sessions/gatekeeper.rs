//! The review queue's own SESSION - one, hardcoded, started by the daemon, whose todo board is
//! the queue. The daemon is the administration: it fills the queue, keeps the worktrees, runs the
//! gates and lands. The teacher is this session, and each review is a `task` child of it.
//!
//! The rows are a VIEW: the fourth half (`TodoBy::Queue`) is replaced from the store on every pass.
//! The STATES stay in the store and this module writes none of them - what takes a row off the board
//! is the child's own `review_verdict`, a person's `/queue approve|veto`, or a verdict another host
//! wrote into the store.
//!
//! It is a ROOT session with no `parent_session_id`, opened through the ordinary door, so it runs on
//! the daemon's model or whatever it was last switched to. [`PROMPT`] is appended to its system
//! prompt at open, and frozen with the prefix like every other prompt.
//!
//! The one honest gap: `cli.rs`'s `is_conversation` is what `--list-sessions` and `--continue`
//! both read, so `--continue` can land the operator here. Splitting the two questions is the
//! operator's call.

use std::path::PathBuf;

use letibot_tokencore::store::{
    ReviewKind, ReviewRow, ReviewState, Store, TodoBy, TodoItem, TodoNeed, TodoStatus,
};
use letibot_tools::builtins::task::Placement;

/// The review-queue session id of the daemon serving `workspace` - `s-gatekeeper-<key>`. It is
/// derived rather than minted because four things are decided by it: "started once" (a restart
/// resumes this id), attaching by name, a board that survives a restart, and the session a review
/// row names. The spelling lives one crate down so a HEAD can name it too.
///
/// [`Registry::resumable`]: letibot_sessionlog::registry::Registry::resumable
pub fn session(workspace: &std::path::Path) -> String {
    letibot_sessionlog::gatekeeper_session(workspace)
}

/// The review queue's id as a fixture spells it - `s-gatekeeper-0123456789ab`, the shape a
/// daemon's own id has, with a key no workspace gives. A fixture that hashed a temp path would pin a
/// number that moves with the box; the card parses only the SHAPE.
#[cfg(test)]
pub(crate) fn session_for_fixture() -> String {
    "s-gatekeeper-0123456789ab".to_string()
}

/// What the session is called in a picker, and on its own store row. A title is set once and only
/// changed on purpose (`SessionBrief::title`), so this is what a person reads in `--sessions`.
pub const TITLE: &str = "the review queue";

/// The session's SEAT, hardcoded, and it is the default one on purpose. It spawns the review
/// children and opens with nobody attached, and `orchestrator` is the only seat carrying the whole
/// task surface and no gated tool - so `Harness::open` has no adjudicator to reach for. It is not
/// `self.base.seat`: `--role coder` seats the operator's conversation, not the queue.
pub const SEAT: crate::config::Seat = crate::config::Seat::Orchestrator;

/// Where a review-queue session's reviewers run - the decision as a STATE, rather than as a
/// placement that happens to come out `host` when something was wrong. The third arm is the point
/// *this box cannot boot a VM* is a fact with the box's own reason attached, so a reader can tell
/// *the session vetoed the VM* from *the box could not give one*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewerPlacement {
    /// A firecode VM. The VM is the boundary, so a reviewer's `bash` asks nobody. The tree it is
    /// booted on is the entry's own REPOSITORY ([`Harness::serve_reviews`] sets `TaskSpec::vm_source`
    /// from `mergequeue::review_copy_source`), not this session's workspace - copying the level above
    /// the repositories is 72.8 GiB of siblings that the ceiling refuses.
    ///
    /// [`Harness::serve_reviews`]: crate::harness::Harness::serve_reviews
    Vm,
    /// This machine, because the project says so - `[queue] reviewers = "host"`, the DEFAULT
    /// and the arm a caller that must not boot one takes. The reviewer works in a checkout of the
    /// head it judges, so it reads the branch and not the operator's checkout.
    Host,
    /// This machine, because the BOX cannot boot one. The session asked for a VM and this is
    /// the box's answer, carrying the reason it gave: declared, so nothing here reads as a choice
    /// nobody made.
    NoVm { why: String },
}

impl ReviewerPlacement {
    /// The `task` placement this state means - what [`Harness::serve_reviews`] puts on the
    /// review child's [`letibot_tools::builtins::task::TaskSpec`].
    ///
    /// [`Harness::serve_reviews`]: crate::harness::Harness::serve_reviews
    pub fn placement(&self) -> Placement {
        match self {
            ReviewerPlacement::Vm => Placement::Firecode,
            ReviewerPlacement::Host | ReviewerPlacement::NoVm { .. } => Placement::Host,
        }
    }

    /// Where the reviewers run, in the session's own words - the paragraph [`prompt`] appends
    /// to [`PROMPT`]. Each arm says where the children run, why that is the answer, and what it costs
    /// the cards that come to this session's head.
    pub fn said(&self) -> String {
        match self {
            ReviewerPlacement::Vm => "\n\
Where your reviewers run: **in a firecode VM**. Each review child is booted in one, on a COPY of \
the REPOSITORY the entry's branch is in — not of this session's workspace, which on a session \
seated above its repositories is a folder holding that repository and its siblings — and the VM \
is the boundary, so a reviewer's `bash` asks nobody, and no permission card reaches your head or \
the operator's. Its commands run in that copy, and `.git` travels with it (only `.git/worktrees` \
is excluded), so `git diff <base>...<branch>` reads the branch the row names. Nothing of the \
reviewer's tree comes back as a branch: what comes back is a VERDICT, and that is what you write.\n"
                .to_string(),
            ReviewerPlacement::Host => "\n\
Where your reviewers run: **on this machine**, in a CHECKOUT OF THE HEAD THEY JUDGE — not in this \
project's working tree, and not in a firecode VM. `[queue] reviewers` says `host` (the default: a \
reviewer is a reader, and the subagents that AUTHOR these branches already run here). Each review \
child is given its own clone of the entry's repository, checked out detached at the sha the row \
names, and that clone is removed when the review ends — so `git diff <base>...<sha>`, `read`, \
`grep` and `glob` all see the branch and never the tree the operator has open.\n\
\n\
**What that gives up, said plainly.** A reviewer here is a `gatekeeper`-seated child with `bash`, \
and nothing but the seat and the permission cards stands between it and this machine: it can run \
anything the operator can, reach the operator's credentials and the whole filesystem, and a branch \
under review may carry a test that does something destructive. The seat is read-only and its \
`bash` calls are cards that come to YOUR head — a card nobody answers times out and the review \
fails — but that is a boundary on ATTENTION, not a sandbox. `[queue] reviewers = \"vm\"` is the \
answer for a project that wants the boundary instead.\n"
                .to_string(),
            ReviewerPlacement::NoVm { why } => format!(
                "\n\
Where your reviewers run: **on this machine, NOT in a VM** — this project asks for one \
(`[queue] reviewers = \"vm\"`, or `--reviewers vm`) and this box cannot boot one: {why}. That is \
the box's answer and not a choice this session can override, because a `firecode` placement this \
box cannot honour would fail every review in the queue at once: a reviewer that cannot start is \
written as a FAILED ATTEMPT on the row. So the reviewer runs here, in a checkout of the head it \
judges, seats `gatekeeper` with `bash`, and its calls are permission cards that come to YOUR head. \
The operator's remedy is a `firecode` on the daemon's PATH (or `$FIRECODE_BIN`) and a restart.\n"
            ),
        }
    }
}

/// Where the reviewers of a review-queue session run: this machine - the DEFAULT, and what a
/// project that says `[queue] reviewers = "host"` or nothing gets - or a firecode VM, which a
/// project asks for by name and this box can only give where it can boot one.
///
/// The default is the host because a VM-placed reviewer paid a guest exec per file read: one
/// repository-scale `grep` was hundreds of round trips and 30–60 minutes of a held queue slot. And
/// the symmetry that settled it: the subagents that AUTHOR these branches already run here, so a VM
/// around only the reader buys isolation the author never had.
///
/// A reviewer has no caller - the queue's own clock starts it - so this is the one child whose
/// placement is decided for it, from a project's [`crate::config::Reviewers`] with the daemon's
/// `--reviewers` as override. `may_use_vm` can only ever REMOVE the VM, never force one
/// [`Harness::serve_reviews`] writes a reviewer that could not start as a failed attempt on the
/// row, so a `Firecode` this box cannot honour would park the whole queue at once. `firecode_available`
/// answers *the CLI is there and runnable*, not *a VM can boot* - nothing cheaper answers the
/// stronger question without booting one.
///
/// Both arms hand the reviewer a tree of its own at the head the row names, never this session's
/// workspace (a folder of repositories on the operator's box: 72.8 GiB against a VM's 8 GiB ceiling).
/// `mergequeue::review::start_reviewer` makes ONE clone per ATTEMPT; a VM is booted on it
/// (`TaskSpec::vm_source`), a host reviewer works in it (`TaskSpec::worktree`).
///
/// [`Harness::serve_reviews`]: crate::harness::Harness::serve_reviews
pub fn reviewer_placement(
    may_use_vm: bool,
    available: Result<PathBuf, String>,
) -> ReviewerPlacement {
    match (may_use_vm, available) {
        // The session's veto first, so a box that COULD boot one is not consulted about a
        // reviewer this session has said must not be placed in one.
        (false, _) => ReviewerPlacement::Host,
        (true, Ok(_)) => ReviewerPlacement::Vm,
        // And the box's inability is a state, not a fallback. Every arm is named; there is no
        // catch-all here, because a catch-all is exactly how *the box cannot boot one* becomes
        // indistinguishable from *the session said no*.
        (true, Err(why)) => ReviewerPlacement::NoVm { why },
    }
}

/// What the review queue's own session is TOLD, in its own system prompt - the operator's *"it
/// managed via its prompt and so on"*, read as an instruction. Appended to the composed prompt at
/// open and frozen with the prefix; `ReseatSession` is the remedy for a session that opened before
/// the paragraph changed.
///
/// It names no verb that has no door (nothing here marks a queue row done or starts a review - the
/// queue's clock does that), and it restates none of the store's vocabulary. What it states plainly
/// is the seam: the reviewer hands its verdict in ITSELF through `review_verdict`, and a reply in
/// prose has judged nothing.
pub const PROMPT: &str = "\
\n\
You are the review queue. This session is the merge queue's own: the rows on your board marked \
`— the merge queue's` are branches waiting for a verdict, one row per entry, and each row carries \
the branch, the entry id and the first line of the ask the work was for. That board is a VIEW — \
it is written from the store on the daemon's own clock and replaced whole — so nothing you write \
moves a row: no field of `todo_write` reaches it, and marking one done would close a review \
nobody made.\n\
\n\
A review is a `task` child of YOURS. It is seated `gatekeeper` — read-only, with `bash` for \
`git diff` — and it judges the branch against the ask. The queue's own clock starts one for each \
row you hold, and the row says which child is on it: `in progress`, waiting on that child's id, \
which is the handle `task_result` collects by. So do not start one yourself — a second reviewer \
on the same entry is two children reading one branch, and the row already names the one that is \
there.\n\
\n\
What you do with a row:\n\
\n\
* `task_result` the handle on it, and read what the reviewer found. **The verdict is what takes \
the row off your board** — and the reviewer hands it in ITSELF: `accept`, `reject` or \
`needs_human`, with its reasons, through the tool it holds (`review_verdict`). The queue's own \
clock writes that onto the entry's review row, and the next pass draws the board without it. \
Nothing you write does it, and **a reviewer that answers in prose without handing a verdict in \
has judged nothing**: its attempt is counted, the row stays, and the queue asks again. So read \
its reply for what it found — and do not treat a sentence in it as a verdict, because the queue \
does not.\n\
* `task_message` that handle when it is stuck, or when it has missed what the ask was. A message \
starts a turn in a child that is between turns, which is how a review is corrected rather than \
abandoned.\n\
* Nothing else. A refusal goes back to the child that did the work — that is the queue's own act \
and not yours.\n\
\n\
If a row's child is gone and no verdict came of it, the row STAYS: the entry is still waiting for \
a verdict, and the queue's own clock starts a new reviewer for it — so do not start one yourself. \
Say on your own log what you found and leave the row where it is. `/queue approve`, `/queue veto` \
and `/queue restart` are the operator's verbs, not yours.\n\
\n\
You may hold a plan of your own on this board like any other session — `todo_write` writes your \
rows and nobody else's. The queue's rows are not yours to write, and they are never buried: they \
come first on the board, and the idle check asks about them before it asks about your plan.\n\
";

/// The paragraph the review queue's session opens with, for the placement its reviewers have.
/// [`PROMPT`] is the constant half and the placement is this box's answer, appended last.
///
/// One function, two callers: this one at open, and [`Harness::serve_reviews`] at every spawn -
/// so the paragraph a session was given and the placement its children get cannot drift apart.
///
/// [`Harness::serve_reviews`]: crate::harness::Harness::serve_reviews
pub fn prompt(placement: &ReviewerPlacement) -> String {
    format!("{PROMPT}{}", placement.said())
}

/// The rows on the review queue's board - one per review this daemon's queues serve that is
/// waiting for a reviewer or under one, in the order the queue claims them, with no child on any
/// of them. The harness marks the ones a review child is working ([`row`]).
pub fn queue_rows(store: &Store, me: &str) -> Vec<TodoItem> {
    waiting(store, me).iter().map(|r| row(r, None)).collect()
}

/// The reviews this daemon's queues serve that are waiting for a verdict - rows `waiting` or
/// `reviewing` in a repository whose LEASE this daemon holds. Another daemon's rows
/// would nag this session about work it must not touch.
///
/// The order is the queue's own claim order - the red target first, then priority, then age - so the
/// board is stable across passes (`set_queue` compares the rows).
pub fn waiting(store: &Store, me: &str) -> Vec<ReviewRow> {
    let held: Vec<String> = store
        .queue_leases()
        .unwrap_or_default()
        .into_iter()
        .filter(|l| l.owner == me)
        .map(|l| l.repository)
        .collect();
    let mut rows: Vec<ReviewRow> = store
        .review_rows(None)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| {
            held.contains(&r.repository)
                && matches!(r.state, ReviewState::Waiting | ReviewState::Reviewing)
        })
        .collect();
    rows.sort_by_key(|r| {
        (
            r.kind != ReviewKind::Target,
            r.priority.rank(),
            r.created_ms,
            r.id.clone(),
        )
    });
    rows
}

/// One review, as a row on the session's board - and, when a review child is on it, the row
/// that says so. The words are stable across passes (the half is compared): the daemon's name for
/// the run, the row id `/queue` takes, and the first line of the ask.
pub fn row(r: &ReviewRow, child: Option<&str>) -> TodoItem {
    TodoItem {
        content: format!(
            "{} (entry {}): {}",
            crate::mergequeue::review_job_name(r),
            r.id,
            first_line(&r.brief),
        ),
        status: match child {
            Some(_) => TodoStatus::InProgress,
            None => TodoStatus::Pending,
        },
        by: TodoBy::Queue,
        when: None,
        needs: child
            .map(|handle| {
                vec![TodoNeed::Child {
                    id: handle.to_string(),
                }]
            })
            .unwrap_or_default(),
    }
}

/// How much of the brief's first line a row carries. Long enough for a sentence that says what the
/// branch was asked to do, short enough that the row is still a name - see [`row`].
const BRIEF_CHARS: usize = 120;

/// The brief's first line, trimmed, capped with an ellipsis when it was longer.
fn first_line(brief: &str) -> String {
    let line = brief.lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(BRIEF_CHARS).collect();
    if line.chars().count() > BRIEF_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests;
