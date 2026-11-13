//! A `task` the OPERATOR runs arms a watcher, so its child's settlement rings the session
//! that ran it.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use letibot_harnessd::Parts;
use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::harness::Harness;
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_tokencore::Vocab;
use letibot_tokencore::store::{SessionRecord, Store};
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

mod support;

use support::canned;
use support::canned::Reply;
use support::qwen::{IM_END, THINK_CLOSE, spoken};

/// How long a correct build takes milliseconds for. Generous on purpose: the assertion is what
/// happened, not how fast.
const PATIENCE: Duration = Duration::from_secs(30);

/// The child's brief, and the words the operator typed to start it.
const BRIEF: &str = "audit the parser's error paths";

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
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
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A root session in its own scratch workspace, with a row in the store.
fn session(tag: &str) -> (TempDir, Config, Parts) {
    let dir = TempDir::new(tag);
    let store = dir.path.join("sessions.db");
    let session_id = format!("{tag}-test");
    {
        let s = Store::open(&store).expect("seeding");
        s.put_session(&SessionRecord {
            id: session_id.clone(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: dir.path.display().to_string(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .expect("the session row");
    }
    let mut cfg = Config::for_this_box(&dir.path);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    // `leticode` because it is the UNCONFINED seat that has the delegation trio - `task`
    // `task_result`, `task_message`.
    cfg.seat = Seat::Leticode;
    cfg.http_retries = 0;
    // A window, because a child is planned against one - a spawn with no window is refused by
    // name, and this session has never measured a server (`local_window` is `None` outside the
    // daemon), so the number here is what `runs_on_the_parents_model` falls back to.
    cfg.context_window = Some(32_768);
    cfg.store = Some(store);
    cfg.session_id = session_id;
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.vocab_gguf = support::vocab::gguf();
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    (dir, cfg, parts)
}

fn a_plain_answer(vocab: &Vocab, answer: &str) -> Reply {
    let thought = vocab.tokenize_text("thinking").expect("tokenizes");
    let answer_ids = vocab.tokenize_text(answer).expect("tokenizes");
    let mut frames = vec![canned::Frame::Progress {
        total: 30,
        processed: 30,
    }];
    frames.extend(spoken(vocab, &thought));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (thought.len() + answer_ids.len() + 2) as u64,
        n_prompt: 30,
        cache_n: 0,
    });
    Reply::Frames(frames)
}

/// The user rows of the transcript, speaker and text.
fn user_rows(h: &Harness) -> Vec<(Speaker, String)> {
    h.items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { speaker, parts } => parts.iter().find_map(|p| match p {
                UserPart::Text { text } => Some((*speaker, text.clone())),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

/// The session's hub adopted into a registry, the way the daemon's are - so the bell the watcher
/// set rings is one this test can ask whether the session was woken.
fn adopted(parts: &Parts, cfg: &Config) -> (Harness, Arc<Registry>) {
    let registry = Registry::new();
    let hub = registry.new_hub(cfg.session_id.clone());
    registry
        .adopt(
            hub.clone(),
            cfg.session_id.clone(),
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: "http://127.0.0.1:1".into(),
                workspace: cfg.workspace.display().to_string(),
            },
            None,
        )
        .expect("the session is in the registry");
    let h = Harness::open_with_registry(parts, cfg.clone(), hub, None, None, registry.clone())
        .expect("the session opens");
    (h, registry)
}

/// Wait for the worker to be woken for `id` - `Work::Woken` and nothing else. Anything the
/// registry queues for another reason is passed over (a child's own `Open`, a wake for the child's
/// session), because the fact under test is which session a settlement rang.
fn woken(registry: &Registry, id: &str) -> bool {
    let began = Instant::now();
    loop {
        match registry.next_work_until(Some(Instant::now())) {
            WorkOrIdle::Work(Work::Woken(w)) if w == id => return true,
            WorkOrIdle::Work(_) => continue,
            WorkOrIdle::Idle => {
                if began.elapsed() >= PATIENCE {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            // The registry closed under us: nothing will ever be rung again.
            WorkOrIdle::Closed => return false,
        }
    }
}

/// The child's own log, which is where its answer is - and the proof that a child was really
/// started rather than a handle invented.
fn answers(hub: &letibot_sessionlog::hub::Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            letibot_sessionlog::SessionEvent::TranscriptContent { item, .. }
            | letibot_sessionlog::SessionEvent::TranscriptAppended {
                item: Some(item), ..
            } => match &**item {
                TranscriptItem::Assistant { text, .. } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Wait until the child is in the registry, or say what the door said.
fn child_hub(registry: &Registry, mine: &str) -> Arc<letibot_sessionlog::hub::Hub> {
    let began = Instant::now();
    loop {
        // Every session in the registry except this one is the child - a root has no other.
        if let Some(hub) = registry
            .ids()
            .into_iter()
            .find(|id| id != mine)
            .and_then(|id| registry.get(&id))
        {
            return hub;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "no child ever entered the registry: {:?}",
            registry.ids()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_task_the_operator_runs_rings_the_session_that_ran_it() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (_dir, mut cfg, parts) = session("op-task-wakes");
    // One script for every request: the child's turn, the `GET /props` a spawn makes of the parent's
    // endpoint, and the turn the wake runs. The order is not this test's subject, and a script per
    // request would make it one.
    let serv = canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "noted")], 8);
    cfg.endpoint = serv.endpoint.clone();
    let (mut h, registry) = adopted(&parts, &cfg);
    let session_id = cfg.session_id.clone();

    // The door's own function. `HEAD_RUN_TOOLS` is the head's list - the server refuses a name
    // outside it on the connection's thread - and this is the function that RUNS a call once it is
    // admitted. A `task` from the operator reaches exactly this.
    h.run_operator_call(
        "op-1",
        "task",
        &serde_json::json!({ "prompt": BRIEF }).to_string(),
        "dead",
    )
    .expect("the operator's call ran");

    // The row the operator got back names the child and says it is running - the promise this test
    // exists to make true.
    let said = h
        .items()
        .iter()
        .find_map(|i| match i {
            TranscriptItem::ToolResult { payload, .. } if payload.contains("subagent") => {
                Some(payload.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "no tool result row for the operator's call: {:?}",
                h.items()
            )
        });
    assert!(
        said.contains(BRIEF) || said.to_lowercase().contains("running"),
        "the row says what the operator started: {said}"
    );

    // The child is real: it opened its own session and answered its own turn.
    let child = child_hub(&registry, &session_id);
    let began = Instant::now();
    while answers(&child).is_empty() {
        assert!(
            began.elapsed() < PATIENCE,
            "the child never answered: {:#?}",
            child
                .retained()
                .iter()
                .map(|e| &e.event)
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // And nobody calls `task_result`. The settlement has to arrive on its own: the ring is what
    // `Sessions::wake` answers, and without it the notice below can never be composed.
    assert!(
        woken(&registry, &session_id),
        "the session that ran the `task` was never woken — no watcher was armed for the handle \
         the door minted, so the settlement went to nobody. That is the whole defect: the row \
         promises a completion and no bell rings for it."
    );

    // The wake the ring bought, and the sentence it composes.
    let reply = h
        .wake()
        .expect("the wake runs")
        .expect("a settlement is something to say");
    assert!(!reply.text.is_empty(), "the wake said nothing");
    let notices: Vec<String> = user_rows(&h)
        .into_iter()
        .filter(|(s, _)| *s == Speaker::Agent)
        .map(|(_, t)| t)
        .collect();
    assert!(
        notices
            .iter()
            .any(|t| t.starts_with("[task]") && t.contains("a subagent you started has finished")),
        "the model is told, unprompted, in the session's own voice: {notices:#?}"
    );
    assert!(
        notices
            .iter()
            .any(|t| t.contains(BRIEF) || t.contains("sub")),
        "and the sentence names the child: {notices:#?}"
    );
}

/// The other door, and the reachable half: a `!` line moved to the background mid-flight.
/// The operator types a line, the command is still running, and they press Ctrl+O.
#[test]
fn an_operator_shell_run_moved_to_the_background_still_reaches_the_model() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let (_dir, mut cfg, parts) = session("op-bang-promote");
    cfg.allow_bash = true;
    let serv = canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "noted")], 4);
    cfg.endpoint = serv.endpoint.clone();
    let (h, registry) = adopted(&parts, &cfg);
    let hub = registry.get(&cfg.session_id).expect("the session's hub");
    let session_id = cfg.session_id.clone();

    // The run, on its own thread, the way the daemon runs it. Long enough that a promote a
    // half-second in is unambiguously mid-flight; short enough that the job's own ending lands inside
    // this test's patience rather than the other way round.
    let (tx, rx) = std::sync::mpsc::channel();
    let join = std::thread::spawn(move || {
        let mut h = h;
        let ran = h.run_operator_shell("! sleep 3", "dead");
        let _ = tx.send(ran.is_ok());
        h
    });
    std::thread::sleep(Duration::from_millis(500));
    // Exactly what Ctrl+O sets. The flag on the hub is what the `bash` wait loop polls; the
    // queued `CommandKind::Promote` is only its announcement, and this test has no worker.
    hub.request_promote("dead");
    assert!(
        rx.recv_timeout(PATIENCE).expect("the run returned"),
        "the operator's line ran"
    );
    let mut h = join.join().expect("the run's own thread");

    // The row says the run is in the background, which is the sentence that promises delivery.
    let said = h
        .items()
        .iter()
        .find_map(|i| match i {
            TranscriptItem::ToolResult { payload, .. }
                if payload.contains("moved this to the background") =>
            {
                Some(payload.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "the run was not promoted — its row is the command's own outcome: {:?}",
                h.items()
            )
        });
    assert!(
        said.contains(letibot_tools::builtins::DELIVERED),
        "the row promises the completion this test is about: {said}"
    );

    // And then it ends, with nobody asking. The handle the promotion minted is in the session's
    // own watcher set only if the sink was armed where the handle was made.
    assert!(
        woken(&registry, &session_id),
        "the session whose `!` line was moved to the background was never woken: the promotion \
         minted a job handle in nobody's table, so the completion the row promised never came"
    );
    h.wake()
        .expect("the wake runs")
        .expect("a settlement is something to say");
    let notices: Vec<String> = user_rows(&h)
        .into_iter()
        .filter(|(s, _)| *s == Speaker::Agent)
        .map(|(_, t)| t)
        .collect();
    assert!(
        notices
            .iter()
            .any(|t| t.starts_with("[job]") && t.contains("backgrounded has ended")),
        "the model is told the job ended, unprompted: {notices:#?}"
    );
}

/// The BETWEEN-TURNS door is a third function, and it is the one a daemon actually uses.
#[test]
fn a_bang_run_on_its_own_thread_watches_what_it_promotes() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let (_dir, mut cfg, parts) = session("op-bangrun-promote");
    cfg.allow_bash = true;
    let serv = canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "noted")], 4);
    cfg.endpoint = serv.endpoint.clone();
    let (mut h, registry) = adopted(&parts, &cfg);
    let hub = registry.get(&cfg.session_id).expect("the session's hub");
    let session_id = cfg.session_id.clone();

    let line = "! sleep 3";
    let (call_id, call) = h.prepare_operator_shell(line).expect("the line mints");
    // The three things `Sessions::dispatch` hands over, taken from the harness exactly as it
    // takes them - and the fourth, which is this change: the watcher.
    let runtime = h.runtime_handle();
    let watch = h.job_watchers();
    let session_for_run = session_id.clone();
    let hub_for_run = hub.clone();
    let line_for_run = line.to_string();
    let call_for_run = call.clone();
    let call_id_for_run = call_id.clone();
    let runtime_for_run = runtime.clone();
    let watch_for_run = watch.clone();
    let spawned = letibot_harnessd::bangrun::spawn_run(letibot_harnessd::bangrun::OperatorRun {
        session: session_for_run,
        call_id: call_id_for_run,
        call: call_for_run,
        line: line_for_run,
        who: "dead".to_string(),
        runtime: runtime_for_run,
        hub: hub_for_run,
        watch: watch_for_run,
    });
    spawned.expect("the run's thread starts");
    std::thread::sleep(Duration::from_millis(500));
    // Exactly what Ctrl+O sets, on the hub the run's own wait loop polls.
    hub.request_promote("dead");

    assert!(
        woken(&registry, &session_id),
        "a `!` line run on its own thread (`bangrun`) and moved to the background was never \
         woken: the thread was handed a runtime and a hub and no watcher, so the handle its \
         promotion minted went to nobody"
    );
    h.wake()
        .expect("the wake runs")
        .expect("a settlement is something to say");
    let notices: Vec<String> = user_rows(&h)
        .into_iter()
        .filter(|(s, _)| *s == Speaker::Agent)
        .map(|(_, t)| t)
        .collect();
    assert!(
        notices
            .iter()
            .any(|t| t.starts_with("[job]") && t.contains("backgrounded has ended")),
        "the model is told the job ended, unprompted: {notices:#?}"
    );
}
