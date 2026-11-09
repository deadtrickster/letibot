//! Signal accounting on a real coordinator socket: what followed each message lands on the
//! scorecard, and the ledger is a file beside the chat.

use std::sync::Arc;
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Act, Event, Request, Response, Room, Seat, Speaker, room_name};

const WINDOW_MS: u64 = 400;

fn seat(server: &Server, name: &str) -> Client {
    let wake = Arc::new(|_: &Event| {});
    let room = room_name(&server.name(), "aaaaaaaaaaaa");
    Client::connect_as(
        server.path(),
        Speaker::top_level(name),
        room,
        Seat::Session,
        wake,
    )
    .expect("a connection")
}

fn say(c: &Client, id: &str, thread: Option<u64>, text: &str) -> Response {
    c.request(&Request::Say {
        id: id.into(),
        // The project room: the host room carries mentions and announcements only.
        room: String::new(),
        text: text.into(),
        to: None,
        thread,
        refs: Vec::new(),
        attachments: Vec::new(),
        kind: Default::default(),
    })
    .expect("an answer")
}

fn seq(r: Response) -> u64 {
    match r {
        Response::Said { message, .. } => message.seq,
        other => panic!("said: {other:?}"),
    }
}

#[test]
fn the_scorecard_counts_what_followed() {
    let dir = std::env::temp_dir().join(format!("letibot-room-signal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_signal_window(WINDOW_MS);
    server.with_chat_store(&dir.join("room.chat"));
    let (a, b) = (seat(&server, "s-a"), seat(&server, "s-b"));

    let replied = seq(say(&a, "1", None, "build is red on main"));
    let ignored = seq(say(&a, "2", None, "fyi the cache moved"));
    seq(say(&b, "3", Some(replied), "fixing it"));
    assert!(matches!(
        say(&a, "4", None, "not for me"),
        Response::Refused { .. }
    ));
    let asked = match a
        .request(&Request::Ask {
            id: "5".into(),
            room: String::new(),
            to: "s-b".into(),
            path: String::new(),
            topic: String::new(),
            question: "which port?".into(),
        })
        .unwrap()
    {
        Response::Asked { ask, .. } => ask.seq,
        other => panic!("asked: {other:?}"),
    };
    b.request(&Request::Reply {
        id: "6".into(),
        ask: asked,
        text: "8080".into(),
    })
    .unwrap();
    b.request(&Request::WakeEnded {
        seqs: vec![ignored],
        cheap: true,
        silent: true,
        tokens: 321,
    })
    .unwrap();
    // A read of the thread by another member is an action too.
    let read = seq(say(&a, "7", None, "who owns the cache?"));
    b.request(&Request::Read {
        room: String::new(),
        since: 0,
        last: None,
        thread: Some(read),
        before: None,
    })
    .unwrap();
    // A claim reported for a message (R163's hook) counts like the rest.
    let claimed = seq(say(&a, "8", None, "cache is mine now"));
    b.request(&Request::Acted {
        seq: claimed,
        act: Act::Claim,
    })
    .unwrap();

    std::thread::sleep(Duration::from_millis(WINDOW_MS + 100));
    server.flush_signals();
    let card = server.scorecard();
    let row = card.senders.iter().find(|r| r.key == "s-a").unwrap();
    // replied, ask, read and claimed were acted on. ignored was not, and its window closed.
    assert_eq!((row.msgs, row.acted, row.pending), (5, 4, 0), "{row:?}");
    assert_eq!((row.replies, row.asks_settled), (2, 1), "{row:?}");
    assert_eq!((row.reads, row.claims), (1, 1), "{row:?}");
    assert_eq!(
        (row.refused, row.silent, row.silent_tokens),
        (1, 1, 321),
        "{row:?}"
    );
    let ask = card.kinds.iter().find(|r| r.key == "ask").unwrap();
    assert_eq!((ask.acted, ask.msgs), (1, 1));

    let over = a.request(&Request::Signals).unwrap();
    assert!(matches!(over, Response::Signals { .. }), "{over:?}");
    let file = std::fs::read_to_string(dir.join("room.chat.signals")).unwrap();
    assert!(file.contains("\"wake\""), "{file}");
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
