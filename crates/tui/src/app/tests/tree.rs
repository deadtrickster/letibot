//! **`/sessions` is a tree**: a project's line, the sessions nobody spawned in it, their
//! sub-sessions at any depth, and one `system` branch per project holding the harness's own agents.
//!
//! The operator, 2026-10-31: *"i want /sessions to show me the tree - top level nodes are projects,
//! then next level are top-level sessions for that projects, then their subsessions for worktrees
//! etc. gatekeepers and reviewers and all other system agents are under
//! `<project>/<system>/<gatekeepers>`"*.
//!
//! What is asserted here is the head's half — [`App::session_rows`], which is the ONE enumeration
//! the drawing, the arrows, Enter, a click and `/switch N` all read. The rule for which project a
//! workspace belongs to is [`App::placements`]' and `project_of`'s, and the tests below pin the two
//! shapes that make it more than a string split: a worktree is not a project, and a review clone is
//! not one either.

use super::*;
use letibot_sessionlog::SessionOrigin;

/// A session with a workspace of its own — every test here is about where a row is DRAWN, and the
/// workspace is what decides a project.
fn in_tree(id: &str, title: &str, workspace: &str) -> SessionBrief {
    let mut b = brief(id, title, false);
    b.wiring.workspace = workspace.into();
    b
}

fn child(id: &str, title: &str, parent: &str, workspace: &str) -> SessionBrief {
    let mut b = in_tree(id, title, workspace);
    b.parent_session_id = Some(parent.into());
    b
}

/// The review queue's own id, as a daemon's is: `s-gatekeeper-<12 hex>`.
fn queue() -> String {
    letibot_sessionlog::gatekeeper_session_for_key("0123456789ab")
}

const LETIBOT: &str = "/home/dead/Projects/letibot/letibot";
const RANO: &str = "/home/dead/Projects/rano/rano";
const WORKTREE: &str = "/home/dead/Projects/letibot/worktrees/agent-read-it";

/// **The daemon the tests read** — two projects, a tree in one of them, and the queue's own two
/// rows plus a keeper.
///
/// The reviewer's workspace is a **review clone under `/tmp`**, which is what a host-placed review
/// really gets (`Config::review_clones`): if a row were placed by its own workspace it would open a
/// project called `review-m-1-1`, and the whole point of the `system` branch is that it does not.
fn a_daemon_with_two_projects() -> App {
    let q = queue();
    let mut reviewer = in_tree(
        &format!("{q}-sub-1791759365511364772"),
        "You are the gatekeeper. review it",
        "/tmp/letibot-review-clones/review-m-1-2-3",
    );
    reviewer.parent_session_id = Some(q.clone());
    let mut gatekeeper = in_tree(&q, "the review queue", LETIBOT);
    gatekeeper.origin = SessionOrigin::Queue;
    let keeper = child(
        "s-keeper",
        "You are the notes keeper. tidy the notes",
        "s-habr",
        LETIBOT,
    );
    let mut a = app();
    a.home = "/home/dead".into();
    a.apply(hello(
        "s-habr",
        vec![
            in_tree("s-habr", "the habr article", LETIBOT),
            child("s-sub-1", "read the repo", "s-habr", WORKTREE),
            // A grandchild whose own workspace is inside its parent's worktree: the chain is what
            // says where it belongs, and the path would say something else.
            child(
                "s-sub-2",
                "and again",
                "s-sub-1",
                &format!("{WORKTREE}/crates"),
            ),
            in_tree("s-rano", "the rano rewrite", RANO),
            gatekeeper,
            reviewer,
            keeper,
        ],
        Hub::new("s-habr").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    a
}

/// Every row as a person reads it: a node's line in `[brackets]`, a session by its title.
fn rows(a: &App) -> Vec<String> {
    a.session_rows()
        .iter()
        .map(|r| match r.idx {
            Some(i) => a.sessions[i].title.clone(),
            None => format!("[{}]", r.node.title()),
        })
        .collect()
}

/// The project lines, as the tree draws them — the top level and nothing else.
fn projects(a: &App) -> Vec<String> {
    a.session_rows()
        .iter()
        .filter(|r| r.idx.is_none() && r.depth == 0)
        .map(|r| r.node.title())
        .collect()
}

/// The row of a session by its id, or a panic naming what was drawn instead.
fn row_of(a: &App, id: &str) -> usize {
    a.session_rows()
        .iter()
        .position(|r| r.idx.is_some_and(|i| a.sessions[i].session_id == id))
        .unwrap_or_else(|| panic!("`{id}` is drawn: {:?}", rows(a)))
}

/// **A project's line per workspace root** — two checkouts, two lines, and no line for the folder
/// above them.
#[test]
fn a_project_line_per_workspace_root() {
    let mut a = a_daemon_with_two_projects();
    assert_eq!(
        projects(&a),
        vec!["letibot (6)", "rano (1)"],
        "one line per project, and the count is every session it holds"
    );
    // **By NAME, not by path** — the operator's own words, and the reason a project's line is
    // readable at 80 columns.
    let screen = a.screen(100, 48).join("\n");
    assert!(screen.contains("letibot (6)"), "{screen}");
    assert!(
        !screen.contains("/home/dead/Projects/letibot/letibot"),
        "the project's line is its name, not its path:\n{screen}"
    );
}

/// **A top-level session is drawn under its project**, one step in.
#[test]
fn a_top_level_session_is_drawn_under_its_project() {
    let a = a_daemon_with_two_projects();
    let habr = row_of(&a, "s-habr");
    let row = &a.session_rows()[habr];
    assert_eq!(row.depth, 1, "one step under its project");
    assert_eq!(row.open, Some(false), "its children are folded away");
    // And the project's line is the row above it.
    assert!(
        a.session_rows()[habr - 1].idx.is_none(),
        "the project's line"
    );
    // A package's conversation is a top-level session like any other: the tree does not ask who
    // started it, only what it hangs from.
    let mut a = app();
    let mut package = in_tree("s-pkg", "make me a landing page", LETIBOT);
    package.origin = SessionOrigin::Package;
    a.apply(hello("s-pkg", vec![package], Hub::new("s-pkg").snapshot()));
    a.push_surface(Surface::Picker);
    assert_eq!(rows(&a), vec!["[letibot (1)]", "make me a landing page"]);
}

/// **A child sits under its parent, and a grandchild under the child** — the tree nests, and it
/// nests only as far as the operator has opened it.
#[test]
fn a_child_sits_under_its_parent_and_a_grandchild_under_it() {
    let mut a = a_daemon_with_two_projects();
    // Folded: the project's own rows, and the sub-sessions are not on the list at all.
    assert!(
        !rows(&a).contains(&"read the repo".to_string()),
        "{:?}",
        rows(&a)
    );
    a.set_row_fold("s-habr", true);
    let child = row_of(&a, "s-sub-1");
    assert_eq!(
        a.session_rows()[child].depth,
        2,
        "under its parent, one step in"
    );
    assert!(
        !rows(&a).contains(&"and again".to_string()),
        "the grandchild is folded under its own parent: {:?}",
        rows(&a)
    );
    a.set_row_fold("s-sub-1", true);
    let deep = row_of(&a, "s-sub-2");
    assert_eq!(a.session_rows()[deep].depth, 3, "and one step further in");
    assert_eq!(
        deep,
        child + 1,
        "the grandchild is drawn directly under the row that spawned it"
    );
}

/// **A `task_start` child's row names the worktree it runs in** — the one fact that tells two
/// children of one parent apart, and it is the row's own second line.
#[test]
fn a_task_starts_child_names_the_worktree_it_runs_in() {
    let mut a = a_daemon_with_two_projects();
    a.set_row_fold("s-habr", true);
    let screen = a.screen(100, 48).join("\n");
    assert!(
        screen.contains("~/Projects/letibot/worktrees/agent-read-it"),
        "the child's row names its worktree:\n{screen}"
    );
}

/// **A worktree is a place a project keeps, not a project** — a session whose workspace is a
/// worktree is drawn under the project that owns it.
#[test]
fn a_worktree_groups_under_the_project_that_owns_it() {
    let a = a_daemon_with_two_projects();
    assert_eq!(
        projects(&a),
        vec!["letibot (6)", "rano (1)"],
        "no `agent-read-it` line: a worktree is not a project"
    );
    // The rule itself, in the three shapes a workspace really takes.
    for (workspace, project) in [
        // Beside the repository, in the folder named after it.
        (WORKTREE, LETIBOT),
        // Inside the repository, where a `task_start` puts it when the layout says nothing.
        (
            "/home/dead/Projects/letibot/letibot/.claude/worktrees/agent-x",
            LETIBOT,
        ),
        // An org folder of several repositories: one folder per repository under `worktrees`.
        (
            "/home/dead/Projects/stroppy-io/worktrees/stroppy/agent-x",
            "/home/dead/Projects/stroppy-io/stroppy",
        ),
        // And a checkout is its own project.
        (LETIBOT, LETIBOT),
    ] {
        assert_eq!(
            letibot_sessionlog_workspace(workspace),
            project,
            "`{workspace}` belongs to `{project}`"
        );
    }
}

/// The project a workspace belongs to, through the head's own rule — a thin alias so the table
/// above reads as a table.
fn letibot_sessionlog_workspace(workspace: &str) -> String {
    crate::app::session::project_of(workspace).key
}

/// **The gatekeeper and its reviewers are under `<project>/system/…`, and nowhere else.**
///
/// This is the row that used to be drawn in a `review queue` fold beside the operator's
/// conversations — the same session, in a place that said *the queue's* and not *which project's*.
#[test]
fn the_gatekeeper_and_the_reviewers_are_under_the_projects_system_branch() {
    let mut a = a_daemon_with_two_projects();
    let q = queue();
    assert_eq!(
        rows(&a),
        vec![
            "[letibot (6)]",
            "the habr article",
            "[system (3)]",
            "[gatekeepers (1)]",
            "[reviewers (1)]",
            "[keepers (1)]",
            "[rano (1)]",
            "the rano rewrite",
        ],
        "the tree, with every session's children and every group folded"
    );
    // **The rows themselves**: opened, and each in its own group and nowhere else.
    for group in ["gatekeepers", "reviewers", "keepers"] {
        a.set_row_fold(&format!("{LETIBOT}/system/{group}"), true);
    }
    let drawn = rows(&a);
    for (group, id) in [
        ("gatekeepers", q.clone()),
        ("reviewers", format!("{q}-sub-1791759365511364772")),
        ("keepers", "s-keeper".to_string()),
    ] {
        let at = row_of(&a, &id);
        let row = &a.session_rows()[at];
        assert_eq!(row.depth, 3, "`{id}` is one step inside its group");
        assert_eq!(
            drawn[at - 1],
            format!("[{group} (1)]"),
            "`{id}` sits under `{group}`"
        );
        // `at` is a ROW of the list and `a.sessions` is indexed by SESSION — the two are not
        // the same list, which is the whole defect this test exists to catch.
        let title = a.sessions[row.idx.expect("a session row")].title.clone();
        assert_eq!(
            drawn.iter().filter(|r| **r == title).count(),
            1,
            "and the row is drawn ONCE: {drawn:?}"
        );
    }
    // **The reviewer's workspace would have made a project of its own** — a `/tmp` review clone —
    // and it is not one: the chain it hangs from says where it belongs.
    assert!(
        !projects(&a).iter().any(|p| p.contains("review-")),
        "{:?}",
        projects(&a)
    );
    // And nothing of the system branch is a child of the operator's conversation.
    assert!(
        !a.session_rows()
            .iter()
            .any(|r| r.depth == 2 && r.node == crate::app::session::Node::Session),
        "a system row was drawn as a sub-session of the project's tree: {:?}",
        rows(&a)
    );
}

/// **The empty case**: one project, one session, no children — and no `system` line, because a
/// branch with no rows is a fold that teaches a reader to skip folds.
#[test]
fn one_project_one_session_and_no_system_line() {
    let mut a = app();
    a.apply(hello(
        "s-only",
        vec![in_tree("s-only", "the only one", LETIBOT)],
        Hub::new("s-only").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    assert_eq!(rows(&a), vec!["[letibot (1)]", "the only one"]);
    let screen = a.screen(100, 30).join("\n");
    assert!(
        !screen.contains("system"),
        "no empty system node:\n{screen}"
    );
    assert!(!screen.contains("gatekeepers"), "{screen}");
}

/// **The keeper's mark is ONE string**, and this is the only crate that can see both halves.
///
/// `letibot-tools` writes the brief (`letibot_tools::notes_keeper::TITLE_PREFIX`) and
/// `letibot-sessionlog` names the mark the tree reads
/// (`letibot_sessionlog::NOTES_KEEPER_TITLE_PREFIX`). The constant cannot be an alias, because
/// `tools` sits BELOW `sessionlog`; so the two spellings are held together here, in the one crate
/// that depends on both. A brief that began differently would draw a keeper as somebody's
/// conversation, under their project, with no `keepers` group to find it in.
#[test]
fn the_keepers_brief_and_the_trees_mark_are_one_string() {
    assert_eq!(
        letibot_sessionlog::NOTES_KEEPER_TITLE_PREFIX,
        letibot_tools::notes_keeper::TITLE_PREFIX,
        "the tree finds a keeper by its title — a brief cut from another sentence is a keeper \
         drawn as a conversation"
    );
}

/// **The fold glyph and the state work at every level** — a project, a session, `system` and a
/// group, each opened by the same key and drawn by the same record.
#[test]
fn the_fold_works_at_every_level() {
    let mut a = a_daemon_with_two_projects();
    // The project: closed by the operator, and its rows go.
    let at = a
        .session_rows()
        .iter()
        .position(|r| r.idx.is_none() && r.node.title() == "letibot (6)")
        .expect("the project's line");
    a.picker_sel = at;
    assert_eq!(a.key(Key::Left), None);
    assert_eq!(
        rows(&a),
        vec!["[letibot (6)]", "[rano (1)]", "the rano rewrite"],
        "a closed project draws its line and nothing else"
    );
    assert_eq!(
        a.session_rows()[at].open,
        Some(false),
        "and the glyph says so"
    );
    // Opened again by the same key, and the tree comes back.
    a.key(Key::Right);
    assert!(
        rows(&a).contains(&"the habr article".to_string()),
        "{:?}",
        rows(&a)
    );
    // `system` folds too, and it is a level of its own.
    let sys = a
        .session_rows()
        .iter()
        .position(|r| r.idx.is_none() && r.node.title() == "system (3)")
        .expect("the system line");
    a.picker_sel = sys;
    a.key(Key::Left);
    assert!(
        !rows(&a).iter().any(|r| r.starts_with("[gatekeepers")),
        "{:?}",
        rows(&a)
    );
}

/// **A click selects what it points at, and acts on a node's line** — the row under the pointer,
/// from the same enumeration the drawing wrote.
#[test]
fn a_click_selects_the_row_it_points_at() {
    let mut a = a_daemon_with_two_projects();
    let screen = a.screen(100, 48);
    let y = screen
        .iter()
        .position(|l| l.contains("system (3)"))
        .expect("the system line is drawn") as u16;
    assert!(
        !a.picker_folds
            .get(&format!("{LETIBOT}/system"))
            .copied()
            .unwrap_or(false)
    );
    a.key(Key::Click { x: 6, y });
    assert_eq!(
        a.picker_row().map(|r| r.id),
        Some(format!("{LETIBOT}/system")),
        "the click marked the row it landed on"
    );
    assert_eq!(
        a.picker_folds.get(&format!("{LETIBOT}/system")).copied(),
        Some(false),
        "and acted on it: a node's line is the one row a click folds"
    );
}

/// **Enter still switches to the session under the cursor**, at any depth — the tree changed where
/// a row is drawn, not what Enter does on it.
#[test]
fn enter_still_switches_to_the_row_under_the_cursor() {
    let mut a = a_daemon_with_two_projects();
    a.set_row_fold("s-habr", true);
    a.picker_sel = row_of(&a, "s-sub-1");
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
}

/// **A number is the n-th SESSION, not the n-th row** — a project's line and every group's line
/// take none, because a number is somewhere to go.
#[test]
fn a_number_is_the_nth_session_and_a_nodes_line_has_none() {
    let mut a = a_daemon_with_two_projects();
    a.set_row_fold(&format!("{LETIBOT}/system/gatekeepers"), true);
    let numbers: Vec<(String, Option<usize>)> = a
        .picker_rows()
        .iter()
        .map(|r| (r.id.clone(), r.number))
        .collect();
    assert_eq!(
        numbers[0].1, None,
        "the project's line is not a number: {numbers:?}"
    );
    assert_eq!(
        numbers[1],
        ("s-habr".to_string(), Some(1)),
        "the first session is 1, whatever is drawn above it"
    );
    // The gatekeeper is the fifth session drawn (habr, rano, queue, reviewer, keeper order is the
    // daemon's) — what matters is that the number exists and reaches it.
    let gatekeeper = a
        .picker_rows()
        .iter()
        .find(|r| r.id == queue())
        .and_then(|r| r.number)
        .expect("the gatekeeper's row is numbered");
    assert_eq!(
        a.session_at_number(gatekeeper),
        a.picker_rows()
            .iter()
            .find(|r| r.id == queue())
            .and_then(|r| r.session)
    );
}

/// **The bar says what Enter does on the row under the cursor** — `switches` on a session, and the
/// fold's own word on a node's line.
#[test]
fn the_bar_follows_the_cursor_onto_a_nodes_line() {
    let mut a = a_daemon_with_two_projects();
    a.picker_sel = row_of(&a, "s-habr");
    assert!(
        a.hint_bar(200).contains("enter switches"),
        "{}",
        a.hint_bar(200)
    );
    a.picker_sel = 0;
    assert!(
        a.hint_bar(200).contains("enter folds it away"),
        "an open project says what Enter does to it: {}",
        a.hint_bar(200)
    );
    a.set_row_fold(&format!("{LETIBOT}/system/gatekeepers"), true);
    let at = a
        .session_rows()
        .iter()
        .position(|r| r.idx.is_none() && r.node.title() == "gatekeepers (1)")
        .expect("the group's line");
    a.picker_sel = at;
    assert!(
        a.hint_bar(200).contains("enter folds it away"),
        "{}",
        a.hint_bar(200)
    );
}

/// **Two projects with the same name are told apart by their paths** — the one case the name
/// cannot do the job.
#[test]
fn two_projects_with_one_name_say_where_they_are() {
    let mut a = app();
    a.home = "/home/dead".into();
    a.apply(hello(
        "s-a",
        vec![
            in_tree("s-a", "one", "/home/dead/Projects/one/letibot"),
            in_tree("s-b", "two", "/home/dead/Projects/two/letibot"),
        ],
        Hub::new("s-a").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    assert_eq!(
        projects(&a),
        vec!["~/Projects/one/letibot (1)", "~/Projects/two/letibot (1)"],
        "two rows a reader can tell apart"
    );
}

/// **A session whose parent this daemon does not hold is a root of its project's tree** — an
/// orphan is not a subagent, and the row is still reachable.
#[test]
fn an_orphan_is_a_root_of_its_projects_tree() {
    let mut a = app();
    let mut orphan = in_tree("s-orphan", "whose child is this", LETIBOT);
    orphan.parent_session_id = Some("s-gone".into());
    a.apply(hello(
        "s-orphan",
        vec![orphan],
        Hub::new("s-orphan").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    assert_eq!(rows(&a), vec!["[letibot (1)]", "whose child is this"]);
    assert_eq!(a.session_rows()[1].depth, 1);
}
