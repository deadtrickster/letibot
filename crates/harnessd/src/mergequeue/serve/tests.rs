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

use super::*;
use crate::mergequeue::gate::{GateControl, GateRun, GateRunner, GreenGate, LocalGate};
use crate::mergequeue::settings::QueueSettings;

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
}

impl QueueDoor for Door {
    fn announce(&self, event: SessionEvent) {
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
    Arc::new(QueueEnv {
        me: me(who),
        gate,
        door,
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
        let env = env_with(gate, self.door.clone(), who, settings);
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
        let merge_id = format!("m-{id}");
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
        assert_ne!(q.pass(&stop).unwrap(), Pass::LostLease);
    }
}

// ===== The landing =====

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
    let row = w.store().merge_row(&m).unwrap().unwrap();
    assert_eq!(
        row.state,
        MergeState::Queued,
        "back in line, not failed: {}",
        row.evidence
    );
    assert!(
        row.evidence.contains("moved during the gate"),
        "{}",
        row.evidence
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
    Arc::get_mut(&mut e).unwrap().gate_deadline = Duration::from_secs(1);
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
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only asks whether the pid exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "the step's process was killed with its group");
    // And nothing lands on a target nobody could judge.
    run(&q, 1);
    assert_eq!(w.tip(), sh(&w.repo, &["rev-parse", "HEAD"]));
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
    q.pass(&stop).unwrap();
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
        !w.door.warnings("queue_blocked").is_empty(),
        "and it was raised"
    );
}

// ===== Red target =====

/// **R61's done-when, in one repository: a branch reviewed, merged, and then main turns red —
/// three rows in two queues — and a red main stops landing without stopping enqueue or review;
/// the red tip goes first in review; only the fix lands; green again, the queue lands again.**
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

    // A hand merge breaks main (decision 10: gated on every change, hand merges included).
    commit(
        &w.repo,
        "broken",
        "x\n",
        "a hand commit that breaks the gate",
    );
    let (wt2, base2, head2) = branch(&w.repo, "two", "two.txt");
    w.enqueue("two", &wt2, &base2, &head2);
    run(&q, 1);
    let h = w.store().target_health(&w.key(), "main").unwrap().unwrap();
    assert_eq!(h.state, HealthState::Red, "{h:?}");
    assert!(
        !w.door.warnings("target_red").is_empty(),
        "an emergency, said on every hub"
    );
    assert!(
        !crate::mergequeue::notices::health(&h).contains(crate::mergequeue::carry::LANDING_LACKS),
        "no ignored directories in this checkout: the notice says nothing about them"
    );
    let rows = w.store().review_rows(Some(&w.key())).unwrap();
    let red = rows
        .iter()
        .find(|r| r.kind == ReviewKind::Target)
        .expect("the red tip is a review row");
    assert_eq!(red.state, ReviewState::Reviewing, "taken FIRST: {red:?}");
    let two = rows.iter().find(|r| r.id == "two").unwrap();
    assert_eq!(
        two.state,
        ReviewState::Reviewing,
        "and other reviews go on (the relaxed rule)"
    );
    // Three rows in two queues: the landed branch's review and merge rows, and the red tip.
    assert_eq!(
        w.store()
            .review_rows(Some(&w.key()))
            .unwrap()
            .iter()
            .filter(|r| r.state == ReviewState::Approved || r.kind == ReviewKind::Target)
            .count(),
        2
    );

    // `two` is approved, and nothing lands while main is red.
    let m2 = w.accept("two", "test/a");
    let red_tip = w.tip();
    run(&q, 2);
    assert_eq!(
        w.store().merge_row(&m2).unwrap().unwrap().state,
        MergeState::Queued,
        "red main lands nothing"
    );
    assert_eq!(w.tip(), red_tip);
    // Enqueue is never refused.
    let (wt3, base3, head3) = branch(&w.repo, "three", "three.txt");
    w.enqueue("three", &wt3, &base3, &head3);
    assert!(w.store().review_row("three").unwrap().is_some());

    // The fix: a branch that removes `broken`, marked as fixing the red tip.
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
    run(&q, 3);
    assert_eq!(
        w.store().merge_row(&m2).unwrap().unwrap().state,
        MergeState::Landed,
        "green again: the queue lands again"
    );
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
/// `superseded: the target was rewritten from X to Y`, and the new tip is gated.
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
        why.starts_with(&format!(
            "superseded: the target was rewritten from {red} to {amended}"
        )),
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

/// **The service, as a daemon runs it**: it finds the repository under its workspace, claims the
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
    let service = QueueService {
        store_path: w.db.clone(),
        workspace: w.dir.0.clone(),
        env: e,
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
