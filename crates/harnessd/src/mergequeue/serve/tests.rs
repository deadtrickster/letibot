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
    let spawned = crate::mergequeue::spawn_for(&cfg, stop.clone(), Registry::new());
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
    let spawned = crate::mergequeue::spawn_for(&cfg, stop.clone(), Registry::new())
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
    let board = crate::sessions::gatekeeper::SESSION;
    let hubs: Vec<_> = ["s-home", "s-repo", "s-wt", board]
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
