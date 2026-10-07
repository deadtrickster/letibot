//! **A daemon with no key asks for one** — end to end, through the real `harnessd` binary.
//!
//! The operator, on a box with only a provider in mind: *"id prefer leticode to ask me of
//! course, iirc we have popus and masking for sudo"*. Before this, a provider whose key did
//! not resolve made the daemon bind its socket and die before any head could attach.
//!
//! Here: no key anywhere (a scratch `HOME`, `$DEEPSEEK_API_KEY` removed), a providers.toml
//! that only points `[deepseek] url` at a fake, and a head that answers the masked card.
//! The fake refuses the first key with a 401 and accepts the second, so one run proves the
//! ask, the re-ask on a refused key, the save (0600, `[default]` set), and that the key
//! never appears in anything a head is sent.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

const GOOD: &str = "sk-right-0123";
const BAD: &str = "sk-wrong-4567";

/// A fake DeepSeek: 401 for any key but [`GOOD`], one streamed answer for that one. Records
/// the bearer of every request.
fn fake() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (mut len, mut auth) = (0usize, String::new());
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let lower = h.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
                if lower.starts_with("authorization:") {
                    auth = h["authorization:".len()..].trim().to_string();
                }
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            seen2.lock().unwrap().push(auth.clone());
            if auth != format!("Bearer {GOOD}") {
                let msg =
                    r#"{"error":{"message":"Authentication Fails, Your api key is invalid"}}"#;
                let _ = write!(
                    conn,
                    "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{msg}",
                    msg.len()
                );
                continue;
            }
            let events = [
                r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"pong"}}]}"#,
                r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
                r#"{"choices":[],"usage":{"prompt_tokens":40,"completion_tokens":1}}"#,
            ];
            let mut sse = String::new();
            for e in events {
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
    (url, seen)
}

struct Daemon(std::process::Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scratch() -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Short: the socket lives under it, and macOS caps a socket path at 104 bytes.
    let d = std::env::temp_dir().join(format!("lb-key-{}-{:x}", std::process::id(), n as u32));
    std::fs::create_dir_all(d.join("ws")).unwrap();
    std::fs::create_dir_all(d.join(".config/letibot")).unwrap();
    d
}

/// The next `SecretRequested` with no command — the key card — and every frame seen on
/// the way, for the no-key-in-any-frame check.
fn next_key_ask(
    r: &mut letibot_sessionlog::wire::FrameReader<std::os::unix::net::UnixStream>,
    log: &mut Vec<String>,
) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "no key card arrived");
        let f: ServerFrame = r.read().expect("frame");
        log.push(format!("{f:?}"));
        if let ServerFrame::Event(env) = &f
            && let SessionEvent::SecretRequested {
                req_id,
                prompt,
                command,
                ..
            } = &env.event
            && command.is_empty()
        {
            return (req_id.clone(), prompt.clone());
        }
    }
}

fn wait_for_warning(
    r: &mut letibot_sessionlog::wire::FrameReader<std::os::unix::net::UnixStream>,
    log: &mut Vec<String>,
    code: &str,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "no `{code}` warning arrived");
        let f: ServerFrame = r.read().expect("frame");
        log.push(format!("{f:?}"));
        if let ServerFrame::Event(env) = &f
            && let SessionEvent::Warning {
                code: c, detail, ..
            } = &env.event
            && c == code
        {
            return detail.clone();
        }
    }
}

#[test]
fn a_daemon_with_no_key_asks_saves_and_asks_again_when_the_key_is_refused() {
    let (url, seen) = fake();
    let home = scratch();
    let file = home.join(".config/letibot/providers.toml");
    std::fs::write(&file, format!("[deepseek]\nurl = \"{url}\"\n")).unwrap();
    let socket = home.join("d.sock");

    let child = std::process::Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .args([
            "--provider",
            "deepseek",
            "--model",
            "deepseek-flash",
            "--role",
            "leticode",
        ])
        .args(["--session", "s-keyask", "--workspace"])
        .arg(home.join("ws"))
        .arg("--store")
        .arg(home.join("s.db"))
        .arg("--socket")
        .arg(&socket)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env_remove("DEEPSEEK_API_KEY")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(home.join("daemon.log")).unwrap())
        .spawn()
        .expect("harnessd starts");
    let _daemon = Daemon(child);

    // **It comes up** — it used to bind and then die on the missing key.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "the daemon never bound its socket: {}",
            std::fs::read_to_string(home.join("daemon.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let caps = Caps {
        can_decide: true,
        ..Caps::default()
    };
    let (mut head, _hello, mut r) =
        HeadClient::attach(&socket, "s-keyask", 0, "tui", "dead", caps).expect("attach");
    let mut log = Vec::new();

    head.prompt(0, "ping").expect("prompt");

    // The first ask: masked card, no command, says where the key goes.
    let (req1, prompt1) = next_key_ask(&mut r, &mut log);
    assert!(prompt1.contains("deepseek needs an API key"), "{prompt1}");
    assert!(prompt1.contains("providers.toml"), "{prompt1}");
    head.secret(&req1, Some(BAD.into())).expect("answer");

    // Refused by the provider: asked again, with the provider's own words.
    let (req2, prompt2) = next_key_ask(&mut r, &mut log);
    assert!(prompt2.contains("refused the key (401)"), "{prompt2}");
    assert!(prompt2.contains("Authentication Fails"), "{prompt2}");
    head.secret(&req2, Some(GOOD.into())).expect("answer");

    let saved = wait_for_warning(&mut r, &mut log, "provider_key_saved");
    assert!(saved.contains("providers.toml"), "{saved}");

    // The turn ran on the good key.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !seen
        .lock()
        .unwrap()
        .iter()
        .any(|a| a == &format!("Bearer {GOOD}"))
    {
        assert!(
            Instant::now() < deadline,
            "the good key never reached the provider"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // **Saved, private, and the default** — the next `letibot` needs no flag.
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains(&format!("key = \"{GOOD}\"")), "{text}");
    assert!(
        !text.contains(BAD),
        "the refused key is still in the file: {text}"
    );
    assert!(
        text.contains(&format!("url = \"{url}\"")),
        "the url was lost: {text}"
    );
    assert!(
        text.contains("[default]") && text.contains("provider = \"deepseek\""),
        "{text}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "providers.toml is {mode:o}");
    }

    // **And neither key is in anything a head was sent**, nor in the session log.
    for f in &log {
        assert!(
            !f.contains(GOOD) && !f.contains(BAD),
            "a key reached a head: {f}"
        );
    }
    // The database and its write-ahead log: a fresh write sits in `-wal` until a checkpoint.
    for name in ["s.db", "s.db-wal"] {
        let raw = std::fs::read(home.join(name)).unwrap_or_default();
        let raw = String::from_utf8_lossy(&raw);
        assert!(
            !raw.contains(GOOD) && !raw.contains(BAD),
            "a key is in {name}"
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// No head attached (a one-shot `--prompt`): refused at once, naming the variable and the
/// file — not a ten-minute wait for a card nobody can see.
#[test]
fn with_no_head_to_ask_the_refusal_names_where_a_key_goes() {
    let home = scratch();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .args([
            "--provider",
            "deepseek",
            "--role",
            "leticode",
            "--workspace",
        ])
        .arg(home.join("ws"))
        .arg("--socket")
        .arg(home.join("d.sock"))
        .args(["--prompt", "ping"])
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env_remove("DEEPSEEK_API_KEY")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("harnessd runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all.contains("no head is attached to ask for one"), "{all}");
    assert!(all.contains("$DEEPSEEK_API_KEY"), "{all}");
    assert!(all.contains("providers.toml"), "{all}");
    let _ = std::fs::remove_dir_all(&home);
}
