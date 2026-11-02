//! Taking a session off the list, and what this head refuses to pretend it can do: `del` asks
//! twice, `ctrl-z` puts the row back, a typed name is a filter and never a removal, and the row this
//! head stands in - nor any above it - cannot go. `/forget` refuses by name and says what it costs.

use super::*;
use letibot_plugins::Stack;
use letibot_sessionlog::SessionOrigin;
use std::sync::Arc;

const LETIBOT: &str = "/home/dead/Projects/letibot/letibot";

/// A session in one project, with a workspace of its own.
fn in_tree(id: &str, title: &str, live: bool) -> SessionBrief {
    let mut b = brief(id, title, false);
    b.live = live;
    b.wiring.workspace = LETIBOT.into();
    b
}

fn child(id: &str, title: &str, parent: &str) -> SessionBrief {
    let mut b = in_tree(id, title, false);
    b.parent_session_id = Some(parent.into());
    b
}

/// A head standing in `s-habr`, on the picker, with the rows it was handed.
fn a_head(sessions: Vec<SessionBrief>) -> App {
    a_head_at("s-habr", sessions)
}

/// The same, standing somewhere else - the fixture for *the session you are in*.
fn a_head_at(here: &str, sessions: Vec<SessionBrief>) -> App {
    let mut a = app();
    a.home = "/home/dead".into();
    a.apply(hello(here, sessions, Hub::new(here).snapshot()));
    a.push_surface(Surface::Picker);
    a
}

/// The head with the built-in `sessions` layer actually installed: without it there is no
/// `sessions_host` and `picker_rows` falls back to the head's own `session_list`, so a test meaning
/// to ask the LAYER would compare two head-side functions over one source and prove nothing.
fn with_the_layer(a: &mut App) {
    let project = std::env::temp_dir().join(format!("letibot-removal-{}", std::process::id()));
    std::fs::create_dir_all(&project).expect("a project dir");
    let mut stack = Stack::discover(
        None,
        Some(&project),
        None,
        Arc::new(crate::app::sessions_pane::RanoLoader),
    );
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "{failed:?}");
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));
    std::fs::remove_dir_all(&project).ok();
}

/// The ids of the session rows the list draws, in order - the drawn list, whichever of the two
/// drawings answered.
fn drawn_ids(a: &App) -> Vec<String> {
    a.picker_rows()
        .iter()
        .filter_map(|r| r.session.map(|i| a.sessions[i].session_id.clone()))
        .collect()
}

/// The row of a session by its id, or a panic naming what is drawn instead.
fn row_of(a: &App, id: &str) -> usize {
    a.picker_rows()
        .iter()
        .position(|r| r.id == id)
        .unwrap_or_else(|| panic!("`{id}` is drawn: {:?}", drawn_ids(a)))
}

/// What the status line says - the one sentence `say` keeps.
fn said(a: &App) -> String {
    a.notice.clone().unwrap_or_default()
}

/// `del` is asked twice: the first press must remove nothing - that is the whole of asking twice -
/// and its sentence names the row, the subtree under it and where it stays. The second removes it.
#[test]
fn del_asks_before_it_removes() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
        child("s-sub-1", "read the repo", "s-habr"),
    ]);
    a.picker_sel = row_of(&a, "s-rust");
    assert_eq!(a.key(Key::Delete), None);
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-rust"],
        "the first press removes nothing"
    );
    assert_eq!(
        a.removal_armed.as_ref().map(|(id, _)| id.clone()),
        Some("s-rust".to_string()),
        "and it arms the row it was pressed on"
    );
    let first = said(&a);
    assert!(first.contains("the rust rewrite"), "{first}");
    assert!(
        first.contains("s-rust"),
        "the refusal names the id: {first}"
    );
    assert!(first.contains("ctrl-z brings it back"), "{first}");
    // The second press is the removal.
    a.key(Key::Delete);
    assert_eq!(drawn_ids(&a), vec!["s-habr"], "the row is gone");
    assert!(
        said(&a).contains("1 session is removed"),
        "and the list says how much it is hiding: {}",
        said(&a)
    );
}

/// **`ctrl-z` brings the last one back**, and says how many are still off the list.
#[test]
fn ctrl_z_brings_the_last_removed_row_back() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
        in_tree("s-pkg", "make me a landing page", false),
    ]);
    for id in ["s-rust", "s-pkg"] {
        a.picker_sel = row_of(&a, id);
        a.key(Key::Delete);
        a.key(Key::Delete);
    }
    assert_eq!(drawn_ids(&a), vec!["s-habr"]);
    a.key(Key::Undo);
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-pkg"],
        "the LAST removed one, back where it was"
    );
    assert!(said(&a).contains("1 session is removed"), "{}", said(&a));
    a.key(Key::Undo);
    assert_eq!(drawn_ids(&a), vec!["s-habr", "s-rust", "s-pkg"]);
    assert!(said(&a).contains("nothing is removed"), "{}", said(&a));
    // And one more `ctrl-z` says there is nothing to bring back rather than doing nothing quietly.
    a.key(Key::Undo);
    assert!(said(&a).contains("nothing is removed"), "{}", said(&a));
}

/// A name typed into the picker is a name, letter by letter: the picker is the one surface whose
/// typed line is not a `/verb`, so a letter bound here matches exactly when the composer is empty -
/// which is the first character of a filter. `del` and `ctrl-z` are spellings a typed line cannot be.
#[test]
fn a_typed_name_is_never_a_removal() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-fix", "fix the parser", false),
        in_tree("s-x", "x marks the spot", false),
        in_tree("s-u", "undo the rename", false),
    ]);
    let before = a.notice.clone();
    typed(&mut a, "fix the parser");
    assert_eq!(
        a.editor.text(),
        "fix the parser",
        "every letter of the filter reached the composer"
    );
    assert_eq!(a.removal_armed, None, "and nothing was armed");
    assert_eq!(a.notice, before, "and no refusal was said: {:?}", a.notice);
    // The three letters the old bindings took, one at a time, each on an empty composer.
    for (c, id) in [('f', "s-fix"), ('x', "s-x"), ('u', "s-u")] {
        let mut a = a_head(vec![
            in_tree("s-habr", "the habr article", false),
            in_tree(id, "a name", false),
        ]);
        let before = a.notice.clone();
        a.key(Key::Char(c));
        assert_eq!(a.editor.text(), c.to_string(), "`{c}` is a letter");
        assert_eq!(a.removal_armed, None, "`{c}` armed nothing");
        assert_eq!(a.notice, before, "`{c}` said nothing");
    }
}

/// **The row this head is standing in cannot be removed** - the list is the whole answer to
/// *where am I*, and a list that removes where you are standing has none.
#[test]
fn the_session_you_are_in_cannot_be_removed() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
    ]);
    a.picker_sel = row_of(&a, "s-habr");
    a.key(Key::Delete);
    a.key(Key::Delete);
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-rust"],
        "the row the head is standing in is still there"
    );
    let said = said(&a);
    assert!(said.contains("s-habr is the session you are in"), "{said}");
    assert!(said.contains("where am I"), "{said}");
    assert_eq!(a.removal_armed, None, "and nothing was armed");
}

/// **A session ABOVE the one this head is in cannot be removed either.** The row you are standing
/// in is drawn through its chain, so removing a parent takes the row with it - the same defect as
/// removing the row itself, one step out, and it was reachable before this test.
#[test]
fn an_ancestor_of_the_session_you_are_in_cannot_be_removed() {
    let mut a = a_head_at(
        "s-sub-1",
        vec![
            in_tree("s-habr", "the habr article", false),
            child("s-sub-1", "read the repo", "s-habr"),
            child("s-sub-2", "and again", "s-sub-1"),
        ],
    );
    // The family view: the parent and the row this head is in. Its own child is folded under it,
    // which is the collapsed default doing its job inside a family too.
    assert_eq!(drawn_ids(&a), vec!["s-habr", "s-sub-1"]);
    a.picker_sel = row_of(&a, "s-habr");
    a.key(Key::Delete);
    a.key(Key::Delete);
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-sub-1"],
        "the chain is still on the list, link by link"
    );
    let first = said(&a);
    assert!(
        first.contains("s-habr is above the session you are in"),
        "{first}"
    );
    assert!(
        first.contains("would take s-sub-1 off this list"),
        "the refusal names the row that would go with it: {first}"
    );
    assert_eq!(a.removal_armed, None, "and nothing was armed");
    // **The grandparent too**, which is the same walk one link further up.
    let mut deep = a_head_at(
        "s-sub-2",
        vec![
            in_tree("s-habr", "the habr article", false),
            child("s-sub-1", "read the repo", "s-habr"),
            child("s-sub-2", "and again", "s-sub-1"),
        ],
    );
    deep.remove_row(deep.index_of("s-habr").expect("the grandparent"));
    assert!(
        said(&deep).contains("is above the session you are in"),
        "{}",
        said(&deep)
    );
    assert_eq!(deep.removed.len(), 0);
}

/// **A removed parent takes its subtree with it** - a child whose parent is not on a tree has no
/// place on it, and the sentence says how many rows go.
#[test]
fn removing_a_parent_takes_its_children_off_the_list() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
        child("s-sub-1", "read the repo", "s-rust"),
        child("s-sub-2", "and again", "s-sub-1"),
    ]);
    // The parent has to be OPEN for its children to be on the list at all.
    a.set_row_fold("s-rust", true);
    a.set_row_fold("s-sub-1", true);
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-rust", "s-sub-1", "s-sub-2"]
    );
    a.picker_sel = row_of(&a, "s-rust");
    a.key(Key::Delete);
    assert!(
        said(&a).contains("the 1 session drawn under it"),
        "the sentence counts the rows that go with it: {}",
        said(&a)
    );
    a.key(Key::Delete);
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr"],
        "the parent and both of its descendants are off the list"
    );
    // And the project's count is the count of what it DRAWS, not of what it holds.
    let line = a
        .session_rows()
        .iter()
        .find(|r| r.idx.is_none() && r.depth == 0)
        .map(|r| r.node.title())
        .expect("the project's line");
    assert_eq!(line, "letibot (1)", "a count that lies is worse than none");
}

/// Landing on a row you removed brings it back. `/switch <name>` searches every session the daemon
/// holds, not the drawn rows, so a person can switch into a row this list is hiding - and the
/// `Hello` that seats them drops the removal.
#[test]
fn landing_on_a_removed_row_brings_it_back() {
    let sessions = vec![
        in_tree("s-habr", "the habr article", false),
        child("s-sub-1", "read the repo", "s-habr"),
    ];
    let mut a = a_head(sessions.clone());
    // The child is folded under `here` until the row is opened - the collapsed default inside a
    // tree, which is where this fixture has to stand for the child to be removable at all.
    a.set_row_fold("s-habr", true);
    a.picker_sel = row_of(&a, "s-sub-1");
    a.key(Key::Delete);
    a.key(Key::Delete);
    assert_eq!(drawn_ids(&a), vec!["s-habr"], "the child's row is gone");
    // The head is moved onto it - the daemon seats it, which is one `Hello`.
    a.apply(hello("s-sub-1", sessions, Hub::new("s-sub-1").snapshot()));
    assert_eq!(a.removed.len(), 0, "the removal went with the seating");
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-sub-1"],
        "and the row this head is standing in is on its own list"
    );
}

/// The chain to where this head stands is never off the list, whatever the list says: the refusal,
/// the seating and the predicate are three doors on one invariant and this test goes through the
/// third - a list naming the parent of this head's session must leave the family view as it was.
#[test]
fn a_removal_above_here_is_not_a_removal_at_all() {
    let mut a = a_head_at(
        "s-sub-1",
        vec![
            in_tree("s-habr", "the habr article", false),
            child("s-sub-1", "read the repo", "s-habr"),
            child("s-sub-2", "and again", "s-sub-1"),
        ],
    );
    // Straight into the list, which is the state the reviewer described: a row above `here` marked
    // removed, and no call site to refuse it.
    a.removed.push("s-habr".into());
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-sub-1"],
        "the family view is unchanged: the parent, and the row the head is in"
    );
    assert!(
        !a.removed_chain(a.index_of("s-sub-1").expect("the row the head is in")),
        "and the predicate says the chain is not removed"
    );
}

/// The two drawings agree about a removed row, and the LAYER is one of them: it draws from
/// `App::sessions_feed`, so a removal has to be applied there - and this asks the layer, with a real
/// `Host` over the modules this binary carries, not the head's own fallback dressed as one.
#[test]
fn the_heads_list_and_the_layers_list_both_drop_the_removed_row() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
    ]);
    with_the_layer(&mut a);
    assert!(
        a.sessions_list(100).is_some(),
        "the built-in `sessions` layer answers in this process"
    );
    a.picker_sel = row_of(&a, "s-rust");
    a.key(Key::Delete);
    a.key(Key::Delete);
    let head: Vec<String> = a
        .session_rows()
        .iter()
        .filter_map(|r| r.idx.map(|i| a.sessions[i].session_id.clone()))
        .collect();
    assert_eq!(head, vec!["s-habr"], "the head's own walk");
    // **The layer's answer, not the fallback's** - `sessions_list` is `Some` above, so these rows
    // came through the guest, the feed and the tree block it answers.
    let layer: Vec<String> = a
        .sessions_list(100)
        .expect("the layer answers")
        .rows
        .iter()
        .filter_map(|r| r.session.map(|i| a.sessions[i].session_id.clone()))
        .collect();
    assert_eq!(
        layer, head,
        "and the layer's, which is what the person sees"
    );
    // **And the feed itself says so**, which is the mechanism: the layer cannot draw a row the
    // feed does not carry.
    let feed = a.sessions_feed();
    let ids: Vec<&str> = feed["sessions"]
        .as_array()
        .expect("the feed's sessions")
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["s-habr"],
        "the removed session is not in the feed"
    );
    // By name it is still reachable: a removal is about the row, not the session - `/switch` searches
    // every session the daemon holds, so a hidden conversation is one keystroke away. It is
    // `ResumeSession` here only because the fixture's session is not live in the daemon.
    assert_eq!(
        a.pick("rust"),
        Some(Action::ResumeSession("s-rust".into())),
        "a removed row is still a session this head can name"
    );
}

/// `/forget` refuses a live session by name, and the sentence carries four facts: which session,
/// that it is live, what a forget would cost, and that nothing was done. A key must not take away
/// something that is running.
#[test]
fn forget_refuses_a_live_session_by_name() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-busy", "a conversation still open", true),
    ]);
    assert_eq!(a.command("forget s-busy"), None);
    let first = said(&a);
    assert!(first.contains("s-busy is not forgotten"), "{first}");
    assert!(first.contains("the daemon is holding it"), "{first}");
    assert!(first.contains("1 head attached"), "the head count: {first}");
    assert!(
        first.contains("6 transcript row(s)"),
        "what it would cost: {first}"
    );
    assert!(first.contains("Nothing was removed"), "{first}");
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-busy"],
        "a refusal is not a removal"
    );
    assert_eq!(a.removed.len(), 0, "and nothing was hidden either");
}

/// **A dead session that holds rows cannot be forgotten by anybody** - `transcript_item` carries a
/// `BEFORE DELETE` trigger, so `letibot --delete` refuses it too, and the sentence says which rule
/// it is rather than blaming this head.
#[test]
fn forget_names_the_append_only_rule_for_a_session_that_holds_rows() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-old", "a conversation from last week", false),
    ]);
    a.command("forget s-old");
    let first = said(&a);
    assert!(first.contains("s-old is not forgotten"), "{first}");
    assert!(
        first.contains("the daemon is not holding it"),
        "the other half of the two cases: {first}"
    );
    assert!(first.contains("BEFORE DELETE trigger"), "{first}");
    assert!(first.contains("letibot --delete s-old"), "{first}");
}

/// **The empty case is the one the store CAN forget - and the door is not this head's.** This is
/// the finding, as a sentence: `Store::delete_empty_session` exists, no frame reaches it, and the
/// one-shot on the store file is the only way in.
#[test]
fn forget_says_which_door_could_forget_an_empty_session() {
    let mut empty = in_tree("s-never-used", "a session nothing was said in", false);
    empty.status.items = 0;
    empty.stored_items = 0;
    let mut a = a_head(vec![in_tree("s-habr", "the habr article", false), empty]);
    a.command("forget s-never-used");
    let first = said(&a);
    assert!(
        first.contains("it holds no rows and nothing is under it"),
        "{first}"
    );
    assert!(
        first.contains("`letibot --delete s-never-used` is the door that would"),
        "{first}"
    );
    assert!(
        first.contains("this head has no frame that reaches the store's delete"),
        "the finding itself: {first}"
    );
}

/// `/forget` resolves the session it is given and refuses to guess - an id or a unique prefix, the
/// same reading `/switch` gives its argument - and a refusal names what it found rather than taking
/// the first match: forgetting the wrong conversation is not a keystroke to take back.
#[test]
fn forget_resolves_the_id_it_is_given() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust-1", "the rust rewrite", false),
        in_tree("s-rust-2", "the rust rewrite, again", false),
    ]);
    a.command("forget rust");
    assert!(
        said(&a).contains("2 sessions match \"rust\""),
        "an ambiguous prefix is refused by count: {}",
        said(&a)
    );
    assert!(said(&a).contains("will not guess"), "{}", said(&a));
    a.command("forget nothing-like-this");
    assert!(said(&a).contains("no session matches"), "{}", said(&a));
    // **A unique one resolves and is then refused for the real reason** - which is the point: the
    // resolution works and the answer is still no, with the sentence that says why.
    a.command("forget s-rust-1");
    assert!(
        said(&a).contains("s-rust-1 is not forgotten"),
        "{}",
        said(&a)
    );
}

/// **A key pressed on a group's line says what the key is for** rather than doing nothing.
#[test]
fn del_on_a_group_line_says_what_the_key_is_for() {
    let mut keeper = in_tree("s-keeper", "You are the notes keeper. tidy", false);
    keeper.origin = SessionOrigin::Operator;
    let mut a = a_head(vec![in_tree("s-habr", "the habr article", false), keeper]);
    // The project's line is the top row of the tree, and it is not a session.
    a.picker_sel = 0;
    a.key(Key::Delete);
    assert!(
        said(&a).contains("that line is a group, not a session"),
        "{}",
        said(&a)
    );
    assert_eq!(a.removal_armed, None);
}

/// **Any other key on the pane disarms** - the second press has to be a press, not a coincidence
/// minutes later.
#[test]
fn another_key_disarms_the_removal() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
        in_tree("s-pkg", "make me a landing page", false),
    ]);
    a.picker_sel = row_of(&a, "s-rust");
    a.key(Key::Delete);
    assert!(a.removal_armed.is_some());
    a.key(Key::Down);
    assert_eq!(a.removal_armed, None, "an arrow is not the confirmation");
    a.key(Key::Delete);
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-rust", "s-pkg"],
        "so this `del` is a first press again"
    );
}

/// **The bar offers the keys, and the destructive sense as the verb it is** - a bar that promised
/// one press would be a bar that removed something on the first one.
#[test]
fn the_bar_offers_the_removal_keys() {
    let a = a_head(vec![in_tree("s-habr", "the habr article", false)]);
    let bar = a.hint_bar(200);
    assert!(
        bar.contains("del removes it from this list (twice)"),
        "{bar}"
    );
    assert!(bar.contains("ctrl-z brings the last removed back"), "{bar}");
    // **And nothing on the bar is a bare letter** - the picker's typed line is a name.
    assert!(
        !bar.contains(" f ") && !bar.contains(" x ") && !bar.contains(" u "),
        "no letter of the alphabet is a key here: {bar}"
    );
}
