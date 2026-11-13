//! The session list's policy, tested where it lives. This is the copy of the tree's shape and of a
//! row's words that a project can override, so it is the copy that needs tests of its own. They run
//! natively, because none of the logic below knows a wasm ABI exists.

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
        moved_ms: 0,
        age: None,
        project: PROJECT.into(),
        project_name: "letibot".into(),
        // The head decides which top-level group a project is in; `own` is the one every test
        // here means unless it says otherwise.
        bucket: "own".into(),
        system: None,
    }
}

/// The answer for a feed, parsed back into the rows this layer draws.
fn draw(feed: &Feed, folded: &[(&str, bool)]) -> Vec<Out> {
    let folded: std::collections::BTreeMap<String, bool> =
        folded.iter().map(|(k, v)| ((*k).to_string(), *v)).collect();
    rows(&Ctx::new(feed, &folded))
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
        unlisted: None,
        more: 0,
        here: here.to_string(),
        home: "/home/dead".into(),
    }
}

/// A project's line, then its conversations under it - the top of the tree, and the level the
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
    // A project's line is closed by default - except this one, which holds the session the head is
    // standing in: a list that hides where you are cannot answer *where am I*.
    assert_eq!(draw(&f, &[])[0].open, Some(Fold::Open));
    assert_eq!(draw(&f, &[])[0].enter, Some(Act::fold()));
}

/// A child is drawn under its parent, and only when the parent is opened - the collapsed default is
/// what keeps thirty subagents from burying four conversations.
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

/// The `system` branch, one folded line per group - and no line for a group with no rows.
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
    // A group is folded until the person opens it - that is what keeps twelve reviewers from burying
    // the work - and so is the branch above it.
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

/// A reviewer nests under the row that spawned it inside its group, one step in - and it is drawn
/// once.
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

/// A session's row says everything a row can say in one line: the gutter's mark, the name, and the
/// right-hand column of rows and age. The waiting is a mark now, not words in the name - a person
/// scanning the list reads the column, and a name is what they are looking for.
#[test]
fn a_row_says_what_it_is() {
    let mut s = session("s-a", "the habr article");
    s.slug = "habr".into();
    s.waits_on = 2;
    s.stored = 412;
    s.heads = 2;
    s.running = true;
    s.moved_ms = 600_000;
    s.age = Some("10m".into());
    let f = feed(vec![s], "s-a");
    let row = &draw(&f, &[])[1];
    assert_eq!(
        row.cells[0].text, "?",
        "the gutter says somebody is waiting"
    );
    assert_eq!(
        row.cells[0].role,
        Some(Role::Attention),
        "and it is the Attention role, not a word in the name"
    );
    assert_eq!(row.cells[1].text, "habr · the habr article");
    assert_eq!(
        row.facts[0].text, "412 rows · 10m",
        "the fact column: how much conversation, how long ago it moved"
    );
    assert_eq!(
        row.facts[0].role,
        Some(Role::Faint),
        "a fact is a fact, whatever the row is doing"
    );
    assert_eq!(
        row.under.len(),
        1,
        "one faint line, drawn for the cursor only"
    );
    assert!(
        row.under[0].contains("qwen") && row.under[0].contains("2 heads"),
        "the detail line carries what the row cannot: {}",
        row.under[0]
    );
    assert_eq!(row.enter, Some(Act::switch("s-a")));
}

/// The gutter's other mark: a turn that is generating. It is the row's, not its parent's - but a
/// parent whose child is generating says so too, because a folded parent whose child is working is
/// a row that would otherwise read as idle.
#[test]
fn the_gutter_says_a_turn_is_running_even_under_a_folded_parent() {
    let parent = session("s-a", "the parent");
    let mut child = session("s-c", "the child");
    child.parent = Some("s-a".into());
    child.running = true;
    let f = feed(vec![parent, child], "s-a");
    let rows = draw(&f, &[]);
    let parent_row = &rows[1];
    assert_eq!(parent_row.id, "s-a");
    assert_eq!(
        parent_row.cells[0].text, "~",
        "the child's turn lights the parent's row"
    );
    assert_eq!(parent_row.cells[0].role, Some(Role::Pending));
}

/// A session that says nothing draws a blank gutter rather than one column less: a name's column
/// is a function of its depth, never of its state.
#[test]
fn a_quiet_row_keeps_the_gutter_column() {
    let f = feed(vec![session("s-a", "one"), session("s-b", "two")], "s-a");
    let rows = draw(&f, &[]);
    for row in rows.iter().filter(|r| r.number.is_some()) {
        assert_eq!(row.cells.len(), 2, "a mark and a name, always: {}", row.id);
        assert_eq!(row.cells[0].text, " ");
    }
}

/// A number is the n-th session - a node's line takes none, because a number is something you type
/// to go somewhere.
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

/// A head standing in a sub-session draws its family - the parent and its children, and no tree at
/// all.
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
/// gatekeeper and the reviewer itself is a `reviewer`, so a family walked in the parent's branch
/// dropped every child and drew one row. The group says where a row is drawn, not who is kin.
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

/// The same rule on the other pair the tree creates: a notes keeper spawned as a `task` child of an
/// ordinary conversation has a parent with no group at all, and it belongs in that family.
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

/// A workspace under the head's home is drawn as `~/…`, and every row carries the detail line - the
/// id, the path, the model and the heads - which the head draws for the row under the cursor only.
#[test]
fn a_row_under_is_the_id_and_a_shortened_path() {
    let mut s = session("s-a", "a");
    s.workspace = "/home/dead/Projects/letibot".into();
    let f = feed(vec![s], "s-a");
    assert_eq!(
        draw(&f, &[])[1].under[0],
        "s-a  ~/Projects/letibot · qwen · 1 head"
    );
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
    // And each opens on its own - the person's answer, one project at a time.
    let opened = draw(&f, &[("/home/dead/Projects/two", true)]);
    assert_eq!(
        words(&opened),
        vec!["[one (1)]", "s-here", "[two (1)]", "s-two", "[three (1)]"],
        "opening one project draws its rows and nothing else's"
    );
}

/// The chain to `here` is drawn whatever the folds say - the second exception a closed default must
/// keep. A head standing two levels down draws the family it is in, and the row it stands in is one
/// of them: a list that hides where you are cannot answer where am I.
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
    // The family view, and the fold map has no say in it - the person shut the middle row and the
    // row the head is standing in is still on the list, one step under it.
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
    // And the project's own exception: the line holding `here` is open, so the sessions of the
    // project the head is working in are on the list while every other project's are behind their
    // line.
    let alone = feed(vec![session("s-a", "a")], "s-a");
    assert_eq!(
        words(&draw(&alone, &[])),
        vec!["[letibot (1)]", "s-a"],
        "the project holding `here` is open"
    );
}

/// The head's own project's line says where you are when two projects would read alike - the name is
/// the head's to resolve, and this layer draws whatever it was handed. Two projects that moved in
/// the same instant keep the order the feed gave them, because a recency sort is stable.
#[test]
fn two_projects_are_drawn_in_the_order_the_feed_gives_them_when_nobody_moved() {
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

/// The projects a box holds are sorted by when they last moved, not by when the store listed them:
/// a list in first-seen order buries the project somebody is working on behind every one that has
/// been idle since the last reboot.
#[test]
fn the_projects_are_sorted_by_when_they_last_moved() {
    let mut stale = session("s-stale", "a conversation from last week");
    stale.project = "/home/dead/Projects/stale".into();
    stale.project_name = "stale".into();
    stale.bucket = "older".into();
    stale.moved_ms = 1_000;
    let mut busy = session("s-busy", "the one being worked on");
    busy.project = "/home/dead/Projects/busy".into();
    busy.project_name = "busy".into();
    busy.bucket = "older".into();
    busy.moved_ms = 5_000;
    // The feed lists the stale one first - the daemon's order, not the reader's.
    let f = feed(vec![stale, busy, session("s-here", "here")], "s-here");
    assert_eq!(
        words(&draw(&f, &[("groups/older", true)])),
        vec![
            "[letibot (1)]",
            "s-here",
            "[older (2)]",
            "[busy (1)]",
            "[stale (1)]"
        ],
        "the project that moved last is drawn first"
    );
}

/// A folded group shows how many of the sessions inside it are waiting on the operator or
/// generating - the facts a fold would otherwise hide, and the reason a person has to open every
/// group to find out who is stuck.
#[test]
fn a_folded_group_says_what_is_waiting_inside_it() {
    let mut asking = session("s-asking", "a child nobody can see");
    asking.parent = Some("s-a".into());
    asking.waits_on = 2;
    let mut working = session("s-working", "a child that is running");
    working.parent = Some("s-a".into());
    working.running = true;
    let f = feed(vec![session("s-a", "the parent"), asking, working], "s-a");
    // The parent is folded, so neither child is drawn.
    let rows = draw(&f, &[("s-a", false)]);
    assert_eq!(ids(&rows), vec![format!("[{PROJECT}]"), "s-a".into()]);
    let project = &rows[0];
    assert_eq!(
        project
            .facts
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        vec!["1 waiting", "1 generating"],
        "the project's line says what is inside it, folded away or not"
    );
    assert_eq!(
        project.facts[0].role,
        Some(Role::Attention),
        "waiting is the Attention role here too"
    );
    assert_eq!(project.facts[1].role, Some(Role::Pending));
    // A group of the `system` branch is a line of its own, and it carries the same rollup - a
    // reviewer waiting on a person inside a branch nobody opened is exactly the row that used to
    // be invisible.
    let mut gatekeeper = session("s-gatekeeper-0123456789ab", "the review queue");
    gatekeeper.system = Some("gatekeepers".into());
    let mut reviewer = session("s-gatekeeper-0123456789ab-sub-1", "review it");
    reviewer.system = Some("reviewers".into());
    reviewer.parent = Some("s-gatekeeper-0123456789ab".into());
    reviewer.waits_on = 1;
    let f = feed(vec![session("s-a", "a"), gatekeeper, reviewer], "s-a");
    let rows = draw(&f, &[]);
    let branch = rows
        .iter()
        .find(|r| r.id == format!("{PROJECT}/system"))
        .expect("the system branch is a line");
    assert_eq!(
        branch
            .facts
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        vec!["1 waiting"],
        "and the branch above the group says it too"
    );
    // A group with nothing to report carries no empty fact back to the host.
    let quiet = draw(&feed(vec![session("s-a", "a")], "s-a"), &[]);
    assert!(quiet[0].facts.is_empty(), "nothing to say is nothing drawn");
}

/// The feed's own cap is stated on the list, as a line no key can enter: a list that stops at 250
/// and says nothing is a list that lies by omission. The row has no id for that reason.
#[test]
fn a_feed_the_host_cut_says_how_many_are_not_shown() {
    let mut f = feed(vec![session("s-a", "one")], "s-a");
    f.more = 42;
    let drawn = draw(&f, &[]);
    assert_eq!(drawn.last().expect("the list is not empty").id, "");
    assert_eq!(
        drawn.last().expect("a last row").cells[0].text,
        "42 more sessions not shown — the list carries 250 at a time",
        "the number is the feed's, and the cap is the SDK's"
    );
    // And a feed that was not cut says nothing about it - not a row with a zero on it.
    assert_eq!(
        words(&draw(&feed(vec![session("s-a", "one")], "s-a"), &[])),
        vec!["[letibot (1)]", "s-a"]
    );
}

/// The synthetic first frame a daemon asks every built-in pane at a session's door
/// (`PluginHost::ensure_builtin_panes`): no feed at all is a pane that says so. A layer that passed
/// instead drew nothing, and a pane that vanishes reads as one somebody closed.
#[test]
fn a_host_with_no_feed_is_a_pane_that_says_so() {
    let input = serde_json::json!({
        "v": letibot_plugin::pane::V,
        "size": {"w": 20, "h": 5},
        "feeds": {},
    });
    let bytes = serde_json::to_vec(&input).expect("the input serialises");
    let text = answer(&bytes).expect("the layer answers rather than vanishing");
    let view: serde_json::Value = serde_json::from_str(&text).expect("and it is JSON");
    let said = view.to_string();
    assert!(said.contains("hands no `sessions` feed"), "{said}");
    assert_eq!(
        view["panels"][0]["summary"][0]["tree"]["rows"],
        serde_json::json!([]),
        "a pane with no feed claims no rows: {said}"
    );
}

/// One session the daemon has put out of sight, as the feed spells it: no project, its own bucket.
fn archived_session(id: &str, title: &str) -> Session {
    let mut s = session(id, title);
    s.live = false;
    s.heads = 0;
    s.project = String::new();
    s.project_name = String::new();
    s.bucket = letibot_plugin::sessions::ARCHIVED_BUCKET.into();
    s
}

/// What is out of sight is a group at the foot of the list and not a project of its own: closed by
/// default, counted, and every row of it a session `enter` resumes.
#[test]
fn archived_sessions_are_a_group_at_the_foot() {
    let f = feed(
        vec![
            session("s-a", "one"),
            archived_session("s-old", "last week's question"),
        ],
        "s-a",
    );
    assert_eq!(
        words(&draw(&f, &[])),
        vec!["[letibot (1)]", "s-a", "[archived (1)]"],
        "the archived row is not one of the project's, and the group says how many are in it"
    );
    let group = &draw(&f, &[])[2];
    assert_eq!(
        group.open,
        Some(Fold::Closed),
        "a group nobody opened is shut, like every other"
    );
    assert_eq!(group.enter, Some(Act::fold()));
    // Opened, the row is drawn one step in, with no number and no fold - and `enter` opens it.
    let open = draw(&f, &[(letibot_plugin::sessions::ARCHIVED_KEY, true)]);
    let row = open.last().expect("the group's row");
    assert_eq!(row.id, "s-old");
    assert_eq!(row.depth, 1);
    assert_eq!(
        row.number, None,
        "an archived row is not on the tree's count"
    );
    assert_eq!(row.open, None, "and it holds nothing to fold");
    assert_eq!(row.enter, Some(Act::switch("s-old")));
    assert_eq!(
        row.cells[0].text, " ",
        "the gutter column is reserved here too"
    );
    assert_eq!(row.cells[1].text, "last week's question");
}

/// An archived row is in the feed and not in the tree: the project line of the workspace it was held
/// in does not count it, and a live child of one is drawn as a root of its own rather than folded
/// under a parent the list does not draw.
#[test]
fn an_archived_row_is_neither_a_member_nor_a_parent() {
    let mut child = session("s-kid", "a child of the one put away");
    child.parent = Some("s-old".into());
    let f = feed(
        vec![
            session("s-a", "one"),
            archived_session("s-old", "last week's question"),
            child,
        ],
        "s-a",
    );
    assert_eq!(
        words(&draw(&f, &[])),
        vec![
            "[letibot (2)]",
            "s-a",
            // The child's parent is not on the tree, so the child is a root of the project's.
            "s-kid",
            "[archived (1)]"
        ],
        "the archived row's project membership is the live one's, not its own"
    );
    assert_eq!(
        draw(&f, &[])[1].open,
        None,
        "a row whose only child is the archived one holds nothing to fold"
    );
}
