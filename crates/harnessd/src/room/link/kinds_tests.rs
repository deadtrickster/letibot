//! What a `say`'s kind does to the wake path: an `fyi` is noted and never rings, a `blocker` rings
//! the room it was said in and arrives as a full turn, and an ordinary line keeps waking as before.

use super::*;

use serde_json::json;

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "letibot-room-kinds-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Everything the room pushes to a session, once the pushes have had time to arrive. The sleep is
/// the room's own reader thread: a note that has not landed yet is not a note this test can judge.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(600));
}

/// The tool's list of kinds and the room's are one list: a word the schema offers is a word the
/// room answers to, and the other way round. Two crates, so the pin is here.
#[test]
fn the_tools_kind_list_is_the_rooms_own() {
    let mine: Vec<&str> = letibot_room::SayKind::NAMED
        .iter()
        .map(|k| k.word())
        .collect();
    assert_eq!(
        mine,
        letibot_tools::builtins::room::SAY_KINDS.to_vec(),
        "the room's kinds and the tool's are the same words in the same order"
    );
    for word in mine {
        assert_ne!(
            letibot_room::SayKind::of(word),
            letibot_room::SayKind::Other,
            "`{word}` is a word this build knows"
        );
    }
}

#[test]
fn an_fyi_is_noted_and_never_rung_and_a_blocker_is_a_full_turn() {
    let scratch = Scratch::new("wake");
    let path = scratch.0.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let me_id = "s-a-kinds";
    let other_id = "s-b-kinds";
    let rung: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |session: &str| rung.lock().unwrap().push(session.to_string()))
    };
    let _me = SessionRoom::connect(
        me_id,
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell.clone(),
        None,
    )
    .expect("this session connects");
    let other = SessionRoom::connect(
        other_id,
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell,
        None,
    )
    .expect("the peer connects");

    // An `fyi` is a note for a cheap turn: no bell, nothing taking the whole of a turn.
    other
        .call(
            "say",
            &json!({"room": "host", "text": "@s-a-kinds the port moved to 5433", "kind": "fyi"}),
        )
        .expect("an fyi");
    settle();
    assert!(
        !rung.lock().unwrap().iter().any(|s| s == me_id),
        "an fyi rang the bell: {:?}",
        rung.lock().unwrap()
    );
    let noted = wake::take_wake(me_id);
    assert!(
        noted.lines.iter().any(|l| l.contains("the port moved")),
        "the fyi was not noted at all: {noted:?}"
    );
    assert!(noted.cheap, "an fyi woke a full turn: {noted:?}");
    rung.lock().unwrap().clear();

    // A `blocker` rings even though nothing in it names this session, and its note is a full turn.
    // It goes to the project room: `host` takes only what names somebody, and a blocker names the
    // state of the box rather than a reader.
    other
        .call(
            "say",
            &json!({"text": "the disk is full on lubuntu1", "kind": "blocker"}),
        )
        .expect("a blocker");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline && wake::waiting(me_id) == 0 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let taken = wake::take_wake(me_id);
    let line = taken
        .lines
        .iter()
        .find(|l| l.contains("the disk is full"))
        .unwrap_or_else(|| panic!("the blocker was not noted: {taken:?}"));
    assert!(!taken.cheap, "a blocker woke a cheap turn: {taken:?}");
    assert!(line.contains(", blocker"), "{line}");
    assert!(!line.contains("passing by"), "{line}");
    assert!(
        rung.lock().unwrap().iter().any(|s| s == me_id),
        "a blocker did not ring: {:?}",
        rung.lock().unwrap()
    );
    server.stop();
}

/// An ordinary line still wakes its room, as it did before kinds: the kind mechanism did not take
/// the old wake away.
#[test]
fn an_ordinary_line_still_wakes_the_room_it_is_said_in() {
    let scratch = Scratch::new("plain");
    let path = scratch.0.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let me_id = "s-a-plain";
    let other_id = "s-b-plain";
    let rung: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |session: &str| rung.lock().unwrap().push(session.to_string()))
    };
    let _me = SessionRoom::connect(
        me_id,
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell.clone(),
        None,
    )
    .expect("this session connects");
    let other = SessionRoom::connect(
        other_id,
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell,
        None,
    )
    .expect("the peer connects");
    other
        .call(
            "say",
            &json!({"room": "host", "text": "the build is green", "kind": "announce"}),
        )
        .expect("an announcement");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline && wake::waiting(me_id) == 0 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let taken = wake::take_wake(me_id);
    let line = taken
        .lines
        .iter()
        .find(|l| l.contains("the build is green"))
        .unwrap_or_else(|| panic!("the line was never noted: {taken:?}"));
    assert!(line.contains("passing by"), "{line}");
    assert!(
        rung.lock().unwrap().iter().any(|s| s == me_id),
        "an ordinary line did not wake the room's session: {:?}",
        rung.lock().unwrap()
    );
    server.stop();
}
