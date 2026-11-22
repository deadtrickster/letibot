//! `room-wait` prints a message ONCE. The plugin's waiter is a loop that restarts this binary for
//! every message, so a message it printed must not come back on the next pass of the same seat: the
//! mark is the seat's, and it survives the process. Each run here is given its own parent shell, so
//! a mark filed under whichever process did the waiting is a different mark every time.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use letibot_bridge::rooms::{self, PROTOCOL};
use serde_json::{Value, json};

/// One message, pushed on every inbox read, and a cursor behind it: the room answers what the seat
/// has not read, and nothing acks here.
fn coordinator(path: &Path) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("the fake room listens");
    std::thread::spawn(move || {
        let mut serving = Vec::new();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let mut writer = stream.try_clone().expect("a writer");
            serving.push(std::thread::spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let _ = writeln!(
                    writer,
                    "{}",
                    json!({"type": "welcome", "protocol": PROTOCOL, "lease_ms": 0, "beat_every_ms": 0,
                        "threshold": 0, "identity": "fake", "missed": 0, "missed_since_ms": 0,
                        "instance": 1})
                );
                for line in reader.lines().map_while(Result::ok) {
                    let f: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                    if f["type"] != "inbox" {
                        let _ = writeln!(writer, "{}", json!({"type": "acked", "cursor": 0}));
                        continue;
                    }
                    let reply = json!({"type": "inbox", "skipped": 0, "cursor": 0, "head": 1,
                        "rows": [{"seq": 1, "ts": 0, "room": "host", "author": "operator",
                            "kind": "operator", "to": null, "thread": null, "refs": [],
                            "text": "the migration is done"}]});
                    let _ = writeln!(writer, "{reply}");
                }
            }));
            // Two runs ask; the third connection would be one this test never made.
            if serving.len() == 2 {
                break;
            }
        }
        for t in serving {
            let _ = t.join();
        }
    })
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bridge-wait-once-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("a scratch root");
    root
}

/// One `room-wait` behind a shell of its own: the shell is the parent, so a mark named after the
/// process that waited would be a new one for every run.
fn run(socket: &Path, data: &Path, timeout: &str) -> (i32, String) {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "exec \"$0\" room-wait --attention to_me --timeout {timeout}",
        ))
        .arg(env!("CARGO_BIN_EXE_letibot-mcp"))
        .env("LETIBOT_ROOM_SOCKET", socket)
        .env("XDG_DATA_HOME", data)
        .env_remove(rooms::SESSION_ENV)
        .env_remove("LETIBOT_ROOM_NAME")
        .current_dir(data)
        .output()
        .expect("the waiter runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

#[test]
fn a_message_one_waiter_printed_is_not_printed_by_the_next() {
    let root = scratch("twice");
    let socket = root.join("room.sock");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let room = coordinator(&socket);

    let (code, said) = run(&socket, &data, "10s");
    assert_eq!(code, 0, "the first pass prints the message: {said:?}");
    assert!(said.contains("the migration is done"), "{said}");

    // The mark is the seat's, so the second run of the same seat finds it and prints nothing.
    let marks = data.join("letibot").join("waiters");
    let kept: Vec<_> = std::fs::read_dir(&marks)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert_eq!(kept.len(), 1, "one mark per seat, not one per process");

    let (code, said) = run(&socket, &data, "2s");
    assert!(
        !said.contains("the migration is done"),
        "the next pass of the same seat prints it again: {said:?}"
    );
    assert_eq!(code, 30, "nothing new before the timeout: {said:?}");
    let after: Vec<_> = std::fs::read_dir(&marks)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert_eq!(after, kept, "the mark did not move to another name");

    let _ = room.join();
    let _ = std::fs::remove_dir_all(&root);
}
