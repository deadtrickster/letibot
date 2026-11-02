//! A room is a project's, and its membership is the tree - the operator's hierarchy
//! `<project>/<top-sessions>/<subagents>*|system/`, as the room holds it. Three facts, each
//! false while there was one room per BOX:

use std::sync::Arc;

use letibot_harnessd::room::link::SessionRoom;
use letibot_harnessd::room::roster::RosterSource;
use letibot_harnessd::room::wake;
use letibot_room::{Group, Member, Server};
use serde_json::json;

/// A directory of this test's own, under the temp dir rather than a shared path.
fn scratch(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("letibot-roomtree-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory of this test's own");
    dir
}

/// The key is the tree's own - the twelve hex characters `scripts/letibot` cuts
/// `letibot_sessionlog::workspace_key` computes and `s-gatekeeper-<key>` is spelled from.
/// This is the pin that lets the room crate keep its own copy of the rule
#[test]
fn the_project_key_is_the_workspace_key_the_rest_of_the_tree_uses() {
    let dir = scratch("key");
    let nested = dir.join("a").join("b");
    std::fs::create_dir_all(&nested).expect("a directory");
    for root in [dir.display().to_string(), nested.display().to_string()] {
        assert_eq!(
            letibot_room::project_key(&root),
            letibot_sessionlog::workspace_key(std::path::Path::new(&root)),
            "the room names `{root}` exactly as the gatekeeper's session id does"
        );
        // And the gatekeeper of that project is named by the same key - which is what makes a
        // `system/gatekeepers` row recognisable as *this* project's and no other's.
        let gate = letibot_sessionlog::gatekeeper_session(std::path::Path::new(&root));
        assert!(
            gate.ends_with(&letibot_room::project_key(&root)),
            "{gate} must carry the project's key"
        );
    }
}

/// A tree the room is told about, standing in for `Registry::list` - the grouping itself is
/// pinned by `harnessd`'s `room::roster` tests, which is where the rules live.
struct Tree(Vec<Member>);

impl RosterSource for Tree {
    fn members(&self, _project: &str) -> Vec<Member> {
        self.0.clone()
    }
}

fn member(session: &str, label: &str, group: Group, parent: Option<&str>, depth: u32) -> Member {
    Member {
        session: session.to_string(),
        label: label.to_string(),
        group,
        parent: parent.map(str::to_string),
        depth,
        live: true,
    }
}

/// Two projects on one socket are two rooms, and neither hears the other. The tag is the
/// same word in both (`ram`), which is the whole point: one lock for the box meant B's
/// claim was refused by A's, and B could not tell whose queue it had walked into.
#[test]
fn two_projects_are_two_rooms_and_a_tag_is_not_shared_between_them() {
    let dir = scratch("projects");
    let socket = dir.join("room.sock");
    let server = Server::serve(&socket, letibot_room::Room::new()).expect("a socket");

    let one = letibot_room::project_key(&dir.join("one").display().to_string());
    let two = letibot_room::project_key(&dir.join("two").display().to_string());
    let quiet: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});

    let a = SessionRoom::connect("s-a", &socket, &one, None, Arc::clone(&quiet), None)
        .expect("a session of project one");
    let b = SessionRoom::connect("s-b", &socket, &two, None, Arc::clone(&quiet), None)
        .expect("a session of project two");
    // Neither session named a room, a socket or an id - the derivation did it, from the project
    // the daemon placed it in.
    assert_eq!(a.project(), one);
    assert_eq!(b.project(), two);
    assert_ne!(a.project(), b.project());

    let posted = a
        .call("post", &json!({"tag": "ram", "body": "8 GB"}))
        .expect("a post in project one");
    assert!(posted.contains("#1"), "{posted}");

    // Project two's `ram` is empty - a tag is a project's, not the box's.
    let theirs = b.call("queue", &json!({"tag": "ram"})).expect("a read");
    assert!(
        !theirs.contains("8 GB"),
        "project two must not see project one's item: {theirs}"
    );
    // And it may claim: the lock A holds is A's.
    let claimed = b
        .call("post", &json!({"tag": "ram", "body": "2 GB"}))
        .and_then(|_| b.call("claim", &json!({"tag": "ram"})))
        .expect("project two claims its own `ram`");
    assert!(claimed.contains("you hold #1"), "{claimed}");
    // While A's own `ram` is still the item A posted, held by nobody.
    let mine = a.call("queue", &json!({"tag": "ram"})).expect("a read");
    assert!(mine.contains("8 GB"), "{mine}");
    assert!(mine.contains("#1"), "{mine}");

    // The pane's read is the room's, and each session is told which room it reached.
    let one_room = server.room_name_of(&one);
    let two_room = server.room_name_of(&two);
    assert!(one_room.ends_with(&one) && two_room.ends_with(&two));
    assert_ne!(one_room, two_room);
    assert_eq!(server.items_in(&one).len(), 1);
    assert_eq!(server.items_in(&two).len(), 1);
    assert_eq!(server.projects().len(), 3, "the box's own room, plus two");

    // And the reads that name no tag at all reach the session's own room: `holding` is what a
    // session asks about itself, and each one is answered from the room its daemon put it in.
    let a_holds = a.call("holding", &json!({})).expect("a read");
    let b_holds = b.call("holding", &json!({})).expect("a read");
    assert!(b_holds.contains("2 GB"), "b holds its own `ram`: {b_holds}");
    assert!(
        !b_holds.contains("8 GB"),
        "and nothing of project one's: {b_holds}"
    );
    assert!(
        a_holds.contains("holds nothing"),
        "project one's room has nothing claimed: {a_holds}"
    );
    // `reminders` is the whole ROOM's, and a session reads its own: b's row is in b's room and
    // nowhere else, which is the same isolation the queue showed, read from the other side.
    let a_reminders = a.call("reminders", &json!({})).expect("a read");
    let b_reminders = b.call("reminders", &json!({})).expect("a read");
    assert!(
        !a_reminders.contains("2 GB") && !a_reminders.contains("8 GB"),
        "project one's room reminds nobody: {a_reminders}"
    );
    assert!(b_reminders.contains("2 GB"), "{b_reminders}");
    assert!(!b_reminders.contains("8 GB"), "{b_reminders}");
    server.stop();
}

/// The tree decides WHICH room an event is in; the bell decides WHO is told - and the two
/// rules meet in one assertion: a post under project one's `ram` rings project one's
/// session and leaves project two's bell silent.
#[test]
fn a_bell_in_one_projects_room_does_not_ring_in_another() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = scratch("bells");
    let socket = dir.join("room.sock");
    let server = Server::serve(&socket, letibot_room::Room::new()).expect("a socket");
    let one = letibot_room::project_key(&dir.join("one").display().to_string());
    let two = letibot_room::project_key(&dir.join("two").display().to_string());

    // One counter per session, so the assertion is about a bell that did or did not ring rather
    // than about a shared total that two rooms could each have moved.
    let rung_one = Arc::new(AtomicUsize::new(0));
    let rung_two = Arc::new(AtomicUsize::new(0));
    let bell = |n: &Arc<AtomicUsize>| -> Arc<dyn Fn(&str) + Send + Sync> {
        let n = Arc::clone(n);
        Arc::new(move |_: &str| {
            n.fetch_add(1, Ordering::SeqCst);
        })
    };
    // Session ids of this test's own: `crate::room::wake` is one queue per session for the whole
    // process, and two tests sharing an id would read each other's notes.
    let a = SessionRoom::connect("s-one-worker", &socket, &one, None, bell(&rung_one), None)
        .expect("a session of project one");
    let b = SessionRoom::connect("s-two-worker", &socket, &two, None, bell(&rung_two), None)
        .expect("a session of project two");

    // Both projects have a queue under the same word, and that is the point: one shared room
    // meant project two's poster was in project one's audience, and every claim rang the wrong
    // session.
    let posted = a
        .call(
            "post",
            &json!({"tag": "ram", "body": "8 GB for project one"}),
        )
        .expect("a post in project one");
    assert!(posted.contains("#1"), "{posted}");
    let theirs = b
        .call(
            "post",
            &json!({"tag": "ram", "body": "2 GB for project two"}),
        )
        .expect("a post in project two");
    assert!(theirs.contains('#'), "{theirs}");

    // Each session is rung for the item it made - the baseline this test measures against, because
    // a poster hearing its own post is the room working, not a leak between rooms.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline
        && (rung_one.load(Ordering::SeqCst) == 0 || rung_two.load(Ordering::SeqCst) == 0)
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        rung_one.load(Ordering::SeqCst) >= 1 && rung_two.load(Ordering::SeqCst) >= 1,
        "each session is rung for the item it made"
    );
    let before_one = rung_one.load(Ordering::SeqCst);
    let before_two = rung_two.load(Ordering::SeqCst);
    wake::take("s-one-worker");
    wake::take("s-two-worker");

    // Project one's claim on its own `ram` - an event about project one's item, and the event
    // that would reach project two if the two shared one room.
    let claimed = a
        .call("claim", &json!({"tag": "ram"}))
        .expect("a claim in project one");
    assert!(claimed.contains("you hold #1"), "{claimed}");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && rung_one.load(Ordering::SeqCst) == before_one {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        rung_one.load(Ordering::SeqCst) > before_one,
        "project one's own session is rung for the claim on its item"
    );
    assert!(
        wake::waiting("s-one-worker") >= 1,
        "and the sentence the ring is about is in its queue"
    );

    // And the other project heard nothing - not a bell, and not a note. A `missed` count would
    // be wrong here too: the event was never addressed to this session, so there is no gap to report.
    assert_eq!(
        rung_two.load(Ordering::SeqCst),
        before_two,
        "project two's bell must not ring for project one's `ram`"
    );
    assert_eq!(
        wake::waiting("s-two-worker"),
        0,
        "and nothing is waiting in project two's queue either"
    );
    // The room itself agrees: the two `ram`s are two queues, each holding only its own project's
    // item - and each numbering its own from 1, which is why the count a `claim` answers with is a
    // number and not a sentence about somebody else's queue.
    let mine = a.call("queue", &json!({"tag": "ram"})).expect("a read");
    assert!(
        mine.contains("8 GB") && !mine.contains("2 GB"),
        "project one's queue is its own: {mine}"
    );
    assert!(mine.contains("#1"), "and numbers its own from 1: {mine}");
    let two_queue = b.call("queue", &json!({"tag": "ram"})).expect("a read");
    assert!(
        two_queue.contains("2 GB") && !two_queue.contains("8 GB"),
        "and project two's is its own: {two_queue}"
    );
    assert!(
        two_queue.contains("#1"),
        "numbered from 1 in its own room: {two_queue}"
    );
    server.stop();
}

/// The room holds what the tree holds: a subagent at depth 2 and the review queue's session
/// under `system`, neither of which ever opens a connection.
#[test]
fn a_subagent_at_depth_two_and_the_gatekeeper_are_members_of_the_same_room() {
    let dir = scratch("members");
    let socket = dir.join("room.sock");
    let server = Server::serve(&socket, letibot_room::Room::new()).expect("a socket");
    let project = letibot_room::project_key(&dir.display().to_string());
    let quiet: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});

    let tree = Tree(vec![
        member("s-a", "the habr article", Group::Top, None, 0),
        member("s-a-sub-1", "the rust rewrite", Group::Sub, Some("s-a"), 1),
        member(
            "s-a-sub-2",
            "the second look",
            Group::Sub,
            Some("s-a-sub-1"),
            2,
        ),
        member(
            "s-gatekeeper-aaaaaaaaaaaa",
            "review queue",
            Group::Gatekeeper,
            None,
            0,
        ),
    ]);
    let a = SessionRoom::connect(
        "s-a",
        &socket,
        &project,
        Some(Arc::new(tree)),
        Arc::clone(&quiet),
        None,
    )
    .expect("a session");

    // The link sends the tree on its own poll thread; wait for it rather than sleep a guess.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut members = Vec::new();
    while std::time::Instant::now() < deadline {
        members = server.members_of(&project);
        if members.len() == 4 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let found: Vec<(&str, Group, u32)> = members
        .iter()
        .map(|m| (m.session.as_str(), m.group, m.depth))
        .collect();
    assert_eq!(
        found,
        vec![
            ("s-a", Group::Top, 0),
            ("s-a-sub-1", Group::Sub, 1),
            ("s-a-sub-2", Group::Sub, 2),
            ("s-gatekeeper-aaaaaaaaaaaa", Group::Gatekeeper, 0),
        ],
        "the tree, in the tree's order"
    );
    // The name the tree draws the gatekeeper under - the operator's `<project>/<system>/…`.
    assert_eq!(members[3].where_in("letibot"), "letibot/system/gatekeepers");
    assert_eq!(members[2].where_in("letibot"), "letibot/the second look");
    // The pane's read carries the same list, so a pane could draw the tree.
    let snap =
        letibot_tools::builtins::room::RoomLink::call(&*a, "holding", &json!({"session": "s-a"}));
    assert!(snap.is_ok(), "the link is still answering: {snap:?}");
    // And the other project's room holds nobody: the roster was sent to the room this session is
    // in, and there is one room per project.
    let elsewhere = letibot_room::project_key(&dir.join("elsewhere").display().to_string());
    assert!(server.members_of(&elsewhere).is_empty());
    server.stop();
}
