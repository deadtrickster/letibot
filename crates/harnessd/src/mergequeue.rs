//! The merge queue: the queue, its persistence, and the daemon thread that serves it.
//!
//! The operator's ask, in their words:
//!
//! > *"we need a gated merge to main, and worktree cleanup. for this we might need a merge
//! > queue. look how flowy does it - it has a nice queue with priorities and dependencies"*
//!
//! > *"merge queue is 2 - together with persistence and harnessd thread that serves it"*
//!
//! > *"it has to be durable so live in the session db"*
//!
//! Three pieces, one module, because they are one thing:
//!
//! * **The queue** — [`MergeEntry`] rows, one per branch the daemon is serving toward main,
//!   with a priority out of a closed set, a `needs` list of dependencies, and a state out of a
//!   closed set. The pure core below is the state machine: which entry is taken next, when an
//!   entry is ready, what base it rebases onto, and what cleanup a state owns. It is pure — no
//!   store, no git, no thread — so the rule is tested as a rule rather than as a daemon.
//! * **The persistence** — the rows live in `sessions.db` beside the job history, in the
//!   `merge_queue` table [`letibot_tokencore::store::Store`] reads and writes. The daemon is
//!   the only writer; a head never writes one.
//! * **The daemon thread** — [`MergeQueueDaemon`] takes the next entry, rebases it onto the
//!   *current* tip of main, runs the gate, fast-forwards main and pushes, and cleans up by
//!   state. It is a thread because the gate is long and the daemon has nothing else to do
//!   with its hands while it runs.
//!
//! # Serial, and tested at the tip
//!
//! The queue is serial: one entry at a time, and the gate runs on the entry *after* it is
//! rebased onto the current tip of main, not on the branch as it was cut. Two branches green
//! in isolation are not green together — the second one was tested against a main that no
//! longer exists, and the only place the combination is true is the tip. That is the whole
//! point of the queue, and it is why the rebase happens on taking rather than on enqueueing:
//! an entry that is rebased when it is enqueued is rebased against a tip that is already
//! stale by the time it is taken.
//!
//! # Nothing is dropped silently
//!
//! flowy's own comment makes the argument: a queue read that returns a shorter list says
//! *"that is all the work there is"*, which is false. So the queue's read is the whole queue,
//! every state, and an entry the queue cannot act on is listed with its reason — the
//! `evidence` on the row says what it is waiting on, why it failed, or why it is stale. A row
//! that says `waiting` while its gate job is dead is a lie the pane would draw, which is why
//! [`MergeState::Stale`] exists rather than a `Waiting` that means two things.
//!
//! # The fallback: the gatekeeper is an organ, not a single point of failure
//!
//! The operator's argument, in their words: *"even if gatekeeper session dies or didnt start -
//! the queue tooling can send a message back to the main project session and projects will gate
//! and merge on their own"*, *"because their code lives inside them in worktrees anyway"*.
//!
//! So the interface between the two halves is a **branch**, not a session. The gatekeeper's
//! contribution is a verdict; the gate and the merge are git and a shell, and a project can run
//! both itself from the worktree it already has. When there is no gatekeeper — never started,
//! died, or an ask that failed ([`NoGatekeeper`] names the three and where each is seen) — the
//! queue does not sit in `waiting` waiting for a verdict nobody will give. It **tells the
//! project's own session**, once, and says so on the entry's row ([`NO_GATEKEEPER`] is the clause
//! every reader of that row keys on): the session is the root of the tree that queued the entry
//! ([`root_session`]), found by `GatekeeperDoor::no_gatekeeper`, resumed from the store when this
//! daemon does not hold it.
//!
//! **And the project's own gate and merge are reached from that message and not from a click.**
//! The message names the entry, the branch and the worktree, and the two ways to finish it: run
//! this repository's gate (`main`'s `Merge gate` section — [`gate_on_main`]) in the worktree and
//! merge the branch into main itself, which the queue records as `Landed` on its next pass
//! (`MergeQueueDaemon::land_what_main_has`); or put `/queue approve` to the operator, which
//! lands it through the queue's own gate. **A person is the last resort, not the mechanism** —
//! and when the project's session is gone too, that is what the row says, with the person's verbs
//! on it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use letibot_tokencore::store::{
    MergeEntry, MergeGateOutcome, MergeGateStep, MergePriority, MergeState, Store,
};

// **The question vocabulary, for the third door.** A `cannot-decide` is a question and not an
// adjudication, so it goes through the questioner the tree already has and not through the gate —
// `crate::answers` says why those two must not be able to settle each other.
use letibot_tools::builtins::intent::{AskError, Question, QuestionAnswer};
// **The gatekeeper's closed set, for the doors.** One word, three doors, and the mapping lives in
// [`Door::of`] so that a STORED word and a reported one go through the same one.
use letibot_tools::gatekeeper::Decision;

// ===== The pure core: the state machine, with no store, no git and no thread =====

/// **What cleanup a state owns** — the answer to *what happens to the worktree and the
/// branch when an entry reaches this state*.
///
/// The operator's ask, in their words: *"we need a gated merge to main, and worktree
/// cleanup."* The cleanup differs by state, and that difference is the rule:
///
/// * **`Landed`** removes the worktree and deletes the branch. `git branch -d` is safe
///   precisely because it refuses an unmerged branch, so a branch that is deleted here is a
///   branch that is merged, and the refusal is the guarantee rather than a check.
/// * **`Failed` and `Conflict`** keep the worktree, with the reason on the row. Removing a
///   tree after a failed test destroys the evidence — the worktree is where the failure is,
///   and a person who answers it needs the tree to be there.
/// * **`Stale`** keeps the worktree for the same reason: the gate job died, and the tree is
///   where it died.
/// * **`Waiting` and `Taken`** keep the worktree because the merge is not done: the tree is
///   where the branch is checked out, and it is needed for the rebase and the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleanup {
    /// Remove the worktree and delete the branch.
    RemoveWorktreeAndBranch,
    /// Keep the worktree (the evidence stays, or the merge is not done).
    KeepWorktree,
}

/// **The cleanup a state owns** — one answer, decided by the state alone.
///
/// A predicate rather than a match at every call site: the daemon, the test and the pane all
/// ask *what happens to the tree when this entry is in this state*, and the answer is the
/// state's, not the caller's.
pub fn cleanup_for(state: MergeState) -> Cleanup {
    match state {
        // The merge is done: the tree and the branch can go.
        MergeState::Landed => Cleanup::RemoveWorktreeAndBranch,
        // The tree stays: the evidence is there (Failed, Conflict, Stale), or the merge is
        // not done yet (Waiting, Taken).
        _ => Cleanup::KeepWorktree,
    }
}

/// **The dependencies an entry names that have not `Landed` yet**, by id.
///
/// A dependency that is not in the queue at all is unmet, for the reason a `needs` list is a
/// list of *entries* rather than of branches: an entry that names a dependency the queue has
/// never held is an entry the queue cannot vouch for, and reading it as *met* would be a
/// guess the queue cannot support.
pub fn unmet_needs<'a>(entry: &'a MergeEntry, all: &'a [MergeEntry]) -> Vec<&'a str> {
    entry
        .needs
        .iter()
        .filter(|id| {
            all.iter()
                .find(|e| &e.id == *id)
                .map(|e| e.state != MergeState::Landed)
                .unwrap_or(true)
        })
        .map(|id| id.as_str())
        .collect()
}

/// **Whether an entry is ready to be taken** — its `needs` have all `Landed`.
///
/// The negation of [`unmet_needs`], and the two are one rule: an entry is ready when it has
/// nothing it is waiting on, and it is not ready when it names a dependency that has not
/// landed. A `Waiting` entry that is not ready is still listed, with the dependencies it is
/// waiting on as its evidence — not dropped, because a queue read that drops it says *"that
/// is all the work there is"*, which is false.
pub fn is_ready(entry: &MergeEntry, all: &[MergeEntry]) -> bool {
    unmet_needs(entry, all).is_empty()
}

/// **The next entry the daemon should take**, by index into `entries`, or `None` when
/// nothing is ready.
///
/// The queue's scheduling, as one answer: the `Waiting` entries whose `needs` have all
/// `Landed`, ordered by priority (an operator's urgent entry jumps subagent work) and then by
/// age (ties inside a rung break oldest-first). The index rather than the entry, because the
/// caller has the slice and the index is what it needs to take the entry by.
///
/// **The order is the rule, and it is the closed set's own order.** The priority is sorted by
/// its position in [`MergePriority::ALL`] — the one list — so a rung added there is reachable
/// by the scheduling and a rung outside it is reachable by none. The age is the entry's own
/// `created_ms`, and the id is the final tiebreaker so two entries enqueued in the same
/// millisecond do not come back in an order that changes between calls.
pub fn next_ready(entries: &[MergeEntry]) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.state == MergeState::Waiting && is_ready(e, entries))
        .min_by_key(|(_, e)| (e.priority.rank(), e.created_ms, e.id.clone()))
        .map(|(i, _)| i)
}

/// **The base an entry should rebase onto**, given the queue as it is now.
///
/// The base is the **current tip of main**, which is the `landed_sha` of the last entry to
/// land — any entry, not just the entry's dependencies — because the queue is serial and
/// everything lands on main, so the last one to land is the tip. The entry's `base_sha` is
/// the SHA it was written against, and it goes stale the moment anything else lands, which
/// is why the base is the landed tip rather than the stale SHA: two branches green in
/// isolation are not green together, and the rebase onto the current tip is what makes the
/// gate test the combination.
///
/// When no entry has landed, the current tip of main is the SHA the entry was written
/// against — its `base_sha` — because nothing has moved since it was cut. So the base is the
/// `landed_sha` of the last entry to land, and the entry's own `base_sha` when nothing has
/// landed.
///
/// The dependencies are a gating condition, not a base condition: [`next_ready`] will not
/// take an entry until its `needs` have all `Landed`, and by then the current tip of main is
/// the `landed_sha` of the last thing to land, which is what the rebase uses.
pub fn effective_base<'a>(entry: &'a MergeEntry, all: &'a [MergeEntry]) -> &'a str {
    all.iter()
        .filter(|e| e.state == MergeState::Landed)
        .filter_map(|e| e.landed_sha.as_deref().map(|sha| (e.updated_ms, sha)))
        .max_by_key(|(t, _)| *t)
        .map(|(_, sha)| sha)
        .unwrap_or(&entry.base_sha)
}

/// **On daemon restart, move every `Taken` entry to `Stale`** — the recovery that keeps a
/// dead job from coming back as `Waiting`.
///
/// A `Taken` row on disk is a gate job the daemon was running when it died, exactly as a
/// `running` job row is. If it came back as `Waiting`, the pane would draw it as *ready to be
/// taken*, which is a lie: the job that was working on it is dead, and the entry is not ready
/// — it is stale, and it needs a re-enqueue rather than a silent retry. So the recovery moves
/// it to `Stale`, with the reason on the row, and the queue lists it with that reason rather
/// than dropping it.
///
/// The move is idempotent: an entry that is already `Stale` is not moved again, and an entry
/// that is `Waiting` or terminal is not touched. So the recovery can run on every startup
/// rather than only on the first, and a daemon that restarts twice does not move an entry
/// twice.
pub fn recover(mut entries: Vec<MergeEntry>) -> Vec<MergeEntry> {
    for entry in &mut entries {
        if entry.state == MergeState::Taken {
            entry.state = MergeState::Stale;
            entry.evidence = "the gate job died with the daemon".into();
        }
    }
    entries
}

// ===== The git operations: the rebase, the merge, the push and the cleanup =====

/// Run one git command in `dir`, returning the stdout on success and the combined output on
/// failure. The output is the evidence: a git command that fails is a failure the queue
/// reports, and the report is git's own words rather than a guess.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let mut msg = String::from_utf8_lossy(&out.stderr).into_owned();
        if msg.is_empty() {
            msg = String::from_utf8_lossy(&out.stdout).into_owned();
        }
        Err(format!("git {args:?}: {msg}"))
    }
}

/// **Rebase the entry's branch, in its worktree, onto `onto`** — the tip of main as it is
/// now, not the SHA the entry was written against.
///
/// This is the half of *"tested at the tip"* that makes it true: the branch is rebased onto
/// the current main before the gate runs, so the gate tests the combination rather than the
/// branch in isolation. A conflict is an `Err` with git's own words, and the caller reports
/// it rather than resolving it — the queue's job is to notice, not to guess.
///
/// **`--autostash`, and without it NOTHING lands.** Every worktree this queue works in is dirty
/// in `.commit-msg` — the operator's TRACKED file, which the landing discipline writes the
/// commit message into — and a plain `git rebase` refuses a dirty tree with `cannot rebase: You
/// have unstaged changes`. That refusal arrives here as an `Err` and the caller turns it into a
/// [`MergeState::Conflict`], so a branch with no conflict at all is parked on git's complaint
/// about a file the landing itself wrote. `--autostash` stashes the dirt, replays the branch and
/// restores it, which is the intent exactly: the message file is the landing's scratch, not the
/// work. The other half of this — stop using the operator's file at all — is the caller's.
fn rebase(worktree: &Path, onto: &str) -> Result<(), String> {
    git(worktree, &["rebase", "--autostash", onto]).map(|_| ())
}

/// **Whether `main` already contains `branch`** — HALF of the check that turns a stale row into
/// what it is, instead of rebasing a branch onto a main that already has it.
///
/// **This half alone is a trap, and [`branch_standing`] is the whole answer.** *The branch is an
/// ancestor of main* is ALSO true of a branch a child never committed to: a branch standing at its
/// own base is an ancestor of main by construction, because its base is where main was when the
/// branch was cut. Read alone, this says *already merged* about a branch with no work on it — so
/// the pass asks the count in [`commits_over_base`] as well, and neither answer is used without
/// the other.
///
/// `git merge-base --is-ancestor BRANCH main` answers with its EXIT STATUS and nothing else: 0
/// when the branch is an ancestor of main, 1 when it is not, and neither prints a word. So the
/// answer is `run_captured`'s `Ok`, and an error — 128, a branch git cannot name, a repository
/// with no `main` — is `false` for the same reason it is not a conflict: a branch that cannot be
/// found is not a branch main contains, and the rebase that follows reports the reason in git's
/// own words rather than in a guess made here.
fn main_contains(repo: &Path, branch: &str) -> bool {
    run_captured(
        repo,
        "git",
        &["merge-base", "--is-ancestor", branch, "main"],
    )
    .is_ok()
}

/// **How many commits the entry's branch carries over the base it was cut from** — the fact
/// [`main_contains`] cannot answer, and the one without which its answer is a trap.
///
/// `git rev-list --count <base>..<branch>` counts the commits reachable from the branch and not
/// from the base, so **0 is *the branch stands where it was cut*: nothing was committed on it.**
/// That is the state a child leaves behind when it does the work and never commits — and it is
/// also the state that reads as *already merged* to the ancestor check, because the base of a
/// branch with nothing on it is, by construction, an ancestor of main.
///
/// **A base git cannot answer for is `Err`, and `Err` is NOT 0.** A base sha that does not
/// resolve, or the empty string a row with no base carries, is *the queue cannot tell* — and
/// *cannot tell* must never be read as the one answer that bins an entry's work. The caller gets
/// git's own words here rather than a guess.
fn commits_over_base(repo: &Path, branch: &str, base: &str) -> Result<usize, String> {
    // **The empty side of a git range means `HEAD`.** `..branch` is `HEAD..branch`, so an empty
    // base would answer a question about a different commit while looking like an answer about
    // this one; it is refused here instead.
    if base.trim().is_empty() {
        return Err(format!(
            "`{branch}` records no base sha, so there is no range to count its commits over"
        ));
    }
    let range = format!("{base}..{branch}");
    let count = git(repo, &["rev-list", "--count", &range])?;
    count.trim().parse::<usize>().map_err(|e| {
        format!(
            "git rev-list --count {range} answered `{}`, which is not a number: {e}",
            count.trim()
        )
    })
}

/// **What a branch is, relative to the base it was cut from AND to `main`** — the one question the
/// pass asks before it may skip the rebase, the gate and the fast-forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchStanding {
    /// It carries commits of its own AND `main` already contains its tip: the work is on main by
    /// somebody else's hand. `Landed`, without a rebase and without a gate.
    OnMain,
    /// **Nothing was committed on it** — its tip is its base, or behind it. Never `Landed`,
    /// whatever `main` contains.
    NothingCommitted,
    /// Anything else: commits of its own that main does not contain, and the ordinary path (take,
    /// rebase, gate, fast-forward) is the one that decides. **Also the answer when git cannot
    /// tell**, because a fact the queue cannot read must not be read as a fact it can.
    CarriesWork,
}

/// **The pass's decision about an entry's branch, as one function of two git facts** — and both
/// facts are needed, which is the whole reason this is a function rather than a condition.
///
/// **Neither half is the answer alone, and the cheap-looking one is the dangerous one.**
/// `main_contains` is the fact the check was added for: an entry whose branch was rebased and
/// landed by another path is genuinely already in main, and it must become `Landed` rather than be
/// rebased onto a main that has it. But *the branch is an ancestor of main* is ALSO true of a
/// branch that was never committed to at all — a branch sitting at its own base is an ancestor of
/// main by construction — and read alone that fact says *already landed* about a branch with no
/// work on it: the row is moved to `Landed`, the cleanup `Landed` owns runs, the worktree is
/// removed and the branch deleted. **The worktree is where the uncommitted work is**, so that
/// reading does not leave a stale row behind — it destroys the work and then says it landed.
///
/// So the count comes first and it is not optional: **0 commits over the base is
/// [`BranchStanding::NothingCommitted`] whatever main contains**, because *main has this branch's
/// tip* is not evidence that the branch's work is on main when the branch's tip is its base. A
/// reader tempted to keep only the ancestor check — it is one command fewer — should read the
/// paragraph above as the reason it is not one command fewer but one bug fewer, and the reason
/// [`main_contains`]'s own doc comment points here.
fn branch_standing(repo: &Path, branch: &str, base: &str) -> BranchStanding {
    match commits_over_base(repo, branch, base) {
        Ok(0) => BranchStanding::NothingCommitted,
        Ok(_) if main_contains(repo, branch) => BranchStanding::OnMain,
        _ => BranchStanding::CarriesWork,
    }
}

/// **Fast-forward main to the entry's branch**, returning the new tip SHA.
///
/// `--ff-only` rather than a merge commit, for the reason the operator's ask is a
/// *fast-forward*: the branch was just rebased onto the current main, so it is a descendant
/// of main, and a fast-forward is the only merge that keeps main's history linear. A
/// `--ff-only` that fails is a branch that is not a descendant of main, which is a state the
/// queue should not have produced, and the error is reported rather than papered over with a
/// merge commit.
fn fast_forward_main(repo: &Path, branch: &str) -> Result<String, String> {
    git(repo, &["merge", "--ff-only", branch])?;
    let tip = git(repo, &["rev-parse", "main"])?;
    Ok(tip.trim().to_string())
}

/// **Push main to the remote**, when there is one.
///
/// A repo with no remote is a local repo, and a local repo has nothing to push: the
/// fast-forward is the merge, and the push is a no-op. So this is a no-op rather than an
/// error when the remote is absent, and an error when the push fails — a push that fails is a
/// merge that did not land, and the queue reports it rather than marking the entry `Landed`.
fn push_main(repo: &Path) -> Result<(), String> {
    // A repo with no `origin` has nothing to push, and that is the local case, not the
    // error case.
    let has_origin = git(repo, &["remote", "get-url", "origin"]).is_ok();
    if !has_origin {
        return Ok(());
    }
    git(repo, &["push", "origin", "main"]).map(|_| ())
}

/// **Remove the entry's worktree and prune** — the cleanup `Landed` owns.
///
/// `git worktree remove` rather than `rm -rf`, for the reason the operator's ask is a
/// *worktree cleanup*: the worktree is a git object, and removing it through git is what
/// keeps the repo's worktree list honest. The `prune` follows, because a `remove` that fails
/// partway leaves a stale entry in the list, and the `prune` is what clears it.
fn remove_worktree(repo: &Path, worktree: &Path) -> Result<(), String> {
    // `to_string_lossy` rather than `to_str().unwrap()`: a worktree path that is not UTF-8 is a
    // path git will be told about lossily, which is what the enqueuer's own `worktree` column
    // holds anyway (it is a `String` on the row). A refusal here would be a refusal to clean up
    // a worktree that is really there.
    let wt = worktree.to_string_lossy();
    git(repo, &["worktree", "remove", "--force", &wt])?;
    git(repo, &["worktree", "prune"])?;
    Ok(())
}

/// **Delete the entry's branch** — the cleanup `Landed` owns, after the worktree is gone.
///
/// `git branch -d` rather than `-D`, for the reason the operator's ask names: `-d` is safe
/// precisely because it refuses an unmerged branch. A branch that is deleted here is a branch
/// that is merged — it was just fast-forwarded into main — and the refusal is the guarantee
/// that a branch the queue has not merged is not deleted by the queue.
fn delete_branch(repo: &Path, branch: &str) -> Result<(), String> {
    git(repo, &["branch", "-d", branch]).map(|_| ())
}

/// **Do the cleanup the state owns** — the one place the rule is acted on.
///
/// The operator's ask is *"worktree cleanup"*, and the answer differs by state for the reason
/// [`cleanup_for`] gives: a `Landed` entry's tree and branch are done with, and every other
/// state's tree is either the evidence or the branch still being merged. The pass routes
/// through this rather than inlining the removal, which is what makes the daemon's act and the
/// pane's reading one answer — and what makes a state added to the closed set reach a `match`
/// that must classify it.
///
/// A cleanup that failed is not reported here: a `git worktree remove` that failed leaves a tree
/// that is still there, which is a fact the row already describes, and a second failure mode
/// invented for the cleanup would be a second thing for a reader to hold.
fn clean_up(repo: &Path, entry: &MergeEntry, state: MergeState) {
    match cleanup_for(state) {
        Cleanup::RemoveWorktreeAndBranch => {
            if let Some(wt) = &entry.worktree {
                let _ = remove_worktree(repo, Path::new(wt));
            }
            let _ = delete_branch(repo, &entry.branch);
        }
        // **The tree stays, and that is the whole of this arm.** `Failed`, `Conflict` and
        // `Stale` are where the evidence is; `Waiting` and `Taken` are where the branch is
        // checked out and the rebase and the gate need it.
        Cleanup::KeepWorktree => {}
    }
}

/// **The git top level of `path`** — the repo a queue serves, from a path inside it.
///
/// The queue is about one repo and one `main`, so the repo is resolved once at startup
/// rather than guessed from a workspace path: `cfg.workspace` is a directory a session was
/// started in, and the repo whose main the queue fast-forwards is the one git names.
pub fn repo_root(path: &Path) -> Result<PathBuf, String> {
    let out = git(path, &["rev-parse", "--show-toplevel"])?;
    let top = out.trim();
    if top.is_empty() {
        return Err(format!("{path:?} is not inside a git repository"));
    }
    Ok(PathBuf::from(top))
}

// ===== The gate: the same checks CI runs, at the tip =====

/// **How much of a failing step's output goes on the row.** The evidence column is a
/// `String` a person reads, and a `cargo test --workspace --nocapture` that fails prints
/// megabytes of it; the failure itself is at the END, which is the part kept.
const EVIDENCE_BYTES: usize = 4_096;

/// **The last `cap` bytes of `text`, saying what was dropped.** A truncation that did not
/// say it truncated is the same lie a short queue read tells — *this is all there is* —
/// which is the rule the whole module is written around.
fn tail(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut start = text.len() - cap;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "… [{} byte(s) dropped from the front]\n{}",
        start,
        &text[start..]
    )
}

/// **One command, run in `dir`, with its output kept** — the gate's own words, whether it
/// passed or failed.
///
/// The environment is inherited deliberately: the gate is CI's own command line run on
/// this machine, and `LETIBOT_LLAMA_LIB` and the vocabulary path are facts about the box,
/// not something the queue should invent. `Ok` is the output of a step that passed (kept
/// for the caller's log) and `Err` is the output of one that did not, which is the
/// evidence the row carries.
fn run_captured(dir: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    run_captured_env(dir, &[], program, args)
}

/// [`run_captured`] with environment the caller needs to add.
///
/// A second function rather than a parameter on every call, because exactly one caller has any
/// environment to add — the gate, and it has one variable ([`GATE_TARGET_DIR`]).
fn run_captured_env(
    dir: &Path,
    env: &[(&str, &str)],
    program: &str,
    args: &[&str],
) -> Result<String, String> {
    let out = Command::new(program)
        .current_dir(dir)
        .envs(env.iter().copied())
        .args(args)
        .output()
        .map_err(|e| format!("{program} {}: {e}", args.join(" ")))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&err);
    }
    if out.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

/// **The root of the session tree `session` is in**, walking the stored parents — a branch is
/// usually finished by a subagent, and the session a person attaches to is its root. Bounded,
/// so a cycle written by a bug ends rather than spins.
pub fn root_session(store: &Store, session: &str) -> String {
    let mut cur = session.to_string();
    for _ in 0..32 {
        let parent = store
            .session(&cur)
            .ok()
            .flatten()
            .and_then(|s| s.parent_session_id);
        match parent {
            Some(p) if !p.is_empty() => cur = p,
            _ => break,
        }
    }
    cur
}

/// **The repository a worktree belongs to** — the directory holding its common `.git` — or
/// `None` for a path git does not know.
pub fn repo_of_worktree(worktree: &Path) -> Option<PathBuf> {
    let common = run_captured(
        worktree,
        "git",
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    let common = PathBuf::from(common.trim());
    if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf)
    } else {
        // A bare repository: the common dir is the repository.
        Some(common)
    }
}

/// **Whether a daemon serving `workspace` owns this entry** — [`MergeQueueDaemon::serves`]'s one
/// rule, as a function.
///
/// One store holds every daemon's queue, and each session is served by the daemon for its
/// workspace — so this is the predicate that keeps two daemons from reviewing, resuming or landing
/// the same entry. The root of the session that queued the entry is what is compared: an entry a
/// SUBAGENT finished belongs to its tree's root, which is the session a person attaches to.
///
/// **It is a free function because the gatekeeper session's board asks the same question.** The
/// daemon fills that board from the store ([`crate::sessions::gatekeeper`]) and a board carrying
/// another daemon's entries would be a session nagged about work it must not touch — so the board
/// reads this, and there is one spelling of the rule rather than two that can drift.
///
/// `repo` is the entry's repository, consulted only when the store has no row for the root session:
/// then where the entry's WORKTREE is decides it, and the caller is the one that can find that out
/// (`MergeQueueDaemon::repo_for` caches it; a caller that has no repository passes the workspace it
/// serves).
pub fn served_by(store: &Store, workspace: Option<&Path>, entry: &MergeEntry, repo: &Path) -> bool {
    let Some(ws) = workspace else {
        return true;
    };
    let root = root_session(store, &entry.session_id);
    match store.session(&root).ok().flatten() {
        Some(s) => {
            let at = Path::new(&s.workspace_root);
            std::fs::canonicalize(at).unwrap_or(at.to_path_buf()) == *ws
        }
        None => repo.starts_with(ws),
    }
}

/// **The repository's merge gate, as `main` says it is** — the steps of the `Merge gate`
/// section of `main`'s `AGENTS.md` ([`letibot_tools::gatekeeper::parse_merge_gate`]), or `None`
/// when there is no such file or section.
///
/// The operator, 2026-10-09: *"make the gate configurable per repo … a project has this gate
/// command, which must go to repo obviously"*, and *"then it becomes a section in agents.md"*.
/// The gate used to be letibot's own four CI commands for every repository, which no other
/// project can pass.
///
/// **Read from `main`, never from the branch.** A branch that rewrote its own gate would be
/// judged by the gate it wrote; `main`'s copy is the one every branch is held to, and a change
/// to the gate itself lands through the queue under the gate it replaces. `dir` is the repo or
/// any worktree of it — they share `main`.
pub fn gate_on_main(dir: &Path) -> Result<Option<Vec<String>>, String> {
    let names = run_captured(dir, "git", &["ls-tree", "--name-only", "main"])?;
    let Some(file) = names
        .lines()
        .find(|n| n.eq_ignore_ascii_case("agents.md"))
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let text = run_captured(dir, "git", &["show", &format!("main:{file}")])?;
    Ok(letibot_tools::gatekeeper::parse_merge_gate(&text))
}

/// **How an entry's row begins while its repository has no gate** — what the queue writes
/// ([`gate_configured`]) and what the project's session looks for to tell its model
/// (`Harness::queue_notices`).
pub const WAITING_FOR_GATE: &str = "waiting for a merge gate";

/// **Whether this repository has a gate yet** — the check the queue makes before it takes an
/// entry, so a repository with none HOLDS its branches (and says why on each row) rather than
/// failing them: once the section lands on `main`, the waiting entries go on by themselves.
pub fn gate_configured(repo: &Path) -> Result<(), String> {
    match gate_on_main(repo) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(format!(
            "{WAITING_FOR_GATE}: `main` has no `Merge gate` section in AGENTS.md at {}. \
             The project's agent can propose one (`merge_gate` lists what this repository \
             suggests) for the operator to choose; the entry goes on once it is on main.",
            repo.display()
        )),
        Err(e) => Err(format!("the merge gate could not be read from main: {e}")),
    }
}

/// **The clause that marks an entry the queue has handed to its own project** — what the fallback
/// writes on the row ([`no_gatekeeper_evidence`]), what stops it being told twice ([`handed_over`]),
/// and what the project's own session looks for to find the message it was rung for
/// (`Harness::handover_notices`).
///
/// A clause rather than a column, because the row's own words ARE the queue's evidence — the store
/// has one `evidence` string per entry and that is where every other state's reason lives — and
/// because the readers are two: the daemon, deciding whether it has already told anybody, and the
/// project's session, deciding whether a message is for it. **A phrase and not a flag**, so that a
/// person reading the pane sees the same sentence the two readers key on.
///
/// **It does not lead the row, and that is deliberate.** A failure's own words go first — the pane
/// truncates the row at its width, and `http 429: …` is what a person needs to see — so this is
/// matched with `contains` rather than `starts_with` ([`WAITING_FOR_GATE`] is the other way round
/// because nothing else can precede it). The clause is phrased so that no other sentence in this
/// module can produce it.
pub const NO_GATEKEEPER: &str = "the entry is handed to the project it came from";

/// **The clause that marks an entry the queue has handed to its own project because the review's
/// REFUSAL could not be delivered** — the second kind of handover, and the reason there are two
/// clauses rather than one.
///
/// The shape is the operator's and the trigger is not: *"even if gatekeeper session dies or didnt
/// start - the queue tooling can send a message back to the main project session and projects will
/// gate and merge on their own"* is about a verdict that never comes, and this is a verdict that
/// came and could not be handed to the child that has to act on it. What the two share is the
/// answer — the entry is parked with nobody in the queue able to move it, and the session that
/// owns the worktree is the one that can — and what they must NOT share is the message: telling a
/// project to gate and land a branch a review REFUSED is the one instruction this whole queue
/// exists to make impossible. So the kind is on the row ([`handover_message`] is where the two are
/// told apart) and the two sentences are two functions.
pub const REFUSAL_UNDELIVERED: &str = "the review's complaints did not reach the child";

/// **Whether the queue has handed an entry to its own project** — both clauses, so the marker has
/// one spelling per KIND and the readers ask one question: the daemon (deciding whether it has
/// already told anybody, and whether main may have the branch anyway) and the project's own
/// session (deciding whether a message is for it).
pub fn handed_over(entry: &MergeEntry) -> bool {
    entry.evidence.contains(NO_GATEKEEPER) || entry.evidence.contains(REFUSAL_UNDELIVERED)
}

/// **The entries this session is the project OF, which the queue has handed over** — the filter
/// `Harness::handover_notices` applies, as a function so the rule can be asserted without a
/// harness, a model or a vocabulary file.
///
/// *This session is the project of* is [`root_session`]'s walk up the stored parents, which is the
/// same rule the queue's own door uses to decide who to ring — one question, one answer, or the
/// message and the ring could reach two different sessions.
pub fn handed_over_to(store: &Store, me: &str) -> Vec<MergeEntry> {
    store
        .merge_entries()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| handed_over(e) && root_session(store, &e.session_id) == me)
        .collect()
}

/// **How long a gatekeeper has to exist before the queue believes none is coming** — three review
/// passes ([`crate::sessions::REVIEW_PASS_EVERY`], which is the clock that opens the review queue's
/// session and the clock that serves its reviews).
///
/// # Why a grace at all, and not *is the session there this instant*
///
/// Because *not open YET* and *never started* are different facts and the same observation. The
/// merge queue is a thread of its own and the session that hosts reviews is opened by the daemon's
/// WORKER on its first idle pass, so there is a real window — milliseconds wide, and it happens on
/// every start — in which the queue's first ask arrives before the session that will serve it
/// exists. Read without a grace, that window is a false alarm: every daemon start with an entry in
/// the queue would tell that entry's project that no gatekeeper can judge its branch.
///
/// Three passes is the smallest number that is honest about it: one pass is the clock that opens
/// the session, and two more cover a worker that is busy running a turn (the review pass is served
/// at the round boundary and on the idle arm, so a long turn delays it by at most one boundary).
/// After that, a session that still does not exist is one nothing is going to open, and the entry
/// has been waiting long enough that telling its project is the right thing to do.
pub const GATEKEEPER_GRACE_MS: u64 = 3 * crate::sessions::REVIEW_PASS_SECS * 1_000;

/// **Why the queue has no gatekeeper to get a verdict from** — the three ways a verdict does not
/// come, and the one answer the queue gives all three: **tell the project that owns the branch**.
///
/// The operator's argument, in their words: *"even if gatekeeper session dies or didnt start - the
/// queue tooling can send a message back to the main project session and projects will gate and
/// merge on their own"*. The three are named here because each is SEEN somewhere else, and the
/// code should say which:
///
/// * **Never started** — the review queue's session is in neither the daemon's registry nor the
///   store, and has not been for [`GATEKEEPER_GRACE_MS`]. Only the door can see this
///   ([`letibot_tools::gatekeeper::Reviewer::gatekeeper_ready`], which is asked before the verdict
///   is), and the grace is what keeps *not open yet* out of it.
/// * **Died** — an attempt was made and no verdict came out of it, [`MAX_REVIEW_ATTEMPTS`] times:
///   the queue's own evidence ([`ReviewRetry::Exhausted`]), and the case that used to end at
///   *a person has to restart it*.
/// * **An ask that failed** — `wake` came back `Err` for any other reason (the request row could
///   not be written, the daemon's bell is closed). The queue asks again by itself on the next pass;
///   the project is told in the same breath, because an entry that waits while the queue retries is
///   an entry nobody outside the store can see.
///
/// Each variant carries the queue's own words for it — the reason a row leads with — because the
/// three sentences come from three places (`review_exhausted_evidence`, the door's refusal, the
/// wake's error) and re-deriving them here would be a second spelling of each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoGatekeeper {
    /// **Nobody was ever going to judge it** — see [`GATEKEEPER_GRACE_MS`]. The sentence is the
    /// door's own (`Reviewer::gatekeeper_ready`'s `Err`).
    NeverStarted(String),
    /// **It was asked and it died** — the failure's own words, as [`review_exhausted_evidence`]
    /// writes them on the row.
    Died(String),
    /// **The ask itself failed** — the queue's sentence for a `wake` that came back `Err`.
    AskFailed(String),
}

impl NoGatekeeper {
    /// **The queue's own words for what happened** — the half the entry's row leads with.
    pub fn why(&self) -> &str {
        match self {
            NoGatekeeper::NeverStarted(why)
            | NoGatekeeper::Died(why)
            | NoGatekeeper::AskFailed(why) => why,
        }
    }

    /// **The state the handover leaves the row in.**
    ///
    /// `Failed` for the two where the queue has STOPPED asking: nothing will judge the branch
    /// until a person restarts the review or the project lands it itself, and `Failed` is the
    /// state the person's verbs move ([`parked`]) — an entry left `Waiting` there would be one
    /// `/queue restart` refuses, which is the opposite of a last resort.
    ///
    /// `Waiting` for an ask that failed, and that is the whole difference between this case and
    /// the other two: the queue tries again by itself on the next pass, and parking an entry for a
    /// provider hiccup or a closed bell would be the queue giving up on a retry it already does.
    pub fn state(&self) -> MergeState {
        match self {
            NoGatekeeper::NeverStarted(_) | NoGatekeeper::Died(_) => MergeState::Failed,
            NoGatekeeper::AskFailed(_) => MergeState::Waiting,
        }
    }
}

/// **The one variable that keeps a gate's build out of the main tree's `target/`.**
///
/// `task_start` writes `<worktree>/.cargo/config.toml` with `[build] target-dir` pointed at the
/// MAIN tree's `target/`, so a child does not pay a cold build of the whole graph — see
/// [`BUILD_CACHE_REDIRECT`], which the sweep keeps out of every commit for the same reason. That
/// is right for a child, and it is wrong for the gate: a shared target dir serves artifacts built
/// from other sources, and the gate then fails a branch for a defect the branch does not have.
///
/// MEASURED 2026-10-09, the entry that made this visible: `agent/queue-reset` (`424c212`) was
/// parked `failed` on
///
/// ```text
/// error[E0599]: no associated function or constant named `RESET_EVIDENCE` found for
///               struct `letibot_tokencore::Store      --> crates/harnessd/src/mergequeue.rs:5922
/// ```
///
/// while the constant is defined in that worktree at `crates/tokencore/src/store.rs:3115` (the
/// file's first `#[cfg(test)]` is at 3754, so it is in the main body of `impl Store`) and a direct
/// `cargo check -p letibot-harnessd --all-targets` **in that worktree** is clean (`rc=0`, having
/// recompiled `letibot-tokencore` from the worktree's own sources). A second child hit the same
/// class: a `harnessd` test build failed on `plan_fold takes 3 arguments but 4 were supplied` in a
/// file it never touched, naming a change that exists only in the *main* tree. And **a green out of
/// that build is worth as little as a red**: a third child, `agent/agents-md`, met *missing field
/// `slug` in initializer of `SpawnRequest`* in a worktree whose own `crates/tools` — and `main`'s —
/// has no such field, because the artifact it reused was built from ANOTHER worktree's sources.
///
/// **And the same dir lies in the other direction, where the green is the false one.** MEASURED by
/// a gatekeeper, 2026-10-10, on another branch, twice: the shared `target/` served a
/// `letibot-tools` test binary that contained NONE of that branch's five new tests —
/// `cargo test -- --list` listed 835 names, none of them theirs, and the run said `ok` — and a
/// `letibot-turn` rmeta older than the branch made `harnessd` fail to compile against a file the
/// change never touches. Both are PATH DEPENDENCIES, so `cargo clean -p <crate>` is not the fix:
/// the next build in the shared dir mixes the trees again. A `CARGO_TARGET_DIR` per worktree is,
/// because the whole graph is built there — every unit the gate compiles, dependencies included,
/// comes from this worktree's own sources.
///
/// `CARGO_TARGET_DIR` is the general mechanism because it is inert for a repository whose gate is
/// not cargo, and it is an OVERRIDE rather than a suggestion: an environment variable beats a
/// config file in cargo's own precedence, which is what makes it able to answer a
/// `[build] target-dir` the tree already carries. The redirect itself is not touched — it stays
/// for the children it was written for; it is the gate that must not be fooled by it.
///
/// The cost is that an entry pays a cold build instead of reusing the main tree's artifacts, and
/// that is the price of the gate meaning anything. The queue is serial, so it is one cold build
/// at a time.
///
/// **And the cost has a second half that nothing said out loud: a gate's steps are PROOF and not
/// INSTALLATION.** Because the build lands in `<worktree>/target/release`, the directory the
/// LAUNCHER starts from — `~/bin/letibot` → `scripts/letibot`, whose `BIN` is the MAIN tree's
/// `target/release`, exec'ing `$BIN/harnessd` and `$BIN/letibot-tui` — is left exactly as it was,
/// whatever the gate proved. So a gate whose last step is only a build is a gate whose landings
/// are never installed. MEASURED 2026-10-10: a daemon came up at 20:55 running code from 17:44 —
/// three hours and seventeen landings behind — and restarting it could not help, because the file
/// it execs had not been rebuilt since. letibot's own gate therefore ends with
/// `sh scripts/install-gate-build.sh` (`AGENTS.md`'s `Merge gate`, the copy the queue reads),
/// which copies what this directory holds into the main tree's `target/release`. Two paths, and a
/// reader who takes them for one is the mistake this paragraph exists to prevent.
pub const GATE_TARGET_DIR: &str = "CARGO_TARGET_DIR";

/// **Where a gate's build must put its artifacts** — inside the worktree being judged, never the
/// main tree's `target/`. See [`GATE_TARGET_DIR`] for the measurement this answers.
///
/// Absolute where it can be, because a relative `CARGO_TARGET_DIR` is resolved by cargo against
/// the gate's own working directory and the entry's worktree is whatever the daemon's workspace
/// was spelled as. A path that cannot be made absolute is still this worktree's own, which is the
/// property that matters.
pub fn gate_target_dir(worktree: &Path) -> PathBuf {
    std::path::absolute(worktree)
        .unwrap_or_else(|_| worktree.to_path_buf())
        .join("target")
}

/// **Run the gate** — `main`'s steps, in order, each through `sh -c` in the entry's worktree
/// after the rebase. The first that fails ends the run, and every step the gate declared leaves
/// a row: the ones that ran with their outcome, the tail of their output and their timing, and
/// the ones after a red step marked [`MergeGateOutcome::NotRun`] rather than passed. There is no
/// `--continue`, because a gate that ran everything after a red step would spend minutes to say
/// what the first line already said — and the rows say the rest did not run, which is the fact a
/// reader needs and a shortened list would not give.
///
/// **And the build is pointed inside that worktree** ([`GATE_TARGET_DIR`]), because the entry's
/// own `.cargo/config.toml` points it at the main tree's `target/` and the artifacts there were
/// built from sources that are not this branch's. A gate that judged a branch on another tree's
/// build is a gate that is red for every branch, whatever the branch does.
///
/// **A repository with no gate is a row and not an empty list.** `main` without a `Merge gate`
/// section yields the one [`MergeGateOutcome::NoGate`] row ([`GateRun::no_gate`]) carrying the
/// queue's own sentence, because the empty list is the entry's word for *the gate has not run*
/// and a blank checklist would read as *every step passed* — the one reading that must not be
/// available for a repository nobody has configured.
pub fn repo_gate(worktree: &Path) -> GateRun {
    let steps = match gate_on_main(worktree) {
        Ok(Some(steps)) => steps,
        Ok(None) => {
            return GateRun::no_gate(
                "`main` has no `Merge gate` section in AGENTS.md, so there is no step to run",
            );
        }
        Err(e) => {
            return GateRun::no_gate(&format!("the merge gate could not be read from main: {e}"));
        }
    };
    let target = gate_target_dir(worktree).to_string_lossy().into_owned();
    let env = [(GATE_TARGET_DIR, target.as_str())];
    let mut rows: Vec<MergeGateStep> = Vec::with_capacity(steps.len());
    for (i, step) in steps.iter().enumerate() {
        let started_ms = gate_now_ms();
        let out = run_captured_env(worktree, &env, "sh", &["-c", step]);
        let elapsed_ms = gate_now_ms().saturating_sub(started_ms);
        match out {
            Ok(text) => rows.push(step_row(
                step,
                MergeGateOutcome::Passed,
                &text,
                started_ms,
                elapsed_ms,
            )),
            Err(text) => {
                rows.push(step_row(
                    step,
                    MergeGateOutcome::Failed,
                    &text,
                    started_ms,
                    elapsed_ms,
                ));
                // **The steps the gate never reached are on the entry too, and marked.** A list
                // that stopped at the red step would leave a reader unable to tell *the gate had
                // three steps and the third was never tried* from *the gate had two steps and
                // both were green* — the two draw identically and only one of them is the truth.
                rows.extend(steps[i + 1..].iter().map(|s| MergeGateStep {
                    command: s.clone(),
                    outcome: MergeGateOutcome::NotRun,
                    // Empty rather than a placeholder sentence: the outcome word is the fact, and
                    // prose here would be this code writing a step's output for it.
                    output: String::new(),
                    started_ms: 0,
                    elapsed_ms: 0,
                }));
                return GateRun {
                    steps: rows,
                    failure: Some(format!("`{step}` failed:\n{}", tail(&text, EVIDENCE_BYTES))),
                };
            }
        }
    }
    GateRun {
        steps: rows,
        failure: None,
    }
}

/// **The clock the gate's rows are stamped with**, Unix ms.
///
/// The same reading the daemon's pass takes (`crate::config::now_ns`), so a row's `started_ms`
/// and the move that carries it are on one clock and a reader can subtract them.
fn gate_now_ms() -> u64 {
    (crate::config::now_ns() / 1_000_000) as u64
}

/// **One step's row**, with the same output cap the evidence uses: the failure is at the END of
/// a failing step's output, and a passing step's output is kept for the same reason the failing
/// one's is — a row a person reads should be the step's own words and not a summary of them.
fn step_row(
    command: &str,
    outcome: MergeGateOutcome,
    output: &str,
    started_ms: u64,
    elapsed_ms: u64,
) -> MergeGateStep {
    MergeGateStep {
        command: command.to_string(),
        outcome,
        output: tail(output, EVIDENCE_BYTES),
        started_ms,
        elapsed_ms,
    }
}

/// **What one run of the gate leaves behind** — the rows a landing records, and the failure's own
/// words when the run was red.
///
/// The two are one value rather than a `Result` because they are not alternatives: a red run has
/// BOTH its rows (the steps that ran and the ones it never reached) and the sentence the entry's
/// `evidence` carries. A `Result<Vec<MergeGateStep>, String>` would make the rows something a
/// caller could drop on the failing path, which is the defect this whole change is about.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GateRun {
    /// One row per step `main` declared, in order — see [`repo_gate`].
    pub steps: Vec<MergeGateStep>,
    /// The gate's own words when it is red, or `None` when it is green.
    pub failure: Option<String>,
}

impl GateRun {
    /// **A green run of a gate that declared nothing** — the test's no-op gate, and the honest
    /// row set for a gate that ran no step at all.
    pub fn green() -> Self {
        Self::default()
    }

    /// **A repository with no gate, as one row.** The command is empty because there was no
    /// command to name, and `reason` is the queue's own sentence — see [`MergeGateOutcome::NoGate`]
    /// for why this is a row and not an empty list.
    pub fn no_gate(reason: &str) -> Self {
        Self {
            steps: vec![MergeGateStep {
                command: String::new(),
                outcome: MergeGateOutcome::NoGate,
                output: reason.to_string(),
                started_ms: gate_now_ms(),
                elapsed_ms: 0,
            }],
            failure: Some(reason.to_string()),
        }
    }
}

// ===== The wire: the store's row as a head reads it =====

/// **One entry, as the wire spells it** — the conversion the two copies of the types exist
/// for.
///
/// The priority and the state are `match`es and not casts, and that is the whole of the
/// argument for the wire's types being copies rather than re-exports: a rung or a state
/// added on either side fails to compile HERE, where the two vocabularies meet, instead of
/// arriving at a head as a word nobody can order or draw.
pub fn wire_entry(entry: &MergeEntry) -> letibot_sessionlog::event::MergeEntry {
    use letibot_sessionlog::event as wire;
    wire::MergeEntry {
        id: entry.id.clone(),
        session_id: entry.session_id.clone(),
        branch: entry.branch.clone(),
        base_sha: entry.base_sha.clone(),
        priority: match entry.priority {
            MergePriority::Urgent => wire::MergePriority::Urgent,
            MergePriority::Subagent => wire::MergePriority::Subagent,
        },
        needs: entry.needs.clone(),
        state: wire_state(entry.state),
        // **The ask travels with the entry.** The reviewer reads it, and a head that draws the
        // queue can show what a row was for without a second read of the child's session —
        // which may be gone by the time anybody asks.
        brief: entry.brief.clone(),
        evidence: entry.evidence.clone(),
        created_ms: entry.created_ms,
        updated_ms: entry.updated_ms,
        worktree: entry.worktree.clone(),
        landed_sha: entry.landed_sha.clone(),
        gate_steps: entry.gate_steps.iter().map(wire_gate_step).collect(),
    }
}

/// **One gate step, as the wire spells it** — the same `match` [`wire_entry`] makes on the
/// states, on its own so the copy of the closed set is one place. A word added on either side
/// fails to compile HERE rather than arriving at a head as an outcome nobody can draw.
pub fn wire_gate_step(step: &MergeGateStep) -> letibot_sessionlog::event::MergeGateStep {
    use letibot_sessionlog::event as wire;
    wire::MergeGateStep {
        command: step.command.clone(),
        outcome: match step.outcome {
            MergeGateOutcome::Passed => wire::MergeGateOutcome::Passed,
            MergeGateOutcome::Failed => wire::MergeGateOutcome::Failed,
            MergeGateOutcome::NotRun => wire::MergeGateOutcome::NotRun,
            MergeGateOutcome::NoGate => wire::MergeGateOutcome::NoGate,
        },
        output: step.output.clone(),
        started_ms: step.started_ms,
        elapsed_ms: step.elapsed_ms,
    }
}

/// **The whole queue, as the wire spells it** — the snapshot a head's `ListMergeQueue` is
/// answered with, every state, in the order the queue was filled.
///
/// The read that answers *what is the queue* must not drop a row it cannot act on, which
/// is why this is a map over the store's own read rather than a filter: a `Failed` entry is
/// listed with its reason, and so is a `Stale` one.
pub fn wire_queue(entries: &[MergeEntry]) -> Vec<letibot_sessionlog::event::MergeEntry> {
    entries.iter().map(wire_entry).collect()
}

/// **One state, as the wire spells it** — the same `match` [`wire_entry`] makes, on its own so
/// a `MergeEntryMoved` (which carries a state and not a whole entry) goes through the same
/// door. A state added on either side fails to compile HERE, which is the whole reason the two
/// vocabularies are two.
pub fn wire_state(state: MergeState) -> letibot_sessionlog::event::MergeState {
    use letibot_sessionlog::event as wire;
    match state {
        MergeState::Waiting => wire::MergeState::Waiting,
        MergeState::Taken => wire::MergeState::Taken,
        MergeState::Landed => wire::MergeState::Landed,
        MergeState::Failed => wire::MergeState::Failed,
        MergeState::Conflict => wire::MergeState::Conflict,
        MergeState::Stale => wire::MergeState::Stale,
        MergeState::Vetoed => wire::MergeState::Vetoed,
    }
}

/// **One verdict, as the wire spells it** — the pane's read, and a copy rather than a re-export
/// for the reason [`wire_entry`] is one: the head draws the reviewer's answer and must not be
/// able to write one.
pub fn wire_review(
    rec: &letibot_tokencore::store::ReviewRecord,
) -> letibot_sessionlog::event::MergeReview {
    letibot_sessionlog::event::MergeReview {
        // **The attempt's failure, which is not a verdict.** The head draws `no verdict` rather
        // than `asked and has not answered` when this is set — see the field's own doc — which
        // is what makes a dead attempt legible on the pane without opening the store.
        failure: rec.failure.clone(),
        entry_id: rec.entry_id.clone(),
        session_id: rec.session_id.clone(),
        branch: rec.branch.clone(),
        base_sha: rec.base_sha.clone(),
        asked_ms: rec.asked_ms,
        answered_ms: rec.answered_ms,
        // **The word travels as the word.** The closed set is the gatekeeper's, and a head
        // that rendered an unknown word as *rejected* would be inventing a decision — the pane
        // prints what is on the row and lets a reader see that it is not one of the three.
        decision: rec.decision.clone(),
        reasons: rec.reasons.clone(),
        files: rec.files.clone(),
        commands: rec.commands.clone(),
    }
}

/// **Every verdict, as the wire spells them** — beside [`wire_queue`] because the pane reads
/// the two together: a queue without its verdicts cannot say why an entry is parked.
pub fn wire_reviews(
    recs: &[letibot_tokencore::store::ReviewRecord],
) -> Vec<letibot_sessionlog::event::MergeReview> {
    recs.iter().map(wire_review).collect()
}

// ===== The enqueue: what a finished `task_start` child leaves behind =====

/// **The entry a finished `task_start` child leaves for the queue** — the row, built from the
/// three things only the runner knows: the ask the child was given, the placement it worked
/// in, and the id it was minted under.
///
/// This is the enqueuer's half of the operator's ask — *"a permanent subagent that lazily
/// starts as soon as task_start was finished and item put into the queue"* — and it is a pure
/// function of its arguments for the reason the queue's own core is: the row is a decision
/// about five fields, and a decision made inside a thread that also writes to a store and
/// rings a bell is a decision nobody can test.
///
/// **The id is the child's own handle, and that is what makes the enqueue idempotent.** A
/// `task_start` child's handle is already unique for ever (`mint_sub_id` mints it against the
/// registry and the store), so *one entry per finished child* is true by construction rather
/// than by a flag: the second enqueue of the same child is the same row, upserted. The queue
/// would otherwise need a "have I already asked" cell, which is the shape that goes wrong
/// across a daemon restart.
///
/// **Priority is `Subagent`, and there is no second answer yet.** The two rungs are the
/// operator's (`Urgent`) and a subagent's, and a child of `task_start` is a subagent's work by
/// definition. An operator's own entry is a door nobody has built, and inventing a rung for it
/// here would be inventing the door.
///
/// **`needs` is empty, and that is a fact about the tool rather than a decision here.**
/// `task_start` takes no dependency argument: a child is started, finishes, and its branch is
/// enqueued, and nothing in the tool layer can name another entry it waits behind. The field
/// is carried — the row has it, the queue's scheduler reads it, and an operator's entry (or a
/// later `task_start` argument) fills it — and a `task_start` child's list is empty.
///
/// **The main tree is not an entry.** A `task_start` with `main_tree: true` works in the main
/// checkout on main's own branch, so there is no branch to land and nothing for the queue to
/// do: the caller is told by name rather than handed an entry whose branch is `main`.
pub fn entry_for_finished(
    id: &str,
    session_id: &str,
    brief: &str,
    placement: &letibot_tools::builtins::task::WorktreePlacement,
    now_ms: u64,
) -> Option<MergeEntry> {
    if placement.main_tree {
        return None;
    }
    Some(MergeEntry {
        id: id.to_string(),
        // **The child that finished the branch**, which is what the column documents itself as
        // being — the entry's origin rather than the parent that enqueued it.
        session_id: session_id.to_string(),
        branch: placement.branch.clone(),
        base_sha: placement.base_sha.clone(),
        priority: MergePriority::Subagent,
        needs: Vec::new(),
        state: MergeState::Waiting,
        // **The ask, verbatim.** This is the whole reason the entry carries a brief: the
        // reviewer is given it and NOT the child's report, so an entry that dropped it would
        // be an entry nobody could review.
        brief: brief.to_string(),
        // **Empty, and it means what it says.** The entry is waiting on nothing but its own
        // turn: the queue's `evidence` is where a reason lives, and "the worktree is not there
        // yet" or "the review is not in" is written by whoever finds it out.
        evidence: String::new(),
        created_ms: now_ms,
        updated_ms: now_ms,
        worktree: Some(placement.path.clone()),
        landed_sha: None,
        // **No rows, and it means *the gate has not run on this entry yet*.** The landing half
        // writes them ([`MergeGateStep`]) as the gate runs; an enqueued entry has been gated by
        // nothing.
        gate_steps: Vec::new(),
    })
}

// ===== The daemon thread: the thing that takes the next entry =====

/// **What one pass of the daemon did** — the vocabulary the loop and the test share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// Nothing was ready: no `Waiting` entry whose `needs` have all `Landed`.
    Idle,
    /// The entry was taken and landed: main was fast-forwarded, pushed, and the worktree and
    /// branch were removed. The tip is the SHA main moved to — or, for an entry whose branch was
    /// **already** on main, the tip main already stood at, since the queue landed the row without
    /// moving anything (see [`branch_standing`]).
    Landed(String),
    /// The entry was taken and the rebase conflicted: the worktree stays, with git's words on
    /// the row.
    Conflict,
    /// The entry was taken and the gate failed: the worktree stays, with the gate's words on
    /// the row. **It is also what a branch that carries nothing lands as** — 0 commits over its
    /// base is never `Landed` ([`branch_standing`]), because the row would then say main has work
    /// that main has never seen — with the reason on the row and the worktree, which is where the
    /// uncommitted work is, kept.
    Failed,
    /// **An entry is ready and its review is not in yet.** The reviewer was asked (again) and
    /// nothing was taken: the queue does not land a branch nobody reviewed, and it does not
    /// guess at a verdict that has not come back. The entry is listed with its ask on the row,
    /// which is what a person watching the queue needs to know.
    AwaitingReview,
    /// **An entry is ready and the repository has no gate** — it waits, with the reason on its
    /// row, until `main` has one ([`gate_configured`]).
    AwaitingGate,
    /// **The reviewer refused the entry, so it did not land.** The row is `Failed` with the
    /// verdict's own words on it and the worktree stays, which is the same shape a failed gate
    /// takes and for the same reason: the tree is where the reason is.
    Refused,
    /// **The reviewer could not be asked and the queue's attempts are spent.** The row is
    /// `Failed` with the failure's own words on it — the provider's, verbatim, so the row says
    /// *429* rather than *failed* — and the worktree stays. Not [`Self::Refused`]: nobody judged
    /// this branch, and a person reading the queue must not be able to mistake one for the
    /// other. [`restart`] is what moves it from here.
    ReviewGaveUp,
    /// **No gatekeeper exists to judge an entry, and the project that owns its branch was told** —
    /// the never-started case of the fallback ([`NoGatekeeper::NeverStarted`]), which is the one
    /// arm of the ask loop where nothing is asked at all. Not [`Self::Idle`]: an entry WAS ready
    /// and the queue did something about it, and *nothing was ready* is the one sentence this
    /// vocabulary must not say falsely. Not [`Self::ReviewGaveUp`] either: no attempt was spent,
    /// because there was nobody to make one.
    NoGatekeeper,
}

/// **The entry's review, as the queue's gate reads it** — the three states a decision can be
/// in, and the reason there are three rather than two.
///
/// *No verdict yet* and *a refusing verdict* are different facts and the queue does different
/// things with them: the first is waited on (and asked for), and the second parks the entry
/// with the reason on its row. Collapsing them would either land a branch nobody reviewed or
/// park one nobody refused — which is why the gate is a function of the review record rather
/// than a `bool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewGate {
    /// Nobody has asked, or the verdict has not come back. The queue waits.
    Awaiting,
    /// The verdict is in and it is not `accept`. This is the verdict's own rendering, which is
    /// what goes on the row: a refusal whose reason is not carried forward is a row a person
    /// cannot act on.
    Refused(String),
    /// The verdict is in and it accepts. This is the only way an entry is taken.
    Accepted,
}

/// **Whether an entry may be taken, by the reviewer's own word** — the gate, as one pure
/// function of the entry and its review record.
///
/// **Absent and unanswered are both `Awaiting`, and neither is an accept.** The operator's
/// ask is a *gated* merge, and the whole point of a gate is that the door does not open by
/// default: a queue that landed an entry nobody had reviewed would be a queue whose gate is
/// decoration. So `None` — no review row at all — waits, and so does a row whose `decision`
/// is `None`.
///
/// **A decision word outside the closed set is a refusal, not an accept.** A row written by a
/// build this one does not know is a row whose verdict cannot be acted on, and reading it as
/// permission would land a branch on the strength of a word nobody can interpret.
pub fn review_gate(
    entry: &MergeEntry,
    review: Option<&letibot_tokencore::store::ReviewRecord>,
) -> ReviewGate {
    use letibot_tools::gatekeeper::{Decision, Verdict};
    let Some(rec) = review else {
        return ReviewGate::Awaiting;
    };
    let Some(word) = rec.decision.as_deref() else {
        return ReviewGate::Awaiting;
    };
    match Decision::parse(word) {
        Some(Decision::Accept) => ReviewGate::Accepted,
        // **The refusal is rendered as the verdict the reviewer gave**, through the same
        // `Verdict` type and the same `render` the reviewer's own session would print — so the
        // row, the pane and the reviewer's log say one thing rather than three paraphrases of
        // it. The branch and base come from the entry, which is where the request's did.
        Some(other) => ReviewGate::Refused(
            Verdict {
                branch: entry.branch.clone(),
                base_sha: entry.base_sha.clone(),
                decision: other,
                reasons: rec.reasons.clone(),
                looked_at: letibot_tools::gatekeeper::LookedAt {
                    files: rec.files.clone(),
                    commands: rec.commands.clone(),
                },
            }
            .render(),
        ),
        None => ReviewGate::Refused(format!(
            "the reviewer's verdict on `{}` is `{word}`, which is not one of {}. A word \
             outside the closed set is not a verdict, so the branch does not land.",
            entry.branch,
            Decision::ALL
                .iter()
                .map(|d| d.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// **The next entry the daemon may actually take**, by index — the ready entries whose review
/// accepts, ordered exactly as [`next_ready`] orders the ready set.
///
/// Separate from [`next_ready`] because the two answer different questions and the queue needs
/// both: `next_ready` is *what is due*, which is what the reviewer is asked about, and this is
/// *what may land*. An entry waiting on a verdict must not hold the queue up — the review is
/// minutes of a model's work and the queue is serial about the GATE, not about the review —
/// so an awaiting entry is skipped and the next accepted one is taken.
pub fn next_takeable(
    entries: &[MergeEntry],
    reviews: &[letibot_tokencore::store::ReviewRecord],
) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.state == MergeState::Waiting && is_ready(e, entries))
        .filter(|(_, e)| {
            let review = reviews.iter().find(|r| r.entry_id == e.id);
            review_gate(e, review) == ReviewGate::Accepted
        })
        .min_by_key(|(_, e)| (e.priority.rank(), e.created_ms, e.id.clone()))
        .map(|(i, _)| i)
}

// ===== One failure, two questions: may the queue ask again, and may a person =====

/// **How many attempts an entry's review gets before the queue stops asking.**
///
/// The operator's report, verbatim: *"so merge queue has 4 failed items, we need a way to
/// restart them"* — and the half of that which must not need a person is the ordinary case,
/// where the reviewer's turn failed for a reason that has already gone away. So the queue asks
/// again, and this is the ceiling on that: three attempts, which is `http_retries`' discipline
/// (a small number, a wait that grows, and then a stop) rather than a second policy invented
/// here.
///
/// **It is deliberately not `MAX_HTTP_RETRIES`.** That ladder is the *turn's*, and one beat of
/// it is a retry inside one model call; this one is a whole reviewer, a child session and a
/// turn of its own per attempt, so the same number would mean something three orders of
/// magnitude larger. What is borrowed is the SHAPE — bounded, doubling, and then a report —
/// which is the thing the operator asked for.
pub const MAX_REVIEW_ATTEMPTS: u32 = 3;

/// **The wait after the first failed attempt, in ms; it doubles per attempt.**
///
/// Thirty seconds and then a minute: about ninety seconds of trying before the entry parks
/// with the failure on its row. Long enough that a provider hiccup, a child that lost its
/// first byte or a spawn that raced the registry is over by the second attempt, and short
/// enough that a reviewer which cannot be started at all is *reported* rather than waited on —
/// the direction [`review_gate`] and `http_retry_after` both fail in.
pub const REVIEW_RETRY_BASE_MS: u64 = 30_000;

/// **Whether an entry's review may be attempted again, and when** — one answer, read by both
/// halves of the review: the queue's pass (which writes the request and rings the queue's session)
/// and that session itself (which starts the reviewer as its own child).
///
/// One function rather than two rules, because the two halves are the same decision one hop
/// apart, and the failure mode of a second rule is the one this defect already produced: the
/// queue rings, the session spawns, and neither of them is the place the *bound* lives. It is a
/// pure function of the review row and the clock, so it is asserted without a daemon, a child
/// or a provider.
///
/// The three answers, and what each is for:
///
/// * **`Due`** — nobody has asked, or an attempt is IN FLIGHT. The second is not a retry and
///   must not be read as one: the queue's re-ask on every pass is the recovery for a daemon
///   that came up between the request row and the bell (`GatekeeperDoor::wake`), and it is
///   cheap by construction because the door writes the row only when there is not one and a
///   ring at the queue's session with nothing new answers `Ok(None)`.
/// * **`Wait`** — the last attempt FAILED and the backoff has not elapsed. Nothing is asked
///   and nothing is spawned; the entry says so on its row.
/// * **`Exhausted`** — the attempts are used up. The queue parks the entry with the failure
///   on it and stops; only a person starts it again ([`restart`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewRetry {
    /// An attempt may be made now.
    Due,
    /// The last attempt failed and the wait has this much left.
    Wait { in_ms: u64 },
    /// The attempts are used up: a person has to restart it.
    Exhausted,
}

/// See [`ReviewRetry`] — the decision, as one pure function of the review row and the clock.
pub fn review_retry(
    review: Option<&letibot_tokencore::store::ReviewRecord>,
    now_ms: u64,
) -> ReviewRetry {
    let Some(rec) = review else {
        return ReviewRetry::Due;
    };
    // **No failure on the row: nobody has asked, or an attempt is in flight.** Both mean *ask*,
    // and the second is why this is not simply *`failed_ms` is set*: an outstanding review is
    // re-asked every pass on purpose.
    let Some(failed_ms) = rec.failed_ms else {
        return ReviewRetry::Due;
    };
    if rec.attempts >= MAX_REVIEW_ATTEMPTS {
        return ReviewRetry::Exhausted;
    }
    // `1 << attempt`, `http_retry_after`'s own shape: the first wait is the base, the second is
    // twice it. `saturating_sub(1)` because `attempts` is *how many failed*, so one failure
    // means the first rung — and the shift is capped so a row written by something that counted
    // differently cannot ask for a wait no clock can hold.
    let wait = REVIEW_RETRY_BASE_MS << rec.attempts.saturating_sub(1).min(6);
    let elapsed = now_ms.saturating_sub(failed_ms);
    if elapsed >= wait {
        ReviewRetry::Due
    } else {
        ReviewRetry::Wait {
            in_ms: wait - elapsed,
        }
    }
}

/// **The four states a re-review may move** — `failed`, `conflict`, `stale` and `vetoed`, which
/// are the same four words [`Store::restart_review`] and [`Store::reset_reviews`] spell out in
/// their `WHERE` clauses.
///
/// One predicate rather than a `matches!` per caller, so a state added to [`MergeState`] is
/// classified in one place instead of being silently left out of a whole-queue reset — and so
/// the two sweeps cannot disagree about which rows they are about.
///
/// [`Store::restart_review`]: letibot_tokencore::store::Store::restart_review
/// [`Store::reset_reviews`]: letibot_tokencore::store::Store::reset_reviews
pub fn parked(state: MergeState) -> bool {
    matches!(
        state,
        MergeState::Failed | MergeState::Conflict | MergeState::Stale | MergeState::Vetoed
    )
}

/// **Whether one entry's review may be restarted, and the sentence for either answer** — the
/// person's act, as one pure function of the two rows.
///
/// The operator's ask is *"we need a way to restart them"*, and the two refusals below are the
/// whole of what keeps that from becoming a second writer:
///
/// * **An attempt in flight is refused.** `decision: None` with no `failed_ms` is *the reviewer
///   is working on it right now*, and a restart there would put a second gatekeeper beside a
///   live one on the same entry — two children, two verdicts, one row. That is the shape this
///   tree refuses everywhere, and the refusal names the wait rather than the entry.
/// * **An entry that is not parked is refused.** `waiting` and `taken` are the queue working;
///   `landed` is done. Restarting one of those would be the operator reaching past the queue's
///   own scheduling to re-ask for something already asked.
///
/// **Safe to ask for twice, and that is not this function's doing** — the second ask finds the
/// entry `waiting` (the first ask moved it) and gets the second refusal. The rule is stated
/// here and the arbiter is the row, in `Store::restart_review`'s conditional `UPDATE`.
pub fn restartable(
    entry: &MergeEntry,
    review: Option<&letibot_tokencore::store::ReviewRecord>,
) -> Result<(), String> {
    if attempt_in_flight(review) {
        return Err(format!(
            "a gatekeeper is working on `{}` right now — the review has been asked for and \
             has neither answered nor failed. A second reviewer beside a live one would be a \
             second writer on one entry's verdict, so nothing was restarted. Wait for it; if \
             its attempt dies, the entry comes back here with the failure on its row.",
            entry.branch
        ));
    }
    if !parked(entry.state) {
        return Err(format!(
            "`{}` is `{}` — nothing to restart. Only a parked entry (failed, conflict, stale, \
             vetoed) is restarted by hand; a waiting one is what the queue's own retry is for.",
            entry.branch,
            entry.state.as_str()
        ));
    }
    Ok(())
}

/// **Restart one entry's review** — the person's act, performed, and the sentence it answers
/// with.
///
/// The whole of the act is [`Store::restart_review`]'s two writes: the entry back to `waiting`
/// and its review row back to *nobody has answered*. What is here is the DECISION
/// ([`restartable`]), the sentence that names what was there, and the announcement — so the
/// verb that calls it holds no rule of its own and a test can drive the act without a socket.
///
/// **`events` is how every head hears**, and it is the queue's own event rather than a new one:
/// a restart IS an entry move, and a head that folded `MergeEntryMoved` needs no second
/// vocabulary to learn that a parked entry is being tried again. It is published AFTER the
/// rows are written, `move_to`'s own order and for its reason.
///
/// **What the sentence names**, because the brief's whole point is that the restart is a
/// decision about something legible: the branch and id it is re-attempting, and the verdict or
/// the failure that was on the row — so a person who presses it on the wrong entry can see
/// that they did.
pub fn restart(
    store: &Store,
    entry_id: &str,
    now_ms: u64,
    events: &dyn Fn(letibot_sessionlog::SessionEvent),
) -> Result<String, String> {
    let entry = read_entry(store, entry_id)?;
    let review = store
        .merge_review(entry_id)
        .map_err(|e| format!("the review of `{entry_id}` could not be read: {e}"))?;
    restartable(&entry, review.as_ref())?;

    // **What is being re-attempted, in the row's own words.** A verdict and a failed attempt are
    // two different things to name, and naming the wrong one would send a person looking for a
    // judgement nobody made.
    let was = match review.as_ref() {
        Some(r) if r.decision.is_some() => format!(
            "the verdict on it was `{}`",
            r.decision.as_deref().unwrap_or_default()
        ),
        Some(r) if !r.failure.is_empty() => format!(
            "{} attempt(s) failed — {}",
            r.attempts,
            first_line(&r.failure)
        ),
        _ => format!("it was `{}` with no verdict on it", entry.state.as_str()),
    };
    let evidence = format!("restarted by the operator: {was}. A gatekeeper is being asked again.");
    if !store
        .restart_review(entry_id, &evidence, now_ms)
        .map_err(|e| format!("the restart of `{entry_id}` could not be written: {e}"))?
    {
        // The row moved under us between the read and the write — a second press, or the queue's
        // own pass parking it somewhere else. Refused rather than reported as done, because a
        // restart that did not happen and a restart that did are the two facts a person acts on.
        let now = state_now(store, entry_id);
        return Err(format!(
            "nothing was restarted: `{entry_id}` is `{now}` and a restart only moves a parked \
             entry. If you have just asked once, this is the second ask and the first one is \
             already in flight."
        ));
    }
    events(letibot_sessionlog::SessionEvent::MergeEntryMoved {
        id: entry_id.to_string(),
        state: crate::mergequeue::wire_state(MergeState::Waiting),
        evidence: evidence.clone(),
    });
    Ok(format!(
        "restarted `{}` (`{entry_id}`): {was}. The queue asks its gatekeeper again — the entry \
         is back to `waiting`, and it does not land until a verdict accepts it and the gate is \
         green.",
        entry.branch
    ))
}

// ===== The sweeps: re-ask every parked review, or forget rows =====
//
// The operator's ruling, in their words: *"as for reset - no reset resets review states. and
// /queue clean deletes"*. Two verbs about a SET of rows rather than about one: `reset` puts
// every parked entry back in front of the tutor and changes nothing else, and `clean` forgets
// rows. Each takes the same `Option<&str>` — an id is the one row, `None` is the whole queue —
// and each is `restart`'s or `rm`'s rule applied to that set, so the two spellings of one verb
// cannot disagree about a row.

/// **Re-ask the review of every parked entry** — `/queue reset [ENTRY-ID]`, and the sentence it
/// answers with.
///
/// The whole of the act is [`Store::reset_reviews`]'s two writes, per row: the entry back to
/// `waiting` and its review row back to *nobody has answered*. What is here is the DECISION
/// ([`parked`]), the sentence, and the announcement — `restart`'s shape, one set wide.
///
/// **What it does not touch is the point of the verb.** The ruling is explicit that a reset is
/// about REVIEW STATES and not about entries: no branch is moved, no worktree is swept, no row
/// is deleted, and the entry's own facts — its brief, its `needs`, its base, its worktree — are
/// exactly what they were. The tutor re-reviews the same branch, and the queue then does what it
/// does with a waiting entry whose verdict accepts.
///
/// **Why a verb of its own when `restart` exists.** `restart` is one entry and it is a person's
/// answer to one row; this is the sweep for a queue that has parked a pile of them — the
/// operator's four, and the nine letidb branches under the new machinery — where re-asking them
/// one at a time is the ceremony the verb exists to remove.
///
/// **The announcement is one `MergeEntryMoved` per row**, the queue's own event rather than a
/// new one, for `restart`'s reason: a reset IS an entry move, and a head that folded the queue
/// needs no second vocabulary to learn that a parked entry is being tried again. The events are
/// published only when the store's count is the count this function counted: a row that moved
/// under us between the read and the write is a row whose event would be a lie, and the
/// snapshot is the head's fallback.
///
/// [`Store::reset_reviews`]: letibot_tokencore::store::Store::reset_reviews
pub fn reset(
    store: &Store,
    entry: Option<&str>,
    now_ms: u64,
    events: &dyn Fn(letibot_sessionlog::SessionEvent),
) -> Result<String, String> {
    // **An id that resolves to nothing is refused in the queue's own grammar** — `read_entry`'s
    // sentence, the same one `restart` and `rm` give — rather than answered with a report of a
    // sweep that found no rows.
    let about: Vec<MergeEntry> = match entry {
        Some(id) => vec![read_entry(store, id)?],
        None => store
            .merge_entries()
            .map_err(|e| format!("the queue could not be read: {e}"))?,
    };
    let rows: Vec<MergeEntry> = about.into_iter().filter(|e| parked(e.state)).collect();
    if rows.is_empty() {
        let said = match entry {
            Some(id) => format!(
                "nothing was reset: `{id}` is not a parked entry. A reset moves a `failed`, \
                 `conflict`, `stale` or `vetoed` entry back into the queue with its review \
                 cleared, and nothing else; `/queue` lists the entries and the state each one is \
                 in."
            ),
            None => "nothing was reset: the queue holds no parked entry. A reset moves the \
                     `failed`, `conflict`, `stale` and `vetoed` rows back into the queue with \
                     their reviews cleared; `/queue` lists what is there."
                .to_string(),
        };
        // **A named entry that is not parked is a refusal** — `restart`'s answer to the same
        // press — while a whole queue with nothing parked is an empty sweep rather than a
        // failure, and says so.
        return match entry {
            Some(_) => Err(said),
            None => Ok(said),
        };
    }
    let moved = store
        .reset_reviews(now_ms, entry)
        .map_err(|e| format!("the reset could not be written: {e}"))?;
    if moved == 0 {
        // The row moved under us between the read and the write: the daemon claimed it, or a
        // person answered it. Refused rather than reported as done, because a reset that did not
        // happen and a reset that did are the two facts a person acts on.
        return Err(match entry {
            Some(id) => format!(
                "nothing was reset: `{id}` is `{}` now, and a reset only moves a parked entry. \
                 The row is still there, with its reason on it.",
                state_now(store, id)
            ),
            None => "nothing was reset: every parked entry moved while this was being written \
                     — the daemon took them, or a person answered them. `/queue` is the truth."
                .to_string(),
        });
    }
    if moved == rows.len() {
        for e in &rows {
            events(letibot_sessionlog::SessionEvent::MergeEntryMoved {
                id: e.id.clone(),
                state: crate::mergequeue::wire_state(MergeState::Waiting),
                evidence: letibot_tokencore::store::Store::RESET_EVIDENCE.to_string(),
            });
        }
    }
    let list = rows
        .iter()
        .map(|e| format!("`{}` ({})", e.branch, e.state.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let word = if moved == 1 { "entry" } else { "entries" };
    // **A short count is said rather than swallowed.** The store moved fewer rows than were read
    // as parked, which is a row somebody else got to first; the sentence says how many and leaves
    // the state of the rest to the queue, which is where it is true.
    let raced = if moved == rows.len() {
        String::new()
    } else {
        format!(
            " {} of them moved while this was being written, so the queue's own state is the \
             truth about those.",
            rows.len() - moved
        )
    };
    Ok(format!(
        "reset {moved} {word}: {list}. The queue asks its gatekeeper about each of them again — \
         they are back to `waiting` with their reviews cleared, and not one of them lands until a \
         verdict accepts it and the gate is green. Nothing else about them changed: the branches, \
         their worktrees and their rows are exactly what they were.{raced}"
    ))
}

// ===== The person's three verbs: approve, veto and rm =====
//
// The operator's ask, in their words: *"i want to be able to approve / veto / delete"*. Three
// acts, and they are the three things a person can say about an entry the queue is holding: *let
// it through* (approve), *send it back* (veto) and *forget it* (rm). Each is a PURE decision
// ([`approvable`], [`vetoable`], [`removable`]) plus a conditional store write plus a sentence —
// `restart`'s shape one verb over, because the rule is what a test asserts without a daemon and
// the write is what the row arbitrates.

/// **The states a person's verb may move an entry out of**, or the sentence that refuses one.
///
/// Two states are refused and the sentences are different because the two facts are: `taken` is
/// the daemon working — a verb there is a second writer on a row the landing is about to
/// overwrite — and `landed` is done. Everything else is a row the queue is not touching.
///
/// **A `match` over the whole closed set rather than a `matches!` on the five it allows**, so a
/// state added to [`MergeState`] fails to compile HERE, in the one place that decides what a
/// person may answer, rather than being silently classified as movable by a list that forgot it.
fn movable(entry: &MergeEntry) -> Result<(), String> {
    match entry.state {
        MergeState::Waiting
        | MergeState::Failed
        | MergeState::Conflict
        | MergeState::Stale
        | MergeState::Vetoed => Ok(()),
        MergeState::Taken => Err(format!(
            "`{}` is `taken` — the queue has claimed it and is rebasing and gating it right now, \
             so a verb here would be a second writer on a row the landing is about to overwrite. \
             Nothing was done. It comes back to `waiting` with a verdict, or to a parked state \
             with the reason, and then it is yours to answer.",
            entry.branch
        )),
        MergeState::Landed => Err(format!(
            "`{}` has already landed — main is where it went, and its worktree and branch are \
             cleaned up. Nothing was done: a merged branch is not something a person can \
             un-decide from here.",
            entry.branch
        )),
    }
}

/// **Whether a review attempt is IN FLIGHT** — asked for, and neither answered nor failed.
///
/// The one predicate the refusals that name a live attempt share ([`restartable`],
/// [`approvable`], [`removable`]). `decision: None` with no `failed_ms` is *the reviewer is
/// working on it right now*, which is why it is not simply *there is no verdict*: an attempt
/// that has DIED leaves `decision` `None` too, and a person may act on that one.
pub fn attempt_in_flight(review: Option<&letibot_tokencore::store::ReviewRecord>) -> bool {
    review.is_some_and(|r| r.answered_ms.is_none() && r.failed_ms.is_none())
}

/// **Whether one entry may be approved by a person, and the sentence for either answer** — the
/// person's verdict, as one pure function of the two rows.
///
/// An approval says *I have read it and it goes*: it REPLACES the review's judgement — the entry
/// returns to `waiting` with an accepting verdict on its review — and then the queue does what it
/// does with any waiting entry whose verdict accepts: rebase at the tip of main, run the gate,
/// land. **It overrides a JUDGEMENT and never the landing machinery**, which is the whole of what
/// keeps a person's verb from being a way around the gate.
///
/// Three refusals, and each names a different fact:
///
/// * **A live attempt is refused.** The reviewer has been asked and has neither answered nor
///   failed, so an approval would write its verdict over one that is about to arrive — two
///   writers on one entry's verdict, the shape this tree refuses everywhere.
/// * **An entry that already accepts is refused.** There is nothing to override, and refusing is
///   what makes a second press of the same verb move nothing: the verdict row is the arbiter.
/// * **`taken` and `landed` are refused** — see [`movable`].
pub fn approvable(
    entry: &MergeEntry,
    review: Option<&letibot_tokencore::store::ReviewRecord>,
) -> Result<(), String> {
    use letibot_tools::gatekeeper::Decision;
    movable(entry)?;
    if attempt_in_flight(review) {
        return Err(format!(
            "a gatekeeper is working on `{}` right now — the review has been asked for and has \
             neither answered nor failed, and an approval would write the person's verdict over \
             the one that is about to arrive. That is two writers on one entry's judgement, so \
             nothing was approved. Wait for the verdict; an attempt that dies comes back to you \
             with its failure on the row.",
            entry.branch
        ));
    }
    if review
        .and_then(|r| r.decision.as_deref())
        .and_then(Decision::parse)
        == Some(Decision::Accept)
    {
        return Err(format!(
            "`{}` already carries an accepting verdict — there is nothing to override, and the \
             queue is already taking it. Nothing was done.",
            entry.branch
        ));
    }
    Ok(())
}

/// **Whether one entry may be vetoed by a person, and the sentence for either answer.**
///
/// A veto is not a park: the entry's ORIGINAL subagent is sent back to work with the refusal, and
/// the entry is parked in `vetoed` — a state the queue never acts on — until that child's work is
/// enqueued again (`MergeState::Vetoed`). It reads the entry alone: the review is not consulted,
/// because a person's rejection overrides whatever the review would say and the queue never reads
/// the review of an entry that is not waiting.
///
/// **Why not `waiting`.** The child is working, so the entry is live rather than dead — but a
/// `waiting` row would have the daemon ask its gatekeeper again on the very next pass, about a
/// branch the child has not touched yet, and park it `Failed` with a second refusal on it. The
/// row would be red within a second of the person sending the child back, which is the opposite
/// of what the verb means. `vetoed` is the state that says *the ball is with the child*.
pub fn vetoable(entry: &MergeEntry) -> Result<(), String> {
    movable(entry)?;
    if entry.state == MergeState::Vetoed {
        return Err(format!(
            "`{}` is already `vetoed` — it was sent back to its child, and a second veto would be \
             the same decision twice. Nothing was done. `/queue restart {}` asks the gatekeeper \
             about the branch as it is now, `/queue approve {}` overrides the review, and \
             `/queue rm {}` drops the entry.",
            entry.branch,
            letibot_sessionlog::registry::short_id(&entry.id),
            letibot_sessionlog::registry::short_id(&entry.id),
            letibot_sessionlog::registry::short_id(&entry.id),
        ));
    }
    Ok(())
}

/// **Whether one entry may be dropped from the queue by a person, and the sentence for either
/// answer.**
///
/// `rm` forgets the entry: the row and its review go, and the branch and its worktree stay —
/// they were never the queue's to delete, and `Landed`'s cleanup is the only thing in this module
/// that removes a tree.
///
/// **A live attempt is refused**, and here it is the sharpest of the three: the review row is
/// deleted with the entry, so a reviewer answering now would be writing a verdict onto a row that
/// is gone. That is the second-writer shape this queue refuses everywhere.
///
/// **A landed row is refused too, and that is a decision this brief did not ask for.** A landed
/// row is the queue's record that main moved, and it is also the base a pending dependent rebases
/// onto ([`effective_base`]); `unmet_needs` reads a dependency that is not in the queue as
/// unmet, so deleting a landed row would leave a dependent waiting for ever on an id that is
/// gone — a silent wedge in place of a row. Nothing about the operator's ask needs the landed
/// rows gone (they are green, not red), so the refusal is cheap and the alternative is a queue
/// that can be broken from the pane.
pub fn removable(
    entry: &MergeEntry,
    review: Option<&letibot_tokencore::store::ReviewRecord>,
) -> Result<(), String> {
    if entry.state == MergeState::Landed {
        return Err(format!(
            "`{}` has landed and its row STAYS: it is the queue's record that main moved, and the \
             base an entry that needs it rebases onto. A dependency the queue cannot find is one \
             it cannot vouch for, so deleting this row would leave a waiting entry waiting for \
             ever. Nothing was removed.",
            entry.branch
        ));
    }
    movable(entry)?;
    if attempt_in_flight(review) {
        return Err(format!(
            "a gatekeeper is working on `{}` right now — the review has been asked for and has \
             neither answered nor failed. The queue forgets an entry's verdict with the entry, so \
             removing this one would have a reviewer writing its verdict onto a row that is gone. \
             Nothing was removed; wait for the verdict, or `/queue approve {}` if the review is \
             not what you want to hear from.",
            entry.branch,
            letibot_sessionlog::registry::short_id(&entry.id),
        ));
    }
    Ok(())
}

// ===== The git the landing needs, when the work is still in the worktree =====
//
// **The operator's ruling, in their words: *"no way i touch git myself, we build this queue not
// for that"*.** An entry whose branch is 0 commits over its base with the work sitting
// uncommitted in the worktree is not a branch nobody reviewed — the reviewer's *"landing the
// branch lands nothing"* is correct and useless. It is three of the six entries the operator was
// looking at, and the answer the queue exists to avoid is *the person runs git*. So `approve`
// commits the worktree's work onto the branch, and then the ordinary landing runs.

/// **The one path `task_start` writes into a worktree, and the reason it is never swept.**
///
/// `harness.rs` writes `<worktree>/.cargo/config.toml` — a `[build] target-dir` pointing at the
/// main tree's `target/`, so a child does not pay a cold build of the whole graph — and then adds
/// the path to the worktree's own `info/exclude`. **That exclude is inert for a TRACKED file**, so
/// in a repository that tracks `.cargo/config.toml` (a musl static-build config, in the repository
/// this was measured against) the redirect arrives as a MODIFICATION of a real file: a blind `git
/// add -A` would commit the repository's own config away, and `task_start` would have done it to
/// every worktree the queue was ever asked to land.
pub const BUILD_CACHE_REDIRECT: &str = ".cargo/config.toml";

/// **Whether a file's content is the build-cache redirect `task_start` wrote** — its own first
/// line is the marker.
///
/// Content rather than the path alone, because the path is a real config in a repository that
/// tracks it and a person may have edited it as part of the work: what the sweep must never take
/// is `task_start`'s own sentence, and what it must leave alone is everything else.
pub fn is_build_cache_redirect(text: &str) -> bool {
    text.lines()
        .next()
        .is_some_and(|l| l.contains("Written by `task_start`"))
}

/// **What the queue must do about the work an entry's worktree is holding** — one decision, from
/// the facts git gave, with no repository in reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sweep {
    /// The branch already carries commits over its base: the work is committed, and the person's
    /// approval is only a verdict.
    Already,
    /// The branch is 0 commits over its base AND the worktree holds nothing but the build-cache
    /// redirect. Approving would land nothing, which is what the reviewer said.
    Nothing,
    /// One commit of these paths, and `redirect` says whether `task_start`'s redirect was among
    /// the changed paths — the act restores it rather than committing it.
    Commit { paths: Vec<String>, redirect: bool },
}

/// See [`Sweep`] — the decision, as one pure function of the two facts git reports.
///
/// The redirect is filtered out of `paths` HERE rather than by the act, so the rule *the
/// build-cache redirect is never swept* is a rule a test asserts with no repository, no git and
/// no store — the same reason the rest of this module's core is pure.
pub fn sweep_for(commits_over_base: usize, changed: &[String]) -> Sweep {
    if commits_over_base > 0 {
        return Sweep::Already;
    }
    let redirect = changed.iter().any(|p| p == BUILD_CACHE_REDIRECT);
    let paths: Vec<String> = changed
        .iter()
        .filter(|p| *p != BUILD_CACHE_REDIRECT)
        .cloned()
        .collect();
    if paths.is_empty() {
        Sweep::Nothing
    } else {
        Sweep::Commit { paths, redirect }
    }
}

/// **The paths `git status --porcelain -z -uall` names**, in git's own order.
///
/// `-z` rather than the line form: a path with a space or a quote in it is a path a line-based
/// read mangles, and this list goes into a commit message and onto a row a person reads — a name
/// that is wrong is worse than a name that is missing. The NUL form puts the two status columns
/// and a space before the path, and a rename or a copy carries its ORIGIN after its destination,
/// so the second path is skipped rather than committed as a path of its own.
///
/// **`-uall` because the default collapses an untracked directory to the directory itself**: a
/// fresh `docs/` holding three files came back as one entry, `docs/`, and the operator's second
/// requirement is that the commit and the row name the FILES that were taken (*"say what it
/// swept"*) — a directory name is a summary a person cannot check. Measured here: without it the
/// sweep's own test read `["b.txt", "docs/"]`.
fn changed_paths(status_z: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut parts = status_z.split('\0').filter(|s| !s.is_empty());
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let (status, path) = entry.split_at(3);
        out.push(path.to_string());
        if status.contains('R') || status.contains('C') {
            let _ = parts.next();
        }
    }
    out
}

/// **The two facts the sweep's decision is made from**, read in the entry's own worktree: how many
/// commits the branch carries over the base it was cut from, and what git says has changed.
fn worktree_facts(worktree: &Path, base: &str) -> Result<(usize, Vec<String>), String> {
    let range = format!("{base}..HEAD");
    let count = git(worktree, &["rev-list", "--count", &range])?;
    let commits = count.trim().parse::<usize>().map_err(|e| {
        format!(
            "git rev-list --count {range} answered `{}`, which is not a number: {e}",
            count.trim()
        )
    })?;
    let status = git(worktree, &["status", "--porcelain", "-z", "-uall"])?;
    Ok((commits, changed_paths(&status)))
}

/// **Put the tracked build-cache redirect back**, and answer whether there was one to put back.
///
/// Three cases, and only one of them writes: the path is TRACKED and its content is
/// `task_start`'s redirect (restore it — the repository's own config belongs in the worktree, and
/// the rebase the queue does next REFUSES to run with a modified tracked file, which is how a
/// sweep that left it alone would turn into the same dirty-tree conflict one pass later); the
/// path is untracked (leave it — it is excluded by construction, `git add -A` will not take it,
/// and the worktree keeps its shared build cache); the path is tracked and its content is NOT the
/// redirect (leave it: that is a person's edit, and the sweep's business is to commit the work,
/// not to decide which work).
fn restore_redirect(worktree: &Path) -> Result<bool, String> {
    let path = worktree.join(BUILD_CACHE_REDIRECT);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    if !is_build_cache_redirect(&text) {
        return Ok(false);
    }
    if git(
        worktree,
        &["ls-files", "--error-unmatch", BUILD_CACHE_REDIRECT],
    )
    .is_err()
    {
        return Ok(false);
    }
    git(worktree, &["checkout", "--", BUILD_CACHE_REDIRECT])?;
    Ok(true)
}

/// **Commit the work the worktree is holding onto the entry's branch** — one commit, and the
/// sentence that says what it took.
///
/// The commit message is the durable half of the operator's second requirement (*"say what it
/// swept"*): it names the entry, says that the operator approved and that this is the queue doing
/// the landing rather than a person running git, and lists the paths. The entry's evidence carries
/// the same list, so a person who pressed the verb sees what was taken without opening git at
/// all.
///
/// **The staging excludes the redirect by pathspec as well as by the restore above**, which is
/// belt and braces on purpose: the restore is what keeps the worktree clean enough for the rebase
/// that follows, and the exclusion is what makes *the redirect is never committed* true even if
/// `task_start`'s marker sentence is ever reworded.
fn sweep_worktree(
    worktree: &Path,
    entry_id: &str,
    paths: &[String],
    redirect: bool,
) -> Result<String, String> {
    let restored = if redirect {
        restore_redirect(worktree)?
    } else {
        false
    };
    let exclude = format!(":(exclude){BUILD_CACHE_REDIRECT}");
    git(worktree, &["add", "-A", "--", ".", &exclude])?;
    let mut message = format!(
        "the work the worktree was holding, committed by the merge queue\n\n\
         The branch carried no commits over its base and the worktree was not clean, so there\n\
         was nothing for the queue to land. The operator approved the entry `{entry_id}`\n\
         (`/queue approve {entry_id}`), and this commit is the queue doing what the landing\n\
         needs rather than asking a person to run git.\n\nSwept from the worktree:\n"
    );
    for p in paths {
        message.push_str(&format!("  {p}\n"));
    }
    if restored {
        message.push_str(&format!(
            "\nThe build-cache redirect at `{BUILD_CACHE_REDIRECT}` (`task_start` writes it) was \
             restored, not swept.\n"
        ));
    }
    git(worktree, &["commit", "-m", &message])?;
    let mut said = format!(
        "The branch was 0 commits over its base and the worktree held work, so the queue \
         committed it — {} file(s): {}",
        paths.len(),
        paths.join(", ")
    );
    if restored {
        said.push_str(&format!(
            ", and restored the build-cache redirect at `{BUILD_CACHE_REDIRECT}` (`task_start` \
             writes it) rather than sweeping it"
        ));
    }
    said.push('.');
    Ok(said)
}

/// **One entry by the id a person typed**, or the refusal that names the grammar.
///
/// One function for the four verbs, because *the id does not resolve* has one true answer and the
/// four acts must not paraphrase it four ways: `/queue` lists the entries and each row carries its
/// id.
fn read_entry(store: &Store, entry_id: &str) -> Result<MergeEntry, String> {
    store
        .merge_entry(entry_id)
        .map_err(|e| format!("the entry `{entry_id}` could not be read: {e}"))?
        .ok_or_else(|| {
            format!(
                "there is no entry `{entry_id}` in the queue — `/queue` lists what is there, and \
                 the pane's rows carry their ids."
            )
        })
}

/// **Approve one entry** — the person's verdict, the git the landing needs, and the sentence it
/// answers with.
///
/// The whole of the act is [`hand_over`] — the one path into the queue, which a reviewer's own
/// `accept` takes as well (see the doors at the foot of this module). What is HERE is the DECISION
/// ([`approvable`]) and what was overridden, so the verb holds no rule of its own and the door
/// that is the same act does not have to restate one.
///
/// **The gate is not bypassed and the sentence says so.** An approval overrides the reviewer's
/// judgement; the queue then rebases, gates and lands exactly as it would for a verdict it was
/// given, and the row says so in as many words — a person reading *approved by the operator* must
/// not be able to conclude that the gate was skipped.
///
/// **`by` is the session the verb came from**, and it is written on the review row: after an
/// approval the argument that matters is the person's, and the pane draws that column as *attach
/// to it to read the argument*.
pub fn approve(
    store: &Store,
    entry_id: &str,
    by: &str,
    now_ms: u64,
    events: &dyn Fn(letibot_sessionlog::SessionEvent),
) -> Result<String, String> {
    let entry = read_entry(store, entry_id)?;
    let review = store
        .merge_review(entry_id)
        .map_err(|e| format!("the review of `{entry_id}` could not be read: {e}"))?;
    approvable(&entry, review.as_ref())?;

    // **What was overridden, in the row's own words.** A verdict, an attempt that failed and a
    // person's own earlier veto are three different things to name, and naming the wrong one
    // would send a reader looking for a judgement nobody made.
    let was = match (entry.state, review.as_ref()) {
        (MergeState::Vetoed, _) => "a person had vetoed it".to_string(),
        (_, Some(r)) if r.decision.is_some() => format!(
            "the verdict on it was `{}`",
            r.decision.as_deref().unwrap_or_default()
        ),
        (_, Some(r)) if !r.failure.is_empty() => format!(
            "{} attempt(s) failed — {}",
            r.attempts,
            first_line(&r.failure)
        ),
        (_, Some(_)) => "nobody had answered the review".to_string(),
        (_, None) => "nobody had asked for a verdict".to_string(),
    };
    hand_over(
        store,
        &entry,
        by,
        Handed::ByTheOperator,
        &was,
        "",
        now_ms,
        events,
    )
}

/// **Who is handing the entry over** — the two callers of [`hand_over`], and the three clauses
/// that differ between them.
///
/// One enum rather than three `&str` parameters because those clauses are ONE fact about the
/// caller: with them separate, the way to write *the operator approved it* into a reviewer's row
/// is to pass the wrong string in one of three places and get the other two right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handed {
    /// The operator's `/queue approve`.
    ByTheOperator,
    /// **The reviewer's own reported `accept`** — the first of the three doors.
    ByTheReview,
}

impl Handed {
    /// What the entry's `evidence` opens with, and what a reader sees first.
    fn lead(&self) -> &'static str {
        match self {
            Handed::ByTheOperator => "approved by the operator",
            Handed::ByTheReview => "accepted by the reviewer",
        }
    }

    /// The verb, for the sentences and the refusals.
    fn verb(&self) -> &'static str {
        match self {
            Handed::ByTheOperator => "approved",
            Handed::ByTheReview => "accepted",
        }
    }

    /// The act as a noun, for the sentences about a write that did not happen.
    fn noun(&self) -> &'static str {
        match self {
            Handed::ByTheOperator => "approval",
            Handed::ByTheReview => "acceptance",
        }
    }

    /// The last clause, which is the one that must never read as *the gate was skipped*.
    fn tail(&self) -> &'static str {
        match self {
            Handed::ByTheOperator => "an approval overrides the judgement, never the landing.",
            Handed::ByTheReview => "a verdict hands the entry over; it never skips the landing.",
        }
    }
}

/// **The one path into the queue** — an entry back to `waiting` with an accepting verdict on its
/// review, which is the state the queue's own pass takes ([`next_takeable`]).
///
/// **One function because `/queue approve` and a reviewer's `accept` are the same act said two
/// ways.** The person's verb is this path with the operator's rules in front of it
/// ([`approvable`]); the reviewer's door is this path with the reviewer's reasons attached. A
/// second copy would be a second answer to *what makes an entry takeable*, and the answer includes
/// the sweep below — the half a copy would silently drop.
///
/// **The verdict is written HERE and not by the caller**, which is why the accept door does not
/// write one first: [`Store::approve_entry`] is guarded on the verdict row (a row that already
/// carries an accepting verdict is not overwritten), so a door that wrote `accept` itself would
/// find its own write refusing the move.
///
/// **What a reviewer's `reasons` do here.** They go into the entry's `evidence`, through `extra`,
/// rather than onto the review row: the accepting row `approve_entry` writes is the OVERRIDE
/// shape — its `reasons` column is empty by construction, because the argument that matters after
/// a person's approval is the person's. The entry's `evidence` is where a reader looks for why an
/// entry moved, so the reviewer's reasons land there, beside the verdict that moved it.
fn hand_over(
    store: &Store,
    entry: &MergeEntry,
    by: &str,
    who: Handed,
    was: &str,
    extra: &str,
    now_ms: u64,
    events: &dyn Fn(letibot_sessionlog::SessionEvent),
) -> Result<String, String> {
    let entry_id = entry.id.as_str();
    // **The work, before the verdict.** A branch with 0 commits over its base is a branch the
    // gate would run on nothing, and the operator's ruling is that the queue absorbs this rather
    // than the person: the worktree's work becomes one commit, and then the ordinary landing runs.
    // The sweep goes FIRST so that a store write that loses a race leaves a commit and an
    // unapproved entry rather than an approved entry the gate cannot land — and a second press
    // finds the work committed (`Sweep::Already`) and commits nothing twice.
    let swept = match entry.worktree.as_deref().map(Path::new) {
        Some(wt) if wt.is_dir() => {
            let (commits, changed) = worktree_facts(wt, &entry.base_sha)?;
            match sweep_for(commits, &changed) {
                Sweep::Already => None,
                Sweep::Nothing => {
                    return Err(format!(
                        "`{}` has nothing to land — its branch is 0 commits over its base (`{}`) \
                         and its worktree at `{}` is clean, so there is no work for the queue to \
                         commit and nothing for the gate to run on. That is what its reviewer \
                         said. Nothing was {} — an entry with no work is not one the queue can \
                         land, whoever asks for it. If the work exists somewhere else, it is not \
                         in this entry's worktree; `/queue rm {entry_id}` drops the entry.",
                        entry.branch,
                        entry.base_sha,
                        wt.display(),
                        who.verb(),
                    ));
                }
                Sweep::Commit { paths, redirect } => {
                    Some(sweep_worktree(wt, entry_id, &paths, redirect)?)
                }
            }
        }
        // **No worktree, so nothing to sweep and nothing to say about it.** The queue cannot
        // rebase a branch that is not checked out either, and its own row says so
        // (`"no worktree: the branch is not checked out"`); the verdict is still somebody's to
        // give, so it goes through rather than being refused for a reason that is the queue's to
        // report.
        _ => None,
    };
    let evidence = format!(
        "{}: {was}. {extra}{}The queue takes it again and the gate still runs — nothing lands \
         until it is green.",
        who.lead(),
        swept.as_deref().unwrap_or_default()
    );
    if !store
        .approve_entry(entry, by, &evidence, now_ms)
        .map_err(|e| {
            format!(
                "the {} of `{entry_id}` could not be written: {e}",
                who.noun()
            )
        })?
    {
        // The row moved under us between the read and the write — a second press, the daemon
        // claiming it, or a landing. Refused rather than reported as done, because an approval
        // that did not happen and one that did are the two facts a person acts on.
        let now = state_now(store, entry_id);
        return Err(format!(
            "nothing was {}: `{entry_id}` is `{now}` and an {} moves an entry that is waiting or \
             parked, never one the queue has taken or landed. If you have just asked once, this \
             is the second ask — the verdict already accepts it.{} ",
            who.verb(),
            who.noun(),
            match &swept {
                Some(what) => format!(" The work was committed, once: {what}"),
                None => String::new(),
            }
        ));
    }
    events(letibot_sessionlog::SessionEvent::MergeEntryMoved {
        id: entry_id.to_string(),
        state: crate::mergequeue::wire_state(MergeState::Waiting),
        evidence: evidence.clone(),
    });
    Ok(format!(
        "{} `{}` (`{entry_id}`): {was}. {extra}{}\
         The entry is back to `waiting` — the queue rebases it at the tip of main, runs the gate \
         and lands it, and the gate is not skipped: {}",
        who.verb(),
        entry.branch,
        swept.as_deref().unwrap_or_default(),
        who.tail()
    ))
}

/// **What the child is told when an entry goes back to it** — one pure function of the entry, the
/// person's words and the verdict.
///
/// The operator's own model of this queue, in their words: *"gatekeeper reviews and drives
/// subagents to completion by nagging them via messages. so it gets queue item - reviews if ok -
/// puts into merge queue ... and if it is not happy - it sends a message with complaints to
/// original subagent"*. **A function rather than a string built at the call site** for exactly that
/// reason: the nagging half is a gatekeeper sending the same message with its own complaints where
/// a person's words go — [`refusal_message`], which is that half — and this is the shape it carries.
///
/// The words are the SENDER's, and the two senders are not interchangeable: a person's veto is an
/// override that has to be quoted verbatim, and the gatekeeper's refusal is the review itself. What
/// they share is [`back_to_work`], which is the part a child acts on.
pub fn veto_message(
    entry: &MergeEntry,
    why: &str,
    review: Option<&letibot_tokencore::store::ReviewRecord>,
) -> String {
    let said = match why.trim() {
        "" => "The operator vetoed it and gave no reason.".to_string(),
        w => format!("This is what the operator said:\n\n  {w}"),
    };
    back_to_work(entry, &said, &review_half(review))
}

/// **What the child is told when the GATEKEEPER refused it** — the complaints, back to the session
/// that did the work, with no person in the sentence at all.
///
/// The other half of [`veto_message`]'s own reason: the operator's model is *"if it is not happy -
/// it sends a message with complaints to original subagent"*, and the complaints a reviewer has are
/// the ones it wrote on the review row. So this is the same act — the ball goes back — with the
/// reviewer's verdict where a person's words go, and the same [`back_to_work`] tail, which is the
/// part the child acts on: which branch, which entry, which worktree, and what has to happen
/// before anything lands.
///
/// **A refusal with no reasons is said as that, not as a silence.** A reviewer that answered
/// `reject` and wrote no `reasons:` has still refused; the child is told the verdict word and told
/// where the rest of it is (the review row and the entry's evidence), rather than being handed a
/// sentence that reads like nobody judged the branch.
pub fn refusal_message(
    entry: &MergeEntry,
    review: Option<&letibot_tokencore::store::ReviewRecord>,
) -> String {
    back_to_work(entry, &the_refusal(review), &review_half(review))
}

/// **The same refusal, to the session that ENQUEUED the entry** — for when the child that wrote the
/// branch cannot be reached.
///
/// The child's sentence ([`refusal_message`]) opens with *your branch `x` was taken out of the merge
/// queue … and the entry is back with you*, which is true of the child and false of anybody else:
/// the root did not write the branch. So this says what IS true of the root — a child it started has
/// been refused, the child could not be told, and the entry, with every verb that moves it, is the
/// root's now — and `why_child` is the delivery door's own words for the failed attempt, QUOTED
/// rather than paraphrased. A reader who cannot see that reason cannot tell a dead subagent from a
/// daemon that would not send.
///
/// **The reviewer's reasons travel whole**, the same [`review_half`] the child's sentence carries and
/// the same [`the_refusal`] opening: *why* is the whole of what the second party has to act on, and a
/// summary here would be this code writing the reviewer's half for it. The remedies are named for
/// the same reason they are in [`back_to_work`] — the root is the party holding them.
pub fn enqueuer_refusal_message(
    entry: &MergeEntry,
    review: Option<&letibot_tokencore::store::ReviewRecord>,
    why_child: &str,
) -> String {
    let head = format!(
        "The branch `{}` of a child you started was taken out of the merge queue: it is NOT landing \
         as it stands, and the entry `{}` is yours to move. The child that wrote it could not be \
         told — {why_child} — so the complaints are here, with you.",
        entry.branch, entry.id
    );
    format!(
        "{head}\n\n{said}\n\n{reviewed}\n\nThe work is in the worktree at `{wt}`, and nothing lands \
         until a review accepts it and the gate is green. `/queue restart {short}` asks the reviewer \
         again about the branch as it is now, `/queue approve {id}` overrides the verdict and lets \
         the queue land it, and `/queue rm {id}` drops the entry.",
        said = the_refusal(review),
        reviewed = review_half(review),
        wt = entry
            .worktree
            .as_deref()
            .unwrap_or("(none — the branch is not checked out)"),
        short = letibot_sessionlog::registry::short_id(&entry.id),
        id = entry.id,
    )
}

/// **What both refusal sentences open with** — the verdict word and whether anybody has overridden
/// it. One function because the child's sentence and the enqueuer's are two addresses of one fact,
/// and two copies of this would be two accounts of whether the branch was judged.
fn the_refusal(review: Option<&letibot_tokencore::store::ReviewRecord>) -> String {
    match review {
        Some(r) if r.decision.is_some() => format!(
            "The gatekeeper refused it: its verdict on this branch is `{}`, and the review is what \
             refused it — nobody has overridden it.",
            r.decision.as_deref().unwrap_or("no verdict")
        ),
        // A refusal whose row is gone is not a refusal anybody can act on with a reason, and the
        // reader is told exactly that rather than left to infer a judgement from a silence.
        _ => {
            "The gatekeeper refused it: the branch does not land as it stands, and the review row \
              it was refused on is no longer in the queue."
                .to_string()
        }
    }
}

/// **The tail both sentences carry** — the part a child acts on, and the reason they are one
/// function: which branch, which entry, which worktree, and what has to happen before anything
/// lands. Two copies of this would be two answers to *what do I do now*.
fn back_to_work(entry: &MergeEntry, said: &str, reviewed: &str) -> String {
    format!(
        "Your branch `{}` was taken out of the merge queue: it is NOT landing as it stands, and \
         the entry `{}` is back with you.\n\n{said}\n\n{reviewed}\n\nCarry on in the worktree at \
         `{}` — the work you do there is what the entry is reviewed against next. Nothing lands \
         until a review accepts it and the gate is green.",
        entry.branch,
        entry.id,
        entry
            .worktree
            .as_deref()
            .unwrap_or("(none — the branch is not checked out)")
    )
}

/// **The review's own half of a sentence that goes back to a child** — the reviewer's reasons when
/// it wrote any, the attempt's failure when the gatekeeper could not be asked, and *nobody has
/// judged* only when the row holds neither.
///
/// One function because both [`veto_message`] and [`refusal_message`] carry it: a person's veto of
/// a branch the gatekeeper already refused must quote the refusal, and a gatekeeper's own refusal
/// is that same paragraph.
fn review_half(review: Option<&letibot_tokencore::store::ReviewRecord>) -> String {
    let reviewed = match review {
        Some(r) if !r.reasons.is_empty() => {
            let mut out = format!(
                "The review's verdict was `{}`, and this is what it said:\n",
                r.decision.as_deref().unwrap_or("no verdict")
            );
            for reason in &r.reasons {
                out.push_str(&format!("\n  - {reason}"));
            }
            out
        }
        // **A failed attempt is not a verdict**, and the child must not read one as the other:
        // nobody judged this branch, the reviewer could not be asked.
        Some(r) if !r.failure.is_empty() => format!(
            "The gatekeeper could not be asked — {} — so nothing has judged the branch.",
            first_line(&r.failure)
        ),
        // **A verdict with no reasons is still a verdict.** `reject` and an empty `reasons:` line
        // is a reviewer that judged and wrote nothing down, and the row is the whole of what it
        // said — *nobody has judged* would be the one sentence that is false about it.
        Some(r) if r.decision.is_some() => format!(
            "The review's verdict was `{}`, and it wrote no reasons down: the review row is the \
             whole of what it said.",
            r.decision.as_deref().unwrap_or("no verdict")
        ),
        _ => "Nobody has judged the branch yet.".to_string(),
    };
    reviewed
}

/// **Veto one entry** — the person's rejection, delivered to the child that did the work.
///
/// **This is not a park.** The ball goes back: the entry's original subagent — the entry's own id,
/// which is the child's session (`entry_for_finished`) — is sent a message carrying the person's
/// words and, when there was one, the review's reasons, and that message starts its turn
/// (`CommandKind::Message`, answered by `serve_child`'s `Hear` arm). The entry is parked `vetoed`
/// meanwhile: a state the queue never acts on, so no gatekeeper is asked again about a branch the
/// child has not touched yet, and the row reads as a decision somebody made rather than as a
/// gate's failure.
///
/// **The message is sent BEFORE the row is written, and a delivery that failed moves nothing.**
/// A child that is gone cannot be sent back to work, and the answer says which happened rather
/// than pretending the ball went back: the refusal names the child and the two moves that are
/// left (`/queue approve`, `/queue rm`).
///
/// `deliver` is a seam like `events` — the daemon's `task_message` door — so the act is a function
/// of a store and two callbacks and a test can drive it without a socket.
pub fn veto(
    store: &Store,
    entry_id: &str,
    why: &str,
    now_ms: u64,
    deliver: &dyn Fn(&str, &str) -> Result<String, String>,
    events: &dyn Fn(letibot_sessionlog::SessionEvent),
) -> Result<String, String> {
    let entry = read_entry(store, entry_id)?;
    vetoable(&entry)?;
    let review = store
        .merge_review(entry_id)
        .map_err(|e| format!("the review of `{entry_id}` could not be read: {e}"))?;

    let said = veto_message(&entry, why, review.as_ref());
    let delivery = deliver(&entry.id, &said).map_err(|e| {
        format!(
            "the veto of `{}` was NOT delivered to `{}`: {e} Nothing was moved — the entry is \
             where it was, and the queue will go on holding it. `/queue approve {entry_id}` \
             overrides the review and lets the queue land it, and `/queue rm {entry_id}` drops \
             the entry.",
            entry.branch, entry.id
        )
    })?;

    let reason = match why.trim() {
        "" => "nothing said".to_string(),
        w => format!("\"{w}\""),
    };
    let evidence = format!(
        "vetoed by the operator: {reason}. The child `{entry_id}` has been sent back to work; the \
         branch and its worktree are untouched, and the queue will not take the entry again until \
         it is enqueued. `/queue restart {}` asks the gatekeeper about it as it is now, \
         `/queue approve {entry_id}` overrides the review, `/queue rm {entry_id}` drops it.",
        letibot_sessionlog::registry::short_id(entry_id)
    );
    if !store
        .veto_entry(entry_id, &evidence, now_ms)
        .map_err(|e| format!("the veto of `{entry_id}` could not be written: {e}"))?
    {
        let now = store
            .merge_entry(entry_id)
            .ok()
            .flatten()
            .map(|e| e.state.as_str().to_string())
            .unwrap_or_else(|| "gone".into());
        return Err(format!(
            "nothing was vetoed: `{entry_id}` is `{now}` and a veto moves an entry that is waiting \
             or parked, never one the queue has taken or landed. The child HAS been sent the \
             message above — the entry is where it was."
        ));
    }
    events(letibot_sessionlog::SessionEvent::MergeEntryMoved {
        id: entry_id.to_string(),
        state: crate::mergequeue::wire_state(MergeState::Vetoed),
        evidence: evidence.clone(),
    });
    Ok(format!(
        "vetoed `{}` (`{entry_id}`): {reason}, and the child has it back — {delivery} The entry is \
         parked as `vetoed`, which is a decision somebody made rather than a gate's failure, and \
         the queue will not take it again while it stands. `/queue restart {}` asks the \
         gatekeeper about the branch as it is now, `/queue approve {entry_id}` overrides the \
         review, and `/queue rm {entry_id}` drops the entry.",
        entry.branch,
        letibot_sessionlog::registry::short_id(entry_id)
    ))
}

/// **Drop one entry from the queue** — the third of the person's verbs.
///
/// The store forgets the entry and its verdict ([`Store::remove_merge_entry`]); the branch and its
/// worktree stay, because they were never the queue's to delete and a person who wanted the tree
/// gone has `git` and the worktree cleanup for it. What is here is the DECISION
/// ([`removable`]), the sentence and the announcement.
///
/// **The announcement is an absence and the queue's own vocabulary has no word for one**, which is
/// why `MergeEntryRemoved` exists: a head that folded the entry in has to be told it went, or an
/// open pane goes on drawing a row the queue no longer holds.
pub fn remove(
    store: &Store,
    entry_id: &str,
    events: &dyn Fn(letibot_sessionlog::SessionEvent),
) -> Result<String, String> {
    let entry = read_entry(store, entry_id)?;
    let review = store
        .merge_review(entry_id)
        .map_err(|e| format!("the review of `{entry_id}` could not be read: {e}"))?;
    removable(&entry, review.as_ref())?;
    let evidence = removed_evidence(&entry);
    if !store
        .remove_merge_entry(entry_id)
        .map_err(|e| format!("the removal of `{entry_id}` could not be written: {e}"))?
    {
        // The row moved under us between the read and the write: the daemon claimed it, or it
        // landed. The sentence names the state rather than guessing, `restart`'s own shape.
        let now = state_now(store, entry_id);
        return Err(format!(
            "nothing was removed: `{entry_id}` is `{now}` and a removal takes an entry the queue \
             has not taken and has not landed. The row is still there, with its reason on it."
        ));
    }
    events(letibot_sessionlog::SessionEvent::MergeEntryRemoved {
        id: entry_id.to_string(),
        evidence,
    });
    Ok(format!(
        "removed `{}` (`{entry_id}`) from the queue, with its verdict. The branch and its worktree \
         are untouched: the queue has forgotten the entry, not the work. Nothing will land it now \
         — a new `task_start` on the same branch is how it would come back.",
        entry.branch
    ))
}

/// **The evidence on a removal** — the queue's own words for an ABSENCE, and one sentence for
/// `rm` and `clean` both because the fact is one fact: the queue has forgotten the entry, not the
/// work.
fn removed_evidence(entry: &MergeEntry) -> String {
    format!(
        "removed from the queue by the operator. The branch `{}` and its worktree are untouched — \
         the queue has forgotten the entry, not the work.",
        entry.branch
    )
}

/// **What an entry's row says NOW** — for the sentences about a write that did not happen: a row
/// that moved under the caller between its read and its write, or one that is gone. The state's
/// own word rather than a guess, and `gone` for an id that no longer resolves at all.
fn state_now(store: &Store, entry_id: &str) -> String {
    store
        .merge_entry(entry_id)
        .ok()
        .flatten()
        .map(|e| e.state.as_str().to_string())
        .unwrap_or_else(|| "gone".into())
}

/// **Why a whole-queue clean left `entry` alone, in one clause** — the state, plus the one detail
/// the state word does not carry: a `waiting` row a clean refused is a row with a review in
/// flight, which is the only reason [`removable`] refuses a state it otherwise allows.
///
/// This is a clause for a LIST, not the decision: whether a row goes is [`removable`]'s answer and
/// nothing else's, and a whole-queue purge that printed that function's full sentence for every
/// row it skipped would be a wall nobody reads.
fn kept_state(
    entry: &MergeEntry,
    review: Option<&letibot_tokencore::store::ReviewRecord>,
) -> String {
    if attempt_in_flight(review) {
        format!("{} — a gatekeeper is working on it", entry.state.as_str())
    } else {
        entry.state.as_str().to_string()
    }
}

/// **Forget rows from the queue** — `/queue clean [ENTRY-ID]`, and the sentence it answers with.
///
/// The operator's ruling, in their words: *"/queue clean deletes"*. One row when an id is named,
/// every row a clean may take when it is not. **Deleting is legal here**: the append-only trigger
/// (`transcript_item_no_delete`) is on `transcript_item`, not on the queue, so a `DELETE` from
/// `merge_queue` is a statement this store is written to run.
///
/// **What may be taken is [`removable`]'s rule and nothing else** — the rule `rm` is built on, so
/// the two verbs cannot disagree about a row. A `landed` row stays (it is the queue's record that
/// main moved and the base a dependent rebases onto), a `taken` row stays (the daemon is landing
/// it right now), and a row with a reviewer mid-answer stays (the verdict would be written onto a
/// row that is gone).
///
/// **Named, a refusal is the answer**, in `removable`'s own words: the operator pressed the verb
/// on one row, and a sentence about why it stays is the honest reply. **Unnamed, the sweep goes
/// on and the refusals are REPORTED**, one per row, because a purge that silently skipped a row
/// is a purge nobody can trust — and the row that went is named by its branch and the state it
/// was in, so a person can see what was deleted rather than take the count on faith.
///
/// The branch and the worktree are untouched, `remove`'s rule and its reason: they were never the
/// queue's to delete.
pub fn clean(
    store: &Store,
    entry: Option<&str>,
    events: &dyn Fn(letibot_sessionlog::SessionEvent),
) -> Result<String, String> {
    let about: Vec<MergeEntry> = match entry {
        Some(id) => vec![read_entry(store, id)?],
        None => store
            .merge_entries()
            .map_err(|e| format!("the queue could not be read: {e}"))?,
    };
    let mut went: Vec<MergeEntry> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for e in &about {
        let review = store
            .merge_review(&e.id)
            .map_err(|err| format!("the review of `{}` could not be read: {err}", e.id))?;
        if let Err(why) = removable(e, review.as_ref()) {
            if entry.is_some() {
                return Err(why);
            }
            kept.push(format!(
                "`{}` ({})",
                e.branch,
                kept_state(e, review.as_ref())
            ));
            continue;
        }
        // **One write per row, and it is `rm`'s own** ([`Store::remove_merge_entry`]): the state
        // guard is the row's, so a row the daemon claims between the read and this write is not
        // deleted out from under it.
        match store.remove_merge_entry(&e.id) {
            Ok(true) => {
                events(letibot_sessionlog::SessionEvent::MergeEntryRemoved {
                    id: e.id.clone(),
                    evidence: removed_evidence(e),
                });
                went.push(e.clone());
            }
            // The row moved under us: named, that is `remove`'s refusal in the state's own word;
            // unnamed, it is a row that stayed and the report says which.
            Ok(false) => {
                if entry.is_some() {
                    return Err(format!(
                        "nothing was removed: `{}` is `{}` and a removal takes an entry the queue \
                         has not taken and has not landed. The row is still there, with its \
                         reason on it.",
                        e.branch,
                        state_now(store, &e.id)
                    ));
                }
                kept.push(format!("`{}` ({})", e.branch, state_now(store, &e.id)));
            }
            Err(err) => return Err(format!("`{}` could not be removed: {err}", e.id)),
        }
    }
    let list = |rows: &[String]| rows.join(", ");
    let kept_list = list(&kept);
    if went.is_empty() {
        // **Nothing went, and that is a real answer rather than a failure.** The whole-queue form
        // of a clean on a queue that is all green is *there is nothing to purge*, and the rows
        // that stayed are named with the rule that kept them.
        return Ok(format!(
            "nothing was cleaned: every row the queue holds is one a clean may not take — \
             {kept_list}. A `landed` row is the queue's record that main moved and the base an \
             entry that needs it rebases onto, a `taken` one is being landed right now, and a row \
             with a gatekeeper mid-answer would have its verdict written onto a row that is gone. \
             The branches and their worktrees are untouched."
        ));
    }
    let went_list = went
        .iter()
        .map(|e| format!("`{}` ({})", e.branch, e.state.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let word = if went.len() == 1 { "entry" } else { "entries" };
    let stayed = if kept.is_empty() {
        String::new()
    } else {
        format!(
            " {} row(s) stayed: {kept_list} — a `landed` row is the base a dependent rebases \
             onto, a `taken` one is being landed right now, and a row with a gatekeeper \
             mid-answer would have its verdict written onto a row that is gone.",
            kept.len()
        )
    };
    Ok(format!(
        "cleaned {} {word} from the queue, with their verdicts: {went_list}. The branches and \
         their worktrees are untouched — the queue has forgotten the entries, not the work. \
         Nothing will land them now: a new `task_start` on the same branch is how one would come \
         back.{stayed}",
        went.len()
    ))
}

/// The first line of a multi-line failure, for a sentence that has to fit on a row.
///
/// A gate's failure and a provider's refusal are both blocks of text; the overlay wraps the
/// whole of one, and a sentence ABOUT it names only where it starts. `clean_line` in the pane
/// joins the rest anyway, so nothing is lost by taking the first line here and nothing is
/// gained by carrying four kilobytes into a sentence whose subject is *which* failure it was.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

/// **What an entry's row says between two attempts** — the failure's own words FIRST, then the
/// queue's.
///
/// The order is the whole of it. The pane draws an entry's reason on a line it hard-truncates at
/// the pane's width, and a sentence that opened with *the gatekeeper's attempt failed* would use
/// the room a person needs for `http 429: Weekly/Monthly Limit Exhausted. Your limit will reset
/// at …`. So the provider's sentence goes first and the queue's explanation after it, which is
/// also the order a reader wants them in: what happened, then what is being done about it. The
/// overlay (`Enter` on the row) wraps the whole thing rather than eliding it, so nothing is lost
/// to the row's width.
pub fn review_failed_evidence(
    review: Option<&letibot_tokencore::store::ReviewRecord>,
    in_ms: u64,
) -> String {
    let failure = review
        .map(|r| r.failure.trim())
        .filter(|f| !f.is_empty())
        .unwrap_or("the gatekeeper's attempt failed");
    let secs = in_ms.div_ceil(1000).max(1);
    format!("{failure} — the gatekeeper's attempt failed; the queue asks again in {secs}s")
}

/// **What an entry's row says when the attempts are spent** — the same order, and the verb.
///
/// [`review_failed_evidence`]'s sibling, and the last sentence is why this is a function: a
/// person reading a parked entry has to be able to find the thing that moves it. The short id is
/// the one the pane draws (`registry::short_id`), so the verb in the sentence is a verb on the id
/// the reader can see rather than on one forty characters long.
pub fn review_exhausted_evidence(
    entry: &MergeEntry,
    review: Option<&letibot_tokencore::store::ReviewRecord>,
) -> String {
    let attempts = review.map(|r| r.attempts).unwrap_or(0);
    let failure = review
        .map(|r| r.failure.trim())
        .filter(|f| !f.is_empty())
        .unwrap_or("the gatekeeper could not be asked");
    format!(
        "{failure} — the gatekeeper's attempt failed {attempts} times and the queue has stopped \
         asking. Nothing judged this branch. `/queue restart {}` asks again.",
        letibot_sessionlog::registry::short_id(&entry.id)
    )
}

/// **Where the branch is, as a clause that stands on its own** — the worktree, or the fact that
/// there is none and the project has to make one.
///
/// One function for the two readers because it is one fact and both sentences have to name it the
/// same way: a row that said *the worktree `/w/b`* and a message that said *wherever it is* would
/// be two descriptions of one path, and the second is the one a model acts on.
///
/// **No worktree is not an error here.** An entry is enqueued before `task_start` has created its
/// tree (`MergeQueueDaemon::step`'s own *no worktree: the branch is not checked out*), and a
/// branch that is checked out nowhere is a branch the project has to check out — which is what
/// this says, rather than guessing at a path.
fn where_the_branch_is(entry: &MergeEntry) -> String {
    match entry.worktree.as_deref() {
        Some(w) if !w.is_empty() => format!("the branch is checked out in the worktree `{w}`"),
        _ => "the branch is checked out in no worktree the queue knows, so it has to be checked \
              out before it can be gated"
            .to_string(),
    }
}

/// **The session that owns an entry's branch, resumed if this daemon does not hold it** — the root
/// of the session tree the entry came from ([`root_session`]'s walk up the stored parents), found
/// in the registry or in the store.
///
/// # One door for both halves of the fallback
///
/// Two places tell a project about its own entry, and they run on different threads: the queue's
/// own pass (through `GatekeeperDoor`, which holds the store and the registry) and the review
/// queue's SESSION, whose refusal could not be delivered to the child that did the work
/// ([`hand_over_from_session`]). *Which session, and how it is found* must have one answer — a
/// second spelling of the walk would let one of them ring a session the other would have resumed,
/// or refuse an entry the other would have reached.
///
/// `what` says what the session is wanted for, and it is the refusal's own words when there is
/// none: a session in neither the registry nor the store is one nothing can reach, and the
/// sentence names the session and the branch rather than reporting a message that went nowhere.
pub fn project_session(
    store: &Store,
    registry: &letibot_sessionlog::registry::Registry,
    entry_id: &str,
    what: &str,
) -> Result<(MergeEntry, String), String> {
    let entry = read_entry(store, entry_id)?;
    let host = root_session(store, &entry.session_id);
    if registry.get(&host).is_none() {
        match registry.resumable(&host) {
            Some(brief) => {
                registry
                    .create(&host, &brief.title, brief.wiring)
                    .map_err(|e| {
                        format!(
                            "the session `{host}` that `{entry_id}` came from could not be resumed \
                             to {what}: {e}"
                        )
                    })?;
            }
            None => {
                return Err(format!(
                    "the session `{host}` that `{entry_id}` (branch `{}`) came from is in neither \
                     this daemon nor the store, so there is no session to {what}. The branch does \
                     not land without it.",
                    entry.branch
                ));
            }
        }
    }
    Ok((entry, host))
}

/// **Hand an entry to its own project, from a session rather than from the queue's thread** — the
/// row, then the ring, for the one kind of handover that does not happen on the queue's pass.
///
/// The queue's own handovers ([`NoGatekeeper`]) are made by [`MergeQueueDaemon::hand_over`], which
/// has the entry, the state and the door. This is the other one: the review queue's SESSION has
/// just found that its refusal could not be delivered to the child that did the work
/// (`Harness::send_a_refusal_back`), the entry is parked with the verdict on its row, and the only
/// session that can put the work back in front of the review is the root of the tree it came from.
///
/// **The verdict stays in FRONT of the sentence this appends**, which is the row rule
/// [`no_gatekeeper_evidence`] states one kind over: the pane truncates, and *why the branch is
/// parked* is the part a person reads there. What is appended is the clause
/// ([`REFUSAL_UNDELIVERED`]) and what became of the ring.
///
/// **Once, and read off the row** — [`handed_over`] is the guard, the same one the queue's own
/// handover uses, so a second failure to deliver does not tell the project twice.
///
/// Returns the sentence for the caller's own log (the Warning it was about to publish), and an
/// `Err` when there is nobody to tell: *the project is gone too*, which the caller says out loud
/// rather than swallowing.
pub fn hand_over_from_session(
    store: &Store,
    registry: &letibot_sessionlog::registry::Registry,
    entry_id: &str,
    why: &str,
) -> Result<String, String> {
    let entry = read_entry(store, entry_id)?;
    if handed_over(&entry) {
        return Ok(format!(
            "the project that owns `{}` was told once already, and nothing was said twice",
            entry.branch
        ));
    }
    let reason = format!("{} — {why}", handover_reason(&entry));
    // **The row first**, because the message the project reads is built out of it.
    let mut moved = entry.clone();
    moved.evidence = refusal_undelivered_evidence(&entry, &reason, None);
    write_and_announce(store, registry, &moved)?;
    // **And a ring that could not happen is written on the row**, not only returned: *the project
    // was told* and *there was nobody to tell* are the two facts a person acts differently on, and
    // the caller's log is not where they look. `no_gatekeeper_evidence`'s own two arms, one kind
    // over.
    let rung = project_session(
        store,
        registry,
        entry_id,
        "act on the review's refusal and put the work back in the queue",
    )
    .and_then(|(_, root)| {
        if registry.bell().is_closed() {
            return Err(format!(
                "the daemon's bell is closed (it is shutting down), so `{root}` was not rung about \
                 `{entry_id}`. The row carries the handover and the next daemon rings for it."
            ));
        }
        registry.bell().ring_wake(&root);
        Ok(root)
    });
    let root = match rung {
        Ok(root) => root,
        Err(e) => {
            moved.evidence = refusal_undelivered_evidence(&entry, &reason, Some(&e));
            write_and_announce(store, registry, &moved)?;
            return Err(e);
        }
    };
    Ok(format!(
        "the session `{root}` that `{entry_id}` (branch `{}`) came from was rung: {why}",
        entry.branch
    ))
}

/// **Write an entry's row and tell the heads** — the first act of a handover, in one place so that
/// the path where the ring failed cannot forget the announcement: a head that folded the entry
/// would otherwise go on drawing the row without the clause the second write put on it.
fn write_and_announce(
    store: &Store,
    registry: &letibot_sessionlog::registry::Registry,
    entry: &MergeEntry,
) -> Result<(), String> {
    store
        .put_merge_entry(entry)
        .map_err(|e| format!("the handover of `{}` could not be written: {e}", entry.id))?;
    registry.broadcast(letibot_sessionlog::SessionEvent::MergeEntryMoved {
        id: entry.id.clone(),
        state: crate::mergequeue::wire_state(entry.state),
        evidence: entry.evidence.clone(),
    });
    Ok(())
}

/// **What an entry's row says when a refusal could not be delivered** — the second kind of
/// handover, on the row. See [`refusal_undelivered_message`] for the sentence the project gets.
///
/// Private, because the two callers that write this row are both in this module
/// ([`hand_over_from_session`] and its own tests): the daemon's handovers go through
/// [`no_gatekeeper_evidence`], and a public constructor for a row only one caller writes would be
/// a second way to spell it.
fn refusal_undelivered_evidence(entry: &MergeEntry, why: &str, told: Option<&str>) -> String {
    let branch = &entry.branch;
    let id = &entry.id;
    let short = letibot_sessionlog::registry::short_id(id);
    let tree = where_the_branch_is(entry);
    match told {
        None => format!(
            "{why} — {REFUSAL_UNDELIVERED}: the entry is handed to the project it came from, which \
             is told that the branch does NOT land as it stands and how to put it back in the queue \
             ({tree}). A person can move it too: `/queue approve {id}` lets it through on the \
             operator's own verdict, `/queue restart {short}` asks a gatekeeper again, \
             `/queue rm {id}` drops the entry."
        ),
        Some(what) => format!(
            "{why} — {REFUSAL_UNDELIVERED}, and nobody could be told either: {what} `{branch}` is \
             left exactly as it is ({tree}). A person is the last resort: `/queue approve {id}` \
             lets it through on the operator's own verdict, `/queue restart {short}` asks a \
             gatekeeper again, `/queue rm {id}` drops the entry."
        ),
    }
}

/// **What an entry's row says when no gatekeeper can judge it** — the fallback, on the row.
///
/// The order is [`review_failed_evidence`]'s and for its reason: the queue's own reason for what
/// happened goes FIRST, because the pane draws a row on one hard-truncated line and *why* is what
/// a person reads there; the handover ([`NO_GATEKEEPER`]) and the person's verbs follow, and the
/// overlay wraps the whole thing.
///
/// `told` is what became of the message to the project's session:
///
/// * **`None`** — the row is being written BEFORE the ring, and that order is forced: the message
///   the project's model reads is read out of this row (`Harness::handover_notices`), so the
///   row has to be there before the bell is rung. It is `gate_missing`'s order, one entry over.
/// * **`Some(what)`** — the door could not reach anybody, and the row then carries that instead:
///   *the project was told* and *there was nobody to tell* are the two facts a person acts
///   differently on, and stderr is not where they look.
pub fn no_gatekeeper_evidence(entry: &MergeEntry, why: &str, told: Option<&str>) -> String {
    let branch = &entry.branch;
    let id = &entry.id;
    let short = letibot_sessionlog::registry::short_id(id);
    let tree = where_the_branch_is(entry);
    match told {
        None => format!(
            "{why} — {NO_GATEKEEPER}: it is told to gate `{branch}` and land it itself ({tree}), \
             and the queue records the landing the moment main has the branch. A person can move \
             it too: `/queue approve {id}` lets it through on the operator's own verdict, \
             `/queue restart {short}` asks a gatekeeper again."
        ),
        Some(what) => format!(
            "{why} — {NO_GATEKEEPER}, and nobody could be told either: {what} `{branch}` is left \
             exactly as it is ({tree}). A person is the last resort: `/queue approve {id}` lets it \
             through on the operator's own verdict, `/queue restart {short}` asks a gatekeeper \
             again, `/queue rm {id}` drops the entry."
        ),
    }
}

/// **The queue's own reason, read back off the row** — everything before the handover clause,
/// which is the half a message ABOUT the entry should quote.
///
/// The clause itself is about what the queue did with the entry, so a message that quoted it would
/// be telling the project what the project has already been told. The split is a function rather
/// than a second field on the row: the row is one sentence, both readers read it, and the reason is
/// exactly the part of it that is not the handover.
///
/// **Whichever clause is on the row**, because the two kinds of handover lead their rows the same
/// way — the reason first, the clause after it — and a reader that only knew about one of them
/// would hand a message the whole of its own evidence as its reason.
pub fn handover_reason(entry: &MergeEntry) -> String {
    let at = [NO_GATEKEEPER, REFUSAL_UNDELIVERED]
        .iter()
        .filter_map(|clause| entry.evidence.find(clause))
        .min()
        .unwrap_or(entry.evidence.len());
    entry.evidence[..at]
        .trim_end_matches([' ', '-', '\u{2014}', ':', '.'])
        .trim()
        .to_string()
}

/// **What the project's own session is told about an entry the queue has handed it** — the one
/// place the two kinds are told apart, so a message can never be the wrong one for the row it
/// quotes.
///
/// The kinds are the two clauses ([`NO_GATEKEEPER`], [`REFUSAL_UNDELIVERED`]) and the split is
/// forced: *no gatekeeper could judge this, so gate it and land it yourself* is the right thing to
/// tell a project about a branch nobody judged, and the worst possible thing to tell it about a
/// branch a review refused.
pub fn handover_message(entry: &MergeEntry) -> String {
    if entry.evidence.contains(REFUSAL_UNDELIVERED) {
        refusal_undelivered_message(entry)
    } else {
        no_gatekeeper_message(entry)
    }
}

/// **What the project's own session is told when no gatekeeper can judge its branch** — the
/// operator's fallback, as the message: *"the queue tooling can send a message back to the main
/// project session and projects will gate and merge on their own"*, *"because their code lives
/// inside them in worktrees anyway"*.
///
/// # How the project's own gate and merge are reached from a MESSAGE and not from a click
///
/// The message is a turn in the project's session, and it names every fact the project needs to
/// finish the entry without anybody clicking anything:
///
/// * **What it is about** — the entry id and the branch, in the first line, because a message
///   about a branch nobody can name is not a message a model can act on.
/// * **Where the work is** — `where_the_branch_is`, which is the worktree the branch is checked
///   out in. That is the operator's *"their code lives inside them in worktrees anyway"*: the
///   project does not have to fetch, copy or reconstruct anything.
/// * **What the gate is** — the `Merge gate` section of `main`'s `AGENTS.md`
///   ([`gate_on_main`]), which the `merge_gate` tool lists for the operator to choose from. The
///   project runs those commands itself, in the worktree.
/// * **What the merge is** — merging the branch into main and pushing. Then the queue's next pass
///   reads main, sees it contains the branch, and records the landing itself
///   (`MergeQueueDaemon::land_what_main_has`): the row goes `landed` and the worktree and the
///   branch are cleaned up by the state's own rule ([`cleanup_for`]).
/// * **The other door, which does need the operator** — `/queue approve ENTRY-ID`, which is the
///   queue's own gate and its own rebase. Named second, and named at all, because a project that
///   does not want to touch main itself has to be able to say so.
///
/// The reason is read back off the row ([`handover_reason`]) rather than passed in, because the
/// row is what the project can also read and the two must not be able to disagree.
pub fn no_gatekeeper_message(entry: &MergeEntry) -> String {
    let branch = &entry.branch;
    let id = &entry.id;
    format!(
        "The merge queue cannot get a verdict on `{branch}` (entry `{id}`), so it will not land \
         through the queue's gate on its own: {reason}\n\
         \n\
         The work is yours and so is the gate: {tree}. This repository's gate is the `Merge gate` \
         section of `AGENTS.md` on main, and the `merge_gate` tool lists what it asks for. Two \
         ways to finish the entry, and the first needs nobody:\n\
         * Gate it and land it yourself. Run those commands where the branch is checked out, then \
         merge `{branch}` into main and push. The queue's next pass reads main, sees that it \
         contains the branch, and moves the entry to `landed` — removing the worktree and deleting \
         the branch, the cleanup every landing owns. No verdict is involved and nothing has to be \
         approved.\n\
         * Or leave it to the operator: `/queue approve {id}` lets the entry through on the \
         operator's own verdict, and the queue then rebases it at the tip of main, runs the gate \
         and lands it; `/queue rm {id}` drops the entry and leaves the branch and its worktree \
         alone.",
        reason = handover_reason(entry),
        tree = where_the_branch_is(entry),
    )
}

/// **What the project's own session is told when a review REFUSED its branch and the complaints
/// could not be delivered** — the second kind of handover, and the one sentence that must not be
/// the first kind's.
///
/// The operator's principle is the same one — *"a person is the last resort, not the mechanism"* —
/// and the act the project is asked for is the opposite of the no-gatekeeper case's: the branch
/// does **not** land as it stands, the verdict on the entry's row says why, and the work is to
/// answer it and put the branch back in the queue. So this message names the verdict, the row it
/// is on, and the door a re-enqueue goes through — and it says in as many words that landing it is
/// not the move, because a project told to *gate it and land it yourself* about a refused branch
/// would be doing the one thing the queue exists to prevent.
///
/// **Why the project is told at all.** The queue's own answer to a refusal is to send the
/// complaints back to the CHILD that did the work ([`refusal_message`], and `TaskRunner::send` is
/// how they travel) — and when that child's session is gone, the delivery fails and the entry is
/// parked with nobody in the queue able to move it. The root of the tree that queued it is the
/// session that owns the worktree, and the only one that can put the work back in front of the
/// review.
pub fn refusal_undelivered_message(entry: &MergeEntry) -> String {
    let branch = &entry.branch;
    let id = &entry.id;
    format!(
        "The merge queue cannot finish with `{branch}` (entry `{id}`): {reason}\n\
         \n\
         **The branch does NOT land as it stands.** {tree}, and the verdict the review reached is on \
         the entry's row — read it there, because it is the whole of what the queue knows about why \
         this did not go in. Nothing in the queue will move the entry on its own: the child that \
         did the work is gone, which is why you are being told rather than it. What finishes it:\n\
         * Answer the review in the worktree, then put the branch back in the queue — a new \
         `task_start` on `{branch}` enqueues it again, and the queue reviews the branch as it is \
         then. That is the door the queue expects, and it is the one that needs no verdict from \
         anybody.\n\
         * Or leave it to the operator: `/queue approve {id}` lets the entry through on the \
         operator's own verdict and the queue then rebases, gates and lands it; `/queue restart \
         {id}` asks a gatekeeper again about the branch as it is now; `/queue rm {id}` drops the \
         entry and leaves the branch and its worktree alone.",
        reason = handover_reason(entry),
        tree = where_the_branch_is(entry),
    )
}

/// **What the daemon calls the work IT starts** — the gate run for an entry, and the review of
/// one.
///
/// # Why the daemon needs a name at all
///
/// `j57` is a counter and `s-1789…-sub-1791…` is a timestamp: a person looking at the work in
/// flight cannot tell one of the daemon's own runs from another. A job the MODEL starts carries
/// a name it stated (`bash(slug: "release-build")`, [`letibot_tools::exec::job_name`]); the two
/// pieces of work the daemon starts on its own account — the **gate run** it does for an entry
/// ([`MergeQueueDaemon::step`]) and the **review** it asks for ([`crate::harness::Harness::serve_reviews`]) —
/// carried none, so they were the only rows a reader could not tell apart. The operator's word
/// for it: *"and no slugs for jobs / sad"*.
///
/// # What a name is made of
///
/// **One word for the KIND, a separator, then the branch it is about**: `gate-agent/foo` and
/// `review-agent/foo`. The kind is first because it is the part that has to survive a cap, and
/// because *a gate run for `agent/foo`* and *a review of `agent/foo`* are two different jobs —
/// they are even, for one entry, two different runs of the queue.
///
/// **And the separator is `-` and not a space, deliberately**: a name is ONE word — that is what
/// [`letibot_tools::exec::job_name`] makes of any space — and a daemon that spelled its own
/// names the other way would be a second rule for the same field. The slash in a branch is kept:
/// it is not whitespace, it is what the branch is called, and `gate-agent/foo` is the branch a
/// person typed.
///
/// **The branch, and the entry id only when there is no branch.** A branch is what a person
/// recognises (`agent/a-row-can-wait-on-a-job`); an entry id is what they act with (`/queue
/// approve <id>`), and a `task_start` entry's id is forty characters of session id that would
/// spend the whole name. So the id is the FALLBACK — an entry with no branch (a plain `task`
/// child's work, or a row written by a shape this build does not know) is still nameable, and a
/// name is never empty: `gate` alone is the last resort, and it still says what KIND of job this
/// is.
///
/// # Bounded and sanitised exactly as a tool's slug is, and by the same code
///
/// The words go through [`letibot_tools::exec::job_name`] — the ONE rule, not a second spelling
/// of it: whitespace and control characters become `-` and the result is capped at
/// [`letibot_tools::exec::MAX_JOB_NAME`]. Two flattenings of *what a name is* is how a row comes
/// to be drawn one way in the pane and another in the transcript.
///
/// **The sanitising is a formality here and the cap is not.** These words are this module's own
/// — a branch name git accepted and an id this tree minted — so there is no control character to
/// flatten; but a branch is a human's sentence and can be longer than a row, so the cap bites
/// and it bites the TAIL (`gate-agent/some-very-long-…` reads as the branch it is; the end of it
/// is on the entry's own row, beside this name).
fn daemon_job_name(what: &str, entry: &MergeEntry) -> String {
    let about = if entry.branch.trim().is_empty() {
        entry.id.trim()
    } else {
        entry.branch.trim()
    };
    letibot_tools::exec::job_name(&format!("{what}-{about}")).unwrap_or_else(|| what.to_string())
}

/// **The name the daemon's gate run for `entry` goes under** — `gate-agent/foo`.
///
/// Written on the entry's own row while the run is going ([`MergeQueueDaemon::step`]), which is
/// where the queue's words for a move are read: a `Taken` row said *"rebasing at the tip"* for
/// the whole of a `cargo test` that takes minutes, and a reader could not tell a gate run from a
/// rebase or from the entry sitting still. See [`daemon_job_name`] for what a name is made of.
pub fn gate_job_name(entry: &MergeEntry) -> String {
    daemon_job_name("gate", entry)
}

/// **The name the daemon's review of `entry` goes under** — `review-agent/foo`.
///
/// Written on the queue session's own board row ([`crate::sessions::gatekeeper::row`]), which is
/// where a review is read: that row is what the todos pane draws, what the idle check nags about,
/// and what says which child is working it. The row spelled its name itself before this —
/// `review \`agent/foo\` (entry …)` — and that was a second spelling of a fact the daemon has one
/// rule for.
///
/// **And the reviewer's own prompt is deliberately NOT named by this**, which is worth writing
/// down because it is the tempting place: a `task` child's name is the first line of its task, and
/// a gatekeeper child is not a row the operator sees — the head drops it by its role
/// (`crate::harness::Harness::serve_reviews` sets `gatekeeper`) and by
/// `letibot_sessionlog::GATEKEEPER_TITLE_PREFIX` for a row rebuilt from the session list, because
/// the operator's ruling is *"it shouldnt be visible in 'normal' agents count"*. A name line in
/// front of that prompt would be a name no pane draws AND would break the mark, so the prompt is
/// left as it is and the name goes where a review is actually read.
///
/// See [`daemon_job_name`] for what a name is made of.
pub fn review_job_name(entry: &MergeEntry) -> String {
    daemon_job_name("review", entry)
}

/// **The daemon thread's half of the merge queue** — the thing that takes the next entry and
/// serves it toward main.
///
/// The operator's ask, in their words: *"merge queue is 2 - together with persistence and
/// harnessd thread that serves it."* This is the thread: it owns a [`Store`] (its own
/// connection to the same file, for the reason the job watcher does — a
/// `rusqlite::Connection` is `Send` and not `Sync`, and the daemon's own connection is the
/// harness's), the repo path, and the gate. It takes the next ready entry, rebases it at the
/// tip, runs the gate, fast-forwards main and pushes, and cleans up by state.
///
/// **It is a thread because the gate is long.** The gate is `cargo test` and a release build,
/// which is minutes, and the daemon has nothing else to do with its hands while it runs. So
/// the gate runs on its own thread, and the daemon's worker loop is free to serve the
/// sessions while the merge runs.
///
/// **The `step` method is the loop body, and it is what the test drives.** A `step` takes the
/// next ready entry and serves it, or reports that nothing is ready. The thread calls `step`
/// in a loop; the test calls it directly, which is what lets the rebase and the fast-forward
/// be tested against a real git repo rather than a mock.
pub struct MergeQueueDaemon {
    store: Store,
    repo: PathBuf,
    /// **The gate**: the steps `main` declared, run at the tip, as a function of the worktree.
    /// It returns a [`GateRun`] — the rows the landing records (one per step, in order) and the
    /// gate's own words when it is red — rather than a bare verdict, because a landing that
    /// records only `failed` can only ever be DRAWN as `failed`.
    ///
    /// A function rather than a fixed command list, for the reason the test needs it: the
    /// production gate is `main`'s own `Merge gate` section ([`repo_gate`]), and the test gate
    /// is a no-op ([`GateRun::green`]), and both are *the gate* — the thing that runs at the tip
    /// after the rebase. The seam is the function, and the production wiring fills it with the
    /// repository's own commands.
    gate: Box<dyn Fn(&Path) -> GateRun + Send>,
    /// **Whether the repository has a gate at all**, asked before an entry is taken — `Err` is
    /// why it has not, and the entry waits with that on its row. Always ready unless set
    /// ([`Self::with_gate_check`]): the production wiring asks `main`'s AGENTS.md
    /// ([`gate_configured`]), and a test's no-op gate needs no file.
    gate_ready: Box<dyn Fn(&Path) -> Result<(), String> + Send>,
    /// **The workspace this daemon serves**, canonical — an entry is this daemon's when the
    /// root of the session that queued it was opened on it ([`Self::serves`]). `None` serves
    /// every entry, which is what a test's daemon does.
    serving: Option<PathBuf>,
    /// Each entry's repository, read once from its worktree ([`repo_of_worktree`]): a worktree
    /// does not change repositories, and the queue asks every second.
    repos: std::sync::Mutex<std::collections::HashMap<String, PathBuf>>,
    /// **The door the reviewer is asked through** — see
    /// [`letibot_tools::gatekeeper::Reviewer`].
    ///
    /// A trait object rather than a fixed call, for the reason the gate is a function: the
    /// production door writes the request into the session store and rings the daemon's bell
    /// for the reviewer's session, and the test's records that it was asked — and both are *the
    /// ask*. The seam is the door, and the daemon holds no opinion about what is behind it.
    reviewer: Box<dyn letibot_tools::gatekeeper::Reviewer + Send>,
    /// **Where the queue's moves go** — the events decision, as a seam.
    ///
    /// The daemon says WHAT happened and this says where it goes, which is the split the
    /// decision needs: *a queue is daemon-level* is a fact about the wire, and *every session's
    /// log records it* is a choice about the logs
    /// ([`letibot_sessionlog::registry::Registry::broadcast`] is the door, and its own doc
    /// carries the cost). A closure rather than a trait, for the reason the gate is one: the
    /// test's is a `Vec` push and the production one is a broadcast.
    events: Box<dyn Fn(letibot_sessionlog::SessionEvent) + Send>,
}

impl MergeQueueDaemon {
    /// Build the daemon over `store`, serving the repo at `repo`, with `gate` as the check
    /// that runs at the tip, `reviewer` as the door an entry's review is asked through, and
    /// `events` as where the queue's moves are announced.
    pub fn new(
        store: Store,
        repo: PathBuf,
        gate: Box<dyn Fn(&Path) -> GateRun + Send>,
        reviewer: Box<dyn letibot_tools::gatekeeper::Reviewer + Send>,
        events: Box<dyn Fn(letibot_sessionlog::SessionEvent) + Send>,
    ) -> Self {
        Self {
            store,
            repo,
            gate,
            gate_ready: Box::new(|_| Ok(())),
            serving: None,
            repos: Default::default(),
            reviewer,
            events,
        }
    }

    /// **Serve only the entries queued from sessions opened on `workspace`** — see
    /// [`Self::serves`].
    pub fn serving(mut self, workspace: &Path) -> Self {
        self.serving = Some(std::fs::canonicalize(workspace).unwrap_or(workspace.to_path_buf()));
        self
    }

    /// **The repository an entry lands in**: its worktree's own repository, so one queue serves
    /// every repository under its workspace — a session may run a level above them. An entry
    /// with no worktree is the daemon's own repository's.
    fn repo_for(&self, entry: &MergeEntry) -> PathBuf {
        if let Some(r) = self.repos.lock().expect("repos").get(&entry.id) {
            return r.clone();
        }
        let repo = entry
            .worktree
            .as_deref()
            .and_then(|wt| repo_of_worktree(Path::new(wt)))
            .unwrap_or_else(|| self.repo.clone());
        self.repos
            .lock()
            .expect("repos")
            .insert(entry.id.clone(), repo.clone());
        repo
    }

    /// **Whether this daemon serves `entry`**: the root of the session that queued it was
    /// opened on this daemon's workspace. One store holds every daemon's queue, and each
    /// session is served by the daemon for its workspace — so this is the rule that keeps two
    /// daemons from reviewing, resuming or landing the same entry. A root the store does not
    /// know falls back to where the entry's repository is: under this workspace, or not.
    ///
    /// The rule itself is [`served_by`], because the gatekeeper session's board reads the same
    /// question — *is this entry this daemon's to work* — and two spellings of it would let a
    /// daemon nag a session about another daemon's entry.
    fn serves(&self, entry: &MergeEntry) -> bool {
        served_by(
            &self.store,
            self.serving.as_deref(),
            entry,
            &self.repo_for(entry),
        )
    }

    /// **Hold entries until the repository has a gate** — see [`Self::gate_ready`].
    pub fn with_gate_check(
        mut self,
        check: Box<dyn Fn(&Path) -> Result<(), String> + Send>,
    ) -> Self {
        self.gate_ready = check;
        self
    }

    /// **The repo this daemon serves** — the path the git operations run in.
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// **Recover the queue on startup**: move every `Taken` entry to `Stale`, for the reason
    /// [`recover`] exists. Called once, before the first `step`, so a daemon that restarts
    /// does not come back to a queue that says `taken` about a job that is dead.
    pub fn recover(&self) -> Result<Vec<MergeEntry>, letibot_tokencore::store::StoreError> {
        let entries = self.store.merge_entries()?;
        let recovered = recover(entries);
        for entry in &recovered {
            if entry.state == MergeState::Stale {
                self.store.put_merge_entry(entry)?;
            }
        }
        Ok(recovered)
    }

    /// **One pass of the loop**: take the next ready entry and serve it, or report that
    /// nothing is ready.
    ///
    /// The pass is the whole of the daemon's work, and it is serial by construction: one
    /// entry at a time, and the gate runs on the entry *after* it is rebased onto the current
    /// tip of main. The outcome is the vocabulary the loop and the test share, and the row is
    /// written on every move, so a daemon that dies mid-pass comes back to a queue that says
    /// what happened.
    pub fn step(&self) -> Result<StepOutcome, letibot_tokencore::store::StoreError> {
        // **The clock the review's backoff reads**, taken once for the pass so two entries in one
        // pass cannot be measured against two different nows.
        let now_ms = (crate::config::now_ns() / 1_000_000) as u64;
        let entries = self.store.merge_entries()?;
        // **This daemon's entries only** — see [`Self::serves`]. The rest are another daemon's,
        // and asking, resuming or landing them from here would race it.
        let entries: Vec<MergeEntry> = entries.into_iter().filter(|e| self.serves(e)).collect();
        let reviews = self.store.reviews()?;

        // **First, the review of everything that is due** — asked for, or parked by its
        // verdict. This happens before anything is taken, because the entry that is taken is
        // the one whose verdict has come back, and an entry whose reviewer said no must be
        // parked rather than left to block the queue for ever.
        //
        // **Every pass re-asks for an outstanding review**, and that is the recovery rather
        // than a poll: the request row survives a daemon restart, and a daemon that came up
        // between the write and the bell would otherwise leave an entry waiting for a verdict
        // nobody was ever asked for. The door is what makes the re-ask cheap — it writes the
        // row only when there is not one, and ringing a bell whose session has nothing new to
        // do costs a wake that answers `Ok(None)`.
        let mut asked = false;
        // **Whether an entry was handed to its project this pass**, which is the third answer the
        // end of the loop has to be able to give — see [`StepOutcome::NoGatekeeper`]. Its own flag
        // rather than a `return`, because the rest of the entries are still asked about in this
        // pass: a queue with three entries the gatekeeper is gone for should tell three projects
        // now and not one per second.
        let mut handed = false;
        for entry in &entries {
            // **An entry the queue handed to its own project, whose branch main now has, has
            // landed** — the project's own gate and merge, seen by the queue. This is checked
            // FIRST, before the state and before the verdict, and both of those are the point:
            // the handover leaves the row `failed` (nothing will judge the branch) and there is no
            // verdict coming, so a check that waited for either would wait for ever while the
            // project's work sat on main. See [`Self::land_what_main_has`].
            if handed_over(entry)
                && let Some(tip) = self.land_what_main_has(entry)?
            {
                return Ok(StepOutcome::Landed(tip));
            }
            if entry.state != MergeState::Waiting || !is_ready(entry, &entries) {
                continue;
            }
            let review = reviews.iter().find(|r| r.entry_id == entry.id);
            match review_gate(entry, review) {
                ReviewGate::Accepted => {}
                // **`Awaiting` is two different facts, and [`review_retry`] is what tells them
                // apart.** *Nobody has asked, or an attempt is in flight* — ask, which is also
                // the recovery for a daemon that came up between the request row and the bell.
                // *The last attempt failed* — the queue asks again, but only after the backoff,
                // and only while the attempts last; and when they do not, the entry parks with
                // the failure on its row rather than waiting for a reviewer that never came.
                ReviewGate::Awaiting => match review_retry(review, now_ms) {
                    ReviewRetry::Due => {
                        // **Is there a gatekeeper at all?** — asked BEFORE the verdict, because
                        // *nobody has asked yet* and *nobody is there to ask* are two different
                        // facts with two different answers: the second is the never-started case
                        // of the fallback ([`NoGatekeeper`]), and the only half of the queue that
                        // can see it is the door ([`Reviewer::gatekeeper_ready`]). The default
                        // says `Ok(())`, so a door with no registry behind it — every test's —
                        // keeps the old behaviour: ask, and let the ask say what happened.
                        if let Err(why) = self.reviewer.gatekeeper_ready(&entry.id) {
                            self.hand_over(entry, &NoGatekeeper::NeverStarted(why))?;
                            handed = true;
                            continue;
                        }
                        asked = true;
                        let req = letibot_tools::gatekeeper::ReviewRequest {
                            brief: entry.brief.clone(),
                            branch: entry.branch.clone(),
                            base_sha: entry.base_sha.clone(),
                        };
                        match letibot_tools::gatekeeper::wake(&entry.id, req, &*self.reviewer) {
                            // **Said on the row, once**: the queue pane shows an entry being
                            // reviewed, and under which session, instead of one that looks stuck.
                            // Written only when it changes, since every pass asks again.
                            Ok(said) => {
                                if entry.evidence != said {
                                    self.move_to(entry, MergeState::Waiting, said, None, None)?;
                                }
                            }
                            Err(e) => {
                                // **A wake that failed is on the row, and the project is told in
                                // the same breath.** An entry waiting for a verdict nobody was
                                // asked for waits for ever, and a queue that said nothing would be
                                // the same silence as an empty one — so the failure leads the row
                                // and the fallback follows it: the queue asks again by itself on
                                // the next pass, and the project that owns the branch is told now
                                // rather than after the retries run out
                                // ([`NoGatekeeper::AskFailed`]).
                                self.hand_over(
                                    entry,
                                    &NoGatekeeper::AskFailed(format!(
                                        "the gatekeeper could not be asked: {e}"
                                    )),
                                )?;
                            }
                        }
                    }
                    // **The failure is on the row while the queue waits to try again**, which is
                    // the whole reason a person can see it: *waiting for a reviewer* and *waiting
                    // for a reviewer whose last attempt died* look identical from the state word
                    // alone, and the second is the one somebody may have to act on.
                    ReviewRetry::Wait { in_ms } => {
                        let said = review_failed_evidence(review, in_ms);
                        if entry.evidence != said {
                            self.move_to(entry, MergeState::Waiting, said, None, None)?;
                        }
                    }
                    // **The attempts are spent: park it, with the failure's own words.** A
                    // reviewer that cannot be asked is not a refusal, so this is not
                    // [`Self::Refused`] — but it is the same shape of stopping, and the worktree
                    // stays for the same reason: the tree is where the reason is.
                    //
                    // **And the project that owns the branch is told**, which is the operator's
                    // correction to what this arm used to say: *"a person has to move it"* is the
                    // last resort, not the mechanism — *"even if gatekeeper session dies … the
                    // queue tooling can send a message back to the main project session and
                    // projects will gate and merge on their own"*. The failure's own words stay on
                    // the row, first, and the handover follows them
                    // ([`NoGatekeeper::Died`]).
                    ReviewRetry::Exhausted => {
                        let said = review_exhausted_evidence(entry, review);
                        self.hand_over(entry, &NoGatekeeper::Died(said))?;
                        return Ok(StepOutcome::ReviewGaveUp);
                    }
                },
                ReviewGate::Refused(verdict) => {
                    // **Parked, with the verdict on the row.** `Failed` is the queue's word for
                    // *this did not land and the tree is where the reason is*, and a refused
                    // review is that shape exactly: the work stays, the worktree stays, and a
                    // person answers it. A state of its own would say the same thing one word
                    // further out and would cost a protocol bump to draw.
                    self.move_to(entry, MergeState::Failed, verdict, None, None)?;
                    return Ok(StepOutcome::Refused);
                }
            }
        }

        let Some(idx) = next_takeable(&entries, &reviews) else {
            // **Nothing may be taken, and the three reasons are different.** *Something is due and
            // its verdict is not in* is not idleness: it is a queue waiting on a reviewer, and
            // the loop says so rather than sleeping the same way as an empty queue. *A gatekeeper
            // was asked about and there is none* is a third: an entry was ready, and what the
            // queue did about it was tell the project that owns the branch.
            return Ok(if asked {
                StepOutcome::AwaitingReview
            } else if handed {
                StepOutcome::NoGatekeeper
            } else {
                StepOutcome::Idle
            });
        };
        let entry = entries[idx].clone();

        // **No gate, no take.** A repository whose `main` has no gate holds its branches —
        // reviewed or not — and says so on the row, once; the entry goes on by itself when the
        // section lands. Not `Failed`: nothing about the branch is wrong.
        let repo = self.repo_for(&entry);
        if let Err(why) = (self.gate_ready)(&repo) {
            if entry.evidence != why {
                // **The absence is recorded on the entry as its own row.** The queue holds the
                // branch rather than failing it, and the row says WHY in the landing half's own
                // vocabulary ([`MergeGateOutcome::NoGate`]) rather than leaving the checklist
                // empty — an empty list is *the gate has not run*, and a pane drawing it as
                // *every step passed* is the one reading this row exists to make unavailable.
                let run = GateRun::no_gate(&why);
                self.move_to(&entry, MergeState::Waiting, why, None, Some(run.steps))?;
                // **And the project's agent is told**, once — the operator: *"let main project
                // agent manage it"*. A door that could not reach anybody is said, not swallowed.
                if let Err(e) = self.reviewer.gate_missing(&entry.id) {
                    eprintln!(
                        "  merge queue: nobody could be told `{}` needs a gate: {e}",
                        entry.id
                    );
                }
            }
            return Ok(StepOutcome::AwaitingGate);
        }

        // **The tip of THIS repository's main, read now** — not the last SHA the queue landed:
        // main also moves by hand (a commit, a push, the gate's own section landing), and with
        // one queue serving several repositories the last landing may be another repository's.
        // The queue's record is the fallback only when git cannot name the tip.
        //
        // **Read HERE, before the entry is claimed**, because two things need it and the second
        // of them is a decision about whether to claim the entry at all: the already-merged check
        // just below, and the rebase's `base`.
        let tip_now = run_captured(&repo, "git", &["rev-parse", "main"])
            .ok()
            .map(|t| t.trim().to_string());

        // **What the branch is, against the base it was cut from AND against main** — one answer
        // from two git facts, and BOTH are needed. [`branch_standing`] is where the reason lives,
        // and it is the reason this pass cannot be read as *"if main contains it, it has landed"*.
        //
        // **An entry whose branch is already in main is what it is: `Landed`.** The operator's
        // ruling on the row that describes exactly this: *"and i love your 2"*. A branch that is
        // an ancestor of main AND carries commits of its own has nothing left to rebase onto and
        // no diff left to gate — the work it was holding is on main already, landed by somebody
        // else's hand (a merge of its own, a later commit that carried it) — and rebasing it would
        // replay commits main already has, which is either a no-op git refuses or a conflict with
        // itself. So the row is moved to `Landed` with main's own tip as its `landed_sha`, and the
        // pass returns without claiming the entry, without rebasing it and without asking
        // anybody's gate about it.
        //
        // **An entry whose branch carries NOTHING is the other case, and it is never `Landed`.**
        // A branch a child never committed to stands at its own base, and its base is an ancestor
        // of main by construction — so *the branch is an ancestor of main* is TRUE of it, and
        // reading that one fact as *already landed* moved the row to `Landed` and ran the cleanup
        // `Landed` owns: the worktree, which is where the uncommitted work was, was removed, the
        // branch was deleted, and the row said the work had landed when main had never seen it.
        // Two branches of real work were lost to exactly that. So the count is asked FIRST, and 0
        // over the base is `Failed` with the reason on the row — which keeps the tree, and the
        // work in it.
        //
        // **The claim is deliberately NOT taken first**, in either case. A claim is the queue
        // saying *I am landing this*; here there is nothing to land, and a row that is claimed and
        // then moved leaves a `taken` state on disk for a daemon that dies in between — for a
        // landing that never needed to happen.
        match (
            branch_standing(&repo, &entry.branch, &entry.base_sha),
            tip_now.as_deref(),
        ) {
            (BranchStanding::NothingCommitted, _) => {
                let evidence = format!(
                    "nothing was committed on `{}`: the branch carries 0 commits over the base it \
                     was cut from (`{}`), so there is no diff to rebase onto and nothing for a gate \
                     to judge — and it is NOT `landed`, whatever main contains. A branch standing \
                     at its own base is an ancestor of main by construction, and that is not the \
                     same fact as *main has this branch's work*. The row is parked `failed` and the \
                     branch and its worktree are left exactly as they are, holding whatever the \
                     tree holds; the child that did the work is this entry's session, so \
                     `/queue veto {}` sends it back to commit what is in the tree, and \
                     `/queue rm {entry_id}` drops the entry.",
                    entry.branch,
                    entry.base_sha,
                    letibot_sessionlog::registry::short_id(&entry.id),
                    entry_id = entry.id,
                );
                self.move_to(&entry, MergeState::Failed, evidence, None, None)?;
                return Ok(StepOutcome::Failed);
            }
            (BranchStanding::OnMain, Some(tip)) => {
                let evidence = format!(
                    "landed without a rebase and without a gate: main already contains `{}` — the \
                     branch is an ancestor of main at {tip} and carries commits of its own over \
                     its base, so there is no diff left to rebase onto and nothing left for a gate \
                     to judge. The row was moved to `landed` as it stands; `/queue clean {}` drops \
                     it if you want it gone.",
                    entry.branch,
                    letibot_sessionlog::registry::short_id(&entry.id)
                );
                self.move_to(
                    &entry,
                    MergeState::Landed,
                    evidence,
                    Some(tip.to_string()),
                    None,
                )?;
                return Ok(StepOutcome::Landed(tip.to_string()));
            }
            // **The ordinary path decides** — a branch carrying work main does not have, and a
            // branch git could not answer about: *cannot tell* is not a conclusion, so it falls
            // through to the take, the rebase, the gate and the fast-forward, which report their
            // own reasons in git's own words.
            _ => {}
        }

        // **Take it**: mark it `Taken`, so a daemon that dies now comes back to a row that
        // says the job was running, not a row that says nothing. Through `move_to`, like every
        // other move, so the pane is told this one too: an entry the queue is working on is not
        // an entry that is still waiting, and a head that only saw the landing would draw a
        // `waiting` row through the whole gate.
        // **Claimed first, atomically**: a second daemon over the same store that reached the
        // same entry in the same second loses here rather than rebasing it twice.
        if !self.store.claim_merge_entry(&entry.id)? {
            return Ok(StepOutcome::Idle);
        }
        self.move_to(
            &entry,
            MergeState::Taken,
            "rebasing at the tip".into(),
            None,
            None,
        )?;

        // **Rebase it at the tip**: onto the current main, not the SHA it was written
        // against. A conflict is reported, never resolved.
        let base = tip_now
            .as_deref()
            .unwrap_or_else(|| effective_base(&entry, &entries));
        let worktree = entry.worktree.as_deref().unwrap_or("");
        if worktree.is_empty() {
            // No worktree: the enqueuer's half has not created one, and the queue cannot
            // rebase a branch that is not checked out. This is the `task_start` seam — the
            // worktree is created when the branch is enqueued, and until then the entry
            // waits. It is reported rather than guessed at.
            self.move_to(
                &entry,
                MergeState::Waiting,
                "no worktree: the branch is not checked out".into(),
                None,
                None,
            )?;
            return Ok(StepOutcome::Idle);
        }
        let worktree = Path::new(worktree);
        if let Err(e) = rebase(worktree, base) {
            self.move_to(&entry, MergeState::Conflict, e, None, None)?;
            return Ok(StepOutcome::Conflict);
        }

        // **And the row says what the daemon is running while it runs it** — the gate job's own
        // name ([`gate_job_name`]), written before the gate is entered and not after, because the
        // run is the minutes-long half: until this move, a `Taken` row said *rebasing at the tip*
        // for the whole of a `cargo test --workspace`, which is the one thing it was not doing.
        //
        // The move is a write and an announcement like every other — the queue's row IS how it
        // talks, and a run nobody can see is the defect the name answers.
        self.move_to(
            &entry,
            MergeState::Taken,
            format!("{}: main's steps at the tip", gate_job_name(&entry)),
            None,
            None,
        )?;

        // **Run the gate**, at the tip, on the rebased branch — and the run's rows ride the
        // move that ends it, whichever way it ends. This is the whole of the change: the
        // landing half records one row per step (the command, its outcome, the tail of its
        // output, when it started and how long it took), so that a landing can be DRAWN rather
        // than only judged.
        let run = (self.gate)(worktree);
        if let Some(e) = run.failure {
            self.move_to(&entry, MergeState::Failed, e, None, Some(run.steps))?;
            return Ok(StepOutcome::Failed);
        }

        // **Fast-forward main and push**: the merge, and the landing.
        let tip = match fast_forward_main(&repo, &entry.branch) {
            Ok(tip) => tip,
            Err(e) => {
                self.move_to(&entry, MergeState::Failed, e, None, Some(run.steps))?;
                return Ok(StepOutcome::Failed);
            }
        };
        if let Err(e) = push_main(&repo) {
            self.move_to(&entry, MergeState::Failed, e, None, Some(run.steps))?;
            return Ok(StepOutcome::Failed);
        }

        // **Land it**: the row carries the tip and the gate's rows, and the cleanup the state
        // owns runs.
        self.move_to(
            &entry,
            MergeState::Landed,
            format!("landed at {tip}"),
            Some(tip.clone()),
            Some(run.steps),
        )?;

        Ok(StepOutcome::Landed(tip))
    }

    /// **Record a landing the PROJECT did, when main already has the branch** — `Some(tip)` when
    /// the entry was moved to `Landed`, `None` when there was nothing to see.
    ///
    /// This is the second half of the fallback ([`NO_GATEKEEPER`]), and the half that makes the
    /// message true. The queue told the project's session to gate the branch and land it itself
    /// (`no_gatekeeper_message`), the project did it with git and a shell, and the queue's next
    /// pass reads the one fact that decides it: **main contains the branch.** The entry goes
    /// `landed` with main's own tip as its `landed_sha`, and the cleanup every landing owns runs
    /// ([`cleanup_for`]) — the worktree is removed and the branch deleted, because the work is on
    /// main and the tree was only ever holding it.
    ///
    /// # Why it is checked before the verdict, and why that is not a hole in the gate
    ///
    /// The pass's ordinary already-merged arm ([`branch_standing`] → `Landed`) sits behind
    /// [`next_takeable`], which requires a verdict that accepts — and a handed-over entry has no
    /// verdict and is not going to get one. Left there, the check would never run for exactly the
    /// entries the fallback is about, and the project's landing would sit on main with the row
    /// still saying `failed`.
    ///
    /// **Nothing is landed here**, and that is why moving the check earlier does not weaken the
    /// gate: this arm rebases nothing, runs no gate and moves no SHA. It RECORDS what git already
    /// says, and git is the authority the queue's own gate exists to reach. The gate's whole job
    /// is to decide whether a branch may be put on main; a branch that is already on main is a
    /// question that has been answered by whoever put it there, and the queue's row is a record of
    /// the queue, not a veto over main.
    ///
    /// **`NothingCommitted` is NOT this case and is deliberately not handled here.** A branch
    /// standing at its own base is an ancestor of main by construction, and the ordinary arm's
    /// long paragraph is the reason that reading is the one that destroys work: this arm asks
    /// [`branch_standing`] for exactly the `OnMain` answer, so a branch with 0 commits over its
    /// base falls through to the ordinary path — where, for a handed-over entry, nothing is taken
    /// either, because there is still no verdict.
    ///
    /// # The cost
    ///
    /// Two git reads per handed-over entry per pass ([`branch_standing`]'s `rev-list --count` and
    /// `merge-base --is-ancestor`, plus `rev-parse main` here), and the pass runs once a second.
    /// That is the same order as the ordinary arm's and it is paid only for entries the queue has
    /// handed over — a queue with none pays nothing.
    fn land_what_main_has(
        &self,
        entry: &MergeEntry,
    ) -> Result<Option<String>, letibot_tokencore::store::StoreError> {
        let repo = self.repo_for(entry);
        if branch_standing(&repo, &entry.branch, &entry.base_sha) != BranchStanding::OnMain {
            return Ok(None);
        }
        let Some(tip) = run_captured(&repo, "git", &["rev-parse", "main"])
            .ok()
            .map(|t| t.trim().to_string())
        else {
            // **git cannot name main, so nothing is recorded.** The same direction the ordinary
            // arm fails in: a fact the queue cannot read is not a fact it may write down.
            return Ok(None);
        };
        // **What the row says is what the queue KNOWS, which is less than who did it.** The queue
        // can see that main has the branch; it cannot see whether the project merged it by hand or
        // a person approved the entry and the branch was already on main. So the sentence leads
        // with the fact the queue read and names the handover that put the entry here — either
        // kind of it, which is why it does not say WHY the entry was handed over — rather than
        // attributing a landing it did not witness.
        let evidence = format!(
            "landed without the queue's gate: main now contains `{}` at {tip}, and the entry \
             (entry `{}`) was one the queue had handed to the project that owns the branch — so the \
             row is the queue's record of a landing that happened elsewhere. The worktree and the \
             branch are cleaned up as every landing's are; `/queue clean {}` drops the row if you \
             want it gone.",
            entry.branch,
            entry.id,
            letibot_sessionlog::registry::short_id(&entry.id)
        );
        self.move_to(entry, MergeState::Landed, evidence, Some(tip.clone()), None)?;
        Ok(Some(tip))
    }

    /// **Hand an entry to the project that owns its branch when no gatekeeper can judge it** —
    /// the fallback, and the operator's argument for it: *"even if gatekeeper session dies or
    /// didnt start - the queue tooling can send a message back to the main project session and
    /// projects will gate and merge on their own"*, *"because their code lives inside them in
    /// worktrees anyway"*.
    ///
    /// [`NoGatekeeper`] names the three ways this is reached, and [`NoGatekeeper::state`] says
    /// what the row becomes. What this adds is the two acts and their order:
    ///
    /// * **The row, then the ring.** The project's session is told by a WAKE, and what its model
    ///   reads is the store ([`handed_over_to`] → `no_gatekeeper_message`) — so the row has to
    ///   carry the handover before the bell is rung, exactly as the gate hold in [`Self::step`]
    ///   does it. A door that could not reach anybody is written onto the row as well, and that is
    ///   not tidiness: *the project was told* and *there was nobody to tell* are the two facts a
    ///   person acts differently on, and the second is the one that makes a person the last resort.
    /// * **Once, read off the row.** [`handed_over`] is the guard and it is the row's own words
    ///   rather than a set in memory, so a daemon that restarts does not tell a project again about
    ///   an entry it was already told about — and a row whose words have since changed (a later ask
    ///   succeeded, or a person restarted the review) is a row the queue may hand over again, which
    ///   is right: the fact it states has changed.
    ///
    /// **The queue does not stop watching the entry after this.** A handed-over entry stays in the
    /// queue, on its row, in its worktree, and the pass goes on reading main for it
    /// ([`Self::land_what_main_has`]) — the project landing it is the expected ending, and a person
    /// approving or restarting it is the other one.
    fn hand_over(
        &self,
        entry: &MergeEntry,
        why: &NoGatekeeper,
    ) -> Result<(), letibot_tokencore::store::StoreError> {
        if handed_over(entry) {
            return Ok(());
        }
        let state = why.state();
        self.move_to(
            entry,
            state,
            no_gatekeeper_evidence(entry, why.why(), None),
            None,
            None,
        )?;
        match self.reviewer.no_gatekeeper(&entry.id, why.why()) {
            Ok(told) => eprintln!(
                "  merge queue: no gatekeeper for `{}` (entry `{}`) — {}",
                entry.branch, entry.id, told
            ),
            Err(e) => {
                // **Nobody to tell, and the row says so rather than only the terminal.** The
                // sentence replaces the one just written, and it keeps the clause the guard and
                // the project's session read ([`NO_GATEKEEPER`]), so the entry is still not told
                // twice by a daemon that comes back.
                eprintln!(
                    "  merge queue: no gatekeeper for `{}` (entry `{}`), and nobody could be told: \
                     {e}",
                    entry.branch, entry.id
                );
                let said = no_gatekeeper_evidence(entry, why.why(), Some(&e));
                self.move_to(entry, state, said, None, None)?;
            }
        }
        Ok(())
    }

    /// **Write the entry's move, then do the cleanup the new state owns** — the two halves of
    /// every move in one place, so which tree survives is the RULE's answer ([`cleanup_for`])
    /// rather than a property of which `return` the pass happened to take.
    ///
    /// The row is written first, and that order is the design rather than an accident: a daemon
    /// that dies between the two comes back to a row that says what happened, and the next
    /// daemon's [`Self::recover`] and its cleanup are both idempotent. `landed_sha` is set only
    /// where there is one to set: a move that is not a landing carries the entry's own, which
    /// is `None` for an entry that has not landed.
    ///
    /// **`gate_steps` is `Some` on exactly the moves that have something to say about the
    /// gate** — the landing's own two (`Landed` and the `Failed` a red step parks), and the
    /// hold that records *this repository has no gate*. `None` leaves the entry's rows as they
    /// are, which for every other move is the empty list the enqueuer wrote: a review that has
    /// not answered, a veto, a restart and a conflict have run no step, and a move that wrote
    /// the empty list over rows an earlier gate run left would be the queue forgetting a
    /// landing it witnessed.
    fn move_to(
        &self,
        entry: &MergeEntry,
        state: MergeState,
        evidence: String,
        landed_sha: Option<String>,
        gate_steps: Option<Vec<MergeGateStep>>,
    ) -> Result<(), letibot_tokencore::store::StoreError> {
        let mut moved = entry.clone();
        moved.state = state;
        moved.evidence = evidence;
        if landed_sha.is_some() {
            moved.landed_sha = landed_sha;
        }
        if let Some(steps) = gate_steps {
            moved.gate_steps = steps;
        }
        self.store.put_merge_entry(&moved)?;
        clean_up(&self.repo_for(entry), entry, state);
        // **The move is announced, and AFTER the row is written.** A head that folded the
        // event before the row was on disk could re-read the queue and find the old state,
        // which is the one order that makes the snapshot and the events disagree; writing
        // first means a head that missed the event still reads the truth, and a head that got
        // it reads a state the queue already holds.
        (self.events)(letibot_sessionlog::SessionEvent::MergeEntryMoved {
            id: moved.id.clone(),
            state: crate::mergequeue::wire_state(state),
            evidence: moved.evidence.clone(),
        });
        Ok(())
    }

    /// **Run the loop until `stop` is set**: recover on the first pass, then `step` until
    /// there is nothing ready, sleeping between passes.
    ///
    /// The sleep is the only clock in the daemon, and it is a give-up rather than a poll:
    /// the loop blocks on it until either an entry is ready or the sleep is over, and the
    /// sleep is over when the `stop` flag is set (the daemon is shutting down) or when the
    /// interval is up (re-check the queue). The interval is short — a second — because the
    /// cost of a long one is an entry that waits a long time to be taken, and the cost of a
    /// short one is a re-check that finds nothing, which is cheap.
    ///
    /// **The `task_start` seam is CLOSED**: the branch that is enqueued on completion is
    /// [`crate::harness::HarnessTaskRunner::finished`], which writes the entry when a
    /// `task_start` child finishes. So this loop has entries to take, and the `stop` flag is
    /// still what ends it when there are none.
    ///
    /// **The `gatekeeper` seam is CLOSED, and this is where.** [`Self::step`] asks the reviewer
    /// about every entry that is due, takes only the ones whose verdict accepts, and parks the
    /// ones whose verdict refuses — so the review is required before the fast-forward rather
    /// than being a check somebody remembers to make. The wake goes through
    /// [`letibot_tools::gatekeeper::wake`] and the door this daemon was built with, and the
    /// verdict comes back through the store on a later pass, which is why the queue's thread
    /// never blocks on a reviewer's turn.
    pub fn run(&self, stop: &AtomicBool) -> Result<(), letibot_tokencore::store::StoreError> {
        self.recover()?;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            match self.step()? {
                StepOutcome::Idle => {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                StepOutcome::Landed(tip) => {
                    eprintln!("  merge queue: landed {tip}");
                }
                StepOutcome::Conflict => {
                    eprintln!("  merge queue: conflict — the worktree stays, with the reason");
                }
                StepOutcome::Failed => {
                    eprintln!("  merge queue: gate failed — the worktree stays, with the reason");
                }
                // **Waiting on a reviewer is not idleness, and it is not a busy loop.** The
                // ask was made (or re-made) this pass; the sleep is the same one, because the
                // answer arrives through the store on somebody else's thread and there is
                // nothing to spin on.
                StepOutcome::AwaitingReview => {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                // The same wait for the same reason: what it is waiting on arrives on `main`,
                // from somebody else's hands.
                StepOutcome::AwaitingGate => {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                }
                StepOutcome::Refused => {
                    eprintln!(
                        "  merge queue: the reviewer refused an entry — it is `failed` with the \
                         verdict on its row, and the worktree stays"
                    );
                }
                StepOutcome::ReviewGaveUp => {
                    eprintln!(
                        "  merge queue: the gatekeeper could not be asked — the entry is `failed` \
                         with the failure on its row and the project that owns the branch has been \
                         told; `/queue restart ID` asks a gatekeeper again"
                    );
                }
                // The same wait, and for the same reason as `AwaitingGate`: what would move it
                // arrives from somebody else's hands — a daemon that opens the review queue's
                // session, or the project's own landing on main.
                StepOutcome::NoGatekeeper => {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    }

    /// **Start the thread that serves the queue** — the daemon's half of the operator's
    /// *"merge queue is 2 - together with persistence and harnessd thread that serves it."*
    ///
    /// Named, so the queue's thread is legible in `ps`; detached, so a daemon that is stopping
    /// does not wait for a gate that is minutes long. **A merge interrupted that way is not lost
    /// and is not silently retried**: the row on disk is `Taken`, the next daemon's
    /// [`Self::recover`] moves it to `Stale` with its reason, and a person re-enqueues it.
    /// Waiting for the gate here would hold the daemon's exit for the length of a `cargo test
    /// --workspace`, which is the opposite of what a shutdown is for.
    ///
    /// TODO(events): **publish `MergeEntryAdded`/`MergeEntryMoved` as the queue moves.** The
    /// wire carries both — the census at `PROTOCOL_VERSION` 29 counts them and `scrub` keeps
    /// them — and the snapshot a head reads is the queue as of now, so a head that was attached
    /// when an entry moved keeps drawing the state it last saw. What is missing is the emitter
    /// and not the event: every `publish` in this daemon is to ONE session's hub
    /// (`Harness::hub`), the queue is daemon-level rather than a session's, and a broadcast
    /// across hubs is a decision about what every session's log records rather than a call that
    /// is missing. So the seam is named here, next to the thread that would emit.
    pub fn spawn(self, stop: Arc<AtomicBool>) -> std::io::Result<JoinHandle<()>> {
        std::thread::Builder::new()
            .name("merge-queue".into())
            .spawn(move || {
                if let Err(e) = self.run(&stop) {
                    // A queue that cannot be read is said out loud rather than going quiet: an
                    // empty queue and an unreadable one look identical from the outside, and
                    // *"that is all the work there is"* is the one thing this module refuses to
                    // say falsely.
                    eprintln!("  merge queue: the queue could not be served: {e}");
                }
            })
    }
}

/// **Start the queue for a daemon** — the whole of the startup wiring in one place, so the
/// binary's own startup is one call and the decisions live with the queue.
///
/// `None` when there is no queue to serve, and the two cases are different:
///
/// * **No `--store`**: there is no durable queue by construction — the queue lives in
///   `sessions.db` — so a daemon without a store has nothing to serve and nothing to say.
///   This is [`crate::sessions::StoreSessions::open`]'s posture, for its reason.
/// * **A workspace that is not in a git repo**: there is no `main` to land on. That IS said,
///   because a daemon that was asked for a queue and silently has none is the same lie as an
///   empty queue that is not empty.
///
/// The poll interval is the one [`MergeQueueDaemon::run`] sleeps on, and it is worth naming
/// here where it is a cost rather than a line: with an empty queue this thread re-reads one
/// indexed `SELECT` a second, and with an entry whose review is outstanding it re-asks that
/// entry's reviewer once a second, which is the recovery for a daemon that came up between the
/// request and the bell.
///
/// **The reviewer's session id is a constant, and the session is the daemon's to hold.**
/// `Sessions::wake` can only run a turn for a session the daemon holds (the `Drive` route) or
/// hand the wake to a thread that owns one (a subagent's, the `ItsOwnReader` route); a
/// reviewer that was neither would be a bell rung at nobody. So the reviewer is a session the
/// daemon OPENS at startup under this name, seated read-only, and the queue names it here.
pub const REVIEWER_SESSION_ID: &str = "gatekeeper";

pub fn spawn_for(
    cfg: &crate::config::Config,
    stop: Arc<AtomicBool>,
    reviewer: Box<dyn letibot_tools::gatekeeper::Reviewer + Send>,
    events: Box<dyn Fn(letibot_sessionlog::SessionEvent) + Send>,
) -> Option<JoinHandle<()>> {
    let store_path = cfg.store.as_ref()?;
    // **The workspace's own repository, or the workspace itself** when it is in none — a
    // daemon a level above its repositories still serves them: each entry lands in its
    // worktree's repository ([`MergeQueueDaemon::repo_for`]).
    let repo = repo_root(&cfg.workspace).unwrap_or_else(|_| cfg.workspace.clone());
    let store = match Store::open(store_path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("  merge queue: not served — {}: {e}", store_path.display());
            return None;
        }
    };
    match MergeQueueDaemon::new(store, repo.clone(), Box::new(repo_gate), reviewer, events)
        .with_gate_check(Box::new(gate_configured))
        .serving(&cfg.workspace)
        .spawn(stop)
    {
        Ok(handle) => {
            eprintln!("  merge queue: serving {} toward main", repo.display());
            Some(handle)
        }
        Err(e) => {
            eprintln!("  merge queue: not served — the thread did not start: {e}");
            None
        }
    }
}

/// **The daemon's half of a wake: the request names the session that REVIEWS the branch, and the
/// reviewer runs as a `task` child of it.**
///
/// The operator's design, in their words: *"reviews are subagents managed by the main gatekeeper
/// session which is hardcoded … it is a normal auto-driving session that treats review queue as
/// todo list"*, and the correction that draws the edge — *"well gatekeeper is not a child so we are
/// not doing up"*. So the reviewer is a subagent of the TUTOR
/// ([`crate::sessions::gatekeeper::SESSION`]), which is a session in its own right and a peer of the
/// main session; this door's whole job is to put the request where that session will find it.
///
/// What this door does is the queue thread's share, and it is two acts:
///
/// * **The request row, naming the session that reviews it** — `s-gatekeeper`, whose board is the
///   queue. Durable, and the whole of the mechanism: a daemon that restarts between the ask and the
///   verdict comes back to a row still waiting, and `Sessions::serve_reviews` serves it on the
///   daemon's own clock whatever daemon asked for it.
/// * **The ring** at that session — the fast path, and only that. A row written before the queue's
///   session is open costs nothing: the daemon's own clock opens it and serves it on the same idle
///   pass (`Sessions::ensure_gatekeeper` → `Sessions::serve_reviews`).
///
/// **Why not the session the branch came from, which is where this used to run.** A reviewer's
/// asks go up its tree to the ROOT's head, so under the old shape the review ran as a hidden child
/// of the session that enqueued the entry, and the row named that session. The operator's design
/// moves both: the review is the tutor's child, so the row names the tutor, the reviewer's `git
/// diff` cards are the tutor's to answer, and the complaints a refusal carries go back to the child
/// that did the work by a LATERAL send (`TaskRunner::send` names any live session) rather than down
/// a tree edge. The column's own doc says the same thing from the store's side: `ReviewRecord::session_id`
/// is *"the session that reviewed it … the one a person attaches to when they want to read the
/// argument"*, which is this session now and was not the tree root before.
///
/// **The ring is not what an owed review depends on, and MEASURED, 2026-10-10, that was the
/// defect.** A ring is a bell and a bell is per-daemon: a mover asking for a review hosted by a
/// session in ANOTHER daemon rings a bell nobody is standing at, and a daemon that restarts
/// between the ask and the wake has nothing left to ring at all — the ring is in memory and the
/// row is not (*"restarted pg-noop and nothing moves"*). So the store is the mechanism and the
/// ring is the latency: `Sessions::serve_reviews` reads the `Due` rows on the daemon's own clock
/// and serves the ones whose host it holds, which needs nothing to reach anything.
///
/// # And the door has a second errand, which is the fallback
///
/// The queue's own half above is about a gatekeeper that will answer. This struct is also where
/// the queue finds out that **none will** — the two questions only it can ask, because it is the
/// half holding the registry and the store:
///
/// * **[`letibot_tools::gatekeeper::Reviewer::gatekeeper_ready`]** — is there a session at all
///   that will judge this entry? `Err` is the never-started case, and [`GATEKEEPER_GRACE_MS`] is
///   what keeps *not open yet* out of it.
/// * **[`letibot_tools::gatekeeper::Reviewer::no_gatekeeper`]** — tell the project that owns the
///   branch, through the same `root_for` door `gate_missing` takes. The message itself is built
///   by the session that is rung, out of the entry's own row.
///
/// The operator's argument for the whole of it, in their words: *"even if gatekeeper session dies
/// or didnt start - the queue tooling can send a message back to the main project session and
/// projects will gate and merge on their own"*, *"because their code lives inside them in
/// worktrees anyway"*. So this door's answer to *no gatekeeper* is not a refusal — it is a second
/// route, and the module docs carry the rest of it.
pub struct GatekeeperDoor {
    /// The session store, opened on the queue's own thread (a `Connection` is `Send`, not
    /// `Sync`, and this struct travels into the merge-queue thread).
    store: Store,
    /// The daemon's bell, which is what the review queue's session is woken by.
    bell: Arc<letibot_sessionlog::registry::Bell>,
    /// The daemon's sessions: whether the queue's session is live (the ring is only sent to one
    /// that is), and how to resume the ROOT of an entry's tree when its own agent has to be told
    /// something (`GatekeeperDoor::gate_missing`).
    registry: Arc<letibot_sessionlog::registry::Registry>,
}

impl GatekeeperDoor {
    pub fn new(
        store: Store,
        bell: Arc<letibot_sessionlog::registry::Bell>,
        registry: Arc<letibot_sessionlog::registry::Registry>,
    ) -> Self {
        Self {
            store,
            bell,
            registry,
        }
    }

    /// **The session an entry's MISSING-GATE notice goes to** — the root of the session that queued
    /// it, resumed from the store when this daemon does not hold it (the operator's ruling), so an
    /// entry whose session closed hours ago still has somebody to tell. `what` says what the session
    /// is wanted for, in the refusal when there is none.
    ///
    /// **Not the reviewer's route any more, and the two are not the same act.** A review runs under
    /// the queue's own session ([`GatekeeperDoor::wake`]), which this daemon always has; telling the
    /// PROJECT's agent that its repository declares no merge gate is a message to the session whose
    /// work is held, and only that session's model can write the `AGENTS.md` section that releases
    /// it. So this is the door `gate_missing` takes and `wake` does not.
    ///
    /// **Two callers, and they are the two facts a project's own session is told**: its repository
    /// has no gate (`gate_missing`), and no gatekeeper can judge its branch
    /// ([`GatekeeperDoor::no_gatekeeper`]). Both are about the WORK being held and neither is about
    /// the review, which is why both come here and `wake` does not.
    ///
    /// **The rule itself is [`project_session`]'s**, because the review queue's session needs the
    /// same answer when its refusal cannot be delivered to the child (`hand_over_from_session`) and
    /// a second spelling of *which session, and how it is found* would let one of the two ring a
    /// session the other would have resumed. This is the door's own store and registry handed to
    /// that function.
    fn root_for(&self, entry_id: &str, what: &str) -> Result<(MergeEntry, String), String> {
        project_session(&self.store, &self.registry, entry_id, what)
    }
}

impl letibot_tools::gatekeeper::Reviewer for GatekeeperDoor {
    fn wake(
        &self,
        entry_id: &str,
        req: &letibot_tools::gatekeeper::ReviewRequest,
    ) -> Result<String, String> {
        // **The entry has to be there, and it is not read for anything else**: the request's branch
        // and base are the caller's (`ReviewRequest`), echoed onto the row so a verdict names what
        // it is about even if the entry leaves the queue.
        if self
            .store
            .merge_entry(entry_id)
            .map_err(|e| format!("the entry `{entry_id}` could not be read: {e}"))?
            .is_none()
        {
            return Err(format!(
                "the entry `{entry_id}` is not in the queue's table"
            ));
        }
        // **The row names the session that reviews it**, which is the queue's own
        // (`crate::sessions::gatekeeper::SESSION`) and not the tree the branch came from: the
        // reviewer is a `task` child of THAT session, so that is whose head its cards reach and
        // where a person attaches to read the argument. See the struct's own doc for why the
        // shape moved.
        let host = crate::sessions::gatekeeper::SESSION.to_string();
        let existing = self
            .store
            .merge_review(entry_id)
            .map_err(|e| format!("the review row for `{entry_id}` could not be read: {e}"))?;
        // **Written when there is none, or when it names another session** — `asked_ms` is when the
        // review was FIRST asked for, and a queue that re-wrote it every pass would lose that. A
        // row written by an older shape (naming the tree's root) is re-hosted here, which is the
        // whole of the migration: the next ask moves it onto the session that runs the review.
        if existing.as_ref().is_none_or(|r| r.session_id != host) {
            self.store
                .put_review(&letibot_tokencore::store::ReviewRecord {
                    entry_id: entry_id.to_string(),
                    session_id: host.clone(),
                    branch: req.branch.clone(),
                    base_sha: req.base_sha.clone(),
                    asked_ms: existing
                        .as_ref()
                        .map(|r| r.asked_ms)
                        .unwrap_or_else(|| (crate::config::now_ns() / 1_000_000) as u64),
                    // **No verdict, and that is the row's whole meaning**: the review is
                    // outstanding. The session that hosts it writes the verdict when its gatekeeper
                    // answers.
                    answered_ms: None,
                    decision: None,
                    // **No failed attempt either, and this is the line the restart reads.** A
                    // request row written here is *an attempt is in flight* — the state
                    // `review_retry` calls `Due` and `restartable` refuses to interrupt — and
                    // it stays that way until the host writes a verdict or a failure onto it.
                    attempts: 0,
                    failed_ms: None,
                    failure: String::new(),
                    reasons: Vec::new(),
                    files: Vec::new(),
                    commands: Vec::new(),
                })
                .map_err(|e| format!("the request for `{entry_id}` could not be written: {e}"))?;
        }
        if self.bell.is_closed() {
            return Err(format!(
                "the daemon's bell is closed (it is shutting down), so `{host}` was not woken to \
                 review `{entry_id}`. The request is on the row; the next daemon rings for it."
            ));
        }
        // **A ring only reaches a session this daemon has already opened, and that is not a lost
        // ask.** The queue's session is started by the daemon's own idle arm
        // (`Sessions::ensure_gatekeeper`) and served by its own clock on the same pass, so an ask
        // that arrives first — a daemon in its first second, or a queue row written by another
        // daemon — is on the row and is served by that clock. Saying which of the two happened is
        // the whole of this sentence's honesty, and it lands on the entry's own row as evidence.
        if self.registry.get(&host).is_some() {
            self.bell.ring_wake(&host);
            return Ok(format!(
                "a gatekeeper reviews `{}` at base `{}` as a `task` child of `{host}` — the review \
                 queue's own session, whose board holds this entry and whose head its asks reach. \
                 The verdict lands on the entry's row and nothing lands without it.",
                req.branch, req.base_sha
            ));
        }
        Ok(format!(
            "a gatekeeper reviews `{}` at base `{}` as a `task` child of `{host}`, the review queue's \
             session — which this daemon has not opened yet, so nothing was rung: the request is on \
             this row and the daemon's own clock serves it on its next pass \
             (`Sessions::ensure_gatekeeper` → `Sessions::serve_reviews`). Nothing lands without a \
             verdict.",
            req.branch, req.base_sha
        ))
    }

    /// **The project's agent is told its repository has no gate**: the root session that queued
    /// the entry is rung (resumed if it is not live), and its wake finds the entry waiting and
    /// tells its model (`Harness::queue_notices`).
    fn gate_missing(&self, entry_id: &str) -> Result<(), String> {
        let (_, root) = self.root_for(entry_id, "ask for a merge gate")?;
        if self.bell.is_closed() {
            return Err("the daemon's bell is closed (it is shutting down)".into());
        }
        self.bell.ring_wake(&root);
        Ok(())
    }

    /// **Whether a gatekeeper exists that will judge an entry's branch** — the never-started case
    /// of [`NoGatekeeper`], and the one question only this door can answer: it is the half holding
    /// the registry and the store.
    ///
    /// # The two places a gatekeeper can be
    ///
    /// * **The registry** — the session is live in this daemon, or in the store as something this
    ///   daemon can resume (`Registry::resumable`), which is the same answer one restart apart.
    /// * **The store** — a row for `s-gatekeeper` that no daemon holds yet. That is the state a
    ///   daemon restarts into, and `Sessions::ensure_gatekeeper` resumes it on the next idle pass.
    ///
    /// Neither is a gatekeeper *right now*, and that is why the grace is here rather than a
    /// verdict: [`GATEKEEPER_GRACE_MS`] is the whole of the difference between *not open yet* and
    /// *never started*, and it is counted from the entry's own `asked_ms` — the request row the
    /// first ask wrote — so the clock starts when the queue started waiting rather than when this
    /// question was first asked. An entry whose review has never been asked for has no row and no
    /// elapsed time, so the first ask always goes out and this stays out of the way until the
    /// request is old enough to be evidence.
    ///
    /// **What it deliberately does not ask.** Whether a harness is RUNNING the session — a session
    /// that is in the registry and never opened is invisible from here. That case is left to the
    /// review row's own failure counter (`ReviewRetry::Exhausted`), which catches it the moment an
    /// attempt is made, and to the queue's own clock, which serves every session it holds. Naming
    /// the gap is the honest half: a door that guessed at it would refuse asks it cannot judge.
    fn gatekeeper_ready(&self, entry_id: &str) -> Result<(), String> {
        let host = crate::sessions::gatekeeper::SESSION;
        if self.registry.get(host).is_some() || self.registry.resumable(host).is_some() {
            return Ok(());
        }
        let asked_ms = self
            .store
            .merge_review(entry_id)
            .map_err(|e| format!("the review row for `{entry_id}` could not be read: {e}"))?
            .map(|r| r.asked_ms);
        let now_ms = (crate::config::now_ns() / 1_000_000) as u64;
        match asked_ms {
            Some(asked) if now_ms.saturating_sub(asked) >= GATEKEEPER_GRACE_MS => Err(format!(
                "the review queue's session `{host}` is in neither this daemon nor the store, and \
                 the review of this entry has been outstanding for {}s without it — so no \
                 gatekeeper was ever started for it and none will be: the clock that opens that \
                 session has had its chance ({GATEKEEPER_GRACE_MS}ms). Nothing was asked.",
                now_ms.saturating_sub(asked) / 1_000
            )),
            _ => Ok(()),
        }
    }

    /// **The project that owns the branch is told that no gatekeeper can judge it** — the
    /// fallback, and the door it goes through.
    ///
    /// # Which session, and how it is found
    ///
    /// **The root of the session tree the entry came from** — [`root_session`]'s walk up the
    /// stored parents, which is the session a person attaches to and the one that owns the
    /// worktree — and it is found by [`GatekeeperDoor::root_for`], the same door `gate_missing`
    /// takes: live in this daemon, or resumed from the store by its own row, or refused by name
    /// when it is in neither. The refusal is the answer to *what happens when that session is gone
    /// too*: `Err`, which the queue writes onto the entry's row so a person reading the pane sees
    /// that the fallback itself fell through, and the person's verbs (`/queue approve`,
    /// `/queue restart`, `/queue rm`) are what is left.
    ///
    /// **Not the queue's own session**, which is what `GatekeeperDoor::wake` rings: the queue's
    /// session hosts reviews, and a review is exactly what nobody can run here. The project's
    /// session is the one that can act, because *"their code lives inside them in worktrees
    /// anyway"*.
    ///
    /// # What the wake delivers
    ///
    /// Nothing on this side: a ring has no payload. The message is built by the session that is
    /// rung, out of the entry's own row (`handed_over_to` → `no_gatekeeper_message`), which is why
    /// the daemon writes that row BEFORE it calls this (`MergeQueueDaemon::hand_over`) — the same
    /// order, and for the same reason, as `gate_missing`.
    fn no_gatekeeper(&self, entry_id: &str, why: &str) -> Result<String, String> {
        let (entry, root) = self.root_for(entry_id, "gate the branch and land it itself")?;
        if self.bell.is_closed() {
            return Err(format!(
                "the daemon's bell is closed (it is shutting down), so `{root}` was not rung about \
                 `{entry_id}`. The row is on the entry and the next daemon rings for it."
            ));
        }
        self.bell.ring_wake(&root);
        Ok(format!(
            "the session `{root}` that `{entry_id}` (branch `{}`) came from was rung: {why} It \
             owns the worktree the branch is checked out in, so it can run this repository's gate \
             there and land the branch itself — the queue records the landing on its next pass — or \
             put `/queue approve {entry_id}` to the operator. Nothing lands through the queue's \
             gate without a verdict.",
            entry.branch
        ))
    }
}

// ===== The three doors: what a REPORTED verdict does =====
//
// The design note's own three, and there are exactly three because the gatekeeper's closed set has
// exactly three words. A verdict used to be read by the session that spawned the reviewer, out of
// the reviewer's own closing block (`Harness::review_outcome`); the submission moving into the
// child's own hands is the slice this one follows, and what it needs is the half that was implicit
// while the parent read the answer: **what the verdict DOES**.
//
// * **`accept` → the entry goes to the merge queue.** Through [`hand_over`] — the SAME path
//   `/queue approve` takes, so there is one answer to *what makes an entry takeable* rather than
//   two. The person's verb is that path with the operator's rules in front of it ([`approvable`]),
//   and a reviewer's `accept` is that path with the reviewer's reasons on the row.
// * **`reject` → the reviewer's complaints go back to a LIVE session, and there are TWO parties
//   they can go to.** The child that wrote the branch is asked FIRST — it is the one that can carry
//   the work on in the worktree, and the party the operator's own model of this queue names (*"if
//   it is not happy - it sends a message with complaints to original subagent"*) — and the session
//   that ENQUEUED the entry is asked when the child's session is gone, which is the ordinary case
//   for a subagent that has finished: it is the root of the entry's tree, and its model is the one
//   holding the verbs that move the entry. One delivery, and the report says WHICH of the two took
//   it ([`refusal_addressees`]); a child whose session has closed and a root nobody can reach leave
//   the complaints where they are — [`complaints_instead`] is that answer, once.
// * **`needs_human` → a question goes to the head**, through the one door a question has
//   (`letibot_tools::builtins::intent::Questioner`, the rendezvous `ask_user_question` already
//   rides). An abstention is not a park in silence: the entry does not land, and the person is
//   ASKED rather than left to notice.
//
// **The doors move the VERDICT; the queue moves the ENTRY.** An `accept` has to move the entry
// itself, because an accepted branch is only taken if its row is `waiting` — and that is exactly
// what the person's own path does, which is why it is the one reused. A `reject` does not: the
// queue's own pass parks a refused entry `Failed` with the verdict on its row
// ([`StepOutcome::Refused`], [`ReviewGate::Refused`]), and a second writer of that state would be
// two answers to *where is this entry*. What the refuse door owns is the MESSAGE, which nothing
// else sends — and what the cannot-decide door owns is the QUESTION, which nothing else asks.

/// **Which of the three doors a verdict's word goes through.**
///
/// The mapping is `Decision`'s closed set and nothing else — `accept`, `reject`, `needs_human` —
/// and a word outside it opens NO door (`None`). That is [`review_gate`]'s own rule one step
/// earlier: a verdict nobody can read is not permission, and it is not a refusal either, so it is
/// not a door — the caller says so rather than picking one for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Door {
    /// **Accept** — the entry goes to the merge queue.
    Accept,
    /// **Refuse** — the complaints go back to a live session: the child that wrote the branch, or
    /// the root that enqueued it when that child is gone.
    Refuse,
    /// **Cannot-decide** — a question goes to the head.
    CannotDecide,
}

impl Door {
    /// **The door a verdict's word opens**, or `None` for a word that is not a verdict.
    ///
    /// One mapping, and it is the one a STORED word goes through as well as a reported one: the
    /// review row keeps the word and not the enum (`ReviewRecord::decision`), so a reader that has
    /// the row and a caller that has a report ask the same question here rather than each spelling
    /// the three words out again.
    pub fn of(word: &str) -> Option<Door> {
        match Decision::parse(word)? {
            Decision::Accept => Some(Door::Accept),
            Decision::Reject => Some(Door::Refuse),
            Decision::NeedsHuman => Some(Door::CannotDecide),
        }
    }

    /// The word this door is the door of.
    pub fn as_str(&self) -> &'static str {
        match self {
            Door::Accept => "accept",
            Door::Refuse => "reject",
            Door::CannotDecide => "needs_human",
        }
    }
}

/// **A verdict as the child reported it** — the judgement, its reasons, and what it looked at.
///
/// The four fields are the four the reviewer's closing block carries and the four the review row
/// holds, so a report is the row's own shape one hop earlier: what arrives at the daemon is what
/// will be written, and nothing here has to guess at a fifth thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported {
    /// The judgement, out of the gatekeeper's closed set — a `Decision` and not a word, so a
    /// verdict that reached this type is one the queue can act on.
    pub decision: Decision,
    /// Why, one per line, exactly as the reviewer wrote them.
    pub reasons: Vec<String>,
    /// The files it read.
    pub files: Vec<String>,
    /// The commands it ran.
    pub commands: Vec<String>,
}

/// **What one door did** — the three endings, and each carries the fact its name promises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    /// **Accept** — the entry is in the queue: back to `waiting` with an accepting verdict on its
    /// review, which is the state the queue's own pass takes.
    Queued(String),
    /// **Refuse** — the complaints, and where they went.
    ///
    /// **`child` is named whether or not anything landed.** *Who the complaints are for* is a fact
    /// about the verdict — the entry's `session_id`, the subagent that finished the branch, which
    /// `entry_for_finished` writes as the child's own handle — and not about this attempt to deliver
    /// them; a report that dropped the id when the session had closed would be a refusal nobody
    /// could trace back.
    ///
    /// **`reached` is WHICH of the two parties took them** ([`refusal_addressees`]), or `None` when
    /// neither did. That is not decoration: the child that wrote the branch and the root that
    /// enqueued it are two sessions with two different things to do about one refusal, and a report
    /// that named only the child would say *the complaints went to a session* about a delivery the
    /// ROOT made. `said` is the sentence either way — the party that took them and the door's own
    /// words when they landed, and [`complaints_instead`] when they did not.
    Refused {
        child: String,
        reached: Option<Addressee>,
        said: String,
    },
    /// **Cannot-decide** — a question was raised at the head.
    ///
    /// `answered_by` is `None` when nobody could take it (no head attached, or the deadline
    /// passed): the question was still RAISED, which is the door's act, and `said` carries why it
    /// went unanswered. An abstention is not a park in silence either way.
    Asked {
        question: Question,
        answered_by: Option<String>,
        said: String,
    },
}

/// **What a reported verdict does** — the three doors, from the verdict and the entry.
///
/// `by` is the session the verdict came from, and it is written on the review row: the pane draws
/// that column as *attach to it to read the argument*, and after a verdict the argument is the
/// reviewer's.
///
/// # The seams
///
/// `deliver` and `ask` are seams, [`veto`]'s own shape and for its reason: the act is a function of
/// a store and two callbacks, so a test drives all three doors with no socket, no child and no
/// head. `deliver(session, text)` is the daemon's `task_message` door (`TaskRunner::send`, the
/// `CommandKind::Message` a session's own reader answers by starting a turn); `ask(&Question)` is
/// the questioner the session already has (`crate::answers::HeadQuestioner`), and it is the
/// caller's business whether it blocks — this function asks and reports.
///
/// **The refuse door asks `deliver` more than once, and that is the point of it.** A refusal has two
/// parties it can reach ([`refusal_addressees`]): the child that wrote the branch, and — when that
/// session is gone, which is the ordinary end of a subagent — the root that enqueued it. One
/// delivery, to the first party that takes it, and the report says which one that was.
///
/// # A delivery that failed does not undo a refusal
///
/// The refusal is written on the review row FIRST and the message is sent after, so a child whose
/// session has closed leaves a verdict that stands and a message that did not go. That is
/// [`Done::Refused`] with `reached: None`, not an `Err`: the entry is refused either way, and what a
/// person needs is where the complaints are instead — [`complaints_instead`], with each attempt's
/// own words rather than an assertion about why they failed.
///
/// # The word that opens no door
///
/// `Reported::decision` is the closed set, so the `None` arm below cannot be reached from a
/// `Reported` — and it is written out rather than `expect`ed because [`Door::of`] is also the
/// mapping a STORED word goes through, and a `panic` in the daemon is not how a word nobody knows
/// should end.
pub fn report_verdict(
    store: &Store,
    entry_id: &str,
    reported: &Reported,
    by: &str,
    now_ms: u64,
    deliver: &dyn Fn(&str, &str) -> Result<String, String>,
    ask: &dyn Fn(&Question) -> Result<(QuestionAnswer, String), AskError>,
    events: &dyn Fn(letibot_sessionlog::SessionEvent),
) -> Result<Done, String> {
    let entry = read_entry(store, entry_id)?;
    let door = Door::of(reported.decision.as_str()).ok_or_else(|| {
        format!(
            "`{}` is not a verdict on `{}`: the words are {}, and a word outside the closed set \
             opens no door. Nothing was moved.",
            reported.decision.as_str(),
            entry.branch,
            Decision::ALL
                .iter()
                .map(|d| d.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    match door {
        // **The entry goes to the merge queue** — the same path `/queue approve` takes, with the
        // reviewer's reasons on the entry's evidence and no operator in the sentence at all.
        Door::Accept => {
            let said = hand_over(
                store,
                &entry,
                by,
                Handed::ByTheReview,
                "the verdict on it is `accept`, from the reviewer that judged it",
                &reasons_block(&reported.reasons),
                now_ms,
                events,
            )?;
            Ok(Done::Queued(said))
        }
        // **The complaints go back to a live session** — the child that wrote the branch, or the
        // root that enqueued it when the child's own session is gone. The row first, so the verdict
        // is durable before anything is sent — and so [`refusal_message`] has the review to quote.
        Door::Refuse => {
            let rec = record_the_verdict(store, &entry, reported, by, now_ms)?;
            // **The delivery's own answer, and not a reading of the sentence.** A refusal is not
            // undone by a message that did not go, so a failure here is a REPORT and not an `Err`:
            // the verdict is on the row either way, and what a person needs is where the complaints
            // are instead ([`complaints_instead`]) — or, when the root took them, that the party the
            // complaints reached is not the child ([`refusal_delivered`]).
            let (reached, said) = match deliver_the_refusal(store, &entry, Some(&rec), deliver) {
                Delivered::To { to, how, missed } => {
                    let said = refusal_delivered(&entry, &to, &how, &missed);
                    (Some(to), said)
                }
                Delivered::Nobody(missed) => (None, complaints_instead(&entry, &missed)),
            };
            Ok(Done::Refused {
                child: child_of(&entry),
                reached,
                said,
            })
        }
        // **A question at the head, and the entry does not land on an abstention.** The verdict is
        // written first for the same reason a refusal is: the queue's own gate reads the row, and a
        // `needs_human` is a refusal to it ([`ReviewGate::Refused`]) — so the question is the
        // person's, and the entry's state is already the queue's answer.
        Door::CannotDecide => {
            // The row first, so the queue's own gate refuses it before anybody is asked.
            record_the_verdict(store, &entry, reported, by, now_ms)?;
            let question = question_for(&entry, reported);
            let (answered_by, said) = match ask(&question) {
                Ok((answer, who)) => (
                    Some(who.clone()),
                    format!(
                        "`{}` did not decide, so `{}` does not land on it — the question was \
                         raised at the head and `{who}` answered it: {}",
                        reported.decision.as_str(),
                        entry.branch,
                        answer.render(&question).trim()
                    ),
                ),
                Err(why) => (
                    None,
                    format!(
                        "`{}` did not decide, so `{}` does not land on it. The question WAS \
                         raised at the head and nobody answered it: {why} The entry is not parked \
                         in silence: its review row says `{}`, with the reviewer's reasons on it, \
                         and `/queue restart {}` asks the reviewer again, `/queue approve {}` \
                         overrides the verdict, and `/queue rm {}` drops the entry.",
                        reported.decision.as_str(),
                        entry.branch,
                        reported.decision.as_str(),
                        letibot_sessionlog::registry::short_id(&entry.id),
                        entry.id,
                        entry.id,
                    ),
                ),
            };
            Ok(Done::Asked {
                question,
                answered_by,
                said,
            })
        }
    }
}

// ===== Who a refusal's complaints reach =====
//
// **The row holds one party and the tree holds the other, and they are not the same party.** A
// `task_start` child writes the branch and its own handle is both the entry's `id` and its
// `session_id` — `HarnessTaskRunner::enqueue_entry` calls [`entry_for_finished`] with the handle
// twice — so *the child whose work it is* is a fact the row carries. The MODEL that enqueued it is
// not on the row at all: it is the ROOT of the child's session tree ([`root_session`]), the session
// a person attaches to and the one that ran the `task_start`. With the review queue now a session
// of its own whose children are the reviewers, those two are, by design, usually different parties.
//
// **So a refusal goes to the child first and to the root second.** The child is the party that can
// carry the branch on in the worktree, and it is the one the operator's own model names — *"if it
// is not happy - it sends a message with complaints to original subagent"*. But a child that
// finished is a child whose turn has ended, and by the time a reviewer has read its branch its
// session may be gone altogether: that is the measured defect this door answers (`queue: a
// reviewer's refusal reaches the MODEL that enqueued it`, 5741101), where entries sat `failed` with
// the verdict on their rows while the sessions that enqueued them carried on as if the work were
// fine. The root is still a model that can act: it holds the verbs that move the entry
// (`/queue restart|approve|rm`), and the entry is its branch's business.
//
// **One delivery, and the report names the party that took it.** Not two messages: the complaints
// are one thing to say, and the second party is a FALLBACK for the first rather than a second
// audience — a refusal said twice into one tree is the furniture this tree keeps refusing to make.
// When neither party takes them the sentence names both attempts with each one's own words, which
// is what makes the fallback TRUE: the door no longer asserts that a session is gone, it quotes the
// door that could not reach it.

/// **Which of the two parties a refusal can reach.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaches {
    /// **The child that wrote the branch** — the entry's `session_id`, the subagent that finished
    /// the work, and the session that can carry it on in the worktree.
    Child,
    /// **The root session that enqueued it** — the session whose `task_start` started that child,
    /// found by walking the stored parents ([`root_session`]).
    ///
    /// **The store's own column doc calls the CHILD "the session that enqueued it"**, because the
    /// child enqueued the BRANCH; the model that enqueued the WORK is this one, one walk up. The two
    /// readings are the whole reason this enum exists.
    Root,
}

impl Reaches {
    /// What this party is to the entry, in the words the sentences use.
    pub fn as_str(&self) -> &'static str {
        match self {
            Reaches::Child => "the child that wrote the branch",
            Reaches::Root => "the root session that enqueued it",
        }
    }
}

/// **One party a refusal's complaints can go to** — which one it is, and the session to send to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addressee {
    /// Which party this is to the entry.
    pub who: Reaches,
    /// The session a message about the refusal is sent to.
    pub session: String,
}

impl Addressee {
    /// The party and its session, as a clause a sentence can carry.
    pub fn describe(&self) -> String {
        format!("`{}`, {}", self.session, self.who.as_str())
    }
}

/// **The child that wrote the branch**, as the entry names it — `session_id`, the column that
/// documents itself as the subagent that finished the work, with the entry's `id` for a row whose
/// column is empty. In production they are the same handle; a row written by hand may differ, and
/// the column is the one that promises to be a SESSION.
///
/// **`veto` keys on the entry's `id`** — the person's verb, whose sentence is written for the child
/// that did the work — and for every entry the enqueuer writes the two are the same handle, so the
/// two doors agree. This column is the one the walk to the root needs, which is why the refusal
/// reads it.
pub fn child_of(entry: &MergeEntry) -> String {
    if entry.session_id.trim().is_empty() {
        entry.id.clone()
    } else {
        entry.session_id.clone()
    }
}

/// **Who a refusal's complaints are for, in the order they are tried** — the child, then the root.
///
/// **The walk is what names the root, and it needs the child's own session row.** An entry carries
/// the child and nothing else about the tree, so a child whose row has gone leaves the root unnamed,
/// and this then returns the one party it can name rather than inventing a second: a session id
/// guessed here would be a message to nobody in particular. What that costs is said where it lands
/// — [`complaints_instead`] names the parties that were actually tried.
pub fn refusal_addressees(store: &Store, entry: &MergeEntry) -> Vec<Addressee> {
    let child = child_of(entry);
    let mut to = vec![Addressee {
        who: Reaches::Child,
        session: child.clone(),
    }];
    let root = root_session(store, &child);
    if root != child {
        to.push(Addressee {
            who: Reaches::Root,
            session: root,
        });
    }
    to
}

/// **What handing a refusal over did** — the party that took the complaints, or why nobody did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    /// **This party took them.** `how` is the delivery door's own words; `missed` is the parties
    /// tried before it, in order, each in its own name — empty when the first party took them.
    To {
        to: Addressee,
        how: String,
        missed: Vec<String>,
    },
    /// **Neither party took them**, and these are the attempts in order, each in its own name.
    Nobody(Vec<String>),
}

/// **The complaints, to the first party that will take them.**
///
/// The order and the walk are [`refusal_addressees`]'s; what is here is the trying. **The two
/// parties are sent two different sentences**, because the same words are not true of both: the
/// child's ([`refusal_message`]) says *your branch … is back with you*, which is false of a session
/// that did not write it, and the root's ([`enqueuer_refusal_message`]) says that a child it started
/// was refused and that the entry is its to move. What both carry whole is the reviewer's own
/// reasons.
///
/// `deliver` is the daemon's message door, [`veto`]'s own seam, and a failure is a REPORT rather
/// than an `Err`: the verdict is on the review row either way, and the second party is what a
/// failure here is for.
pub fn deliver_the_refusal(
    store: &Store,
    entry: &MergeEntry,
    review: Option<&letibot_tokencore::store::ReviewRecord>,
    deliver: &dyn Fn(&str, &str) -> Result<String, String>,
) -> Delivered {
    let mut missed: Vec<String> = Vec::new();
    for to in refusal_addressees(store, entry) {
        let text = match to.who {
            Reaches::Child => refusal_message(entry, review),
            // **The child's own failure is quoted, not paraphrased.** It is the reason this party is
            // being asked at all, and a reader who cannot see it cannot tell a dead subagent from a
            // daemon that would not send.
            Reaches::Root => enqueuer_refusal_message(entry, review, &missed_clause(&missed)),
        };
        match deliver(&to.session, &text) {
            Ok(how) => return Delivered::To { to, how, missed },
            Err(why) => missed.push(format!("{}: {why}", to.describe())),
        }
    }
    Delivered::Nobody(missed)
}

/// **The parties that could not be reached, as one clause** — each in its own name, so a sentence
/// built from it says WHO was tried rather than only that somebody was not.
fn missed_clause(missed: &[String]) -> String {
    if missed.is_empty() {
        // Unreachable from [`deliver_the_refusal`], which only asks the second party after the first
        // has failed — and written out rather than `expect`ed because a sentence is no place for a
        // panic, and an empty clause would read as *nobody was tried*.
        return "the party before it could not be reached".to_string();
    }
    missed.join("; and ")
}

/// **The door's sentence for a refusal that landed** — and the half that says WHICH party took it.
///
/// Not decoration: the child that wrote the branch and the root that enqueued it are two sessions
/// with two different things to do about one refusal, and a sentence that named only the child would
/// say *the complaints went to a session* about a delivery the ROOT made. When the child was skipped
/// the reason is in it, quoted from the door that could not reach it.
fn refusal_delivered(entry: &MergeEntry, to: &Addressee, how: &str, missed: &[String]) -> String {
    match to.who {
        Reaches::Child => format!(
            "The complaints went to `{}`, the child that wrote `{}`: {how}",
            to.session, entry.branch
        ),
        Reaches::Root => format!(
            "The child that wrote `{}` could not be told ({}) — so the complaints went to `{}`, the \
             root session that enqueued it: {how}",
            entry.branch,
            missed_clause(missed),
            to.session
        ),
    }
}

/// **Where a refusal's complaints go when NOBODY can be reached.**
///
/// The operator's model of this queue, in their words: *"if it is not happy - it sends a message
/// with complaints to original subagent"* — and a subagent whose session has closed cannot be sent
/// anything, nor can a root that is not live either. That is not a reason to drop the complaints,
/// and it is not a reason to call the refusal a failure: the verdict is already written on the
/// review row, and the entry does not land on it. So this names, once, where the complaints ARE:
///
/// * **the review row** (`merge_review`), with the reviewer's reasons, the files it read and the
///   commands it ran — which `/queue` and the merge-queue pane draw, and which a person reads by
///   attaching to the session that reviewed it;
/// * **the entry's own `evidence`**, where the move is recorded;
/// * **the operator**, who moves the entry by hand.
///
/// **`missed` is what makes it true.** It is the attempts, in order, each in its own name and with
/// its own words — so the sentence QUOTES the delivery that failed instead of asserting why it did.
/// The sentence this replaces said *that session is gone* about every failure there is, which is
/// false for a daemon whose bell has closed, for an entry that left the queue mid-write, and for a
/// row whose `session_id` names nothing at all; a fallback that says a false thing about WHY is
/// worse than one that says only what happened.
///
/// A function rather than a sentence built at the call site so the two cannot drift: the warning the
/// daemon publishes, the door's own report and this paragraph are one answer to one question, and
/// the branch and the entry are in all three.
pub fn complaints_instead(entry: &MergeEntry, missed: &[String]) -> String {
    let short = letibot_sessionlog::registry::short_id(&entry.id);
    let why = missed_clause(missed);
    format!(
        "The complaints did NOT reach a live session — {why}. A refusal is not a delivery: the \
         verdict stands, and `{branch}` does not land on it. Where the complaints are instead: on \
         the review row of `{id}`, with the reviewer's reasons, the files it read and the commands \
         it ran — which `/queue` and the merge-queue pane draw, and which a person reads by \
         attaching to the session that reviewed it; on the entry's own evidence; and with the \
         operator, who moves the entry by hand — `/queue restart {short}` asks the reviewer again \
         about the branch as it is now, `/queue approve {id}` overrides the verdict and lets the \
         queue land it, and `/queue rm {id}` drops the entry.",
        branch = entry.branch,
        id = entry.id,
        why = why,
        short = short,
    )
}

/// **A reviewer's reasons, as a paragraph for the entry's `evidence`** — one per line, indented,
/// and empty when the reviewer wrote none.
///
/// Empty rather than a placeholder sentence, because the evidence's own tail already says what
/// happened: *the reviewer wrote no reasons down* is a fact the review row carries (an empty
/// `reasons` beside a decision), and prose invented here would be this code writing the reviewer's
/// half for it.
fn reasons_block(reasons: &[String]) -> String {
    if reasons.is_empty() {
        return String::new();
    }
    let mut out = String::from("The review's reasons:\n");
    for reason in reasons {
        out.push_str(&format!("\n  - {reason}"));
    }
    out.push('\n');
    out
}

/// **The verdict, onto the entry's review row** — the row the queue's own gate reads
/// ([`review_gate`]), and the row a person reads by attaching to the session that reviewed it.
///
/// `asked_ms` is the row's own and is NOT restamped (`Harness::write_outcome`'s rule, one host
/// over): it is when the review was first asked for, which is a fact about the review rather than
/// about the verdict, and a restart is the one thing that restamps it (`Store::restart_review`).
///
/// **The write is unconditional and the row is the arbiter of nothing here**, which is the
/// difference between this and a person's verb: the reviewer IS the live writer `approvable`
/// protects a row from, so the door has no guard to keep — it is the answer arriving.
fn record_the_verdict(
    store: &Store,
    entry: &MergeEntry,
    reported: &Reported,
    by: &str,
    now_ms: u64,
) -> Result<letibot_tokencore::store::ReviewRecord, String> {
    let asked_ms = store
        .merge_review(&entry.id)
        .ok()
        .flatten()
        .map(|r| r.asked_ms)
        .unwrap_or(now_ms);
    let rec = letibot_tokencore::store::ReviewRecord {
        entry_id: entry.id.clone(),
        session_id: by.to_string(),
        branch: entry.branch.clone(),
        base_sha: entry.base_sha.clone(),
        asked_ms,
        answered_ms: Some(now_ms),
        decision: Some(reported.decision.as_str().to_string()),
        // **A verdict clears the last attempt's failure**, `write_outcome`'s own rule: leaving a
        // failure beside a decision would make a reader ask which of the two the queue acted on.
        attempts: 0,
        failed_ms: None,
        failure: String::new(),
        reasons: reported.reasons.clone(),
        files: reported.files.clone(),
        commands: reported.commands.clone(),
    };
    store
        .put_review(&rec)
        .map_err(|e| format!("the verdict on `{}` could not be written: {e}", entry.id))?;
    Ok(rec)
}

/// **The question a `cannot-decide` raises** — the entry, what the reviewer said, and the three
/// moves a person has.
///
/// The options are the operator's own verbs rather than a paraphrase of them, because the person
/// answering this card is answering it to move an entry: accept it and let the queue land it, send
/// it back to the child with the reviewer's complaints, or drop it. `because` carries the
/// reviewer's reasons verbatim — a question whose *why* is a summary of the reviewer is a question
/// the person has to go and check.
fn question_for(entry: &MergeEntry, reported: &Reported) -> Question {
    Question {
        text: format!(
            "The review of `{}` (entry `{}`) cannot decide, so nothing lands. The branch is `{}`, \
             cut from `{}`, and the ask it was written for was: {}",
            entry.branch,
            entry.id,
            entry.branch,
            entry.base_sha,
            first_line(&entry.brief)
        ),
        options: vec![
            "accept it — the queue lands it".into(),
            "send it back to the child with the reviewer's complaints".into(),
            "drop the entry from the queue".into(),
        ],
        because: if reported.reasons.is_empty() {
            format!(
                "the reviewer answered `{}` and wrote no reasons down; its review row is the whole \
                 of what it said",
                reported.decision.as_str()
            )
        } else {
            format!("the reviewer said: {}", reported.reasons.join("; "))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `MergeEntry` for the tests, with the fields the test does not care about set to
    /// defaults.
    pub(super) fn entry(
        id: &str,
        priority: MergePriority,
        state: MergeState,
        created_ms: u64,
    ) -> MergeEntry {
        MergeEntry {
            id: id.into(),
            session_id: "s".into(),
            branch: format!("b-{id}"),
            base_sha: "base".into(),
            priority,
            needs: vec![],
            state,
            brief: String::new(),
            evidence: String::new(),
            created_ms,
            updated_ms: created_ms,
            worktree: None,
            landed_sha: None,
            gate_steps: Vec::new(),
        }
    }

    /// **An operator's urgent entry jumps subagent work** — the whole of the scheduling, and
    /// the reason there are exactly two rungs.
    #[test]
    fn an_urgent_entry_jumps_subagent_work() {
        let subagent = entry("sub", MergePriority::Subagent, MergeState::Waiting, 1_000);
        let urgent = entry("urg", MergePriority::Urgent, MergeState::Waiting, 2_000);
        // The subagent entry is older, but the urgent entry is taken first.
        let entries = vec![subagent, urgent];
        assert_eq!(
            next_ready(&entries),
            Some(1),
            "the urgent entry jumps the older subagent entry"
        );
    }

    /// **Ties inside a rung break by age** — the oldest first, and the id is the final
    /// tiebreaker so two entries enqueued in the same millisecond do not reorder.
    #[test]
    fn ties_break_by_age_then_id() {
        let older = entry("b", MergePriority::Subagent, MergeState::Waiting, 1_000);
        let newer = entry("a", MergePriority::Subagent, MergeState::Waiting, 2_000);
        let entries = vec![newer, older];
        assert_eq!(
            next_ready(&entries),
            Some(1),
            "the older entry is taken first"
        );

        // Same age: the id breaks the tie, so the order is stable.
        let a = entry("a", MergePriority::Subagent, MergeState::Waiting, 1_000);
        let b = entry("b", MergePriority::Subagent, MergeState::Waiting, 1_000);
        let entries = vec![b, a];
        assert_eq!(next_ready(&entries), Some(1), "the id breaks the tie");
    }

    /// **An entry is not taken until its `needs` have all `Landed`** — the dependency gating,
    /// and the reason a `Waiting` entry that is not ready is listed with its dependencies
    /// rather than dropped.
    #[test]
    fn an_entry_waits_on_its_dependencies() {
        let dep = entry("dep", MergePriority::Subagent, MergeState::Waiting, 1_000);
        let mut dependent = entry(
            "dep-ent",
            MergePriority::Subagent,
            MergeState::Waiting,
            2_000,
        );
        dependent.needs = vec!["dep".into()];
        let entries = vec![dep.clone(), dependent.clone()];
        // The dependency is not landed, so the dependent is not ready — and the dependency
        // itself is taken first.
        assert_eq!(
            next_ready(&entries),
            Some(0),
            "the dependency is taken before the dependent"
        );
        assert!(
            !is_ready(&dependent, &entries),
            "the dependent is not ready"
        );
        assert_eq!(
            unmet_needs(&dependent, &entries),
            vec!["dep"],
            "the unmet dependency is named"
        );

        // The dependency lands, and the dependent is ready.
        let mut landed_dep = dep.clone();
        landed_dep.state = MergeState::Landed;
        landed_dep.landed_sha = Some("tip".into());
        let entries = vec![landed_dep, dependent.clone()];
        assert!(
            is_ready(&dependent, &entries),
            "the dependent is ready once its dependency lands"
        );
        assert_eq!(
            next_ready(&entries),
            Some(1),
            "the dependent is taken once its dependency lands"
        );
    }

    /// **A dependency that is not in the queue is unmet** — an entry that names a dependency
    /// the queue has never held is an entry the queue cannot vouch for, and reading it as
    /// *met* would be a guess.
    #[test]
    fn a_missing_dependency_is_unmet() {
        let mut dependent = entry(
            "dep-ent",
            MergePriority::Subagent,
            MergeState::Waiting,
            1_000,
        );
        dependent.needs = vec!["ghost".into()];
        let entries = vec![dependent.clone()];
        assert!(
            !is_ready(&dependent, &entries),
            "a missing dependency is not met"
        );
        assert_eq!(
            unmet_needs(&dependent, &entries),
            vec!["ghost"],
            "the missing dependency is named"
        );
        assert_eq!(
            next_ready(&entries),
            None,
            "the entry is not taken while its dependency is missing"
        );
    }

    /// **The base becomes the landed tip rather than the stale SHA** — the half of the
    /// dependency rule that makes the rebase true: a dependent rebases onto the SHA its
    /// dependency landed at, not the SHA it was written against.
    #[test]
    fn the_base_becomes_the_landed_tip() {
        let mut dep = entry("dep", MergePriority::Subagent, MergeState::Landed, 1_000);
        dep.landed_sha = Some("landed-tip".into());
        dep.updated_ms = 5_000;
        let mut dependent = entry(
            "dep-ent",
            MergePriority::Subagent,
            MergeState::Waiting,
            2_000,
        );
        dependent.needs = vec!["dep".into()];
        dependent.base_sha = "stale-sha".into();
        let entries = vec![dep, dependent.clone()];
        assert_eq!(
            effective_base(&dependent, &entries),
            "landed-tip",
            "the base is the landed tip, not the stale SHA"
        );

        // **And it is not the entry's own `needs` that decide the base.** The queue is serial
        // and everything lands on main, so the last thing to land is the current tip whatever
        // an entry's `needs` say: an entry with no dependencies at all still rebases onto it,
        // which is the difference between a gating condition and a base condition.
        let independent = entry("ind", MergePriority::Subagent, MergeState::Waiting, 3_000);
        assert_eq!(
            effective_base(&independent, &entries),
            "landed-tip",
            "an entry with no dependencies rebases onto the current tip of main"
        );

        // **With nothing landed, the base is the SHA the entry was written against** — nothing
        // has moved since it was cut, so its own `base_sha` IS the current tip.
        let nothing_landed = vec![dependent.clone()];
        assert_eq!(
            effective_base(&dependent, &nothing_landed),
            "stale-sha",
            "nothing has landed, so the entry's own base is the tip"
        );
    }

    /// **The last dependency to land is the base** — when an entry has several dependencies,
    /// the base is the one that landed last, because the queue is serial and the last to land
    /// is the tip.
    #[test]
    fn the_last_dependency_to_land_is_the_base() {
        let mut dep1 = entry("dep1", MergePriority::Subagent, MergeState::Landed, 1_000);
        dep1.landed_sha = Some("tip1".into());
        dep1.updated_ms = 1_000;
        let mut dep2 = entry("dep2", MergePriority::Subagent, MergeState::Landed, 2_000);
        dep2.landed_sha = Some("tip2".into());
        dep2.updated_ms = 2_000;
        let mut dependent = entry(
            "dep-ent",
            MergePriority::Subagent,
            MergeState::Waiting,
            3_000,
        );
        dependent.needs = vec!["dep1".into(), "dep2".into()];
        let entries = vec![dep1, dep2, dependent.clone()];
        assert_eq!(
            effective_base(&dependent, &entries),
            "tip2",
            "the base is the last dependency to land"
        );
    }

    /// **A `Taken` entry comes back `Stale`, not `Waiting`** — the recovery that keeps a dead
    /// job from coming back as ready. A row that says `waiting` while its job is dead is a lie
    /// the pane would draw, and the recovery is what stops it.
    #[test]
    fn a_taken_entry_comes_back_stale() {
        let taken = entry("taken", MergePriority::Subagent, MergeState::Taken, 1_000);
        let waiting = entry(
            "waiting",
            MergePriority::Subagent,
            MergeState::Waiting,
            2_000,
        );
        let landed = entry("landed", MergePriority::Subagent, MergeState::Landed, 3_000);
        let recovered = recover(vec![taken, waiting, landed]);
        assert_eq!(
            recovered[0].state,
            MergeState::Stale,
            "the taken entry is stale"
        );
        assert!(
            !recovered[0].evidence.is_empty(),
            "the stale entry has its reason"
        );
        assert_eq!(
            recovered[1].state,
            MergeState::Waiting,
            "the waiting entry is untouched"
        );
        assert_eq!(
            recovered[2].state,
            MergeState::Landed,
            "the landed entry is untouched"
        );

        // And the recovery is idempotent: a second pass does not move the stale entry again.
        let recovered_twice = recover(recovered);
        assert_eq!(
            recovered_twice[0].state,
            MergeState::Stale,
            "the recovery is idempotent"
        );
    }

    /// **The cleanup is decided by the state** — `Landed` removes the worktree and the branch,
    /// and every other state keeps it. The difference is the rule: a tree that is removed
    /// after a failed test destroys the evidence, and a branch that is deleted before it is
    /// merged is a branch the queue cannot vouch for.
    #[test]
    fn the_cleanup_is_decided_by_the_state() {
        assert_eq!(
            cleanup_for(MergeState::Landed),
            Cleanup::RemoveWorktreeAndBranch
        );
        assert_eq!(cleanup_for(MergeState::Failed), Cleanup::KeepWorktree);
        assert_eq!(cleanup_for(MergeState::Conflict), Cleanup::KeepWorktree);
        assert_eq!(cleanup_for(MergeState::Stale), Cleanup::KeepWorktree);
        assert_eq!(cleanup_for(MergeState::Waiting), Cleanup::KeepWorktree);
        assert_eq!(cleanup_for(MergeState::Taken), Cleanup::KeepWorktree);
    }

    /// **A `Stale` entry is not taken** — it is listed with its reason, and it waits for a
    /// re-enqueue rather than a silent retry.
    #[test]
    fn a_stale_entry_is_not_taken() {
        let stale = entry("stale", MergePriority::Urgent, MergeState::Stale, 1_000);
        let entries = vec![stale];
        assert_eq!(next_ready(&entries), None, "a stale entry is not taken");
    }

    /// **A terminal entry is not taken** — `Landed`, `Failed`, `Conflict` and `Stale` are all
    /// terminal, and none of them is taken again.
    #[test]
    fn a_terminal_entry_is_not_taken() {
        for state in [
            MergeState::Landed,
            MergeState::Failed,
            MergeState::Conflict,
            MergeState::Stale,
        ] {
            let e = entry("t", MergePriority::Urgent, state, 1_000);
            let entries = vec![e];
            assert_eq!(next_ready(&entries), None, "{state:?} is not taken");
        }
    }

    // ===== The enqueue: what a finished `task_start` child leaves =====

    /// **The entry a finished child leaves carries the five things the row needs** — and the
    /// brief is the one that cannot be recovered later.
    ///
    /// The reviewer is handed the ask and NOT the child's report, so an entry whose brief was
    /// dropped would be an entry nobody could review against anything. This is the assertion
    /// that the enqueuer's own function fills it from the child's prompt rather than leaving it
    /// empty and calling it somebody else's problem.
    #[test]
    fn a_finished_child_leaves_the_row_the_queue_needs() {
        let placement = letibot_tools::builtins::task::WorktreePlacement {
            path: "/repo/.claude/worktrees/agent-fix-the-bug".into(),
            branch: "agent/fix-the-bug".into(),
            base_sha: "deadbeef".into(),
            main_tree: false,
        };
        let e = entry_for_finished(
            "s-1-sub-42",
            "s-1-sub-42",
            "fix the bug in the parser",
            &placement,
            1_700_000_000_000,
        )
        .expect("a worktree child leaves an entry");

        assert_eq!(e.id, "s-1-sub-42", "the id is the child's own handle");
        assert_eq!(e.session_id, "s-1-sub-42");
        assert_eq!(e.branch, "agent/fix-the-bug");
        assert_eq!(e.base_sha, "deadbeef");
        assert_eq!(
            e.priority,
            MergePriority::Subagent,
            "a child of `task_start` is a subagent's work"
        );
        assert!(
            e.needs.is_empty(),
            "`task_start` takes no dependency argument, so a child waits behind nothing"
        );
        assert_eq!(e.state, MergeState::Waiting, "it is enqueued, not taken");
        assert_eq!(
            e.brief, "fix the bug in the parser",
            "the ask travels with the entry: the reviewer starts from it and nothing else"
        );
        assert_eq!(e.worktree.as_deref(), Some(placement.path.as_str()));
        assert_eq!(e.created_ms, 1_700_000_000_000);
        assert_eq!(e.landed_sha, None);
    }

    /// **The main checkout leaves no entry, and it says so by returning nothing.** A
    /// `task_start` with `main_tree: true` works on main's own branch: there is no branch of
    /// its own to land, and an entry whose branch was `main` would be a queue entry that means
    /// *land main on main*.
    #[test]
    fn a_child_of_the_main_tree_leaves_no_entry() {
        let placement = letibot_tools::builtins::task::WorktreePlacement {
            path: "/repo".into(),
            branch: "main".into(),
            base_sha: "cafe0000".into(),
            main_tree: true,
        };
        assert!(
            entry_for_finished("s-1-sub-7", "s-1-sub-7", "do the work", &placement, 1).is_none(),
            "the main checkout has no branch of its own to land"
        );
    }

    /// **The same child enqueued twice is one entry.** The id is the handle, so the second
    /// write is the same row — which is what lets the two notices (the child's own settlement
    /// and a later `task_result`) be one act rather than two.
    #[test]
    fn the_same_child_enqueued_twice_is_one_row() {
        let placement = letibot_tools::builtins::task::WorktreePlacement {
            path: "/repo/wt".into(),
            branch: "agent/x".into(),
            base_sha: "abc".into(),
            main_tree: false,
        };
        let first = entry_for_finished("sub-1", "sub-1", "ask", &placement, 1_000).unwrap();
        let second = entry_for_finished("sub-1", "sub-1", "ask", &placement, 2_000).unwrap();
        assert_eq!(first.id, second.id, "one child, one id");
        let mut entries = vec![first.clone()];
        // What the store does with the second write: an upsert on the id.
        let at = entries.iter().position(|e| e.id == second.id).unwrap();
        entries[at] = second;
        assert_eq!(entries.len(), 1, "a second enqueue is not a second entry");
    }

    /// **The wire carries the brief**, because the pane draws it and the reviewer reads it — and
    /// the conversion is where the store's row and the head's row meet.
    #[test]
    fn the_wire_entry_carries_the_brief() {
        let mut e = entry("m-1", MergePriority::Subagent, MergeState::Waiting, 1_000);
        e.brief = "the ask, verbatim".into();
        let wire = wire_entry(&e);
        assert_eq!(wire.brief, "the ask, verbatim");
        assert_eq!(wire.id, e.id);
        assert_eq!(wire.branch, e.branch);
        assert_eq!(wire.state, letibot_sessionlog::event::MergeState::Waiting);
    }

    // ===== The git path: a real repo in a temp dir, driven through the daemon =====

    /// Run one git command in `root`, asserting it succeeds. The test's own harness, shaped
    /// on `letibot_tools::detect`'s: a `git -C` and an assert on the status, so a failure
    /// names the command and the output rather than panicking on a bare `expect`.
    fn git(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The SHA of `rev` in `root`.
    fn sha(root: &Path, rev: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", rev])
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// **The store at `path`, opened** — the daemon's connection and the test's are two
    /// connections to one file, which is the shape `jobwatch`'s recorder test uses and the only
    /// one that proves a row is on disk rather than in the connection that wrote it.
    pub(super) fn store_at(path: &Path) -> Store {
        Store::open(path).expect("a store")
    }

    /// **Enqueue one entry through its own connection** — the enqueuer's half: open the file,
    /// write the row, close. The daemon then opens its own and the test opens a third to read,
    /// so nothing asserted below is a value a shared connection kept in hand.
    pub(super) fn enqueue(path: &Path, entry: &MergeEntry) {
        store_at(path)
            .put_merge_entry(entry)
            .expect("the entry row");
    }

    /// **The gatekeeper's verdict on an entry, written where the queue reads it.**
    ///
    /// Every fixture that expects an entry to be TAKEN needs one, and that is the point of the
    /// gate rather than a test convenience: before this, an entry was taken as soon as it was
    /// ready, and now nothing is taken without a verdict that accepts it. A fixture that forgot
    /// this would be a fixture asserting a merge nobody reviewed.
    fn approve(path: &Path, entry_id: &str) {
        let entry = store_at(path)
            .merge_entry(entry_id)
            .expect("reads")
            .expect("the entry");
        store_at(path)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: entry_id.to_string(),
                session_id: crate::mergequeue::REVIEWER_SESSION_ID.to_string(),
                branch: entry.branch,
                base_sha: entry.base_sha,
                asked_ms: 1,
                answered_ms: Some(2),
                decision: Some("accept".into()),
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec!["the artifact does what the brief asked".into()],
                files: vec!["crates/harnessd/src/mergequeue.rs".into()],
                commands: vec!["git diff base...branch".into()],
            })
            .expect("the verdict row");
    }

    /// **A door that records what it was asked and does nothing else** — the reviewer's half of
    /// the wake, in the queue's own tests. The production door writes a row and rings a bell;
    /// what these tests are about is *whether* the queue asked and what it did with the answer.
    #[derive(Default)]
    pub(super) struct RecordingReviewer {
        pub(super) asked: std::sync::Mutex<Vec<(String, String)>>,
        /// The sentence the door answers with. `None` is the production door's refusal, which
        /// is the state a daemon with no reviewer session is in.
        pub(super) says: Option<String>,
        /// **What the door was asked to TELL a project, and with which reason** — `(entry id, why)`
        /// per [`letibot_tools::gatekeeper::Reviewer::no_gatekeeper`], in order. The fallback's
        /// half of the recording: the production door rings a session, and what the tests are about
        /// is *whether the queue told anybody, about what, and how often*.
        pub(super) told: std::sync::Mutex<Vec<(String, String)>>,
        /// The sentence the fallback answers with. `None` is *the project was told*, which is the
        /// production door's ordinary answer; `Some(e)` is a door that could not reach anybody —
        /// the project's session gone too, which the entry's row then has to carry.
        pub(super) told_says: Option<String>,
        /// **What [`letibot_tools::gatekeeper::Reviewer::gatekeeper_ready`] answers** — `None` is
        /// *a gatekeeper exists*, which is the trait's own default; `Some(why)` is the
        /// never-started refusal, which is a door with no registry behind it.
        pub(super) gatekeeper_missing: Option<String>,
    }

    impl letibot_tools::gatekeeper::Reviewer for RecordingReviewer {
        fn wake(
            &self,
            entry_id: &str,
            req: &letibot_tools::gatekeeper::ReviewRequest,
        ) -> Result<String, String> {
            self.asked
                .lock()
                .unwrap()
                .push((entry_id.to_string(), req.branch.clone()));
            match &self.says {
                Some(s) => Ok(s.clone()),
                None => Err(format!("no gatekeeper session for `{entry_id}`")),
            }
        }

        fn gatekeeper_ready(&self, _entry_id: &str) -> Result<(), String> {
            match &self.gatekeeper_missing {
                Some(why) => Err(why.clone()),
                None => Ok(()),
            }
        }

        fn no_gatekeeper(&self, entry_id: &str, why: &str) -> Result<String, String> {
            self.told
                .lock()
                .unwrap()
                .push((entry_id.to_string(), why.to_string()));
            match &self.told_says {
                Some(e) => Err(e.clone()),
                None => Ok(format!("the session `s-root` was rung about `{entry_id}`")),
            }
        }
    }

    /// **The recording door, shared with the daemon and readable by the test** — the same fixture
    /// `mod restart`'s `Shared` is, with the fallback's two doors forwarded as well.
    ///
    /// The wrapper exists because the daemon owns its reviewer and the test has to read what it was
    /// asked; `Arc` because both need the same recording.
    pub(super) struct SharedDoor(pub(super) Arc<RecordingReviewer>);

    impl letibot_tools::gatekeeper::Reviewer for SharedDoor {
        fn wake(
            &self,
            entry_id: &str,
            req: &letibot_tools::gatekeeper::ReviewRequest,
        ) -> Result<String, String> {
            letibot_tools::gatekeeper::Reviewer::wake(&*self.0, entry_id, req)
        }

        fn gatekeeper_ready(&self, entry_id: &str) -> Result<(), String> {
            letibot_tools::gatekeeper::Reviewer::gatekeeper_ready(&*self.0, entry_id)
        }

        fn no_gatekeeper(&self, entry_id: &str, why: &str) -> Result<String, String> {
            letibot_tools::gatekeeper::Reviewer::no_gatekeeper(&*self.0, entry_id, why)
        }
    }

    /// A door that accepts every ask — the ordinary case, and what a test uses when it is not
    /// asserting the ask itself.
    fn quiet_reviewer() -> Box<dyn letibot_tools::gatekeeper::Reviewer + Send> {
        Box::new(RecordingReviewer {
            says: Some("asked".into()),
            ..Default::default()
        })
    }

    /// **A sink that records every event the queue emits** — the events half of a pass, in the
    /// queue's own tests. The production one broadcasts to every session's log; what these tests
    /// are about is *what* the queue says and *in what order* relative to the row.
    #[derive(Clone, Default)]
    pub(super) struct RecordingEvents(Arc<std::sync::Mutex<Vec<letibot_sessionlog::SessionEvent>>>);

    impl RecordingEvents {
        pub(super) fn sink(&self) -> Box<dyn Fn(letibot_sessionlog::SessionEvent) + Send> {
            let me = self.clone();
            Box::new(move |e| me.0.lock().unwrap().push(e))
        }

        pub(super) fn moves(&self) -> Vec<(String, letibot_sessionlog::event::MergeState, String)> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    letibot_sessionlog::SessionEvent::MergeEntryMoved {
                        id,
                        state,
                        evidence,
                    } => Some((id.clone(), *state, evidence.clone())),
                    _ => None,
                })
                .collect()
        }

        /// **The removals**, which are the queue's one event about an ABSENCE — see
        /// [`letibot_sessionlog::SessionEvent::MergeEntryRemoved`]. A head that folded the entry
        /// in has to be told it went, so this is as load-bearing as a move and is asserted the
        /// same way.
        pub(super) fn removals(&self) -> Vec<(String, String)> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    letibot_sessionlog::SessionEvent::MergeEntryRemoved { id, evidence } => {
                        Some((id.clone(), evidence.clone()))
                    }
                    _ => None,
                })
                .collect()
        }
    }

    /// A sink that drops everything, for the tests that are not about the events.
    pub(super) fn quiet_events() -> Box<dyn Fn(letibot_sessionlog::SessionEvent) + Send> {
        Box::new(|_| {})
    }

    /// **A repo with a `main` branch and a worktree for `branch`**, checked out at
    /// `root/worktrees/branch`. The repo is the shape the daemon serves: a main that the
    /// entry's branch will be fast-forwarded into, and a worktree where the branch is
    /// checked out and the rebase and the gate run.
    fn repo_with_branch(name: &str) -> (PathBuf, PathBuf, String, String) {
        let root = std::env::temp_dir().join(format!("letibot-mq-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "first"]);
        let main_sha = sha(&root, "main");

        // The branch, cut from main, with a commit of its own.
        git(&root, &["branch", "feature"]);
        // The worktree, where the branch is checked out.
        let wt = root.join("worktrees").join("feature");
        git(
            &root,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "feature"],
        );
        std::fs::write(wt.join("b.txt"), "three\n").expect("write");
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "feature work"]);
        let feature_sha = sha(&wt, "feature");
        (root, wt, main_sha, feature_sha)
    }

    /// **The rebase and the fast-forward, against a real repo** — the part that matters, and
    /// the reason the test drives git rather than a mock.
    ///
    /// The shape: a `main` with a commit, an `other` branch that lands first (moving main),
    /// and a `feature` branch cut from the original main with a commit of its own, checked out
    /// in a worktree. The daemon takes `other` and lands it, then takes `feature`, rebases it
    /// onto the *current* main (the tip of `other`, not the stale SHA it was cut from), runs
    /// the gate (a no-op here), fast-forwards main to `feature`, and cleans up.
    ///
    /// **The rebase is what makes the fast-forward a fast-forward, and it is not a no-op.**
    /// `feature` was cut from the original main, but `other` has landed since, so the current
    /// main is the tip of `other`. The rebase of `feature` onto the tip of `other` replays
    /// `feature`'s commit on top of `other`, and the fast-forward then moves main to the
    /// rebased `feature`. The `--ff-only` in [`fast_forward_main`] is the assertion that the
    /// rebase did its job: a branch that is not a descendant of main would fail the
    /// `--ff-only`, and the test would fail with git's own words rather than a silent merge
    /// commit.
    #[test]
    fn the_rebase_and_fast_forward_land_the_branch() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("land");
        let db = root.join("sessions.db");

        // An `other` branch, cut from main, that lands first and moves main.
        git(&root, &["branch", "other"]);
        let other_wt = root.join("worktrees").join("other");
        git(
            &root,
            &["worktree", "add", "-q", other_wt.to_str().unwrap(), "other"],
        );
        std::fs::write(other_wt.join("c.txt"), "other\n").expect("write");
        git(&other_wt, &["add", "-A"]);
        git(&other_wt, &["commit", "-qm", "other work"]);
        let other_sha = sha(&other_wt, "other");

        // The `other` entry, urgent so it is taken first.
        let other_entry = MergeEntry {
            id: "m-other".into(),
            session_id: "s".into(),
            branch: "other".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Urgent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: String::new(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(other_wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &other_entry);
        approve(&db, "m-other");

        // The `feature` entry, enqueued against the main it was cut from — which is now stale.
        let feature_entry = MergeEntry {
            id: "m-land".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: String::new(),
            evidence: String::new(),
            created_ms: 2_000,
            updated_ms: 2_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &feature_entry);
        approve(&db, "m-land");

        // The daemon, with a no-op gate: the gate is the seam, and the test is about the
        // rebase and the fast-forward, not the gate.
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        );

        // The first pass takes `other` (urgent) and lands it, moving main to the tip of
        // `other`.
        let outcome = daemon.step().expect("the pass");
        assert_eq!(
            outcome,
            StepOutcome::Landed(other_sha.clone()),
            "other lands first"
        );
        assert_eq!(
            sha(&root, "main"),
            other_sha,
            "main moved to the tip of other"
        );

        // The second pass takes `feature`, rebases it onto the current main (the tip of
        // `other`), and lands it.
        let outcome = daemon.step().expect("the pass");
        let feature_tip = match &outcome {
            StepOutcome::Landed(tip) => tip.clone(),
            other => panic!("the feature entry should land, got {other:?}"),
        };
        // **main moved to the rebased feature tip** — the fast-forward, and the whole point.
        assert_eq!(
            sha(&root, "main"),
            feature_tip,
            "main was fast-forwarded to the rebased feature tip"
        );
        // **The rebase was not a no-op**: the feature tip is not the original feature SHA,
        // because it was replayed on top of `other`.
        assert_ne!(
            feature_tip,
            sha(&root, "main~1"),
            "the feature was rebased, not fast-forwarded as-is"
        );

        // **The worktree is gone and the branch is deleted** — the cleanup `Landed` owns.
        assert!(!wt.exists(), "the worktree was removed");
        let branches = git_output(&root, &["branch", "--list", "feature"]);
        assert!(
            branches.trim().is_empty(),
            "the branch was deleted: {branches:?}"
        );

        // **The row says `Landed`, with the tip** — the durable record of the merge, read
        // through a connection the daemon never held.
        let store = store_at(&db);
        let landed = store
            .merge_entry("m-land")
            .expect("reads")
            .expect("the entry");
        assert_eq!(landed.state, MergeState::Landed, "the row is landed");
        assert_eq!(
            landed.landed_sha.as_deref(),
            Some(feature_tip.as_str()),
            "the row keeps the tip"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **An entry whose branch is already in main is `Landed` — without a rebase and without a
    /// gate.**
    ///
    /// The operator's ruling on the row that describes exactly this: *"and i love your 2"*. The
    /// shape is the two stale rows the live queue holds: a branch main ALREADY contains, landed by
    /// somebody else's hand, with an entry still parked over it. Rebasing it would replay commits
    /// main already has, and gating it would judge a diff that is already in. So the pass moves the
    /// row to `landed` with main's own tip and returns — and the gate is never run, which a gate
    /// that panics if it is called is the assertion of.
    #[test]
    fn a_branch_main_already_contains_is_landed_without_a_rebase_or_a_gate() {
        let (root, wt, main_sha, feature_sha) = repo_with_branch("already-merged");
        let db = root.join("sessions.db");
        // Somebody else's hand lands the branch: main fast-forwards to the feature's own tip.
        git(&root, &["merge", "--ff-only", "feature"]);
        let tip = sha(&root, "main");
        assert_eq!(tip, feature_sha, "main is at the feature's own tip");

        let entry = MergeEntry {
            id: "m-merged".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: String::new(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        // An accepting verdict, so the queue may TAKE it: this is about what the pass does with
        // an entry it is allowed to take, not about a row nobody reviewed.
        approve(&db, "m-merged");

        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| panic!("the gate must not run for a branch main already contains")),
            quiet_reviewer(),
            quiet_events(),
        );
        let outcome = daemon.step().expect("the pass");
        assert_eq!(
            outcome,
            StepOutcome::Landed(tip.clone()),
            "the entry is landed at main's own tip"
        );
        // **Main did not move**: there was nothing left to land.
        assert_eq!(sha(&root, "main"), tip, "main did not move");
        // **The row says `Landed`, with the tip and the reason on it.**
        let store = store_at(&db);
        let landed = store
            .merge_entry("m-merged")
            .expect("reads")
            .expect("the entry");
        assert_eq!(landed.state, MergeState::Landed, "the row is landed");
        assert_eq!(
            landed.landed_sha.as_deref(),
            Some(tip.as_str()),
            "the row keeps main's tip"
        );
        assert!(
            landed.evidence.contains("main already contains"),
            "the row says why: {:?}",
            landed.evidence
        );
        // **And the cleanup `Landed` owns ran** — `git branch -d` refuses an unmerged branch, so
        // the branch being gone is the same fact as the check being right.
        assert!(!wt.exists(), "the worktree was removed");
        assert!(!branch_exists(&root, "feature"), "the branch was deleted");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The two facts the pass decides on, over a real repo** — *ahead of its base* AND *in main*
    /// — and the three standings they make together.
    ///
    /// Neither fact alone is the answer, and this is the test that says so in git's own terms:
    /// the branch `nothing` IS an ancestor of main and is NOT already merged, because it carries
    /// nothing at all. That pair of facts is the whole bug — the ancestor check read alone says
    /// *landed* about a branch with no work on it.
    #[test]
    fn a_branch_is_judged_on_both_its_base_and_main() {
        let (root, _wt, main_sha, _feature_sha) = repo_with_branch("standing");
        // Ahead of its base, tip NOT in main: the ordinary path decides (take, rebase, gate).
        assert_eq!(
            branch_standing(&root, "feature", &main_sha),
            BranchStanding::CarriesWork
        );
        // Ahead of its base, tip IN main: somebody else's hand landed it, and the row is `Landed`
        // without a rebase and without a gate — the case the ancestor check was added for.
        git(&root, &["merge", "--ff-only", "feature"]);
        assert_eq!(
            branch_standing(&root, "feature", &main_sha),
            BranchStanding::OnMain
        );
        // Equal to its base: nothing was committed — and the ancestor check still says yes, which
        // is exactly the reading that binned two branches of work.
        let tip = sha(&root, "main");
        git(&root, &["branch", "nothing", "main"]);
        assert!(
            main_contains(&root, "nothing"),
            "a branch at its own base IS an ancestor of main — the trap"
        );
        assert_eq!(
            branch_standing(&root, "nothing", &tip),
            BranchStanding::NothingCommitted
        );
        // **And a base git cannot name is NOT *nothing was committed*.** The queue cannot tell,
        // and *cannot tell* must never be read as the one answer that bins work: the empty string
        // a row with no base carries, and a sha that does not resolve, both leave the ordinary
        // path to decide.
        assert_eq!(
            branch_standing(&root, "feature", ""),
            BranchStanding::CarriesWork,
            "no base recorded"
        );
        assert_eq!(
            branch_standing(&root, "feature", "0000000000000000000000000000000000000000"),
            BranchStanding::CarriesWork,
            "a base git cannot resolve"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A branch that is its own base is NEVER `Landed`** — the regression, and the one that cost
    /// two branches of real work.
    ///
    /// A child that never commits leaves its branch standing at its own base, and that base is an
    /// ancestor of `main` *by construction*: `git merge-base --is-ancestor <branch> main` answers
    /// yes about a branch with nothing on it at all. Read as *"already landed"*, that one true
    /// fact moved the row to `Landed` and ran the cleanup `Landed` owns — the worktree, which is
    /// where the uncommitted work was, was removed and the branch deleted. The work was never on
    /// main and the row said it was.
    ///
    /// So the pass asks the other question first: does the branch carry any commit over its base?
    /// `git rev-list --count <base>..<branch>` is 0 here, and 0 is *nothing was committed*, which
    /// is never `Landed`. Against the code that asked only whether main contains the branch, this
    /// test is red on every assertion below the pass: the row comes back `Landed`, the worktree is
    /// gone, and the uncommitted `b.txt` with it.
    #[test]
    fn a_branch_that_is_its_own_base_is_never_landed() {
        let root = std::env::temp_dir().join(format!("letibot-mq-nothing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "first"]);
        let main_sha = sha(&root, "main");
        // The branch, cut from main — with NOTHING committed on it. This is the shape of the two
        // lost entries: a child that did the work in the worktree and never committed.
        git(&root, &["branch", "feature"]);
        let wt = root.join("worktrees").join("feature");
        git(
            &root,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "feature"],
        );
        std::fs::write(wt.join("b.txt"), "three\n").expect("write");
        assert_eq!(
            sha(&root, "feature"),
            main_sha,
            "the branch is at its own base"
        );
        assert!(
            main_contains(&root, "feature"),
            "and the ancestor check says yes about it — that is the trap"
        );

        let db = root.join("sessions.db");
        let entry = MergeEntry {
            id: "m-nothing".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        // An accepting verdict, so the queue may TAKE it: the bug is in what the pass does with
        // an entry it is allowed to take, not in a row nobody reviewed.
        approve(&db, "m-nothing");

        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        );
        let outcome = daemon.step().expect("the pass");
        assert_ne!(
            outcome,
            StepOutcome::Landed(main_sha.clone()),
            "a branch with nothing committed on it is not landed"
        );

        let row = store_at(&db)
            .merge_entry("m-nothing")
            .expect("reads")
            .expect("the entry");
        assert_ne!(row.state, MergeState::Landed, "the row is not landed");
        assert_eq!(
            row.state,
            MergeState::Failed,
            "it is parked with the reason on it: {:?}",
            row.evidence
        );
        assert!(
            row.evidence.contains("nothing was committed"),
            "and the row says why: {:?}",
            row.evidence
        );

        // **The work survives** — the whole point. The worktree and the uncommitted file in it are
        // where they were, because `Failed` keeps the tree.
        assert!(wt.exists(), "the worktree is still there");
        assert!(wt.join("b.txt").exists(), "and the uncommitted work in it");
        assert!(branch_exists(&root, "feature"), "the branch is still there");
        assert_eq!(sha(&root, "main"), main_sha, "main did not move");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The rebase goes through the dirty `.commit-msg` every worktree carries** — the fix
    /// without which NOTHING lands.
    ///
    /// The landing discipline writes the commit message into the operator's TRACKED `.commit-msg`,
    /// so a worktree is dirty in it, and a plain `git rebase` refuses a dirty tree with `cannot
    /// rebase: You have unstaged changes`. That refusal arrives at the caller as an `Err` and is
    /// reported as a [`MergeState::Conflict`], so a branch with no conflict at all is parked on
    /// git's complaint about a file the landing itself wrote. The test makes the file real —
    /// tracked on the branch and then modified in the worktree, which is the shape the live queue
    /// is in — and asserts both halves: the plain form refuses, and the queue's own rebase does
    /// not, with the message file still there afterwards.
    #[test]
    fn the_rebase_autostashes_the_operators_commit_msg() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("autostash");
        // `.commit-msg` is the operator's TRACKED file, and the landing discipline leaves it
        // modified: committed on the branch, then written again without a commit.
        std::fs::write(wt.join(".commit-msg"), "the message the queue wrote\n").expect("write");
        git(&wt, &["add", ".commit-msg"]);
        git(&wt, &["commit", "-qm", "the tracked commit message file"]);
        std::fs::write(
            wt.join(".commit-msg"),
            "the message the landing just wrote\n",
        )
        .expect("write");
        // **And main moves on**, so the rebase has something to replay — a rebase onto the
        // branch's own base is a no-op git may take before it ever looks at the tree.
        std::fs::write(root.join("d.txt"), "main moves\n").expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "main moves"]);
        let tip = sha(&root, "main");
        assert_ne!(tip, main_sha, "main is past the branch's base");

        // **The defect, in git's own words**: the plain form refuses the tree the landing
        // discipline leaves behind, which is what parked every entry the queue took.
        let plain = Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(["rebase", "main"])
            .output()
            .expect("git");
        assert!(
            !plain.status.success(),
            "a plain rebase refuses a dirty tree — that is the defect this fixes"
        );
        let said = String::from_utf8_lossy(&plain.stderr).into_owned();
        assert!(said.contains("cannot rebase"), "{said}");

        // **And the queue's own rebase goes through**, with the message file restored rather than
        // dropped: `--autostash` is the whole of the difference.
        rebase(&wt, &tip).expect("the rebase goes through a dirty tracked file");
        assert_eq!(
            std::fs::read_to_string(wt.join(".commit-msg")).unwrap(),
            "the message the landing just wrote\n",
            "the stash was restored, not dropped"
        );
        assert!(
            git_output(&wt, &["status", "--porcelain"]).contains(".commit-msg"),
            "and the tree is still as the landing left it"
        );
        // **The branch was really replayed**: it is a descendant of the new main.
        git(&wt, &["merge-base", "--is-ancestor", "main", "HEAD"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A conflict is reported, never resolved** — the rebase fails, the entry is `Conflict`
    /// with git's words on the row, and the worktree stays. The queue's job is to notice, not
    /// to guess, and the worktree is where the conflict is, so it is not removed.
    ///
    /// The shape: `main` with a commit, an `other` branch that changes line 2 of `a.txt` and
    /// lands first (moving main), and a `feature` branch that changes the same line. When the
    /// daemon rebases `feature` onto the current main (the tip of `other`), the two changes to
    /// the same line conflict, and the rebase fails.
    #[test]
    fn a_conflict_is_reported_and_the_worktree_stays() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("conflict");
        let db = root.join("sessions.db");

        // The `feature` branch touches line 2 of a.txt, so it will conflict with `other`.
        std::fs::write(wt.join("a.txt"), "one\nTHREE\n").expect("write");
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "feature touches line two"]);

        // An `other` branch, cut from main, that changes the same line and lands first.
        git(&root, &["branch", "other"]);
        let other_wt = root.join("worktrees").join("other");
        git(
            &root,
            &["worktree", "add", "-q", other_wt.to_str().unwrap(), "other"],
        );
        std::fs::write(other_wt.join("a.txt"), "one\nTWO\n").expect("write");
        git(&other_wt, &["add", "-A"]);
        git(&other_wt, &["commit", "-qm", "other touches line two"]);
        // The SHA, taken before the daemon runs: landing deletes the branch, so `other` is not
        // a rev to ask for afterwards — and the assertion below is about where main is, not
        // about a ref that the cleanup owns.
        let other_sha = sha(&other_wt, "other");

        // The `other` entry, urgent so it is taken first.
        let other_entry = MergeEntry {
            id: "m-other".into(),
            session_id: "s".into(),
            branch: "other".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Urgent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: String::new(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(other_wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &other_entry);
        approve(&db, "m-other");

        // The `feature` entry, which will conflict when rebased onto the tip of `other`.
        let feature_entry = MergeEntry {
            id: "m-conflict".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: String::new(),
            evidence: String::new(),
            created_ms: 2_000,
            updated_ms: 2_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &feature_entry);
        approve(&db, "m-conflict");

        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        );

        // The first pass takes `other` (urgent) and lands it, moving main to the tip of
        // `other`.
        let outcome = daemon.step().expect("the pass");
        assert!(
            matches!(outcome, StepOutcome::Landed(_)),
            "other lands first: {outcome:?}"
        );

        // The second pass takes `feature`, rebases it onto the current main (the tip of
        // `other`), and the rebase conflicts.
        let outcome = daemon.step().expect("the pass");
        assert_eq!(outcome, StepOutcome::Conflict, "the conflict is reported");

        // **The worktree stays** — the evidence is there, and removing it would destroy it.
        assert!(wt.exists(), "the worktree stays after a conflict");
        // **The row says `Conflict`, with git's words** — the reason is on the row.
        let store = store_at(&db);
        let conflict = store
            .merge_entry("m-conflict")
            .expect("reads")
            .expect("the entry");
        assert_eq!(conflict.state, MergeState::Conflict, "the row is conflict");
        assert!(!conflict.evidence.is_empty(), "the row has the reason");
        // **main did not move past `other`** — the feature merge did not happen.
        assert_eq!(
            sha(&root, "main"),
            other_sha,
            "main is at the tip of other, not the feature"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The daemon's own two runs have names, and they name the entry they are about.**
    ///
    /// `j57` is a counter and a session id is a timestamp; the two pieces of work the daemon
    /// starts on its own account — the gate run for an entry and the review of one — were the
    /// only rows a reader could not tell apart, and *a gate run for `agent/foo`* and *a review of
    /// `agent/foo`* are two different jobs. The operator's word for the gap: *"and no slugs for
    /// jobs / sad"*.
    ///
    /// The three things this pins: the KIND is first and it is what tells the two apart; the
    /// branch is what the name is about; and the id is only the FALLBACK — an entry with no
    /// branch is still nameable, and a name is never empty, because an empty name is the one
    /// rendering this rule must not produce.
    #[test]
    fn a_daemon_job_is_named_by_its_kind_and_the_branch_it_is_about() {
        let mut entry = MergeEntry {
            id: "m-1234".into(),
            session_id: "s".into(),
            branch: "agent/a-row-can-wait-on-a-job".into(),
            base_sha: String::new(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: String::new(),
            evidence: String::new(),
            created_ms: 0,
            updated_ms: 0,
            worktree: None,
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        assert_eq!(gate_job_name(&entry), "gate-agent/a-row-can-wait-on-a-job");
        assert_eq!(
            review_job_name(&entry),
            "review-agent/a-row-can-wait-on-a-job"
        );
        assert_ne!(
            gate_job_name(&entry),
            review_job_name(&entry),
            "a gate run for a branch and a review of it are two different jobs"
        );

        // **No branch, and the id is the fallback** — a plain `task` child's work, or a row
        // written by a shape this build does not know, is still nameable.
        entry.branch = String::new();
        assert_eq!(gate_job_name(&entry), "gate-m-1234");
        // **And a name is never empty**, even for a row with nothing to be about: the kind alone
        // still says what sort of job this is, which is more than an id says.
        entry.id = String::new();
        assert_eq!(gate_job_name(&entry), "gate");
        assert_eq!(review_job_name(&entry), "review");

        // **Bounded and sanitised by the SAME code a tool's slug is** — one rule, not two. A
        // branch longer than a row is cut to the bound and keeps its head (the part that names
        // the work), and the words never carry a control character into a row.
        entry.id = "m-1234".into();
        entry.branch = format!("agent/{}", "x".repeat(200));
        let long = gate_job_name(&entry);
        assert_eq!(long.chars().count(), letibot_tools::exec::MAX_JOB_NAME);
        assert!(long.starts_with("gate-agent/xxx"), "{long}");
        entry.branch = "agent/two\nwords".into();
        assert_eq!(gate_job_name(&entry), "gate-agent/two-words");

        // **And an ordinary job nobody named still draws as its id rather than as an empty
        // name** — the rule the daemon's names must not disturb: the label's fallback IS the id,
        // for a name that is absent and for one that is blank, which the wire spells the same
        // way.
        assert_eq!(letibot_tools::exec::job_label(None, "j65"), "j65");
        assert_eq!(letibot_tools::exec::job_label(Some(""), "j65"), "j65");
        assert_eq!(
            letibot_tools::exec::job_label(Some("release-build"), "j65"),
            "release-build · j65"
        );
    }

    /// **The daemon's name for a review is the name the queue's board carries.**
    ///
    /// A review is a piece of work the daemon starts, and it is read in exactly one place: the
    /// queue session's board row ([`crate::sessions::gatekeeper::row`]), which is what the todos
    /// pane draws and what the idle check nags about. So the row's name IS
    /// [`review_job_name`] — one rule, one spelling, rather than a hand-written `review
    /// \`branch\`` here and a second name there.
    ///
    /// **And the child is deliberately NOT named by it**, which is worth asserting because it is
    /// the tempting place: a `task` child's name is the first line of its task, and a gatekeeper
    /// child is filtered out of the operator's panes by its role and by
    /// `GATEKEEPER_TITLE_PREFIX` (*"it shouldnt be visible in 'normal' agents count"*). A name
    /// line in front of the reviewer's prompt would be a name nothing draws AND would break that
    /// mark, so the prompt is left alone.
    #[test]
    fn a_review_is_named_by_the_entry_it_is_about() {
        let entry = MergeEntry {
            id: "m-1234".into(),
            session_id: "s".into(),
            branch: "agent/foo".into(),
            base_sha: "abc123".into(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the thing".into(),
            evidence: String::new(),
            created_ms: 0,
            updated_ms: 0,
            worktree: None,
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        let name = review_job_name(&entry);
        assert_eq!(name, "review-agent/foo");
        let row = crate::sessions::gatekeeper::row(&entry, None);
        assert!(
            row.content.starts_with(&name),
            "the board row is named by the daemon's name for the run: {:?}",
            row.content
        );
        assert!(
            row.content.contains("m-1234") && row.content.contains("do the thing"),
            "and it still carries the entry and the ask, which is what a verdict is about: {:?}",
            row.content
        );
        // The reviewer's prompt is untouched: its first line is still the mark the head reads.
        let req = letibot_tools::gatekeeper::ReviewRequest {
            brief: entry.brief.clone(),
            branch: entry.branch.clone(),
            base_sha: entry.base_sha.clone(),
        };
        assert!(
            letibot_tools::gatekeeper::review_prompt(&req)
                .starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX),
            "a gatekeeper child stays hidden by the mark its title is cut from"
        );
    }

    /// **The gate run's name is on the entry's row while the run is going.**
    ///
    /// A `Taken` row said *rebasing at the tip* for the whole of a `cargo test --workspace` —
    /// minutes of a run the row denied was happening, and a reader who could not tell a gate run
    /// from a rebase or from an entry sitting still. The name is [`gate_job_name`]'s, and this
    /// pins it where a reader meets it: the store is read from INSIDE the gate, so the row is
    /// asserted while the run is in flight rather than after the landing has replaced its words.
    #[test]
    fn the_gate_run_is_named_on_the_row_while_it_runs() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("gate-name");
        let db = root.join("sessions.db");
        let entry = MergeEntry {
            id: "m-named".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: String::new(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        approve(&db, "m-named");

        // What the row said while the gate was running, read by the gate itself — the same store
        // the daemon writes through, and a third connection like every other fixture's.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let read = seen.clone();
        let path = db.clone();
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(move |_| {
                let row = store_at(&path)
                    .merge_entry("m-named")
                    .expect("reads")
                    .expect("the entry");
                read.lock().expect("seen").push((row.state, row.evidence));
                GateRun::green()
            }),
            quiet_reviewer(),
            quiet_events(),
        );
        let outcome = daemon.step().expect("the pass");
        assert!(
            matches!(outcome, StepOutcome::Landed(_)),
            "a green gate lands the entry: {outcome:?}"
        );

        let seen = seen.lock().expect("seen");
        assert_eq!(seen.len(), 1, "the gate ran once");
        assert_eq!(
            seen[0].0,
            MergeState::Taken,
            "the row is taken while the gate runs"
        );
        assert_eq!(
            seen[0].1, "gate-feature: main's steps at the tip",
            "the row names the daemon's run, and the name is the branch it is about"
        );
        assert_eq!(gate_job_name(&entry), "gate-feature");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A gate failure is reported and the worktree stays** — the gate fails, the entry is
    /// `Failed` with the gate's words on the row, and the worktree stays. Removing a tree
    /// after a failed test destroys the evidence, and the worktree is where the failure is.
    #[test]
    fn a_gate_failure_keeps_the_worktree() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("failed");
        let db = root.join("sessions.db");

        let entry = MergeEntry {
            id: "m-failed".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: String::new(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        approve(&db, "m-failed");

        // The gate fails, with its own words.
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            // **A red run with the rows it leaves behind**: the failing step carries the gate's
            // own words and the one after it is marked not-run rather than passed — see
            // `a_failing_step_records_its_tail_and_the_rest_are_not_run` for the same shape
            // against a real `AGENTS.md`.
            Box::new(|_| GateRun {
                steps: vec![
                    MergeGateStep {
                        command: "cargo fmt --check".into(),
                        outcome: MergeGateOutcome::Failed,
                        output: "the gate is red".into(),
                        started_ms: 1_000,
                        elapsed_ms: 5,
                    },
                    MergeGateStep {
                        command: "cargo test".into(),
                        outcome: MergeGateOutcome::NotRun,
                        output: String::new(),
                        started_ms: 0,
                        elapsed_ms: 0,
                    },
                ],
                failure: Some("the gate is red".into()),
            }),
            quiet_reviewer(),
            quiet_events(),
        );
        let outcome = daemon.step().expect("the pass");
        assert_eq!(outcome, StepOutcome::Failed, "the failure is reported");

        // **The worktree stays** — the evidence is there.
        assert!(wt.exists(), "the worktree stays after a gate failure");
        // **The row says `Failed`, with the gate's words.**
        let store = store_at(&db);
        let failed = store
            .merge_entry("m-failed")
            .expect("reads")
            .expect("the entry");
        assert_eq!(failed.state, MergeState::Failed, "the row is failed");
        assert_eq!(
            failed.evidence, "the gate is red",
            "the row has the gate's words"
        );
        // **And the landing half's rows came with the move.** A parked entry whose row said
        // only `failed` is exactly what this change is about.
        assert_eq!(
            failed.gate_steps.len(),
            2,
            "one row per step the gate declared: {:?}",
            failed.gate_steps
        );
        assert_eq!(failed.gate_steps[0].outcome, MergeGateOutcome::Failed);
        assert_eq!(failed.gate_steps[1].outcome, MergeGateOutcome::NotRun);
        assert_eq!(failed.gate_steps[1].command, "cargo test");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A `Taken` entry that the daemon died on comes back `Stale`** — the recovery, driven
    /// through the store rather than the pure function, so the row on disk is what is
    /// asserted. A daemon that dies mid-merge writes a `Taken` row, and the next daemon's
    /// `recover` moves it to `Stale` rather than letting it come back as `Waiting`.
    #[test]
    fn a_dead_gate_job_comes_back_stale() {
        let (root, _wt, main_sha, _feature_sha) = repo_with_branch("stale");
        let db = root.join("sessions.db");

        // The entry, taken by a daemon that then died: the row is `Taken` on disk.
        let entry = MergeEntry {
            id: "m-stale".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Taken,
            brief: String::new(),
            evidence: "rebasing at the tip".into(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(_wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);

        // The next daemon recovers: the `Taken` row is moved to `Stale` on disk.
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        );
        let recovered = daemon.recover().expect("the recovery");
        assert_eq!(
            recovered[0].state,
            MergeState::Stale,
            "the taken entry is stale"
        );
        let store = store_at(&db);
        let on_disk = store
            .merge_entry("m-stale")
            .expect("reads")
            .expect("the entry");
        assert_eq!(on_disk.state, MergeState::Stale, "the row on disk is stale");
        assert!(!on_disk.evidence.is_empty(), "the row has the reason");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ===== The gate, the wire and the thread =====

    /// **An entry with no verdict does NOT land** — the operator's *"gated merge"* as one
    /// assertion, and the half of the gate that matters most: the door does not open by
    /// default. The daemon asks the reviewer, reports that it is waiting, and takes nothing.
    #[test]
    fn an_entry_with_no_verdict_does_not_land() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("no-verdict");
        let db = root.join("sessions.db");
        let entry = MergeEntry {
            id: "m-wait".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);

        let door = Arc::new(RecordingReviewer {
            says: Some("asked".into()),
            ..Default::default()
        });
        // A door this test can read back, since the daemon takes ownership of the boxed one.
        struct Shared(Arc<RecordingReviewer>);
        impl letibot_tools::gatekeeper::Reviewer for Shared {
            fn wake(
                &self,
                entry_id: &str,
                req: &letibot_tools::gatekeeper::ReviewRequest,
            ) -> Result<String, String> {
                self.0.wake(entry_id, req)
            }
        }
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            Box::new(Shared(door.clone())),
            quiet_events(),
        );
        let outcome = daemon.step().expect("the pass");
        assert_eq!(
            outcome,
            StepOutcome::AwaitingReview,
            "an entry with no verdict is waited on, not taken"
        );

        // **The reviewer was asked, and asked about the right branch.** An entry that waits for
        // a verdict nobody asked for waits for ever, so the ask is half of this claim.
        assert_eq!(
            *door.asked.lock().unwrap(),
            vec![("m-wait".to_string(), "feature".to_string())]
        );
        // **And nothing moved.** The entry is still `Waiting`, main is where it was, and the
        // branch and its worktree are untouched.
        let store = store_at(&db);
        let still = store
            .merge_entry("m-wait")
            .expect("reads")
            .expect("the entry");
        assert_eq!(still.state, MergeState::Waiting, "the row did not move");
        assert_eq!(sha(&root, "main"), main_sha, "main did not move");
        assert!(wt.exists(), "the worktree is untouched");
        assert!(
            branch_exists(&root, "feature"),
            "the branch is untouched — a landing that must not have happened would have \
             deleted it"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A rejecting verdict does not land, and it does not vanish either** — the entry is
    /// parked `Failed` with the verdict's own words on the row and the worktree stays, which is
    /// the shape a failed gate takes for the same reason: the tree is where the reason is.
    #[test]
    fn a_rejecting_verdict_does_not_land() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("rejected");
        let db = root.join("sessions.db");
        let entry = MergeEntry {
            id: "m-reject".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        // The verdict, written by the reviewer's own half — a reject with a reason and the
        // evidence it was based on.
        store_at(&db)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: "m-reject".into(),
                session_id: "gatekeeper".into(),
                branch: "feature".into(),
                base_sha: main_sha.clone(),
                asked_ms: 1,
                answered_ms: Some(2),
                decision: Some("reject".into()),
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec!["the brief asked for X and the change does Y".into()],
                files: vec!["crates/x.rs".into()],
                commands: vec!["git diff base...feature".into()],
            })
            .expect("the verdict row");

        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        );
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::Refused,
            "a rejected entry is parked, not landed"
        );

        let store = store_at(&db);
        let parked = store
            .merge_entry("m-reject")
            .expect("reads")
            .expect("the entry");
        assert_eq!(parked.state, MergeState::Failed, "the row is parked");
        assert!(
            parked.evidence.contains("reject"),
            "the row carries the verdict: {:?}",
            parked.evidence
        );
        assert!(
            parked
                .evidence
                .contains("the brief asked for X and the change does Y"),
            "the reviewer's reason is carried forward: {:?}",
            parked.evidence
        );
        assert!(
            parked.evidence.contains("crates/x.rs"),
            "so is the evidence: {:?}",
            parked.evidence
        );
        assert_eq!(sha(&root, "main"), main_sha, "main did not move");
        assert!(wt.exists(), "the worktree stays with the reason");
        assert!(branch_exists(&root, "feature"), "the branch stays");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **`needs_human` is a refusal too.** It is the third word of the closed set and it is not
    /// an accept: the reviewer saying *I cannot decide* is not permission, and a queue that
    /// landed on it would be landing on an abstention.
    #[test]
    fn an_abstaining_verdict_does_not_land() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("needs-human");
        let db = root.join("sessions.db");
        let entry = MergeEntry {
            id: "m-human".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        store_at(&db)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: "m-human".into(),
                session_id: "gatekeeper".into(),
                branch: "feature".into(),
                base_sha: main_sha.clone(),
                asked_ms: 1,
                answered_ms: Some(2),
                decision: Some("needs_human".into()),
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec!["the spec is ambiguous".into()],
                files: vec![],
                commands: vec![],
            })
            .expect("the verdict row");
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        );
        assert_eq!(daemon.step().expect("the pass"), StepOutcome::Refused);
        assert_eq!(sha(&root, "main"), main_sha, "main did not move");
        assert!(branch_exists(&root, "feature"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **An accepting verdict is the one way through**, and a verdict whose word nobody knows
    /// is not one — the closed set is exact, and a row written by a build this one does not
    /// know must not read as permission.
    #[test]
    fn only_an_accepting_verdict_is_a_way_through() {
        let e = entry("m-1", MergePriority::Subagent, MergeState::Waiting, 1_000);
        let rec = |decision: Option<&str>| letibot_tokencore::store::ReviewRecord {
            entry_id: "m-1".into(),
            session_id: "gatekeeper".into(),
            branch: "b-m-1".into(),
            base_sha: "base".into(),
            asked_ms: 1,
            answered_ms: decision.map(|_| 2),
            decision: decision.map(|d| d.to_string()),
            attempts: 0,
            failed_ms: None,
            failure: String::new(),
            reasons: vec![],
            files: vec![],
            commands: vec![],
        };
        // No row at all, and a row with no verdict: both wait, and neither is an accept.
        assert_eq!(review_gate(&e, None), ReviewGate::Awaiting);
        assert_eq!(review_gate(&e, Some(&rec(None))), ReviewGate::Awaiting);
        // An accept is the only one that gets through.
        assert_eq!(
            review_gate(&e, Some(&rec(Some("accept")))),
            ReviewGate::Accepted
        );
        // The other two words, and a word outside the set, are refusals.
        for word in ["reject", "needs_human", "probably", ""] {
            match review_gate(&e, Some(&rec(Some(word)))) {
                ReviewGate::Refused(why) => {
                    assert!(!why.is_empty(), "a refusal must carry its reason")
                }
                other => panic!("`{word}` must not be a way through: {other:?}"),
            }
        }
        // **An entry awaiting a verdict does not hold the queue up.** A second, accepted entry
        // is taken while the first waits, because the queue is serial about the GATE and not
        // about the review.
        let waiting = entry("m-wait", MergePriority::Urgent, MergeState::Waiting, 1_000);
        let accepted = entry("m-go", MergePriority::Subagent, MergeState::Waiting, 2_000);
        let entries = vec![waiting, accepted];
        let reviews = vec![rec(Some("accept"))];
        let mut accepted_rec = reviews[0].clone();
        accepted_rec.entry_id = "m-go".into();
        assert_eq!(
            next_takeable(&entries, &[accepted_rec]),
            Some(1),
            "the accepted entry is taken past the one that is still being reviewed"
        );
        assert_eq!(
            next_takeable(&entries, &[]),
            None,
            "with no verdicts at all, nothing is taken"
        );
    }

    /// **A door that cannot be reached is said on the row.** An entry waiting for a verdict
    /// nobody was asked for waits for ever, so a failed wake has to leave a reason a person can
    /// act on rather than the same silence an empty queue makes.
    #[test]
    fn a_wake_that_failed_is_on_the_row() {
        let (root, wt, _main_sha, _feature_sha) = repo_with_branch("no-door");
        let db = root.join("sessions.db");
        let entry = MergeEntry {
            id: "m-nodoor".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: "base".into(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        // The door refuses, which is what a daemon with no reviewer session does.
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            Box::new(RecordingReviewer::default()),
            quiet_events(),
        );
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::AwaitingReview
        );
        let row = store_at(&db)
            .merge_entry("m-nodoor")
            .expect("reads")
            .expect("the entry");
        assert_eq!(row.state, MergeState::Waiting, "it is still waiting");
        assert!(
            row.evidence.contains("could not be asked"),
            "the row says the ask failed: {:?}",
            row.evidence
        );
        assert!(
            row.evidence.contains("no gatekeeper session"),
            "and it carries the door's own words: {:?}",
            row.evidence
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **With no gatekeeper the entry does not sit in silence: the project that owns the branch is
    /// told, once, and the row says so** — the operator's fallback, in their words: *"even if
    /// gatekeeper session dies or didnt start - the queue tooling can send a message back to the
    /// main project session and projects will gate and merge on their own"*.
    ///
    /// Three things are asserted and they are the three the ask is: **the door was asked to tell
    /// the project** (once, with the queue's own reason), **the row carries the handover** — which
    /// is what the project's session reads, so a row without it would be a message nobody could
    /// find — and **the second pass does not tell again**, because the row already says it.
    ///
    /// **The retry is NOT stopped by any of this**, which is the other half: an ask that failed is
    /// the case where the queue tries again by itself, so the entry stays `Waiting` and the ask
    /// goes out on every pass. What the guard stops is the TELLING, not the asking.
    #[test]
    fn a_wake_that_failed_tells_the_project_once() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("fallback-told");
        let db = root.join("sessions.db");
        let entry = MergeEntry {
            id: "m-fb".into(),
            session_id: "s-child".into(),
            branch: "feature".into(),
            // **The base is the real one**, so the pass's own already-merged check can read the
            // branch: `git rev-list --count base..feature` with a base git cannot resolve answers
            // *carries work*, which would make the fixture assert nothing.
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        // The door refuses the ask, which is what a daemon with no reviewer session does.
        let door = Arc::new(RecordingReviewer::default());
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            Box::new(SharedDoor(door.clone())),
            quiet_events(),
        );
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::AwaitingReview,
            "the queue asked, and it is waiting for a verdict"
        );
        {
            let told = door.told.lock().unwrap();
            assert_eq!(told.len(), 1, "the project is told: {told:?}");
            assert_eq!(told[0].0, "m-fb", "about the entry that is stuck");
            assert!(
                told[0].1.contains("could not be asked"),
                "and with the queue's own reason: {}",
                told[0].1
            );
        }
        let row = store_at(&db).merge_entry("m-fb").unwrap().unwrap();
        assert_eq!(row.state, MergeState::Waiting, "the queue is still trying");
        assert!(
            handed_over(&row),
            "the row carries the handover the project's session reads: {:?}",
            row.evidence
        );
        assert!(
            row.evidence.contains("feature") && row.evidence.contains("m-fb"),
            "and it names the branch and the entry: {:?}",
            row.evidence
        );
        assert!(
            row.evidence.contains(&wt.display().to_string()),
            "and the worktree the work is in: {:?}",
            row.evidence
        );

        // The second pass: the ask goes out again and the project is NOT told again.
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::AwaitingReview
        );
        assert_eq!(
            door.told.lock().unwrap().len(),
            1,
            "an entry already past that point is not re-told"
        );
        assert_eq!(
            door.asked.lock().unwrap().len(),
            2,
            "but the ask itself is retried — this is the case the queue recovers by itself"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **An entry with no gatekeeper at all — never started — is handed over instead of being
    /// asked about for ever.** The door is the only half that can see this
    /// ([`letibot_tools::gatekeeper::Reviewer::gatekeeper_ready`]), and what it is asked for is the
    /// answer to *is there anybody to ask*, not a verdict: the queue does not ask, and the row is
    /// `failed` because nothing will judge the branch until a person restarts the review or the
    /// project lands it.
    #[test]
    fn no_gatekeeper_at_all_hands_the_entry_over() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("fallback-none");
        let db = root.join("sessions.db");
        let entry = MergeEntry {
            id: "m-none".into(),
            session_id: "s-child".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        let why = "the review queue's session `s-gatekeeper` is in neither this daemon nor the \
                   store, and the review of this entry has been outstanding for 400s without it";
        let door = Arc::new(RecordingReviewer {
            gatekeeper_missing: Some(why.into()),
            ..Default::default()
        });
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            Box::new(SharedDoor(door.clone())),
            quiet_events(),
        );
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::NoGatekeeper,
            "nothing was asked — there was nobody to ask"
        );
        assert!(
            door.asked.lock().unwrap().is_empty(),
            "and that is the point: no ask goes out to a session that does not exist"
        );
        let told = door.told.lock().unwrap();
        assert_eq!(told.len(), 1, "the project is told: {told:?}");
        assert_eq!(told[0].0, "m-none");
        assert!(told[0].1.contains("neither this daemon"), "{}", told[0].1);
        drop(told);
        let row = store_at(&db).merge_entry("m-none").unwrap().unwrap();
        assert_eq!(
            row.state,
            MergeState::Failed,
            "the queue has stopped asking, and `failed` is the state the person's verbs move"
        );
        assert!(handed_over(&row), "{:?}", row.evidence);
        assert!(
            row.evidence.contains("/queue restart") && row.evidence.contains("/queue approve"),
            "and the person's verbs are on the row as the last resort: {:?}",
            row.evidence
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A branch the project gated and landed ITSELF is recorded as landed** — the half that makes
    /// the message true, and the operator's *"projects will gate and merge on their own"* read as
    /// the queue's own act.
    ///
    /// The fixture is the whole of it: main is fast-forwarded to the branch by hand (which is what
    /// the project's session would do with git and a shell), the entry carries the handover, and
    /// the queue's next pass finds main contains the branch. No verdict is involved, none is asked
    /// for, and the cleanup every landing owns runs — the worktree is removed and the branch
    /// deleted, because the work is on main.
    #[test]
    fn a_branch_the_project_landed_itself_is_recorded_as_landed() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("project-lands");
        // **The project's own merge**, by hand: this is the act the message asks for.
        git(&root, &["merge", "--ff-only", "feature"]);
        let tip = sha(&root, "main");
        let db = root.join("sessions.db");
        let mut entry = MergeEntry {
            id: "m-proj".into(),
            session_id: "s-child".into(),
            branch: "feature".into(),
            // The base the branch was cut from, which is what the pass's own count reads.
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        let said = no_gatekeeper_evidence(
            &entry,
            "the gatekeeper could not be asked: no gatekeeper session for `m-proj`",
            None,
        );
        entry.evidence = said;
        enqueue(&db, &entry);
        let door = Arc::new(RecordingReviewer::default());
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            Box::new(SharedDoor(door.clone())),
            quiet_events(),
        );
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::Landed(tip.clone()),
            "the queue reads main and records the landing the project did"
        );
        assert!(
            door.told.lock().unwrap().is_empty(),
            "and it tells nobody again: the entry is answered"
        );
        let row = store_at(&db).merge_entry("m-proj").unwrap().unwrap();
        assert_eq!(row.state, MergeState::Landed);
        assert_eq!(row.landed_sha.as_deref(), Some(tip.as_str()));
        assert!(
            row.evidence.contains("landed without the queue's gate"),
            "{:?}",
            row.evidence
        );
        assert!(
            !handed_over(&row),
            "the handover is off the row: it has an answer now: {:?}",
            row.evidence
        );
        assert!(
            !wt.exists(),
            "the cleanup every landing owns ran — the worktree is gone"
        );
        assert!(!branch_exists(&root, "feature"), "and so is the branch");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **Only the entries a session is the PROJECT of are its notices** — the filter behind
    /// `Harness::handover_notices`, which is the end of the fallback that reaches a model.
    ///
    /// Three entries and three answers: one handed over from a child of this session (its own), one
    /// handed over from another tree (not its business), and one waiting for a verdict (not handed
    /// over at all). The root is found by walking the stored parents — `s-child` is the session
    /// that finished the branch and `s-root` is what a person attaches to — so the fixture carries
    /// the walk rather than assuming it.
    #[test]
    fn only_this_sessions_handed_over_entries_are_its_own() {
        let dir = std::env::temp_dir().join(format!("letibot-mq-handed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.db");
        let store = store_at(&path);
        let session = |id: &str, parent: Option<&str>| {
            store
                .put_session(&letibot_tokencore::store::SessionRecord {
                    id: id.into(),
                    title: None,
                    model_id: "m".into(),
                    dialect_sha: "sha".into(),
                    workspace_root: "/w".into(),
                    owner: "dead".into(),
                    role: None,
                    approvers: vec![],
                    parent_session_id: parent.map(str::to_string),
                })
                .unwrap();
        };
        session("s-root", None);
        session("s-child", Some("s-root"));
        session("s-other-root", None);
        let mut mine = entry("m-mine", MergePriority::Subagent, MergeState::Waiting, 1);
        mine.session_id = "s-child".into();
        mine.evidence = no_gatekeeper_evidence(&mine, "the ask failed", None);
        let mut theirs = entry("m-theirs", MergePriority::Subagent, MergeState::Waiting, 2);
        theirs.session_id = "s-other-root".into();
        theirs.evidence = no_gatekeeper_evidence(&theirs, "the ask failed", None);
        let waiting = entry("m-waiting", MergePriority::Subagent, MergeState::Waiting, 3);
        for e in [&mine, &theirs, &waiting] {
            store.put_merge_entry(e).unwrap();
        }

        let mine_ids: Vec<String> = handed_over_to(&store, "s-root")
            .iter()
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(
            mine_ids,
            vec!["m-mine".to_string()],
            "the handed-over entry of THIS session's tree, and nothing else"
        );
        assert!(
            handed_over_to(&store, "s-other-root")
                .iter()
                .any(|e| e.id == "m-theirs"),
            "and another session gets its own"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The message the project's session is told names the entry, the branch and the worktree**,
    /// and both doors out of it — the project's own gate and merge, and the operator's verdict.
    ///
    /// The one assertion that matters most is the first: a message about a branch nobody can name
    /// is not a message a model can act on, and the entry id is what every verb takes.
    #[test]
    fn the_message_to_the_project_names_the_entry_and_the_branch() {
        let mut e = entry("m-1", MergePriority::Subagent, MergeState::Failed, 1_000);
        e.worktree = Some("/w/worktrees/feature".into());
        let said = no_gatekeeper_evidence(
            &e,
            "http 429: Weekly/Monthly Limit Exhausted — the gatekeeper's attempt failed 3 times \
             and the queue has stopped asking",
            None,
        );
        e.evidence = said;
        let message = no_gatekeeper_message(&e);
        assert!(
            message.contains("`b-m-1`") && message.contains("(entry `m-1`)"),
            "the branch and the entry are in the first line: {message}"
        );
        assert!(
            message.contains("/w/worktrees/feature"),
            "and the worktree the work is in: {message}"
        );
        assert!(
            message.contains("Merge gate") && message.contains("merge_gate"),
            "the gate is named, and so is the tool that lists it: {message}"
        );
        assert!(
            message.contains("merge `b-m-1` into main and push"),
            "and the project's own merge is spelled out: {message}"
        );
        assert!(
            message.contains("/queue approve m-1"),
            "and the operator's door, for a project that will not touch main: {message}"
        );
        assert!(
            message.contains("http 429"),
            "and the reason the queue is telling it, in the queue's own words: {message}"
        );
        assert!(
            !message.contains(NO_GATEKEEPER),
            "but not the handover clause itself, which is what the queue did with the entry and \
             not why: {message}"
        );
        // The queue's own reason, read back off the row: everything before the clause.
        assert_eq!(
            handover_reason(&e),
            "http 429: Weekly/Monthly Limit Exhausted — the gatekeeper's attempt failed 3 times \
             and the queue has stopped asking"
        );
    }

    /// **Every move the queue makes is announced, after the row is written.** The events
    /// decision's second half: a head that is attached when an entry moves sees the move rather
    /// than only the next snapshot.
    ///
    /// The ORDER is asserted as well as the content, and it is the load-bearing half: an event
    /// folded before the row was on disk would let a head re-read the queue and find the state
    /// it just heard was over — the one way the snapshot and the events can disagree.
    ///
    /// **Three moves for a landing, and the middle one is the gate run's own.** A take says
    /// *rebasing at the tip*, then — because the rebase and the gate are minutes apart and the row
    /// said the first while the second ran — the run names itself (`gate-feature: main's steps at
    /// the tip`), and the landing ends it. A move nobody announced would be a row that changed
    /// under a head that was watching.
    #[test]
    fn every_move_is_announced_after_the_row() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("events");
        let db = root.join("sessions.db");
        let entry = MergeEntry {
            id: "m-events".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        approve(&db, "m-events");

        let events = RecordingEvents::default();
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            events.sink(),
        );
        assert!(matches!(
            daemon.step().expect("the pass"),
            StepOutcome::Landed(_)
        ));

        let moves = events.moves();
        assert_eq!(
            moves
                .iter()
                .map(|(id, state, _)| (id.as_str(), *state))
                .collect::<Vec<_>>(),
            vec![
                ("m-events", letibot_sessionlog::event::MergeState::Taken),
                ("m-events", letibot_sessionlog::event::MergeState::Taken),
                ("m-events", letibot_sessionlog::event::MergeState::Landed),
            ],
            "the take, the gate run's own name and the landing are announced, in that order"
        );
        // **The middle move is the run's name**, which is the whole of why it is a move at all:
        // a reader watching the queue sees which run is going while it goes.
        assert_eq!(
            moves[1].2, "gate-feature: main's steps at the tip",
            "the gate run names itself on the row while it runs"
        );
        // **The reason travels with the move.** A state without its reason is a row the pane
        // draws and the operator cannot read — and the event carries the same words the row
        // does, so the two readers see one account.
        let on_disk = store_at(&db)
            .merge_entry("m-events")
            .expect("reads")
            .expect("the entry");
        assert_eq!(
            moves.last().unwrap().2,
            on_disk.evidence,
            "the event's evidence is the row's"
        );
        assert!(
            on_disk.evidence.contains("landed at"),
            "{:?}",
            on_disk.evidence
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A refusal is announced too** — a move to `Failed` by a verdict is a move, and a pane
    /// that showed the entry `waiting` while its reviewer had refused it would be the lie the
    /// `Stale` state exists to prevent, one state over.
    #[test]
    fn a_parked_entry_is_announced() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("events-refused");
        let db = root.join("sessions.db");
        let entry = MergeEntry {
            id: "m-ref".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };
        enqueue(&db, &entry);
        store_at(&db)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: "m-ref".into(),
                session_id: "gatekeeper".into(),
                branch: "feature".into(),
                base_sha: main_sha.clone(),
                asked_ms: 1,
                answered_ms: Some(2),
                decision: Some("reject".into()),
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec!["it does not do what the brief asked".into()],
                files: vec![],
                commands: vec![],
            })
            .expect("the verdict row");
        let events = RecordingEvents::default();
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            events.sink(),
        );
        assert_eq!(daemon.step().expect("the pass"), StepOutcome::Refused);
        let moves = events.moves();
        assert_eq!(moves.len(), 1, "{moves:?}");
        assert_eq!(moves[0].1, letibot_sessionlog::event::MergeState::Failed);
        assert!(
            moves[0].2.contains("reject"),
            "the verdict travels with the move: {:?}",
            moves[0].2
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Whether `branch` exists in `root`.
    fn branch_exists(root: &Path, branch: &str) -> bool {
        !git_output(root, &["branch", "--list", branch])
            .trim()
            .is_empty()
    }

    /// **The cleanup the state owns is APPLIED, not merely asserted** — driven through the
    /// function the pass itself calls, so a `Failed` entry's tree is kept by the rule rather
    /// than by a `return` that happened to skip a removal.
    ///
    /// The `git branch -d` at the end is the interesting half: it refuses a branch that is not
    /// merged, so the fast-forward has to have happened for the branch to go. That refusal is
    /// the guarantee the cleanup leans on rather than a check it makes, and this test is where
    /// a reader can see it.
    #[test]
    fn the_cleanup_the_state_owns_is_applied() {
        let (root, wt, _main_sha, _feature_sha) = repo_with_branch("cleanup");
        let entry = MergeEntry {
            id: "m-clean".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: "base".into(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            brief: String::new(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
            gate_steps: Vec::new(),
        };

        // **Every state that keeps its tree, driven through the rule.** These are the states
        // the operator's ask is about: removing a tree after a failure destroys the evidence.
        for state in [
            MergeState::Waiting,
            MergeState::Taken,
            MergeState::Failed,
            MergeState::Conflict,
            MergeState::Stale,
        ] {
            clean_up(&root, &entry, state);
            assert!(wt.exists(), "{state:?} removed the worktree");
            assert!(
                branch_exists(&root, "feature"),
                "{state:?} deleted the branch"
            );
        }

        // **And `Landed` removes both.** The fast-forward first, because the branch deletion
        // is `-d`: an unmerged branch is refused, which is what makes the deletion safe.
        git(&root, &["merge", "--ff-only", "feature"]);
        clean_up(&root, &entry, MergeState::Landed);
        assert!(!wt.exists(), "a landed entry's worktree is removed");
        assert!(
            !branch_exists(&root, "feature"),
            "a landed entry's branch is deleted"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The repo is the one git names** — resolved from a path inside it, which is how the
    /// daemon finds the `main` it will fast-forward.
    ///
    /// A subdirectory is the case that matters: a daemon's workspace is a directory somebody
    /// started it in, and the repo is a fact git answers rather than one the queue derives.
    #[test]
    fn the_repo_is_the_one_git_names() {
        let (root, _wt, _main_sha, _feature_sha) = repo_with_branch("root");
        let canonical = |p: &Path| std::fs::canonicalize(p).expect("canonical");
        let named = repo_root(&root).expect("the repo is named");
        assert_eq!(canonical(&named), canonical(&root), "git named {named:?}");
        // From inside it — a workspace that is a subdirectory of the checkout, which is what
        // `--workspace` usually is.
        let sub = root.join("worktrees");
        let from_sub = repo_root(&sub).expect("a subdirectory is in the repo");
        assert_eq!(
            canonical(&from_sub),
            canonical(&root),
            "the subdirectory named {from_sub:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The gate is `main`'s `AGENTS.md`, the step that failed is the answer, and a branch
    /// cannot change its own gate.** Against a real repository: no section on `main` is no gate
    /// (and the queue holds the entry rather than failing it); a section is its steps, run in
    /// order through `sh -c`, the first red one ending the run; and a branch that rewrote the
    /// section to pass is still judged by `main`'s.
    ///
    /// **And every step the gate declared leaves a ROW**, which is the half this test grew: the
    /// step that ran green, the step that was red with the tail of its output, and the step the
    /// run never reached — marked [`MergeGateOutcome::NotRun`] rather than passed, because a list
    /// that stopped at the red step cannot be told from a shorter gate that was all green.
    #[test]
    fn the_gate_is_mains_agents_md_and_the_first_red_step_ends_it() {
        let dir = std::env::temp_dir().join(format!("letibot-mq-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .current_dir(&dir)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .expect("git")
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("README.md"), "x\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        // No AGENTS.md on main: no gate, and the queue's check says so in words.
        assert_eq!(gate_on_main(&dir).unwrap(), None);
        assert!(gate_configured(&dir).unwrap_err().contains("Merge gate"));

        std::fs::write(
            dir.join("AGENTS.md"),
            "# Project\n\n## Merge gate\n\n```sh\ntrue\nsh -c 'echo red-step >&2; exit 3'\n\
             touch ran-after\n```\n",
        )
        .unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "gate"]);
        assert!(gate_configured(&dir).is_ok());
        let run = repo_gate(&dir);
        let err = run.failure.expect("the second step is red");
        assert!(
            err.contains("exit 3") && err.contains("red-step"),
            "{err:?}"
        );
        // **The three rows, in the order `main` declared them.** The first ran and was green,
        // the second ran and was red with the failing command's own output as its tail, and the
        // third — the step after the red one — is `not_run`, which is not `passed`.
        assert_eq!(
            run.steps
                .iter()
                .map(|s| (s.command.as_str(), s.outcome))
                .collect::<Vec<_>>(),
            vec![
                ("true", MergeGateOutcome::Passed),
                (
                    "sh -c 'echo red-step >&2; exit 3'",
                    MergeGateOutcome::Failed
                ),
                ("touch ran-after", MergeGateOutcome::NotRun),
            ],
            "the gate's rows: {:?}",
            run.steps
        );
        assert!(
            run.steps[1].output.contains("red-step"),
            "the failing step's own words are its output tail: {:?}",
            run.steps[1].output
        );
        assert!(
            run.steps[0].started_ms > 0 && run.steps[0].elapsed_ms < 60_000,
            "a step that ran is stamped with when and how long: {:?}",
            run.steps[0]
        );
        assert_eq!(
            (run.steps[2].started_ms, run.steps[2].elapsed_ms),
            (0, 0),
            "a step that never ran has no timing to give"
        );
        assert!(
            !dir.join("ran-after").exists(),
            "a step after the red one ran"
        );

        // A branch that rewrites its own gate to pass is judged by main's.
        git(&["checkout", "-q", "-b", "agent/sneaky"]);
        std::fs::write(dir.join("AGENTS.md"), "## Merge gate\n\n```sh\ntrue\n```\n").unwrap();
        git(&["commit", "-qam", "loosen the gate"]);
        assert!(
            repo_gate(&dir).failure.is_some(),
            "the branch's own gate was used"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A repository with no gate records that as its own fact** — one row saying so, and NOT an
    /// empty list. `[]` is the entry's word for *the gate has not run on this entry*, and a blank
    /// checklist read as *every step passed* is the one reading a repository nobody has
    /// configured must not be able to produce.
    #[test]
    fn a_repository_with_no_gate_records_that_as_its_own_row() {
        let dir =
            std::env::temp_dir().join(format!("letibot-mq-nogate-row-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "t@t"]);
        git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("README.md"), "x\n").expect("write");
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "init"]);
        assert_eq!(
            gate_on_main(&dir).expect("reads"),
            None,
            "no section on main"
        );

        let run = repo_gate(&dir);
        assert_eq!(
            run.steps.len(),
            1,
            "one row, not an empty list: {:?}",
            run.steps
        );
        assert_eq!(run.steps[0].outcome, MergeGateOutcome::NoGate);
        assert!(
            run.steps[0].command.is_empty(),
            "there is no command to name: {:?}",
            run.steps[0].command
        );
        assert!(
            run.steps[0].output.contains("Merge gate"),
            "the row carries the queue's own sentence: {:?}",
            run.steps[0].output
        );
        assert!(
            run.failure.is_some(),
            "and it is not a green run: there was nothing to pass"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A landing whose gate has three steps records three rows, in order** — through the whole
    /// pass, on a real repository whose `AGENTS.md` declares them, with the rows read back out of
    /// the store the landing was written to.
    ///
    /// This is the change in one test: the entry reaches `landed_sha` carrying what its gate did,
    /// so a pane can draw the landing rather than only its verdict.
    #[test]
    fn a_landing_records_one_row_per_step_in_order() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("gate-rows");
        // `main` declares the gate, and the branch is cut from it — so the rebase replays the
        // branch on top of the section and the gate the queue reads is the one it was held to.
        std::fs::write(
            root.join("AGENTS.md"),
            "# Project\n\n## Merge gate\n\n```sh\necho first\necho second\necho third\n```\n",
        )
        .expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "the gate"]);

        let db = root.join("sessions.db");
        let mut e = entry(
            "m-steps",
            MergePriority::Subagent,
            MergeState::Waiting,
            1_000,
        );
        e.branch = "feature".into();
        e.base_sha = main_sha;
        e.worktree = Some(wt.to_str().unwrap().to_string());
        enqueue(&db, &e);
        approve(&db, "m-steps");

        // The PRODUCTION gate, not a stub: the rows under test are the ones `repo_gate` builds
        // out of `main`'s own `AGENTS.md`.
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(repo_gate),
            quiet_reviewer(),
            quiet_events(),
        )
        .with_gate_check(Box::new(gate_configured));
        assert!(
            matches!(daemon.step().expect("the pass"), StepOutcome::Landed(_)),
            "the gate is three green steps"
        );

        let landed = store_at(&db)
            .merge_entry("m-steps")
            .expect("reads")
            .expect("the entry");
        assert_eq!(landed.state, MergeState::Landed);
        assert!(landed.landed_sha.is_some(), "the entry landed with its tip");
        assert_eq!(
            landed
                .gate_steps
                .iter()
                .map(|s| (s.command.as_str(), s.outcome))
                .collect::<Vec<_>>(),
            vec![
                ("echo first", MergeGateOutcome::Passed),
                ("echo second", MergeGateOutcome::Passed),
                ("echo third", MergeGateOutcome::Passed),
            ],
            "one row per step, in the order main declared them: {:?}",
            landed.gate_steps
        );
        assert!(
            landed.gate_steps[0].output.contains("first")
                && landed.gate_steps[2].output.contains("third"),
            "each row keeps the tail of its OWN step's output: {:?}",
            landed.gate_steps
        );
        // **The rows are on disk, not in the daemon**: a second connection reads them.
        assert_eq!(
            store_at(&db).merge_entry("m-steps").unwrap().unwrap(),
            landed
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The gate's build is pointed inside the worktree it is judging** — see
    /// [`GATE_TARGET_DIR`] for the measurement this answers (`agent/queue-reset` was parked
    /// `failed` on an `E0599` for a constant its own worktree defines, because the build came
    /// out of the main tree's shared `target/`).
    ///
    /// Pinned without cargo: the gate's step writes the variable it was handed, and the test
    /// reads what it wrote. The fixture carries `task_start`'s redirect — a `.cargo/config.toml`
    /// whose `[build] target-dir` is a directory that is not this worktree — so what is asserted
    /// is the value the gate sets *while that redirect is in the tree*, which is the state every
    /// entry is in.
    #[test]
    fn the_gates_build_is_pointed_inside_the_worktree_it_judges() {
        let dir =
            std::env::temp_dir().join(format!("letibot-mq-gate-target-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".cargo")).expect("mkdir");
        let git_with_identity = |args: &[&str]| {
            let mut all = vec![
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ];
            all.extend_from_slice(args);
            git(&dir, &all);
        };
        git_with_identity(&["init", "-q", "-b", "main"]);
        // **The redirect, in `task_start`'s own words**, pointing somewhere that is not here.
        std::fs::write(
            dir.join(".cargo/config.toml"),
            "# Written by `task_start`: the build cache is shared with the main tree, so this \
             worktree does not pay a cold build of the whole graph.\n[build]\ntarget-dir = \
             \"/elsewhere/target\"\n",
        )
        .expect("write");
        std::fs::write(
            dir.join("AGENTS.md"),
            "# Project\n\n## Merge gate\n\n```sh\nprintf '%s' \"$CARGO_TARGET_DIR\" > \
             gate-target-dir\n```\n",
        )
        .expect("write");
        git_with_identity(&["add", "-A"]);
        git_with_identity(&["commit", "-qm", "gate"]);

        repo_gate(&dir)
            .failure
            .is_none()
            .then_some(())
            .expect("the gate is green");
        let seen = std::fs::read_to_string(dir.join("gate-target-dir")).expect("the step wrote it");
        assert_eq!(
            Path::new(seen.trim()),
            gate_target_dir(&dir).as_path(),
            "the gate's build was not pointed inside the worktree it judged"
        );
        assert_ne!(
            seen.trim(),
            "/elsewhere/target",
            "the tree's own redirect won: the gate is building the main tree again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A daemon above its repositories lands each entry in its worktree's repository.** The
    /// daemon's own directory is in no repository at all — a session at `~/Projects` — and the
    /// entry still rebases, gates and fast-forwards the `main` of the repository its worktree
    /// belongs to.
    #[test]
    fn an_entry_lands_in_its_worktrees_repository_when_the_daemon_serves_above_it() {
        let (root, wt, main_sha, feature_sha) = repo_with_branch("above");
        let above =
            std::env::temp_dir().join(format!("letibot-mq-above-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&above);
        std::fs::create_dir_all(&above).expect("mkdir");
        assert!(
            repo_root(&above).is_err(),
            "the daemon's directory is in no repository"
        );
        let db = above.join("s.db");
        let mut e = entry("up", MergePriority::Subagent, MergeState::Waiting, 1_000);
        e.branch = "feature".into();
        e.base_sha = main_sha;
        e.worktree = Some(wt.to_str().unwrap().to_string());
        enqueue(&db, &e);
        approve(&db, "up");
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            above.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        );
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::Landed(feature_sha.clone())
        );
        assert_eq!(
            sha(&root, "main"),
            feature_sha,
            "the worktree's repository's main moved"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&above);
    }

    /// **Main moved by hand, and the entry still lands** — the rebase is onto the tip of main as
    /// it is now, not onto the last SHA the queue itself landed. A commit straight to main (a
    /// person's, or a gate section) used to leave the branch rebased onto a stale base and the
    /// fast-forward refused.
    #[test]
    fn an_entry_lands_after_main_moved_outside_the_queue() {
        let (root, wt, main_sha, _) = repo_with_branch("moved");
        std::fs::write(root.join("hand.txt"), "by hand\n").unwrap();
        git(&root, &["add", "hand.txt"]);
        git(&root, &["commit", "-qm", "a commit straight to main"]);
        let db = root.join("s.db");
        let mut e = entry("mv", MergePriority::Subagent, MergeState::Waiting, 1_000);
        e.branch = "feature".into();
        e.base_sha = main_sha;
        e.worktree = Some(wt.to_str().unwrap().to_string());
        enqueue(&db, &e);
        approve(&db, "mv");
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        );
        let out = daemon.step().expect("the pass");
        assert!(matches!(out, StepOutcome::Landed(_)), "{out:?}");
        assert!(
            root.join("hand.txt").exists() && root.join("b.txt").exists(),
            "both are on main"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A daemon serves only the entries queued from sessions opened on its workspace.** One
    /// store holds every daemon's queue; an entry whose root session belongs to another
    /// workspace is that daemon's, and this one neither asks about it nor touches its row.
    #[test]
    fn a_daemon_leaves_another_workspaces_entries_alone() {
        let dir = std::env::temp_dir().join(format!("letibot-mq-serves-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let db = dir.join("s.db");
        let store = store_at(&db);
        for (id, ws) in [("s-a", &a), ("s-b", &b)] {
            store
                .put_session(&letibot_tokencore::store::SessionRecord {
                    id: id.into(),
                    title: None,
                    model_id: "m".into(),
                    dialect_sha: "sha".into(),
                    workspace_root: ws.display().to_string(),
                    owner: "dead".into(),
                    role: None,
                    approvers: vec![],
                    parent_session_id: None,
                })
                .unwrap();
        }
        let mut ea = entry("ea", MergePriority::Subagent, MergeState::Waiting, 1_000);
        ea.session_id = "s-a".into();
        let mut eb = entry("eb", MergePriority::Subagent, MergeState::Waiting, 900);
        eb.session_id = "s-b".into();
        enqueue(&db, &ea);
        enqueue(&db, &eb);
        approve(&db, "ea");
        approve(&db, "eb");
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            a.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        )
        .serving(&a);
        daemon.step().expect("the pass");
        let row = |id: &str| store_at(&db).merge_entry(id).unwrap().unwrap();
        assert!(
            row("ea").evidence.contains("no worktree"),
            "its own entry was acted on: {:?}",
            row("ea")
        );
        assert_eq!(
            row("eb").evidence,
            "",
            "another workspace's entry was touched"
        );
        assert_eq!(row("eb").state, MergeState::Waiting);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The project's agent is told once that its repository has no gate** — on the pass the
    /// entry starts waiting, not on every pass after.
    #[test]
    fn the_agent_is_told_once_that_a_gate_is_missing() {
        #[derive(Default)]
        struct Counting(Arc<std::sync::atomic::AtomicUsize>);
        impl letibot_tools::gatekeeper::Reviewer for Counting {
            fn wake(
                &self,
                _: &str,
                _: &letibot_tools::gatekeeper::ReviewRequest,
            ) -> Result<String, String> {
                Ok("asked".into())
            }
            fn gate_missing(&self, _: &str) -> Result<(), String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        }
        let dir = std::env::temp_dir().join(format!("letibot-mq-told-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("s.db");
        enqueue(
            &db,
            &entry("t1", MergePriority::Subagent, MergeState::Waiting, 1_000),
        );
        approve(&db, "t1");
        let told = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            dir.clone(),
            Box::new(|_| GateRun::green()),
            Box::new(Counting(told.clone())),
            quiet_events(),
        )
        .with_gate_check(Box::new(|_| Err(format!("{WAITING_FOR_GATE}: none"))));
        for _ in 0..3 {
            assert_eq!(daemon.step().expect("the pass"), StepOutcome::AwaitingGate);
        }
        assert_eq!(
            told.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "told more than once"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Two daemons cannot take one entry**: the claim is one conditional update, and only the
    /// first sees it succeed.
    #[test]
    fn an_entry_is_claimed_once() {
        let dir = std::env::temp_dir().join(format!("letibot-mq-claim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("s.db");
        enqueue(
            &db,
            &entry("c1", MergePriority::Subagent, MergeState::Waiting, 1_000),
        );
        let (one, two) = (store_at(&db), store_at(&db));
        assert!(one.claim_merge_entry("c1").unwrap());
        assert!(!two.claim_merge_entry("c1").unwrap(), "claimed twice");
        assert_eq!(
            two.merge_entry("c1").unwrap().unwrap().state,
            MergeState::Taken
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **No gate, no take — and no failure.** A reviewed entry in a repository whose `main`
    /// has no gate is held `Waiting` with the reason on its row, written once; the pass says
    /// it is waiting on the gate, and the entry is taken the moment the gate exists.
    #[test]
    fn a_repository_with_no_gate_holds_its_entries_with_the_reason() {
        let dir = std::env::temp_dir().join(format!("letibot-mq-nogate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let db = dir.join("s.db");
        let mut e = entry("g1", MergePriority::Subagent, MergeState::Waiting, 1_000);
        e.brief = "do it".into();
        enqueue(&db, &e);
        approve(&db, "g1");
        let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r2 = ready.clone();
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            dir.clone(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        )
        .with_gate_check(Box::new(move |_| {
            if r2.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(())
            } else {
                Err("waiting for a merge gate".into())
            }
        }));
        assert_eq!(daemon.step().expect("the pass"), StepOutcome::AwaitingGate);
        let row = store_at(&db).merge_entry("g1").unwrap().unwrap();
        assert_eq!(row.state, MergeState::Waiting, "held, not failed");
        assert_eq!(row.evidence, "waiting for a merge gate");
        // **And the absence is a ROW on the entry, not a blank checklist.** The queue never ran
        // a step, and the row says so in the landing half's own vocabulary rather than leaving
        // an empty list that a pane could draw as *every step passed*.
        assert_eq!(
            row.gate_steps.iter().map(|s| s.outcome).collect::<Vec<_>>(),
            vec![MergeGateOutcome::NoGate],
            "the repository with no gate: {:?}",
            row.gate_steps
        );
        assert!(
            row.gate_steps[0]
                .output
                .contains("waiting for a merge gate"),
            "the row carries the queue's own sentence: {:?}",
            row.gate_steps[0].output
        );
        // The gate lands on main: the next pass takes it (and, with no worktree here, says so).
        ready.store(true, std::sync::atomic::Ordering::SeqCst);
        daemon.step().expect("the pass");
        let row = store_at(&db).merge_entry("g1").unwrap().unwrap();
        assert_ne!(
            row.evidence, "waiting for a merge gate",
            "it was taken: {row:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **letibot's own gate is CI's four steps, plus the one that installs them** — read out of
    /// this repository's `agents.md` the way the queue reads it, so an edit that drops a step from
    /// the section is caught here.
    ///
    /// The first four are `.github/workflows/ci.yml`'s, character for character: a queue that lands
    /// on a weaker standard than CI lands branches CI would refuse. The fifth is the queue's own —
    /// the install, which a runner has no use for — and it is asserted rather than left to prose,
    /// because *the pipeline ends with what it proved in place* is the whole of why it is there
    /// (see [`GATE_TARGET_DIR`] for the two paths it joins).
    #[test]
    fn letibots_own_gate_is_cis_steps_and_the_install() {
        let steps = letibot_tools::gatekeeper::parse_merge_gate(include_str!("../../../agents.md"))
            .expect("agents.md has a Merge gate section");
        let ci = [
            "sh scripts/check-fmt.sh main",
            "cargo clippy --all-targets",
            "cargo test --workspace --no-fail-fast -- --nocapture",
            "cargo build --release --bins",
        ];
        assert_eq!(
            steps.len(),
            ci.len() + 1,
            "the gate is CI's steps and the install: {steps:?}"
        );
        assert_eq!(&steps[..ci.len()], &ci, "CI's steps, in CI's order");
        assert_eq!(
            steps.last().map(String::as_str),
            Some("sh scripts/install-gate-build.sh"),
            "the gate does not end by installing what it built: {steps:?}"
        );
    }

    /// **The gate's last step installs into the directory the launcher starts from** — the two
    /// paths pinned together, in the four files that have to agree, so neither can move alone.
    ///
    /// The step is ONE line because the gate is one command per line
    /// ([`letibot_tools::gatekeeper::parse_merge_gate`]), so the destination is named in the script
    /// the step runs — and an assertion that read only the step would be satisfied by a script that
    /// installed nowhere. So this reads the script, the launcher and `install.sh` too, and each
    /// assertion is a fact one of them states:
    ///
    ///   * the script names the main tree's `target/release`, and reads [`GATE_TARGET_DIR`] — the
    ///     variable the BUILD step was handed, which is what makes the two steps one pipeline
    ///     rather than two commands that happen to sit next to each other;
    ///   * `scripts/letibot` resolves `BIN` through `../target/release` and execs `harnessd` and
    ///     `letibot-tui` from it, which is the claim the script's destination rests on — a change
    ///     to the launcher's resolution fails HERE rather than on the operator's next restart;
    ///   * `install.sh`'s `BINARIES` names both of those, which is where the script's list of
    ///     names comes from.
    ///
    /// **What it cannot see is a BUILD**, and that is named rather than papered over: nothing here
    /// runs cargo, so the link between the artifacts `cargo build --release --bins` leaves in
    /// `$CARGO_TARGET_DIR/release` and the files this step copies is asserted as *both read one
    /// variable* rather than watched. [`the_gates_install_step_copies_what_was_built`] runs the step
    /// itself, against a fixture's stand-in binaries, which is the half a test can reach.
    #[test]
    fn the_gates_install_step_lands_where_the_launcher_looks() {
        let script = include_str!("../../../scripts/install-gate-build.sh");
        assert!(
            script.contains("target/release"),
            "the install step does not name the main tree's target/release"
        );
        assert!(
            script.contains(GATE_TARGET_DIR),
            "the install step must read {GATE_TARGET_DIR} — the variable the build step was \
             handed — or it looks somewhere cargo did not write"
        );
        assert!(
            script.contains("install.sh") && script.contains("BINARIES"),
            "the names installed must come from install.sh's own list rather than a copy"
        );

        let launcher = include_str!("../../../scripts/letibot");
        assert!(
            launcher.contains("../target/release"),
            "the launcher no longer resolves BIN through ../target/release, which is the path \
             this step installs into"
        );
        for name in ["harnessd", "letibot-tui"] {
            assert!(
                launcher.contains(name),
                "the launcher does not exec {name}, so the install cannot be about it"
            );
        }

        let installer = include_str!("../../../install.sh");
        let names = installer
            .lines()
            .find_map(|l| l.strip_prefix("BINARIES=\""))
            .and_then(|rest| rest.strip_suffix('"'))
            .expect("install.sh has a BINARIES= line");
        for want in ["harnessd", "letibot-tui"] {
            assert!(
                names.split_whitespace().any(|n| n == want),
                "install.sh's BINARIES ({names}) does not name {want}"
            );
        }
    }

    /// **The install step, RUN** — against a repository of the shape the queue gates: a main tree
    /// with a linked worktree, a private `CARGO_TARGET_DIR` inside the worktree holding what a build
    /// would have left, and the step's own working directory.
    ///
    /// What is asserted is the mechanism the static test cannot see: the MAIN tree is found from
    /// the worktree (through the common git directory), every name lands there with the same bytes,
    /// the temporary name the rename went through is gone, and the run says which path it proved
    /// and which one it installed into.
    ///
    /// **The binaries are stand-ins, and that is the limit of this test.** Nothing here builds
    /// anything: the fixture writes four one-line scripts where `cargo build --release --bins`
    /// would have left real binaries. What is therefore untested is the link between the build
    /// step's output and this step's input — that the files cargo leaves in
    /// `$CARGO_TARGET_DIR/release` are named `harnessd`, `letibot-tui`, `letibot-askpass` and
    /// `letibot` — which is cargo's business and not this script's. The list, the copy, the rename
    /// and the destination are the real ones, and a real build is what the gate itself runs.
    #[test]
    fn the_gates_install_step_copies_what_was_built() {
        use std::os::unix::fs::PermissionsExt as _;

        let (root, wt, _main_sha, _feature_sha) = repo_with_branch("gate-install");
        let names = ["harnessd", "letibot-tui", "letibot-askpass", "letibot"];
        // `install.sh`'s own line, in the tree that was built — where the step reads it.
        std::fs::write(
            wt.join("install.sh"),
            format!("BINARIES=\"{}\"\n", names.join(" ")),
        )
        .expect("write install.sh");
        let built = wt.join("target/release");
        std::fs::create_dir_all(&built).expect("mkdir");
        for name in names {
            let p = built.join(name);
            std::fs::write(&p, format!("#!/bin/sh\necho \"{name} 0.1.1\"\n")).expect("write");
            // Executable, because the step refuses a name a build did not produce — and because
            // the step runs the two the launcher execs from where they landed.
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }

        let step =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install-gate-build.sh");
        let out = Command::new("sh")
            .arg(&step)
            .current_dir(&wt)
            .env(GATE_TARGET_DIR, wt.join("target"))
            .output()
            .expect("run the install step");
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "the step failed: {said}");

        let installed = root.join("target/release");
        for name in names {
            assert_eq!(
                std::fs::read(installed.join(name)).expect("the name landed in the main tree"),
                std::fs::read(built.join(name)).expect("the built name"),
                "{name} did not land in the main tree's target/release"
            );
        }
        // **It says what it proved and where it went**, which is what the gate's row carries.
        assert!(
            said.contains(&built.display().to_string()),
            "the step did not name what it proved: {said}"
        );
        assert!(
            said.contains(&installed.display().to_string()),
            "the step did not name where it installed: {said}"
        );
        // **And nothing is left beside it.** The temporary name is what makes the replacement
        // atomic; one left behind would be a file nothing reads and nobody knows about.
        let leftovers: Vec<String> = std::fs::read_dir(&installed)
            .expect("the destination")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".new."))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A truncation says it truncated** — the same rule the queue's own read is written to,
    /// applied to the evidence a failing gate puts on the row.
    #[test]
    fn a_truncated_evidence_says_so_and_keeps_the_end() {
        let short = "the gate is red";
        assert_eq!(
            tail(short, EVIDENCE_BYTES),
            short,
            "a short evidence is untouched"
        );
        let long = format!("{}THE END", "x".repeat(10_000));
        let cut = tail(&long, 100);
        assert!(cut.contains("byte(s) dropped from the front"), "{cut:?}");
        assert!(
            cut.ends_with("THE END"),
            "the end is the part worth keeping: {cut:?}"
        );
    }

    /// **Every state and every rung survives the hop to the wire** — the whole closed set, not
    /// the one state a test happened to need. The `match`es in [`wire_entry`] are what make a
    /// rung added on either side a compile error rather than a word nobody can order; this is
    /// the half a compiler cannot check, which is that the values come across unchanged.
    #[test]
    fn every_state_and_rung_survives_the_hop_to_the_wire() {
        use letibot_sessionlog::event as wire;
        for (state, want) in [
            (MergeState::Waiting, wire::MergeState::Waiting),
            (MergeState::Taken, wire::MergeState::Taken),
            (MergeState::Landed, wire::MergeState::Landed),
            (MergeState::Failed, wire::MergeState::Failed),
            (MergeState::Conflict, wire::MergeState::Conflict),
            (MergeState::Stale, wire::MergeState::Stale),
        ] {
            let mut e = entry("m-1", MergePriority::Subagent, state, 1_000);
            e.needs = vec!["m-0".into()];
            e.evidence = "the reason".into();
            e.worktree = Some("/wt".into());
            e.landed_sha = Some("tip".into());
            e.gate_steps = vec![MergeGateStep {
                command: "make test".into(),
                outcome: MergeGateOutcome::Failed,
                output: "2 failed".into(),
                started_ms: 7,
                elapsed_ms: 8,
            }];
            let w = wire_entry(&e);
            assert_eq!(w.state, want, "{state:?} did not survive the hop");
            assert_eq!(w.id, e.id);
            assert_eq!(w.session_id, e.session_id);
            assert_eq!(w.branch, e.branch);
            assert_eq!(w.base_sha, e.base_sha);
            assert_eq!(w.needs, e.needs);
            assert_eq!(w.evidence, e.evidence);
            assert_eq!(w.created_ms, e.created_ms);
            assert_eq!(w.updated_ms, e.updated_ms);
            assert_eq!(w.worktree, e.worktree);
            assert_eq!(w.landed_sha, e.landed_sha);
            // **The landing's rows cross too**, word for word: the outcome is a closed set the
            // head draws, and `wire_gate_step` is where a variant added on either side fails to
            // compile.
            assert_eq!(w.gate_steps, vec![wire_gate_step(&e.gate_steps[0])]);
            assert_eq!(
                w.gate_steps[0].outcome,
                letibot_sessionlog::event::MergeGateOutcome::Failed
            );
        }
        for (p, want) in [
            (MergePriority::Urgent, wire::MergePriority::Urgent),
            (MergePriority::Subagent, wire::MergePriority::Subagent),
        ] {
            let e = entry("m-1", p, MergeState::Waiting, 1_000);
            assert_eq!(
                wire_entry(&e).priority,
                want,
                "{p:?} did not survive the hop"
            );
        }
    }

    /// **The snapshot is the whole queue** — an entry the queue cannot act on is in it, with
    /// its reason. A shorter list would say *"that is all the work there is"*, which is the
    /// one thing the queue's read must not say.
    #[test]
    fn the_snapshot_carries_every_state_and_its_reason() {
        use letibot_sessionlog::event as wire;
        let mut failed = entry("f", MergePriority::Subagent, MergeState::Failed, 1_000);
        failed.evidence = "the gate is red".into();
        let waiting = entry("w", MergePriority::Urgent, MergeState::Waiting, 2_000);
        let stale = entry("s", MergePriority::Subagent, MergeState::Stale, 3_000);
        let queue = wire_queue(&[failed, waiting, stale]);
        assert_eq!(queue.len(), 3, "the snapshot dropped a row: {queue:?}");
        assert_eq!(queue[0].state, wire::MergeState::Failed);
        assert_eq!(queue[0].evidence, "the gate is red", "the reason travels");
        assert_eq!(queue[1].priority, wire::MergePriority::Urgent);
        assert_eq!(queue[2].state, wire::MergeState::Stale);
    }

    /// **A daemon thread serves the queue** — the thread starts, takes a pass over the queue
    /// on its own thread, and stops when it is told to.
    ///
    /// The pass is observable because the entry it takes has no worktree: the queue cannot
    /// rebase a branch that is not checked out, so it puts the entry back `Waiting` with that
    /// reason on the row, and the row is read back through a connection the thread never held.
    /// That is the whole of the operator's *"merge queue is 2 - together with persistence and
    /// harnessd thread that serves it"*: a thread, a row, and the row saying what happened.
    #[test]
    fn the_thread_serves_the_queue_and_stops() {
        let dir = std::env::temp_dir().join(format!("letibot-mq-thread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let db = dir.join("sessions.db");
        enqueue(
            &db,
            &entry(
                "m-thread",
                MergePriority::Subagent,
                MergeState::Waiting,
                1_000,
            ),
        );
        approve(&db, "m-thread");

        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            std::env::temp_dir(),
            Box::new(|_| GateRun::green()),
            quiet_reviewer(),
            quiet_events(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let handle = daemon.spawn(stop.clone()).expect("the thread starts");
        // The first pass is immediate; the flag is what ends the loop, at the next check.
        std::thread::sleep(std::time::Duration::from_millis(250));
        stop.store(true, Ordering::Relaxed);
        handle.join().expect("the thread stops when it is told to");

        let on_disk = store_at(&db)
            .merge_entry("m-thread")
            .expect("reads")
            .expect("the entry");
        assert_eq!(
            on_disk.state,
            MergeState::Waiting,
            "the entry is still waiting"
        );
        assert!(
            on_disk.evidence.contains("no worktree"),
            "the thread's own pass left its reason on the row: {:?}",
            on_disk.evidence
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The output of a git command that is allowed to fail, for the assertions that read it.
    fn git_output(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[cfg(test)]
mod gatekeeper_door {
    //! **The queue's door to a gatekeeper** — the operator, 2026-10-09: *"looks like there is no
    //! gatekeeper agent that does reviews … or - subagent"*, with entries ten hours old waiting on a
    //! reviewer nobody could start. The door's half is the row naming the session that REVIEWS the
    //! branch — the review queue's own (`crate::sessions::gatekeeper::SESSION`) — and the ring at
    //! it; that session's half is `Harness::serve_reviews`, which starts the reviewer as its own
    //! `task` child, reached by the ring when the session is open and by
    //! `Sessions::serve_reviews`' own clock when it is not.
    use super::*;
    use letibot_sessionlog::registry::{
        Registry as SessionRegistry, SessionWiring, Work, WorkOrIdle,
    };
    use letibot_tokencore::store::{MergePriority, SessionRecord};
    use letibot_tools::gatekeeper::{ReviewRequest, Reviewer};

    fn session(store: &Store, id: &str, parent: Option<&str>) {
        store
            .put_session(&SessionRecord {
                id: id.into(),
                title: Some(format!("title of {id}")),
                model_id: "m".into(),
                dialect_sha: "sha".into(),
                workspace_root: "/w".into(),
                owner: "dead".into(),
                role: None,
                approvers: vec![],
                parent_session_id: parent.map(str::to_string),
            })
            .unwrap();
    }

    fn wakes(registry: &SessionRegistry) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            match registry.next_work_until(Some(std::time::Instant::now())) {
                WorkOrIdle::Work(Work::Woken(id)) => out.push(format!("woken {id}")),
                WorkOrIdle::Work(Work::Open(id)) => out.push(format!("open {id}")),
                WorkOrIdle::Work(_) => {}
                WorkOrIdle::Idle | WorkOrIdle::Closed => return out,
            }
        }
    }

    /// **A review is asked for on the row, and the session it names is rung** — the queue's own
    /// session, whose board holds the entry.
    ///
    /// **Not the tree the branch came from, and the fixture carries both on purpose**: a root and
    /// the child under it that finished the branch, neither of which reviews anything now. The
    /// column is *the session that reviewed it* (`ReviewRecord::session_id`), and the reviewer is a
    /// `task` child of the queue's session — so that is the id on the row, and that is the session
    /// the ring reaches.
    #[test]
    fn a_review_is_hosted_by_the_queue_session_and_rung() {
        use crate::sessions::gatekeeper::{SESSION, TITLE};
        let dir = std::env::temp_dir().join(format!("letibot-gk-door-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.db");
        let store = Store::open(&path).unwrap();
        // A root, and the child of it that finished the branch.
        session(&store, "s-root", None);
        session(&store, "s-child", Some("s-root"));
        store
            .put_merge_entry(&MergeEntry {
                id: "e1".into(),
                session_id: "s-child".into(),
                branch: "agent/x".into(),
                base_sha: "abc".into(),
                priority: MergePriority::Subagent,
                needs: vec![],
                state: MergeState::Waiting,
                brief: "make it work".into(),
                evidence: String::new(),
                created_ms: 1,
                updated_ms: 1,
                worktree: None,
                landed_sha: None,
                gate_steps: Vec::new(),
            })
            .unwrap();
        let registry = SessionRegistry::new();
        // **The queue's session, open in this daemon** — which is the whole of what the ring needs,
        // and the reason no session has to be resumed for a review any more.
        registry
            .create(SESSION, TITLE, SessionWiring::default())
            .expect("the queue's session registers");
        // The `open` the registry queued for that creation, drained: what is asserted below is the
        // RING, and a work item left on the bell would be read as one.
        let _ = wakes(&registry);
        let door = GatekeeperDoor::new(
            Store::open(&path).unwrap(),
            registry.bell().clone(),
            registry.clone(),
        );
        let req = ReviewRequest {
            brief: "make it work".into(),
            branch: "agent/x".into(),
            base_sha: "abc".into(),
        };

        let said = door.wake("e1", &req).expect("asked");
        assert!(
            said.contains(SESSION),
            "the sentence names the session that reviews it: {said}"
        );
        assert!(
            !said.contains("s-root"),
            "and NOT the tree the branch came from — that session is not asked to review anything \
             any more: {said}"
        );
        assert_eq!(wakes(&registry), vec![format!("woken {SESSION}")]);
        let row = store.merge_review("e1").unwrap().expect("a request row");
        assert_eq!(
            row.session_id, SESSION,
            "hosted by the session that RUNS the review, not by the tree root"
        );
        assert!(row.answered_ms.is_none(), "outstanding");

        // Asked again on the next pass: the same row, the first `asked_ms`, and only a ring.
        let first = row.asked_ms;
        door.wake("e1", &req).expect("asked again");
        assert_eq!(store.merge_review("e1").unwrap().unwrap().asked_ms, first);
        assert_eq!(wakes(&registry), vec![format!("woken {SESSION}")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A review asked for before the queue's session is open is on the ROW and nothing is rung**
    /// — the durable half alone, which is what a daemon that starts between an ask and a ring, or a
    /// second daemon over the same store, comes back to.
    ///
    /// This used to be a refusal (*"the session `s-…` that `e2` came from is in neither this daemon
    /// nor the store, so there is no session to run its gatekeeper under"*). That refusal was about
    /// the tree the branch came from, and the reviewer does not run there any more: the queue's own
    /// session is the daemon's, it is opened by the daemon's own idle arm, and the ask that arrives
    /// first is served by that clock (`Sessions::ensure_gatekeeper` → `Sessions::serve_reviews`).
    #[test]
    fn a_review_is_asked_for_before_the_queue_session_is_open() {
        use crate::sessions::gatekeeper::SESSION;
        let dir = std::env::temp_dir().join(format!("letibot-gk-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.db");
        let store = Store::open(&path).unwrap();
        let mut e = super::tests_entry_for_door("e2", "s-vanished");
        e.brief = "x".into();
        store.put_merge_entry(&e).unwrap();
        // **No session in the registry at all**: the daemon has not reached its first idle pass.
        let registry = SessionRegistry::new();
        let door = GatekeeperDoor::new(
            Store::open(&path).unwrap(),
            registry.bell().clone(),
            registry.clone(),
        );
        let said = door
            .wake(
                "e2",
                &ReviewRequest {
                    brief: "x".into(),
                    branch: e.branch.clone(),
                    base_sha: e.base_sha.clone(),
                },
            )
            .expect("the row is what carries the ask");
        assert!(said.contains(SESSION), "{said}");
        assert!(
            said.contains("not opened yet") && said.contains("next pass"),
            "the sentence says nothing was rung and what serves it instead: {said}"
        );
        assert!(
            wakes(&registry).is_empty(),
            "nothing to ring: no session of this daemon is open"
        );
        let row = store.merge_review("e2").unwrap().expect("a request row");
        assert_eq!(
            row.session_id, SESSION,
            "on the row, which is the durable half"
        );
        assert!(row.answered_ms.is_none(), "and still waiting for a verdict");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A registry with the store behind it** — the two halves `GatekeeperDoor::root_for` reads,
    /// wired the way the daemon wires them (`Sessions::open` is the same `Arc<dyn SessionSource>`
    /// the daemon sets).
    ///
    /// Needed by the fallback's own tests and by nothing else: `wake` never resumes a session, so
    /// the tests above need no source at all, and `root_for` does.
    fn wired(dir: &std::path::Path, path: &std::path::Path) -> Arc<SessionRegistry> {
        let mut cfg = crate::config::Config::for_this_box(dir);
        cfg.store = Some(path.to_path_buf());
        let registry = SessionRegistry::new();
        registry.set_source(
            crate::sessions::StoreSessions::open(&cfg).expect("a source over the store"),
        );
        registry
    }

    /// **The project that owns the branch is rung when no gatekeeper can judge it** — the
    /// fallback's door, and the session it reaches is the ROOT of the tree the branch came from.
    ///
    /// The fixture carries both sessions on purpose: the queue's own (`s-gatekeeper`), which is
    /// open in this daemon and is what a REVIEW rings, and the root (`s-root`), which is in the
    /// store and NOT in the registry — so the assertion is that the ring goes to the second, that
    /// the second is RESUMED rather than refused, and that the first hears nothing. That last one
    /// is the difference between this door and `wake`, and it is the whole of the fallback: the
    /// queue's session hosts reviews, and a review is exactly what nobody can run here.
    #[test]
    fn the_project_that_owns_the_branch_is_rung() {
        use crate::sessions::gatekeeper::{SESSION, TITLE};
        let dir = std::env::temp_dir().join(format!("letibot-gk-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.db");
        let store = Store::open(&path).unwrap();
        // A root, and the child of it that finished the branch.
        session(&store, "s-root", None);
        session(&store, "s-child", Some("s-root"));
        store
            .put_merge_entry(&super::tests_entry_for_door("e-fb", "s-child"))
            .unwrap();
        let registry = wired(&dir, &path);
        // **The queue's own session is open**, which is the state a review is asked in — and it is
        // the session that must NOT be rung about this.
        registry
            .create(SESSION, TITLE, SessionWiring::default())
            .expect("the queue's session registers");
        let _ = wakes(&registry);
        let door = GatekeeperDoor::new(
            Store::open(&path).unwrap(),
            registry.bell().clone(),
            registry.clone(),
        );

        let said = door
            .no_gatekeeper("e-fb", "the gatekeeper's attempt failed 3 times")
            .expect("the project is told");
        assert!(
            said.contains("s-root"),
            "the sentence names the session that was rung: {said}"
        );
        assert!(
            !said.contains(SESSION),
            "and it is NOT the queue's own session, which hosts reviews and cannot run one \
             here: {said}"
        );
        assert!(
            said.contains("agent/e-fb"),
            "and the branch, so the message can be about something: {said}"
        );
        assert!(
            said.contains("failed 3 times"),
            "and the reason the queue is telling it: {said}"
        );
        assert_eq!(
            wakes(&registry),
            vec!["open s-root".to_string(), "woken s-root".to_string()],
            "the root is RESUMED from the store (it was in neither the registry nor a harness) \
             and then rung — and nothing was rung at the queue's session"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A project that is gone too is refused by name, with the branch in the sentence** — the
    /// answer to *what happens when that session is gone as well*, and it is an `Err` rather than a
    /// silence: the daemon writes it onto the entry's row, so a person reading the pane sees that
    /// the fallback itself fell through and the person's verbs are what is left.
    #[test]
    fn a_project_that_is_gone_too_is_refused_by_name() {
        let dir = std::env::temp_dir().join(format!("letibot-gk-orphan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.db");
        let store = Store::open(&path).unwrap();
        // **The entry's session is in NEITHER the registry nor the store**: the tree it came from
        // has been forgotten, so there is no root to walk to and nobody to ring.
        store
            .put_merge_entry(&super::tests_entry_for_door("e-orphan", "s-vanished"))
            .unwrap();
        let registry = wired(&dir, &path);
        let door = GatekeeperDoor::new(
            Store::open(&path).unwrap(),
            registry.bell().clone(),
            registry.clone(),
        );
        let err = door
            .no_gatekeeper("e-orphan", "the gatekeeper could not be asked")
            .expect_err("there is no session to tell");
        assert!(
            err.contains("s-vanished") && err.contains("agent/e-orphan"),
            "the refusal names the session it looked for and the branch it is about: {err}"
        );
        assert!(
            err.contains("neither this daemon nor the store"),
            "and says which two places it looked: {err}"
        );
        assert!(wakes(&registry).is_empty(), "nothing was rung at anybody");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A refusal whose complaints could not be delivered goes to the PROJECT** — the second kind
    /// of handover, and the one the operator's sentence names: *"a person is the last resort, not
    /// the mechanism"*.
    ///
    /// The shape is the one `Harness::send_a_refusal_back` is in when the child's session is gone:
    /// the entry is parked with the VERDICT on its row, nobody in the queue can move it, and the
    /// only session that can put the work back in front of the review is the root of the tree it
    /// came from. Three things are asserted — the root is rung (resumed from the store), the row
    /// keeps the verdict in FRONT and gains the clause, and a second delivery failure does not tell
    /// the project twice.
    #[test]
    fn a_refusal_that_could_not_be_delivered_goes_to_the_project() {
        let dir = std::env::temp_dir().join(format!("letibot-gk-refusal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.db");
        let store = Store::open(&path).unwrap();
        session(&store, "s-root", None);
        session(&store, "s-child", Some("s-root"));
        let mut e = super::tests_entry_for_door("e-ref", "s-child");
        e.state = MergeState::Failed;
        // **The verdict, as `review_gate` renders it onto the row** — which is what is there when
        // the delivery fails, and what the handover has to leave in front of its own sentence.
        e.evidence =
            "gatekeeper verdict\n  branch   agent/e-ref\n  decision reject\n  reasons\n    \
                      - it does not do what the brief asked\n"
                .to_string();
        store.put_merge_entry(&e).unwrap();
        let registry = wired(&dir, &path);

        let said = hand_over_from_session(
            &store,
            &registry,
            "e-ref",
            "the complaints did NOT reach the child: the session is gone",
        )
        .expect("the project is told");
        assert!(
            said.contains("s-root") && said.contains("agent/e-ref"),
            "the sentence names the session that was rung and the branch: {said}"
        );
        assert_eq!(
            wakes(&registry),
            vec!["open s-root".to_string(), "woken s-root".to_string()],
            "the root is resumed from the store and rung"
        );

        let row = store.merge_entry("e-ref").unwrap().unwrap();
        assert_eq!(
            row.state,
            MergeState::Failed,
            "the handover does not move the entry: the verdict parked it, and it stays parked"
        );
        assert!(
            handed_over(&row),
            "the row carries the handover: {:?}",
            row.evidence
        );
        assert!(
            row.evidence.starts_with("gatekeeper verdict"),
            "and the VERDICT still leads it, because the pane truncates: {:?}",
            row.evidence
        );
        assert!(
            row.evidence.contains(REFUSAL_UNDELIVERED),
            "{:?}",
            row.evidence
        );

        // **The message the project gets is the refusal's, and it does not tell it to land.** The
        // two kinds share a door and must not share a sentence: *gate it and land it yourself* is
        // the one instruction this queue exists to make impossible about a refused branch.
        let message = handover_message(&row);
        assert!(
            message.contains("`agent/e-ref`") && message.contains("(entry `e-ref`)"),
            "the message names the branch and the entry: {message}"
        );
        assert!(
            message.contains("does NOT land as it stands"),
            "and says the branch does not land: {message}"
        );
        assert!(
            !message.contains("Gate it and land it yourself"),
            "a refused branch must never be told to land itself: {message}"
        );
        assert!(
            message.contains("/queue approve e-ref"),
            "and the operator's door is named: {message}"
        );

        // The second failure does not re-tell.
        let again = hand_over_from_session(&store, &registry, "e-ref", "and it failed again")
            .expect("a second call is answered, not an error");
        assert!(
            again.contains("was told once already"),
            "an entry already past that point is not re-told: {again}"
        );
        assert!(wakes(&registry).is_empty(), "and nothing is rung again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **And when the project is gone too, the ROW says so** — the last resort, written where a
    /// person reads it rather than only returned to the caller's log.
    ///
    /// The entry's session is in neither the registry nor the store, so there is no root to walk to
    /// and nobody to ring: the row carries the handover AND the fact that nobody could be told, and
    /// the person's three verbs are on it. That is `no_gatekeeper_evidence`'s two arms, one kind
    /// over, and the reason the ring's failure is written down rather than published and forgotten.
    #[test]
    fn a_refusal_with_no_project_to_tell_is_written_on_the_row() {
        let dir = std::env::temp_dir().join(format!("letibot-gk-lost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.db");
        let store = Store::open(&path).unwrap();
        let mut e = super::tests_entry_for_door("e-lost", "s-vanished");
        e.state = MergeState::Failed;
        e.evidence = "gatekeeper verdict\n  decision reject\n".to_string();
        store.put_merge_entry(&e).unwrap();
        let registry = wired(&dir, &path);

        let err = hand_over_from_session(
            &store,
            &registry,
            "e-lost",
            "the complaints did NOT reach the child: the session is gone",
        )
        .expect_err("there is no session to tell");
        assert!(
            err.contains("s-vanished") && err.contains("neither this daemon nor the store"),
            "the refusal names the session and the two places: {err}"
        );
        let row = store.merge_entry("e-lost").unwrap().unwrap();
        assert!(handed_over(&row), "{:?}", row.evidence);
        assert!(
            row.evidence.contains("nobody could be told either"),
            "and the row says the fallback itself fell through: {:?}",
            row.evidence
        );
        assert!(
            row.evidence.contains("A person is the last resort"),
            "with the person's verbs on it: {:?}",
            row.evidence
        );
        assert!(
            wakes(&registry).is_empty(),
            "and nothing was rung at anybody"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **`never started` and `not open yet` are different facts, and the grace is what draws the
    /// line between them** — the three answers of `gatekeeper_ready`, on one fixture.
    ///
    /// The queue is a thread of its own and the session that hosts reviews is opened by the
    /// daemon's WORKER, so an ask that arrives in the first second of a daemon's life arrives
    /// before the session that will serve it exists. Read without a grace, that window is a false
    /// alarm on every start — which is why the middle case below is `Ok` and the third is not.
    #[test]
    fn a_gatekeeper_that_never_started_is_not_one_that_is_not_open_yet() {
        use crate::sessions::gatekeeper::{SESSION, TITLE};
        let dir = std::env::temp_dir().join(format!("letibot-gk-grace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.db");
        let store = Store::open(&path).unwrap();
        store
            .put_merge_entry(&super::tests_entry_for_door("e-grace", "s-child"))
            .unwrap();
        let now_ms = (crate::config::now_ns() / 1_000_000) as u64;
        let request = |asked_ms: u64| letibot_tokencore::store::ReviewRecord {
            entry_id: "e-grace".into(),
            session_id: SESSION.into(),
            branch: "b-e-grace".into(),
            base_sha: "abc".into(),
            asked_ms,
            answered_ms: None,
            decision: None,
            attempts: 0,
            failed_ms: None,
            failure: String::new(),
            reasons: vec![],
            files: vec![],
            commands: vec![],
        };

        // **(1) The session is open in this daemon** — the ordinary case, and the only one where
        // the queue asks for a verdict at all.
        let registry = wired(&dir, &path);
        registry
            .create(SESSION, TITLE, SessionWiring::default())
            .expect("the queue's session registers");
        let door = GatekeeperDoor::new(
            Store::open(&path).unwrap(),
            registry.bell().clone(),
            registry.clone(),
        );
        assert!(
            door.gatekeeper_ready("e-grace").is_ok(),
            "an open session is a gatekeeper"
        );

        // **(2) Nothing anywhere, and the ask is young** — the startup window. `Ok`, because a
        // session that is not open yet is what the daemon's own clock is about to fix, and an ask
        // that went out is on the row for that clock to serve.
        let registry = SessionRegistry::new();
        let door = GatekeeperDoor::new(
            Store::open(&path).unwrap(),
            registry.bell().clone(),
            registry.clone(),
        );
        assert!(
            door.gatekeeper_ready("e-grace").is_ok(),
            "no request row at all is *nobody has asked yet*, not *nobody is there*"
        );
        store.put_review(&request(now_ms)).unwrap();
        assert!(
            door.gatekeeper_ready("e-grace").is_ok(),
            "and a request written a moment ago is the same answer: not open YET"
        );

        // **(3) Nothing anywhere, and the request has waited out the grace** — never started, and
        // the sentence says which two places were looked in.
        store
            .put_review(&request(now_ms.saturating_sub(GATEKEEPER_GRACE_MS + 1_000)))
            .unwrap();
        let why = door
            .gatekeeper_ready("e-grace")
            .expect_err("no session has judged this entry and none is coming");
        assert!(
            why.contains(SESSION) && why.contains("neither this daemon nor the store"),
            "the refusal names the session and the two places: {why}"
        );
        assert!(
            why.contains("Nothing was asked"),
            "and says that nothing was asked, which is what the queue's row then reports: {why}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// **A failed review, and the two ways it is attempted again** — the operator, verbatim: *"so
/// merge queue has 4 failed items, we need a way to restart them"*.
///
/// Its own module rather than a section of `tests`, because these are about the two doors onto a
/// review that failed — the queue's own bounded retry, and the person's `restart` — and the
/// fixtures they need (a store on disk, an entry, a door that records what it was asked) are the
/// queue's own, borrowed from `tests` rather than copied.
#[cfg(test)]
mod restart {
    use super::tests::{
        RecordingEvents, RecordingReviewer, enqueue, entry, quiet_events, store_at,
    };
    use super::*;
    use letibot_tokencore::store::MergePriority;

    // ===== A failed review, and the two ways it is attempted again =====

    /// **The bound, as a ladder** — `http_retry_after`'s discipline, asserted without a daemon,
    /// a child or a provider. Three attempts, doubling from thirty seconds, and then a stop.
    #[test]
    fn the_review_retry_is_bounded_and_doubles() {
        let row = |attempts: u32, failed_ms: Option<u64>| letibot_tokencore::store::ReviewRecord {
            entry_id: "m-1".into(),
            session_id: "s-host".into(),
            branch: "b".into(),
            base_sha: "base".into(),
            asked_ms: 0,
            answered_ms: None,
            decision: None,
            attempts,
            failed_ms,
            failure: "http 429: Weekly/Monthly Limit Exhausted".into(),
            reasons: vec![],
            files: vec![],
            commands: vec![],
        };
        let t = 1_000_000;
        // Nobody has asked, and an attempt in flight: both are DUE. The second is the queue's
        // re-ask on every pass — the recovery for a daemon that came up between the request row
        // and the bell — and it must not be read as a retry.
        assert_eq!(review_retry(None, t), ReviewRetry::Due);
        assert_eq!(review_retry(Some(&row(0, None)), t), ReviewRetry::Due);
        // One failure: thirty seconds, and the wait is counted down rather than repeated.
        assert_eq!(
            review_retry(Some(&row(1, Some(t))), t),
            ReviewRetry::Wait {
                in_ms: REVIEW_RETRY_BASE_MS
            }
        );
        assert_eq!(
            review_retry(Some(&row(1, Some(t))), t + 10_000),
            ReviewRetry::Wait {
                in_ms: REVIEW_RETRY_BASE_MS - 10_000
            }
        );
        assert_eq!(
            review_retry(Some(&row(1, Some(t))), t + 30_000),
            ReviewRetry::Due
        );
        // The second failure doubles it, which is the shape borrowed from `http_retry_after`.
        assert_eq!(
            review_retry(Some(&row(2, Some(t))), t),
            ReviewRetry::Wait {
                in_ms: REVIEW_RETRY_BASE_MS * 2
            }
        );
        // **And it STOPS.** The attempts are spent whatever the clock says, which is the half
        // that makes this a bound rather than a loop: an entry behind a weekly quota must be
        // reported, not asked about once a beat for ever.
        for elapsed in [0, 1_000_000, u64::MAX / 2] {
            assert_eq!(
                review_retry(Some(&row(MAX_REVIEW_ATTEMPTS, Some(t))), t + elapsed),
                ReviewRetry::Exhausted,
                "after {MAX_REVIEW_ATTEMPTS} failures the queue stops asking"
            );
        }
    }

    /// A store with one entry in it, and the entry's id.
    fn parked(name: &str, state: MergeState) -> (PathBuf, PathBuf, String) {
        let root =
            std::env::temp_dir().join(format!("letibot-mq-restart-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        let db = root.join("sessions.db");
        let mut e = entry("m-1", MergePriority::Subagent, state, 1_000);
        e.brief = "do the work".into();
        e.evidence = "the gate is red".into();
        enqueue(&db, &e);
        (root, db, e.id)
    }

    /// **A restart re-attempts exactly ONE review, and it names what it re-attempted.**
    ///
    /// The operator's ask, in their words: *"so merge queue has 4 failed items, we need a way to
    /// restart them"*. The entry is parked with a verdict on it — which is what the four are —
    /// and the restart puts it back to `Waiting` with its review cleared, so the queue's next
    /// pass asks once and takes nothing.
    #[test]
    fn a_restart_re_attempts_exactly_one_review() {
        let (root, db, id) = parked("once", MergeState::Failed);
        // The verdict that parked it, written by the reviewer's own half.
        store_at(&db)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: id.clone(),
                session_id: "s-host".into(),
                branch: "b-m-1".into(),
                base_sha: "base".into(),
                asked_ms: 1,
                answered_ms: Some(2),
                decision: Some("needs_human".into()),
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec!["the spec is ambiguous".into()],
                files: vec![],
                commands: vec![],
            })
            .expect("the verdict row");

        let events = RecordingEvents::default();
        let said = restart(&store_at(&db), &id, 5_000, &events.sink()).expect("the restart");
        // **What it names**, because the restart is a decision about something legible: the
        // branch, the entry, and the verdict that was on the row.
        assert!(said.contains("b-m-1"), "{said}");
        assert!(said.contains(&id), "{said}");
        assert!(said.contains("needs_human"), "{said}");

        let store = store_at(&db);
        let back = store.merge_entry(&id).expect("reads").expect("the entry");
        assert_eq!(
            back.state,
            MergeState::Waiting,
            "the parked entry is back in the queue"
        );
        assert!(
            back.evidence.contains("restarted by the operator"),
            "the row says who moved it: {:?}",
            back.evidence
        );
        let review = store.merge_review(&id).expect("reads").expect("the review");
        assert!(
            review.decision.is_none() && review.answered_ms.is_none(),
            "the verdict is cleared: the review is outstanding again, not judged twice"
        );
        assert_eq!(review.asked_ms, 5_000, "and the ask is restamped");
        // **The move is announced**, so a head that folded the queue's events sees it.
        assert_eq!(
            events.moves(),
            vec![(
                id.clone(),
                letibot_sessionlog::event::MergeState::Waiting,
                back.evidence.clone()
            )],
            "one move, and it is the restart"
        );

        // **And the queue asks ONCE.** One pass, one wake, and nothing taken — the entry is
        // `Waiting` with no verdict, which is exactly where it was before it was reviewed.
        let door = Arc::new(RecordingReviewer {
            says: Some("asked".into()),
            ..Default::default()
        });
        struct Shared(Arc<RecordingReviewer>);
        impl letibot_tools::gatekeeper::Reviewer for Shared {
            fn wake(
                &self,
                entry_id: &str,
                req: &letibot_tools::gatekeeper::ReviewRequest,
            ) -> Result<String, String> {
                self.0.wake(entry_id, req)
            }
        }
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            Box::new(Shared(door.clone())),
            quiet_events(),
        );
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::AwaitingReview
        );
        assert_eq!(
            *door.asked.lock().unwrap(),
            vec![(id.clone(), "b-m-1".to_string())],
            "exactly one review was asked for, and about this entry's branch"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A second ask does not double-spawn, and it says why** — in BOTH shapes the operator will
    /// actually press it in. The arbiter is the row rather than the caller's memory of what it
    /// just did, and the two refusals are the two different things a second press can be.
    #[test]
    fn a_second_restart_does_not_double_spawn() {
        // **Shape one: the entry has a review row on it** — which is every entry that has been
        // reviewed at all, and therefore all four of the operator's. The first ask clears the row
        // to *outstanding*, so the second press is refused as the live attempt it now is.
        let (root, db, id) = parked("twice", MergeState::Failed);
        store_at(&db)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: id.clone(),
                session_id: "s-host".into(),
                branch: "b-m-1".into(),
                base_sha: "base".into(),
                asked_ms: 1,
                answered_ms: Some(2),
                decision: Some("needs_human".into()),
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec![],
                files: vec![],
                commands: vec![],
            })
            .expect("the verdict row");
        let store = store_at(&db);
        let events = RecordingEvents::default();
        assert!(
            restart(&store, &id, 5_000, &events.sink()).is_ok(),
            "the first ask"
        );
        let again = restart(&store, &id, 5_001, &events.sink()).expect_err("the second ask");
        assert!(
            again.contains("a gatekeeper is working on") && again.contains("b-m-1"),
            "the second ask is refused by name: {again}"
        );
        // **One attempt, and it is the first one's**: the row is the same row, still outstanding,
        // still on the first ask's clock.
        let review = store.merge_review(&id).expect("reads").expect("the review");
        assert_eq!(
            review.asked_ms, 5_000,
            "the second ask did not restamp the ask, so it did not start a second attempt"
        );
        assert_eq!(
            events.moves().len(),
            1,
            "and only one move was announced, so no head is told about a restart that did not \
             happen"
        );
        let _ = std::fs::remove_dir_all(&root);

        // **Shape two: the entry has no review row at all** — an entry that was parked by the
        // gate rather than by a reviewer, or one whose session was never reachable. The first ask
        // moves it to `waiting`; the second is refused by the STATE, which is the other honest
        // answer to *you already did that*.
        let (root, db, id) = parked("twice-norow", MergeState::Failed);
        let store = store_at(&db);
        assert!(
            restart(&store, &id, 5_000, &|_| {}).is_ok(),
            "the first ask"
        );
        let again = restart(&store, &id, 5_001, &|_| {}).expect_err("the second ask");
        assert!(
            again.contains("nothing to restart") && again.contains("waiting"),
            "the second ask names the state it found: {again}"
        );
        assert!(
            store.merge_review(&id).expect("reads").is_none(),
            "and it wrote no review row for an entry nobody had asked about"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A live review cannot be restarted into a duplicate.** A reviewer is working on the
    /// entry — the row is outstanding and has neither answered nor failed — and a restart there
    /// would put a second gatekeeper beside a live one on one entry's verdict.
    ///
    /// The state is a real one and not a contrived one: a daemon that died mid-gate leaves the
    /// entry `Stale` (`recover`) while its review is still outstanding, which is precisely the
    /// row a person is most likely to press the key on.
    #[test]
    fn a_live_review_cannot_be_restarted_into_a_duplicate() {
        let (root, db, id) = parked("live", MergeState::Stale);
        store_at(&db)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: id.clone(),
                session_id: "s-host".into(),
                branch: "b-m-1".into(),
                base_sha: "base".into(),
                asked_ms: 1,
                // Outstanding: asked for, no verdict, no failure. An attempt in flight.
                answered_ms: None,
                decision: None,
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec![],
                files: vec![],
                commands: vec![],
            })
            .expect("the request row");

        let events = RecordingEvents::default();
        let why = restart(&store_at(&db), &id, 5_000, &events.sink()).expect_err("a refusal");
        assert!(
            why.contains("a gatekeeper is working on") && why.contains("b-m-1"),
            "the refusal names the live attempt: {why}"
        );
        // **And the entry was parked, so the ONLY thing that could have stopped this is the
        // live attempt.** A refusal that came from the state guard would leave this test green
        // about the wrong rule.
        assert!(restartable(&back_borrowed(&db, &id), None).is_ok());
        assert!(
            events.moves().is_empty(),
            "nothing moved, so no head is told an entry moved"
        );
        // **And the row is untouched** — the live attempt keeps its request, which is what stops
        // a second reviewer being started beside it.
        let review = store_at(&db)
            .merge_review(&id)
            .expect("reads")
            .expect("the review");
        assert_eq!(review.asked_ms, 1, "the live attempt's ask is untouched");
        let back = store_at(&db)
            .merge_entry(&id)
            .expect("reads")
            .expect("the entry");
        assert_eq!(
            back.state,
            MergeState::Stale,
            "and the entry is where it was"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The entry, read back — for the one assertion that has to say *this refusal came from the
    /// live attempt and not from the state*.
    fn back_borrowed(db: &Path, id: &str) -> MergeEntry {
        store_at(db)
            .merge_entry(id)
            .expect("reads")
            .expect("the entry")
    }

    /// **A restart on a reviewer that cannot answer surfaces the failure rather than hanging
    /// silently** — the operator's four entries, as a rule.
    ///
    /// The whole life of one: the first attempt fails, the entry waits with the failure ON ITS
    /// ROW and says when the queue will try again; the queue tries again; the attempts run out
    /// and the entry PARKS with the failure's own words — and nothing judged the branch, so the
    /// row does not read as a verdict. Then a person restarts it, and it is asked again.
    #[test]
    fn a_review_that_cannot_be_asked_parks_with_the_failure_on_the_row() {
        let (root, db, id) = parked("no-quota", MergeState::Waiting);
        let quota = "http 429: Weekly/Monthly Limit Exhausted. Your limit will reset at \
                     2026-10-12 15:01:48";
        let door = Arc::new(RecordingReviewer {
            says: Some("asked".into()),
            ..Default::default()
        });
        struct Shared(Arc<RecordingReviewer>);
        impl letibot_tools::gatekeeper::Reviewer for Shared {
            fn wake(
                &self,
                entry_id: &str,
                req: &letibot_tools::gatekeeper::ReviewRequest,
            ) -> Result<String, String> {
                self.0.wake(entry_id, req)
            }
        }
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| GateRun::green()),
            Box::new(Shared(door.clone())),
            quiet_events(),
        );
        // The first attempt: the queue asks, and the reviewer's attempt dies on the quota.
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::AwaitingReview
        );
        // **The request row the production door writes**, then the failure the HOST writes when
        // its gatekeeper's turn dies. Two writers, one row, exactly as in the daemon.
        store_at(&db)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: id.clone(),
                session_id: "s-host".into(),
                branch: "b-m-1".into(),
                base_sha: "base".into(),
                asked_ms: 1,
                answered_ms: None,
                decision: None,
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec![],
                files: vec![],
                commands: vec![],
            })
            .expect("the request row");
        let now = (crate::config::now_ns() / 1_000_000) as u64;
        let failure = |attempts: u32, failed_ms: u64| {
            let mut rec = store_at(&db).merge_review(&id).unwrap().unwrap();
            rec.attempts = attempts;
            rec.failed_ms = Some(failed_ms);
            rec.failure = quota.to_string();
            rec.answered_ms = None;
            rec.decision = None;
            store_at(&db).put_review(&rec).expect("the failure row");
        };
        failure(1, now);

        // **Between two attempts the failure is on the row, in the provider's own words**, and
        // the row says when the queue will try again. This is the half that makes the entry
        // legible without opening the store — the operator's *"invisible as failures"*.
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::Idle,
            "a failed attempt inside its backoff is not re-asked"
        );
        assert_eq!(
            door.asked.lock().unwrap().len(),
            1,
            "the backoff is what stops the queue asking once a pass"
        );
        let waiting = store_at(&db).merge_entry(&id).unwrap().unwrap();
        assert_eq!(waiting.state, MergeState::Waiting);
        assert!(
            waiting
                .evidence
                .starts_with("http 429: Weekly/Monthly Limit Exhausted"),
            "the row LEADS with the failure, because the pane truncates it: {:?}",
            waiting.evidence
        );
        assert!(
            waiting.evidence.contains("asks again in"),
            "and says what is being done about it: {:?}",
            waiting.evidence
        );

        // The backoff elapses and the queue tries again — no person needed.
        failure(2, now.saturating_sub(REVIEW_RETRY_BASE_MS * 4));
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::AwaitingReview
        );
        assert_eq!(door.asked.lock().unwrap().len(), 2, "the queue tried again");

        // **The attempts run out, and the entry PARKS with the failure on its row.**
        failure(
            MAX_REVIEW_ATTEMPTS,
            now.saturating_sub(REVIEW_RETRY_BASE_MS * 4),
        );
        assert_eq!(daemon.step().expect("the pass"), StepOutcome::ReviewGaveUp);
        assert_eq!(
            door.asked.lock().unwrap().len(),
            2,
            "a spent reviewer is not asked a third time"
        );
        let parked = store_at(&db).merge_entry(&id).unwrap().unwrap();
        assert_eq!(parked.state, MergeState::Failed, "the row is parked");
        assert!(
            parked
                .evidence
                .contains("http 429: Weekly/Monthly Limit Exhausted"),
            "the quota sentence is on the row: {:?}",
            parked.evidence
        );
        assert!(
            parked.evidence.contains("Nothing judged this branch"),
            "and it does not read as a verdict: {:?}",
            parked.evidence
        );
        assert!(
            parked.evidence.contains("/queue restart"),
            "and the verb that moves it is on the row: {:?}",
            parked.evidence
        );
        // **`decision` is still NULL**, which is the constraint this whole change turns on: a
        // failure must not be recorded as something a reader could take for a judgement.
        let rec = store_at(&db).merge_review(&id).unwrap().unwrap();
        assert!(
            rec.decision.is_none(),
            "a reviewer that could not be asked reached no judgement: {:?}",
            rec.decision
        );

        // **And a person can start it again** — which is the whole of the ask, and it is the
        // same retry the queue runs by itself rather than a second mechanism.
        let said = restart(&store_at(&db), &id, now, &|_| {}).expect("the restart");
        assert!(said.contains("attempt(s) failed"), "{said}");
        let back = store_at(&db).merge_entry(&id).unwrap().unwrap();
        assert_eq!(back.state, MergeState::Waiting, "restarted");
        let rec = store_at(&db).merge_review(&id).unwrap().unwrap();
        assert_eq!(rec.attempts, 0, "the bound is reset by the restart");
        assert_eq!(rec.failure, "", "and so is the failure");
        assert_eq!(
            daemon.step().expect("the pass"),
            StepOutcome::AwaitingReview,
            "and the queue asks again"
        );
        assert_eq!(door.asked.lock().unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The failure's own words lead the row, and the queue's explanation follows.** Asserted
    /// directly, because the order is the whole of what makes the failure readable: the pane
    /// hard-truncates this line at its width, so a sentence that opened with *the gatekeeper's
    /// attempt failed* would use the room `http 429` needs.
    #[test]
    fn the_failure_leads_the_row_it_is_written_on() {
        let quota = "http 429: Weekly/Monthly Limit Exhausted. Your limit will reset at \
                     2026-10-12 15:01:48";
        let rec = letibot_tokencore::store::ReviewRecord {
            entry_id: "m-1".into(),
            session_id: "s-host".into(),
            branch: "b".into(),
            base_sha: "base".into(),
            asked_ms: 1,
            answered_ms: None,
            decision: None,
            attempts: 1,
            failed_ms: Some(2),
            failure: quota.into(),
            reasons: vec![],
            files: vec![],
            commands: vec![],
        };
        let waiting = review_failed_evidence(Some(&rec), 30_000);
        assert!(waiting.starts_with("http 429"), "{waiting}");
        assert!(waiting.contains("asks again in 30s"), "{waiting}");
        let e = entry("m-1", MergePriority::Subagent, MergeState::Waiting, 1);
        let parked = review_exhausted_evidence(&e, Some(&rec));
        assert!(parked.starts_with("http 429"), "{parked}");
        assert!(parked.contains("failed 1 times"), "{parked}");
        assert!(parked.contains("/queue restart"), "{parked}");
    }
}

#[cfg(test)]
fn tests_entry_for_door(id: &str, session: &str) -> MergeEntry {
    MergeEntry {
        id: id.into(),
        session_id: session.into(),
        branch: format!("agent/{id}"),
        base_sha: "abc".into(),
        priority: letibot_tokencore::store::MergePriority::Subagent,
        needs: vec![],
        state: MergeState::Waiting,
        brief: String::new(),
        evidence: String::new(),
        created_ms: 1,
        updated_ms: 1,
        worktree: None,
        landed_sha: None,
        gate_steps: Vec::new(),
    }
}

/// **The person's three verbs, and the git `approve` absorbs** — the operator's ask, in their
/// words: *"i want to be able to approve / veto / delete"* and *"no way i touch git myself, we
/// build this queue not for that"*.
///
/// Its own module rather than a section of `tests`, for `restart`'s reason: these are about the
/// four doors onto an entry a person answers, and the fixtures they need (a store on disk, an
/// entry, a delivery that records what it was asked) are the queue's own, borrowed from `tests`
/// rather than copied.
#[cfg(test)]
mod person {
    use super::tests::{RecordingEvents, enqueue, entry, quiet_events, store_at};
    use super::*;

    /// **The pure decisions, asserted with no store, no git and no daemon** — every state of the
    /// closed set, and the two rows' facts that make a verb legal.
    #[test]
    fn the_pure_decisions_answer_for_every_state() {
        use letibot_tokencore::store::ReviewRecord;
        let rec = |decision: Option<&str>, answered: bool, failed: bool| ReviewRecord {
            entry_id: "m-1".into(),
            session_id: "s-reviewer".into(),
            branch: "b-m-1".into(),
            base_sha: "base".into(),
            asked_ms: 1,
            answered_ms: answered.then_some(2),
            decision: decision.map(str::to_string),
            attempts: usize::from(failed) as u32,
            failed_ms: failed.then_some(3),
            failure: if failed {
                "http 429".into()
            } else {
                String::new()
            },
            reasons: vec![],
            files: vec![],
            commands: vec![],
        };
        let at = |state: MergeState| entry("m-1", MergePriority::Subagent, state, 1_000);
        // **The two states no person's verb moves**, each with its own sentence: the queue is
        // working on one, the other is done.
        for state in [MergeState::Taken, MergeState::Landed] {
            let why = approvable(&at(state), None).expect_err("a refusal");
            assert!(why.contains(state.as_str()), "{state:?}: {why}");
            assert!(vetoable(&at(state)).is_err(), "{state:?} is not vetoable");
            assert!(
                removable(&at(state), None).is_err(),
                "{state:?} is not removable"
            );
        }
        // **Waiting and the parked four are a person's to answer.**
        for state in [
            MergeState::Waiting,
            MergeState::Failed,
            MergeState::Conflict,
            MergeState::Stale,
        ] {
            assert!(approvable(&at(state), None).is_ok(), "{state:?}");
            assert!(vetoable(&at(state)).is_ok(), "{state:?}");
            assert!(removable(&at(state), None).is_ok(), "{state:?}");
        }
        // **A live attempt is refused by all three of the verbs that name one**, and the refusal
        // says which fact stopped it: asked for, neither answered nor failed.
        let live = rec(None, false, false);
        for why in [
            approvable(&at(MergeState::Waiting), Some(&live)).expect_err("a refusal"),
            removable(&at(MergeState::Waiting), Some(&live)).expect_err("a refusal"),
            restartable(&at(MergeState::Failed), Some(&live)).expect_err("a refusal"),
        ] {
            assert!(why.contains("a gatekeeper is working on"), "{why}");
        }
        // **An attempt that DIED is not a live one** — the entry is a person's to answer.
        let died = rec(None, false, true);
        assert!(approvable(&at(MergeState::Waiting), Some(&died)).is_ok());
        assert!(removable(&at(MergeState::Waiting), Some(&died)).is_ok());
        // **A verdict that already accepts is nothing to override** — the second press.
        let accepting = rec(Some("accept"), true, false);
        let why = approvable(&at(MergeState::Waiting), Some(&accepting)).expect_err("a refusal");
        assert!(
            why.contains("already carries an accepting verdict"),
            "{why}"
        );
        assert!(
            approvable(
                &at(MergeState::Waiting),
                Some(&rec(Some("reject"), true, false))
            )
            .is_ok(),
            "a refusal is exactly what a person may override"
        );
        // **A veto is not a park, so a second veto is the same decision twice** — and it is the
        // only state a veto refuses beyond the two every verb refuses.
        let why = vetoable(&at(MergeState::Vetoed)).expect_err("a refusal");
        assert!(why.contains("already `vetoed`"), "{why}");
        // **And a person's veto is a state `approve` and `rm` may still move**: the way back from
        // a decision is a person's, one verb over.
        assert!(approvable(&at(MergeState::Vetoed), None).is_ok());
        assert!(removable(&at(MergeState::Vetoed), None).is_ok());
        // **A landed row is refused by `rm` for a reason of its own** — it is the base a
        // dependent rebases onto, so deleting it wedges one.
        let why = removable(&at(MergeState::Landed), None).expect_err("a refusal");
        assert!(why.contains("has landed and its row STAYS"), "{why}");
        // **And the four acts refuse an id that does not resolve, in the queue's own grammar.**
        let store = Store::open_in_memory().expect("a store");
        for why in [read_entry(&store, "m-nope").expect_err("a refusal")] {
            assert!(why.contains("no entry `m-nope`"), "{why}");
            assert!(why.contains("`/queue` lists what is there"), "{why}");
        }
    }

    /// **The sweep's decision, with no repository in reach** — and the one rule the operator's
    /// correction is load-bearing about: the build-cache redirect `task_start` writes is never
    /// swept, whatever else is in the worktree.
    #[test]
    fn the_sweep_never_takes_the_build_cache_redirect() {
        let redirect = BUILD_CACHE_REDIRECT.to_string();
        // **The branch has commits: the work is committed and the person's approval is a verdict.**
        assert_eq!(sweep_for(1, &["a.rs".into()]), Sweep::Already);
        assert_eq!(sweep_for(3, &[]), Sweep::Already);
        // **Nothing to land**: 0 commits and nothing changed, or 0 commits and nothing changed
        // but the redirect — which is not work, so the reviewer's *"lands nothing"* is true.
        assert_eq!(sweep_for(0, &[]), Sweep::Nothing);
        assert_eq!(sweep_for(0, &[redirect.clone()]), Sweep::Nothing);
        // **The sweep names the paths, and the redirect is not one of them.**
        assert_eq!(
            sweep_for(
                0,
                &["crates/a.rs".into(), redirect.clone(), "docs/b.md".into()]
            ),
            Sweep::Commit {
                paths: vec!["crates/a.rs".into(), "docs/b.md".into()],
                redirect: true,
            }
        );
        assert_eq!(
            sweep_for(0, &["a.rs".into()]),
            Sweep::Commit {
                paths: vec!["a.rs".into()],
                redirect: false,
            }
        );
        // **The marker is the writer's own sentence**, so a repository that tracks the path with
        // a REAL config in it is not mistaken for the redirect.
        assert!(is_build_cache_redirect(
            "# Written by `task_start`: the build cache is shared with the main tree, so this \
             worktree does not pay a cold build of the whole graph.\n[build]\ntarget-dir = \"/x/target\"\n"
        ));
        assert!(!is_build_cache_redirect(
            "[build]\ntarget = \"x86_64-unknown-linux-musl\"\n"
        ));
        assert!(!is_build_cache_redirect(""));
    }

    /// **The paths git names, from its own NUL-separated output** — the parse that decides what a
    /// commit message and a row will say, asserted on git's shape rather than on a guess about it.
    #[test]
    fn the_changed_paths_come_back_as_git_names_them() {
        assert_eq!(
            changed_paths(" M crates/a.rs\0?? docs/b.md\0"),
            vec!["crates/a.rs", "docs/b.md"]
        );
        // **A rename carries TWO paths and only the destination is committed** — the origin is
        // the path it came from, and listing it as a change would put a file that is not there
        // into the commit message.
        assert_eq!(
            changed_paths("R  new/name.rs\0old/name.rs\0 M a.rs\0"),
            vec!["new/name.rs", "a.rs"]
        );
        // A path with a space survives, which is the whole reason for `-z`.
        assert_eq!(changed_paths("?? a b c.txt\0"), vec!["a b c.txt"]);
        assert!(changed_paths("").is_empty());
    }

    /// **A store with one entry in it**, and the entry's id — `restart`'s own fixture.
    fn parked(name: &str, state: MergeState) -> (PathBuf, PathBuf, String) {
        let root =
            std::env::temp_dir().join(format!("letibot-mq-person-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        let db = root.join("sessions.db");
        let mut e = entry("m-1", MergePriority::Subagent, state, 1_000);
        e.brief = "do the work".into();
        e.evidence = "the branch is 0 commits over its base".into();
        enqueue(&db, &e);
        (root, db, e.id)
    }

    // ===== The two sweeps: reset the reviews, or clean the rows out =====

    /// **A whole-queue reset re-asks every parked review and moves nothing else.**
    ///
    /// The operator's ruling, in their words: *"as for reset - no reset resets review states"*.
    /// Two entries are parked with verdicts on them and two are not — a `waiting` one and a
    /// `landed` one, both with a verdict of their own — and the sweep moves the two parked rows,
    /// clears exactly their reviews, and announces one move per row, which is what a head that
    /// folded the queue needs. The branches, the worktrees and the untouched rows are asserted to
    /// be exactly what they were: a reset that touched them would not be the verb that was asked
    /// for.
    #[test]
    fn a_whole_queue_reset_re_asks_the_parked_reviews_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("letibot-mq-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        let db = root.join("sessions.db");
        for (id, state, at) in [
            ("m-failed", MergeState::Failed, 1_000u64),
            ("m-conflict", MergeState::Conflict, 2_000),
            ("m-waiting", MergeState::Waiting, 3_000),
            ("m-landed", MergeState::Landed, 4_000),
        ] {
            let mut e = entry(id, MergePriority::Subagent, state, at);
            e.evidence = "the gate is red".into();
            e.worktree = Some(format!("/wt/{id}"));
            enqueue(&db, &e);
            // A verdict on every one of them, so "cleared" and "left alone" are both claims
            // about a row that had something to clear.
            refused(&db, id);
        }

        let events = RecordingEvents::default();
        let said = reset(&store_at(&db), None, 9_000, &events.sink()).expect("the reset");
        assert!(said.contains("reset 2 entries"), "{said}");
        assert!(said.contains("`b-m-failed` (failed)"), "{said}");
        assert!(said.contains("`b-m-conflict` (conflict)"), "{said}");

        let store = store_at(&db);
        for id in ["m-failed", "m-conflict"] {
            let back = store.merge_entry(id).expect("reads").expect("the entry");
            assert_eq!(back.state, MergeState::Waiting, "{id} is back in the queue");
            assert_eq!(
                back.evidence,
                letibot_tokencore::store::Store::RESET_EVIDENCE,
                "{id}: the row says what moved it"
            );
            assert_eq!(
                back.worktree,
                Some(format!("/wt/{id}")),
                "{id}: the worktree is the entry's own, untouched"
            );
            let cleared = store.merge_review(id).expect("reads").expect("the review");
            assert_eq!(cleared.asked_ms, 9_000, "{id}: the ask is a NEW ask");
            assert!(
                cleared.decision.is_none() && cleared.answered_ms.is_none(),
                "{id}: the verdict is cleared, so the queue asks again"
            );
        }
        for (id, state) in [
            ("m-waiting", MergeState::Waiting),
            ("m-landed", MergeState::Landed),
        ] {
            let still = store.merge_entry(id).expect("reads").expect("the entry");
            assert_eq!(still.state, state, "{id} did not move");
            assert_eq!(
                still.evidence, "the gate is red",
                "{id}: its row is its own"
            );
            let kept = store.merge_review(id).expect("reads").expect("the review");
            assert_eq!(kept.asked_ms, 1, "{id}: its review was not restamped");
            assert_eq!(kept.decision.as_deref(), Some("reject"), "{id}");
        }
        // **One move per row, and they are the two that moved** — the queue's own event, so no
        // head needs a second vocabulary to learn that a parked entry is being tried again.
        let moves = events.moves();
        assert_eq!(moves.len(), 2, "{moves:?}");
        let ids: Vec<&str> = moves.iter().map(|(id, _, _)| id.as_str()).collect();
        assert!(
            ids.contains(&"m-failed") && ids.contains(&"m-conflict"),
            "{ids:?}"
        );
        assert!(
            moves.iter().all(|(_, state, evidence)| {
                *state == letibot_sessionlog::event::MergeState::Waiting
                    && evidence == letibot_tokencore::store::Store::RESET_EVIDENCE
            }),
            "{moves:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A named reset of an entry that is not parked is refused, and a whole queue with nothing
    /// parked is not a failure** — two different facts, and they get two different answers.
    #[test]
    fn a_named_reset_refuses_an_entry_that_is_not_parked() {
        let (root, db, id) = parked("reset-named", MergeState::Waiting);
        let why = reset(&store_at(&db), Some(&id), 9_000, &quiet_events()).expect_err("a refusal");
        assert!(why.contains("nothing was reset"), "{why}");
        assert!(why.contains(&id), "{why}");
        // **The whole queue, with nothing parked, says so rather than failing**: `/queue reset` on
        // a queue that is all in flight is a sweep that found nothing, not a verb that did not
        // work.
        let said = reset(&store_at(&db), None, 9_000, &quiet_events()).expect("the empty sweep");
        assert!(said.contains("nothing was reset"), "{said}");
        assert!(said.contains("no parked entry"), "{said}");
        assert_eq!(
            store_at(&db).merge_entry(&id).unwrap().unwrap().state,
            MergeState::Waiting,
            "and nothing moved"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A whole-queue clean deletes what may go and REPORTS what stayed.**
    ///
    /// The operator's ruling, in their words: *"/queue clean deletes"*. Three rows go — a
    /// `failed`, a `conflict` and a `vetoed` — and three stay, each for [`removable`]'s own
    /// reason: a `landed` row is the base a dependent rebases onto, a `taken` one is being landed
    /// right now, and a `waiting` one has a gatekeeper mid-answer. A purge that skipped a row
    /// silently is a purge nobody can trust, so the sentence names both lists — and the branch and
    /// the state of every row that went, so nothing is deleted without a person being able to see
    /// what.
    #[test]
    fn a_whole_queue_clean_deletes_what_may_go_and_reports_what_stayed() {
        let root = std::env::temp_dir().join(format!("letibot-mq-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        let db = root.join("sessions.db");
        for (id, state, at) in [
            ("m-failed", MergeState::Failed, 1_000u64),
            ("m-conflict", MergeState::Conflict, 2_000),
            ("m-vetoed", MergeState::Vetoed, 3_000),
            ("m-waiting", MergeState::Waiting, 4_000),
            ("m-taken", MergeState::Taken, 5_000),
            ("m-landed", MergeState::Landed, 6_000),
        ] {
            let e = entry(id, MergePriority::Subagent, state, at);
            enqueue(&db, &e);
        }
        // **A live attempt on the waiting row** — asked for, neither answered nor failed — which
        // is the one refusal a clean makes that the state word does not carry.
        let e = store_at(&db)
            .merge_entry("m-waiting")
            .expect("reads")
            .expect("the entry");
        store_at(&db)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: "m-waiting".into(),
                session_id: crate::mergequeue::REVIEWER_SESSION_ID.to_string(),
                branch: e.branch,
                base_sha: e.base_sha,
                asked_ms: 1,
                answered_ms: None,
                decision: None,
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec![],
                files: vec![],
                commands: vec![],
            })
            .expect("the live ask");

        let events = RecordingEvents::default();
        let said = clean(&store_at(&db), None, &events.sink()).expect("the clean");
        assert!(said.contains("cleaned 3 entries"), "{said}");
        for want in [
            "`b-m-failed` (failed)",
            "`b-m-conflict` (conflict)",
            "`b-m-vetoed` (vetoed)",
        ] {
            assert!(said.contains(want), "the purge names what went: {said}");
        }
        assert!(said.contains("3 row(s) stayed"), "{said}");
        assert!(said.contains("`b-m-landed` (landed)"), "{said}");
        assert!(said.contains("`b-m-taken` (taken)"), "{said}");
        assert!(
            said.contains("`b-m-waiting` (waiting — a gatekeeper is working on it)"),
            "{said}"
        );

        let store = store_at(&db);
        for id in ["m-failed", "m-conflict", "m-vetoed"] {
            assert!(store.merge_entry(id).unwrap().is_none(), "{id} went");
            assert!(
                store.merge_review(id).unwrap().is_none(),
                "{id}: its verdict went with it"
            );
        }
        for id in ["m-waiting", "m-taken", "m-landed"] {
            assert!(store.merge_entry(id).unwrap().is_some(), "{id} stayed");
        }
        // **One absence per row, and it names the branch** — a head that folded the entries in
        // needs the event, or the pane goes on drawing rows the queue no longer holds.
        let removals = events.removals();
        assert_eq!(removals.len(), 3, "{removals:?}");
        let ids: Vec<&str> = removals.iter().map(|(id, _)| id.as_str()).collect();
        assert!(
            ids.contains(&"m-failed") && ids.contains(&"m-conflict") && ids.contains(&"m-vetoed"),
            "{ids:?}"
        );
        assert!(
            removals
                .iter()
                .all(|(_, evidence)| evidence.contains("forgotten the entry, not the work")),
            "{removals:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A named clean refuses a row that may not go, and a whole queue with nothing to purge says
    /// so** — `rm`'s rule and `rm`'s sentence for the one row the operator named, and a report for
    /// the sweep that found nothing it may take.
    #[test]
    fn a_named_clean_refuses_a_landed_row_and_the_whole_queue_says_when_there_is_nothing() {
        let (root, db, id) = parked("clean-named", MergeState::Landed);
        let why = clean(&store_at(&db), Some(&id), &quiet_events()).expect_err("a refusal");
        assert!(why.contains("has landed and its row STAYS"), "{why}");
        assert!(
            store_at(&db).merge_entry(&id).unwrap().is_some(),
            "nothing was deleted"
        );
        // **The whole queue, with nothing a clean may take, is not a failure** — the sweep reports
        // what it found and what stayed, with the rule that kept it.
        let said = clean(&store_at(&db), None, &quiet_events()).expect("the empty sweep");
        assert!(said.contains("nothing was cleaned"), "{said}");
        assert!(said.contains("`b-m-1` (landed)"), "{said}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The review a person overrides**: a refusal, with the reasons and the files it was based
    /// on, which is what a veto carries to the child and what an approval replaces.
    fn refused(db: &Path, id: &str) {
        let e = store_at(db)
            .merge_entry(id)
            .expect("reads")
            .expect("the entry");
        store_at(db)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: id.to_string(),
                session_id: crate::mergequeue::REVIEWER_SESSION_ID.to_string(),
                branch: e.branch,
                base_sha: e.base_sha,
                asked_ms: 1,
                answered_ms: Some(2),
                decision: Some("reject".into()),
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec!["the branch is 0 commits over its base".into()],
                files: vec!["crates/widget.rs".into()],
                commands: vec!["git diff base...branch".into()],
            })
            .expect("the verdict");
    }

    /// **An approval writes the verdict, moves the entry, and says the gate still runs.**
    #[test]
    fn an_approval_moves_the_entry_and_names_what_it_overrode() {
        let (root, db, id) = parked("approve", MergeState::Failed);
        refused(&db, &id);
        let events = RecordingEvents::default();
        let said = approve(&store_at(&db), &id, "s-operator", 5_000, &events.sink())
            .expect("the approval");
        // **What it overrode, in the row's own words** — a person who pressed it on the wrong
        // entry has to be able to see that they did.
        assert!(said.contains("the verdict on it was `reject`"), "{said}");
        // **And the half that must never be silent: the gate was not skipped.**
        assert!(said.contains("the gate is not skipped"), "{said}");
        let back = store_at(&db)
            .merge_entry(&id)
            .expect("reads")
            .expect("the entry");
        assert_eq!(back.state, MergeState::Waiting, "back in the queue");
        assert!(
            back.evidence.starts_with("approved by the operator"),
            "{back:?}"
        );
        assert!(back.evidence.contains("the gate still runs"), "{back:?}");
        // **The verdict the queue reads, with the person on it.**
        let ruled = store_at(&db)
            .merge_review(&id)
            .expect("reads")
            .expect("a row");
        assert_eq!(ruled.decision.as_deref(), Some("accept"));
        assert_eq!(ruled.session_id, "s-operator");
        assert!(
            ruled.reasons.is_empty(),
            "the reviewer's argument is replaced"
        );
        // **Every head hears**, through the queue's own event and no new vocabulary.
        assert_eq!(
            events.moves(),
            vec![(
                id.clone(),
                letibot_sessionlog::event::MergeState::Waiting,
                back.evidence.clone()
            )]
        );
        // **The second press is refused, and it is the verdict row that refuses it.**
        let why = approve(&store_at(&db), &id, "s-operator", 5_001, &quiet_events())
            .expect_err("a second press");
        assert!(
            why.contains("already carries an accepting verdict"),
            "{why}"
        );
        assert_eq!(
            store_at(&db).merge_entry(&id).unwrap().unwrap().evidence,
            back.evidence,
            "and the second press wrote nothing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **An approval of an entry with nothing to land refuses, and says what it found** — the
    /// reviewer's *"landing the branch lands nothing"* is true, and a person pressing the verb
    /// has to be told that rather than obeyed into an empty landing.
    #[test]
    fn an_approval_of_a_clean_branch_with_nothing_on_it_refuses() {
        // A real repository, because the two facts are git's: 0 commits over the base, and a
        // worktree with nothing in it.
        let root = std::env::temp_dir().join(format!("letibot-mq-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "one\n").expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "first"]);
        let base = git(&root, &["rev-parse", "main"])
            .expect("rev-parse")
            .trim()
            .to_string();
        git(&root, &["branch", "feature"]);
        let wt = root.join("worktrees").join("feature");
        git(
            &root,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "feature"],
        );
        let db = root.join("sessions.db");
        let mut e = entry("m-1", MergePriority::Subagent, MergeState::Failed, 1_000);
        e.base_sha = base;
        e.worktree = Some(wt.to_string_lossy().into_owned());
        e.branch = "feature".into();
        enqueue(&db, &e);
        let why = approve(&store_at(&db), &e.id, "s-operator", 5_000, &quiet_events())
            .expect_err("a refusal");
        assert!(why.contains("has nothing to land"), "{why}");
        assert!(why.contains("0 commits over its base"), "{why}");
        assert_eq!(
            store_at(&db).merge_entry(&e.id).unwrap().unwrap().state,
            MergeState::Failed,
            "nothing was approved and nothing was committed"
        );
        assert_eq!(
            git(&wt, &["rev-list", "--count", "main..HEAD"])
                .unwrap()
                .trim(),
            "0",
            "and the branch is where it was"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The sweep commits the worktree's work and does NOT commit the build-cache redirect** —
    /// the operator's first constraint, measured against a real repository that TRACKS the path
    /// (the letidb shape), because that is the case a blind `git add -A` gets wrong.
    #[test]
    fn the_sweep_commits_the_work_and_restores_the_tracked_redirect() {
        let root = std::env::temp_dir().join(format!("letibot-mq-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "t"]);
        // **A TRACKED `.cargo/config.toml`** — the repository's own real config, which is what a
        // blind sweep would commit away.
        std::fs::create_dir_all(root.join(".cargo")).expect("mkdir");
        let real_config = "[build]\ntarget = \"x86_64-unknown-linux-musl\"\n";
        std::fs::write(root.join(BUILD_CACHE_REDIRECT), real_config).expect("write");
        std::fs::write(root.join("a.txt"), "one\n").expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "first"]);
        let base = git(&root, &["rev-parse", "main"])
            .expect("rev-parse")
            .trim()
            .to_string();
        git(&root, &["branch", "feature"]);
        let wt = root.join("worktrees").join("feature");
        git(
            &root,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "feature"],
        );
        // **`task_start`'s own two writes**, verbatim: the redirect over the tracked config, and
        // the work the child left.
        std::fs::write(
            wt.join(BUILD_CACHE_REDIRECT),
            "# Written by `task_start`: the build cache is shared with the main tree, so this \
             worktree does not pay a cold build of the whole graph.\n[build]\ntarget-dir = \
             \"/x/target\"\n",
        )
        .expect("write");
        std::fs::create_dir_all(wt.join("docs")).expect("mkdir");
        std::fs::write(wt.join("b.txt"), "the work\n").expect("write");
        std::fs::write(wt.join("docs").join("c.md"), "more work\n").expect("write");

        // **The facts, then the decision, then the act** — the three steps the verb takes.
        let (commits, changed) = worktree_facts(&wt, &base).expect("git's facts");
        assert_eq!(commits, 0, "the branch carries nothing yet");
        let Sweep::Commit { paths, redirect } = sweep_for(commits, &changed) else {
            panic!("the sweep is the act here: {changed:?}");
        };
        assert!(redirect, "the redirect was among the changed paths");
        assert!(
            !paths.iter().any(|p| p == BUILD_CACHE_REDIRECT),
            "and it is not one of the paths to commit: {paths:?}"
        );
        assert!(paths.contains(&"b.txt".to_string()), "{paths:?}");
        assert!(paths.contains(&"docs/c.md".to_string()), "{paths:?}");
        let said = sweep_worktree(&wt, "m-1", &paths, redirect).expect("the sweep");
        assert!(
            said.contains("b.txt") && said.contains("docs/c.md"),
            "{said}"
        );
        assert!(said.contains("restored the build-cache redirect"), "{said}");

        // **The commit is on the branch, with the work in it.**
        assert_eq!(
            git(&wt, &["rev-list", "--count", &format!("{base}..HEAD")])
                .unwrap()
                .trim(),
            "1",
            "one commit, not two"
        );
        let files = git(&wt, &["show", "--name-only", "--format=", "HEAD"]).expect("show");
        assert!(
            files.contains("b.txt") && files.contains("docs/c.md"),
            "{files}"
        );
        assert!(
            !files.contains(BUILD_CACHE_REDIRECT),
            "the redirect is NOT in the commit: {files}"
        );
        // **And the repository's own config is what is in the tree and in the commit** — the
        // first constraint, measured: a blind `git add -A` would have committed the redirect.
        let in_commit = git(&wt, &["show", &format!("HEAD:{BUILD_CACHE_REDIRECT}")]).expect("show");
        assert_eq!(in_commit, real_config, "the real config survived the sweep");
        // **And the worktree is clean, which is what the rebase the queue does next needs** — a
        // modified tracked file is a rebase git REFUSES (`cannot rebase: You have unstaged
        // changes`), and a sweep that left it dirty would turn into the same conflict one pass
        // later.
        assert_eq!(
            git(&wt, &["status", "--porcelain"]).unwrap().trim(),
            "",
            "the worktree is clean after the sweep"
        );
        // **And the sweep is idempotent**: a second look finds the branch carrying a commit, so
        // there is nothing to sweep and nothing is committed twice.
        let (commits, changed) = worktree_facts(&wt, &base).expect("git's facts");
        assert_eq!(sweep_for(commits, &changed), Sweep::Already);
        assert_eq!(
            git(&wt, &["rev-list", "--count", &format!("{base}..HEAD")])
                .unwrap()
                .trim(),
            "1"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A veto sends the child back to work, carries the reasons, and parks the entry as a
    /// person's decision.**
    #[test]
    fn a_veto_sends_the_child_back_and_parks_the_entry() {
        let (root, db, id) = parked("veto", MergeState::Waiting);
        refused(&db, &id);
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let inbox = sent.clone();
        let deliver = move |handle: &str, text: &str| {
            inbox
                .lock()
                .unwrap()
                .push((handle.to_string(), text.to_string()));
            Ok("its turn is running, so it hears this at its next round boundary.".to_string())
        };
        let events = RecordingEvents::default();
        let said = veto(
            &store_at(&db),
            &id,
            "it lands nothing — put the work in a commit",
            5_000,
            &deliver,
            &events.sink(),
        )
        .expect("the veto");
        // **The message went to the ENTRY's own id** — the child that did the work, which is what
        // the id is (`entry_for_finished`) — and it carries the person's words and the reasons the
        // review gave, because a rejection a child cannot read a reason in is one it cannot act on.
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "one message, to one child");
        assert_eq!(sent[0].0, id, "the entry's id IS the child");
        assert!(
            sent[0].1.contains("put the work in a commit"),
            "the person's own words: {}",
            sent[0].1
        );
        assert!(
            sent[0].1.contains("the branch is 0 commits over its base"),
            "and the review's reasons: {}",
            sent[0].1
        );
        assert!(
            sent[0].1.contains("b-m-1"),
            "and which branch: {}",
            sent[0].1
        );
        drop(sent);
        // **The row is a person's decision, not a machine's**, and the report says what happened
        // to the ball.
        assert!(said.contains("the child has it back"), "{said}");
        assert!(said.contains("a decision somebody made"), "{said}");
        let back = store_at(&db).merge_entry(&id).unwrap().unwrap();
        assert_eq!(back.state, MergeState::Vetoed);
        assert!(back.evidence.contains("vetoed by the operator"), "{back:?}");
        assert!(
            back.evidence.contains("put the work in a commit"),
            "{back:?}"
        );
        // **And the review is untouched**: the judgement the person overrode is a fact about the
        // branch, and the pane reads that column as the reviewer's.
        let ruled = store_at(&db).merge_review(&id).unwrap().unwrap();
        assert_eq!(ruled.decision.as_deref(), Some("reject"));
        assert_eq!(ruled.reasons, vec!["the branch is 0 commits over its base"]);
        assert_eq!(
            events.moves(),
            vec![(
                id.clone(),
                letibot_sessionlog::event::MergeState::Vetoed,
                back.evidence.clone()
            )]
        );
        // **A second veto is the same decision twice**, and the row says so.
        let why = veto(
            &store_at(&db),
            &id,
            "again",
            5_001,
            &deliver,
            &quiet_events(),
        )
        .expect_err("a second veto");
        assert!(why.contains("already `vetoed`"), "{why}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A refusing verdict's own sentence, back to the child** — the gatekeeper's half of the
    /// same act a person's veto is, and the complaints are the review's.
    ///
    /// The second caller of the session channel, at the level of the words: `serve_reviews`
    /// delivers exactly this through `TaskRunner::send`. What it must carry is the verdict word,
    /// the reviewer's reasons, which branch and entry are back with the child, and where to carry
    /// on — and it must NOT carry a person's voice, which is the one thing that would make a
    /// gatekeeper's refusal read as an override.
    #[test]
    fn a_refusal_message_carries_the_reviewers_complaints_and_no_persons_words() {
        let (root, db, id) = parked("refusal-message", MergeState::Waiting);
        refused(&db, &id);
        let entry = store_at(&db).merge_entry(&id).unwrap().unwrap();
        let review = store_at(&db).merge_review(&id).unwrap();
        let said = refusal_message(&entry, review.as_ref());
        assert!(
            said.contains("The gatekeeper refused it"),
            "the sender is named: {said}"
        );
        assert!(said.contains("`reject`"), "and its verdict: {said}");
        assert!(
            said.contains("the branch is 0 commits over its base"),
            "and the complaints, verbatim: {said}"
        );
        assert!(
            said.contains(&format!("`{id}`")),
            "and which entry is back with the child: {said}"
        );
        assert!(
            said.contains("Carry on in the worktree"),
            "and what to do next: {said}"
        );
        assert!(
            !said.contains("operator"),
            "nothing in it may read as a person's override: {said}"
        );
        // **A refusal with no reasons written down is still a refusal.** `reject` and an empty
        // `reasons:` is a reviewer that judged and wrote nothing; the one sentence that is false
        // about it is *nobody has judged the branch*.
        let mut bare = store_at(&db).merge_review(&id).unwrap().unwrap();
        bare.reasons.clear();
        let said = refusal_message(&entry, Some(&bare));
        assert!(!said.contains("Nobody has judged"), "{said}");
        assert!(
            said.contains("wrote no reasons down"),
            "the row is said to be the whole of what it said: {said}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A child that is gone is refused, and NOTHING is moved** — the ball cannot go back to a
    /// session that is not there, and the act must not report a delivery that did not happen.
    #[test]
    fn a_veto_whose_child_is_gone_moves_nothing() {
        let (root, db, id) = parked("gone", MergeState::Failed);
        let gone = |_: &str, _: &str| {
            Err(
                "no session `m-1` is live in this daemon, so there is nothing to say it to."
                    .to_string(),
            )
        };
        let events = RecordingEvents::default();
        let why =
            veto(&store_at(&db), &id, "no", 5_000, &gone, &events.sink()).expect_err("a refusal");
        // **The refusal names the child and the moves that are left**, rather than pretending the
        // ball went back.
        assert!(why.contains("was NOT delivered to `m-1`"), "{why}");
        assert!(why.contains("Nothing was moved"), "{why}");
        assert!(why.contains("/queue approve m-1"), "{why}");
        assert!(why.contains("/queue rm m-1"), "{why}");
        assert_eq!(
            store_at(&db).merge_entry(&id).unwrap().unwrap().state,
            MergeState::Failed,
            "the entry is where it was"
        );
        assert!(events.moves().is_empty(), "and no head was told it moved");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A removal forgets the entry and its verdict, and every head is told it went.**
    #[test]
    fn a_removal_forgets_the_entry_and_its_verdict() {
        let (root, db, id) = parked("rm", MergeState::Failed);
        refused(&db, &id);
        let events = RecordingEvents::default();
        let said = remove(&store_at(&db), &id, &events.sink()).expect("the removal");
        assert!(
            said.contains("the queue has forgotten the entry, not the work"),
            "{said}"
        );
        assert!(
            store_at(&db).merge_entry(&id).unwrap().is_none(),
            "the row is gone"
        );
        assert!(
            store_at(&db).merge_review(&id).unwrap().is_none(),
            "and its verdict with it: a verdict nothing can be joined against is a leak"
        );
        let removals = events.removals();
        assert_eq!(
            removals.len(),
            1,
            "the head is told, or it draws a row that is gone"
        );
        assert_eq!(removals[0].0, id);
        assert!(
            removals[0]
                .1
                .contains("removed from the queue by the operator")
        );
        // **A second press is refused BY NAME, and that is the shape a deletion has.** The row is
        // gone, so the second press cannot resolve the id at all — the same refusal an id that
        // never existed gets, which is the truth of it. The conditional `DELETE` is still the
        // arbiter for the race (the daemon claiming the entry, or a landing, between the read and
        // the write), which is why the store's `false` is handled rather than assumed.
        let why = remove(&store_at(&db), &id, &quiet_events()).expect_err("a second press");
        assert!(why.contains("no entry `m-1`"), "{why}");
        assert!(why.contains("`/queue` lists what is there"), "{why}");
        let why = remove(&store_at(&db), "m-nope", &quiet_events()).expect_err("an unknown id");
        assert!(why.contains("no entry `m-nope`"), "{why}");
        assert!(why.contains("`/queue` lists what is there"), "{why}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The three verbs refuse a live attempt and a row the queue has taken**, at the act's own
    /// door and not only in the pure decision — the sentences a person reads.
    #[test]
    fn the_verbs_refuse_a_live_attempt_and_a_taken_row() {
        use letibot_tokencore::store::ReviewRecord;
        let (root, db, id) = parked("live", MergeState::Failed);
        let e = store_at(&db).merge_entry(&id).unwrap().unwrap();
        store_at(&db)
            .put_review(&ReviewRecord {
                entry_id: id.clone(),
                session_id: crate::mergequeue::REVIEWER_SESSION_ID.to_string(),
                branch: e.branch.clone(),
                base_sha: e.base_sha.clone(),
                asked_ms: 1,
                answered_ms: None,
                decision: None,
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec![],
                files: vec![],
                commands: vec![],
            })
            .expect("the live attempt");
        for why in [
            approve(&store_at(&db), &id, "s-operator", 5_000, &quiet_events())
                .expect_err("a refusal"),
            remove(&store_at(&db), &id, &quiet_events()).expect_err("a refusal"),
        ] {
            assert!(why.contains("a gatekeeper is working on"), "{why}");
        }
        // **And the entry is untouched** — the live attempt keeps its request.
        let review = store_at(&db).merge_review(&id).unwrap().unwrap();
        assert!(review.decision.is_none() && review.answered_ms.is_none());
        assert_eq!(
            store_at(&db).merge_entry(&id).unwrap().unwrap().state,
            MergeState::Failed
        );
        // **A taken row is refused by all three**, with the daemon's own reason.
        let (root2, db2, id2) = parked("taken", MergeState::Taken);
        for why in [
            approve(&store_at(&db2), &id2, "s-operator", 5_000, &quiet_events())
                .expect_err("a refusal"),
            veto(
                &store_at(&db2),
                &id2,
                "no",
                5_000,
                &|_, _| Ok(String::new()),
                &quiet_events(),
            )
            .expect_err("a refusal"),
            remove(&store_at(&db2), &id2, &quiet_events()).expect_err("a refusal"),
        ] {
            assert!(why.contains("is `taken`"), "{why}");
            assert!(why.contains("Nothing was done"), "{why}");
        }
        assert_eq!(
            store_at(&db2).merge_entry(&id2).unwrap().unwrap().state,
            MergeState::Taken
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root2);
    }

    // ===== The three doors: what a REPORTED verdict does =====

    /// **The live ask a verdict is the answer to** — a review row that is neither answered nor
    /// failed, which is the state a reported verdict arrives in.
    ///
    /// It is a fixture rather than a convenience: `approvable` refuses exactly this row, so a door
    /// that went through the person's guard instead of the person's PATH would fail here and
    /// nowhere else.
    fn asking(db: &Path, id: &str) {
        let e = store_at(db)
            .merge_entry(id)
            .expect("reads")
            .expect("the entry");
        store_at(db)
            .put_review(&letibot_tokencore::store::ReviewRecord {
                entry_id: id.to_string(),
                session_id: crate::sessions::gatekeeper::SESSION.to_string(),
                branch: e.branch,
                base_sha: e.base_sha,
                asked_ms: 1,
                answered_ms: None,
                decision: None,
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: vec![],
                files: vec![],
                commands: vec![],
            })
            .expect("the live ask");
    }

    /// **A delivery that always lands** — the child is live, so the complaints go.
    fn delivers(
        into: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    ) -> impl Fn(&str, &str) -> Result<String, String> {
        move |to: &str, text: &str| {
            into.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((to.to_string(), text.to_string()));
            Ok("the message started a turn in that session".into())
        }
    }

    /// **A head that cannot be reached** — the `ask` seam for the doors that are not about it.
    fn no_head(_: &Question) -> Result<(QuestionAnswer, String), AskError> {
        Err(AskError::NoHead("no head is attached".into()))
    }

    /// **The door mapping IS the gatekeeper's closed set** — three words, three doors, and a word
    /// outside the set opens none.
    ///
    /// The last clause is the one worth pinning: `review_gate` reads an unknown word as a refusal,
    /// and a door for it would be this module choosing a door for a word nobody can interpret.
    #[test]
    fn the_three_doors_are_the_gatekeepers_three_words() {
        assert_eq!(Door::of("accept"), Some(Door::Accept));
        assert_eq!(Door::of("reject"), Some(Door::Refuse));
        assert_eq!(Door::of("needs_human"), Some(Door::CannotDecide));
        assert_eq!(Door::of("probably"), None, "a word outside the set");
        assert_eq!(Door::of("Accept"), None, "the words are exact");
        assert_eq!(Door::of(""), None);
        // And each door names its own word back, so a sentence about a door cannot drift from the
        // word that opened it.
        for word in Decision::ALL {
            let door = Door::of(word.as_str()).expect("a closed-set word is a door");
            assert_eq!(door.as_str(), word.as_str());
        }
    }

    /// **`accept` hands the entry to the merge queue** — through the same path `/queue approve`
    /// takes, measured by the QUEUE's own two predicates rather than by this module's word for it.
    ///
    /// The entry starts parked, so the door has something to move; the review row is the live ask,
    /// so the person's guard would refuse it and only the shared PATH can pass.
    #[test]
    fn the_accept_door_hands_the_entry_to_the_queue() {
        let (root, db, id) = parked("door-accept", MergeState::Failed);
        asking(&db, &id);
        let events = RecordingEvents::default();

        let done = report_verdict(
            &store_at(&db),
            &id,
            &Reported {
                decision: Decision::Accept,
                reasons: vec!["the branch does what the brief asked".into()],
                files: vec!["crates/widget.rs".into()],
                commands: vec!["git diff base...branch".into()],
            },
            crate::sessions::gatekeeper::SESSION,
            5_000,
            &|_, _| Ok(String::new()),
            &no_head,
            &events.sink(),
        )
        .expect("the accept");

        let Done::Queued(said) = &done else {
            panic!("an accept goes to the queue: {done:?}")
        };
        assert!(said.contains("accepted `b-m-1`"), "{said}");
        assert!(
            said.contains("the gate is not skipped"),
            "an accept hands the entry over; it never skips the landing: {said}"
        );

        // **The entry is in the queue** — back to `waiting`, and the queue's OWN answer to *what
        // may land* is the one that says so.
        let store = store_at(&db);
        let back = store.merge_entry(&id).unwrap().expect("the entry");
        assert_eq!(back.state, MergeState::Waiting, "handed to the queue");
        assert!(
            back.evidence.starts_with("accepted by the reviewer"),
            "the row says who handed it over, and it is not the operator: {back:?}"
        );
        assert!(
            back.evidence
                .contains("the branch does what the brief asked"),
            "and it carries the reviewer's reasons, which the accepting row cannot: {back:?}"
        );
        assert!(
            !back.evidence.contains("the operator"),
            "no operator in a reviewer's row: {back:?}"
        );
        let ruled = store.merge_review(&id).unwrap().expect("the verdict");
        assert_eq!(ruled.decision.as_deref(), Some("accept"));
        assert_eq!(
            ruled.session_id,
            crate::sessions::gatekeeper::SESSION,
            "the decider on the row is the session the verdict came from"
        );
        assert_eq!(ruled.asked_ms, 1, "the ask's own stamp is not restamped");
        let entries = store.merge_entries().unwrap();
        assert_eq!(
            next_takeable(&entries, &store.reviews().unwrap()),
            Some(0),
            "the queue may take it now, which is what *handed to the merge queue* means"
        );
        assert_eq!(events.moves().len(), 1, "every head is told it moved");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A session tree over the store** — a root and one child under it, which is the shape an
    /// entry's `session_id` names (`HarnessTaskRunner::enqueue_entry` calls `entry_for_finished`
    /// with the child's handle twice, and the child's own row names its parent).
    ///
    /// The walk is the point: an entry written as its own root would pass whether
    /// [`root_session`] worked or not, so every refusal fixture below has the two rows.
    fn a_child_of(db: &Path, parent: &str, child: &str) {
        let store = store_at(db);
        for (id, up) in [(parent, None), (child, Some(parent))] {
            store
                .put_session(&letibot_tokencore::store::SessionRecord {
                    id: id.to_string(),
                    title: None,
                    model_id: String::new(),
                    dialect_sha: String::new(),
                    workspace_root: String::new(),
                    owner: String::new(),
                    approvers: Vec::new(),
                    role: None,
                    parent_session_id: up.map(str::to_string),
                })
                .expect("the session row");
        }
    }

    /// **An entry whose `session_id` is a child under `parent`** — the entry `parked` wrote, with
    /// its `session_id` pointed at the child the tree really has.
    fn an_entry_of(db: &Path, id: &str, child: &str) -> MergeEntry {
        let mut e = store_at(db).merge_entry(id).unwrap().expect("the entry");
        e.session_id = child.to_string();
        enqueue(db, &e);
        e
    }

    /// **`reject` sends the reviewer's complaints to the CHILD that wrote the branch**, as a
    /// message to that live session — and the entry does not land on it.
    ///
    /// **The recipient is the entry's `session_id`, the column that documents itself as the subagent
    /// that finished the work** — not the review row's `session_id`, which is the session that
    /// HOSTED the review: sending there would be the reviewer being handed its own verdict. And not
    /// the root either: the child is the first party, and a child that is there is the one that has
    /// the worktree.
    #[test]
    fn the_refuse_door_sends_the_complaints_to_the_child_that_wrote_the_branch() {
        let (root, db, id) = parked("door-refuse", MergeState::Waiting);
        let child = "s-child-of-door-refuse";
        a_child_of(&db, "s-root-of-door-refuse", child);
        an_entry_of(&db, &id, child);
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));

        let done = report_verdict(
            &store_at(&db),
            &id,
            &Reported {
                decision: Decision::Reject,
                reasons: vec!["the retry is not bounded".into()],
                files: vec!["crates/widget.rs".into()],
                commands: vec!["git diff base...branch".into()],
            },
            crate::sessions::gatekeeper::SESSION,
            5_000,
            &delivers(sent.clone()),
            &no_head,
            &quiet_events(),
        )
        .expect("the refusal");

        let Done::Refused {
            child: named,
            reached,
            said,
        } = &done
        else {
            panic!("a reject is a refusal: {done:?}")
        };
        assert_eq!(named, child, "a refusal names the child whose work it is");
        assert_eq!(
            reached.as_ref().map(|to| (to.who, to.session.as_str())),
            Some((Reaches::Child, child)),
            "the child is live, so the complaints went to it: {said}"
        );
        let went = sent.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(went.len(), 1, "one message, to one child: {went:?}");
        assert_eq!(went[0].0, child, "and to the child, not to the reviewer");
        assert!(
            went[0].1.contains("the retry is not bounded"),
            "the complaints are the reviewer's own reasons: {}",
            went[0].1
        );
        assert!(
            went[0].1.contains("b-m-1") && went[0].1.contains("was taken out of the merge queue"),
            "and they name the branch and what happens next: {}",
            went[0].1
        );

        // **The verdict is on the row, so the queue's own gate refuses it** — the door moved the
        // VERDICT, and the queue's own pass is what parks the entry on it.
        let store = store_at(&db);
        let ruled = store.merge_review(&id).unwrap().expect("the verdict");
        assert_eq!(ruled.decision.as_deref(), Some("reject"));
        assert_eq!(ruled.reasons, vec!["the retry is not bounded"]);
        let back = store.merge_entry(&id).unwrap().expect("the entry");
        assert!(
            matches!(review_gate(&back, Some(&ruled)), ReviewGate::Refused(_)),
            "the queue refuses it on the row the door wrote"
        );
        let entries = store.merge_entries().unwrap();
        assert_eq!(
            next_takeable(&entries, &store.reviews().unwrap()),
            None,
            "a refusal does not land it"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **When the child's session is gone the complaints reach the ROOT that enqueued it** — and the
    /// sentence says WHICH of the two took them.
    ///
    /// This is the measured defect the whole change is against: a subagent that finished is a
    /// session that is gone, and a refusal that stopped at it left the entry parked while the
    /// session that enqueued the work carried on as if nothing had happened. The root is the model
    /// that holds the verbs, so the door asks it second — with the child's own failure quoted, and
    /// with the reviewer's reasons whole.
    #[test]
    fn a_refusal_reaches_the_root_that_enqueued_it_when_the_child_is_gone() {
        let (root, db, id) = parked("door-root", MergeState::Waiting);
        let child = "s-child-of-door-root";
        let parent = "s-root-of-door-root";
        a_child_of(&db, parent, child);
        an_entry_of(&db, &id, child);
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = sent.clone();

        let done = report_verdict(
            &store_at(&db),
            &id,
            &Reported {
                decision: Decision::Reject,
                reasons: vec!["the retry is not bounded".into()],
                files: vec![],
                commands: vec![],
            },
            crate::sessions::gatekeeper::SESSION,
            5_000,
            // **The child is gone; the root is live.** The door's own answer decides which is
            // which, so this stub answers by the session it was handed rather than by a flag.
            &move |to: &str, text: &str| {
                if to == child {
                    return Err(format!("no session `{to}` is live in this daemon"));
                }
                seen.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((to.to_string(), text.to_string()));
                Ok("the message started a turn in that session".into())
            },
            &no_head,
            &quiet_events(),
        )
        .expect("the refusal");

        let Done::Refused {
            child: named,
            reached,
            said,
        } = &done
        else {
            panic!("a reject is a refusal: {done:?}")
        };
        assert_eq!(
            named, child,
            "the child is named whether or not it was reached"
        );
        assert_eq!(
            reached.as_ref().map(|to| (to.who, to.session.as_str())),
            Some((Reaches::Root, parent)),
            "the root took them, and the report says so: {said}"
        );
        // **The sentence says WHICH one it reached**, which is the whole reason the two are
        // distinguished: a report that named only the child would be wrong about who has the ball.
        assert!(said.contains(child), "the child it could not reach: {said}");
        assert!(
            said.contains(parent),
            "and the session that took them: {said}"
        );
        assert!(
            said.contains("could not be told")
                && said.contains("no session `s-child-of-door-root`"),
            "with the door's own reason for the miss, quoted: {said}"
        );
        assert!(
            said.contains("root session that enqueued it"),
            "and with the party named: {said}"
        );

        // **And the root was sent the SECOND sentence, not the child's.** *Your branch is back with
        // you* is false of the root — it did not write the branch — so the message it gets is the
        // one that says a child it started was refused and that the entry is its to move.
        let went = sent.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(
            went.len(),
            1,
            "the child was tried and failed, so one message: {went:?}"
        );
        assert_eq!(went[0].0, parent);
        for asked in [
            "a child you started was taken out of the merge queue",
            "m-1",
            "b-m-1",
            "the retry is not bounded",
            "/queue restart",
            "/queue approve m-1",
            "/queue rm m-1",
        ] {
            assert!(
                went[0].1.contains(asked),
                "`{asked}` is missing from: {}",
                went[0].1
            );
        }
        assert!(
            !went[0].1.contains("Your branch"),
            "the root is not told its own branch came back: {}",
            went[0].1
        );

        // **And the refusal stands** — a delivery that moved to the second party does not undo it.
        let store = store_at(&db);
        let ruled = store.merge_review(&id).unwrap().expect("the verdict");
        assert_eq!(ruled.decision.as_deref(), Some("reject"));
        let entries = store.merge_entries().unwrap();
        assert_eq!(next_takeable(&entries, &store.reviews().unwrap()), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A refusal nobody can take keeps the complaints somewhere and says where** — and the
    /// sentence is TRUE: it quotes the two attempts rather than asserting why they failed.
    ///
    /// This is a REPORT rather than an `Err`: the verdict is already written and the entry does not
    /// land either way, so what a person needs is the three places the complaints ARE. The two
    /// failures are deliberately DIFFERENT KINDS — a session that is not live, and a daemon whose
    /// bell is closed — because the sentence this replaced said *that session is gone* about every
    /// failure there is, and a fallback that says a false thing about WHY is worse than one that
    /// says only what happened.
    #[test]
    fn a_refusal_nobody_can_take_says_where_the_complaints_are_and_why() {
        let (root, db, id) = parked("door-gone", MergeState::Waiting);
        let child = "s-child-of-door-gone";
        let parent = "s-root-of-door-gone";
        a_child_of(&db, parent, child);
        an_entry_of(&db, &id, child);

        let done = report_verdict(
            &store_at(&db),
            &id,
            &Reported {
                decision: Decision::Reject,
                reasons: vec!["the test it added does not assert anything".into()],
                files: vec![],
                commands: vec![],
            },
            crate::sessions::gatekeeper::SESSION,
            5_000,
            &|to: &str, _: &str| {
                if to == child {
                    Err(format!("no session `{to}` is live in this daemon"))
                } else {
                    Err("the daemon's bell is closed (it is shutting down)".into())
                }
            },
            &no_head,
            &quiet_events(),
        )
        .expect("the refusal is still a report and not a failure");

        let Done::Refused {
            child: named,
            reached,
            said,
        } = &done
        else {
            panic!("a reject is a refusal: {done:?}")
        };
        assert_eq!(named, child, "the child is named even though it is gone");
        assert!(reached.is_none(), "nobody took them: {said}");
        for asked in [
            "The complaints did NOT reach a live session",
            // **Both parties, in order, each in its own name** — so a reader can tell WHICH one was
            // which rather than being handed two anonymous failures.
            "`s-child-of-door-gone`, the child that wrote the branch",
            "`s-root-of-door-gone`, the root session that enqueued it",
            // **And each attempt's own words, quoted.**
            "no session `s-child-of-door-gone` is live in this daemon",
            "the daemon's bell is closed (it is shutting down)",
            "the verdict stands",
            "the review row of `m-1`",
            "on the entry's own evidence",
            "/queue restart",
            "/queue approve m-1",
            "/queue rm m-1",
        ] {
            assert!(said.contains(asked), "`{asked}` is missing from: {said}");
        }
        // **The sentence does not invent a reason.** The old one asserted *that session is gone*
        // about every failure there is, which is false of a closed bell — and of an entry that left
        // the queue mid-write, which is the other way this path is reached.
        assert!(
            !said.contains("That session is gone") && !said.contains("is gone"),
            "the fallback must quote the attempts rather than claim why they failed: {said}"
        );

        // **And the refusal stands** — a message that did not go does not undo a judgement.
        let store = store_at(&db);
        let ruled = store.merge_review(&id).unwrap().expect("the verdict");
        assert_eq!(ruled.decision.as_deref(), Some("reject"));
        assert_eq!(
            ruled.reasons,
            vec!["the test it added does not assert anything"]
        );
        let entries = store.merge_entries().unwrap();
        assert_eq!(next_takeable(&entries, &store.reviews().unwrap()), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **`needs_human` raises a question at the head** — and the entry does not land on an
    /// abstention, whether or not anybody answered.
    ///
    /// The two halves are one test because they are one door: a question nobody could take is still
    /// a question RAISED, which is the door's act, and *parked in silence* is the outcome neither
    /// half may produce.
    #[test]
    fn a_cannot_decide_raises_a_question_rather_than_parking_in_silence() {
        let (root, db, id) = parked("door-human", MergeState::Waiting);
        let asked: Arc<std::sync::Mutex<Vec<Question>>> = Default::default();
        let seen = asked.clone();

        let done = report_verdict(
            &store_at(&db),
            &id,
            &Reported {
                decision: Decision::NeedsHuman,
                reasons: vec!["the spec is ambiguous about the retry".into()],
                files: vec!["docs/spec.md".into()],
                commands: vec![],
            },
            crate::sessions::gatekeeper::SESSION,
            5_000,
            &|_, _| Ok(String::new()),
            &move |q: &Question| {
                seen.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(q.clone());
                Ok((QuestionAnswer::choosing(0), "deadtrickster".into()))
            },
            &quiet_events(),
        )
        .expect("the question");

        let Done::Asked {
            question,
            answered_by,
            said,
        } = &done
        else {
            panic!("needs_human raises a question: {done:?}")
        };
        assert_eq!(answered_by.as_deref(), Some("deadtrickster"));
        assert!(
            question.text.contains("b-m-1") && question.text.contains("m-1"),
            "the question names the branch and the entry: {question:?}"
        );
        assert!(
            question
                .because
                .contains("the spec is ambiguous about the retry"),
            "and carries the reviewer's own why: {question:?}"
        );
        assert_eq!(
            question.options.len(),
            3,
            "the three moves a person has: {question:?}"
        );
        assert!(said.contains("does not land on it"), "{said}");
        assert_eq!(
            asked.lock().unwrap_or_else(|e| e.into_inner()).len(),
            1,
            "the head was asked exactly once"
        );

        // **And the row the queue reads says the entry does not land on it.**
        let store = store_at(&db);
        let ruled = store.merge_review(&id).unwrap().expect("the verdict");
        assert_eq!(ruled.decision.as_deref(), Some("needs_human"));
        let back = store.merge_entry(&id).unwrap().expect("the entry");
        assert!(matches!(
            review_gate(&back, Some(&ruled)),
            ReviewGate::Refused(_)
        ));
        let entries = store.merge_entries().unwrap();
        assert_eq!(next_takeable(&entries, &store.reviews().unwrap()), None);

        // **And a question nobody can take is still raised and still said.** A door that went quiet
        // here would be the *parked in silence* this test is named against.
        let (root2, db2, id2) = parked("door-human-none", MergeState::Waiting);
        let done = report_verdict(
            &store_at(&db2),
            &id2,
            &Reported {
                decision: Decision::NeedsHuman,
                reasons: vec![],
                files: vec![],
                commands: vec![],
            },
            crate::sessions::gatekeeper::SESSION,
            5_000,
            &|_, _| Ok(String::new()),
            &no_head,
            &quiet_events(),
        )
        .expect("the question, raised and unanswered");
        let Done::Asked {
            answered_by, said, ..
        } = &done
        else {
            panic!("needs_human raises a question: {done:?}")
        };
        assert_eq!(*answered_by, None, "nobody could take it");
        assert!(
            said.contains("The question WAS raised at the head")
                && said.contains("no head is attached"),
            "and the door says so rather than going quiet: {said}"
        );
        assert!(
            said.contains("is not parked in silence"),
            "which is the one thing this door must not do: {said}"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root2);
    }
}
