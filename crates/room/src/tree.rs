//! **A room is a project's, and a project's name is the tree's** — the derivation, and the
//! vocabulary a room's membership is made of.
//!
//! The operator, 2026-10-31: *"yeah i want a room (it is actually in the markdowns). the herarchy
//! was `<project>/<top-sessions>/<subagents>*|system/`"*. The tree that sentence draws already
//! exists — `crates/tui/src/app/session.rs`'s `App::placements()` and the doc comment at line 327 —
//! and this module is the same hierarchy in the room's own terms: **which room a session is in, and
//! who else is in it.**
//!
//! # The key is not a new hash
//!
//! [`project_key`] answers **the twelve hex characters the rest of the tree already names a folder
//! by**: `letibot_sessionlog::workspace_key`'s rule, which is `sha256sum | cut -c1-12` in
//! `scripts/letibot` and is what `s-gatekeeper-<key>` is spelled from. A second hash of the same
//! path would be a second chance to disagree about which folder is which, and this tree has paid
//! for that once already (`sessionlog`'s own doc says so). The room crate cannot depend on
//! `letibot-sessionlog` — its whole design is that it holds no store and no `tokencore` — so the
//! rule is written here a second time and **pinned by a test in a crate that has both**
//! (`crates/harnessd/tests/room_tree.rs`), which is exactly how `scripts/letibot`'s bash copy of
//! the host key is held to `lease::host_key_of`.
//!
//! # A room's name, and why it is derived
//!
//! [`room_name`] is `<host key>/<project key>` — the box and the project, in that order, because
//! both have to be in it: two boxes can serve the same repository path (a clone, a backup) and two
//! projects can live on one box. A client that names a room is held to it (`Server`'s `Hello`
//! check), and [`room_of_name`] is the inverse — with the round trip, so a name this function did
//! not write is not a room. That is `tag_session`/`tag_of_session`'s rule one level up, and the
//! reason is the same: a name two readers derive is a name neither may invent.
//!
//! **The default project is the empty key**, and its room is the bare host key — so a session that
//! belongs to no project (a one-shot, a fixture, the coordinator's own arbiter) still has a room,
//! and `Client::connect`'s existing spelling of the box's name still reaches it.

use std::fmt;

/// **The project a session with no project is in** — the box's own room.
///
/// Not a name a person types: it is what a `Hello` carries when the daemon could not place the
/// session (a one-shot, a fixture, a workspace outside any repository), and it is the room the
/// box's arbiter and a head's own `room` tool have always reached.
pub const DEFAULT_PROJECT: &str = "";

/// How many hex characters a project key is — `workspace_key`'s twelve, which is what
/// `scripts/letibot` cuts and what `s-gatekeeper-<key>` carries.
pub const PROJECT_KEY_LEN: usize = 12;

/// **The project a workspace root belongs to, as the key the rest of the tree already uses.**
///
/// The path is canonicalised first, because a daemon is started by a shell (`cd … && pwd -P`) and
/// a head may name the same folder by any path that reaches it — `/var/folders` and
/// `/private/var/folders` are one directory and two strings, and two strings are two rooms.
///
/// A path that cannot be canonicalised is hashed as given: the key is then stable for that
/// spelling, which is the most a process can say about a directory it cannot reach.
pub fn project_key(root: &str) -> String {
    use sha2::Digest;
    let path = std::fs::canonicalize(root).unwrap_or_else(|_| std::path::PathBuf::from(root));
    let mut h = sha2::Sha256::new();
    h.update(path.as_os_str().as_encoded_bytes());
    let digest = h.finalize();
    digest
        .iter()
        .take(PROJECT_KEY_LEN / 2)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// **The room a project is** — `<host key>/<project key>`, or the bare host key for
/// [`DEFAULT_PROJECT`].
///
/// Both halves are needed. The host key is what tells two boxes apart (two machines that both
/// answer `localhost`, or one repository cloned at one path on two of them); the project key is
/// what tells two projects on one box apart. A room's name is therefore the pair, and
/// [`room_of_name`] reads it back.
pub fn room_name(host_key: &str, project: &str) -> String {
    let host = host_key.trim();
    let project = project.trim();
    if project.is_empty() {
        return host.to_string();
    }
    format!("{host}/{project}")
}

/// **The project a room's name belongs to**, or `None` — the inverse of [`room_name`], with the
/// round trip.
///
/// The round trip is what makes this exact rather than a guess: a name that merely starts with
/// this box's key (`lab2x1-e666427d/../etc`) does not derive back to itself, so it is not a room
/// this function wrote. The answer is `Some("")` for the bare host key — the default project is a
/// project, not an absence.
pub fn room_of_name(host_key: &str, name: &str) -> Option<String> {
    let host = host_key.trim();
    let name = name.trim();
    let project = match name.strip_prefix(host) {
        Some("") => DEFAULT_PROJECT.to_string(),
        Some(rest) => rest.strip_prefix('/')?.to_string(),
        None => return None,
    };
    // **A project key has one shape**, and it is [`project_key`]'s: twelve hex characters. A name
    // whose project part is anything else was written by hand, and a hand-written name that merely
    // starts with the box's key (`…/../etc`) must not become a room — the round trip and the shape
    // are two halves of the same check.
    let shaped = project.len() == PROJECT_KEY_LEN && project.bytes().all(|b| b.is_ascii_hexdigit());
    (shaped || project.is_empty()).then_some(project)
        .filter(|p| room_name(host, p) == name)
}

/// **Where a member sits in the tree** — the four kinds of row the operator's sentence names, plus
/// the three groups under `system`.
///
/// The vocabulary is the tree's (`crates/tui/src/app/session.rs`'s `Node` and `SystemKind`), kept
/// here as one enum because the room has to be able to say *who is in it* in the same words a head
/// draws it in. The room does not decide which group a session is: the daemon does, from the marks
/// on the row (`sessionlog`'s own rules — the gatekeeper's id, a queue origin, a brief's title),
/// and it says so with this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// A session nobody spawned: a top-level session of the project.
    Top,
    /// A subagent, at any depth.
    Sub,
    /// The review queue's own session, one per project (`s-gatekeeper-<key>`).
    Gatekeeper,
    /// A review's own child, and anything it spawned.
    Reviewer,
    /// The notes keeper, which repairs memory and runs nothing.
    Keeper,
}

impl Group {
    pub const ALL: [Group; 5] = [
        Group::Top,
        Group::Sub,
        Group::Gatekeeper,
        Group::Reviewer,
        Group::Keeper,
    ];

    /// **The name the tree draws the row under**, which is also what the wire carries — one
    /// spelling, so a pane and the room cannot disagree about what a group is called.
    pub fn as_str(self) -> &'static str {
        match self {
            Group::Top => "top",
            Group::Sub => "sub",
            Group::Gatekeeper => "gatekeepers",
            Group::Reviewer => "reviewers",
            Group::Keeper => "keepers",
        }
    }

    /// The group from the wire's word, or `None` for a word this build does not know.
    pub fn from_wire(word: &str) -> Option<Group> {
        Group::ALL.into_iter().find(|g| g.as_str() == word)
    }

    /// **Is this row one of the harness's own agents?** — the three groups under `system`, which
    /// is the branch the operator's sentence draws beside the project's own sessions.
    pub fn is_system(self) -> bool {
        matches!(self, Group::Gatekeeper | Group::Reviewer | Group::Keeper)
    }

    /// The path segment this group's rows hang under, from the project down.
    pub fn segment(self) -> &'static str {
        match self {
            Group::Top | Group::Sub => "",
            Group::Gatekeeper => "system/gatekeepers",
            Group::Reviewer => "system/reviewers",
            Group::Keeper => "system/keepers",
        }
    }
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// **One session the room holds** — who, drawn where.
///
/// The room's membership is this list, and it is the TREE's: a project's top-level sessions, their
/// subagents at any depth, and the `system` agents that serve them. It is not the list of
/// connections — a subagent never opens one (the room's door refuses it, `docs/room.md` § *The
/// wire*) and is a member all the same, which is the whole difference this type draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The session's id — what a `hand` names, and what `task_message` takes.
    pub session: String,
    /// **What the tree writes on the row**: the session's title, shortened by the daemon if it is
    /// long. A member is recognised by this and addressed by `session`.
    pub label: String,
    pub group: Group,
    /// The parent it hangs from, for the rows that hang from one.
    pub parent: Option<String>,
    /// How far under the project's top level it sits: 0 for a top-level session, 1 for its child,
    /// 2 for its child's child.
    pub depth: u32,
    /// Whether the session is live in the daemon that sent this row, or only on disk. The tree
    /// draws both (`stored` in the picker); a room that forgot the dead would forget the row a
    /// person is looking for.
    pub live: bool,
}

impl Member {
    /// **The path the tree draws this row at**, under the project's own name —
    /// `letibot/agent-x` for a top session's subagent, `letibot/system/gatekeepers` for the
    /// review queue's.
    ///
    /// A system agent's path stops at its group: the group's line is what a reader folds, and a
    /// member that repeated it would be a second copy of the tree's own words.
    pub fn where_in(&self, project: &str) -> String {
        let project = project.trim_end_matches('/');
        if self.group.is_system() {
            return format!("{project}/{}", self.group.segment());
        }
        if self.depth == 0 {
            return project.to_string();
        }
        format!("{project}/{}", self.label)
    }
}

/// **The room a set of rows belongs to** — every row that names the same project, in the order the
/// tree draws them: the project's own sessions first, then `system`, each by depth and then by
/// label.
///
/// A function rather than a sort at each reader, because two readers that sort differently are two
/// trees: the room's membership and a pane's drawing must be one list (`App::placements`' argument
/// one crate over).
pub fn tree_order(members: &mut [Member]) {
    members.sort_by(|a, b| {
        (a.group.is_system(), a.depth, &a.label, &a.session).cmp(&(
            b.group.is_system(),
            b.depth,
            &b.label,
            &b.session,
        ))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "lab2x1-e666427d";

    /// **A room's name runs both ways.** Two readers derive it — the daemon that opens the
    /// connection and the coordinator that holds the room — and a name either of them guessed
    /// would be a room the other cannot find.
    #[test]
    fn a_room_is_the_host_and_the_project_and_the_name_derives_back() {
        let project = project_key("/home/dead/Projects/letibot/letibot");
        assert_eq!(project.len(), PROJECT_KEY_LEN, "{project}");
        assert!(
            project.bytes().all(|b| b.is_ascii_hexdigit()),
            "a key is hex: {project}"
        );
        // The same folder is the same room, whatever path reaches it.
        assert_eq!(
            project,
            project_key("/home/dead/Projects/letibot/letibot/"),
            "a trailing slash is one folder, not two"
        );
        let name = room_name(KEY, &project);
        assert_eq!(name, format!("{KEY}/{project}"));
        assert_eq!(room_of_name(KEY, &name).as_deref(), Some(project.as_str()));
    }

    /// **Two projects are two rooms, and two boxes are two rooms of the same project** — the two
    /// leaks the pair exists to stop.
    #[test]
    fn two_projects_and_two_boxes_are_four_rooms() {
        let one = room_name(KEY, "aaaaaaaaaaaa");
        let two = room_name(KEY, "bbbbbbbbbbbb");
        assert_ne!(one, two, "two projects on one box share no room");
        assert_eq!(
            room_of_name(KEY, &two).as_deref(),
            Some("bbbbbbbbbbbb"),
            "the other project's room is not this project's"
        );
        assert_eq!(
            room_of_name("localhost-5eed1234", &one),
            None,
            "another box's name is not a room here"
        );
        // And the same repository path on two boxes is two rooms, because the host key is in the
        // name — the two machines that both answer `localhost` are the case this is for.
        assert_ne!(
            room_name("localhost-0badf00d", "aaaaaaaaaaaa"),
            room_name("localhost-5eed1234", "aaaaaaaaaaaa")
        );
    }

    /// **A name this function did not write is not a room.** The round trip is the whole check:
    /// a hand-written name that merely starts with the box's key is somebody else's.
    #[test]
    fn a_name_that_does_not_derive_back_is_not_a_room() {
        assert_eq!(room_of_name(KEY, KEY), Some(DEFAULT_PROJECT.to_string()));
        assert_eq!(room_of_name(KEY, ""), None);
        assert_eq!(room_of_name(KEY, "elsewhere"), None);
        // A trailing slash is not the default project's name: `room_name` never writes one, so the
        // round trip refuses it rather than folding it into a room it would then be wrong about.
        assert_eq!(room_of_name(KEY, &format!("{KEY}/")), None);
        assert_eq!(room_of_name(KEY, &format!("{KEY}//x")), None);
        assert_eq!(room_of_name(KEY, &format!("{KEY}x/y")), None);
        // The default project's room is the bare key, and its name is the box's — so a client that
        // spells the box's name still reaches a room.
        assert_eq!(room_name(KEY, DEFAULT_PROJECT), KEY);
    }

    /// **The tree's path is the group's, and a system agent hangs under `system`.** The operator's
    /// sentence, as a string.
    #[test]
    fn a_member_is_drawn_where_the_tree_puts_it() {
        let top = Member {
            session: "s-a".into(),
            label: "the habr article".into(),
            group: Group::Top,
            parent: None,
            depth: 0,
            live: true,
        };
        let sub = Member {
            session: "s-a-sub-1".into(),
            label: "the rust rewrite".into(),
            group: Group::Sub,
            parent: Some("s-a".into()),
            depth: 1,
            live: true,
        };
        let gate = Member {
            session: "s-gatekeeper-aaaaaaaaaaaa".into(),
            label: "review queue".into(),
            group: Group::Gatekeeper,
            parent: None,
            depth: 0,
            live: true,
        };
        assert_eq!(top.where_in("letibot"), "letibot");
        assert_eq!(sub.where_in("letibot"), "letibot/the rust rewrite");
        assert_eq!(gate.where_in("letibot"), "letibot/system/gatekeepers");
        assert!(gate.group.is_system());
        assert!(!top.group.is_system());
        // The wire's word for a group is the tree's, and it reads back.
        for g in Group::ALL {
            assert_eq!(Group::from_wire(g.as_str()), Some(g));
        }
        assert_eq!(Group::from_wire("subagents"), None);
    }

    /// **The tree's order puts the project's own sessions before `system`, and a parent before its
    /// children** — one list, so a room's membership and a pane's drawing cannot be two orders.
    #[test]
    fn the_tree_order_is_the_trees() {
        let mk = |session: &str, label: &str, group: Group, depth: u32| Member {
            session: session.into(),
            label: label.into(),
            group,
            parent: None,
            depth,
            live: true,
        };
        let mut rows = vec![
            mk("s-gatekeeper-aaaaaaaaaaaa", "review queue", Group::Gatekeeper, 0),
            mk("s-a-sub-2", "grandchild", Group::Sub, 2),
            mk("s-a-sub-1", "child", Group::Sub, 1),
            mk("s-a", "top", Group::Top, 0),
        ];
        tree_order(&mut rows);
        let order: Vec<&str> = rows.iter().map(|m| m.session.as_str()).collect();
        assert_eq!(
            order,
            vec!["s-a", "s-a-sub-1", "s-a-sub-2", "s-gatekeeper-aaaaaaaaaaaa"]
        );
    }
}
