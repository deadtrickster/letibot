//! **The review queue's reviewers, served on the daemon's own clock** — `Sessions::serve_reviews`
//! through the review queue's session (`gatekeeper::SESSION`), against a temp store.
//!
//! The queue's thread CLAIMS a review (`waiting → reviewing`, owner = this daemon's lease
//! identity) and rings the session; this pass starts a reviewer for every claimed row nobody is on,
//! writes the verdict the reviewer HANDS IN (`review_verdict`, the daemon's desk) bound to the head
//! the claim read, and counts an attempt that reached none. Asserted here, at the harness:
//!
//! * **A claimed review is the review queue session's own `task` child**, and the daemon's own
//!   session is asked for nothing.
//! * **A row another daemon claimed is left alone** — R61 defect 9: every daemon served the one
//!   global session, and two would each spawn a reviewer for one entry.
//! * **A verdict handed in approves the head the claim read** (decision 6) and hands the branch to
//!   the merge queue in the same transaction; a second verdict is refused.
//! * **A child of another session cannot submit.**
//! * **An answer in prose is a counted attempt, and so is a child that vanished** — R61 defect 10:
//!   a vanished child wrote nothing and was not counted, so the next pass spawned another (148
//!   reviewer sessions for 21 rows).
//!
//! It needs the vocabulary GGUF (`Harness::open` renders a stable prefix) and skips without it;
//! `LETIBOT_REQUIRE_APPARATUS=1` refuses the skip. No model, and never the live store.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{
    LeaseClaim, MergePriority, MergeState, Move, NewReview, ReviewKind, ReviewState, Store,
};
use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus};

use super::*;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "letibot-review-pass-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("the temp dir");
        Self { path }
    }
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// What a runner was asked to start.
#[derive(Debug, Clone)]
struct Asked {
    prompt: String,
    role: String,
}

/// How a recording runner's children end.
#[derive(Clone)]
enum Ending {
    /// Running for ever.
    Running,
    /// Answered in prose, never handing a verdict in.
    Prose(String),
    /// Gone — a handle the runner no longer knows.
    Gone,
}

struct Recording {
    started: Arc<Mutex<Vec<Asked>>>,
    ending: Ending,
}

impl TaskRunner for Recording {
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
        let mut g = self.started.lock().unwrap_or_else(|e| e.into_inner());
        g.push(Asked {
            prompt: prompt.to_string(),
            role: spec.role.clone(),
        });
        Ok(format!("sub-{}", g.len()))
    }

    fn collect(&self, _handle: &str, _timeout: Duration) -> TaskStatus {
        match &self.ending {
            Ending::Running => TaskStatus::Running {
                note: None,
                child: None,
            },
            Ending::Prose(answer) => TaskStatus::Done {
                answer: answer.clone(),
                child: None,
            },
            Ending::Gone => TaskStatus::Unknown,
        }
    }

    fn kill(&self, handle: &str) -> Result<String, String> {
        Ok(format!("{handle} stopped"))
    }
}

const REPO: &str = "/the/repository";

struct Fixture {
    _dir: TempDir,
    db: PathBuf,
    cfg: Config,
    registry: Arc<Registry>,
    started: Arc<Mutex<Vec<Asked>>>,
    started_by_the_daemon: Arc<Mutex<Vec<Asked>>>,
    host: String,
}

impl Fixture {
    fn new(tag: &str) -> Option<Self> {
        let gguf = crate::vocab_gguf()?;
        let dir = TempDir::new(tag);
        let db = dir.path().join("sessions.db");
        let mut cfg = Config::for_this_box(dir.path());
        cfg.vocab_gguf = Some(gguf);
        // A temp store, never the daemon's own.
        cfg.store = Some(db.clone());
        cfg.session_id = "s-host".into();
        cfg.http_retries = 0;
        let registry = Registry::new();
        registry
            .create(&cfg.session_id, "", Sessions::wiring(&cfg))
            .expect("the held session registers");
        registry
            .create("s-elsewhere", "", Sessions::wiring(&cfg))
            .expect("another session registers");
        Some(Self {
            _dir: dir,
            db,
            cfg,
            registry,
            started: Arc::new(Mutex::new(Vec::new())),
            started_by_the_daemon: Arc::new(Mutex::new(Vec::new())),
            host: crate::sessions::gatekeeper::SESSION.to_string(),
        })
    }

    fn store(&self) -> Store {
        Store::open(&self.db).expect("the store reopens")
    }

    fn me(&self) -> String {
        crate::mergequeue::lease::this_daemon().owner.clone()
    }

    /// A review row, put up for review and CLAIMED by `owner`, the way the queue's thread does.
    fn claimed(&self, id: &str, brief: &str, owner: &str) {
        let s = self.store();
        s.acquire_queue_lease(
            REPO,
            &LeaseClaim {
                owner: owner.into(),
                pid: 1,
                host: "t".into(),
                build: "t".into(),
            },
            1,
            u64::MAX / 2,
        )
        .expect("the lease");
        s.enqueue_review(
            &NewReview {
                id: id.into(),
                repository: REPO.into(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: format!("s-{id}"),
                branch: format!("agent/{id}"),
                worktree: None,
                base_sha: "base".into(),
                head_sha: format!("head-{id}"),
                brief: brief.into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: String::new(),
            },
            "test",
            1,
        )
        .expect("the row");
        assert!(
            s.move_review(
                id,
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new(owner, "claimed").set("owner", owner),
                2,
            )
            .expect("the claim")
        );
    }

    fn sessions<'p>(&self, parts: &'p Parts, ending: Ending) -> Sessions<'p> {
        let mut sessions = Sessions::open_first(parts, self.cfg.clone(), self.registry.clone())
            .expect("the daemon's own session opens");
        sessions
            .ensure_gatekeeper()
            .expect("the review queue's session opens");
        sessions
            .open
            .get_mut(&self.host)
            .expect("the review queue's session is open")
            .set_subagents_for_test(Arc::new(Recording {
                started: self.started.clone(),
                ending,
            }));
        sessions
            .open
            .get_mut(&self.cfg.session_id)
            .expect("the daemon's own session is open")
            .set_subagents_for_test(Arc::new(Recording {
                started: self.started_by_the_daemon.clone(),
                ending: Ending::Running,
            }));
        sessions
    }

    fn asked(&self) -> Vec<Asked> {
        self.started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// **A claimed review is the review queue session's own `task` child, bound to the claimed
/// head, and started by the pass with no wake; a second pass starts no second child.**
#[test]
fn a_claimed_review_is_spawned_as_the_queue_sessions_own_child() {
    let Some(f) = Fixture::new("due") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.claimed("m-mine", "make the reader keep the last record", &f.me());
    let mut sessions = f.sessions(&parts, Ending::Running);
    assert_eq!(sessions.serve_reviews(), 1, "{:?}", f.asked());
    let asked = f.asked();
    assert_eq!(asked[0].role, "gatekeeper");
    assert!(
        asked[0]
            .prompt
            .starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX)
    );
    assert!(
        asked[0]
            .prompt
            .contains("make the reader keep the last record")
    );
    assert!(
        asked[0].prompt.contains("base...head-m-mine"),
        "the head the claim bound: {}",
        asked[0].prompt
    );
    assert!(
        f.started_by_the_daemon.lock().unwrap().is_empty(),
        "the daemon's own session was asked for nothing"
    );
    assert_eq!(sessions.serve_reviews(), 0, "no second child for one row");
    let row = f.store().review_row("m-mine").unwrap().unwrap();
    assert_eq!(row.state, ReviewState::Reviewing);
    assert_eq!(row.reviewer.as_deref(), Some("sub-1"));
}

/// **A row another daemon claimed is not this daemon's to serve** (defect 9).
#[test]
fn a_review_another_daemon_claimed_is_left_alone() {
    let Some(f) = Fixture::new("theirs") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.claimed("m-theirs", "not mine", "another-daemon/1/1");
    let mut sessions = f.sessions(&parts, Ending::Running);
    assert_eq!(sessions.serve_reviews(), 0);
    assert!(f.asked().is_empty());
    let row = f.store().review_row("m-theirs").unwrap().unwrap();
    assert!(row.reviewer.is_none(), "{row:?}");
}

/// **A verdict handed in approves the head the claim read, and the branch is in the merge queue
/// in the same transaction** (decision 6); the review is judged once.
#[test]
fn a_submitted_verdict_approves_the_bound_head_and_queues_the_landing() {
    use letibot_tools::gatekeeper::{Decision, LookedAt, Submission};
    let Some(f) = Fixture::new("submitted") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.claimed("m-mine", "keep the last record", &f.me());
    let mut sessions = f.sessions(&parts, Ending::Running);
    assert_eq!(sessions.serve_reviews(), 1);
    assert_eq!(sessions.fill_gatekeeper_board(), 1);
    parts
        .reviews
        .submit(
            "sub-1",
            Submission {
                decision: Decision::Accept,
                reasons: vec!["the test covers it".into()],
                looked_at: LookedAt {
                    files: vec!["src/reader.rs".into()],
                    commands: vec!["git diff base...head-m-mine".into()],
                },
            },
        )
        .expect("a reviewer of this queue may hand its verdict in");
    assert_eq!(sessions.serve_reviews(), 0);
    let row = f.store().review_row("m-mine").unwrap().unwrap();
    assert_eq!(row.state, ReviewState::Approved, "{row:?}");
    assert_eq!(row.reviewed_sha.as_deref(), Some("head-m-mine"));
    assert_eq!(row.verdict_by.as_deref(), Some("reviewer:sub-1"));
    assert_eq!(row.reasons, vec!["the test covers it"]);
    let merges = f.store().merge_rows(Some(REPO)).unwrap();
    assert_eq!(merges.len(), 1);
    assert_eq!(merges[0].approved_sha, "head-m-mine");
    assert_eq!(merges[0].state, MergeState::Queued);
    assert_eq!(
        sessions.fill_gatekeeper_board(),
        0,
        "the row comes off the board"
    );
    let refused = parts
        .reviews
        .submit(
            "sub-1",
            Submission {
                decision: Decision::Reject,
                reasons: vec!["on reflection".into()],
                looked_at: LookedAt::default(),
            },
        )
        .unwrap_err();
    assert!(refused.contains("is not reviewing an entry"), "{refused}");
}

/// **A child of another session cannot submit for this queue.**
#[test]
fn a_child_of_another_session_cannot_submit_for_this_queue() {
    use letibot_tools::gatekeeper::{Decision, LookedAt, Submission};
    let Some(f) = Fixture::new("foreign") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    let stranger = "s-elsewhere-sub-1";
    f.registry
        .create_under(
            stranger,
            "a child of another tree",
            Sessions::wiring(&f.cfg),
            Some("s-elsewhere".into()),
        )
        .expect("the stranger registers");
    f.claimed("m-mine", "keep the last record", &f.me());
    let mut sessions = f.sessions(&parts, Ending::Running);
    assert_eq!(sessions.serve_reviews(), 1);
    let accept = |why: &str| Submission {
        decision: Decision::Accept,
        reasons: vec![why.into()],
        looked_at: LookedAt::default(),
    };
    let refused = parts
        .reviews
        .submit(stranger, accept("looks fine from over here"))
        .unwrap_err();
    assert!(refused.contains(stranger) && refused.contains("No verdict was recorded"));
    sessions.serve_reviews();
    assert_eq!(
        f.store().review_row("m-mine").unwrap().unwrap().state,
        ReviewState::Reviewing,
        "nothing moved"
    );
    parts
        .reviews
        .submit("sub-1", accept("the test covers it"))
        .expect("the reviewer that was asked still can");
    sessions.serve_reviews();
    assert_eq!(
        f.store().review_row("m-mine").unwrap().unwrap().state,
        ReviewState::Approved
    );
}

/// **An answer in prose is not a verdict: the attempt is counted and the review waits again.**
/// The prose carries the words the old closing block was parsed for; a build that read them would
/// approve this branch.
#[test]
fn an_answer_with_no_submission_is_a_counted_attempt() {
    let Some(f) = Fixture::new("prose") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.claimed("m-mine", "keep the last record", &f.me());
    let mut sessions = f.sessions(
        &parts,
        Ending::Prose("I read the diff.\n\nverdict: accept\nreasons: - fine".into()),
    );
    assert_eq!(sessions.serve_reviews(), 1);
    sessions.serve_reviews();
    let row = f.store().review_row("m-mine").unwrap().unwrap();
    assert_eq!(row.state, ReviewState::Waiting, "{row:?}");
    assert!(row.verdict.is_none());
    assert_eq!(row.attempts, 1);
    assert!(
        row.failure.contains("review_verdict") && row.failure.contains("sub-1"),
        "{}",
        row.failure
    );
    assert!(
        f.store().merge_rows(None).unwrap().is_empty(),
        "nothing approved"
    );
}

/// **A reviewer that vanished is a counted attempt too** (defect 10) — the v23 queue wrote
/// nothing for it and spawned another, for ever.
#[test]
fn a_child_that_vanishes_is_a_counted_attempt() {
    let Some(f) = Fixture::new("gone") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.claimed("m-mine", "keep the last record", &f.me());
    let mut sessions = f.sessions(&parts, Ending::Gone);
    assert_eq!(sessions.serve_reviews(), 1);
    assert_eq!(
        sessions.serve_reviews(),
        0,
        "no replacement is started by this pass: the row waits for the queue's next claim"
    );
    let row = f.store().review_row("m-mine").unwrap().unwrap();
    assert_eq!(row.state, ReviewState::Waiting, "{row:?}");
    assert_eq!(row.attempts, 1, "counted");
    assert!(row.failure.contains("vanished"), "{}", row.failure);
}

/// **The clock is armed, so the pass is reached at all.**
#[test]
fn the_daemon_has_a_clock_for_the_store_it_reads() {
    let Some(f) = Fixture::new("clock") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    let mut sessions = f.sessions(&parts, Ending::Running);
    assert!(sessions.next_idle_at().is_some());
    assert!(sessions.reviews_due(), "armed at open to now");
    assert!(!sessions.reviews_due(), "spending it arms the next");
    assert!(sessions.next_idle_at().is_some());
}
