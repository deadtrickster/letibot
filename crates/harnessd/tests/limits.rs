//! A usage limit with a known reset never fails a session (fake providers, a hand-moved clock).
//! `wait` parks until the provider's instant and resumes the turn; `switch` moves to an unlimited
//! model and back after the reset. No instant: the ladder, then long beats, then a failure.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::Parts;
use letibot_harnessd::sessions::{NagHold, Sessions};
use letibot_sessionlog::registry::Registry;
use letibot_transcript::TranscriptItem;

/// The answer a stub gives when it answers at all. A marker, so the row can be found.
const ANSWER: &str = "the-turn-went-through";

/// The first prompt. Nothing tool-shaped in it: where a round is sent is this file's subject.
const PROMPT: &str = "say hello";

/// The provider's own refusal with a time on it - the operator's own 429, verbatim in shape.
const LIMIT_BODY: &str = "{\"error\":{\"message\":\"Usage limit reached for 5 hour. Your limit \
                          will reset at 2026-11-07 21:02:27\"}}";

/// A limit with no instant at all: the ladder's, then the long beat's.
const NO_INSTANT_BODY: &str = "{\"error\":{\"message\":\"Rate limit reached\"}}";

/// The clock every scenario starts at, Unix millis - fixed, so a park of five minutes is measured
/// without sleeping through it.
const T0: u64 = 1_794_000_000_000;

/// The environment and `providers.toml` are process-wide, and every scenario writes its own: one at
/// a time, or a second `XDG_CONFIG_HOME` points every test at somebody else's stubs.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn alone() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a stub does with one request.
#[derive(Clone, Copy)]
enum Act {
    /// A `429` naming an instant, as `Retry-After` seconds.
    Refuse(u64),
    /// A `429` whose body names a wall clock and the window it closed.
    RefuseProse,
    /// A `429` that names nothing.
    RefuseBare,
    /// A `429` whose body is this prose and nothing else.
    RefuseWith(&'static str),
    /// One text answer, all at once.
    Answer,
}

/// A loopback model with a script: the nth request gets the nth entry, the last repeats for ever.
/// `Clone` shares the recorder.
#[derive(Clone)]
struct Model {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Model {
    fn count(&self) -> usize {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

fn model(script: &'static [Act]) -> Model {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    let record = seen.clone();
    std::thread::spawn(move || {
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(mut s) = stream else { continue };
            let body = read_request(&mut s);
            record.lock().unwrap_or_else(|e| e.into_inner()).push(body);
            let act = script[n.min(script.len() - 1)];
            let (head, payload) = match act {
                Act::Refuse(secs) => (
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {secs}\r\n\
                         Content-Type: application/json\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n",
                        LIMIT_BODY.len()
                    ),
                    LIMIT_BODY.to_string(),
                ),
                Act::RefuseProse => (
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        LIMIT_BODY.len()
                    ),
                    LIMIT_BODY.to_string(),
                ),
                Act::RefuseBare => (
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        NO_INSTANT_BODY.len()
                    ),
                    NO_INSTANT_BODY.to_string(),
                ),
                Act::RefuseWith(body) => (
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    ),
                    body.to_string(),
                ),
                Act::Answer => (
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
                        .to_string(),
                    format!(
                        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{ANSWER}\"}}}}]}}\n\n\
                         data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
                         data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":50,\"completion_tokens\":4}}}}\n\n\
                         data: [DONE]\n\n"
                    ),
                ),
            };
            let _ = write!(s, "{head}{payload}");
            let _ = s.flush();
        }
    });
    Model { port, seen }
}

/// Drain one request - headers plus the body `Content-Length` names.
fn read_request(s: &mut std::net::TcpStream) -> String {
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

/// A scratch config home for this process, so no scenario can read or write the operator's
/// `providers.toml`. Called once per binary.
fn scratch_home(tag: &str) -> std::path::PathBuf {
    let root = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-limits-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("letibot")).expect("the scratch config dir");
    // SAFETY: this binary's scenarios run in sequence on this thread, and nothing reads the
    // environment concurrently with these calls.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &root);
        std::env::set_var("XDG_DATA_HOME", &root);
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
    root
}

/// A config for one scenario, on the box's own vocabulary, pointing at two loopback stubs.
fn config(
    scratch: &std::path::Path,
    gguf: &std::path::Path,
    tag: &str,
    here: (&str, &str),
    fallback: Vec<String>,
) -> Config {
    let ws = scratch.join(format!("ws-{tag}"));
    std::fs::create_dir_all(&ws).expect("the workspace");
    let mut cfg = Config::for_this_box(&ws);
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = letibot_harnessd::dialect::Dialect::Qwen;
    cfg.vocab_gguf = Some(gguf.to_path_buf());
    cfg.session_id = format!("limit-{tag}");
    cfg.socket = scratch.join(format!("{tag}.sock"));
    cfg.store = Some(scratch.join(format!("{tag}.db")));
    cfg.provider = Some(letibot_harnessd::config::ProviderConfig {
        name: here.0.to_string(),
        model: Some(here.1.to_string()),
        api_key: None,
        thinking: false,
    });
    cfg.fallback = fallback;
    // The ladder is spent at once where a scenario is about the LIMIT and not about the rungs;
    // the no-instant scenario sets its own patience.
    cfg.http_retries = 0;
    cfg.limit_retry_wait = Duration::from_millis(20);
    cfg.limit_retry_rounds = 2;
    // A park waits for the reset alone unless a scenario is about the early re-tries.
    cfg.limit_probe = Vec::new();
    let (clock, _handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    cfg
}

/// The providers file the stubs are named in: one name per stub, as `providers.toml` does it.
fn providers(scratch: &std::path::Path, glm: u16, deepseek: u16) {
    providers3(scratch, glm, glm, deepseek)
}

fn providers3(scratch: &std::path::Path, glm: u16, coding: u16, deepseek: u16) {
    let text = format!(
        "# Written by tests/limits.rs. Nothing here is the operator's file.\n\
         [glm]\nkey = \"sk-test\"\nurl = \"http://127.0.0.1:{glm}/v1/chat/completions\"\n\n\
         [glm-coding]\nkey = \"sk-test\"\nurl = \"http://127.0.0.1:{coding}/v1/chat/completions\"\n\n\
         [deepseek]\nkey = \"sk-test\"\nurl = \"http://127.0.0.1:{deepseek}/v1/chat/completions\"\n"
    );
    std::fs::write(scratch.join("letibot/providers.toml"), text).expect("the config file");
}

/// Is the stub's answer a row yet - the proof a turn finished.
fn answered(hub: &letibot_sessionlog::Hub) -> bool {
    hub.snapshot()
        .items
        .iter()
        .filter_map(|i| i.item.as_ref())
        .any(|i| matches!(i, TranscriptItem::Assistant { text, .. } if text.contains(ANSWER)))
}

/// Every warning of one code the daemon published, in order.
fn said(hub: &letibot_sessionlog::Hub, code: &str) -> Vec<String> {
    hub.snapshot()
        .warnings
        .into_iter()
        .filter(|w| w.code == code)
        .map(|w| w.detail)
        .collect()
}

/// Open one session of a scenario, and hand back the pieces a test needs.
fn open<'a>(parts: &'a Parts, cfg: Config) -> (Sessions<'a>, Arc<letibot_sessionlog::Hub>, String) {
    let id = cfg.session_id.clone();
    let registry = Registry::new();
    registry
        .create(id.clone(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry has no session by that name");
    let sessions = Sessions::open_first(parts, cfg, registry.clone()).expect("the session opens");
    let hub = registry.get(&id).expect("the session is in the registry");
    (sessions, hub, id)
}

/// The whole first scenario: a limit with an instant parks the session, the plan-check goes
/// silent, and the same turn resumes at the reset - measured on a clock the test moves.
#[test]
fn a_limit_with_a_reset_parks_the_session_and_the_same_turn_resumes_at_the_reset() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("wait");
    let glm = model(&[Act::Refuse(300), Act::Answer]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let cfg = config(&scratch, &gguf, "wait", ("glm", "glm-5.3"), Vec::new());
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    let mut cfg = cfg;
    cfg.limit_clock = clock;
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions
        .submit_operator(&id, PROMPT)
        .expect_err("a limit is not an answer");
    let said_park = out.to_string();
    assert!(
        said_park.contains("paused until"),
        "the ending says the session is parked and not that it failed: {said_park}"
    );
    assert_eq!(
        glm.count(),
        1,
        "the round was taken once - the retry ladder is off in this scenario"
    );
    let parks = said(&hub, "limit_park");
    assert_eq!(parks.len(), 1, "one park, said once: {parks:?}");
    assert!(
        !hub.turn_in_flight(),
        "the park ends the round it refused, so no head keeps a spinner up"
    );
    assert!(
        matches!(
            hub.snapshot().turn.map(|t| t.state),
            Some(letibot_sessionlog::view::TurnState::Parked { .. })
        ),
        "the turn's state is the park, not an interrupt"
    );
    assert!(
        said(&hub, "turn_failed").is_empty(),
        "a limit with a reset is not a failure of the turn"
    );
    let h = sessions.harness_of(&id).expect("the session is open");
    let park = h.limit_park().expect("the session is parked").clone();
    assert_eq!(
        park.until_ms,
        T0 + 300_000 + 2_000,
        "the provider's own instant plus the jitter, on the injected clock"
    );
    assert_eq!(
        h.nag_hold(),
        NagHold::Parked,
        "the plan-check is silent while parked: a check is a turn, and a turn is what was refused"
    );
    assert!(
        !h.limit_park_due(T0),
        "nothing resumes before the provider's instant"
    );

    // A prompt that arrives while parked is QUEUED, not sent: nothing reaches the provider, and the
    // ending says what the prompt is waiting for. The row is already in the conversation, so the
    // turn the reset opens answers it.
    let queued = sessions
        .submit_operator(&id, "and one more thing")
        .expect_err("a parked session sends nothing");
    assert!(
        queued.to_string().contains("paused until"),
        "the queued prompt says what it waits for: {queued}"
    );
    assert_eq!(glm.count(), 1, "nothing was sent while parked");

    // The reset comes. The clock says so and nothing else does.
    handle.store(T0 + 400_000, Ordering::SeqCst);
    assert_eq!(sessions.resume_due_limits(), 1, "the park resumed");
    assert!(
        sessions
            .harness_of(&id)
            .expect("open")
            .limit_park()
            .is_none(),
        "the park is spent"
    );
    assert_eq!(glm.count(), 2, "the resumed turn took the round again");
    assert!(
        answered(&hub),
        "the answer the resumed turn earned is a row: {}",
        said(&hub, "limit_resumed").join(" / ")
    );
    assert_eq!(said(&hub, "limit_resumed").len(), 1);
    assert_ne!(
        sessions.harness_of(&id).expect("open").nag_hold(),
        NagHold::Parked,
        "a resumed session's plan-check is its own again"
    );
}

/// The switch: the round that met the limit is re-issued on the fallback model, and the session
/// comes home once the limited model's own instant has passed.
#[test]
fn a_switch_moves_the_turn_to_the_fallback_and_back_after_the_reset() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("switch");
    let glm = model(&[Act::Refuse(300)]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(
        &scratch,
        &gguf,
        "switch",
        ("glm", "glm-5.3"),
        vec!["deepseek/deepseek-flash".into()],
    );
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    cfg.on_limit = leticode_on_limit_switch();
    // A five-minute reset is under the default threshold; `0` is the switch this test is about.
    cfg.wait_if_within = Duration::ZERO;
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions
        .submit_operator(&id, PROMPT)
        .expect("the turn went on, on another model");
    assert!(
        out.text.contains(ANSWER),
        "the fallback answered: {}",
        out.text
    );
    assert_eq!(
        glm.count(),
        1,
        "the limited model was asked once, and the turn was not re-issued there"
    );
    assert_eq!(
        deepseek.count(),
        1,
        "the fallback took the turn exactly once: {:?}",
        deepseek.seen.lock().unwrap()
    );
    let home = sessions
        .harness_of(&id)
        .expect("open")
        .model_name()
        .to_string();
    assert_eq!(
        home, "deepseek/deepseek-flash",
        "the session moved, and the row says where"
    );
    let moved = said(&hub, "model_fallback");
    assert!(
        moved.iter().any(|d| d.contains("[fallback]")),
        "the operator is told the session moved: {moved:?}"
    );
    let row = sessions
        .harness_of(&id)
        .expect("open")
        .limit_park()
        .is_none();
    assert!(row, "a moving session is not parked");

    // The limit lifts. The switch back is the daemon's to make, and it is not a turn.
    handle.store(T0 + 400_000, Ordering::SeqCst);
    assert_eq!(sessions.resume_due_limits(), 1, "the way back was taken");
    let back = sessions
        .harness_of(&id)
        .expect("open")
        .model_name()
        .to_string();
    assert_eq!(
        back, "glm/glm-5.3",
        "the session went back to the model the limit was on"
    );
    assert!(
        said(&hub, "limit_returned")
            .iter()
            .any(|d| d.contains("glm")),
        "and said so: {:?}",
        said(&hub, "limit_returned")
    );
}

/// A `429` that named no instant is the long beat, not a park: the round is taken again after it,
/// and a run of them still ends as it always did.
#[test]
fn a_429_with_no_instant_waits_the_long_beat_and_then_fails_as_before() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("bare");
    let glm = model(&[Act::RefuseBare]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(&scratch, &gguf, "bare", ("glm", "glm-5.3"), Vec::new());
    cfg.limit_retry_rounds = 2;
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions
        .submit_operator(&id, PROMPT)
        .expect_err("no instant, no instant");
    let text = out.to_string();
    assert!(
        text.contains("429") && text.contains("Rate limit reached"),
        "after the long waits it fails with the provider's own words, as the tree always did: {text}"
    );
    assert_eq!(
        glm.count(),
        3,
        "one round and the two long retries the config asked for"
    );
    assert_eq!(
        said(&hub, "limit_retry").len(),
        2,
        "each long wait is said once: {:?}",
        said(&hub, "limit_retry")
    );
    assert!(
        said(&hub, "limit_park").is_empty(),
        "nothing was parked: there was no instant to park until"
    );
    assert!(
        sessions
            .harness_of(&id)
            .expect("open")
            .limit_park()
            .is_none()
    );
}

/// And a `429` with no instant that answers on the second try is a turn, not a failure - the whole
/// point of the long beat.
#[test]
fn a_429_with_no_instant_that_lifts_is_a_turn_and_not_a_failure() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("bare-ok");
    let glm = model(&[Act::RefuseBare, Act::Answer]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let cfg = config(&scratch, &gguf, "bare-ok", ("glm", "glm-5.3"), Vec::new());
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions
        .submit_operator(&id, PROMPT)
        .expect("the second attempt answered");
    assert!(out.text.contains(ANSWER), "{}", out.text);
    assert_eq!(glm.count(), 2, "one wait, one round, one answer");
    assert!(answered(&hub));
    assert!(said(&hub, "turn_failed").is_empty());
}

/// The operator's own file: `[provider] on_limit = "switch"`.
fn leticode_on_limit_switch() -> letibot_harnessd::leticode_config::OnLimit {
    letibot_harnessd::leticode_config::OnLimit::Switch
}

/// A fallback the provider has already put a time on is not a fallback: the second turn's limit meets
/// a list whose only other name is still limited, so the session waits instead of burning a round
/// on a model it knows will refuse it.
#[test]
fn a_fallback_that_is_limited_too_is_skipped() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("skip");
    let glm = model(&[Act::Refuse(300), Act::Answer]);
    let deepseek = model(&[Act::Answer, Act::Refuse(300), Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(
        &scratch,
        &gguf,
        "skip",
        ("glm", "glm-5.3"),
        vec!["deepseek/deepseek-flash".into(), "glm/glm-5.3".into()],
    );
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    cfg.on_limit = leticode_on_limit_switch();
    cfg.wait_if_within = Duration::ZERO;
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    // The first turn moves off the limited model, to the first name that is not limited.
    sessions
        .submit_operator(&id, PROMPT)
        .expect("the fallback answered the first turn");
    assert_eq!(glm.count(), 1, "the limited model was asked once");
    assert_eq!(
        deepseek.count(),
        1,
        "and the session moved to the name that answers"
    );

    // The second turn is limited on the model the session moved TO. The only other name in
    // `[fallback]` is the model it came from, whose own limit has not lifted.
    let out = sessions
        .submit_operator(&id, "and again")
        .expect_err("nothing could take this turn");
    assert!(
        out.to_string().contains("paused until"),
        "the session waits rather than moving to a model known to be limited: {out}"
    );
    assert_eq!(
        glm.count(),
        1,
        "the limited fallback was NOT tried again: a switch there would meet the same refusal"
    );
    assert_eq!(
        sessions
            .harness_of(&id)
            .expect("open")
            .model_name()
            .to_string(),
        "deepseek/deepseek-flash",
        "and the session did not move to a model that is limited too"
    );
    assert!(
        said(&hub, "model_fallback_skip")
            .iter()
            .any(|d| d.contains("put a time on this model's own limit")),
        "the skip is said, not silent: {:?}",
        said(&hub, "model_fallback_skip")
    );

    // The wait was real: the reset resumes the turn on the model that refused it, and the way back
    // to the model the first limit moved this session off is owed at the same instant.
    handle.store(T0 + 400_000, Ordering::SeqCst);
    assert_eq!(
        sessions.resume_due_limits(),
        2,
        "the park resumed and the switch back was taken"
    );
    assert_eq!(
        deepseek.count(),
        3,
        "the resumed turn went back to the model that was limited"
    );
    assert_eq!(
        sessions
            .harness_of(&id)
            .expect("open")
            .model_name()
            .to_string(),
        "glm/glm-5.3",
        "and the session went home once both limits had lifted"
    );
}

/// The prose the provider actually sends - a wall clock and the window it closed - is a park too,
/// and the window bounds it: five hours, not the day the wall clock would have named.
#[test]
fn a_429_naming_a_wall_clock_and_a_window_parks_for_the_window() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("prose");
    let glm = model(&[Act::RefuseProse, Act::Answer]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(&scratch, &gguf, "prose", ("glm", "glm-5.3"), Vec::new());
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions
        .submit_operator(&id, PROMPT)
        .expect_err("a limit is not an answer");
    assert!(out.to_string().contains("paused until"), "{out}");
    let park = sessions
        .harness_of(&id)
        .expect("open")
        .limit_park()
        .expect("parked on the prose")
        .clone();
    assert_eq!(
        park.until_ms,
        T0 + 5 * 3_600_000 + 2_000,
        "the window the provider named, not the wall clock beside it"
    );
    handle.store(T0 + 6 * 3_600_000, Ordering::SeqCst);
    assert_eq!(sessions.resume_due_limits(), 1);
    assert!(answered(&hub));
}

/// A weekly limit as a provider words it: a wall clock and no window. T0 is Fri 2026-11-06
/// 21:20:00 UTC, so the reset is three and a half days out - past the park's six-hour bound.
const WEEKLY_BODY: &str = "{\"error\":{\"message\":\"Weekly/Monthly Limit Exhausted. Your limit \
                           will reset at 2026-11-10 14:00:00.\"}}";

/// 2026-11-10 14:00:00 UTC, in millis.
const WEEKLY_RESET: u64 = 1_794_319_200_000;

/// The limit's words on the session's activity, the status row a head draws.
fn limit_row(hub: &letibot_sessionlog::Hub) -> Option<String> {
    use letibot_sessionlog::activity::{Activity, Waiting};
    match hub.activity() {
        Activity::Busy { waiting_on } => waiting_on.into_iter().find_map(|w| match w {
            Waiting::Limit(words) => Some(words),
            _ => None,
        }),
        _ => None,
    }
}

/// Under `switch`, a reset sooner than `wait_if_within` is waited for on the same model: a switch
/// there and back costs more than five minutes.
#[test]
fn a_reset_under_wait_if_within_parks_even_with_a_fallback() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("within");
    let glm = model(&[Act::Refuse(300), Act::Answer]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(
        &scratch,
        &gguf,
        "within",
        ("glm", "glm-5.3"),
        vec!["deepseek/deepseek-flash".into()],
    );
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    cfg.on_limit = leticode_on_limit_switch();
    assert_eq!(
        cfg.wait_if_within,
        Duration::from_secs(45 * 60),
        "the default threshold"
    );
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions
        .submit_operator(&id, PROMPT)
        .expect_err("the session waits instead of moving");
    let words = "waiting 5m for the reset at 21:25:02 UTC (under wait_if_within 45m)";
    assert!(out.to_string().contains(words), "{out}");
    assert_eq!(
        limit_row(&hub).as_deref(),
        Some(
            "parked on glm/glm-5.3's limit until 21:25:02 UTC - esc ends the turn, /models \
             switches"
        )
    );
    assert_eq!(deepseek.count(), 0, "the fallback was not asked");
    let h = sessions.harness_of(&id).expect("open");
    assert_eq!(h.model_name().to_string(), "glm/glm-5.3");
    assert_eq!(h.limit_park().expect("parked").until_ms, T0 + 302_000);
    assert_eq!(
        h.limit_return_at(),
        None,
        "no way back is owed: nothing moved"
    );

    handle.store(T0 + 400_000, Ordering::SeqCst);
    assert_eq!(sessions.resume_due_limits(), 1);
    assert_eq!(glm.count(), 2, "the turn resumed on the same model");
    assert!(answered(&hub));
}

/// A reset past `wait_if_within` moves the session, and the way back is at the provider's real
/// reset - days out - not at the six-hour bound a park is held to.
#[test]
fn a_reset_past_wait_if_within_switches_and_returns_at_the_real_reset() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("past");
    let glm = model(&[Act::RefuseWith(WEEKLY_BODY)]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(
        &scratch,
        &gguf,
        "past",
        ("glm", "glm-5.3"),
        vec!["deepseek/deepseek-flash".into()],
    );
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    cfg.on_limit = leticode_on_limit_switch();
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions
        .submit_operator(&id, PROMPT)
        .expect("the fallback answered");
    assert!(out.text.contains(ANSWER), "{}", out.text);
    let words = "glm/glm-5.3 reset Tue 14:00:02 UTC is past wait_if_within 45m - on \
                 deepseek/deepseek-flash until then";
    let moved = said(&hub, "model_fallback");
    assert!(
        moved
            .iter()
            .any(|d| d.starts_with(&format!("[fallback] {words}."))),
        "{moved:?}"
    );
    assert_eq!(limit_row(&hub).as_deref(), Some(words));
    let back = WEEKLY_RESET + 2_000;
    assert_eq!(
        sessions.harness_of(&id).expect("open").limit_return_at(),
        Some(back),
        "the way back is the real reset"
    );
    assert!(back > T0 + letibot_http::reset::LONGEST);

    // Past the bound and before the reset: still on the fallback.
    handle.store(T0 + 7 * 3_600_000, Ordering::SeqCst);
    assert_eq!(sessions.resume_due_limits(), 0);
    handle.store(back, Ordering::SeqCst);
    assert_eq!(sessions.resume_due_limits(), 1);
    assert_eq!(
        sessions
            .harness_of(&id)
            .expect("open")
            .model_name()
            .to_string(),
        "glm/glm-5.3"
    );
}

/// No fallback and a weekly reset: the park is held to its bound, and every sentence names the
/// real reset rather than the bound as if it were one.
#[test]
fn a_weekly_limit_with_no_fallback_parks_and_names_the_real_reset() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("weekly");
    let glm = model(&[Act::RefuseWith(WEEKLY_BODY), Act::Answer]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(&scratch, &gguf, "weekly", ("glm", "glm-5.3"), Vec::new());
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    cfg.on_limit = leticode_on_limit_switch();
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions.submit_operator(&id, PROMPT).expect_err("parked");
    let words = "limited until Tue 14:00:02 UTC, asking again at 03:20:02 UTC";
    assert!(out.to_string().contains(words), "{out}");
    assert_eq!(
        limit_row(&hub).as_deref(),
        Some(
            "parked on glm/glm-5.3's limit until Tue 14:00:02 UTC, asking again at 03:20:02 \
             UTC - esc ends the turn, /models switches"
        )
    );
    let park = sessions
        .harness_of(&id)
        .expect("open")
        .limit_park()
        .expect("parked")
        .clone();
    assert_eq!(park.until_ms, T0 + letibot_http::reset::LONGEST + 2_000);
    assert_eq!(park.reset_ms, WEEKLY_RESET + 2_000);

    handle.store(park.until_ms, Ordering::SeqCst);
    assert_eq!(sessions.resume_due_limits(), 1, "the bound asks again");
    assert_eq!(glm.count(), 2);
}

/// The per-model table: the limited model's own list, else its provider's, else `*`. A
/// limited entry in the chosen list is skipped like any other.
#[test]
fn a_per_model_fallback_list_is_picked_by_the_limited_model_and_skips_limited_entries() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("per-model");
    let glm = model(&[Act::RefuseWith(WEEKLY_BODY)]);
    let coding = model(&[Act::Answer]);
    let deepseek = model(&[Act::Answer, Act::RefuseWith(WEEKLY_BODY)]);
    providers3(&scratch, glm.port, coding.port, deepseek.port);
    // `*` would send glm to glm-coding; `glm/*` wins and sends it to deepseek.
    let mut cfg = config(
        &scratch,
        &gguf,
        "per-model",
        ("glm", "glm-5.3"),
        vec!["glm-coding/glm-5.3".into()],
    );
    cfg.fallback_for = [
        ("glm/*".to_string(), vec!["deepseek/deepseek-flash".into()]),
        (
            "deepseek/deepseek-flash".to_string(),
            vec!["glm/glm-5.3".into(), "glm-coding/glm-5.3".into()],
        ),
    ]
    .into_iter()
    .collect();
    let (clock, _handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    cfg.on_limit = leticode_on_limit_switch();
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);
    let on = |s: &Sessions| s.harness_of(&id).expect("open").model_name().to_string();

    sessions
        .submit_operator(&id, PROMPT)
        .expect("deepseek answered");
    assert_eq!(on(&sessions), "deepseek/deepseek-flash", "`glm/*` beat `*`");
    assert_eq!(coding.count(), 0);

    // deepseek's own key lists glm first, which is limited, so glm-coding takes the turn.
    sessions
        .submit_operator(&id, "and again")
        .expect("glm-coding answered");
    assert_eq!(on(&sessions), "glm-coding/glm-5.3");
    assert_eq!(glm.count(), 1, "the limited entry was not asked");
    assert!(
        said(&hub, "model_fallback_skip")
            .iter()
            .any(|d| d.contains("glm/glm-5.3")),
        "{:?}",
        said(&hub, "model_fallback_skip")
    );
}

/// Kimi's 5-hour refusal, verbatim: the window it closed and no instant.
const KIMI_LIMIT_BODY: &str = "{\"error\":{\"message\":\"You've reached your 5-hour usage limit. \
                               Your quota will reset when the current 5-hour window ends. To \
                               continue now, purchase extra usage or upgrade your plan: \
                               https://www.kimi.com/settings/subscription?tab=quota\",\
                               \"type\":\"rate_limit_reached_error\"}}";

/// The plan's usage report with the 5-hour window spent, verbatim.
const USAGES_5H: &str = r#"{"limits":[{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},"detail":{"limit":"100","used":"100","resetTime":"2026-11-14T01:07:08.146515Z"}}],"usages":{"limit_5h":{"used_ratio":1,"reset_time":"2026-11-14T01:07:07Z"},"limit_month_total":{"used_ratio":0.1688,"reset_time":"2026-12-13T00:07:40Z"},"limit_month_code":{"used_ratio":0.1688,"reset_time":"2026-12-13T00:07:40Z"}}}"#;

/// The same report with the month spent as well.
const USAGES_MONTH: &str = r#"{"usages":{"limit_5h":{"used_ratio":1,"reset_time":"2026-11-14T01:07:07Z"},"limit_month_total":{"used_ratio":1,"reset_time":"2026-12-13T00:07:40Z"},"limit_month_code":{"used_ratio":0.5,"reset_time":"2026-12-13T00:07:40Z"}}}"#;

/// 2026-11-14T01:07:07Z and 2026-12-13T00:07:40Z, in millis.
const RESET_5H: u64 = 1_794_618_427_000;
const RESET_MONTH: u64 = 1_797_120_460_000;

/// The Kimi scenarios' clock: three hours into the spent 5-hour window.
const KIMI_NOW: u64 = RESET_5H - 3 * 3_600_000;

/// A Kimi door on loopback: every POST is the 5-hour refusal, `GET /coding/v1/usages` is
/// `usages` (a 500 when `None`). Records each request line beside its `Authorization`.
fn kimi_door(usages: Option<&'static str>) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    let record = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && s.read(&mut byte).is_ok_and(|n| n == 1) {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let line = head.lines().next().unwrap_or_default().to_string();
            let header = |name: &str| {
                head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.trim()
                        .eq_ignore_ascii_case(name)
                        .then(|| v.trim().to_string())
                })
            };
            let auth = header("authorization").unwrap_or_default();
            record
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("{line} | {auth}"));
            let want: usize = header("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; want];
            let _ = s.read_exact(&mut body);
            let (status, payload) = if line.starts_with("GET /coding/v1/usages ") {
                match usages {
                    Some(u) => ("200 OK", u),
                    None => ("500 Internal Server Error", "{}"),
                }
            } else {
                ("429 Too Many Requests", KIMI_LIMIT_BODY)
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = s.flush();
        }
    });
    (port, seen)
}

/// One Kimi scenario: the refusal, then the park the usage report earns, and what the door saw.
fn kimi_park(tag: &str, usages: Option<&'static str>) -> Option<(u64, u64, Vec<String>)> {
    let gguf = letibot_harnessd::vocab_gguf()?;
    let scratch = scratch_home(tag);
    // SAFETY: as in `scratch_home` - one scenario at a time, nothing reading the environment.
    unsafe {
        std::env::remove_var("KIMI_API_KEY");
        std::env::remove_var("KIMI_CODE_API_KEY");
    }
    let (port, seen) = kimi_door(usages);
    std::fs::write(
        scratch.join("letibot/providers.toml"),
        format!(
            "[kimi-code]\nkey = \"sk-test\"\n\
             url = \"http://127.0.0.1:{port}/coding/v1/chat/completions\"\n"
        ),
    )
    .expect("the config file");
    let mut cfg = config(&scratch, &gguf, tag, ("kimi-code", "k3.1-pro"), Vec::new());
    cfg.limit_clock = letibot_harnessd::config::LimitClock::fixed(KIMI_NOW).0;
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, _hub, id) = open(&parts, cfg);
    let out = sessions
        .submit_operator(&id, PROMPT)
        .expect_err("a limit is not an answer");
    let out = out.to_string();
    assert!(
        out.contains("paused until") || out.contains("limited until"),
        "{out}"
    );
    let park = sessions
        .harness_of(&id)
        .expect("open")
        .limit_park()
        .expect("parked")
        .clone();
    let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Some((park.until_ms, park.reset_ms, seen))
}

/// The refusal names only its window, so the plan is asked once and its spent window's reset is
/// the park: the instant Kimi reports, not five hours from the refusal.
#[test]
fn a_kimi_limit_parks_on_the_reset_the_usage_endpoint_reports() {
    let _alone = alone();
    let Some((until, reset, seen)) = kimi_park("kimi-5h", Some(USAGES_5H)) else {
        return;
    };
    assert_eq!(
        until,
        RESET_5H + 2_000,
        "limit_5h's reset_time plus the jitter"
    );
    assert_eq!(reset, until);
    let asked: Vec<_> = seen.iter().filter(|l| l.starts_with("GET ")).collect();
    assert_eq!(asked.len(), 1, "one usage request: {seen:?}");
    assert!(
        asked[0].starts_with("GET /coding/v1/usages ") && asked[0].ends_with("| Bearer sk-test"),
        "on the door's own host, with its key: {seen:?}"
    );
}

/// A spent month outlasts the 5-hour window: nothing lifts until both have reset. The park is
/// still bounded, so the turn is asked again in six hours rather than slept through a month.
#[test]
fn a_kimi_limit_with_the_month_spent_parks_until_the_month_resets() {
    let _alone = alone();
    let Some((until, reset, _)) = kimi_park("kimi-month", Some(USAGES_MONTH)) else {
        return;
    };
    assert_eq!(reset, RESET_MONTH + 2_000, "the month's reset_time");
    assert_eq!(until, KIMI_NOW + letibot_http::reset::LONGEST + 2_000);
}

/// A usage endpoint that fails costs nothing: the window in the refusal is the reading.
#[test]
fn a_failed_usage_request_falls_back_to_the_window() {
    let _alone = alone();
    let Some((until, _, seen)) = kimi_park("kimi-fail", None) else {
        return;
    };
    assert_eq!(
        until,
        KIMI_NOW + 5 * 3_600_000 + 2_000,
        "the 5-hour window: {seen:?}"
    );
}

/// A reset three hours out: far enough that only an interrupt or a switch ends the park.
const FAR: u64 = 3 * 3600;

/// The conversation as the store holds it, for the restart's own reading.
fn items(hub: &letibot_sessionlog::Hub) -> Vec<TranscriptItem> {
    hub.snapshot()
        .items
        .into_iter()
        .filter_map(|i| i.item)
        .collect()
}

/// A stop, as a head's Esc queues it.
fn esc() -> letibot_sessionlog::hub::QueuedCommand {
    letibot_sessionlog::hub::QueuedCommand {
        head_id: letibot_sessionlog::hub::DAEMON_SUBMITTER.into(),
        identity: "operator".into(),
        client_request_id: "esc-1".into(),
        at_seq: 0,
        enqueued_ms: 0,
        kind: letibot_sessionlog::hub::CommandKind::Interrupt {
            reason: "esc".into(),
        },
    }
}

/// Parked on a far reset, the session says so with both ways out; Esc ends the parked turn at
/// once, keeps its rows, and leaves a row a restart does not read as a turn still owed.
#[test]
fn an_interrupt_ends_a_park_at_once_and_keeps_the_rows() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("esc");
    let glm = model(&[Act::Refuse(FAR), Act::Answer]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let cfg = config(&scratch, &gguf, "esc", ("glm", "glm-5.3"), Vec::new());
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    sessions.submit_operator(&id, PROMPT).expect_err("parked");
    assert_eq!(
        limit_row(&hub).as_deref(),
        Some(
            "parked on glm/glm-5.3's limit until 00:20:02 UTC - esc ends the turn, /models \
             switches"
        ),
        "the session's facts carry the park, its reset and the two ways out"
    );
    assert!(
        letibot_harnessd::restart::mid_turn(&items(&hub)),
        "while parked, a restart reads the turn as owed and resumes it"
    );

    let started = std::time::Instant::now();
    let out = sessions.dispatch(&id, &esc());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the stop took {:?}",
        started.elapsed()
    );
    assert!(
        matches!(&out, letibot_harnessd::sessions::Outcome::Failed(said)
            if said.contains("interrupted this turn while it was parked")),
        "the stop ended the parked turn"
    );
    let h = sessions.harness_of(&id).expect("open");
    assert!(h.limit_park().is_none(), "the park is gone");
    assert_ne!(h.nag_hold(), NagHold::Parked);
    assert_eq!(limit_row(&hub), None, "nothing says parked any more");
    let rows = items(&hub);
    let kept = |needle: &str| {
        rows.iter().any(|i| {
            match i {
            TranscriptItem::User { parts, .. } => parts.iter().any(|p| {
                matches!(p, letibot_transcript::UserPart::Text { text } if text.contains(needle))
            }),
            _ => false,
        }
        })
    };
    assert!(kept(PROMPT), "the parked turn's prompt is kept");
    assert!(
        kept("parked on glm/glm-5.3's usage limit (reset 00:20:02 UTC): esc"),
        "the row names the limit and its reset"
    );
    assert!(
        !letibot_harnessd::restart::mid_turn(&rows),
        "a restart after the stop owes nothing"
    );
    assert_eq!(said(&hub, "limit_park_interrupted").len(), 1);
    assert_eq!(glm.count(), 1, "nothing was sent by the stop");
    assert_eq!(sessions.resume_due_limits(), 0, "nothing resumes later");

    sessions
        .submit_operator(&id, "carry on")
        .expect("the next prompt is a turn of its own");
    assert!(answered(&hub));
}

/// `/models` while parked: the park was the refusing model's, so the parked request goes to
/// the new one at once and the turn completes there.
#[test]
fn a_switch_while_parked_retries_the_parked_turn_on_the_new_model() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("unpark");
    let glm = model(&[Act::Refuse(FAR)]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let cfg = config(&scratch, &gguf, "unpark", ("glm", "glm-5.3"), Vec::new());
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    sessions.submit_operator(&id, PROMPT).expect_err("parked");
    let reply = sessions.slash(&id, "models deepseek/deepseek-flash");
    assert!(reply.ok, "{:?}", reply.lines);
    let h = sessions.harness_of(&id).expect("open");
    assert!(
        h.limit_park_due(h.limit_now_ms()),
        "the switch made the park due, on a clock that has not moved"
    );

    assert_eq!(sessions.resume_due_limits(), 1, "the parked turn went on");
    assert_eq!(glm.count(), 1, "the limited model was not asked again");
    assert_eq!(deepseek.count(), 1, "the new model took the parked request");
    let sent = deepseek.seen.lock().unwrap()[0].clone();
    assert!(sent.contains(PROMPT), "the parked prompt went with it");
    assert!(answered(&hub), "the turn completed on the new model");
    let h = sessions.harness_of(&id).expect("open");
    assert!(h.limit_park().is_none());
    assert_eq!(
        h.limit_return_at(),
        None,
        "an operator's switch owes no way back"
    );
    assert_eq!(limit_row(&hub), None);
    assert_eq!(said(&hub, "limit_unparked").len(), 1);
    assert!(said(&hub, "limit_resumed").is_empty());
}

/// A prompt that lands after the switch and before the idle pass is the continuation itself:
/// the park is lifted by it, and no second turn is opened for the same park.
#[test]
fn a_prompt_after_a_switch_while_parked_is_sent_and_lifts_the_park() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("unpark-prompt");
    let glm = model(&[Act::Refuse(FAR)]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let cfg = config(
        &scratch,
        &gguf,
        "unpark-prompt",
        ("glm", "glm-5.3"),
        Vec::new(),
    );
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    sessions.submit_operator(&id, PROMPT).expect_err("parked");
    assert!(sessions.slash(&id, "models deepseek/deepseek-flash").ok);
    sessions
        .submit_operator(&id, "and go on")
        .expect("the prompt is sent to the new model");
    assert_eq!(deepseek.count(), 1);
    assert!(answered(&hub));
    assert!(
        sessions
            .harness_of(&id)
            .expect("open")
            .limit_park()
            .is_none()
    );
    assert_eq!(
        sessions.resume_due_limits(),
        0,
        "no second turn for one park"
    );
    assert_eq!(deepseek.count(), 1);
}

// ---------------------------------------------------------------------------- GLM's own codes
// The bodies are z.ai's templates (https://docs.z.ai/api-reference/api-code) as the door returns
// them: `{"error":{"code":"NNNN","message":...}}`.

/// `1302`: z.ai's rate or concurrency limit. Not a window.
const GLM_1302: &str =
    "{\"error\":{\"code\":\"1302\",\"message\":\"Rate limit reached for requests\"}}";

/// `1308` three minutes after T0, in the China time z.ai writes: T0 is 2026-11-06 21:20:00 UTC.
const GLM_1308_SOON: &str = "{\"error\":{\"code\":\"1308\",\"message\":\"Usage limit reached for 5 \
                             hour. Your limit will reset at 2026-11-07 05:23:00\"}}";

/// `1308` four hours after T0: far enough that every early re-try comes before it.
const GLM_1308_FAR: &str = "{\"error\":{\"code\":\"1308\",\"message\":\"Usage limit reached for 5 \
                            hour. Your limit will reset at 2026-11-07 09:20:00\"}}";

/// A rate limit coded `1302` is the long retry and never a park, and the turn it delayed answers.
#[test]
fn a_glm_rate_limit_retries_and_never_parks() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("glm-1302");
    let glm = model(&[Act::RefuseWith(GLM_1302), Act::Answer]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let cfg = config(&scratch, &gguf, "glm-1302", ("glm", "glm-5.3"), Vec::new());
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions
        .submit_operator(&id, PROMPT)
        .expect("a rate limit that lifts is a turn");
    assert!(out.text.contains(ANSWER), "{}", out.text);
    assert_eq!(glm.count(), 2, "one refusal, one long wait, one answer");
    assert!(said(&hub, "limit_park").is_empty(), "no window to park on");
    assert_eq!(said(&hub, "limit_retry").len(), 1);
    assert!(
        sessions
            .harness_of(&id)
            .expect("open")
            .limit_park()
            .is_none()
    );
}

/// A spent window coded `1308` parks on the reset its body names, read as China time: three
/// minutes, not the five hours the window's width would say.
#[test]
fn a_glm_window_parks_on_its_china_time_reset() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("glm-1308");
    let glm = model(&[Act::RefuseWith(GLM_1308_SOON), Act::Answer]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(&scratch, &gguf, "glm-1308", ("glm", "glm-5.3"), Vec::new());
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions.submit_operator(&id, PROMPT).expect_err("parked");
    assert!(
        out.to_string().contains("paused until 21:23:02 UTC"),
        "{out}"
    );
    let park = sessions
        .harness_of(&id)
        .expect("open")
        .limit_park()
        .expect("parked")
        .clone();
    assert_eq!(park.reset_ms, T0 + 180_000 + 2_000);
    assert_eq!(park.until_ms, park.reset_ms);
    handle.store(park.until_ms, Ordering::SeqCst);
    assert_eq!(sessions.resume_due_limits(), 1);
    assert!(answered(&hub));
}

/// A park re-tries early on `limit_probe`'s steps. Refused twice, the third re-try is answered:
/// the park lifts there, hours before the reset, and the log says the limit cleared early.
#[test]
fn a_park_re_tries_on_its_steps_and_the_first_answer_lifts_it() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("probe");
    let glm = model(&[
        Act::RefuseWith(GLM_1308_FAR),
        Act::RefuseWith(GLM_1308_FAR),
        Act::RefuseWith(GLM_1308_FAR),
        Act::Answer,
    ]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(&scratch, &gguf, "probe", ("glm", "glm-5.3"), Vec::new());
    let min = |n: u64| Duration::from_secs(n * 60);
    cfg.limit_probe = vec![min(2), min(5), min(15)];
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);
    let reset = T0 + 4 * 3_600_000 + 2_000;

    let out = sessions.submit_operator(&id, PROMPT).expect_err("parked");
    assert!(
        out.to_string()
            .contains("limited until 01:20:02 UTC, asking again at 21:22:02 UTC"),
        "{out}"
    );
    let mut now = T0;
    for (step, refused) in [(2, 2), (5, 3)] {
        let park = sessions
            .harness_of(&id)
            .expect("open")
            .limit_park()
            .expect("still parked")
            .clone();
        assert_eq!(park.reset_ms, reset, "a re-try never moves the reset");
        assert_eq!(park.until_ms, now + min(step).as_millis() as u64 + 2_000);
        assert!(
            !sessions
                .harness_of(&id)
                .expect("open")
                .limit_park_due(park.until_ms - 1)
        );
        now = park.until_ms;
        handle.store(now, Ordering::SeqCst);
        sessions.resume_due_limits();
        assert_eq!(glm.count(), refused, "one call per re-try");
    }
    let park = sessions
        .harness_of(&id)
        .expect("open")
        .limit_park()
        .expect("refused twice, still parked")
        .clone();
    assert_eq!(park.until_ms, now + min(15).as_millis() as u64 + 2_000);
    assert!(said(&hub, "limit_cleared_early").is_empty());
    handle.store(park.until_ms, Ordering::SeqCst);
    assert_eq!(sessions.resume_due_limits(), 1);
    assert_eq!(glm.count(), 4, "the third re-try was answered");
    assert!(answered(&hub));
    let h = sessions.harness_of(&id).expect("open");
    assert!(h.limit_park().is_none(), "the answer lifted the park");
    assert!(
        h.limited_until("glm/glm-5.3").is_none(),
        "nor is the model limited"
    );
    let cleared = said(&hub, "limit_cleared_early");
    assert_eq!(cleared.len(), 1, "{cleared:?}");
    assert!(
        cleared[0].starts_with("limit cleared early at 21:42:06 UTC"),
        "{}",
        cleared[0]
    );
}

/// A `limit_decide` rule over the hook's input: its answer, or a trap.
type Rule = Arc<dyn Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>;

/// A `limit_decide` layer for the hook scenarios: a rule over the input, and every input it saw.
struct LimitRule {
    rule: Rule,
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl letibot_plugins::Module for LimitRule {
    fn hooks(&self) -> Vec<String> {
        vec![letibot_plugin::limit::HOOK.to_string()]
    }

    fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
        let input: serde_json::Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
        self.seen.lock().unwrap().push(input.clone());
        (self.rule)(&input).map(|v| Some(v.to_string()))
    }

    fn fuel(&self) -> u64 {
        5
    }
}

struct LimitLoader {
    rule: Rule,
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl letibot_plugins::Loader for LimitLoader {
    fn load(
        &self,
        _wasm: &std::path::Path,
        _caps: &[letibot_plugins::Capability],
    ) -> Result<Box<dyn letibot_plugins::Module>, String> {
        Ok(Box::new(LimitRule {
            rule: self.rule.clone(),
            seen: self.seen.clone(),
        }))
    }
}

struct NoBuild;

impl letibot_plugins::Builder for NoBuild {
    fn build(&self, dir: &std::path::Path) -> Result<String, letibot_plugins::BuildError> {
        Ok(format!("not built: {}", dir.display()))
    }
}

/// The operator, always choosing the first option.
struct FirstOption;

impl letibot_tools::builtins::intent::Questioner for FirstOption {
    fn ask(
        &self,
        _q: &letibot_tools::builtins::intent::Question,
    ) -> Result<
        (letibot_tools::builtins::intent::QuestionAnswer, String),
        letibot_tools::builtins::intent::AskError,
    > {
        Ok((
            letibot_tools::builtins::intent::QuestionAnswer::choosing(0),
            "operator".into(),
        ))
    }

    fn describe(&self) -> String {
        "a test's operator".into()
    }
}

/// Put a `limit_decide` layer answering with `rule` on the session's stack, and hand back the
/// inputs it is given.
fn with_limit_hook(
    sessions: &Sessions<'_>,
    id: &str,
    workspace: &std::path::Path,
    rule: Rule,
) -> Arc<Mutex<Vec<serde_json::Value>>> {
    let layer = workspace.join(".letibot/plugins/limit-policy");
    std::fs::create_dir_all(&layer).expect("the layer directory");
    std::fs::write(
        layer.join("plugin.toml"),
        "name = \"limit-policy\"\npurpose = \"decides a rate limit\"\n\
         hooks = [\"limit_decide\"]\ncapabilities = []\ncontract = 1\n",
    )
    .expect("a manifest");
    std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Default::default();
    let host = letibot_harnessd::plugins::PluginHost::discover(
        id,
        workspace,
        None,
        None,
        Arc::new(LimitLoader {
            rule,
            seen: seen.clone(),
        }),
        Arc::new(NoBuild),
    );
    // Only a pinned layer decides a limit.
    let trust = letibot_plugins::Trust::in_memory();
    trust.pin("limit-policy", &layer).expect("a pin");
    let host = host.with_trust(trust);
    let h = sessions.harness_of(id).expect("open");
    *h.plugin_slot().lock().unwrap() = Some(Arc::new(host));
    *h.question_slot().lock().unwrap() = Some(Arc::new(FirstOption));
    seen
}

/// A hook keeping a counter in its state answers `wait`, then `ask_user`, then
/// `switch` with the answer, across three refusals; the state is carried between the calls and
/// destroyed after the first success.
#[test]
fn a_limit_hook_waits_then_asks_then_switches_and_its_state_is_carried_and_destroyed() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("hook");
    let glm = model(&[Act::Refuse(300)]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(
        &scratch,
        &gguf,
        "hook",
        ("glm", "glm-5.3"),
        vec!["deepseek/deepseek-flash".into()],
    );
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    let workspace = cfg.workspace.clone();
    // Keyed by session: the fixture has no logs root, so the file lives under the
    // workspace in this session's own directory.
    let state_file = cfg.logs_for(&cfg.session_id).join("limit-hook.json");
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);
    let seen = with_limit_hook(
        &sessions,
        &id,
        &workspace,
        Arc::new(|input| {
            let n = input["state"]["n"].as_u64().unwrap_or(0);
            let now = input["now_ms"].as_u64().unwrap_or(0);
            Ok(match n {
                0 => serde_json::json!({"state": {"n": 1}, "do": "wait", "when": now + 60_000}),
                1 => serde_json::json!({"state": {"n": 2}, "do": "ask_user",
                    "question": "glm is limited - move?", "options": ["deepseek", "stop"]}),
                _ => {
                    let chose = input["answered"]["text"].as_str().unwrap_or("none");
                    assert_eq!(chose, "deepseek", "the answer came back: {input}");
                    serde_json::json!({"state": {"n": 3}, "do": "switch",
                        "provider": "deepseek", "model": "deepseek-flash"})
                }
            })
        }),
    );

    let out = sessions
        .submit_operator(&id, PROMPT)
        .expect_err("parked by the hook");
    assert!(out.to_string().contains("paused until"), "{out}");
    let h = sessions.harness_of(&id).expect("open");
    assert_eq!(
        h.limit_park().expect("parked").until_ms,
        T0 + 60_000 + 2_000,
        "the hook's `when`, not the provider's 300 s"
    );
    assert_eq!(
        h.limit_hook_state("glm/glm-5.3"),
        Some(&serde_json::json!({"n": 1}))
    );
    let kept = std::fs::read_to_string(&state_file).expect("the state is on disk");
    assert!(kept.contains("\"n\":1"), "{kept}");

    handle.store(T0 + 70_000, Ordering::SeqCst);
    sessions.resume_due_limits();
    assert_eq!(glm.count(), 3, "three refusals");
    assert_eq!(deepseek.count(), 1, "the switch took the turn");
    assert!(answered(&hub), "{:?}", said(&hub, "limit_hook"));

    let inputs = seen.lock().unwrap().clone();
    assert_eq!(inputs.len(), 3, "{inputs:?}");
    let states: Vec<_> = inputs.iter().map(|i| i["state"].clone()).collect();
    assert_eq!(
        states,
        vec![
            serde_json::Value::Null,
            serde_json::json!({"n": 1}),
            serde_json::json!({"n": 2})
        ],
        "each call gets the state the last one returned"
    );
    let attempts: Vec<_> = inputs
        .iter()
        .map(|i| i["refusal"]["attempt"].clone())
        .collect();
    assert_eq!(attempts, vec![1, 2, 3]);
    assert_eq!(said(&hub, "limit_hook").len(), 3, "one row per call");

    let h = sessions.harness_of(&id).expect("open");
    assert_eq!(h.model_name(), "deepseek/deepseek-flash");
    assert_eq!(
        h.limit_hook_state("glm/glm-5.3"),
        None,
        "destroyed on success"
    );
    assert!(!state_file.exists(), "and off the disk");
    assert_eq!(said(&hub, "limit_hook_done").len(), 1);
}

/// A hook that runs out of fuel falls back to the fixed logic for the episode, with a row, and is
/// not asked again in it.
#[test]
fn a_limit_hook_out_of_fuel_falls_back_to_the_fixed_rules() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("hook-fuel");
    let glm = model(&[Act::Refuse(300), Act::Answer]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(&scratch, &gguf, "hook-fuel", ("glm", "glm-5.3"), Vec::new());
    let (clock, handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    let workspace = cfg.workspace.clone();
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);
    let seen = with_limit_hook(
        &sessions,
        &id,
        &workspace,
        Arc::new(|_| Err("all fuel consumed by WebAssembly".into())),
    );

    let out = sessions.submit_operator(&id, PROMPT).expect_err("parked");
    assert!(out.to_string().contains("paused until"), "{out}");
    assert_eq!(
        sessions
            .harness_of(&id)
            .expect("open")
            .limit_park()
            .expect("the fixed rule parked it")
            .until_ms,
        T0 + 300_000 + 2_000,
        "the provider's own reset: the fixed rule decided"
    );
    let rows = said(&hub, "limit_hook_fallback");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("fuel"), "{rows:?}");
    assert_eq!(seen.lock().unwrap().len(), 1);

    handle.store(T0 + 400_000, Ordering::SeqCst);
    assert_eq!(sessions.resume_due_limits(), 1);
    assert!(answered(&hub));
    assert_eq!(seen.lock().unwrap().len(), 1, "not asked again");
    assert_eq!(
        sessions
            .harness_of(&id)
            .expect("open")
            .limit_hook_state("glm/glm-5.3"),
        None
    );
}

/// A `switch` to a model the operator did not list as a fallback is refused, and the fixed rules
/// decide the episode.
#[test]
fn a_limit_hook_switch_outside_the_candidates_falls_back() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("hook-off-list");
    let glm = model(&[Act::Refuse(300), Act::Answer]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let mut cfg = config(
        &scratch,
        &gguf,
        "hook-off-list",
        ("glm", "glm-5.3"),
        Vec::new(),
    );
    let (clock, _handle) = letibot_harnessd::config::LimitClock::fixed(T0);
    cfg.limit_clock = clock;
    let workspace = cfg.workspace.clone();
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);
    with_limit_hook(
        &sessions,
        &id,
        &workspace,
        Arc::new(|_| {
            Ok(
                serde_json::json!({"do": "switch", "provider": "deepseek", "model": "deepseek-flash"}),
            )
        }),
    );

    let out = sessions.submit_operator(&id, PROMPT).expect_err("parked");
    assert!(out.to_string().contains("paused until"), "{out}");
    assert_eq!(
        deepseek.count(),
        0,
        "no request went to a model nobody listed"
    );
    let rows = said(&hub, "limit_hook_fallback");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("not one of the candidates"), "{rows:?}");
}

/// `new_session` mid-turn: the conversation is forked into a new session, the
/// turn continues there on the hook's target, and the old session's row names the fork.
#[test]
fn a_limit_hooks_new_session_forks_the_conversation_and_continues_the_turn_there() {
    let _alone = alone();
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = scratch_home("hook-fork");
    let glm = model(&[Act::Refuse(300)]);
    let deepseek = model(&[Act::Answer]);
    providers(&scratch, glm.port, deepseek.port);
    let cfg = config(&scratch, &gguf, "hook-fork", ("glm", "glm-5.3"), Vec::new());
    let workspace = cfg.workspace.clone();
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);
    let seen = with_limit_hook(
        &sessions,
        &id,
        &workspace,
        Arc::new(|_| {
            Ok(serde_json::json!({"state": {}, "do": "new_session",
                "provider": "deepseek", "model": "deepseek-flash"}))
        }),
    );

    let _ = sessions.submit_operator(&id, PROMPT);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(glm.count(), 1);
    assert!(
        seen.lock().unwrap()[0]["session"]["turn_in_flight"]
            .as_bool()
            .unwrap_or(false),
        "the hook was asked mid-turn"
    );
    let fork: Vec<String> = sessions
        .registry()
        .live_ids()
        .into_iter()
        .filter(|s| *s != id)
        .collect();
    assert_eq!(fork.len(), 1, "one new session: {fork:?}");
    let fork = fork[0].clone();
    let rows = said(&hub, "limit_new_session");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains(&format!("forked this conversation into {fork}")),
        "{rows:?}"
    );

    let fork_hub = sessions.registry().get(&fork).expect("the fork is live");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while let letibot_sessionlog::hub::OwnWork::Command(cmd) =
        fork_hub.take_own_work_until(Some(deadline))
    {
        let _ = sessions.dispatch(&fork, &cmd);
        if deepseek.count() > 0 {
            break;
        }
    }
    assert_eq!(deepseek.count(), 1, "the turn continued on the new model");
    assert_eq!(glm.count(), 1, "and not on the refusing one");
    assert!(answered(&fork_hub), "the fork's turn answered");
    let h = sessions.harness_of(&fork).expect("the fork is open");
    assert_eq!(h.model_name(), "deepseek/deepseek-flash");
    let carried = h.items().iter().any(|i| {
        matches!(i, TranscriptItem::User { parts, .. } if parts.iter().any(|p| matches!(
            p, letibot_transcript::UserPart::Text { text } if text == PROMPT)))
    });
    assert!(carried, "the fork holds the conversation: {:?}", h.items());
    let sent = deepseek.seen.lock().unwrap()[0].clone();
    assert!(
        sent.contains(PROMPT),
        "the request carried the conversation: {sent}"
    );
}
