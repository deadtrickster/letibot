//! **A restarted daemon gives a head the session's earlier prompts, bodies and all** — the
//! fact the composer's Up history is now derived from.
//!
//! The operator: *"i worked - sent 30 prompts. then restart, send 2. and arrow up sees only
//! these two"*. The head now builds its history from the session's operator rows; that is
//! only as good as what a fresh attach to a RESUMED session carries. This runs the real
//! `harnessd` twice over one store — three prompts, a kill, a resume — and reads the second
//! daemon's `Hello` snapshot.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

/// A fake provider that answers every request with one short message, counting them.
fn fake() -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let n = Arc::new(Mutex::new(0usize));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            *n2.lock().unwrap() += 1;
            let mut sse = String::new();
            for e in [
                r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"ok"}}]}"#,
                r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
                r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            ] {
                sse.push_str(&format!("data: {e}\n\n"));
            }
            sse.push_str("data: [DONE]\n\n");
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
                sse.len()
            );
        }
    });
    (url, n)
}

struct Daemon(std::process::Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(home: &Path) -> Daemon {
    let socket = home.join("d.sock");
    let _ = std::fs::remove_file(&socket);
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .args([
            "--provider",
            "deepseek",
            "--model",
            "deepseek-flash",
            "--role",
            "leticode",
        ])
        .args(["--session", "s-hist", "--workspace"])
        .arg(home.join("ws"))
        .arg("--store")
        .arg(home.join("s.db"))
        .arg("--socket")
        .arg(&socket)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env_remove("DEEPSEEK_API_KEY")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("harnessd starts");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "the daemon never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon(child)
}

fn scratch() -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "lb-hist-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("ws")).unwrap();
    std::fs::create_dir_all(d.join(".config/letibot")).unwrap();
    d
}

#[test]
fn a_resumed_sessions_snapshot_carries_every_earlier_prompt() {
    let (url, seen) = fake();
    let home = scratch();
    std::fs::write(
        home.join(".config/letibot/providers.toml"),
        format!("[deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n"),
    )
    .unwrap();
    let caps = Caps {
        can_decide: true,
        ..Caps::default()
    };
    let prompts = ["first prompt", "second prompt", "third prompt"];

    {
        let _d = start(&home);
        let (mut head, _hello, _r) = HeadClient::attach(
            &home.join("d.sock"),
            "s-hist",
            0,
            "tui",
            "dead",
            caps.clone(),
        )
        .expect("attach");
        for (i, p) in prompts.iter().enumerate() {
            head.prompt(0, p).expect("prompt");
            let deadline = Instant::now() + Duration::from_secs(30);
            while *seen.lock().unwrap() < i + 1 {
                assert!(
                    Instant::now() < deadline,
                    "turn {i} never reached the provider"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        // Let the last turn land in the store before the kill.
        std::thread::sleep(Duration::from_millis(800));
        // **A head restarted against the live daemon** — the other "restart".
        let (_h2, hello2, mut r2) = HeadClient::attach(
            &home.join("d.sock"),
            "s-hist",
            0,
            "tui",
            "dead2",
            caps.clone(),
        )
        .expect("a second head");
        let live = collect(hello2, &mut r2, &prompts);
        for p in prompts {
            assert!(
                live.iter().any(|c| c == p),
                "a re-attached head lost `{p}`: {live:?}"
            );
        }
    }

    // **The restart**, over the same store and session id: a resume.
    let _d = start(&home);
    let (_head, hello, mut r) =
        HeadClient::attach(&home.join("d.sock"), "s-hist", 0, "tui", "dead", caps)
            .expect("attach after restart");
    let carried = collect(hello, &mut r, &prompts);
    for p in prompts {
        assert!(
            carried.iter().any(|c| c == p),
            "the resumed snapshot lost `{p}` — Up would not recall it: {carried:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// The operator's prompts a fresh head gets: from the snapshot, and from what follows it (a
/// resume republishes its rows as events) — the two places a head builds its rows from.
fn collect(
    hello: ServerFrame,
    r: &mut letibot_sessionlog::wire::FrameReader<std::os::unix::net::UnixStream>,
    prompts: &[&str],
) -> Vec<String> {
    let ServerFrame::Hello { snapshot, .. } = hello else {
        panic!("not a hello: {hello:?}");
    };
    let text_of = |item: &TranscriptItem| match item {
        TranscriptItem::User {
            speaker: Speaker::Operator,
            parts,
        } => Some(
            parts
                .iter()
                .filter_map(|p| match p {
                    UserPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<String>(),
        ),
        _ => None,
    };
    // What the snapshot carries, and what the resume republishes after it — a head builds
    // its rows from both, so the history is built from both.
    let mut carried: Vec<String> = snapshot
        .map(|s| {
            s.items
                .iter()
                .filter_map(|it| text_of(it.item.as_ref()?))
                .collect()
        })
        .unwrap_or_default();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && prompts.iter().any(|p| !carried.iter().any(|c| c == p)) {
        let Ok(f) = r.read::<ServerFrame>() else {
            break;
        };
        if let ServerFrame::Event(env) = f
            && let letibot_sessionlog::event::SessionEvent::TranscriptContent { item, .. } =
                &env.event
            && let Some(t) = text_of(item)
        {
            carried.push(t);
        }
    }
    carried
}
