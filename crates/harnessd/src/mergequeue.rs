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

/// **Run the gate** — `main`'s steps, in order, each through `sh -c` in the entry's worktree
/// after the rebase. The first that fails is the answer, with the tail of its output; there is
/// no `--continue`, because a gate that ran everything after a red step would spend minutes to
/// say what the first line already said.
pub fn repo_gate(worktree: &Path) -> Result<(), String> {
    let steps = gate_on_main(worktree)?.ok_or_else(|| {
        "there is no merge gate on main (a `Merge gate` section in AGENTS.md)".to_string()
    })?;
    for step in steps {
        if let Err(out) = run_captured(worktree, "sh", &["-c", &step]) {
            return Err(format!("`{step}` failed:\n{}", tail(&out, EVIDENCE_BYTES)));
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
    })
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
/// halves of the review: the queue's pass (which rings the host) and the host itself (which
/// starts the child).
///
/// One function rather than two rules, because the two halves are the same decision one hop
/// apart, and the failure mode of a second rule is the one this defect already produced: the
/// queue rings, the host spawns, and neither of them is the place the *bound* lives. It is a
/// pure function of the review row and the clock, so it is asserted without a daemon, a child
/// or a provider.
///
/// The three answers, and what each is for:
///
/// * **`Due`** — nobody has asked, or an attempt is IN FLIGHT. The second is not a retry and
///   must not be read as one: the queue's re-ask on every pass is the recovery for a daemon
///   that came up between the request row and the bell (`GatekeeperDoor::wake`), and it is
///   cheap by construction because the door writes the row only when there is not one and a
///   ring at a host with nothing new answers `Ok(None)`.
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
    if let Some(rec) = review {
        if rec.answered_ms.is_none() && rec.failed_ms.is_none() {
            return Err(format!(
                "a gatekeeper is working on `{}` right now — the review has been asked for and \
                 has neither answered nor failed. A second reviewer beside a live one would be a \
                 second writer on one entry's verdict, so nothing was restarted. Wait for it; if \
                 its attempt dies, the entry comes back here with the failure on its row.",
                entry.branch
            ));
        }
    }
    if !matches!(
        entry.state,
        MergeState::Failed | MergeState::Conflict | MergeState::Stale
    ) {
        return Err(format!(
            "`{}` is `{}` — nothing to restart. Only a parked entry (failed, conflict, stale) \
             is restarted by hand; a waiting one is what the queue's own retry is for.",
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
    let entry = store
        .merge_entry(entry_id)
        .map_err(|e| format!("the entry `{entry_id}` could not be read: {e}"))?
        .ok_or_else(|| {
            format!(
                "there is no entry `{entry_id}` in the queue — `/queue` lists what is there, and \
                 the pane's rows carry their ids."
            )
        })?;
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
        let now = store
            .merge_entry(entry_id)
            .ok()
            .flatten()
            .map(|e| e.state.as_str().to_string())
            .unwrap_or_else(|| "gone".into());
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
        gate: Box<dyn Fn(&Path) -> Result<(), String> + Send>,
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
    fn serves(&self, entry: &MergeEntry) -> bool {
        let Some(ws) = &self.serving else {
            return true;
        };
        let root = root_session(&self.store, &entry.session_id);
        match self.store.session(&root).ok().flatten() {
            Some(s) => {
                let at = Path::new(&s.workspace_root);
                std::fs::canonicalize(at).unwrap_or(at.to_path_buf()) == *ws
            }
            None => self.repo_for(entry).starts_with(ws),
        }
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
        for entry in &entries {
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
                                    self.move_to(entry, MergeState::Waiting, said, None)?;
                                }
                            }
                            Err(e) => {
                                // **A wake that failed is on the row.** An entry waiting for a
                                // verdict nobody was asked for waits for ever, and a queue that
                                // said nothing would be the same silence as an empty one.
                                self.move_to(
                                    entry,
                                    MergeState::Waiting,
                                    format!("the gatekeeper could not be asked: {e}"),
                                    None,
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
                            self.move_to(entry, MergeState::Waiting, said, None)?;
                        }
                    }
                    // **The attempts are spent: park it, with the failure's own words.** A
                    // reviewer that cannot be asked is not a refusal, so this is not
                    // [`Self::Refused`] — but it is the same shape of stopping, and the worktree
                    // stays for the same reason: the tree is where the reason is.
                    ReviewRetry::Exhausted => {
                        let said = review_exhausted_evidence(entry, review);
                        self.move_to(entry, MergeState::Failed, said, None)?;
                        return Ok(StepOutcome::ReviewGaveUp);
                    }
                },
                ReviewGate::Refused(verdict) => {
                    // **Parked, with the verdict on the row.** `Failed` is the queue's word for
                    // *this did not land and the tree is where the reason is*, and a refused
                    // review is that shape exactly: the work stays, the worktree stays, and a
                    // person answers it. A state of its own would say the same thing one word
                    // further out and would cost a protocol bump to draw.
                    self.move_to(entry, MergeState::Failed, verdict, None)?;
                    return Ok(StepOutcome::Refused);
                }
            }
        }

        let Some(idx) = next_takeable(&entries, &reviews) else {
            // **Nothing may be taken, and the two reasons are different.** *Something is due and
            // its verdict is not in* is not idleness: it is a queue waiting on a reviewer, and
            // the loop says so rather than sleeping the same way as an empty queue.
            return Ok(if asked {
                StepOutcome::AwaitingReview
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
                self.move_to(&entry, MergeState::Waiting, why, None)?;
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
        )?;

        // **Rebase it at the tip**: onto the current main, not the SHA it was written
        // against. A conflict is reported, never resolved.
        // **The tip of THIS repository's main, read now** — not the last SHA the queue landed:
        // main also moves by hand (a commit, a push, the gate's own section landing), and with
        // one queue serving several repositories the last landing may be another repository's.
        // The queue's record is the fallback only when git cannot name the tip.
        let tip_now = run_captured(&repo, "git", &["rev-parse", "main"])
            .ok()
            .map(|t| t.trim().to_string());
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
        let tip = match fast_forward_main(&repo, &entry.branch) {
            Ok(tip) => tip,
            Err(e) => {
                self.move_to(&entry, MergeState::Failed, e, None)?;
                return Ok(StepOutcome::Failed);
            }
        };
        if let Err(e) = push_main(&repo) {
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
                         with the failure on its row, and `/queue restart ID` asks again"
                    );
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

/// **The daemon's half of a wake: a gatekeeper SUBAGENT, under the session the branch came from.**
///
/// The operator, 2026-10-09: *"looks like there is no gatekeeper agent that does reviews"* —
/// entries had sat in the queue for ten hours and more — and then *"or - subagent"*. A reviewer
/// the daemon owned by itself could not be opened: the gatekeeper seats `bash`, and a session
/// with an exec tool must have somebody to rule on its commands, which a daemon-owned session
/// has not. A **subagent** has: its asks go up its tree to the root's head
/// (`SubagentAdjudicator`). So the review runs as a child of the root of the session that
/// enqueued the entry, and a reviewer's `git diff` card lands where the work was asked for.
///
/// What this door does is the queue thread's share, and it is two acts:
///
/// * **The request row, naming its HOST** — the root session that will run the gatekeeper.
///   Durable for the reason it always was: a daemon that restarts between the ask and the verdict
///   comes back to a row still waiting, and the next pass rings for it again. The host session
///   spawns the child when it is woken (`Harness::serve_reviews`) and writes the verdict onto
///   the same row when the child answers.
/// * **The ring** at the host. A host the daemon does not hold is **resumed** from the store
///   first — the operator's ruling — so an entry whose session closed hours ago is still
///   reviewed; its approvals then wait as cards until somebody opens that session, and the row
///   says which one.
pub struct GatekeeperDoor {
    /// The session store, opened on the queue's own thread (a `Connection` is `Send`, not
    /// `Sync`, and this struct travels into the merge-queue thread).
    store: Store,
    /// The daemon's bell, which is what the host's turn is started by.
    bell: Arc<letibot_sessionlog::registry::Bell>,
    /// The daemon's sessions: whether the host is live, and how to resume it when it is not.
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

    /// **The session an entry's queue business goes to** — the root of the session that queued
    /// it, resumed from the store when this daemon does not hold it (the operator's ruling), so
    /// an entry whose session closed hours ago still has somebody to ask. `what` says what the
    /// session is wanted for, in the refusal when there is none.
    fn host_for(&self, entry_id: &str, what: &str) -> Result<(MergeEntry, String), String> {
        let entry = self
            .store
            .merge_entry(entry_id)
            .map_err(|e| format!("the entry `{entry_id}` could not be read: {e}"))?
            .ok_or_else(|| format!("the entry `{entry_id}` is not in the queue's table"))?;
        let host = root_session(&self.store, &entry.session_id);
        if self.registry.get(&host).is_none() {
            match self.registry.resumable(&host) {
                Some(brief) => {
                    self.registry
                        .create(&host, &brief.title, brief.wiring)
                        .map_err(|e| {
                            format!(
                                "the session `{host}` that `{entry_id}` came from could not be \
                                 resumed to {what}: {e}"
                            )
                        })?;
                }
                None => {
                    return Err(format!(
                        "the session `{host}` that `{entry_id}` (branch `{}`) came from is in \
                         neither this daemon nor the store, so there is no session to {what}. \
                         The branch does not land without it.",
                        entry.branch
                    ));
                }
            }
        }
        Ok((entry, host))
    }
}

impl letibot_tools::gatekeeper::Reviewer for GatekeeperDoor {
    fn wake(
        &self,
        entry_id: &str,
        req: &letibot_tools::gatekeeper::ReviewRequest,
    ) -> Result<String, String> {
        let (_, host) = self.host_for(entry_id, "run its gatekeeper under")?;
        let existing = self
            .store
            .merge_review(entry_id)
            .map_err(|e| format!("the review row for `{entry_id}` could not be read: {e}"))?;
        // **Written when there is none, or when it names another host** — `asked_ms` is when the
        // review was FIRST asked for, and a queue that re-wrote it every pass would lose that.
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
                    // outstanding. The host writes the verdict when its gatekeeper answers.
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
        self.bell.ring_wake(&host);
        Ok(format!(
            "a gatekeeper reviews `{}` at base `{}` as a subagent of `{host}`; the verdict lands \
             on the entry's row and nothing lands without it. Its approvals ask in `{host}`.",
            req.branch, req.base_sha
        ))
    }

    /// **The project's agent is told its repository has no gate**: the root session that queued
    /// the entry is rung (resumed if it is not live), and its wake finds the entry waiting and
    /// tells its model (`Harness::queue_notices`).
    fn gate_missing(&self, entry_id: &str) -> Result<(), String> {
        let (_, host) = self.host_for(entry_id, "ask for a merge gate")?;
        if self.bell.is_closed() {
            return Err("the daemon's bell is closed (it is shutting down)".into());
        }
        self.bell.ring_wake(&host);
        Ok(())
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
        };
        enqueue(&db, &feature_entry);
        approve(&db, "m-land");

        // The daemon, with a no-op gate: the gate is the seam, and the test is about the
        // rebase and the fast-forward, not the gate.
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| Ok(())),
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
        };
        enqueue(&db, &feature_entry);
        approve(&db, "m-conflict");

        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| Ok(())),
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
        };
        enqueue(&db, &entry);
        approve(&db, "m-failed");

        // The gate fails, with its own words.
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| Err("the gate is red".into())),
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
        };
        enqueue(&db, &entry);

        // The next daemon recovers: the `Taken` row is moved to `Stale` on disk.
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| Ok(())),
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
            Box::new(|_| Ok(())),
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
            Box::new(|_| Ok(())),
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
            Box::new(|_| Ok(())),
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
        };
        enqueue(&db, &entry);
        // The door refuses, which is what a daemon with no reviewer session does.
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| Ok(())),
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

    /// **Every move the queue makes is announced, after the row is written.** The events
    /// decision's second half: a head that is attached when an entry moves sees the move rather
    /// than only the next snapshot.
    ///
    /// The ORDER is asserted as well as the content, and it is the load-bearing half: an event
    /// folded before the row was on disk would let a head re-read the queue and find the state
    /// it just heard was over — the one way the snapshot and the events can disagree.
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
        };
        enqueue(&db, &entry);
        approve(&db, "m-events");

        let events = RecordingEvents::default();
        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            root.clone(),
            Box::new(|_| Ok(())),
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
                ("m-events", letibot_sessionlog::event::MergeState::Landed),
            ],
            "the take and the landing are both announced, in that order"
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
            Box::new(|_| Ok(())),
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
        let err = repo_gate(&dir).expect_err("the second step is red");
        assert!(
            err.contains("exit 3") && err.contains("red-step"),
            "{err:?}"
        );
        assert!(
            !dir.join("ran-after").exists(),
            "a step after the red one ran"
        );

        // A branch that rewrites its own gate to pass is judged by main's.
        git(&["checkout", "-q", "-b", "agent/sneaky"]);
        std::fs::write(dir.join("AGENTS.md"), "## Merge gate\n\n```sh\ntrue\n```\n").unwrap();
        git(&["commit", "-qam", "loosen the gate"]);
        assert!(repo_gate(&dir).is_err(), "the branch's own gate was used");
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
            Box::new(|_| Ok(())),
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
            Box::new(|_| Ok(())),
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
            Box::new(|_| Ok(())),
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
            Box::new(|_| Ok(())),
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
            Box::new(|_| Ok(())),
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

    /// **letibot's own gate is CI's four steps**, read out of this repository's `agents.md` the
    /// way the queue reads it — so an edit that drops a step from the section is caught here.
    #[test]
    fn letibots_own_gate_is_cis_steps() {
        let steps = letibot_tools::gatekeeper::parse_merge_gate(include_str!("../../../agents.md"))
            .expect("agents.md has a Merge gate section");
        assert_eq!(
            steps,
            vec![
                "sh scripts/check-fmt.sh main",
                "cargo clippy --all-targets",
                "cargo test --workspace --no-fail-fast -- --nocapture",
                "cargo build --release --bins",
            ]
        );
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
        approve(&db, "m-thread");

        let daemon = MergeQueueDaemon::new(
            store_at(&db),
            std::env::temp_dir(),
            Box::new(|_| Ok(())),
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
    //! **The queue's door to a gatekeeper subagent** — the operator, 2026-10-09: *"looks like
    //! there is no gatekeeper agent that does reviews … or - subagent"*, with entries ten hours
    //! old waiting on a reviewer nobody could start. The door's half is the row naming the HOST
    //! (the root of the session that enqueued the entry), the host resumed when it is not live,
    //! and the ring; the host's half is `Harness::serve_reviews`.
    use super::*;
    use letibot_sessionlog::registry::{
        Registry as SessionRegistry, SessionSource, SessionWiring, StoredBrief, Work, WorkOrIdle,
    };
    use letibot_tokencore::store::{MergePriority, SessionRecord};
    use letibot_tools::gatekeeper::{ReviewRequest, Reviewer};
    use std::sync::Mutex;

    /// The store's sessions, as the daemon's own source answers them.
    struct Disk(Mutex<Vec<StoredBrief>>);
    impl SessionSource for Disk {
        fn list(&self) -> Vec<StoredBrief> {
            self.0.lock().unwrap().clone()
        }
    }

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

    fn brief(id: &str) -> StoredBrief {
        StoredBrief {
            session_id: id.into(),
            title: format!("title of {id}"),
            items: 3,
            last_activity_ms: 1,
            wiring: SessionWiring::default(),
            parent_session_id: None,
            context_tokens: None,
            context_cached: None,
            stored_end: None,
        }
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

    #[test]
    fn a_review_is_hosted_by_the_root_resumed_and_rung() {
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
            })
            .unwrap();
        let registry = SessionRegistry::new();
        registry.set_source(Arc::new(Disk(Mutex::new(vec![brief("s-root")]))));
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
        assert!(said.contains("s-root"), "the row names its host: {said}");
        // The host was not live, so it was resumed — and then rung.
        assert!(registry.get("s-root").is_some(), "the root was resumed");
        assert_eq!(wakes(&registry), vec!["open s-root", "woken s-root"]);
        let row = store.merge_review("e1").unwrap().expect("a request row");
        assert_eq!(
            row.session_id, "s-root",
            "hosted by the ROOT, not the child"
        );
        assert!(row.answered_ms.is_none(), "outstanding");

        // Asked again on the next pass: the same row, the first `asked_ms`, and only a ring.
        let first = row.asked_ms;
        door.wake("e1", &req).expect("asked again");
        assert_eq!(store.merge_review("e1").unwrap().unwrap().asked_ms, first);
        assert_eq!(wakes(&registry), vec!["woken s-root"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_entry_whose_session_is_nowhere_is_refused_by_name() {
        let dir = std::env::temp_dir().join(format!("letibot-gk-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.db");
        let store = Store::open(&path).unwrap();
        let mut e = super::tests_entry_for_door("e2", "s-vanished");
        e.brief = "x".into();
        store.put_merge_entry(&e).unwrap();
        let registry = SessionRegistry::new();
        let door = GatekeeperDoor::new(
            Store::open(&path).unwrap(),
            registry.bell().clone(),
            registry.clone(),
        );
        let why = door
            .wake(
                "e2",
                &ReviewRequest {
                    brief: "x".into(),
                    branch: e.branch.clone(),
                    base_sha: e.base_sha.clone(),
                },
            )
            .expect_err("nothing to host it");
        assert!(why.contains("s-vanished"), "{why}");
        assert!(
            store.merge_review("e2").unwrap().is_none(),
            "nothing was asked"
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
            Box::new(|_| Ok(())),
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
            Box::new(|_| Ok(())),
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
    }
}
