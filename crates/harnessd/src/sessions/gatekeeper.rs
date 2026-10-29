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
//! **And a TREE of its own, though not a branch of its own.** A review is spawned as a plain
//! `task` and not a `task_start`: the reviewer READS a branch (`git diff base...sha` from the
//! checkout it is given — `gatekeeper::review_prompt` is the whole of its framing), and a
//! `task_start` tree would be a branch of its own that `HarnessTaskRunner::enqueue_entry` then
//! mints a queue entry for — a reviewer's empty tree offered to the merge queue as work. What it
//! does get is a detached CLONE of the entry's repository at the head the row names
//! (`mergequeue::review::start_reviewer`, both placements), handed over as the child's workspace,
//! so a reviewer on this machine reads the branch under review and not the tree the operator has
//! open.
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

/// **The review-queue session id of the daemon serving `workspace`** — `s-gatekeeper-<key>`.
///
/// Session ids are `s-<now_ns>` — unique per creation and meaningless. This one is the opposite
/// WITHIN a folder and never across them, because it is the key four things are decided by:
///
/// * **"Started once"** is a fact about this string. A daemon that comes up finds the session in the
///   store by it and RESUMES it ([`Registry::resumable`]'s own door), so the same daemon restarted
///   does not make a twin. *The same daemon*, and not *any daemon*: a second folder's daemon has a
///   key of its own and an id of its own, which is what stops two daemons writing one transcript —
///   every daemon on this box shares one store, and the store's append-only trigger refuses the
///   second writer's rows while the damage is already in the file.
/// * **Attaching is by name.** *"so - i can connect to that session"*: the operator types this id
///   (or the bridge's `leticode_attach {"session": "gatekeeper"}`, which resolves through
///   [`letibot_sessionlog::gatekeeper_session`]), and it is the same id tomorrow.
/// * **The board is the queue for THAT session**, and a per-run id would strand a board and its
///   store rows behind every restart.
/// * **It is the session a review row NAMES** (`mergequeue::GatekeeperDoor`): *the session that
///   reviewed it* — which is the column's own documented meaning, and the reason the review is
///   spawned by THIS harness and not by the tree the branch came from.
///
/// **The id itself is spelled one crate down**, in [`letibot_sessionlog::gatekeeper_session`],
/// because a HEAD has to be able to name it — the bridge's `leticode_attach` takes the name
/// `gatekeeper` — and a head that linked this crate to learn one id would be linking the daemon,
/// its providers and its wasm JIT. One definition, two names for it.
///
/// [`Registry::resumable`]: letibot_sessionlog::registry::Registry::resumable
pub fn session(workspace: &std::path::Path) -> String {
    letibot_sessionlog::gatekeeper_session(workspace)
}

/// **The review queue's id as a fixture spells it** — `s-gatekeeper-0123456789ab`, the shape a
/// daemon's own id has, with a key no workspace gives.
///
/// The TUI's review-card fixtures are golden text, and one of them names the parent session a
/// refusal came from. A fixture that hashed a temp path would pin a number no reader can check
/// and that moves with the box's own temp directory; what the card actually parses is the SHAPE
/// (`message from your parent session `s-gatekeeper-…``), so one invented key is enough.
#[cfg(test)]
pub(crate) fn session_for_fixture() -> String {
    "s-gatekeeper-0123456789ab".to_string()
}

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
    /// **This machine, because the project says so** ([`crate::config::Config::reviewers`]) —
    /// `[queue] reviewers = "host"`, the DEFAULT, and the arm a caller that must not boot one
    /// takes. The reviewer is a `gatekeeper`-seated child of this session working in a checkout of
    /// the head it judges ([`Harness::serve_reviews`] makes the clone and hands it over as the
    /// child's tree), so it reads the branch and not the operator's checkout.
    ///
    /// [`Harness::serve_reviews`]: crate::harness::Harness::serve_reviews
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

/// **Where the reviewers of a review-queue session run**: this machine — the DEFAULT, and what a
/// project that says `[queue] reviewers = "host"` or nothing gets — or a firecode VM, which a
/// project asks for by name and this box can only give where it can boot one.
///
/// # The default is the host, and the reason is a night of measurements (2026-10-29)
///
/// A VM-placed reviewer paid a **guest exec per file read**: `crates/tools/src/builtins/grep.rs`
/// did one `backend.read` per candidate, which over the firecode backend is one `firecode in`
/// each, so a repository-scale `grep` was hundreds of round trips and minutes of a held queue
/// slot. One exec lost its request between `socat`'s `OK` and `execserver.sh`'s first read and
/// both ends waited for ever (containment landed a 120 s deadline; the race did not). A review
/// therefore took 30–60 minutes and held one of two slots.
///
/// And the symmetry that settled it: the subagents that AUTHOR these branches already run on this
/// machine — `task_start`'s children do `cargo test` in the operator's own tree with the
/// operator's credentials — so placing only the READER in a VM buys isolation the author never
/// had, and pays for it in a protocol with no ack.
///
/// # Why the reviewer is the one child whose placement is decided FOR it
///
/// Every other child's placement is the caller's word (`task`'s `where`, `task_start`'s). A
/// reviewer has no caller: the queue's own clock starts it ([`Harness::serve_reviews`]). So this is
/// the one place that has to decide — and the decision is a project's ([`crate::config::Reviewers`]),
/// with the daemon's `--reviewers` as its override.
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
/// `may_use_vm` is [`crate::config::Config::reviewers`] read as one question, and it is an argument
/// rather than an environment read for the reason [`letibot_tools::firecode::FirecodeSpec`]'s
/// `bin` is one: a caller that must not boot a VM says so as a value, instead of racing every
/// other reader of `$FIRECODE_BIN` in the process.
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
/// # The other half of the decision: WHICH TREE the reviewer gets
///
/// **Both arms hand the reviewer a tree of its own, at the head the row names** — and neither is
/// this session's workspace, which for the review queue is the daemon's workspace and not always
/// the repository (on the operator's box it is a folder of repositories, 72.8 GiB against a VM's
/// 8 GiB ceiling, measured 2026-10-10). `mergequeue::review::start_reviewer` makes ONE clone per
/// review — `git::clone_branch(repo, branch, head_sha, <clones>/review-<row id>)` — and the two
/// arms differ only in what is done with it: a VM is booted on it (`TaskSpec::vm_source`), and a
/// host reviewer WORKS in it (`TaskSpec::worktree`, so its workspace and the `cwd` its prompt
/// names are both that checkout). The clone is detached at the sha, so a reviewer reads the head
/// it judges and never the tip the operator happens to have out.
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
mod tests;
