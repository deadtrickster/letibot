//! **The merge queue, end to end: the child that finishes, and the row the queue then serves.**
//!
//! The operator's ask, in their words: *"well we definitely want an end to end test for the
//! merge queue"*. This file is that test, written against the gap the child who wired the queue
//! named rather than against the gap a reader would guess at:
//!
//! > every link is proven in isolation, but the glue is only read, not measured
//!
//! # The link this closes
//!
//! [`HarnessTaskRunner::enqueue_entry`] and its caller [`HarnessTaskRunner::enqueue_finished`],
//! and the two things the first of them reads that only [`HarnessTaskRunner::start_worktree`]
//! can put there.
//!
//! * **The record, and the lookup.** [`PlacedChild::of`] is the rule `start_worktree` records a
//!   child by — *a worktree child is recorded, a main-checkout child is not* — and
//!   `enqueue_entry` finds the child by handle in that record. Before this file, the rule had no
//!   caller under test and the lookup had no test at all: a `PlacedChild` could be spelled
//!   wrongly and every assertion about it would still pass, because nothing executed the read.
//! * **The store.** `base.store` is `--store`, and `Store::open` on first use plus
//!   `put_merge_entry` are the two lines that turn a finished child into a durable row. The
//!   `base.store`-is-`None` refusal is the other half of the same line, and it is a refusal
//!   rather than a quiet `NoBranch` on purpose — a parent told *queued* about a branch in no
//!   queue is the lie this tree keeps refusing to tell.
//! * **The two notices.** `enqueue_finished` broadcasts `MergeEntryAdded` to **every** session's
//!   log (a queue is daemon-level, and a head attached elsewhere can open the pane) and publishes
//!   `merge_queued` to **this** conversation's hub. Both are asserted here off real hubs, because
//!   the door under test is [`letibot_sessionlog::registry::Registry::broadcast`] and a `Vec` the
//!   test owned would prove nothing about it.
//! * **And then the queue acts on the row.** The entry the runner wrote is read back through a
//!   second connection, given the verdict the gate requires, and taken through the daemon's own
//!   [`crate::mergequeue::MergeQueueDaemon::step`] — so what is asserted is not that a row
//!   exists but that `main` moved, and moved to the commit the child's branch was at.
//!
//! # What it is, and what it deliberately is not
//!
//! It is **not** a live test: no model server is contacted, no child is spawned, and nothing here
//! waits on a clock. The two fields of the runner that nothing on this path reads — the
//! vocabulary and the dialect wiring — are the only reason it needs the box's GGUF at all, which
//! is `compact.rs`'s situation exactly and is answered the same way: [`present_gguf`] announces a
//! skip when the vocabulary is not here rather than failing, and `LETIBOT_REQUIRE_APPARATUS=1`
//! refuses that skip.
//!
//! # What it still cannot reach
//!
//! * **`start_worktree`'s own body.** The recording is exercised (through the function it
//!   calls), but the git work — `worktree add`, the base SHA read before the branch is cut, the
//!   shared build cache — is not, because the last line of that method is
//!   [`HarnessTaskRunner::start`], which spawns a real child on a real model. The fixture builds
//!   the same *shape* of tree that method builds; it does not run that method. This is the one
//!   link in the chain still read rather than measured, and it is named here rather than left
//!   for a reader to discover.
//! * **The gate.** The daemon is driven with a no-op gate, which is the seam
//!   `MergeQueueDaemon::new` exists to make replaceable; `ci_gate` itself is not run, because it
//!   is `cargo test --workspace` and a release build.
//! * **`Stale`.** Reachable through `MergeQueueDaemon::recover` on a `Taken` row, and asserted
//!   there, in `mergequeue`'s own tests against a real temp repo — which is why this file does
//!   not repeat it. What this file adds is that the entry those paths act on is one the *runner*
//!   wrote; what it does not add is a second test of the paths themselves.
//! * **The reviewer.** The verdict is written straight into the store, which is where the queue
//!   reads it. The reviewer's own session — the door that writes it in production — is
//!   `gatekeeper`'s and is not run here.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use letibot_sessionlog::registry::{Registry as SessionRegistry, SessionWiring};
use letibot_tokencore::apparatus::present_gguf;
use letibot_tokencore::store::{MergePriority, MergeState, ReviewRecord};
use letibot_tools::builtins::task::{Finished, TaskRunner as _, WorktreePlacement};
use letibot_tools::gatekeeper::NoReviewer;

use super::*;
use crate::mergequeue::{MergeQueueDaemon, REVIEWER_SESSION_ID, StepOutcome};

/// **The branch every child in this file is on** — `worktree_branch`'s own shape,
/// `agent/<slug>`, so a name that reaches the queue here is a name `task_start` produces.
const SLUG: &str = "queue-e2e-child";

// ===== The fixture: the vocabulary, a temp store, and two hubs =====

/// A directory that removes itself, because a test that leaks a repo and a store per run
/// eventually fills the disk and the run that finds out is not this one.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// **Everything the tests here start from, built once.**
///
/// The store is a **temp** file and never the daemon's own: the queue lives in `sessions.db`,
/// and a test that opened the operator's would be writing rows into a live queue. The two
/// sessions are registered because `enqueue_finished` publishes to one hub and broadcasts to
/// every hub, and the difference between the two doors is half of what this file asserts.
struct Fixture {
    /// Held, not read: dropping it deletes the repo and the store.
    dir: TempDir,
    db: PathBuf,
    parts: Parts,
    cfg: Config,
    registry: Arc<SessionRegistry>,
    /// The session whose child finished — where the `merge_queued` sentence goes.
    parent: Arc<Hub>,
    /// A session attached somewhere else — where only the broadcast goes, and the case the
    /// queue pane exists for.
    elsewhere: Arc<Hub>,
}

impl Fixture {
    /// `None` after announcing the skip, when this box has no vocabulary GGUF — the shape
    /// `compact.rs` uses, and the only apparatus any test here needs.
    fn new(tag: &str) -> Option<Self> {
        let gguf = present_gguf()?;
        let dir = TempDir::new(&format!("letibot-queue-e2e-{tag}"));
        let db = dir.path().join("sessions.db");
        let mut cfg = Config::for_this_box(dir.path());
        cfg.vocab_gguf = gguf;
        // **The `--store`**, which is the whole of what makes a daemon able to enqueue at all.
        cfg.store = Some(db.clone());
        cfg.session_id = "s-parent".into();
        // Nothing here reaches an endpoint, and the one path that could would spend the whole
        // retry ladder learning that.
        cfg.http_retries = 0;
        let parts = Parts::load(&cfg).expect("the vocabulary must load");
        let registry = SessionRegistry::new();
        let parent = registry
            .create("s-parent", "", SessionWiring::default())
            .expect("the parent registers");
        let elsewhere = registry
            .create("s-elsewhere", "", SessionWiring::default())
            .expect("a session attached elsewhere registers");
        Some(Self {
            dir,
            db,
            parts,
            cfg,
            registry,
            parent,
            elsewhere,
        })
    }

    /// **The runner, with the children `start_worktree` would have recorded.**
    ///
    /// `HarnessTaskRunner::for_enqueue` is the test-only constructor beside the type: the four
    /// facts the enqueue reads, and `None` for the eleven a spawn reads.
    fn runner(&self, placed: Vec<PlacedChild>) -> HarnessTaskRunner {
        HarnessTaskRunner::for_enqueue(&self.parts, self.cfg.clone(), self.registry.clone(), placed)
    }

    /// The store, opened fresh — a connection that did not write the row.
    fn store(&self) -> Store {
        Store::open(&self.db).expect("the store reopens")
    }
}

/// **A repo with `main` and one worktree per child** — the shape `task_start` arranges and the
/// queue serves: each branch cut from `main`'s single commit, checked out at
/// `<root>/worktrees/<slug>`, with one commit of its own that writes `a.txt`.
///
/// `children` is `(slug, what the child put in a.txt)`. Two children writing DIFFERENT text to
/// the same line is the conflict fixture, and it is the only difference between the two tests
/// below — the queue rebases the second onto what the first landed, which is what makes two
/// green branches red together.
///
/// Returns the repo, the SHA every branch was cut from, and the branches in the order given.
fn repo_with_children(under: &Path, children: &[(&str, &str)]) -> (PathBuf, String, Vec<Branch>) {
    let root = under.join("repo");
    std::fs::create_dir_all(&root).expect("the repo's directory");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "queue-e2e@test"]);
    git(&root, &["config", "user.name", "queue-e2e"]);
    std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").expect("write");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "main's own work"]);
    let base_sha = sha(&root, "main");

    let mut branches = Vec::new();
    for (slug, body) in children {
        let worktree = root.join("worktrees").join(slug);
        let branch = format!("agent/{slug}");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                worktree.to_str().expect("a utf-8 temp path"),
                "-b",
                &branch,
                "main",
            ],
        );
        std::fs::write(worktree.join("a.txt"), body).expect("write");
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-qm", "the child's work"]);
        let at = sha(&worktree, &branch);
        branches.push(Branch {
            worktree,
            branch,
            sha: at,
        });
    }
    (root, base_sha, branches)
}

/// One child's branch, as `task_start` left it.
struct Branch {
    worktree: PathBuf,
    branch: String,
    /// The commit the branch is at — what `main` must be after the queue lands it.
    sha: String,
}

impl Branch {
    /// **The placement the runner records for this child** — the three facts `start_worktree`
    /// reads out of git and keeps, and the one it decides (`main_tree: false`).
    fn placement(&self, base_sha: &str) -> WorktreePlacement {
        WorktreePlacement {
            path: self.worktree.display().to_string(),
            branch: self.branch.clone(),
            base_sha: base_sha.to_string(),
            main_tree: false,
        }
    }
}

/// Run one git command in `dir`, asserting it succeeds — so a failure names the command and
/// git's own words rather than panicking on a bare `expect`.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Whether a git command in `dir` succeeds — for the assertions that are about a command
/// FAILING, which is how a deleted branch is observed.
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The SHA of `rev` in `dir`.
fn sha(dir: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", rev])
        .output()
        .expect("git");
    assert!(out.status.success(), "git rev-parse {rev}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// **The verdict the queue reads, written where it reads it.** Nothing is taken without one —
/// that is the gate — so every fixture that expects a landing needs this, and it goes in
/// through a connection of its own, like the enqueuer's own row.
fn approve(fx: &Fixture, entry_id: &str) {
    let entry = fx
        .store()
        .merge_entry(entry_id)
        .expect("reads")
        .expect("the entry");
    fx.store()
        .put_review(&ReviewRecord {
            entry_id: entry_id.to_string(),
            session_id: REVIEWER_SESSION_ID.to_string(),
            branch: entry.branch,
            base_sha: entry.base_sha,
            asked_ms: 1,
            answered_ms: Some(2),
            decision: Some("accept".into()),
            reasons: vec!["the artifact does what the brief asked".into()],
            files: vec!["a.txt".into()],
            commands: vec!["git diff base...branch".into()],
        })
        .expect("the verdict row");
}

/// The daemon over this fixture's store, serving `repo`, with a gate that is green and a sink
/// that drops — the two seams `MergeQueueDaemon::new` exists to make replaceable.
fn daemon(fx: &Fixture, repo: &Path) -> MergeQueueDaemon {
    MergeQueueDaemon::new(
        fx.store(),
        repo.to_path_buf(),
        Box::new(|_: &Path| Ok(())),
        Box::new(NoReviewer),
        Box::new(|_: SessionEvent| {}),
    )
}

/// **Whether this hub's log carries an entry's arrival** — read off a real hub, because the
/// door under test is `Registry::broadcast` and a list the test owned would prove nothing
/// about it.
fn heard_about(hub: &Hub, id: &str) -> bool {
    hub.retained()
        .iter()
        .any(|env| matches!(&env.event, SessionEvent::MergeEntryAdded { entry } if entry.id == id))
}

/// The sentence a hub was given under `code`, if it was given one.
fn warning(hub: &Hub, code: &str) -> Option<String> {
    hub.retained().iter().find_map(|env| match &env.event {
        SessionEvent::Warning {
            code: c, detail, ..
        } if c == code => Some(detail.clone()),
        _ => None,
    })
}

// ===== The tests =====

/// **A finished child leaves a queue row, and the queue lands it.**
///
/// The whole chain in one test, because the chain is the thing that had no test: the child is
/// recorded by the rule `start_worktree` records by, `enqueue_finished` turns that record into
/// a row in a temp store and tells two audiences about it, and the daemon then reads that same
/// row back, is given the verdict its gate requires, and fast-forwards `main` onto the child's
/// commit.
///
/// **What the row must carry is not decoration.** The branch and the base SHA are what the
/// rebase is done with, and the **brief is the reviewer's only framing** — an entry that lost
/// it is an entry nobody can review, which is why it is asserted field by field off a
/// connection that did not write it.
#[test]
fn a_finished_child_leaves_a_row_and_the_queue_lands_it() {
    let Some(fx) = Fixture::new("lands") else {
        return;
    };
    let (root, base_sha, branches) =
        repo_with_children(fx.dir.path(), &[(SLUG, "ONE\ntwo\nthree\n")]);
    let child = &branches[0];

    let handle = "s-parent-sub-1";
    let brief = "make the queue pane show the brief the child was given";
    let placement = child.placement(&base_sha);
    // **The record `start_worktree` keeps**, through the function it keeps it with.
    let placed = PlacedChild::of(handle, brief, &placement).expect("a worktree child is recorded");
    let runner = fx.runner(vec![placed]);

    // **The child finishes.** This is the glue: the `placed` lookup, the store opened from
    // `--store` on first use, `entry_for_finished`'s row, `put_merge_entry`, and the two
    // notices. Nothing below this line is a fixture.
    runner.enqueue_finished(handle);

    // (1) **The row, in the store, read back through a second connection.**
    let row = fx
        .store()
        .merge_entry(handle)
        .expect("reads")
        .expect("the enqueue wrote an entry for a child that finished");
    assert_eq!(
        row.branch, child.branch,
        "the branch to land is the child's"
    );
    assert_eq!(
        row.base_sha, base_sha,
        "the base is the SHA it was cut from"
    );
    assert_eq!(
        row.brief, brief,
        "the reviewer's input is the ask the child was given, verbatim"
    );
    assert_eq!(
        row.worktree.as_deref(),
        Some(child.worktree.to_str().expect("a utf-8 temp path")),
        "the rebase and the gate need the tree the branch is checked out in"
    );
    assert_eq!(
        row.session_id, handle,
        "the entry's session is the CHILD that finished the branch, not the parent that noticed"
    );
    assert_eq!(row.priority, MergePriority::Subagent);
    assert_eq!(row.state, MergeState::Waiting, "it is due, not taken");
    assert_eq!(row.landed_sha, None, "nothing has landed yet");

    // **And the two audiences, through the two doors.** The broadcast is the queue pane's
    // (`MergeEntryAdded` to every session, so a head attached elsewhere sees the arrival); the
    // sentence is this conversation's.
    assert!(
        heard_about(&fx.parent, handle),
        "the parent's own log does not carry the entry's arrival"
    );
    assert!(
        heard_about(&fx.elsewhere, handle),
        "a session attached elsewhere is not told: a queue is daemon-level, and the pane is the \
         case this broadcast exists for"
    );
    let said = warning(&fx.parent, "merge_queued")
        .expect("the parent is told, unprompted, that its child's branch is queued");
    assert!(said.contains(&child.branch), "{said}");
    assert!(
        warning(&fx.elsewhere, "merge_queued").is_none(),
        "the sentence is about THIS conversation and must not be broadcast with the event"
    );

    // (2) **The queue acts on it.** The verdict goes in where the queue reads it, and then one
    // `step` is the whole of the daemon's work.
    approve(&fx, handle);
    let said = daemon(&fx, &root).step().expect("the pass");
    let StepOutcome::Landed(tip) = said else {
        panic!("the entry the runner wrote was not landed: {said:?}");
    };
    assert_eq!(
        tip, child.sha,
        "main moved to something other than the child's commit"
    );
    assert_eq!(
        sha(&root, "main"),
        child.sha,
        "the landing is main itself moving, not a row that says it did"
    );
    // **And the cleanup the state owns** — `Landed` removes the worktree and deletes the
    // branch, and the deletion is `git branch -d`, which refuses a branch that is not merged.
    assert!(
        !child.worktree.exists(),
        "a landed entry's worktree is removed"
    );
    assert!(
        !git_ok(&root, &["rev-parse", "--verify", "--quiet", &child.branch]),
        "a landed entry's branch is deleted"
    );

    let after = fx
        .store()
        .merge_entry(handle)
        .expect("reads")
        .expect("the entry is still there, with its ending on it");
    assert_eq!(after.state, MergeState::Landed);
    assert_eq!(after.landed_sha.as_deref(), Some(child.sha.as_str()));
    assert!(
        after.evidence.contains("landed at"),
        "the row says what happened: {:?}",
        after.evidence
    );
}

/// **The second child's rebase conflicts, and the queue parks it with git's own words.**
///
/// The same chain as above with one argument changed: two children, cut from the same commit,
/// each writing a different thing to the same line of `a.txt`. The first lands; the second is
/// then rebased onto **what the first landed** rather than onto the SHA it was written against
/// — which is the queue's whole thesis — and that rebase conflicts.
///
/// It is here rather than only in `mergequeue`'s own tests because what it demonstrates is
/// about *this* seam: two entries the runner wrote, served in the order the queue scheduled
/// them, with the second one's conflict a fact about the first one's landing rather than about
/// the fixture.
#[test]
fn the_second_childs_rebase_conflicts_and_the_queue_parks_it() {
    let Some(fx) = Fixture::new("conflict") else {
        return;
    };
    let (root, base_sha, branches) = repo_with_children(
        fx.dir.path(),
        &[
            ("queue-e2e-first", "ONE\ntwo\nthree\n"),
            ("queue-e2e-second", "one-two\ntwo\nthree\n"),
        ],
    );
    let first = &branches[0];
    let second = &branches[1];

    let mut placed = Vec::new();
    for (handle, branch) in [("s-parent-sub-1", first), ("s-parent-sub-2", second)] {
        placed.push(
            PlacedChild::of(
                handle,
                "change the first line of a.txt",
                &branch.placement(&base_sha),
            )
            .expect("a worktree child is recorded"),
        );
    }
    let runner = fx.runner(placed);
    runner.enqueue_finished("s-parent-sub-1");
    runner.enqueue_finished("s-parent-sub-2");

    approve(&fx, "s-parent-sub-1");
    approve(&fx, "s-parent-sub-2");
    let daemon = daemon(&fx, &root);

    let said = daemon.step().expect("the first pass");
    assert_eq!(
        said,
        StepOutcome::Landed(first.sha.clone()),
        "the older entry is taken first"
    );
    let said = daemon.step().expect("the second pass");
    assert_eq!(
        said,
        StepOutcome::Conflict,
        "the second entry rebases onto what the first landed, and its edit of the same line \
         conflicts"
    );

    let row = fx
        .store()
        .merge_entry("s-parent-sub-2")
        .expect("reads")
        .expect("the entry");
    assert_eq!(row.state, MergeState::Conflict);
    assert!(
        !row.evidence.is_empty(),
        "a conflict carries git's own words, not a bare state"
    );
    // **The evidence stays, so the tree stays**: a `Conflict` owns `KeepWorktree`, because the
    // worktree is where the failure is and a person answering it needs the tree.
    assert!(
        second.worktree.exists(),
        "a conflicted entry's worktree is removed, and the evidence with it"
    );
    assert!(
        git_ok(&root, &["rev-parse", "--verify", "--quiet", &second.branch]),
        "a conflicted entry's branch is deleted"
    );
    assert_eq!(
        sha(&root, "main"),
        first.sha,
        "a conflict lands nothing: main is still where the first entry left it"
    );
}

/// **A child of the main checkout leaves no entry** — the negative, at both of its two guards.
///
/// A `task_start` with `main_tree: true` works on `main`'s own branch, so there is no branch of
/// its own to land. Two places know that and this asserts both, because they fail in different
/// ways: [`PlacedChild::of`] never records such a child (so the lookup cannot find one), and
/// `entry_for_finished` refuses one anyway (so a record that exists — an older build's, a
/// hand-written row — still lands nothing).
///
/// **The second guard is exercised through the enqueue**, with a record built by hand precisely
/// because the rule refuses to build one: `Ok(None)` is the answer, and it is the same answer
/// the tool layer's `task_result` renders as `NoBranch`. The store stays empty, and — the half
/// that matters for a person watching — nothing is announced, because there is nothing to
/// announce.
#[test]
fn a_child_of_the_main_tree_leaves_no_entry() {
    let Some(fx) = Fixture::new("main-tree") else {
        return;
    };
    let (root, base_sha, _branches) = repo_with_children(fx.dir.path(), &[]);
    let main_tree = WorktreePlacement {
        path: root.display().to_string(),
        branch: "main".into(),
        base_sha: base_sha.clone(),
        main_tree: true,
    };

    // (a) **The recording rule**: `start_worktree` keeps nothing for a main-checkout child.
    assert!(
        PlacedChild::of("s-parent-sub-1", "work in the main tree", &main_tree).is_none(),
        "a main-checkout child was recorded, and the queue would then be handed `main` to land"
    );

    // (b) **And the enqueue's own guard**, for a record that exists anyway.
    let runner = fx.runner(vec![PlacedChild {
        handle: "s-parent-sub-1".into(),
        brief: "work in the main tree".into(),
        placement: main_tree,
    }]);
    let said = runner
        .finished("s-parent-sub-1")
        .expect("a child of the main tree is not an error");
    let Finished::NoBranch { why } = said else {
        panic!("a main-checkout child answered with a branch to land: {said:?}");
    };
    assert!(
        why.contains("main checkout"),
        "the sentence must name the reason, not just the absence: {why}"
    );
    runner.enqueue_finished("s-parent-sub-1");

    assert!(
        fx.store().merge_entries().expect("reads").is_empty(),
        "a main-checkout child left a row in the queue"
    );
    assert!(
        !heard_about(&fx.parent, "s-parent-sub-1"),
        "nothing was queued, and an announcement would say otherwise"
    );
    assert!(
        warning(&fx.parent, "merge_not_queued").is_none(),
        "a main-checkout child is the ordinary case, and a line per such child is noise"
    );
}

/// **A second finish of one child is one row** — the two doors, and the reason there are two.
///
/// `finished` is called from the child's own settlement *and* from `task_result` on a `Done`,
/// because the first can be missed (a parent that never collects a child) and the second can be
/// missed (a model that never asks). Both look the same child up by handle and write the same
/// row, and `put_merge_entry` is an upsert keyed by the id — so the queue holds one entry for
/// one child however many times the finish is noticed.
///
/// The count is asserted off a connection that did not write either row, and so is the id: two
/// rows would mean the queue would try to land the same branch twice.
#[test]
fn a_second_finish_of_one_child_is_one_row() {
    let Some(fx) = Fixture::new("twice") else {
        return;
    };
    let (_root, base_sha, branches) =
        repo_with_children(fx.dir.path(), &[(SLUG, "ONE\ntwo\nthree\n")]);
    let child = &branches[0];

    let handle = "s-parent-sub-1";
    let placed = PlacedChild::of(handle, "the ask", &child.placement(&base_sha))
        .expect("a worktree child is recorded");
    let runner = fx.runner(vec![placed]);

    // The child's own settlement, then a `task_result` that arrives later and notices the same
    // finish. Two calls, one child.
    runner.enqueue_finished(handle);
    runner.enqueue_finished(handle);

    let rows = fx.store().merge_entries().expect("reads");
    assert_eq!(
        rows.len(),
        1,
        "one child, one row — two rows is the same branch landed twice: {rows:?}"
    );
    assert_eq!(rows[0].id, handle);
    assert_eq!(
        rows[0].branch, child.branch,
        "the row the second finish left is about the same branch"
    );
}

/// **A daemon with no `--store` refuses by name rather than dropping the branch.**
///
/// The other end of the line `Store::open(--store)` is on. The queue lives in `sessions.db`, so
/// a daemon started without one has no queue at all — and the two things it must not do are
/// both asserted: it must not answer *queued* about a branch in no queue (the tool layer's
/// `finished` is an `Err`), and it must not stay silent about it (the child's own settlement
/// prints `merge_not_queued` on the parent's log, because a finished child must not be turned
/// into a failed one by a queue that could not be reached).
///
/// The branch is still there and still in its worktree, which is what the refusal says, and is
/// the reason this is a refusal rather than a lost commit.
#[test]
fn a_daemon_with_no_store_refuses_by_name() {
    let Some(mut fx) = Fixture::new("no-store") else {
        return;
    };
    let (root, base_sha, branches) =
        repo_with_children(fx.dir.path(), &[(SLUG, "ONE\ntwo\nthree\n")]);
    let child = &branches[0];
    let handle = "s-parent-sub-1";

    // The daemon the operator did not pass `--store` to.
    fx.cfg.store = None;
    let placed = PlacedChild::of(handle, "the ask", &child.placement(&base_sha))
        .expect("a worktree child is recorded");
    let runner = fx.runner(vec![placed]);

    let why = runner
        .finished(handle)
        .expect_err("a daemon with no queue must not answer `queued`");
    assert!(
        why.contains("--store"),
        "the refusal must name the knob that fixes it: {why}"
    );
    assert!(
        why.contains(&child.branch),
        "and the branch it is about: {why}"
    );

    // **And the child's own settlement says so rather than swallowing it.** The enqueue is
    // infallible from the child's side — its turn is over either way — so the refusal comes
    // back as a sentence on the parent's log.
    runner.enqueue_finished(handle);
    let said = warning(&fx.parent, "merge_not_queued")
        .expect("a queue that could not be reached is said out loud, not swallowed");
    assert!(said.contains("--store"), "{said}");
    assert!(
        warning(&fx.parent, "merge_queued").is_none(),
        "nothing was queued, so nothing may say it was"
    );

    // Nothing is lost: the branch is where it was.
    assert!(child.worktree.exists());
    assert!(
        git_ok(&root, &["rev-parse", "--verify", "--quiet", &child.branch]),
        "the refusal is about a queue that is not there, not about a branch that is"
    );
}
