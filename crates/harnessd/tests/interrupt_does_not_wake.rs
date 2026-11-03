//! A turn nobody asked to continue does not ask for one: the held results wait for the next turn a
//! person types, whether the operator stopped this one or it failed.
//!
//! ONE test in this binary, because it writes the process's config dirs before anything reads them.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::harness::Harness;
use letibot_harnessd::jobwatch::{BackgroundKind, JobCompletion, JobWatchers};
use letibot_harnessd::turn_tail::{Bare, Door, run_turn};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::CommandKind;
use letibot_sessionlog::hub::{DAEMON_SUBMITTER, Hub};
use letibot_sessionlog::registry::{Bell, Registry, SessionWiring, Work, WorkOrIdle};

/// How long the turn runs before the test calls it a hang.
const PATIENCE: Duration = Duration::from_secs(20);

/// How often the stub says another word while the operator is still deciding to stop it. The
/// engine polls steering once per delta, so this is also how soon a stop is heard.
const TICK: Duration = Duration::from_millis(50);

/// In the prompt, this is the turn the stub streams slowly - the one an interrupt has to land in.
const KEEP_TALKING: &str = "keep-talking-marker";

/// The prompt itself: the marker, and words a real operator might type.
const LONG_STORY: &str = "keep-talking-marker and tell me a long story";

/// In the prompt, this is the turn the stub refuses: a turn that fails with nobody asking for another.
const REFUSED: &str = "refuse-this-turn-marker";

/// What the refusing stub says, so a failure's reason can be traced to it.
const REFUSAL: &str = "stub-refusal-r91-interrupt";

#[test]
fn a_stopped_or_failed_turn_does_not_wake_the_session_for_its_held_results() {
    let dir = TempDir::new("interrupt-no-wake");
    std::fs::create_dir_all(dir.path().join("config/letibot")).expect("config dir");
    // Nothing of the operator's may reach this session: their `providers.toml` would decide where
    // a request goes, and no request may leave loopback.
    // SAFETY: this binary has ONE test, and these are set before any harness exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
    }
    let fake = Fake::serve();
    std::fs::write(
        dir.path().join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/interrupt_does_not_wake.rs — a fake on loopback.\n\
             [deepseek]\nkey = \"sk-not-used\"\nurl = \"{}\"\n",
            fake.url
        ),
    )
    .expect("providers.toml");

    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.http_retries = 0;
    cfg.permission = Vec::new();
    cfg.web_search = None;
    // The byte vocabulary and a provider, so this runs on a machine with no model file at all.
    cfg.vocab_gguf = None;
    cfg.session_id = "s-stopped".into();
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");

    let registry = Registry::new();
    // ----------------------------------------------------------------------
    // The control: a turn that finished on its own rings for the result it held.
    // ----------------------------------------------------------------------
    let mut answered = open(&parts, &cfg, &registry, "s-answered", &workspace, &fake.url);
    let watch = answered
        .job_watchers()
        .expect("every session has a watcher: `task` needs no host");
    let landing = while_the_turn_streams(
        watch.clone(),
        "s-answered",
        fake.arrived.clone(),
        None,
        None,
        None,
    );
    let out = run_turn(&mut answered, &mut Bare, Door::Prompt, |h| {
        h.submit("answer me").map(Some)
    });
    landing.join().expect("the thread that held the result");
    assert!(
        matches!(&out, Some(Ok(_))),
        "the stub answered, so the turn finished: {:?}",
        out.as_ref().map(|r| r.is_ok())
    );
    assert_eq!(
        watch.held_count(),
        1,
        "the result landed mid-turn, so the turn did not open with it: it is still held"
    );
    assert!(
        matches!(
            registry.next_work_until(Some(Instant::now() + PATIENCE)),
            WorkOrIdle::Work(Work::Woken(_))
        ),
        "a turn that finishes on its own wakes the session for the result it held"
    );

    // ----------------------------------------------------------------------
    // And the stop: the same held result, and a turn the operator ended.
    // ----------------------------------------------------------------------
    let mut stopped = open(&parts, &cfg, &registry, "s-stopped", &workspace, &fake.url);
    let watch = stopped.job_watchers().expect("every session has a watcher");
    let hub: Arc<Hub> = registry.get("s-stopped").expect("the session's hub");
    let landing = while_the_turn_streams(
        watch.clone(),
        "s-stopped",
        fake.arrived.clone(),
        Some(hub.clone()),
        None,
        None,
    );
    let out = run_turn(&mut stopped, &mut Bare, Door::Prompt, |h| {
        h.submit(LONG_STORY).map(Some)
    });
    landing.join().expect("the thread that stopped the turn");
    // The turn ran, and the tree reports a stopped one as a turn failure whose words are the
    // interrupt's own - which is why the stop is read off the log below rather than off `out`.
    assert!(out.is_some(), "the turn ran");
    let interrupted = interrupted_reasons(&hub);
    assert!(
        interrupted.iter().any(|r| r == "steering_urgent"),
        "the premise: the turn was stopped by the operator's interrupt, not by the stream: \
         {interrupted:?}"
    );
    assert_eq!(
        watch.held_count(),
        1,
        "the result is still held, which is what makes the ring the only thing under test"
    );
    assert!(
        matches!(
            registry.next_work_until(Some(Instant::now() + Duration::from_secs(2))),
            WorkOrIdle::Idle
        ),
        "a turn the operator stopped rang a wake: that is a turn nobody asked for, straight \
         after the one they stopped"
    );

    // ----------------------------------------------------------------------
    // And a turn that failed: nobody asked for the next one either.
    // ----------------------------------------------------------------------
    let mut refused = open(&parts, &cfg, &registry, "s-refused", &workspace, &fake.url);
    let watch = refused.job_watchers().expect("every session has a watcher");
    let landing = while_the_turn_streams(
        watch.clone(),
        "s-refused",
        fake.arrived.clone(),
        None,
        Some(fake.holds.clone()),
        None,
    );
    let out = run_turn(&mut refused, &mut Bare, Door::Prompt, |h| {
        h.submit(REFUSED).map(Some)
    });
    landing.join().expect("the thread that held the result");
    assert!(
        matches!(&out, Some(Err(_))),
        "the premise: the provider refused this turn: {:?}",
        out.as_ref()
            .map(|r| r.as_ref().err().map(|e| e.to_string()))
    );
    assert_eq!(
        watch.held_count(),
        1,
        "the result is still held for the turn that failed"
    );
    assert!(
        matches!(
            registry.next_work_until(Some(Instant::now() + Duration::from_secs(2))),
            WorkOrIdle::Idle
        ),
        "a failed turn rang a wake nobody asked for: its next turn is the one somebody types"
    );

    // ----------------------------------------------------------------------
    // And a stop, with the settlement arriving the way a watcher hands one over: queued, and rung
    // on the bell. The wake is standing when the turn ends, nothing can un-ring it, so the stop has
    // to take it back rather than let the court run the turn it just ended.
    // ----------------------------------------------------------------------
    let bell = registry.bell().clone();
    let mut rung_stopped = open(&parts, &cfg, &registry, "s-rang", &workspace, &fake.url);
    let watch = rung_stopped
        .job_watchers()
        .expect("every session has a watcher");
    let hub: Arc<Hub> = registry.get("s-rang").expect("the session's hub");
    let landing = while_the_turn_streams(
        watch.clone(),
        "s-rang",
        fake.arrived.clone(),
        Some(hub.clone()),
        None,
        Some(bell.clone()),
    );
    let out = run_turn(&mut rung_stopped, &mut Bare, Door::Prompt, |h| {
        h.submit(LONG_STORY).map(Some)
    });
    landing.join().expect("the thread that stopped the turn");
    assert!(out.is_some(), "the turn ran");
    let interrupted = interrupted_reasons(&hub);
    assert!(
        interrupted.iter().any(|r| r == "steering_urgent"),
        "the premise: the turn was stopped by the operator's interrupt: {interrupted:?}"
    );
    assert!(
        matches!(
            registry.next_work_until(Some(Instant::now() + Duration::from_secs(2))),
            WorkOrIdle::Idle
        ),
        "R91.4: a settlement that landed in a stopped turn rang a turn of its own"
    );
    assert!(
        watch.pending(),
        "the settlement the stopped turn rang for is still owed to the next turn a person types"
    );

    // ----------------------------------------------------------------------
    // And the same delivery into a turn that fails: nobody asked for the next one either.
    // ----------------------------------------------------------------------
    let mut rung_refused = open(
        &parts,
        &cfg,
        &registry,
        "s-rang-failed",
        &workspace,
        &fake.url,
    );
    let watch = rung_refused
        .job_watchers()
        .expect("every session has a watcher");
    let landing = while_the_turn_streams(
        watch.clone(),
        "s-rang-failed",
        fake.arrived.clone(),
        None,
        Some(fake.holds.clone()),
        Some(bell.clone()),
    );
    let out = run_turn(&mut rung_refused, &mut Bare, Door::Prompt, |h| {
        h.submit(REFUSED).map(Some)
    });
    landing.join().expect("the thread that held the result");
    assert!(
        matches!(&out, Some(Err(_))),
        "the premise: the provider refused this turn"
    );
    assert!(
        matches!(
            registry.next_work_until(Some(Instant::now() + Duration::from_secs(2))),
            WorkOrIdle::Idle
        ),
        "R91.4: a settlement that landed in a failed turn rang a turn of its own"
    );

    let _ = std::fs::remove_dir_all(dir.path());
}

/// Every reason the hub published for a turn this session had interrupted.
fn interrupted_reasons(hub: &Hub) -> Vec<String> {
    hub.retained()
        .into_iter()
        .filter_map(|e| match e.event {
            letibot_sessionlog::SessionEvent::TurnInterrupted { reason, .. } => Some(reason),
            _ => None,
        })
        .collect()
}

/// One session, opened the way the daemon opens one.
fn open(
    parts: &Parts,
    base: &Config,
    registry: &Arc<Registry>,
    session_id: &str,
    workspace: &std::path::Path,
    url: &str,
) -> Harness {
    let mut cfg = base.clone();
    cfg.session_id = session_id.to_string();
    let hub = registry.new_hub(session_id.to_string());
    registry
        .adopt(
            hub.clone(),
            session_id.to_string(),
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: url.to_string(),
                workspace: workspace.display().to_string(),
            },
            None,
        )
        .expect("the session is in the registry");
    Harness::open_with_registry(parts, cfg, hub, None, None, registry.clone())
        .expect("the session opens")
}

/// What happens while the turn streams: a job's ending lands - so the turn cannot open with it,
/// and it is still held when the turn ends - and, when `stop` is given, the operator stops the turn.
/// `ring` delivers it the way a watcher does, queued and rung, where `hold` is a door that does not
/// ring. `held` is signalled once the result landed, so a stub can wait for it.
fn while_the_turn_streams(
    watch: Arc<JobWatchers>,
    owner: &str,
    arrived: Arc<Mutex<Receiver<()>>>,
    stop: Option<Arc<Hub>>,
    held: Option<Sender<()>>,
    ring: Option<Arc<Bell>>,
) -> std::thread::JoinHandle<()> {
    let owner = owner.to_string();
    std::thread::spawn(move || {
        arrived
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recv_timeout(PATIENCE)
            .expect("the turn reached the model");
        let landed = a_settlement(&owner);
        match ring {
            Some(bell) => {
                watch.queue_for_test(landed);
                bell.ring_wake(&owner);
            }
            None => watch.hold(vec![landed]),
        }
        if let Some(said) = held {
            let _ = said.send(());
        }
        let Some(hub) = stop else { return };
        let said = hub.submit(
            DAEMON_SUBMITTER,
            "interrupt-1",
            0,
            CommandKind::Interrupt {
                reason: "the operator stopped this turn".into(),
            },
        );
        assert!(
            matches!(said, letibot_sessionlog::ServerFrame::Accepted { .. }),
            "the hub refused the interrupt: {said:?}"
        );
    })
}

/// A result the model has not been told about, held for the next break.
fn a_settlement(owner: &str) -> JobCompletion {
    JobCompletion {
        memory: None,
        kind: BackgroundKind::Job,
        owner: owner.into(),
        job: "j41".into(),
        command: "cargo test --release".into(),
        slug: Some("release-build".into()),
        state: "exited 0".into(),
        produced: 4_096,
        elapsed_ms: 8_000,
        detail: String::new(),
        arrived: SystemTime::now(),
        later: false,
    }
}

/// A provider on a loopback port. A request carrying [`KEEP_TALKING`] is streamed until the client
/// hangs up, so a stop has somewhere to land; anything else is answered at once.
struct Fake {
    url: String,
    /// One token per request taken, so the stopper knows a turn is in flight.
    arrived: Arc<Mutex<Receiver<()>>>,
    /// The test says a result is held; a stub that refuses the turn waits for it.
    holds: Sender<()>,
}

impl Fake {
    fn serve() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().expect("the bound port").port()
        );
        let (tx, rx) = channel();
        let (holds, held) = channel();
        let serving = Arc::new(Mutex::new(held));
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let body = read_request(&conn);
                if tx.send(()).is_err() {
                    return;
                }
                let _ = match body.contains(REFUSED) {
                    true => refuse_the_turn(conn, &serving),
                    false if body.contains(KEEP_TALKING) => stream_until_hung_up(conn),
                    false => answer(conn),
                };
            }
        });
        Fake {
            url,
            arrived: Arc::new(Mutex::new(rx)),
            holds,
        }
    }
}

/// Say another word every [`TICK`] until the client hangs up: the turn ends because it was
/// stopped, not because the stream ran out.
fn stream_until_hung_up(mut conn: TcpStream) -> std::io::Result<()> {
    write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
    )?;
    conn.flush()?;
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        let frame = format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"delta":{"content":" and then"}}]})
        );
        conn.write_all(frame.as_bytes())?;
        conn.flush()?;
        std::thread::sleep(TICK);
    }
    Ok(())
}

/// A request the provider refuses, so the turn fails. It answers only once the result is held, so
/// the turn's ending is judged with something held.
fn refuse_the_turn(mut conn: TcpStream, held: &Arc<Mutex<Receiver<()>>>) -> std::io::Result<()> {
    let _ = held
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .recv_timeout(PATIENCE);
    write!(
        conn,
        "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{REFUSAL}",
        REFUSAL.len()
    )?;
    conn.flush()
}

/// One finished answer, in the three frames `letibot-provider` parses.
fn answer(mut conn: TcpStream) -> std::io::Result<()> {
    let mut body = String::new();
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{"content":" the answer"}}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":9}})
    ));
    body.push_str("data: [DONE]\n\n");
    write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    conn.flush()
}

/// Drain one HTTP request off the socket: headers, then the body `content-length` names.
fn read_request(stream: &TcpStream) -> String {
    use std::io::{BufRead, BufReader, Read};
    let mut reader = BufReader::new(stream.try_clone().expect("clone the socket"));
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    let mut len = 0usize;
    loop {
        let mut head = String::new();
        if reader.read_line(&mut head).is_err() || head.trim_end().is_empty() {
            break;
        }
        if let Some(v) = head.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    let _ = reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

/// A directory that removes itself, because a test that leaks a store per run eventually fills the
/// disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
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
