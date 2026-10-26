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

/// **A number that names a group's line is refused with the reason**, rather than switching to the
/// session above or below it — the numbering is what the screen draws, and a group's line is one
/// of its rows.
#[test]
fn a_typed_number_on_a_group_line_says_what_it_is() {
    let mut a = a_daemon_with_every_kind();
    a.push_surface(Surface::Picker);
    // Row 4 of the drawn list is `[subagents]`: two mine rows above it, numbered from 1.
    assert_eq!(a.pick("4"), None);
    let screen = a.screen(100, 43).join("\n");
    assert!(
        screen.contains("is the subagents group's line"),
        "the number is answered rather than acted on:\n{screen}"
    );
    // And a number that names a session still switches — row 3 is the second conversation, the
    // first being the one this head is already in.
    assert_eq!(
        a.pick("3"),
        Some(Action::Switch("s-rust".into())),
        "row 3 is the second conversation"
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
