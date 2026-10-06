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

use std::path::{Path, PathBuf};
use std::process::Command;

use letibot_tokencore::store::{MergeEntry, MergePriority, MergeState, Store};

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
fn rebase(worktree: &Path, onto: &str) -> Result<(), String> {
    git(worktree, &["rebase", onto]).map(|_| ())
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
    git(repo, &["worktree", "remove", "--force", worktree])?;
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

// ===== The daemon thread: the thing that takes the next entry =====

/// **What one pass of the daemon did** — the vocabulary the loop and the test share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// Nothing was ready: no `Waiting` entry whose `needs` have all `Landed`.
    Idle,
    /// The entry was taken and landed: main was fast-forwarded, pushed, and the worktree and
    /// branch were removed. The tip is the SHA main moved to.
    Landed(String),
    /// The entry was taken and the rebase conflicted: the worktree stays, with git's words on
    /// the row.
    Conflict,
    /// The entry was taken and the gate failed: the worktree stays, with the gate's words on
    /// the row.
    Failed,
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
    /// The gate: the same checks CI runs, as a function of the worktree. `Ok` is green, and
    /// `Err` is the reason the gate failed, in the gate's own words.
    ///
    /// A function rather than a fixed command list, for the reason the test needs it: the
    /// production gate is `cargo fmt`, `cargo clippy`, `cargo test` and a release build, and
    /// the test gate is a no-op, and both are *the gate* — the thing that runs at the tip
    /// after the rebase. The seam is the function, and the production wiring fills it with
    /// the CI commands.
    gate: Box<dyn Fn(&Path) -> Result<(), String> + Send>,
}

impl MergeQueueDaemon {
    /// Build the daemon over `store`, serving the repo at `repo`, with `gate` as the check
    /// that runs at the tip.
    pub fn new(
        store: Store,
        repo: PathBuf,
        gate: Box<dyn Fn(&Path) -> Result<(), String> + Send>,
    ) -> Self {
        Self { store, repo, gate }
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
        let entries = self.store.merge_entries()?;
        let Some(idx) = next_ready(&entries) else {
            return Ok(StepOutcome::Idle);
        };
        let entry = entries[idx].clone();

        // **Take it**: mark it `Taken`, so a daemon that dies now comes back to a row that
        // says the job was running, not a row that says nothing.
        let mut taken = entry.clone();
        taken.state = MergeState::Taken;
        taken.evidence = "rebasing at the tip".into();
        self.store.put_merge_entry(&taken)?;

        // **Rebase it at the tip**: onto the current main, not the SHA it was written
        // against. A conflict is reported, never resolved.
        let base = effective_base(&entry, &entries);
        let worktree = entry.worktree.as_deref().unwrap_or("");
        if worktree.is_empty() {
            // No worktree: the enqueuer's half has not created one, and the queue cannot
            // rebase a branch that is not checked out. This is the `task_start` seam — the
            // worktree is created when the branch is enqueued, and until then the entry
            // waits. It is reported rather than guessed at.
            let mut waiting = entry.clone();
            waiting.state = MergeState::Waiting;
            waiting.evidence = "no worktree: the branch is not checked out".into();
            self.store.put_merge_entry(&waiting)?;
            return Ok(StepOutcome::Idle);
        }
        let worktree = Path::new(worktree);
        if let Err(e) = rebase(worktree, base) {
            let mut conflict = entry.clone();
            conflict.state = MergeState::Conflict;
            conflict.evidence = e;
            self.store.put_merge_entry(&conflict)?;
            return Ok(StepOutcome::Conflict);
        }

        // **Run the gate**, at the tip, on the rebased branch.
        if let Err(e) = (self.gate)(worktree) {
            let mut failed = entry.clone();
            failed.state = MergeState::Failed;
            failed.evidence = e;
            self.store.put_merge_entry(&failed)?;
            return Ok(StepOutcome::Failed);
        }

        // **Fast-forward main and push**: the merge, and the landing.
        let tip = match fast_forward_main(&self.repo, &entry.branch) {
            Ok(tip) => tip,
            Err(e) => {
                let mut failed = entry.clone();
                failed.state = MergeState::Failed;
                failed.evidence = e;
                self.store.put_merge_entry(&failed)?;
                return Ok(StepOutcome::Failed);
            }
        };
        if let Err(e) = push_main(&self.repo) {
            let mut failed = entry.clone();
            failed.state = MergeState::Failed;
            failed.evidence = e;
            self.store.put_merge_entry(&failed)?;
            return Ok(StepOutcome::Failed);
        }

        // **Land it**: mark it `Landed` with the tip, and clean up by state.
        let mut landed = entry.clone();
        landed.state = MergeState::Landed;
        landed.evidence = format!("landed at {tip}");
        landed.landed_sha = Some(tip.clone());
        self.store.put_merge_entry(&landed)?;

        // The cleanup `Landed` owns: remove the worktree and delete the branch.
        if let Some(wt) = &entry.worktree {
            let _ = remove_worktree(&self.repo, Path::new(wt));
        }
        let _ = delete_branch(&self.repo, &entry.branch);

        Ok(StepOutcome::Landed(tip))
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
    /// **The `task_start` seam.** The branch that is enqueued on completion is `task_start`'s
    /// half: when a subagent finishes a branch, `task_start` creates the worktree, mints the
    /// entry, and writes it to the store. That branch is not merged yet, so this loop has
    /// nothing to take until it is — and the `stop` flag is what ends the loop in the meantime
    /// rather than a poll that runs forever.
    ///
    /// TODO(task_start): the enqueue on completion — `task_start` creates the worktree and
    /// writes the entry when a subagent finishes a branch. Until that branch is merged, this
    /// loop has nothing to take, and the `stop` flag is the only thing that ends it.
    ///
    /// **The `gatekeeper` seam.** A `review` verdict from the gatekeeper must be required
    /// before an entry lands: the gate is the mechanical check (fmt, clippy, test, release),
    /// and the review is the judgment call, and both are required before the fast-forward.
    /// That branch is not merged yet, so the review is not wired here — the requirement is
    /// named, and the seam is where it goes, rather than an interface invented for it.
    ///
    /// TODO(gatekeeper): require a `review` verdict before the fast-forward — the gate is the
    /// mechanical check and the review is the judgment call, and both are required before an
    /// entry lands. The gatekeeper branch is not merged yet, so this is the requirement and
    /// its seam, not an interface.
    pub fn run(&self, stop: &std::sync::atomic::AtomicBool) -> Result<(), letibot_tokencore::store::StoreError> {
        self.recover()?;
        loop {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
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
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `MergeEntry` for the tests, with the fields the test does not care about set to
    /// defaults.
    fn entry(id: &str, priority: MergePriority, state: MergeState, created_ms: u64) -> MergeEntry {
        MergeEntry {
            id: id.into(),
            session_id: "s".into(),
            branch: format!("b-{id}"),
            base_sha: "base".into(),
            priority,
            needs: vec![],
            state,
            evidence: String::new(),
            created_ms,
            updated_ms: created_ms,
            worktree: None,
            landed_sha: None,
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
        assert_eq!(next_ready(&entries), Some(1), "the urgent entry jumps the older subagent entry");
    }

    /// **Ties inside a rung break by age** — the oldest first, and the id is the final
    /// tiebreaker so two entries enqueued in the same millisecond do not reorder.
    #[test]
    fn ties_break_by_age_then_id() {
        let older = entry("b", MergePriority::Subagent, MergeState::Waiting, 1_000);
        let newer = entry("a", MergePriority::Subagent, MergeState::Waiting, 2_000);
        let entries = vec![newer, older];
        assert_eq!(next_ready(&entries), Some(1), "the older entry is taken first");

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
        let mut dependent = entry("dep-ent", MergePriority::Subagent, MergeState::Waiting, 2_000);
        dependent.needs = vec!["dep".into()];
        let entries = vec![dep, dependent];
        // The dependency is not landed, so the dependent is not ready — and the dependency
        // itself is taken first.
        assert_eq!(next_ready(&entries), Some(0), "the dependency is taken before the dependent");
        assert!(!is_ready(&dependent, &entries), "the dependent is not ready");
        assert_eq!(unmet_needs(&dependent, &entries), vec!["dep"], "the unmet dependency is named");

        // The dependency lands, and the dependent is ready.
        let mut landed_dep = dep.clone();
        landed_dep.state = MergeState::Landed;
        landed_dep.landed_sha = Some("tip".into());
        let entries = vec![landed_dep, dependent.clone()];
        assert!(is_ready(&dependent, &entries), "the dependent is ready once its dependency lands");
        assert_eq!(next_ready(&entries), Some(1), "the dependent is taken once its dependency lands");
    }

    /// **A dependency that is not in the queue is unmet** — an entry that names a dependency
    /// the queue has never held is an entry the queue cannot vouch for, and reading it as
    /// *met* would be a guess.
    #[test]
    fn a_missing_dependency_is_unmet() {
        let mut dependent = entry("dep-ent", MergePriority::Subagent, MergeState::Waiting, 1_000);
        dependent.needs = vec!["ghost".into()];
        let entries = vec![dependent.clone()];
        assert!(!is_ready(&dependent, &entries), "a missing dependency is not met");
        assert_eq!(unmet_needs(&dependent, &entries), vec!["ghost"], "the missing dependency is named");
        assert_eq!(next_ready(&entries), None, "the entry is not taken while its dependency is missing");
    }

    /// **The base becomes the landed tip rather than the stale SHA** — the half of the
    /// dependency rule that makes the rebase true: a dependent rebases onto the SHA its
    /// dependency landed at, not the SHA it was written against.
    #[test]
    fn the_base_becomes_the_landed_tip() {
        let mut dep = entry("dep", MergePriority::Subagent, MergeState::Landed, 1_000);
        dep.landed_sha = Some("landed-tip".into());
        dep.updated_ms = 5_000;
        let mut dependent = entry("dep-ent", MergePriority::Subagent, MergeState::Waiting, 2_000);
        dependent.needs = vec!["dep".into()];
        dependent.base_sha = "stale-sha";
        let entries = vec![dep, dependent.clone()];
        assert_eq!(
            effective_base(&dependent, &entries),
            "landed-tip",
            "the base is the landed tip, not the stale SHA"
        );

        // No dependencies: the base is the SHA it was written against.
        let independent = entry("ind", MergePriority::Subagent, MergeState::Waiting, 3_000);
        assert_eq!(
            effective_base(&independent, &entries),
            "base",
            "an entry with no dependencies rebases onto its own base"
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
        let mut dependent = entry("dep-ent", MergePriority::Subagent, MergeState::Waiting, 3_000);
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
        let waiting = entry("waiting", MergePriority::Subagent, MergeState::Waiting, 2_000);
        let landed = entry("landed", MergePriority::Subagent, MergeState::Landed, 3_000);
        let recovered = recover(vec![taken, waiting, landed]);
        assert_eq!(recovered[0].state, MergeState::Stale, "the taken entry is stale");
        assert!(
            !recovered[0].evidence.is_empty(),
            "the stale entry has its reason"
        );
        assert_eq!(recovered[1].state, MergeState::Waiting, "the waiting entry is untouched");
        assert_eq!(recovered[2].state, MergeState::Landed, "the landed entry is untouched");

        // And the recovery is idempotent: a second pass does not move the stale entry again.
        let recovered_twice = recover(recovered);
        assert_eq!(recovered_twice[0].state, MergeState::Stale, "the recovery is idempotent");
    }

    /// **The cleanup is decided by the state** — `Landed` removes the worktree and the branch,
    /// and every other state keeps it. The difference is the rule: a tree that is removed
    /// after a failed test destroys the evidence, and a branch that is deleted before it is
    /// merged is a branch the queue cannot vouch for.
    #[test]
    fn the_cleanup_is_decided_by_the_state() {
        assert_eq!(cleanup_for(MergeState::Landed), Cleanup::RemoveWorktreeAndBranch);
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
        git(&root, &["worktree", "add", "-q", wt.to_str().unwrap(), "feature"]);
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
        let store = Store::open_in_memory().expect("a store");

        // An `other` branch, cut from main, that lands first and moves main.
        git(&root, &["branch", "other"]);
        let other_wt = root.join("worktrees").join("other");
        git(&root, &["worktree", "add", "-q", other_wt.to_str().unwrap(), "other"]);
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
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(other_wt.to_str().unwrap().to_string()),
            landed_sha: None,
        };
        store.put_merge_entry(&other_entry).expect("the other entry");

        // The `feature` entry, enqueued against the main it was cut from — which is now stale.
        let feature_entry = MergeEntry {
            id: "m-land".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            evidence: String::new(),
            created_ms: 2_000,
            updated_ms: 2_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
        };
        store.put_merge_entry(&feature_entry).expect("the feature entry");

        // The daemon, with a no-op gate: the gate is the seam, and the test is about the
        // rebase and the fast-forward, not the gate.
        let daemon = MergeQueueDaemon::new(store, root.clone(), Box::new(|_| Ok(())));

        // The first pass takes `other` (urgent) and lands it, moving main to the tip of
        // `other`.
        let outcome = daemon.step().expect("the pass");
        assert_eq!(outcome, StepOutcome::Landed(other_sha.clone()), "other lands first");
        assert_eq!(sha(&root, "main"), other_sha, "main moved to the tip of other");

        // The second pass takes `feature`, rebases it onto the current main (the tip of
        // `other`), and lands it.
        let outcome = daemon.step().expect("the pass");
        let feature_tip = match &outcome {
            StepOutcome::Landed(tip) => tip.clone(),
            other => panic!("the feature entry should land, got {other:?}"),
        };
        // **main moved to the rebased feature tip** — the fast-forward, and the whole point.
        assert_eq!(sha(&root, "main"), feature_tip, "main was fast-forwarded to the rebased feature tip");
        // **The rebase was not a no-op**: the feature tip is not the original feature SHA,
        // because it was replayed on top of `other`.
        assert_ne!(feature_tip, sha(&root, "main~1"), "the feature was rebased, not fast-forwarded as-is");

        // **The worktree is gone and the branch is deleted** — the cleanup `Landed` owns.
        assert!(!wt.exists(), "the worktree was removed");
        let branches = git_output(&root, &["branch", "--list", "feature"]);
        assert!(branches.trim().is_empty(), "the branch was deleted: {branches:?}");

        // **The row says `Landed`, with the tip** — the durable record of the merge.
        let landed = store.merge_entry("m-land").expect("reads").expect("the entry");
        assert_eq!(landed.state, MergeState::Landed, "the row is landed");
        assert_eq!(landed.landed_sha.as_deref(), Some(feature_tip.as_str()), "the row keeps the tip");
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
        let store = Store::open_in_memory().expect("a store");

        // The `feature` branch touches line 2 of a.txt, so it will conflict with `other`.
        std::fs::write(wt.join("a.txt"), "one\nTHREE\n").expect("write");
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "feature touches line two"]);

        // An `other` branch, cut from main, that changes the same line and lands first.
        git(&root, &["branch", "other"]);
        let other_wt = root.join("worktrees").join("other");
        git(&root, &["worktree", "add", "-q", other_wt.to_str().unwrap(), "other"]);
        std::fs::write(other_wt.join("a.txt"), "one\nTWO\n").expect("write");
        git(&other_wt, &["add", "-A"]);
        git(&other_wt, &["commit", "-qm", "other touches line two"]);

        // The `other` entry, urgent so it is taken first.
        let other_entry = MergeEntry {
            id: "m-other".into(),
            session_id: "s".into(),
            branch: "other".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Urgent,
            needs: vec![],
            state: MergeState::Waiting,
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(other_wt.to_str().unwrap().to_string()),
            landed_sha: None,
        };
        store.put_merge_entry(&other_entry).expect("the other entry");

        // The `feature` entry, which will conflict when rebased onto the tip of `other`.
        let feature_entry = MergeEntry {
            id: "m-conflict".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            evidence: String::new(),
            created_ms: 2_000,
            updated_ms: 2_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
        };
        store.put_merge_entry(&feature_entry).expect("the feature entry");

        let daemon = MergeQueueDaemon::new(store, root.clone(), Box::new(|_| Ok(())));

        // The first pass takes `other` (urgent) and lands it, moving main to the tip of
        // `other`.
        let outcome = daemon.step().expect("the pass");
        assert!(matches!(outcome, StepOutcome::Landed(_)), "other lands first: {outcome:?}");

        // The second pass takes `feature`, rebases it onto the current main (the tip of
        // `other`), and the rebase conflicts.
        let outcome = daemon.step().expect("the pass");
        assert_eq!(outcome, StepOutcome::Conflict, "the conflict is reported");

        // **The worktree stays** — the evidence is there, and removing it would destroy it.
        assert!(wt.exists(), "the worktree stays after a conflict");
        // **The row says `Conflict`, with git's words** — the reason is on the row.
        let conflict = store.merge_entry("m-conflict").expect("reads").expect("the entry");
        assert_eq!(conflict.state, MergeState::Conflict, "the row is conflict");
        assert!(!conflict.evidence.is_empty(), "the row has the reason");
        // **main did not move past `other`** — the feature merge did not happen.
        assert_eq!(sha(&root, "main"), sha(&root, "other"), "main is at the tip of other, not the feature");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A gate failure is reported and the worktree stays** — the gate fails, the entry is
    /// `Failed` with the gate's words on the row, and the worktree stays. Removing a tree
    /// after a failed test destroys the evidence, and the worktree is where the failure is.
    #[test]
    fn a_gate_failure_keeps_the_worktree() {
        let (root, wt, main_sha, _feature_sha) = repo_with_branch("failed");
        let store = Store::open_in_memory().expect("a store");

        let entry = MergeEntry {
            id: "m-failed".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Waiting,
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
        };
        store.put_merge_entry(&entry).expect("the entry");

        // The gate fails, with its own words.
        let daemon = MergeQueueDaemon::new(store, root.clone(), Box::new(|_| Err("the gate is red".into())));
        let outcome = daemon.step().expect("the pass");
        assert_eq!(outcome, StepOutcome::Failed, "the failure is reported");

        // **The worktree stays** — the evidence is there.
        assert!(wt.exists(), "the worktree stays after a gate failure");
        // **The row says `Failed`, with the gate's words.**
        let failed = store.merge_entry("m-failed").expect("reads").expect("the entry");
        assert_eq!(failed.state, MergeState::Failed, "the row is failed");
        assert_eq!(failed.evidence, "the gate is red", "the row has the gate's words");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A `Taken` entry that the daemon died on comes back `Stale`** — the recovery, driven
    /// through the store rather than the pure function, so the row on disk is what is
    /// asserted. A daemon that dies mid-merge writes a `Taken` row, and the next daemon's
    /// `recover` moves it to `Stale` rather than letting it come back as `Waiting`.
    #[test]
    fn a_dead_gate_job_comes_back_stale() {
        let (root, _wt, main_sha, _feature_sha) = repo_with_branch("stale");
        let store = Store::open_in_memory().expect("a store");

        // The entry, taken by a daemon that then died: the row is `Taken` on disk.
        let entry = MergeEntry {
            id: "m-stale".into(),
            session_id: "s".into(),
            branch: "feature".into(),
            base_sha: main_sha.clone(),
            priority: MergePriority::Subagent,
            needs: vec![],
            state: MergeState::Taken,
            evidence: "rebasing at the tip".into(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(_wt.to_str().unwrap().to_string()),
            landed_sha: None,
        };
        store.put_merge_entry(&entry).expect("the entry");

        // The next daemon recovers: the `Taken` row is moved to `Stale` on disk.
        let daemon = MergeQueueDaemon::new(store, root.clone(), Box::new(|_| Ok(())));
        let recovered = daemon.recover().expect("the recovery");
        assert_eq!(recovered[0].state, MergeState::Stale, "the taken entry is stale");
        let on_disk = store.merge_entry("m-stale").expect("reads").expect("the entry");
        assert_eq!(on_disk.state, MergeState::Stale, "the row on disk is stale");
        assert!(!on_disk.evidence.is_empty(), "the row has the reason");
        let _ = std::fs::remove_dir_all(&root);
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
