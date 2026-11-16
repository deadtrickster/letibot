//! A coordinator older than this build, at its own socket. The frame this build added mid-series
//! (`may_prompt`) must be refused here, by name, instead of being sent to a reader that answers it
//! by closing the connection.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use letibot_room::client::Client;
use letibot_room::{Request, Speaker};

struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        use std::sync::atomic::AtomicU64;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let p =
            std::env::temp_dir().join(format!("letibot-room-{what}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A coordinator that speaks protocol `protocol` and, like the build that shipped 12 before
/// `may_prompt`, answers a frame it does not know by closing the connection.
fn mid_series_coordinator(socket: PathBuf, protocol: u16, saw: Arc<AtomicBool>) {
    let listener = UnixListener::bind(&socket).expect("the stub coordinator binds");
    for stream in listener.incoming() {
        let Ok(stream) = stream else { return };
        let said = saw.clone();
        std::thread::spawn(move || {
            let mut writer = stream.try_clone().expect("a writer");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok() && !line.trim().is_empty() {
                let frame: serde_json::Value =
                    serde_json::from_str(line.trim()).unwrap_or_default();
                match frame["type"].as_str() {
                    Some("hello") => {
                        let welcome = serde_json::json!({
                            "type": "welcome",
                            "protocol": protocol,
                            "lease_ms": 30_000,
                            "beat_every_ms": 5_000,
                            "threshold": 3,
                        });
                        let _ = writeln!(writer, "{welcome}");
                    }
                    // The frame a 12 build predates: it is not a frame to this reader at all, so
                    // it drops the connection exactly as `ReadFrame::Bad` does.
                    Some("may_prompt") => {
                        said.store(true, Ordering::SeqCst);
                        return;
                    }
                    _ => {
                        let _ = writeln!(writer, "{}", serde_json::json!({ "type": "noted" }));
                    }
                }
                line.clear();
            }
        });
    }
}

#[test]
fn a_v12_mid_series_coordinator_refuses_may_prompt_by_name() {
    let scratch = Scratch::new("skew");
    let socket = scratch.0.join("room.sock");
    let saw = Arc::new(AtomicBool::new(false));
    let stub = {
        let (socket, saw) = (socket.clone(), saw.clone());
        std::thread::spawn(move || mid_series_coordinator(socket, 12, saw))
    };
    // The bind happens on the stub's thread; a connect before it is a refused connection, not a
    // failure of the thing under test.
    let mut tries = 0;
    while !socket.exists() && tries < 200 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        tries += 1;
    }

    let client = Client::connect(&socket, Speaker::top_level("s-a"), Arc::new(|_| {}))
        .expect("the stub coordinator welcomes this session");
    let asked = client.request(&Request::MayPrompt {
        id: "p1".into(),
        to: "s-b".into(),
        project: "letibot".into(),
        because: None,
    });
    let why = match asked {
        Err(e) => e.to_string(),
        Ok(other) => panic!("a 12 coordinator is not sent `may_prompt`: {other:?}"),
    };
    assert!(
        why.contains("may_prompt") && why.contains("12"),
        "the refusal names the frame and the protocol: {why}"
    );
    assert!(
        !saw.load(Ordering::SeqCst),
        "nothing was written to the old coordinator: {why}"
    );
    assert!(
        client.connected(),
        "a refused frame is not a dropped connection"
    );
    drop(stub);
}
