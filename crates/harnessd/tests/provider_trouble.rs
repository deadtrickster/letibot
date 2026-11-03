//! **A provider's four troubles reach the wire as types**, measured against a real daemon and a
//! scripted provider on loopback: a 429 with and without `Retry-After`, a quota refusal, a model API
//! that never answers, and a round that stalls past the first-byte threshold.
//!
//! The subject is `SessionEvent::ProviderTrouble`, so the assertions are on the frames a head actually
//! receives: the kind, the model the round was on, the reset instant the provider named, and the count
//! that makes *the second timeout* a different fact from *a timeout*. Nothing here mocks the daemon:
//! the turn runs, the provider refuses or says nothing, and the ladder's own end is where the event is
//! published.
//!
//! # One test, five scenarios, and why
//!
//! `XDG_CONFIG_HOME` is process-global and libtest runs a binary's tests in parallel, so the scratch
//! config dir is pointed at once, by the single test that owns this process, and the scenarios run in
//! sequence inside it - the same shape `model_switch_at_turn_end.rs` uses for its four.
//!
//! The timeout and the stall cannot share one daemon: a session told to give up on a silent provider
//! after 400 ms would fail the round that is meant to stall for fifteen seconds and then answer.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;

/// The question every scenario asks, one prompt each.
const PROMPT: &str = "say hello";

/// What the scripted provider is asked to do with one request.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    /// `429` with a `Retry-After` of these many seconds, or none at all.
    RateLimited(Option<u64>),
    /// `429` whose body says the account cannot pay.
    Quota,
    /// Read the request and then say nothing at all: a model API that never comes back.
    Silent,
    /// Say nothing for longer than the daemon's first-byte threshold, then answer.
    Stall,
    /// One plain answer.
    Answer,
}

/// A loopback model with a script: the nth request takes the nth act, and an empty script answers.
#[derive(Clone)]
struct Fake {
    port: u16,
    script: Arc<Mutex<Vec<Act>>>,
    seen: Arc<AtomicU64>,
    /// The `model` field of every request, in order: which model a turn actually went to is the only
    /// evidence a switch landed, and this is the provider's own copy of the body.
    models: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn serve() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let port = listener.local_addr().unwrap().port();
        let script: Arc<Mutex<Vec<Act>>> = Default::default();
        let seen: Arc<AtomicU64> = Default::default();
        let models: Arc<Mutex<Vec<String>>> = Default::default();
        let acts = script.clone();
        let requests = seen.clone();
        let asked = models.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let body = read_request(&mut s);
                requests.fetch_add(1, Ordering::SeqCst);
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                    asked
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(v["model"].as_str().unwrap_or_default().to_string());
                }
                let act = {
                    let mut g = acts.lock().unwrap_or_else(|e| e.into_inner());
                    if g.is_empty() {
                        Act::Answer
                    } else {
                        g.remove(0)
                    }
                };
                // **One thread per connection, taken in arrival order.** A real provider serves
                // concurrent requests, and a silent one that held the accept loop would make the
                // request AFTER it time out as well - a fixture failing its own subject.
                std::thread::spawn(move || match act {
                    Act::RateLimited(secs) => {
                        let body = "{\"error\":{\"message\":\"rate limited\"}}";
                        // The header is present only when the provider said something: `None` is the
                        // 429 a caller cannot size, and the two must not be told apart by a zero.
                        let header = match secs {
                            Some(secs) => format!("Retry-After: {secs}\r\n"),
                            None => String::new(),
                        };
                        let _ = write!(
                            s,
                            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n\
                             {header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = s.flush();
                    }
                    Act::Quota => {
                        let body = "{\"error\":{\"code\":\"insufficient_quota\",\"message\":\"You \
                                    exceeded your current quota\"}}";
                        let _ = write!(
                            s,
                            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = s.flush();
                    }
                    // Longer than any `provider_timeout` a scenario sets, by enough that a loaded box
                    // cannot let the close become the round's ending: the deadline is the subject.
                    Act::Silent => std::thread::sleep(Duration::from_secs(30)),
                    Act::Stall => {
                        std::thread::sleep(Duration::from_secs(16));
                        answer(&mut s);
                    }
                    Act::Answer => answer(&mut s),
                });
            }
        });
        Fake {
            port,
            script,
            seen,
            models,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/chat/completions", self.port)
    }

    /// The models the requests named, in the order they arrived.
    fn models(&self) -> Vec<String> {
        self.models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The next `acts` this provider takes, in order. The script is a queue, not a repeat: each
    /// scenario says what its own requests do.
    fn script(&self, acts: &[Act]) {
        let mut g = self.script.lock().unwrap_or_else(|e| e.into_inner());
        g.clear();
        g.extend_from_slice(acts);
    }

    fn requests(&self) -> u64 {
        self.seen.load(Ordering::SeqCst)
    }
}

/// One plain answer, in the provider's own frame shape.
fn answer(s: &mut TcpStream) {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n\
                data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                data: [DONE]\n\n";
    let _ = write!(
        s,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = s.flush();
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
            let have = acc.len() - (head_end + 4);
            let want = acc[..head_end]
                .split(|b| *b == b'\n')
                .find_map(|line| {
                    let line = String::from_utf8_lossy(line);
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if have >= want {
                return String::from_utf8_lossy(&acc[head_end + 4..head_end + 4 + want])
                    .into_owned();
            }
        }
        if acc.len() > (1 << 22) {
            break;
        }
    }
    String::new()
}

/// One trouble as the head received it: the fields this file asserts on, read off the wire.
#[derive(Debug, Clone, PartialEq)]
struct Trouble {
    kind: String,
    model: String,
    reset_ms: Option<u64>,
    count: u32,
    switched_to: Option<String>,
}

/// What one head saw, folded to the facts this file asserts on.
#[derive(Default, Clone)]
struct Rec {
    troubles: Vec<Trouble>,
    failed: usize,
    /// A turn that ended in anything: the end of one scenario's wait.
    ended: usize,
    /// Every warning the daemon raised, for a failure message that says WHICH refusal this was.
    said: Vec<String>,
}

fn fold(rec: &mut Rec, inb: Inbound) {
    match Inbound::frame(inb) {
        ServerFrame::Event(env) => match &env.event {
            SessionEvent::ProviderTrouble {
                kind,
                model,
                reset_ms,
                count,
                switched_to,
                ..
            } => rec.troubles.push(Trouble {
                kind: kind.clone(),
                model: model.clone(),
                reset_ms: *reset_ms,
                count: *count,
                switched_to: switched_to.clone(),
            }),
            SessionEvent::TurnFailed { .. } => {
                rec.failed += 1;
                rec.ended += 1;
            }
            SessionEvent::TurnFinished { .. } => rec.ended += 1,
            SessionEvent::Warning { code, detail, .. } => {
                rec.said.push(format!("{code}: {detail}"))
            }
            _ => {}
        },
        ServerFrame::Rejected { reason, .. } => {
            panic!("the daemon refused a frame this test depends on: {reason}")
        }
        _ => {}
    }
}

/// Pump until `done`, folding every frame in. The deadline is generous - it exists so a regression
/// reads as its missing fact rather than as a hung test.
fn watch(rx: &Receiver<Inbound>, rec: &mut Rec, done: impl Fn(&Rec) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline && !done(rec) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(inb) => fold(rec, inb),
            Err(RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("the head's pump died: {e}"),
        }
    }
}

/// A session on a provider daemon, on the byte vocabulary: no GGUF, no tool, no apparatus, so this
/// file measures what the daemon publishes and nothing about the machine it runs on.
fn config(scratch: &std::path::Path, tag: &str, answer_within: Option<Duration>) -> Config {
    let ws = scratch.join(format!("ws-{tag}"));
    std::fs::create_dir_all(&ws).expect("the workspace");
    let mut cfg = Config::for_this_box(&ws);
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.unconfined = true;
    cfg.allow_bash = false;
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // The ladder is not this file's subject: one attempt, one failure, one event.
    cfg.http_retries = 0;
    cfg.context_window = Some(32_768);
    cfg.provider_timeout = answer_within;
    cfg.store = Some(scratch.join(format!("{tag}.db")));
    cfg.socket = scratch.join(format!("{tag}.sock"));
    cfg.session_id = format!("provider-trouble-{tag}");
    cfg
}

/// One session, one daemon, one head, with `body` on a thread of its own. Closing the registry is what
/// ends `daemon.run`, and it is closed even when `body` panics so a regression reads as its missing
/// fact rather than as a hung test.
fn drive<F>(parts: &Parts, cfg: Config, body: F) -> Rec
where
    F: FnOnce(&mut HeadClient, &Receiver<Inbound>) -> Rec + Send + 'static,
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
        "provider-trouble-test",
        Caps::default(),
    )
    .expect("a head attaches");
    let (tx, rx) = channel();
    std::thread::spawn(move || pump(reader, tx));
    let mut sessions =
        Sessions::open_first(parts, cfg.clone(), registry.clone()).expect("the session opens");

    let driver = {
        let registry = registry.clone();
        std::thread::spawn(move || {
            let mut client = client;
            let ran =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut client, &rx)));
            registry.close();
            match ran {
                Ok(rec) => rec,
                Err(why) => std::panic::resume_unwind(why),
            }
        })
    };

    daemon.run(&mut sessions, |_, _| {});
    let rec = driver.join().expect("the driver thread");
    daemon.shutdown();
    rec
}

/// Unix millis now, the clock `reset_ms` is measured against.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[test]
fn the_four_provider_troubles_reach_the_wire_as_types() {
    let scratch = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-provider-trouble-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(scratch.join("letibot")).expect("the scratch config dir");
    // SAFETY: this binary has one test, and everything below runs in sequence on this thread or on
    // threads it starts afterwards; nothing reads the environment concurrently with these calls.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        std::env::set_var("XDG_DATA_HOME", &scratch);
        std::env::remove_var("DEEPSEEK_API_KEY");
    }

    let fake = Fake::serve();
    std::fs::write(
        scratch.join("letibot/providers.toml"),
        // Two providers on one loopback stub, because a fallback names another of the box's models and
        // a name with no key is refused before it is ever tried.
        format!(
            "# Written by provider_trouble.rs. Nothing here is the operator's file.\n\
             [deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n\n\
             [glm]\nkey = \"sk-test\"\nurl = \"{url}\"\n",
            url = fake.url()
        ),
    )
    .expect("providers.toml");

    let probe = config(&scratch, "probe", None);
    let parts = Parts::load(&probe).expect("a provider session opens on the byte vocabulary");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    // A. A rate limit that says when it lifts: the kind, the model the round was on, and the reset
    // instant measured from `Retry-After` rather than guessed.
    let f = fake.clone();
    let rec = drive(&parts, config(&scratch, "rate", None), move |client, rx| {
        f.script(&[Act::RateLimited(Some(90))]);
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        let mut rec = Rec::default();
        watch(rx, &mut rec, |rec| rec.troubles.len() == 1);
        rec
    });
    assert_eq!(
        rec.troubles.len(),
        1,
        "one 429 is one event: {:?}",
        rec.troubles
    );
    let rate = &rec.troubles[0];
    assert_eq!(rate.kind, "rate_limit", "{rate:?}");
    assert_eq!(rate.model, "deepseek/deepseek-chat", "{rate:?}");
    assert_eq!(rate.count, 1, "the first of its kind: {rate:?}");
    let reset = rate.reset_ms.expect("the provider said when");
    let left = reset.saturating_sub(now_ms());
    assert!(
        (60_000..=120_000).contains(&left),
        "the reset is ninety seconds out, not {left} ms"
    );

    // B. The same 429 with no `Retry-After` is the same kind and NO reset: a provider that did not say
    // has not said *now*.
    let f = fake.clone();
    let rec = drive(
        &parts,
        config(&scratch, "rate-no-header", None),
        move |client, rx| {
            f.script(&[Act::RateLimited(None)]);
            client.prompt(0, PROMPT).expect("the prompt is accepted");
            let mut rec = Rec::default();
            watch(rx, &mut rec, |rec| rec.troubles.len() == 1);
            rec
        },
    );
    assert_eq!(rec.troubles.len(), 1, "{:?}", rec.troubles);
    assert_eq!(rec.troubles[0].kind, "rate_limit", "{:?}", rec.troubles);
    assert_eq!(
        rec.troubles[0].reset_ms, None,
        "a provider that said nothing has not said *now*: {:?}",
        rec.troubles[0]
    );

    // C. A 429 whose body names the account is a QUOTA: no wait lifts it, and a caller that slept on
    // it would sleep for nothing.
    let f = fake.clone();
    let rec = drive(
        &parts,
        config(&scratch, "quota", None),
        move |client, rx| {
            f.script(&[Act::Quota]);
            client.prompt(0, PROMPT).expect("the prompt is accepted");
            let mut rec = Rec::default();
            watch(rx, &mut rec, |rec| rec.troubles.len() == 1);
            rec
        },
    );
    assert_eq!(rec.troubles.len(), 1, "{:?}", rec.troubles);
    assert_eq!(rec.troubles[0].kind, "quota", "{:?}", rec.troubles);

    // D. A model API that never answers: the daemon's own deadline ends the round, and two of them in
    // a row is what `count` is for - a caller that must move a session after N needs the N.
    let f = fake.clone();
    let rec = drive(
        &parts,
        config(&scratch, "timeout", Some(Duration::from_millis(400))),
        move |client, rx| {
            let mut rec = Rec::default();
            for _ in 0..2 {
                f.script(&[Act::Silent]);
                let before = rec.ended;
                client.prompt(0, PROMPT).expect("the prompt is accepted");
                watch(rx, &mut rec, |rec| rec.ended > before);
            }
            rec
        },
    );
    let kinds: Vec<(&str, u32)> = rec
        .troubles
        .iter()
        .map(|t| (t.kind.as_str(), t.count))
        .collect();
    assert_eq!(
        kinds,
        vec![("timeout", 1), ("timeout", 2)],
        "two silent rounds are two timeouts and the second says so: {:?}",
        rec.troubles
    );
    assert_eq!(rec.failed, 2, "and both turns failed: {:?}", rec.failed);

    // E. A round that produces nothing for longer than the first-byte threshold is a stall, and it is
    // still a round that answered: the event does not mean the turn died.
    let f = fake.clone();
    let before = fake.requests();
    let rec = drive(
        &parts,
        config(&scratch, "stall", None),
        move |client, rx| {
            f.script(&[Act::Stall]);
            client.prompt(0, PROMPT).expect("the prompt is accepted");
            let mut rec = Rec::default();
            watch(rx, &mut rec, |rec| rec.troubles.len() == 1 && rec.ended > 0);
            rec
        },
    );
    assert!(
        fake.requests() > before,
        "the stall never reached the provider, so this measured nothing"
    );
    assert_eq!(rec.troubles.len(), 1, "{:?}", rec.troubles);
    assert_eq!(rec.troubles[0].kind, "stall", "{:?}", rec.troubles);
    assert_eq!(rec.troubles[0].count, 1, "{:?}", rec.troubles);
    assert_eq!(
        rec.failed, 0,
        "a stall is a slow round, not a failed one: {:?}",
        rec.troubles
    );

    // F. **A quota moves the session by itself.** The account cannot pay, a fallback list names where
    // this session goes, and the daemon makes the switch and says so: `switched_to` on the event is the
    // announcement, and the continuation's request is sent to the new model.
    let mut cfg = config(&scratch, "quota-fallback", None);
    cfg.fallback = vec!["glm/glm-4.6".to_string()];
    let f = fake.clone();
    let requests_before = fake.models().len();
    let rec = drive(&parts, cfg, move |client, rx| {
        f.script(&[Act::Quota, Act::Answer]);
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        let mut rec = Rec::default();
        // The wait is for a request that NAMES the fallback, not for one more request: the quota
        // request is already on the wire when the trouble is published, so a count of requests
        // returns before the continuation is sent and the assertion below races it.
        watch(rx, &mut rec, |rec| {
            rec.troubles.len() == 1
                && f.models()
                    .iter()
                    .skip(requests_before)
                    .any(|m| m == "glm-4.6")
        });
        rec
    });
    let after = fake.models();
    assert_eq!(rec.troubles.len(), 1, "{:?}", rec.troubles);
    assert_eq!(rec.troubles[0].kind, "quota", "{:?}", rec.troubles);
    assert_eq!(
        rec.troubles[0].switched_to.as_deref(),
        Some("glm/glm-4.6"),
        "a quota names where the session went: {:?} / warnings {:?}",
        rec.troubles,
        rec.said
    );
    assert_eq!(
        after.last().map(String::as_str),
        Some("glm-4.6"),
        "and the turn that follows is sent there: {after:?}"
    );

    // G. **Two timeouts in a row move it, and one does not.** A timeout says nothing about the model
    // on its own, so the budget is what makes it evidence: at two, the first turn stays where it is and
    // the second moves the session with the same `switched_to` announcement.
    // Three seconds and not four hundred milliseconds: the deadline still ends a silent round, and a
    // loaded box now has room to put the request on the wire before it fires.
    let mut cfg = config(&scratch, "timeout-fallback", Some(Duration::from_secs(3)));
    cfg.fallback = vec!["glm/glm-4.6".to_string()];
    cfg.fallback_after_timeouts = 2;
    let f = fake.clone();
    let requests_before = fake.models().len();
    let rec = drive(&parts, cfg, move |client, rx| {
        f.script(&[Act::Silent, Act::Silent, Act::Answer]);
        let mut rec = Rec::default();
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, &mut rec, |rec| rec.troubles.len() == 1);
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, &mut rec, |rec| {
            rec.troubles.len() == 2
                && f.models()
                    .iter()
                    .skip(requests_before)
                    .any(|m| m == "glm-4.6")
        });
        rec
    });
    let after = fake.models();
    assert_eq!(rec.troubles.len(), 2, "{:?}", rec.troubles);
    assert_eq!(
        rec.troubles[0].switched_to, None,
        "one timeout is weather, and a session that moved on it would move on every slow round: {:?}",
        rec.troubles
    );
    assert_eq!(
        rec.troubles[1].switched_to.as_deref(),
        Some("glm/glm-4.6"),
        "the second in a row is the model: {:?}",
        rec.troubles
    );
    assert_eq!(
        after.last().map(String::as_str),
        Some("glm-4.6"),
        "and the continuation is sent there: {after:?}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
