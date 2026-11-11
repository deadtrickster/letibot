//! R178, over the real daemon: a folder whose package session closed still takes the next one.
//! Live, the daemon stayed up holding 0 sessions and every later `leticode_start` on that folder
//! was refused with "no such session" and a list of ids from every project in the store. Here the
//! only session is closed, a bare attach is served as the control connection that makes one, and
//! the session it makes runs a turn - which is what says it opened.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{ClientError, HeadClient};
use letibot_sessionlog::protocol::{Caps, NOTE_SESSION_CLOSED, ServerFrame};

/// A fake provider on loopback answering every request with one short message, counting them.
fn fake() -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let n = Arc::new(Mutex::new(0usize));
    let seen = n.clone();
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
            *seen.lock().unwrap() += 1;
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
            // No queue, so no review pass and no gatekeeper session: the daemon holds exactly the
            // one session this test closes, which is the state the failure was found in.
            "--no-queue",
        ])
        .args(["--session", "s-only", "--workspace"])
        .arg(home.join("ws"))
        // A throwaway store directory: `shared.db` and this folder's own project file land here.
        .arg("--store")
        .arg(home.join("store"))
        .arg("--socket")
        .arg(&socket)
        .env("HOME", home)
        // The landing root, never the box's real `~/.local/share/letibot` (an inherited
        // `XDG_DATA_HOME` would reach it past the `HOME` above).
        .env("LETIBOT_LANDING_ROOT", home.join("landing"))
        .env("LETIBOT_NOTES_REPO", home.join("notes"))
        .env("LETIBOT_LOGS_ROOT", home.join("logs"))
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
    let d = letibot_sessionlog::testing::scratch_root().join(format!(
        "lb-new-session-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("ws")).unwrap();
    std::fs::create_dir_all(d.join(".config/letibot")).unwrap();
    d
}

fn caps() -> Caps {
    Caps {
        can_decide: true,
        ..Caps::default()
    }
}

#[test]
fn a_daemon_whose_only_session_closed_takes_a_new_one_and_runs_it() {
    let (url, seen) = fake();
    let home = scratch();
    std::fs::write(
        home.join(".config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/daemon_takes_a_new_session.rs — a fake on loopback.\n\
             [deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n"
        ),
    )
    .unwrap();
    let socket = home.join("d.sock");
    let _daemon = start(&home);

    // The package session ends - the close `leticode_stop` sends, served by the daemon.
    {
        let (mut head, hello, _reader) =
            HeadClient::attach(&socket, "s-only", 0, "tui", "dead", caps()).expect("attach");
        assert!(
            matches!(&hello, ServerFrame::Hello { session_id, .. } if session_id == "s-only"),
            "the daemon opens its named session: {hello:?}"
        );
        head.close_session(0, "dead", "the package is done")
            .expect("the close is sent");
        // Read to the ack: the close is served before it, so the answer is the fact.
        let mut reader = _reader;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                Instant::now() < deadline,
                "the close was never acked, so this test never reached a daemon holding none"
            );
            match reader.read::<ServerFrame>() {
                Ok(ServerFrame::Accepted { note, .. }) => {
                    assert_eq!(note, NOTE_SESSION_CLOSED);
                    break;
                }
                Ok(_) => {}
                Err(e) => panic!("the close was answered with no ack: {e}"),
            }
        }
        let _ = head.detach();
    }

    // Wait for the daemon to hold nothing: an attach by name is the read of exactly that.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            Instant::now() < deadline,
            "the daemon still holds the closed session, so nothing here is about a daemon \
             holding none"
        );
        match HeadClient::attach(&socket, "s-only", 0, "tui", "dead", caps()) {
            Err(ClientError::Refused(reason)) => {
                assert!(
                    reason.contains("holds none live"),
                    "the refusal names what it holds, and it holds nothing: {reason}"
                );
                break;
            }
            // Still live (or just closed): the removal is the worker's own next step.
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    // The bare attach a `leticode_start` makes on a folder whose daemon is already up.
    let (mut control, hello, mut reader) =
        HeadClient::attach(&socket, "", 0, "leticode", "bridge", caps()).expect("the bare attach");
    assert!(
        matches!(&hello, ServerFrame::Hello { session_id, .. } if session_id.is_empty()),
        "it is a control connection, named no session: {hello:?}"
    );
    control
        .new_session("the next package", &home.join("ws").display().to_string())
        .expect("the new-session frame goes");
    let deadline = Instant::now() + Duration::from_secs(30);
    let made = loop {
        assert!(
            Instant::now() < deadline,
            "the daemon never answered the new-session frame"
        );
        match reader.read::<ServerFrame>() {
            Ok(ServerFrame::Sessions { created, .. }) => break created.expect("an id"),
            Ok(_) => {}
            Err(e) => panic!("the new-session frame went unanswered: {e}"),
        }
    };
    assert!(
        !made.is_empty() && made != "s-only",
        "the next session is its own: {made}"
    );

    // And it OPENS: a head holds it, a prompt reaches the model, and an answer comes back. The
    // frames it sends back are not this test's subject - the provider's own count is.
    let (mut head, hello, _reader) = HeadClient::attach(&socket, &made, 0, "tui", "dead", caps())
        .expect("attach to the new one");
    assert!(
        matches!(&hello, ServerFrame::Hello { session_id, .. } if *session_id == made),
        "the socket belongs to the session it names: {hello:?}"
    );
    let before = *seen.lock().unwrap();
    head.prompt(0, "are you there").expect("the prompt goes");
    let deadline = Instant::now() + Duration::from_secs(60);
    while *seen.lock().unwrap() == before {
        assert!(
            Instant::now() < deadline,
            "the new session never ran a turn, so it never opened"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = head.detach();
    let _ = std::fs::remove_dir_all(&home);
}
