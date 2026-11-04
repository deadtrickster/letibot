//! The bridge as the `claude-code` seat, against a fake coordinator that speaks the room's chat
//! wire and records every frame it takes.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_bridge::mcp;
use letibot_bridge::rooms::{self, Attention, RoomLink};
use letibot_bridge::tools::Bridge;
use serde_json::{Value, json};

struct Fake {
    path: PathBuf,
    frames: Arc<Mutex<Vec<Value>>>,
    conns: Arc<Mutex<Vec<UnixStream>>>,
    log: Arc<Mutex<Vec<Value>>>,
    cursor: Arc<Mutex<u64>>,
    /// The coordinator start the welcome names; `restart` moves it.
    instance: Arc<Mutex<u64>>,
}

impl Fake {
    fn start(tag: &str) -> Fake {
        // The handle is read once per process: these tests speak as the legacy handle.
        static NAME: std::sync::Once = std::sync::Once::new();
        NAME.call_once(|| {
            // SAFETY: runs before any thread of this file reads the environment for the handle.
            unsafe { std::env::set_var(rooms::NAME_ENV, "claude-code") };
        });
        let dir = letibot_sessionlog::testing::scratch_root().join(format!(
            "room-fake-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let fake = Fake {
            path,
            frames: Arc::default(),
            conns: Arc::default(),
            log: Arc::default(),
            cursor: Arc::default(),
            instance: Arc::new(Mutex::new(1)),
        };
        let (frames, conns, log, cursor, instance) = (
            fake.frames.clone(),
            fake.conns.clone(),
            fake.log.clone(),
            fake.cursor.clone(),
            fake.instance.clone(),
        );
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                conns.lock().unwrap().push(stream.try_clone().unwrap());
                let (frames, log, cursor, instance) = (
                    frames.clone(),
                    log.clone(),
                    cursor.clone(),
                    instance.clone(),
                );
                std::thread::spawn(move || serve_one(stream, frames, log, cursor, instance));
            }
        });
        fake
    }

    /// A message appears in the log, and is pushed to every connection with `names_you`.
    fn push(&self, author: &str, kind: &str, room: &str, to: Option<&str>, text: &str) -> Value {
        let mut log = self.log.lock().unwrap();
        let m = json!({"seq": log.len() as u64 + 1, "ts": 0, "room": room, "author": author,
            "kind": kind, "to": to, "thread": null, "refs": [], "text": text});
        log.push(m.clone());
        let names = to == Some("claude-code") || text.contains("@claude-code");
        let frame = json!({"type": "chat", "message": m, "names_you": names});
        for c in self.conns.lock().unwrap().iter_mut() {
            let _ = writeln!(c, "{frame}");
        }
        m
    }

    /// A coordinator that came back with no store: empty log and cursor, a new instance.
    fn restart(&self) {
        self.log.lock().unwrap().clear();
        *self.cursor.lock().unwrap() = 0;
        *self.instance.lock().unwrap() += 1;
        self.drop_connections();
    }

    fn drop_connections(&self) {
        for c in self.conns.lock().unwrap().drain(..) {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
    }

    fn frames(&self, kind: &str) -> Vec<Value> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["type"] == kind)
            .cloned()
            .collect()
    }
}

fn serve_one(
    stream: UnixStream,
    frames: Arc<Mutex<Vec<Value>>>,
    log: Arc<Mutex<Vec<Value>>>,
    cursor: Arc<Mutex<u64>>,
    instance: Arc<Mutex<u64>>,
) {
    let mut w = stream.try_clone().unwrap();
    let mut seat = String::from("session");
    let mut name = String::new();
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let f: Value = serde_json::from_str(&line).unwrap();
        frames.lock().unwrap().push(f.clone());
        let reply = match f["type"].as_str().unwrap() {
            "hello" => {
                seat = f["seat"].as_str().unwrap().to_string();
                name = f["speaker"]["session"].as_str().unwrap().to_string();
                if f["protocol"] != letibot_room::ROOM_PROTOCOL_VERSION {
                    let _ = writeln!(w, "{}", json!({"type": "bye", "why": "protocol"}));
                    return;
                }
                json!({"type": "welcome", "protocol": letibot_room::ROOM_PROTOCOL_VERSION,
                    "lease_ms": 0, "beat_every_ms": 0, "threshold": 0, "identity": "fake",
                    "missed": 0, "missed_since_ms": 0, "instance": *instance.lock().unwrap()})
            }
            "rooms" => json!({"type": "rooms", "rows": [
                {"id": "host", "kind": "host", "project": "", "members": 2,
                 "last_seq": log.lock().unwrap().len(), "unread": 1}]}),
            "say" | "dm" => {
                // The author and kind are the connection's, as on the real coordinator.
                let mut log = log.lock().unwrap();
                let room = if f["type"] == "dm" {
                    "dm"
                } else {
                    f["room"].as_str().unwrap()
                };
                let to = f.get("to").cloned().unwrap_or(Value::Null);
                let m = json!({"seq": log.len() as u64 + 1, "ts": 0, "room": room, "author": name,
                    "kind": seat, "to": to, "thread": f.get("thread").cloned().unwrap_or(Value::Null),
                    "refs": f.get("refs").cloned().unwrap_or(json!([])), "text": f["text"]});
                log.push(m.clone());
                json!({"type": "said", "id": f["id"], "message": m})
            }
            "read" => {
                let log = log.lock().unwrap();
                let since = f["since"].as_u64().unwrap_or(0);
                let rows: Vec<Value> = log
                    .iter()
                    .filter(|m| m["seq"].as_u64().unwrap() > since && m["room"] == f["room"])
                    .cloned()
                    .collect();
                json!({"type": "history", "room": f["room"], "rows": rows,
                    "last_seq": log.len(), "trimmed": false})
            }
            "roster" => json!({"type": "roster", "room": f["room"], "rows": [
                {"name": "claude-code", "kind": "external", "connected": true, "project": "", "label": ""}]}),
            "inbox" => {
                let log = log.lock().unwrap();
                let cur = *cursor.lock().unwrap();
                let att = f["attention"].as_str().unwrap_or("to_me");
                let limit = f["limit"].as_u64().unwrap_or(50) as usize;
                let rows: Vec<Value> = log
                    .iter()
                    .filter(|m| m["seq"].as_u64().unwrap() > cur && m["author"] != "claude-code")
                    .filter(|m| {
                        att == "all"
                            || m["to"] == "claude-code"
                            || (att == "to_me" && m["kind"] == "operator" && m["to"].is_null())
                    })
                    .take(limit)
                    .cloned()
                    .collect();
                json!({"type": "inbox", "rows": rows, "skipped": 0, "cursor": cur, "head": log.len()})
            }
            "ack" => {
                let mut c = cursor.lock().unwrap();
                *c = (*c).max(f["seq"].as_u64().unwrap());
                json!({"type": "acked", "cursor": *c})
            }
            other => json!({"type": "refused", "id": null, "why": {"refused": other}}),
        };
        if writeln!(w, "{reply}").is_err() {
            return;
        }
    }
}

fn call(link: &RoomLink, tool: &str, args: Value) -> Result<Value, String> {
    rooms::call(link, tool, &args)
}

#[test]
fn the_tools_round_trip_against_a_coordinator() {
    let fake = Fake::start("tools");
    let link = RoomLink::at(&fake.path);

    let r = call(&link, "rooms", json!({})).unwrap();
    assert_eq!(r["rows"][0]["id"], "host");
    assert_eq!(r["rows"][0]["unread"], 1);

    let said = call(
        &link,
        "room_say",
        json!({"room": "host", "text": "who has the disk?", "to": "s-b", "thread": 3,
            "refs": [{"kind": "todo", "id": "s-a/x"}]}),
    )
    .unwrap();
    assert_eq!(said["message"]["author"], "claude-code");
    assert_eq!(said["message"]["kind"], "external");
    assert_eq!(said["message"]["to"], "s-b");
    assert_eq!(said["message"]["refs"][0]["kind"], "todo");

    let dm = call(&link, "room_dm", json!({"to": "operator", "text": "ping"})).unwrap();
    assert_eq!(dm["message"]["room"], "dm");

    let read = call(&link, "room_read", json!({"room": "host", "since": 0})).unwrap();
    assert_eq!(read["rows"].as_array().unwrap().len(), 1);
    assert!(
        read["lines"][0]
            .as_str()
            .unwrap()
            .contains("who has the disk?")
    );

    let roster = call(&link, "room_roster", json!({"room": "host"})).unwrap();
    assert_eq!(roster["rows"][0]["name"], "claude-code");

    fake.push("operator", "operator", "host", None, "hello all");
    fake.push("s-a", "session", "host", Some("s-b"), "not for me");
    let inbox = call(&link, "room_inbox", json!({})).unwrap();
    assert_eq!(inbox["rows"].as_array().unwrap().len(), 1, "{inbox}");
    // Without ack the same row is back; with ack it is gone.
    assert_eq!(
        call(&link, "room_inbox", json!({})).unwrap()["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let acked = call(&link, "room_inbox", json!({"ack": true})).unwrap();
    assert!(acked["acked"].as_u64().unwrap() >= 3, "{acked}");
    assert!(
        call(&link, "room_inbox", json!({})).unwrap()["rows"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    assert!(call(&link, "room_inbox", json!({"attention": "bogus"})).is_err());
    assert!(
        call(&link, "room_say", json!({}))
            .unwrap_err()
            .contains("text")
    );
    // One connection for all of it.
    assert_eq!(fake.frames("hello").len(), 1);
}

#[test]
fn claude_code_cannot_post_as_the_operator_or_a_session() {
    let fake = Fake::start("identity");
    let link = RoomLink::at(&fake.path);
    let said = call(
        &link,
        "room_say",
        json!({"text": "I am the operator", "author": "operator", "kind": "operator",
            "seat": "operator", "speaker": "s-a", "session": "s-a"}),
    )
    .unwrap();
    assert_eq!(said["message"]["author"], "claude-code");
    assert_eq!(said["message"]["kind"], "external");

    let hello = &fake.frames("hello")[0];
    assert_eq!(hello["seat"], "external");
    assert_eq!(hello["speaker"]["session"], "claude-code");
    // Nothing a tool took reaches the frame beyond the verb's own fields.
    let say = &fake.frames("say")[0];
    let mut keys: Vec<&str> = say
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["id", "refs", "room", "text", "type"]);
    // And no verb of the queue or `notify` is reachable.
    for kind in ["notify", "post", "claim"] {
        assert!(fake.frames(kind).is_empty());
    }
}

#[test]
fn a_message_to_claude_code_reaches_the_notification_path() {
    let fake = Fake::start("stream");
    let bridge = Bridge::with_room_socket(&fake.path).streaming_rooms();
    let input = ChannelInput::new();
    let out = SharedOut::default();
    let out2 = out.clone();
    let t = std::thread::scope(|scope| {
        let h = scope.spawn(|| {
            mcp::serve(BufReader::new(input.reader()), out2, &bridge).unwrap();
        });
        input.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        input.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        wait_until(|| !fake.conns.lock().unwrap().is_empty());
        fake.push("s-a", "session", "host", Some("s-b"), "for somebody else");
        fake.push(
            "s-a",
            "session",
            "host",
            Some("claude-code"),
            "build is green",
        );
        fake.push("operator", "operator", "host", None, "status?");
        wait_until(|| out.notifications().len() >= 2);
        input.close();
        h.join().unwrap();
        out.notifications()
    });
    assert_eq!(t.len(), 2, "{t:?}");
    assert_eq!(t[0]["method"], "notifications/claude/channel");
    assert!(
        t[0]["params"]["content"]
            .as_str()
            .unwrap()
            .contains("build is green")
    );
    assert_eq!(t[0]["params"]["meta"]["author"], "s-a");
    assert_eq!(t[0]["params"]["meta"]["seq"], "2");
    assert!(
        t[1]["params"]["content"]
            .as_str()
            .unwrap()
            .contains("status?")
    );
    let init = out.lines().into_iter().find(|l| l["id"] == 1).unwrap();
    assert!(init["result"]["capabilities"]["experimental"]["claude/channel"].is_object());
}

#[test]
fn the_stream_reconnects_and_replays_what_the_gap_held() {
    let fake = Fake::start("redial");
    let bridge = Bridge::with_room_socket(&fake.path).streaming_rooms();
    let input = ChannelInput::new();
    let out = SharedOut::default();
    let out2 = out.clone();
    std::thread::scope(|scope| {
        let h = scope.spawn(|| {
            mcp::serve(BufReader::new(input.reader()), out2, &bridge).unwrap();
        });
        input.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        wait_until(|| !fake.conns.lock().unwrap().is_empty());
        fake.push("s-a", "session", "host", Some("claude-code"), "one");
        wait_until(|| out.notifications().len() == 1);
        // The connection drops, a message lands in the log, the supervisor redials and replays.
        fake.drop_connections();
        {
            let mut log = fake.log.lock().unwrap();
            let seq = log.len() as u64 + 1;
            log.push(
                json!({"seq": seq, "ts": 0, "room": "host", "author": "s-a", "kind": "session",
                "to": "claude-code", "thread": null, "refs": [], "text": "two"}),
            );
        }
        wait_until(|| out.notifications().len() == 2);
        input.close();
        h.join().unwrap();
    });
    let n = out.notifications();
    assert!(n[1]["params"]["content"].as_str().unwrap().contains("two"));
    assert_eq!(fake.frames("hello").len(), 2);
}

#[test]
fn the_bridge_says_the_rooms_protocol_number() {
    assert_eq!(rooms::PROTOCOL, letibot_room::ROOM_PROTOCOL_VERSION);
    // The fake refuses any other number, so a link that connects spoke the constant.
    let fake = Fake::start("protocol");
    let link = RoomLink::at(&fake.path);
    link.ensure().expect("the hello was accepted");
    let hello = fake.frames("hello");
    assert_eq!(hello[0]["protocol"], letibot_room::ROOM_PROTOCOL_VERSION);
}

#[test]
fn a_restarted_coordinator_resets_the_delivered_mark() {
    let fake = Fake::start("restart");
    let bridge = Bridge::with_room_socket(&fake.path).streaming_rooms();
    let input = ChannelInput::new();
    let out = SharedOut::default();
    let out2 = out.clone();
    std::thread::scope(|scope| {
        let h = scope.spawn(|| {
            mcp::serve(BufReader::new(input.reader()), out2, &bridge).unwrap();
        });
        input.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        wait_until(|| !fake.conns.lock().unwrap().is_empty());
        fake.push("s-a", "session", "host", Some("claude-code"), "one");
        fake.push("s-a", "session", "host", Some("claude-code"), "two");
        wait_until(|| out.notifications().len() == 2);
        // The coordinator lost its log: the next message is seq 1 again.
        fake.restart();
        {
            let mut log = fake.log.lock().unwrap();
            log.push(
                json!({"seq": 1, "ts": 0, "room": "host", "author": "s-a", "kind": "session",
                "to": "claude-code", "thread": null, "refs": [], "text": "after"}),
            );
        }
        wait_until(|| out.notifications().len() == 3);
        input.close();
        h.join().unwrap();
    });
    let n = out.notifications();
    assert!(
        n[2]["params"]["content"]
            .as_str()
            .unwrap()
            .contains("after")
    );
}

#[test]
fn a_message_the_writer_did_not_take_is_offered_again() {
    let fake = Fake::start("retake");
    let link = RoomLink::at(&fake.path);
    let taken = Arc::new(Mutex::new(Vec::<u64>::new()));
    let refuse = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (t, r) = (taken.clone(), refuse.clone());
    link.stream(Arc::new(move |m| {
        if r.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        t.lock().unwrap().push(m["seq"].as_u64().unwrap());
        true
    }));
    wait_until(|| link.connected());
    fake.push("s-a", "session", "host", Some("claude-code"), "one");
    std::thread::sleep(Duration::from_millis(100));
    assert!(taken.lock().unwrap().is_empty());
    refuse.store(false, std::sync::atomic::Ordering::SeqCst);
    // The supervisor's next replay (after a drop) offers the unacked row again.
    fake.drop_connections();
    wait_until(|| taken.lock().unwrap().as_slice() == [1]);
}

#[test]
fn room_wait_exits_on_a_matching_message_and_not_on_others() {
    let fake = Fake::start("wait");
    let link = RoomLink::at(&fake.path);
    // Nothing for claude-code: the deadline passes with code 30 and no output.
    let lonely = std::thread::scope(|scope| {
        let h = scope.spawn(|| {
            rooms::wait(
                &link,
                Attention::ToMe,
                Instant::now() + Duration::from_millis(1500),
            )
        });
        wait_until(|| !fake.conns.lock().unwrap().is_empty());
        fake.push("s-a", "session", "host", Some("s-b"), "not for me");
        fake.push("claude-code", "external", "host", None, "my own");
        h.join().unwrap()
    });
    assert_eq!(lonely, (30, String::new()));

    // A message for it ends the wait, and the cursor moves past it so the next wait blocks.
    let link2 = RoomLink::at(&fake.path);
    let (code, said) = std::thread::scope(|scope| {
        let h = scope.spawn(|| {
            rooms::wait(
                &link2,
                Attention::ToMe,
                Instant::now() + Duration::from_secs(20),
            )
        });
        wait_until(|| fake.conns.lock().unwrap().len() >= 2);
        std::thread::sleep(Duration::from_millis(200));
        fake.push(
            "s-a",
            "session",
            "host",
            Some("claude-code"),
            "disk is free",
        );
        h.join().unwrap()
    });
    assert_eq!(code, 0);
    assert!(
        said.contains("disk is free") && said.contains("s-a"),
        "{said}"
    );
    let link3 = RoomLink::at(&fake.path);
    let (code, _) = rooms::wait(
        &link3,
        Attention::ToMe,
        Instant::now() + Duration::from_millis(1200),
    );
    assert_eq!(code, 30, "the acked message is not delivered again");

    // A coordinator that is not there is exit 20 with the reason.
    let gone = RoomLink::at("/nonexistent/room.sock");
    let (code, why) = rooms::wait(
        &gone,
        Attention::ToMe,
        Instant::now() + Duration::from_secs(5),
    );
    assert_eq!(code, 20);
    assert!(why.contains("no room coordinator"), "{why}");
}

fn wait_until(mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[derive(Clone, Default)]
struct SharedOut(Arc<Mutex<Vec<u8>>>);

impl Write for SharedOut {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedOut {
    fn lines(&self) -> Vec<Value> {
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
    fn notifications(&self) -> Vec<Value> {
        self.lines()
            .into_iter()
            .filter(|l| l["method"] == "notifications/claude/channel")
            .collect()
    }
}

/// A stdin that stays open until `close`, fed one line at a time.
struct ChannelInput {
    tx: Mutex<Option<std::sync::mpsc::Sender<Vec<u8>>>>,
    rx: Mutex<Option<std::sync::mpsc::Receiver<Vec<u8>>>>,
}

impl ChannelInput {
    fn new() -> ChannelInput {
        let (tx, rx) = std::sync::mpsc::channel();
        ChannelInput {
            tx: Mutex::new(Some(tx)),
            rx: Mutex::new(Some(rx)),
        }
    }
    fn reader(&self) -> ChannelReader {
        ChannelReader {
            rx: self.rx.lock().unwrap().take().unwrap(),
            buf: Vec::new(),
        }
    }
    fn send(&self, line: &str) {
        let _ = self
            .tx
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(format!("{line}\n").into_bytes());
    }
    fn close(&self) {
        self.tx.lock().unwrap().take();
    }
}

struct ChannelReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
}

impl std::io::Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.buf.is_empty() {
            match self.rx.recv() {
                Ok(b) => self.buf = b,
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len());
        out[..n].copy_from_slice(&self.buf[..n]);
        self.buf.drain(..n);
        Ok(n)
    }
}
