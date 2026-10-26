//! **The queues, end to end: the child that finishes, the review row it leaves, and the landing.**
//!
//! The operator: *"well we definitely want an end to end test for the merge queue"*. The glue
//! measured here is [`HarnessTaskRunner::enqueue_entry`] and its caller
//! [`HarnessTaskRunner::enqueue_finished`]: a `task_start` child recorded by [`PlacedChild::of`]
//! finishes, and its branch is put up for REVIEW (R61: *"review is review and merge is merge"*) —
//! a review row naming the repository its worktree is in, the target the project lands on and the
//! head as it is now — announced on every hub (`QueueMoved`) and told to this conversation
//! (`merge_queued`). Then the repository's queue worker (`mergequeue::serve::RepoQueue`) claims
//! it, the reviewer's verdict is written the way the review pool writes it, and the worker lands
//! it: rebased in the queue's own worktree, gated, the target moved by compare-and-swap.
//!
//! Not a live test: no model, no child spawned. The vocabulary is the only apparatus (the runner
//! carries it), and the test skips without it; `LETIBOT_REQUIRE_APPARATUS=1` refuses the skip.
//! The store is a temp file, never the daemon's own.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use letibot_sessionlog::registry::{Registry as SessionRegistry, SessionWiring};
use letibot_tokencore::store::{
    Approval, LeaseClaim, MergePriority, MergeState, Move, NewReview, ReviewKind, ReviewState,
};
use letibot_tools::builtins::task::{Finished, TaskRunner as _, WorktreePlacement};

use super::*;
use crate::mergequeue::gate::GreenGate;
use crate::mergequeue::serve::{Pass, QueueDoor, QueueEnv, RepoQueue};
use crate::sessions::Sessions;

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
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
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
        let gguf = crate::vocab_gguf()?;
        let dir = TempDir::new(&format!("letibot-queue-e2e-{tag}"));
        let db = dir.path().join("sessions.db");
        let mut cfg = Config::for_this_box(dir.path());
        cfg.vocab_gguf = Some(gguf);
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
    std::fs::write(
        root.join("AGENTS.md"),
        "## Merge gate\n\n```sh\ntrue\n```\n",
    )
    .expect("write");
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

/// **A door that drops** — what the queue says is asserted where it is said.
struct Quiet;

impl QueueDoor for Quiet {
    fn announce(&self, _event: SessionEvent) {}
    fn ring_reviewers(&self) {}
    fn ring_project(&self, _session_id: &str) -> Result<String, String> {
        Ok(String::new())
    }
}

/// **The repository's queue worker, holding its lease** — with a green gate (the gate itself is
/// asserted in `mergequeue::serve::tests` against real steps).
fn worker(fx: &Fixture, repo: &Path) -> RepoQueue {
    let me = LeaseClaim {
        owner: "e2e/1/1".into(),
        pid: 1,
        host: "e2e".into(),
        build: "e2e".into(),
    };
    let store = fx.store();
    let repo = std::fs::canonicalize(repo).expect("the repo canonicalizes");
    store
        .acquire_queue_lease(&repo.display().to_string(), &me, 1, u64::MAX / 2)
        .expect("the lease");
    RepoQueue::new(
        store,
        repo,
        Arc::new(QueueEnv {
            me,
            gate: Arc::new(GreenGate),
            door: Arc::new(Quiet),
            settings: Arc::new(|_| Ok(Default::default())),
            landing_root: crate::mergequeue::serve::tests::test_landing_root(),
            gate_deadline: std::time::Duration::from_secs(60),
            review_deadline_ms: crate::mergequeue::serve::REVIEW_DEADLINE_MS,
            reviewer_start_grace_ms: crate::mergequeue::serve::REVIEWER_START_GRACE_MS,
            idle: std::time::Duration::from_millis(10),
        }),
    )
}

/// **The reviewer's accept, written the way the review pool writes it** — bound to the head the
/// queue's claim read.
fn accept(fx: &Fixture, id: &str) -> String {
    let s = fx.store();
    let r = s.review_row(id).expect("reads").expect("the row");
    assert_eq!(r.state, ReviewState::Reviewing, "claimed first: {r:?}");
    let merge_id = format!("m-{id}");
    assert!(
        s.approve_review(
            id,
            &Approval {
                expect: ReviewState::Reviewing,
                expect_owner: Some(r.owner.clone()),
                reviewed_sha: r.head_sha.clone(),
                by: "reviewer:sub-r".into(),
                reasons: vec!["the artifact does what the brief asked".into()],
                files: vec!["a.txt".into()],
                commands: vec![],
                why: "accepted".into(),
                merge_id: merge_id.clone(),
            },
            3,
        )
        .expect("the verdict")
    );
    merge_id
}

fn passes(q: &RepoQueue, n: usize) {
    let stop = std::sync::atomic::AtomicBool::new(false);
    for _ in 0..n {
        assert_ne!(q.settle(&stop).expect("a pass"), Pass::LostLease);
    }
}

/// **Whether this hub's log carries a queue move about `id`** — read off a real hub, because the
/// door under test is `Registry::broadcast`.
fn heard_about(hub: &Hub, id: &str) -> bool {
    hub.retained()
        .iter()
        .any(|env| matches!(&env.event, SessionEvent::QueueMoved { id: i, .. } if i == id))
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

/// **A finished child leaves a review row, and the queues review and land it.**
#[test]
fn a_finished_child_leaves_a_review_row_and_the_queues_land_it() {
    let Some(fx) = Fixture::new("lands") else {
        return;
    };
    let (root, base_sha, branches) =
        repo_with_children(fx.dir.path(), &[(SLUG, "ONE\ntwo\nthree\n")]);
    let child = &branches[0];
    let handle = "s-parent-sub-1";
    let brief = "make the queue pane show the brief the child was given";
    let placed = PlacedChild::of(handle, brief, &child.placement(&base_sha))
        .expect("a worktree child is recorded");
    let runner = fx.runner(vec![placed]);

    runner.enqueue_finished(handle);

    let row = fx
        .store()
        .review_row(handle)
        .expect("reads")
        .expect("the enqueue wrote a review row for a child that finished");
    let repo_key = std::fs::canonicalize(&root).unwrap().display().to_string();
    assert_eq!(row.repository, repo_key, "the worktree's own repository");
    assert_eq!(
        row.target, "main",
        "the project's target (decision 12; `main` by default)"
    );
    assert_eq!(row.branch, child.branch);
    assert_eq!(row.base_sha, base_sha);
    assert_eq!(row.head_sha, child.sha, "the head as it is now");
    assert_eq!(
        row.brief, brief,
        "the ask, verbatim — the reviewer's only framing"
    );
    assert_eq!(row.session_id, handle, "the CHILD that finished the branch");
    assert_eq!(row.kind, ReviewKind::Branch);
    assert_eq!(row.priority, MergePriority::Subagent);
    assert_eq!(row.state, ReviewState::Waiting);
    assert!(
        fx.store().merge_rows(None).unwrap().is_empty(),
        "nothing is in the MERGE queue before a review approves it — two queues"
    );

    assert!(heard_about(&fx.parent, handle));
    assert!(
        heard_about(&fx.elsewhere, handle),
        "the move is broadcast: a pane may be open on any session"
    );
    let said = warning(&fx.parent, "merge_queued").expect("the parent is told");
    assert!(said.contains(&child.branch), "{said}");
    assert!(warning(&fx.elsewhere, "merge_queued").is_none());

    // The queues act on it: claimed, approved at the claimed head, landed.
    let q = worker(&fx, &root);
    passes(&q, 1);
    let m = accept(&fx, handle);
    passes(&q, 2);
    let landed = fx
        .store()
        .merge_row(&m)
        .expect("reads")
        .expect("the merge row");
    assert_eq!(landed.state, MergeState::Landed, "{landed:?}");
    assert_eq!(
        sha(&root, "main"),
        child.sha,
        "main had not moved, so the rebase is a no-op and main IS the child's commit"
    );
    assert!(
        !child.worktree.exists(),
        "the landed branch's worktree is retired"
    );
    assert!(!git_ok(
        &root,
        &["rev-parse", "--verify", "--quiet", &child.branch]
    ));
}

/// **The second child's rebase conflicts, and the merge queue parks it with git's own words.**
/// Two children cut from one commit, writing the same line differently: the first lands; the
/// second is rebased onto what the first landed, and that conflicts.
#[test]
fn the_second_childs_rebase_conflicts_and_the_queue_parks_it() {
    let Some(fx) = Fixture::new("conflict") else {
        return;
    };
    let (root, base_sha, branches) = repo_with_children(
        fx.dir.path(),
        &[
            ("first", "FIRST\ntwo\nthree\n"),
            ("second", "SECOND\ntwo\nthree\n"),
        ],
    );
    let placed: Vec<PlacedChild> = branches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            PlacedChild::of(
                &format!("s-parent-sub-{i}"),
                "the ask",
                &b.placement(&base_sha),
            )
            .expect("recorded")
        })
        .collect();
    let runner = fx.runner(placed);
    runner.enqueue_finished("s-parent-sub-0");
    runner.enqueue_finished("s-parent-sub-1");
    let q = worker(&fx, &root);
    passes(&q, 1);
    let m0 = accept(&fx, "s-parent-sub-0");
    let m1 = accept(&fx, "s-parent-sub-1");
    passes(&q, 3);
    assert_eq!(
        fx.store().merge_row(&m0).unwrap().unwrap().state,
        MergeState::Landed
    );
    let second = fx.store().merge_row(&m1).unwrap().unwrap();
    assert_eq!(second.state, MergeState::Conflict, "{second:?}");
    assert!(
        second.evidence.contains("does not rebase"),
        "{}",
        second.evidence
    );
    assert!(
        branches[1].worktree.exists(),
        "a conflict keeps the branch's own worktree — the evidence stays"
    );
}

/// **A child of the main tree leaves no row** — the rule `start_worktree` records by, and the
/// enqueue's own guard.
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
    assert!(PlacedChild::of("s-parent-sub-1", "work in the main tree", &main_tree).is_none());
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
    assert!(why.contains("main checkout"), "{why}");
    runner.enqueue_finished("s-parent-sub-1");
    assert!(fx.store().review_rows(None).expect("reads").is_empty());
    assert!(!heard_about(&fx.parent, "s-parent-sub-1"));
    assert!(warning(&fx.parent, "merge_not_queued").is_none());
}

/// **A second finish of one child is one row.**
#[test]
fn a_second_finish_of_one_child_is_one_row() {
    let Some(fx) = Fixture::new("twice") else {
        return;
    };
    let (_root, base_sha, branches) =
        repo_with_children(fx.dir.path(), &[(SLUG, "ONE\ntwo\nthree\n")]);
    let handle = "s-parent-sub-1";
    let placed = PlacedChild::of(handle, "the ask", &branches[0].placement(&base_sha))
        .expect("a worktree child is recorded");
    let runner = fx.runner(vec![placed]);
    runner.enqueue_finished(handle);
    runner.enqueue_finished(handle);
    let rows = fx.store().review_rows(None).expect("reads");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].id, handle);
}

/// **A daemon with no `--store` refuses by name rather than dropping the branch.**
#[test]
fn a_daemon_with_no_store_refuses_by_name() {
    let Some(mut fx) = Fixture::new("no-store") else {
        return;
    };
    let (root, base_sha, branches) =
        repo_with_children(fx.dir.path(), &[(SLUG, "ONE\ntwo\nthree\n")]);
    let child = &branches[0];
    let handle = "s-parent-sub-1";
    fx.cfg.store = None;
    let placed = PlacedChild::of(handle, "the ask", &child.placement(&base_sha))
        .expect("a worktree child is recorded");
    let runner = fx.runner(vec![placed]);
    let why = runner
        .finished(handle)
        .expect_err("a daemon with no queue must not answer `queued`");
    assert!(
        why.contains("--store") && why.contains(&child.branch),
        "{why}"
    );
    runner.enqueue_finished(handle);
    let said = warning(&fx.parent, "merge_not_queued").expect("said out loud");
    assert!(said.contains("--store"), "{said}");
    assert!(warning(&fx.parent, "merge_queued").is_none());
    assert!(child.worktree.exists());
    assert!(git_ok(
        &root,
        &["rev-parse", "--verify", "--quiet", &child.branch]
    ));
}

/// **Decision 4's per-project option, at the end that reaches a model**: a review the queue
/// could not run, handed to the project's main session, is TOLD to that session — once, naming
/// the row, the branch, the worktree and the operator's door. A review merely BLOCKED (the
/// default: raise only) tells the project nothing.
#[test]
fn a_review_handed_to_the_project_is_told_once_and_a_blocked_one_is_not() {
    let Some(f) = Fixture::new("handover") else {
        return;
    };
    let s = f.store();
    for (id, handed) in [("m-handed", true), ("m-blocked", false)] {
        s.enqueue_review(
            &NewReview {
                id: id.into(),
                repository: "/the/repository".into(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: "s-parent".into(),
                branch: format!("agent/{id}"),
                worktree: Some(format!("/w/worktrees/{id}")),
                base_sha: "base".into(),
                head_sha: "head".into(),
                brief: "the ask".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: String::new(),
            },
            "test",
            1,
        )
        .expect("the row");
        let why = if handed {
            format!(
                "{}: the review could not run (no reviewer could start)",
                crate::mergequeue::HANDED_TO_PROJECT
            )
        } else {
            "no reviewer could start".to_string()
        };
        assert!(
            s.move_review(
                id,
                ReviewState::Waiting,
                ReviewState::Blocked,
                &Move::new("queue", why).set("failure", "no reviewer could start"),
                2,
            )
            .unwrap()
        );
    }
    let mut sessions = Sessions::open_first(&f.parts, f.cfg.clone(), f.registry.clone())
        .expect("the session opens");
    let project = sessions
        .held("s-parent")
        .expect("the project's own session");
    let notices = project.handover_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    let said = &notices[0];
    assert!(
        said.contains("`agent/m-handed`") && said.contains("`m-handed`"),
        "{said}"
    );
    assert!(said.contains("/w/worktrees/m-handed"), "{said}");
    assert!(said.contains("/queue approve m-handed"), "{said}");
    assert!(project.handover_notices().is_empty(), "said ONCE");
}

/// **A red target is TOLD to the project's root session's model — once, the failing step and the
/// end of its output first** (R76 item 1). FAILS before R76: `target_red` was a warning on the
/// hubs and nothing the session's model reads; pg-noop's agent never heard its `main` was red.
/// The review of the red tip, blocked, is told the same way; a subagent and the review queue's own
/// session are told nothing.
#[test]
fn a_red_target_is_told_to_the_projects_root_session_once_with_the_failing_step_first() {
    use letibot_tokencore::store::{HealthState, MergeGateOutcome, MergeGateStep, TargetHealth};
    let Some(f) = Fixture::new("red-told") else {
        return;
    };
    let (root, _base, _) = repo_with_children(f.dir.path(), &[]);
    let repository = std::fs::canonicalize(&root).unwrap().display().to_string();
    let sha = "8b9366aa12ffffffffffffffffffffffffffffff".to_string();
    let s = f.store();
    s.set_target_health(
        &TargetHealth {
            repository: repository.clone(),
            target: "main".into(),
            sha: sha.clone(),
            state: HealthState::Red,
            checked_ms: 1,
            evidence: "`scripts/merge_gate.sh` failed".into(),
            gate_steps: vec![
                MergeGateStep {
                    command: "cargo fmt --check".into(),
                    outcome: MergeGateOutcome::Passed,
                    output: String::new(),
                    started_ms: 0,
                    elapsed_ms: 0,
                },
                MergeGateStep {
                    command: "scripts/merge_gate.sh".into(),
                    outcome: MergeGateOutcome::Failed,
                    output: "running: cargo test\nmerge gate: FAIL: the corpus got worse".into(),
                    started_ms: 0,
                    elapsed_ms: 0,
                },
            ],
        },
        "test",
    )
    .unwrap();
    s.enqueue_review(
        &NewReview {
            id: "red-main-old".into(),
            repository: repository.clone(),
            target: "main".into(),
            kind: ReviewKind::Target,
            session_id: String::new(),
            branch: "main".into(),
            worktree: None,
            base_sha: String::new(),
            head_sha: sha.clone(),
            brief: "red".into(),
            priority: MergePriority::Urgent,
            fixes: None,
            evidence: String::new(),
        },
        "test",
        1,
    )
    .unwrap();
    assert!(
        s.move_review(
            "red-main-old",
            ReviewState::Waiting,
            ReviewState::Blocked,
            &Move::new("queue", "blocked")
                .set("failure", "git checkout: fatal: unable to read tree"),
            2,
        )
        .unwrap()
    );
    let mut sessions = Sessions::open_first(&f.parts, f.cfg.clone(), f.registry.clone())
        .expect("the session opens");
    let project = sessions
        .held("s-parent")
        .expect("the project's own session");
    let notices = project.queue_emergency_notices();
    assert_eq!(notices.len(), 2, "{notices:#?}");
    let red = &notices[0];
    assert!(red.starts_with("EMERGENCY"), "{red}");
    let step = red
        .find("`scripts/merge_gate.sh`")
        .expect("the failing step");
    let tail = red.find("FAIL: the corpus got worse").expect("its tail");
    let policy = red.find("Nothing lands").expect("then the policy");
    assert!(
        step < tail && tail < policy,
        "the step and its tail FIRST: {red}"
    );
    assert!(red.contains("`queue`"), "how to get out: {red}");
    let blocked = &notices[1];
    assert!(
        blocked.contains("red-main-old") && blocked.contains("unable to read tree"),
        "{blocked}"
    );
    assert!(project.queue_emergency_notices().is_empty(), "told ONCE");
    // **A restart forgets nothing it said** (R92.7): the daemon's memory gone, the store's
    // `queue_told` rows are what keep it once.
    project.gate_told.clear();
    assert!(
        project.queue_emergency_notices().is_empty(),
        "told once across a restart too"
    );
    // **Green again is said once** (R92.6), and only to a session that was told it was red.
    let mut green = s.target_health(&repository, "main").unwrap().unwrap();
    green.state = HealthState::Green;
    s.set_target_health(&green, "test").unwrap();
    let said = project.queue_emergency_notices();
    assert_eq!(said.len(), 1, "{said:#?}");
    assert!(said[0].contains("green again"), "{}", said[0]);
    assert!(project.queue_emergency_notices().is_empty(), "once");
    // **And the daemon's door rings exactly the repository's root** — `s-elsewhere` is held by
    // the same registry and is no session of this repository.
    let door = crate::mergequeue::RegistryDoor {
        registry: f.registry.clone(),
        store_path: f.db.clone(),
    };
    let rang = crate::mergequeue::serve::QueueDoor::ring_repository(&door, &repository)
        .expect("the project's root is rung");
    assert!(
        rang.contains("s-parent") && !rang.contains("s-elsewhere"),
        "{rang}"
    );
    assert!(
        crate::mergequeue::serve::QueueDoor::ring_repository(&door, "/no/such/repository").is_err(),
        "a repository with no root here rings nobody, and says so"
    );
}

// ===== A refusal, at the end that reaches a model =====

/// **A `--no-queue` daemon tells its sessions' models nothing about the queue** — not the red
/// target, not the target with no gate, not a branch handed back — and the store in this test is
/// the same store a daemon that DOES serve the queues is told from, so the assertions are about
/// the flag and not about a store with nothing in it.
///
/// The defect (`docs/claude-code-bridge.md`, 2026-10-26): the daemon the Claude Code bridge
/// started for package P1 in a fresh worktree of this repository is one of its session daemons by
/// the queue's own rule, so it took letibot's lease and put an EMERGENCY in front of the package's
/// model before its brief. [`Harness::queue_emergency_notices`], [`Harness::queue_notices`] and
/// [`Harness::handover_notices`] are every model-facing queue notice there is, and this is the
/// one place all three are checked against the flag.
///
/// No vocabulary is needed: a session with no GGUF and no provider opens on the byte vocabulary
/// (`Parts::load`), and this test runs no turn.
#[test]
fn a_no_queue_daemon_tells_its_sessions_nothing_about_the_queues() {
    use letibot_tokencore::store::{HealthState, MergeGateOutcome, MergeGateStep, TargetHealth};
    let dir = TempDir::new("letibot-no-queue-notices");
    let db = dir.path().join("sessions.db");
    let (root, _base, _) = repo_with_children(dir.path(), &[]);
    let repository = std::fs::canonicalize(&root).unwrap().display().to_string();
    let mut cfg = Config::for_this_box(dir.path());
    cfg.store = Some(db.clone());
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no apparatus");
    let s = Store::open(&db).expect("the store opens");

    // A red target, with the failing step a real gate would have left.
    s.set_target_health(
        &TargetHealth {
            repository: repository.clone(),
            target: "main".into(),
            sha: "8b9366aa12ffffffffffffffffffffffffffffff".into(),
            state: HealthState::Red,
            checked_ms: 1,
            evidence: "`scripts/merge_gate.sh` failed".into(),
            gate_steps: vec![MergeGateStep {
                command: "scripts/merge_gate.sh".into(),
                outcome: MergeGateOutcome::Failed,
                output: "merge gate: FAIL".into(),
                started_ms: 0,
                elapsed_ms: 0,
            }],
        },
        "test",
    )
    .unwrap();
    // And a second target with no gate declared, which is `Harness::queue_notices`'s notice.
    s.set_target_health(
        &TargetHealth {
            repository: repository.clone(),
            target: "next".into(),
            sha: "1111111111111111111111111111111111111111".into(),
            state: HealthState::Blocked,
            checked_ms: 2,
            evidence: format!(
                "{}: no `## Merge gate` section in AGENTS.md",
                crate::mergequeue::WAITING_FOR_GATE
            ),
            gate_steps: vec![],
        },
        "test",
    )
    .unwrap();
    // One session's rows each: the waiting one, and one the queue handed back to it.
    let rows = |session: &str| {
        let waiting = format!("waiting-{session}");
        s.enqueue_review(
            &NewReview {
                id: waiting.clone(),
                repository: repository.clone(),
                target: "next".into(),
                kind: ReviewKind::Branch,
                session_id: session.into(),
                branch: format!("agent/{session}"),
                worktree: None,
                base_sha: "base".into(),
                head_sha: "head".into(),
                brief: "the ask".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: String::new(),
            },
            "test",
            1,
        )
        .unwrap();
        let handed = format!("handed-{session}");
        s.enqueue_review(
            &NewReview {
                id: handed.clone(),
                repository: repository.clone(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: session.into(),
                branch: format!("agent/{session}-handed"),
                worktree: None,
                base_sha: "base".into(),
                head_sha: "head".into(),
                brief: "the ask".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: String::new(),
            },
            "test",
            1,
        )
        .unwrap();
        assert!(
            s.move_review(
                &handed,
                ReviewState::Waiting,
                ReviewState::Blocked,
                &Move::new(
                    "queue",
                    format!(
                        "{}: no reviewer could start",
                        crate::mergequeue::HANDED_TO_PROJECT
                    )
                ),
                2,
            )
            .unwrap()
        );
    };
    rows("s-pkg");
    rows("s-proj");

    // The package's daemon first: while every notice below is still DUE in this store.
    let pkg_registry = SessionRegistry::new();
    pkg_registry
        .create("s-pkg", "", SessionWiring::default())
        .expect("the package's session registers");
    let mut pkg_cfg = cfg.clone();
    pkg_cfg.session_id = "s-pkg".into();
    pkg_cfg.serve_queues = false;
    let mut package =
        Sessions::open_first(&parts, pkg_cfg, pkg_registry).expect("the package's session opens");
    {
        let package = package.held("s-pkg").expect("the package's session");
        assert!(
            package.queue_emergency_notices().is_empty(),
            "a --no-queue daemon tells its model nothing about a red main"
        );
        assert!(
            package.queue_notices().is_empty(),
            "nor that its target has no gate"
        );
        assert!(
            package.handover_notices().is_empty(),
            "nor that a branch was handed back to it"
        );
    }

    // The same store, a daemon that serves the queues: all three ARE due, so the three
    // assertions above are the flag's and not the fixture's.
    let proj_registry = SessionRegistry::new();
    proj_registry
        .create("s-proj", "", SessionWiring::default())
        .expect("the project's session registers");
    let mut proj_cfg = cfg.clone();
    proj_cfg.session_id = "s-proj".into();
    let mut project =
        Sessions::open_first(&parts, proj_cfg, proj_registry).expect("the project's session opens");
    let project = project.held("s-proj").expect("the project's session");
    let said = project.queue_emergency_notices();
    assert_eq!(said.len(), 1, "{said:#?}");
    assert!(said[0].starts_with("EMERGENCY"), "{}", said[0]);
    let no_gate = project.queue_notices();
    assert_eq!(no_gate.len(), 1, "{no_gate:#?}");
    assert!(
        no_gate[0].contains("no `Merge gate` section"),
        "{}",
        no_gate[0]
    );
    let handed = project.handover_notices();
    assert_eq!(handed.len(), 1, "{handed:#?}");
    assert!(handed[0].contains("agent/s-proj-handed"), "{}", handed[0]);
}

/// **A review row of the child `s-parent-sub-1`, under this fixture's `s-parent`**, with the
/// child's session row naming its parent — the root is only reachable by walking the store, so a
/// row written as its own root would pass whether the walk worked or not.
fn a_childs_review(s: &Store, id: &str, branch: &str, worktree: &str) {
    s.put_session(&letibot_tokencore::store::SessionRecord {
        id: id.into(),
        title: None,
        model_id: String::new(),
        dialect_sha: String::new(),
        workspace_root: String::new(),
        owner: String::new(),
        approvers: Vec::new(),
        role: None,
        parent_session_id: Some("s-parent".into()),
    })
    .expect("the child's session row");
    s.enqueue_review(
        &NewReview {
            id: id.into(),
            repository: "/the/repository".into(),
            target: "main".into(),
            kind: ReviewKind::Branch,
            session_id: id.into(),
            branch: branch.into(),
            worktree: Some(worktree.into()),
            base_sha: "base".into(),
            head_sha: "head".into(),
            brief: "the ask".into(),
            priority: MergePriority::Subagent,
            fixes: None,
            evidence: String::new(),
        },
        "test",
        1,
    )
    .expect("the row");
}

/// **A reviewer's `reject`, handed in at the desk and served** by the review pool, whose
/// `deliver` is exactly what `Harness::serve_reviews` wires: [`HarnessTaskRunner::send`] on the
/// daemon's registry. Three passes, so a delivery that repeated would be seen.
fn refuse(fx: &Fixture, id: &str, reasons: &[&str]) {
    let s = fx.store();
    assert!(
        s.move_review(
            id,
            ReviewState::Waiting,
            ReviewState::Reviewing,
            &Move::new("e2e", "claimed")
                .set("owner", "e2e")
                .set("reviewer", "s-gk-sub-1"),
            2,
        )
        .unwrap()
    );
    let desk = letibot_tools::builtins::verdict::ReviewDesk::new();
    desk.opened(
        "s-gk",
        "s-gk-sub-1",
        id,
        letibot_tools::gatekeeper::ReviewRequest {
            brief: "the ask".into(),
            branch: format!("agent/{id}"),
            base_sha: "base".into(),
            head_sha: "head".into(),
            cwd: String::new(),
        },
    );
    desk.submit(
        "s-gk-sub-1",
        letibot_tools::gatekeeper::Submission {
            decision: letibot_tools::gatekeeper::Decision::Reject,
            reasons: reasons.iter().map(|r| r.to_string()).collect(),
            looked_at: Default::default(),
        },
    )
    .expect("the verdict is taken");
    let runner = fx.runner(Vec::new());
    let deliver = |to: &str, text: &str| runner.send(to, text);
    let registry = fx.registry.clone();
    let events = move |e: SessionEvent| {
        registry.broadcast(e);
    };
    let settings = |_: &Path| Ok(crate::mergequeue::settings::QueueSettings::default());
    let pool = crate::mergequeue::review::Pool {
        store: &s,
        runner: &runner,
        desk: &desk,
        host: "s-gk",
        me: "e2e",
        placement: crate::sessions::gatekeeper::ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, e| events(e),
        deliver: &deliver,
        settings: &settings,
        clones: std::env::temp_dir(),
        slots: &Vec::new,
    };
    for _ in 0..3 {
        crate::mergequeue::review::serve(&pool).expect("a pass");
    }
    assert_eq!(
        s.review_row(id).unwrap().unwrap().state,
        ReviewState::Rejected
    );
}

/// **Every message a hub's queue holds** — drained, which is what the session's own reader does
/// with it: a `Message` here is a turn the model runs about it.
fn queued_messages(hub: &Hub) -> Vec<String> {
    std::iter::from_fn(|| hub.try_command())
        .filter_map(|c| match c.kind {
            letibot_sessionlog::CommandKind::Message { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

/// **A refusal reaches the LIVE child that wrote the branch as a message on its own queue** — the
/// door `task_message` uses, so the child's model hears the reviewer's reasons and nothing else
/// does. Once, over three passes.
#[test]
fn a_refusal_is_queued_to_the_live_child_that_wrote_the_branch() {
    let Some(fx) = Fixture::new("refusal-child") else {
        return;
    };
    let child = fx
        .registry
        .create("s-parent-sub-1", "", SessionWiring::default())
        .expect("the child registers");
    a_childs_review(
        &fx.store(),
        "s-parent-sub-1",
        "agent/s-parent-sub-1",
        "/w/worktrees/the-child",
    );
    refuse(
        &fx,
        "s-parent-sub-1",
        &["the retry is not bounded: `loop` can run for ever"],
    );

    let heard = queued_messages(&child);
    assert_eq!(heard.len(), 1, "one refusal is one message: {heard:?}");
    for asked in [
        "Your branch `agent/s-parent-sub-1` was REFUSED",
        "the retry is not bounded: `loop` can run for ever",
        "/w/worktrees/the-child",
    ] {
        assert!(
            heard[0].contains(asked),
            "`{asked}` is missing from: {}",
            heard[0]
        );
    }
    assert!(
        queued_messages(&fx.parent).is_empty(),
        "the root is the FALLBACK, not a second audience"
    );
    assert!(queued_messages(&fx.elsewhere).is_empty());
    assert!(warning(&fx.parent, "refusal_undelivered").is_none());
}

/// **When the child's session has closed, the refusal is queued to the session that ENQUEUED it**
/// — the project's root, found by walking the store — as a message its model runs a turn about,
/// not a pane row and not a `!` line on a head. This is the case of 2026-10-11: the child that
/// finished is the party most likely to be gone.
#[test]
fn a_refusal_is_queued_to_the_root_when_the_child_has_closed() {
    let Some(fx) = Fixture::new("refusal-root") else {
        return;
    };
    // The child's session ended: registered once, closed since.
    let child = fx
        .registry
        .create("s-parent-sub-1", "", SessionWiring::default())
        .expect("the child registers");
    child.close();
    a_childs_review(
        &fx.store(),
        "s-parent-sub-1",
        "agent/s-parent-sub-1",
        "/w/worktrees/the-child",
    );
    refuse(
        &fx,
        "s-parent-sub-1",
        &["the test it added asserts nothing"],
    );

    let heard = queued_messages(&fx.parent);
    assert_eq!(heard.len(), 1, "one delivery, to the root: {heard:?}");
    for asked in [
        "a child you started (`s-parent-sub-1`)",
        "could not be told",
        "the test it added asserts nothing",
        "/w/worktrees/the-child",
        "`/queue approve s-parent-sub-1`",
    ] {
        assert!(
            heard[0].contains(asked),
            "`{asked}` is missing from: {}",
            heard[0]
        );
    }
    assert!(queued_messages(&fx.elsewhere).is_empty());
    assert!(warning(&fx.parent, "refusal_undelivered").is_none());
}

/// **A review the queue could not run, of a CHILD's branch, under `merge_via_session`, reaches the
/// child's ROOT** — the queue rings the project session the row's tree walks up to (the daemon's
/// own door, [`crate::mergequeue::RegistryDoor`]), and that session's model is told once.
/// `a_review_handed_to_the_project_is_told_once_and_a_blocked_one_is_not` writes the row as the
/// root's own; this is the row as `enqueue_finished` writes it.
#[test]
fn a_blocked_childs_review_is_rung_and_told_to_its_root_once() {
    let Some(fx) = Fixture::new("handover-child") else {
        return;
    };
    let (root, _base_sha, branches) =
        repo_with_children(fx.dir.path(), &[(SLUG, "ONE\ntwo\nthree\n")]);
    let repo = std::fs::canonicalize(&root).unwrap();
    let handle = "s-parent-sub-1";
    let s = fx.store();
    // The project session is seated in the repository: a queue's raise reaches only the hubs of
    // that repository's sessions (and the queue's board), never every hub of the daemon.
    s.put_session(&letibot_tokencore::store::SessionRecord {
        id: "s-parent".into(),
        title: None,
        model_id: String::new(),
        dialect_sha: String::new(),
        workspace_root: repo.display().to_string(),
        owner: String::new(),
        approvers: Vec::new(),
        role: None,
        parent_session_id: None,
    })
    .expect("the project session's row");
    a_childs_review(
        &s,
        handle,
        &branches[0].branch,
        &branches[0].worktree.display().to_string(),
    );
    s.move_review(
        handle,
        ReviewState::Waiting,
        ReviewState::Waiting,
        &Move::new("e2e", "resolved")
            .set("repository", repo.display().to_string())
            .set("head_sha", branches[0].sha.clone()),
        2,
    )
    .unwrap();
    let me = LeaseClaim {
        owner: "e2e/1/1".into(),
        pid: 1,
        host: "e2e".into(),
        build: "e2e".into(),
    };
    s.acquire_queue_lease(&repo.display().to_string(), &me, 1, u64::MAX / 2)
        .expect("the lease");
    let q = RepoQueue::new(
        s,
        repo.clone(),
        Arc::new(QueueEnv {
            me,
            gate: Arc::new(GreenGate),
            door: Arc::new(crate::mergequeue::RegistryDoor {
                registry: fx.registry.clone(),
                store_path: fx.db.clone(),
            }),
            settings: Arc::new(|_| {
                Ok(crate::mergequeue::settings::QueueSettings {
                    merge_via_session: true,
                    ..Default::default()
                })
            }),
            landing_root: crate::mergequeue::serve::tests::test_landing_root(),
            gate_deadline: std::time::Duration::from_secs(60),
            review_deadline_ms: crate::mergequeue::serve::REVIEW_DEADLINE_MS,
            // Nobody starts a reviewer here, so the claim is blocked on the next pass.
            reviewer_start_grace_ms: 0,
            idle: std::time::Duration::from_millis(10),
        }),
    );
    passes(&q, 1);
    std::thread::sleep(std::time::Duration::from_millis(5));
    passes(&q, 3);
    let r = fx.store().review_row(handle).unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Blocked, "{r:?}");
    assert!(
        r.evidence.starts_with(crate::mergequeue::HANDED_TO_PROJECT)
            && r.evidence
                .contains("the project session `s-parent` was rung"),
        "the ROOT was rung, not the child the row names: {}",
        r.evidence
    );
    let blocked = warning(&fx.parent, "queue_blocked").expect("raised for the operator");
    assert!(blocked.contains("which is told next"), "{blocked}");

    let mut sessions = Sessions::open_first(&fx.parts, fx.cfg.clone(), fx.registry.clone())
        .expect("the session opens");
    let project = sessions
        .held("s-parent")
        .expect("the project's own session");
    let notices = project.handover_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains(&format!("`{}`", branches[0].branch))
            && notices[0].contains(&format!("/queue approve {handle}")),
        "{}",
        notices[0]
    );
    assert!(project.handover_notices().is_empty(), "said ONCE");
}
