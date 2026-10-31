//! **The session list's policy, tested where it lives.**
//!
//! This is the copy of the tree's shape and of a row's words that a project can override, so it is
//! the copy that needs tests of its own: the host-side tests say *the list draws what the head
//! draws*, and these say *the list is the one the plan describes* — a project's line, its
//! conversations under it, a `system` branch with one folded line per group, a session that waits
//! on you saying so, the numbers running over the sessions alone.
//!
//! They run natively (`cargo test` in this directory): the module is a `cdylib` for the host and
//! an `rlib` for these, and none of the logic below knows that a wasm ABI exists.

use super::*;

const PROJECT: &str = "/home/dead/Projects/letibot/letibot";

/// One session in a feed, with the defaults an ordinary row has — in the one project every test
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
        here: &feed.here,
        home: &feed.home,
        folded: &folded,
        path,
    };
    rows(&ctx)
}

/// The ids a drawing holds, in order — `[<a node's id>]` for a line that is not a session.
fn ids(rows: &[Out]) -> Vec<String> {
    rows.iter()
        .map(|r| match r.number {
            None => format!("[{}]", r.id),
            Some(_) => r.id.clone(),
        })
        .collect()
}

/// The lines a person reads, in order — a node by its words, a session by its id.
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
        here: here.to_string(),
        home: "/home/dead".into(),
    }
}

/// **A project's line, then its conversations under it** — the top of the tree, and the level the
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
    // **A project is open by default** — the tree is what the operator asked to see.
    assert_eq!(draw(&f, &[])[0].open, Some(Fold::Open));
    assert_eq!(draw(&f, &[])[0].enter, Some(Act::fold()));
}

/// **A child is drawn under its parent, and only when the parent is opened** — the collapsed
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

/// **The `system` branch, one folded line per group** — and no line for a group with no rows.
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
        vec![
            "[letibot (3)]",
            "s-a",
            "[system (2)]",
            "[gatekeepers (1)]",
            "[reviewers (1)]",
        ],
        "two groups, and no `keepers` line for a group the daemon has nothing in"
    );
    // **A group is folded until the person opens it** — that is what keeps twelve reviewers from
    // burying the work.
    let folded = draw(&f, &[]);
    assert_eq!(folded[3].open, Some(Fold::Closed));
    let key = format!("{PROJECT}/system/reviewers");
    let open = draw(&f, &[(&key, true)]);
    assert!(
        open.iter().any(|r| r.id.ends_with("-sub-1")),
        "the reviewer is drawn once its group is opened: {:?}",
        ids(&open)
    );
}

/// **A reviewer nests under the row that spawned it inside its group**, one step in — and it is
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

/// **A number is the n-th SESSION** — a node's line takes none, because a number is something you
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

/// **A head standing in a sub-session draws its FAMILY** — the parent and its children, and no
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

/// **A workspace under the head's home is drawn as `~/…`**, and the full id sits under every row.
#[test]
fn a_row_under_is_the_id_and_a_shortened_path() {
    let mut s = session("s-a", "a");
    s.workspace = "/home/dead/Projects/letibot".into();
    let f = feed(vec![s], "s-a");
    assert_eq!(draw(&f, &[])[1].under[0], "s-a  ~/Projects/letibot");
}

/// **The head's own project's line says where you are** when two projects would read alike — the
/// name is the head's to resolve, and this layer draws whatever it was handed.
#[test]
fn two_projects_are_drawn_in_the_order_the_feed_gives_them() {
    let mut other = session("s-b", "b");
    other.project = "/home/dead/Projects/other/letibot".into();
    other.project_name = "~/Projects/other/letibot".into();
    let f = feed(vec![session("s-a", "a"), other], "s-a");
    assert_eq!(
        words(&draw(&f, &[])),
        vec![
            "[letibot (1)]",
            "s-a",
            "[~/Projects/other/letibot (1)]",
            "s-b"
        ]
    );
}
