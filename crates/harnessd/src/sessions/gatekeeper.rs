//! **The review queue's own SESSION** — one, hardcoded, started by the daemon, whose todo board is
//! the queue.
//!
//! # The design, in the operator's words
//!
//! *"reviews are subagents managed by the main gatekeeper session which is hardcoded, started
//! either inside default model or configured model. it is a normal auto-driving session that treats
//! review queue as todo list - so nags and all. gatekeepers are subagents. so - i can connect to
//! that session, it managed via its prompt and so on."*
//!
//! And the analogy that draws the boundary, which is the cleanest statement of it there is:
//! *"Parents with children sending their homework to the teachers. School administration doesnt
//! teach or checks homework. it just makes sure pencils are here and rooms are warm."*
//!
//! So: the daemon is the **administration**. It fills the queue, keeps the worktrees and runs the
//! gates and the landing — and it does **not** review. The **teacher is a session**: the one this
//! module starts, whose board carries the queue as work. The parents are the working sessions and
//! the children are their subagents, and the reviews are the teacher's own children — which is the
//! NEXT slice and is deliberately absent here.
//!
//! **The design note this comes out of is `the-review-queue-and-the-merge-queue.md`** (the box-wide
//! notes, beside the operator's other ones), and one of its corrections is load-bearing here:
//! *"well gatekeeper is not a child so we are not doing up"* — the tutor is a session in its own
//! right, a **peer** of the main session, with no tree edge to walk. So this one is a ROOT with no
//! `parent_session_id`, and its rows carry the queue's own authorship and never `Parent <id>` (that
//! string is a claim about a session that wrote a child's board, and the daemon is not that session).
//! The note's own list of what the tutor still owes — the lateral `send`, the review/merge split —
//! is the work after this.
//!
//! # What this slice is, and what the next one still owes
//!
//! **In — the session, and the review it runs.** The session exists (started once, resumable,
//! listed, attachable), its board carries one row per entry waiting for a verdict, and **a review is
//! a `task` child of THIS session**: the reviewer is spawned by this session's own harness through
//! the same runner the `task` tool uses (`Harness::serve_reviews`), so the child's
//! `parent_session_id` is `s-gatekeeper`, its settlement is published on this session's hub, its
//! approvals ask at this session's head, and the row it is working says so on this board ([`row`]).
//! The review row names this session (`mergequeue::GatekeeperDoor`) — *the session that reviewed
//! it*, which is what that column was always documented to mean and could not be while the review
//! ran under the tree the branch came from.
//!
//! **In — the child's own verdict, which is what this slice adds.** The reviewer hands its
//! verdict IN: it holds one tool (`review_verdict`, `crates/tools/src/builtins/verdict.rs`),
//! calls it with `accept`/`reject`/`needs_human` and its reasons, and the desk that call writes
//! to is the daemon's own. The queue's pass (`Harness::serve_reviews`) takes the submission and
//! writes it onto the entry's review row through the same `write_outcome` it always used — so the
//! STATES, their storage and the per-workspace split never moved into the model, and the child
//! never holds a store, an entry or a SHA.
//!
//! **And the parent no longer reads its answer.** The closing block the review prompt used to ask
//! for, and the parser that read it (`letibot_tools::gatekeeper::parse_verdict`), are gone: a
//! reply in prose is not a verdict, and `Harness::review_outcome` counts such an attempt rather
//! than inventing one. A child that dies without submitting writes nothing at all — the row stays
//! and the queue starts a new reviewer.
//!
//! **So the three things that take a row off this board** are: the child's own verdict, a
//! person's `/queue approve|veto`, and a verdict written by any other host straight into the
//! store. All three move the STORE, and the board is a view of it.
//!
//! **And not a worktree of its own, deliberately.** A review is spawned as a plain `task` and not a
//! `task_start`: the reviewer READS a branch (`git diff base...branch` from the repository it is
//! already in — `gatekeeper::review_prompt` is the whole of its framing), and a `task_start` tree
//! would be a branch of its own that `HarnessTaskRunner::enqueue_entry` then mints a queue entry
//! for — a reviewer's empty tree offered to the merge queue as work.
//!
//! # Where the rows live, and where the STATES live — the boundary this must not cross
//!
//! **The rows are on the session's board**, on the fourth half (`TodoBy::Queue`, written by
//! `TodoBoard::set_queue`), and they are a **view**: the half is REPLACED from the store on every
//! pass, so a row is on the board exactly while its entry is waiting for a verdict and is gone the
//! moment its verdict arrives — wherever that verdict was written.
//!
//! **The states are in the store and nowhere else.** `MergeEntry::state` is the queue's own word for
//! where an entry is, `ReviewRecord::decision` is the reviewer's, and this module writes neither.
//! The board is what a SESSION can be nagged about; it is not a second place a state can be read
//! from, and nothing that COUNTS the queue — the status line, the merge-queue pane, a head attached
//! to another daemon — counts it. That is the operator's boundary, in their words: *"there is a
//! mechanical review queue with well defined states. it is just not me moving between them but
//! gatekeeper subagent"* — the STATES do not move, only who moves an entry between them.
//!
//! # Why the row is on the board at all, rather than a pane
//!
//! Because the operator asked for a session *"that treats review queue as todo list - so nags and
//! all"*. Everything a session already has then applies to the queue with no new mechanism: the
//! idle plan-check (`Sessions::rearm_todo_nag` → `Harness::nag_notice`) names the next row, the pane
//! draws it, the store keeps it, and a head that attaches sees it. Nothing here arms anything: the
//! arming is the ordinary one, taken by `Sessions::fill_gatekeeper_board` after the board moves.
//!
//! # What the model is told about the rows
//!
//! Four places, and the first is the one this slice adds — the operator's *"it managed via its
//! prompt and so on"* read as an instruction to WRITE the paragraph, because a session whose board
//! is a queue nobody explained is a session that has to guess what the rows are for:
//!
//! * **[`PROMPT`], appended to this session's system prompt at open** — what the rows are, that a
//!   review is a `task` child of this session and that the queue's own clock starts it, that the
//!   row names the child's handle, and that the verdict (not `todo_write`) is what takes a row off
//!   — and that the verdict is the CHILD's own call, so what this session does with the child is
//!   `task_result` and `task_message` and nothing else.
//!   It is frozen with the rest of the prefix, like every other prompt (`ReseatSession` is the
//!   tree's remedy for a session that opened before a prompt changed).
//! * **The nag** (`[todo check]`, `unfinished_plan_for`): the row's words, then `— the merge queue's`,
//!   then the advice, which says where the row comes off and that no field of `todo_write` moves it.
//!   A row with a review child on it is BLOCKED on that child (`TodoNeed::Child`), so the check
//!   says which child and how long it has been running instead of asking for the review twice —
//!   and it is silent about it for the child's first half hour (`CHILD_ROOM_TO_BREATHE`).
//! * **`todo_write`'s reply** (`render`): the whole board, each of these rows marked `— the merge
//!   queue's`, so the model can tell what it wrote from what it was given.
//! * **The pane**, which draws these rows like any other: the one author it marks specially is the
//!   operator's (`crates/tui/src/ui/panes/todos.rs`), so a queue row draws as ordinary open work.
//!   That is a gap and a small one — the row says `review \`agent/…\` (entry …)` and the queue is
//!   legible from its own words.
//!
//! # Which model it runs on
//!
//! The daemon's own — the operator's *"started either inside default model or configured model"*
//! read as *the default is the daemon's model, and the configured one is whatever this session was
//! switched to*: it is opened through the ordinary door, so `restore_provider_choice` hands it back
//! the model it was last switched to, which is the rule `--continue` already keeps for a
//! conversation. **A `[gatekeeper] model` of its own is not wired**, and deliberately not borrowed
//! from `Config::oracle_model`: that key is the GUARD's model — the local endpoint behind the smart
//! `!` and the adjudicator — and one key with two meanings is how a session comes to run on a model
//! nobody chose.
//!
//! # The one honest gap, named rather than discovered
//!
//! `cli.rs`'s `is_conversation` is the ONE predicate `--list-sessions` and `--continue` both read,
//! and the slice requires that `letibot --sessions` SHOW this session — so it must pass that
//! predicate. Which means `--continue` (and the launcher, which reads the same listing) can land the
//! operator in the review queue's session once it has rows, which is the shape of the defect
//! `GATEKEEPER_TITLE_PREFIX` was added for one word over (*"restart pg-noop and was brought to one
//! of the gatekeeper sessions"*). The fix is a decision and not a line: the two questions have to be
//! split, or this session marked non-conversational and the listing given a way to show it anyway.
//! It is left as it is because the slice's requirement is that the listing shows it, and because
//! choosing between those two is the operator's call.

use std::path::PathBuf;

use letibot_tokencore::store::{
    ReviewKind, ReviewRow, ReviewState, Store, TodoBy, TodoItem, TodoNeed, TodoStatus,
};
use letibot_tools::builtins::task::Placement;

/// **The session id, hardcoded, and the reason it is not minted like every other one.**
///
/// Session ids are `s-<now_ns>` — unique per creation and meaningless. This one has to be the
/// OPPOSITE: the same string in every daemon, on every start, forever, because it is the key three
/// things are decided by:
///
/// * **"Started once"** is a fact about this string. A daemon that comes up finds the session in the
///   store by it and RESUMES it ([`Registry::resumable`]'s own door), so a second daemon — or the
///   same daemon restarted — does not make a twin.
/// * **Attaching is by name.** *"so - i can connect to that session"*: the operator types this id,
///   and it is the same id tomorrow.
/// * **The board is the queue for THAT session**, and a per-run id would strand a board and its
///   store rows behind every restart.
/// * **It is the session a review row NAMES** (`mergequeue::GatekeeperDoor`): *the session that
///   reviewed it* — which is the column's own documented meaning, and the reason the review is
///   spawned by THIS harness and not by the tree the branch came from.
///
/// [`Registry::resumable`]: letibot_sessionlog::registry::Registry::resumable
pub const SESSION: &str = "s-gatekeeper";

/// What the session is called in a picker, and on its own store row. A title is set once and only
/// changed on purpose (`SessionBrief::title`), so this is what a person reads in `--sessions`.
pub const TITLE: &str = "the review queue";

/// **The session's SEAT, hardcoded, and it is the default one on purpose.**
///
/// The session has to spawn the review children (`task`, `task_result`, `task_message`,
/// `task_start`) and it has to open with **nobody attached** — the daemon starts it at startup,
/// before any head exists. Those two facts pick the seat between them: `orchestrator` is the only
/// seat that carries the whole task surface and **no gated tool at all**, so `Harness::open` has no
/// adjudicator to reach for and does not refuse.
///
/// A seat with `bash` (`gatekeeper` itself, `coder`, `runner`) would be refused at open with *"the
/// adjudicator attached is `none attached right now`"* — which is the right refusal, and the reason
/// the operator's *"gatekeepers are subagents"* is load-bearing: the CHILD that runs a review seats
/// `gatekeeper` and its asks go up to its tree's root, which is this session's head.
///
/// **It is not `self.base.seat`.** The daemon's `--role` is the seat of the OPERATOR's conversation;
/// a daemon started `--role coder` would otherwise seat the review queue as a coder, which cannot
/// open unattended.
pub const SEAT: crate::config::Seat = crate::config::Seat::Orchestrator;

/// **Where a review-queue session's reviewers run** — the decision as a STATE, rather than as a
/// placement that happens to come out `host` when something was wrong.
///
/// There are three states and not two, and the third is the point of the type: *this box cannot
/// boot a VM* is a fact with a name and with the box's own reason attached, and not a silent
/// fallback to the machine the session asked to leave. The session is told which one it is
/// ([`prompt`]), so the difference is legible from the transcript and not only from a log line —
/// and a reader can tell *the session vetoed the VM* from *the box could not give one*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewerPlacement {
    /// **A firecode VM.** The VM is the boundary, so a reviewer's `bash` asks nobody: there is no
    /// card for this session's head or the operator to answer. **The tree it is booted on is the
    /// entry's own REPOSITORY** ([`Harness::serve_reviews`] sets `TaskSpec::vm_source` from
    /// `mergequeue::review_copy_source`), not this session's workspace — a session may be seated a
    /// level above its repositories, and copying the level is 72.8 GiB of siblings that the
    /// ceiling refuses.
    ///
    /// [`Harness::serve_reviews`]: crate::harness::Harness::serve_reviews
    Vm,
    /// **This machine, because the session states no VM** ([`crate::config::Config::gatekeeper_vm`])
    /// — the arm a caller that must not boot one takes, and the behaviour every review had before
    /// the placement existed.
    Host,
    /// **This machine, because the BOX cannot boot one.** The session asked for a VM and this is
    /// the box's answer, carrying the reason it gave: declared, so nothing here reads as a choice
    /// nobody made.
    NoVm { why: String },
}

impl ReviewerPlacement {
    /// **The `task` placement this state means** — what [`Harness::serve_reviews`] puts on the
    /// review child's [`letibot_tools::builtins::task::TaskSpec`].
    ///
    /// [`Harness::serve_reviews`]: crate::harness::Harness::serve_reviews
    pub fn placement(&self) -> Placement {
        match self {
            ReviewerPlacement::Vm => Placement::Firecode,
            ReviewerPlacement::Host | ReviewerPlacement::NoVm { .. } => Placement::Host,
        }
    }

    /// **Where the reviewers run, in the session's own words** — the paragraph [`prompt`] appends
    /// to [`PROMPT`].
    ///
    /// Each arm says the same three things, because each is what a reader of this session's
    /// transcript needs to know: where the children run, why that is the answer, and what the
    /// consequence is for the cards that come to this session's head. The two `host` arms differ in
    /// their *why* and that is exactly the distinction the state exists to keep — a session that
    /// vetoed the VM is not a box that cannot give one, and a person reading the log has to be able
    /// to tell which of the two they are looking at.
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
Where your reviewers run: **on this machine**, inside this session's own boundary — this session \
states no VM (`gatekeeper_vm = false`), so its reviewers are not placed in one. A reviewer seats \
`gatekeeper` and has `bash`, so its calls are permission cards, and they come to YOUR head: a card \
nobody answers times out and the review fails. If a review stalls waiting on one, that is why.\n"
                .to_string(),
            ReviewerPlacement::NoVm { why } => format!(
                "\n\
Where your reviewers run: **on this machine, NOT in a VM** — this session asks for a VM \
(`gatekeeper_vm`) and this box cannot boot one: {why}. That is the box's answer and not a choice \
this session can override, because a `firecode` placement this box cannot honour would fail every \
review in the queue at once: a reviewer that cannot start is written as a FAILED ATTEMPT on the \
row. So the reviewer runs here, seats `gatekeeper` with `bash`, and its calls are permission cards \
that come to YOUR head. The operator's remedy is a `firecode` on the daemon's PATH (or \
`$FIRECODE_BIN`) and a restart.\n"
            ),
        }
    }
}

/// **Where the reviewers of a review-queue session run**: a firecode VM where this box can boot
/// one, this machine where it cannot — and this machine when the session states no VM.
///
/// # Why the reviewer is the one child whose placement is decided FOR it
///
/// Every other child's placement is the caller's word (`task`'s `where`, `task_start`'s). A
/// reviewer has no caller: the queue's own clock starts it ([`Harness::serve_reviews`]), and the
/// session it belongs to is a HOST session seating `bash`, so every `bash` call a reviewer makes is
/// a permission card — measured as one card per reviewer, and with no head attached the card times
/// out at 300 s and the review FAILS. Inside a VM there is no card to answer, because the VM is the
/// boundary: the reviewer runs at allow-all and asks nobody, which is the whole of what the queue
/// needs from it. The operator's own rule for the substrate says the same thing from the other end:
/// *"Use it for work that must not spend the operator's attention — a reviewer, a long build, a
/// probe."* So this is the one place that has to decide.
///
/// # Why the box's answer cannot be overridden into a VM it cannot boot
///
/// `task(where: firecode)` refuses AT THE CALL on a box with no firecode
/// ([`letibot_tools::builtins::task`]'s `placement_arg`), because there is a caller there to refuse
/// to. This path has none: [`Harness::serve_reviews`] writes a reviewer that could not start as a
/// **failed attempt** on the review's row, so a `Firecode` this box cannot honour would park every
/// entry in the queue at once — on the one box that can run no reviewer at all. So `may_use_vm` can
/// only ever REMOVE the VM, never force one, and the box's own answer is a declared state
/// ([`ReviewerPlacement::NoVm`]) rather than a silent `Host`.
///
/// `may_use_vm` is [`crate::config::Config::gatekeeper_vm`], and it is an argument rather than an
/// environment read for the reason [`letibot_tools::firecode::FirecodeSpec`]'s `bin` is one: a
/// caller that must not boot a VM says so as a value, instead of racing every other reader of
/// `$FIRECODE_BIN` in the process.
///
/// **What the check is, and what it is not.** `firecode_available()` answers *the CLI is there and
/// runnable*, which is not the same claim as *a VM can boot*: a box with the CLI and no working
/// hypervisor would fail every reviewer and park the queue on the failed attempts. Nothing cheaper
/// answers the stronger question without booting one, and the limit is stated here rather than left
/// for a reader to assume it away.
///
/// **The review prompt is not told any of this, on purpose.**
/// [`letibot_tools::gatekeeper::review_prompt`] names no absolute path — `git diff
/// <base>...<branch>`, `rg PATTERN PATH`, and the advice to hand `bash` a `cwd` rather than
/// `cd`-ing are all relative, and relative is what a copy preserves — so a reviewer reads the same
/// words wherever it runs. See [`ReviewerPlacement::said`] for what THIS session is told instead.
///
/// # The other half of the decision: WHICH TREE the VM is booted on
///
/// A `Firecode` placement copies a tree, and this function answers only *where*, not *what*. The
/// what is [`Harness::serve_reviews`]'s next line — `TaskSpec::vm_source`, from
/// `mergequeue::review_copy_source` — and it is the half that was missing for the whole life of
/// the placement: the copy source was the session's own workspace, which for the review queue is
/// the daemon's workspace, which is not always the repository. On the operator's box it is a
/// folder of repositories and the copy is 72.8 GiB, refused at `crates/tools`' ceiling, so no
/// reviewer started and the queue landed nothing (measured 2026-10-10, three refusals for two
/// branches).
///
/// **A VM-placed reviewer had never been exercised end to end on this box, and that is the larger
/// finding of that night.** The placement itself is asserted (`review_pass` records what the pass
/// asked for; `gatekeeper`'s own tests are pure arms), the real-spawn tests state no VM — a fixture
/// built the ordinary way does, and that is `Config::for_this_box`'s default — and the `firecode`
/// backend's own live test is behind `FIRECODE_LIVE=1` and never had a reviewer's tree put through
/// it. So the first time a reviewer was actually booted was the first time the queue tried to
/// review — and the thing that failed was not the placement but the tree under it.
///
/// [`Harness::serve_reviews`]: crate::harness::Harness::serve_reviews
pub fn reviewer_placement(
    may_use_vm: bool,
    available: Result<PathBuf, String>,
) -> ReviewerPlacement {
    match (may_use_vm, available) {
        // **The session's veto first**, so a box that COULD boot one is not consulted about a
        // reviewer this session has said must not be placed in one.
        (false, _) => ReviewerPlacement::Host,
        (true, Ok(_)) => ReviewerPlacement::Vm,
        // **And the box's inability is a state, not a fallback.** Every arm is named; there is no
        // catch-all here, because a catch-all is exactly how *the box cannot boot one* becomes
        // indistinguishable from *the session said no*.
        (true, Err(why)) => ReviewerPlacement::NoVm { why },
    }
}

/// **What the review queue's own session is TOLD, in its own system prompt** — the operator's
/// *"it managed via its prompt and so on"*, read as an instruction rather than as a description.
/// The session that works this board is managed by this paragraph, so the paragraph says what the
/// rows are, who starts the review, what the handle on a row is for, and what alone takes a row
/// off.
///
/// # Where it goes, and when it is read
///
/// Appended to the composed prompt at open ([`crate::sessions::OwnSession::prompt`]), after the
/// standing notes. It is part of the FROZEN prefix like every other session's prompt — a session
/// that is already open keeps the prompt it opened with, and `ReseatSession` is the tree's remedy
/// for that (`the-review-queue-and-the-merge-queue.md`'s own note about the pg-noop session, one
/// session over).
///
/// # What it deliberately does not say
///
/// * **No verb that has no door.** It does not tell the session to mark a queue row done — no
///   field of `todo_write` reaches that half — and it does not tell it to start the review, because
///   the queue's own clock does that ([`Harness::serve_reviews`]) and a second child for one entry
///   would be two reviewers reading one branch.
/// * **No second copy of the queue's vocabulary.** The row's words carry the branch and the entry
///   id; the STATE is the store's (`MergeEntry::state`, `ReviewRecord::decision`) and the paragraph
///   says where to read it rather than restating a set of words that could then disagree with it.
///
/// # The seam it states plainly
///
/// The reviewer hands its verdict in ITSELF — `review_verdict`, the one tool a reviewer holds —
/// and this session does not read its answer for one. The paragraph is written for what is true:
/// the verdict is what takes a row off, wherever it was written from, and the session's own verbs
/// on a child are `task_result` and `task_message`.
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

/// **The paragraph the review queue's session opens with**, for the placement its reviewers have.
///
/// [`PROMPT`] is the constant half — what the rows are, that a review is this session's own child,
/// and what alone takes a row off. The placement is the half that CANNOT be a constant, because it
/// is this box's answer ([`reviewer_placement`]), and it is appended last so the session reads what
/// it is, then what it does about it, then where the children that do it run.
///
/// **One function, two callers**: this one at open, and [`Harness::serve_reviews`] at every spawn.
/// They ask the same question of the same two facts, so the paragraph a session was given and the
/// placement its children actually get cannot drift apart — which is the whole reason the decision
/// is a function here rather than a line at each site.
///
/// [`Harness::serve_reviews`]: crate::harness::Harness::serve_reviews
pub fn prompt(placement: &ReviewerPlacement) -> String {
    format!("{PROMPT}{}", placement.said())
}

/// **The rows on the review queue's board** — one per review this daemon's queues serve that is
/// waiting for a reviewer or under one, in the order the queue claims them, with no child on any
/// of them. The harness marks the ones a review child is working ([`row`]).
pub fn queue_rows(store: &Store, me: &str) -> Vec<TodoItem> {
    waiting(store, me).iter().map(|r| row(r, None)).collect()
}

/// **The reviews this daemon's queues serve that are waiting for a verdict** — rows `waiting` or
/// `reviewing` in a repository whose LEASE this daemon holds (R61 decision 1). A board carrying
/// another daemon's rows would nag a session about work it must not touch (R61 defect 9: every
/// daemon served the one global review session).
///
/// The order is the queue's own claim order — the red target first, then priority, then age — so
/// the first row is the next one a reviewer is put on, and the board is stable across passes
/// (`set_queue` compares the rows).
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

/// **One review, as a row on the session's board** — and, when a review child is on it, the row
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
/// branch was asked to do, short enough that the row is still a name — see [`row`].
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
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    use letibot_sessionlog::registry::Registry;
    use letibot_tokencore::store::{
        Approval, LeaseClaim, MergePriority, Move, NewReview, ReviewKind,
    };
    use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus};

    use crate::config::Config;
    use crate::harness::Parts;
    use crate::sessions::{Sessions, StoreSessions};

    /// **A runner that records what it was asked to start and answers `Running` for ever** — the
    /// child a real runner would start needs a model, and what is measured here is *whose runner
    /// was asked for what*.
    struct Recording {
        /// `(prompt, role)` per `start`, in order.
        started: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl TaskRunner for Recording {
        fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
            let mut g = self.started.lock().unwrap_or_else(|e| e.into_inner());
            g.push((prompt.to_string(), spec.role.clone()));
            Ok(format!("sub-{}", g.len()))
        }

        fn collect(&self, _handle: &str, _timeout: Duration) -> TaskStatus {
            TaskStatus::Running {
                note: None,
                child: None,
            }
        }
    }

    /// A directory that removes itself.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "letibot-gatekeeper-{tag}-{}-{}",
                std::process::id(),
                {
                    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                }
            ));
            std::fs::create_dir_all(&path).expect("creating the temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// This test process's daemon identity — the owner the board is read for.
    fn me() -> String {
        crate::mergequeue::lease::this_daemon().owner.clone()
    }

    fn store_with(tag: &str) -> (TempDir, Store, PathBuf) {
        let dir = TempDir::new(tag);
        let ws = dir.path.join("ws");
        std::fs::create_dir_all(&ws).expect("the workspace");
        let ws = std::fs::canonicalize(&ws).expect("the workspace canonicalizes");
        let store = Store::open(&dir.path.join("sessions.db")).expect("the store opens");
        (dir, store, ws)
    }

    /// The lease on `repo`, held by `owner`.
    fn hold(store: &Store, repo: &Path, owner: &str) {
        let claim = LeaseClaim {
            owner: owner.into(),
            pid: 1,
            host: "t".into(),
            build: "t".into(),
        };
        store
            .acquire_queue_lease(
                &repo.display().to_string(),
                &claim,
                (crate::config::now_ns() / 1_000_000) as u64,
                crate::mergequeue::lease::LEASE_TTL_MS,
            )
            .expect("the lease");
    }

    /// One review row in `repo`, waiting for a reviewer.
    fn entry(store: &Store, repo: &Path, id: &str, brief: &str) {
        entry_of(store, repo, id, brief, ReviewKind::Branch);
    }

    fn entry_of(store: &Store, repo: &Path, id: &str, brief: &str, kind: ReviewKind) {
        store
            .enqueue_review(
                &NewReview {
                    id: id.into(),
                    repository: repo.display().to_string(),
                    target: "main".into(),
                    kind,
                    session_id: format!("s-{id}"),
                    branch: format!("agent/{id}"),
                    worktree: None,
                    base_sha: "base".into(),
                    head_sha: format!("head-{id}"),
                    brief: brief.into(),
                    priority: MergePriority::Subagent,
                    fixes: None,
                    evidence: String::new(),
                },
                "test",
                (crate::config::now_ns() / 1_000_000) as u64,
            )
            .expect("the review row");
    }

    /// The queue's claim, as its thread makes it: `reviewing`, owned by this daemon.
    fn claim(store: &Store, id: &str) {
        assert!(
            store
                .move_review(
                    id,
                    ReviewState::Waiting,
                    ReviewState::Reviewing,
                    &Move::new(me(), "claimed").set("owner", me()),
                    (crate::config::now_ns() / 1_000_000) as u64,
                )
                .expect("the claim")
        );
    }

    /// An approval, written the way another host writes it — straight into the store.
    fn approve(store: &Store, id: &str) {
        let r = store.review_row(id).unwrap().unwrap();
        assert!(
            store
                .approve_review(
                    id,
                    &Approval {
                        expect: r.state,
                        expect_owner: None,
                        reviewed_sha: r.head_sha,
                        by: "operator:s-x".into(),
                        reasons: vec![],
                        files: vec![],
                        commands: vec![],
                        why: "approved".into(),
                        merge_id: format!("m-{id}"),
                    },
                    (crate::config::now_ns() / 1_000_000) as u64,
                )
                .unwrap()
        );
    }

    /// **One row per review waiting for a verdict, named by the daemon's run name, the row id and
    /// the first line of the ask.**
    #[test]
    fn one_row_per_review_waiting_for_a_verdict() {
        let (_dir, store, ws) = store_with("rows");
        hold(&store, &ws, &me());
        entry(
            &store,
            &ws,
            "m-one",
            "make the reader keep the last record\nand then some",
        );
        entry(&store, &ws, "m-two", "the other branch");
        let rows = queue_rows(&store, &me());
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows.iter()
                .all(|r| r.by == TodoBy::Queue && r.status == TodoStatus::Pending)
        );
        let first = &rows[0].content;
        assert!(
            first.contains("m-one") && first.contains("agent/m-one"),
            "{first}"
        );
        assert!(
            first.contains("make the reader keep the last record"),
            "{first}"
        );
        assert!(!first.contains("and then some"), "{first}");
        assert_eq!(
            rows[1].content,
            "review-agent/m-two (entry m-two): the other branch"
        );
    }

    /// **A verdict reached elsewhere takes the row off** — approved and rejected rows are not
    /// waiting for anything.
    #[test]
    fn a_verdict_reached_elsewhere_takes_the_row_off() {
        let (_dir, store, ws) = store_with("verdict");
        hold(&store, &ws, &me());
        entry(&store, &ws, "m-one", "keep the last record");
        entry(&store, &ws, "m-two", "the other branch");
        assert_eq!(queue_rows(&store, &me()).len(), 2);
        approve(&store, "m-one");
        let rows = queue_rows(&store, &me());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].content.contains("m-two"));
        store
            .move_review(
                "m-two",
                ReviewState::Waiting,
                ReviewState::Rejected,
                &Move::new("r", "no").set("verdict", "reject"),
                1,
            )
            .unwrap();
        assert!(queue_rows(&store, &me()).is_empty());
    }

    /// **A repository whose lease another daemon holds is not on this board** — R61 defect 9:
    /// every daemon served the one global review session, and two would each spawn a reviewer.
    #[test]
    fn another_daemons_repository_is_not_on_this_board() {
        let (_dir, store, ws) = store_with("served");
        let theirs = ws.parent().expect("a parent").join("other");
        hold(&store, &ws, &me());
        hold(&store, &theirs, "another-daemon");
        entry(&store, &ws, "m-mine", "mine to review");
        entry(&store, &theirs, "m-theirs", "theirs to review");
        let rows = queue_rows(&store, &me());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].content.contains("m-mine"), "{:?}", rows[0]);
    }

    /// **The red target's review comes first on the board, as it does in the queue.**
    #[test]
    fn the_red_targets_review_is_first() {
        let (_dir, store, ws) = store_with("red-first");
        hold(&store, &ws, &me());
        entry(&store, &ws, "m-branch", "a branch");
        entry_of(&store, &ws, "red-main", "main is red", ReviewKind::Target);
        let rows = waiting(&store, &me());
        assert_eq!(rows[0].id, "red-main", "{rows:?}");
    }

    /// **A daemon over a temp store**, with the queue's session and a recording runner. `None`
    /// after announcing the skip when this box has no vocabulary GGUF.
    struct Fixture {
        _dir: TempDir,
        db: PathBuf,
        ws: PathBuf,
        cfg: Config,
        registry: Arc<Registry>,
        started: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Fixture {
        fn new(tag: &str) -> Option<Self> {
            let gguf = crate::vocab_gguf()?;
            let dir = TempDir::new(tag);
            let ws = dir.path.join("ws");
            std::fs::create_dir_all(&ws).expect("the workspace");
            let ws = std::fs::canonicalize(&ws).expect("the workspace canonicalizes");
            let db = dir.path.join("sessions.db");
            let mut cfg = Config::for_this_box(&ws);
            cfg.vocab_gguf = Some(gguf);
            cfg.store = Some(db.clone());
            cfg.session_id = "s-host".into();
            cfg.http_retries = 0;
            let registry = Registry::new();
            registry
                .create(&cfg.session_id, "", Sessions::wiring(&cfg))
                .expect("the daemon's own session registers");
            registry.set_source(StoreSessions::open(&cfg).expect("the store as a source"));
            Some(Self {
                _dir: dir,
                db,
                ws,
                cfg,
                registry,
                started: Arc::new(Mutex::new(Vec::new())),
            })
        }

        fn sessions<'p>(&self, parts: &'p Parts) -> Sessions<'p> {
            Sessions::open_first(parts, self.cfg.clone(), self.registry.clone())
                .expect("the daemon opens")
        }

        fn opened<'p>(&self, parts: &'p Parts) -> Sessions<'p> {
            let mut sessions = self.sessions(parts);
            sessions
                .ensure_gatekeeper()
                .expect("the queue's session opens");
            sessions
                .open
                .get_mut(SESSION)
                .expect("the queue's session is open")
                .set_subagents_for_test(Arc::new(Recording {
                    started: self.started.clone(),
                }));
            sessions
        }

        fn asked(&self) -> Vec<(String, String)> {
            self.started
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        fn prompt<'p>(&self, sessions: &'p Sessions<'_>) -> &'p str {
            &sessions
                .open
                .get(SESSION)
                .expect("the queue's session is open")
                .config()
                .system
        }

        fn store(&self) -> Store {
            Store::open(&self.db).expect("the store reopens")
        }

        /// A review row in this daemon's repository (its workspace), lease held by this daemon.
        fn entry(&self, id: &str, brief: &str) {
            let s = self.store();
            hold(&s, &self.ws, &me());
            entry(&s, &self.ws, id, brief);
        }

        fn board(&self, sessions: &Sessions<'_>) -> Vec<TodoItem> {
            sessions
                .open
                .get(SESSION)
                .expect("the gatekeeper session is open")
                .todo_list()
                .into_iter()
                .filter(|r| r.by == TodoBy::Queue)
                .collect()
        }
    }

    /// **The session is TOLD what its board is** — the operator's *"it managed via its prompt and
    /// so on"*, which is the one place a session can be managed without a person at the keyboard.
    ///
    /// Four facts the paragraph has to carry, and each is asserted rather than the whole text: the
    /// rows are the queue's, a review is a `task` child of THIS session, the row names the child's
    /// handle, and the verdict — not `todo_write` — is what takes a row off. A prompt that lost one
    /// of them would be a session guessing at exactly the thing this slice moved.
    #[test]
    fn the_session_is_told_what_its_board_is() {
        let Some(f) = Fixture::new("prompt") else {
            return;
        };
        let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
        let sessions = f.opened(&parts);
        let said = f.prompt(&sessions);

        assert!(
            said.contains("You are the review queue"),
            "the session is told what it is: {said}"
        );
        assert!(
            said.contains("A review is a `task` child of YOURS"),
            "and that the reviewer is its own child, which is the half this slice moves: {said}"
        );
        assert!(
            said.contains("task_result") && said.contains("task_message"),
            "and which doors it has on that child: {said}"
        );
        assert!(
            said.contains("do not start one yourself"),
            "and that the queue's own clock starts it, so it must not start a second: {said}"
        );
        assert!(
            said.contains("no field of `todo_write` reaches it"),
            "and that the rows are not its to strike off: {said}"
        );
        assert!(
            said.contains("The verdict is what takes the row off your board"),
            "and what DOES take a row off: {said}"
        );
        // **And that the verdict is the CHILD's own act**, which is the half this slice adds: the
        // session is told the door the reviewer holds, and that a reply in prose is not a verdict —
        // because a session that went looking for one in `task_result`'s payload would be reading
        // exactly what the queue stopped reading.
        assert!(
            said.contains("review_verdict"),
            "and that the reviewer hands its verdict in ITSELF: {said}"
        );
        assert!(
            said.contains("has judged nothing"),
            "and what an answer in prose means: {said}"
        );
        // **And it is the session's OWN prompt, not the daemon's** — the ordinary sessions are the
        // composed prompt byte for byte, which is what `OwnSession::prompt`'s `None` means.
        assert!(
            !sessions
                .open
                .get(&f.cfg.session_id)
                .expect("the daemon's own session is open")
                .config()
                .system
                .contains("You are the review queue"),
            "the paragraph goes to the queue's session and to no other"
        );
    }

    /// **A reviewer is placed in a VM where this box can boot one, and on this machine where it
    /// cannot — and *the box cannot* is a DECLARED state, not a silent fallback.**
    ///
    /// The box is an ARGUMENT here rather than the environment, so every arm holds on whatever
    /// machine runs this and `$FIRECODE_BIN` is not raced — the discipline `firecode.rs` states for
    /// its own `bin` field. Three arms, and the third is the point: *the session said no* and *the
    /// box could not* are two different states, and a catch-all `_ => Host` is exactly how they
    /// become one thing that neither a reader nor the session can tell apart.
    #[test]
    fn a_reviewer_is_placed_where_the_box_can_place_one() {
        let here = PathBuf::from("/usr/local/bin/firecode");
        let absent = "there is no `firecode` on this host's PATH (nor $FIRECODE_BIN)".to_string();
        // The box can boot one and the session does not object: the reviewer runs in a VM.
        assert_eq!(
            reviewer_placement(true, Ok(here)).placement(),
            Placement::Firecode,
            "a box that can boot a VM places the reviewer in one: the VM is the boundary, so no \
             `bash` call of its is a permission card"
        );
        // **And the box cannot: a state carrying the box's own words**, not a bare `Host`.
        let no_vm = reviewer_placement(true, Err(absent.clone()));
        assert_eq!(
            no_vm,
            ReviewerPlacement::NoVm { why: absent },
            "a box that cannot boot one runs the reviewer here and SAYS SO — the reason travels \
             with the state instead of being dropped on the floor"
        );
        assert_eq!(
            no_vm.placement(),
            Placement::Host,
            "and what the spawn carries is this machine — the only placement left"
        );
        // **And a session that states no VM is not overridden by a box that could boot one.**
        assert_eq!(
            reviewer_placement(false, Ok(PathBuf::from("/usr/bin/firecode"))),
            ReviewerPlacement::Host,
            "the veto is the session's, and a box that could boot a VM does not undo it"
        );
    }

    /// **The session is told WHERE ITS REVIEWERS RUN**, and told which of the three states it is
    /// in — because the two `host` states have different consequences for the session reading them.
    ///
    /// `the_session_is_told_what_its_board_is` asserts the constant half. This is the half that
    /// cannot be a constant, so all three arms are asserted on the pure function — where the box is
    /// an argument and no machine's `$PATH` is consulted — and the arm a real session can be put in
    /// without racing the box is asserted on the session itself below.
    #[test]
    fn the_session_is_told_where_its_reviewers_run() {
        let vm = prompt(&reviewer_placement(
            true,
            Ok(PathBuf::from("/usr/bin/firecode")),
        ));
        assert!(
            vm.contains("You are the review queue"),
            "the constant half is still there, ahead of the placement: {vm}"
        );
        assert!(
            vm.contains("Where your reviewers run: **in a firecode VM**"),
            "a VM placement is said, and said as WHERE ITS REVIEWERS RUN: {vm}"
        );
        let why = "there is no `firecode` on this host's PATH (nor $FIRECODE_BIN)".to_string();
        let none = prompt(&reviewer_placement(true, Err(why.clone())));
        assert!(
            none.contains("NOT in a VM") && none.contains(&why),
            "and a box that cannot boot one is a DECLARED state carrying the box's own words — \
             the difference between this and the arm below is the whole reason the state exists: \
             {none}"
        );
        let host = prompt(&reviewer_placement(
            false,
            Ok(PathBuf::from("/usr/bin/firecode")),
        ));
        assert!(
            host.contains("`gatekeeper_vm = false`"),
            "and a session that vetoed the VM is told the veto is why, not the box: {host}"
        );
    }

    /// **A session that states no VM reads it in its own prompt** — the arm a test can put a real
    /// session in on ANY box, because `gatekeeper_vm = false` is the answer whatever
    /// `firecode_available()` says. The other two arms are the pure function's, above.
    ///
    /// **The fixture says nothing to get this.** It is built the ordinary way
    /// (`Config::for_this_box`), which states no VM — so this is also the assertion that a session a
    /// test opens cannot boot a microVM over its workspace by forgetting a line. A test that means
    /// to exercise the VM arm asks for one by name (`Config::allow_vm_reviewers`), and the VM arm's
    /// own words are asserted on the pure function above, where the box is an argument rather than a
    /// `$FIRECODE_BIN` race.
    #[test]
    fn a_session_that_states_no_vm_is_told_its_reviewers_run_here() {
        let Some(f) = Fixture::new("prompt-host") else {
            return;
        };
        let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
        let sessions = f.opened(&parts);
        let said = f.prompt(&sessions);

        assert!(
            said.contains("Where your reviewers run: **on this machine**")
                && said.contains("`gatekeeper_vm = false`"),
            "the queue's session reads where its reviewers run, and the veto is named as the \
             reason rather than the box: {said}"
        );
    }

    /// **The session is started once, and a second pass or a second daemon does not make a twin.**
    ///
    /// The idle arm calls `ensure_gatekeeper` on every pass it reads the store — every thirty
    /// seconds — so "once" has to be a property of the door and not of the caller. It is two
    /// checks: `self.open` for this daemon, and the store (through the registry) for the box.
    #[test]
    fn the_session_is_started_once_and_not_twice() {
        let Some(f) = Fixture::new("once") else {
            return;
        };
        let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
        let mut sessions = f.sessions(&parts);
        let held = |reg: &Registry| {
            reg.list()
                .iter()
                .filter(|b| b.session_id == SESSION)
                .count()
        };

        assert!(
            sessions.ensure_gatekeeper().expect("it starts"),
            "the daemon starts it"
        );
        assert!(
            sessions.open.contains_key(SESSION),
            "and holds a harness for it"
        );
        assert_eq!(held(&f.registry), 1, "one row in the registry, not two");

        // **The second pass is the same session, not a second one.** This is the common case: the
        // idle arm reaches this line every time the review clock is due.
        assert!(
            !sessions.ensure_gatekeeper().expect("a no-op"),
            "the second pass starts nothing"
        );
        assert_eq!(
            sessions.opened(),
            2,
            "the held session and the queue's, and no third"
        );
        assert_eq!(held(&f.registry), 1, "still one");

        // **And a second DAEMON does not make a twin either** — the restart, one process along: a
        // fresh registry over the same store, which finds the session on disk and RESUMES it.
        //
        // **The name is changed in the store first, and that is what makes this an assertion rather
        // than a coincidence**: a session made fresh would carry the hardcoded `TITLE`, and one
        // RESUMED carries the name its own row holds. The count alone could not tell the two apart —
        // `put_session` is skipped for a row that is already there, so a twin would leave one row too.
        f.store()
            .set_title(SESSION, "the queue, renamed")
            .expect("the store takes a name");
        let second = Registry::new();
        second
            .create("s-host-2", "", Sessions::wiring(&f.cfg))
            .expect("the second daemon's own session registers");
        second.set_source(StoreSessions::open(&f.cfg).expect("the store as a source"));
        let mut other = Sessions::open_first(
            &parts,
            Config {
                session_id: "s-host-2".into(),
                ..f.cfg.clone()
            },
            second.clone(),
        )
        .expect("the second daemon opens");
        assert!(other.ensure_gatekeeper().expect("it brings it in"));
        assert_eq!(held(&second), 1, "under the SAME id, resumed");
        assert_eq!(
            held(&f.registry),
            1,
            "and the first daemon's row is untouched"
        );
        assert_eq!(
            second
                .list()
                .iter()
                .find(|b| b.session_id == SESSION)
                .map(|b| b.title.as_str()),
            Some("the queue, renamed"),
            "RESUMED, not re-made: it came back under the name its own store row holds"
        );

        let stored = f.store().list_sessions().expect("the store lists");
        assert_eq!(
            stored.iter().filter(|s| s.id == SESSION).count(),
            1,
            "ONE row in the store: a second daemon resumes the session, it does not mint a twin"
        );
        assert_eq!(
            stored
                .iter()
                .find(|s| s.id == SESSION)
                .map(|s| s.title.as_deref()),
            Some(Some("the queue, renamed")),
            "and it is the session a person reads in `--sessions`: {stored:?}"
        );
    }

    /// **A claimed review is a `task` child of THIS session, bound to the head the claim read**
    /// (decision 6), and the row says which child is on it; a second pass starts no second child;
    /// the verdict takes the row off.
    #[test]
    fn a_claimed_review_is_this_sessions_child_and_its_row_says_so() {
        let Some(f) = Fixture::new("child") else {
            return;
        };
        let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
        f.entry("m-one", "make the reader keep the last record");
        let mut sessions = f.opened(&parts);
        assert_eq!(sessions.fill_gatekeeper_board(), 1);
        assert_eq!(f.board(&sessions)[0].status, TodoStatus::Pending);
        assert_eq!(
            sessions.serve_reviews(),
            0,
            "nothing is started for a row the queue has not claimed"
        );

        claim(&f.store(), "m-one");
        assert_eq!(sessions.serve_reviews(), 1, "{:?}", f.asked());
        let asked = f.asked();
        let (prompt, role) = &asked[0];
        assert_eq!(role, "gatekeeper");
        assert!(
            prompt.starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX),
            "{prompt}"
        );
        assert!(
            prompt.contains("make the reader keep the last record"),
            "{prompt}"
        );
        assert!(
            prompt.contains("git diff base...head-m-one"),
            "the reviewer reads the HEAD the claim bound, not the branch as it is: {prompt}"
        );
        let row = f.store().review_row("m-one").unwrap().unwrap();
        assert_eq!(row.reviewer.as_deref(), Some("sub-1"), "{row:?}");
        let during = f.board(&sessions);
        assert_eq!(during[0].status, TodoStatus::InProgress, "{during:?}");
        assert_eq!(
            during[0].needs,
            vec![TodoNeed::Child { id: "sub-1".into() }]
        );

        assert_eq!(sessions.serve_reviews(), 0, "no second child for one row");
        assert_eq!(f.asked().len(), 1);

        // **The daemon's own session serves nothing**: reviewers are the queue session's.
        assert_eq!(
            sessions
                .open
                .get_mut(&f.cfg.session_id)
                .expect("open")
                .serve_reviews(),
            0
        );
    }
}
