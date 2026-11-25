//! A turn whose provider name stops resolving, through a real daemon, a real head and a loopback
//! stub: it waits the no-network budget out and finishes when the name comes back, and ends saying
//! it was offline when it does not. The resolver is the test's, so no DNS is asked and no packet
//! leaves the box.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;
use letibot_transcript::TranscriptItem;

/// The name the stub is reached by. It resolves through the box's own hosts file, so a turn that
/// gets it back reaches the socket without a DNS query.
const HOST: &str = "localhost";

/// What a resolver says when a name does not exist, in the words `Policy::what` reads as
/// "name resolution failed".
const UNRESOLVED: &str = "failed to lookup address information: Name or service not known";

/// The answer the stub streams when the name resolves. A marker, so the row can be found.
const ANSWER: &str = "the-provider-answered";

/// The prompt. Nothing tool-shaped in it: this file is about the network, not the work.
const PROMPT: &str = "say hello";

// ---------------------------------------------------------------------------
// The stub endpoint
// ---------------------------------------------------------------------------

fn endpoint() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut conn) = stream else { continue };
            let _body = read_request(&mut conn);
            let body = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{ANSWER}\"}}}}]}}\n\n\
                 data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
                 data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":50,\"completion_tokens\":4}}}}\n\n\
                 data: [DONE]\n\n"
            );
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = conn.flush();
        }
    });
    port
}

/// Drain one request - headers plus the body `Content-Length` names - so the client's write side is
/// finished before the reply arrives.
fn read_request(s: &mut TcpStream) -> String {
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
        }
        if let Some(head_end) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
            let body_have = acc.len() - (head_end + 4);
            let body_want = acc[..head_end]
                .split(|b| *b == b'\n')
                .find_map(|line| {
                    let line = String::from_utf8_lossy(line);
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if body_have >= body_want {
                return String::from_utf8_lossy(&acc[head_end + 4..head_end + 4 + body_want])
                    .into_owned();
            }
        }
        if acc.len() > (1 << 22) {
            break;
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// The outage
// ---------------------------------------------------------------------------

/// A name that does not resolve, for `hold` from the turn's first attempt: every connect the
/// provider makes while it lasts fails the way a resolver with no answer does.
#[derive(Clone)]
struct Outage {
    hold: Duration,
    first: Arc<Mutex<Option<Instant>>>,
    attempts: Arc<Mutex<Vec<Instant>>>,
}

impl Outage {
    fn new(hold: Duration) -> Outage {
        Outage {
            hold,
            first: Arc::new(Mutex::new(None)),
            attempts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Install this outage as the process's resolution fault, and keep it until it is cleared.
    fn install(&self) {
        let me = self.clone();
        let fault: letibot_provider::agent::Fault = Arc::new(move |host: &str| {
            if host != HOST {
                return None;
            }
            let mut first = me.first.lock().unwrap();
            let start = *first.get_or_insert_with(Instant::now);
            drop(first);
            if start.elapsed() >= me.hold {
                return None;
            }
            me.attempts.lock().unwrap().push(Instant::now());
            Some(std::io::Error::other(UNRESOLVED))
        });
        letibot_provider::agent::fail_resolution_with(Some(fault));
    }

    /// When each attempt was refused, in order.
    fn attempts(&self) -> Vec<Instant> {
        self.attempts.lock().unwrap().clone()
    }
}

// ---------------------------------------------------------------------------
// What one head saw
// ---------------------------------------------------------------------------

/// Everything one head was told, folded to the facts this file asserts on.
#[derive(Default)]
struct Rec {
    /// Every `turn_failed` sentence, in order - what the operator read under `FAILED - `.
    failed: Vec<String>,
    answered: usize,
}

fn fold(rec: &mut Rec, inb: Inbound) {
    if let ServerFrame::Event(env) = Inbound::frame(inb)
        && let letibot_sessionlog::event::SessionEvent::TurnFailed { error, .. } = &env.event
    {
        rec.failed.push(error.clone());
    }
}

/// Pump until `done`, folding every frame in. The deadline is generous - it exists so a regression
/// reads as its missing fact rather than as a hung test.
fn watch(rx: &Receiver<Inbound>, rec: &mut Rec, done: impl Fn(&Rec) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline && !done(rec) {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(inb) => fold(rec, inb),
            Err(RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("the head's pump died: {e}"),
        }
    }
}

/// How many of the stub's answers are rows yet - read off the daemon's own snapshot rather than
/// off an event tally, so this is the turn having ENDED and not a frame having arrived.
fn answers(hub: &letibot_sessionlog::hub::Hub) -> usize {
    hub.snapshot()
        .items
        .iter()
        .filter_map(|i| i.item.as_ref())
        .filter(|i| matches!(i, TranscriptItem::Assistant { text, .. } if text.contains(ANSWER)))
        .count()
}

/// A config for one session of this file, on the byte vocabulary and a loopback provider - so
/// nothing here needs a GGUF, a llama.cpp or a model file of any kind. The budget comes from the
/// workspace's `leticode.toml`; `session_fallback` is the daemon's private door, so the file's span
/// is put where that door puts it.
fn config(scratch: &std::path::Path, tag: &str, budget: &str) -> Config {
    let ws = scratch.join(format!("ws-{tag}"));
    std::fs::create_dir_all(&ws).expect("the workspace");
    let file = ws.join("leticode.toml");
    std::fs::write(
        &file,
        format!("[provider]\noffline_budget = \"{budget}\"\n"),
    )
    .expect("the workspace's leticode.toml");
    let mut cfg = Config::for_this_box(&ws);
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    cfg.vocab_gguf = None;
    cfg.session_id = format!("provider-offline-{tag}");
    cfg.socket = scratch.join(format!("{tag}.sock"));
    cfg.store = Some(scratch.join(format!("{tag}.db")));
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".to_string(),
        model: Some("deepseek-chat".to_string()),
        api_key: Some("sk-test".to_string()),
        thinking: false,
    });
    // No window, so no wall: a compaction door firing in the middle of an outage is a second
    // subject.
    cfg.context_window = None;
    cfg.leticode = letibot_harnessd::leticode_config::LeticodeConfig::load(&file)
        .expect("the workspace's own file");
    cfg.http_offline_budget = cfg
        .leticode
        .offline_budget()
        .expect("the file names a budget");
    // No `[fallback]`, so an exhausted ladder ends the turn rather than moving the session.
    cfg.fallback = Vec::new();
    cfg
}

/// One session, one daemon, one head - the driver on a thread of its own and the worker loop on
/// this one.
fn drive<F>(parts: &Parts, cfg: Config, body: F) -> Rec
where
    F: FnOnce(&mut HeadClient, &Receiver<Inbound>, &letibot_sessionlog::hub::Hub, &mut Rec)
        + Send
        + 'static,
{
    let session = cfg.session_id.clone();
    let registry = Registry::new();
    registry
        .create(session.clone(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry has no session by that name");
    let daemon = Daemon::serve(registry.clone(), &cfg.socket).expect("the socket binds");
    let (client, _hello, reader) = HeadClient::attach(
        &cfg.socket,
        &session,
        0,
        "tui",
        "provider-offline-test",
        Caps::default(),
    )
    .expect("a head attaches");
    let (tx, rx) = channel();
    std::thread::spawn(move || pump(reader, tx));
    let hub = registry
        .get(&session)
        .expect("the session is in the registry");
    let mut sessions =
        Sessions::open_first(parts, cfg.clone(), registry.clone()).expect("the session opens");

    let driver = {
        let registry = registry.clone();
        let hub = hub.clone();
        std::thread::spawn(move || {
            let mut client = client;
            let mut rec = Rec::default();
            // The registry is closed even if the driver panics: a panic here would otherwise leave
            // `daemon.run` blocked on a bell nobody rings, and a regression would read as a hung
            // test rather than as the missing fact it is.
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                body(&mut client, &rx, &hub, &mut rec);
            }));
            registry.close();
            if let Err(why) = ran {
                std::panic::resume_unwind(why);
            }
            rec
        })
    };

    daemon.run(&mut sessions, |_, _| {});
    let rec = driver.join().expect("the driver thread");
    daemon.shutdown();
    rec
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// The plan's done-when, in seconds instead of ninety: an endpoint unreachable for a while and then
/// up completes the turn without "gave up", and one that stays down ends saying the box is offline.
#[test]
fn a_turn_waits_out_a_name_that_will_not_resolve_and_gives_up_when_the_budget_runs_out() {
    // The operator's `providers.toml` must not be touched: point the config home at a scratch
    // directory for this process, and take the keys this file writes out of anybody else's reach.
    let scratch = letibot_sessionlog::testing::scratch("letibot-provider-offline");
    std::fs::create_dir_all(scratch.join("letibot")).expect("the scratch config dir");
    // SAFETY: this test binary has one test, and everything below runs in sequence on this thread
    // or on threads it starts afterwards; nothing reads the environment concurrently.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        std::env::set_var("XDG_DATA_HOME", &scratch);
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
    let port = endpoint();
    std::fs::write(
        scratch.join("letibot/providers.toml"),
        format!(
            "# Written by provider_offline.rs. Nothing here is the operator's file.\n\
             [deepseek]\nkey = \"sk-test\"\n\
             url = \"http://{HOST}:{port}/v1/chat/completions\"\n"
        ),
    )
    .expect("the config file");

    let first = config(&scratch, "parts", "1h");
    let parts = Parts::load(&first).expect("the byte vocabulary needs no model file");
    // The operator's per-project mode is not this test's business.
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    // ── A. The name comes back and the turn finishes ──────────────────────────────────────────
    // The fault answers for the first 2.2 s of the turn and then lets the resolver through, which
    // is an outage that ends: the ladder has to outlive it rather than fail the round.
    let outage = Outage::new(Duration::from_millis(2200));
    outage.install();
    let cfg = config(&scratch, "recovered", "6s");
    let rec = drive(&parts, cfg, |client, rx, hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |_| answers(hub) >= 1);
        rec.answered = answers(hub);
    });
    let attempts = outage.attempts();
    assert!(
        attempts.len() >= 2,
        "the turn did not wait out a single rung: {} attempt(s) refused",
        attempts.len()
    );
    let first_wait = attempts[1].duration_since(attempts[0]);
    assert!(
        first_wait >= Duration::from_millis(700) && first_wait <= Duration::from_millis(1500),
        "the first wait is the offline ladder's one-second rung, moved by at most a fifth: \
         {first_wait:?}"
    );
    assert_eq!(
        rec.answered, 1,
        "the turn did not finish once the name resolved again"
    );
    assert!(
        rec.failed.is_empty(),
        "a budget of 6 s outlived a 2.2 s outage, so nothing failed: {:?}",
        rec.failed
    );

    // ── B. It never comes back, and the turn says the box is offline ──────────────────────────
    // A three-second budget: the waits are the one-second and two-second rungs, then what is left.
    let outage = Outage::new(Duration::from_secs(3600));
    outage.install();
    let cfg = config(&scratch, "given-up", "3s");
    let rec = drive(&parts, cfg, |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |rec| !rec.failed.is_empty());
    });
    letibot_provider::agent::fail_resolution_with(None);
    assert_eq!(
        rec.failed.len(),
        1,
        "one turn, one ending: {:?}",
        rec.failed
    );
    let said = &rec.failed[0];
    assert!(
        said.starts_with("no network for 3s: "),
        "the ending states the budget it spent: {said}"
    );
    assert!(
        said.contains("name resolution failed"),
        "and the class of failure, in words: {said}"
    );
    assert!(
        said.contains("stops retrying after 3s"),
        "and that the daemon stopped on the budget: {said}"
    );
    assert!(
        outage.attempts().len() >= 3,
        "the ladder did not take the round again enough times to have waited: {said}"
    );
}
