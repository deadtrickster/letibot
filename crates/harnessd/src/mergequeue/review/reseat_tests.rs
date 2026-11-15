//! A daemon restart gives a claimed review its reviewer back: the row stays claimed by the daemon
//! that takes the queue, and the review pass reopens the stored reviewer session by id.

use std::path::Path;
use std::sync::Mutex;

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{MergePriority, Move, NewReview, ReviewKind, ReviewState, Store};
use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus};
use letibot_tools::builtins::verdict::ReviewDesk;

use super::hosts::ReviewHosts;
use super::*;
use crate::mergequeue::serve::tests::{TempDir, branch, commit, repo};
use crate::mergequeue::settings::QueueSettings;
use crate::sessions::gatekeeper::ReviewerPlacement;

const OLD: &str = "host/1/1";
const ME: &str = "host/2/2";
const HOST: &str = "s-gatekeeper";
const REVIEWER: &str = "s-gatekeeper-sub-1";

/// What the pass asked of the runner: `start` and `reopen` calls, by handle.
#[derive(Default)]
struct Runner {
    reopen_refused: bool,
    started: Mutex<Vec<String>>,
    reopened: Mutex<Vec<String>>,
}

impl TaskRunner for Runner {
    fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        self.started.lock().unwrap().push("fresh".into());
        Ok("s-gatekeeper-sub-fresh".into())
    }
    fn reopen(&self, handle: &str, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        if self.reopen_refused {
            return Err(format!("`{handle}` is not in the store"));
        }
        self.reopened.lock().unwrap().push(handle.into());
        Ok(handle.into())
    }
    fn collect(&self, handle: &str, _timeout: std::time::Duration) -> TaskStatus {
        // The runner of a daemon that just started knows only what it started or reopened.
        let known = self.reopened.lock().unwrap().iter().any(|h| h == handle)
            || handle == "s-gatekeeper-sub-fresh" && !self.started.lock().unwrap().is_empty();
        match known {
            true => TaskStatus::Running {
                note: None,
                child: None,
            },
            false => TaskStatus::Unknown,
        }
    }
}

/// A review the old daemon claimed and had a reviewer on, and the new daemon taking the queue.
fn left_by_a_stop(dir: &TempDir) -> (Store, String) {
    let repo = repo(&dir.0.join("the-repository"));
    let (_wt, base, head) = branch(&repo, "the-work", "b.txt");
    commit(&repo, "a.txt", "two\n", "main moves on");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    let key = repo.display().to_string();
    store
        .enqueue_review(
            &NewReview {
                id: "the-review".into(),
                repository: key.clone(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: "s-child".into(),
                branch: "agent/the-work".into(),
                worktree: None,
                base_sha: base,
                head_sha: head,
                brief: "do the work".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            1,
        )
        .unwrap();
    assert!(
        store
            .move_review(
                "the-review",
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new(OLD, "claimed")
                    .set("owner", OLD)
                    .set("reviewer", REVIEWER),
                2,
            )
            .unwrap()
    );
    (store, key)
}

fn pass(store: &Store, dir: &TempDir, runner: &Runner, desk: &ReviewDesk) -> Served {
    let machines = ReviewHosts::new();
    machines.set_spare(None, Some(32.0), None);
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let pool = Pool {
        store,
        runner,
        desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, _e| {},
        deliver: &|_: &str, _: &str| Ok(String::new()),
        settings: &settings,
        clones: dir.0.join(CLONES_DIR),
        live: &|_: &str| true,
        reviewer_skills: &[],
        machines,
        reviewer_hosts: None,
        slots: &Vec::new,
        claude: None,
        layers: None,
    };
    serve(&pool).expect("a pass")
}

#[test]
fn a_restart_reopens_the_stored_reviewer_of_a_claimed_row() {
    let dir = TempDir::new("reseat-reopen");
    let (store, key) = left_by_a_stop(&dir);
    assert_eq!(store.recover_queue(&key, ME, 10, 60_000).unwrap(), 1);
    let r = store.review_row("the-review").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Reviewing, "still claimed: {r:?}");
    assert_eq!(r.owner, ME, "by the daemon that took the queue");
    assert_eq!(r.reviewer.as_deref(), Some(REVIEWER), "with its reviewer");
    assert_eq!(r.deadline_ms, Some(60_010), "and a deadline from now");

    let runner = Runner::default();
    let desk = ReviewDesk::new();
    let served = pass(&store, &dir, &runner, &desk);
    assert_eq!(served.started, 1);
    assert_eq!(*runner.reopened.lock().unwrap(), vec![REVIEWER.to_string()]);
    assert!(
        runner.started.lock().unwrap().is_empty(),
        "no fresh reviewer"
    );
    assert!(desk.is_open_for("the-review"), "the review is on the desk");
    let r = store.review_row("the-review").unwrap().unwrap();
    assert_eq!(r.reviewer.as_deref(), Some(REVIEWER));
    assert_eq!(r.attempts, 0, "a restart spends no attempt");

    // The next pass finds it running and leaves it.
    pass(&store, &dir, &runner, &desk);
    assert_eq!(runner.reopened.lock().unwrap().len(), 1);
}

#[test]
fn a_reviewer_that_cannot_be_reopened_is_replaced_without_an_attempt() {
    let dir = TempDir::new("reseat-fresh");
    let (store, key) = left_by_a_stop(&dir);
    store.recover_queue(&key, ME, 10, 60_000).unwrap();
    let runner = Runner {
        reopen_refused: true,
        ..Runner::default()
    };
    let desk = ReviewDesk::new();
    pass(&store, &dir, &runner, &desk);
    assert_eq!(runner.started.lock().unwrap().len(), 1, "a fresh reviewer");
    let r = store.review_row("the-review").unwrap().unwrap();
    assert_eq!(r.reviewer.as_deref(), Some("s-gatekeeper-sub-fresh"));
    assert_eq!(r.attempts, 0);
    let why = store
        .transitions("review", "the-review")
        .unwrap()
        .last()
        .unwrap()
        .why
        .clone();
    assert!(why.contains("could not be reopened"), "{why}");
}
