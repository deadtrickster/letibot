//! **The queues against real git repositories** — every requirement of R61 with a test that would
//! fail on the v23 queue. Temp repositories and temp stores only: no model, no daemon, and never
//! `~/.local/share/letibot/sessions.db`.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{
    Approval, HealthState, LeaseClaim, LeaseOutcome, MergePriority, MergeState, Move, NewReview,
    ReviewKind, ReviewState, Store,
};
use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus};
use letibot_tools::builtins::verdict::ReviewDesk;
use letibot_tools::gatekeeper::{Decision, LookedAt, ReviewRequest, Submission};

use super::*;
use crate::mergequeue::gate::{GateControl, GateRun, GateRunner, GreenGate, LocalGate};
use crate::mergequeue::settings::QueueSettings;
use crate::sessions::gatekeeper::ReviewerPlacement;

// ===== Fixtures =====

pub(crate) struct TempDir(pub(crate) PathBuf);

impl TempDir {
    pub(crate) fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-r61-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(std::fs::canonicalize(&p).unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn sh(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

const GATE_MD: &str = "# agents\n\n## Merge gate\n\n```sh\ntest ! -f broken\n```\n";

/// **A repository with a gate**: `main` carries `AGENTS.md` whose gate is red while a file named
/// `broken` exists — so a test turns the target red with one commit.
pub(crate) fn repo(under: &Path) -> PathBuf {
    let repo = under.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    sh(&repo, &["config", "user.email", "q@test"]);
    sh(&repo, &["config", "user.name", "queue test"]);
    std::fs::write(repo.join("AGENTS.md"), GATE_MD).unwrap();
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    sh(&repo, &["add", "."]);
    sh(&repo, &["commit", "-q", "-m", "base"]);
    std::fs::canonicalize(repo).unwrap()
}

pub(crate) fn commit(dir: &Path, file: &str, text: &str, msg: &str) -> String {
    std::fs::write(dir.join(file), text).unwrap();
    sh(dir, &["add", "-A"]);
    sh(dir, &["commit", "-q", "-m", msg]);
    sh(dir, &["rev-parse", "HEAD"])
}

/// A branch in a worktree of its own (what `task_start` makes), with one commit on it.
pub(crate) fn branch(repo: &Path, name: &str, file: &str) -> (PathBuf, String, String) {
    let wt = repo.parent().unwrap().join(format!("wt-{name}"));
    let base = sh(repo, &["rev-parse", "main"]);
    sh(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            &wt.to_string_lossy(),
            "main",
        ],
    );
    let head = commit(&wt, file, &format!("{name}\n"), &format!("work on {name}"));
    (wt, base, head)
}

#[derive(Default)]
pub(crate) struct Door {
    pub(crate) events: Mutex<Vec<SessionEvent>>,
    pub(crate) rung: Mutex<usize>,
    pub(crate) projects: Mutex<Vec<String>>,
    pub(crate) repositories: Mutex<Vec<String>>,
    /// `(repository, code)` of every warning published for one repository only.
    pub(crate) scoped: Mutex<Vec<(String, String)>>,
    /// The sessions [`QueueDoor::session_live`] answers `true` for.
    pub(crate) live: Mutex<Vec<String>>,
}

impl QueueDoor for Door {
    fn announce(&self, event: SessionEvent) {
        self.events.lock().unwrap().push(event);
    }
    fn announce_to(&self, repository: &str, event: SessionEvent) {
        if let SessionEvent::Warning { code, .. } = &event {
            self.scoped
                .lock()
                .unwrap()
                .push((repository.to_string(), code.clone()));
        }
        self.events.lock().unwrap().push(event);
    }
    fn ring_reviewers(&self) {
        *self.rung.lock().unwrap() += 1;
    }
    fn ring_project(&self, session_id: &str) -> Result<String, String> {
        self.projects.lock().unwrap().push(session_id.into());
        Ok(format!("rang {session_id}"))
    }
    fn ring_repository(&self, repository: &str) -> Result<String, String> {
        self.repositories.lock().unwrap().push(repository.into());
        Ok(format!("rang the roots of {repository}"))
    }
    fn session_live(&self, session_id: &str) -> bool {
        self.live.lock().unwrap().iter().any(|s| s == session_id)
    }
    /// This door has a list of live sessions, so a reviewer absent from it is a reviewer gone.
    fn sees_sessions(&self) -> bool {
        true
    }
}

impl Door {
    pub(crate) fn warnings(&self, code: &str) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                SessionEvent::Warning {
                    code: c, detail, ..
                } if c == code => Some(detail.clone()),
                _ => None,
            })
            .collect()
    }
}

pub(crate) fn me(tag: &str) -> LeaseClaim {
    LeaseClaim {
        owner: format!("test/{tag}"),
        pid: std::process::id(),
        host: "test".into(),
        build: "test".into(),
    }
}

/// The tests' landing root: beside the repository, inside the test's own temporary directory,
/// so a test never writes into the box's real `~/.local/share/letibot/landing`.
pub(crate) fn test_landing_root() -> crate::mergequeue::serve::LandingRootFor {
    Arc::new(|repo: &Path| {
        Ok(repo
            .parent()
            .map(|p| p.join("landing"))
            .unwrap_or_else(|| repo.join("landing")))
    })
}

pub(crate) fn env(gate: Arc<dyn GateRunner>, door: Arc<Door>, who: &str) -> Arc<QueueEnv> {
    env_with(gate, door, who, QueueSettings::default())
}

pub(crate) fn env_with(
    gate: Arc<dyn GateRunner>,
    door: Arc<Door>,
    who: &str,
    settings: QueueSettings,
) -> Arc<QueueEnv> {
    env_with_desk(gate, door, who, settings, None)
}

/// [`env_with`], with the review desk the worker asks about a reviewer that is no longer alive —
/// `None` for a queue whose daemon has no desk to ask (every test that starts no reviewer).
pub(crate) fn env_with_desk(
    gate: Arc<dyn GateRunner>,
    door: Arc<Door>,
    who: &str,
    settings: QueueSettings,
    desk: Option<Arc<letibot_tools::builtins::verdict::ReviewDesk>>,
) -> Arc<QueueEnv> {
    Arc::new(QueueEnv {
        me: me(who),
        gate,
        door,
        desk,
        settings: Arc::new(move |_| Ok(settings.clone())),
        landing_root: test_landing_root(),
        gate_deadline: Duration::from_secs(60),
        review_deadline_ms: REVIEW_DEADLINE_MS,
        reviewer_start_grace_ms: REVIEWER_START_GRACE_MS,
        idle: Duration::from_millis(50),
    })
}

struct World {
    dir: TempDir,
    repo: PathBuf,
    db: PathBuf,
    door: Arc<Door>,
}

impl World {
    fn new(tag: &str) -> Self {
        let dir = TempDir::new(tag);
        let repo = repo(&dir.0);
        let db = dir.0.join("sessions.db");
        World {
            repo,
            db,
            dir,
            door: Arc::new(Door::default()),
        }
    }
    fn store(&self) -> Store {
        Store::open(&self.db).unwrap()
    }
    fn key(&self) -> String {
        self.repo.display().to_string()
    }
    /// A worker that holds the lease, as the supervisor would hand it one.
    fn worker(&self, gate: Arc<dyn GateRunner>, who: &str) -> RepoQueue {
        self.worker_with(gate, who, QueueSettings::default())
    }
    fn worker_with(
        &self,
        gate: Arc<dyn GateRunner>,
        who: &str,
        settings: QueueSettings,
    ) -> RepoQueue {
        self.worker_holding(gate, who, settings, None)
    }
    /// [`World::worker_with`], with a review desk the worker can ask — see
    /// [`QueueEnv::desk`].
    fn worker_holding(
        &self,
        gate: Arc<dyn GateRunner>,
        who: &str,
        settings: QueueSettings,
        desk: Option<Arc<letibot_tools::builtins::verdict::ReviewDesk>>,
    ) -> RepoQueue {
        let env = env_with_desk(gate, self.door.clone(), who, settings, desk);
        let store = self.store();
        assert!(matches!(
            store
                .acquire_queue_lease(&self.key(), &env.me, gate::now_ms(), LEASE_TTL_MS)
                .unwrap(),
            LeaseOutcome::Held { .. }
        ));
        RepoQueue::new(store, self.repo.clone(), env)
    }
    fn enqueue(&self, name: &str, wt: &Path, base: &str, head: &str) {
        self.store()
            .enqueue_review(
                &NewReview {
                    id: name.into(),
                    repository: self.key(),
                    target: "main".into(),
                    kind: ReviewKind::Branch,
                    session_id: format!("s-{name}"),
                    branch: format!("agent/{name}"),
                    worktree: Some(wt.display().to_string()),
                    base_sha: base.into(),
                    head_sha: head.into(),
                    brief: format!("do {name}"),
                    priority: MergePriority::Subagent,
                    fixes: None,
                    evidence: "enqueued".into(),
                },
                "test",
                gate::now_ms(),
            )
            .unwrap();
    }
    /// The reviewer's accept, as the review pool writes it — bound to the head the claim read.
    fn accept(&self, id: &str, owner: &str) -> String {
        let s = self.store();
        let r = s.review_row(id).unwrap().unwrap();
        assert_eq!(r.state, ReviewState::Reviewing, "{r:?}");
        // Unique per approval, as the review pool's is: a branch sent back to review is approved
        // again under a new merge row.
        let merge_id = match s.merge_row(&format!("m-{id}")).unwrap() {
            None => format!("m-{id}"),
            Some(_) => format!("m-{id}-{}", gate::now_ms()),
        };
        assert!(
            s.approve_review(
                id,
                &Approval {
                    expect: ReviewState::Reviewing,
                    expect_owner: Some(owner.into()),
                    reviewed_sha: r.head_sha.clone(),
                    by: "reviewer:sub-1".into(),
                    reasons: vec!["fine".into()],
                    files: vec![],
                    commands: vec![],
                    why: "accepted".into(),
                    merge_id: merge_id.clone(),
                },
                gate::now_ms(),
            )
            .unwrap()
        );
        merge_id
    }
    fn tip(&self) -> String {
        sh(&self.repo, &["rev-parse", "main"])
    }
}

fn run(q: &RepoQueue, passes: usize) {
    let stop = AtomicBool::new(false);
    for _ in 0..passes {
        assert_ne!(q.settle(&stop).unwrap(), Pass::LostLease);
    }
}

// ===== The landing =====

/// **A landing taken out of line while its gate runs does not land** — the safety half of *`rm` on a
/// `gating` row*.
///
/// The person's verb moves the row to `withdrawn`; the gate is on a thread of its own and finishes
/// afterwards, saying *passed*. Nothing about that may undo the person's word, and nothing about it
/// may move `main`: `finish_land` reads the row before it writes into the repository.
#[test]
fn a_landing_withdrawn_while_its_gate_runs_does_not_land() {
    /// A gate that waits until the test lets it go, so the row can be withdrawn WHILE it runs.
    ///
    /// **Only the LANDING's gate is held** — the branch's own file is in the tree it was handed,
    /// and the target's health gate runs on the target alone — and the wait has a deadline, so a
    /// test that fails before it lets go cannot wedge the whole binary.
    struct HeldGate {
        go: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }
    impl GateRunner for HeldGate {
        fn run(&self, wt: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
            if !wt.join("held.txt").exists() {
                return GreenGate.run(wt, steps, ctl);
            }
            let (lock, cv) = &*self.go;
            let mut open = lock.lock().unwrap();
            let until = std::time::Instant::now() + Duration::from_secs(30);
            while !*open {
                let left = until.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    break;
                }
                open = cv.wait_timeout(open, left).unwrap().0;
            }
            drop(open);
            GreenGate.run(wt, steps, ctl)
        }
        fn name(&self) -> String {
            "held (test)".into()
        }
    }
    let w = World::new("withdraw-gating");
    let (wt, base, head) = branch(&w.repo, "held", "held.txt");
    w.enqueue("held", &wt, &base, &head);
    let go = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let q = w.worker(Arc::new(HeldGate { go: go.clone() }), "a");
    let stop = AtomicBool::new(false);
    // **Passes, not `settle`** — a pass starts a gate and goes on; `settle` waits for a gate in
    // flight to end, and this one is held until the row has been withdrawn. The target is gated
    // first (its own health), so it takes a few passes to reach the landing.
    assert_ne!(q.pass(&stop).unwrap(), Pass::LostLease);
    let m = w.accept("held", "test/a");
    let mut gating = false;
    for _ in 0..12 {
        assert_ne!(q.pass(&stop).unwrap(), Pass::LostLease);
        if w.store().merge_row(&m).unwrap().unwrap().state == MergeState::Gating {
            gating = true;
            break;
        }
    }
    assert!(gating, "the premise: the landing's gate is running");
    let before = w.tip();
    // **The person takes it out of line** — the verb the pane offers on a `gating` row.
    let said = crate::mergequeue::verbs::remove(&w.store(), &m, "operator", &|_| {})
        .expect("a gating landing can be withdrawn");
    assert!(said.contains("withdrawn"), "{said}");
    // …and the gate finishes, having passed.
    *go.0.lock().unwrap() = true;
    go.1.notify_all();
    run(&q, 3);
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(
        row.state,
        MergeState::Withdrawn,
        "a verdict does not undo the person's word: {row:?}"
    );
    assert_eq!(w.tip(), before, "and nothing landed on main");
    assert!(
        !w.repo.join("held.txt").exists(),
        "the branch's work is not in the target's tree"
    );
}

/// **The whole path: claimed for review, approved bound to its head, rebased onto the target's
/// tip in the dedicated worktree, gated, landed by compare-and-swap, the operator's checkout
/// brought along, the branch and its worktree retired — and every move in the history.**
#[test]
fn an_approved_branch_lands_through_the_gate_in_the_queues_own_worktree() {
    let w = World::new("land");
    let (wt, base, head) = branch(&w.repo, "one", "one.txt");
    // main moves after the branch was cut, so the landing is a real rebase.
    let moved = commit(&w.repo, "a.txt", "two\n", "main moves");
    w.enqueue("one", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");

    run(&q, 1);
    let r = w.store().review_row("one").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Reviewing, "claimed: {r:?}");
    assert_eq!(r.owner, "test/a");
    assert_eq!(r.head_sha, head, "the claim binds the head it read");
    assert!(
        *w.door.rung.lock().unwrap() >= 1,
        "the reviewers' session was rung"
    );

    let m = w.accept("one", "test/a");
    run(&q, 2);
    let landed = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(landed.state, MergeState::Landed, "{landed:?}");
    let tip = w.tip();
    assert_eq!(landed.landed_sha.as_deref(), Some(tip.as_str()));
    assert_eq!(
        sh(&w.repo, &["rev-parse", "HEAD~1"]),
        moved,
        "rebased onto the tip main had, not merged over a stale base"
    );
    assert_eq!(
        sh(&w.repo, &["status", "--porcelain"]),
        "",
        "the operator's checkout of main followed the landing — its index is not the old tree"
    );
    assert!(
        w.repo.join("one.txt").exists(),
        "and its working tree has the work"
    );
    assert!(
        landed
            .gate_steps
            .iter()
            .any(|s| s.command == "test ! -f broken"),
        "the gate is the target's own: {:?}",
        landed.gate_steps
    );
    assert!(
        !wt.exists(),
        "the branch's own worktree is retired after the landing"
    );
    assert!(
        crate::mergequeue::git::branch_head(&w.repo, "agent/one").is_none(),
        "and the branch"
    );
    let health = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(health.state, HealthState::Green);
    assert_eq!(health.sha, tip, "the gate passed on exactly this tree");
    let history: Vec<String> = w
        .store()
        .transitions("merge", &m)
        .unwrap()
        .into_iter()
        .map(|t| t.to_state)
        .collect();
    assert_eq!(history, vec!["queued", "gating", "gating", "landed"]);
    assert!(
        !crate::mergequeue::git::checkouts_of(&w.repo, "main").is_empty(),
        "the landing never checked anything out in the operator's place"
    );
    // The operator, 2026-10-11: "the merges and branches shoudlnt land into .git".
    assert!(
        !w.repo.join(".git").join(git::OLD_LANDING_NAME).exists(),
        "nothing landed inside .git"
    );
    assert!(
        git::landing_dir_in(&w.dir.0.join("landing"), &w.repo)
            .join("one.txt")
            .exists(),
        "the landing tree is the queue's own, outside .git, at the landed tip"
    );
}

/// **The target moved during the gate → rebase and gate again, not fail** (requirement 3). The
/// v23 queue's `merge --ff-only` failed and parked the entry `failed` (defect 7).
#[test]
fn a_target_that_moves_during_the_gate_is_rebased_onto_and_gated_again() {
    /// A gate that, the first time it runs on a branch, lets somebody commit to main by hand.
    struct HandCommitDuringGate {
        repo: PathBuf,
        done: Mutex<bool>,
    }
    impl GateRunner for HandCommitDuringGate {
        fn run(&self, wt: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
            let on_branch = wt.join("x.txt").exists();
            let mut done = self.done.lock().unwrap();
            if on_branch && !*done {
                *done = true;
                commit(&self.repo, "hand.txt", "by hand\n", "a hand commit");
            }
            GreenGate.run(wt, steps, ctl)
        }
    }
    let w = World::new("moved-target");
    let (wt, base, head) = branch(&w.repo, "x", "x.txt");
    w.enqueue("x", &wt, &base, &head);
    let q = w.worker(
        Arc::new(HandCommitDuringGate {
            repo: w.repo.clone(),
            done: Mutex::new(false),
        }),
        "a",
    );
    run(&q, 1);
    let m = w.accept("x", "test/a");
    run(&q, 1);
    // The gates run on their own thread, so the pass that applies the moved target's verdict
    // goes straight on to gate the new tip and land again: the move back in line is read off
    // the history.
    let back: Vec<_> = w
        .store()
        .transitions("merge", &m)
        .unwrap()
        .into_iter()
        .filter(|t| t.from_state.as_deref() == Some("gating") && t.to_state != "gating")
        .collect();
    assert_eq!(
        back.first().map(|t| t.to_state.as_str()),
        Some("queued"),
        "back in line, not failed: {back:?}"
    );
    assert!(
        back[0].why.contains("moved during the gate"),
        "{}",
        back[0].why
    );
    run(&q, 3);
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Landed, "{row:?}");
    assert!(w.repo.join("hand.txt").exists() && w.repo.join("x.txt").exists());
    assert_eq!(
        sh(&w.repo, &["log", "--format=%s", "-3", "main"]),
        "work on x\na hand commit\nbase",
        "the hand commit is kept and the branch is on top of it"
    );
}

/// **The verdict is bound to the head it judged** (decision 6, defect 5): a branch that moved
/// after approval goes back to review; a rebase onto a newer main is not a move.
#[test]
fn a_branch_that_moves_after_approval_goes_back_to_review_and_a_rebase_does_not() {
    let w = World::new("bound");
    let (wt, base, head) = branch(&w.repo, "b", "b.txt");
    w.enqueue("b", &wt, &base, &head);
    let q = w.worker(Arc::new(GreenGate), "a");
    run(&q, 1);
    let m = w.accept("b", "test/a");
    // A commit after the approval: the change is not the one judged.
    let newer = commit(&wt, "b2.txt", "more\n", "unreviewed");
    run(&q, 1);
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Moved, "{row:?}");
    let r = w.store().review_row("b").unwrap().unwrap();
    assert!(
        matches!(r.state, ReviewState::Waiting | ReviewState::Reviewing),
        "back in review: {r:?}"
    );
    assert_eq!(r.head_sha, newer, "with the new head");
    assert_eq!(w.tip(), base, "and nothing landed");

    // Now a branch rebased onto a newer main, same change: it lands.
    let (wt2, base2, head2) = branch(&w.repo, "c", "c.txt");
    w.enqueue("c", &wt2, &base2, &head2);
    // Claim it (the first one still waits for a reviewer and holds one of two slots).
    run(&q, 1);
    let m2 = w.accept("c", "test/a");
    commit(&w.repo, "z.txt", "z\n", "main moves");
    sh(&wt2, &["rebase", "-q", "main"]);
    run(&q, 3);
    let row = w.store().merge_row(&m2).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Landed, "{}", row.evidence);
}

/// **Thirty minutes, then `timed out` — its own outcome, and the gate's whole process group is
/// killed** (decision 9, defect 6). Here the deadline is a second and the step sleeps a minute.
#[test]
fn a_gate_past_its_deadline_is_killed_and_timed_out_is_its_own_outcome() {
    let w = World::new("deadline");
    let pidfile = w.dir.0.join("sleeper.pid");
    std::fs::write(
        w.repo.join("AGENTS.md"),
        format!(
            "## Merge gate\n\n```sh\nsh -c 'echo $$ > {}; exec sleep 60'\n```\n",
            pidfile.display()
        ),
    )
    .unwrap();
    sh(&w.repo, &["commit", "-q", "-am", "a slow gate"]);
    let (wt, base, head) = branch(&w.repo, "slow", "s.txt");
    w.enqueue("slow", &wt, &base, &head);
    let mut e = env(Arc::new(LocalGate), w.door.clone(), "a");
    // **The deadline has to be longer than the gate's own SETUP, or the test measures the
    // machine.** The clock starts in `start_gate`, and before the step's first line runs the gate
    // makes its scratch directory and resolves the cgroup its step joins — and on a loaded box
    // that is more than a second: MEASURED 2026-10-30, this test failed the landing gate of
    // another branch (`crates/harnessd/src/mergequeue/serve/tests.rs:545`, `Result::unwrap()` on
    // `Os { code: 2, kind: NotFound }` reading `sleeper.pid`) because the deadline had passed
    // before the step existed to write it. Ten seconds is still six times shorter than the step's
    // own sleep, so what the test is about — a step still running when the deadline passes — is
    // unchanged; what is no longer asserted is that this box can start a shell in a second.
    Arc::get_mut(&mut e).unwrap().gate_deadline = Duration::from_secs(10);
    let store = w.store();
    store
        .acquire_queue_lease(&w.key(), &e.me, gate::now_ms(), LEASE_TTL_MS)
        .unwrap();
    let q = RepoQueue::new(store, w.repo.clone(), e);
    let started = std::time::Instant::now();
    run(&q, 1);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the deadline was kept"
    );
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::TimedOut, "{h:?}");
    assert!(
        h.gate_steps
            .iter()
            .any(|s| s.outcome == letibot_tokencore::store::MergeGateOutcome::TimedOut)
    );
    // The step writes this before it sleeps, so a `NotFound` here says the gate's setup outran
    // the deadline above — not that the step ran and was killed.
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .expect("the step started and wrote its pid before the deadline")
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only asks whether the pid exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "the step's process was killed with its group");
    // And nothing lands on a target nobody could judge.
    run(&q, 1);
    assert_eq!(w.tip(), sh(&w.repo, &["rev-parse", "HEAD"]));
    // R92.27: and it was said, on this repository's hubs, once.
    let said = w.door.warnings("target_timed_out");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("TIMED OUT"), "{}", said[0]);
    assert!(
        w.door
            .scoped
            .lock()
            .unwrap()
            .iter()
            .any(|(r, c)| r == &w.key() && c == "target_timed_out")
    );
}

/// **A target whose landing tree cannot be made ready is said** (R92.27): `target_unjudged`,
/// once — not again on the passes after it.
#[test]
fn a_target_whose_landing_tree_cannot_be_made_is_said_once() {
    let w = World::new("nolanding");
    let mut e = env(Arc::new(GreenGate), w.door.clone(), "a");
    Arc::get_mut(&mut e).unwrap().landing_root = Arc::new(|_| Err("no room at the inn".into()));
    let store = w.store();
    store
        .acquire_queue_lease(&w.key(), &e.me, gate::now_ms(), LEASE_TTL_MS)
        .unwrap();
    let q = RepoQueue::new(store, w.repo.clone(), e);
    run(&q, 3);
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Blocked, "{h:?}");
    let said = w.door.warnings("target_unjudged");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("no room at the inn"), "{}", said[0]);
}

/// **A merge row that does not rebase, or whose landing gate fails, is said** (R92.28).
#[test]
fn a_merge_row_that_conflicts_or_fails_its_gate_is_said() {
    let w = World::new("mergefail");
    // `c` edits a.txt; main then edits it too, so `c` no longer rebases.
    let (wt, base, _) = branch(&w.repo, "c", "c.txt");
    let head = commit(&wt, "a.txt", "theirs\n", "c edits a");
    w.enqueue("c", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let m = w.accept("c", "test/a");
    commit(&w.repo, "a.txt", "ours\n", "main edits a");
    run(&q, 3);
    assert_eq!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Conflict
    );
    let said = w.door.warnings("merge_conflict");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("agent/c"), "{}", said[0]);

    // `f` adds the file the gate refuses: its landing gate fails.
    let (wt, base, head) = branch(&w.repo, "f", "broken");
    w.enqueue("f", &wt, &base, &head);
    run(&q, 1);
    let m = w.accept("f", "test/a");
    run(&q, 2);
    assert_eq!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Failed
    );
    assert_eq!(w.door.warnings("merge_failed").len(), 1);
}

/// **A merge row landed by hand settles** (R92.28, second half) — even while the target cannot
/// be judged, which is when `land_next` would never take it.
#[test]
fn a_merge_row_landed_by_hand_settles() {
    let w = World::new("byhand");
    let (wt, base, head) = branch(&w.repo, "h", "h.txt");
    w.enqueue("h", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let m = w.accept("h", "test/a");
    // The target cannot be gated from here on, so the queue lands nothing.
    let mut e = env(Arc::new(LocalGate), w.door.clone(), "a");
    Arc::get_mut(&mut e).unwrap().landing_root = Arc::new(|_| Err("no room".into()));
    let q = RepoQueue::new(w.store(), w.repo.clone(), e);
    sh(&w.repo, &["merge", "--ff-only", "agent/h"]);
    run(&q, 2);
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Withdrawn, "{row:?}");
}

/// **A daemon that stops kills its gate and puts the row back in line** — never left `taken`
/// for a restart to call `stale` while the orphan holds the build lock (defect 6).
#[test]
fn a_stopping_daemon_kills_the_gate_and_the_row_goes_back_in_line() {
    let w = World::new("stop");
    let (wt, base, head) = branch(&w.repo, "s", "s.txt");
    w.enqueue("s", &wt, &base, &head);
    let q = w.worker(Arc::new(GreenGate), "a");
    run(&q, 1);
    let m = w.accept("s", "test/a");
    // Main is green now; make the BRANCH's gate slow, then stop mid-gate.
    struct Slow;
    impl GateRunner for Slow {
        fn run(&self, wt: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
            LocalGate.run(wt, &["sleep 60".to_string()], ctl);
            GateRun {
                steps: steps.iter().map(|_| gate::no_gate_row("x")).collect(),
                verdict: gate::GateVerdict::Stopped("the daemon is stopping".into()),
            }
        }
    }
    let q = RepoQueue::new(
        w.store(),
        w.repo.clone(),
        env(Arc::new(Slow), w.door.clone(), "a"),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        s2.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let started = std::time::Instant::now();
    q.settle(&stop).unwrap();
    assert!(started.elapsed() < Duration::from_secs(20));
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Queued, "{row:?}");
    assert_eq!(row.owner, "", "nobody's");
}

/// **Push after the gate, install only after the push** (decision 7).
#[test]
fn the_landing_pushes_then_installs_and_a_failed_push_installs_nothing() {
    let w = World::new("push");
    let origin = w.dir.0.join("origin.git");
    sh(
        &w.dir.0,
        &["init", "-q", "--bare", &origin.to_string_lossy()],
    );
    sh(
        &w.repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    sh(&w.repo, &["push", "-q", "origin", "main"]);
    let marker = w.dir.0.join("installed");
    std::fs::write(
        w.repo.join("AGENTS.md"),
        format!(
            "## Merge gate\n\n```sh\ntest ! -f broken\ntouch {} # install-gate-build\n```\n",
            marker.display()
        ),
    )
    .unwrap();
    sh(&w.repo, &["commit", "-q", "-am", "an install step"]);
    sh(&w.repo, &["push", "-q", "origin", "main"]);
    let (wt, base, head) = branch(&w.repo, "p", "p.txt");
    w.enqueue("p", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    assert!(!marker.exists(), "gating the target installs nothing");
    let m = w.accept("p", "test/a");
    run(&q, 2);
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Landed, "{row:?}");
    assert_eq!(
        sh(&origin, &["rev-parse", "main"]),
        w.tip(),
        "pushed: {}",
        row.pushed
    );
    assert!(marker.exists(), "installed after the push");

    // A push that fails: landed locally, said, and NOT installed.
    std::fs::remove_file(&marker).unwrap();
    sh(
        &w.repo,
        &["remote", "set-url", "origin", "/nonexistent/origin.git"],
    );
    let (wt, base, head) = branch(&w.repo, "q", "q.txt");
    w.enqueue("q", &wt, &base, &head);
    run(&q, 1);
    let m = w.accept("q", "test/a");
    run(&q, 2);
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Landed);
    assert!(row.pushed.contains("FAILED"), "{}", row.pushed);
    assert!(!marker.exists(), "nothing installed that was not pushed");
    assert!(
        !w.door.warnings("queue_push_failed").is_empty(),
        "and it was raised"
    );
    assert!(
        w.door.warnings("queue_blocked").is_empty(),
        "a landed branch blocks nothing"
    );
}

/// **A push that fails is said even when the project has no install step** (R92.33: the raise
/// was inside `if !install.is_empty()`).
#[test]
fn a_failed_push_with_no_install_step_is_said() {
    let w = World::new("push-noinstall");
    sh(
        &w.repo,
        &["remote", "add", "origin", "/nonexistent/origin.git"],
    );
    let (wt, base, head) = branch(&w.repo, "p", "p.txt");
    w.enqueue("p", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let m = w.accept("p", "test/a");
    run(&q, 2);
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Landed, "{row:?}");
    let said = w.door.warnings("queue_push_failed");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("FAILED"), "{}", said[0]);
}

// ===== Red target =====

/// **R61's done-when, in one repository, under the operator's R92.18 rule: a red target HALTS
/// BOTH queues.** A branch reviewed and merged; a second approved and waiting to land when main
/// turns red goes back to review (its approval is spent); a third enqueued while red is not
/// claimed; the red tip goes first in review; only the fix lands; green again, the held reviews
/// are claimed and the queue lands again.
#[test]
fn a_red_target_lands_nothing_but_its_fix_and_goes_first_in_review() {
    let w = World::new("red");
    let (wt, base, head) = branch(&w.repo, "one", "one.txt");
    w.enqueue("one", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let m1 = w.accept("one", "test/a");
    run(&q, 2);
    assert_eq!(
        w.store().merge_row(&m1).unwrap().unwrap().state,
        MergeState::Landed
    );

    // `two` is reviewed and approved while main is green…
    let (wt2, base2, head2) = branch(&w.repo, "two", "two.txt");
    w.enqueue("two", &wt2, &base2, &head2);
    run(&q, 1);
    let m2 = w.accept("two", "test/a");
    // …and before it lands, a hand merge breaks main (decision 10: gated on every change, hand
    // merges included).
    commit(
        &w.repo,
        "broken",
        "x\n",
        "a hand commit that breaks the gate",
    );
    run(&q, 1);
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Red, "{h:?}");
    let said = w.door.warnings("target_red");
    assert_eq!(said.len(), 1, "an emergency, said once: {said:?}");
    assert!(
        said[0].contains("approved branches go back to review"),
        "{}",
        said[0]
    );
    assert!(
        !crate::mergequeue::notices::health(&h).contains(crate::mergequeue::carry::LANDING_LACKS),
        "no ignored directories in this checkout: the notice says nothing about them"
    );
    assert_eq!(
        w.store().merge_row(&m2).unwrap().unwrap().state,
        MergeState::Moved,
        "a red target halts the merge queue: the approved branch goes back to review"
    );
    let rows = w.store().review_rows(Some(&w.key())).unwrap();
    let red = rows
        .iter()
        .find(|r| r.kind == ReviewKind::Target)
        .expect("the red tip is a review row")
        .clone();
    assert_eq!(red.state, ReviewState::Reviewing, "taken FIRST: {red:?}");
    let two = rows.iter().find(|r| r.id == "two").unwrap();
    assert_eq!(
        two.state,
        ReviewState::Waiting,
        "and the review queue is halted for it too: {two:?}"
    );
    let red_tip = w.tip();
    // Enqueue is never refused — but nothing claims it while red.
    let (wt3, base3, head3) = branch(&w.repo, "three", "three.txt");
    w.enqueue("three", &wt3, &base3, &head3);
    // Free a reviewer slot: the red tip's own reviewer rejects (the code is broken).
    let s = w.store();
    s.move_review(
        &red.id,
        ReviewState::Reviewing,
        ReviewState::Rejected,
        &Move::new("reviewer", "broken by the hand commit")
            .owned_by("test/a")
            .set("owner", ""),
        gate::now_ms(),
    )
    .unwrap();
    run(&q, 2);
    for id in ["two", "three"] {
        assert_eq!(
            w.store().review_row(id).unwrap().unwrap().state,
            ReviewState::Waiting,
            "`{id}` is not claimed while main is red"
        );
    }
    assert_eq!(w.tip(), red_tip);

    // The fix: a branch that removes `broken`, marked as fixing the red tip — the one thing that
    // moves.
    let fixwt = w.dir.0.join("wt-fix");
    sh(
        &w.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/fix",
            &fixwt.to_string_lossy(),
            "main",
        ],
    );
    sh(&fixwt, &["rm", "-q", "broken"]);
    sh(&fixwt, &["commit", "-q", "-m", "fix the gate"]);
    let fixhead = sh(&fixwt, &["rev-parse", "HEAD"]);
    w.store()
        .enqueue_review(
            &NewReview {
                id: "fix".into(),
                repository: w.key(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: "s-fix".into(),
                branch: "agent/fix".into(),
                worktree: Some(fixwt.display().to_string()),
                base_sha: red_tip.clone(),
                head_sha: fixhead,
                brief: "fix main".into(),
                priority: MergePriority::Urgent,
                fixes: Some(red_tip.clone()),
                evidence: "the fix".into(),
            },
            "test",
            gate::now_ms(),
        )
        .unwrap();
    run(&q, 1);
    let mfix = w.accept("fix", "test/a");
    run(&q, 2);
    assert_eq!(
        w.store().merge_row(&mfix).unwrap().unwrap().state,
        MergeState::Landed,
        "the fix lands on the red target"
    );
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Green);
    assert_eq!(
        w.door.warnings("target_green").len(),
        1,
        "green again, said once (R92.6)"
    );
    assert_eq!(
        w.store().review_row(&red.id).unwrap().unwrap().state,
        ReviewState::Withdrawn,
        "a rejected red-tip review is withdrawn on green too (R92.29)"
    );
    // Green again: the held reviews are claimed, and `two` is reviewed again against the tip.
    run(&q, 1);
    assert_eq!(
        w.store().review_row("two").unwrap().unwrap().state,
        ReviewState::Reviewing
    );
    let m2 = w.accept("two", "test/a");
    run(&q, 3);
    assert_eq!(
        w.store().merge_row(&m2).unwrap().unwrap().state,
        MergeState::Landed,
        "green again: the queue lands again"
    );
    assert_eq!(w.door.warnings("target_green").len(), 1, "still once");
}

/// **The same sha red twice is said twice, and a restart re-gating it to the same verdict is
/// said once** (R92.31, R92.7). A gate that reads the machine: red, green (the machine got
/// better), red again at the very same tip.
#[test]
fn the_same_sha_red_again_after_green_is_said_and_a_restart_is_not() {
    let w = World::new("redtwice");
    let flag = w.dir.0.join("sick");
    std::fs::write(&flag, "x").unwrap();
    std::fs::write(
        w.repo.join("AGENTS.md"),
        format!(
            "# agents\n\n## Merge gate\n\n```sh\ntest ! -f {}\n```\n",
            flag.display()
        ),
    )
    .unwrap();
    sh(&w.repo, &["add", "-A"]);
    sh(
        &w.repo,
        &["commit", "-q", "-m", "a gate that reads the machine"],
    );
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    assert_eq!(w.door.warnings("target_red").len(), 1);
    assert_eq!(w.door.repositories.lock().unwrap().len(), 1);

    // A restarted daemon re-gates the red tip (`regate_at_start`) and finds it red again: nothing
    // is said or rung a second time.
    let q2 = {
        let env = env(Arc::new(LocalGate), w.door.clone(), "a");
        RepoQueue::new(w.store(), w.repo.clone(), env)
    };
    crate::mergequeue::verbs::regate_at_start(&w.store(), &w.key(), "test/a").unwrap();
    run(&q2, 2);
    assert_eq!(
        w.door.warnings("target_red").len(),
        1,
        "a restart is not news"
    );
    assert_eq!(w.door.repositories.lock().unwrap().len(), 1, "nor a ring");

    // Green at the same tip, then red again at the same tip: said, and reviewed again.
    std::fs::remove_file(&flag).unwrap();
    crate::mergequeue::verbs::regate(&w.store(), &w.key(), "main", "operator:s-1", &|_| {})
        .unwrap();
    run(&q2, 1);
    assert_eq!(w.door.warnings("target_green").len(), 1);
    std::fs::write(&flag, "x").unwrap();
    crate::mergequeue::verbs::regate(&w.store(), &w.key(), "main", "operator:s-1", &|_| {})
        .unwrap();
    run(&q2, 1);
    let said = w.door.warnings("target_red");
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[1].contains("RED again"), "{}", said[1]);
    let red = w
        .store()
        .review_rows(Some(&w.key()))
        .unwrap()
        .into_iter()
        .find(|r| r.kind == ReviewKind::Target)
        .unwrap();
    assert!(
        matches!(red.state, ReviewState::Waiting | ReviewState::Reviewing),
        "the withdrawn red-tip review is open again: {red:?}"
    );
}

/// **A `[queue]` that does not parse is said once per error, not every pass** (R92.19).
#[test]
fn unreadable_queue_settings_are_said_once_per_error() {
    let w = World::new("settings");
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut e = env(Arc::new(GreenGate), w.door.clone(), "a");
    let m = n.clone();
    Arc::get_mut(&mut e).unwrap().settings = Arc::new(move |_| {
        if m.load(std::sync::atomic::Ordering::Relaxed) < 2 {
            Err("bad toml at line 3".into())
        } else {
            Err("bad toml at line 4".into())
        }
    });
    let store = w.store();
    store
        .acquire_queue_lease(&w.key(), &e.me, gate::now_ms(), LEASE_TTL_MS)
        .unwrap();
    let q = RepoQueue::new(store, w.repo.clone(), e);
    for _ in 0..2 {
        run(&q, 1);
        n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    assert_eq!(w.door.warnings("queue_settings_unreadable").len(), 1);
    run(&q, 3);
    let said = w.door.warnings("queue_settings_unreadable");
    assert_eq!(said.len(), 2, "a changed error is said: {said:?}");
}

/// **One repository's red target does not stop another's landings** (decision 2) — two
/// repositories, two leases, two workers.
#[test]
fn a_red_target_in_one_repository_does_not_stop_another() {
    let a = World::new("red-a");
    let b = World::new("red-b");
    commit(&a.repo, "broken", "x\n", "red");
    let (wt, base, head) = branch(&b.repo, "ok", "ok.txt");
    b.enqueue("ok", &wt, &base, &head);
    let qa = a.worker(Arc::new(LocalGate), "a");
    let qb = b.worker(Arc::new(LocalGate), "a");
    run(&qa, 1);
    run(&qb, 1);
    let m = b.accept("ok", "test/a");
    run(&qa, 1);
    run(&qb, 2);
    assert_eq!(
        a.store()
            .target_health(&a.key(), "main")
            .unwrap()
            .unwrap()
            .state,
        HealthState::Red
    );
    assert_eq!(
        b.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Landed
    );
}

// ===== R76: a gate that cannot judge, a rewritten target, a re-gate =====

/// **A gate whose build and tests passed and which then found no binary is UNJUDGED, not red**
/// (R76 item 5, pg-noop's `main` of 2026-10-11). FAILS before R76: the health was `red`, a
/// `red-main-…` review asked a reviewer what in the code broke it, and `target_red` went out at
/// emergency severity. After: `blocked` with the setup named first, no review of the code, a
/// `gate_setup_failed` raise — and still nothing lands.
#[test]
fn a_gate_whose_own_setup_fails_after_the_build_is_unjudged_not_red_and_lands_nothing() {
    let w = World::new("setup");
    std::fs::write(
        w.repo.join("gate.sh"),
        "echo 'test result: ok. 3 passed; 0 failed'\necho '    Finished `release` profile'\n\
         echo \"merge gate: FAIL: no binary at $PWD/target/release/letidb after a successful build\" >&2\n\
         exit 1\n",
    )
    .unwrap();
    std::fs::write(
        w.repo.join("AGENTS.md"),
        "# agents\n\n## Merge gate\n\n```sh\nsh gate.sh\n```\n",
    )
    .unwrap();
    sh(&w.repo, &["add", "-A"]);
    sh(
        &w.repo,
        &["commit", "-q", "-m", "a gate that names the wrong binary"],
    );
    let (wt, base, head) = branch(&w.repo, "one", "one.txt");
    w.enqueue("one", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Blocked, "{h:?}");
    assert!(
        h.evidence
            .starts_with(crate::mergequeue::setup::GATE_SETUP_FAILED),
        "{}",
        h.evidence
    );
    assert!(h.evidence.contains("no binary at"), "{}", h.evidence);
    assert!(w.door.warnings("target_red").is_empty(), "not an emergency");
    assert_eq!(w.door.warnings("gate_setup_failed").len(), 1);
    assert!(
        w.store()
            .review_rows(Some(&w.key()))
            .unwrap()
            .iter()
            .all(|r| r.kind != ReviewKind::Target),
        "no reviewer is sent to find what in the code broke it"
    );
    let m = w.accept("one", "test/a");
    let tip = w.tip();
    run(&q, 2);
    assert_eq!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Queued,
        "nothing lands on an unjudged target"
    );
    assert_eq!(w.tip(), tip);
}

/// **firecode's red, 2026-10-11** — *"A git worktree doesn't carry gitignored files, so that tree
/// has no `images/` and no `vendor/` … Every check that boots a VM fails"*. A gate that needs a
/// gitignored input fails in the landing tree; the red notice names what the checkout has and
/// the landing tree lacks, and `[queue] gate_carry` brings it in so the same gate passes.
#[test]
fn a_gate_that_needs_an_ignored_input_is_told_its_name_and_passes_once_it_is_carried() {
    let w = World::new("carry");
    std::fs::write(w.repo.join(".gitignore"), "images/\n").unwrap();
    std::fs::write(
        w.repo.join("AGENTS.md"),
        "# agents\n\n## Merge gate\n\n```sh\ntest -f images/rootfs.ext4\n```\n",
    )
    .unwrap();
    sh(&w.repo, &["add", "-A"]);
    sh(
        &w.repo,
        &["commit", "-q", "-m", "a gate that boots the image"],
    );
    std::fs::create_dir_all(w.repo.join("images")).unwrap();
    std::fs::write(w.repo.join("images/rootfs.ext4"), "rootfs\n").unwrap();

    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Red, "{h:?}");
    let sentence = "The landing tree has no `images/` (gitignored, present in your checkout); if \
                    the gate needs them, set `[queue] gate_carry` in the project's \
                    `leticode.toml`.";
    assert!(h.evidence.ends_with(sentence), "{}", h.evidence);
    let notice = crate::mergequeue::notices::health(&h);
    assert!(notice.starts_with("EMERGENCY"), "{notice}");
    assert_eq!(notice.matches(sentence).count(), 1, "{notice}");

    // The project names it; the next tip is gated with it carried, and passes.
    let carrying = QueueSettings {
        gate_carry: vec!["images".into()],
        ..Default::default()
    };
    let q = w.worker_with(Arc::new(LocalGate), "a", carrying);
    commit(&w.repo, "a.txt", "two\n", "main moves");
    run(&q, 1);
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Green, "{h:?}");
    let carried = git::landing_dir_in(&w.dir.0.join("landing"), &w.repo).join("images");
    let m = std::fs::symlink_metadata(&carried).unwrap();
    assert!(
        m.is_dir() && !m.file_type().is_symlink(),
        "a copy, never a link"
    );
}

/// **An amended target supersedes the review of its old red tip** (R76 item 4). FAILS before
/// R76: the review of the amended-away tip stayed open — and, once its commit was gone, `blocked`
/// on *"fatal: unable to read tree"* (pg-noop's `red-main-ec3cb01e8a6f`). After: withdrawn as
/// `superseded by <the new tip>`, and the new tip is gated. (A rewritten tip is one way the tip
/// moves; `supersede_older_tip` withdraws for every move, and the tests below are the rest.)
#[test]
fn an_amended_target_supersedes_the_review_of_its_old_red_tip_and_gates_the_new_one() {
    let w = World::new("amend");
    let red = commit(&w.repo, "broken", "x\n", "a commit that breaks the gate");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let rows = w.store().review_rows(Some(&w.key())).unwrap();
    let old = rows
        .iter()
        .find(|r| r.kind == ReviewKind::Target)
        .expect("the red tip is reviewed")
        .clone();
    assert_eq!(old.head_sha, red);
    // A review of a commit that no longer exists at all, blocked the way pg-noop's was.
    w.store()
        .enqueue_review(
            &NewReview {
                id: "red-main-gone".into(),
                repository: w.key(),
                target: "main".into(),
                kind: ReviewKind::Target,
                session_id: String::new(),
                branch: "main".into(),
                worktree: None,
                base_sha: String::new(),
                head_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                brief: "red".into(),
                priority: MergePriority::Urgent,
                fixes: None,
                evidence: "red".into(),
            },
            "test",
            gate::now_ms(),
        )
        .unwrap();
    assert!(
        w.store()
            .move_review(
                "red-main-gone",
                ReviewState::Waiting,
                ReviewState::Blocked,
                &Move::new("test", "git checkout …: fatal: unable to read tree"),
                gate::now_ms(),
            )
            .unwrap()
    );
    // The amend: same tree, new commit — the old tip is no longer `main`'s history.
    sh(
        &w.repo,
        &["commit", "-q", "--amend", "-m", "the same break, amended"],
    );
    let amended = w.tip();
    assert_ne!(amended, red);
    run(&q, 1);
    let s = w.store();
    let was = s.review_row(&old.id).unwrap().unwrap();
    assert_eq!(was.state, ReviewState::Withdrawn, "{was:?}");
    let moves = s.transitions("review", &old.id).unwrap();
    let why = &moves.last().unwrap().why;
    assert!(
        why.starts_with(&format!("superseded by {amended}")),
        "{why}"
    );
    let gone = s.review_row("red-main-gone").unwrap().unwrap();
    assert_eq!(gone.state, ReviewState::Withdrawn, "{gone:?}");
    let h = s.target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.sha, amended, "the new tip was gated");
    assert_eq!(h.state, HealthState::Red);
    assert!(
        s.review_rows(Some(&w.key()))
            .unwrap()
            .iter()
            .any(|r| r.kind == ReviewKind::Target && r.head_sha == amended && r.state.is_open()),
        "the new red tip is the one under review"
    );
}

/// **A judged tip is gated again on request, and by a daemon taking the queue** (R76 item 3, R74
/// #4). The red here is not in the code — the gate reads a file outside the repository (the
/// machine) — so the tip never changes. FAILS before R76: no verb existed, and a new lease holder
/// left a tip judged red alone, so the target stayed red with nothing left to change.
#[test]
fn a_judged_tip_is_gated_again_on_request_and_when_a_daemon_takes_the_queue() {
    let w = World::new("regate");
    let flag = w.dir.0.join("the-machine-is-sick");
    std::fs::write(&flag, "x").unwrap();
    std::fs::write(
        w.repo.join("AGENTS.md"),
        format!(
            "# agents\n\n## Merge gate\n\n```sh\ntest ! -f {}\n```\n",
            flag.display()
        ),
    )
    .unwrap();
    sh(&w.repo, &["add", "-A"]);
    sh(
        &w.repo,
        &["commit", "-q", "-m", "a gate that reads the machine"],
    );
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let health = || w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(health().state, HealthState::Red);
    std::fs::remove_file(&flag).unwrap();
    run(&q, 2);
    assert_eq!(health().state, HealthState::Red, "the tip did not change");

    // `/queue regate` (and the tool's verb): gated again at the same tip.
    let said =
        crate::mergequeue::verbs::regate(&w.store(), &w.key(), "main", "operator:s-1", &|_| {})
            .unwrap();
    assert!(said.contains("gated again"), "{said}");
    run(&q, 1);
    assert_eq!(health().state, HealthState::Green);
    assert!(
        w.store()
            .review_rows(Some(&w.key()))
            .unwrap()
            .iter()
            .filter(|r| r.kind == ReviewKind::Target)
            .all(|r| r.state == ReviewState::Withdrawn),
        "green again: the red tip's review is withdrawn"
    );

    // A daemon taking the queue re-gates a tip judged red (here: sick again), and leaves green.
    std::fs::write(&flag, "x").unwrap();
    assert!(
        crate::mergequeue::verbs::regate_at_start(&w.store(), &w.key(), "test/b")
            .unwrap()
            .is_empty(),
        "a green tip is not in doubt"
    );
    crate::mergequeue::verbs::regate(&w.store(), &w.key(), "main", "operator:s-1", &|_| {})
        .unwrap();
    run(&q, 1);
    assert_eq!(health().state, HealthState::Red);
    std::fs::remove_file(&flag).unwrap();
    assert_eq!(
        crate::mergequeue::verbs::regate_at_start(&w.store(), &w.key(), "test/b").unwrap(),
        vec!["main".to_string()]
    );
    run(&q, 1);
    assert_eq!(health().state, HealthState::Green);
}

/// **A red target rings its project's root sessions, once per red tip; a branch blocked in
/// review rings the root of its tree, once** (R76 item 1). FAILS before R76: the only door was
/// `raise`, a warning on every hub that no model reads.
#[test]
fn a_red_target_and_a_blocked_review_ring_the_project_once_each() {
    let w = World::new("ring");
    commit(&w.repo, "broken", "x\n", "red");
    let (wt, base, head) = branch(&w.repo, "b", "b.txt");
    w.enqueue("b", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 3);
    assert_eq!(
        *w.door.repositories.lock().unwrap(),
        vec![w.key()],
        "the red tip rang the repository's roots once"
    );
    let r = w.store().review_row("b").unwrap().unwrap();
    assert!(
        w.store()
            .move_review(
                "b",
                r.state,
                ReviewState::Blocked,
                &Move::new("test", "no reviewer could start").set("failure", "no VM"),
                gate::now_ms(),
            )
            .unwrap()
    );
    run(&q, 3);
    assert_eq!(*w.door.projects.lock().unwrap(), vec!["s-b".to_string()]);
    assert_eq!(w.door.repositories.lock().unwrap().len(), 1, "still once");
}

// ===== The review queue's upkeep =====

/// **A reviewer has a deadline; attempts are counted; the third is a raise, not a fourth**
/// (defect 10: 148 reviewer sessions for 21 rows, `Running` waited on for ever).
#[test]
fn a_reviewer_past_its_deadline_is_counted_and_the_third_is_raised() {
    let w = World::new("review-deadline");
    let (wt, base, head) = branch(&w.repo, "r", "r.txt");
    w.enqueue("r", &wt, &base, &head);
    let mut e = env(Arc::new(GreenGate), w.door.clone(), "a");
    Arc::get_mut(&mut e).unwrap().review_deadline_ms = 0;
    let store = w.store();
    store
        .acquire_queue_lease(&w.key(), &e.me, gate::now_ms(), LEASE_TTL_MS)
        .unwrap();
    let q = RepoQueue::new(store, w.repo.clone(), e);
    for attempt in 1..=MAX_REVIEW_ATTEMPTS {
        run(&q, 1); // claimed (and, after the first, the expired one counted)
        let s = w.store();
        let r = s.review_row("r").unwrap().unwrap();
        if r.state == ReviewState::Reviewing {
            // A reviewer started, and never answers.
            s.move_review(
                "r",
                ReviewState::Reviewing,
                ReviewState::Reviewing,
                &Move::new("pool", "started").set("reviewer", format!("sub-{attempt}")),
                gate::now_ms(),
            )
            .unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    run(&q, 1);
    let r = w.store().review_row("r").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Blocked, "{r:?}");
    assert_eq!(r.attempts, MAX_REVIEW_ATTEMPTS);
    assert!(
        !w.door.warnings("queue_blocked").is_empty(),
        "raised at high severity"
    );
    run(&q, 2);
    assert_eq!(
        w.store().review_row("r").unwrap().unwrap().state,
        ReviewState::Blocked,
        "and it stays blocked: no fourth reviewer"
    );
}

/// **A claim nobody picks up is a reviewer that cannot start — raised** (decision 4), and with
/// a project's `merge_via_session` it is handed to the project's main session, once.
#[test]
fn a_review_nobody_starts_is_raised_and_handed_over_only_where_the_project_opted_in() {
    let w = World::new("no-start");
    let (wt, base, head) = branch(&w.repo, "n", "n.txt");
    w.enqueue("n", &wt, &base, &head);
    let mut e = env(Arc::new(GreenGate), w.door.clone(), "a");
    {
        let e = Arc::get_mut(&mut e).unwrap();
        e.reviewer_start_grace_ms = 0;
        e.settings = Arc::new(|_| {
            Ok(QueueSettings {
                merge_via_session: true,
                ..Default::default()
            })
        });
    }
    let store = w.store();
    store
        .acquire_queue_lease(&w.key(), &e.me, gate::now_ms(), LEASE_TTL_MS)
        .unwrap();
    let q = RepoQueue::new(store, w.repo.clone(), e);
    run(&q, 1);
    std::thread::sleep(Duration::from_millis(5));
    run(&q, 3);
    let r = w.store().review_row("n").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Blocked, "{r:?}");
    assert!(
        r.evidence.starts_with(crate::mergequeue::HANDED_TO_PROJECT),
        "{}",
        r.evidence
    );
    assert_eq!(
        *w.door.projects.lock().unwrap(),
        vec!["s-n".to_string()],
        "the project was told once"
    );
    // And the project landing it by hand withdraws the row.
    sh(&w.repo, &["merge", "-q", "--ff-only", "agent/n"]);
    run(&q, 1);
    assert_eq!(
        w.store().review_row("n").unwrap().unwrap().state,
        ReviewState::Withdrawn
    );
}

/// **A refusal does not outlive the commits that answered it** (defect 5's other half).
#[test]
fn a_rejected_branch_that_moves_goes_back_to_review() {
    let w = World::new("rejected-moves");
    let (wt, base, head) = branch(&w.repo, "rj", "rj.txt");
    w.enqueue("rj", &wt, &base, &head);
    // **The child that did the work is still there** — the reviewer's complaints went back to it
    // and it is expected to answer them in its own tree, so the tree is not the sweep's to take
    // (R92.24; the resource sweep keeps a tree a live session works in, and releases it later).
    w.door.live.lock().unwrap().push("s-rj".into());
    let q = w.worker(Arc::new(GreenGate), "a");
    run(&q, 1);
    let s = w.store();
    s.move_review(
        "rj",
        ReviewState::Reviewing,
        ReviewState::Rejected,
        &Move::new("reviewer:sub-1", "no")
            .owned_by("test/a")
            .set("owner", "")
            .set("verdict", "reject")
            .set("reviewed_sha", head.clone()),
        gate::now_ms(),
    )
    .unwrap();
    run(&q, 1);
    assert_eq!(
        s.review_row("rj").unwrap().unwrap().state,
        ReviewState::Rejected
    );
    let newer = commit(&wt, "rj2.txt", "answered\n", "answer the review");
    run(&q, 1);
    let r = s.review_row("rj").unwrap().unwrap();
    assert!(
        matches!(r.state, ReviewState::Waiting | ReviewState::Reviewing),
        "{r:?}"
    );
    assert_eq!(r.head_sha, newer);
}

// ===== ONE tip: a target review is about the tip that is there now (2026-10-26) =====

/// **Four red tips in a row leave ONE open review** — and three withdrawn, each naming the tip
/// that replaced it.
///
/// The morning this is against: `e51847ed`, `f35d19c0`, `54e5a372` and `0158b957` were four tips
/// of one branch, and all four `red-main-<sha>` rows were open at once. A newer tip never closed
/// the older rows, so each got a reviewer child (one ran to its hour), each was rejected, and each
/// opened a fix branch with its own worktree — for reds `main` had already left.
#[test]
fn four_red_tips_leave_one_open_row_and_three_superseded_by_the_tip_that_replaced_them() {
    let w = World::new("reds-supersede");
    let q = w.worker(Arc::new(LocalGate), "a");
    let mut tips: Vec<String> = Vec::new();
    for n in 1..=4 {
        tips.push(commit(
            &w.repo,
            "broken",
            &format!("{n}\n"),
            &format!("tip {n}, still red"),
        ));
        run(&q, 1);
    }
    let s = w.store();
    let rows: Vec<_> = s
        .review_rows(Some(&w.key()))
        .unwrap()
        .into_iter()
        .filter(|r| r.kind == ReviewKind::Target)
        .collect();
    assert_eq!(rows.len(), 4, "one review per red tip: {rows:?}");
    let open: Vec<_> = rows.iter().filter(|r| r.state.is_open()).collect();
    assert_eq!(open.len(), 1, "one tip is open, not four: {open:?}");
    assert_eq!(
        open[0].head_sha, tips[3],
        "and it is the tip that is red now"
    );
    for r in rows.iter().filter(|r| !r.state.is_open()) {
        assert_eq!(r.state, ReviewState::Withdrawn, "{r:?}");
        let i = tips
            .iter()
            .position(|t| *t == r.head_sha)
            .expect("a tip this test made");
        let why = s
            .transitions("review", &r.id)
            .unwrap()
            .last()
            .expect("a transition")
            .why
            .clone();
        assert!(
            why.starts_with(&format!("superseded by {}", tips[i + 1])),
            "the row for tip {i} says which tip superseded it: {why}"
        );
    }
}

/// **A green tip after a red leaves nothing open** — the red row is withdrawn by the same rule,
/// saying which tip replaced it (the reviewer of a tip nobody has is a reviewer wasted).
#[test]
fn a_green_tip_withdraws_the_red_rows_review_and_leaves_nothing_open() {
    let w = World::new("green-supersedes");
    let red = commit(&w.repo, "broken", "x\n", "a tip that breaks the gate");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    assert_eq!(
        w.store()
            .target_health(&w.key(), "main")
            .unwrap()
            .unwrap()
            .state,
        HealthState::Red,
        "the fixture's tip really is red"
    );
    // `broken` goes, so the next tip is green.
    sh(&w.repo, &["rm", "-q", "broken"]);
    sh(&w.repo, &["commit", "-q", "-m", "the tip that fixes it"]);
    let green = w.tip();
    run(&q, 2);
    let s = w.store();
    assert_eq!(
        s.target_health(&w.key(), "main").unwrap().unwrap().state,
        HealthState::Green
    );
    let rows: Vec<_> = s
        .review_rows(Some(&w.key()))
        .unwrap()
        .into_iter()
        .filter(|r| r.kind == ReviewKind::Target)
        .collect();
    assert!(
        rows.iter().all(|r| !r.state.is_open()),
        "nothing is left open on a green target: {rows:?}"
    );
    let was = rows
        .iter()
        .find(|r| r.head_sha == red)
        .expect("the red tip's review");
    assert_eq!(was.state, ReviewState::Withdrawn, "{was:?}");
    let why = s
        .transitions("review", &was.id)
        .unwrap()
        .last()
        .unwrap()
        .why
        .clone();
    assert!(why.starts_with(&format!("superseded by {green}")), "{why}");
}

/// **A `reviewing` row whose reviewer child is not alive goes back to `waiting`** — the operator's
/// other report: after a daemon restart two rows stayed `reviewing` with reviewer children that no
/// longer existed, until their hour was up.
///
/// Liveness is the session registry's, through [`QueueDoor::session_live`]: a subagent is a real
/// registered session (that is what lets an operator attach to one), so a handle the registry does
/// not hold is a child that finished, died, or did not come back. The reviewers' host collects a
/// child that ends WHILE the daemon is up; this is the door for one whose host will never look
/// again — and the attempt is counted, so a child that keeps vanishing reaches `blocked` rather
/// than looping for ever.
#[test]
fn a_reviewing_row_whose_reviewer_is_not_alive_is_re_seated_on_the_next_pass() {
    let w = World::new("reviewer-gone");
    let (wt, base, head) = branch(&w.repo, "g", "g.txt");
    w.enqueue("g", &wt, &base, &head);
    let q = w.worker(Arc::new(GreenGate), "a");
    run(&q, 1);
    let s = w.store();
    let claimed = s.review_row("g").unwrap().unwrap();
    assert_eq!(claimed.state, ReviewState::Reviewing, "{claimed:?}");
    // A reviewer was started on it, and its child is in no registry this daemon holds.
    assert!(
        s.move_review(
            "g",
            ReviewState::Reviewing,
            ReviewState::Reviewing,
            &Move::new("pool", "reviewer sub-1 started").set("reviewer", "sub-1"),
            gate::now_ms(),
        )
        .unwrap()
    );
    run(&q, 1);
    let r = s.review_row("g").unwrap().unwrap();
    assert_eq!(r.attempts, 1, "the attempt is counted: {r:?}");
    let why = s
        .transitions("review", "g")
        .unwrap()
        .into_iter()
        .rfind(|t| t.to_state == "waiting")
        .expect("the row was re-seated")
        .why;
    assert!(
        why.contains("sub-1") && why.contains("not alive"),
        "the transition says why: {why}"
    );

    // The negative half: a child the daemon DOES hold is left where it is, and costs no attempt.
    w.door.live.lock().unwrap().push("sub-1".into());
    assert!(
        s.move_review(
            "g",
            r.state,
            ReviewState::Reviewing,
            &Move::new("pool", "reviewer sub-1 started again")
                .set("owner", "test/a")
                .set("reviewer", "sub-1"),
            gate::now_ms(),
        )
        .unwrap()
    );
    run(&q, 1);
    let r = s.review_row("g").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Reviewing, "left under review: {r:?}");
    assert_eq!(r.attempts, 1, "a live child spends no attempt");

    // **And a child of the id every daemon shared before this change is the SAME case.** The
    // review queue's session used to be the constant `s-gatekeeper` on every box, so a row an old
    // build left `reviewing` names a reviewer `s-gatekeeper-sub-…` — a session no daemon of this
    // build holds, and therefore exactly the vanished child this rule re-seats. Nothing has to
    // migrate such a row: it is stopped being waited for the next time the queue passes.
    w.door.live.lock().unwrap().clear();
    assert!(
        s.move_review(
            "g",
            ReviewState::Reviewing,
            ReviewState::Reviewing,
            &Move::new("pool", "an old build's reviewer")
                .set("owner", "test/a")
                .set("reviewer", "s-gatekeeper-sub-1791759365511364772"),
            gate::now_ms(),
        )
        .unwrap()
    );
    run(&q, 1);
    // **The worker's pass is a whole cycle**: the re-seat puts the row back to `waiting` and the
    // same pass claims it again, so what proves the re-seat is the TRANSITION, not the row's
    // resting state. Two attempts: one for the live child that vanished, one for the old id's.
    let legacy = s.review_row("g").unwrap().unwrap();
    assert_eq!(
        legacy.attempts, 2,
        "an old id's vanished reviewer spends an attempt like any other: {legacy:?}"
    );
    let why = s
        .transitions("review", "g")
        .unwrap()
        .into_iter()
        .rfind(|t| t.to_state == "waiting")
        .expect("the row was re-seated again")
        .why;
    assert!(
        why.contains("s-gatekeeper-sub-1791759365511364772") && why.contains("not alive"),
        "and the same sentence, naming the old child: {why}"
    );
}

// ===== A reviewer whose verdict is waiting is not a dead reviewer =====

/// The review queue's session, as `Harness::serve_reviews` passes it to the pool: the host a
/// reviewer's desk entry is opened under.
const QUEUE_SESSION: &str = "s-gatekeeper";

/// A reviewers' host that starts nothing — every reviewer in this section is already at the desk,
/// and what is under test is what the two loops make of the verdict it handed in.
struct NoReviewers;

impl TaskRunner for NoReviewers {
    fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        Err("no reviewer is started in this test".into())
    }
    fn collect(&self, _handle: &str, _timeout: Duration) -> TaskStatus {
        TaskStatus::Running {
            note: None,
            child: None,
        }
    }
}

/// **One pass of the reviewers' host** — [`crate::mergequeue::review::serve`], over this fixture's
/// store and desk, exactly as `Harness::serve_reviews` runs it: the queue's session is the host,
/// this daemon's lease identity is the owner, and `deliver` answers as the real door does for a
/// session this fixture does not hold.
fn served(store: &Store, desk: &ReviewDesk, me: &str) -> crate::mergequeue::review::Served {
    let runner = NoReviewers;
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let deliver = |_: &str, _: &str| Err("no session is live in this fixture".to_string());
    let pool = crate::mergequeue::review::Pool {
        store,
        runner: &runner,
        desk,
        host: QUEUE_SESSION,
        me,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, e| events(e),
        deliver: &deliver,
        settings: &settings,
        live: &|_: &str| true,
        clones: std::env::temp_dir(),
        slots: &Vec::new,
    };
    crate::mergequeue::review::serve(&pool).expect("a pass of the reviewers' host")
}

/// **A reviewer has started on `id` and handed its verdict in at the desk** — the two acts that
/// belong to the child, and the state the queue's worker then has to read correctly. The row is
/// left `reviewing` under that child and owned by `me`, which is what the claim wrote.
///
/// `tree` is the checkout the reviewer was given, and it is load-bearing rather than decoration:
/// `review::verdict` asks whether that tree is still there before it writes a judgement (a verdict
/// about a tree that is gone is not a verdict about the branch, so it is a WAIT). A fixture that
/// named `""` there would hand the pass a reviewer whose tree does not exist, and the row would
/// wait for a tree for ever — which is what this fixture did, and what the two tests below caught
/// when `main` grew that rule (2026-10-30).
fn a_reviewer_has_judged(
    store: &Store,
    desk: &ReviewDesk,
    id: &str,
    child: &str,
    branch: &str,
    tree: &Path,
) {
    assert!(
        store
            .move_review(
                id,
                ReviewState::Reviewing,
                ReviewState::Reviewing,
                &Move::new("pool", format!("reviewer {child} started")).set("reviewer", child),
                gate::now_ms(),
            )
            .unwrap(),
        "the fixture must be able to put a reviewer on the claimed row"
    );
    desk.opened(
        QUEUE_SESSION,
        child,
        id,
        ReviewRequest {
            brief: "do the work".into(),
            branch: branch.into(),
            base_sha: "base".into(),
            head_sha: String::new(),
            cwd: tree.display().to_string(),
        },
    );
    desk.submit(
        child,
        Submission {
            decision: Decision::Accept,
            reasons: vec!["the change does what the brief asked".into()],
            looked_at: LookedAt {
                files: vec!["vw.txt".into()],
                commands: vec!["git diff base...HEAD".into()],
            },
        },
    )
    .expect("the reviewer's verdict is taken at the desk");
}

/// **A reviewer whose verdict is waiting at the desk is not `gone`** — the defect measured on the
/// operator's daemon, 2026-10-29 evening.
///
/// A reviewer's last act is handing its verdict in (`review_verdict`, the daemon's desk) and its
/// turn ending — and a session whose turn has ended is not live. So the queue's worker read *"the
/// reviewer … is not alive in this daemon any more (it finished without handing a verdict in)"*
/// eight seconds after that reviewer HAD handed one in, re-seated the row and counted an attempt
/// for a review that had succeeded; the pass that then found the verdict (`review::serve`) found
/// the row was no longer its reviewer's and threw the judgement away. Eighteen `reviewing`
/// transitions and nine reviewers on one branch, and not one `approved`.
///
/// **The contract is BETWEEN the two loops, so both halves are here**: one pass of the worker
/// leaves a judged row exactly where it is — `reviewing`, no attempt spent, the reviewer still
/// named — and the next pass of the reviewers' host writes the verdict, and the row is `approved`.
#[test]
fn a_verdict_waiting_at_the_desk_is_not_a_dead_reviewer() {
    let w = World::new("verdict-waiting");
    let (wt, base, head) = branch(&w.repo, "vw", "vw.txt");
    w.enqueue("vw", &wt, &base, &head);
    let desk = Arc::new(ReviewDesk::new());
    let q = w.worker_holding(
        Arc::new(GreenGate),
        "a",
        QueueSettings::default(),
        Some(desk.clone()),
    );
    run(&q, 1);
    let s = w.store();
    assert_eq!(
        s.review_row("vw").unwrap().unwrap().state,
        ReviewState::Reviewing,
        "claimed by this worker"
    );
    a_reviewer_has_judged(&s, &desk, "vw", "sub-1", "agent/vw", &wt);
    assert!(
        w.door.live.lock().unwrap().is_empty(),
        "and its session is gone, as a reviewer's is the moment its turn ends"
    );

    // **The worker's pass.** The row is judged, so it is not the worker's to re-seat: no attempt,
    // no cleared reviewer, and no `waiting` transition at all.
    run(&q, 1);
    let r = s.review_row("vw").unwrap().unwrap();
    assert_eq!(
        r.state,
        ReviewState::Reviewing,
        "left under its reviewer: {r:?}"
    );
    assert_eq!(
        r.attempts, 0,
        "a review that succeeded spends no attempt: {r:?}"
    );
    assert_eq!(r.reviewer.as_deref(), Some("sub-1"), "{r:?}");
    assert!(
        s.transitions("review", "vw")
            .unwrap()
            .iter()
            .all(|t| t.to_state != "waiting" || t.from_state.is_none()),
        "nothing was re-seated: {:?}",
        s.transitions("review", "vw").unwrap()
    );

    // **The reviewers' host's pass** — the same desk, one loop later: the verdict is taken and the
    // row is `approved` at the head the claim read.
    served(&s, &desk, "test/a");
    let r = s.review_row("vw").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Approved, "the verdict landed: {r:?}");
    assert_eq!(r.reviewed_sha.as_deref(), Some(head.as_str()), "{r:?}");
    assert_eq!(r.verdict_by.as_deref(), Some("reviewer:sub-1"), "{r:?}");
    assert_eq!(
        r.attempts, 0,
        "and it is still the same review, not a retry"
    );
    assert!(
        !s.merge_rows(Some(&w.key())).unwrap().is_empty(),
        "an accept puts the branch in the merge queue"
    );
}

/// **A verdict handed in AT the deadline is still a verdict** — the `late` half of the same rule:
/// a reviewer that ran past its deadline and handed its judgement in is stopped by nobody, because
/// there is nothing left to stop it for.
#[test]
fn a_verdict_waiting_at_the_deadline_is_not_a_stopped_reviewer() {
    let w = World::new("verdict-at-deadline");
    let (wt, base, head) = branch(&w.repo, "vd", "vd.txt");
    w.enqueue("vd", &wt, &base, &head);
    let desk = Arc::new(ReviewDesk::new());
    let q = w.worker_holding(
        Arc::new(GreenGate),
        "a",
        QueueSettings::default(),
        Some(desk.clone()),
    );
    run(&q, 1);
    let s = w.store();
    a_reviewer_has_judged(&s, &desk, "vd", "sub-1", "agent/vd", &wt);
    // **Alive and past its deadline**: only the deadline is true, so this is the `late` branch and
    // not the `gone` one. Its reviewer is held here so the two cannot be confused.
    w.door.live.lock().unwrap().push("sub-1".into());
    assert!(
        s.move_review(
            "vd",
            ReviewState::Reviewing,
            ReviewState::Reviewing,
            &Move::new("pool", "still at it when the hour ran out").set("deadline_ms", 1u64),
            gate::now_ms(),
        )
        .unwrap()
    );

    run(&q, 1);
    let r = s.review_row("vd").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Reviewing, "not stopped: {r:?}");
    assert_eq!(r.attempts, 0, "and not counted: {r:?}");
    assert_eq!(r.reviewer.as_deref(), Some("sub-1"), "{r:?}");

    served(&s, &desk, "test/a");
    let r = s.review_row("vd").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Approved, "{r:?}");
    assert_eq!(r.reviewed_sha.as_deref(), Some(head.as_str()));
}

/// **A reviewer that died with nothing at the desk is re-seated and counted, exactly as before** —
/// the other half of the rule, and the reason the desk is PEEKED and not believed: an open review
/// with no verdict in it is not a judgement. Without this, the fix above would be a row stuck
/// `reviewing` for ever under a child that will never speak again.
#[test]
fn a_reviewer_that_died_without_handing_a_verdict_in_is_still_re_seated() {
    let w = World::new("reviewer-gone-no-verdict");
    let (wt, base, head) = branch(&w.repo, "gn", "gn.txt");
    w.enqueue("gn", &wt, &base, &head);
    let desk = Arc::new(ReviewDesk::new());
    let q = w.worker_holding(
        Arc::new(GreenGate),
        "a",
        QueueSettings::default(),
        Some(desk.clone()),
    );
    run(&q, 1);
    let s = w.store();
    // The desk KNOWS this child — the queue's session opened a review on it — and nothing has been
    // handed in: the reviewer died with its judgement unwritten. Its tree is the branch's worktree,
    // as a host reviewer's is the checkout it was given.
    desk.opened(
        QUEUE_SESSION,
        "sub-1",
        "gn",
        ReviewRequest {
            brief: "do the work".into(),
            branch: "agent/gn".into(),
            base_sha: "base".into(),
            head_sha: String::new(),
            cwd: wt.display().to_string(),
        },
    );
    assert!(!desk.verdict_waiting("sub-1"), "nothing was handed in");
    assert!(
        s.move_review(
            "gn",
            ReviewState::Reviewing,
            ReviewState::Reviewing,
            &Move::new("pool", "reviewer sub-1 started").set("reviewer", "sub-1"),
            gate::now_ms(),
        )
        .unwrap()
    );

    run(&q, 1);
    let r = s.review_row("gn").unwrap().unwrap();
    assert_eq!(r.attempts, 1, "the attempt is counted: {r:?}");
    let why = s
        .transitions("review", "gn")
        .unwrap()
        .into_iter()
        .rfind(|t| t.to_state == "waiting")
        .expect("the row was re-seated")
        .why;
    assert!(
        why.contains("sub-1") && why.contains("not alive"),
        "and the sentence is the honest one: {why}"
    );
}

// ===== The reviewer is alive while it is OPENING =====

/// **A model endpoint that says how big its window is, when the test says it may.**
///
/// The one thing a spawn does before its child's session exists is ask the PARENT's endpoint for
/// the child's window (`HarnessTaskRunner::start`'s `served_ctx`), so an endpoint that HOLDS that
/// answer holds the child inside its own open. That is the window the operator's daemon re-seated
/// a live reviewer in; holding it is what makes this test measure a state rather than race one.
struct HeldProps {
    url: String,
    state: Arc<(Mutex<PropsState>, std::sync::Condvar)>,
}

#[derive(Default)]
struct PropsState {
    /// How many asks have arrived — the proof that a child is inside its open.
    asked: usize,
    /// Set by [`HeldProps::release`]: this ask, and every later one, is answered.
    released: bool,
}

impl HeldProps {
    fn hold() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "127.0.0.1:{}",
            listener.local_addr().expect("a bound port").port()
        );
        let state: Arc<(Mutex<PropsState>, std::sync::Condvar)> = Default::default();
        let shared = state.clone();
        std::thread::Builder::new()
            .name("held-props".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    let Ok(mut stream) = conn else { return };
                    // Read the request head so the client's write completes. What it says does not
                    // matter: every ask wants the same one number.
                    let mut buf = [0u8; 2048];
                    let _ = std::io::Read::read(&mut stream, &mut buf);
                    let (lock, cv) = &*shared;
                    let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
                    g.asked += 1;
                    cv.notify_all();
                    while !g.released {
                        g = cv.wait(g).unwrap_or_else(|e| e.into_inner());
                    }
                    drop(g);
                    const BODY: &str = "{\"default_generation_settings\":{\"n_ctx\":4096}}";
                    let _ = std::io::Write::write_all(
                        &mut stream,
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                            BODY.len()
                        )
                        .as_bytes(),
                    );
                    let _ = std::io::Write::flush(&mut stream);
                }
            })
            .expect("the props thread");
        Self { url, state }
    }

    fn url(&self) -> &str {
        &self.url
    }

    /// Block until a spawn has asked, or give up: *a child is opening, and it is in there now*.
    fn wait_for_ask(&self, timeout: Duration) -> bool {
        let (lock, cv) = &*self.state;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = std::time::Instant::now() + timeout;
        while g.asked == 0 {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            let (next, _) = cv.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner());
            g = next;
        }
        true
    }

    fn release(&self) {
        let (lock, cv) = &*self.state;
        lock.lock().unwrap_or_else(|e| e.into_inner()).released = true;
        cv.notify_all();
    }
}

impl Drop for HeldProps {
    fn drop(&mut self) {
        // A held connection would otherwise sit in `served_ctx`'s 180 s read timeout.
        self.release();
    }
}

/// **A reviewer that is still OPENING is alive; one that ends without a verdict still is not.**
///
/// The operator's report, 2026-10-26: every review on their daemon was re-seated seconds after it
/// started and BLOCKED at *attempt 3 of 3*, with a sentence naming a reviewer that was *"not alive
/// in this daemon any more"*. The store's own history is the measurement: a reviewer started at
/// `…095383` was re-seated at `…097403` — 2.0 s, one queue pass.
///
/// The cause is not the re-seat rule but the answer it was given. A child's HANDLE exists before
/// its SESSION does: `HarnessTaskRunner::start` mints the id and returns it, and the hub is adopted
/// only once the child's own open has finished — which for a reviewer placed in a firecode VM is a
/// clone and a boot — so the registry answered *nothing by that id* about a child that had just
/// been started, and `Reviewing`'s upkeep believed it.
///
/// The child here is a real one, started by the review queue's own session through
/// `HarnessTaskRunner`, and the window is made a state the test stands in rather than a race it
/// hopes to lose: the child's open asks its parent's endpoint for a window, and the endpoint holds
/// that answer until the assertions below are done. It then ends — its turn needs a model this
/// fixture does not have — and the row is re-seated, which is the half the rule exists for.
#[test]
fn a_reviewer_that_is_still_opening_is_not_re_seated_and_one_that_ends_is() {
    let dir = TempDir::new("reviewer-opening");
    let repo = repo(&dir.0);
    let (wt, base, head) = branch(&repo, "opening", "opening.txt");
    let db = dir.0.join("sessions.db");
    let props = HeldProps::hold();

    let mut cfg = crate::config::Config::for_this_box(&repo);
    // **The byte vocabulary, so this test needs no GGUF**: a reviewer's liveness has nothing to do
    // with a model file. It is also why the child's first turn is refused the moment it opens (no
    // model is chosen), which is what makes *the child ended without a verdict* an event rather
    // than a wait.
    cfg.vocab_gguf = None;
    cfg.store = Some(db.clone());
    cfg.session_id = "s-reviewer-opening".into();
    // **The reviewer's clone goes in this fixture's own directory** — a sweep refuses any directory
    // not named `CLONES_DIR`, and a test must never write into (or sweep) the box's own.
    cfg.review_clones = Some(dir.0.join(crate::mergequeue::review::CLONES_DIR));
    // A loopback endpoint of ours: never the box's model server.
    cfg.endpoint = letibot_turn::Endpoint::parse(props.url()).expect("a loopback endpoint");
    cfg.http_retries = 0;
    cfg.context_window = Some(4096);
    cfg.permission = Vec::new();
    cfg.web_search = None;
    // **A fixture must open on a box with no namespaces** — the reviewer child is seated
    // `gatekeeper`, which wants a confined backend where there is one, and nothing here asserts a
    // boundary.
    cfg.unconfined = true;

    let registry = letibot_sessionlog::registry::Registry::new();
    registry
        .create(&cfg.session_id, "", crate::sessions::Sessions::wiring(&cfg))
        .expect("the daemon's own session registers");
    let parts = crate::harness::Parts::load(&cfg).expect("the byte vocabulary needs no file");
    *parts.mode_store.write().expect("the mode store") = crate::modes::ModeStore::default();
    let mut sessions = crate::sessions::Sessions::open_first(&parts, cfg.clone(), registry.clone())
        .expect("the daemon's own session opens");
    sessions
        .ensure_gatekeeper()
        .expect("the review queue's session opens");

    // The row, and the lease that lets this daemon claim it.
    let me = crate::mergequeue::lease::this_daemon().clone();
    let key = repo.display().to_string();
    let s = Store::open(&db).expect("the store opens");
    assert!(matches!(
        s.acquire_queue_lease(&key, &me, gate::now_ms(), LEASE_TTL_MS)
            .expect("the lease"),
        LeaseOutcome::Held { .. }
    ));
    s.enqueue_review(
        &NewReview {
            id: "opening".into(),
            repository: key.clone(),
            target: "main".into(),
            kind: ReviewKind::Branch,
            session_id: "s-opening".into(),
            branch: "agent/opening".into(),
            worktree: Some(wt.display().to_string()),
            base_sha: base,
            head_sha: head,
            brief: "read this branch".into(),
            priority: MergePriority::Subagent,
            fixes: None,
            evidence: "enqueued".into(),
        },
        "test",
        gate::now_ms(),
    )
    .expect("the review row");

    let env = Arc::new(QueueEnv {
        me: me.clone(),
        gate: Arc::new(GreenGate),
        // **The daemon's own door, not a test double** — the question this test is about is the
        // registry the spawn announces into, and a door that answers from a list would answer it
        // whichever way it was written.
        door: Arc::new(crate::mergequeue::RegistryDoor {
            registry: registry.clone(),
            store_path: db.clone(),
        }),
        // Nothing here starts a reviewer through the desk; the queue is asked about a row whose
        // child this daemon holds, and no verdict is waiting at any desk.
        desk: None,
        settings: Arc::new(|_| Ok(QueueSettings::default())),
        landing_root: test_landing_root(),
        gate_deadline: Duration::from_secs(60),
        review_deadline_ms: REVIEW_DEADLINE_MS,
        reviewer_start_grace_ms: REVIEWER_START_GRACE_MS,
        idle: Duration::from_millis(50),
    });
    let q = RepoQueue::new(
        Store::open(&db).expect("the queue's own connection"),
        repo.clone(),
        env,
    );

    // Claimed by the queue's pass, which rings the review queue's session.
    run(&q, 1);
    let claimed = s.review_row("opening").expect("the row").expect("a row");
    assert_eq!(claimed.state, ReviewState::Reviewing, "{claimed:?}");
    assert!(claimed.reviewer.is_none(), "{claimed:?}");

    // The reviewer itself, started by that session through the real runner.
    assert_eq!(sessions.serve_reviews(), 1, "one reviewer is started");
    let row = s.review_row("opening").expect("the row").expect("a row");
    let reviewer = row.reviewer.clone().expect("the row names its reviewer");
    assert!(
        reviewer.starts_with(&format!("{}-sub-", sessions.review_session())),
        "the reviewer is a child of this daemon's review session: {reviewer}"
    );
    assert_eq!(row.attempts, 0, "{row:?}");

    // **Provably inside its open**: it has asked for its window and has not been answered. The
    // handle exists, and the session it names does not.
    assert!(
        props.wait_for_ask(Duration::from_secs(30)),
        "the reviewer never asked its endpoint for a window, so it never started opening"
    );
    assert!(
        registry.get(&reviewer).is_none(),
        "the session does not exist yet — this is the window"
    );

    // Supervisor passes while it is opening: the operator's three attempts were exactly this, a
    // pass every couple of seconds against a child that had not finished opening.
    run(&q, 3);
    let still = s.review_row("opening").expect("the row").expect("a row");
    assert_eq!(
        still.state,
        ReviewState::Reviewing,
        "a reviewer that is opening is still a reviewer: {still:?}"
    );
    assert_eq!(
        still.reviewer.as_deref(),
        Some(reviewer.as_str()),
        "the same reviewer, not a second one: {still:?}"
    );
    assert_eq!(
        still.attempts, 0,
        "a reviewer that is opening spends no attempt: {still:?}"
    );

    // Let it finish opening. It has no model to turn to, so its first turn is refused and the child
    // ENDS — without a verdict, which is the one case this rule must still re-seat.
    props.release();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut after = None;
    while std::time::Instant::now() < deadline {
        sessions.serve_reviews();
        let now = s.review_row("opening").expect("the row").expect("a row");
        if now.state == ReviewState::Waiting {
            after = Some(now);
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let after = after.expect("a reviewer that ended without a verdict is re-seated");
    assert_eq!(after.attempts, 1, "the attempt is counted: {after:?}");
    assert!(after.reviewer.is_none(), "{after:?}");
    let why = s
        .transitions("review", "opening")
        .expect("the history")
        .into_iter()
        .rfind(|t| t.to_state == "waiting")
        .expect("the re-seat is in the history")
        .why;
    assert!(
        why.contains(&reviewer),
        "the transition names the reviewer that ended: {why}"
    );
    assert!(
        !registry.opening(&reviewer) && registry.get(&reviewer).is_none(),
        "and the ended child is no longer alive: {reviewer}"
    );
}

/// **A fix branch opened for a tip the target has moved past is withdrawn, and its empty tree and
/// branch go with it** — the queue opened four of these in one morning, each with a worktree and a
/// fixer child, and every one ended empty.
#[test]
fn a_superseded_fix_branch_is_withdrawn_and_its_empty_tree_removed() {
    let w = World::new("fix-superseded");
    let red = commit(&w.repo, "broken", "x\n", "a commit that breaks the gate");
    let (branch_name, wt) = a_fix_tree(&w, &red, "0123abcd");
    fix_branch(&w, "fixer-1", &branch_name, &wt, &red);
    // `main` moves: the tip the fix was cut from is not the target's any more.
    let tip = commit(&w.repo, "broken", "y\n", "the tip after it");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let s = w.store();
    let r = s.review_row("fixer-1").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Withdrawn, "{r:?}");
    let why = s
        .transitions("review", "fixer-1")
        .unwrap()
        .last()
        .unwrap()
        .why
        .clone();
    assert!(why.starts_with(&format!("superseded by {tip}")), "{why}");
    assert!(
        !wt.exists(),
        "the tree of a fix that carries nothing is removed: {}",
        wt.display()
    );
    assert_eq!(
        git::branch_head(&w.repo, &branch_name),
        None,
        "and so is the branch"
    );
}

/// **…but a fix that carries a commit of its own keeps its tree, and a person is told where it
/// is** — `main` moving past a tip does not make the fixer's work wrong, only unlandable.
#[test]
fn a_superseded_fix_branch_that_carries_work_keeps_its_tree_and_says_so() {
    let w = World::new("fix-carries");
    let red = commit(&w.repo, "broken", "x\n", "a commit that breaks the gate");
    let (wt, base, head) = branch(&w.repo, "fixx", "f.txt");
    assert_eq!(base, red, "cut from the red tip");
    let branch_name = "agent/fixx".to_string();
    fix_branch(&w, "fixer-2", &branch_name, &wt, &red);
    let tip = commit(&w.repo, "broken", "y\n", "the tip after it");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let s = w.store();
    let r = s.review_row("fixer-2").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Withdrawn, "{r:?}");
    assert!(wt.exists(), "the tree holds the fixer's commit");
    assert_eq!(git::branch_head(&w.repo, &branch_name), Some(head));
    let said = w.door.warnings("queue_branch_kept");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("carries 1 commit") && said[0].contains("agent/fixx"),
        "{}",
        said[0]
    );
    assert!(said[0].contains(&tip), "and it names the tip that did it");
}

/// **A superseded fix whose session is still live keeps its tree too** (R92.24): nothing is
/// removed from under a session that is still working in it.
#[test]
fn a_superseded_fix_branch_whose_session_is_live_keeps_its_tree() {
    let w = World::new("fix-live");
    let red = commit(&w.repo, "broken", "x\n", "a commit that breaks the gate");
    let (branch_name, wt) = a_fix_tree(&w, &red, "abcd0123");
    fix_branch(&w, "fixer-3", &branch_name, &wt, &red);
    w.door.live.lock().unwrap().push("s-fixer-3".into());
    commit(&w.repo, "broken", "y\n", "the tip after it");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let r = w.store().review_row("fixer-3").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Withdrawn, "{r:?}");
    assert!(wt.exists(), "a live session's tree is not removed");
    let said = w.door.warnings("queue_branch_kept");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("still live"), "{}", said[0]);
}

/// A worktree on a fresh branch cut from `at`, with nothing committed on it — what a fixer's tree
/// is between the spawn and its first commit.
fn a_fix_tree(w: &World, at: &str, short: &str) -> (String, PathBuf) {
    let branch = format!("agent/fix-main-{short}");
    let wt = w.dir.0.join(format!("wt-fix-{short}"));
    sh(
        &w.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            &wt.to_string_lossy(),
            at,
        ],
    );
    (branch, wt)
}

/// The review row a fixer's finished branch gets (`enqueue_finished`), carrying the red tip it was
/// opened for — the fact that makes it a fix and the only branch a red target lands.
fn fix_branch(w: &World, id: &str, branch: &str, wt: &Path, red: &str) {
    w.store()
        .enqueue_review(
            &NewReview {
                id: id.into(),
                repository: w.key(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: format!("s-{id}"),
                branch: branch.into(),
                worktree: Some(wt.display().to_string()),
                base_sha: red.into(),
                head_sha: git::branch_head(&w.repo, branch).unwrap_or_else(|| red.into()),
                brief: "fix main".into(),
                priority: MergePriority::Urgent,
                fixes: Some(red.into()),
                evidence: format!("put up for review onto `main` — it fixes the red tip {red}"),
            },
            "test",
            gate::now_ms(),
        )
        .unwrap();
}

/// **Recovery after a lease changes hands** — a row the gone owner was gating goes back in line;
/// the new owner lands it (requirement 1). And the old owner, still running, finds out at its
/// next heartbeat and stops.
#[test]
fn a_new_lease_holder_recovers_what_the_old_one_held_and_the_old_one_stops() {
    let w = World::new("takeover");
    let (wt, base, head) = branch(&w.repo, "t", "t.txt");
    w.enqueue("t", &wt, &base, &head);
    let old = w.worker(Arc::new(GreenGate), "old");
    run(&old, 1);
    let m = w.accept("t", "test/old");
    let s = w.store();
    // The old owner took the row and died mid-gate.
    s.move_merge(
        &m,
        MergeState::Queued,
        MergeState::Gating,
        &Move::new("test/old", "taken").set("owner", "test/old"),
        gate::now_ms(),
    )
    .unwrap();
    // Its heartbeat stops; a new daemon takes the lease after it expires.
    let new = me("new");
    let later = gate::now_ms() + LEASE_TTL_MS + 1;
    let LeaseOutcome::Held { previous } = s
        .acquire_queue_lease(&w.key(), &new, later, LEASE_TTL_MS)
        .unwrap()
    else {
        panic!("an expired lease is taken over");
    };
    assert_eq!(previous.unwrap().owner, "test/old");
    assert_eq!(
        s.recover_queue(&w.key(), &new.owner, later).unwrap(),
        1,
        "the gating row the old owner held"
    );
    let q = RepoQueue::new(
        w.store(),
        w.repo.clone(),
        env(Arc::new(GreenGate), w.door.clone(), "new"),
    );
    let stop = AtomicBool::new(false);
    assert_eq!(
        old.pass(&stop).unwrap(),
        Pass::LostLease,
        "the old owner stops"
    );
    run(&q, 3);
    assert_eq!(s.merge_row(&m).unwrap().unwrap().state, MergeState::Landed);
}

/// **A store error is weather** — retried with backoff, said once, never the end of a thread
/// (R67 item 12, kept).
#[test]
fn the_retry_wait_doubles_and_is_capped() {
    use crate::mergequeue::{STORE_RETRY_CAP, STORE_RETRY_FIRST, store_retry_wait};
    assert_eq!(store_retry_wait(1), STORE_RETRY_FIRST);
    assert_eq!(store_retry_wait(2), STORE_RETRY_FIRST * 2);
    assert_eq!(store_retry_wait(40), STORE_RETRY_CAP);
}

// ===== The supervisor =====

/// **The service, as a daemon runs it**: it finds the repository its workspace is in, claims the
/// lease, serves both queues on its own threads, resolves a row carried over from v23, and on
/// stop joins its workers and releases the lease (the v23 thread was detached and never joined).
#[test]
fn the_service_serves_the_queues_and_stops() {
    let w = World::new("service");
    let (wt, base, head) = branch(&w.repo, "svc", "svc.txt");
    w.enqueue("svc", &wt, &base, &head);
    // A carried-over v23 row: no repository yet, only its worktree.
    let (wt2, base2, head2) = branch(&w.repo, "carried", "c.txt");
    w.store()
        .enqueue_review(
            &NewReview {
                id: "carried".into(),
                repository: String::new(),
                target: String::new(),
                kind: ReviewKind::Branch,
                session_id: "s-old".into(),
                branch: "agent/carried".into(),
                worktree: Some(wt2.display().to_string()),
                base_sha: base2,
                head_sha: String::new(),
                brief: "old".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "carried forward".into(),
            },
            "migration v24",
            gate::now_ms(),
        )
        .unwrap();
    let door = w.door.clone();
    let e = env(Arc::new(GreenGate), door.clone(), "svc");
    let me = e.me.owner.clone();
    // A daemon seated IN the repository: the session daemon of it. (Seated at `w.dir`, which
    // merely contains the repository and its worktrees, it serves nothing —
    // `a_daemon_above_a_repository_neither_takes_its_lease_nor_shows_its_red`.)
    let service = QueueService {
        store_path: w.db.clone(),
        workspace: w.repo.clone(),
        env: e,
        not_target_said: Default::default(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let h = std::thread::spawn({
        let stop = stop.clone();
        move || service.run(&stop)
    });
    let s = w.store();
    let until = std::time::Instant::now() + Duration::from_secs(20);
    while s.review_row("svc").unwrap().unwrap().state != ReviewState::Reviewing {
        assert!(std::time::Instant::now() < until, "never claimed");
        std::thread::sleep(Duration::from_millis(50));
    }
    let carried = s.review_row("carried").unwrap().unwrap();
    assert_eq!(carried.repository, w.key(), "the carried row was resolved");
    assert_eq!(carried.head_sha, head2);
    assert_eq!(carried.target, "main");
    let m = w.accept("svc", &me);
    while s.merge_row(&m).unwrap().unwrap().state != MergeState::Landed {
        assert!(std::time::Instant::now() < until, "never landed");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(s.queue_leases().unwrap().iter().any(|l| l.owner == me));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    h.join().unwrap();
    assert!(
        s.queue_leases().unwrap().iter().all(|l| l.owner != me),
        "the lease is released on stop"
    );
    let _ = head;
}

// ===== Who serves a repository, and who hears its alerts (2026-10-12) =====

/// **A gate whose step is killed at the gate's cap** — the words the local gate says, with the
/// numbers measured on letibot's `main` at 79c32c1.
struct KilledAtCapGate;

impl GateRunner for KilledAtCapGate {
    fn run(&self, _wt: &Path, steps: &[String], _ctl: &GateControl) -> GateRun {
        GateRun {
            steps: steps
                .iter()
                .map(|s| letibot_tokencore::store::MergeGateStep {
                    command: s.clone(),
                    outcome: letibot_tokencore::store::MergeGateOutcome::Killed,
                    output:
                        "warning: `letibot-tui` (lib test) generated 17 warnings\n[gate memory: \
                             peak 24.1 GiB of a 24.9 GiB cap]"
                            .into(),
                    started_ms: gate::now_ms(),
                    elapsed_ms: 74_332,
                })
                .collect(),
            verdict: GateVerdict::Infra(gate::killed_at_cap_words(
                &steps[0],
                "peak 24.1 GiB of a 24.9 GiB cap",
                "it died with its peak within 5% of the cap",
                128,
            )),
        }
    }
}

/// **A tip killed at its cap is not re-gated at once by every daemon that takes the queue**
/// (R92.25): the wait doubles per kill; and the kill is said once per tip, not once per re-gate.
#[test]
fn a_tip_killed_at_its_cap_backs_off_before_a_daemon_regates_it() {
    let w = World::new("killedbackoff");
    let q = w.worker(Arc::new(KilledAtCapGate), "a");
    run(&q, 1);
    assert_eq!(
        w.store()
            .target_health(&w.key(), "main")
            .unwrap()
            .unwrap()
            .state,
        HealthState::Blocked
    );
    assert!(
        crate::mergequeue::verbs::regate_at_start(&w.store(), &w.key(), "test/b")
            .unwrap()
            .is_empty(),
        "killed a moment ago: it waits"
    );
    assert_eq!(
        crate::mergequeue::verbs::killed_backoff_ms(1),
        15 * 60 * 1000
    );
    assert_eq!(
        crate::mergequeue::verbs::killed_backoff_ms(2),
        30 * 60 * 1000
    );
    assert_eq!(
        crate::mergequeue::verbs::killed_backoff_ms(40),
        8 * 60 * 60 * 1000
    );
    // `/queue regate` is the operator's way past the wait; killed again, it is not said again.
    crate::mergequeue::verbs::regate(&w.store(), &w.key(), "main", "operator:s-1", &|_| {})
        .unwrap();
    run(&q, 1);
    assert_eq!(w.door.warnings("gate_killed").len(), 1);
    let key = killed_key(&w.key(), "main", &w.tip());
    assert_eq!(w.store().told(&key).unwrap().unwrap().count, 2);
}

/// **A landing gate killed at its cap says it was killed, and names `restart`** (R92.25).
#[test]
fn a_landing_gate_killed_at_its_cap_says_so_and_names_restart() {
    let w = World::new("killedmerge");
    let (wt, base, head) = branch(&w.repo, "k", "k.txt");
    w.enqueue("k", &wt, &base, &head);
    let green = w.worker(Arc::new(GreenGate), "a");
    run(&green, 1);
    let m = w.accept("k", "test/a");
    let killed = {
        let env = env(Arc::new(KilledAtCapGate), w.door.clone(), "a");
        RepoQueue::new(w.store(), w.repo.clone(), env)
    };
    run(&killed, 1);
    assert_eq!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Blocked
    );
    let said = w.door.warnings("queue_blocked");
    let s = said.last().unwrap();
    assert!(s.contains("KILLED at its memory cap"), "{s}");
    assert!(s.contains(&format!("/queue restart {m}")), "{s}");
    assert!(!s.contains("could not run"), "{s}");
}

/// **A target whose gate was killed at its memory cap is UNJUDGED, not red** — `blocked`, nothing
/// lands, no red-tip review, no `target_red`; the project is rung with *cannot judge* and the hubs
/// are told `gate_killed`, naming the cap, the peak and `[memory] gate`.
#[test]
fn a_target_whose_gate_was_killed_at_its_cap_is_unjudged_not_red() {
    let w = World::new("killedcap");
    let q = w.worker(Arc::new(KilledAtCapGate), "a");
    run(&q, 2);
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Blocked, "{h:?}");
    assert!(h.evidence.starts_with(gate::GATE_KILLED), "{}", h.evidence);
    assert!(crate::mergequeue::setup::unjudged(&h.evidence));
    assert!(emergency_health(&h), "the project hears it");
    assert!(w.door.warnings("target_red").is_empty(), "never RED");
    let said = w.door.warnings("gate_killed");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("UNJUDGED, not red")
            && said[0].contains("peak 24.1 GiB of a 24.9 GiB cap")
            && said[0].contains("[memory] gate"),
        "{}",
        said[0]
    );
    assert!(
        w.store()
            .review_rows(Some(&w.key()))
            .unwrap()
            .iter()
            .all(|r| r.kind != ReviewKind::Target),
        "no reviewer is sent to find what in the code broke it"
    );
    let notice = crate::mergequeue::notices::health(&h);
    assert!(
        notice.starts_with("The merge queue CANNOT JUDGE"),
        "{notice}"
    );
    assert_eq!(*w.door.repositories.lock().unwrap(), vec![w.key()]);
}

/// **A gate that could not run** — the verdict a host produces when its poll was not answered, or
/// when ssh could not dial it. Nothing about the code was read.
struct CouldNotRunGate {
    runs: Arc<std::sync::atomic::AtomicUsize>,
}

impl CouldNotRunGate {
    fn new() -> (CouldNotRunGate, Arc<std::sync::atomic::AtomicUsize>) {
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        (CouldNotRunGate { runs: runs.clone() }, runs)
    }
}

impl GateRunner for CouldNotRunGate {
    fn run(&self, _wt: &Path, steps: &[String], _ctl: &GateControl) -> GateRun {
        self.runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        GateRun {
            steps: steps
                .iter()
                .map(|s| letibot_tokencore::store::MergeGateStep {
                    command: s.clone(),
                    outcome: letibot_tokencore::store::MergeGateOutcome::NotRun,
                    output: String::new(),
                    started_ms: 0,
                    elapsed_ms: 0,
                })
                .collect(),
            verdict: GateVerdict::Infra(
                "the gate could not run on the declared host: asking how `lab2x1` is doing on \
                 lab2x1 did not answer within 30 s and the ssh this daemon started was killed with \
                 its whole process group. Nothing was judged — the tip is UNJUDGED, not red, and \
                 the queue is not halted by it: it is gated again on a later pass."
                    .into(),
            ),
        }
    }
}

/// **A gate that could not run does not halt the queue** — the 2026-10-30 halt, pinned.
///
/// A first gate on `lab2x1` was killed by a poll that took longer than ten seconds on a machine
/// busy compiling its own workspace, and the target went `blocked` — which stops BOTH queues, so
/// four branches that had just been re-queued sat behind a host that was healthy. A run that could
/// not start judged nothing, so the tip goes back to `gating` and is tried again by itself: not
/// `blocked` (nothing may land while a tip is unjudged, which is right) and not `red` (nothing
/// about the code was read).
#[test]
fn a_gate_that_could_not_run_leaves_the_target_gating_and_tries_again_by_itself() {
    let w = World::new("unjudgedretry");
    let (gate, runs) = CouldNotRunGate::new();
    let q = w.worker(Arc::new(gate), "a");
    run(&q, 2);
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Gating, "{h:?}");
    assert!(h.evidence.starts_with(UNJUDGED_RETRY), "{}", h.evidence);
    assert!(h.evidence.contains("UNJUDGED, not red"), "{}", h.evidence);
    // Nothing is in doubt about the code: no red tip, no review of the target, no emergency.
    assert!(!emergency_health(&h), "nothing to ring a project about");
    assert!(w.door.warnings("target_red").is_empty(), "never RED");
    assert!(
        w.store()
            .review_rows(Some(&w.key()))
            .unwrap()
            .iter()
            .all(|r| r.kind != ReviewKind::Target),
        "nobody is sent to find what in the code broke it"
    );
    // Said once, on the hub, so an operator learns the host is not answering — and once per tip,
    // not once per attempt.
    let said = w.door.warnings("target_unjudged");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("UNJUDGED"), "{}", said[0]);
    // **The window**: the very next pass does not dial the host again. Without it a broken
    // `gate_host` would be one ssh attempt per second, because the worker goes again at once when
    // a pass worked.
    assert_eq!(runs.load(std::sync::atomic::Ordering::Relaxed), 1);
    run(&q, 2);
    assert_eq!(
        runs.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the host is not asked again inside its retry window"
    );
    // ... and once the window is out, the next pass gates the tip again — by itself, which is the
    // whole point: no `/queue regate`, no restart, no operator.
    let mut backdated = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    backdated.checked_ms -= UNJUDGED_RETRY_MS;
    w.store().set_target_health(&backdated, "test").unwrap();
    run(&q, 2);
    assert_eq!(
        runs.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "the tip is gated again once the window is out"
    );
    // Still not blocked, and still not red — a host that keeps not answering is not a verdict.
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Gating, "{h:?}");
    assert!(w.door.warnings("target_red").is_empty());
}

/// **A branch whose own gate could not run goes back in line, and waits its window too** — the
/// landing half of the same rule: parked `blocked` it needed `/queue restart`, and taken again on
/// the next pass it was one ssh attempt per second at a host that was down.
#[test]
fn a_landing_gate_that_could_not_run_goes_back_in_line_and_waits() {
    let w = World::new("unjudgedlanding");
    let (wt, base, head) = branch(&w.repo, "u", "u.txt");
    w.enqueue("u", &wt, &base, &head);
    let green = w.worker(Arc::new(GreenGate), "a");
    run(&green, 1);
    let m = w.accept("u", "test/a");
    let (gate, runs) = CouldNotRunGate::new();
    let q = {
        let env = env(Arc::new(gate), w.door.clone(), "a");
        RepoQueue::new(w.store(), w.repo.clone(), env)
    };
    run(&q, 2);
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Queued, "{row:?}");
    assert!(row.evidence.starts_with(UNJUDGED_RETRY), "{}", row.evidence);
    assert!(
        row.evidence.contains("goes back in line"),
        "{}",
        row.evidence
    );
    let attempts = runs.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(attempts, 1, "one attempt, then the window");
    run(&q, 2);
    assert_eq!(
        runs.load(std::sync::atomic::Ordering::Relaxed),
        attempts,
        "the branch is not taken again inside its window"
    );
    // And it is still the operator's to unstick or wait out: the row is not `blocked`.
    assert_ne!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Blocked
    );
}

/// **A daemon seated ABOVE a repository is none of its session daemons** — the operator's
/// `/home/dead` daemon took letibot's lease, gated its `main`, and showed `‼ main RED` to a
/// session about the box's RSS. Seated in the directory that holds the repository, its worktree
/// and another checkout, it takes no lease and says nothing; the row waits. A daemon seated in
/// the repository — or in one of its worktrees — is one of them, and takes it.
#[test]
fn a_daemon_above_a_repository_neither_takes_its_lease_nor_shows_its_red() {
    let w = World::new("ancestor");
    commit(&w.repo, "broken", "x\n", "red");
    let (wt, base, head) = branch(&w.repo, "anc", "anc.txt");
    w.enqueue("anc", &wt, &base, &head);
    let other = w.dir.0.join("other");
    std::fs::create_dir_all(&other).unwrap();
    sh(&other, &["init", "-q", "-b", "main"]);
    let home = w.dir.0.clone();

    assert_eq!(crate::mergequeue::repository_of_workspace(&home), None);
    assert!(!crate::mergequeue::serves(&home, &w.repo));
    assert!(crate::mergequeue::serves(&w.repo, &w.repo));
    assert!(
        crate::mergequeue::serves(&wt, &w.repo),
        "a worktree is its repository's"
    );
    assert!(!crate::mergequeue::serves(&other, &w.repo));

    let serve_for = |workspace: PathBuf, who: &str, until: &dyn Fn(&Store) -> bool| {
        let e = env(Arc::new(LocalGate), w.door.clone(), who);
        let service = QueueService {
            store_path: w.db.clone(),
            workspace,
            env: e,
            not_target_said: Default::default(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let h = std::thread::spawn({
            let stop = stop.clone();
            move || service.run(&stop)
        });
        let s = w.store();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !until(&s) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        h.join().unwrap();
    };

    // The ancestor: three supervisor passes' worth of time, and nothing.
    let started = std::time::Instant::now();
    serve_for(home, "home", &|_| {
        started.elapsed() > Duration::from_millis(4500)
    });
    assert!(
        w.store().queue_leases().unwrap().is_empty(),
        "no lease taken"
    );
    assert!(
        w.store().target_health(&w.key(), "main").unwrap().is_none(),
        "nothing gated"
    );
    assert!(
        w.door.events.lock().unwrap().is_empty(),
        "nothing said on its hubs"
    );
    assert_eq!(
        w.store().review_row("anc").unwrap().unwrap().state,
        ReviewState::Waiting,
        "the row waits for a daemon seated in its repository"
    );

    // The repository's own daemon: the lease, the gate, and the red said for THIS repository.
    serve_for(w.repo.clone(), "repo", &|s| {
        s.target_health(&w.key(), "main")
            .ok()
            .flatten()
            .is_some_and(|h| h.state == HealthState::Red)
    });
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Red, "{h:?}");
    let scoped = w.door.scoped.lock().unwrap().clone();
    assert!(
        scoped.contains(&(w.key(), "target_red".to_string())),
        "the red is published for the repository, not on every hub: {scoped:?}"
    );
}

/// **`--no-queue`: a daemon seated in a package's worktree serves no queue** — it claims no
/// lease and gates nothing — and the flag is the whole of the difference, because the same tree
/// without it claims the lease at once.
///
/// The defect (`docs/claude-code-bridge.md`, 2026-10-26): a daemon started by the Claude Code
/// bridge for package P1 in a fresh worktree of this repository is one of its session daemons by
/// [`crate::mergequeue::serves`]'s rule, so it took letibot's lease, found `main` red and put an
/// EMERGENCY in front of the package's model before its brief. This is the queue's half of that
/// flag; the model's half is `harness::queue_e2e`'s
/// `a_no_queue_daemon_tells_its_sessions_nothing_about_the_queues`.
#[test]
fn a_daemon_started_with_no_queue_on_a_worktree_never_takes_the_lease() {
    use letibot_sessionlog::registry::Registry;
    let w = World::new("noqueue");
    let (wt, _, _) = branch(&w.repo, "nq", "nq.txt");
    let mut cfg = crate::config::Config::for_this_box(&wt);
    cfg.store = Some(w.db.clone());

    cfg.serve_queues = false;
    let stop = Arc::new(AtomicBool::new(false));
    let spawned = crate::mergequeue::spawn_for(
        &cfg,
        stop.clone(),
        Registry::new(),
        Arc::new(letibot_tools::builtins::verdict::ReviewDesk::new()),
    );
    assert!(
        spawned.is_none(),
        "a --no-queue daemon starts no queue thread at all"
    );
    // Four supervisor passes' worth of time — how the neighbour test measures *it did nothing*:
    // a supervisor that meant to claim would have by now.
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_millis(4500) {
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(
        w.store().queue_leases().unwrap().is_empty(),
        "no lease taken"
    );
    assert!(
        w.store().target_health(&w.key(), "main").unwrap().is_none(),
        "nothing gated"
    );

    // The same worktree and the same store without the flag: the lease IS taken — so the two
    // assertions above are about the flag, not about a supervisor that could never claim here.
    cfg.serve_queues = true;
    let stop = Arc::new(AtomicBool::new(false));
    let spawned = crate::mergequeue::spawn_for(
        &cfg,
        stop.clone(),
        Registry::new(),
        Arc::new(letibot_tools::builtins::verdict::ReviewDesk::new()),
    )
    .expect("the queue's thread starts without --no-queue");
    let until = std::time::Instant::now() + Duration::from_secs(20);
    while w.store().queue_leases().unwrap().is_empty() {
        assert!(
            std::time::Instant::now() < until,
            "a daemon seated in a worktree never claimed its repository's lease"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    spawned.join().unwrap();
}

/// **The hubs a repository's alert reaches**: the sessions whose workspace is that repository
/// or one of its worktrees, and the review queue's board — not a session seated above it.
#[test]
fn a_repositorys_alert_reaches_its_sessions_and_the_board_and_not_the_home_session() {
    use letibot_sessionlog::registry::{Registry, SessionWiring};
    let w = World::new("hubs");
    let (wt, _, _) = branch(&w.repo, "h", "h.txt");
    let s = w.store();
    let seat = |id: &str, ws: &Path| {
        s.put_session(&letibot_tokencore::store::SessionRecord {
            id: id.into(),
            title: None,
            model_id: String::new(),
            dialect_sha: String::new(),
            workspace_root: ws.display().to_string(),
            owner: String::new(),
            approvers: Vec::new(),
            role: None,
            parent_session_id: None,
        })
        .unwrap();
    };
    seat("s-home", &w.dir.0);
    seat("s-repo", &w.repo);
    seat("s-wt", &wt);
    let reg = Registry::new();
    // **The review queue's session of THIS daemon** — one per workspace, found by its shape
    // rather than by a constant every daemon on the box once shared.
    let board = crate::sessions::gatekeeper::session(&w.dir.0);
    let hubs: Vec<_> = ["s-home", "s-repo", "s-wt", board.as_str()]
        .iter()
        .map(|id| reg.create(*id, *id, SessionWiring::default()).unwrap())
        .collect();
    let reached = crate::mergequeue::publish_for_repository(
        &reg,
        Some(&w.db),
        &w.key(),
        SessionEvent::Warning {
            code: "target_red".into(),
            detail: "EMERGENCY".into(),
            compaction: None,
        },
    );
    assert_eq!(reached, 3);
    let heard = |i: usize| {
        hubs[i]
            .retained()
            .iter()
            .any(|e| matches!(&e.event, SessionEvent::Warning { code, .. } if code == "target_red"))
    };
    assert!(
        !heard(0),
        "the home session hears nothing of letibot's main"
    );
    assert!(heard(1) && heard(2) && heard(3));
}

// ===== The queue is the repositories that asked for it (2026-10-26) =====

/// **A repository named `name` under `under`, with `agents` as its `AGENTS.md`** — and an origin on
/// `github.com/stroppy-io/…`, so a folder holding two of them is ONE project
/// ([`crate::mergequeue::git::repos_of_workspace`] keeps the children only when every remote names
/// the same owner).
fn repo_with_agents(under: &Path, name: &str, agents: Option<&str>) -> PathBuf {
    let repo = under.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    sh(&repo, &["config", "user.email", "q@test"]);
    sh(&repo, &["config", "user.name", "queue test"]);
    sh(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            &format!("git@github.com:stroppy-io/{name}.git"),
        ],
    );
    if let Some(text) = agents {
        std::fs::write(repo.join("AGENTS.md"), text).unwrap();
    }
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    sh(&repo, &["add", "."]);
    sh(&repo, &["commit", "-q", "-m", "base"]);
    std::fs::canonicalize(repo).unwrap()
}

/// **Run a supervisor until `until` says so of the store** — the ancestor test's own loop, for the
/// tests that are about what a passing supervisor does rather than about a landing.
fn serve_until(
    workspace: &Path,
    gate: Arc<dyn GateRunner>,
    door: Arc<Door>,
    db: &Path,
    who: &str,
    until: &dyn Fn(&Store) -> bool,
) -> Store {
    let service = QueueService {
        store_path: db.to_path_buf(),
        workspace: workspace.to_path_buf(),
        env: env(gate, door, who),
        not_target_said: Default::default(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let h = std::thread::spawn({
        let stop = stop.clone();
        move || service.run(&stop)
    });
    let s = Store::open(db).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !until(&s) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    h.join().unwrap();
    s
}

/// **Serve `workspace` for a few passes, and record every lease held at any observation** — a
/// lease is given up when the service stops, so the question *was it ever claimed* is answered by
/// what was seen while it ran.
fn leases_seen_over(
    workspace: &Path,
    db: &Path,
    door: Arc<Door>,
    who: &str,
    for_ms: u64,
) -> (Store, Vec<String>) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let started = std::time::Instant::now();
    let watch = {
        let seen = seen.clone();
        move |s: &Store| {
            for l in s.queue_leases().unwrap() {
                let mut said = seen.lock().unwrap();
                if !said.contains(&l.repository) {
                    said.push(l.repository.clone());
                }
            }
            started.elapsed() > Duration::from_millis(for_ms)
        }
    };
    let store = serve_until(workspace, Arc::new(GreenGate), door, db, who, &watch);
    let out = seen.lock().unwrap().clone();
    (store, out)
}

/// **A repository whose target declares no `Merge gate` is not a queue target** — the operator,
/// 2026-10-26: *"red most of the time everywhere, no real visibility … something happens somewhere
/// and result is ignored"*. Their supervisor held a lease on all fifteen repositories of one
/// project folder and fourteen of them were `blocked: no Merge gate section in AGENTS.md`: rows for
/// repositories that had never asked for a queue, each counted *unjudged* in every head's summary
/// and warned about. So a repository asks by declaring a gate: until it does, it takes no lease, no
/// health row is written for it, and nothing about it reaches a hub — and the pass after it
/// commits one, it is served like the rest.
#[test]
fn a_repository_with_no_merge_gate_is_not_a_queue_target() {
    let dir = TempDir::new("opt-in");
    let queued = repo_with_agents(&dir.0, "one", Some(GATE_MD));
    let never = repo_with_agents(
        &dir.0,
        "two",
        Some("# agents\n\nNothing about a gate here.\n"),
    );
    assert_eq!(
        crate::mergequeue::repositories_of_workspace(&dir.0).len(),
        2,
        "one project of two repositories: both are found, and only one asked"
    );
    let queued_key = queued.display().to_string();
    let never_key = never.display().to_string();
    let db = dir.0.join("sessions.db");
    let door = Arc::new(Door::default());
    // **Two passes' worth of time**: the supervisor claims every repository it serves in ONE pass,
    // so a lease it would ever take would have been seen by the second.
    let (store, leases) = leases_seen_over(&dir.0, &db, door.clone(), "a", 3_500);
    assert_eq!(
        leases,
        vec![queued_key.clone()],
        "only the repository that asked is served"
    );
    assert!(
        store.target_health(&never_key, "main").unwrap().is_none(),
        "and no health row was written for the other: nothing is `blocked` about it"
    );
    assert!(
        store.target_health(&queued_key, "main").unwrap().is_some(),
        "the one that asked was gated"
    );
    assert!(
        !door.events.lock().unwrap().iter().any(|e| {
            matches!(e, SessionEvent::Warning { detail, .. } if detail.contains(&never_key))
        }),
        "nothing about it is said on any hub"
    );
    assert!(
        store
            .recent_transitions(crate::mergequeue::RECENT_MOVES)
            .unwrap()
            .iter()
            .all(|t| t.repository != never_key),
        "and nothing moves for it"
    );

    // **The section is the whole of the opt-in**, so committing one is the whole of what it takes
    // to be served: the next pass asks again and claims the lease.
    commit(&never, "AGENTS.md", GATE_MD, "declare a merge gate");
    let (store, leases) = leases_seen_over(&dir.0, &db, door.clone(), "a", 3_500);
    let mut leases = leases;
    leases.sort();
    assert_eq!(
        leases,
        vec![queued_key, never_key.clone()],
        "a repository that gains the section is served on the next pass"
    );
    assert!(
        store.target_health(&never_key, "main").unwrap().is_some(),
        "and gated, as any queue target is"
    );
}

/// **A stale lease whose holder is gone is given up by whoever passes** — the operator's store
/// held `queue_lease` rows from daemons that had been dead for weeks (`letim`, `fctop`), each drawn
/// as a repository *served by* a pid nobody could reach. Takeover is `acquire_queue_lease`'s and it
/// needs a claimant: no daemon serves those repositories now, so nobody ever passed and the rows
/// stood. A supervisor's pass sweeps them, and the history says why.
#[test]
fn a_stale_lease_whose_holder_is_gone_is_released_by_a_passing_supervisor() {
    let dir = TempDir::new("stale-lease");
    let db = dir.0.join("sessions.db");
    let store = Store::open(&db).unwrap();
    let now = gate::now_ms();
    // **No such pid**: above every box's `pid_max` and inside `pid_t`, so the kernel answers
    // *gone* on either platform rather than the test having to kill something to make it so.
    let dead_pid = 999_999_999u32;
    let claim = |owner: &str, pid: u32| LeaseClaim {
        owner: owner.into(),
        pid,
        host: "test".into(),
        build: "harnessd test".into(),
    };
    let long_ago = now - 10 * 60_000;
    for (repo, owner, pid, at) in [
        ("/projects/letim", "letim/1/1", dead_pid, long_ago),
        ("/projects/fctop", "fctop/2/2", std::process::id(), long_ago),
        ("/projects/stroppy", "stroppy/3/3", dead_pid, now),
    ] {
        assert!(matches!(
            store
                .acquire_queue_lease(repo, &claim(owner, pid), at, LEASE_TTL_MS)
                .unwrap(),
            LeaseOutcome::Held { .. }
        ));
    }
    // A workspace that is a session of no repository at all: the sweep is the SUPERVISOR's, which
    // is the half that matters — the rows the operator saw were for repositories nobody passes on.
    let door = Arc::new(Door::default());
    let store = serve_until(&dir.0, Arc::new(GreenGate), door.clone(), &db, "me", &|s| {
        s.queue_leases().unwrap().len() == 2
    });
    let left: Vec<String> = store
        .queue_leases()
        .unwrap()
        .into_iter()
        .map(|l| l.repository)
        .collect();
    assert_eq!(
        left,
        vec![
            "/projects/fctop".to_string(),
            "/projects/stroppy".to_string()
        ],
        "the dead daemon's lease is gone; a live pid's and a fresh heartbeat's are not"
    );
    let moves = store.transitions("lease", "/projects/letim").unwrap();
    let last = moves.last().expect("the release is in the history");
    assert_eq!(last.to_state, "released");
    assert_eq!(last.from_state.as_deref(), Some("letim/1/1"));
    assert!(
        last.why.contains("999999999") && last.why.contains("600 s old"),
        "the history says which pid and how old the heartbeat was: {}",
        last.why
    );
    assert_eq!(
        store.transitions("lease", "/projects/fctop").unwrap().len(),
        1,
        "a pid that IS running here keeps its row — the sweep deletes no live daemon's lease"
    );
    assert!(
        door.events.lock().unwrap().is_empty(),
        "it is said in the store's history, not on a hub: a dead lease is not an alarm"
    );
}

// ===== A gate does not stop the review queue (R92 item 8) =====

/// **A gate held shut until the test opens it** — a thirty-minute gate in miniature. It counts
/// its runs and, like every real runner, ends `stopped` when the daemon stops.
#[derive(Default)]
struct Held {
    open: AtomicBool,
    runs: Mutex<usize>,
}

impl Held {
    fn open(&self) {
        self.open.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    fn runs(&self) -> usize {
        *self.runs.lock().unwrap()
    }
}

impl GateRunner for Held {
    fn run(&self, wt: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
        *self.runs.lock().unwrap() += 1;
        while !self.open.load(std::sync::atomic::Ordering::Relaxed) {
            if ctl.stop.load(std::sync::atomic::Ordering::Relaxed) {
                return GateRun {
                    steps: vec![],
                    verdict: gate::GateVerdict::Stopped("the daemon is stopping".into()),
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        GreenGate.run(wt, steps, ctl)
    }
}

/// Run passes until `done` holds (or fail after ten seconds).
fn pass_until(q: &RepoQueue, done: impl Fn() -> bool) {
    let stop = AtomicBool::new(false);
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(std::time::Instant::now() < until, "never happened");
        assert_ne!(q.pass(&stop).unwrap(), Pass::LostLease);
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// **The R92 freeze**: while the target's gate runs — here, for as long as the test keeps it
/// shut — a pass still returns at once, claims a review, and enforces a reviewer's deadline; and
/// what the landings wait for is said: `gate running on <sha> for Nm`.
#[test]
fn a_running_gate_does_not_stop_claims_or_deadlines() {
    let w = World::new("gate-freeze");
    let held = Arc::new(Held::default());
    let q = w.worker(held.clone(), "a");
    let stop = AtomicBool::new(false);

    let started = std::time::Instant::now();
    q.pass(&stop).unwrap();
    assert!(q.gate_in_flight(), "the target's tip is being gated");
    let tip = w.tip();
    let said = q.gate_running().expect("the reason the landings wait");
    assert!(
        said.starts_with(&format!("gate running on {}", &tip[..10])) && said.ends_with("for 0m"),
        "{said}"
    );
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Gating);

    // A review enqueued mid-gate is claimed mid-gate.
    let (wt, base, head) = branch(&w.repo, "one", "one.txt");
    w.enqueue("one", &wt, &base, &head);
    q.pass(&stop).unwrap();
    let r = w.store().review_row("one").unwrap().unwrap();
    assert_eq!(
        r.state,
        ReviewState::Reviewing,
        "claimed while the gate runs: {r:?}"
    );
    assert!(*w.door.rung.lock().unwrap() >= 1, "and the reviewers rung");

    // Its reviewer started and ran past its deadline: enforced mid-gate.
    assert!(
        w.store()
            .move_review(
                "one",
                ReviewState::Reviewing,
                ReviewState::Reviewing,
                &Move::new("test", "the reviewer started")
                    .set("reviewer", "sub-1")
                    .set("deadline_ms", 1u64),
                gate::now_ms(),
            )
            .unwrap()
    );
    q.pass(&stop).unwrap();
    let late: Vec<_> = w
        .store()
        .transitions("review", "one")
        .unwrap()
        .into_iter()
        .filter(|t| t.to_state == "waiting" && t.why.contains("deadline"))
        .collect();
    assert_eq!(late.len(), 1, "the late reviewer was stopped mid-gate");
    assert_eq!(w.store().review_row("one").unwrap().unwrap().attempts, 1);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "no pass waited for the gate"
    );
    assert!(q.gate_in_flight(), "and the gate is still running");
    assert_eq!(
        w.store()
            .target_health(&w.key(), "main")
            .unwrap()
            .unwrap()
            .state,
        HealthState::Gating
    );

    held.open();
    pass_until(&q, || !q.gate_in_flight());
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Green, "{h:?}");
    assert_eq!(held.runs(), 1, "one gate of the tip, not one per pass");
    assert!(q.gate_running().is_none());
}

/// **A gate's verdict is applied exactly once** — however many passes look at it while it runs
/// and after it ended — and a landing's gate is never started twice.
#[test]
fn a_gate_verdict_is_applied_exactly_once() {
    let w = World::new("gate-once");
    let (wt, base, head) = branch(&w.repo, "one", "one.txt");
    w.enqueue("one", &wt, &base, &head);
    let q0 = w.worker(Arc::new(GreenGate), "a");
    run(&q0, 1);
    let m = w.accept("one", "test/a");
    drop(q0);

    let held = Arc::new(Held::default());
    let q = RepoQueue::new(
        w.store(),
        w.repo.clone(),
        env(held.clone(), w.door.clone(), "a"),
    );
    let stop = AtomicBool::new(false);
    // The gate runs on its own thread: under load the passes below can all return before it
    // starts, and the count then reads 0 (seen twice in full workspace runs). Wait for it to be
    // running, then pass again — the claim is that more passes do not start a second one.
    pass_until(&q, || held.runs() >= 1);
    for _ in 0..5 {
        q.pass(&stop).unwrap();
    }
    assert_eq!(held.runs(), 1, "one landing gate");
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Gating);
    let said = q.gate_running().unwrap();
    assert!(
        said.starts_with(&format!(
            "gate running on {}",
            &row.rebased_sha.as_deref().unwrap()[..10]
        )),
        "{said}"
    );

    held.open();
    pass_until(&q, || {
        w.store().merge_row(&m).unwrap().unwrap().state == MergeState::Landed
    });
    for _ in 0..3 {
        q.pass(&stop).unwrap();
    }
    let history: Vec<String> = w
        .store()
        .transitions("merge", &m)
        .unwrap()
        .into_iter()
        .map(|t| t.to_state)
        .collect();
    assert_eq!(history, vec!["queued", "gating", "gating", "landed"]);
    assert_eq!(held.runs(), 1);
    let greens = w
        .store()
        .transitions("target", "main")
        .unwrap()
        .into_iter()
        .filter(|t| t.to_state == "green")
        .count();
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.sha, w.tip(), "green at the landed tree: {h:?}");
    assert!(greens >= 1);
}

/// **A worker that ends mid-gate leaves nothing `gating`**: the gate is killed, its row goes back
/// in line, and the next worker gates it again and lands it once.
#[test]
fn a_worker_stopped_mid_gate_puts_the_row_back_and_the_next_one_lands_it() {
    let w = World::new("gate-restart");
    let (wt, base, head) = branch(&w.repo, "one", "one.txt");
    w.enqueue("one", &wt, &base, &head);
    let q0 = w.worker(Arc::new(GreenGate), "a");
    run(&q0, 1);
    let m = w.accept("one", "test/a");
    drop(q0);

    let held = Arc::new(Held::default());
    let q1 = RepoQueue::new(
        w.store(),
        w.repo.clone(),
        env(held.clone(), w.door.clone(), "a"),
    );
    q1.pass(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Gating
    );
    let started = std::time::Instant::now();
    drop(q1);
    assert!(started.elapsed() < Duration::from_secs(5));
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Queued, "{row:?}");
    assert_eq!(row.owner, "");
    assert_eq!(w.tip(), base, "nothing landed");

    held.open();
    let q2 = RepoQueue::new(
        w.store(),
        w.repo.clone(),
        env(held.clone(), w.door.clone(), "a"),
    );
    run(&q2, 2);
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Landed, "{row:?}");
    assert_eq!(held.runs(), 2, "gated again, once");
}

/// **A worker that died mid-gate without winding down** (a panic, an abort) leaves a `gating`
/// row and a `gating` tip under its own name. The next worker adopts neither as running: the row
/// goes back in line and the tip is gated again.
#[test]
fn a_gate_nobody_runs_any_more_is_run_again() {
    let w = World::new("gate-orphan");
    let (wt, base, head) = branch(&w.repo, "one", "one.txt");
    w.enqueue("one", &wt, &base, &head);
    let q0 = w.worker(Arc::new(GreenGate), "a");
    run(&q0, 1);
    let m = w.accept("one", "test/a");
    drop(q0);

    let held = Arc::new(Held::default());
    let q1 = RepoQueue::new(
        w.store(),
        w.repo.clone(),
        env(held.clone(), w.door.clone(), "a"),
    );
    q1.pass(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Gating
    );
    // Gone without a word: its gate thread ends, and nobody applies the verdict.
    std::mem::forget(q1);
    held.open();

    let q2 = w.worker(Arc::new(GreenGate), "a");
    run(&q2, 2);
    let history = w.store().transitions("merge", &m).unwrap();
    assert!(
        history
            .iter()
            .any(|t| t.to_state == "queued" && t.why.contains("restarted mid-gate")),
        "{history:?}"
    );
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Landed, "{row:?}");
    assert_eq!(history.iter().filter(|t| t.to_state == "landed").count(), 1);
}

/// **A landed branch whose child session is still live keeps its tree, and that is said** (R92.24,
/// R92.4) — on the repository's hubs, not only in the daemon's log.
#[test]
fn a_landed_branch_whose_session_is_live_keeps_its_tree_and_says_so() {
    let w = World::new("livechild");
    let (wt, base, head) = branch(&w.repo, "live", "live.txt");
    w.enqueue("live", &wt, &base, &head);
    w.door.live.lock().unwrap().push("s-live".into());
    let q = w.worker(Arc::new(GreenGate), "a");
    run(&q, 1);
    let m = w.accept("live", "test/a");
    run(&q, 2);
    assert_eq!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Landed
    );
    assert!(
        wt.exists(),
        "a live child's tree is not removed from under it"
    );
    let said = w.door.warnings("queue_branch_kept");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("s-live") && said[0].contains("still live"),
        "{}",
        said[0]
    );
}

// ===== The resources a terminal row gives back =====

/// The sweep's own notes on one row's record, in order.
fn release_notes(w: &World, queue: &str, id: &str) -> Vec<String> {
    w.store()
        .transitions(queue, id)
        .unwrap()
        .into_iter()
        .map(|t| t.why)
        .filter(|why| why.starts_with("resources released") || why.starts_with("resources kept"))
        .collect()
}

/// A person's withdrawal of a row, as the verbs write it — the row is terminal before any pass.
fn withdraw(w: &World, id: &str) {
    let r = w.store().review_row(id).unwrap().unwrap();
    assert!(
        w.store()
            .move_review(
                id,
                r.state,
                ReviewState::Withdrawn,
                &Move::new("operator:s-op", "withdrawn: the operator landed it by hand"),
                gate::now_ms(),
            )
            .unwrap()
    );
}

/// **A withdrawn row gives back its tree and its branch, and keeps its record.**
///
/// The operator's own case, and most of the nine directories they cleared by hand: the branch was
/// merged with `git merge --ff-only` (the queue was slow), so `landed`'s retirement never ran —
/// it is only reached by a landing the QUEUE made. MEASURED 2026-10-29: 52 such rows, nine trees.
#[test]
fn a_withdrawn_row_gives_back_its_tree_and_branch_and_keeps_its_record() {
    let w = World::new("release-withdrawn");
    let (wt, base, head) = branch(&w.repo, "gone", "gone.txt");
    // A build cache inside the tree, which is what makes one of these weigh 32 GB on this box —
    // excluded from git, so it is not the tree's work, and it goes with the tree.
    std::fs::write(w.repo.join(".git/info/exclude"), "target/\n").unwrap();
    std::fs::create_dir_all(wt.join("target/debug")).unwrap();
    std::fs::write(wt.join("target/debug/blob"), vec![b'x'; 4096]).unwrap();
    // Landed by hand: everything the branch carries is on `main`, and nothing retired anything.
    sh(&w.repo, &["merge", "--ff-only", "agent/gone"]);
    w.enqueue("gone", &wt, &base, &head);
    withdraw(&w, "gone");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);

    let r = w.store().review_row("gone").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Withdrawn, "the record stays: {r:?}");
    assert_eq!(r.branch, "agent/gone");
    assert_eq!(r.base_sha, base, "and its shas");
    assert_eq!(r.head_sha, head);
    assert!(
        r.worktree.is_some(),
        "the row still says where the tree was"
    );
    assert!(
        !wt.exists(),
        "the tree is gone, and the build cache inside it: {}",
        wt.display()
    );
    assert_eq!(
        git::branch_head(&w.repo, "agent/gone"),
        None,
        "and so is the branch"
    );
    let notes = release_notes(&w, "review", "gone");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("resources released"), "{}", notes[0]);
    assert!(notes[0].contains(&wt.display().to_string()), "{}", notes[0]);
    assert!(notes[0].contains("agent/gone"), "{}", notes[0]);
    assert!(
        notes[0].contains(&head) && notes[0].contains(&base),
        "the note says which shas the record keeps: {}",
        notes[0]
    );
}

/// **A landing done by hand is a landing** — the row the queue itself withdraws as *already on
/// `main`* gives its tree back in the same pass, which is the case `landed` could not reach.
#[test]
fn a_branch_landed_by_hand_is_released_like_a_landing() {
    let w = World::new("release-byhand");
    let (wt, base, head) = branch(&w.repo, "byh", "byh.txt");
    w.enqueue("byh", &wt, &base, &head);
    sh(&w.repo, &["merge", "--ff-only", "agent/byh"]);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);

    let r = w.store().review_row("byh").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Withdrawn, "{r:?}");
    assert!(r.evidence.contains("already on `main`"), "{}", r.evidence);
    assert!(!wt.exists(), "the tree goes with the withdrawal");
    assert_eq!(git::branch_head(&w.repo, "agent/byh"), None);
    assert_eq!(release_notes(&w, "review", "byh").len(), 1);
}

/// **A tree a live session works in is left alone — and released once that session ends**, which
/// is what R92.24 asks for and what a person used to have to do by hand.
#[test]
fn a_terminal_rows_tree_a_live_session_holds_is_left_alone() {
    let w = World::new("release-live");
    let (wt, base, head) = branch(&w.repo, "live2", "l2.txt");
    sh(&w.repo, &["merge", "--ff-only", "agent/live2"]);
    w.enqueue("live2", &wt, &base, &head);
    withdraw(&w, "live2");
    w.door.live.lock().unwrap().push("s-live2".into());
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 2);

    assert!(
        wt.exists(),
        "a live session's tree is not removed from under it"
    );
    assert_eq!(
        git::branch_head(&w.repo, "agent/live2"),
        Some(head),
        "and neither is the branch it has out"
    );
    let said = w.door.warnings("queue_branch_kept");
    assert_eq!(said.len(), 1, "said once, not once per pass: {said:?}");
    assert!(
        said[0].contains("s-live2") && said[0].contains("still live"),
        "{}",
        said[0]
    );
    assert_eq!(
        release_notes(&w, "review", "live2").len(),
        1,
        "the record says it was kept"
    );

    // The session ends: the next pass releases what it held.
    w.door.live.lock().unwrap().clear();
    run(&q, 1);
    assert!(!wt.exists(), "released once nobody is in it");
    assert_eq!(git::branch_head(&w.repo, "agent/live2"), None);
}

/// **A tree with work in it is kept, with the reason on the row and a raised line** — the branch
/// may not carry what was never committed, so both stay.
#[test]
fn a_dirty_tree_is_kept_and_says_why() {
    let w = World::new("release-dirty");
    let (wt, base, head) = branch(&w.repo, "dirty", "d.txt");
    sh(&w.repo, &["merge", "--ff-only", "agent/dirty"]);
    w.enqueue("dirty", &wt, &base, &head);
    withdraw(&w, "dirty");
    std::fs::write(wt.join("draft.md"), "not committed anywhere\n").unwrap();
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 2);

    assert!(wt.exists(), "the tree is kept");
    assert_eq!(
        git::branch_head(&w.repo, "agent/dirty"),
        Some(head),
        "and the branch with it"
    );
    let said = w.door.warnings("queue_branch_kept");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("uncommitted") && said[0].contains("draft.md"),
        "{}",
        said[0]
    );
    let notes = release_notes(&w, "review", "dirty");
    assert_eq!(notes.len(), 1, "and the row says it: {notes:?}");
    assert!(notes[0].starts_with("resources kept"), "{}", notes[0]);
    assert!(notes[0].contains("draft.md"), "{}", notes[0]);
}

/// **A terminal landing whose branch carries work the target does not have keeps both, and says
/// why** — the branch is the only copy of that work, so the tree it was worked in goes with it.
/// This is the half of the sweep a merge row answers to (the review row that opened it names the
/// same tree, and gets the same note).
#[test]
fn a_failed_landings_branch_and_tree_are_kept_and_said() {
    let w = World::new("release-failed");
    // `broken` is the file the fixture's gate refuses, so the landing's own gate fails.
    let (wt, base, head) = branch(&w.repo, "f2", "broken");
    w.enqueue("f2", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let m = w.accept("f2", "test/a");
    run(&q, 3);
    assert_eq!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Failed
    );
    assert!(wt.exists(), "the tree holds the work the branch carries");
    assert_eq!(
        git::branch_head(&w.repo, "agent/f2"),
        Some(head),
        "and the branch is not on `main`"
    );
    let notes = release_notes(&w, "merge", &m);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("resources kept"), "{}", notes[0]);
    assert!(
        notes[0].contains("carries 1 commit") && notes[0].contains("agent/f2"),
        "{}",
        notes[0]
    );
    assert!(
        notes[0].contains(&wt.display().to_string()),
        "and it says where the tree is: {}",
        notes[0]
    );
}

/// **A merge row landed by hand gives its tree back too** — the row settles as `withdrawn` (the
/// queue did not land it, so `landed` would claim a push that never ran), and the resources go
/// with it: the operator's `git merge --ff-only` mornings, from the merge queue's side.
#[test]
fn a_merge_row_landed_by_hand_gives_back_its_tree() {
    let w = World::new("release-byhand-merge");
    let (wt, base, head) = branch(&w.repo, "hm", "hm.txt");
    w.enqueue("hm", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let m = w.accept("hm", "test/a");
    // The queue cannot make its landing tree, so it lands nothing; the operator does it by hand.
    let mut e = env(Arc::new(LocalGate), w.door.clone(), "a");
    Arc::get_mut(&mut e).unwrap().landing_root = Arc::new(|_| Err("no room".into()));
    let q = RepoQueue::new(w.store(), w.repo.clone(), e);
    sh(&w.repo, &["merge", "--ff-only", "agent/hm"]);
    run(&q, 2);
    assert_eq!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Withdrawn
    );
    assert!(!wt.exists(), "the tree of a hand landing goes too");
    assert_eq!(git::branch_head(&w.repo, "agent/hm"), None);
    assert_eq!(release_notes(&w, "merge", &m).len(), 1);
    assert_eq!(head, w.tip(), "the tip is the hand landing's");
}

/// **The sweep twice is the same as once** — the second pass writes nothing, releases nothing and
/// says nothing, and the row's record keeps exactly one note.
#[test]
fn the_sweep_twice_is_the_same_as_once() {
    let w = World::new("release-twice");
    let (wt, base, head) = branch(&w.repo, "twice", "t.txt");
    sh(&w.repo, &["merge", "--ff-only", "agent/twice"]);
    w.enqueue("twice", &wt, &base, &head);
    withdraw(&w, "twice");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let once = w.store().transitions("review", "twice").unwrap().len();
    assert!(!wt.exists());
    run(&q, 3);
    let thrice = w.store().transitions("review", "twice").unwrap().len();
    assert_eq!(once, thrice, "the passes after it write nothing");
    assert_eq!(release_notes(&w, "review", "twice").len(), 1);
    assert!(
        w.door.warnings("queue_branch_kept").is_empty(),
        "and nothing was raised about a tree that was released"
    );
}

/// **A row whose `worktree` names the repository itself is never removed into it** — the column is
/// text somebody wrote, and the rail is what keeps a wrong row from taking a checkout with it. The
/// branch is already on `main` here, so the branch's own test says the tree may go: the only thing
/// that stops the removal is the rail.
#[test]
fn a_row_naming_the_main_checkout_is_not_removed_into_it() {
    let w = World::new("release-rail");
    let (_, base, head) = branch(&w.repo, "rail", "r.txt");
    w.enqueue("rail", &w.repo, &base, &head);
    sh(&w.repo, &["merge", "--ff-only", "agent/rail"]);
    withdraw(&w, "rail");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);

    assert!(w.repo.join("a.txt").exists(), "the checkout is still there");
    assert!(w.repo.join(".git").is_dir());
    assert_eq!(
        git::branch_head(&w.repo, "agent/rail"),
        Some(head),
        "the branch is kept with the tree that would not go"
    );
    let said = w.door.warnings("queue_branch_kept");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("is not a worktree of"), "{}", said[0]);
}

// ===== The idle exit's queue half =====

/// **A daemon serving a queue with a review in line is not idle; one whose only row is blocked
/// is** (`crate::mergequeue::busy_here`, the idle exit's queue test). The blocked row waits on a
/// person, and the lease's next holder re-gates it at start — so holding the daemon up for it
/// would keep every daemon with a parked row up forever, which is the leak the idle exit closes.
/// And a queue another daemon serves is that daemon's business, whatever is in it.
#[test]
fn a_queue_holds_its_daemon_up_only_while_it_has_work_in_line() {
    let dir = TempDir::new("idle-exit-queue");
    let path = dir.0.join("sessions.db");
    let store = Store::open(&path).expect("the store");
    let repo = "/the/repository";
    assert_eq!(crate::mergequeue::busy_here(&path), None, "an empty store");
    store
        .enqueue_review(
            &NewReview {
                id: "r1".into(),
                repository: repo.into(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: "s-child".into(),
                branch: "topic".into(),
                worktree: None,
                base_sha: "base".into(),
                head_sha: "head".into(),
                brief: "do the work".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            1,
        )
        .expect("the review row");
    assert_eq!(
        crate::mergequeue::busy_here(&path),
        None,
        "a queue nobody's lease covers is not this daemon's work"
    );
    let me = crate::mergequeue::lease::this_daemon();
    assert!(matches!(
        store
            .acquire_queue_lease(repo, me, gate::now_ms(), LEASE_TTL_MS)
            .expect("the lease"),
        LeaseOutcome::Held { .. }
    ));
    assert_eq!(
        crate::mergequeue::busy_here(&path).as_deref(),
        Some("the merge queue of /the/repository has work in line"),
        "a waiting review in a queue this daemon serves"
    );
    assert!(
        store
            .move_review(
                "r1",
                ReviewState::Waiting,
                ReviewState::Blocked,
                &Move::new("test", "raised for a person"),
                2,
            )
            .expect("the move")
    );
    assert_eq!(
        crate::mergequeue::busy_here(&path),
        None,
        "a blocked row waits on a person, not on this daemon"
    );
}

// ===== What the daemon's log says, in one line =====
//
// The daemon started in the foreground writes its log to the operator's terminal, so these lines
// are read interleaved with the banner. MEASURED 2026-10-26 in `~/Projects/stroppy-io`: fifteen
// *"… is not a queue target"* lines, twice each, around a banner that also said *"queues: serving
// nothing"* about a daemon that was examining all fifteen. The functions below are the whole of
// what the log says about each of those, so they can be asserted without a terminal.

/// **The not-a-target line is the folder's, not the repository's, and it is said once.**
///
/// `not_targets_to_say` is the state machine (`eprintln!` is the only thing left out of this): the
/// first pass answers a line, the same set answers `None`, and a repository gaining its gate
/// answers a different one.
#[test]
fn the_not_a_target_line_is_one_line_per_pass_and_only_when_the_set_changes() {
    let dir = TempDir::new("not-target-line");
    let one = dir.0.join("one");
    let two = dir.0.join("two");
    let pairs = vec![
        (one.display().to_string(), "main".to_string()),
        (two.display().to_string(), "main".to_string()),
    ];

    let (said, line) =
        not_targets_to_say(&[], &dir.0, &pairs).expect("the first pass has something to say");
    assert_eq!(
        line,
        format!(
            "  queues: 2 repositories under {} declare no `Merge gate` — none of them is queued \
             (one, two). The section in a repository's AGENTS.md is the whole of what opts it in",
            dir.0.display()
        ),
        "one sentence for the folder, naming both — never one line per repository"
    );
    assert_eq!(
        said,
        vec![
            dir.0.join("one").display().to_string(),
            dir.0.join("two").display().to_string()
        ],
        "the set is remembered in a stable order"
    );
    assert!(
        not_targets_to_say(&said, &dir.0, &pairs).is_none(),
        "the same set, said once: the next pass says NOTHING"
    );
    assert!(
        not_targets_to_say(
            &said,
            &dir.0,
            &pairs.clone().into_iter().rev().collect::<Vec<_>>()
        )
        .is_none(),
        "and the order the pass happened to walk them in is not a change"
    );

    // A repository committed its gate: the set changed, so the line changes with it.
    let (said, line) = not_targets_to_say(&said, &dir.0, &pairs[..1])
        .expect("a set that shrank is news — it is how a reader knows to look again");
    assert_eq!(
        line,
        format!(
            "  queues: 1 repository under {} declares no `Merge gate` — one is not queued (the \
             section in its AGENTS.md is the whole of what opts a repository in; its target \
             branch is `main`)",
            dir.0.display()
        )
    );

    // And all of them did: the line says so rather than leaving the last one standing for ever.
    let (_, line) =
        not_targets_to_say(&said, &dir.0, &[]).expect("an empty set after a full one is a change");
    assert!(
        line.contains("every repository under") && line.contains("declares a `Merge gate` now"),
        "{line}"
    );
}

/// **The stale-lease sweep is one line for the set** — the operator's next daemon after a dead
/// one-shot logged six `gave up the lease of …` lines for six repositories of one folder.
#[test]
fn the_leases_a_pass_gave_up_are_said_in_one_line() {
    let holder = "pid 28089 is not running on this box (its heartbeat was 90 s old)".to_string();
    let stale = vec![
        ("/projects/stroppy".to_string(), holder.clone()),
        ("/projects/stroppy-conformity".to_string(), holder.clone()),
        ("/projects/stroppy-rust".to_string(), holder.clone()),
    ];
    assert_eq!(
        stale_leases_line(&stale),
        "  queues: gave up 3 stale leases (stroppy, stroppy-conformity, stroppy-rust): pid 28089 \
         is not running on this box (its heartbeat was 90 s old)"
    );
    assert_eq!(
        stale_leases_line(&stale[..1]),
        "  queues: gave up the lease of stroppy: pid 28089 is not running on this box (its \
         heartbeat was 90 s old)"
    );
    // Two holders is the case where the sentence cannot name one of them.
    let mut mixed = stale.clone();
    mixed[2].1 = "its holder on elsewhere stopped heartbeating 1200 s ago".to_string();
    let line = stale_leases_line(&mixed);
    assert!(
        line.contains("2 holders, all gone — the first was"),
        "{line}"
    );
}

/// **The startup line is the supervisor's own list** — the contradiction the operator read on
/// 2026-10-26: *"queues: serving nothing — … is a session of no repository"*, and then fifteen
/// repositories examined under it, whose leases that same daemon had held that morning.
///
/// The rule kept is the one `serve`'s module doc states and `serves` implements: **a daemon serves
/// a repository when its workspace is a session of that repository** — the checkout, a
/// one-project folder of checkouts (two or more children sharing one `origin` owner), or a
/// worktree of either. A folder that merely CONTAINS unrelated checkouts is still nobody's
/// session, which is the 2026-10-12 guard both forms keep. And the line is
/// `repositories_of_workspace`, which is what the supervisor's pass reads, so the two cannot
/// disagree again.
#[test]
fn the_serving_line_names_what_the_supervisor_will_actually_examine() {
    // **One project of several** — the operator's `~/Projects/stroppy-io`, in miniature.
    let dir = TempDir::new("serving-line");
    let _a = repo_with_agents(&dir.0, "stroppy", Some(GATE_MD));
    let _b = repo_with_agents(&dir.0, "stroppy-contrib", Some(GATE_MD));
    assert_eq!(
        crate::mergequeue::repositories_of_workspace(&dir.0).len(),
        2,
        "the folder IS a session of both of them"
    );
    let line = crate::mergequeue::serving_line(&dir.0);
    assert_eq!(
        line,
        format!(
            "  queues: serving the 2 repositories {} is a session of, when this daemon can take \
             each one's lease: stroppy, stroppy-contrib",
            dir.0.display()
        ),
        "the line counts and names them — 'serving nothing' was a lie about fifteen"
    );
    assert!(
        line.contains("stroppy, stroppy-contrib") && !line.contains("serving nothing"),
        "the line names them, and never says `nothing` about a folder it serves: {line}"
    );

    // **One checkout** — the singular form, unchanged for the case it was written for.
    let single = TempDir::new("serving-line-one");
    let repo = repo(&single.0);
    assert_eq!(
        crate::mergequeue::serving_line(&repo),
        format!(
            "  queues: serving the repository {} when this daemon can take its lease",
            repo.display()
        )
    );

    // **A folder of unrelated checkouts is nobody's session** — two repositories, no `origin`
    // between them, which is the guard of 2026-10-12 and not a queue.
    let loose = TempDir::new("serving-line-loose");
    for name in ["x", "y"] {
        let r = loose.0.join(name);
        std::fs::create_dir_all(&r).unwrap();
        sh(&r, &["init", "-q", "-b", "main"]);
    }
    let line = crate::mergequeue::serving_line(&loose.0);
    assert!(
        line.starts_with(&format!(
            "  queues: serving nothing — {} is a session of no repository",
            loose.0.display()
        )),
        "{line}"
    );
    assert_eq!(
        crate::mergequeue::repositories_of_workspace(&loose.0).len(),
        0,
        "and that is the same answer the supervisor's own list gives"
    );
}

/// **A one-shot serves no queue** — it claimed six leases for one prompt on 2026-10-26 and the
/// operator's next daemon had to give them all up. The flag is `cfg.serve_queues`, and the
/// daemon's log says which of the two causes took it away.
#[test]
fn a_one_shot_takes_no_lease() {
    let dir = TempDir::new("oneshot-no-queue");
    let repo = repo(&dir.0);
    let db = dir.0.join("sessions.db");
    let mut cfg = crate::config::Config::for_this_box(&dir.0);
    cfg.store = Some(db.clone());
    cfg.workspace = repo.clone();
    // The daemon's own rule, from `cli::run`: a run of steps is a one-shot and serves no queue.
    cfg.one_shot = true;
    cfg.serve_queues = false;
    let stop = Arc::new(AtomicBool::new(false));
    let handle = crate::mergequeue::spawn_for(
        &cfg,
        stop.clone(),
        letibot_sessionlog::registry::Registry::new(),
        Arc::new(letibot_tools::builtins::verdict::ReviewDesk::new()),
    );
    assert!(
        handle.is_none(),
        "no supervisor thread at all, so no lease can be claimed"
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let store = Store::open(&db).unwrap();
    assert!(
        store.queue_leases().unwrap().is_empty(),
        "and nothing in the store's lease table: {:?}",
        store.queue_leases().unwrap()
    );
}

// ===== The invariant: the COMMITS decide, never the row's state =====

/// **A `blocked` landing whose branch is not merged keeps everything, and the sweep removes
/// nothing** — the ref, the worktree and the caches inside it all stay.
///
/// A landing is `blocked` exactly when the work has to be landed or fixed by somebody: its gate
/// could not run, or the queue could not make its landing tree. `blocked` is still the merge
/// queue's business (`MergeState::is_open`), so the sweep does not even look at the row — and this
/// test is the guard on that: it is the state a person's `/queue restart` resumes, and a sweep that
/// released it would leave the restart nothing to land.
#[test]
fn a_blocked_landings_branch_and_tree_are_never_released() {
    let w = World::new("invariant-blocked");
    let (wt, base, head) = branch(&w.repo, "blk", "blk.txt");
    // A build cache in the tree, so "nothing was released" covers the caches too.
    std::fs::write(w.repo.join(".git/info/exclude"), "target/\n").unwrap();
    std::fs::create_dir_all(wt.join("target/debug")).unwrap();
    std::fs::write(wt.join("target/debug/blob"), vec![b'x'; 4096]).unwrap();
    w.enqueue("blk", &wt, &base, &head);
    let green = w.worker(Arc::new(GreenGate), "a");
    run(&green, 1);
    let m = w.accept("blk", "test/a");
    // A gate that cannot judge the branch: the landing parks as `blocked`.
    let blocked = {
        let env = env(Arc::new(KilledAtCapGate), w.door.clone(), "a");
        RepoQueue::new(w.store(), w.repo.clone(), env)
    };
    run(&blocked, 1);
    assert_eq!(
        w.store().merge_row(&m).unwrap().unwrap().state,
        MergeState::Blocked,
        "the landing is blocked, which is the case the work has to survive"
    );
    assert_ne!(w.tip(), head, "and its commits are NOT on the target");
    run(&blocked, 3);

    assert!(wt.exists(), "the worktree stays: {}", wt.display());
    assert!(
        wt.join("target/debug/blob").exists(),
        "and the build cache inside it"
    );
    assert_eq!(
        git::branch_head(&w.repo, "agent/blk"),
        Some(head),
        "and the ref, which is the only copy of the work"
    );
    assert!(
        release_notes(&w, "merge", &m).is_empty() && release_notes(&w, "review", "blk").is_empty(),
        "and the sweep said nothing, because it touched nothing"
    );
    assert!(w.door.warnings("queue_branch_kept").is_empty());
}

/// **A landed row gives its resources back** — the one case where the proof says the commits are on
/// the target, so the branch and its tree go and the record keeps `landed_sha`.
#[test]
fn a_landed_row_releases_its_branch_and_its_tree() {
    let w = World::new("invariant-landed");
    let (wt, base, head) = branch(&w.repo, "lan", "lan.txt");
    w.enqueue("lan", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let m = w.accept("lan", "test/a");
    run(&q, 3);

    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Landed, "{row:?}");
    let landed = row
        .landed_sha
        .clone()
        .expect("the row keeps the sha it landed");
    assert!(
        git::is_ancestor(&w.repo, &landed, "main").unwrap(),
        "and that sha is on the target"
    );
    assert!(!wt.exists(), "the tree is released");
    assert_eq!(
        git::branch_head(&w.repo, "agent/lan"),
        None,
        "and so is the ref"
    );
}

/// **An `approved` row whose landing was taken out keeps an unmerged branch** — `approved` is the
/// REVIEW queue's terminal and says nothing about the target, and a person's withdrawal of the
/// landing (`d rm`, *"the branch and its worktree stay"*) is not permission either.
#[test]
fn an_approved_row_whose_landing_was_taken_out_keeps_an_unmerged_branch() {
    let w = World::new("invariant-approved");
    let (wt, base, head) = branch(&w.repo, "app", "app.txt");
    w.enqueue("app", &wt, &base, &head);
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 1);
    let m = w.accept("app", "test/a");
    // The operator takes the landing out, as the `d rm` verb does — the review row stays approved.
    assert!(
        w.store()
            .move_merge(
                &m,
                MergeState::Queued,
                MergeState::Withdrawn,
                &Move::new(
                    "operator:s-op",
                    "withdrawn by the operator; the branch and its worktree stay"
                ),
                gate::now_ms(),
            )
            .unwrap()
    );
    assert_eq!(
        w.store().review_row("app").unwrap().unwrap().state,
        ReviewState::Approved
    );
    run(&q, 3);

    assert!(wt.exists(), "the tree stays");
    assert_eq!(
        git::branch_head(&w.repo, "agent/app"),
        Some(head),
        "and the ref, whose commits are not on the target"
    );
    let notes = release_notes(&w, "review", "app");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("resources kept"), "{}", notes[0]);
    assert!(notes[0].contains("carries 1 commit"), "{}", notes[0]);
}

/// **A withdrawn row keeps the ref of a branch that is not on the target** — the operator's veto is
/// not permission, whatever the `d rm` verb's own words promise about the tree: only *merged* is.
#[test]
fn a_withdrawn_row_keeps_the_ref_of_a_branch_that_is_not_on_the_target() {
    let w = World::new("invariant-withdrawn");
    let (wt, base, head) = branch(&w.repo, "wd", "wd.txt");
    w.enqueue("wd", &wt, &base, &head);
    withdraw(&w, "wd");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 3);

    assert_eq!(
        git::branch_head(&w.repo, "agent/wd"),
        Some(head),
        "the ref survives the withdrawal"
    );
    assert!(wt.exists(), "and so does the tree it is worked in");
    let notes = release_notes(&w, "review", "wd");
    assert_eq!(notes.len(), 1, "said once: {notes:?}");
    assert!(notes[0].starts_with("resources kept"), "{}", notes[0]);
}

/// **The invariant, end to end**: plant a branch with one unmerged commit, put a terminal row of
/// each kind on it, run the whole sweep over and over, and the ref still resolves to that commit.
///
/// *Nothing in the queue may delete a ref whose commits are not on the target.* The rows here are
/// the four a person or the queue can end with — a withdrawn review, an approved review whose
/// landing was taken out, a failed landing, and a review row of a branch no landing names — because
/// the sweep decides per row and the invariant must hold for every one of them.
#[test]
fn the_sweep_never_deletes_a_ref_whose_commits_are_not_on_the_target() {
    let w = World::new("invariant-every-row");
    let q = w.worker(Arc::new(LocalGate), "a");
    // One branch, one commit that the target does not have, named by four terminal rows.
    let (wt, base, head) = branch(&w.repo, "inv", "inv.txt");
    assert_ne!(w.tip(), head);
    w.enqueue("inv", &wt, &base, &head);
    run(&q, 1);
    let m = w.accept("inv", "test/a");
    assert!(
        w.store()
            .move_merge(
                &m,
                MergeState::Queued,
                MergeState::Failed,
                &Move::new("test/a", "the gate was red"),
                gate::now_ms(),
            )
            .unwrap()
    );
    withdraw(&w, "inv");
    for _ in 0..4 {
        run(&q, 1);
    }

    let sha = sh(&w.repo, &["rev-parse", "--verify", "refs/heads/agent/inv"]);
    assert_eq!(
        sha, head,
        "the ref still resolves, and to the unmerged commit"
    );
    assert!(
        !git::is_ancestor(&w.repo, &head, "main").unwrap(),
        "and it is still not on the target"
    );
    assert!(wt.exists(), "the tree it is worked in is still there");
    assert_eq!(
        git::rev_parse(&w.repo, &format!("{head}^{{commit}}")),
        Some(head.clone()),
        "the commit is still reachable by its ref"
    );
}

/// **A tree whose own `HEAD` is not on the target is kept even when the row's branch is** — the
/// hole this test was written for.
///
/// `settle_tree` used to prove the row's BRANCH and then remove the worktree whole. A tree can hold
/// a commit no ref of the row names — here a detached `HEAD` carrying one commit made after the
/// branch landed by hand — and `git worktree remove --force` is unrecoverable for it. MEASURED
/// 2026-10-29: before the second proof, this tree was removed and the commit survived only in
/// `.git`'s reflog.
#[test]
fn a_tree_whose_own_head_is_not_on_the_target_is_kept_and_says_so() {
    let w = World::new("invariant-tree-head");
    let (wt, base, head) = branch(&w.repo, "det", "det.txt");
    // The branch lands by hand, so the sweep's proof about the REF says *may go* …
    sh(&w.repo, &["merge", "--ff-only", "agent/det"]);
    assert_eq!(w.tip(), head);
    // … and the tree then gets a commit that no ref holds, on a detached HEAD.
    sh(&wt, &["checkout", "--detach", "HEAD"]);
    let extra = commit(
        &wt,
        "later.txt",
        "not on any branch\n",
        "work after the landing",
    );
    assert_eq!(git::branch_head(&w.repo, "agent/extra"), None);
    w.enqueue("det", &wt, &base, &head);
    withdraw(&w, "det");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 3);

    assert!(wt.exists(), "the tree holds a commit no ref does");
    assert_eq!(
        git::head_of(&wt).unwrap(),
        extra,
        "and it is still the commit that was made in it"
    );
    assert_eq!(
        git::branch_head(&w.repo, "agent/det"),
        Some(head),
        "and the branch stays WITH the tree: one tree is one decision, and the branch is where \
         that tree's work would go"
    );
    let notes = release_notes(&w, "review", "det");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("resources kept"), "{}", notes[0]);
    assert!(
        notes[0].contains("HEAD") && notes[0].contains("carries 1 commit"),
        "the note says which proof refused it: {}",
        notes[0]
    );
    assert!(
        notes[0].contains(&wt.display().to_string()),
        "and where the tree is: {}",
        notes[0]
    );
}

/// **A landing the target was rewritten past keeps the branch** — the second ref-deleting path,
/// `landed`, proved too.
///
/// The queue's landing is a compare-and-swap and the row says `landed_sha`, and that IS the proof —
/// while the sha is still on the target. The window is not zero: the install gate is a SECOND gate
/// on its own thread, so a target reset (a force-push, a person's `git reset --hard`) can happen
/// between the swap and the retirement. This used to delete the branch on the strength of the row's
/// state; the branch is the only copy of that work once the target no longer has it.
#[test]
fn a_landing_the_target_was_rewritten_past_keeps_the_branch() {
    /// Green, and it puts the target back while the landing's INSTALL step runs.
    struct RewritesTheTargetAtInstall {
        repo: PathBuf,
        to: String,
    }
    impl GateRunner for RewritesTheTargetAtInstall {
        fn run(&self, _wt: &Path, steps: &[String], _ctl: &GateControl) -> GateRun {
            if steps.iter().any(|s| s.contains(gate::INSTALL_MARK)) {
                sh(&self.repo, &["reset", "--hard", &self.to]);
            }
            GateRun {
                steps: steps
                    .iter()
                    .map(|s| letibot_tokencore::store::MergeGateStep {
                        command: s.clone(),
                        outcome: letibot_tokencore::store::MergeGateOutcome::Passed,
                        output: String::new(),
                        started_ms: gate::now_ms(),
                        elapsed_ms: 0,
                    })
                    .collect(),
                verdict: GateVerdict::Passed,
            }
        }
    }

    let w = World::new("landed-rewritten");
    // An install step, so the landing reaches `landed` through a second gate: that is the window.
    std::fs::write(
        w.repo.join("AGENTS.md"),
        format!(
            "## Merge gate\n\n```sh\ntest ! -f broken\ntrue # {}\n```\n",
            gate::INSTALL_MARK
        ),
    )
    .unwrap();
    sh(&w.repo, &["commit", "-q", "-am", "an install step"]);
    let before = w.tip();
    let (wt, base, head) = branch(&w.repo, "rw", "rw.txt");
    w.enqueue("rw", &wt, &base, &head);
    let q = w.worker(
        Arc::new(RewritesTheTargetAtInstall {
            repo: w.repo.clone(),
            to: before.clone(),
        }),
        "a",
    );
    run(&q, 1);
    let m = w.accept("rw", "test/a");
    run(&q, 3);

    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Landed, "{row:?}");
    assert_eq!(w.tip(), before, "the target was put back under the landing");
    let landed = row.landed_sha.clone().expect("the row keeps its sha");
    assert!(
        !git::is_ancestor(&w.repo, &landed, "main").unwrap(),
        "and that sha is not on it any more"
    );
    assert_eq!(
        git::branch_head(&w.repo, "agent/rw"),
        Some(head),
        "so the branch is the only copy of the work, and it is kept"
    );
    let said = w.door.warnings("queue_branch_kept");
    assert_eq!(said.len(), 1, "and a person is told: {said:?}");
    assert!(said[0].contains("agent/rw"), "{}", said[0]);
}

/// **A proof that cannot be taken keeps the branch and says why** — a target git cannot name is not
/// a target the branch is on.
#[test]
fn a_target_that_cannot_be_named_keeps_the_branch_and_says_why() {
    let w = World::new("invariant-no-target");
    let (wt, base, head) = branch(&w.repo, "nt", "nt.txt");
    w.enqueue("nt", &wt, &base, &head);
    withdraw(&w, "nt");
    // The row names a target this repository does not have.
    w.store()
        .enqueue_review(
            &NewReview {
                id: "nt2".into(),
                repository: w.key(),
                target: "trunk".into(),
                kind: ReviewKind::Branch,
                session_id: "s-nt2".into(),
                branch: "agent/nt".into(),
                worktree: Some(wt.display().to_string()),
                base_sha: base.clone(),
                head_sha: head.clone(),
                brief: "do nt".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            gate::now_ms(),
        )
        .unwrap();
    withdraw(&w, "nt2");
    let q = w.worker(Arc::new(LocalGate), "a");
    run(&q, 2);

    assert_eq!(git::branch_head(&w.repo, "agent/nt"), Some(head.clone()));
    assert!(wt.exists());
    let notes = release_notes(&w, "review", "nt2");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("resources kept"), "{}", notes[0]);
    assert!(
        notes[0].contains("trunk") && notes[0].contains("cannot be named"),
        "the note names the target it could not read: {}",
        notes[0]
    );
}

// ===== The train (`[queue] gate_train`) =====

/// The settings a project that batches its gate writes.
fn train_of(n: usize) -> QueueSettings {
    QueueSettings {
        gate_train: n,
        ..Default::default()
    }
}

/// **Claim a review by hand** — the move `review_upkeep` makes, so a test can approve every branch
/// BEFORE a pass runs. A pass in between would land a train of whatever had been approved so far.
fn claim(w: &World, id: &str) {
    let s = w.store();
    let r = s.review_row(id).unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Waiting, "{r:?}");
    let head = git::branch_head(&w.repo, &r.branch).expect("the branch is there");
    assert!(
        s.move_review(
            &r.id,
            ReviewState::Waiting,
            ReviewState::Reviewing,
            &Move::new("test/a", "claimed for review")
                .set("owner", "test/a")
                .set("head_sha", head),
            gate::now_ms(),
        )
        .unwrap()
    );
}

/// Three branches, each one file, each cut from `main`, all approved — and no pass has run.
fn three_approved(w: &World) -> Vec<String> {
    let mut ids = Vec::new();
    for name in ["one", "two", "three"] {
        let (wt, base, head) = branch(&w.repo, name, &format!("{name}.txt"));
        w.enqueue(name, &wt, &base, &head);
        claim(w, name);
        ids.push(w.accept(name, "test/a"));
    }
    ids
}

/// **A gate that remembers which tree each run judged** — the count is the point: a train is ONE
/// run for several branches.
struct Counting {
    judged: Arc<Mutex<Vec<String>>>,
    inner: Arc<dyn GateRunner>,
}

impl Counting {
    fn new() -> Self {
        Counting {
            judged: Arc::new(Mutex::new(Vec::new())),
            inner: Arc::new(LocalGate),
        }
    }
    fn runs(&self) -> Vec<String> {
        self.judged.lock().unwrap().clone()
    }
}

impl GateRunner for Counting {
    fn run(&self, wt: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
        let head = git::head_of(wt).unwrap_or_default();
        self.judged.lock().unwrap().push(head);
        self.inner.run(wt, steps, ctl)
    }
    fn name(&self) -> String {
        "counting (test)".into()
    }
}

/// **One gate for three branches, and the target moves once per landing.**
///
/// The operator's decision of 2026-10-30 — *"we can batch merges at this point"* — on the numbers
/// that made it: the gate is 683 s of an 11.8-minute landing and it is paid per branch, while the
/// REVIEW (which is what says a branch is sound) stays per branch and does not move.
#[test]
fn a_train_of_three_green_branches_lands_three_commits_with_one_gate() {
    let w = World::new("train-green");
    let before = w.tip();
    let gate = Arc::new(Counting::new());
    let q = w.worker_with(gate.clone(), "a", train_of(3));
    let ms = three_approved(&w);
    run(&q, 2);

    // Every row landed, and each says which train it rode in and where.
    let mut shas = Vec::new();
    for (i, m) in ms.iter().enumerate() {
        let row = w.store().merge_row(m).unwrap().unwrap();
        assert_eq!(row.state, MergeState::Landed, "car {i}: {row:?}");
        assert!(row.evidence.contains("in a train of 3"), "{}", row.evidence);
        assert!(
            row.evidence.contains(&format!("(car {})", i + 1)),
            "{}",
            row.evidence
        );
        assert!(
            !row.gate_steps.is_empty(),
            "the one gate's steps are on every row"
        );
        shas.push(row.landed_sha.clone().expect("a landed row keeps its sha"));
    }
    // **The target moved once per car** — each row's `landed_sha` is the tip at its own landing,
    // so a bisect over the train walks three commits and each names the branch that made it.
    let tip = w.tip();
    assert_eq!(shas[2], tip, "the last car is the tip");
    assert_eq!(sh(&w.repo, &["rev-parse", "main~1"]), shas[1]);
    assert_eq!(sh(&w.repo, &["rev-parse", "main~2"]), shas[0]);
    assert_eq!(
        sh(&w.repo, &["rev-parse", "main~3"]),
        before,
        "three commits, one per branch"
    );
    // **One gate ran on the train's tree** — and one more for the target's own health, which is
    // what any landing waits for.
    let runs = gate.runs();
    assert_eq!(
        runs.len(),
        2,
        "one gate of the tip and one of the train: {runs:?}"
    );
    assert_eq!(runs[1], tip, "the train's gate judged the final tree");
    // The branches are retired, the tree is green at the last car, and the operator's checkout
    // followed every landing (not only the last).
    for name in ["one", "two", "three"] {
        assert!(
            git::branch_head(&w.repo, &format!("agent/{name}")).is_none(),
            "`agent/{name}` is retired"
        );
    }
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Green);
    assert_eq!(h.sha, tip);
    assert_eq!(
        sh(&w.repo, &["status", "--porcelain"]),
        "",
        "the operator's checkout followed every landing"
    );
    assert!(w.repo.join("one.txt").exists() && w.repo.join("three.txt").exists());
}

/// **A red train dissolves** — nothing lands, the target never moves, one review row names every
/// branch in its order, and every row goes back in line to be gated ON ITS OWN, which is what
/// finds the branch that is bad.
#[test]
fn a_red_train_lands_nothing_names_all_its_branches_and_leaves_the_target_where_it_was() {
    let w = World::new("train-red");
    let before = w.tip();
    let gate = Arc::new(Counting::new());
    let q = w.worker_with(gate.clone(), "a", train_of(3));
    let mut ms = Vec::new();
    for (name, file) in [("one", "one.txt"), ("two", "two.txt"), ("bad", "broken")] {
        let (wt, base, head) = branch(&w.repo, name, file);
        w.enqueue(name, &wt, &base, &head);
        claim(&w, name);
        ms.push(w.accept(name, "test/a"));
    }
    // **One pass at a time, and stop at the pass that judged the train.** That pass goes on to
    // take the first branch on its own (a row out of a red train is gated alone), so what each row
    // was put in is read off its history rather than off the moment after the pass.
    let stop = AtomicBool::new(false);
    let mut judged = false;
    for _ in 0..200 {
        q.pass(&stop).unwrap();
        if w.store()
            .review_rows(Some(&w.key()))
            .unwrap()
            .iter()
            .any(|r| r.id.starts_with("train-"))
        {
            judged = true;
            break;
        }
        // A pass does not wait for its gate; the next one is a no-op until it has finished.
        if q.gate_in_flight() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(judged, "the train's gate was judged and its row filed");
    assert_eq!(
        w.tip(),
        before,
        "a red train is caught before the target moves"
    );
    assert!(
        !w.repo.join("one.txt").exists(),
        "and no car's work is in the target's tree"
    );
    // **Each merge row is back in line**, with the sentence that keeps it out of the next train
    // and the gate's own steps as its evidence. Nothing of the train is terminal: no car landed,
    // none failed — the train judged a tree, not a branch.
    for m in &ms {
        let history = w.store().transitions("merge", m).unwrap();
        assert!(
            history
                .iter()
                .any(|t| t.to_state == "queued" && t.why.starts_with(TRAIN_SPLIT)),
            "{m} went back in line with the train's sentence: {history:?}"
        );
        let row = w.store().merge_row(m).unwrap().unwrap();
        assert!(
            row.state.is_open(),
            "nothing of the train is terminal: {row:?}"
        );
        if row.state == MergeState::Queued {
            assert_eq!(row.owner, "", "a row back in line is nobody's");
            assert!(row.evidence.starts_with(TRAIN_SPLIT), "{}", row.evidence);
        } else {
            // The pass that judged the train went on to take the first branch on its own: the row
            // is `gating` already, and its history above is what says it came back in line first.
            assert_eq!(row.state, MergeState::Gating, "{row:?}");
        }
        assert!(
            !row.gate_steps.is_empty(),
            "the failing gate's steps ride with the row"
        );
    }
    // **The review rows are untouched** — still `approved`, bound to the head each judged.
    for id in ["one", "two", "bad"] {
        let r = w.store().review_row(id).unwrap().unwrap();
        assert_eq!(r.state, ReviewState::Approved, "{r:?}");
    }
    // **One review row for the train**, naming every branch in the order it was rebased.
    let trains: Vec<ReviewRow> = w
        .store()
        .review_rows(Some(&w.key()))
        .unwrap()
        .into_iter()
        .filter(|r| r.id.starts_with("train-"))
        .collect();
    assert_eq!(trains.len(), 1, "{trains:?}");
    let t = &trains[0];
    let order: Vec<usize> = ["agent/one", "agent/two", "agent/bad"]
        .iter()
        .map(|b| {
            t.brief
                .find(b)
                .unwrap_or_else(|| panic!("`{b}` is named in the brief: {}", t.brief))
        })
        .collect();
    assert!(
        order[0] < order[1] && order[1] < order[2],
        "the order is named, in order: {}",
        t.brief
    );
    assert!(
        t.brief.contains("test ! -f broken"),
        "the failing step is named: {}",
        t.brief
    );
    // The row's own birth is where its naming lives — a claim overwrites `evidence`, as it does
    // for every row the review queue picks up.
    let born = w.store().transitions("review", &t.id).unwrap();
    let first = born
        .first()
        .unwrap_or_else(|| panic!("the row was born: {born:?}"));
    assert!(
        first.why.contains("train of 3") && first.why.contains("agent/bad"),
        "{}",
        first.why
    );
    assert!(t.state.is_open(), "the gatekeeper has it to decide: {t:?}");
    assert!(
        t.branch.starts_with("agent/train-"),
        "the row is about the train's own tree: {}",
        t.branch
    );
    assert!(
        git::branch_head(&w.repo, &t.branch).is_some(),
        "the tree the gate judged is a branch a reviewer can be given: {}",
        t.branch
    );
    let said = w.door.warnings("merge_failed");
    assert_eq!(said.len(), 1, "{said:?}");
    for b in ["agent/one", "agent/two", "agent/bad"] {
        assert!(said[0].contains(b), "`{b}` in {}", said[0]);
    }

    // **The next passes gate each branch ALONE** — the train cannot re-form, and the branch that is
    // bad is the one that ends `failed`, while the other two land.
    run(&q, 4);
    assert_eq!(
        w.store().merge_row(&ms[2]).unwrap().unwrap().state,
        MergeState::Failed,
        "the branch the gate refused, on its own"
    );
    for m in [&ms[0], &ms[1]] {
        assert_eq!(
            w.store().merge_row(m).unwrap().unwrap().state,
            MergeState::Landed,
            "a branch that is fine on its own lands"
        );
    }
    let runs = gate.runs();
    assert_eq!(
        runs.len(),
        5,
        "one gate of the tip, one of the red train, then one per branch: {runs:?}"
    );
}

/// **A stacked pair is ordered parent-first**, whatever the clock says: the child's recorded base
/// is the parent's head, so the child is rebased after it — rebasing the child first would replay
/// it without the commit it was cut from.
#[test]
fn a_stacked_pair_is_ordered_parent_first() {
    let w = World::new("train-stacked");
    let before = w.tip();
    let (wt_p, base_p, head_p) = branch(&w.repo, "parent", "parent.txt");
    // The child is cut from the PARENT's branch, not from main.
    let wt_c = w.dir.0.join("wt-child");
    sh(
        &w.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/child",
            &wt_c.to_string_lossy(),
            "agent/parent",
        ],
    );
    let head_c = commit(&wt_c, "child.txt", "child\n", "child work");
    // **The child is approved FIRST**, so the clock alone would put it first.
    w.enqueue("child", &wt_c, &head_p, &head_c);
    claim(&w, "child");
    let m_c = w.accept("child", "test/a");
    w.enqueue("parent", &wt_p, &base_p, &head_p);
    claim(&w, "parent");
    let m_p = w.accept("parent", "test/a");
    assert!(
        w.store().merge_row(&m_c).unwrap().unwrap().created_ms
            <= w.store().merge_row(&m_p).unwrap().unwrap().created_ms,
        "the premise: the child is not later in the queue's clock"
    );

    let gate = Arc::new(Counting::new());
    let q = w.worker_with(gate.clone(), "a", train_of(2));
    run(&q, 2);

    let p = w.store().merge_row(&m_p).unwrap().unwrap();
    let c = w.store().merge_row(&m_c).unwrap().unwrap();
    assert_eq!(p.state, MergeState::Landed, "{p:?}");
    assert_eq!(c.state, MergeState::Landed, "{c:?}");
    // **The parent is the first commit and the child the second.**
    assert_eq!(
        sh(&w.repo, &["rev-parse", "main~1"]),
        p.landed_sha.clone().unwrap(),
        "the parent landed first"
    );
    assert_eq!(w.tip(), c.landed_sha.clone().unwrap());
    assert_eq!(sh(&w.repo, &["rev-parse", "main~2"]), before);
    assert!(
        w.repo.join("parent.txt").exists() && w.repo.join("child.txt").exists(),
        "both changes are on the target"
    );
    assert_eq!(
        gate.runs().len(),
        2,
        "one gate of the tip and one of the pair: {:?}",
        gate.runs()
    );
}

/// **`gate_train = 1` is the queue that never had a train** — one gate per branch, one landing
/// each, and no sentence about a train. The default path, bit for bit.
#[test]
fn a_gate_train_of_one_gates_every_branch_on_its_own() {
    let w = World::new("train-one");
    let before = w.tip();
    assert_eq!(
        QueueSettings::default().gate_train,
        1,
        "the default is today's behaviour"
    );
    let gate = Arc::new(Counting::new());
    let q = w.worker_with(gate.clone(), "a", QueueSettings::default());
    let ms = three_approved(&w);
    run(&q, 4);

    for (i, m) in ms.iter().enumerate() {
        let row = w.store().merge_row(m).unwrap().unwrap();
        assert_eq!(row.state, MergeState::Landed, "row {i}: {row:?}");
        assert!(
            !row.evidence.contains("in a train of"),
            "a lone landing's sentence, as it always was: {}",
            row.evidence
        );
    }
    // One gate of the tip, then one per branch: three landings, three gates.
    assert_eq!(gate.runs().len(), 4, "{:?}", gate.runs());
    assert_eq!(
        sh(&w.repo, &["rev-parse", "main~2"]),
        w.store()
            .merge_row(&ms[0])
            .unwrap()
            .unwrap()
            .landed_sha
            .unwrap_or_default(),
        "and each landing is its own commit"
    );
    assert_eq!(sh(&w.repo, &["rev-parse", "main~3"]), before);
}

/// **A train of one is not a train** — one approved branch takes the single path, bit for bit,
/// even when the project asked for a train of three.
#[test]
fn one_approved_branch_takes_the_single_path_even_with_a_train_of_three() {
    let w = World::new("train-lone");
    let gate = Arc::new(Counting::new());
    let q = w.worker_with(gate.clone(), "a", train_of(3));
    let (wt, base, head) = branch(&w.repo, "lone", "lone.txt");
    w.enqueue("lone", &wt, &base, &head);
    claim(&w, "lone");
    let m = w.accept("lone", "test/a");
    run(&q, 2);

    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(row.state, MergeState::Landed, "{row:?}");
    assert!(!row.evidence.contains("in a train of"), "{}", row.evidence);
    assert_eq!(gate.runs().len(), 2, "{:?}", gate.runs());
}
