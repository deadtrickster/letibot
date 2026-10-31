//! **A room is a project's, and its membership is the tree** — the operator's hierarchy,
//! `<project>/<top-sessions>/<subagents>*|system/`, as the room holds it.
//!
//! # What this file exists to have caught
//!
//! Until this slice there was one room per BOX. Two unrelated sessions could not find each other
//! (nothing in the tree will name a session), and two projects on one machine shared one set of
//! tags — project B's `ram` was refused `Busy` by project A's claim, and the refusal named a
//! session nobody in B had ever heard of. The room also held only the sessions *connected* to it,
//! and a subagent never connects: the door refuses it (`docs/room.md` § *The wire*), so the one
//! kind of session the operator's sentence names twice was the one kind the room could not see.
//!
//! So there are three facts here, and each was false before: a session reaches its project's room
//! **without naming an id**; a subagent at depth 2 is a member of that same room; and a session in
//! another project gets another room and never hears the first.
//!
//! # What it needs
//!
//! Nothing on the box: a `Server` on a socket in a directory of the test's own, and
//! `SessionRoom::connect` — the same two objects a daemon uses. No store, no model, no turn, and no
//! coordinator process, so this cannot touch `~/.local/share/letibot` or the operator's room.

use std::sync::Arc;

use letibot_harnessd::room::link::SessionRoom;
use letibot_harnessd::room::roster::RosterSource;
use letibot_room::{Group, Member, Server};
use serde_json::json;

/// A directory of this test's own, under the temp dir rather than a shared path.
fn scratch(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("letibot-roomtree-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory of this test's own");
    dir
}

/// **The key is the tree's own** — the twelve hex characters `scripts/letibot` cuts,
/// `letibot_sessionlog::workspace_key` computes and `s-gatekeeper-<key>` is spelled from.
///
/// This is the pin that lets the room crate keep its own copy of the rule: two spellings, one
/// table, exactly as `lease::host_key_of` is pinned to the launcher's bash.
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
        // **And the gatekeeper of that project is named by the same key** — which is what makes a
        // `system/gatekeepers` row recognisable as *this* project's and no other's.
        let gate = letibot_sessionlog::gatekeeper_session(std::path::Path::new(&root));
        assert!(
            gate.ends_with(&letibot_room::project_key(&root)),
            "{gate} must carry the project's key"
        );
    }
}

/// A tree the room is told about, standing in for `Registry::list()` — the grouping itself is
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

/// **Two projects on one socket are two rooms, and neither hears the other.**
///
/// The tag is the same word in both (`ram`), which is the whole point: before this slice it was one
/// lock, so B's claim was refused by A's and B could not tell whose queue it had walked into.
#[test]
fn two_projects_are_two_rooms_and_a_tag_is_not_shared_between_them() {
    let dir = scratch("projects");
    let socket = dir.join("room.sock");
    let server = Server::serve(&socket, letibot_room::Room::new()).expect("a socket");

    let one = letibot_room::project_key(&dir.join("one").display().to_string());
    let two = letibot_room::project_key(&dir.join("two").display().to_string());
    let quiet: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});

    let a = SessionRoom::connect(
        "s-a",
        &socket,
        &one,
        None,
        Arc::clone(&quiet),
        None,
    )
    .expect("a session of project one");
    let b = SessionRoom::connect(
        "s-b",
        &socket,
        &two,
        None,
        Arc::clone(&quiet),
        None,
    )
    .expect("a session of project two");
    // **Neither session named a room, a socket or an id** — the derivation did it, from the project
    // the daemon placed it in.
    assert_eq!(a.project(), one);
    assert_eq!(b.project(), two);
    assert_ne!(a.project(), b.project());

    let posted = a
        .call("post", &json!({"tag": "ram", "body": "8 GB"}))
        .expect("a post in project one");
    assert!(posted.contains("#1"), "{posted}");

    // **Project two's `ram` is empty** — a tag is a project's, not the box's.
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

    // **The pane's read is the room's**, and each session is told which room it reached.
    let one_room = server.room_name_of(&one);
    let two_room = server.room_name_of(&two);
    assert!(one_room.ends_with(&one) && two_room.ends_with(&two));
    assert_ne!(one_room, two_room);
    assert_eq!(server.items_in(&one).len(), 1);
    assert_eq!(server.items_in(&two).len(), 1);
    assert_eq!(server.projects().len(), 3, "the box's own room, plus two");
    server.stop();
}

/// **The room holds what the tree holds**: a subagent at depth 2 and the review queue's session
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
        member("s-a-sub-2", "the second look", Group::Sub, Some("s-a-sub-1"), 2),
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
    // **The name the tree draws the gatekeeper under** — the operator's `<project>/<system>/…`.
    assert_eq!(
        members[3].where_in("letibot"),
        "letibot/system/gatekeepers"
    );
    assert_eq!(members[2].where_in("letibot"), "letibot/the second look");
    // The pane's read carries the same list, so a pane could draw the tree.
    let snap = letibot_tools::builtins::room::RoomLink::call(
        &*a,
        "holding",
        &json!({"session": "s-a"}),
    );
    assert!(snap.is_ok(), "the link is still answering: {snap:?}");
    // And the other project's room holds nobody: the roster was sent to the room this session is
    // in, and there is one room per project.
    let elsewhere = letibot_room::project_key(&dir.join("elsewhere").display().to_string());
    assert!(server.members_of(&elsewhere).is_empty());
    server.stop();
}
