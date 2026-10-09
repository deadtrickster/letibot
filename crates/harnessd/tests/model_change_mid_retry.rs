//! **A model change during a retrying turn: the switch must take, and the turn must
//! continue.**
//!
//! # The report, and the screen it was measured on
//!
//! 2026-10-12, the operator's GLM coding plan, weekly quota exhausted. Every notice
//! on their screen read
//!
//! ```text
//! · model_endpoint_retry — the model endpoint at api.z.ai did not answer: http 429:
//!   Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-10-12 15:01:48.
//!   Taking this round again in 1s (attempt 1 of 6). Nothing was recorded, so the
//!   retry sends exactly the bytes this one did.
//! ```
//!
//! at 1, 2, 4, 8, 16, 32 s, and then
//!
//! ```text
//! · slash — /models deepseek/deepseek-flash
//!   turns go to deepseek/deepseek-flash from the next one on — METERED
//! ── FAILED — http 429: Weekly/Monthly Limit Exhausted … (nothing was recorded)
//! ```
//!
//! Their two sentences are the defect and the acceptance criterion at once: *"while
//! those backoffs were cycling - the model change didnt take"* and *"when it finally
//! applied the turn didnt continue"*. The switch was ACCEPTED and bought nothing,
//! because the command sat in the hub's queue until the worker came back — which is
//! after the turn, and the turn was already FAILED.
//!
//! # What is measured here, and against a real model
//!
//! Three sessions against two loopback stubs, through the daemon's own socket: a
//! **refusing** model that answers the operator's own 429 after a hold, and an
//! **answering** one that streams a text answer. Nothing is mocked above the socket and
//! nothing is injected into the retry policy — the point of the change is what happens
//! to a round that has already failed, and the only honest way to measure that is to let
//! one fail.
//!
//!   * **A — the switch takes mid-turn.** A prompt goes out against the refusing model;
//!     a `/models glm/glm-4.6` goes in while that round is in flight; the turn must
//!     finish with glm's answer, and the retry notice must say the model changed and
//!     that the round is being taken again there.
//!   * **B — a turn whose retries are exhausted continues.** The same, with
//!     `http_retries = 0`, so the ladder has nothing to offer and the ONLY thing that
//!     can keep the turn alive is the re-issue on the model the session moved to. It
//!     also asserts that no backoff notice was published at all: there was no ladder to
//!     walk.
//!   * **C — the model is out, and `[fallback]` names somewhere else.** No `/models`
//!     typed at all: `[fallback] models` in `providers.toml` moves the session and the
//!     round goes there. This is the half that answers the operator's standing ruling —
//!     *"it is a standard thing to do - limits, 5xx, etc. we must be able to change
//!     models like for the main session"* — and it is the reason the key exists.
//!
//! **The ledger, in all three**: exactly one user row and one assistant row, counted off
//! the hub's snapshot (rows with bodies, not a tally of events). A round that failed
//! records nothing, so a round re-issued somewhere else must not leave half of the failed
//! one behind — the invariant the retry notice's own sentence promises (*"nothing was
//! recorded"*).
//!
//! **The turn's end is proven by a second command, not by a sleep.** Every scenario
//! finishes by asking the session `/models`: the worker is inside the turn until it ends,
//! so the listing's answer cannot arrive before it. A turn that ended as FAILED still
//! answers, and then the row counts below are what fail.
//!
//! # One test, three sessions, and why not three tests
//!
//! `XDG_CONFIG_HOME` is process-global and libtest runs the tests in a binary in
//! parallel — `apparatus::present`'s own tests carry the measurement of what that costs.
//! So the scratch config dir is pointed at once, by the single test that owns this
//! process, and the three scenarios run in sequence inside it. `slash.rs` is the same
//! shape for the same reason.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;
use letibot_transcript::TranscriptItem;

/// The answer the answering stub streams. A marker, so the row can be found.
const ANSWER: &str = "the-round-went-through";

/// The prompt. Nothing tool-shaped: this file measures where a round is sent, not what
/// it does when it gets there.
const PROMPT: &str = "say hello";

/// How long the refusing model holds a request before answering 429.
///
/// **This is the operator's window**, made reachable: a round in flight with a ladder
/// about to cycle is when they typed `/models`, and a stub that answered instantly would
/// close that window before a test thread could get a line into the queue. Not a
/// synchronisation — the driver is prompt and the margin is 750 ms — but the difference
/// between a window of milliseconds and one of a second.
const REFUSAL_HOLD_MS: u64 = 900;

/// The provider's own words, as they arrived: the operator's screen, verbatim.
///
/// It matters that this is the REAL body rather than a generic 429. `http_retry_after`
/// reads it — a 429 whose body names money is refused at once, because no wait can lift a
/// quota — and this one names a reset time, which is why the ladder ran six times on their
/// screen. A test body that happened to hit the credit rule would be measuring a different
/// path.
const REFUSAL_BODY: &str = "{\"error\":{\"message\":\"Weekly/Monthly Limit Exhausted. \
                             Your limit will reset at 2026-10-12 15:01:48.\"}}";

// ---------------------------------------------------------------------------
// The two stubs
// ---------------------------------------------------------------------------

/// **A model that is OUT.** Every request is read, held, and answered `429` with the
/// provider's own body — forever, which is what *out* means.
fn refusing_model() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            read_request(&mut s);
            std::thread::sleep(Duration::from_millis(REFUSAL_HOLD_MS));
            let _ = write!(
                s,
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{REFUSAL_BODY}",
                REFUSAL_BODY.len()
            );
            let _ = s.flush();
        }
    });
    port
}

/// **A model that answers** — one text answer, streamed in the OpenAI shape the provider
/// reads.
fn answering_model() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            read_request(&mut s);
            let script = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{ANSWER}\"}}}}]}}\n\n\
                 data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
                 data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":50,\"completion_tokens\":4}}}}\n\n\
                 data: [DONE]\n\n"
            );
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{script}",
                script.len()
            );
            let _ = s.flush();
        }
    });
    port
}

/// Drain one request — headers plus the body `Content-Length` names — so the client's
/// write side is finished before the answer arrives. `run_off_worker.rs`'s reader, which
/// exists for the same reason.
fn read_request(s: &mut TcpStream) {
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
                break;
            }
        }
        if acc.len() > (1 << 20) {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/// Everything one head has seen, folded to the facts this file asserts on.
#[derive(Default)]
struct Rec {
    /// Every `model_endpoint_retry` sentence, in order — what a person read.
    retries: Vec<String>,
    /// Every `slash` answer, in order.
    slashes: Vec<String>,
    /// The session's own answer to `/models` after the turn.
    listing: String,
    /// **The rows the ledger ended up holding**, counted off the snapshot's bodies
    /// rather than off a tally of events: a row announced twice with one body is one row,
    /// and a row that landed without a body is a defect this file must not read as a
    /// success.
    user_rows: usize,
    assistant_rows: usize,
}

impl Rec {
    /// The sentence that says the round moved to another model because somebody moved the
    /// session — the one the operator's screen was missing.
    fn model_changed(&self) -> Option<&String> {
        self.retries
            .iter()
            .find(|d| d.contains("the model changed to"))
    }

    /// The sentence that says `[fallback]` moved it.
    fn fell_back(&self) -> Option<&String> {
        self.retries.iter().find(|d| d.contains("moved to"))
    }

    /// **Every notice that says the round is being waited for.** The count is the evidence
    /// for *did this turn walk the ladder at all*: scenario B is the one where the answer
    /// must be no.
    fn backoffs(&self) -> Vec<&String> {
        self.retries
            .iter()
            .filter(|d| d.contains("did not answer"))
            .collect()
    }

    /// The answers to a `/models NAME` switch — *turns go to …* — as opposed to the
    /// listing, which answers a bare `/models`.
    fn switch_answers(&self) -> Vec<&String> {
        self.slashes
            .iter()
            .filter(|d| d.contains("turns go to"))
            .collect()
    }

    /// The rows, as one assertion's worth of evidence.
    fn rows(&self) -> (usize, usize) {
        (self.user_rows, self.assistant_rows)
    }
}

enum Seen {
    Retry(String),
    Slash(String),
    Rejected(String),
    Other,
}

fn classify(frame: ServerFrame) -> Seen {
    match frame {
        ServerFrame::Event(env) => match &env.event {
            SessionEvent::Warning { code, detail, .. } if code == "model_endpoint_retry" => {
                Seen::Retry(detail.clone())
            }
            SessionEvent::Warning { code, detail, .. } if code == "slash" => {
                Seen::Slash(detail.clone())
            }
            _ => Seen::Other,
        },
        ServerFrame::Rejected { reason, .. } => Seen::Rejected(reason.clone()),
        _ => Seen::Other,
    }
}

/// Fold one frame in. A refused frame panics: every frame this driver sends is one the
/// premise depends on, and a rejection silently awaited would read as a timeout rather
/// than as what it is.
fn fold(rec: &mut Rec, inb: Inbound) {
    match classify(Inbound::frame(inb)) {
        Seen::Retry(detail) => rec.retries.push(detail),
        Seen::Slash(detail) => rec.slashes.push(detail),
        Seen::Rejected(reason) => {
            panic!("the daemon refused a frame this test depends on: {reason}")
        }
        Seen::Other => {}
    }
}

/// Pump until `done`, folding every frame in. The deadline is generous — it exists so a
/// regression reads as its missing fact rather than as a hung test.
fn watch(rx: &Receiver<Inbound>, rec: &mut Rec, done: impl Fn(&Rec) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline && !done(rec) {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(inb) => fold(rec, inb),
            Err(RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("the head's pump died: {e}"),
        }
    }
}

/// **The ledger's own rows, counted off the snapshot** — and it is the snapshot rather
/// than an event tally on purpose. A row is announced once and its body arrives in a
/// second event; a retry that recorded half a round would show up here as a row with no
/// body, or as two rows for one answer, and either is what this file is checking for.
fn count_rows(hub: &Hub) -> (usize, usize) {
    let snap = hub.snapshot();
    let items: Vec<&TranscriptItem> = snap.items.iter().filter_map(|i| i.item.as_ref()).collect();
    let users = items
        .iter()
        .filter(|i| {
            matches!(
                i,
                TranscriptItem::User { parts, .. }
                    if parts.iter().any(|p| matches!(p, letibot_transcript::UserPart::Text { text } if text == PROMPT))
            )
        })
        .count();
    let answers = items
        .iter()
        .filter(|i| matches!(i, TranscriptItem::Assistant { text, .. } if text.contains(ANSWER)))
        .count();
    (users, answers)
}

/// **A config for one session of this file**, on the box's own vocabulary.
fn config(
    scratch: &std::path::Path,
    gguf: &std::path::Path,
    tag: &str,
    provider: &str,
    model: &str,
) -> Config {
    let ws = scratch.join("ws");
    std::fs::create_dir_all(&ws).expect("the workspace");
    let mut cfg = Config::for_this_box(&ws);
    // **Not this box's configuration** (`wired.rs`'s rule): no web search, no permission
    // file, no seat that asks a person anything.
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    cfg.vocab_gguf = Some(gguf.to_path_buf());
    cfg.session_id = format!("model-change-{tag}");
    cfg.socket = scratch.join(format!("{tag}.sock"));
    // **A store, because the choice is written to the session row** — that is half of what
    // a switch is (`Harness::persist_provider_choice`), and a session without one would
    // let this file pass while that half did nothing.
    cfg.store = Some(scratch.join(format!("{tag}.db")));
    cfg.provider = Some(ProviderConfig {
        name: provider.to_string(),
        model: Some(model.to_string()),
        // From `providers.toml`, which this file writes: the same door a daemon uses, so
        // the URL that decides where the round is POSTed is the one in the file.
        api_key: None,
        thinking: false,
    });
    cfg
}

/// **One session, one daemon, one head** — the arrangement `run_off_worker.rs` uses, with
/// the driver on a thread of its own and the worker loop on this one.
///
/// `body` runs on the driver thread with the head, the pump and the hub. When it returns,
/// this asks the session `/models` — **the turn's own end**, since the worker is inside
/// the turn until it finishes — and only then closes the registry, which is what ends
/// `daemon.run`.
fn drive<F>(parts: &Parts, cfg: Config, body: F) -> (Rec, Arc<Hub>, Config)
where
    F: FnOnce(&mut HeadClient, &Receiver<Inbound>, &Arc<Hub>, &mut Rec) + Send + 'static,
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
        "model-change-test",
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
            // **The registry is closed even if the driver panics**, and that is not
            // tidiness: a panic here would otherwise leave `daemon.run` blocked on a bell
            // nobody rings, and a regression would read as a hung test rather than as the
            // missing fact it is. The panic is re-raised below, after the close.
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                body(&mut client, &rx, &hub, &mut rec);
                let before = rec.slashes.len();
                client
                    .slash(0, "models")
                    .expect("the listing frame goes out");
                let deadline = Instant::now() + Duration::from_secs(30);
                while Instant::now() < deadline && rec.slashes.len() == before {
                    match rx.recv_timeout(Duration::from_millis(500)) {
                        Ok(inb) => fold(&mut rec, inb),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(e) => panic!("the head's pump died: {e}"),
                    }
                }
                rec.listing = rec.slashes.get(before).cloned().unwrap_or_default();
            }));
            registry.close();
            if let Err(why) = ran {
                std::panic::resume_unwind(why);
            }
            rec
        })
    };

    daemon.run(&mut sessions, |_, _, _| {});
    let mut rec = driver.join().expect("the driver thread");
    let (users, answers) = count_rows(&hub);
    rec.user_rows = users;
    rec.assistant_rows = answers;
    daemon.shutdown();
    (rec, hub, cfg)
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// The whole of the change, measured once, in one process.
#[test]
fn a_model_change_during_a_retrying_turn_takes_and_the_turn_continues() {
    let Some(gguf) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    // The operator's `providers.toml` must not be touched: point the config home at a
    // scratch directory for this process, and take opencode's key store out of reach so
    // the keys this file writes are the ones that resolve.
    let scratch = std::env::temp_dir().join(format!(
        "letibot-model-change-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(scratch.join("letibot")).expect("the scratch config dir");
    // SAFETY: this test binary has one test, and everything below runs in sequence on this
    // thread or on threads it starts afterwards; nothing reads the environment
    // concurrently with these two calls.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        std::env::set_var("XDG_DATA_HOME", &scratch);
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
    }

    let out_port = refusing_model();
    let glm_port = answering_model();
    let providers = format!(
        "# Written by model_change_mid_retry.rs. Nothing here is the operator's file.\n\
         [deepseek]\nkey = \"sk-test\"\n\
         url = \"http://127.0.0.1:{out_port}/v1/chat/completions\"\n\n\
         [glm]\nkey = \"sk-test\"\n\
         url = \"http://127.0.0.1:{glm_port}/v1/chat/completions\"\n\n\
         [fallback]\nmodels = [\"glm/glm-4.6\"]\n"
    );
    std::fs::write(scratch.join("letibot/providers.toml"), providers).expect("the config file");

    let first = config(&scratch, &gguf, "ladder", "deepseek", "deepseek-chat");
    let parts = Parts::load(&first).expect("the vocabulary loads");
    // The operator's per-project mode is not this test's business (`wired.rs`'s rule).
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    // ── A. The switch takes mid-turn, and the round goes there ────────────────
    //
    // The default ladder is left in place (six attempts, doubling from a second): this is
    // the operator's own configuration, and the assertion is that a switch does not have to
    // sit out — or wait behind — a ladder that has nothing to offer it. The switch goes in
    // after the first backoff notice has landed, which is the window their screen showed.
    let cfg = config(&scratch, &gguf, "ladder", "deepseek", "deepseek-chat");
    let (rec, _hub, _cfg) = drive(&parts, cfg, |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        // **The switch goes in while the ladder is cycling** — the operator's own window,
        // and it is reached deterministically rather than by sleeping: the first backoff
        // notice is the proof that the round has failed once and the loop is waiting, and
        // the wait it announces is a second long.
        watch(rx, rec, |rec| !rec.backoffs().is_empty());
        assert!(
            !rec.backoffs().is_empty(),
            "the ladder never started, so this scenario measured nothing: {:?}",
            rec.retries
        );
        client
            .slash(0, "models glm/glm-4.6")
            .expect("the switch is accepted");
        watch(rx, rec, |rec| {
            rec.model_changed().is_some() && !rec.switch_answers().is_empty()
        });
    });
    let said = rec
        .model_changed()
        .unwrap_or_else(|| {
            panic!(
                "the retry never said the model changed, so the switch never reached the \
                 turn — the defect this file exists for. Notices: {:?}",
                rec.retries
            )
        })
        .clone();
    assert!(
        said.contains("glm/glm-4.6") && said.contains("taking this round again NOW"),
        "the notice must say which model the round is being taken on, and that nothing is \
         waited for: {said}"
    );
    assert_eq!(
        rec.switch_answers().len(),
        1,
        "the switch is answered once, by whichever side applied it: {:?}",
        rec.slashes
    );
    assert!(
        rec.listing.contains("now answering: glm/glm-4.6 (metered)"),
        "and the session itself says where it is now: {}",
        rec.listing
    );
    assert_eq!(
        rec.rows(),
        (1, 1),
        "one round's worth of rows and no more — the round that failed on the way left \
         nothing behind"
    );

    // ── B. The ladder is spent, and the turn continues anyway ────────────────
    //
    // `http_retries = 0`: there is no backoff to walk, so the ONLY thing that can keep
    // this turn alive is the re-issue on the model the session moved to. This is the
    // operator's *"when it finally applied the turn didnt continue"* — the arm that used
    // to `break Err`.
    let mut cfg = config(&scratch, &gguf, "spent", "deepseek", "deepseek-chat");
    cfg.http_retries = 0;
    let (rec, _hub, _cfg) = drive(&parts, cfg, |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        // There is no notice to wait for — no ladder means no notice — so this is the
        // 150 ms/900 ms margin `run_off_worker.rs` uses for its own window: the round is in
        // flight, and the switch is in the queue before the first failure is handled.
        std::thread::sleep(Duration::from_millis(150));
        client
            .slash(0, "models glm/glm-4.6")
            .expect("the switch is accepted");
        watch(rx, rec, |rec| {
            rec.model_changed().is_some() && !rec.switch_answers().is_empty()
        });
    });
    let said = rec
        .model_changed()
        .unwrap_or_else(|| {
            panic!(
                "a turn whose retries are exhausted did not continue on the model the session \
                 had moved to. Notices: {:?}",
                rec.retries
            )
        })
        .clone();
    assert!(
        said.contains("glm/glm-4.6"),
        "and it names the model: {said}"
    );
    assert!(
        rec.backoffs().is_empty(),
        "there is no ladder with http_retries = 0, so nothing may have been waited for: {:?}",
        rec.backoffs()
    );
    assert!(
        rec.listing.contains("now answering: glm/glm-4.6 (metered)"),
        "the session is on the model the round was re-issued on: {}",
        rec.listing
    );
    assert_eq!(rec.rows(), (1, 1), "one round's worth of rows and no more");

    // ── C. The model is out, and `[fallback]` says where else ────────────────
    //
    // No `/models` at all. The list is read from the file this test wrote, through the
    // same reader a daemon uses, and the round is re-issued on the first name that
    // answers.
    let mut cfg = config(&scratch, &gguf, "fallback", "deepseek", "deepseek-chat");
    cfg.http_retries = 0;
    cfg.fallback =
        letibot_provider::keys::fallback_models(None).expect("the fallback list is readable");
    assert_eq!(
        cfg.fallback,
        vec!["glm/glm-4.6".to_string()],
        "the key this test wrote is the key the daemon reads"
    );
    let (rec, _hub, _cfg) = drive(&parts, cfg, |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        watch(rx, rec, |rec| rec.fell_back().is_some());
    });
    let said = rec
        .fell_back()
        .unwrap_or_else(|| {
            panic!(
                "the model was out and [fallback] named somewhere else, and the turn ended \
                 anyway. Notices: {:?}",
                rec.retries
            )
        })
        .clone();
    assert!(
        said.contains("glm/glm-4.6") && said.contains("[fallback]"),
        "the notice must name where it went and where that came from: {said}"
    );
    assert!(
        rec.listing.contains("now answering: glm/glm-4.6 (metered)"),
        "and the session stays there, which is what makes the move a switch rather than a \
         detour: {}",
        rec.listing
    );
    assert_eq!(
        rec.rows(),
        (1, 1),
        "one round's worth of rows and no more — the round that failed on the way left \
         nothing behind"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
