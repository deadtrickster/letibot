//! A waiter's mark belongs to the name the ROOM settled on. A second bridge in one folder is
//! renamed on its hello, and a mark filed under the folder's own name is the other seat's, so the
//! wait replays what that seat has already read.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_bridge::rooms::{self, Attention, RoomLink};
use serde_json::{Value, json};

/// A coordinator that refuses the folder's name once - another bridge in a folder of the same
/// name holds it - and admits the suffixed one it is asked for next.
fn coordinator(path: &Path) -> std::thread::JoinHandle<Vec<String>> {
    let listener = UnixListener::bind(path).expect("the fake room listens");
    std::thread::spawn(move || {
        let mut names: Vec<String> = Vec::new();
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(stream) = stream else { continue };
            let mut writer = stream.try_clone().expect("a writer");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            let Ok(_) = reader.read_line(&mut line) else {
                continue;
            };
            let hello: Value = serde_json::from_str(line.trim()).expect("a hello");
            let name = hello["speaker"]["session"]
                .as_str()
                .unwrap_or("")
                .to_string();
            names.push(name);
            if n == 0 {
                let _ = writeln!(
                    writer,
                    "{}",
                    json!({"type": "refused", "why": {"refused": "name_in_use"}})
                );
                continue;
            }
            let _ = writeln!(
                writer,
                "{}",
                json!({"type": "welcome", "protocol": rooms::PROTOCOL, "lease_ms": 0,
                    "beat_every_ms": 0, "threshold": 0, "identity": "fake", "missed": 0,
                    "missed_since_ms": 0, "instance": 1})
            );
            for line in reader.lines().map_while(Result::ok) {
                let f: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                let reply = match f["type"].as_str() {
                    Some("inbox") => json!({"type": "inbox", "skipped": 0, "cursor": 0, "head": 1,
                        "rows": [{"seq": 1, "ts": 0, "room": "host", "author": "operator",
                            "kind": "operator", "to": null, "thread": null, "refs": [],
                            "text": "the migration is done"}]}),
                    _ => json!({"type": "acked", "cursor": 0}),
                };
                let _ = writeln!(writer, "{reply}");
                if f["type"] == "inbox" {
                    // The waiter is answered and hangs up, so the listener closes here: the test's
                    // join must not be a wait on an open socket.
                    return names;
                }
            }
        }
        names
    })
}

/// A scratch data dir of this test's own, before anything reads the bridge's name.
fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bridge-wait-name-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("a scratch root");
    // SAFETY: this test binary has one test, and it sets these before the bridge reads a name.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", &root);
        std::env::remove_var(rooms::NAME_ENV);
    }
    root
}

#[test]
fn a_waiters_mark_is_the_name_the_room_settled_on() {
    let root = scratch("mark");
    let socket = root.join("room.sock");
    let room = coordinator(&socket);
    let base = rooms::handle();
    let link = RoomLink::at(&socket);
    let (code, said) = rooms::wait(
        &link,
        Attention::ToMe,
        Instant::now() + Duration::from_secs(10),
    );
    assert_eq!(code, 0, "the queued line woke the waiter: {said}");
    assert!(said.contains("the migration is done"), "{said}");

    let asked = room.join().expect("the fake room ends");
    assert_eq!(
        asked.len(),
        2,
        "the base name, then the suffixed one: {asked:?}"
    );
    assert_eq!(asked[0], base, "the folder's own name is asked for first");
    let settled = rooms::handle();
    assert_ne!(settled, base, "the room renamed this bridge: {settled}");
    assert_eq!(asked[1], settled, "the name the room admitted");

    // The mark is the settled seat's: the wait wrote it after the hello, and a waiter that filed
    // it under the folder's own name would have left this path empty.
    let mark = rooms::wait_mark_path();
    let text = std::fs::read_to_string(&mark)
        .unwrap_or_else(|e| panic!("no mark at {}: {e}", mark.display()));
    assert_eq!(
        text.split_whitespace().nth(1),
        Some("1"),
        "the mark is past the row the waiter read: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
