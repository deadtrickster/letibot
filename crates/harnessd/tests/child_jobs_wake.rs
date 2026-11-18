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

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::harness::{ChildTurns, serve_child_under, turned_back_for_its_jobs};
use letibot_harnessd::sessions::TodoNagClock;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_tools::authorise::Speaker;
use letibot_tools::exec::{ScopeKind, SpawnRequest};

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
    cfg.permission = Vec::new();
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");
    *parts.mode_store.write().unwrap_or_else(|e| e.into_inner()) =
        letibot_harnessd::modes::ModeStore::default();
    let registry = Registry::new();
    let hub: Arc<Hub> = registry.new_hub("s-child".to_string());
    let mut sub = Harness::open_with_registry(&parts, cfg, hub.clone(), None, None, registry)
        .expect("the child opens");

    let mut reply = sub
        .submit_as_a_normal_session(Speaker::Operator, "run the tests and hand off")
        .expect("the first turn answers");
    assert_eq!(reply.text, SAID[0], "the answer said over the job");

    // A job that really runs here, in this child's own host.
    let host = sub
        .runtime_handle()
        .backend
        .processes_arc()
        .expect("a process host, which the gate above proved");
    let job = host
        .spawn(&SpawnRequest {
            command: "sleep 300".into(),
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

    // The job ends. What must follow is a turn of the child's own: the settlement is rung on its
    // queue and the loop takes it, which is the delivery a wake has always made.
    host.kill_job(&job).expect("the job is killed");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline && rounds.load(Ordering::SeqCst) < 3 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let turned = rounds.load(Ordering::SeqCst);
    hub.close();
    let _ = serving.join();
    assert!(
        turned >= 3,
        "the job's exit woke no turn in the child that started it ({turned} turns, and the \
         settlement's own is the third)"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
