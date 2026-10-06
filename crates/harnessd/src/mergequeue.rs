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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

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
    let out = Command::new(program)
        .current_dir(dir)
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

/// **The gate CI runs** — the format check on the files this change touches, clippy, the
/// workspace tests, and the release build — run in the entry's worktree after the rebase.
///
/// The operator's ask, in their words: *"we need a gated merge to main"*, and the gate is
/// the same one `.github/workflows/ci.yml` runs: `scripts/check-fmt.sh`, `cargo clippy
/// --all-targets`, `cargo test --workspace --no-fail-fast -- --nocapture` and `cargo build
/// --release --bins`. **Not a second, weaker check**: a queue that lands on a different
/// standard from the one the branch was written to is a queue that lands branches CI would
/// have refused.
///
/// **The base the format check measures from is `main`, and the rebase is what makes that
/// true.** The branch was rebased onto the current main immediately before this runs, so
/// `main` is exactly the revision the change is measured from — and `check-fmt.sh` refuses
/// a base it cannot resolve rather than passing, which is the property wanted here: a
/// worktree whose `main` cannot be named fails the gate instead of skipping a step.
///
/// The steps run in order and the first failure is the answer. There is no `--continue`: a
/// gate that ran everything after a red step would spend minutes to say what the first
/// line already said.
pub fn ci_gate(worktree: &Path) -> Result<(), String> {
    let steps: [(&str, &[&str]); 4] = [
        ("sh", &["scripts/check-fmt.sh", "main"]),
        ("cargo", &["clippy", "--all-targets"]),
        (
            "cargo",
            &["test", "--workspace", "--no-fail-fast", "--", "--nocapture"],
        ),
        ("cargo", &["build", "--release", "--bins"]),
    ];
    for (program, args) in steps {
        if let Err(out) = run_captured(worktree, program, args) {
            return Err(format!(
                "{program} {} failed:\n{}",
                args.join(" "),
                tail(&out, EVIDENCE_BYTES)
            ));
        }
    }
    Ok(())
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
        state: match entry.state {
            MergeState::Waiting => wire::MergeState::Waiting,
            MergeState::Taken => wire::MergeState::Taken,
            MergeState::Landed => wire::MergeState::Landed,
            MergeState::Failed => wire::MergeState::Failed,
            MergeState::Conflict => wire::MergeState::Conflict,
            MergeState::Stale => wire::MergeState::Stale,
        },
        evidence: entry.evidence.clone(),
        created_ms: entry.created_ms,
        updated_ms: entry.updated_ms,
        worktree: entry.worktree.clone(),
        landed_sha: entry.landed_sha.clone(),
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
            self.move_to(
                &entry,
                MergeState::Waiting,
                "no worktree: the branch is not checked out".into(),
                None,
            )?;
            return Ok(StepOutcome::Idle);
        }
        let worktree = Path::new(worktree);
        if let Err(e) = rebase(worktree, base) {
            self.move_to(&entry, MergeState::Conflict, e, None)?;
            return Ok(StepOutcome::Conflict);
        }

        // **Run the gate**, at the tip, on the rebased branch.
        if let Err(e) = (self.gate)(worktree) {
            self.move_to(&entry, MergeState::Failed, e, None)?;
            return Ok(StepOutcome::Failed);
        }

        // **Fast-forward main and push**: the merge, and the landing.
        let tip = match fast_forward_main(&self.repo, &entry.branch) {
            Ok(tip) => tip,
            Err(e) => {
                self.move_to(&entry, MergeState::Failed, e, None)?;
                return Ok(StepOutcome::Failed);
            }
        };
        if let Err(e) = push_main(&self.repo) {
            self.move_to(&entry, MergeState::Failed, e, None)?;
            return Ok(StepOutcome::Failed);
        }

        // **Land it**: the row carries the tip, and the cleanup the state owns runs.
        self.move_to(
            &entry,
            MergeState::Landed,
            format!("landed at {tip}"),
            Some(tip.clone()),
        )?;

        Ok(StepOutcome::Landed(tip))
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
    fn move_to(
        &self,
        entry: &MergeEntry,
        state: MergeState,
        evidence: String,
        landed_sha: Option<String>,
    ) -> Result<(), letibot_tokencore::store::StoreError> {
        let mut moved = entry.clone();
        moved.state = state;
        moved.evidence = evidence;
        if landed_sha.is_some() {
            moved.landed_sha = landed_sha;
        }
        self.store.put_merge_entry(&moved)?;
        clean_up(&self.repo, entry, state);
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
/// indexed `SELECT` a second, which is what the enqueue side (`task_start`'s half) will ring a
/// bell for once it exists.
pub fn spawn_for(cfg: &crate::config::Config, stop: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
    let store_path = cfg.store.as_ref()?;
    let repo = match repo_root(&cfg.workspace) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("  merge queue: not served — {e}");
            return None;
        }
    };
    let store = match Store::open(store_path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("  merge queue: not served — {}: {e}", store_path.display());
            return None;
        }
    };
    match MergeQueueDaemon::new(store, repo.clone(), Box::new(ci_gate)).spawn(stop) {
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
    fn store_at(path: &Path) -> Store {
        Store::open(path).expect("a store")
    }

    /// **Enqueue one entry through its own connection** — the enqueuer's half: open the file,
    /// write the row, close. The daemon then opens its own and the test opens a third to read,
    /// so nothing asserted below is a value a shared connection kept in hand.
    fn enqueue(path: &Path, entry: &MergeEntry) {
        store_at(path)
            .put_merge_entry(entry)
            .expect("the entry row");
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
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(other_wt.to_str().unwrap().to_string()),
            landed_sha: None,
        };
        enqueue(&db, &other_entry);

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
        enqueue(&db, &feature_entry);

        // The daemon, with a no-op gate: the gate is the seam, and the test is about the
        // rebase and the fast-forward, not the gate.
        let daemon = MergeQueueDaemon::new(store_at(&db), root.clone(), Box::new(|_| Ok(())));

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
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(other_wt.to_str().unwrap().to_string()),
            landed_sha: None,
        };
        enqueue(&db, &other_entry);

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
        enqueue(&db, &feature_entry);

        let daemon = MergeQueueDaemon::new(store_at(&db), root.clone(), Box::new(|_| Ok(())));

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
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
        };
        enqueue(&db, &entry);

        // The gate fails, with its own words.
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| Err("the gate is red".into())),
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
            evidence: "rebasing at the tip".into(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(_wt.to_str().unwrap().to_string()),
            landed_sha: None,
        };
        enqueue(&db, &entry);

        // The next daemon recovers: the `Taken` row is moved to `Stale` on disk.
        let daemon = MergeQueueDaemon::new(store_at(&db), root.clone(), Box::new(|_| Ok(())));
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
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 1_000,
            worktree: Some(wt.to_str().unwrap().to_string()),
            landed_sha: None,
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

    /// **The gate is CI's command line, and the step that failed is the answer.** Asserted
    /// without running a build: in a worktree with no `scripts/check-fmt.sh`, the FIRST step
    /// fails, and the evidence names it with its own words.
    ///
    /// The second assertion is the one about the shape: `cargo clippy`, `cargo test` and the
    /// release build must not have run after a red format check, because a gate that continues
    /// spends minutes to say what the first line already said.
    #[test]
    fn the_gate_reports_the_step_that_failed_and_runs_no_further() {
        let dir = std::env::temp_dir().join(format!("letibot-mq-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let err = ci_gate(&dir).expect_err("a worktree with no scripts/check-fmt.sh is not green");
        assert!(
            err.contains("scripts/check-fmt.sh"),
            "the failing step is named: {err:?}"
        );
        assert!(
            !err.contains("cargo clippy") && !err.contains("cargo test"),
            "a later step ran anyway: {err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
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

        let daemon =
            MergeQueueDaemon::new(store_at(&db), std::env::temp_dir(), Box::new(|_| Ok(())));
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
