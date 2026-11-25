//! A row the model has not been handed is not acked away: the bridge's cursor answers a real
//! coordinator, and `pending` is the ceiling the ack reads.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_bridge::rooms::{self, Attention, RoomLink};
use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Event, Request, Response, Room, Seat, Speaker, room_name};

fn say(speaker: &Client, n: usize) -> u64 {
    match speaker
        .request(&Request::Say {
            id: format!("p{n}"),
            room: "host".into(),
            text: format!("@cc@acker row {n}"),
            to: None,
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            kind: Default::default(),
        })
        .expect("said")
    {
        Response::Said { message, .. } => message.seq,
        other => panic!("{other:?}"),
    }
}

fn inbox(link: &RoomLink) -> Vec<u64> {
    link.inbox(Attention::ToMe, 200)
        .expect("inbox")
        .get("rows")
        .and_then(|r| r.as_array())
        .map(|rows| rows.iter().filter_map(|m| m["seq"].as_u64()).collect())
        .unwrap_or_default()
}

#[test]
fn a_row_the_model_has_not_taken_stays_in_the_inbox_until_it_is_seen() {
    // The data directory is this binary's own: the link keeps its cursor under it, and a test that
    // leaves it unsaid writes into the operator's.
    letibot_testenv::isolate_data();
    // SAFETY: the first statement of the only test in this file, before the handle is read.
    unsafe { std::env::set_var(rooms::NAME_ENV, "cc@acker") };
    let dir = std::env::temp_dir().join(format!("letibot-room-ack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");

    let first = RoomLink::at(server.path());
    first.ensure().expect("the seat is known with a cursor");
    drop(first);

    let speaker = Client::connect_as(
        server.path(),
        Speaker::top_level("s-a"),
        room_name(&server.name(), ""),
        Seat::Session,
        Arc::new(|_: &Event| {}),
    )
    .expect("a session");
    std::thread::sleep(Duration::from_millis(500));
    let sent = say(&speaker, 1);

    // The stream refuses the row: the model has not seen it, so it is pending and the ack may not
    // pass it. Two supervisor passes prove the ceiling outlasts more than one ack attempt.
    let taken: Arc<Mutex<Vec<u64>>> = Arc::default();
    let yes = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let link = RoomLink::at(server.path());
    let sink = taken.clone();
    let takes = yes.clone();
    link.stream(Arc::new(move |m, _| {
        if let Some(seq) = m["seq"].as_u64() {
            sink.lock().unwrap().push(seq);
        }
        takes.load(std::sync::atomic::Ordering::SeqCst)
    }));
    let until = Instant::now() + Duration::from_secs(20);
    while taken.lock().unwrap().is_empty() {
        assert!(Instant::now() < until, "the row never reached the stream");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        inbox(&link),
        vec![sent],
        "an unseen row was acked away: the coordinator dropped it from the seat's cursor"
    );

    // Seen, and taken: the cursor moves past it, which is the ack frame the real coordinator
    // reads. A replay re-raises what it hands over, so the model keeps saying it has seen it.
    yes.store(true, std::sync::atomic::Ordering::SeqCst);
    let until = Instant::now() + Duration::from_secs(15);
    while !inbox(&link).is_empty() {
        link.saw_upto(sent);
        assert!(Instant::now() < until, "a seen row is acked");
        std::thread::sleep(Duration::from_millis(100));
    }
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
