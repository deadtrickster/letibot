//! What an external seat was already handed is not handed again. A bridge's reader is the channel
//! it writes to, so a push is a delivery and an ack is a durable seen mark: a coordinator restart
//! (`letibot rooms restart`) re-wrote a dozen old dms and ask answers to one seat because neither
//! was true for it.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Attention, Event, Message, Request, Response, Room, Seat, Speaker, project_room,
};

mod support;

use support::{wait_socket_gone, wait_until};

/// The seat's project room: a dm is what the report is about, and a room line naming the seat is
/// the second door into the same delivery.
const A: &str = "aaaaaaaaaaaa";
/// The seat a bridged Claude Code session is: minted once per session, not per connection.
const SEAT: &str = "lab2x1/cc-7001";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-redelivery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Scratch(p)
    }

    /// The coordinator's chat log. The state file beside it is `<path>.state`.
    fn chat(&self) -> PathBuf {
        self.0.join("room.chat")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Seated {
    client: Client,
    heard: Receiver<(Message, bool)>,
}

/// A coordinator keeping chat in the scratch, so a restart reads the same log and the same marks.
fn coordinator(s: &Scratch) -> Server {
    let server = Server::serve(&s.0.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_seat_host("lab2x1");
    server.with_chat_store(&s.chat());
    server
}

fn seat(server: &Server, name: &str, id: &str, seat: Seat) -> Seated {
    let (tx, heard) = mpsc::channel();
    let wake = Arc::new(|_: &Event| {});
    let room = letibot_room::room_name(&server.name(), A);
    let client = Client::connect_seated(
        server.path(),
        Speaker::top_level(name),
        room,
        seat,
        id,
        "",
        wake,
    )
    .unwrap_or_else(|e| panic!("`{name}` must connect: {e}"));
    let tx = Mutex::new(tx);
    client.on_chat(Arc::new(move |m, names| {
        let _ = tx.lock().unwrap().send((m.clone(), names));
    }));
    Seated { client, heard }
}

impl Seated {
    fn call(&self, r: Request) -> Response {
        self.client.request(&r).expect("an answer")
    }

    fn dm(&self, id: &str, to: &str, text: &str) -> Response {
        self.call(Request::Dm {
            id: id.into(),
            to: to.into(),
            text: text.into(),
            thread: None,
            attachments: Vec::new(),
        })
    }

    /// The next thing pushed to this connection, and whether it names the seat.
    fn next(&self) -> (Message, bool) {
        self.heard
            .recv_timeout(Duration::from_secs(5))
            .expect("a pushed message")
    }

    fn silent(&self) {
        assert!(
            self.heard.recv_timeout(Duration::from_millis(500)).is_err(),
            "nothing was pushed"
        );
    }

    /// What the bridge's own stream does: everything written to the channel is acked, so the cursor
    /// stands at the newest row it wrote.
    fn ack_delivered(&self, upto: u64) {
        match self.call(Request::Ack { seq: upto }) {
            Response::Acked { cursor } => assert_eq!(cursor, upto, "the ack moved the cursor"),
            other => panic!("{other:?}"),
        }
    }
}

fn said(r: Response) -> Message {
    match r {
        Response::Said { message, .. } => message,
        other => panic!("expected the message to be said: {other:?}"),
    }
}

/// The seen mark the store has written for this seat, the newest one: what a coordinator that stops
/// at this moment would come back with.
fn store_says_the_seat_saw(s: &Scratch) -> u64 {
    let state =
        std::fs::read_to_string(format!("{}.state", s.chat().display())).unwrap_or_default();
    state
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|v| v.get("seen").cloned())
        .filter(|r| r["name"] == SEAT)
        .filter_map(|r| r["seq"].as_u64())
        .max()
        .unwrap_or(0)
}

/// Put the state file back to what a build that never marked an external's seen would have left: the
/// cursors stay, and the marks go. It is the shape the store on a box that has been running for
/// weeks has, which is why the fix may not depend on one being written from now on.
fn forget_the_seats_marks(s: &Scratch) {
    let path = format!("{}.state", s.chat().display());
    let state = std::fs::read_to_string(&path).unwrap_or_default();
    let kept: String = state
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("seen").cloned())
                .is_none_or(|r| r["name"] != SEAT)
        })
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&path, kept).expect("the state file this test owns");
}

/// The report, in one process: a seat is handed two dms and acks them, the coordinator restarts over
/// the same store, and the seat that comes back is handed neither of them again.
#[test]
fn a_restart_hands_an_external_seat_nothing_it_already_acked() {
    let dir = Scratch::new();
    let first = coordinator(&dir);
    let session = seat(&first, "s-a", "", Seat::Session);
    let bridge = seat(&first, "cc@app", SEAT, Seat::External);

    let one = said(session.dm("d1", "cc@app", "one"));
    let (heard, names) = bridge.next();
    assert_eq!(heard.seq, one.seq);
    assert!(names, "a dm names the one it is for");
    let two = said(session.dm("d2", "cc@app", "two"));
    assert_eq!(bridge.next().0.seq, two.seq);
    bridge.ack_delivered(two.seq);
    drop(bridge);
    first.stop();
    drop(session);
    wait_socket_gone(&dir.0.join("room.sock"));

    let second = coordinator(&dir);
    let session = seat(&second, "s-a", "", Seat::Session);
    let again = seat(&second, "cc@app", SEAT, Seat::External);
    again.silent();

    // And the next one reaches it once, which is the other half of the same rule.
    let three = said(session.dm("d3", "cc@app", "three"));
    assert_eq!(three.seq, two.seq + 1, "the log went on from where it was");
    let (heard, names) = again.next();
    assert_eq!(heard.seq, three.seq);
    assert!(names);
    again.silent();
    second.stop();
}

/// The rule the report asks for: a row written to the channel counts as seen, acked or not. The
/// seat is handed it, the process is stopped before the bridge's next pass acks, and the reconnect
/// writes it to the channel no second time - the inbox is where a row it still wants is read.
#[test]
fn a_row_written_to_the_channel_but_never_acked_is_not_written_to_it_again() {
    let dir = Scratch::new();
    let first = coordinator(&dir);
    let session = seat(&first, "s-a", "", Seat::Session);
    let bridge = seat(&first, "cc@app", SEAT, Seat::External);

    let one = said(session.dm("d1", "cc@app", "handed over, never acked"));
    assert_eq!(bridge.next().0.seq, one.seq);
    // The connection closes without an ack: the push is the whole of what the seat was handed.
    drop(bridge);
    wait_until("the seat's push to be written down", || {
        store_says_the_seat_saw(&dir) >= one.seq
    });
    first.stop();
    drop(session);
    wait_socket_gone(&dir.0.join("room.sock"));

    let second = coordinator(&dir);
    let _session = seat(&second, "s-a", "", Seat::Session);
    let again = seat(&second, "cc@app", SEAT, Seat::External);
    again.silent();
    // Nothing was lost by counting it seen: the cursor never moved, so the inbox still has it.
    match again.call(Request::Inbox {
        attention: Attention::All,
        limit: None,
        after: None,
    }) {
        Response::Inbox { rows, cursor, .. } => {
            assert_eq!(
                rows.iter().map(|m| m.seq).collect::<Vec<_>>(),
                vec![one.seq],
                "the row is still the seat's to read"
            );
            assert_eq!(cursor, 0, "a push moves no cursor");
        }
        other => panic!("{other:?}"),
    }
    second.stop();
}

/// A seat with no ack at all - one whose cursor was never moved - hears what happened after its
/// seen mark and no more, and a room line naming it is the same path as a dm.
#[test]
fn a_restart_hands_no_room_line_that_named_the_seat_again() {
    let dir = Scratch::new();
    let first = coordinator(&dir);
    let session = seat(&first, "s-a", "", Seat::Session);
    let bridge = seat(&first, "cc@app", SEAT, Seat::External);

    let line = said(session.call(Request::Say {
        id: "r1".into(),
        room: project_room(A),
        text: "@cc@app: which port?".into(),
        to: None,
        thread: None,
        refs: Vec::new(),
        attachments: Vec::new(),
        kind: Default::default(),
    }));
    let (heard, names) = bridge.next();
    assert_eq!(heard.seq, line.seq);
    assert!(names, "the line names the seat");
    bridge.ack_delivered(line.seq);
    drop(bridge);
    first.stop();
    drop(session);
    wait_socket_gone(&dir.0.join("room.sock"));

    let second = coordinator(&dir);
    let _session = seat(&second, "s-a", "", Seat::Session);
    let again = seat(&second, "cc@app", SEAT, Seat::External);
    again.silent();
    second.stop();
}

/// The mark is the SEAT's, not the handle it spoke as: a bridge's label changes with a twin and its
/// id does not, so a store that filed the mark under `cc@app` was one a reconnect never read.
#[test]
fn an_ack_writes_the_mark_under_the_seats_id() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let session = seat(&server, "s-a", "", Seat::Session);
    let bridge = seat(&server, "cc@app", SEAT, Seat::External);
    let one = said(session.dm("d1", "cc@app", "one"));
    assert_eq!(bridge.next().0.seq, one.seq);
    bridge.ack_delivered(one.seq);
    let deadline = Instant::now() + Duration::from_secs(5);
    while store_says_the_seat_saw(&dir) < one.seq && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        store_says_the_seat_saw(&dir),
        one.seq,
        "an ack writes the seat's mark, under the seat it is"
    );
    // And the label has no mark of its own to be read instead.
    let state =
        std::fs::read_to_string(format!("{}.state", dir.chat().display())).unwrap_or_default();
    assert!(
        !state.contains("\"name\":\"cc@app\""),
        "one seat has one key: {state}"
    );
    server.stop();
}

/// A store with no mark for the seat - what every box running before this fix has - is read from the
/// seat's CURSOR, not from a window of wall-clock: the read position is the cursor, and it was there
/// all along.
#[test]
fn a_store_with_no_mark_for_the_seat_resumes_at_its_cursor() {
    let dir = Scratch::new();
    let first = coordinator(&dir);
    let session = seat(&first, "s-a", "", Seat::Session);
    let bridge = seat(&first, "cc@app", SEAT, Seat::External);
    let one = said(session.dm("d1", "cc@app", "one"));
    assert_eq!(bridge.next().0.seq, one.seq);
    let two = said(session.dm("d2", "cc@app", "two"));
    assert_eq!(bridge.next().0.seq, two.seq);
    bridge.ack_delivered(two.seq);
    drop(bridge);
    // The close writes the seat's mark, so the store is only the test's once that thread is done.
    wait_until("the seat's connection to close", || {
        first.connections() == 1
    });
    first.stop();
    drop(session);
    wait_socket_gone(&dir.0.join("room.sock"));
    forget_the_seats_marks(&dir);

    let second = coordinator(&dir);
    let session = seat(&second, "s-a", "", Seat::Session);
    let again = seat(&second, "cc@app", SEAT, Seat::External);
    again.silent();
    // The cursor is the floor, so what comes next is handed over and nothing else is.
    let three = said(session.dm("d3", "cc@app", "three"));
    assert_eq!(again.next().0.seq, three.seq);
    again.silent();
    second.stop();
}

/// What a bridge resumes from: a seat whose reader is the channel the row was written to. The push
/// moves the seat's mark, an inbox answer names it, and the cursor stays where the MODEL is - so the
/// row is still owed to a model that never read it.
#[test]
fn an_inbox_answer_names_the_mark_the_channel_was_written_to() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let session = seat(&server, "s-a", "", Seat::Session);
    let bridge = seat(&server, "cc@app", SEAT, Seat::External);
    let one = said(session.dm("d1", "cc@app", "written to the channel"));
    assert_eq!(bridge.next().0.seq, one.seq);

    match bridge.call(Request::Inbox {
        attention: Attention::All,
        limit: None,
        after: None,
    }) {
        Response::Inbox {
            rows, cursor, seen, ..
        } => {
            assert_eq!(seen, Some(one.seq), "the push is the delivery");
            assert_eq!(cursor, 0, "a push moves no cursor");
            assert_eq!(
                rows.iter().map(|m| m.seq).collect::<Vec<_>>(),
                vec![one.seq],
                "the model still has it to read"
            );
        }
        other => panic!("{other:?}"),
    }
    server.stop();
}

/// The mark a bridge writes for rows it took out of the INBOX (they were said while it was down, so
/// no push ever moved it): a watermark, like an ack, and not a cursor. It outlives the coordinator,
/// which is what a bridge that restarts resumes from.
#[test]
fn a_seen_frame_writes_the_mark_and_a_restart_resumes_there() {
    let dir = Scratch::new();
    let first = coordinator(&dir);
    let session = seat(&first, "s-a", "", Seat::Session);
    let bridge = seat(&first, "cc@app", SEAT, Seat::External);
    let one = said(session.dm("d1", "cc@app", "handed over from the inbox"));
    assert_eq!(bridge.next().0.seq, one.seq);
    // A row that is not this seat's at all (a dm between two others), above the one it was handed:
    // what a bridge says when it has drained the log up to there.
    let _sibling = seat(&first, "s-b", "", Seat::Session);
    let two = said(session.dm("d2", "s-b", "somebody else's"));
    match bridge.call(Request::Seen { seq: two.seq }) {
        Response::Acked { cursor } => assert_eq!(cursor, 0, "a mark is not a cursor"),
        other => panic!("{other:?}"),
    }
    wait_until("the mark to be written", || {
        store_says_the_seat_saw(&dir) >= two.seq
    });
    drop(bridge);
    first.stop();
    drop(session);
    wait_socket_gone(&dir.0.join("room.sock"));

    // The coordinator over its store, and a bridge that starts now: the mark says the channel has
    // what it has, and the row is still the seat's to read.
    let second = coordinator(&dir);
    let _session = seat(&second, "s-a", "", Seat::Session);
    let again = seat(&second, "cc@app", SEAT, Seat::External);
    again.silent();
    match again.call(Request::Inbox {
        attention: Attention::All,
        limit: None,
        after: None,
    }) {
        Response::Inbox {
            rows, cursor, seen, ..
        } => {
            assert_eq!(seen, Some(two.seq), "the mark came back with the store");
            assert_eq!(cursor, 0);
            assert_eq!(
                rows.iter().map(|m| m.seq).collect::<Vec<_>>(),
                vec![one.seq]
            );
        }
        other => panic!("{other:?}"),
    }
    second.stop();
}
