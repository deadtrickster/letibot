//! The session list's policy, tested where it lives.
//!
//! This is the copy of the tree's shape and of a row's words that a project can override, so it is
//! the copy that needs tests of its own: the host-side tests say *the list draws what the head
//! draws*, and these say *the list is the one the plan describes*.
//!
//! They run natively (`cargo test` in this directory): the module is a `cdylib` for the host and an
//! `rlib` for these, and none of the logic below knows that a wasm ABI exists.

use super::*;

const PROJECT: &str = "/home/dead/Projects/letibot/letibot";

/// One session in a feed, with the defaults an ordinary row has - in the one project every test
/// here uses unless it says otherwise.
fn session(id: &str, title: &str) -> Session {
    Session {
        id: id.to_string(),
        title: title.to_string(),
        slug: String::new(),
        parent: None,
        origin: "operator".into(),
        role: None,
        live: true,
        running: false,
        heads: 1,
        items: 6,
        stored: 0,
        model: "qwen".into(),
        workspace: String::new(),
        waits_on: 0,
        nags_paused: false,
        project: PROJECT.into(),
        project_name: "letibot".into(),
        system: None,
    }
}

/// The answer for a feed, parsed back into the rows this layer draws.
fn draw(feed: &Feed, folded: &[(&str, bool)]) -> Vec<Out> {
    let mut path = vec![feed.here.clone()];
    let mut cur = feed.here.clone();
    for _ in 0..MAX_DEPTH {
        match feed.sessions.iter().find(|s| s.id == cur) {
            Some(s) => match s.parent.clone() {
                Some(p) => {
                    path.push(p.clone());
                    cur = p;
                }
                None => break,
            },
            None => break,
        }
    }
    let folded: std::collections::BTreeMap<String, bool> =
        folded.iter().map(|(k, v)| ((*k).to_string(), *v)).collect();
    let ctx = Ctx {
        sessions: &feed.sessions,
        fleet: &feed.fleet,
        here: &feed.here,
        home: &feed.home,
        folded: &folded,
        path,
    };
    rows(&ctx)
}

/// The ids a drawing holds, in order - `[<a node's id>]` for a line that is not a session.
fn ids(rows: &[Out]) -> Vec<String> {
    rows.iter()
        .map(|r| match r.number {
            None => format!("[{}]", r.id),
            Some(_) => r.id.clone(),
        })
        .collect()
}

/// The lines a person reads, in order - a node by its words, a session by its id.
fn words(rows: &[Out]) -> Vec<String> {
    rows.iter()
        .map(|r| match r.number {
            None => format!("[{}]", r.cells[0].text),
            Some(_) => r.id.clone(),
        })
        .collect()
}

fn feed(sessions: Vec<Session>, here: &str) -> Feed {
    Feed {
        sessions,
        fleet: Vec::new(),
        here: here.to_string(),
        home: "/home/dead".into(),
    }
}

/// **A project's line, then its conversations under it** - the top of the tree, and the level the
/// operator asked for by name.
#[test]
fn a_projects_line_and_its_conversations_under_it() {
    let f = feed(vec![session("s-a", "one"), session("s-b", "two")], "s-a");
    assert_eq!(
        words(&draw(&f, &[])),
        vec!["[letibot (2)]", "s-a", "s-b"],
        "the project's line carries the count of every session it holds"
    );
    assert_eq!(draw(&f, &[])[0].depth, 0, "the project is the top");
    assert_eq!(draw(&f, &[])[1].depth, 1, "a session is one step in");
    // **A project's line is CLOSED by default** - except this one, which holds the session the head
    // is standing in: a list that hides where you are cannot answer *where am I*.
    assert_eq!(draw(&f, &[])[0].open, Some(Fold::Open));
    assert_eq!(draw(&f, &[])[0].enter, Some(Act::fold()));
}

/// **A child is drawn under its parent, and only when the parent is opened** - the collapsed
/// default is what keeps thirty subagents from burying four conversations.
#[test]
fn a_child_is_drawn_under_its_parent_once_it_is_opened() {
    let mut child = session("s-c", "read the repo");
    child.parent = Some("s-a".into());
    let f = feed(vec![session("s-a", "a"), child], "s-a");
    assert_eq!(
        ids(&draw(&f, &[])),
        vec![format!("[{PROJECT}]"), "s-a".into()]
    );
    let parent = &draw(&f, &[])[1];
    assert_eq!(parent.open, Some(Fold::Closed), "the glyph says `+`");
    let open = draw(&f, &[("s-a", true)]);
    assert_eq!(open[2].id, "s-c", "the child is drawn once the parent is");
    assert_eq!(open[2].depth, 2);
}

/// **The `system` branch, one folded line per group** - and no line for a group with no rows.
#[test]
fn the_system_branch_is_one_folded_line_per_group() {
    let mut gatekeeper = session("s-gatekeeper-0123456789ab", "the review queue");
    gatekeeper.system = Some("gatekeepers".into());
    let mut reviewer = session(
        "s-gatekeeper-0123456789ab-sub-1",
        "You are the gatekeeper. review",
    );
    reviewer.system = Some("reviewers".into());
    reviewer.parent = Some("s-gatekeeper-0123456789ab".into());
    let f = feed(vec![session("s-a", "a"), gatekeeper, reviewer], "s-a");
    assert_eq!(
        words(&draw(&f, &[])),
        vec!["[letibot (3)]", "s-a", "[system (2)]"],
        "`system` starts closed, and a closed branch draws its own line and nothing under it"
    );
    // **A group is folded until the person opens it** - that is what keeps twelve reviewers from
    // burying the work - and so is the branch above it.
    let key = format!("{PROJECT}/system");
    let branch = draw(&f, &[(&key, true)]);
    assert_eq!(
        words(&branch),
        vec![
            "[letibot (3)]",
            "s-a",
            "[system (2)]",
            "[gatekeepers (1)]",
            "[reviewers (1)]",
        ],
        "two groups, and no `keepers` line for a group the daemon has nothing in"
    );
    assert_eq!(branch[3].open, Some(Fold::Closed));
    let key = format!("{PROJECT}/system/reviewers");
    let open = draw(&f, &[(&format!("{PROJECT}/system"), true), (&key, true)]);
    assert!(
        open.iter().any(|r| r.id.ends_with("-sub-1")),
        "the reviewer is drawn once its group is opened: {:?}",
        ids(&open)
    );
}

/// **A reviewer nests under the row that spawned it inside its group**, one step in - and it is
/// drawn ONCE.
#[test]
fn a_reviewer_nests_inside_its_group() {
    let mut gatekeeper = session("s-gatekeeper-0123456789ab", "the review queue");
    gatekeeper.system = Some("gatekeepers".into());
    let mut reviewer = session(
        "s-gatekeeper-0123456789ab-sub-1",
        "You are the gatekeeper. review",
    );
    reviewer.system = Some("reviewers".into());
    reviewer.parent = Some("s-gatekeeper-0123456789ab".into());
    let f = feed(vec![session("s-a", "a"), gatekeeper, reviewer], "s-a");
    let open = draw(
        &f,
        &[
            (format!("{PROJECT}/system").as_str(), true),
            (format!("{PROJECT}/system/reviewers").as_str(), true),
            (format!("{PROJECT}/system/gatekeepers").as_str(), true),
        ],
    );
    let reviewer_at = open
        .iter()
        .position(|r| r.id.ends_with("-sub-1"))
        .expect("drawn");
    assert_eq!(
        open[reviewer_at].depth, 3,
        "one step inside its group: project, system, group, row"
    );
    assert_eq!(
        open[reviewer_at - 1].cells[0].text,
        "reviewers (1)",
        "and under the group's own line"
    );
    assert_eq!(
        ids(&open).iter().filter(|i| i.ends_with("-sub-1")).count(),
        1,
        "drawn once"
    );
}

/// **A session's row says everything a row can say**: the slug, `WAITING ON YOU`, paused nags, and
/// a fact column.
#[test]
fn a_row_says_what_it_is() {
    let mut s = session("s-a", "the habr article");
    s.slug = "habr".into();
    s.waits_on = 2;
    s.stored = 412;
    s.heads = 2;
    s.running = true;
    let f = feed(vec![s], "s-a");
    let row = &draw(&f, &[])[1];
    assert_eq!(
        row.cells[0].text,
        "habr · the habr article · WAITING ON YOU (2)"
    );
    assert_eq!(
        row.facts[0].text, "generating · 412 rows · 2 heads · qwen",
        "the fact column, in the order a reader scans it"
    );
    assert_eq!(
        row.facts[0].role,
        Some(Role::Pending),
        "a running turn is lit"
    );
    assert_eq!(row.enter, Some(Act::switch("s-a")));
}

/// **A number is the n-th SESSION** - a node's line takes none, because a number is something you
/// type to go somewhere.
#[test]
fn numbers_run_over_the_sessions_and_never_over_a_node() {
    let mut child = session("s-c", "read the repo");
    child.parent = Some("s-a".into());
    let f = feed(vec![session("s-a", "a"), session("s-b", "b"), child], "s-a");
    let numbers: Vec<(String, Option<usize>)> = draw(&f, &[("s-a", true)])
        .into_iter()
        .map(|r| (r.id, r.number))
        .collect();
    assert_eq!(
        numbers,
        vec![
            (PROJECT.to_string(), None),
            ("s-a".to_string(), Some(1)),
            ("s-c".to_string(), Some(2)),
            ("s-b".to_string(), Some(3)),
        ],
        "the numbers run over the sessions drawn, in drawing order, and a node takes none"
    );
}

/// **A head standing in a sub-session draws its FAMILY** - the parent and its children, and no
/// tree at all.
#[test]
fn the_family_view_has_no_tree() {
    let mut child = session("s-c", "read the repo");
    child.parent = Some("s-a".into());
    let f = feed(vec![session("s-a", "the parent"), child], "s-c");
    assert_eq!(ids(&draw(&f, &[])), vec!["s-a", "s-c"]);
    assert_eq!(draw(&f, &[])[1].depth, 1);
    assert!(
        !ids(&draw(&f, &[])).iter().any(|i| i.starts_with('[')),
        "no project's line and no `system` line in a family list"
    );
}

/// The review queue's own id, as a daemon's is: `s-gatekeeper-<12 hex>`.
const GATEKEEPER: &str = "s-gatekeeper-0123456789ab";

/// A family is a family whatever group its members are drawn in: a reviewer's parent is the
/// gatekeeper and the reviewer itself is a `reviewer`, so a family walked in the PARENT's branch
/// dropped every child and drew one row. The group says where a row is DRAWN, not who is kin.
#[test]
fn a_family_is_drawn_across_the_groups_its_members_are_in() {
    let reviewer = format!("{GATEKEEPER}-sub-1791759365511364772");
    let mut gatekeeper = session(GATEKEEPER, "the review queue");
    gatekeeper.system = Some("gatekeepers".into());
    let mut review = session(&reviewer, "You are the gatekeeper. review it");
    review.system = Some("reviewers".into());
    review.parent = Some(GATEKEEPER.into());
    let f = feed(vec![session("s-a", "a"), gatekeeper, review], &reviewer);
    assert_eq!(
        ids(&draw(&f, &[])),
        vec![GATEKEEPER.to_string(), reviewer.clone()],
        "the parent and the child, and no group's line between them"
    );
    let rows = draw(&f, &[]);
    assert_eq!(
        rows[0].open,
        Some(Fold::Open),
        "the parent holds a row that is on the list, so it says `-`"
    );
    assert_eq!(
        rows[1].depth, 1,
        "one step in, whatever group it is drawn in"
    );
    assert_eq!(
        rows[1].number,
        Some(2),
        "and the numbers still run over the rows drawn"
    );
}

/// **The same rule on the other pair the tree creates**: a notes keeper spawned as a `task` child
/// of an ordinary conversation has a parent with no group at all, and it belongs in that family.
#[test]
fn a_keeper_child_of_a_conversation_is_in_its_family() {
    let mut keeper = session("s-keeper", "You are the notes keeper. tidy the notes");
    keeper.system = Some("keepers".into());
    keeper.parent = Some("s-a".into());
    let f = feed(vec![session("s-a", "the habr article"), keeper], "s-keeper");
    assert_eq!(
        ids(&draw(&f, &[])),
        vec!["s-a", "s-keeper"],
        "the keeper is the conversation's child, and the head is standing in it"
    );
}

/// A `-` over a row whose child is on the screen: the person can fold the parent of the session
/// they are standing in away, and the walk still draws that session. The glyph reads the chain to
/// `here` first, because `expanded || child_on_path` is the head's rule and the chain wins.
#[test]
fn a_row_whose_child_is_on_the_path_is_drawn_open_though_it_was_folded() {
    let mut child = session("s-c", "read the repo");
    child.parent = Some("s-a".into());
    let f = feed(vec![session("s-a", "the parent"), child], "s-c");
    let rows = draw(&f, &[("s-a", false)]);
    assert_eq!(
        ids(&rows),
        vec!["s-a", "s-c"],
        "the row the head is standing in cannot be hidden"
    );
    assert_eq!(
        rows[0].open,
        Some(Fold::Open),
        "`-`, because the child it holds is drawn under it"
    );
}

/// **A workspace under the head's home is drawn as `~/…`**, and the full id sits under every row.
#[test]
fn a_row_under_is_the_id_and_a_shortened_path() {
    let mut s = session("s-a", "a");
    s.workspace = "/home/dead/Projects/letibot".into();
    let f = feed(vec![s], "s-a");
    assert_eq!(draw(&f, &[])[1].under[0], "s-a  ~/Projects/letibot");
}

/// A box with several projects draws every project's line and NO session row under the ones the
/// head is not standing in. The list is a map of the box first: which projects there are, and how
/// many sessions each holds - a person opens the one they are working on.
#[test]
fn every_project_line_is_drawn_and_no_session_under_the_ones_you_are_not_in() {
    let mut here = session("s-here", "the one i am in");
    here.project = "/home/dead/Projects/one".into();
    here.project_name = "one".into();
    let mut two = session("s-two", "a session of project two");
    two.project = "/home/dead/Projects/two".into();
    two.project_name = "two".into();
    let mut three = session("s-three", "a session of project three");
    three.project = "/home/dead/Projects/three".into();
    three.project_name = "three".into();
    let f = feed(vec![here, two, three], "s-here");
    assert_eq!(
        words(&draw(&f, &[])),
        vec!["[one (1)]", "s-here", "[two (1)]", "[three (1)]"],
        "every project is a line; the sessions of the two the head is not in are behind them"
    );
    // **And each opens on its own** - the person's answer, one project at a time.
    let opened = draw(&f, &[("/home/dead/Projects/two", true)]);
    assert_eq!(
        words(&opened),
        vec!["[one (1)]", "s-here", "[two (1)]", "s-two", "[three (1)]"],
        "opening one project draws its rows and nothing else's"
    );
}

/// **The chain to `here` is drawn whatever the folds say** - the second exception a closed default
/// must keep. A head standing two levels down draws the family it is in, and the row it stands in
/// is one of them: *a list that hides where you are cannot answer where am I*.
#[test]
fn the_chain_to_here_is_drawn_though_every_fold_is_shut() {
    let mut grandchild = session("s-gc", "the one i am in");
    grandchild.parent = Some("s-child".into());
    let mut child = session("s-child", "the middle");
    child.parent = Some("s-root".into());
    let f = feed(
        vec![session("s-root", "the root"), child, grandchild],
        "s-gc",
    );
    // **The family view, and the fold map has no say in it** - the person shut the middle row and
    // the row the head is standing in is still on the list, one step under it.
    assert_eq!(
        ids(&draw(&f, &[("s-child", false)])),
        vec!["s-child".to_string(), "s-gc".to_string()],
        "the parent, and the row between it and the session the head is standing in"
    );
    let rows = draw(&f, &[("s-child", false)]);
    assert_eq!(
        rows[0].open,
        Some(Fold::Open),
        "and the row above says `-`, because the row it holds is on the screen"
    );
    assert_eq!(
        rows[1].depth, 1,
        "drawn at its depth, not promoted to the top"
    );
    // **And the project's own exception**: the line holding `here` is open, so the sessions of the
    // project the head is working in are on the list while every other project's are behind their
    // line (see `every_project_line_is_drawn_and_no_session_under_the_ones_you_are_not_in`).
    let alone = feed(vec![session("s-a", "a")], "s-a");
    assert_eq!(
        words(&draw(&alone, &[])),
        vec!["[letibot (1)]", "s-a"],
        "the project holding `here` is open"
    );
}

/// **The head's own project's line says where you are** when two projects would read alike - the
/// name is the head's to resolve, and this layer draws whatever it was handed.
#[test]
fn two_projects_are_drawn_in_the_order_the_feed_gives_them() {
    let mut other = session("s-b", "b");
    other.project = "/home/dead/Projects/other/letibot".into();
    other.project_name = "~/Projects/other/letibot".into();
    let f = feed(vec![session("s-a", "a"), other], "s-a");
    assert_eq!(
        words(&draw(&f, &[])),
        vec!["[letibot (1)]", "s-a", "[~/Projects/other/letibot (1)]"],
        "the project this head is standing in is open, and the other one is a closed line"
    );
    let opened = draw(&f, &[("/home/dead/Projects/other/letibot", true)]);
    assert_eq!(
        words(&opened),
        vec![
            "[letibot (1)]",
            "s-a",
            "[~/Projects/other/letibot (1)]",
            "s-b"
        ]
    );
}
