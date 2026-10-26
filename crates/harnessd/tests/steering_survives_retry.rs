//! **A follow-up the operator typed into a round that then failed is in the retried round**
//! (TODO R91.1).
//!
//! The engine hands back what a failed round had taken out of the hub
//! (`Pending::give_back`), and the harness used to build a fresh `HubSteering` on every
//! pass of the HTTP retry loop — so what was handed back went with the value the next
//! pass replaced, and the retried round's prompt never carried it. The operator's words
//! were already in the authorisation trail and nowhere else.
//!
//! Measured here against a loopback stub, through the daemon's own socket: the first
//! request streams a piece, waits until the follow-up is in the hub's queue, streams a
//! second piece (the poll on which the engine takes the follow-up) and then breaks the
//! stream — `Malformed`, which the ladder takes again. The second request must carry the
//! follow-up.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::Registry;

const PROMPT: &str = "say hello";
const FOLLOW_UP: &str = "and-also-mention-the-follow-up-marker";
const ANSWER: &str = "the-retried-round-went-through";

fn delta(text: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\n")
}

/// The stub: the first request is broken mid-stream after `go` is signalled, every later
/// one answers. Each request's body is recorded.
fn flaky_model(
    bodies: Arc<Mutex<Vec<String>>>,
    first_arrived: Sender<()>,
    go: Receiver<()>,
) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut n = 0usize;
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let body = read_request(&mut s);
            bodies.lock().unwrap().push(body);
            n += 1;
            if n == 1 {
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Connection: close\r\n\r\n{}",
                    delta("partial ")
                );
                let _ = s.flush();
                let _ = first_arrived.send(());
                let _ = go.recv_timeout(Duration::from_secs(30));
                let _ = write!(s, "{}data: {{not json\n\n", delta("more "));
                let _ = s.flush();
                let _ = s.shutdown(std::net::Shutdown::Both);
                continue;
            }
            let script = format!(
                "{}data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
                 data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":50,\"completion_tokens\":4}}}}\n\n\
                 data: [DONE]\n\n",
                delta(ANSWER)
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
                return String::from_utf8_lossy(&acc[head_end + 4..]).into_owned();
            }
        }
        if acc.len() > (1 << 22) {
            break;
        }
    }
    String::from_utf8_lossy(&acc).into_owned()
}

#[test]
fn a_follow_up_taken_by_a_failed_round_is_in_the_retried_round() {
    let Some(gguf) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let scratch = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-steer-retry-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(scratch.join("letibot")).expect("the scratch config dir");
    // SAFETY: one test in this binary; nothing reads the environment concurrently.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        std::env::set_var("XDG_DATA_HOME", &scratch);
        std::env::remove_var("DEEPSEEK_API_KEY");
    }

    let bodies = Arc::new(Mutex::new(Vec::new()));
    let (arrived_tx, arrived_rx) = channel();
    let (go_tx, go_rx) = channel();
    let port = flaky_model(bodies.clone(), arrived_tx, go_rx);
    std::fs::write(
        scratch.join("letibot/providers.toml"),
        format!(
            "# Written by steering_survives_retry.rs.\n[deepseek]\nkey = \"sk-test\"\n\
             url = \"http://127.0.0.1:{port}/v1/chat/completions\"\n"
        ),
    )
    .expect("the config file");

    let ws = scratch.join("ws");
    std::fs::create_dir_all(&ws).expect("the workspace");
    let mut cfg = Config::for_this_box(&ws);
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    cfg.vocab_gguf = Some(gguf);
    cfg.session_id = "steer-retry".into();
    cfg.socket = scratch.join("steer.sock");
    cfg.store = Some(scratch.join("steer.db"));
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    let session = cfg.session_id.clone();
    let registry = Registry::new();
    registry
        .create(session.clone(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry");
    let daemon = Daemon::serve(registry.clone(), &cfg.socket).expect("the socket binds");
    let (client, _hello, reader) = HeadClient::attach(
        &cfg.socket,
        &session,
        0,
        "tui",
        "steer-retry-test",
        Caps::default(),
    )
    .expect("a head attaches");
    let (tx, rx) = channel();
    std::thread::spawn(move || pump(reader, tx));
    let hub = registry.get(&session).expect("the session");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    let driver = {
        let registry = registry.clone();
        let hub = hub.clone();
        let bodies = bodies.clone();
        std::thread::spawn(move || {
            let mut client = client;
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                client.prompt(0, PROMPT).expect("the prompt is accepted");
                arrived_rx
                    .recv_timeout(Duration::from_secs(30))
                    .expect("the first round reached the model");
                client
                    .prompt(0, FOLLOW_UP)
                    .expect("the follow-up is accepted");
                // The follow-up is taken by whichever streamed piece is polled after it
                // reaches the hub — the first, if the client had not read it yet, or the
                // second, which goes out only after this pause. Either way it is in the
                // round's `Pending` when the stream breaks.
                std::thread::sleep(Duration::from_millis(500));
                go_tx.send(()).expect("the stub is waiting");
                // The turn's end: the second request answered, or the deadline.
                let deadline = Instant::now() + Duration::from_secs(45);
                while Instant::now() < deadline && bodies.lock().unwrap().len() < 2 {
                    match rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                        Err(e) => panic!("the head's pump died: {e}"),
                    }
                }
                // And the turn is over before the registry closes: a command after it is
                // served only when the worker is back.
                let before = Instant::now();
                client.slash(0, "models").expect("the listing goes out");
                while before.elapsed() < Duration::from_secs(20) && hub.turn_in_flight() {
                    std::thread::sleep(Duration::from_millis(50));
                }
                std::thread::sleep(Duration::from_millis(300));
            }));
            registry.close();
            if let Err(why) = ran {
                std::panic::resume_unwind(why);
            }
        })
    };
    daemon.run(&mut sessions, |_, _| {});
    driver.join().expect("the driver thread");
    daemon.shutdown();

    let bodies = bodies.lock().unwrap().clone();
    assert!(
        bodies.len() >= 2,
        "the failed round was never taken again: {} request(s)",
        bodies.len()
    );
    assert!(
        !bodies[0].contains(FOLLOW_UP),
        "the premise: the follow-up arrived mid-stream, not before the first request"
    );
    assert!(
        bodies[1].contains(FOLLOW_UP),
        "the follow-up the failed round took out of the hub is not in the retried round — \
         it was handed back to a steering the retry loop threw away"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
