use super::*;
use letibot_tools::RecordingToolSink;
use letibot_tools::exec::{HostProcesses, ScopeKind, SpawnRequest};
use std::time::Instant;

/// **A row the store refuses is said on the session's hub** — R67 item 15: `put_job` was a
/// `let _`, so a job whose row never landed was a fact nobody heard, and a `when: job` todo row
/// waiting on it would not be released after a restart. The refusal here is the foreign key:
/// no session row, so `job.session_id` names nothing.
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

/// **A settlement leaves a row, and a subagent does not.**
///
/// The recorder is the piece with logic in it — a filter, a lazy connection, an upsert — and
/// it is reachable without a cgroup host, which the spawning tests below need. So this is the
/// half of the contract that runs everywhere.
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
        kind,
        owner: "s-jobs".into(),
        job: job.into(),
        command: "cargo test --release".into(),
        // The row this writes carries no name — `JobRecord` has no such field and this
        // path is about the durable settlement, not the label.
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
    // **`how` is empty, and that MEANS *not recorded*.** It is a property of the start
    // (`asked`, `operator`, `promoted`) and this is the ending; the start's write is the
    // other half. An empty field here is a fact, and a guessed word would not be.
    assert!(rows[0].how.is_empty(), "{rows:?}");

    // **A subagent is not a job.** `task`'s handle is a session id, `job_list` has never
    // heard of it, and a row here would put a child in the jobs pane — the confusion R58
    // removed from the notice, which this must not put back in the history.
    rec.record(&done(BackgroundKind::Subagent, "s-p-sub-1", "done"));
    assert_eq!(
        s.jobs("s-jobs").unwrap().len(),
        1,
        "a subagent was filed as a job"
    );

    // **And the same handle twice is one row** — the upsert, through this path rather than
    // through the store's own test: a job has one state at a time.
    rec.record(&done(BackgroundKind::Job, "j7", "signalled 15"));
    let rows = s.jobs("s-jobs").unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a second settlement made a second row: {rows:?}"
    );
    assert_eq!(rows[0].state, "signalled 15");

    // **A START and then a SETTLEMENT is ONE row holding both halves.** The start knows
    // `how` and not the command; the settlement knows the command and not `how` — and neither
    // may erase the other, or the row could never hold both. j9's command redirects its
    // output, so the settlement's `redirect` is exercised in the same breath.
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

/// A host over a temp root, the way `exec::host`'s own tests build one. The
/// box needs a cgroup v2 subtree to delegate; every host-spawning test in
/// tools already assumes the same.
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

/// Poll the hub's log for the settlement rather than sleeping a fixed time:
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
    // The job slept ~300ms; the number comes off the job's own stamps, so it
    // is the real runtime and not the watcher's wait time.
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
    // A re-finished call id, or a second head reporting the same job, must
    // not start a second watcher: one job, one settlement.
    watchers.watch(id.0.clone());

    wait_for_settlement(&hub, &id.0);
    // And a watch that arrives after the settlement is a no-op too — the
    // settlement is already on the wire and a second one would be a job
    // that died twice.
    watchers.watch(id.0.clone());
    std::thread::sleep(Duration::from_millis(300));

    let count = hub
        .retained()
        .iter()
        .filter(|e| matches!(&e.event, SessionEvent::JobSettled { job, .. } if job == &id.0))
        .count();
    assert_eq!(count, 1, "one job, one settlement");
}

/// **R23's question, answered by the one thing that can answer it.**
///
/// `delivering` is what `job_wait` asks before it decides whether to block, so
/// the two readings it must get right are: **true** while a watcher is live for
/// that job — the promise is real and the wait would be waiting for something
/// already in flight — and **false** everywhere else. The false cases are the
/// whole of "R23 removes one case and only that one": a job this session never
/// watched keeps blocking, and so does a job whose settlement has already been
/// published.
///
/// The settled case is the subtle one and is asserted separately from the
/// never-watched one on purpose. Both are `false`, but for different reasons and
/// with different consequences: for a settled job `job_wait`'s own "had already
/// finished" branch answers, which is the honest sentence — *nothing was waited
/// for* — while for an unwatched job the wait really does block, which is what
/// the operator's "a deliberate block before a dependent step" depends on.
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
    // Per job, not per session. The closure `job_wait` is given takes an id for
    // exactly this reason, and a second id must not ride on the first's watcher.
    assert!(
        !watchers.delivering("j-not-this-one"),
        "the promise is per job; another id must not inherit it"
    );

    // Let it settle: the watcher publishes and ends, so it is no longer
    // delivering — and the wait on it is answered by the caller's own
    // already-finished branch instead.
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

/// **The arming path, end to end: the sink is what makes `delivering` true.**
///
/// The test above arms the watcher with a direct `watch()` call, which is not how
/// a session arms it. A real session routes every tool result through
/// [`JobWatchSink`], and *that* is the composition `harness.rs` depends on when it
/// wires `job_wait`'s question to the same watcher set: the sink sees the
/// `Backgrounded` outcome, arms a watcher, and the moment it does, the promise is
/// real and `job_wait` must stop blocking.
///
/// Asserted in both directions because the `false` half is the one that keeps R23
/// from being larger than it is: a job no `Backgrounded` result has passed through
/// is a job with no promise, and a wait on it blocks.
///
/// **No race.** `watch()` inserts into `watching` synchronously, before it spawns
/// the thread, so `delivering` is true by the time the sink's `emit` returns — which
/// is before the turn that emitted it has ended, and therefore necessarily before
/// the model's next call. A test on a clock would be testing the thread scheduler.
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

/// **R7: the settlement reaches the MODEL, not only the heads.**
///
/// `JobSettled` has gone to the hub since protocol 14 and the jobs pane folds it,
/// so every head knows. Nothing read it back into a turn, so the model's only route
/// to a result was `job_wait` — which is the operator's complaint, and this is the
/// hop that answers it. What is asserted is the two things the event cannot carry:
/// that a completion is queued for the harness at all, and that it carries the
/// **command** (read off the live view, which the durable event does not have and
/// which is gone by the time a settlement is delivered).
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
    // **Taken, not left to arrive again.** The harness drains once per wake; a
    // second drain is empty, which is what stops a settlement being delivered as
    // two turns.
    assert!(
        watchers.take_completions().is_empty(),
        "a completion is taken once, not left for the next wake"
    );
}

/// **R7's floor rule, structurally: a completion never jumps a queued human line.**
///
/// A job ending is not urgent in the operator's sense — nothing is waiting on it but
/// the model — so it must not displace a person who pressed enter. That is not a
/// check anywhere in this file; it is `Bell::next_any`'s order, where commands drain
/// before wakes. Asserted here for the wake a job rings, in the shape the registry's
/// own `a_wake_is_served_after_every_queued_command` asserts for a monitor's.
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
    // The watcher is given the daemon's own bell, which is what makes a settlement
    // between turns wake the worker rather than sit until something else asks.
    let watchers = JobWatchers::new(
        &(host.clone() as Arc<dyn ProcessHost>),
        &hub,
        Some(Arc::clone(r.bell())),
    );
    let id = spawn_a_short_job(&host, "0.2");
    watchers.watch(id.0.clone());
    wait_for_settlement(&hub, &id.0);

    // **The operator types while the job is ending.** Their line is in the hub's
    // queue before the wake is drained; it must come out of `next_work` first.
    let head = hub.attach("tui", "dead", Default::default(), 0);
    hub.submit(
        &head.head_id,
        "c1",
        0,
        letibot_sessionlog::CommandKind::Prompt {
            text: "answer me first".into(),
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
        // `Work` is not `Debug` — it carries a `QueuedCommand` — so the failure
        // names the expectation rather than the value.
        _ => panic!("the operator's line must be served before the job's completion"),
    }
    match r.next_work() {
        Some(Work::Woken(id)) => assert_eq!(id, "s-jobs", "the job's completion wakes it"),
        _ => panic!("the completion must arrive as a wake, after the operator's line"),
    }
}

/// **A subagent settles through the job channel, because it IS one.**
///
/// The operator's ruling: *"i think it should arrive the same way backgrounded jobs
/// complete. in a way agent is a background job."* `task` already returns the same
/// `Backgrounded` outcome `bash --background` does, so the sink above already arms a
/// watcher for a subagent's handle. Before this, that watcher called
/// `ProcessHost::wait_job` on a name the host had never heard: the host answers
/// `NeverStarted`, which `watch_one` reads as *"a boot window and not a finish"*, and
/// the thread then re-asked every `WATCH_CHUNK` until the session closed. So **the
/// completion was never queued, the bell was never rung, and a thread was leaked per
/// `task` call for the session's life** — the parent turn ended, the subagent
/// finished, and the one process that could act on it was never told.
///
/// This asserts the two things that were missing and the wake that carries them. On
/// the code before the dispatch it does not merely fail: it times out with an empty
/// queue, because nothing in the old path could ever put anything in it.
#[test]
fn a_subagent_settles_through_the_jobs_own_channel() {
    use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

    let r = Registry::new();
    let hub = r
        .create("s-tasks", "", SessionWiring::default())
        .expect("the session registers");
    assert!(matches!(r.next_work(), Some(Work::Open(_))));

    // A host is still required: a session has one channel and it is built with both
    // waits. A subagent simply never reaches this one.
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

/// **A GRANDCHILD's settlement reaches its PARENT, and the ring names the PARENT** —
/// R58, corrected twice: 2026-10-05 for the drain, and again for the ring.
///
/// Two facts, and they are one fact now: **the session that owns the settlement is the
/// session whose bell is rung for it.** The drain was already the parent's, and the ring
/// used to go to the tree's ROOT instead — on R58's own reasoning that the daemon can only
/// serve a session in `open`. That left the ring and the notice on two different sessions:
/// the root woke, drained ITS queue, found nothing and ran no turn, while the notice sat in
/// the parent's queue until the parent happened to reach a round boundary. The operator's
/// design settles it — *"think about it like it is an erlang supervision tree. we talk to
/// parents and they own lifecycle"* — and the daemon serves a session it does not hold by
/// handing the wake to the thread that does (`Hub::wake_its_own_reader`, `Sessions::wake`).
///
/// No process host anywhere: the whole point of the tree is that `task` needs none, so
/// a test that required a cgroup would be testing the wrong channel.
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

    // The root's own set — its own session is the one it rings for.
    let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));

    // The child's runner knows the grandchild's handle, and the child's set is built
    // FROM the root's — which is where it learns the ROOT (for an ask) and the level
    // above it (for a handed-up settlement).
    let child_runner = Arc::new(FakeTask::new("s-child-sub-1"));
    let child_watch = JobWatchers::watching_tasks(&child, Some(Arc::clone(r.bell())))
        .with_tasks(&(child_runner.clone() as Arc<dyn TaskRunner>))
        .shares_tree(&root_watch);

    // The grandchild settles; the watcher runs in the CHILD's set.
    child_watch.watch("s-child-sub-1".into());
    child_runner.finish("the grandchild answered");

    // **The CHILD drains it, through the door a child actually has.** Not
    // `take_completions` — the between-turns drain belongs to a session a worker
    // drives, and nothing drives a child; `take_mid_turn_completions` is its own round
    // boundary, which is the drain that exists.
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

    // **And the PARENT's drain is empty** — that is the defect, one assert away.
    assert!(
        root_watch.take_completions().is_empty(),
        "a grandchild's settlement is its parent's to be told, not the main session's"
    );
    assert!(
        root_watch.take_mid_turn_completions().is_empty(),
        "and a root has no mid-turn door at all: R7's wake is its only one"
    );

    // **And the bell was rung for the session that owns it — the CHILD.** A ring for the
    // root would be a wake for a session with nothing to do: `Sessions::wake` would drain
    // the root's queue, find nothing, and run no turn. The daemon then serves the child by
    // handing the wake to the thread that runs it, which is what makes this ring a promise.
    assert!(
        matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-child"),
        "a grandchild's settlement must ring the session that owns it, which is its PARENT"
    );
}

/// **A job a subagent started is the SUBAGENT's to be told, and the parent never sees
/// it.** The acceptance test for 2026-10-05, in the three sentences this was asked for.
///
/// Measured twice from the parent's own conversation: a `sleep 25; echo done` a subagent
/// backgrounded arrived in the MAIN session's transcript as *"a job you backgrounded has
/// ended"*, and the main session's own `job_output` answered *no job called `j17`*. So
/// the session that was woken was the one session that could not read the job — and
/// unless the child also got it, the settlement was lost at both ends.
///
/// The three sentences, each one assert below:
///
///   1. **the child sees it** — drained through the child's own door
///      ([`JobWatchers::take_mid_turn_completions`]);
///   2. **the parent never does** — the root's between-turns drain is empty while the
///      child lives;
///   3. **the id is the child's** — `owner` names the child, so the sentence that reaches
///      anybody cannot claim *you backgrounded* nor send them to `job_output` for a handle
///      their own `job_list` does not hold.
///
/// What the parent's `job_list` holds is the third sentence's other half and is true by
/// construction rather than by this code: `Harness::job_entries` reads the session's OWN
/// process host, and a child's host is not its parent's — which is precisely why the
/// notice must not reach the parent.
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
    // above it and the root — see `JobWatchers::shares_tree`.
    let child_watch = JobWatchers::new(
        &(host.clone() as Arc<dyn ProcessHost>),
        &child,
        Some(Arc::clone(r.bell())),
    )
    .shares_tree(&root_watch);

    // **1. The child that backgrounded it is the one told.**
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

    // **2. The parent is not told while the child is there.**
    assert!(
        root_watch.take_completions().is_empty(),
        "the main session must not be handed a job its own host has never heard of"
    );

    // And the ring is the OWNER's, not the root's: the settlement is the child's to
    // answer for, and a ring for a session with nothing in its queue is a wake that runs
    // no turn.
    assert!(
        matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-child"),
        "the ring goes to the session that owns the settlement — the parent, not the root"
    );

    // **3. And what a stopped child could not drain goes UP, saying whose it was.**
    // `Harness::close_backend` runs at the end of the child's turn, so from here on
    // nothing will read the child's queue: a job that settles now is the parent's to be
    // told, as somebody else's, rather than a settlement nobody reads.
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
    // **And the ring names the session it was handed TO** — the level above, which is the
    // one holding it now. A ring for the stopped child would be a wake for a session that
    // has nothing left to drain, which is the shape R58 spent a change removing from the
    // other end of this route.
    assert!(
        matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-root"),
        "a handed-up settlement rings the level above, not the session that stopped"
    );
}

/// **A settlement reaches the session that started it and not its sibling.**
///
/// The *"and to nobody else"* half: two children of one root, each watching its own
/// subagent, and neither can be told the other's news — each drains its own queue. The
/// one direction a settlement does travel is UP, when the session that owns it has
/// stopped ((`JobWatchers::stop`)), which is the only direction that keeps it inside a
/// line that can still act on it. No host anywhere: `task` needs none.
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

    // **The child's turn ends**, and one more of its children settles afterwards: up,
    // with its owner's name on it, and still not across to the sibling.
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

/// **A child's turn ending does not silence its tree's watchers** — measured 2026-10-04.
///
/// [`JobWatchers::shares_tree`] shared the `stop` flag along with the queue, on the
/// reasoning that *"a tree closes together"*. It does not: `Harness::close_backend` runs at
/// the end of a **child's** turn — *"release its substrate now, not when the harness is
/// dropped"* — so the first subagent to finish set the ROOT's stop flag and every watcher in
/// the tree broke out of its wait at its next chunk. The operator, on their own head: *"it
/// suddenly stopped getting job completion events"* — and the shape is in the same
/// measurement, because a job that settles inside one [`WATCH_CHUNK`] still raced through
/// the `Happened` arm, so three- and four-second demo jobs kept reporting and a
/// forty-five-second one did not.
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

    // The child's turn ends and it releases its substrate — which is the moment the tree's
    // job notices used to die.
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

    /// **`Unknown` for everything but its one handle**, which is the answer that
    /// tells `JobWatchers::watch` a handle is not a subagent — and which a mirror of
    /// this test on the jobs side relies on to keep its own path.
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

/// The one completion, or a deadline. A poll rather than a `job_wait`: what is being
/// waited for is another thread's queue push, and a test that blocked on a condvar
/// this file does not own would be testing its own sleep.
fn wait_for_completion(w: &JobWatchers) -> JobCompletion {
    wait_for(w, |w| w.take_completions())
}

/// **The child's own door** — the same wait, drained the way a CHILD's round boundary
/// drains. Which door a settlement comes through is the whole of the routing question, so
/// the two are asserted through the two functions rather than through one of them twice.
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
