//! **The bell, per client — the two halves that are the client's own act**: a note read at the round
//! boundary a running turn actually reaches (firecode's first rule, *listen before you work*), and a
//! claim that says what it is doing before it does it (the third, *acknowledge before you act*).
//!
//! # What each test would have failed on before it existed
//!
//! **`a_room_note_posted_mid_turn_is_read_at_the_next_round_boundary`** — `HubSteering::try_next` is
//! the source the engine polls at every step boundary of a running turn, and it had five arms
//! (a held message, the hub's command, the harness's own injections, the deliveries, a monitor) and
//! **none of them was the room**. A room event became a sentence in `crate::room::wake` and was read
//! by `Harness::wake` — a door that only opens BETWEEN turns — so a message posted while a session
//! was mid-turn reached it after the work it would have changed. The operator's own words for the
//! rule: *the test is a message posted while a session is mid-turn reaching it before that turn's
//! next decision, not after the work is done.*
//!
//! **`a_claim_is_acknowledged_on_the_room_before_any_work_begins`** — `SessionRoom::call("claim")`
//! sent one frame and returned. The room's `Claim` has a `waits_on` field and the `waiting` verb
//! fills it, so *seen, doing X* was available and was nobody's act: a model at work does not think
//! about the queue's bookkeeping, which is the whole reason firecode's rule exists. From the queue,
//! a claim with no word on it is indistinguishable from a session that took the item and died.
//!
//! # Why these two are here and not in `crates/room`
//!
//! Both are claims about the DAEMON's client (`SessionRoom`) rather than about the wire: the room
//! only carries the frames. `crates/room/tests/bell.rs` is the socket's own half of the rules, and
//! `crates/harnessd/tests/room_bell_turn.rs` is the one that needs a live session.

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

/// **Rule 1 — listen before working.** A sentence pushed while a turn is running comes out of the
/// steering source at the next round boundary, and it comes out ONCE.
///
/// The push is what the room's reader thread does (`crate::room::link`'s wake closure), so this is
/// the same queue the real path fills. The engine is the poller (`turn/src/engine.rs` asks its
/// `SteeringSource` before every round), and the harnessd suite that runs a turn against a stub
/// provider asserts the other end of it — what this test owns is the one thing that was missing:
/// that a room note is among the answers at all.
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

    // **The message that would have changed the work**, posted by the room's reader thread while
    // this session's turn is in flight.
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

/// **Rule 3 — acknowledge before acting.** The claim and the word *seen, doing X* are on the room by
/// the time `claim` returns, so nothing a model does next can be the first thing the queue hears.
///
/// What the test measures is the ORDER the rule is about: the observer reads the room immediately
/// after the claimant's call returns and before the claimant has done anything else, and it finds
/// both the claim and the sentence. A client that had to remember a second call would leave the
/// window this rule exists to close, and a claim with no word on it is what the queue reads as a
/// session that took the item and died.
#[test]
fn a_claim_is_acknowledged_on_the_room_before_any_work_begins() {
    let scratch = Scratch::new("ack");
    // The server must outlive the clients: dropping it closes the listener, and a connection that
    // is still being served would lose its socket mid-frame.
    let _server = Server::serve(&scratch.socket(), Room::new()).expect("a room to talk to");
    let rung = Arc::new(AtomicUsize::new(0));
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |_: &str| {
            rung.fetch_add(1, Ordering::SeqCst);
        })
    };
    // The DAEMON's client — the one the `room` tool calls through, with the same ack in it.
    let me = SessionRoom::connect("s-worker", &scratch.socket(), bell, None)
        .expect("the session's connection to the room");

    let posted = me
        .call(
            "post",
            &serde_json::json!({"verb": "post", "tag": "ram", "body": "8 GB for the suite"}),
        )
        .expect("a post");
    assert!(posted.contains("#1"), "{posted}");

    // **The claim, in the words a model gives it.** `text` is the `waiting` verb's field and a claim
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

    // **An observer, reading the room as it stands** — which is what anybody queued behind this
    // claim would see. The claimant has done no work: the call has only just returned.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let heard = Arc::clone(&seen);
    let observer = Client::connect(
        &scratch.socket(),
        Speaker::top_level("s-observer"),
        Arc::new(move |e: &letibot_room::Event| {
            heard.lock().unwrap_or_else(|e| e.into_inner()).push(e.clone())
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
    let waits_on = claim
        .waits_on
        .as_ref()
        .unwrap_or_else(|| panic!("the claim says what it is for, before the work: {item:?}"));
    assert!(
        waits_on.text.contains("the 40-minute suite"),
        "in the claimant's own words: {waits_on:?}"
    );

    // **And a claim with nothing to say still says something.** A model that passes no `text` gets
    // the bare acknowledgement rather than silence — the rule is *acknowledge*, not *acknowledge
    // when you happen to have a sentence*.
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
    let waits_on = match &rows[0].state {
        State::Claimed { claim } => claim.waits_on.clone(),
        other => panic!("#1 is claimed again: {other:?}"),
    };
    assert!(
        waits_on.as_ref().is_some_and(|w| w.text.contains("working on it")),
        "a claim with no words of its own still acknowledges: {waits_on:?}"
    );
    // The observer's own channel is not this test's subject; dropping it closes its socket, which
    // is the room's `note_gone` and the claim's way back — nothing here asserts on it.
    drop(observer);
}
