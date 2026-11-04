//! The tree, as a function of six values - the grouping, pinned without a registry, a store or
//! a git. `crates/harnessd/tests/room_tree.rs` is the same rules through a real daemon; this is the
//! part a diff can be read against.

use super::*;

const PROJECT: &str = "/home/dead/Projects/letibot/letibot";

fn row<'a>(session: &'a str, title: &'a str, parent: Option<&'a str>) -> Row<'a> {
    Row {
        session,
        title,
        parent,
        workspace: PROJECT,
        origin: letibot_sessionlog::SessionOrigin::Operator,
        live: true,
    }
}

/// The four kinds of row the operator's sentence names, in one list: the project's own
/// sessions, a subagent at depth 2, and the review queue's session under `system`.
#[test]
fn the_project_s_own_sessions_their_subagents_and_its_system_agent() {
    let rows = vec![
        row("s-a", "the habr article", None),
        row("s-a-sub-1", "the rust rewrite", Some("s-a")),
        row("s-a-sub-2", "the second look", Some("s-a-sub-1")),
        row("s-gatekeeper-aaaaaaaaaaaa", "review queue", None),
    ];
    let members = members_of(&rows, &letibot_room::project_key(PROJECT), |_| {
        PROJECT.to_string()
    });
    let found: Vec<(&str, Group, u32)> = members
        .iter()
        .map(|m| (m.session.as_str(), m.group, m.depth))
        .collect();
    assert_eq!(
        found,
        vec![
            ("s-a", Group::Top, 0),
            ("s-a-sub-1", Group::Sub, 1),
            // Depth 2 - the row the tree draws one level further down, and the room holds it.
            ("s-a-sub-2", Group::Sub, 2),
            ("s-gatekeeper-aaaaaaaaaaaa", Group::Gatekeeper, 0),
        ],
        "the project's own sessions first, then `system`"
    );
    // The name the tree draws a gatekeeper under - `<project>/<system>/<gatekeepers>`.
    let gate = &members[3];
    assert_eq!(
        gate.where_in("letibot"),
        "letibot/system/gatekeepers",
        "a system agent hangs under `system`, not under the project"
    );
    assert_eq!(members[0].where_in("letibot"), "letibot");
}

/// Two projects do not leak into each other - the room's key is the project's, so the roster of
/// one holds none of the other's rows.
#[test]
fn a_row_of_another_project_is_not_in_this_one_s_roster() {
    let other = "/home/dead/Projects/stroppy-io/stroppy";
    let rows = vec![
        row("s-a", "here", None),
        Row {
            session: "s-b",
            title: "there",
            parent: None,
            workspace: other,
            origin: letibot_sessionlog::SessionOrigin::Operator,
            live: true,
        },
    ];
    let project_of = |w: &str| w.to_string();
    let here = members_of(&rows, &letibot_room::project_key(PROJECT), project_of);
    assert_eq!(here.len(), 1);
    assert_eq!(here[0].session, "s-a");
    let there = members_of(&rows, &letibot_room::project_key(other), project_of);
    assert_eq!(there.len(), 1);
    assert_eq!(there[0].session, "s-b");
}

/// A child hangs under the project of the ROOT of its chain, even when its own workspace is
/// somewhere else - a review clone under `/tmp`, a worktree beside the repository.
#[test]
fn a_child_of_another_workspace_is_placed_by_its_root() {
    let rows = vec![
        row("s-a", "the project", None),
        Row {
            session: "s-a-sub-1",
            title: "a review clone",
            parent: Some("s-a"),
            workspace: "/tmp/letibot-review-clones/review-7",
            origin: letibot_sessionlog::SessionOrigin::Queue,
            live: true,
        },
    ];
    // The clone is not in any repository, so its own workspace would be its own project - and the
    // ROOT is what says otherwise.
    let project_of = |w: &str| w.to_string();
    let members = members_of(&rows, &letibot_room::project_key(PROJECT), project_of);
    assert_eq!(
        members
            .iter()
            .map(|m| m.session.as_str())
            .collect::<Vec<_>>(),
        vec!["s-a", "s-a-sub-1"],
        "a child's own workspace is not its project"
    );
    // And a queue-origin child of the project is a REVIEWER, not a second gatekeeper.
    assert_eq!(members[1].group, Group::Reviewer);
}

/// A gatekeeper is `gatekeepers`, and everything under it is a `reviewer` - the order the tree
/// reads its marks in, which is what keeps one queue from being drawn as three.
#[test]
fn the_gatekeeper_is_the_queue_and_its_children_are_reviewers() {
    let rows = vec![
        row("s-gatekeeper-aaaaaaaaaaaa", "", None),
        row(
            "s-gatekeeper-aaaaaaaaaaaa-sub-1",
            "",
            Some("s-gatekeeper-aaaaaaaaaaaa"),
        ),
        row(
            "s-gatekeeper-aaaaaaaaaaaa-sub-2",
            "",
            Some("s-gatekeeper-aaaaaaaaaaaa-sub-1"),
        ),
    ];
    let members = members_of(&rows, &letibot_room::project_key(PROJECT), |_| {
        PROJECT.to_string()
    });
    let groups: Vec<Group> = members.iter().map(|m| m.group).collect();
    assert_eq!(
        groups,
        vec![Group::Gatekeeper, Group::Reviewer, Group::Reviewer],
        "a reviewer of a reviewer is still a reviewer"
    );
    // A row with no title is its folder and a short id.
    assert_eq!(members[0].label, "letibot …aaaaaaaa");
}

/// The notes keeper is its own group - it is a child of nobody, it repairs memory and it runs
/// nothing, so drawing it as a subagent would say it works for a session.
#[test]
fn the_notes_keeper_is_a_keeper_and_a_brief_is_a_reviewer() {
    let keeper = format!(
        "{} please repair the notes",
        letibot_sessionlog::NOTES_KEEPER_TITLE_PREFIX
    );
    let review = format!(
        "{} review this branch",
        letibot_sessionlog::GATEKEEPER_TITLE_PREFIX
    );
    let rows = vec![
        row("s-keeper-1", &keeper, None),
        row("s-review-1", &review, None),
    ];
    let members = members_of(&rows, &letibot_room::project_key(PROJECT), |_| {
        PROJECT.to_string()
    });
    let group = |session: &str| {
        members
            .iter()
            .find(|m| m.session == session)
            .map(|m| m.group)
            .unwrap_or_else(|| panic!("no member {session}"))
    };
    assert_eq!(group("s-keeper-1"), Group::Keeper);
    assert_eq!(group("s-review-1"), Group::Reviewer);
}

/// A folder outside any repository is its own project, so a session there has a room. Only an
/// empty or missing workspace is the box's room.
#[test]
fn a_workspace_outside_a_repository_is_its_own_project() {
    let dir = std::env::temp_dir().join(format!("letibot-roster-norepo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let roster = Roster::new(letibot_sessionlog::registry::Registry::new());
    let found = roster.project_of(&dir.display().to_string());
    assert_eq!(
        letibot_room::project_key(&found),
        letibot_room::project_key(&dir.display().to_string()),
        "a scratch folder keys its own room"
    );
    assert_eq!(roster.project_of(""), "");
    assert_eq!(roster.project_of("/definitely/not/here"), "");

    let at = dir.display().to_string();
    let rows = vec![Row {
        session: "s-a",
        title: "here",
        parent: None,
        workspace: &at,
        origin: letibot_sessionlog::SessionOrigin::Operator,
        live: true,
    }];
    let project_of = |w: &str| roster.project_of(w);
    let key = letibot_room::project_key(&at);
    assert_eq!(members_of(&rows, &key, project_of).len(), 1);
    assert!(members_of(&rows, "", project_of).is_empty());
    assert!(members_of(&rows, &letibot_room::project_key(PROJECT), project_of).is_empty());
}

/// A child, and a queue reviewer, join the project room of the chain's root, not their own temp
/// folder's. A top-level session in a plain folder is its own project.
#[test]
fn a_child_and_a_gatekeeper_join_their_roots_project_room() {
    let base = std::env::temp_dir().join(format!("letibot-roster-root-{}", std::process::id()));
    let project = base.join("proj");
    let clone = base.join("clone");
    std::fs::create_dir_all(&project).expect("a project");
    std::fs::create_dir_all(&clone).expect("a clone");
    let roster = Roster::new(letibot_sessionlog::registry::Registry::new());
    let want = letibot_room::project_key(&project.display().to_string());
    let top = roster.room_key_of("s-top", None, &project.display().to_string());
    assert_eq!(top, want, "a plain folder is its own project");
    // The child is not in the registry yet, so its parent's row is absent too: the own-workspace
    // answer is what a dangling chain gives, and the registry-backed chain is `project_of_session`.
    let child = roster.room_key_of("s-child", Some("s-top"), &clone.display().to_string());
    assert_ne!(child, "");
    let gate_id = letibot_sessionlog::gatekeeper_session(&project);
    let gate = roster.room_key_of(&gate_id, None, &clone.display().to_string());
    assert_eq!(gate, want, "a gatekeeper carries its project's key");
    let (ps, cs) = (project.display().to_string(), clone.display().to_string());
    let rows = vec![
        Row {
            session: "s-top",
            title: "",
            parent: None,
            workspace: &ps,
            origin: letibot_sessionlog::SessionOrigin::Operator,
            live: true,
        },
        Row {
            session: "s-child",
            title: "",
            parent: Some("s-top"),
            workspace: &cs,
            origin: letibot_sessionlog::SessionOrigin::Operator,
            live: true,
        },
    ];
    assert_eq!(
        letibot_room::project_key(&roster.project_of_session(&rows, "s-child")),
        want,
        "a child under a temp dir is in its root's room"
    );
}

/// A named top-level session is a member by the slug of its name. Unnamed, a subagent, a slug
/// that reads as an id, a bridge alias and a repeated slug are not.
#[test]
fn a_named_top_level_session_is_a_member_by_its_slug() {
    let rows = vec![
        row("s-a", "Stroppy Conformity", None),
        row("s-b", "stroppy-conformity", None),
        row("s-c", "", None),
        row("s-d", "s-looks-like-an-id", None),
        row("s-e", "claude code", None),
        row("s-f", "claude-code", None),
        row("s-a-sub", "the rust rewrite", Some("s-a")),
    ];
    let members = members_of(&rows, &letibot_room::project_key(PROJECT), |_| {
        PROJECT.to_string()
    });
    let handle = |id: &str| {
        members
            .iter()
            .find(|m| m.session == id)
            .map(|m| m.handle.as_str())
            .unwrap_or("?")
    };
    assert_eq!(handle("s-a"), "stroppy-conformity");
    assert_eq!(
        handle("s-b"),
        "",
        "the later of two equal slugs keeps its id"
    );
    assert_eq!(handle("s-c"), "");
    assert_eq!(handle("s-d"), "");
    assert_eq!(handle("s-e"), "", "`claude-code` is a bridge alias");
    assert_eq!(handle("s-f"), "");
    assert_eq!(handle("s-a-sub"), "", "a subagent is not addressed by name");
}
