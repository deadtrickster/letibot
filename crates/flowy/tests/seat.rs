//! The seat against a fake node: the delivery contract, end to end, through the
//! monitor registry the harness wakes on.
//!
//! The fake speaks exactly the doors the seat uses — `whoami`, `inbox/wait`
//! (a real long poll, on a condvar), `inbox/ack`, `inbox/readers`,
//! `chat/{room}/say`, `artifact/{id}`, `events` — and records the ORDER things
//! happened in, because the contract under test is an ordering: spool before
//! ack, mark over everything read, notice before silence.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_flowy::attention::{Attention, Level};
use letibot_flowy::creds::{Credentials, Source, parse_addr};
use letibot_flowy::seat::{PollOutcome, Seat, SeatState};
use letibot_tools::exec::monitor::{Condition, CustomWatch, MAX_TTL, Monitors, Watch};
use letibot_tools::exec::{ScopeId, ScopeKind};
use serde_json::{Value, json};

// ------------------------------------------------------------- the fake node

#[derive(Default)]
struct NodeState {
    queue: VecDeque<Value>,
    next_hlc: i64,
    cursor: i64,
    acks: Vec<(i64, bool, bool)>, // (cursor, delivered, spool_had_it)
    said: Vec<Value>,
    reader_exists: bool,
    reject_token: bool,
    artifact: Value,
    /// The seat's spool path, so an ack can check what was written before it.
    spool: Option<PathBuf>,
    last_query: String,
}

struct FakeNode {
    addr: String,
    st: Arc<(Mutex<NodeState>, Condvar)>,
    down: Arc<Mutex<bool>>,
}

impl FakeNode {
    fn start() -> FakeNode {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let st = Arc::new((
            Mutex::new(NodeState {
                next_hlc: 1000,
                reader_exists: true,
                artifact: json!({"id": "ART", "type": "diagram", "title": "the plan", "body": "v1", "updated": "t1"}),
                ..Default::default()
            }),
            Condvar::new(),
        ));
        let down = Arc::new(Mutex::new(false));
        let (st2, down2) = (st.clone(), down.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                if *down2.lock().unwrap() {
                    drop(conn); // slam the door: unreachable
                    continue;
                }
                let st = st2.clone();
                std::thread::spawn(move || serve(conn, st));
            }
        });
        FakeNode { addr, st, down }
    }

    fn push(&self, e: Value) {
        let (m, cv) = &*self.st;
        m.lock().unwrap().queue.push_back(e);
        cv.notify_all();
    }

    fn set_down(&self, d: bool) {
        *self.down.lock().unwrap() = d;
    }

    fn state(&self) -> std::sync::MutexGuard<'_, NodeState> {
        self.st.0.lock().unwrap()
    }
}

fn serve(conn: TcpStream, st: Arc<(Mutex<NodeState>, Condvar)>) {
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut len = 0usize;
    let mut auth = String::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 {
            return;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some(v) = h.strip_prefix("Content-Length:") {
            len = v.trim().parse().unwrap_or(0);
        }
        if let Some(v) = h.strip_prefix("Authorization:") {
            auth = v.trim().to_string();
        }
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body).unwrap();
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let q = |k: &str| -> String {
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(String::from))
            .unwrap_or_default()
    };

    if st.0.lock().unwrap().reject_token || auth != "Bearer good" {
        respond(conn, 401, &json!({"error": "bad token"}));
        return;
    }

    match (method.as_str(), path) {
        ("GET", "/api/whoami") => respond(
            conn,
            200,
            &json!({"user": "U-SEAT", "agent": "A-SEAT", "agent_kind": "worker", "project": "Lab"}),
        ),
        ("GET", "/api/inbox/readers") => {
            let s = st.0.lock().unwrap();
            let readers = if s.reader_exists {
                json!([{"reader": "seat", "cursor": s.cursor}])
            } else {
                json!([])
            };
            respond(conn, 200, &json!({"readers": readers}));
        }
        ("GET", "/api/inbox/wait") => {
            let window = Duration::from_secs(q("window").parse().unwrap_or(1));
            let (m, cv) = &*st;
            let mut s = m.lock().unwrap();
            if !s.reader_exists {
                drop(s);
                respond(
                    conn,
                    404,
                    &json!({"error": "no inbox reader called seat for this principal - declare it first with --new. readers here: other-seat", "readers": ["other-seat"]}),
                );
                return;
            }
            s.last_query = query.to_string();
            let deadline = Instant::now() + window.min(Duration::from_secs(2));
            while s.queue.is_empty() && Instant::now() < deadline {
                let (g, _) = cv.wait_timeout(s, Duration::from_millis(50)).unwrap();
                s = g;
            }
            let mut events = Vec::new();
            let since = s.cursor;
            while let Some(mut e) = s.queue.pop_front() {
                s.next_hlc += 1;
                e["seq_hlc"] = json!(s.next_hlc);
                s.cursor = s.next_hlc;
                events.push(e);
            }
            let cursor = s.cursor;
            drop(s);
            respond(
                conn,
                200,
                &json!({"reader": "seat", "events": events, "skipped": 2, "since": since,
                        "cursor": cursor, "now": "2026-09-14T10:00:00Z"}),
            );
        }
        ("POST", "/api/inbox/ack") => {
            let cursor = body["cursor"].as_i64().unwrap_or(0);
            let delivered = body["delivered"].as_bool().unwrap_or(false);
            let mut s = st.0.lock().unwrap();
            let spool_has = s
                .spool
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| t.contains(&format!("\"seq_hlc\":{cursor}")))
                .unwrap_or(false);
            s.acks.push((cursor, delivered, spool_has));
            respond(conn, 200, &json!({"reader": "seat", "cursor": cursor}));
        }
        ("POST", p) if p.starts_with("/api/chat/") && p.ends_with("/say") => {
            let room = p.trim_start_matches("/api/chat/").trim_end_matches("/say");
            let mut s = st.0.lock().unwrap();
            let mut e = body.clone();
            e["id"] = json!("SAID-1");
            e["type"] = json!("chat");
            e["room"] = json!(room);
            s.said.push(e.clone());
            respond(conn, 200, &e);
        }
        ("GET", p) if p.starts_with("/api/artifact/") => {
            let s = st.0.lock().unwrap();
            respond(conn, 200, &s.artifact.clone());
        }
        ("GET", "/api/events") => respond(conn, 200, &json!({"events": []})),
        _ => respond(
            conn,
            404,
            &json!({"error": format!("no such door {method} {path}")}),
        ),
    }
}

fn respond(mut conn: TcpStream, code: u16, body: &Value) {
    let b = body.to_string();
    let _ = write!(
        conn,
        "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
        b.len()
    );
    let _ = conn.flush();
}

// ------------------------------------------------------------- helpers

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "letibot-flowy-it-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn creds(node: &FakeNode, token_file: Option<PathBuf>) -> Credentials {
    Credentials {
        addr: node.addr.clone(),
        endpoint: parse_addr(&node.addr).unwrap(),
        agent: "seat".into(),
        token: "good".into(),
        token_file,
        addr_from: Source::Explicit,
        agent_from: Source::Explicit,
        token_from: Source::Explicit,
    }
}

fn chat(from: &str, kind: &str, to: &str, body: &str) -> Value {
    json!({
        "id": format!("m-{body}"), "type": "chat", "project": "Lab", "room": "general",
        "actor": from, "addressee": to, "body": body,
        "meta": {"actor_kind": kind, "actor_name": from}, "created": "2026-09-14T09:59:00Z"
    })
}

fn scope(dir: &std::path::Path) -> ScopeId {
    ScopeId {
        kind: ScopeKind::Session,
        name: "t".into(),
        path: dir.to_path_buf(),
    }
}

// ------------------------------------------------------------- the tests

#[test]
fn a_message_is_spooled_then_acked_then_fires_the_monitor_through_the_table() {
    let node = FakeNode::start();
    let d = tmp("deliver");
    let seat = Seat::open(creds(&node, None), Some(&d), Some(&d)).unwrap();
    node.state().spool = Some(seat.spool().path().to_path_buf());

    let cond = seat.attach("s1", Attention::default());
    let monitors = Arc::new(Monitors::new());
    monitors
        .declare(
            "flowy",
            scope(&d),
            Watch::Custom(CustomWatch(cond.clone())),
            None,
            "harnessd",
            MAX_TTL,
            true,
        )
        .unwrap();
    seat.keep_renewing(&monitors, "flowy");

    // Addressed to the seat, a person's broadcast, and an agent talking to
    // somebody else: two wake, one is counted.
    node.push(chat(
        "claude-host",
        "agent",
        "A-SEAT",
        "gating X - run Y on Z",
    ));
    node.push(chat("deadtrickster", "user", "", "who is here?"));
    node.push(chat("claude-host", "agent", "A-OTHER", "not for the seat"));

    assert_eq!(seat.poll_once(), PollOutcome::Delivered(3));
    assert!(seat.state().is_listening());

    // Spool before ack, and the ack said `delivered`.
    let s = node.state();
    assert_eq!(s.acks.len(), 1, "{:?}", s.acks);
    let (cursor, delivered, spool_had_it) = s.acks[0];
    assert_eq!(cursor, 1003);
    assert!(delivered);
    assert!(
        spool_had_it,
        "the spool must hold the page BEFORE the ack goes out"
    );
    drop(s);

    // The firing, through the registry the harness wakes on.
    let fired = monitors.wait_for_any(0, Duration::from_secs(3));
    assert_eq!(fired.len(), 1, "{fired:?}");
    let why = match &fired[0].fired {
        letibot_tools::exec::monitor::Fired::Fired { why, .. } => why.clone(),
        other => panic!("{other:?}"),
    };
    assert!(why.contains("2 message(s) for seat `seat`"), "{why}");
    assert!(why.contains("Lab/#general · claude-host → you"), "{why}");
    assert!(why.contains("deadtrickster (person)"), "{why}");
    assert!(!why.contains("not for the seat"), "{why}");
    assert!(
        why.contains("1 went past that your attention table did not ask for; 2 the node filtered"),
        "{why}"
    );
    assert!(why.contains("clock reads 2026-09-14T10:00:00Z"), "{why}");
    // Continuous: still live after firing.
    assert!(monitors.get("flowy").is_some());

    // The identity was filled from whoami on the first poll.
    assert_eq!(cond.identity().agent_id, "A-SEAT");

    // A quiet window acks with delivered=false and the mark still moves.
    assert_eq!(seat.poll_once(), PollOutcome::Delivered(0));
    let s = node.state();
    assert_eq!(s.acks.len(), 2);
    assert!(!s.acks[1].1);
}

#[test]
fn the_wire_level_is_the_loosest_any_session_wants() {
    let node = FakeNode::start();
    let d = tmp("wire");
    let seat = Seat::open(creds(&node, None), Some(&d), Some(&d)).unwrap();
    // Nobody attached: addressed.
    seat.poll_once();
    assert!(
        node.state().last_query.contains("addressed=1"),
        "{}",
        node.state().last_query
    );
    let a = seat.attach("a", Attention::default());
    let mut all = Attention::default();
    all.set_room("build", Level::All);
    let _b = seat.attach("b", all);
    seat.poll_once();
    let q = node.state().last_query.clone();
    assert!(
        !q.contains("addressed=1") && !q.contains("mentions=1"),
        "{q}"
    );
    assert!(q.contains("focus=Lab"), "{q}");
    assert!(q.contains("pid="), "{q}");
    drop(a);
}

#[test]
fn what_arrives_while_nobody_is_attached_is_handed_to_the_next_session_labelled() {
    let node = FakeNode::start();
    let d = tmp("backlog");
    let seat = Seat::open(creds(&node, None), Some(&d), Some(&d)).unwrap();
    node.push(chat("deadtrickster", "user", "A-SEAT", "are you there?"));
    assert_eq!(seat.poll_once(), PollOutcome::Delivered(1));
    // Acked on the node — the seat heard it — but no session had it.
    assert_eq!(node.state().acks.len(), 1);
    let cond = seat.attach("later", Attention::default());
    let why = cond.met().unwrap();
    assert!(
        why.contains("[seat] 1 delivery(ies) arrived while no session was attached"),
        "{why}"
    );
    assert!(why.contains("are you there?"), "{why}");
}

#[test]
fn unreachable_is_a_declared_stall_that_refuses_say_and_announces_reattachment() {
    let node = FakeNode::start();
    let d = tmp("stall");
    let seat = Seat::open(creds(&node, None), Some(&d), Some(&d)).unwrap();
    let cond = seat.attach("s", Attention::default());
    seat.poll_once();
    assert!(seat.say("general", "hello", None, None).is_ok());

    node.set_down(true);
    assert_eq!(seat.poll_once(), PollOutcome::Unreachable);
    assert!(
        matches!(seat.state(), SeatState::Stalled { .. }),
        "{:?}",
        seat.state()
    );
    let why = cond.met().unwrap();
    assert!(why.contains("[seat] STALLED"), "{why}");
    assert!(why.contains("`say` is refused"), "{why}");
    let err = seat
        .say("general", "into the void", None, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not sending into the void"), "{err}");
    // Told once, not on every failed poll.
    assert_eq!(seat.poll_once(), PollOutcome::Unreachable);
    assert!(cond.met().is_none());

    node.set_down(false);
    node.push(chat("deadtrickster", "user", "", "back?"));
    assert_eq!(seat.poll_once(), PollOutcome::Delivered(1));
    let why = cond.met().unwrap();
    assert!(why.contains("[seat] reattached"), "{why}");
    assert!(why.contains("back?"), "{why}");
    assert!(seat.state().is_listening());
}

#[test]
fn no_reader_stops_the_listener_and_hands_over_the_refusal_verbatim() {
    let node = FakeNode::start();
    let d = tmp("noreader");
    node.state().reader_exists = false;
    let seat = Seat::open(creds(&node, None), Some(&d), Some(&d)).unwrap();
    let cond = seat.attach("s", Attention::default());
    assert_eq!(seat.poll_once(), PollOutcome::Stop);
    assert!(matches!(seat.state(), SeatState::Stopped { .. }));
    let why = cond.met().unwrap();
    assert!(why.contains("no inbox reader called `seat`"), "{why}");
    assert!(why.contains("readers here: other-seat"), "{why}");
    assert!(why.contains("SWITCHED"), "{why}");
    // Declaring is explicit, and available.
    assert!(seat.reader().unwrap().is_none());
}

#[test]
fn a_re_minted_token_file_stops_the_listener() {
    let node = FakeNode::start();
    let d = tmp("remint");
    let tf = d.join("seat");
    std::fs::write(&tf, "good\n").unwrap();
    let seat = Seat::open(creds(&node, Some(tf.clone())), Some(&d), Some(&d)).unwrap();
    let cond = seat.attach("s", Attention::default());
    assert_eq!(seat.poll_once(), PollOutcome::Delivered(0));
    std::fs::write(&tf, "newer\n").unwrap();
    assert_eq!(seat.poll_once(), PollOutcome::Stop);
    let why = cond.met().unwrap();
    assert!(why.contains("re-minted"), "{why}");
    assert!(why.contains("OLD identity"), "{why}");
    let err = seat
        .say("general", "x", None, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("stopped"), "{err}");
}

#[test]
fn a_second_seat_under_a_held_name_is_refused_naming_the_holder() {
    let node = FakeNode::start();
    let d = tmp("claim");
    // Somebody alive that is not us holds the name: pid 1.
    std::fs::write(d.join("inbox-seat.pid"), "1\n").unwrap();
    let err = Seat::open(creds(&node, None), Some(&d), Some(&d))
        .unwrap_err()
        .to_string();
    assert!(err.starts_with("LISTENER REFUSED"), "{err}");
    assert!(err.contains("pid 1,"), "{err}");
}

#[test]
fn a_watched_artifact_fires_on_change_and_a_todo_envelope_is_re_read() {
    let node = FakeNode::start();
    let d = tmp("subs");
    let seat = Seat::open(creds(&node, None), Some(&d), Some(&d)).unwrap();
    seat.start();
    let cond = seat.attach("s", Attention::default());
    let summary = seat
        .subscribe(&cond, letibot_flowy::Subscription::Artifact("ART".into()))
        .unwrap();
    assert!(summary.contains("`the plan`"), "{summary}");
    // Nothing changed: no firing.
    seat.entities().look_at_artifacts();
    assert!(cond.met().is_none());
    node.state().artifact = json!({"id": "ART", "type": "diagram", "title": "the plan", "body": "v2 longer", "updated": "t2"});
    seat.entities().look_at_artifacts();
    let why = cond.met().unwrap();
    assert!(
        why.contains(
            "[artifact ART] artifact ART `the plan` changed; body 2 → 9 bytes; updated t2"
        ),
        "{why}"
    );

    // A todo envelope for a watched row is re-read; for an unwatched one, ignored.
    seat.subscribe(&cond, letibot_flowy::Subscription::Todo("ART".into()))
        .unwrap();
    seat.entities()
        .on_todo_envelope(r#"{"topic":"todos","hlc":5,"artifact":"OTHER","type":"todo.note"}"#);
    assert!(cond.met().is_none());
    seat.entities()
        .on_todo_envelope(r#"{"topic":"todos","hlc":5,"artifact":"ART","type":"todo.note"}"#);
    let why = cond.met().unwrap();
    assert!(why.contains("[todo ART] todo ART: todo.note"), "{why}");
    seat.stop();
}

#[test]
fn a_session_tag_routes_to_exactly_one_session_and_the_others_count_it() {
    let node = FakeNode::start();
    let d = tmp("tag");
    let seat = Seat::open(creds(&node, None), Some(&d), Some(&d)).unwrap();
    let mut all = Attention::default();
    all.default = Level::All;
    let a = seat.attach_as("s-1", "planner", all.clone());
    let b = seat.attach_as("s-2", "coder", all);

    // By alias, case-insensitive, with the node having resolved the seat as the
    // addressee and left `/coder` in the body.
    node.push(chat(
        "deadtrickster",
        "user",
        "A-SEAT",
        "@Seat/Coder run the tests",
    ));
    // By id.
    node.push(chat("claude-host", "agent", "A-SEAT", "@seat/s-1 plan it"));
    // A fragment nobody here answers to: an ordinary addressed message, to both.
    node.push(chat(
        "claude-host",
        "agent",
        "A-SEAT",
        "@seat/nobody anyone?",
    ));
    assert_eq!(seat.poll_once(), PollOutcome::Delivered(3));

    let why_a = a.met().unwrap();
    assert!(why_a.contains("plan it"), "{why_a}");
    assert!(why_a.contains("to this session (@seat/s-1)"), "{why_a}");
    assert!(!why_a.contains("run the tests"), "{why_a}");
    assert!(why_a.contains("anyone?"), "{why_a}");
    assert!(
        why_a.contains("1 went past that your attention table did not ask for"),
        "{why_a}"
    );

    let why_b = b.met().unwrap();
    assert!(why_b.contains("run the tests"), "{why_b}");
    assert!(why_b.contains("to this session (@seat/s-2)"), "{why_b}");
    assert!(!why_b.contains("plan it"), "{why_b}");
    assert!(why_b.contains("anyone?"), "{why_b}");
}

#[test]
fn two_sessions_on_one_seat_talk_through_the_daemon_with_the_room_as_the_record() {
    let node = FakeNode::start();
    let d = tmp("local");
    let seat = Seat::open(creds(&node, None), Some(&d), Some(&d)).unwrap();
    seat.poll_once(); // identity from whoami
    let a = seat.attach_as("s-1", "planner", Attention::default());
    let b = seat.attach_as("s-2", "coder", Attention::default());

    // Planner tells coder. The node gets `to: seat` and the tagged body — the
    // record — and coder gets it locally, because the node would never echo the
    // seat's own message back.
    let sent = seat
        .say("general", "tests first", Some("seat/coder"), None)
        .unwrap();
    assert_eq!(sent.body, "@seat/coder tests first");
    let said = node.state().said.clone();
    assert_eq!(said.len(), 1);
    assert_eq!(said[0]["to"], "seat");
    let why_b = b.met().unwrap();
    assert!(why_b.contains("tests first"), "{why_b}");
    assert!(why_b.contains("to this session (@seat/s-2)"), "{why_b}");
    assert!(a.met().is_none());

    // A session that is not here: posted for the record, and refused as a
    // delivery, naming who IS here.
    let err = seat
        .say("general", "hello?", Some("seat/tester"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no session called `tester`"), "{err}");
    assert!(err.contains("s-1 (planner), s-2 (coder)"), "{err}");
    assert_eq!(node.state().said.len(), 2);

    // Another seat's session: tagged for its daemon to route, `to` that seat.
    seat.say("general", "over to you", Some("other-seat/s-9"), None)
        .unwrap();
    let said = node.state().said.clone();
    assert_eq!(said[2]["to"], "other-seat");
    assert_eq!(said[2]["body"], "@other-seat/s-9 over to you");
}

/// A nag is LEVEL-triggered; the seat delivers its EDGES. The four things that
/// must all hold, and the reason each was easy to get wrong:
///   * an unchanged level is silent (else the same line every 20 s, forever);
///   * a NEW id in an unchanged count still speaks (one row done, one arrived —
///     a count-only check would never mention the second);
///   * a bucket emptying speaks once (else the seat's last word on a bucket is
///     the arrival, and "is it still open?" is unanswerable);
///   * the first reading is a standing total, not N arrivals.
#[test]
fn the_board_is_sampled_as_a_level_and_delivered_as_edges() {
    use letibot_flowy::client::Nag;
    use letibot_flowy::seat::NagState;

    let nag = |ids: &[&str]| Nag {
        mine_todo: ids.len() as i64,
        mine_todo_ids: ids.iter().map(|s| s.to_string()).collect(),
        ..Nag::default()
    };
    let mut st = NagState::default();

    // First reading: what is true, stated once, as a total.
    let first = st.diff(&nag(&["a", "b"])).expect("a standing total");
    assert!(first.contains("2 assigned to you"), "{first}");
    assert!(
        !first.contains("new"),
        "the baseline must not read as arrivals: {first}"
    );

    // Unchanged level: silent. This is the whole point.
    assert_eq!(st.diff(&nag(&["a", "b"])), None);
    assert_eq!(st.diff(&nag(&["a", "b"])), None);

    // One finished, one arrived — the COUNT is unchanged at 2 and the seat must
    // still say so, because the id set moved.
    let moved = st.diff(&nag(&["a", "c"])).expect("a new id is an edge");
    assert!(moved.contains("1 new assigned to you"), "{moved}");
    assert!(moved.contains('c'), "{moved}");
    assert!(!moved.contains("\"b\""), "{moved}");

    // Emptying speaks once, then goes quiet.
    let cleared = st.diff(&nag(&[])).expect("a clear is an edge");
    assert!(
        cleared.contains("nothing assigned to you any more"),
        "{cleared}"
    );
    assert_eq!(st.diff(&nag(&[])), None, "and only once");

    // A different bucket is tracked independently.
    let owed = Nag {
        answers_owed: 1,
        answers_owed_ids: vec!["q1".into()],
        ..Nag::default()
    };
    let o = st.diff(&owed).expect("answers owed is its own bucket");
    assert!(o.contains("waiting on an answer from you"), "{o}");
}
