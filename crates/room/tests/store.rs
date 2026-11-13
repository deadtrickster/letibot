//! The queues' store, end to end on a real coordinator socket: a claim and its queue survive a
//! restart, and a lease whose holder never beats again expires on reload exactly as the live
//! sweep expires it - back in the queue below the threshold, raised at it.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{About, Event, Refused, Request, Response, Room, Speaker, Why};

/// A lease the tests can walk past without sleeping one out.
const LEASE: u64 = 90_000;
/// A clock period longer than the test, so nothing sweeps unless the test asks: `Server::tick`
/// and the reload's own sweep are the only clocks here.
const LONG_SWEEP: u64 = 3_600_000;

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

    fn items(&self) -> PathBuf {
        self.0.join("room.items")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A coordinator whose clock never fires on its own, so every sweep in the test is one the test
/// asked for by name.
fn coordinator(s: &Scratch) -> Server {
    Server::with_options(
        &s.0.join("room.sock"),
        Room::with_lease_ms(LEASE),
        LONG_SWEEP,
    )
    .expect("a coordinator")
}

/// A coordinator keeping its queues in the scratch's own store.
fn stored(s: &Scratch) -> Server {
    let server = coordinator(s);
    server.with_item_store(&s.items());
    server
}

/// [`stored`], reloading the store at a clock the test names.
fn stored_at(s: &Scratch, now: u64) -> Server {
    let server = coordinator(s);
    server.with_item_store_at(&s.items(), now);
    server
}

fn client(server: &Server, session: &str) -> (Client, mpsc::Receiver<Event>) {
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

fn claim(c: &Client, id: &str, tag: &str) -> Response {
    call(
        c,
        Request::Claim {
            id: id.into(),
            tag: tag.into(),
        },
    )
}

fn abandon(c: &Client, id: &str, item: u64, why: &str) -> Response {
    call(
        c,
        Request::Abandon {
            id: id.into(),
            item,
            why: why.into(),
        },
    )
}

fn item_of(server: &Server, id: u64) -> letibot_room::Item {
    server
        .items()
        .into_iter()
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("item #{id} is in the room"))
}

/// One abandonment the polite way, so the count is where the test wants it.
fn claim_and_give_up(server: &Server, session: &str, tag: &str) {
    let (c, _heard) = client(server, session);
    let Response::Claimed { item, .. } = claim(&c, "c1", tag) else {
        panic!("`{session}` must take the head of `{tag}`");
    };
    match abandon(&c, "a1", item.id, "the suite blew up") {
        Response::Abandoned { .. } => {}
        other => panic!("a holder may give it up: {other:?}"),
    }
    // Held to the end of the test: dropping the client would fire the close handler, which is a
    // DIFFERENT way to lose a claim, and this test is about the lease.
    std::mem::forget(c);
    std::mem::forget(_heard);
}

#[test]
fn a_claim_and_its_queue_survive_a_coordinator_restart() {
    let dir = Scratch::new("items");
    let first = stored(&dir);
    let (a, _heard) = client(&first, "s-a");
    let one = post(&a, "p1", "ram", "8 GB, ~10 min");
    post(&a, "p2", "ram", "2 GB, ~1 min");
    let Response::Claimed { item, .. } = claim(&a, "c1", "ram") else {
        panic!("the head must be claimable");
    };
    assert_eq!(item.id, one);
    let (by, claim_ms, beat_ms) = match item.claim() {
        Some(c) => (c.by.clone(), c.claim_ms, c.beat_ms),
        None => panic!("a claim is a lease"),
    };
    assert_eq!(by, "s-a");

    // The stop, not the close: `a` is kept alive so the old coordinator's close handler - which
    // abandons a holder's claims at once - never runs. A real restart is the same shape: the
    // sessions' daemons keep running and their sockets die with the coordinator's process.
    first.stop();
    std::thread::sleep(Duration::from_millis(20));

    let second = stored(&dir);
    let items = second.items();
    assert_eq!(items.len(), 2, "the queue is the queue: {items:?}");
    let held = item_of(&second, one);
    let lease = held.claim().expect("the claim came back");
    assert_eq!(lease.by, "s-a");
    assert_eq!(
        (lease.claim_ms, lease.beat_ms),
        (claim_ms, beat_ms),
        "the same lease, not a new one"
    );
    assert_eq!(item_of(&second, 2).state_name(), "open");
    assert!(
        second.identity().contains("kept in a store"),
        "the pane's own sentence says the queues were kept: {}",
        second.identity()
    );

    // The tag is still a lock: another session is refused Busy naming the holder.
    let (b, _heard_b) = client(&second, "s-b");
    match claim(&b, "c-b", "ram") {
        Response::Refused {
            why: Refused::Busy { by, item, .. },
            ..
        } => {
            assert_eq!((by.as_str(), item), ("s-a", one));
        }
        other => panic!("a restart must not free a tag somebody holds: {other:?}"),
    }
    match call(
        &b,
        Request::Holding {
            session: Some("s-a".into()),
        },
    ) {
        Response::Holding { rows, .. } => assert_eq!(
            rows.len(),
            1,
            "the holder still holds it, and the reminder says so"
        ),
        other => panic!("{other:?}"),
    }
    second.stop();
    std::mem::forget((a, _heard, b, _heard_b));
}

#[test]
fn a_stale_lease_expires_on_reload_as_the_live_sweep_expires_it() {
    // The live twin: the same claim, taken back by the clock's own sweep.
    let live_dir = Scratch::new("stale-live");
    let live = coordinator(&live_dir);
    let (a, _heard) = client(&live, "s-a");
    post(&a, "p1", "ram", "8 GB, ~10 min");
    claim(&a, "c1", "ram");
    let t = letibot_room::server::now_ms();
    let taken = live.tick(t + LEASE + 1);
    assert_eq!(taken.len(), 1, "the live sweep takes it back: {taken:?}");
    let live_item = item_of(&live, 1);
    assert_eq!(live_item.state_name(), "abandoned");
    assert_eq!(live_item.abandonments(), 1);
    assert!(
        live_item.claimable(),
        "one loss, so it is back in the queue"
    );
    assert!(matches!(
        live_item.last_abandonment().map(|a| &a.why),
        Some(Why::BeatStale { .. })
    ));

    // The reloaded twin: the same claim in a store, taken back by the reload's own sweep.
    let dir = Scratch::new("stale");
    let first = stored(&dir);
    let (a, _heard) = client(&first, "s-a");
    post(&a, "p1", "ram", "8 GB, ~10 min");
    claim(&a, "c1", "ram");
    let t = letibot_room::server::now_ms();
    first.stop();
    std::thread::sleep(Duration::from_millis(20));
    let second = stored_at(&dir, t + LEASE + 1);

    let reloaded = item_of(&second, 1);
    assert_eq!(
        reloaded.state_name(),
        live_item.state_name(),
        "reload expires it as the live code does"
    );
    assert_eq!(reloaded.abandonments(), live_item.abandonments());
    assert_eq!(reloaded.claimable(), live_item.claimable());
    assert!(matches!(
        reloaded.last_abandonment().map(|a| &a.why),
        Some(Why::BeatStale { .. })
    ));
    // And the requeue is real: the next session takes it at once.
    let (b, _heard_b) = client(&second, "s-b");
    match claim(&b, "c-b", "ram") {
        Response::Claimed { item, .. } => assert_eq!(item.id, 1),
        other => panic!("an expired claim is off the tag: {other:?}"),
    }
    second.stop();
    live.stop();
    std::mem::forget((a, _heard, b, _heard_b));
}

#[test]
fn a_lease_that_reaches_the_threshold_on_reload_raises_and_asks() {
    let dir = Scratch::new("raise");
    let first = stored(&dir);
    let (a, _heard_a) = client(&first, "s-a");
    let one = post(&a, "p1", "ram", "8 GB, ~10 min");
    claim_and_give_up(&first, "s-b", "ram");
    claim_and_give_up(&first, "s-c", "ram");
    // The third claimant holds it and goes quiet - the coordinator stops before its lease does.
    let (d, _heard_d) = client(&first, "s-d");
    let Response::Claimed { item, .. } = claim(&d, "c3", "ram") else {
        panic!("two losses in, the head is claimable");
    };
    assert_eq!(item.id, one);
    let t = letibot_room::server::now_ms();
    first.stop();
    std::thread::sleep(Duration::from_millis(20));

    // Reloaded an hour later: the reload's sweep is the third loss, so the item raises exactly as
    // it would live - frozen, its question owed to the tie-breaker who has not connected yet.
    let second = stored_at(&dir, t + LEASE + 1);
    let raised = item_of(&second, one);
    assert_eq!(raised.state_name(), "abandoned");
    assert_eq!(raised.abandonments(), 3);
    assert!(!raised.claimable(), "a raised item is frozen");
    let question = raised.question.expect("the third loss raises it");
    let owed = second.open_questions();
    assert_eq!(owed.len(), 1, "the tie-breaker is owed one ruling");
    assert_eq!(owed[0].id, question);
    assert_eq!(owed[0].facts.item, one);
    assert!(matches!(
        owed[0].about,
        About::ItemRaised { item, ref tag } if item == one && tag.as_str() == "ram"
    ));
    // And a claim on the row is refused with the raise, not with the tag: the freeze is what the
    // lock now is.
    let (e, _heard_e) = client(&second, "s-e");
    match call(
        &e,
        Request::ClaimItem {
            id: "c-e".into(),
            item: one,
        },
    ) {
        Response::Refused {
            why: Refused::Raised { question, .. },
            ..
        } => assert_eq!(question, owed[0].id),
        other => panic!("a raised item is not claimable: {other:?}"),
    }
    second.stop();
    std::mem::forget((a, _heard_a, d, _heard_d, e, _heard_e));
}

/// The bar: six hundred items posted and settled, the coordinator stopped and started again, and
/// the FIRST of them read by its id over the socket. The window bounds the memory; the closed
/// segment is the record.
#[test]
fn a_settled_item_out_of_the_window_is_read_by_its_id_after_a_restart() {
    let dir = Scratch::new("closed");
    let first = stored(&dir);
    let (a, heard) = client(&first, "s-a");
    for i in 0..600u64 {
        let one = post(&a, &format!("p{i}"), "ram", "8 GB, ~10 min");
        assert!(matches!(
            claim(&a, &format!("c{i}"), "ram"),
            Response::Claimed { .. }
        ));
        let done = call(
            &a,
            Request::Done {
                id: format!("d{i}"),
                item: one,
            },
        );
        assert!(matches!(done, Response::Finished { .. }), "{done:?}");
    }
    first.stop();
    std::thread::sleep(Duration::from_millis(20));

    let second = stored(&dir);
    assert_eq!(second.items().len(), 500, "the window is what memory holds");
    let (b, heard_b) = client(&second, "s-b");
    match call(&b, Request::Item { item: 1 }) {
        Response::Item { item: Some(one) } => {
            assert_eq!(one.id, 1);
            assert_eq!(one.state_name(), "done", "the state it closed in");
            assert_eq!(one.body, "8 GB, ~10 min");
        }
        other => panic!("item 1 is readable by its id after a restart: {other:?}"),
    }
    // A number this room never minted is said, not refused: a read is not a verb.
    match call(&b, Request::Item { item: 10_000 }) {
        Response::Item { item: None } => {}
        other => panic!("an unminted number answers nothing: {other:?}"),
    }
    second.stop();
    std::mem::forget((a, heard, b, heard_b));
}
