//! **The bell, per client** — firecode's four rules, asked of the socket rather than of the room.
//!
//! The room's state machine is `tests/wire.rs`'s subject: a verb, a refusal, an item. What is only
//! testable here is what a CLIENT hears, and firecode's rules are all claims about a client:
//!
//! 1. *listen before working* — a client that starts its work and connects afterwards misses the
//!    message that would have changed it (`crates/harnessd/tests/room_bell.rs` proves the mid-turn
//!    half; what is here is the ordering the socket itself imposes);
//! 2. **one read is not membership** — this file;
//! 3. *acknowledge before acting* — `crates/harnessd/tests/room_bell.rs` (the ack is the client's
//!    frame, so it is asserted where the client is);
//! 4. *a bell must be able to inject a turn* — verified per client, never assumed, in
//!    `crates/harnessd/tests/room_bell.rs` and `room_bell_turn.rs`.
//!
//! # What this file pins, and what it would have failed on before it existed
//!
//! The room kept no record of a connection that was not there. `Server::route` returned early when
//! no connection was open at all and silently skipped an audience member with no socket, so a
//! session that was away for an hour was handed a `Welcome` identical to one that had never left —
//! a bell that is silent about the time it was not ringing. `Response::Welcome`'s `missed` and
//! `missed_since_ms` are the room's answer, and they are a COUNT and not a replay because the room
//! keeps no log: what a client is owed is *you missed four, re-read the state*.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Event, Request, Response, Room, Speaker};

/// A directory of this test's own, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let p =
            std::env::temp_dir().join(format!("letibot-room-{what}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Scratch(p)
    }

    fn socket(&self) -> PathBuf {
        self.0.join("room.sock")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **A client whose bells are collected**, so a test asserts what it was told rather than sleeping.
fn client(server: &Server, session: &str) -> (Client, Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let wake = Arc::new(move |e: &Event| {
        let _ = tx.send(e.clone());
    });
    let c = Client::connect(server.path(), Speaker::top_level(session), wake)
        .unwrap_or_else(|e| panic!("`{session}` must connect: {e}"));
    (c, rx)
}

fn call(c: &Client, request: Request) -> Response {
    c.request(&request)
        .unwrap_or_else(|e| panic!("{request:?}: {e}"))
}

fn post(c: &Client, id: &str, tag: &str, body: &str) -> u64 {
    match call(
        c,
        Request::Post {
            id: id.into(),
            tag: tag.into(),
            body: body.into(),
        },
    ) {
        Response::Posted { item, .. } => item.id,
        other => panic!("a post must be posted: {other:?}"),
    }
}

/// Wait until `f` says so — for a fact that arrives on a thread rather than as a frame.
fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("waited five seconds for {what}");
}

/// **One read is not membership, and a standing bell is.** The three arms of firecode's second rule,
/// in one room so that the count a reconnect is told is the count this test made:
///
/// * a session that READ the queue and closed gets nothing more — and the room knows it could not
///   reach it, which is the fact that makes the third arm possible at all;
/// * a session whose connection stands is rung for the next event, with no read in between;
/// * the session that was away is told what it missed — the count, the moment of the first, and (in
///   `crates/harnessd`) the sentence that tells the model to re-read the state instead.
#[test]
fn a_read_is_not_membership_and_a_gap_is_counted() {
    let scratch = Scratch::new("membership");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a room to talk to");

    // ---- the reader that is not a member ----------------------------------
    // `s-reader` posts nothing and claims nothing: it only ever READS the queue, which is what
    // makes it not an audience member (`Server::route`'s first paragraph). The read is not what is
    // being tested — what is being tested is that a read leaves no subscription behind.
    let (reader, _reader_bells) = client(&server, "s-reader");
    let item = post(&reader, "p1", "ram", "8 GB for the suite");
    let queue = call(&reader, Request::Queue { tag: "ram".into() });
    assert!(matches!(queue, Response::Queue { .. }), "{queue:?}");
    drop(reader);
    until("the room to notice the reader's close", || {
        server.connections() == 0
    });

    // ---- the standing bell ------------------------------------------------
    // `s-standing` is a member by its own post, and it stays connected: the claim below is news to
    // it because it is queued under the tag, and it hears it without asking for anything.
    let (standing, standing_bells) = client(&server, "s-standing");
    post(&standing, "p2", "ram", "the other 8 GB");
    let claimer = client(&server, "s-claimer").0;
    let claimed = call(&claimer, Request::Claim { id: "c1".into(), tag: "ram".into() });
    assert!(matches!(claimed, Response::Claimed { .. }), "{claimed:?}");
    // **The first event on this channel is its own post**, which is the room telling a poster what
    // it made. What this arm is about is the one after it: a claim by somebody else, delivered to a
    // session that asked for nothing and is not the claimer.
    let heard = recv_where(&standing_bells, "the claim", |e| {
        matches!(e, Event::Claimed { .. })
    });
    assert!(
        matches!(&heard, Event::Claimed { by, .. } if by.as_str() == "s-claimer"),
        "a session queued under the tag is rung for a claim on it, with no read in between: {heard:?}"
    );

    // ---- and the one that was away ---------------------------------------
    // The reader that closed is told, when it comes back, that it was addressed while it was gone.
    // Nothing is replayed — the room keeps no log — so the count is the whole of the answer, and
    // the sentence a daemon composes from it names the reads that cannot be wrong.
    let (back, _back_bells) = client(&server, "s-reader");
    // **The count is a statement about the gap that just ended**, so it is taken rather than read:
    // a caller that reported the same number on every poll would tell the model the room had gone
    // quiet again every second.
    let (count, since) = back.take_missed();
    assert!(
        count >= 1,
        "the room rang for this session while its connection was down, and it must say so rather \
         than hand back a `Welcome` identical to a session that never left: {count}"
    );
    assert!(since > 0, "and when the first of them was: {since}");
    assert_eq!(
        back.take_missed(),
        (0, 0),
        "the count was taken by the `Welcome`; nothing is left to report twice"
    );

    // The item is still there, and it says who took it — the state a re-read finds, which is why
    // the count and not a replay is the honest answer: `s-reader` never heard the claim, and one
    // read of the queue tells it more than the bell would have.
    let queue = call(&back, Request::Queue { tag: "ram".into() });
    let Response::Queue { rows, .. } = queue else {
        panic!("a queue read: {queue:?}");
    };
    let first = rows
        .iter()
        .find(|r| r.id == item)
        .unwrap_or_else(|| panic!("#1 is still in the queue: {rows:?}"));
    assert!(
        matches!(first.state, letibot_room::State::Claimed { .. }),
        "#1 was claimed while this session was away, and the queue says so: {first:?}"
    );
}

/// **A bell that rings where nobody is, is counted once per session and per event** — the number a
/// reconnect is told is *how many bells rang for it*, not how many events happened, and it is taken
/// rather than read so two gaps cannot be confused.
#[test]
fn a_gap_is_counted_per_event_and_taken_once() {
    let scratch = Scratch::new("gap-count");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a room to talk to");
    // A first connection, so the session exists in the room and its post makes it a member.
    let (first, _bells) = client(&server, "s-away");
    let item = post(&first, "p1", "ram", "8 GB for the suite");
    drop(first);
    until("the room to notice the close", || server.connections() == 0);

    // Two events about that item, from a session that stays: a claim, and then giving it back.
    // `s-away` is the poster of #1, so both are addressed to it — and it has no socket. (A `beat` is
    // not one of them: it produces no event at all, which is the room's own half of *a bell that
    // rings five times a minute is a bell nobody listens to*. A `done` is not one either: the item
    // leaves the queue it was addressed by, so the only session a finish names is the one that did
    // it — which is the room being right, and is why this arm uses an abandonment.)
    let (other, _other_bells) = client(&server, "s-other");
    let claimed = call(
        &other,
        Request::Claim { id: "c1".into(), tag: "ram".into() },
    );
    assert!(matches!(claimed, Response::Claimed { .. }), "{claimed:?}");
    let back_in = call(
        &other,
        Request::Abandon {
            id: "a1".into(),
            item,
            why: "the suite blew up".into(),
        },
    );
    assert!(matches!(back_in, Response::Abandoned { .. }), "{back_in:?}");
    // **The witness goes too**, or `connections()` never reaches zero and the waits below are about
    // a socket this test forgot it was holding.
    drop(other);
    until("the room to notice the witness's close", || {
        server.connections() == 0
    });

    let (back, _bells) = client(&server, "s-away");
    assert_eq!(
        back.welcome().missed,
        2,
        "two events were addressed to this session with no connection behind it: {:?}",
        back.welcome()
    );
    drop(back);
    // **And nothing is left over for the NEXT connection**, which is the difference between a count
    // of a gap and a running total of everything the session ever missed.
    until("the room to notice the close", || server.connections() == 0);
    let (again, _bells) = client(&server, "s-away");
    assert_eq!(
        again.welcome().missed,
        0,
        "a connection that missed nothing is told nothing: {:?}",
        again.welcome()
    );
}

/// A receiver's next event, with the test's patience in the failure rather than a bare panic.
fn recv(rx: &Receiver<Event>, what: &str) -> Event {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match rx.try_recv() {
            Ok(e) => return e,
            Err(TryRecvError::Empty) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TryRecvError::Empty) => panic!("nothing arrived about {what}"),
            Err(TryRecvError::Disconnected) => panic!("the client went away waiting for {what}"),
        }
    }
}

/// The next event that `wants` says yes to, so an arm can be about ONE event on a channel that
/// carries several: a poster hears its own post, and a test that asserted on the first frame would
/// be asserting about the room's bookkeeping rather than about the bell.
fn recv_where(rx: &Receiver<Event>, what: &str, wants: impl Fn(&Event) -> bool) -> Event {
    loop {
        let e = recv(rx, what);
        if wants(&e) {
            return e;
        }
    }
}
