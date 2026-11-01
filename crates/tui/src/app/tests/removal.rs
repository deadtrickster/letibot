//! **Taking a session off the list, and what this head refuses to pretend it can do.**
//!
//! The operator, on the pane they use most: *"and add controls to remove sessions"*. There are
//! three senses of *remove* and they are different — hide it from this pane, forget it in the
//! store, or close it and then forget it — and `app::removal` is the argument for the one built
//! here. These are the tests of that argument:
//!
//! * `x` takes a row off the list, twice over (armed, then removed), and `u` puts it back;
//! * the row this head is standing in cannot go — the list is the answer to *where am I*;
//! * `f` refuses, by name, and the refusal says what a forget would cost and which door could do
//!   it — a live session first of all, because the daemon is holding it.

use super::*;
use letibot_sessionlog::SessionOrigin;

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
    let mut a = app();
    a.home = "/home/dead".into();
    a.apply(hello("s-habr", sessions, Hub::new("s-habr").snapshot()));
    a.push_surface(Surface::Picker);
    a
}

/// The ids of the session rows the list draws, in order — the drawn list, whichever of the two
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

/// What the status line says — the one sentence `say` keeps.
fn said(a: &App) -> String {
    a.notice.clone().unwrap_or_default()
}

/// **`x` is asked twice: the first press says what would go, the second removes it.**
///
/// The first press must not remove anything — that is the whole of "ask for the act twice" — and
/// the sentence has to name the row, the subtree under it, and where it stays.
#[test]
fn x_asks_before_it_removes() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
        child("s-sub-1", "read the repo", "s-habr"),
    ]);
    a.picker_sel = row_of(&a, "s-rust");
    assert_eq!(a.key(Key::Char('x')), None);
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
    assert!(first.contains("u brings it back"), "{first}");
    // The second press is the removal.
    a.key(Key::Char('x'));
    assert_eq!(drawn_ids(&a), vec!["s-habr"], "the row is gone");
    assert!(
        said(&a).contains("1 session is removed"),
        "and the list says how much it is hiding: {}",
        said(&a)
    );
}

/// **`u` brings the last one back**, and says how many are still off the list.
#[test]
fn u_brings_the_last_removed_row_back() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
        in_tree("s-pkg", "make me a landing page", false),
    ]);
    for id in ["s-rust", "s-pkg"] {
        a.picker_sel = row_of(&a, id);
        a.key(Key::Char('x'));
        a.key(Key::Char('x'));
    }
    assert_eq!(drawn_ids(&a), vec!["s-habr"]);
    a.key(Key::Char('u'));
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-pkg"],
        "the LAST removed one, back where it was"
    );
    assert!(said(&a).contains("1 session is removed"), "{}", said(&a));
    a.key(Key::Char('u'));
    assert_eq!(drawn_ids(&a), vec!["s-habr", "s-rust", "s-pkg"]);
    assert!(said(&a).contains("nothing is removed"), "{}", said(&a));
    // And one more `u` says there is nothing to bring back rather than doing something silent.
    a.key(Key::Char('u'));
    assert!(said(&a).contains("nothing is removed"), "{}", said(&a));
}

/// **The row this head is standing in cannot be removed** — the list is the whole answer to
/// *where am I*, and a list that removes where you are standing has none.
#[test]
fn the_session_you_are_in_cannot_be_removed() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
    ]);
    a.picker_sel = row_of(&a, "s-habr");
    a.key(Key::Char('x'));
    a.key(Key::Char('x'));
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

/// **A removed parent takes its subtree with it** — a child whose parent is not on a tree has no
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
    a.key(Key::Char('x'));
    assert!(
        said(&a).contains("the 1 session drawn under it"),
        "the sentence counts the rows that go with it: {}",
        said(&a)
    );
    a.key(Key::Char('x'));
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

/// **The two drawings agree about a removed row** — the built-in layer draws from the feed, and
/// the feed is where the removal is applied for it. A layer that went on drawing a removed row
/// would be a list whose keys act on rows the person cannot see.
#[test]
fn the_heads_list_and_the_layers_list_both_drop_the_removed_row() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
    ]);
    a.picker_sel = row_of(&a, "s-rust");
    a.key(Key::Char('x'));
    a.key(Key::Char('x'));
    let head: Vec<String> = a
        .session_rows()
        .iter()
        .filter_map(|r| r.idx.map(|i| a.sessions[i].session_id.clone()))
        .collect();
    assert_eq!(head, vec!["s-habr"], "the head's own walk");
    let layer: Vec<String> = a
        .picker_rows()
        .iter()
        .filter_map(|r| r.session.map(|i| a.sessions[i].session_id.clone()))
        .collect();
    assert_eq!(layer, head, "and the drawn list, which is the layer's");
    // **By name it is still reachable**: a removal is about the row, not the session — `/switch`
    // searches every session the daemon holds, so a hidden conversation is one keystroke away.
    // (This one is `ResumeSession` because the fixture's session is not live in the daemon: the
    // point is that `pick` still finds it at all.)
    assert_eq!(
        a.pick("rust"),
        Some(Action::ResumeSession("s-rust".into())),
        "a removed row is still a session this head can name"
    );
}

/// **`f` refuses a live session by name** — the destructive sense, and the one refusal the
/// operator's own history asks for: a key must not take away something that is running.
///
/// The sentence has to carry four facts: which session, that it is live, what a forget would cost,
/// and that nothing was done.
#[test]
fn f_refuses_a_live_session_by_name() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-busy", "a conversation still open", true),
    ]);
    a.picker_sel = row_of(&a, "s-busy");
    a.key(Key::Char('f'));
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

/// **A dead session that holds rows cannot be forgotten by anybody** — `transcript_item` carries a
/// `BEFORE DELETE` trigger, so `letibot --delete` refuses it too, and the sentence says which rule
/// it is rather than blaming this head.
#[test]
fn f_names_the_append_only_rule_for_a_session_that_holds_rows() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-old", "a conversation from last week", false),
    ]);
    a.picker_sel = row_of(&a, "s-old");
    a.key(Key::Char('f'));
    let first = said(&a);
    assert!(first.contains("s-old is not forgotten"), "{first}");
    assert!(
        first.contains("the daemon is not holding it"),
        "the other half of the two cases: {first}"
    );
    assert!(first.contains("BEFORE DELETE trigger"), "{first}");
    assert!(first.contains("letibot --delete s-old"), "{first}");
}

/// **The empty case is the one the store CAN forget — and the door is not this head's.** This is
/// the finding, as a sentence: `Store::delete_empty_session` exists, no frame reaches it, and the
/// one-shot on the store file is the only way in.
#[test]
fn f_says_which_door_could_forget_an_empty_session() {
    let mut empty = in_tree("s-never-used", "a session nothing was said in", false);
    empty.status.items = 0;
    empty.stored_items = 0;
    let mut a = a_head(vec![in_tree("s-habr", "the habr article", false), empty]);
    a.picker_sel = row_of(&a, "s-never-used");
    a.key(Key::Char('f'));
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

/// **A key pressed on a group's line says what the key is for** rather than doing nothing.
#[test]
fn x_on_a_group_line_says_what_the_key_is_for() {
    let mut keeper = in_tree("s-keeper", "You are the notes keeper. tidy", false);
    keeper.origin = SessionOrigin::Operator;
    let mut a = a_head(vec![in_tree("s-habr", "the habr article", false), keeper]);
    // The project's line is the top row of the tree, and it is not a session.
    a.picker_sel = 0;
    a.key(Key::Char('x'));
    assert!(
        said(&a).contains("that line is a group, not a session"),
        "{}",
        said(&a)
    );
    assert_eq!(a.removal_armed, None);
}

/// **Any other key disarms** — the second press has to be a press, not a coincidence minutes later.
#[test]
fn another_key_disarms_the_removal() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
        in_tree("s-pkg", "make me a landing page", false),
    ]);
    a.picker_sel = row_of(&a, "s-rust");
    a.key(Key::Char('x'));
    assert!(a.removal_armed.is_some());
    a.key(Key::Down);
    assert_eq!(a.removal_armed, None, "an arrow is not the confirmation");
    a.key(Key::Char('x'));
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-rust", "s-pkg"],
        "so this `x` is a first press again"
    );
}

/// **The bar offers the three keys**, and says `x` is asked twice — a key whose help promised one
/// press would be a key that removes something on the first one.
#[test]
fn the_bar_offers_the_removal_keys() {
    let a = a_head(vec![in_tree("s-habr", "the habr article", false)]);
    let bar = a.hint_bar(200);
    assert!(bar.contains("x removes it from this list (twice)"), "{bar}");
    assert!(bar.contains("u brings the last removed back"), "{bar}");
    assert!(
        bar.contains("f forgets it — this head has no store door"),
        "a key that only refuses says so on the bar: {bar}"
    );
}
