//! **One thread per session, and the turn that used to hold the daemon** — the operator's
//! requirement, in their words: *"one daemon, several sessions … give a thread to each session and
//! that is it"*.
//!
//! # The defect this pins
//!
//! Every session the daemon held used to be served by ONE worker, one command at a time across all
//! of them. MEASURED 2026-10-26 in the operator's own daemon: a prompt to session B sat queued from
//! 14:08 until 14:10:52 — exactly one second after session A's three-minute turn ended. The
//! sessions were unrelated conversations and neither was waiting for the other's answer; the wait
//! was one thread.
//!
//! # The three properties, in the order they are asked
//!
//! 1. **A held turn in one session does not delay another.** Session A's model answers only when
//!    this test releases it — a socket the test owns, which is what a three-minute turn is made of —
//!    and B's prompt is answered while A's is still held.
//! 2. **A session's own commands stay ordered.** Two prompts into the held session are answered in
//!    the order they were submitted, whatever else the daemon is doing.
//! 3. **A stop with both sessions busy ends inside its bound** and names what it could not wait for,
//!    rather than hanging on a turn nobody can interrupt.
//!
//! # One test function, on purpose
//!
//! This file sets `XDG_CONFIG_HOME`/`XDG_DATA_HOME` (the only door a session has to a non-preset
//! provider address, which is how the fake on loopback is reached), and `set_var` is only sound in a
//! process where nothing else is reading the environment. Three test functions in one binary would
//! be three of them racing over that. So the phases are numbered below, each with its own
//! assertions and its own sentence when it fails.
//!
//! # What makes this runnable anywhere
//!
//! Both sessions open on the **byte vocabulary** with a provider pointed at a loopback fake
//! (`vocab_gguf: None`), so no model file is needed and nothing leaves 127.0.0.1.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::hub::{Attached, CommandKind, Owner};
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::Registry;
use letibot_transcript::TranscriptItem;

/// The daemon's own first session — `open_first` opens it eagerly, which is what the banner needs.
const SLOW: &str = "s-held-turn";
/// The second session: a head makes it while the daemon is running, which is the path that has to
/// hand it a thread too.
const QUICK: &str = "s-quick-turn";

/// How long anything in this test may take. Nothing here is a performance claim — the held turn is
/// released by the test itself — so this is only a bound on a hang.
const PATIENCE: Duration = Duration::from_secs(30);

/// The wait the daemon's own stop uses (`cli::SUBAGENT_EXIT_GRACE`'s shape, smaller so the test is
/// quick): a turn that cannot be interrupted is what it is for.
const STOP_BOUND: Duration = Duration::from_secs(2);

#[test]
fn a_held_turn_in_one_session_does_not_delay_another() {
    let dir = TempDir::new("thread-per-session");
    let root = dir.path();
    // **Nothing of the operator's may reach these sessions**: a key in their `providers.toml`
    // would decide where a request goes, and this test's whole point is that no request leaves
    // loopback.
    std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
    // SAFETY: this binary has ONE test, and these are set before the daemon, any thread that reads
    // config, or the harness exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
    }

    let fake = Fake::serve();
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/thread_per_session.rs — a fake on loopback.\n\
             [deepseek]\nkey = \"sk-not-used\"\nurl = \"{}\"\n",
            fake.url
        ),
    )
    .expect("providers.toml");

    let workspace = root.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.http_retries = 0;
    cfg.session_id = SLOW.into();
    // The byte vocabulary and a provider — the door `activity_two_sessions.rs` takes, so this runs
    // on a machine with no model file at all.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let parts: &'static Parts = Box::leak(Box::new(
        Parts::load(&cfg).expect("the byte vocabulary needs no model file"),
    ));

    let registry = Registry::new();
    registry
        .create(SLOW.to_string(), "", Sessions::wiring(&cfg))
        .expect("the daemon's own first session");
    let socket = letibot_sessionlog::testing::socket_path(root, "harnessd.sock");
    let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");

    // **The daemon, running exactly as `cli.rs` runs it**: `open_first`, the loop, the stop, and the
    // bounded wait for every session's thread that the process's exit needs.
    let worker = {
        let cfg = cfg.clone();
        let registry = registry.clone();
        std::thread::spawn(move || {
            let mut sessions =
                Sessions::open_first(parts, cfg, registry).expect("the daemon opens its session");
            daemon.run(&mut sessions, |_, _| {});
            daemon.shutdown();
            let began = Instant::now();
            let said = sessions.drain_subagents(STOP_BOUND).sentence();
            (said, began.elapsed())
        })
    };

    // ---- 1. the sessions the daemon holds are served by their own threads ----------------
    let slow = registry.get(SLOW).expect("the first session's hub");
    let slow_head = slow.attach("tui", "tester", Caps::default(), 0);
    // **A session made WHILE the daemon is running**, which is the path that has to hand it a
    // thread as well: `Registry::create` rings the open, the daemon opens it, and its court is
    // spawned with the harness it just built.
    registry
        .create(QUICK.to_string(), "", Sessions::wiring(&cfg))
        .expect("a second session on the same daemon");
    let quick = registry.get(QUICK).expect("the second session's hub");
    let quick_head = quick.attach("tui", "tester", Caps::default(), 0);
    wait_until("both sessions to be served by their own threads", || {
        quick.owner() == Owner::OwnThread && slow.owner() == Owner::OwnThread
    });

    // ---- 2. a held turn in one session does not delay another ----------------------------
    fake.hold(Hold::Next);
    prompt(&slow, &slow_head, "req-slow", HOLD_THIS);
    wait_until("the held turn to reach the model", || fake.arrived() >= 1);

    prompt(&quick, &quick_head, "req-quick", ANSWER_WHILE_HELD);
    wait_until("the second session's answer", || answers(&quick) >= 1);
    assert_eq!(
        answers(&slow),
        0,
        "the held session answered while its turn was still held: something released the fake \
         that this test did not"
    );
    assert!(
        fake.still_held(),
        "the held request was let go before the test released it — the second session's answer \
         proves nothing about whose thread ran it"
    );
    // ---- 3. and the held session's answer arrives when the test says so -------------------
    fake.release();
    wait_until("the held session's answer", || answers(&slow) >= 1);
    assert!(
        fake.asked().iter().any(|body| body.contains(HOLD_THIS)),
        "the model was asked the held session's prompt: {:?}",
        fake.asked()
    );

    // ---- 4. a session's own commands stay ordered ----------------------------------------
    // **The second prompt is submitted while the first is inside the model call**, so it is queued
    // behind it — the case a single worker serialised by holding the daemon, and the case a thread
    // of its own must keep the ORDER of. The evidence is the order the model was asked in: whichever
    // turn the second prompt ends up in (its own, or a follow-up item of the first), it may not
    // reach the model before the first one did.
    let before = fake.arrived();
    fake.hold(Hold::Next);
    prompt(&slow, &slow_head, "req-one", MARK_ONE);
    wait_until("the first of the two prompts to reach the model", || {
        fake.arrived() > before
    });
    prompt(&slow, &slow_head, "req-two", MARK_TWO);
    wait_until("the queued prompt to be in the session's own queue", || {
        slow.has_queued_prompt()
    });
    fake.release();
    wait_until("the queued prompt to reach the model after it", || {
        fake.asked().iter().any(|body| body.contains(MARK_TWO))
    });
    let asked = fake.asked();
    let at = |mark: &str| asked.iter().position(|body| body.contains(mark));
    assert!(
        matches!((at(MARK_ONE), at(MARK_TWO)), (Some(a), Some(b)) if a < b),
        "the session's commands were not answered in the order they were submitted: {} request(s), \
         first asking {} at {:?} and second asking {} at {:?}",
        asked.len(),
        MARK_ONE,
        at(MARK_ONE),
        MARK_TWO,
        at(MARK_TWO)
    );
    wait_until("both prompts to land as the operator's own rows", || {
        let rows = slow.snapshot().items;
        [MARK_ONE, MARK_TWO].iter().all(|mark| {
            rows.iter().any(|row| match &row.item {
                Some(TranscriptItem::User { parts, .. }) => parts.iter().any(|part| {
                    matches!(part, letibot_transcript::UserPart::Text { text } if text.contains(mark))
                }),
                _ => false,
            })
        })
    });
    assert_eq!(
        answers(&quick),
        1,
        "the second session ran exactly the one prompt it was given"
    );

    // ---- 5. a stop with both sessions busy ends inside its bound -------------------------
    let before = fake.arrived();
    fake.hold(Hold::Every);
    prompt(&slow, &slow_head, "req-busy-slow", BUSY_SLOW);
    prompt(&quick, &quick_head, "req-busy-quick", BUSY_QUICK);
    wait_until("both sessions to be mid-turn", || {
        fake.arrived() >= before + 2 && slow.turn_in_flight() && quick.turn_in_flight()
    });

    // The stop, exactly as `cli.rs`'s signal path makes it: close every hub, then wait for every
    // session's thread with the bound.
    registry.close();
    let (said, waited) = worker.join().expect("the daemon's own thread");
    // **Nothing may be waited for for ever**: the bound is what keeps a Ctrl-C from hanging on a
    // turn that cannot be interrupted.
    assert!(
        waited <= STOP_BOUND + Duration::from_secs(2),
        "the stop took {waited:?} against a bound of {STOP_BOUND:?}"
    );
    // **And what it could not wait for is NAMED** — both sessions are inside a model call, so the
    // honest answer is that it gave up rather than that it waited.
    let sentence = said.expect("a stop that could not wait for two busy sessions says so");
    assert!(
        sentence.contains("gave up") && sentence.contains(SLOW) && sentence.contains(QUICK),
        "the stop's sentence names what it could not wait for: {sentence}"
    );
    // Let the two held turns finish so nothing is left inside a model call when this process exits.
    fake.release();
}

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// Three markers no prompt this test sends could produce by accident: the transcript's own system
/// message is full of ordinary English, so a search for "first" would find the prompt's preamble.
const HOLD_THIS: &str = "zz-marker-held";
const ANSWER_WHILE_HELD: &str = "zz-marker-quick";
const MARK_ONE: &str = "zz-marker-one";
const MARK_TWO: &str = "zz-marker-two";
const BUSY_SLOW: &str = "zz-marker-busy-slow";
const BUSY_QUICK: &str = "zz-marker-busy-quick";

/// The model's answers in this session's transcript — the row a head draws when the model speaks.
fn answers(hub: &Arc<letibot_sessionlog::Hub>) -> usize {
    hub.snapshot()
        .items
        .iter()
        .filter(|row| matches!(row.item, Some(TranscriptItem::Assistant { .. })))
        .count()
}

/// Submit one prompt the way a head does — the hub's own door, which is the one a session's thread
/// takes its commands from.
fn prompt(hub: &Arc<letibot_sessionlog::Hub>, head: &Attached, request: &str, text: &str) {
    let frame = hub.submit(
        &head.head_id,
        request,
        0,
        CommandKind::Prompt {
            text: text.into(),
            attachments: Vec::new(),
        },
    );
    assert!(
        !matches!(frame, letibot_sessionlog::ServerFrame::Rejected { .. }),
        "{request} was not accepted by the hub: {frame:?}"
    );
}

/// Wait for `what`, or fail saying what never happened.
fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("waited {PATIENCE:?} for {what}");
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
                .duration_since(std::time::UNIX_EPOCH)
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

/// **Which answers the fake holds** — `Next` for one, `Every` for all of them until released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    Next,
    Every,
}

/// **A fake provider on a loopback port that answers every request except the ones this test
/// holds.**
///
/// One thread per connection, and that is load-bearing rather than tidy: a fake that served its
/// connections from one thread would make the SECOND session wait behind the held first — the very
/// defect this test exists to catch, reproduced in the fixture and blamed on the daemon.
struct Fake {
    url: String,
    state: Arc<State>,
}

struct State {
    /// Every request body the fake has read, in the order it arrived.
    seen: Mutex<Vec<String>>,
    held: Mutex<HeldNow>,
    cv: Condvar,
}

#[derive(Debug, Default, Clone, Copy)]
struct HeldNow {
    /// Hold the next request to arrive, once.
    next: bool,
    /// Hold every request until released.
    every: bool,
    /// How many requests are inside the hold right now.
    out: usize,
    /// Whether the held ones have been let go.
    released: bool,
}

impl Fake {
    fn serve() -> Fake {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().unwrap().port()
        );
        let state = Arc::new(State {
            seen: Mutex::new(Vec::new()),
            held: Mutex::new(HeldNow::default()),
            cv: Condvar::new(),
        });
        let for_thread = state.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let state = for_thread.clone();
                std::thread::spawn(move || {
                    let body = read_request(&conn);
                    let (n, hold) = {
                        let mut held = state.held.lock().unwrap_or_else(|e| e.into_inner());
                        let mut seen = state.seen.lock().unwrap_or_else(|e| e.into_inner());
                        seen.push(body);
                        let hold = held.next || held.every;
                        held.next = false;
                        if hold {
                            held.out += 1;
                        }
                        (seen.len(), hold)
                    };
                    if hold {
                        let mut held = state.held.lock().unwrap_or_else(|e| e.into_inner());
                        while !held.released {
                            held = state.cv.wait(held).unwrap_or_else(|e| e.into_inner());
                        }
                        held.out = held.out.saturating_sub(1);
                    }
                    let _ = write_sse(conn, &format!("answer {n}"));
                });
            }
        });
        Fake { url, state }
    }

    /// Hold the answers `how` names.
    fn hold(&self, how: Hold) {
        let mut held = self.state.held.lock().unwrap_or_else(|e| e.into_inner());
        match how {
            Hold::Next => held.next = true,
            Hold::Every => held.every = true,
        }
        // A hold taken after a release is a new hold: the flag that let the last one go is cleared
        // here rather than at the release, so the two cannot come apart.
        if held.out == 0 {
            held.released = false;
        }
    }

    /// Let every held answer go, and hold nothing more.
    fn release(&self) {
        let mut held = self.state.held.lock().unwrap_or_else(|e| e.into_inner());
        held.next = false;
        held.every = false;
        held.released = true;
        self.state.cv.notify_all();
    }

    fn arrived(&self) -> usize {
        self.state
            .seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn asked(&self) -> Vec<String> {
        self.state
            .seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Whether a request is inside the hold right now — read so the test can say *nothing released
    /// it* about a turn that is still waiting.
    fn still_held(&self) -> bool {
        self.state
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .out
            > 0
    }
}

/// Drain one HTTP request off the socket: headers, then the body `content-length` names.
fn read_request(stream: &std::net::TcpStream) -> String {
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

/// One round as SSE — the frames `letibot-provider` parses, in the shapes
/// `crates/bridge/tests/end_to_end.rs` serves (so two fakes cannot drift on the wire bytes that
/// matter).
fn write_sse(mut stream: std::net::TcpStream, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut body = String::new();
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{"content": text}}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[],"usage":{
            "prompt_tokens": 50,
            "completion_tokens": 9,
            "prompt_cache_hit_tokens": 20,
        }})
    ));
    body.push_str("data: [DONE]\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}
