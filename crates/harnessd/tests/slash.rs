//! `/flowy` and `/models` over the socket: a head sends the line, the daemon
//! answers on the session log, and the answer says what to type next.
//!
//! Needs the GLM vocabulary on this box to open a session (no model server —
//! nothing here runs a turn); skips loudly without it.

use std::time::{Duration, Instant};

use letibot_harnessd::config::Config;
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::Registry;

const VOCAB: &str = "/home/dead/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf";

#[test]
fn slash_verbs_answer_on_the_log_and_name_the_next_command() {
    if !std::path::Path::new(VOCAB).is_file() {
        eprintln!(
            "SKIPPED: no GLM vocabulary at {VOCAB} — the check did not run and this is not a pass"
        );
        return;
    }
    // The operator's providers.toml must not be touched: point the config home
    // at a scratch directory for this process.
    let scratch = std::env::temp_dir().join(format!("letibot-slash-it-{}", std::process::id()));
    std::fs::create_dir_all(scratch.join("letibot")).unwrap();
    // SAFETY: this test binary is single-threaded at this point; nothing else
    // reads the environment concurrently.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        // opencode's store is a key source too; the developer's own login must not
        // reach this assertion.
        std::env::set_var("XDG_DATA_HOME", &scratch);
        std::env::remove_var("DEEPSEEK_API_KEY");
    }

    let ws = scratch.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let mut cfg = Config::for_this_box(&ws);
    cfg.dialect = Dialect::Glm;
    cfg.model = "glm-5.3-flash".into();
    cfg.vocab_gguf = VOCAB.into();
    cfg.socket = scratch.join("d.sock");
    cfg.session_id = "slash-test".into();
    let session = cfg.session_id.clone();
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let registry = Registry::new();
    registry
        .create(session.clone(), "", Sessions::wiring(&cfg))
        .unwrap();
    let daemon = Daemon::serve(registry.clone(), &cfg.socket).expect("the socket binds");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    let (mut client, _hello, reader) =
        HeadClient::attach(&cfg.socket, &session, 0, "tui", "test", Caps::default())
            .expect("a head attaches");
    let (tx, rx) = std::sync::mpsc::channel();
    let _pump = std::thread::spawn(move || pump(reader, tx));

    let mut send = |line: &str, sessions: &mut Sessions<'_>| -> String {
        client.slash(0, line).expect("the frame goes out");
        // The test is the worker: take work off the bell until the command.
        loop {
            match registry.next_work().expect("the bell is open") {
                letibot_sessionlog::registry::Work::Open(id) => {
                    let _ = sessions.open(&id);
                }
                letibot_sessionlog::registry::Work::Woken(_) => {}
                letibot_sessionlog::registry::Work::Command(id, cmd) => {
                    let _ = sessions.dispatch(&id, &cmd);
                    break;
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(frame) => {
                    if let letibot_sessionlog::client::Inbound::Frame(
                        letibot_sessionlog::protocol::ServerFrame::Event(env),
                    ) = frame
                        && let SessionEvent::Warning { code, detail, .. } = env.event
                        && code.starts_with("slash")
                    {
                        return detail;
                    }
                }
                Err(_) => panic!("no slash answer for `{line}` within 10s"),
            }
        }
    };

    let status = send("flowy status", &mut sessions);
    assert!(status.contains("no seat is attached"), "{status}");
    assert!(status.contains("/flowy login"), "{status}");

    let models = send("models", &mut sessions);
    assert!(
        models.contains("now answering: local — glm-5.3-flash"),
        "{models}"
    );
    assert!(
        models.contains("deepseek") && models.contains("NO KEY"),
        "{models}"
    );

    let refused = send("models deepseek", &mut sessions);
    assert!(
        refused.contains("/models deepseek --key PASTE"),
        "{refused}"
    );

    let switched = send("models deepseek/deepseek-chat --key sk-test", &mut sessions);
    assert!(switched.contains("stored the deepseek key"), "{switched}");
    assert!(
        switched.contains("turns go to deepseek/deepseek-chat"),
        "{switched}"
    );
    let now = send("models", &mut sessions);
    assert!(
        now.contains("now answering: deepseek/deepseek-chat (metered)"),
        "{now}"
    );
    assert!(scratch.join("letibot/providers.toml").is_file());

    let back = send("models local", &mut sessions);
    assert!(back.contains("turns go to the local server"), "{back}");

    let login = send("flowy login nosuchseat", &mut sessions);
    assert!(login.contains("no token for seat `nosuchseat`"), "{login}");

    daemon.shutdown();
    let _ = std::fs::remove_dir_all(&scratch);
}
