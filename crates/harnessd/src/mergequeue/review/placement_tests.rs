//! **Where a reviewer is placed, now that the placement fans out** — a declared host, the per-host
//! cap, and what happens when a machine refuses.
//!
//! Every test here goes through the real [`serve`] with a real store, a real desk and a runner that
//! records the [`TaskSpec`] it was handed. Nothing is spawned and no machine is touched: the pool's
//! facts are installed (`ReviewHosts::set_spare`) and `$LETIBOT_HOSTS` names a file of this test's
//! own, so a test can say *lab2x1 has thirty-one spare cores* without a lab2x1.

use std::path::Path;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{MergePriority, Move, NewReview, ReviewKind, ReviewState, Store};
use letibot_tools::builtins::task::{Placement, TaskRunner, TaskSpec, TaskStatus};
use letibot_tools::builtins::verdict::ReviewDesk;

use super::hosts::ReviewHosts;
use super::hosts::tests::HostsFile;
use super::*;
use crate::mergequeue::serve::tests::{TempDir, branch, commit, repo, sh};
use crate::mergequeue::settings::QueueSettings;
use crate::sessions::gatekeeper::ReviewerPlacement;

const ME: &str = "reviewer-daemon";
const HOST: &str = "s-gatekeeper";
const CHILD: &str = "s-child";

/// What a [`Recorder`] was asked to start, and what it answers.
#[derive(Default)]
struct Started {
    items: Mutex<Vec<(String, TaskSpec)>>,
}

impl Started {
    fn specs(&self) -> Vec<TaskSpec> {
        self.items
            .lock()
            .unwrap()
            .iter()
            .map(|(_, s)| s.clone())
            .collect()
    }
    fn prompts(&self) -> Vec<String> {
        self.items
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
}

/// **A runner that records and answers a handle per start** — and, when `fail_first` is set, whose
/// FIRST child dies at its open with the sentence a remote placement gives.
struct Recorder {
    started: Arc<Started>,
    fail_first: bool,
}

impl TaskRunner for Recorder {
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
        let mut items = self.started.items.lock().unwrap();
        let n = items.len();
        items.push((prompt.to_string(), spec.clone()));
        Ok(format!("s-gatekeeper-sub-{n}"))
    }
    fn collect(&self, handle: &str, _timeout: std::time::Duration) -> TaskStatus {
        if self.fail_first && handle == "s-gatekeeper-sub-0" {
            return TaskStatus::Failed {
                why: format!(
                    "placing this session on lab2x1: ssh exited 255, which is a connection or \
                     authentication failure and never a remote command's own status — \
                     ssh: connect to host lab2x1 port 22: No route to host"
                ),
            };
        }
        TaskStatus::Running {
            note: None,
            child: None,
        }
    }
}

/// **One claimed review of one branch**, in a repository of this test's own at a head that is not
/// `main`'s tip.
fn claimed(dir: &TempDir) -> (Store, std::path::PathBuf, String) {
    let repo = repo(&dir.0.join("the-repository"));
    let (_wt, base, head) = branch(&repo, "the-work", "b.txt");
    let tip = commit(&repo, "a.txt", "two\n", "main moves on");
    assert_ne!(head, tip, "the fixture needs two different commits");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    store
        .enqueue_review(
            &NewReview {
                id: "the-review".into(),
                repository: repo.display().to_string(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: CHILD.into(),
                branch: "agent/the-work".into(),
                worktree: None,
                base_sha: base,
                head_sha: head.clone(),
                brief: "do the work".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            1,
        )
        .expect("the review row");
    assert!(
        store
            .move_review(
                "the-review",
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new(ME, "claimed").set("owner", ME),
                2,
            )
            .unwrap(),
        "the fixture must really have claimed the row"
    );
    (store, repo, head)
}

/// **A reviewer placed on a declared host is a `where: \"host:NAME\"` child** — the same door a
/// `task` child takes — and the tree it judges travels with it.
///
/// Three assertions, and each fails if the arm goes back to running everything here:
/// * the spec's placement is `Remote("lab2x1")`, which is what makes the child's whole seat that
///   machine's (its `bash`, `read`, `grep`, jobs and confinement probe are `SshBackend`'s);
/// * `vm_source` is the queue's own clone, whose detached HEAD is the row's `head_sha` — so
///   `SshBackend::place` bundles THAT commit and never the operator's checkout or `main`'s tip;
/// * there is no `worktree` and the prompt names no path, because on that machine the reviewer's
///   own workspace IS the checkout and a path from here would name one it does not have.
#[test]
fn a_reviewer_on_a_declared_host_is_a_remote_child_at_the_head_it_judges() {
    let _hosts = HostsFile::with(&["lab2x1"]);
    let dir = TempDir::new("review-on-a-host");
    let (store, repo, head) = claimed(&dir);

    let machines = ReviewHosts::new();
    machines.set_spare(Some("lab2x1"), Some(31.9), None);
    // This machine is full, so the count below is the host's alone and the reviewer can only have
    // gone there.
    machines.set_spare(None, Some(0.0), None);
    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
        fail_first: false,
    };
    let desk = ReviewDesk::new();
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
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
        machines: machines.clone(),
        reviewer_hosts: Some(vec!["lab2x1".to_string()]),
        slots: &Vec::new,
    };
    assert_eq!(
        serve(&pool).expect("a pass").started,
        1,
        "the claimed row's reviewer is started"
    );

    let spec = started.specs().remove(0);
    assert_eq!(
        spec.placement,
        Placement::Remote("lab2x1".into()),
        "the child's whole seat runs there — the `where: \"host:NAME\"` door, reused"
    );
    let source = spec
        .vm_source
        .as_ref()
        .expect("the tree it judges is named for the machine that will check it out");
    assert_ne!(
        source.as_path(),
        repo.as_path(),
        "a reviewer must not send the operator's checkout"
    );
    assert!(
        source.starts_with(dir.0.join(CLONES_DIR)),
        "the queue's own clone of the head: {}",
        source.display()
    );
    assert_eq!(
        sh(source, &["rev-parse", "HEAD"]),
        head,
        "and its HEAD is the commit the verdict is bound to, which is what `SshBackend::place` \
         bundles and checks out there"
    );
    assert!(
        spec.worktree.is_none(),
        "no worktree: `worktree_build_env` would run `git` on a path this machine has and that \
         one does not"
    );
    let prompt = started.prompts().remove(0);
    assert!(
        !prompt.contains(&dir.0.join(CLONES_DIR).display().to_string()),
        "the prompt names no path — on that machine the reviewer's own workspace IS the checkout: \
         {prompt}"
    );
    assert!(
        prompt.contains(&head),
        "and it still names the head: {prompt}"
    );
    // The per-host cap counts it, so a second reviewer does not go to the same box.
    assert_eq!(
        machines.capacity(Some(&["lab2x1".to_string()])),
        capacity::PER_HOST_MAX - 1,
        "four places on the box and one taken by the reviewer just started; this machine has no \
         spare, so the whole count is the host's"
    );
}

/// **The per-host cap refuses a second reviewer on the same box, and the row waits rather than
/// failing** — one reviewer's build must not eat a machine another reviewer needs, and a queue with
/// no free slot has not tried and failed.
///
/// `lab2x1` is given four spare cores, which is room for exactly one reviewer. The first goes
/// there; the second finds this machine with no spare too (installed as `0.0`) and waits.
#[test]
fn the_per_host_cap_puts_the_second_reviewer_in_line_instead_of_on_the_same_box() {
    let _hosts = HostsFile::with(&["lab2x1"]);
    let dir = TempDir::new("review-per-host-cap");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    let repo = repo(&dir.0.join("the-repository"));
    let (_wt, base, head) = branch(&repo, "the-work", "b.txt");
    for id in ["one", "two"] {
        store
            .enqueue_review(
                &NewReview {
                    id: id.into(),
                    repository: repo.display().to_string(),
                    target: "main".into(),
                    kind: ReviewKind::Branch,
                    session_id: format!("s-child-{id}"),
                    branch: "agent/the-work".into(),
                    worktree: None,
                    base_sha: base.clone(),
                    head_sha: head.clone(),
                    brief: "do the work".into(),
                    priority: MergePriority::Subagent,
                    fixes: None,
                    evidence: "enqueued".into(),
                },
                "test",
                1,
            )
            .expect("the review row");
        assert!(
            store
                .move_review(
                    id,
                    ReviewState::Waiting,
                    ReviewState::Reviewing,
                    &Move::new(ME, "claimed").set("owner", ME),
                    2,
                )
                .unwrap()
        );
    }
    let machines = ReviewHosts::new();
    machines.set_spare(Some("lab2x1"), Some(4.0), None);
    // This machine is full: the pool must not quietly fall back to it.
    machines.set_spare(None, Some(0.0), None);
    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
        fail_first: false,
    };
    let desk = ReviewDesk::new();
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
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
        machines: machines.clone(),
        reviewer_hosts: Some(vec!["lab2x1".to_string(), "local".to_string()]),
        slots: &Vec::new,
    };
    let served = serve(&pool).expect("a pass");
    assert_eq!(served.started, 1, "one reviewer, on the one box with room");
    let one = store.review_row("one").unwrap().unwrap();
    let two = store.review_row("two").unwrap().unwrap();
    assert_eq!(one.state, ReviewState::Reviewing, "{one:?}");
    assert_eq!(two.state, ReviewState::Waiting, "the second waits: {two:?}");
    assert!(
        two.failure.starts_with(capacity::WAITING_FOR_ROOM),
        "and says which wait it is: {}",
        two.failure
    );
    assert_eq!(two.attempts, 0, "nothing was tried and failed: {two:?}");
    // **And the wait is held, not claimed every two seconds** — the queue's own gate on it.
    assert!(
        capacity::held_until(&two.failure, two.deadline_ms, now_ms()),
        "a row waiting for a reviewer slot is not claimed again until its not-before time"
    );
    assert_eq!(started.specs().len(), 1, "one child was started, not two");
}

/// **A reviewer that handed its verdict in frees the place it held** — the leak this test exists
/// against, and it was a real one: the first cut freed a place at each place a reviewer ENDED and
/// missed the one ending EVERY successful review takes (a verdict at the desk), so each completed
/// review held a place for the life of the daemon and the pool ran down to nothing while the queue
/// went on claiming. A queue that stops is worse than the one this change set out to speed up.
///
/// Nothing here is asserted by hand: `serve` starts the reviewer, the desk takes the verdict the
/// way a reviewer hands one in, and the next `serve` collects it — and then the pool is asked what
/// it has room for.
#[test]
fn a_verdict_frees_the_place_the_reviewer_held() {
    use letibot_tools::gatekeeper::{Decision, LookedAt, Submission};
    let _hosts = HostsFile::with(&["lab2x1"]);
    let dir = TempDir::new("review-frees-its-place");
    let (store, _repo, _head) = claimed(&dir);

    let machines = ReviewHosts::new();
    // **Four spare cores: exactly ONE place**, so *freed* and *not freed* are two different answers
    // and not two ways of saying `PER_HOST_MAX`.
    machines.set_spare(Some("lab2x1"), Some(4.0), None);
    machines.set_spare(None, Some(0.0), None);
    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
        fail_first: false,
    };
    let desk = ReviewDesk::new();
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
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
        machines: machines.clone(),
        reviewer_hosts: Some(vec!["lab2x1".to_string()]),
        slots: &Vec::new,
    };
    assert_eq!(serve(&pool).expect("a pass").started, 1);
    let child = "s-gatekeeper-sub-0";
    assert_eq!(
        machines.capacity(Some(&["lab2x1".to_string()])),
        0,
        "the one place is taken while the reviewer is running"
    );

    // **The reviewer hands its verdict in** — the desk is the only door one goes through.
    desk.submit(
        child,
        Submission {
            decision: Decision::Accept,
            reasons: vec!["the change does what the brief asked".into()],
            looked_at: LookedAt {
                files: vec!["b.txt".into()],
                commands: vec!["git diff".into()],
            },
        },
    )
    .expect("the verdict is taken at the desk");
    assert!(
        serve(&pool).expect("a pass").moved,
        "the verdict is written"
    );
    assert_eq!(
        store.review_row("the-review").unwrap().unwrap().state,
        ReviewState::Approved,
        "the review is over"
    );
    assert_eq!(
        machines.capacity(Some(&["lab2x1".to_string()])),
        1,
        "and the place it held is free again — a second review of a fourteen-row queue must be \
         able to go to the same machine"
    );
    assert!(
        machines.choose(Some(&["lab2x1".to_string()])).is_some(),
        "the pool offers the machine again"
    );
}

/// **A host that cannot be reached falls back to this machine, and says so** — the operator's rule:
/// *"the queue must not go red because a machine was rebooting"*.
///
/// The first reviewer is placed on `lab2x1` and dies at its open with the sentence
/// `SshBackend::place` gives (`placing this session on lab2x1: …`). Nothing is counted against the
/// row, the host is out for the rest of the pass, and a second reviewer is started HERE with a
/// `worktree` — and the move, and the warning, name the machine.
#[test]
fn a_host_that_refused_the_placement_is_left_out_and_the_review_runs_here() {
    let _hosts = HostsFile::with(&["lab2x1"]);
    let dir = TempDir::new("review-falls-back");
    let (store, _repo, head) = claimed(&dir);

    let machines = ReviewHosts::new();
    machines.set_spare(Some("lab2x1"), Some(31.9), None);
    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
        fail_first: true,
    };
    let desk = ReviewDesk::new();
    let raised: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let events = |_: SessionEvent| {};
    let warn = {
        let raised = raised.clone();
        move |_: &str, e: SessionEvent| {
            if let SessionEvent::Warning { code, detail, .. } = e {
                raised.lock().unwrap().push((code, detail));
            }
        }
    };
    let settings = |_: &Path| Ok(QueueSettings::default());
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &warn,
        deliver: &|_: &str, _: &str| Ok(String::new()),
        settings: &settings,
        clones: dir.0.join(CLONES_DIR),
        live: &|_: &str| true,
        reviewer_skills: &[],
        machines: machines.clone(),
        reviewer_hosts: Some(vec!["lab2x1".to_string(), "local".to_string()]),
        slots: &Vec::new,
    };
    // **One pass does it, and that is the point of the fallback being in the COLLECT half**: the
    // reviewer that goes to the host is opened at the desk in the same pass, its open fails, and
    // the review is started again here before the pass ends — so a machine that is down costs one
    // pass and no attempt, not a wait for a deadline.
    let served = serve(&pool).expect("a pass");
    assert_eq!(served.started, 1, "one reviewer was started on the host");
    let row = store.review_row("the-review").unwrap().unwrap();
    assert_eq!(
        row.state,
        ReviewState::Reviewing,
        "the review is still running, on this machine: {row:?}"
    );
    assert_eq!(
        row.reviewer.as_deref(),
        Some("s-gatekeeper-sub-1"),
        "a second reviewer, started here: {row:?}"
    );
    assert_eq!(
        row.attempts, 0,
        "a machine that was rebooting is not an attempt against the branch: {row:?}"
    );
    let specs = started.specs();
    assert_eq!(specs.len(), 2, "two children: one there, one here");
    assert_eq!(specs[0].placement, Placement::Remote("lab2x1".into()));
    assert_eq!(
        specs[1].placement,
        Placement::Host,
        "the fallback is this machine, and a child of this machine's queue"
    );
    let tree = specs[1]
        .worktree
        .as_ref()
        .expect("and it works in a checkout of the head, as every reviewer here does")
        .path
        .clone();
    assert_eq!(
        sh(Path::new(&tree), &["rev-parse", "HEAD"]),
        head,
        "at the same head the verdict is bound to"
    );
    // **And it was said**, both in the row's history and as a warning naming the machine.
    let why = store
        .transitions("review", "the-review")
        .expect("the history")
        .into_iter()
        .rfind(|t| t.to_state == "reviewing")
        .expect("the fallback is in the history")
        .why;
    assert!(
        why.contains("lab2x1") && why.contains("runs here"),
        "the move names the machine and what happened: {why}"
    );
    let said = raised.lock().unwrap().clone();
    assert!(
        said.iter()
            .any(|(c, d)| c == "queue_reviewer_fell_back" && d.contains("lab2x1")),
        "the operator is told once, by name: {said:?}"
    );
    // **And it is a reviewer the queue can HEAR** — the desk still holds it, and a verdict it hands
    // in is taken. This is the assertion the first cut was missing: the fallback opens its desk entry
    // from inside the collect loop, AFTER the list of live children was taken, so a fallback left off
    // that list had its entry closed in the same pass that opened it. It then ran a whole review,
    // called `review_verdict`, and was refused with *"is not reviewing an entry of the merge queue"*
    // — a fallback nobody can hear, spending the three attempts and then blocking.
    let child = row.reviewer.clone().expect("the row names its reviewer");
    assert!(
        desk.is_open_for("the-review"),
        "the fallback reviewer is still on the desk after the pass that started it"
    );
    assert_eq!(
        desk.open(HOST)
            .iter()
            .map(|o| o.child.clone())
            .collect::<Vec<_>>(),
        vec![child.clone()],
        "and it is the one the row names, not the dead child from the host"
    );
    desk.submit(
        &child,
        letibot_tools::gatekeeper::Submission {
            decision: letibot_tools::gatekeeper::Decision::Accept,
            reasons: vec!["the fallback read the branch".into()],
            looked_at: letibot_tools::gatekeeper::LookedAt {
                files: vec!["b.txt".into()],
                commands: vec!["git diff".into()],
            },
        },
    )
    .expect("a verdict from the fallback reviewer is taken at the desk");
    assert!(
        serve(&pool).expect("a pass").moved,
        "the verdict is written"
    );
    assert_eq!(
        store.review_row("the-review").unwrap().unwrap().state,
        ReviewState::Approved,
        "and the review ends on it — the fallback's verdict is the queue's"
    );

    // **And the pass that used it is the pass that clears it** — `end_pass` is the queue's promise
    // that a reboot is minutes, so a host that failed here is tried again next time. That the
    // FAILURE removes it from the rest of ITS pass is `hosts::tests`'
    // `a_host_that_said_no_is_skipped_this_pass_and_asked_again_next_one`, which is where a second
    // row in one pass can be staged.
    assert!(
        machines.down(Some("lab2x1")).is_none(),
        "the pass is over, so nothing is held against the machine"
    );
}
