//! The session list's live filter: the line the list holds narrows it as it is typed, a hit inside a
//! folded group is drawn, and Enter takes the row under the cursor. The line starts EMPTY - a prompt
//! waiting in the composer is the operator's next message, not a query.

use super::*;

const LETIBOT: &str = "/home/dead/Projects/letibot/letibot";
const OTHER: &str = "/home/dead/Projects/other";

fn at(id: &str, title: &str, workspace: &str) -> SessionBrief {
    let mut b = brief(id, title, false);
    b.wiring.workspace = workspace.into();
    b
}

/// A head in `s-habr`, with the list NOT yet open: the state a person is in when they have typed a
/// prompt and have not sent it. `s-bench` is folded under `s-pkg`, and `s-far` sits in a project this
/// head is not in, which starts folded too.
fn a_head_with_the_list_closed() -> App {
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
    a
}

/// The same head with the list open.
fn a_head() -> App {
    let mut a = a_head_with_the_list_closed();
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

/// A prompt waiting in the composer is the operator's next message, not a query: `ctrl-s` opens the
/// list on an EMPTY search, so a sentence that names no session cannot filter every session out of
/// sight - the bug that read a typed line as a search and answered with an empty list.
#[test]
fn a_draft_in_the_composer_is_not_the_search() {
    let mut a = a_head_with_the_list_closed();
    let every = drawn_ids(&a);
    assert!(!every.is_empty(), "the premise: there are sessions to list");
    typed(&mut a, "please look at the bench");
    assert_eq!(a.key(Key::CtrlS), Some(Action::ListSessions));
    assert!(
        a.picker_filter.is_empty(),
        "the draft was taken as the query: {:?}",
        a.picker_filter
    );
    assert_eq!(drawn_ids(&a), every, "every session is on the list");
    // And no search field is drawn, because there is nothing being searched for.
    let screen = a.screen(110, 30).join("\n");
    assert!(
        !screen.contains("match"),
        "a search field for a draft:\n{screen}"
    );
}

/// The list's own line is its search, exactly as before: a letter narrows it, and `esc` comes back
/// to the composer with the draft untouched - the query was a question about the list, and a query
/// is not a message.
#[test]
fn the_lists_own_line_narrows_it_and_esc_gives_the_draft_back() {
    let mut a = a_head_with_the_list_closed();
    let every = drawn_ids(&a);
    typed(&mut a, "the bench, please");
    a.key(Key::CtrlS);
    typed(&mut a, "landing");
    let hits = drawn_ids(&a);
    assert_eq!(a.picker_filter, "landing");
    assert_eq!(
        hits,
        vec!["s-pkg".to_string()],
        "the line narrowed the list to its one hit"
    );
    assert_ne!(hits, every, "the list was narrowed, not left whole");

    assert_eq!(a.key(Key::Esc), None);
    assert!(!a.is_open(Surface::Picker));
    assert_eq!(a.input(), "the bench, please", "the draft came back");
    assert!(a.picker_filter.is_empty(), "the query went with the list");
    assert_eq!(drawn_ids(&a), every, "the list is whole again");
}

/// A phrase is the characters in that order and nothing looser: the letters-in-order fallback that
/// answers `rwrt` below does not answer `"rwrt"`, which is what quoting one is for.
#[test]
fn a_quoted_phrase_matches_the_words_as_typed() {
    let mut a = a_head();
    typed(&mut a, "\"rust bench\"");
    assert_eq!(
        drawn_ids(&a),
        vec!["s-bench".to_string()],
        "the phrase narrowed to the title that holds it"
    );
    // The same letters, unquoted, still reach it by the ladder - and the phrase around them does
    // not, because no title contains `r-b-u-s-t` in that order as words.
    a.key(Key::Esc);
    a.key(Key::CtrlS);
    typed(&mut a, "\"rwrt\"");
    assert_eq!(
        drawn_ids(&a),
        Vec::<String>::new(),
        "a phrase fell back to letters"
    );
}

/// `re:` is a regular expression over the same fields: an id and a title can be asked for at once,
/// which no tier of the ladder can do.
#[test]
fn a_regex_narrows_the_session_list_by_its_pattern() {
    let mut a = a_head();
    typed(&mut a, "re:^s-(pkg|far)$");
    assert_eq!(
        drawn_ids(&a),
        vec!["s-pkg".to_string(), "s-far".to_string()],
        "the pattern matched two ids and the list drew them"
    );
    // The same pattern with a case the ids do not have matches nothing at all.
    a.key(Key::Esc);
    a.key(Key::CtrlS);
    typed(&mut a, "re:^S-PKG$");
    assert_eq!(drawn_ids(&a), Vec::<String>::new());
}

/// A pattern that does not compile keeps no row and says why in the search field: the line is the
/// only place the answer can be, since the list it emptied cannot explain itself.
#[test]
fn a_bad_regex_empties_the_list_and_says_why_in_the_field() {
    let mut a = a_head();
    typed(&mut a, "re:s-(");
    assert_eq!(drawn_ids(&a), Vec::<String>::new(), "a bad pattern matched");
    let screen = a.screen(140, 30).join("\n");
    assert!(
        screen.contains("unclosed group") || screen.contains("regex"),
        "the search field does not say why the list is empty:\n{screen}"
    );
}

/// The other way out: `ctrl-s` again closes the list, and the search it was narrowed with is not
/// left in the composer to be sent as a prompt.
#[test]
fn ctrl_s_again_gives_the_draft_back_and_drops_the_query() {
    let mut a = a_head();
    typed(&mut a, "rust");
    assert_eq!(a.key(Key::CtrlS), None, "the chord closes it");
    assert!(!a.is_open(Surface::Picker));
    assert!(a.input().is_empty(), "the query stayed behind as a prompt");

    // And from a draft, the same round trip gives the operator their words back.
    typed(&mut a, "a line I was writing");
    a.key(Key::CtrlS);
    typed(&mut a, "habr");
    a.key(Key::CtrlS);
    assert_eq!(a.input(), "a line I was writing");
}

/// A multi-line draft is held whole: the composer is emptied for the search and put back line for
/// line, since a draft is not the one line `set_composer` clears.
#[test]
fn a_multi_line_draft_survives_the_list() {
    let mut a = a_head_with_the_list_closed();
    typed(&mut a, "first line");
    a.key(Key::SoftEnter);
    typed(&mut a, "second line");
    a.key(Key::CtrlS);
    assert!(a.input().is_empty(), "the search starts empty");
    a.key(Key::Esc);
    assert_eq!(a.input(), "first line\nsecond line");
}
