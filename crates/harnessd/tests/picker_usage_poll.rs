//! A picker opening polls a door no session watches, against a mock endpoint on loopback, and the
//! reading lands in the session's `models.limits` row. One test: the environment is process-wide.

use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::protocol::MODEL_LIMITS_KEY;
use letibot_sessionlog::registry::Registry;

const SID: &str = "s-picker-poll";

/// Kimi's usages answer with 44% of the 5-hour window spent.
const KIMI: &str =
    r#"{"usages":{"limit_5h":{"used_ratio":0.44,"reset_time":"2099-01-01T07:07:00Z"}}}"#;

/// A usage endpoint on loopback that answers [`KIMI`] and counts the requests.
fn mock() -> (String, Arc<AtomicUsize>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let base = format!("http://{}", l.local_addr().expect("its address"));
    let hits = Arc::new(AtomicUsize::new(0));
    let n = hits.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(&s);
            let mut line = String::new();
            while r.read_line(&mut line).is_ok_and(|k| k > 2) {
                line.clear();
            }
            n.fetch_add(1, Ordering::SeqCst);
            let _ = write!(
                &s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{KIMI}",
                KIMI.len()
            );
        }
    });
    (base, hits)
}

/// The doors this process could reach, and the operator's keys, taken off it: only the fixture's
/// `providers.toml` may name a key.
fn isolate(root: &std::path::Path) {
    let mut keys: Vec<&str> = Vec::new();
    for p in letibot_provider::presets::ALL {
        keys.push(p.key_env);
        keys.extend(p.alt_envs.iter().copied());
    }
    let doors: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("LETIBOT_"))
        .collect();
    // SAFETY: this binary has ONE test, and this runs before any thread exists.
    unsafe {
        for k in keys {
            std::env::remove_var(k);
        }
        for k in doors {
            std::env::remove_var(k);
        }
        std::env::set_var("HOME", root.join("home"));
        for (var, sub) in [
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            std::env::set_var(var, root.join(sub));
        }
    }
}

#[test]
fn opening_the_picker_polls_a_door_no_session_has_watched() {
    let root = letibot_sessionlog::testing::scratch("picker-usage-poll");
    for d in ["home", "config/letibot", "data", "state", "cache", "ws"] {
        std::fs::create_dir_all(root.join(d)).expect("scratch dirs");
    }
    isolate(&root);
    let (base, hits) = mock();
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/picker_usage_poll.rs - loopback only.\n\
             [deepseek]\nkey = \"sk-not-used\"\nurl = \"http://127.0.0.1:9/chat/completions\"\n\
             [kimi-code]\nkey = \"sk-fixture\"\nurl = \"{base}/coding/v1/chat/completions\"\n"
        ),
    )
    .expect("providers.toml");

    let mut cfg = Config::for_this_box(root.join("ws"));
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.http_retries = 0;
    cfg.session_id = SID.into();
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let parts: &'static Parts = Box::leak(Box::new(Parts::load(&cfg).expect("byte vocabulary")));
    let registry = Registry::new();
    registry
        .create(SID.to_string(), "", Sessions::wiring(&cfg))
        .expect("the session");
    let _sessions = Sessions::open_first(parts, cfg, registry.clone()).expect("it opens");

    let kimi = || {
        registry
            .settings(SID)
            .into_iter()
            .find(|r| r.key == MODEL_LIMITS_KEY)
            .and_then(|r| r.choices.into_iter().find(|c| c.starts_with("kimi-code ")))
    };
    assert_eq!(kimi(), None, "nothing is known of a door nobody watched");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no poll before the picker");

    assert!(registry.poll_usage(SID), "the harness installed the poll");
    let deadline = Instant::now() + Duration::from_secs(10);
    while kimi().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let line = kimi().expect("the mock's reading reached the row");
    assert!(line.starts_with("kimi-code 0 5h 44% · resets "), "{line}");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // A second open inside the period is debounced: the door is not asked again.
    assert!(registry.poll_usage(SID));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(&root);
}
