use super::*;
use letibot_tools::RecordingToolSink;
use letibot_tools::exec::{HostProcesses, ScopeKind, SpawnRequest};
use std::time::Instant;

/// A row the store refuses is said on the session's hub : a job whose row never
/// landed used to be a fact nobody heard, and a `when: job` todo row waiting on it would not be
/// released after a restart. The refusal here is the foreign key: no session row.
#[test]
fn a_row_the_store_refuses_is_said_on_the_hub() {
    let dir = std::env::temp_dir().join(format!("letibot-recorder-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    let hub = Hub::new("s-nobody");
    let rec = JobRecorder::new(path, "s-nobody".into(), Arc::downgrade(&hub));
    rec.start("j1", "asked", 0);
    let said: Vec<String> = hub
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail, .. } if code == "job_not_recorded" => {
                Some(detail.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(said.len(), 1, "the refused row reached no hub: {said:?}");
    assert!(said[0].contains("`j1`"), "{said:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A settlement leaves a row, and a subagent does not. The recorder is the piece with logic
/// in it, and it is reachable without a cgroup host - so this is the half of the contract
/// that runs everywhere.
#[test]
fn a_settlement_is_written_into_the_store_and_a_subagent_is_not() {
    let dir = std::env::temp_dir().join(format!("letibot-recorder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    {
        // The session row first: `job.session_id` is a foreign key and `foreign_keys` is on.
        let s = Store::open(&path).unwrap();
        s.put_session(&letibot_tokencore::store::SessionRecord {
            id: "s-jobs".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    }
    let rec = JobRecorder::new(path.clone(), "s-jobs".into(), Weak::new());
    let done = |kind, job: &str, state: &str| JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind,
        owner: "s-jobs".into(),
        job: job.into(),
        command: "cargo test --release".into(),
        // The row this writes carries no name: `JobRecord` has no such field, and this path is
        // about the durable settlement, not the label.
        slug: None,
        state: state.into(),
        produced: 4_096,
        elapsed_ms: 8_000,
        detail: String::new(),
        memory: None,
    };
    rec.record(&done(BackgroundKind::Job, "j7", "exited 0"));

    let s = Store::open(&path).unwrap();
    let rows = s.jobs("s-jobs").unwrap();
    assert_eq!(rows.len(), 1, "the settlement left no row: {rows:?}");
    assert_eq!(rows[0].handle, "j7");
    assert_eq!(rows[0].state, "exited 0");
    assert_eq!(rows[0].produced, 4_096);
    assert_eq!(rows[0].command, "cargo test --release");
    // `how` is empty, and that MEANS *not recorded*. It is a property of the start and this
    // is the ending; an empty field here is a fact, and a guessed word would not be.
    assert!(rows[0].how.is_empty(), "{rows:?}");

    // A subagent is not a job: a row here would put a child in the jobs pane - the confusion
    // removed from the notice, which this must not put back in the history.
    rec.record(&done(BackgroundKind::Subagent, "s-p-sub-1", "done"));
    assert_eq!(
        s.jobs("s-jobs").unwrap().len(),
        1,
        "a subagent was filed as a job"
    );

    // And the same handle twice is one row - a job has one state at a time.
    rec.record(&done(BackgroundKind::Job, "j7", "signalled 15"));
    let rows = s.jobs("s-jobs").unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a second settlement made a second row: {rows:?}"
    );
    assert_eq!(rows[0].state, "signalled 15");

    // A START and then a SETTLEMENT is ONE row holding both halves. The start knows `how`
    // and not the command, the settlement the reverse, and neither may erase the other. j9's
    // command redirects its output, so the settlement's `redirect` is exercised too.
    rec.start("j9", "you asked for this to run in the background", 1_500);
    let row = s
        .jobs("s-jobs")
        .unwrap()
        .into_iter()
        .find(|r| r.handle == "j9")
        .expect("the start row");
    assert_eq!(
        row.state, "running",
        "a job is a row from the moment it exists"
    );
    assert!(
        row.how.contains("you asked for this"),
        "the start records who backgrounded it: {row:?}"
    );
    assert!(
        row.command.is_empty(),
        "the start cannot know the command — its event carries no arguments: {row:?}"
    );

    let mut finished = done(BackgroundKind::Job, "j9", "exited 0");
    finished.command = "cargo build > /tmp/build.log 2>&1".into();
    rec.record(&finished);
    let row = s
        .jobs("s-jobs")
        .unwrap()
        .into_iter()
        .find(|r| r.handle == "j9")
        .expect("the settled row");
    assert_eq!(
        row.state, "exited 0",
        "the settlement is the ending: {row:?}"
    );
    assert!(
        row.command.contains("cargo build"),
        "the settlement records the command, which only its side has: {row:?}"
    );
    assert!(
        row.how.contains("you asked for this"),
        "and does NOT erase the who the start recorded: {row:?}"
    );
    assert_eq!(
        row.redirect.as_deref(),
        Some("/tmp/build.log"),
        "the redirect is read out of the command, so only the settlement can fill it: {row:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host over a temp root, the way `exec::host`'s own tests build one: the box needs a cgroup v2
/// subtree to delegate.
fn a_host() -> Arc<HostProcesses> {
    let root = std::env::temp_dir().join(format!("letibot-jobwatch-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    Arc::new(HostProcesses::new(&root).expect("this box has a cgroup v2 tree"))
}

fn spawn_a_short_job(host: &HostProcesses, secs: &str) -> JobId {
    host.spawn(&SpawnRequest {
        command: format!("sleep {secs}"),
        // Nobody named this run: the watcher's own fixture.
        slug: None,
        cwd: "/".into(),
        scope: ScopeKind::Session,
        scope_name: None,
        background: true,
        env: vec![],
        // No terminal: these are the substrate's own tests, not an operator's run.
        tty: false,
        memory: None,
    })
    .expect("spawn")
}

/// The one `JobSettled` the hub holds for `job`, if it has arrived yet.
fn settlement_for(hub: &Hub, job: &str) -> Option<(String, u64, u64)> {
    hub.retained().into_iter().find_map(|e| match e.event {
        SessionEvent::JobSettled {
            job: ref j,
            ref state,
            produced,
            elapsed_ms,
        } if j == job => Some((state.clone(), produced, elapsed_ms)),
        _ => None,
    })
}

/// Poll the hub's log for the settlement rather than sleeping a fixed time
/// the watcher publishes the moment the job exits, and the test ends at
/// whichever comes first.
fn wait_for_settlement(hub: &Hub, job: &str) -> (String, u64, u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(hit) = settlement_for(hub, job) {
            return hit;
        }
        assert!(
            Instant::now() < deadline,
            "no JobSettled for {job} within 10s; log: {:?}",
            hub.retained()
                .iter()
                .map(|e| e.event.kind())
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_backgrounded_result_through_the_sink_publishes_the_jobs_settlement() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let hub = Hub::new("s-jobs");
    let host = a_host();
    let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);
    let mut sink = JobWatchSink::new(RecordingToolSink::default(), Some(watchers));

    let id = spawn_a_short_job(&host, "0.3");
    sink.emit(ToolEvent::Finished {
        turn_id: "t1".into(),
        call_id: "c1".into(),
        outcome: ToolOutcome::Backgrounded {
            handle: id.0.clone(),
            ran_for_ms: 0,
            how: letibot_transcript::Backgrounding::Asked,
            next: "job_wait".into(),
        },
        payload_digest: letibot_tools::payload_digest(""),
        inline_bytes: 0,
        full_bytes: 0,
        spill: None,
        repairs: 0,
        edit: None,
    });

    let (state, _produced, elapsed_ms) = wait_for_settlement(&hub, &id.0);
    assert_eq!(state, "exited 0", "the word a listing shows, not ok/error");
    // The job slept ~300ms; the number comes off the job's own stamps, so it is the real runtime
    // and not the watcher's wait time.
    assert!(
        (250..5_000).contains(&elapsed_ms),
        "elapsed_ms {elapsed_ms} is not the job's runtime"
    );
}

#[test]
fn a_job_watched_twice_and_again_after_settlement_publishes_once() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let hub = Hub::new("s-jobs");
    let host = a_host();
    let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);

    let id = spawn_a_short_job(&host, "0.2");
    watchers.watch(id.0.clone());
    // A re-finished call id, or a second head reporting the same job, must not start a second
    // watcher: one job, one settlement.
    watchers.watch(id.0.clone());

    wait_for_settlement(&hub, &id.0);
    // And a watch that arrives after the settlement is a no-op too: a second settlement would be
    // a job that died twice.
    watchers.watch(id.0.clone());
    std::thread::sleep(Duration::from_millis(300));

    let count = hub
        .retained()
        .iter()
        .filter(|e| matches!(&e.event, SessionEvent::JobSettled { job, .. } if job == &id.0))
        .count();
    assert_eq!(count, 1, "one job, one settlement");
}

/// 's question, answered by the one thing that can answer it. `delivering` is what
/// `job_wait` asks before deciding whether to block, so the two readings it must get right
/// are true while a watcher is live for that job and false everywhere else.
#[test]
fn delivering_is_true_only_while_a_watcher_is_live_for_that_job() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let hub = Hub::new("s-jobs");
    let host = a_host();
    let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);

    let id = spawn_a_short_job(&host, "30");

    // Before anything watched it: not delivering, so a wait on it blocks.
    assert!(
        !watchers.delivering(&id.0),
        "a job nobody watches must not claim its answer is on the way"
    );

    watchers.watch(id.0.clone());
    assert!(
        watchers.delivering(&id.0),
        "a live watcher IS the promise: the settlement will be submitted to the \
             model whether it asks or not"
    );
    // Per job, not per session: a second id must not ride on the first's watcher.
    assert!(
        !watchers.delivering("j-not-this-one"),
        "the promise is per job; another id must not inherit it"
    );

    // Let it settle: the watcher publishes and ends, so it is no longer delivering - and the
    // wait on it is answered by the caller's already-finished branch instead.
    host.kill_job(&id).expect("killing the job settles it");
    wait_for_settlement(&hub, &id.0);
    let deadline = Instant::now() + Duration::from_secs(5);
    while watchers.delivering(&id.0) {
        assert!(
            Instant::now() < deadline,
            "the watcher must stop claiming delivery once it has delivered"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !watchers.delivering(&id.0),
        "a settled job is not being delivered any more; it HAS been"
    );
}

/// The arming path, end to end: the sink is what makes `delivering` true. A real session
/// routes every tool result through [`JobWatchSink`], and that is the composition
/// `harness.rs` depends on
#[test]
fn the_sink_arms_the_watcher_and_that_is_what_makes_a_wait_decline() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let hub = Hub::new("s-jobs");
    let host = a_host();
    let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);
    let mut sink = JobWatchSink::new(RecordingToolSink::default(), Some(watchers.clone()));

    let id = spawn_a_short_job(&host, "30");
    // Before any result has passed through the sink: no promise, so a wait blocks.
    assert!(
        !watchers.delivering(&id.0),
        "a job the sink has not seen must not claim its answer is on the way"
    );

    sink.emit(ToolEvent::Finished {
        turn_id: "t1".into(),
        call_id: "c1".into(),
        outcome: ToolOutcome::Backgrounded {
            handle: id.0.clone(),
            ran_for_ms: 0,
            how: letibot_transcript::Backgrounding::Asked,
            next: "job_wait".into(),
        },
        payload_digest: letibot_tools::payload_digest(""),
        inline_bytes: 0,
        full_bytes: 0,
        spill: None,
        repairs: 0,
        edit: None,
    });

    // Synchronously true, with no sleep: the promise exists from the emit.
    assert!(
        watchers.delivering(&id.0),
        "the sink is what arms the watcher, and arming it is what makes \
             `job_wait` decline to block"
    );

    let _ = host.kill_job(&id);
}

/// : the settlement reaches the MODEL, not only the heads. `JobSettled` reaches the pane
/// and nothing read it back into a turn - the operator's complaint. What is asserted is
/// what the event cannot carry
#[test]
fn a_settled_job_is_queued_for_the_model_with_the_command_the_event_omits() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let hub = Hub::new("s-jobs");
    let host = a_host();
    let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);

    let id = spawn_a_short_job(&host, "0.2");
    watchers.watch(id.0.clone());
    wait_for_settlement(&hub, &id.0);

    let done = watchers.take_completions();
    assert_eq!(done.len(), 1, "one job, one completion for the model");
    assert_eq!(done[0].job, id.0);
    assert_eq!(done[0].state, "exited 0");
    assert!(
        done[0].command.contains("sleep 0.2"),
        "the command must reach the model: {:?}",
        done[0].command
    );
    // Taken, not left to arrive again. The harness drains once per wake; a
    // second drain is empty, which is what stops a settlement being delivered as
    // two turns.
    assert!(
        watchers.take_completions().is_empty(),
        "a completion is taken once, not left for the next wake"
    );
}

/// 's floor rule, structurally: a completion never jumps a queued human line. A job ending
/// is not urgent in the operator's sense, so it must not displace a person who pressed
/// enter.
#[test]
fn a_jobs_completion_wakes_the_worker_with_no_command_behind_it_and_after_one_that_is() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

    let r = Registry::new();
    let hub = r
        .create("s-jobs", "", SessionWiring::default())
        .expect("the session registers");
    // `create` rings an open, and an open is drained first by design.
    assert!(matches!(r.next_work(), Some(Work::Open(_))));

    let host = a_host();
    // The watcher is given the daemon's own bell, which is what makes a settlement between turns
    // wake the worker rather than sit until something else asks.
    let watchers = JobWatchers::new(
        &(host.clone() as Arc<dyn ProcessHost>),
        &hub,
        Some(Arc::clone(r.bell())),
    );
    let id = spawn_a_short_job(&host, "0.2");
    watchers.watch(id.0.clone());
    wait_for_settlement(&hub, &id.0);

    // The operator types while the job is ending. Their line is in the hub's queue before
    // the wake is drained; it must come out of `next_work` first.
    let head = hub.attach("tui", "dead", Default::default(), 0);
    hub.submit(
        &head.head_id,
        "c1",
        0,
        letibot_sessionlog::CommandKind::Prompt {
            text: "answer me first".into(),
            attachments: Vec::new(),
        },
    );
    match r.next_work() {
        Some(Work::Command(id, cmd)) => {
            assert_eq!(id, "s-jobs");
            assert!(matches!(
                cmd.kind,
                letibot_sessionlog::CommandKind::Prompt { .. }
            ));
        }
        // `Work` is not `Debug` - it carries a `QueuedCommand` - so the failure names the
        // expectation rather than the value.
        _ => panic!("the operator's line must be served before the job's completion"),
    }
    match r.next_work() {
        Some(Work::Woken(id)) => assert_eq!(id, "s-jobs", "the job's completion wakes it"),
        _ => panic!("the completion must arrive as a wake, after the operator's line"),
    }
}

/// A subagent settles through the job channel, because it IS one. `task` returns the same
/// `Backgrounded` outcome `bash --background` does, so the sink arms a watcher for a
/// subagent's handle too.
#[test]
fn a_subagent_settles_through_the_jobs_own_channel() {
    use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

    let r = Registry::new();
    let hub = r
        .create("s-tasks", "", SessionWiring::default())
        .expect("the session registers");
    assert!(matches!(r.next_work(), Some(Work::Open(_))));

    // A host is still required: a session has one channel and it is built with both waits. A
    // subagent simply never reaches this one.
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let host = a_host();

    let runner = Arc::new(FakeTask::new("s-1-sub-1"));
    let watchers = JobWatchers::new(
        &(host as Arc<dyn ProcessHost>),
        &hub,
        Some(Arc::clone(r.bell())),
    )
    .with_tasks(&(runner.clone() as Arc<dyn TaskRunner>));

    watchers.watch("s-1-sub-1".into());
    // The child answers. Nothing else happens: there is no job by that name.
    runner.finish("the child answered");

    let done = wait_for_completion(&watchers);
    assert_eq!(
        done.kind,
        BackgroundKind::Subagent,
        "one channel, and the field that lets the sentence be true for each"
    );
    assert_eq!(done.state, "done", "the same word the pane's row carries");
    assert!(
        done.detail.contains("the child answered"),
        "the model is handed what the child said, not only that it stopped: {:?}",
        done.detail
    );
    assert!(
        done.command.is_empty(),
        "a subagent runs no command, and that field must not quietly hold an answer"
    );

    assert!(
        matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-tasks"),
        "and it WAKES the worker — a settlement that lands between turns is a \
             condition nobody acts on unless this rings"
    );
    assert!(
        watchers.take_completions().is_empty(),
        "a completion is taken once, not left for the next wake"
    );
}

/// A GRANDCHILD's settlement reaches its PARENT, and the ring names the PARENT. The session
/// that owns the settlement is the session whose bell is rung for it.
#[test]
fn a_grandchilds_settlement_reaches_its_parent_and_rings_its_parent() {
    use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

    let r = Registry::new();
    // The root is the session a daemon drives; the child is the one it does not.
    let root = r
        .create("s-root", "", SessionWiring::default())
        .expect("the root registers");
    let child = r
        .create_under(
            "s-child",
            "",
            SessionWiring::default(),
            Some("s-root".into()),
        )
        .expect("the child registers");
    // Each create rings an Open; drain both so the next ring is the settlement's.
    assert!(matches!(r.next_work(), Some(Work::Open(id)) if id == "s-root"));
    assert!(matches!(r.next_work(), Some(Work::Open(id)) if id == "s-child"));

    // The root's own set - its own session is the one it rings for.
    let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));

    // The child's runner knows the grandchild's handle, and the child's set is built FROM the
    // root's - which is where it learns the ROOT (for an ask) and the level above (for a
    // handed-up settlement).
    let child_runner = Arc::new(FakeTask::new("s-child-sub-1"));
    let child_watch = JobWatchers::watching_tasks(&child, Some(Arc::clone(r.bell())))
        .with_tasks(&(child_runner.clone() as Arc<dyn TaskRunner>))
        .shares_tree(&root_watch);

    // The grandchild settles; the watcher runs in the CHILD's set.
    child_watch.watch("s-child-sub-1".into());
    child_runner.finish("the grandchild answered");

    // The CHILD drains it, through the door a child actually has. Not `take_completions`
    // that between-turns drain belongs to a session a worker drives, and nothing drives a child.
    let done = wait_for_mid_turn_completion(&child_watch);
    assert_eq!(done.job, "s-child-sub-1");
    assert_eq!(done.state, "done");
    assert_eq!(
        done.owner, "s-child",
        "and it is stamped with the session that started it, which is the one reading it"
    );
    assert!(
        !done.detail.is_empty(),
        "the row carries what the grandchild said"
    );

    // And the PARENT's drain is empty - that is the defect, one assert away.
    assert!(
        root_watch.take_completions().is_empty(),
        "a grandchild's settlement is its parent's to be told, not the main session's"
    );
    assert!(
        root_watch.take_mid_turn_completions().is_empty(),
        "and a root has no mid-turn door at all: R7's wake is its only one"
    );

    // And the bell was rung for the session that owns it - the CHILD. A ring for the root
    // would be a wake for a session with nothing to do: it would drain an empty queue and run no
    // turn, while the daemon hands the wake to the thread that runs the child.
    assert!(
        matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-child"),
        "a grandchild's settlement must ring the session that owns it, which is its PARENT"
    );
}

/// **A child's later turn is queued on its PARENT and rung there** - `settle_up`, which is the one
/// door a child has for a settlement its parent's watcher has already stopped waiting on.
#[test]
fn a_later_turn_is_queued_on_the_level_above_and_rings_it() {
    use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

    let r = Registry::new();
    let root = r
        .create("s-root", "", SessionWiring::default())
        .expect("the root registers");
    let child = r
        .create_under(
            "s-child",
            "",
            SessionWiring::default(),
            Some("s-root".into()),
        )
        .expect("the child registers");
    // Each create rings an Open; drain both so the next ring is this one.
    assert!(matches!(r.next_work(), Some(Work::Open(_))));
    assert!(matches!(r.next_work(), Some(Work::Open(_))));

    let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));
    let child_watch =
        JobWatchers::watching_tasks(&child, Some(Arc::clone(r.bell()))).shares_tree(&root_watch);
    assert!(
        child_watch.settle_up(JobCompletion {
            arrived: std::time::SystemTime::UNIX_EPOCH,
            later: true,
            kind: BackgroundKind::Subagent,
            owner: "s-root".into(),
            job: "s-child".into(),
            command: String::new(),
            slug: None,
            state: "done".into(),
            produced: 0,
            elapsed_ms: 0,
            detail: "the second half".into(),
            memory: None,
        }),
        "a child has a level above it, and that is where a later turn settles"
    );

    let up = root_watch.take_completions();
    assert_eq!(up.len(), 1, "one settlement, on the parent's queue: {up:?}");
    assert_eq!(up[0].owner, "s-root", "and it is the parent's own news");
    assert!(
        up[0].later,
        "said to be a later turn, not the child's answer"
    );
    assert!(
        child_watch.take_completions().is_empty(),
        "the child's own queue is not where a parent hears this"
    );
    assert!(
        matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-root"),
        "and the PARENT is rung: a settlement nobody is woken for sits until something else asks"
    );

    // A root has no level above it, so there is nowhere to put one and the caller is told so.
    assert!(
        !root_watch.settle_up(JobCompletion {
            arrived: std::time::SystemTime::UNIX_EPOCH,
            later: true,
            kind: BackgroundKind::Subagent,
            owner: "s-nobody".into(),
            job: "s-x".into(),
            command: String::new(),
            slug: None,
            state: "done".into(),
            produced: 0,
            elapsed_ms: 0,
            detail: String::new(),
            memory: None,
        }),
        "a root is the top of its tree and nothing is handed up from it"
    );
}

/// **A settlement handed up from below a session that has itself stopped goes to the level above
/// that.** A stopped set has handed its own queue up, so a settlement pushed into it afterwards sits
/// where nothing will ever drain it - which is the one thing a hand-up exists to prevent.
#[test]
fn a_settlement_handed_up_past_a_stopped_level_reaches_the_one_above_it() {
    use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

    let r = Registry::new();
    let root = r
        .create("s-root", "", SessionWiring::default())
        .expect("the root registers");
    let child = r
        .create_under(
            "s-child",
            "",
            SessionWiring::default(),
            Some("s-root".into()),
        )
        .expect("the child registers");
    let grandchild = r
        .create_under(
            "s-grandchild",
            "",
            SessionWiring::default(),
            Some("s-child".into()),
        )
        .expect("the grandchild registers");
    // Each create rings an Open; drain all three so the next ring is this one.
    for _ in 0..3 {
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
    }

    let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));
    let child_watch =
        JobWatchers::watching_tasks(&child, Some(Arc::clone(r.bell()))).shares_tree(&root_watch);
    let grandchild_watch = JobWatchers::watching_tasks(&grandchild, Some(Arc::clone(r.bell())))
        .shares_tree(&child_watch);

    // The middle session closes, which hands its queue up to the root and leaves nothing to drain
    // what arrives after it.
    child_watch.stop();

    assert!(
        grandchild_watch.settle_up(JobCompletion {
            arrived: std::time::SystemTime::UNIX_EPOCH,
            later: true,
            kind: BackgroundKind::Subagent,
            owner: "s-child".into(),
            job: "s-grandchild".into(),
            command: String::new(),
            slug: None,
            state: "done".into(),
            produced: 0,
            elapsed_ms: 0,
            detail: "the second half".into(),
            memory: None,
        }),
        "a grandchild has a level above it"
    );
    assert!(
        child_watch.take_completions().is_empty(),
        "the stopped session's queue is the one nothing drains"
    );
    let up = root_watch.take_completions();
    assert_eq!(
        up.len(),
        1,
        "the settlement belongs to the level that is still running: {up:?}"
    );
    assert!(
        matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-root"),
        "and the session it reached is the one woken"
    );
}

/// A job a subagent started is the SUBAGENT's to be told, and the parent never sees it.
/// Measured twice from the parent's own conversation: a `sleep 25
#[test]
fn a_childs_job_completion_is_the_childs_and_never_the_parents() {
    use letibot_sessionlog::registry::{Registry, SessionWiring, Work};
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };

    let r = Registry::new();
    let root = r
        .create("s-root", "", SessionWiring::default())
        .expect("the root registers");
    let child = r
        .create_under(
            "s-child",
            "",
            SessionWiring::default(),
            Some("s-root".into()),
        )
        .expect("the child registers");
    assert!(matches!(r.next_work(), Some(Work::Open(_))));
    assert!(matches!(r.next_work(), Some(Work::Open(_))));

    let host = a_host();
    let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));
    // The child's set keeps its own queue and shares only the tree's bookkeeping, the level
    // above and the root - see `JobWatchers::shares_tree`.
    let child_watch = JobWatchers::new(
        &(host.clone() as Arc<dyn ProcessHost>),
        &child,
        Some(Arc::clone(r.bell())),
    )
    .shares_tree(&root_watch);

    // 1. The child that backgrounded it is the one told.
    let id = spawn_a_short_job(&host, "0.2");
    child_watch.watch(id.0.clone());
    let done = wait_for_mid_turn_completion(&child_watch);
    assert_eq!(done.job, id.0);
    assert_eq!(
        done.owner, "s-child",
        "stamped with the session whose own action backgrounded it"
    );
    assert!(
        done.command.contains("sleep 0.2"),
        "and with the command, which is what makes the notice actionable: {:?}",
        done.command
    );

    // 2. The parent is not told while the child is there.
    assert!(
        root_watch.take_completions().is_empty(),
        "the main session must not be handed a job its own host has never heard of"
    );

    // And the ring is the OWNER's, not the root's: a ring for a session with nothing in its
    // queue is a wake that runs no turn.
    assert!(
        matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-child"),
        "the ring goes to the session that owns the settlement — the parent, not the root"
    );

    // 3. And what a stopped child could not drain goes UP, saying whose it was. From here
    // nothing reads the child's queue, so a job that settles now is the parent's to be told - as
    // somebody else's - rather than a settlement nobody reads.
    child_watch.stop();
    let after = spawn_a_short_job(&host, "0.2");
    child_watch.watch(after.0.clone());
    let handed = wait_for_completion(&root_watch);
    assert_eq!(handed.job, after.0);
    assert_eq!(
        handed.owner, "s-child",
        "handed up, and still naming its owner — `completion_notices` is what turns this \
             into *a session below this one started*, not *you backgrounded*"
    );
    // And the ring names the session it was handed TO - the level above, which holds it now.
    // A ring for the stopped child would be a wake for a session with nothing left to drain.
    assert!(
        matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-root"),
        "a handed-up settlement rings the level above, not the session that stopped"
    );
}

/// A settlement reaches the session that started it and not its sibling. The *"and to
/// nobody else"* half: two children of one root, each watching its own subagent and neither
/// can be told the other's news.
#[test]
fn a_settlement_reaches_the_session_that_started_it_and_not_its_sibling() {
    use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

    let r = Registry::new();
    let root = r
        .create("s-root", "", SessionWiring::default())
        .expect("the root registers");
    let a = r
        .create_under("s-a", "", SessionWiring::default(), Some("s-root".into()))
        .expect("the first child registers");
    let b = r
        .create_under("s-b", "", SessionWiring::default(), Some("s-root".into()))
        .expect("the second child registers");
    for _ in 0..3 {
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
    }

    let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));
    let a_runner = Arc::new(FakeTask::new("a-sub-1"));
    let a_watch = JobWatchers::watching_tasks(&a, Some(Arc::clone(r.bell())))
        .with_tasks(&(a_runner.clone() as Arc<dyn TaskRunner>))
        .shares_tree(&root_watch);
    let b_runner = Arc::new(FakeTask::new("b-sub-1"));
    let b_watch = JobWatchers::watching_tasks(&b, Some(Arc::clone(r.bell())))
        .with_tasks(&(b_runner.clone() as Arc<dyn TaskRunner>))
        .shares_tree(&root_watch);

    a_watch.watch("a-sub-1".into());
    a_runner.finish("from a");
    let a_done = wait_for_mid_turn_completion(&a_watch);
    assert_eq!(a_done.job, "a-sub-1");
    assert_eq!(a_done.owner, "s-a");
    assert!(
        b_watch.take_completions().is_empty(),
        "a sibling's settlement must not surface in this session"
    );
    assert!(
        root_watch.take_completions().is_empty(),
        "nor in the parent's, while the sibling that started it is still there"
    );

    // The other child's own, the same two facts the other way round.
    b_watch.watch("b-sub-1".into());
    b_runner.finish("from b");
    let b_done = wait_for_mid_turn_completion(&b_watch);
    assert_eq!(b_done.job, "b-sub-1");
    assert_eq!(b_done.owner, "s-b");
    assert!(a_watch.take_completions().is_empty(), "still not across");
    assert!(
        root_watch.take_completions().is_empty(),
        "and still not up, while its owner is running"
    );

    // The child's turn ends, and one more of its children settles afterwards: up, with its
    // owner's name on it, and still not across to the sibling.
    b_watch.stop();
    let late_runner = Arc::new(FakeTask::new("b-sub-2"));
    let b_after = b_watch
        .clone()
        .with_tasks(&(late_runner.clone() as Arc<dyn TaskRunner>));
    b_after.watch("b-sub-2".into());
    late_runner.finish("after b stopped");
    let handed = wait_for_completion(&root_watch);
    assert_eq!(handed.job, "b-sub-2");
    assert_eq!(handed.owner, "s-b");
    assert!(
        a_watch.take_completions().is_empty(),
        "a stopped sibling's settlement goes UP, never across: a sibling cannot act on \
             a handle it does not hold"
    );
}

/// A child's turn ending does not silence its tree's watchers. [`JobWatchers::shares_tree`]
/// shared the `stop` flag along with the queue, on the reasoning that *"a tree closes
/// together"*. It does not
#[test]
fn a_childs_close_does_not_stop_its_trees_watchers() {
    use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

    let r = Registry::new();
    let root = r
        .create("s-root", "", SessionWiring::default())
        .expect("the root registers");
    let child = r
        .create_under(
            "s-child",
            "",
            SessionWiring::default(),
            Some("s-root".into()),
        )
        .expect("the child registers");
    assert!(matches!(r.next_work(), Some(Work::Open(_))));
    assert!(matches!(r.next_work(), Some(Work::Open(_))));

    let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));
    let child_watch =
        JobWatchers::watching_tasks(&child, Some(Arc::clone(r.bell()))).shares_tree(&root_watch);
    assert!(
        !root_watch.stopped() && !child_watch.stopped(),
        "neither set is stopped to begin with"
    );

    // The child's turn ends and it releases its substrate - which is the moment the tree's job
    // notices used to die.
    child_watch.stop();
    assert!(child_watch.stopped(), "the child's own set is stopped");
    assert!(
        !root_watch.stopped(),
        "a child's turn ending must not stop the ROOT's watchers: that is every job \
             notice the root was going to get, and it is what the operator saw stop"
    );
}

/// A runner that knows exactly one handle and answers when the test says so.
struct FakeTask {
    handle: String,
    answer: Mutex<Option<String>>,
    settled: std::sync::Condvar,
}

impl FakeTask {
    fn new(handle: &str) -> Self {
        FakeTask {
            handle: handle.to_string(),
            answer: Mutex::new(None),
            settled: std::sync::Condvar::new(),
        }
    }

    fn finish(&self, said: &str) {
        *self.answer.lock().expect("fake task") = Some(said.to_string());
        self.settled.notify_all();
    }
}

impl TaskRunner for FakeTask {
    fn start(
        &self,
        _prompt: &str,
        _spec: &letibot_tools::builtins::task::TaskSpec,
    ) -> Result<String, String> {
        Err("this runner starts nothing".into())
    }

    /// `Unknown` for everything but its one handle, which is the answer that tells
    /// `JobWatchers::watch` a handle is not a subagent.
    fn collect(&self, handle: &str, timeout: Duration) -> TaskStatus {
        if handle != self.handle {
            return TaskStatus::Unknown;
        }
        let g = self.answer.lock().expect("fake task");
        let (g, _) = self
            .settled
            .wait_timeout_while(g, timeout, |a| a.is_none())
            .expect("fake task");
        match &*g {
            Some(a) => TaskStatus::Done {
                answer: a.clone(),
                child: None,
            },
            None => TaskStatus::Running {
                note: None,
                child: None,
            },
        }
    }
}

/// The one completion, or a deadline. A poll rather than a `job_wait`: what is waited for is
/// another thread's queue push, and blocking on a condvar this file does not own would be testing
/// its own sleep.
fn wait_for_completion(w: &JobWatchers) -> JobCompletion {
    wait_for(w, |w| w.take_completions())
}

/// The child's own door - the same wait, drained the way a CHILD's round boundary drains.
/// Which door a settlement comes through is the whole of the routing question, so the two are
/// asserted through the two functions rather than through one of them twice.
fn wait_for_mid_turn_completion(w: &JobWatchers) -> JobCompletion {
    wait_for(w, |w| w.take_mid_turn_completions())
}

fn wait_for(w: &JobWatchers, drain: fn(&JobWatchers) -> Vec<JobCompletion>) -> JobCompletion {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(c) = drain(w).into_iter().next() {
            return c;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no completion within 10s — nothing queued it, or it is on a queue this \
                 session does not drain"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A background job's PROCESS is written down, and its ending moves it - both halves
/// provable without a delegated cgroup, because neither needs a process this daemon
/// started
#[test]
fn a_jobs_process_record_reads_running_then_done() {
    let dir = std::env::temp_dir().join(format!("letibot-recorder-proc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    {
        let s = Store::open(&path).unwrap();
        s.put_session(&letibot_tokencore::store::SessionRecord {
            id: "s-jobs".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    }

    let hub = Hub::new("s-jobs");
    let rec = JobRecorder::new(path.clone(), "s-jobs".into(), Arc::downgrade(&hub));
    let scope = letibot_tools::exec::ScopeId {
        kind: ScopeKind::Session,
        name: "s-jobs".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/letibot/session.s-jobs"),
    };
    let view = letibot_tools::exec::JobView {
        memory_cap: letibot_tools::exec::MemoryCap::NotAsked,
        memory: None,
        id: letibot_tools::exec::JobId("j42".into()),
        command: "cargo test --offline --workspace".into(),
        slug: Some("workspace-tests".into()),
        scope: scope.clone(),
        owner: scope,
        cwd: "/w".into(),
        pid: std::process::id(),
        background: None,
        state: letibot_tools::exec::JobState::Running,
        elapsed: std::time::Duration::from_secs(1),
        ran_for: None,
        produced: 0,
        since_last_output: None,
        output_file: None,
    };

    rec.process(&view);
    // A record that did not land is SAID, not silent - the hub is where `not_recorded` puts
    // it, and a test that read the store without asking would report a missing row as a missing
    // feature.
    let refused: Vec<String> = hub
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { detail, .. } => Some(detail.clone()),
            _ => None,
        })
        .collect();
    assert!(refused.is_empty(), "the record was refused: {refused:?}");
    let s = Store::open(&path).unwrap();
    let got = s.process("s-jobs", "j42").unwrap().expect("the record");
    assert_eq!(got.state, letibot_tokencore::store::ProcState::Running);
    assert_eq!(got.pid, std::process::id());
    assert_eq!(got.command, "cargo test --offline --workspace");
    assert_eq!(got.cwd, "/w");
    assert_eq!(
        got.identity,
        crate::supervise::identity_of(std::process::id()).expect("this process has an identity"),
        "the identity is which process has the pid, not the pid"
    );
    assert!(
        got.host.is_none(),
        "a job on this box names no host: {got:?}"
    );
    assert_eq!(
        s.open_processes().unwrap().len(),
        1,
        "an open record, which is what a restart finds"
    );

    // The ending. `exited 3` is `done` with the code, because the code is the command's own
    // answer; a signal or a kill is `stopped`, which is a different thing to have happened.
    let done = JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind: BackgroundKind::Job,
        owner: "s-jobs".into(),
        job: "j42".into(),
        command: "cargo test --offline --workspace".into(),
        slug: Some("workspace-tests".into()),
        state: "exited 3".into(),
        produced: 1_024,
        elapsed_ms: 9_000,
        detail: String::new(),
        memory: None,
    };
    rec.record(&done);
    let got = s.process("s-jobs", "j42").unwrap().expect("the record");
    assert_eq!(
        (got.state, got.exit),
        (letibot_tokencore::store::ProcState::Done, Some(3)),
        "the settlement is the record's ending too"
    );
    assert_eq!(got.check_how.as_deref(), Some("settle"));
    assert!(
        s.open_processes().unwrap().is_empty(),
        "a done record is not one the supervisor still looks after"
    );

    // And a kill is NOT an exit status: `stopped`, with no code to report. This half is the
    // HOST's, so the other arm of the same function is exercised too
    let remote =
        JobRecorder::new(path.clone(), "s-jobs".into(), Weak::new()).on(Some("lab2x1".into()));
    let mut elsewhere = view.clone();
    elsewhere.id = letibot_tools::exec::JobId("j43".into());
    elsewhere.pid = 424_242;
    remote.process(&elsewhere);
    let got = s.process("s-jobs", "j43").unwrap().expect("the record");
    assert_eq!(got.host.as_deref(), Some("lab2x1"));
    assert_eq!(
        got.identity, "cmd:cargo test --offline --workspace",
        "a host's job is identified by the command line the far side greps for"
    );
    let mut killed = done.clone();
    killed.job = "j43".into();
    killed.state = "killed by job_kill".into();
    remote.record(&killed);
    let got = s.process("s-jobs", "j43").unwrap().expect("the record");
    assert_eq!(
        (got.state, got.exit),
        (letibot_tokencore::store::ProcState::Stopped, None)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------- a job of jobs (FB)

use letibot_tools::exec::Aggregate as _;
use std::sync::Condvar;

/// A job of jobs the test settles by hand - an [`letibot_tools::exec::Aggregate`] with no
/// branches behind it. Deliberately not a process
struct Table {
    state: Mutex<Option<JobState>>,
    rung: Condvar,
    report: Mutex<String>,
}

impl Table {
    fn new(report: &str) -> Table {
        Table {
            state: Mutex::new(None),
            rung: Condvar::new(),
            report: Mutex::new(report.to_string()),
        }
    }

    /// Settle it the way a fanout's last branch does.
    fn finish(&self, state: JobState) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
        self.rung.notify_all();
    }
}

impl letibot_tools::exec::Aggregate for Table {
    fn command(&self) -> String {
        "fanout: 2 branch(es) on fake1, fake2".into()
    }

    fn slug(&self) -> Option<String> {
        Some("fanout".into())
    }

    fn state(&self) -> JobState {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or(JobState::Running)
    }

    fn elapsed(&self) -> Duration {
        Duration::from_millis(1_200)
    }

    fn ran_for(&self) -> Option<Duration> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|_| Duration::from_millis(1_200))
    }

    fn produced(&self) -> u64 {
        self.report.lock().unwrap_or_else(|e| e.into_inner()).len() as u64
    }

    fn since_last_output(&self) -> Option<Duration> {
        Some(Duration::from_millis(1_200))
    }

    /// The journal is the report here - what matters is that `output` answers at all, which is
    /// what `job_output` reads.
    fn output(&self, from: u64, limit: usize) -> letibot_tools::exec::OutputSlice {
        let all = self
            .report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .into_bytes();
        let bytes: Vec<u8> = all
            .iter()
            .skip(from as usize)
            .take(limit)
            .copied()
            .collect();
        let to = from + bytes.len() as u64;
        let produced = all.len() as u64;
        slice_of(bytes, from, to, produced, produced)
    }

    fn wait(&self, within: Duration) -> Option<JobState> {
        let began = Instant::now();
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(state) = &*g {
                return Some(state.clone());
            }
            let left = within.saturating_sub(began.elapsed());
            if left.is_zero() {
                return None;
            }
            let (held, _) = self
                .rung
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner());
            g = held;
        }
    }

    fn kill(&self, _by: &str) -> Vec<letibot_tools::exec::Reaped> {
        // Nothing to end: this fake starts nothing. `job_kill` on a real one is measured in
        // `crates/tools/tests/fanout.rs`, where there are branches to stop.
        Vec::new()
    }

    fn report(&self) -> String {
        self.report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// A slice of a fake's whole output, spelled once because [`letibot_tools::exec::OutputSlice`] has
/// six fields and a reader should not have to check two of them against each other.
fn slice_of(
    bytes: Vec<u8>,
    from: u64,
    to: u64,
    produced: u64,
    retained: u64,
) -> letibot_tools::exec::OutputSlice {
    letibot_tools::exec::OutputSlice {
        bytes,
        from,
        to,
        produced,
        dropped: 0,
        retained,
    }
}

/// A job of jobs settles through the job channel, and its table is what the model is woken
/// with. Three hops, and the third is the one this slice adds: the session says what it is
/// waiting on while the thing runs
#[test]
fn a_job_of_jobs_settles_through_the_jobs_channel_and_carries_its_table() {
    let root = std::env::temp_dir().join(format!("letibot-fanout-watch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // A host that starts nothing, because a job of jobs starts nothing here: `hold` is a map
    // and every branch behind the handle is a job of another machine's table.
    let host = Arc::new(HostProcesses::with_tree(
        &root,
        Box::new(letibot_tools::exec::NoScopes::new(
            "a job of jobs is a table of one handle; no process of this box is involved",
        )),
    ));
    let hub = Hub::new("s-jobs");
    let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);
    let table = Arc::new(Table::new(
        "fanout · 2 planned, 2 answered · 1.2s · work dir `fanout-c1` on each host\n  \
         fake1 on fake1 · exited 0 · 0.3s\n  fake2 on fake2 · exited 0 · 0.9s\n\
         every cell exited 0",
    ));
    let id = JobId::next();
    host.hold(
        &id,
        Arc::clone(&table) as Arc<dyn letibot_tools::exec::Aggregate>,
    )
    .expect("a host holds a job of jobs");
    // The row a listing draws: a name, a command that says what it is, and no process.
    let view = host.job(&id).expect("the handle is in the table");
    assert_eq!(view.slug.as_deref(), Some("fanout"));
    assert_eq!(view.pid, 0, "no process of its own");
    assert!(view.state.is_running(), "{view:?}");

    // 1. What the session is waiting on, while it runs.
    watchers.watch(id.0.clone());
    assert_eq!(
        watchers.watching_now(),
        vec![(id.0.clone(), BackgroundKind::Job, Some("fanout".into()))],
        "the activity names the job it is waiting on, by handle and by name"
    );
    assert!(
        watchers.take_completions().is_empty(),
        "nothing has ended, so nothing is queued"
    );

    // 2 and 3. It settles, and the settlement carries the table.
    table.finish(JobState::Exited { code: 0 });
    let (word, produced, elapsed_ms) = wait_for_settlement(&hub, &id.0);
    assert_eq!(word, "exited 0");
    assert_eq!(produced, table.produced(), "the numbers are the view's");
    assert!((1_000..2_000).contains(&elapsed_ms), "{elapsed_ms}");
    let done = wait_for_completion(&watchers);
    assert_eq!(done.job, id.0);
    assert_eq!(done.slug.as_deref(), Some("fanout"));
    assert_eq!(done.state, "exited 0");
    assert!(
        done.detail.contains("fanout · 2 planned, 2 answered"),
        "the settlement carries the per-host table: {done:?}"
    );
    assert!(
        done.detail.contains("fake2 on fake2 · exited 0"),
        "every row is in it: {done:?}"
    );
    // And the wait is over: the handle is no longer something the session waits on, which is
    // what lets `wait` say the turn is free to end.
    let ended = Instant::now() + Duration::from_secs(5);
    while !watchers.watching_now().is_empty() && Instant::now() < ended {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        watchers.watching_now().is_empty(),
        "a settled job is not something the session is still waiting on"
    );
    let _ = std::fs::remove_dir_all(&root);
}
