//! Lead 57: a bridge redials its room coordinator, and what that costs when the coordinator is not
//! there or is one this build cannot speak. The leads' case is a bridge left running for days
//! against a coordinator that went away: it must not redial at one rate for ever.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_bridge::mcp;
use letibot_bridge::rooms;
use letibot_bridge::tools::{self, Bridge};
use serde_json::{Value, json};

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

fn call_rooms() -> String {
    json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
           "params": {"name": "rooms", "arguments": {}}})
    .to_string()
}

/// A coordinator on a socket of its own, counting every connection it accepts. With `up` false it
/// takes the socket and hangs up without a welcome, which is what a bridge sees of a coordinator
/// that is gone; with `up` true it welcomes the hello, naming `speaks`.
struct Fake {
    path: PathBuf,
    up: Arc<AtomicBool>,
    /// When each accepted connection arrived, from the fake's own start.
    accepted: Arc<Mutex<Vec<Duration>>>,
    /// The live connections, so a test can drop the one the bridge holds.
    conns: Arc<Mutex<Vec<Arc<Mutex<UnixStream>>>>>,
}

impl Fake {
    fn start(tag: &str, speaks: u16) -> Fake {
        // The handle is read once per process: these tests speak as the legacy handle.
        static NAME: std::sync::Once = std::sync::Once::new();
        NAME.call_once(|| {
            // SAFETY: runs before any thread of this file reads the environment for the handle.
            unsafe { std::env::set_var(rooms::NAME_ENV, "claude-code") };
        });
        let dir =
            std::env::temp_dir().join(format!("letibot-room-backoff-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let up = Arc::new(AtomicBool::new(true));
        let accepted: Arc<Mutex<Vec<Duration>>> = Arc::default();
        let conns: Arc<Mutex<Vec<Arc<Mutex<UnixStream>>>>> = Arc::default();
        let fake = Fake {
            path,
            up: up.clone(),
            accepted: accepted.clone(),
            conns: conns.clone(),
        };
        let born = Instant::now();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                accepted.lock().unwrap().push(born.elapsed());
                let conn = Arc::new(Mutex::new(stream.try_clone().unwrap()));
                conns.lock().unwrap().push(conn);
                let up = up.clone();
                std::thread::spawn(move || answer(stream, up, speaks));
            }
        });
        fake
    }

    fn set_up(&self, up: bool) {
        self.up.store(up, Ordering::SeqCst);
    }

    fn seen(&self) -> usize {
        self.accepted.lock().unwrap().len()
    }

    fn times(&self) -> Vec<Duration> {
        self.accepted.lock().unwrap().clone()
    }

    fn drop_connections(&self) {
        for c in self.conns.lock().unwrap().drain(..) {
            let _ = c.lock().unwrap().shutdown(std::net::Shutdown::Both);
        }
    }
}

/// One connection: a welcome when the coordinator is up, then one answer per frame. `inbox` and
/// `ack` are what the supervisor's own pass sends, and they are answered at once so the redial is
/// never measured through a round trip's timeout.
fn answer(stream: UnixStream, up: Arc<AtomicBool>, speaks: u16) {
    let mut out = stream.try_clone().unwrap();
    if !up.load(Ordering::SeqCst) {
        let _ = out.shutdown(std::net::Shutdown::Both);
        return;
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let welcome = json!({"type": "welcome", "protocol": speaks, "lease_ms": 0,
        "beat_every_ms": 0, "threshold": 0, "identity": "fake", "missed": 0,
        "missed_since_ms": 0, "instance": 1});
    if writeln!(out, "{welcome}").is_err() {
        return;
    }
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let reply = json!({"type": "ok", "rows": [], "cursor": 0});
        if writeln!(out, "{reply}").is_err() {
            return;
        }
    }
}

fn wait_until(what: &str, bound: Duration, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + bound;
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The sentence a room tool answers with once the channel is off, waiting for it to become so.
fn wait_for_the_channel_to_go_quiet(bridge: &Bridge) -> String {
    let end = Instant::now() + Duration::from_secs(10);
    let mut last;
    loop {
        match tools::call(bridge, "rooms", &json!({})) {
            Err(why) if why.contains("rooms are off") => return why,
            other => last = format!("{other:?}"),
        }
        assert!(
            Instant::now() < end,
            "the room channel never went quiet: {last}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A coordinator that is not there is not redialled at one rate for ever. Over ten seconds the
/// attempts come at the backoff's spacing - 2s, then 4s, then 8s - not once every two seconds.
#[test]
fn a_coordinator_that_is_not_there_is_redialled_less_and_less_often() {
    let fake = Fake::start("gone", rooms::PROTOCOL);
    fake.set_up(false);
    let link = rooms::RoomLink::at(&fake.path);
    link.stream(Arc::new(|_: &Value, _: bool| true));

    std::thread::sleep(Duration::from_secs(10));
    let times = fake.times();
    assert!(
        times.len() <= 4,
        "a gone coordinator was dialled {} times in ten seconds: {times:?}",
        times.len()
    );
    // The wait grows: the first retry is REDIAL_EVERY, and the one after it is twice as long.
    let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        gaps.len() >= 2,
        "too few attempts to read a backoff: {times:?}"
    );
    assert!(
        gaps[1] >= Duration::from_millis(3_300),
        "the wait between redials did not grow: {gaps:?}"
    );
}

/// A coordinator that comes back is redialled, and the success clears the wait the failure streak
/// had grown: the drop after it is picked up in REDIAL_EVERY, not in minutes.
#[test]
fn a_coordinator_that_comes_back_is_redialled_and_the_wait_starts_over() {
    let fake = Fake::start("reset", rooms::PROTOCOL);
    let link = rooms::RoomLink::at(&fake.path);
    link.stream(Arc::new(|_: &Value, _: bool| true));
    wait_until("the first hello", Duration::from_secs(10), || {
        fake.seen() >= 1
    });

    // The coordinator goes away and stays away long enough for the redial to be slowing down.
    fake.set_up(false);
    fake.drop_connections();
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        fake.seen() >= 2,
        "a gone coordinator is still redialled, more slowly: {:?}",
        fake.times()
    );

    // It comes back: the next attempt is welcomed, which is the success the wait resets on.
    fake.set_up(true);
    let before = fake.seen();
    wait_until(
        "the returned coordinator to be redialled",
        Duration::from_secs(10),
        || fake.seen() > before,
    );
    wait_until(
        "the returned coordinator to be connected",
        Duration::from_secs(5),
        || link.connected(),
    );

    // And with the wait back at its first value, a drop is picked up at once.
    let seen = fake.seen();
    fake.drop_connections();
    let from = Instant::now();
    let end = from + Duration::from_secs(6);
    let mut waited = Duration::ZERO;
    while fake.seen() <= seen {
        assert!(
            Instant::now() < end,
            "the drop after a success was not redialled within {waited:?}: {seen} connections \
             of {:?}, connected {}",
            fake.times(),
            link.connected()
        );
        std::thread::sleep(Duration::from_millis(25));
        waited = from.elapsed();
    }
    assert!(
        from.elapsed() < Duration::from_secs(4),
        "a success left the redial at the failure streak's wait: redialled at {:?} of {:?}",
        from.elapsed(),
        fake.times()
    );
}

/// A welcome naming a protocol this build cannot speak ends the room channel for good: no redial
/// into the same wall, and the MCP server still answers.
#[test]
fn an_unspeakable_coordinator_ends_the_room_channel_and_the_tools_still_answer() {
    // The wire serves 11 through this build's number; a 9 coordinator would misread every frame the
    // bridge sends, and the sentence names the coordinator as the side that is out of date.
    let fake = Fake::start("unspeakable", 9);
    let bridge = Bridge::with_room_socket(&fake.path);
    bridge.rooms().stream(Arc::new(|_: &Value, _: bool| true));

    let said = wait_for_the_channel_to_go_quiet(&bridge);
    assert!(
        said.contains("the room coordinator speaks protocol 9"),
        "{said}"
    );
    assert!(
        said.contains(&format!("this bridge speaks {}", rooms::PROTOCOL)),
        "{said}"
    );
    assert!(
        said.contains("the coordinator is the older of the two"),
        "{said}"
    );
    assert!(
        said.contains("rooms are off until it is restarted"),
        "{said}"
    );

    // Nothing is redialled into it: the count stops where the welcome left it.
    let seen = fake.seen();
    assert!(
        seen <= 2,
        "a coordinator this build cannot speak was dialled {seen} times: {:?}",
        fake.times()
    );
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        fake.seen(),
        seen,
        "the supervisor redialled a coordinator it cannot speak to: {:?}",
        fake.times()
    );

    // The MCP server is not the room: initialize and a tool call are still answered.
    let mut input = Vec::new();
    for line in [INIT, INITIALIZED, &call_rooms()] {
        input.extend_from_slice(line.as_bytes());
        input.push(b'\n');
    }
    let mut out: Vec<u8> = Vec::new();
    mcp::serve(std::io::Cursor::new(input), &mut out, &bridge).expect("the stdio server answers");
    let lines: Vec<Value> = String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert!(
        lines
            .iter()
            .any(|l| l["id"] == 1 && l["result"]["protocolVersion"].is_string()),
        "{lines:?}"
    );
    let answered = lines
        .iter()
        .find(|l| l["id"] == 2)
        .unwrap_or_else(|| panic!("the tool call went unanswered: {lines:?}"));
    assert_eq!(answered["result"]["isError"], true, "{answered}");
    let text = answered["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("rooms are off until it is restarted"),
        "{answered}"
    );
}
