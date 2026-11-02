//! The bell, per client - the two halves that are the client's own act: a note read at the
//! round boundary a running turn actually reaches (*listen before you work*), and a claim
//! that says what it is doing before it does it (*acknowledge before you act*). The first
//! test would fail without the room among `HubSteering::try_next`'s arms: a room event
//! became a sentence read by `Harness::wake`, a door that only opens BETWEEN turns, so a
//! message posted while a session was mid-turn reached it after the work it would have
//! changed. The second would fail on a `claim` that sent one frame and returned:

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use letibot_harnessd::harness::HubSteering;
use letibot_harnessd::room::link::SessionRoom;
use letibot_harnessd::room::wake;
use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Request, Response, Room, Speaker, State};
use letibot_sessionlog::hub::Hub;
use letibot_turn::steering::SteeringSource;

/// Rule 1 - listen before working. A sentence pushed while a turn is running comes out of
/// the steering source at the next round boundary, and it comes out ONCE. The push is what
/// the room's reader thread does, so this is the same queue the real path fills.
#[test]
fn a_room_note_posted_mid_turn_is_read_at_the_next_round_boundary() {
    let me = "s-mid-turn";
    let hub = Hub::new(me);
    let mut steering = HubSteering::new(Arc::clone(&hub));
    // Nothing is waiting, so the boundary finds nothing: the arm must not invent a message.
    assert!(
        steering.try_next().is_none(),
        "a boundary with nothing to say says nothing"
    );

    // The message that would have changed the work, posted by the room's reader thread while this
    // session's turn is in flight.
    let note = "[room] `s-b` claimed #1 under `ram` — while that claim lives, `ram` is locked and \
                your own claim under it would be refused.";
    wake::push(me, note.to_string());
    // Addressed: another session's note is not this session's news.
    wake::push("s-elsewhere", "[room] somebody else's news".to_string());

    let said = steering
        .try_next()
        .expect("the room's note must be read at the round boundary, not after the turn ends");
    assert!(
        said.text.contains("`s-b` claimed #1 under `ram`"),
        "the boundary hands the model the room's sentence: {said:?}"
    );
    assert!(
        !said.from_operator && !said.urgent,
        "and as a notice, not as the operator's words and not as an interruption: {said:?}"
    );
    assert_eq!(
        wake::waiting(me),
        0,
        "taken is taken: the between-turns door (`Harness::wake`) must not say it a second time"
    );
    assert_eq!(
        wake::waiting("s-elsewhere"),
        1,
        "a bell is addressed — another session's note is still waiting for that session"
    );
    assert!(
        steering.try_next().is_none(),
        "and the boundary is empty again: a note is not re-offered by every round"
    );
}

/// A directory of this test's own, removed when the test ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        use std::sync::atomic::AtomicU64;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let p =
            std::env::temp_dir().join(format!("letibot-bell-{what}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Scratch(p)
    }

    fn socket(&self) -> std::path::PathBuf {
        self.0.join("room.sock")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Rule 3 - acknowledge before acting. The claim and the word *seen, doing X* are on the
/// room by the time `claim` returns, so nothing a model does next can be the first thing
/// the queue hears. What the test measures is the ORDER the rule is about:
#[test]
fn a_claim_is_acknowledged_on_the_room_before_any_work_begins() {
    let scratch = Scratch::new("ack");
    // The server must outlive the clients: dropping it closes the listener, and a connection that is
    // still being served would lose its socket mid-frame.
    let _server = Server::serve(&scratch.socket(), Room::new()).expect("a room to talk to");
    let rung = Arc::new(AtomicUsize::new(0));
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |_: &str| {
            rung.fetch_add(1, Ordering::SeqCst);
        })
    };
    // The DAEMON's client - the one the `room` tool calls through, with the same ack in it.
    let me = SessionRoom::connect(
        "s-worker",
        &scratch.socket(),
        letibot_room::DEFAULT_PROJECT,
        None,
        bell,
        None,
    )
    .expect("the session's connection to the room");

    let posted = me
        .call(
            "post",
            &serde_json::json!({"verb": "post", "tag": "ram", "body": "8 GB for the suite"}),
        )
        .expect("a post");
    assert!(posted.contains("#1"), "{posted}");

    // The claim, in the words a model gives it. `text` is the `waiting` verb's field and a claim
    // may carry it, so the acknowledgement can be the model's own sentence about the work.
    let claimed = me
        .call(
            "claim",
            &serde_json::json!({"verb": "claim", "tag": "ram", "text": "the 40-minute suite"}),
        )
        .expect("a claim");
    assert!(
        claimed.contains("before the work"),
        "the sentence a model reads says the acknowledgement went with the claim: {claimed}"
    );

    // An observer, reading the room as it stands - which is what anybody queued behind this claim
    // would see. The claimant has done no work: the call has only just returned.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let heard = Arc::clone(&seen);
    let observer = Client::connect(
        scratch.socket(),
        Speaker::top_level("s-observer"),
        Arc::new(move |e: &letibot_room::Event| {
            heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(e.clone())
        }),
    )
    .expect("an observer joins the room");
    let queue = observer
        .request(&Request::Queue { tag: "ram".into() })
        .expect("a queue read");
    let Response::Queue { rows, .. } = queue else {
        panic!("a queue read: {queue:?}");
    };
    let item = rows.first().expect("#1 is in the queue");
    let State::Claimed { claim } = &item.state else {
        panic!("#1 was claimed before any work began: {item:?}");
    };
    assert_eq!(claim.by, "s-worker", "and by the session that claimed it");
    // `said`, not `waits_on` - the two are opposite facts and the room keeps them apart. The
    // acknowledgement is what the holder is DOING;
    let said = claim
        .said
        .as_ref()
        .unwrap_or_else(|| panic!("the claim says what it is for, before the work: {item:?}"));
    assert!(
        said.text.contains("the 40-minute suite"),
        "in the claimant's own words: {said:?}"
    );
    assert!(
        claim.waits_on.is_none(),
        "and it did NOT say the holder is waiting on anything: a claim is the holder's own work, \
         and `waits_on` is what makes a reminder quiet: {claim:?}"
    );

    // And a claim with nothing to say still says something. A model that passes no `text` gets the
    // bare acknowledgement rather than silence - the rule is *acknowledge*, not *acknowledge when you
    // happen to have a sentence*.
    let _ = observer
        .request(&Request::Abandon {
            id: "a1".into(),
            item: item.id,
            why: "the test is done with it".into(),
        })
        .expect("the observer gives it back");
    let again = me
        .call("claim", &serde_json::json!({"verb": "claim", "tag": "ram"}))
        .expect("a second claim");
    assert!(again.contains("working on it"), "{again}");
    let queue = observer
        .request(&Request::Queue { tag: "ram".into() })
        .expect("a queue read");
    let Response::Queue { rows, .. } = queue else {
        panic!("a queue read: {queue:?}");
    };
    let (said, waits_on) = match &rows[0].state {
        State::Claimed { claim } => (claim.said.clone(), claim.waits_on.clone()),
        other => panic!("#1 is claimed again: {other:?}"),
    };
    assert!(
        said.as_ref()
            .is_some_and(|w| w.text.contains("working on it")),
        "a claim with no words of its own still acknowledges: {said:?}"
    );
    assert!(
        waits_on.is_none(),
        "and a bare claim is not a wait either: {waits_on:?}"
    );
    // The observer's own channel is not this test's subject; dropping it closes its socket, which is
    // the room's `note_gone` and the claim's way back - nothing here asserts on it.
    drop(observer);
}

/// Rule 2 at the daemon's door - a reconnect is told what it missed, in words, once.
/// `crates/room/tests/bell.rs` proves the room COUNTS a gap and hands the count over with
/// the `Welcome`. The count is not the rule;
#[test]
fn a_reconnect_is_told_what_it_missed_once_and_in_words() {
    let scratch = Scratch::new("gap-line");
    let server = Server::serve(&scratch.socket(), Room::new()).expect("a room to talk to");
    let rung = Arc::new(AtomicUsize::new(0));
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |_: &str| {
            rung.fetch_add(1, Ordering::SeqCst);
        })
    };
    let session = "s-gap-line";

    // ---- the session, and then its absence --------------------------------
    let first = SessionRoom::connect(
        session,
        &scratch.socket(),
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::clone(&bell),
        None,
    )
    .expect("the session's connection to the room");
    first
        .call(
            "post",
            &serde_json::json!({"verb": "post", "tag": "ram", "body": "8 GB for the suite"}),
        )
        .expect("a post");
    // Its own post is news to it, and the queue is process-wide: take it now, or the gap's
    // sentence below is asserted against a list that already had something in it.
    wait_until("the poster to be told what it made", || {
        wake::waiting(session) >= 1
    });
    let own = wake::take(session);
    assert!(
        own.iter().any(|l| l.contains("posted #1")),
        "the poster is told what it made: {own:?}"
    );
    drop(first);
    wait_until("the room to notice the close", || server.connections() == 0);

    // ---- an event with nobody behind the socket ---------------------------
    // A claim by somebody else, addressed to the poster - which is `session`, and `session` is not
    // here. The room counts it and the count is the whole of what it can say.
    let claimant = Client::connect(
        scratch.socket(),
        Speaker::top_level("s-gap-claimant"),
        Arc::new(|_: &letibot_room::Event| {}),
    )
    .expect("the claimant joins the room");
    let claimed = claimant
        .request(&Request::Claim {
            id: "c1".into(),
            tag: "ram".into(),
        })
        .expect("a claim");
    assert!(matches!(claimed, Response::Claimed { .. }), "{claimed:?}");
    let rung_before = rung.load(Ordering::SeqCst);

    // ---- the reconnect ----------------------------------------------------
    let back = SessionRoom::connect(
        session,
        &scratch.socket(),
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::clone(&bell),
        None,
    )
    .expect("the session comes back");
    wait_until("the gap to be said", || wake::waiting(session) >= 1);
    let said = wake::take(session);
    assert_eq!(
        said.len(),
        1,
        "one gap is ONE sentence, however many events fell into it: {said:?}"
    );
    let line = &said[0];
    for wanted in [
        "1 event(s)",
        "not replayed",
        "`room queue <tag>`",
        "`room holding`",
    ] {
        assert!(
            line.contains(wanted),
            "the sentence names the count and the reads that cannot be wrong ({wanted:?}): {line}"
        );
    }
    assert!(
        rung.load(Ordering::SeqCst) > rung_before,
        "and it RINGS: a sentence nobody is woken for is a mailbox, which is the thing this file \
         exists not to be"
    );

    // And nothing is left for the next turn. The count was taken by the `Welcome`, so the reader's
    // next pass finds zero - a client that re-reported the same gap would tell the model the room had
    // gone quiet again every few seconds.
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(wake::waiting(session), 0, "the gap is said once: {said:?}");
    drop(back);
}

/// A turn ending rings nothing; a message under the tag does. A child's completion is not a
/// firing.
#[test]
fn a_turn_ending_rings_no_bell_and_a_message_under_the_tag_does() {
    let scratch = Scratch::new("r57");
    // The server must outlive the clients: dropping it closes the listener under a connection that is
    // still being served.
    let _server = Server::serve(&scratch.socket(), Room::new()).expect("a room to talk to");
    let rung = Arc::new(AtomicUsize::new(0));
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |_: &str| {
            rung.fetch_add(1, Ordering::SeqCst);
        })
    };
    let me = SessionRoom::connect(
        "s-turn-end",
        &scratch.socket(),
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::clone(&bell),
        None,
    )
    .expect("the session's connection to the room");
    me.call(
        "post",
        &serde_json::json!({"verb": "post", "tag": "ram", "body": "8 GB for the suite"}),
    )
    .expect("a post");
    let claimed = me
        .call("claim", &serde_json::json!({"verb": "claim", "tag": "ram"}))
        .expect("a claim");
    assert!(claimed.contains("#1"), "{claimed}");

    // An observer queued under the same tag, so *nothing happened* is a fact about a channel that
    // works rather than about a channel that was never open.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let heard = Arc::clone(&seen);
    let observer = Client::connect(
        scratch.socket(),
        Speaker::top_level("s-r57-observer"),
        Arc::new(move |e: &letibot_room::Event| {
            heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(e.clone())
        }),
    )
    .expect("an observer joins the room");
    observer
        .request(&Request::Post {
            id: "o1".into(),
            tag: "ram".into(),
            body: "the other 8 GB".into(),
        })
        .expect("the observer posts, which makes it a member of the tag");
    std::thread::sleep(std::time::Duration::from_millis(150));
    seen.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let rung_before = rung.load(Ordering::SeqCst);

    // ---- a turn, begun and ended ------------------------------------------
    // The whole of what "the subagent finished" is on this side of the socket.
    me.turn_started();
    std::thread::sleep(std::time::Duration::from_millis(120));
    me.turn_ended();
    std::thread::sleep(std::time::Duration::from_millis(200));

    let events = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(
        events.is_empty(),
        "a turn ending is not a room event: the room hears nothing about it: {events:?}"
    );
    assert_eq!(
        rung.load(Ordering::SeqCst),
        rung_before,
        "and nothing rings — not for the session, not for the tag"
    );
    // And the claim survives, which is the half that makes the confusion expensive: a completion
    // is not an abandonment, so the item is still held and the queue still says so.
    let queue = observer
        .request(&Request::Queue { tag: "ram".into() })
        .expect("a queue read");
    let Response::Queue { rows, .. } = queue else {
        panic!("a queue read: {queue:?}");
    };
    let held = rows.iter().find(|r| r.id == 1).expect("#1 is in the queue");
    assert!(
        matches!(&held.state, State::Claimed { claim } if claim.by == "s-turn-end"),
        "a turn ending gives nothing back: {held:?}"
    );

    // ---- and a message under the tag IS a bell ----------------------------
    let third = Client::connect(
        scratch.socket(),
        Speaker::top_level("s-r57-third"),
        Arc::new(|_: &letibot_room::Event| {}),
    )
    .expect("a third session joins the room");
    third
        .request(&Request::Post {
            id: "t1".into(),
            tag: "ram".into(),
            body: "8 GB more".into(),
        })
        .expect("a post under the tag");
    wait_until(
        "the message to reach the session queued under the tag",
        || {
            seen.lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|e| matches!(e, letibot_room::Event::Posted { by, .. } if by == "s-r57-third"))
        },
    );
}

/// Wait until `f` says so - for a fact that arrives on a thread rather than as a frame.
fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("waited ten seconds for {what}");
}
