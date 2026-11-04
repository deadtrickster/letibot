//! A room is a project's, and a project's name is the tree's: the derivation and the vocabulary of membership.
//! [`project_key`] repeats the twelve-hex folder rule because this crate holds no store; a test in a
//! crate that has both pins it. [`room_name`] is `<host key>/<project key>`; the default project is the empty key.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The project a session with no project is in - the box's own room. Not a name a person types: it is
/// what a `Hello` carries when the daemon could not place the session (a one-shot, a fixture, a
/// workspace outside any repository), and the room the box's arbiter has always reached.
pub const DEFAULT_PROJECT: &str = "";

/// How many hex characters a project key is - `workspace_key`'s twelve, which is what `scripts/letibot`
/// cuts and what `s-gatekeeper-<key>` carries.
pub const PROJECT_KEY_LEN: usize = 12;

/// The project a workspace root belongs to, as the key the rest of the tree already uses. The path is
/// canonicalised first, because `/var/folders` and `/private/var/folders` are one directory and two
/// strings, and two strings are two rooms; one that cannot be is hashed as given.
/// A label for each project folder: its own name, `~` for the home folder, and for two folders
/// of one name a suffix of the parent folder (then the key) so they stay apart. `rows` is
/// `(key, root)`. A key with no known root is labelled by its first eight characters.
pub fn project_labels(
    rows: &[(String, String)],
    home: &str,
) -> std::collections::BTreeMap<String, String> {
    let base = |key: &str, root: &str| -> String {
        let root = root.trim_end_matches('/');
        if root.is_empty() {
            return key.chars().take(8).collect();
        }
        if !home.is_empty() && root == home.trim_end_matches('/') {
            return "~".to_string();
        }
        std::path::Path::new(root)
            .file_name()
            .map_or_else(|| root.to_string(), |n| n.to_string_lossy().to_string())
    };
    let parent = |root: &str| -> String {
        std::path::Path::new(root.trim_end_matches('/'))
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    let mut out = std::collections::BTreeMap::new();
    for (key, root) in rows {
        let name = base(key, root);
        let same: Vec<&(String, String)> =
            rows.iter().filter(|(k, r)| base(k, r) == name).collect();
        let label = if same.len() < 2 {
            name
        } else {
            let p = parent(root);
            let twins = same.iter().filter(|(_, r)| parent(r) == p).count();
            if twins > 1 {
                format!("{name} ({})", key.chars().take(4).collect::<String>())
            } else if p.is_empty() {
                name
            } else {
                format!("{name} ({p})")
            }
        };
        out.insert(key.clone(), label);
    }
    out
}

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

/// The room a project is: `<host key>/<project key>`, or the bare host key for [`DEFAULT_PROJECT`].
/// Both halves are needed - the host key tells two boxes apart, the project key two projects on one box
/// - and [`room_of_name`] reads the pair back.
pub fn room_name(host_key: &str, project: &str) -> String {
    let host = host_key.trim();
    let project = project.trim();
    if project.is_empty() {
        return host.to_string();
    }
    format!("{host}/{project}")
}

/// The project a room's name belongs to, or `None` - the inverse of [`room_name`], with the round trip.
/// A name that merely starts with this box's key (`lab2x1-e666427d/../etc`) does not derive back to
/// itself, so it is not a room this function wrote. `Some("")` is the bare host key.
pub fn room_of_name(host_key: &str, name: &str) -> Option<String> {
    let host = host_key.trim();
    let name = name.trim();
    let project = match name.strip_prefix(host) {
        Some("") => DEFAULT_PROJECT.to_string(),
        Some(rest) => rest.strip_prefix('/')?.to_string(),
        None => return None,
    };
    // A project key has one shape - twelve hex characters. A name whose project part is anything else
    // was written by hand, and must not become a room: the round trip and the shape are two halves of
    // one check.
    (is_project_key(&project) || project.is_empty())
        .then_some(project)
        .filter(|p| room_name(host, p) == name)
}

/// Is this a project key? - twelve hex characters, or empty for the box's own room. One function because
/// two doors ask it: [`room_of_name`], and `Server::answer_frame`, which must not create a room for a key
/// nobody derived.
pub fn is_project_key(key: &str) -> bool {
    key.len() == PROJECT_KEY_LEN && key.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Where a member sits in the tree - the four kinds of row the tree draws, plus the three groups under
/// `system`. The vocabulary is the tree's, kept here as one enum because the room has to say *who is in
/// it* in the same words a head draws it in; the daemon decides which group a session is, not the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
// One spelling of a group's name, and it is [`Group::as_str`]'s: the wire goes through these two
// conversions rather than through `serde`'s renaming, because two spellings is how a pane and a room
// come to disagree about what `gatekeepers` is called.
#[serde(into = "String", try_from = "String")]
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

    /// The name the tree draws the row under, which is also what the wire carries - one spelling, so a
    /// pane and the room cannot disagree about what a group is called.
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

    /// Is this row one of the harness's own agents? - the three groups under `system`.
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

impl From<Group> for String {
    fn from(g: Group) -> String {
        g.as_str().to_string()
    }
}

impl TryFrom<String> for Group {
    type Error = String;

    fn try_from(word: String) -> Result<Group, String> {
        Group::from_wire(&word).ok_or_else(|| {
            format!(
                "`{word}` is not a group this build knows — the tree draws {}",
                Group::ALL
                    .iter()
                    .map(|g| g.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }
}

/// One session the room holds: who, drawn where. The membership is the TREE's - a project's top-level
/// sessions, their subagents at any depth, and the `system` agents that serve them - and not the
/// connections: a subagent never opens one and is a member all the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// The session's id - what a `hand` names, and what `task_message` takes.
    pub session: String,
    /// What the tree writes on the row: the session's title, shortened by the daemon if it is long. A
    /// member is recognised by this and addressed by `session`.
    pub label: String,
    /// The slug of the session's name, empty when it has none (or the slug would clash with an id
    /// or a reserved name). It is the name the room shows and takes for this member; `session`
    /// stays the identity.
    #[serde(default)]
    pub handle: String,
    pub group: Group,
    /// The parent it hangs from, for the rows that hang from one.
    pub parent: Option<String>,
    /// How far under the project's top level it sits: 0 for a top-level session, 1 for its child,
    /// 2 for its child's child.
    pub depth: u32,
    /// Whether the session is live in the daemon that sent this row, or only on disk. The tree draws
    /// both; a room that forgot the dead would forget the row a person is looking for.
    pub live: bool,
}

impl Member {
    /// The path the tree draws this row at, under the project's own name: `letibot/agent-x` for a top
    /// session's subagent, `letibot/system/gatekeepers` for the review queue's. A system agent's path
    /// stops at its group, because the group's line is what a reader folds.
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

/// The room a set of rows belongs to, in the order the tree draws them: the project's own sessions
/// first, then `system`, each by depth and then by label. A function rather than a sort at each reader,
/// because two readers that sort differently are two trees.
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

    #[test]
    fn a_project_is_labelled_by_its_folder_and_namesakes_by_their_parent() {
        let rows = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let l = project_labels(
            &rows(&[
                ("k1", "/home/d/Projects/rano"),
                ("k2", "/home/d"),
                ("k3", "/home/d/a/app"),
                ("k4", "/home/d/b/app"),
                ("k5", "/home/d/b/app2/../x"),
                ("k6", "/home/d/c/x"),
                ("k7", ""),
            ]),
            "/home/d",
        );
        assert_eq!(l["k1"], "rano");
        assert_eq!(l["k2"], "~");
        assert_eq!(l["k3"], "app (a)");
        assert_eq!(l["k4"], "app (b)");
        assert_eq!(l["k7"], "k7");
        assert_ne!(l["k5"], l["k6"]);
    }

    /// SAFETY: the directory is one this test makes, because a key is the resolved folder and a path the
    /// machine happens to have would make the answer a fact about the machine. The name itself runs both
    /// ways - two readers derive it, and a name either guessed would be a room the other cannot find.
    #[test]
    fn a_room_is_the_host_and_the_project_and_the_name_derives_back() {
        let root = std::env::temp_dir().join(format!("letibot-room-key-{}", std::process::id()));
        let folder = root.join("project");
        std::fs::create_dir_all(&folder).expect("a directory to be a project");
        let at = folder.display().to_string();

        let project = project_key(&at);
        assert_eq!(project.len(), PROJECT_KEY_LEN, "{project}");
        assert!(
            project.bytes().all(|b| b.is_ascii_hexdigit()),
            "a key is hex: {project}"
        );
        // The same folder is the same room, whatever path reaches it: a trailing slash, and a
        // path that walks away and back.
        assert_eq!(
            project,
            project_key(&format!("{at}/")),
            "a trailing slash is one folder, not two"
        );
        assert_eq!(
            project,
            project_key(&format!("{at}/../project")),
            "a path that resolves to this folder is this folder"
        );
        // And a second folder is a second room, so the key above is not a constant.
        let other = root.join("other");
        std::fs::create_dir_all(&other).expect("a second directory");
        assert_ne!(project, project_key(&other.display().to_string()));

        let name = room_name(KEY, &project);
        assert_eq!(name, format!("{KEY}/{project}"));
        assert_eq!(room_of_name(KEY, &name).as_deref(), Some(project.as_str()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two projects are two rooms, and two boxes are two rooms of the same project - the two leaks the
    /// pair exists to stop.
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
        // And the same repository path on two boxes is two rooms, because the host key is in the name -
        // the two machines that both answer `localhost` are the case this is for.
        assert_ne!(
            room_name("localhost-0badf00d", "aaaaaaaaaaaa"),
            room_name("localhost-5eed1234", "aaaaaaaaaaaa")
        );
    }

    /// A name this function did not write is not a room: the round trip is the whole check, and a
    /// hand-written name that merely starts with the box's key is somebody else's.
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
        // The default project's room is the bare key, and its name is the box's - so a client that
        // spells the box's name still reaches a room.
        assert_eq!(room_name(KEY, DEFAULT_PROJECT), KEY);
    }

    /// The tree's path is the group's, and a system agent hangs under `system`.
    #[test]
    fn a_member_is_drawn_where_the_tree_puts_it() {
        let top = Member {
            session: "s-a".into(),
            label: "the habr article".into(),
            handle: String::new(),
            group: Group::Top,
            parent: None,
            depth: 0,
            live: true,
        };
        let sub = Member {
            session: "s-a-sub-1".into(),
            label: "the rust rewrite".into(),
            handle: String::new(),
            group: Group::Sub,
            parent: Some("s-a".into()),
            depth: 1,
            live: true,
        };
        let gate = Member {
            session: "s-gatekeeper-aaaaaaaaaaaa".into(),
            label: "review queue".into(),
            handle: String::new(),
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

    /// The tree's order puts the project's own sessions before `system`, and a parent before its
    /// children - one list, so the room and a pane cannot be two orders.
    #[test]
    fn the_tree_order_is_the_trees() {
        let mk = |session: &str, label: &str, group: Group, depth: u32| Member {
            session: session.into(),
            label: label.into(),
            handle: String::new(),
            group,
            parent: None,
            depth,
            live: true,
        };
        let mut rows = vec![
            mk(
                "s-gatekeeper-aaaaaaaaaaaa",
                "review queue",
                Group::Gatekeeper,
                0,
            ),
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
