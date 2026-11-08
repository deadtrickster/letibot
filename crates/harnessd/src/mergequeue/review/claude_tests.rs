//! A Claude reviewer, end to end: the REAL `serve`, store and desk against a stub executable that
//! prints the JSON shapes the headless docs give. No `claude` binary is started anywhere - the stub
//! is a shell script this file writes, and it records the argv it was called with so the run's own
//! flags can be asserted rather than believed.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{MergePriority, Move, NewReview, ReviewKind, ReviewState, Store};
use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus};
use letibot_tools::builtins::verdict::ReviewDesk;

use super::claude::{self, Runs};
use super::*;
use crate::mergequeue::serve::tests::{TempDir, branch, repo};
use crate::mergequeue::settings::QueueSettings;
use crate::sessions::gatekeeper::ReviewerPlacement;

const ME: &str = "claude-daemon";
const HOST: &str = "s-gatekeeper";
const CHILD: &str = "s-child";
const ID: &str = "the-review";

/// A runner that starts nothing: a project whose reviewers are Claude runs must never reach a
/// session child, and a test that recorded a start here would be asserting the wrong kind.
struct NoChild;

impl TaskRunner for NoChild {
    fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        Err("no session reviewer is started in this file".into())
    }
    fn collect(&self, _handle: &str, _timeout: Duration) -> TaskStatus {
        TaskStatus::Running {
            note: None,
            child: None,
        }
    }
}

/// A runner that records what it was asked to start - the other half of *the kind is the project's*.
#[derive(Default)]
struct Recorder {
    started: Mutex<Vec<String>>,
}

impl TaskRunner for Recorder {
    fn start(&self, prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        let mut started = self.started.lock().unwrap();
        started.push(prompt.to_string());
        Ok(format!("s-gatekeeper-sub-{}", started.len()))
    }
    fn collect(&self, _handle: &str, _timeout: Duration) -> TaskStatus {
        TaskStatus::Running {
            note: None,
            child: None,
        }
    }
}

/// The stub binary: a script that appends its arguments, one per line, to `argv.txt` in the tree it
/// was started in - so two runs on one review are two records - and then does what its body says.
fn stub(dir: &TempDir, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.0.join(name);
    let script =
        format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> argv.txt; done\n{body}\n");
    std::fs::write(&path, script).expect("the stub is written");
    let mut perms = std::fs::metadata(&path).expect("the stub").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("the stub is executable");
    path
}

/// The JSON a run answers with: the docs' result object, carrying `structured_output` when the body
/// says so. Single quotes keep the shell out of the prose.
fn result(session: &str, structured: Option<&str>) -> String {
    match structured {
        Some(s) => format!(
            "echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\
             \"result\":\"I read the change.\",\"session_id\":\"{session}\",\
             \"structured_output\":{s}}}'"
        ),
        None => format!(
            "echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\
             \"result\":\"Looks good to me!\",\"session_id\":\"{session}\"}}'"
        ),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// One review of one branch, claimed by this daemon, in a repository of this test's own.
fn claimed(dir: &TempDir) -> (Store, PathBuf, String) {
    let repo = repo(&dir.0.join("the-repository"));
    let (_wt, base, head) = branch(&repo, "the-work", "b.txt");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    store
        .enqueue_review(
            &NewReview {
                id: ID.into(),
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
    claim(&store, head.clone());
    (store, repo, head)
}

/// Put the row in `reviewing` under this daemon with an hour's deadline - the claim the queue's own
/// thread would have written.
fn claim(store: &Store, head: String) {
    assert!(
        store
            .move_review(
                ID,
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new(ME, "claimed")
                    .set("owner", ME)
                    .set("head_sha", head)
                    .set("deadline_ms", now_ms() + 3_600_000)
                    .clear("reviewer"),
                2,
            )
            .unwrap(),
        "the fixture must really have claimed the row"
    );
}

/// The settings a project that reviews with Claude has: a binary, and the rest at their defaults.
fn settings_with(binary: &Path, timeout_secs: u64) -> QueueSettings {
    let mut settings = QueueSettings::default();
    let mut claude = claude::Settings::new(binary.to_path_buf());
    claude.timeout = Duration::from_secs(timeout_secs);
    settings.claude = Some(claude);
    settings
}

/// One pool over one store, with the settings the test wants. `runs` is the daemon's table of
/// processes, which lives across the passes exactly as it does in the daemon.
struct Fixture<'a> {
    store: &'a Store,
    desk: ReviewDesk,
    runs: Runs,
    clone_dir: TempDir,
    settings: QueueSettings,
    runner: &'a dyn TaskRunner,
    /// Whether this pool holds a Claude runner at all - `false` is a daemon without the plumbing.
    holds_runs: bool,
    events: Mutex<Vec<String>>,
}

impl<'a> Fixture<'a> {
    fn new(store: &'a Store, settings: QueueSettings, runner: &'a dyn TaskRunner) -> Self {
        Fixture {
            store,
            desk: ReviewDesk::new(),
            runs: Runs::new(),
            clone_dir: TempDir::new("claude-review-clones"),
            settings,
            runner,
            holds_runs: true,
            events: Mutex::new(Vec::new()),
        }
    }

    /// The clones directory, named as the queue's is - `sweep_clones` refuses any other name.
    fn clones(&self) -> PathBuf {
        self.clone_dir.0.join(CLONES_DIR)
    }

    fn row(&self) -> ReviewRow {
        self.store
            .review_row(ID)
            .expect("a store read")
            .expect("the row")
    }

    /// One pass of the reviewers' host.
    fn pass(&mut self) -> Served {
        let settings = self.settings.clone();
        let settings = move |_: &Path| Ok(settings.clone());
        let none = |_: &str, _: &str| Ok(String::new());
        let pool = Pool {
            store: self.store,
            runner: self.runner,
            desk: &self.desk,
            host: HOST,
            me: ME,
            placement: ReviewerPlacement::Host,
            events: &|e: SessionEvent| {
                if let SessionEvent::QueueMoved { id, to, .. } = e {
                    self.events.lock().unwrap().push(format!("{id} {to}"));
                }
            },
            warn: &|_: &str, e: SessionEvent| {
                if let SessionEvent::Warning { code, .. } = e {
                    self.events.lock().unwrap().push(code);
                }
            },
            deliver: &none,
            live: &|_: &str| true,
            settings: &settings,
            reviewer_skills: &[],
            machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
            reviewer_hosts: Some(vec!["local".to_string()]),
            clones: self.clones(),
            slots: &Vec::new,
            claude: self.holds_runs.then_some(&self.runs),
        };
        serve(&pool).expect("a pass")
    }

    /// Passes until `done` holds, or the budget runs out: a run is a process, so its verdict arrives
    /// on a pass after the one that started it and never inside it.
    fn until(&mut self, what: &str, done: impl Fn(&Store) -> bool) {
        for _ in 0..150 {
            if done(self.store) {
                return;
            }
            self.pass();
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("`{what}` never happened; the row is {}", self.row().failure);
    }
}

/// The red tip's own review, by a Claude run: the prompt is the RED TIP's - there the words mean
/// something else - and the run's `accept` reaches the queue's own act for a target review.
#[test]
fn a_red_tips_review_is_a_claude_run_too() {
    let dir = TempDir::new("claude-red");
    let repo = repo(&dir.0.join("the-repository"));
    let tip = crate::mergequeue::serve::tests::commit(&repo, "a.txt", "two\n", "a tip");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    store
        .enqueue_review(
            &NewReview {
                id: ID.into(),
                repository: repo.display().to_string(),
                target: "main".into(),
                kind: ReviewKind::Target,
                session_id: String::new(),
                branch: "main".into(),
                worktree: None,
                base_sha: tip.clone(),
                head_sha: tip.clone(),
                brief: "`main` went red".into(),
                priority: MergePriority::Urgent,
                fixes: None,
                evidence: "red".into(),
            },
            "test",
            1,
        )
        .expect("the red tip's review");
    claim(&store, tip.clone());
    let binary = stub(
        &dir,
        "claude",
        &result(
            "cc-red",
            Some(
                r#"{"decision":"accept","reasons":["the red is a flake"],"files":[],"commands":[]}"#,
            ),
        ),
    );
    let mut f = Fixture::new(&store, settings_with(&binary, 60), &NoChild);
    f.until("the tip's review is judged", |s| {
        s.review_row(ID).unwrap().unwrap().state == ReviewState::Withdrawn
    });
    let row = f.row();
    assert_eq!(row.verdict.as_deref(), Some("accept"));
    assert!(
        row.evidence.contains("the red is not in the code"),
        "the queue's own sentence for a target review: {}",
        row.evidence
    );
    assert_eq!(
        store
            .target_health(&repo.display().to_string(), "main")
            .unwrap()
            .expect("the tip was put back in line for a gate")
            .state,
        letibot_tokencore::store::HealthState::Gating,
        "an accept on a red tip gates it again"
    );
    // And the prompt the run was given is the red tip's, with the door this reviewer has: the words
    // a target verdict means, and no tool it does not hold.
    let said = recorded(&f.clones()).join("\n");
    assert!(said.contains("THIS REVIEW IS DIFFERENT"), "{said}");
    assert!(
        said.contains("structured output") && !said.contains("`review_verdict`"),
        "the run is told where ITS verdict goes: {said}"
    );
}

/// The queue's upkeep asks the daemon's door whether a row's reviewer is still alive, and a Claude
/// reviewer is a PROCESS with no session to be alive in - so without the runs table the row would be
/// re-seated on the very next pass, which is the defect that would make the whole kind useless. The
/// control is in the same test: a row under a child nobody holds IS re-seated in the same pass, so
/// the difference is the runs table and not a door that was never asked.
#[test]
fn a_claude_reviewer_is_not_re_seated_by_the_upkeep() {
    use crate::mergequeue::gate::GreenGate;
    use crate::mergequeue::serve::tests::{Door, env};
    use std::sync::Arc;

    let dir = TempDir::new("claude-upkeep");
    let repo = repo(&dir.0.join("the-repository"));
    let (_wt, base, head) = branch(&repo, "the-work", "b.txt");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    let door = Arc::new(Door::default());
    let mut e = env(Arc::new(GreenGate), door.clone(), "a");
    let owner = e.me.owner.clone();
    // The daemon's own table of runs, for the half of this rule that is about a process.
    let runs = Arc::new(Runs::new());
    Arc::get_mut(&mut e).unwrap().claude = Some(runs.clone());
    for id in ["claude-row", "child-row"] {
        store
            .enqueue_review(
                &NewReview {
                    id: id.into(),
                    repository: repo.display().to_string(),
                    target: "main".into(),
                    kind: ReviewKind::Branch,
                    session_id: CHILD.into(),
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
                    &Move::new(&owner, "claimed")
                        .set("owner", owner.clone())
                        .set("head_sha", head.clone())
                        .set("deadline_ms", now_ms() + 3_600_000),
                    2,
                )
                .unwrap()
        );
    }
    // A real process for one row, and a handle no session of this daemon holds for the other.
    let binary = stub(&dir, "claude", "sleep 60\n");
    let handle = runs
        .start(
            "claude-row",
            &claude::Settings::new(binary),
            "the prompt",
            &dir.0,
            None,
        )
        .expect("a run starts");
    for (id, reviewer) in [
        ("claude-row", handle.clone()),
        ("child-row", "sub-dead".into()),
    ] {
        assert!(
            store
                .move_review(
                    id,
                    ReviewState::Reviewing,
                    ReviewState::Reviewing,
                    &Move::new(&owner, "a reviewer is on it").set("reviewer", reviewer),
                    3,
                )
                .unwrap()
        );
    }
    let key = repo.display().to_string();
    assert!(matches!(
        store
            .acquire_queue_lease(&key, &e.me, crate::mergequeue::gate::now_ms(), 60_000)
            .unwrap(),
        letibot_tokencore::store::LeaseOutcome::Held { .. }
    ));
    let q = crate::mergequeue::serve::RepoQueue::new(store, repo.clone(), e);
    assert_ne!(
        q.pass(&std::sync::atomic::AtomicBool::new(false)).unwrap(),
        crate::mergequeue::serve::Pass::LostLease
    );
    // A second handle on the same file, as a pane's reader has: the queue holds the first.
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");

    let judged = store.review_row("claude-row").unwrap().unwrap();
    assert_eq!(
        judged.state,
        ReviewState::Reviewing,
        "the run is alive, so its row is still the run's: {judged:?}"
    );
    assert_eq!(judged.reviewer.as_deref(), Some(handle.as_str()));
    assert_eq!(judged.attempts, 0, "nothing was charged against it");
    let child = store.review_row("child-row").unwrap().unwrap();
    assert_eq!(
        child.attempts, 1,
        "a child nobody holds is re-seated: {child:?}"
    );
    assert!(child.reviewer.is_none(), "{child:?}");

    // And the process this test lit is put out, so nothing outlives it.
    let now = now_ms();
    runs.keep(&[], now);
    for _ in 0..250 {
        runs.keep(&[], now + 10_000);
        if !runs.owns(&handle) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!runs.owns(&handle), "the stub was ended");
}

/// The argv the stub was called with, as the lines it appended to `argv.txt` in its trees.
fn recorded(clones: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(clones) else {
        return out;
    };
    for e in entries.flatten() {
        if let Ok(text) = std::fs::read_to_string(e.path().join("argv.txt")) {
            out.extend(text.lines().map(str::to_string));
        }
    }
    out
}

/// Wait for a file the stub writes, so a test can say *the run was up* or *the signal arrived*
/// without guessing at a sleep.
fn wait_for(path: &Path) -> bool {
    for _ in 0..250 {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// The verdict a run answers with in `structured_output` is the queue's verdict: the row carries the
/// decision, the reviewer and the head, the evidence is the run's own, and the merge row is born from
/// the same approval a letibot reviewer's `accept` makes.
#[test]
fn a_verdict_in_structured_output_becomes_the_queues_verdict() {
    let dir = TempDir::new("claude-verdict");
    let (store, _repo, head) = claimed(&dir);
    let binary = stub(
        &dir,
        "claude",
        &result(
            "cc-1",
            Some(
                r#"{"decision":"accept","reasons":["the change is the ask"],"files":["b.txt"],"commands":["git diff base...head"]}"#,
            ),
        ),
    );
    let mut f = Fixture::new(&store, settings_with(&binary, 60), &NoChild);
    f.until("the row is judged", |s| {
        s.review_row(ID).unwrap().unwrap().state == ReviewState::Approved
    });
    let row = f.row();
    assert_eq!(row.state, ReviewState::Approved);
    assert_eq!(row.verdict.as_deref(), Some("accept"));
    assert_eq!(row.reviewed_sha.as_deref(), Some(head.as_str()));
    assert!(
        row.verdict_by
            .as_deref()
            .is_some_and(|b| b.starts_with("reviewer:claude-")),
        "the verdict names the run that made it: {:?}",
        row.verdict_by
    );
    assert_eq!(row.reasons, vec!["the change is the ask".to_string()]);
    assert_eq!(row.files, vec!["b.txt".to_string()]);
    assert_eq!(row.commands, vec!["git diff base...head".to_string()]);
    assert!(
        store
            .merge_rows(None)
            .unwrap()
            .iter()
            .any(|m| m.branch == "agent/the-work" && m.approved_sha == head),
        "an accept lands the same way a letibot reviewer's does"
    );
    // And the run's conversation is kept, which is what a re-review resumes.
    let kept = store.claude_review(ID).unwrap().expect("the conversation");
    assert_eq!(kept.session_id.as_deref(), Some("cc-1"));
    assert_eq!(kept.handle, None, "no run is using it any more");
}

/// A run that answers in prose, without `structured_output`, has judged nothing: the row goes back in
/// line with one attempt counted and a failure that names the missing field. It is NEVER a pass.
#[test]
fn a_run_without_structured_output_is_a_failed_review() {
    let dir = TempDir::new("claude-prose");
    let (store, _repo, _head) = claimed(&dir);
    let binary = stub(&dir, "claude", &result("cc-2", None));
    let mut f = Fixture::new(&store, settings_with(&binary, 60), &NoChild);
    f.until("the attempt is counted", |s| {
        s.review_row(ID).unwrap().unwrap().attempts == 1
    });
    let row = f.row();
    assert_eq!(row.state, ReviewState::Waiting, "waiting, not approved");
    assert_eq!(row.verdict, None, "nothing was decided");
    assert_eq!(row.attempts, 1);
    assert!(
        row.failure.contains("structured_output"),
        "the row says which field was missing: {}",
        row.failure
    );
    assert!(
        row.failure.contains("Looks good to me!"),
        "and keeps the run's own answer as the evidence: {}",
        row.failure
    );
}

/// A re-review of the same review resumes the conversation the last run left: `--resume <id>` with
/// the id the store kept, and no resume at all on the first run.
#[test]
fn a_re_review_passes_resume_with_the_stored_id() {
    let dir = TempDir::new("claude-resume");
    let (store, _repo, head) = claimed(&dir);
    let binary = stub(
        &dir,
        "claude",
        &result(
            "cc-1",
            Some(r#"{"decision":"reject","reasons":["not yet"],"files":[],"commands":[]}"#),
        ),
    );
    let mut f = Fixture::new(&store, settings_with(&binary, 60), &NoChild);
    f.until("the first run judged", |s| {
        s.review_row(ID).unwrap().unwrap().state == ReviewState::Rejected
    });
    let argv = recorded(&f.clones());
    assert!(
        !argv.iter().any(|a| a == "--resume"),
        "a review nobody has run before resumes nothing: {argv:?}"
    );
    assert_eq!(
        store
            .claude_review(ID)
            .unwrap()
            .and_then(|c| c.session_id)
            .as_deref(),
        Some("cc-1"),
        "the first run's conversation is what a re-review has to go on"
    );
    // The same review, put back in line at the same head (a person's restart): the second run
    // continues the conversation rather than making the reviewer read the branch from nothing.
    assert!(
        store
            .move_review(
                ID,
                ReviewState::Rejected,
                ReviewState::Waiting,
                &Move::new(ME, "put back in line"),
                3
            )
            .unwrap()
    );
    claim(&store, head);
    f.pass();
    let argv = recorded(&f.clones());
    let at = argv
        .iter()
        .position(|a| a == "--resume")
        .unwrap_or_else(|| panic!("the re-review resumes: {argv:?}"));
    assert_eq!(argv[at + 1], "cc-1", "with the id the first run left");
}

/// The queue's own time limit ends the run with `SIGINT` - not `SIGTERM`, which exits 143 and reads
/// as a crash - and the row says why. The stub traps the signal and writes a marker, so the test
/// knows the signal was `SIGINT` and not the process's own exit.
#[test]
fn the_time_limit_ends_the_run_with_sigint_and_the_row_says_why() {
    let dir = TempDir::new("claude-time-limit");
    let (store, _repo, _head) = claimed(&dir);
    let ready = dir.0.join("READY");
    let got = dir.0.join("GOT-INT");
    let binary = stub(
        &dir,
        "claude",
        &format!(
            "trap 'echo int > {}; exit 0' INT\ntouch {}\nsleep 60\n",
            got.display(),
            ready.display()
        ),
    );
    let mut f = Fixture::new(&store, settings_with(&binary, 60), &NoChild);
    // The run starts and says it is up, so the trap is installed before any signal can arrive.
    f.pass();
    assert!(
        wait_for(&ready),
        "the stub started, or there is nothing under test"
    );
    assert_eq!(
        f.row().state,
        ReviewState::Reviewing,
        "the run is in flight"
    );
    // The row's time is up: the queue's deadline, which `[queue.claude] timeout_seconds` sets.
    assert!(
        store
            .move_review(
                ID,
                ReviewState::Reviewing,
                ReviewState::Reviewing,
                &Move::new(ME, "the limit passes").set("deadline_ms", 1u64),
                4
            )
            .unwrap()
    );
    f.until("the row says the limit passed", |s| {
        s.review_row(ID).unwrap().unwrap().attempts == 1
    });
    let row = f.row();
    assert_eq!(row.state, ReviewState::Waiting);
    assert!(
        row.failure.contains("time limit") && row.failure.contains("SIGINT"),
        "the row says the queue ended it, and how: {}",
        row.failure
    );
    assert!(
        wait_for(&got),
        "the run was ended with SIGINT, so the stub's trap ran - SIGTERM would leave this file \
         unmade and the row saying nothing"
    );
}

/// The reviewer is OFF unless a project configures one: with no `[queue.claude]` the queue starts a
/// session reviewer exactly as it always did, and no process is started at all.
#[test]
fn the_claude_reviewer_is_off_unless_configured() {
    let dir = TempDir::new("claude-off");
    let (store, _repo, _head) = claimed(&dir);
    stub(
        &dir,
        "claude",
        &result("cc-9", Some(r#"{"decision":"accept"}"#)),
    );
    let recorder = Recorder::default();
    let mut f = Fixture::new(&store, QueueSettings::default(), &recorder);
    let served = f.pass();
    assert_eq!(served.started, 1, "a reviewer was started");
    assert_eq!(
        recorder.started.lock().unwrap().len(),
        1,
        "and it is the letibot child, because nothing configured a Claude one"
    );
    assert!(
        recorded(&f.clones()).is_empty(),
        "no stub ran: a binary on this box that nobody named was never started"
    );
    assert_eq!(
        QueueSettings::default().claude,
        None,
        "the settings say it too: the table absent is the letibot reviewer"
    );
}

/// A project that asks for a Claude reviewer on a daemon that holds no runner is refused BY NAME on
/// the row rather than quietly given the other kind: two reviewers that answer differently must not
/// be swapped without somebody being told.
#[test]
fn a_project_asking_for_claude_on_a_daemon_without_one_is_refused_by_name() {
    let dir = TempDir::new("claude-no-runs");
    let (store, _repo, _head) = claimed(&dir);
    let binary = stub(
        &dir,
        "claude",
        &result("cc-9", Some(r#"{"decision":"accept"}"#)),
    );
    let mut f = Fixture::new(&store, settings_with(&binary, 60), &NoChild);
    f.holds_runs = false;
    let served = f.pass();
    assert_eq!(served.started, 0);
    let row = f.row();
    assert_eq!(row.state, ReviewState::Blocked, "{row:?}");
    assert!(
        row.failure.contains("`claude -p`") && row.failure.contains("no runner"),
        "refused by name: {}",
        row.failure
    );
}

/// A daemon's table of runs is what tells a process reviewer from a session one, and it forgets a run
/// only once its process is gone: `keep` stops what nobody polls, and drops it when it is dead.
#[test]
fn the_runs_table_holds_a_process_until_it_is_dead() {
    let dir = TempDir::new("claude-runs");
    let binary = stub(&dir, "claude", "sleep 60\n");
    let runs = Runs::new();
    let handle = runs
        .start(
            "the-review",
            &claude::Settings::new(binary),
            "the prompt",
            &dir.0,
            None,
        )
        .expect("a run starts");
    assert!(runs.owns(&handle), "the table holds it");
    assert_eq!(
        runs.poll(&handle, None, 1),
        claude::Polled::Running,
        "nothing has been judged while it works"
    );
    // Nobody polls it any more: it is stopped with SIGINT, and stays in the table until it is gone.
    runs.keep(&[], 2);
    assert!(runs.owns(&handle), "not forgotten before it is dead");
    let mut gone = false;
    for _ in 0..250 {
        runs.keep(&[], 3);
        if !runs.owns(&handle) {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(gone, "and forgotten once it is");
}
