//! A bridge that comes back to a backlog larger than the coordinator's open push gets every row,
//! and its cursor never moves past a row it was not handed.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_bridge::rooms::{self, Attention, RoomLink};
use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Event, Request, Response, Room, Seat, Speaker, room_name};

// Above one 200-row inbox page plus the 64-row open push, so both the cap and the paging count.
const BACKLOG: usize = 300;

fn inbox_seqs(link: &RoomLink) -> BTreeSet<u64> {
    let reply = link.inbox(Attention::ToMe, 200).expect("inbox");
    reply["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["seq"].as_u64())
        .collect()
}

#[test]
fn gap_rows_older_than_the_open_cap_are_not_acked_away() {
    // The data directory is this binary's own: the link keeps its cursor under it, and a test that
    // leaves it unsaid writes into the operator's.
    letibot_testenv::isolate_data();
    // SAFETY: first thing in the only test of this file, before the handle is read.
    unsafe { std::env::set_var(rooms::NAME_ENV, "cc@gapper") };
    let dir = std::env::temp_dir().join(format!("letibot-room-gap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_budget(10_000);

    // Known to the coordinator with a cursor, then gone while the backlog builds. A gone external
    // takes no `to`, so the backlog names it in the text.
    let first = RoomLink::at(server.path());
    first.ensure().expect("the bridge connects");
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
    let mut sent = BTreeSet::new();
    for n in 0..BACKLOG {
        let said = speaker
            .request(&Request::Say {
                id: format!("g{n}"),
                room: "host".into(),
                text: format!("@cc@gapper row {n}"),
                to: None,
                thread: None,
                refs: Vec::new(),
                attachments: Vec::new(),
                kind: Default::default(),
            })
            .expect("said");
        match said {
            Response::Said { message, .. } => sent.insert(message.seq),
            other => panic!("{other:?}"),
        };
    }

    let got: Arc<Mutex<BTreeSet<u64>>> = Arc::default();
    let back = RoomLink::at(server.path());
    let sink = got.clone();
    back.stream(Arc::new(move |m, _| {
        if let Some(seq) = m["seq"].as_u64() {
            sink.lock().unwrap().insert(seq);
        }
        true
    }));
    let until = Instant::now() + Duration::from_secs(20);
    while !sent.is_subset(&got.lock().unwrap()) {
        if Instant::now() > until {
            let missing: Vec<_> = sent.difference(&got.lock().unwrap()).copied().collect();
            panic!(
                "{} of {BACKLOG} rows never reached the stream: {missing:?}",
                missing.len()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Two supervisor passes: whatever it acks, it acks by now.
    std::thread::sleep(Duration::from_secs(5));
    let left = inbox_seqs(&back);
    let oldest: BTreeSet<u64> = sent.iter().take(200).copied().collect();
    assert_eq!(left, oldest, "nothing the model has not seen is acked away");

    // Once the model has seen them, the cursor moves past them all.
    back.saw(sent.iter().copied());
    let until = Instant::now() + Duration::from_secs(10);
    while !inbox_seqs(&back).is_empty() {
        assert!(Instant::now() < until, "seen rows are acked");
        std::thread::sleep(Duration::from_millis(100));
    }
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
