//! The socket end to end: a real `Server` and `Client`s over a unix socket.
//! Frames are tested without a socket in `tests/wire.rs`; here: one connection per session, a push
//! with an address, and the close as liveness.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Decision, Event, HOST_ROOM, Message, OPERATOR, ROOM_PROTOCOL_VERSION, Request, Response, Room,
    Seat, Speaker, State, operator_token_path,
};

/// A directory of this test's own, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        // A counter beside the clock: two tests can start inside one clock tick, and a shared
        // directory would make one test's cleanup another test's mystery.
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

/// A client whose bells are collected, so a test can assert what it was told rather than sleeping and
/// hoping.
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

fn wait_for(rx: &Receiver<Event>, what: &str, within: Duration) -> Event {
    let deadline = Instant::now() + within;
    // Not a `loop`: every arm of the match below returns or panics, so a wrapping loop would be dead,
    // and `clippy::never_loop` is denied - the gate lints this test target, `cargo test -p letibot-room`
    // does not.
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        panic!("nothing arrived about {what}");
    }
    match rx.recv_timeout(left) {
        Ok(e) => e,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("nothing arrived about {what}"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the client went away"),
    }
}

/// Wait until `f` says so, polling - for a fact that arrives on a thread (a connection served, a close
/// noticed) rather than as a frame.
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

#[test]
fn a_client_that_named_another_room_is_refused_with_both_names() {
    let scratch = Scratch::new("name");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a room to talk to");
    server.with_name("lab2x1");
    let wake = Arc::new(|_: &Event| {});
    // The sentence reaches the CALLER, not only the frame: a client that dialled the wrong coordinator
    // must be able to say which two machines it confused.
    let refused = Client::connect_named(
        server.path(),
        Speaker::top_level("s-a"),
        "the-other-box",
        wake.clone(),
    )
    .expect_err("a wrong room is refused");
    let said = refused.to_string();
    assert!(said.contains("lab2x1"), "the room's own name: {said}");
    assert!(
        said.contains("the-other-box"),
        "and the one asked for: {said}"
    );

    // And the room's own name is let in - the same client, naming the right room, connects. (`connect`
    // sends `host_name()`, which is what a server's default name IS; every other test here leans on that.)
    let good = Client::connect_named(server.path(), Speaker::top_level("s-a"), "lab2x1", wake)
        .expect("the right room is reached");
    assert!(
        good.welcome().identity.contains("no store"),
        "the welcome carries the room's own sentence: {}",
        good.welcome().identity
    );
    drop(good);
    server.stop();
}

#[test]
fn a_frame_is_answered_over_the_socket_and_a_subagent_is_refused_at_the_door() {
    let scratch = Scratch::new("hello");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");

    let (a, _events) = client(&server, "s-a");
    assert_eq!(a.welcome().lease_ms, letibot_room::DEFAULT_LEASE_MS);
    let item = post(&a, "p1", "ram", "8 GB");
    assert_eq!(item, 1);

    // The door: a subagent is refused where it says who it is, not at the first verb - and the refusal
    // is the room's own sentence, not the transport's.
    let (tx, _rx) = mpsc::channel();
    let refused = Client::connect(
        server.path(),
        Speaker::subagent("s-child", "s-a"),
        Arc::new(move |e: &Event| {
            let _ = tx.send(e.clone());
        }),
    )
    .expect_err("a subagent cannot open a room connection");
    assert!(
        refused.0.contains("a subagent speaks through its parent"),
        "{refused}"
    );

    server.stop();
}

#[test]
fn a_claim_is_a_lock_over_the_socket_and_the_refusal_names_the_holder() {
    let scratch = Scratch::new("busy");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");
    let (a, _ea) = client(&server, "s-a");
    let (b, _eb) = client(&server, "s-b");
    post(&a, "p1", "ram", "8 GB");

    assert!(matches!(
        call(
            &a,
            Request::Claim {
                id: "c1".into(),
                tag: "ram".into()
            }
        ),
        Response::Claimed { .. }
    ));
    match call(
        &b,
        Request::Claim {
            id: "c2".into(),
            tag: "ram".into(),
        },
    ) {
        Response::Refused { why, .. } => {
            let said = why.to_string();
            assert!(said.contains("s-a"), "the refusal names the holder: {said}");
            assert!(said.contains("a tag is a lock"), "{said}");
        }
        other => panic!("a tag is a lock: {other:?}"),
    }
    server.stop();
}

#[test]
fn an_acknowledgement_is_not_a_wait_and_the_socket_keeps_them_apart() {
    let scratch = Scratch::new("ack");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");
    let (a, _ea) = client(&server, "s-a");
    let item = post(&a, "p1", "ram", "8 GB");
    assert!(matches!(
        call(
            &a,
            Request::Claim {
                id: "c1".into(),
                tag: "ram".into()
            }
        ),
        Response::Claimed { .. }
    ));

    // What the holder is DOING: the acknowledgement the daemon's claim writes, on its own frame.
    assert!(matches!(
        call(
            &a,
            Request::Acknowledge {
                id: "k1".into(),
                item,
                text: "seen #1; working on it".into(),
            }
        ),
        Response::Acknowledged { .. }
    ));
    let held = match call(&a, Request::Holding { session: None }) {
        Response::Holding { rows, .. } => rows,
        other => panic!("a holding read: {other:?}"),
    };
    assert_eq!(
        held[0].said.as_ref().map(|w| w.text.as_str()),
        Some("seen #1; working on it"),
        "the words are on the claim"
    );
    assert!(
        held[0].waits_on.is_none(),
        "and they are NOT a wait: `waits_on` is what makes a reminder quiet, and a holder working \
         on its own item must still be reminded to finish it: {:?}",
        held[0]
    );

    // And what it is WAITING on goes in the other field, leaving the acknowledgement alone.
    assert!(matches!(
        call(
            &a,
            Request::Waiting {
                id: "w1".into(),
                item,
                text: "the suite".into(),
            }
        ),
        Response::Waited { .. }
    ));
    let held = match call(&a, Request::Holding { session: None }) {
        Response::Holding { rows, .. } => rows,
        other => panic!("a holding read: {other:?}"),
    };
    assert_eq!(
        held[0].waits_on.as_ref().map(|w| w.text.as_str()),
        Some("the suite")
    );
    assert_eq!(
        held[0].said.as_ref().map(|w| w.text.as_str()),
        Some("seen #1; working on it"),
        "two facts, two fields: a wait does not erase the acknowledgement"
    );
    server.stop();
}

#[test]
fn a_bell_goes_to_the_connections_the_event_is_about_and_not_to_a_reader() {
    let scratch = Scratch::new("routing");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");
    let (a, ea) = client(&server, "s-a");
    let (b, eb) = client(&server, "s-b");
    let (watcher, ew) = client(&server, "s-watcher");

    let item = post(&a, "p1", "ram", "8 GB");
    // `s-a` posted, so `s-a` is a member of `ram` and hears its own item arrive. `s-b` has nothing
    // under the tag and hears nothing.
    match wait_for(&ea, "the post", Duration::from_secs(2)) {
        Event::Posted { item: i, by, .. } => {
            assert_eq!(i, item);
            assert_eq!(by, "s-a");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        eb.recv_timeout(Duration::from_millis(200)).is_err(),
        "a session with nothing under the tag is not rung by a post"
    );

    // `s-b` posts under the same tag: now it is a member, and the claim is news to it.
    post(&b, "p2", "ram", "2 GB");
    let _ = wait_for(&eb, "its own post", Duration::from_secs(2));
    let _ = wait_for(&ea, "s-b's post", Duration::from_secs(2));

    call(
        &a,
        Request::Claim {
            id: "c1".into(),
            tag: "ram".into(),
        },
    );
    let claimed_by_b = wait_for(&eb, "the claim", Duration::from_secs(2));
    assert!(
        matches!(&claimed_by_b, Event::Claimed { by, .. } if by == "s-a"),
        "{claimed_by_b:?}"
    );

    // One read is not membership: a session that only asked the queue heard none of this.
    let _ = call(&watcher, Request::Queue { tag: "ram".into() });
    assert!(
        ew.recv_timeout(Duration::from_millis(200)).is_err(),
        "reading a queue does not enrol a session for its bells"
    );
    server.stop();
}

#[test]
fn a_closed_connection_gives_its_claim_back_at_once_and_says_so() {
    let scratch = Scratch::new("close");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");
    let (a, _ea) = client(&server, "s-a");
    let (b, eb) = client(&server, "s-b");
    post(&a, "p1", "ram", "8 GB");
    post(&b, "p2", "ram", "2 GB");
    call(
        &a,
        Request::Claim {
            id: "c1".into(),
            tag: "ram".into(),
        },
    );

    // Drop `s-a`'s connection the way a dying daemon does: the socket closes.
    drop(a);
    // `s-b` hears the abandonment with the close's own reason, not a stale beat ninety seconds later and
    // not silence.
    let mut heard = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(e) = eb.recv_timeout(Duration::from_millis(200)) {
            heard.push(e.clone());
            if matches!(
                &e,
                Event::Abandoned {
                    why: letibot_room::Why::SessionGone { .. },
                    ..
                }
            ) {
                break;
            }
        }
    }
    let gone = heard
        .iter()
        .find(|e| matches!(e, Event::Abandoned { .. }))
        .unwrap_or_else(|| panic!("no abandonment after the close: {heard:?}"));
    match gone {
        Event::Abandoned { by, count, .. } => {
            assert_eq!(by, "s-a");
            assert_eq!(*count, 1);
        }
        other => panic!("{other:?}"),
    }

    // And the item is claimable again immediately: the queue moved on.
    match call(
        &b,
        Request::Claim {
            id: "c2".into(),
            tag: "ram".into(),
        },
    ) {
        Response::Claimed { item, .. } => assert_eq!(item.id, 1, "the abandoned item is the head"),
        other => panic!("the queue moves on: {other:?}"),
    }
    until("the server to forget the closed connection", || {
        server.connections() == 1
    });
    server.stop();
}

#[test]
fn the_clock_takes_back_a_claim_whose_beat_stopped() {
    let scratch = Scratch::new("sweep");
    // A room whose lease is 200 ms, so the test does not wait out the real ninety seconds.
    let server =
        Server::with_options(&scratch.socket(), Room::with_lease_ms(200), 50).expect("a socket");
    let (a, _ea) = client(&server, "s-a");
    let (b, eb) = client(&server, "s-b");
    post(&a, "p1", "ram", "8 GB");
    post(&b, "p2", "ram", "2 GB");
    call(
        &a,
        Request::Claim {
            id: "c1".into(),
            tag: "ram".into(),
        },
    );

    // No beats. The server's own clock is the one that notices.
    let mut stale = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match eb.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Abandoned {
                why: letibot_room::Why::BeatStale { age_ms },
                ..
            }) => {
                stale = Some(age_ms);
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let age = stale.expect("the sweep is what abandons a silent claim, and it says how stale");
    assert!(age > 200, "the age is the evidence: {age}");
    server.stop();
}

#[test]
fn the_daemon_beats_for_a_session_while_its_turn_runs() {
    let scratch = Scratch::new("beat");
    // A lease of 300 ms: a session that never beats loses its item inside the test's patience, and
    // one whose daemon beats keeps it. That difference IS the feature.
    let server =
        Server::with_options(&scratch.socket(), Room::with_lease_ms(300), 100).expect("a socket");
    let (a, _ea) = client(&server, "s-a");
    post(&a, "p1", "ram", "8 GB");
    call(
        &a,
        Request::Claim {
            id: "c1".into(),
            tag: "ram".into(),
        },
    );

    // The turn runs - and the daemon, not the model, says so.
    let beat = a.start_beating();
    std::thread::sleep(Duration::from_millis(900));
    let held = call(&a, Request::Holding { session: None });
    match held {
        Response::Holding { rows, .. } => {
            assert_eq!(rows.len(), 1, "the daemon's beats kept the claim alive");
            assert_eq!(rows[0].item, 1);
        }
        other => panic!("{other:?}"),
    }

    // The turn ends. Nothing beats, and the lease takes it back - the fact the lease measures.
    beat.stop();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut lost = false;
    while Instant::now() < deadline {
        if let Response::Holding { rows, .. } = call(&a, Request::Holding { session: None })
            && rows.is_empty()
        {
            lost = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        lost,
        "with the turn over and no beats, the room takes it back"
    );
    server.stop();
}

#[test]
fn the_arbiter_hears_every_event_even_with_nothing_under_the_tag() {
    let scratch = Scratch::new("arbiter");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");
    server.with_arbiter("s-host");
    let (host, eh) = client(&server, "s-host");
    let (a, _ea) = client(&server, "s-a");
    post(&a, "p1", "ram", "8 GB");
    match wait_for(
        &eh,
        "the arbiter's copy of the post",
        Duration::from_secs(2),
    ) {
        Event::Posted { by, .. } => assert_eq!(by, "s-a"),
        other => panic!("{other:?}"),
    }

    // And a question reaches it: three abandonments raise the item, and the raise is addressed to
    // the arbiter, which is what makes it able to rule without polling.
    for k in 1..=3u64 {
        let c = call(
            &a,
            Request::Claim {
                id: format!("c{k}"),
                tag: "ram".into(),
            },
        );
        assert!(matches!(c, Response::Claimed { .. }), "{c:?}");
        let ab = call(
            &a,
            Request::Abandon {
                id: format!("a{k}"),
                item: 1,
                why: "the suite blew up".into(),
            },
        );
        assert!(matches!(ab, Response::Abandoned { .. }), "{ab:?}");
    }
    let mut raised = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match eh.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Raised { question, .. }) => {
                raised = Some(question);
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let question = raised.expect("the arbiter is told the item was raised");

    // The ruling comes back over the arbiter's own connection, and the item unfreezes.
    match call(
        &host,
        Request::Rule {
            id: "r1".into(),
            question,
            decision: Decision::Drop {
                why: "three claimants died on it".into(),
            },
            project: None,
        },
    ) {
        Response::Ruled { question, .. } => assert!(question.answer.is_some()),
        other => panic!("{other:?}"),
    }
    let items = server.items();
    assert!(
        matches!(items[0].state, State::Dropped { .. }),
        "{:?}",
        items[0]
    );
    server.stop();
}

#[test]
fn a_reconnect_is_a_new_life_and_the_repeat_post_is_still_one_item() {
    let scratch = Scratch::new("reconnect");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");
    let (a, _ea) = client(&server, "s-a");
    let first = post(&a, "p1", "ram", "8 GB");
    // A second client for the same session: a daemon that restarted its link. The frame carries the same
    // id, so the room answers from its replay window - one item, not two.
    let (again, _eb) = client(&server, "s-a");
    let second = post(&again, "p1", "ram", "8 GB");
    assert_eq!(first, second);
    assert_eq!(server.items().len(), 1, "a repeat is not a second item");
    server.stop();
}

#[test]
fn the_lease_refuses_a_second_coordinator_and_the_socket_is_not_bound() {
    use letibot_room::lease::{Acquired, CoordinatorLease};

    let scratch = Scratch::new("lease");
    let path = scratch.0.join("room.lease");
    let first = CoordinatorLease::owned_by(&path, "box/first/1");
    assert_eq!(first.acquire(1_000), Ok(Acquired::Fresh));

    // The second one is refused by the LEASE, before any socket exists - which is the order the daemon
    // must do it in, or a second room would be bound and served for a moment.
    let second = CoordinatorLease::owned_by(&path, "box/second/2");
    let held = second
        .acquire(1_001)
        .expect_err("two coordinators, one name");
    assert!(held.sentence().contains("box/first/1"));
    assert!(!scratch.socket().exists());

    // A stale lease is taken over, and the takeover says which way it was dead.
    let took = second
        .acquire(1_000 + letibot_room::COORDINATOR_LEASE_TTL_MS * 2)
        .expect("a stale lease is not a refusal");
    assert!(matches!(took, Acquired::TookOver { .. }), "{took:?}");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("now a socket");
    assert!(scratch.socket().exists());
    server.stop();
}

#[test]
fn a_mutex_guarded_room_is_one_writer_under_two_sessions_at_once() {
    // Not a property test: the point is that the server's `&mut` is behind ONE lock, so two
    // sessions claiming the same tag at the same instant cannot both win.
    let scratch = Scratch::new("race");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");
    let (a, _ea) = client(&server, "s-a");
    let (b, _eb) = client(&server, "s-b");
    post(&a, "p1", "ram", "8 GB");
    post(&b, "p2", "ram", "2 GB");

    let barrier = Arc::new(Mutex::new(()));
    let mut handles = Vec::new();
    for (c, id) in [(a.clone(), "c-a"), (b.clone(), "c-b")] {
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            // Both threads reach the claim as close together as the scheduler allows; the point is
            // that the SERVER's one lock decides, not that the race is perfectly even.
            let _together = barrier.lock().expect("the barrier");
            drop(_together);
            matches!(
                c.request(&Request::Claim {
                    id: id.into(),
                    tag: "ram".into(),
                }),
                Ok(Response::Claimed { .. })
            )
        }));
    }
    let won: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        won.iter().filter(|w| **w).count(),
        1,
        "a tag is a lock: exactly one claim wins {won:?}"
    );
    server.stop();
}

/// One frame written by hand over a raw socket, and the one line that answers it. A refusal closes
/// the connection, so a test reads exactly one reply and lets the drop do the rest.
fn hello_raw(socket: &std::path::Path, hello: &Request) -> Response {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    let stream = UnixStream::connect(socket).expect("a connection");
    let mut reader = BufReader::new(stream.try_clone().expect("a reader"));
    let mut writer = stream;
    let mut line = serde_json::to_string(hello).expect("a frame");
    line.push('\n');
    writer.write_all(line.as_bytes()).expect("the frame");
    writer.flush().expect("the frame");
    let mut answer = String::new();
    reader.read_line(&mut answer).expect("the answer");
    serde_json::from_str(answer.trim()).unwrap_or_else(|e| panic!("{e}: {answer:?}"))
}

fn operator_hello(token: Option<String>, session: &str, seat: Seat) -> Request {
    Request::Hello {
        protocol: ROOM_PROTOCOL_VERSION,
        speaker: Speaker::top_level(session),
        room: String::new(),
        seat,
        token,
    }
}

/// The socket and the token beside it are the user's alone. The mode is the first gate and the peer
/// uid is the second: a file's mode cannot see a descriptor inherited from somebody else.
#[test]
fn the_socket_and_its_token_are_the_users_alone() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("mode");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");
    let token = operator_token_path(&scratch.socket());
    assert_eq!(token, scratch.0.join("room.token"), "beside the socket");
    assert!(token.exists(), "the coordinator writes it at its start");
    for path in [scratch.socket(), token.clone()] {
        let mode = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{} is {mode:o}", path.display());
    }
    // And it dies with the socket: a word left behind would be one a later process could present
    // to a coordinator that never minted it.
    server.stop();
    assert!(!token.exists(), "the token goes when the room does");
}

/// The operator's seat is granted by the token the coordinator minted, and by nothing else. A seat
/// is what a connection CLAIMS; the file beside the socket is what proves it.
#[test]
fn the_operator_seat_is_granted_by_the_coordinators_token() {
    let scratch = Scratch::new("token");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");
    let token = std::fs::read_to_string(operator_token_path(&scratch.socket()))
        .expect("the token")
        .trim()
        .to_string();
    // No token: refused, and the sentence says where the one it wants is.
    let refused = hello_raw(
        &scratch.socket(),
        &operator_hello(None, OPERATOR, Seat::Operator),
    );
    match refused {
        Response::Refused { id: None, why } => {
            let said = why.to_string();
            assert!(said.contains("no token"), "{said}");
            assert!(
                said.contains(&operator_token_path(&scratch.socket()).display().to_string()),
                "the sentence names the file: {said}"
            );
        }
        other => panic!("a bare operator seat is refused: {other:?}"),
    }
    // A token this coordinator did not mint: refused too, so a token from another room on the same
    // box is not a password.
    let elsewhere = Scratch::new("token-other");
    let other = Server::serve(&elsewhere.socket(), Room::new()).expect("another socket");
    let theirs =
        std::fs::read_to_string(operator_token_path(&elsewhere.socket())).expect("a token");
    assert_ne!(theirs.trim(), token, "two coordinators, two tokens");
    match hello_raw(
        &scratch.socket(),
        &operator_hello(Some(theirs), OPERATOR, Seat::Operator),
    ) {
        Response::Refused { why, .. } => {
            assert!(why.to_string().contains("not the one"), "{why}");
        }
        other => panic!("another room's token is not this room's: {other:?}"),
    }
    // The token the file holds: the seat, and the room answers it.
    match hello_raw(
        &scratch.socket(),
        &operator_hello(Some(token.clone()), OPERATOR, Seat::Operator),
    ) {
        Response::Welcome { .. } => {}
        other => panic!("the minted token is the seat: {other:?}"),
    }
    // And a token is not an upgrade: a session seat that presents it and names itself `operator`
    // is still refused by name, because the name is checked against the seat.
    match hello_raw(
        &scratch.socket(),
        &operator_hello(Some(token), OPERATOR, Seat::Session),
    ) {
        Response::Refused { why, .. } => {
            assert!(
                matches!(why, letibot_room::Refused::ReservedName { .. }),
                "{why}"
            );
        }
        other => panic!("a session seat cannot be the operator: {other:?}"),
    }
    other.stop();
    server.stop();
}

/// A client whose pushed chat is collected, so a test can assert what reached it rather than
/// sleeping and hoping.
fn chatty(server: &Server, session: &str) -> (Client, Receiver<Message>) {
    let (client, _events) = client(server, session);
    let (tx, heard) = mpsc::channel();
    let tx = Mutex::new(tx);
    client.on_chat(Arc::new(move |m: &Message, _names: bool| {
        let _ = tx.lock().expect("the heard lock").send(m.clone());
    }));
    (client, heard)
}

/// A reader that stops reading must cost its own connection and nobody else's turn. The bound is
/// the queue in front of its writer, and the resource is the memory one silent peer may hold.
#[test]
fn a_stalled_reader_is_disconnected_and_does_not_block_the_others() {
    let scratch = Scratch::new("stall");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a socket");
    // A healthy reader never has this many frames in flight; a stalled one has nothing else.
    server.with_outbox(64);
    // The stalled connection: a hand-written client that reads its `welcome` and then nothing.
    let mut stalled = {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(scratch.socket()).expect("a connection");
        let hello = serde_json::to_string(&operator_hello(None, "s-stalled", Seat::Session))
            .expect("a frame");
        stream
            .write_all(format!("{hello}\n").as_bytes())
            .expect("the hello");
        let mut answer = String::new();
        BufReader::new(stream.try_clone().expect("a reader"))
            .read_line(&mut answer)
            .expect("the welcome");
        assert!(answer.contains("welcome"), "{answer}");
        stream
    };
    let (healthy, heard) = chatty(&server, "s-healthy");
    let (poster, _events) = client(&server, "s-poster");
    // A frame is at most 4 000 bytes of text, and this is more than any socket buffer on a box:
    // the stalled writer blocks on its socket, and then its queue is the only thing taking frames.
    const FRAMES: usize = 400;
    let text = "x".repeat(3_990);
    for i in 0..FRAMES {
        let answer = call(
            &poster,
            Request::Say {
                id: format!("s{i}"),
                room: HOST_ROOM.to_string(),
                text: format!("{i:05}{text}"),
                to: None,
                thread: None,
                refs: Vec::new(),
            },
        );
        assert!(matches!(answer, Response::Said { .. }), "{answer:?}");
    }
    // The stalled connection went, saying so, and the room is still serving the other two.
    until("the stalled reader to be disconnected", || {
        server.connections() == 2
    });
    // Every message reached the reader that was reading, in order - the poster was never blocked
    // behind the reader that was not.
    let mut got = Vec::new();
    while let Ok(m) = heard.recv_timeout(Duration::from_secs(5)) {
        got.push(m.seq);
        if got.len() == FRAMES {
            break;
        }
    }
    assert_eq!(
        got.len(),
        FRAMES,
        "every message reached the healthy reader"
    );
    assert!(
        got.windows(2).all(|w| w[0] < w[1]),
        "in the order they were said"
    );
    // And the stalled socket is at end-of-file: the peer the room gave up on is told by the close.
    {
        use std::io::Read;
        let mut sink = Vec::new();
        let _ = stalled.read_to_end(&mut sink);
    }
    healthy.session();
    server.stop();
}
