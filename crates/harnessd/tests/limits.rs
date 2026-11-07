//! A usage limit with a known reset never fails a session: the operator's ruling, on fake
//! providers and a clock the test moves by hand.
//!
//! Two answers, one file: `wait` parks the session until the instant the provider named and
//! resumes the same turn there; `switch` moves the turn to a model that is not limited and moves
//! back after the reset. A rate limit that named no instant keeps the ladder, then waits the long
//! beat - and after a few of those it fails as the tree always failed it.

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
        .submit(&id, PROMPT)
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
        .submit(&id, "and one more thing")
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
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    let out = sessions
        .submit(&id, PROMPT)
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
    let moved = said(&hub, "model_endpoint_retry");
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
        .submit(&id, PROMPT)
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
        .submit(&id, PROMPT)
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
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let (mut sessions, hub, id) = open(&parts, cfg);

    // The first turn moves off the limited model, to the first name that is not limited.
    sessions
        .submit(&id, PROMPT)
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
        .submit(&id, "and again")
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
        said(&hub, "model_endpoint_retry")
            .iter()
            .any(|d| d.contains("put a time on this model's own limit")),
        "the skip is said, not silent: {:?}",
        said(&hub, "model_endpoint_retry")
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
        .submit(&id, PROMPT)
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
