//! **A model switch takes effect when the turn ends, never inside one.**
//!
//! # The report, and the screen it was measured on
//!
//! 2026-10-26 14:36, the operator's Stroppy session, `glm-coding/glm-5.3` → `deepseek/deepseek-flash`:
//!
//! ```text
//! slash · /models deepseek/deepseek-flash   turns go to deepseek/deepseek-flash from the next one on…
//! model_endpoint_retry · the model changed to deepseek/deepseek-flash (metered) while this round was
//!   failing at glm-coding/glm-5.3 (metered)…
//! ! model_switch_refused · the first round on deepseek/deepseek-flash was refused by the provider (http 400)
//! FAILED — http 400: The `reasoning_content` in the thinking mode must be passed back to the API.
//! ```
//!
//! The daemon's request dump (`harnessd-2000f08acfaa.log`, request `9f2a5669…`) shows why: the round
//! was re-issued on deepseek carrying the rest of GLM's turn — a run of assistant tool-call rounds
//! since the last user message, most with no `reasoning_content`. Deepseek's thinking mode requires
//! reasoning to be passed back for the tool-call rounds of the CURRENT turn, and is right to: those
//! rounds are another model's. Their words for the rule: **"we can switch after turn ends"**.
//!
//! # What is measured here, and against a real daemon
//!
//! Sessions against two loopback stubs, through the daemon's own socket. Nothing is mocked above the
//! socket and nothing is injected into the retry policy: the point is what happens to a turn that is
//! already running when the switch arrives, and the only honest way to measure that is to let one
//! run and fail.
//!
//!   * **A — a switch during a running turn.** The prompt goes to the OLD model, which answers its
//!     first round with a tool call and refuses the second; `/models glm/glm-4.6` goes in while that
//!     second round is failing. The turn stays on the old model and ends FAILED; the switch is said
//!     as *"turns go to glm/glm-4.6 when this turn ends"*, then applied. The next prompt's request
//!     is the FIRST thing the new model ever sees, and what it carries is the assertion that
//!     matters: the old turn's tool-call rounds are in the history, the last message is a USER
//!     message, and there is no assistant tool-call round after it.
//!   * **B — a switch during a failing round's retries.** `http_retries = 1`, so the ladder takes the
//!     round again: every attempt is still sent to the old model, and the new one is sent nothing at
//!     all until the next prompt. This is the *"the model changed … while this round was failing"*
//!     line being unmade.
//!   * **C — `[fallback]` is the same rule for the daemon's own switch.** No `/models` at all: the
//!     model is out, `[fallback] models` names where this session goes, and it goes there when the
//!     turn ends rather than being re-issued there.
//!   * **D — an interrupted turn applies the pending switch.** A switch recorded during a failing
//!     turn's ladder survives the operator's own interrupt: the turn ends the other way, and the
//!     switch still lands.
//!
//! # One test, four sessions, and why not four tests
//!
//! `XDG_CONFIG_HOME` is process-global and libtest runs the tests in a binary in parallel —
//! `apparatus::present`'s own tests carry the measurement of what that costs. So the scratch config
//! dir is pointed at once, by the single test that owns this process, and the four scenarios run in
//! sequence inside it. `slash.rs` is the same shape for the same reason.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;
use letibot_transcript::{TranscriptItem, UserPart};

/// The answer the new model streams. A marker, so the row can be found.
const ANSWER: &str = "the-round-went-through";

/// The first prompt. Nothing tool-shaped in it: where a round is sent is this file's subject.
const PROMPT: &str = "say hello";

/// The second prompt — **the user message the first request on the new model must follow**.
const NEXT: &str = "now carry on";

/// The file the old model's tool round reads, so that turn has a real tool-call round in its
/// history. Its contents do not matter; that the round EXISTS is the point.
const TOOL_FILE: &str = "notes.txt";

/// How long the old model holds a request before answering `429`.
///
/// **This is the operator's window**, made reachable: a round in flight when they typed `/models`,
/// and a stub that answered instantly would close that window before a test thread could get a line
/// into the queue. Not a synchronisation — the driver waits on the stub's own request count — but
/// the difference between a window of milliseconds and one of most of a second.
const REFUSAL_HOLD_MS: u64 = 900;

/// The provider's own words, as they arrived. `http_retry_after` reads this: a 429 whose body names
/// money is refused at once, because no wait can lift a quota, and this one names a reset time.
const REFUSAL_BODY: &str = "{\"error\":{\"message\":\"Weekly/Monthly Limit Exhausted. \
                             Your limit will reset at 2026-10-12 15:01:48.\"}}";

// ---------------------------------------------------------------------------
// The two stubs
// ---------------------------------------------------------------------------

/// One loopback model, plus every request body it was sent.
///
/// The bodies are the evidence: `model` says which model the harness believed it was talking to,
/// and `messages` says what the model was asked to continue — which is the whole of what deepseek
/// refused. `Clone` is cheap and shares the recorder: the stub's thread holds one handle and each
/// scenario's driver thread takes another.
#[derive(Clone)]
struct Model {
    port: u16,
    bodies: Arc<Mutex<Vec<String>>>,
    /// Requests seen since the last [`Model::forget`]. The old model's FIRST one is answered with a
    /// tool call and every later one with a 429, so this is what makes each scenario's turn start
    /// from the same place.
    seen: Arc<AtomicU64>,
}

impl Model {
    fn count(&self) -> usize {
        self.bodies.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn sent(&self) -> Vec<String> {
        self.bodies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Back to the state a new daemon finds: no requests recorded, and the next one is the first.
    fn forget(&self) {
        self.bodies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.seen.store(0, Ordering::SeqCst);
    }
}

/// **What a stub does with one request**, from `model`'s script — the nth request gets the nth
/// entry, and the last entry repeats for ever.
#[derive(Clone, Copy)]
enum Act {
    /// A `read` tool call: a round that runs a real tool and asks the model again after it.
    ToolCall,
    /// The provider's own 429, after [`REFUSAL_HOLD_MS`] — the operator's quota, and the window a
    /// `/models` is typed in.
    Refuse,
    /// One text answer, all at once.
    Answer,
    /// **An answer dribbled out one delta at a time**, slowly enough that a steer arrives between
    /// two of them. An urgent steering message is only acted on where a delta is delivered (a
    /// refused round delivers none), so this is what makes an interrupted turn reachable at all.
    Slow,
}

/// **A loopback model with a script** — see [`Act`].
fn model(script: &'static [Act]) -> Model {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    let bodies: Arc<Mutex<Vec<String>>> = Default::default();
    let seen = bodies.clone();
    let n: Arc<AtomicU64> = Default::default();
    let counter = n.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let body = read_request(&mut s);
            seen.lock().unwrap_or_else(|e| e.into_inner()).push(body);
            let at = counter.fetch_add(1, Ordering::SeqCst) as usize;
            let act = script[at.min(script.len() - 1)];
            match act {
                Act::ToolCall => {
                    let body = format!(
                        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_1\",\
                         \"function\":{{\"name\":\"read\",\"arguments\":\"{{\\\"path\\\":\\\"{TOOL_FILE}\\\"}}\"}}}}]}}}}]}}\n\n\
                         data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n\
                         data: [DONE]\n\n"
                    );
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.flush();
                }
                Act::Refuse => {
                    std::thread::sleep(Duration::from_millis(REFUSAL_HOLD_MS));
                    let _ = write!(
                        s,
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{REFUSAL_BODY}",
                        REFUSAL_BODY.len()
                    );
                    let _ = s.flush();
                }
                Act::Answer => {
                    let body = format!(
                        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{ANSWER}\"}}}}]}}\n\n\
                         data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
                         data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":50,\"completion_tokens\":4}}}}\n\n\
                         data: [DONE]\n\n"
                    );
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.flush();
                }
                Act::Slow => {
                    // The whole body is announced and then dribbled out: a streaming reader takes
                    // each line as it lands, and the harness's steer is polled once per delta.
                    let mut body = String::new();
                    for i in 0..24 {
                        body.push_str(&format!(
                            "data: {{\"choices\":[{{\"delta\":{{\"content\":\" tick {i}\"}}}}]}}\n\n"
                        ));
                    }
                    body.push_str(
                        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                         data: [DONE]\n\n",
                    );
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.flush();
                    for line in body.split_inclusive("\n\n") {
                        if write!(s, "{line}").is_err() || s.flush().is_err() {
                            // The interrupt closed the connection. Nothing to answer.
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        }
    });
    Model {
        port,
        bodies,
        seen: n,
    }
}

/// **The model every scenario begins on**: one tool round, then the operator's own 429 for ever.
fn old_model() -> Model {
    model(&[Act::ToolCall, Act::Refuse])
}

/// **The model a switch moves to**: one plain answer.
fn new_model() -> Model {
    model(&[Act::Answer])
}

/// **The model an INTERRUPT is measured on**: the same tool round and refusal, and then a slow
/// answer — so the operator's interrupt arrives at a round that is generating and is heard there.
fn slow_model() -> Model {
    model(&[Act::ToolCall, Act::Refuse, Act::Slow])
}

/// Drain one request — headers plus the body `Content-Length` names — and answer its body, so the
/// client's write side is finished before the reply arrives.
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
// What one request says about the model it was sent to
// ---------------------------------------------------------------------------

/// **The facts about one request this file asserts on** — read off the body the harness actually
/// sent, not off the daemon's intent.
struct Sent {
    /// What `model` said: the identity the harness believed it was talking to.
    model: String,
    /// The roles, in order, as the provider received them.
    roles: Vec<String>,
    /// **Is this request ending inside a run of assistant tool-call rounds?** That is the shape
    /// deepseek's thinking mode refuses — another model's half-finished turn — and the reason the
    /// switch waits for the turn to end.
    ends_mid_tool_run: bool,
    /// The last message's text: the operator's prompt for a turn they started, and the daemon's
    /// `[fallback]` notice for the turn it started itself.
    last_text: String,
}

fn sent(body: &str) -> Sent {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let model = v["model"].as_str().unwrap_or_default().to_string();
    let msgs = v["messages"].as_array().cloned().unwrap_or_default();
    let roles: Vec<String> = msgs
        .iter()
        .map(|m| m["role"].as_str().unwrap_or("?").to_string())
        .collect();
    // **The last message's own words** — for the continuation, that is the notice the daemon
    // wrote, which is the only thing that says where the work is being picked up from.
    let last_text = msgs
        .last()
        .map(|m| match &m["content"] {
            serde_json::Value::String(s) => s.clone(),
            _ => String::new(),
        })
        .unwrap_or_default();
    let after_last_user = match msgs.iter().rposition(|m| m["role"] == "user") {
        Some(i) => &msgs[i + 1..],
        None => &msgs[..],
    };
    let ends_mid_tool_run = after_last_user.iter().any(|m| {
        m["role"] == "assistant"
            && m["tool_calls"]
                .as_array()
                .is_some_and(|calls| !calls.is_empty())
    });
    Sent {
        model,
        roles,
        ends_mid_tool_run,
        last_text,
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
    /// Every `model_switch_pending` sentence: *when* the switch will take.
    pending: Vec<String>,
    /// Every `model_switch_applied` sentence.
    applied: Vec<String>,
    /// Every `model_switch_not_applied` sentence — there must be none.
    refused: Vec<String>,
    /// Every `slash` answer, in order.
    slashes: Vec<String>,
    /// The session's own answer to `/models` after the scenario.
    listing: String,
    /// Turns the daemon reported as failed.
    failed: usize,
    /// **Why each of those turns failed**, in the daemon's words — the interrupt's own name is in
    /// here when the turn ended on the operator's word rather than on a provider's refusal.
    failures: Vec<String>,
}

impl Rec {
    /// The sentence that says the switch is waiting for this turn: the operator's own words.
    fn when_it_ends(&self) -> Option<&String> {
        self.pending.first()
    }

    /// The sentence that says `[fallback]` moved the session.
    fn fell_back(&self) -> Option<&String> {
        self.retries.iter().find(|d| d.contains("[fallback]"))
    }

    /// **Every notice that says the round is being waited for** — the evidence a retry happened.
    fn backoffs(&self) -> Vec<&String> {
        self.retries
            .iter()
            .filter(|d| d.contains("did not answer"))
            .collect()
    }

    /// The answers to a `/models NAME` switch — *turns go to …* — as opposed to the listing.
    fn switch_answers(&self) -> Vec<&String> {
        self.slashes
            .iter()
            .filter(|d| d.contains("turns go to"))
            .collect()
    }
}

enum Seen {
    Retry(String),
    Pending(String),
    Applied(String),
    NotApplied(String),
    TurnFailed(String),
    Slash(String),
    Rejected(String),
    Other,
}

fn classify(frame: ServerFrame) -> Seen {
    match frame {
        ServerFrame::Event(env) => match &env.event {
            SessionEvent::Warning { code, detail, .. } => match code.as_str() {
                "model_endpoint_retry" => Seen::Retry(detail.clone()),
                "model_switch_pending" => Seen::Pending(detail.clone()),
                "model_switch_applied" => Seen::Applied(detail.clone()),
                "model_switch_not_applied" => Seen::NotApplied(detail.clone()),
                "slash" => Seen::Slash(detail.clone()),
                _ => Seen::Other,
            },
            SessionEvent::TurnFailed { error, .. } => Seen::TurnFailed(error.clone()),
            _ => Seen::Other,
        },
        ServerFrame::Rejected { reason, .. } => Seen::Rejected(reason.clone()),
        _ => Seen::Other,
    }
}

/// Fold one frame in. A refused frame panics: every frame this driver sends is one the premise
/// depends on, and a rejection silently awaited would read as a timeout rather than as what it is.
fn fold(rec: &mut Rec, inb: Inbound) {
    match classify(Inbound::frame(inb)) {
        Seen::Retry(detail) => rec.retries.push(detail),
        Seen::Pending(detail) => rec.pending.push(detail),
        Seen::Applied(detail) => rec.applied.push(detail),
        Seen::NotApplied(detail) => rec.refused.push(detail),
        Seen::TurnFailed(error) => {
            rec.failed += 1;
            rec.failures.push(error);
        }
        Seen::Slash(detail) => rec.slashes.push(detail),
        Seen::Rejected(reason) => {
            panic!("the daemon refused a frame this test depends on: {reason}")
        }
        Seen::Other => {}
    }
}

/// Pump until `done`, folding every frame in. The deadline is generous — it exists so a regression
/// reads as its missing fact rather than as a hung test.
fn watch(rx: &Receiver<Inbound>, rec: &mut Rec, done: impl Fn(&Rec) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !done(rec) {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(inb) => fold(rec, inb),
            Err(RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("the head's pump died: {e}"),
        }
    }
}

/// **Wait for a fact about the STUB, not about the socket** — the request counts are the only thing
/// in this file that moves on the model's side of the wire, and every scenario's window is opened by
/// one of them.
fn until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("waited 30s for {what}");
}

/// **Is the model's answer a row yet** — the proof a turn finished successfully, read off the
/// daemon's own snapshot rather than off an event tally.
fn answered(hub: &Hub) -> bool {
    hub.snapshot()
        .items
        .iter()
        .filter_map(|i| i.item.as_ref())
        .any(|i| matches!(i, TranscriptItem::Assistant { text, .. } if text.contains(ANSWER)))
}

/// **Every user row's speaker and first words**, off the daemon's snapshot — the transcript is
/// where "whose words are these" is recorded, and a notice the daemon wrote must not read as the
/// operator's (`Speaker::Agent`, R42).
fn user_rows(hub: &Hub) -> Vec<(String, String)> {
    hub.snapshot()
        .items
        .iter()
        .filter_map(|i| i.item.as_ref())
        .filter_map(|i| match i {
            TranscriptItem::User { speaker, parts } => Some((
                speaker.as_str().to_string(),
                parts
                    .iter()
                    .find_map(|p| match p {
                        UserPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default(),
            )),
            _ => None,
        })
        .collect()
}

/// **Does the transcript carry the daemon's `[fallback]` notice, in the harness's own voice?**
fn carries_the_daemons_notice(hub: &Hub) -> bool {
    user_rows(hub)
        .iter()
        .any(|(speaker, text)| speaker == "agent" && text.starts_with("[fallback] "))
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
    // The file the tool round reads, so the turn has a real tool-call round behind it.
    if !ws.join(TOOL_FILE).exists() {
        std::fs::write(ws.join(TOOL_FILE), "the file a tool round reads\n").expect("the file");
    }
    let mut cfg = Config::for_this_box(&ws);
    // **Not this box's configuration** (`wired.rs`'s rule): no web search, no permission file, no
    // seat that asks a person anything.
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    cfg.vocab_gguf = Some(gguf.to_path_buf());
    cfg.session_id = format!("model-switch-{tag}");
    cfg.socket = scratch.join(format!("{tag}.sock"));
    // **A store, because the choice is written to the session row** — that is half of what a switch
    // is (`Harness::persist_provider_choice`), and a session without one would let this file pass
    // while that half did nothing.
    cfg.store = Some(scratch.join(format!("{tag}.db")));
    cfg.provider = Some(ProviderConfig {
        name: provider.to_string(),
        model: Some(model.to_string()),
        // From `providers.toml`, which this file writes: the same door a daemon uses, so the URL
        // that decides where the round is POSTed is the one in the file.
        api_key: None,
        thinking: false,
    });
    // **No ladder unless a scenario asks for one.** A 429 that names a reset time is not something
    // to wait out: the turn ends, which is what these scenarios need to see.
    cfg.http_retries = 0;
    cfg
}

/// **One session, one daemon, one head** — the arrangement `run_off_worker.rs` uses, with the
/// driver on a thread of its own and the worker loop on this one.
///
/// `body` runs on the driver thread with the head, the pump and the hub. When it returns, this asks
/// the session `/models` — **the turn's own end**, since the worker is inside the turn until it
/// finishes — and only then closes the registry, which is what ends `daemon.run`.
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
        "model-switch-test",
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
            // **The registry is closed even if the driver panics**, and that is not tidiness: a
            // panic here would otherwise leave `daemon.run` blocked on a bell nobody rings, and a
            // regression would read as a hung test rather than as the missing fact it is.
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                body(&mut client, &rx, &hub, &mut rec);
                let before = rec.slashes.len();
                client
                    .slash(0, "models")
                    .expect("the listing frame goes out");
                let deadline = Instant::now() + Duration::from_secs(30);
                while Instant::now() < deadline && rec.slashes.len() == before {
                    match rx.recv_timeout(Duration::from_millis(250)) {
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
    let rec = driver.join().expect("the driver thread");
    daemon.shutdown();
    (rec, hub, cfg)
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// The whole of the rule, measured once, in one process.
#[test]
fn a_model_switch_takes_effect_when_the_turn_ends() {
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // The operator's `providers.toml` must not be touched: point the config home at a scratch
    // directory for this process, and take the keys this file writes out of anybody else's reach.
    let scratch = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-model-at-turn-end-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    std::fs::create_dir_all(scratch.join("letibot")).expect("the scratch config dir");
    // SAFETY: this test binary has one test, and everything below runs in sequence on this thread or
    // on threads it starts afterwards; nothing reads the environment concurrently with these calls.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        std::env::set_var("XDG_DATA_HOME", &scratch);
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
    }

    let old = old_model();
    let new = new_model();
    let slow = slow_model();
    let providers = format!(
        "# Written by model_switch_at_turn_end.rs. Nothing here is the operator's file.\n\
         [deepseek]\nkey = \"sk-test\"\n\
         url = \"http://127.0.0.1:{}/v1/chat/completions\"\n\n\
         [glm]\nkey = \"sk-test\"\n\
         url = \"http://127.0.0.1:{}/v1/chat/completions\"\n\n\
         [glm-coding]\nkey = \"sk-test\"\n\
         url = \"http://127.0.0.1:{}/v1/chat/completions\"\n\n\
         [fallback]\nmodels = [\"glm/glm-4.6\"]\n",
        old.port, new.port, slow.port
    );
    std::fs::write(scratch.join("letibot/providers.toml"), providers).expect("the config file");

    let first = config(&scratch, &gguf, "probe", "deepseek", "deepseek-chat");
    let parts = Parts::load(&first).expect("the vocabulary loads");
    // The operator's per-project mode is not this test's business (`wired.rs`'s rule).
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    // ── A. The switch waits for the turn, and the next turn is the first request on the new model
    //
    // The old model runs one round with a tool call and then refuses; `/models glm/glm-4.6` goes in
    // while that second round is in flight, which is the operator's own window.
    old.forget();
    new.forget();
    let cfg = config(&scratch, &gguf, "window", "deepseek", "deepseek-chat");
    let (o, _n) = (old.clone(), new.clone());
    let (rec, _hub, _cfg) = drive(&parts, cfg, move |client, rx, hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        // **The switch goes in while the refusing round is in flight.** The stub's own request count
        // is the signal: one was the tool round, two means the round that will fail has arrived and
        // is being held.
        until("the refusing round to arrive", || o.count() >= 2);
        client
            .slash(0, "models glm/glm-4.6")
            .expect("the switch is accepted");
        // **It is not applied.** The sentence says when, and the turn goes on to its own end.
        watch(rx, rec, |rec| rec.when_it_ends().is_some());
        // And the switch lands where the turn did — after it, on the way out.
        watch(rx, rec, |rec| !rec.applied.is_empty());
        // The next turn, and the first thing the new model is ever sent.
        client
            .prompt(0, NEXT)
            .expect("the second prompt is accepted");
        until("the new model's answer", || answered(hub));
    });
    let said = rec
        .when_it_ends()
        .unwrap_or_else(|| {
            panic!(
                "the switch was never said to be waiting for the turn. Notices: retries {:?}, \
                 slashes {:?}",
                rec.retries, rec.slashes
            )
        })
        .clone();
    assert!(
        said.contains("glm/glm-4.6") && said.contains("when this turn ends"),
        "the pending sentence must name the model and when it takes: {said}"
    );
    assert!(
        said.contains("deepseek/deepseek-chat"),
        "and the model the turn is finishing on, which is the half the operator could not see: \
         {said}"
    );
    assert!(
        !rec.applied.is_empty(),
        "the switch never landed once the turn ended: {:?}",
        rec.slashes
    );
    assert!(
        rec.refused.is_empty(),
        "the switch was refused: {:?}",
        rec.refused
    );
    // **The verb's own reply says when it takes**, and is answered inside the turn — which is the
    // half of the 2026-10-12 change worth keeping now that the switch itself waits.
    assert_eq!(
        rec.switch_answers().len(),
        1,
        "the switch is answered once, by the turn that took it: {:?}",
        rec.slashes
    );
    assert!(
        rec.switch_answers()[0].contains("when this turn ends"),
        "and the answer says when: {:?}",
        rec.switch_answers()
    );
    // **The turn it was typed in stayed on the old model.** No retry notice, because nothing was
    // re-issued anywhere: the round failed on the model the turn started on.
    let old_sent: Vec<Sent> = old.sent().iter().map(|b| sent(b)).collect();
    assert!(
        old_sent.iter().all(|s| s.model == "deepseek-chat"),
        "every request of that turn went to the model it started on: {:?}",
        old_sent.iter().map(|s| &s.model).collect::<Vec<_>>()
    );
    assert!(
        rec.retries.iter().all(|d| !d.contains("the model changed")),
        "the retry loop announced a model change: {:?}",
        rec.retries
    );
    // **And the new model's first request is the next turn's, not the rest of this one.** This is
    // the assertion the whole change exists for: the history it inherits ends at a user message,
    // and the old turn's tool-call rounds sit BEFORE it — which is what a provider that demands
    // `reasoning_content` for the current turn's rounds can read.
    let new_sent: Vec<Sent> = new.sent().iter().map(|b| sent(b)).collect();
    assert_eq!(
        new_sent.len(),
        1,
        "the new model was sent {} request(s), and only the next prompt may reach it: {:?}",
        new_sent.len(),
        new_sent.iter().map(|s| &s.roles).collect::<Vec<_>>()
    );
    let first_new = &new_sent[0];
    assert_eq!(
        first_new.model, "glm-4.6",
        "the switch is what sent it there"
    );
    assert_eq!(
        first_new.roles.last().map(String::as_str),
        Some("user"),
        "the first request on a new model must end at a USER message: {:?}",
        first_new.roles
    );
    assert!(
        !first_new.ends_mid_tool_run,
        "the new model was handed a run of assistant tool-call rounds: {:?}",
        first_new.roles
    );
    // And it really did carry the old turn's tool round — otherwise the assertion above is about an
    // empty history rather than about a turn that ended.
    assert!(
        first_new
            .roles
            .windows(2)
            .any(|w| w == ["assistant".to_string(), "tool".to_string()]),
        "the old turn's tool-call round is in the history the new model reads: {:?}",
        first_new.roles
    );
    assert!(
        rec.listing.contains("now answering: glm/glm-4.6 (metered)"),
        "and the session itself says where it is now: {}",
        rec.listing
    );

    // ── B. A switch during a failing round's RETRIES changes nothing about the retries
    //
    // `http_retries = 1`: the ladder takes the round again once. Every attempt is still sent to the
    // old model, and the new one is sent nothing until the next prompt. This is the operator's *"the
    // model changed to … while this round was failing"* line being unmade.
    old.forget();
    new.forget();
    let mut cfg = config(&scratch, &gguf, "ladder", "deepseek", "deepseek-chat");
    cfg.http_retries = 1;
    let (o, _n) = (old.clone(), new.clone());
    let (rec, _hub, _cfg) = drive(&parts, cfg, move |client, rx, hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        until("the first refusal to arrive", || o.count() >= 2);
        client
            .slash(0, "models glm/glm-4.6")
            .expect("the switch is accepted");
        watch(rx, rec, |rec| rec.when_it_ends().is_some());
        // The turn ends when the ladder runs out: one backoff, then the second refusal.
        watch(rx, rec, |rec| rec.failed > 0 && !rec.applied.is_empty());
        client
            .prompt(0, NEXT)
            .expect("the second prompt is accepted");
        until("the new model's answer", || answered(hub));
    });
    assert!(
        !rec.backoffs().is_empty(),
        "the ladder never ran, so this scenario measured nothing: {:?}",
        rec.retries
    );
    let old_sent: Vec<Sent> = old.sent().iter().map(|b| sent(b)).collect();
    assert!(
        old_sent.len() >= 3,
        "the retries went somewhere other than the model the turn started on: {:?}",
        old_sent.iter().map(|s| &s.model).collect::<Vec<_>>()
    );
    assert!(
        old_sent.iter().all(|s| s.model == "deepseek-chat"),
        "every retry was sent to the model the turn started on: {:?}",
        old_sent.iter().map(|s| &s.model).collect::<Vec<_>>()
    );
    // **ONE request in all** — the next prompt's. A retry that had picked the switch up would be a
    // second one here, and it is the request the operator's screen named.
    let after: Vec<Sent> = new.sent().iter().map(|b| sent(b)).collect();
    assert_eq!(
        after.len(),
        1,
        "a retry was re-issued on the new model: {:?}",
        after.iter().map(|s| &s.roles).collect::<Vec<_>>()
    );
    assert_eq!(after[0].roles.last().map(String::as_str), Some("user"));
    assert!(!after[0].ends_mid_tool_run);
    assert!(
        rec.failed > 0,
        "the turn on the model that refused its last round did not fail: {:?}",
        rec.slashes
    );

    // ── C. `[fallback]` — the switch takes at the turn's end, and the WORK GOES ON
    //
    // No `/models` at all and no second prompt either: the model is out, the list names where this
    // session goes, and the daemon opens the next turn itself — in the harness's voice, on the new
    // model, with the notice as its user item. That is the operator's ruling of 2026-10-12 (*"when
    // it finally applied the turn didnt continue"*) measured end to end: nobody types anything,
    // and the work is carried on.
    old.forget();
    new.forget();
    let mut cfg = config(&scratch, &gguf, "fallback", "deepseek", "deepseek-chat");
    cfg.fallback = vec!["glm/glm-4.6".to_string()];
    let (rec, hub, _cfg) = drive(&parts, cfg, move |client, rx, hub, rec| {
        // The first turn is the operator's.
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        // **And then nothing**: the daemon's own turn is what answers, so this waits for the answer
        // row rather than sending a prompt to provoke it.
        until("the continuation turn's answer", || answered(hub));
        watch(rx, rec, |rec| rec.failed > 0 && !rec.applied.is_empty());
    });
    let said = rec
        .fell_back()
        .unwrap_or_else(|| {
            panic!(
                "the model was out and [fallback] named somewhere else, and nothing was said: {:?}",
                rec.retries
            )
        })
        .clone();
    assert!(
        said.contains("glm/glm-4.6") && said.contains("[fallback]"),
        "the notice names where it went and where that came from: {said}"
    );
    assert!(
        said.contains("the daemon opens the next turn itself"),
        "and that the work goes on without anyone typing: {said}"
    );
    // **One request, and it is the continuation's** — no operator prompt follows the fallback in
    // this scenario, so a second request here would be a turn nobody started.
    let new_sent: Vec<Sent> = new.sent().iter().map(|b| sent(b)).collect();
    assert_eq!(
        new_sent.len(),
        1,
        "the continuation is the only turn the new model saw: {:?}",
        new_sent.iter().map(|s| &s.roles).collect::<Vec<_>>()
    );
    let continuation = &new_sent[0];
    assert_eq!(
        continuation.model, "glm-4.6",
        "and it ran on the model the fallback named"
    );
    assert_eq!(
        continuation.roles.last().map(String::as_str),
        Some("user"),
        "its first request ends at a USER message: {:?}",
        continuation.roles
    );
    assert!(
        !continuation.ends_mid_tool_run,
        "and not inside the refused turn's tool rounds: {:?}",
        continuation.roles
    );
    assert!(
        continuation.last_text.starts_with("[fallback] "),
        "the message is the daemon's own notice, in its own register: {:?}",
        continuation.last_text
    );
    assert!(
        continuation.last_text.contains("refused (http 429)")
            && continuation
                .last_text
                .contains("continuing this task on glm/glm-4.6"),
        "which names what refused and where the work goes on: {:?}",
        continuation.last_text
    );
    // **And it is the harness's row, not the operator's.** A head draws the two differently
    // (R42), and a continuation that read as a prompt would be the session talking as a person
    // who said nothing.
    assert!(
        carries_the_daemons_notice(&hub),
        "the continuation's user row is not marked as the harness's own: {:?}",
        user_rows(&hub)
    );
    assert!(
        rec.listing.contains("now answering: glm/glm-4.6 (metered)"),
        "and the session stays there, which is what makes the move a switch rather than a detour: \
         {}",
        rec.listing
    );

    // ── D. An interrupted turn applies the pending switch
    //
    // This is the session that goes slow after its refusal, so the operator's interrupt arrives at a
    // round that is GENERATING and is heard there (a refused round delivers no delta, and an urgent
    // steer is only acted on where one is). The switch is recorded after the first refusal, and the
    // turn then ends the other way — and it still lands.
    old.forget();
    new.forget();
    slow.forget();
    let mut cfg = config(&scratch, &gguf, "interrupted", "glm-coding", "glm-5.3");
    cfg.http_retries = 1;
    let (s, _n) = (slow.clone(), new.clone());
    let (rec, _hub, _cfg) = drive(&parts, cfg, move |client, rx, _hub, rec| {
        client.prompt(0, PROMPT).expect("the prompt is accepted");
        until("the first refusal to arrive", || s.count() >= 2);
        client
            .slash(0, "models glm/glm-4.6")
            .expect("the switch is accepted");
        // **The interrupt goes in once the switch is recorded**, and reaches the slow round the
        // retry takes after the backoff — a turn that ends on the operator's word rather than on
        // the ladder's.
        watch(rx, rec, |rec| rec.when_it_ends().is_some());
        until("the slow round to start", || s.count() >= 3);
        client
            .interrupt(0, "the operator stopped this turn")
            .expect("the interrupt is accepted");
        watch(rx, rec, |rec| !rec.applied.is_empty());
    });
    assert!(
        !rec.applied.is_empty(),
        "an interrupted turn left the switch pending: {:?}",
        rec.slashes
    );
    assert!(
        rec.refused.is_empty(),
        "the switch was refused: {:?}",
        rec.refused
    );
    assert!(
        rec.listing.contains("now answering: glm/glm-4.6 (metered)"),
        "the interrupted turn still ends on the new model: {}",
        rec.listing
    );
    assert!(
        rec.failures.iter().any(|e| e.contains("steering_urgent")),
        "the turn did not end on the operator's interrupt — it ended some other way, so this \
         scenario measured a slow answer rather than an interrupted one: {:?}",
        rec.failures
    );
    assert_eq!(
        new.count(),
        0,
        "and nothing was sent to it until a turn starts there"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
