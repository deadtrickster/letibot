//! `/sessions` is a tree: a project's line, the sessions nobody spawned in it, their sub-sessions at
//! any depth, and one `system` branch per project holding the harness's own agents. What is asserted
//! is the head's half, [`App::session_rows`], the one enumeration every reader of the tree uses.

use super::*;
use letibot_sessionlog::SessionOrigin;

/// A session with a workspace of its own - every test here is about where a row is DRAWN, and the
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

/// The daemon the tests read: two projects, a tree in one of them, and the queue's own two rows plus
/// a keeper. The reviewer's workspace is a review clone under `/tmp`, what a host-placed review
/// really gets, so a row placed by its own workspace would open a project of its own.
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
            // Moved just now, so it sits in `other projects`.
            {
                let mut b = in_tree("s-rano", "the rano rewrite", RANO);
                b.moved_ms = letibot_sessionlog::registry::now_ms();
                b
            },
            gatekeeper,
            reviewer,
            keeper,
        ],
        Hub::new("s-habr").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    a
}

/// Every drawn row as a person reads it: a node's line in `[brackets]`, a session by its title.
/// The drawn list is the one the keys act on, so this is the screen and the enumeration at once.
fn rows(a: &App) -> Vec<String> {
    a.picker_rows()
        .iter()
        .map(|r| match r.session {
            Some(i) => a.sessions[i].title.clone(),
            None => format!("[{}]", node_text(a, &r.id)),
        })
        .collect()
}

/// A node's own words, off the line the list drew for it.
fn node_text(a: &App, id: &str) -> String {
    let (lines, spans) = a.picker_layout(100, 40);
    let rows = a.picker_rows();
    rows.iter()
        .position(|r| r.id == id)
        .and_then(|k| spans.get(k))
        .and_then(|s| lines.get(s.start))
        .map(|l| {
            // The fold column is the renderer's, and the right column is pushed to the edge behind
            // a run of spaces: a node's words are what is left once both are cut.
            let words = l
                .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '~' && c != '/')
                .trim();
            words.split("  ").next().unwrap_or_default().to_string()
        })
        .unwrap_or_default()
}

/// The project lines, as the tree draws them, with the two recency groups opened so every project
/// is on the list.
fn projects(a: &mut App) -> Vec<String> {
    a.set_row_fold("groups/recent", true);
    a.set_row_fold("groups/older", true);
    let out = a
        .picker_rows()
        .iter()
        // A project's key is a path; the `system` branch and its groups live under one, which is
        // why they are filtered out rather than matched by their words.
        .filter(|r| r.session.is_none() && r.id.starts_with('/') && !r.id.contains("/system"))
        .map(|r| node_text(a, &r.id))
        .collect();
    a.set_row_fold("groups/recent", false);
    a.set_row_fold("groups/older", false);
    out
}

/// The row of a session by its id, or a panic naming what was drawn instead.
fn row_of(a: &App, id: &str) -> usize {
    a.picker_rows()
        .iter()
        .position(|r| r.id == id)
        .unwrap_or_else(|| panic!("`{id}` is drawn: {:?}", rows(a)))
}

/// The depth a session's row is drawn at.
fn depth_of(a: &App, id: &str) -> usize {
    a.picker_rows()[row_of(a, id)].depth
}

/// Whether a row folds, and which way.
fn fold_of(a: &App, id: &str) -> Option<rano::agent::tree_list::Fold> {
    a.picker_rows()[row_of(a, id)].open
}

/// A project's line per workspace root - two checkouts, two lines, and no line for the folder
/// above them.
#[test]
fn a_project_line_per_workspace_root() {
    let mut a = a_daemon_with_two_projects();
    assert_eq!(
        projects(&mut a),
        vec!["letibot (6)", "rano (1)"],
        "one line per project, and the count is every session it holds"
    );
    // By NAME, not by path - the operator's own words, and the reason a project's line is
    // readable at 80 columns.
    let screen = a.screen(100, 48).join("\n");
    // The header names the session's folder, and how depends on $HOME, so only the line is read.
    let line = screen
        .lines()
        .find(|l| l.contains("letibot (6)"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(
        !line.contains("Projects/letibot/letibot"),
        "the project's line is its name, not its path:\n{screen}"
    );
}

/// A top-level session is drawn under its project, one step in.
#[test]
fn a_top_level_session_is_drawn_under_its_project() {
    let a = a_daemon_with_two_projects();
    let habr = row_of(&a, "s-habr");
    assert_eq!(depth_of(&a, "s-habr"), 1, "one step under its project");
    assert_eq!(
        fold_of(&a, "s-habr"),
        Some(rano::agent::tree_list::Fold::Closed),
        "its children are folded away"
    );
    // And the project's line is the row above it.
    assert!(
        a.picker_rows()[habr - 1].session.is_none(),
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

/// A child sits under its parent, and a grandchild under the child - the tree nests, and it
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
    assert_eq!(depth_of(&a, "s-sub-1"), 2, "under its parent, one step in");
    assert!(
        !rows(&a).contains(&"and again".to_string()),
        "the grandchild is folded under its own parent: {:?}",
        rows(&a)
    );
    a.set_row_fold("s-sub-1", true);
    let deep = row_of(&a, "s-sub-2");
    assert_eq!(depth_of(&a, "s-sub-2"), 3, "and one step further in");
    assert_eq!(
        deep,
        child + 1,
        "the grandchild is drawn directly under the row that spawned it"
    );
}

/// A `task_start` child's identity line names the worktree it runs in - the one fact that tells two
/// children of one parent apart - and it is drawn for the row under the cursor.
#[test]
fn a_task_starts_child_names_the_worktree_it_runs_in() {
    let mut a = a_daemon_with_two_projects();
    a.set_row_fold("s-habr", true);
    a.picker_sel = row_of(&a, "s-sub-1");
    let screen = a.screen(100, 48).join("\n");
    assert!(
        screen.contains("~/Projects/letibot/worktrees/agent-read-it"),
        "the child's row names its worktree:\n{screen}"
    );
}

/// A worktree is a place a project keeps, not a project - a session whose workspace is a
/// worktree is drawn under the project that owns it.
#[test]
fn a_worktree_groups_under_the_project_that_owns_it() {
    let mut a = a_daemon_with_two_projects();
    assert_eq!(
        projects(&mut a),
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

/// The project a workspace belongs to, through the head's own rule - a thin alias so the table
/// above reads as a table.
fn letibot_sessionlog_workspace(workspace: &str) -> String {
    crate::app::session::project_of(workspace).key
}

/// The gatekeeper and its reviewers are under `<project>/system/...`, and nowhere else: they are one
/// project's agents, and a `review queue` fold beside the operator's conversations said the queue's
/// and not which project's.
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
            "[other projects (1)]",
        ],
        "the tree, folded: the two project lines, the `system` branch's own line, and the one \
         session of the project this head is standing in"
    );
    assert_eq!(
        fold_of(&a, &format!("{LETIBOT}/system")),
        Some(rano::agent::tree_list::Fold::Closed),
        "`system` starts closed — a head in an ordinary conversation is not standing in it"
    );
    // The rows themselves: opened, and each in its own group and nowhere else.
    a.set_row_fold(&format!("{LETIBOT}/system"), true);
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
        assert_eq!(depth_of(&a, &id), 3, "`{id}` is one step inside its group");
        assert_eq!(
            drawn[at - 1],
            format!("[{group} (1)]"),
            "`{id}` sits under `{group}`"
        );
        // `at` is a ROW of the list and `a.sessions` is indexed by SESSION - the two are not
        // the same list, which is the whole defect this test exists to catch.
        let row = &a.picker_rows()[at];
        let title = a.sessions[row.session.expect("a session row")]
            .title
            .clone();
        assert_eq!(
            drawn.iter().filter(|r| **r == title).count(),
            1,
            "and the row is drawn ONCE: {drawn:?}"
        );
    }
    // The reviewer's workspace would have made a project of its own - a `/tmp` review clone  -
    // and it is not one: the chain it hangs from says where it belongs.
    assert!(
        !projects(&mut a).iter().any(|p| p.contains("review-")),
        "{:?}",
        projects(&mut a)
    );
    // And nothing of the system branch is a child of the operator's conversation.
    assert!(
        !a.picker_rows()
            .iter()
            .any(|r| r.depth == 2 && r.session.is_some()),
        "a system row was drawn as a sub-session of the project's tree: {:?}",
        rows(&a)
    );
}

/// The empty case: one project, one session, no children - and no `system` line, because a
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

/// The keeper's mark is ONE string, and this is the only crate that sees both halves: `tools` writes
/// the brief and sits below `sessionlog`, which names the mark the tree reads, so the two spellings
/// are held together here. A brief that began differently would draw a keeper as a conversation.
#[test]
fn the_keepers_brief_and_the_trees_mark_are_one_string() {
    assert_eq!(
        letibot_sessionlog::NOTES_KEEPER_TITLE_PREFIX,
        letibot_tools::notes_keeper::TITLE_PREFIX,
        "the tree finds a keeper by its title — a brief cut from another sentence is a keeper \
         drawn as a conversation"
    );
}

/// The fold glyph and the state work at every level - a project, a session, `system` and a
/// group, each opened by the same key and drawn by the same record.
#[test]
fn the_fold_works_at_every_level() {
    let mut a = a_daemon_with_two_projects();
    // The project: closed by the operator, and its rows go.
    let project = a.picker_rows()[0].id.clone();
    a.picker_sel = 0;
    assert_eq!(a.key(Key::Left), None);
    assert_eq!(
        rows(&a),
        vec!["[letibot (6)]", "[other projects (1)]"],
        "a closed project draws its line and nothing else"
    );
    assert_eq!(
        fold_of(&a, &project),
        Some(rano::agent::tree_list::Fold::Closed),
        "and the glyph says so"
    );
    // Opened again by the same key, and the tree comes back.
    a.key(Key::Right);
    assert!(
        rows(&a).contains(&"the habr article".to_string()),
        "{:?}",
        rows(&a)
    );
    // `system` folds too, and it is a level of its own. It starts CLOSED, so the key that puts its
    // groups on the list is `→`.
    let sys = a
        .picker_rows()
        .iter()
        .position(|r| r.session.is_none() && node_text(&a, &r.id) == "system (3)")
        .expect("the system line");
    a.picker_sel = sys;
    a.key(Key::Right);
    assert!(
        rows(&a).iter().any(|r| r.starts_with("[gatekeepers")),
        "`→` on `system` puts its groups on the list: {:?}",
        rows(&a)
    );
    a.key(Key::Left);
    assert!(
        !rows(&a).iter().any(|r| r.starts_with("[gatekeepers")),
        "{:?}",
        rows(&a)
    );
}

/// The rows the KEYS act on, as ids in drawing order - the list `←` and `→` walk, whichever of
/// the two drew it (the built-in layer's answer or the head's own).
fn drawn(a: &App) -> Vec<String> {
    a.picker_rows().into_iter().map(|r| r.id).collect()
}

/// Put the cursor on the row with this id, failing with the picture when it is not drawn.
fn cursor_on(a: &mut App, id: &str) {
    let rows = drawn(a);
    let at = rows
        .iter()
        .position(|r| r == id)
        .unwrap_or_else(|| panic!("`{id}` is not drawn: {rows:?}"));
    a.picker_sel = at;
}

/// The id under the cursor.
fn cursor(a: &App) -> Option<String> {
    a.picker_row().map(|r| r.id)
}

/// A tree with a conversation that has rows under it: the project's line, the conversation, and
/// its child's own child - the shape R159's keys are asked about.
fn a_tree_with_three_levels() -> App {
    let mut a = a_daemon_with_two_projects();
    a.set_row_fold("s-habr", true);
    a.set_row_fold("s-sub-1", true);
    a
}

/// `←` on an expanded session closes THAT session and nothing else, and the cursor stays on the
/// row it just closed. Filed by the operator on 2026-11-08: this key closed the whole project and
/// left the cursor on whichever project's line slid into the hole.
#[test]
fn left_on_an_expanded_session_closes_only_it_and_keeps_the_cursor() {
    let mut a = a_tree_with_three_levels();
    let before = drawn(&a);
    assert!(
        before.contains(&"s-sub-1".to_string()) && before.contains(&"s-sub-2".to_string()),
        "the conversation's rows are drawn: {before:?}"
    );
    cursor_on(&mut a, "s-habr");
    assert_eq!(a.key(Key::Left), None);
    let after = drawn(&a);
    assert_eq!(
        cursor(&a).as_deref(),
        Some("s-habr"),
        "the cursor left the row it closed: {after:?}"
    );
    let kept: Vec<String> = before
        .into_iter()
        .filter(|id| id != "s-sub-1" && id != "s-sub-2")
        .collect();
    assert_eq!(
        after, kept,
        "left closed more than the row under the cursor"
    );
}

/// `←` on a leaf steps OUT to the row it is drawn under and closes nothing: a tree's left is the
/// way back up, and the small subtree a person just opened stays open behind them.
#[test]
fn left_on_a_leaf_steps_out_to_its_parent_and_closes_nothing() {
    let mut a = a_tree_with_three_levels();
    let before = drawn(&a);
    cursor_on(&mut a, "s-sub-2");
    assert_eq!(a.key(Key::Left), None);
    assert_eq!(
        cursor(&a).as_deref(),
        Some("s-sub-1"),
        "left did not land on the row it is drawn under"
    );
    assert_eq!(drawn(&a), before, "a leaf's left closed something");
    // The row it landed on is EXPANDED, so the next press closes that one and stays - and only
    // then, on a closed row, does the same key step out to the conversation.
    assert_eq!(a.key(Key::Left), None);
    assert_eq!(cursor(&a).as_deref(), Some("s-sub-1"));
    assert!(
        !drawn(&a).contains(&"s-sub-2".to_string()),
        "the second press did not close the row it was on: {:?}",
        drawn(&a)
    );
    assert_eq!(a.key(Key::Left), None);
    assert_eq!(cursor(&a).as_deref(), Some("s-habr"));
    assert!(
        drawn(&a).contains(&"s-sub-1".to_string()),
        "stepping out closed the row above it too: {:?}",
        drawn(&a)
    );
}

/// `←` on a closed row that is already at the top of its branch moves nothing at all - it does not
/// close the branch it is in, and it does not jump to the next project's line.
#[test]
fn left_on_a_closed_project_moves_nothing_to_another_project() {
    let mut a = a_daemon_with_two_projects();
    // The project's line is closed by the fold, and the row after it belongs to another project.
    a.set_row_fold(LETIBOT, false);
    let before = drawn(&a);
    cursor_on(&mut a, LETIBOT);
    assert_eq!(a.key(Key::Left), None);
    assert_eq!(
        cursor(&a).as_deref(),
        Some(LETIBOT),
        "the cursor jumped off the closed line: {before:?}"
    );
    assert_eq!(drawn(&a), before, "a closed row's left folded something");
}

/// `→` opens the row under the cursor, and `→` again walks INTO the row it just opened: the first
/// row drawn under it. A leaf is where the gesture ends.
#[test]
fn right_expands_and_right_again_enters_the_first_child() {
    let mut a = a_tree_with_three_levels();
    a.set_row_fold("s-sub-1", false);
    let closed = drawn(&a);
    assert!(
        !closed.contains(&"s-sub-2".to_string()),
        "the grandchild starts folded away: {closed:?}"
    );
    cursor_on(&mut a, "s-sub-1");
    assert_eq!(a.key(Key::Right), None, "expanding is not a daemon action");
    assert!(
        drawn(&a).contains(&"s-sub-2".to_string()),
        "right did not open the row under the cursor: {:?}",
        drawn(&a)
    );
    assert_eq!(
        cursor(&a).as_deref(),
        Some("s-sub-1"),
        "opening a row moved the cursor off it"
    );
    // The second press walks in, and the row it lands on is the first one drawn under it.
    assert_eq!(a.key(Key::Right), None);
    assert_eq!(cursor(&a).as_deref(), Some("s-sub-2"));
    // A leaf has nothing under it, so the gesture ends there rather than moving elsewhere.
    assert_eq!(a.key(Key::Right), None);
    assert_eq!(cursor(&a).as_deref(), Some("s-sub-2"));
}

/// A click selects what it points at, and acts on a node's line - the row under the pointer,
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
        Some(true),
        "and acted on it: a node's line is the one row a click folds, and a closed one opens"
    );
    // The same click again, and the branch shuts - the state is the drawn one either way.
    a.key(Key::Click { x: 6, y });
    assert_eq!(
        a.picker_folds.get(&format!("{LETIBOT}/system")).copied(),
        Some(false),
        "and a second click closes it"
    );
}

/// Enter still switches to the session under the cursor, at any depth - the tree changed where
/// a row is drawn, not what Enter does on it.
#[test]
fn enter_still_switches_to_the_row_under_the_cursor() {
    let mut a = a_daemon_with_two_projects();
    a.set_row_fold("s-habr", true);
    a.picker_sel = row_of(&a, "s-sub-1");
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
}

/// A number is the n-th SESSION, not the n-th row - a project's line and every group's line
/// take none, because a number is somewhere to go.
#[test]
fn a_number_is_the_nth_session_and_a_nodes_line_has_none() {
    let mut a = a_daemon_with_two_projects();
    // `system` starts closed, so the branch is opened before its group can be.
    a.set_row_fold(&format!("{LETIBOT}/system"), true);
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
    // daemon's) - what matters is that the number exists and reaches it.
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

/// The bar says what Enter does on the row under the cursor - `switches` on a session, and the
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
    a.set_row_fold(&format!("{LETIBOT}/system"), true);
    a.set_row_fold(&format!("{LETIBOT}/system/gatekeepers"), true);
    let group = format!("{LETIBOT}/system/gatekeepers");
    a.picker_sel = row_of(&a, &group);
    assert!(
        a.hint_bar(200).contains("enter folds it away"),
        "{}",
        a.hint_bar(200)
    );
}

/// A box with several projects draws every project's line, and no session under the one this head is
/// not standing in - collapsed by default. The built-in layer's copy of the same rule is
/// `plugins/builtin/sessions`.
#[test]
fn every_project_line_is_drawn_and_the_others_are_shut() {
    let mut a = a_daemon_with_two_projects();
    assert_eq!(
        projects(&mut a),
        vec!["letibot (6)", "rano (1)"],
        "every project is a line, and the count is what it holds"
    );
    assert_eq!(
        rows(&a),
        vec![
            "[letibot (6)]",
            "the habr article",
            "[system (3)]",
            "[other projects (1)]"
        ],
        "the project the head is in draws its rows; the others are a folded group"
    );
    // And it opens on its own, by the person's key, and draws only its own rows.
    a.set_row_fold("groups/recent", true);
    a.set_row_fold(RANO, true);
    assert!(
        rows(&a).contains(&"the rano rewrite".to_string()),
        "opening `rano` draws its session: {:?}",
        rows(&a)
    );
    assert_eq!(
        a.picker_rows()
            .iter()
            .filter(|r| r.session.is_some())
            .count(),
        2,
        "and nothing else's"
    );
}

/// The two exceptions a closed default must keep, in the head's drawing: the project holding
/// the session this head is in, and the chain down to it - *a list that hides where you are cannot
/// answer where am I*.
#[test]
fn the_project_holding_here_is_open_and_the_chain_to_it_is_drawn() {
    let mut a = a_daemon_with_two_projects();
    assert_eq!(
        fold_of(&a, LETIBOT),
        Some(rano::agent::tree_list::Fold::Open),
        "the project this head is standing in is open, folded by nobody"
    );
    assert_eq!(
        a.picker_folds.get(LETIBOT),
        None,
        "and by rule, not by a fold entry: the person has said nothing about it"
    );
    // The chain: `s-sub-1` is a child of the row the head is in, so the row above it says `-` the
    // moment its parent is opened - and `here` is never hidden.
    a.set_row_fold("s-habr", true);
    assert_eq!(
        fold_of(&a, "s-habr"),
        Some(rano::agent::tree_list::Fold::Open)
    );
    assert!(
        rows(&a).contains(&"read the repo".to_string()),
        "{:?}",
        rows(&a)
    );
}

/// Enter on a project's line opens and closes it - the key the bar names (`enter folds it
/// away`), which is the gesture a closed default exists to be undone by.
#[test]
fn enter_on_a_project_line_opens_and_closes_it() {
    let mut a = a_daemon_with_two_projects();
    // The head is standing in `letibot`, so its line is open and Enter closes it.
    a.picker_sel = 0;
    assert_eq!(a.key(Key::Enter), None);
    assert!(
        !rows(&a).contains(&"the habr article".to_string()),
        "Enter closed the project: {:?}",
        rows(&a)
    );
    assert_eq!(
        fold_of(&a, &a.picker_rows()[0].id),
        Some(rano::agent::tree_list::Fold::Closed)
    );
    // And the same key brings them back.
    a.key(Key::Enter);
    assert!(
        rows(&a).contains(&"the habr article".to_string()),
        "Enter opened it again: {:?}",
        rows(&a)
    );
}

/// Two projects with the same name are told apart by their paths - the one case the name
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
        projects(&mut a),
        vec!["~/Projects/one/letibot (1)", "~/Projects/two/letibot (1)"],
        "two rows a reader can tell apart"
    );
}

/// A session whose parent this daemon does not hold is a root of its project's tree - an
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
    assert_eq!(depth_of(&a, "s-orphan"), 1);
}
