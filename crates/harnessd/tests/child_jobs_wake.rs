//! A job's exit while the child is turned back wakes it, as it always did: the settlement is rung
//! on the child's own queue and the child takes the turn. The turned-back half of that - a child's
//! answer not going up over a job it started - is `child_jobs.rs`; this file is the wake.
//!
//! It needs a real process host to have a job at all, so a box with no cgroup v2 tree skips it and
//! says so, the way `kept_jobs` does. One test, because `XDG_CONFIG_HOME` is process-global and it
//! is how the model stub is reached.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::harness::{ChildTurns, serve_child_under, turned_back_for_its_jobs};
use letibot_harnessd::sessions::TodoNagClock;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, Work, WorkOrIdle};
use letibot_tools::authorise::Speaker;
use letibot_tools::exec::{ScopeKind, SpawnRequest};
use letibot_transcript::Backgrounding;

mod support;

/// What the stub says on each request: the answer said over the job, the turn-back's answer, and
/// then the turn the job's exit earns.
const SAID: [&str; 3] = ["All done.", "Waiting for the tests.", "The tests passed."];

fn a_stub() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a port");
    let url = format!(
        "http://127.0.0.1:{}/v1/chat/completions",
        listener.local_addr().expect("an address").port()
    );
    let rounds = Arc::new(AtomicUsize::new(0));
    let seen = rounds.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let (line, _body) = support::stub::read_request(conn.try_clone().expect("a clone"));
            if !line.starts_with("POST") {
                let _ = write_json(conn, "{\"data\":[]}");
                continue;
            }
            let round = seen.fetch_add(1, Ordering::SeqCst);
            let _ = write_sse(conn, SAID[round.min(SAID.len() - 1)]);
        }
    });
    (url, rounds)
}

fn write_json(mut stream: TcpStream, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn write_sse(mut stream: TcpStream, answer: &str) -> std::io::Result<()> {
    let frame = |v: serde_json::Value| format!("data: {v}\n\n");
    let mut body = String::new();
    body.push_str(&frame(
        serde_json::json!({"choices":[{"delta":{"content":answer}}]}),
    ));
    body.push_str(&frame(
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
    ));
    body.push_str(&frame(serde_json::json!({"choices":[],"usage":{
        "prompt_tokens": 50,
        "completion_tokens": 9,
    }})));
    body.push_str("data: [DONE]\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// A child turned back for a job that really runs, and then that job's end waking it.
#[test]
fn a_job_that_ends_while_the_child_is_turned_back_wakes_it() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process host this session can start a job in (cgroup v2)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let scratch = letibot_sessionlog::testing::scratch("letibot-child-jobs-wake");
    std::fs::create_dir_all(scratch.join("letibot")).expect("the scratch config dir");
    // SAFETY: one test in this binary sets these, before any thread it starts reads them.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        std::env::set_var("XDG_DATA_HOME", &scratch);
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
    let (url, rounds) = a_stub();
    std::fs::write(
        scratch.join("letibot/providers.toml"),
        format!(
            "# Written by child_jobs_wake.rs. Nothing here is the operator's file.\n\
             [deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n"
        ),
    )
    .expect("the providers file");

    let dir = scratch.join("ws");
    std::fs::create_dir_all(&dir).expect("the workspace");
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    cfg.context_window = None;
    cfg.store = Some(scratch.join("sessions.db"));
    cfg.session_id = "s-child".into();
    // A seat that runs commands: the default seat is read-only and has no process host to spawn in.
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    cfg.permission = Vec::new();
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");
    *parts.mode_store.write().unwrap_or_else(|e| e.into_inner()) =
        letibot_harnessd::modes::ModeStore::default();
    let registry = Registry::new();
    let hub: Arc<Hub> = registry.new_hub("s-child".to_string());
    // The registry must HOLD the session it serves: a ring for one it does not hold is dropped, and
    // this is the half `Sessions` does for a child before it serves it.
    registry
        .adopt(
            Arc::clone(&hub),
            "the child",
            letibot_sessionlog::registry::SessionWiring {
                workspace: dir.display().to_string(),
                ..Default::default()
            },
            None,
        )
        .expect("the registry holds the session it serves");
    let mut sub =
        Harness::open_with_registry(&parts, cfg, hub.clone(), None, None, Arc::clone(&registry))
            .expect("the child opens");

    let mut reply = sub
        .submit_as_a_normal_session(Speaker::Operator, "run the tests and hand off")
        .expect("the first turn answers");
    assert_eq!(reply.text, SAID[0], "the answer said over the job");

    // A job that really runs here, in this child's own host, and ends BY ITSELF: one this session
    // kills is quiet by design (`watch_one` settles `killed by job_kill` with no push, because the
    // `job_kill` result already told the model), so the only ending that owes a turn is the job's
    // own. The marker is how this file decides when that happens. And it is watched - the half a
    // `bash` call gets from `JobWatchSink` and a bare spawn does not, since a job nobody watches
    // settles nowhere and no wake is ever owed.
    let marker = scratch.join("the-job-may-end");
    let host = sub
        .runtime_handle()
        .backend
        .processes_arc()
        .expect("a process host, which the gate above proved");
    let job = host
        .spawn(&SpawnRequest {
            command: format!("while [ ! -f '{}' ]; do sleep 0.2; done", marker.display()),
            slug: Some("tests".into()),
            cwd: ".".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("the job starts");
    let watching = sub
        .job_watchers()
        .expect("a session with a host watches what it starts");
    watching.record_start(&job.0, &Backgrounding::Asked.phrasing(), 0);
    watching.watch(job.0.clone());

    // The turn-back, on the REAL reading of this session's jobs.
    let mut turns = ChildTurns::new(&sub, "s-child", TodoNagClock::new());
    turned_back_for_its_jobs(
        &mut sub,
        &mut turns,
        "the child",
        &mut reply,
        &|h: &Harness| h.open_jobs(),
        &|_state, _said| {},
    );
    assert_eq!(
        reply.text, SAID[1],
        "the child was turned back for the job it started"
    );

    // The child is served, so the settlement the job's exit makes has somewhere to land.
    let serving = {
        let hub = hub.clone();
        std::thread::Builder::new()
            .name("serve-child".into())
            .spawn(move || serve_child_under(&mut sub, &hub, "s-child", TodoNagClock::new()))
            .expect("the child's thread starts")
    };

    // The job ends by itself now. What must follow is a turn of the child's own: the watcher queues
    // the settlement and rings the daemon's bell, and the ring is served here because a test has no
    // `Sessions` - the route `Sessions::wake` takes for a child on its own thread is
    // `WakeRoute::ItsOwnReader`, which is the hub's own reader.
    std::fs::write(&marker, b"go").expect("the marker the job is waiting for");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut rang = false;
    while std::time::Instant::now() < deadline && rounds.load(Ordering::SeqCst) < 3 {
        let look = std::time::Instant::now() + std::time::Duration::from_millis(20);
        if let WorkOrIdle::Work(Work::Woken(id)) = registry.next_work_until(Some(look)) {
            assert_eq!(
                id, "s-child",
                "a wake was rung for a session this test never opened"
            );
            rang = true;
            assert!(
                hub.wake_its_own_reader(),
                "the child's reader took the wake its own settlement rang"
            );
        }
    }
    let turned = rounds.load(Ordering::SeqCst);
    hub.close();
    let _ = serving.join();
    assert!(
        turned >= 3,
        "the job's exit woke no turn in the child that started it ({turned} turns, and the \
         settlement's own is the third; the settlement {})",
        if rang { "did ring" } else { "never rang" }
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
