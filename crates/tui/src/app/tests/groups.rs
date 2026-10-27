//! **My conversations and the harness's, on one screen** — the session picker's groups.
//!
//! The operator, 2026-10-26: *"in session view the automatic sessions must be grouped somehow, so
//! my vs auto. otherwise im lost in subagents."* A daemon holds one conversation of theirs and
//! thirty rows of work it did for that conversation — task children, the review queue's own
//! session and its reviewers, whatever a package started, the leftovers of `--prompt` runs — and a
//! list ordered by *when something last happened* put a reviewer above the thing they were doing.
//!
//! The rule itself is `letibot_sessionlog::origin`'s (it is not a rendering question: `--sessions`
//! draws the same groups). What is asserted here is the head's half: the line, its count, the fold,
//! the order, and that a key on a group's line acts on the group rather than on a session that is
//! not there.

use super::*;
use letibot_sessionlog::SessionOrigin;

fn brief_of(id: &str, title: &str, origin: SessionOrigin) -> SessionBrief {
    let mut b = brief(id, title, false);
    b.origin = origin;
    b
}

fn child_of(id: &str, title: &str, parent: &str) -> SessionBrief {
    let mut b = brief_of(id, title, SessionOrigin::Operator);
    b.parent_session_id = Some(parent.into());
    b
}

/// A daemon with one of each kind, which is the shape the operator's own store had.
fn a_daemon_with_every_kind() -> App {
    let queue = letibot_sessionlog::gatekeeper_session_for_key("0123456789ab");
    let mut reviewer = brief_of(
        &format!("{queue}-sub-1791759365511364772"),
        "You are the gatekeeper. review it",
        SessionOrigin::Operator,
    );
    reviewer.parent_session_id = Some(queue.clone());
    let mut a = app();
    a.apply(hello(
        "s-habr",
        vec![
            brief_of("s-habr", "the habr article", SessionOrigin::Operator),
            brief_of("s-rust", "the rust rewrite", SessionOrigin::Operator),
            child_of("s-sub-1", "read the repo", "s-habr"),
            child_of("s-sub-2", "run the tests", "s-habr"),
            child_of("s-sub-2-deep", "and again", "s-sub-2"),
            brief_of(&queue, "the review queue", SessionOrigin::Queue),
            reviewer,
            brief_of("s-pkg-1", "make me a landing page", SessionOrigin::Package),
            brief_of("s-shot-1", "one prompt, one answer", SessionOrigin::OneShot),
        ],
        Hub::new("s-habr").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    a
}

/// Every row as a person reads it: a group's line in `[brackets]`, a session by its title.
fn rows(a: &App) -> Vec<String> {
    a.session_rows()
        .iter()
        .map(|r| match r.idx {
            Some(i) => a.sessions[i].title.clone(),
            None => format!("[{}]", r.group.title()),
        })
        .collect()
}

/// **Mine first, then one line per automatic kind with its count** — and a daemon with thirty
/// children costs four lines instead of thirty.
#[test]
fn the_list_is_mine_first_and_then_one_folded_line_per_kind() {
    let mut a = a_daemon_with_every_kind();
    assert_eq!(
        rows(&a),
        vec![
            "[mine]",
            "the habr article",
            "the rust rewrite",
            "[subagents]",
            "[review queue]",
            "[packages]",
            "[one-shots]",
        ],
        "the groups and their folded rows"
    );
    let screen = a.screen(100, 48).join("\n");
    for line in [
        "subagents (3)",
        "review queue (2)",
        "packages (1)",
        "one-shots (1)",
    ] {
        assert!(
            screen.contains(line),
            "the group line {line:?} is not drawn:\n{screen}"
        );
    }
    // **The conversation is on the screen and the work is not**, which is the whole of the ask.
    assert!(screen.contains("the habr article"), "{screen}");
    assert!(
        !screen.contains("read the repo"),
        "a subagent was drawn while its group is folded:\n{screen}"
    );
}

/// **The count is the group's, and a list of one kind is a plain list** — no `mine` header over
/// the only rows there are, and no fold saying `(0)` for a kind the daemon has nothing of.
#[test]
fn a_list_of_one_kind_draws_no_header_and_no_empty_fold() {
    let mut a = app();
    a.apply(hello(
        "s-only",
        vec![brief_of("s-only", "the only one", SessionOrigin::Operator)],
        Hub::new("s-only").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    assert_eq!(rows(&a), vec!["the only one"]);
    let screen = a.screen(100, 30).join("\n");
    assert!(!screen.contains("subagents (0)"), "{screen}");
    assert!(
        !screen.contains("mine ("),
        "a header over the only rows there are separates nothing:\n{screen}"
    );
}

/// **`mine` is never folded**, because the operator's own conversations are why the list is open —
/// and the key says so rather than redrawing nothing.
#[test]
fn mine_is_always_open_and_the_key_says_so() {
    let mut a = a_daemon_with_every_kind();
    a.picker_sel = a
        .session_rows()
        .iter()
        .position(|r| r.idx.is_none() && r.group.is_mine())
        .expect("the mine line");
    assert_eq!(a.key(Key::Left), None);
    assert!(a.open_groups.is_empty(), "nothing was folded");
    assert_eq!(
        rows(&a)[0],
        "[mine]",
        "mine's rows are still under it after the key that would fold a group"
    );
    let screen = a.screen(100, 48).join("\n");
    assert!(
        screen.contains("mine is always open"),
        "a key that did nothing must say why:\n{screen}"
    );
}

/// **`→` and `enter` on a group's line open it, and the child is then drawn under its parent
/// inside the group** — the tree is not lost, it is one fold deeper.
#[test]
fn opening_a_group_draws_its_tree_under_its_parent() {
    let mut a = a_daemon_with_every_kind();
    a.picker_sel = a
        .session_rows()
        .iter()
        .position(|r| r.idx.is_none() && r.group == letibot_sessionlog::SessionGroup::Subagents)
        .expect("the subagents line");
    assert_eq!(a.key(Key::Right), None);
    assert_eq!(
        rows(&a),
        vec![
            "[mine]",
            "the habr article",
            "the rust rewrite",
            "[subagents]",
            "read the repo",
            "run the tests",
            "[review queue]",
            "[packages]",
            "[one-shots]",
        ],
        "a child of a MINE conversation is a root of the group's tree — its parent is not on this \
         list — and the grandchild stays folded under its own parent"
    );
    // **The grandchild is under its own parent, one fold deeper still.**
    let at = a
        .session_rows()
        .iter()
        .position(|r| r.idx.is_some_and(|i| a.sessions[i].session_id == "s-sub-2"))
        .expect("the sibling");
    a.picker_sel = at;
    a.key(Key::Right);
    let deep = a
        .session_rows()
        .iter()
        .position(|r| {
            r.idx
                .is_some_and(|i| a.sessions[i].session_id == "s-sub-2-deep")
        })
        .expect("the grandchild, under the session that spawned it");
    assert!(
        a.session_rows()[deep].depth > a.session_rows()[at].depth,
        "the grandchild is not indented under its parent"
    );
}

/// **A number is the n-th SESSION, not the n-th row** — `/switch 2` takes the second
/// conversation even though its row is the third on the screen, and a group's line, which has no
/// number at all, cannot be reached by one.
///
/// This is the semantic the port to rano's tree list moved: the list numbers the sessions it
/// draws and nothing else (`rano::agent::picker`), so a number is somewhere to GO and a group's
/// line — which you can only fold — is not.
#[test]
fn a_number_names_the_nth_session_and_a_group_line_has_none() {
    let mut a = a_daemon_with_every_kind();
    a.push_surface(Surface::Picker);
    // The drawn order: `[mine]`, the habr article, the rust rewrite, `[subagents]`, …
    assert_eq!(
        rows(&a)[..4],
        [
            "[mine]",
            "the habr article",
            "the rust rewrite",
            "[subagents]"
        ]
    );
    // **Three rows down, the second session.**
    assert_eq!(
        a.pick("3"),
        None,
        "there is no third session on a list with two drawn"
    );
    assert_eq!(
        a.pick("2"),
        Some(Action::Switch("s-rust".into())),
        "the second session, which is the third ROW"
    );
    // A number past the end says so rather than reading as a name that did not match.
    assert_eq!(a.pick("9"), None);
    let screen = a.screen(100, 43).join("\n");
    assert!(
        screen.contains("there is no session 9"),
        "the list is shorter than the number typed:\n{screen}"
    );
}

/// **A number past the last session still searches the names** — `3` is also a thing a title can
/// say, and a list that refused it would have stopped searching for names that look like numbers.
///
/// With two sessions, `2` is the second one and `3` is nobody's number: it falls through to the
/// name search, finds `3 days of work`, and only a number that neither reading matches is
/// answered as a number.
#[test]
fn a_number_past_the_end_still_finds_a_title_that_says_it() {
    let mut a = app();
    a.apply(hello(
        "s-a",
        vec![
            brief("s-a", "the one", true),
            brief("s-b", "3 days of work", false),
        ],
        Hub::new("s-a").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    assert_eq!(
        a.pick("3"),
        Some(Action::Switch("s-b".into())),
        "no third session, but a title that starts with 3"
    );
    assert_eq!(
        a.pick("2"),
        Some(Action::Switch("s-b".into())),
        "and a number that IS one of the list's still wins"
    );
    assert_eq!(a.pick("7"), None, "neither a session nor a name");
    let screen = a.screen(100, 30).join("\n");
    assert!(
        screen.contains("there is no session 7"),
        "the number is answered as a number:\n{screen}"
    );
}

/// **A group's line is drawn without a number**, and the sessions are numbered from 1 in the
/// order they appear — the drawn number and the typed one are one number.
#[test]
fn the_drawn_numbers_are_the_sessions_and_a_group_has_none() {
    let a = a_daemon_with_every_kind();
    // **The list's own rows**, not the frame: the session header at the top of the screen also
    // carries a title, and a `contains` over the whole screen would find that one first.
    let rows = a.picker_lines(100);
    let line = |needle: &str| {
        rows.iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("`{needle}` is drawn:\n{}", rows.join("\n")))
            .clone()
    };
    let before = |text: &str, needle: &str| text[..text.find(needle).unwrap()].to_string();
    // The two conversations carry 1 and 2, whatever is drawn between them.
    let habr = line("the habr article");
    assert!(
        before(&habr, "the habr article").trim().ends_with('1'),
        "the first session is numbered 1: {habr:?}"
    );
    let rust = line("the rust rewrite");
    assert!(
        before(&rust, "the rust rewrite").trim().ends_with('2'),
        "the second session is numbered 2: {rust:?}"
    );
    // **A group's line has no field at all**: nothing before its name is a number. The `(3)`
    // after it is its COUNT, which is why the test reads the part before the word.
    let group = line("subagents (3)");
    assert!(
        !before(&group, "subagents")
            .chars()
            .any(|c| c.is_ascii_digit()),
        "a group's line is not a number you can type: {group:?}"
    );
}

/// **A click on a group's line folds it** — the one row in this head where a click acts rather
/// than marks, because a group has no session to mark for a later Enter.
#[test]
fn a_click_on_a_groups_line_folds_it() {
    let mut a = a_daemon_with_every_kind();
    let screen = a.screen(100, 48);
    let y = screen
        .iter()
        .position(|l| l.contains("subagents (3)"))
        .expect("the subagents line is drawn") as u16;
    let group = letibot_sessionlog::SessionGroup::Subagents;
    assert!(a.open_groups.is_empty(), "it starts folded");
    a.key(Key::Click { x: 6, y });
    assert_eq!(
        a.open_groups,
        vec![group],
        "the click opened the group it landed on"
    );
    assert!(
        a.screen(100, 48).join("\n").contains("read the repo"),
        "and its rows are drawn now"
    );
    a.key(Key::Click { x: 6, y });
    assert!(a.open_groups.is_empty(), "a second click folds it away");
}

/// **The bar says what Enter does on the row under the cursor** — `switches` on a session,
/// `unfolds it` / `folds it away` on a group's line, and the truth on `mine`, where Enter does
/// nothing at all.
///
/// The picker's footer used to say *enter switches* over a group, because the sentence was written
/// once for the list instead of once for the row the cursor is on. This is that sentence read off
/// the row (`Surface::row_says`), the way the subagents and jobs panes already read theirs.
#[test]
fn the_bar_follows_the_cursor_onto_a_groups_line() {
    /// The drawn row of a title, or of a group's `[name]`.
    fn row_of(a: &App, title: &str) -> usize {
        rows(a)
            .iter()
            .position(|r| r == title)
            .unwrap_or_else(|| panic!("{title} is drawn: {:?}", rows(a)))
    }
    let mut a = a_daemon_with_every_kind();
    a.picker_sel = row_of(&a, "the habr article");
    let bar = a.hint_bar(200);
    assert!(bar.contains("enter switches"), "{bar}");
    a.picker_sel = row_of(&a, "[subagents]");
    let bar = a.hint_bar(200);
    assert!(
        bar.contains("enter unfolds it"),
        "a folded group says what Enter does to it: {bar}"
    );
    // Opened by hand, the same row says the other half.
    a.open_groups
        .push(letibot_sessionlog::SessionGroup::Subagents);
    let bar = a.hint_bar(200);
    assert!(bar.contains("enter folds it away"), "{bar}");
    // `mine` is the row where nothing happens, and the bar does not pretend otherwise.
    a.picker_sel = row_of(&a, "[mine]");
    let bar = a.hint_bar(200);
    assert!(
        bar.contains("on mine does nothing"),
        "the one row Enter cannot act on says so: {bar}"
    );
}

/// **A session a package started is a conversation the operator still opens** — it is grouped
/// apart because they did not start it, not because it is not theirs to drive.
#[test]
fn a_packages_conversation_opens_and_switches_like_any_other() {
    let mut a = a_daemon_with_every_kind();
    // Open `packages` (the sixth group line) and enter the session under it.
    let at = a
        .session_rows()
        .iter()
        .position(|r| r.idx.is_none() && r.group == letibot_sessionlog::SessionGroup::Packages)
        .expect("the packages line");
    a.picker_sel = at;
    a.key(Key::Right);
    let row = a.session_rows()[at + 1];
    assert_eq!(
        row.idx.map(|i| a.sessions[i].title.clone()),
        Some("make me a landing page".into()),
        "the package's session is under the packages group"
    );
}
