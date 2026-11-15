//! The session list's live filter: the composer's line narrows the list as it is typed, a hit
//! inside a folded group is drawn, and Enter takes the row under the cursor.

use super::*;

const LETIBOT: &str = "/home/dead/Projects/letibot/letibot";
const OTHER: &str = "/home/dead/Projects/other";

fn at(id: &str, title: &str, workspace: &str) -> SessionBrief {
    let mut b = brief(id, title, false);
    b.wiring.workspace = workspace.into();
    b
}

/// A head in `s-habr`, the list open. `s-bench` is folded under `s-pkg`, and `s-far` sits in a
/// project this head is not in, which starts folded too.
fn a_head() -> App {
    let mut bench = at("s-bench", "the rust bench", LETIBOT);
    bench.parent_session_id = Some("s-pkg".into());
    let sessions = vec![
        at("s-habr", "the habr article", LETIBOT),
        at("s-rust", "the rust rewrite", LETIBOT),
        at("s-pkg", "make me a landing page", LETIBOT),
        bench,
        at("s-far", "rust elsewhere", OTHER),
    ];
    let mut a = app();
    a.home = "/home/dead".into();
    a.apply(hello("s-habr", sessions, Hub::new("s-habr").snapshot()));
    a.key(Key::CtrlS);
    a
}

fn drawn_ids(a: &App) -> Vec<String> {
    a.picker_rows()
        .iter()
        .filter_map(|r| r.session)
        .map(|i| a.sessions[i].session_id.clone())
        .collect()
}

#[test]
fn three_typed_characters_narrow_the_list_without_enter() {
    let mut a = a_head();
    let before = drawn_ids(&a);
    assert!(!before.contains(&"s-bench".to_string()), "{before:?}");
    assert!(!before.contains(&"s-far".to_string()), "{before:?}");
    for c in "rus".chars() {
        assert_eq!(a.key(Key::Char(c)), None, "typing sends nothing");
    }
    let after = drawn_ids(&a);
    assert_eq!(after.len(), 3, "{after:?}");
    for id in ["s-rust", "s-bench", "s-far"] {
        assert!(after.contains(&id.to_string()), "{id} missing: {after:?}");
    }
    let screen = a.screen(110, 30).join("\n");
    assert!(screen.contains("the rust bench"), "{screen}");
    assert!(screen.contains("rust elsewhere"), "{screen}");
    assert!(
        screen.contains("3 matches"),
        "the search field counts: {screen}"
    );
    assert!(!screen.contains("make me a landing page"), "{screen}");
}

#[test]
fn the_cursor_starts_on_the_top_hit_and_enter_switches_to_it() {
    let mut a = a_head();
    typed(&mut a, "landing");
    assert_eq!(drawn_ids(&a), vec!["s-pkg".to_string()]);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-pkg".into())));
    assert!(a.picker_filter.is_empty(), "the line went with the switch");
}

#[test]
fn enter_takes_the_hit_the_arrows_moved_to() {
    let mut a = a_head();
    typed(&mut a, "rust");
    let hits = drawn_ids(&a);
    assert!(hits.len() > 1, "{hits:?}");
    a.key(Key::Down);
    let rows = a.picker_rows();
    let under = rows[a.picker_sel]
        .session
        .map(|i| a.sessions[i].session_id.clone());
    let under = under.expect("the cursor is on a session");
    assert_eq!(a.key(Key::Enter), Some(Action::Switch(under)));
}

#[test]
fn a_number_or_a_command_is_not_a_filter() {
    let mut a = a_head();
    let before = drawn_ids(&a);
    typed(&mut a, "2");
    assert_eq!(drawn_ids(&a), before, "a number names a row");
    a.key(Key::Backspace);
    typed(&mut a, "/new rust");
    assert!(a.picker_filter.is_empty());
    assert_eq!(drawn_ids(&a), before);
}

#[test]
fn letters_in_order_are_a_fallback_only() {
    let mut a = a_head();
    typed(&mut a, "rwrt");
    assert_eq!(
        drawn_ids(&a),
        vec!["s-rust".to_string()],
        "no session contains it, so letters in order match"
    );
}
