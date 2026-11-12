//! The permission relay, end to end against a fake coordinator and a fake MCP client: Claude Code's
//! own prompt becomes one DM to the operator, and the operator's answer becomes the verdict - from
//! nobody else, for an open id only, and never twice.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_bridge::mcp;
use letibot_bridge::relay::{CAPABILITY, REQUEST, VERDICT};
use letibot_bridge::rooms;
use letibot_bridge::tools::Bridge;
use serde_json::{Value, json};

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

/// A `permission_request` as Claude Code sends one: a notification, and no answer is expected.
fn request(id: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": REQUEST,
        "params": {
            "request_id": id,
            "tool_name": "Bash",
            "description": "list the files in the worktree",
            "input_preview": "{\"command\":\"ls -l\"}",
        },
    })
    .to_string()
}

/// One accepted connection and the single writer every frame on it goes through - the shape the
/// coordinator has (`Conn` in `crates/room/src/server.rs`), and what keeps a pushed frame from
/// arriving before the welcome the bridge reads as the answer to its hello.
struct Conn {
    out: Mutex<Out>,
    greeted: Condvar,
}

struct Out {
    stream: UnixStream,
    greeted: bool,
}

impl Conn {
    fn new(stream: UnixStream) -> Arc<Conn> {
        Arc::new(Conn {
            out: Mutex::new(Out {
                stream,
                greeted: false,
            }),
            greeted: Condvar::new(),
        })
    }

    fn send(&self, frame: &Value, welcome: bool) {
        let mut out = self.out.lock().unwrap();
        let _ = writeln!(out.stream, "{frame}");
        if welcome {
            out.greeted = true;
            self.greeted.notify_all();
        }
    }

    /// A pushed message waits for the welcome: a coordinator says nothing before it, and a chat
    /// frame first is a line the bridge reads as its hello's answer. Bounded, so a connection
    /// that is never greeted cannot hang a test.
    fn deliver(&self, frame: &Value) {
        let mut out = self.out.lock().unwrap();
        let end = Instant::now() + Duration::from_secs(20);
        while !out.greeted {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            out = self.greeted.wait_timeout(out, left).unwrap().0;
        }
        let _ = writeln!(out.stream, "{frame}");
    }

    fn close(&self) {
        let _ = self
            .out
            .lock()
            .unwrap()
            .stream
            .shutdown(std::net::Shutdown::Both);
    }
}

/// A coordinator that speaks the room's chat wire: it records every frame and pushes what a test
/// says into the log, as the real one pushes what somebody said.
struct Fake {
    path: PathBuf,
    frames: Arc<Mutex<Vec<Value>>>,
    conns: Arc<Mutex<Vec<Arc<Conn>>>>,
    log: Arc<Mutex<Vec<Value>>>,
}

impl Fake {
    fn start(tag: &str) -> Fake {
        // The handle is read once per process: these tests speak as the legacy handle, which is
        // the one the pushes below address.
        static NAME: std::sync::Once = std::sync::Once::new();
        NAME.call_once(|| {
            // SAFETY: runs before any thread of this file reads the environment for the handle.
            unsafe { std::env::set_var(rooms::NAME_ENV, "claude-code") };
        });
        let dir = letibot_sessionlog::testing::scratch_root().join(format!(
            "permission-relay-{tag}-{}-{}",
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
        };
        let (frames, conns, log) = (fake.frames.clone(), fake.conns.clone(), fake.log.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let conn = Conn::new(stream.try_clone().unwrap());
                conns.lock().unwrap().push(conn.clone());
                let (frames, log) = (frames.clone(), log.clone());
                std::thread::spawn(move || serve_one(stream, conn, frames, log));
            }
        });
        fake
    }

    /// A message appears in the room and is pushed to every connection, as the coordinator pushes
    /// what somebody said.
    fn push(&self, author: &str, kind: &str, room: &str, to: Option<&str>, text: &str) {
        let m = {
            let mut log = self.log.lock().unwrap();
            let m = json!({"seq": log.len() as u64 + 1, "ts": 0, "room": room, "author": author,
                "kind": kind, "to": to, "thread": null, "refs": [], "text": text});
            log.push(m.clone());
            m
        };
        let names = to == Some("claude-code") || text.contains("@claude-code");
        let frame = json!({"type": "chat", "message": m, "names_you": names});
        let conns: Vec<Arc<Conn>> = self.conns.lock().unwrap().clone();
        for c in &conns {
            c.deliver(&frame);
        }
    }

    /// The coordinator hangs up on everybody, as a restart does. What is pushed until the bridge
    /// redials reaches the log only.
    fn hang_up(&self) {
        for c in self.conns.lock().unwrap().drain(..) {
            c.close();
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
    conn: Arc<Conn>,
    frames: Arc<Mutex<Vec<Value>>>,
    log: Arc<Mutex<Vec<Value>>>,
) {
    let mut name = String::new();
    let mut seat = String::from("session");
    let mut cursor: u64 = 0;
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let f: Value = serde_json::from_str(&line).unwrap();
        frames.lock().unwrap().push(f.clone());
        let welcome = f["type"] == "hello";
        let reply = match f["type"].as_str() {
            Some("hello") => {
                name = f["speaker"]["session"].as_str().unwrap_or("").to_string();
                seat = f["seat"].as_str().unwrap_or("external").to_string();
                json!({"type": "welcome", "protocol": letibot_room::ROOM_PROTOCOL_VERSION,
                    "lease_ms": 0, "beat_every_ms": 0, "threshold": 0, "identity": "fake",
                    "missed": 0, "missed_since_ms": 0, "instance": 1})
            }
            // The author and the seat are the connection's, as on the real coordinator: nothing a
            // frame carries can make a bridge or a session an operator.
            Some(kind @ ("say" | "dm")) => {
                let room = if kind == "dm" {
                    "dm".to_string()
                } else {
                    f["room"].as_str().unwrap_or("host").to_string()
                };
                let mut m = json!({"seq": log.lock().unwrap().len() as u64 + 1, "ts": 0,
                    "room": room, "author": name, "kind": seat,
                    "to": f.get("to").cloned().unwrap_or(Value::Null),
                    "thread": Value::Null, "refs": [], "text": f["text"]});
                m["to"] = f.get("to").cloned().unwrap_or(Value::Null);
                log.lock().unwrap().push(m.clone());
                json!({"type": "said", "id": f["id"], "message": m})
            }
            Some("inbox") => {
                let log = log.lock().unwrap();
                let rows: Vec<Value> = log
                    .iter()
                    .filter(|m| m["seq"].as_u64().unwrap() > cursor && m["author"] != "claude-code")
                    .take(f["limit"].as_u64().unwrap_or(50) as usize)
                    .cloned()
                    .collect();
                json!({"type": "inbox", "rows": rows, "skipped": 0, "cursor": cursor,
                    "head": log.len()})
            }
            Some("ack") => {
                cursor = cursor.max(f["seq"].as_u64().unwrap_or(0));
                json!({"type": "acked", "cursor": cursor})
            }
            other => json!({"type": "refused", "id": null, "why": {"refused": other}}),
        };
        conn.send(&reply, welcome);
    }
}

/// Run one MCP session against `bridge` while `body` drives the client, and hand back every line
/// the bridge wrote: the answers and the notifications together, since a verdict is a notification.
fn with_client(bridge: &Bridge, body: impl FnOnce(&ChannelInput, &SharedOut)) -> Vec<Value> {
    let input = ChannelInput::new();
    let out = SharedOut::default();
    let out2 = out.clone();
    std::thread::scope(|scope| {
        let h = scope.spawn(|| mcp::serve(BufReader::new(input.reader()), out2, bridge).unwrap());
        // A failed assertion still closes the input: the scope otherwise waits on `serve` forever
        // and the failure reads as a hang.
        let body = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&input, &out)));
        input.close();
        h.join().unwrap();
        if let Err(panic) = body {
            std::panic::resume_unwind(panic);
        }
    });
    out.lines()
}

fn wait_until(mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn verdicts(lines: &[Value]) -> Vec<Value> {
    lines
        .iter()
        .filter(|l| l["method"] == VERDICT)
        .cloned()
        .collect()
}

/// The capability is absent when the key is off - a client that is promised a verdict would wait
/// for one - and present when it is on.
#[test]
fn the_capability_is_declared_only_when_the_config_asks() {
    let off = with_client(&Bridge::new(), |input, _| {
        input.send(INIT);
    });
    let offered = &off[0]["result"]["capabilities"]["experimental"];
    assert!(offered["claude/channel"].is_object(), "{offered}");
    assert!(
        offered[CAPABILITY].is_null(),
        "the relay was not asked for: {offered}"
    );

    let on = with_client(&Bridge::new().relaying(), |input, _| {
        input.send(INIT);
    });
    let offered = &on[0]["result"]["capabilities"]["experimental"];
    assert!(offered["claude/channel"].is_object(), "{offered}");
    assert!(offered[CAPABILITY].is_object(), "{offered}");
}

/// One prompt, one DM, and the DM is the operator's: the tool, both untrusted halves, the id and
/// the line that says how to answer it.
#[test]
fn a_request_dms_the_operator_once_and_carries_the_id() {
    let fake = Fake::start("ask");
    let bridge = Bridge::with_room_socket(&fake.path)
        .streaming_rooms()
        .relaying();
    with_client(&bridge, |input, _| {
        input.send(INIT);
        input.send(INITIALIZED);
        wait_until(|| !fake.conns.lock().unwrap().is_empty());
        input.send(&request("abcde"));
        wait_until(|| fake.frames("dm").len() == 1);
        assert_eq!(bridge.relay_open(), vec!["abcde".to_string()]);
        // The same call is not a second card.
        input.send(&request("abcde"));
        std::thread::sleep(Duration::from_millis(300));
    });
    let dms = fake.frames("dm");
    assert_eq!(dms.len(), 1, "one prompt, one DM: {dms:?}");
    assert_eq!(dms[0]["to"], "operator");
    let text = dms[0]["text"].as_str().expect("a DM has text");
    assert!(text.contains("`Bash`"), "{text}");
    assert!(
        text.contains("description (untrusted): \"list the files in the worktree\""),
        "{text}"
    );
    assert!(
        text.contains("input (untrusted): {\"command\":\"ls -l\"}"),
        "{text}"
    );
    assert!(text.contains("abcde"), "{text}");
    assert!(
        text.contains("reply \"yes abcde\" or \"no abcde\""),
        "{text}"
    );
    // Nobody answered it, so the prompt is still open: an id is closed by a verdict, not by time
    // passing in a test.
    assert_eq!(bridge.relay_open(), vec!["abcde".to_string()]);
}

/// The operator's word is the verdict: `yes` allows and `no` denies, one call each, and the id is
/// closed by the one answer it gets.
#[test]
fn the_operators_yes_allows_and_no_denies() {
    for (answer, behavior) in [("yes", "allow"), ("no", "deny")] {
        let fake = Fake::start(&format!("verdict-{answer}"));
        let bridge = Bridge::with_room_socket(&fake.path)
            .streaming_rooms()
            .relaying();
        let lines = with_client(&bridge, |input, out| {
            input.send(INIT);
            input.send(INITIALIZED);
            wait_until(|| !fake.conns.lock().unwrap().is_empty());
            input.send(&request("abcde"));
            wait_until(|| fake.frames("dm").len() == 1);
            fake.push(
                "operator",
                "operator",
                "dm",
                Some("claude-code"),
                &format!("{answer} abcde"),
            );
            wait_until(|| !verdicts(&out.lines()).is_empty());
            assert!(
                bridge.relay_open().is_empty(),
                "the id is closed by its verdict"
            );
        });
        let sent = verdicts(&lines);
        assert_eq!(sent.len(), 1, "{answer}: {sent:?}");
        assert_eq!(sent[0]["params"]["request_id"], "abcde");
        assert_eq!(sent[0]["params"]["behavior"], behavior);
    }
}

/// Only the authenticated operator answers. A session saying `yes abcde` is a model writing words,
/// and the bridge's own seat saying it is the same - neither can approve tool use.
#[test]
fn a_session_or_a_bridge_saying_yes_sends_nothing() {
    let fake = Fake::start("not-the-operator");
    let bridge = Bridge::with_room_socket(&fake.path)
        .streaming_rooms()
        .relaying();
    let lines = with_client(&bridge, |input, out| {
        input.send(INIT);
        input.send(INITIALIZED);
        wait_until(|| !fake.conns.lock().unwrap().is_empty());
        input.send(&request("abcde"));
        wait_until(|| fake.frames("dm").len() == 1);
        // Each is read by the bridge's reader, which is what the test needs to know: the stream
        // does not deliver the bridge's own words back to it, so that one is waited out.
        for (author, kind, room, to) in [
            ("s-a", "session", "host", Some("claude-code")),
            ("cc@x", "external", "host", Some("claude-code")),
            ("claude-code", "external", "dm", Some("operator")),
        ] {
            let seen = out.lines().len();
            fake.push(author, kind, room, to, "yes abcde");
            if author == "claude-code" {
                std::thread::sleep(Duration::from_millis(200));
            } else {
                wait_until(|| out.lines().len() > seen);
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(bridge.relay_open(), vec!["abcde".to_string()], "still open");
    });
    assert!(verdicts(&lines).is_empty(), "{lines:?}");
}

/// An id nobody asked about sends no verdict, and the operator is told once why: they believe they
/// answered something, and silence would leave them waiting.
#[test]
fn an_unknown_id_sends_nothing_and_costs_one_line() {
    let fake = Fake::start("unknown");
    let bridge = Bridge::with_room_socket(&fake.path)
        .streaming_rooms()
        .relaying();
    let lines = with_client(&bridge, |input, out| {
        input.send(INIT);
        input.send(INITIALIZED);
        wait_until(|| !fake.conns.lock().unwrap().is_empty());
        let seen = out.lines().len();
        fake.push(
            "operator",
            "operator",
            "dm",
            Some("claude-code"),
            "yes zzzzz",
        );
        wait_until(|| out.lines().len() > seen);
        wait_until(|| fake.frames("dm").len() == 1);
        std::thread::sleep(Duration::from_millis(200));
    });
    assert!(verdicts(&lines).is_empty(), "{lines:?}");
    let dms = fake.frames("dm");
    assert_eq!(dms.len(), 1, "{dms:?}");
    let said = dms[0]["text"].as_str().unwrap_or("");
    assert!(
        said.contains("zzzzz") && said.contains("not an open request"),
        "{said}"
    );
    assert_eq!(said.lines().count(), 1, "one line: {said}");
}

/// A verdict said while the bridge was offline still lands: the redial's catch-up reaches the relay,
/// not only the model.
#[test]
fn a_verdict_said_while_the_bridge_redials_still_lands() {
    let fake = Fake::start("redial");
    let bridge = Bridge::with_room_socket(&fake.path)
        .streaming_rooms()
        .relaying();
    let lines = with_client(&bridge, |input, out| {
        input.send(INIT);
        input.send(INITIALIZED);
        wait_until(|| !fake.conns.lock().unwrap().is_empty());
        input.send(&request("abcde"));
        wait_until(|| fake.frames("dm").len() == 1);
        let hellos = fake.frames("hello").len();
        fake.hang_up();
        fake.push(
            "operator",
            "operator",
            "dm",
            Some("claude-code"),
            "yes abcde",
        );
        wait_until(|| fake.frames("hello").len() > hellos);
        wait_until(|| !verdicts(&out.lines()).is_empty());
    });
    let sent = verdicts(&lines);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["params"]["behavior"], "allow");
}

/// A verdict covers one call: the second `yes` for an id that is already answered sends nothing.
#[test]
fn a_second_verdict_for_the_same_id_sends_nothing() {
    let fake = Fake::start("twice");
    let bridge = Bridge::with_room_socket(&fake.path)
        .streaming_rooms()
        .relaying();
    let lines = with_client(&bridge, |input, out| {
        input.send(INIT);
        input.send(INITIALIZED);
        wait_until(|| !fake.conns.lock().unwrap().is_empty());
        input.send(&request("abcde"));
        wait_until(|| fake.frames("dm").len() == 1);
        fake.push(
            "operator",
            "operator",
            "dm",
            Some("claude-code"),
            "yes abcde",
        );
        wait_until(|| !verdicts(&out.lines()).is_empty());
        let seen = out.lines().len();
        fake.push(
            "operator",
            "operator",
            "dm",
            Some("claude-code"),
            "no abcde",
        );
        wait_until(|| out.lines().len() > seen);
        wait_until(|| fake.frames("dm").len() == 2);
        std::thread::sleep(Duration::from_millis(200));
    });
    let sent = verdicts(&lines);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["params"]["behavior"], "allow");
}

/// A request with the key off changes nothing: no DM, and no verdict even when the operator says
/// one - the capability was never declared, so nothing asked.
#[test]
fn nothing_happens_when_the_key_is_off() {
    let fake = Fake::start("off");
    let bridge = Bridge::with_room_socket(&fake.path).streaming_rooms();
    let lines = with_client(&bridge, |input, out| {
        input.send(INIT);
        input.send(INITIALIZED);
        wait_until(|| !fake.conns.lock().unwrap().is_empty());
        input.send(&request("abcde"));
        std::thread::sleep(Duration::from_millis(300));
        let seen = out.lines().len();
        fake.push(
            "operator",
            "operator",
            "dm",
            Some("claude-code"),
            "yes abcde",
        );
        wait_until(|| out.lines().len() > seen);
        std::thread::sleep(Duration::from_millis(200));
    });
    assert!(fake.frames("dm").is_empty(), "no prompt was asked");
    assert!(verdicts(&lines).is_empty(), "{lines:?}");
    assert!(bridge.relay_open().is_empty());
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
