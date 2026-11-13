//! Putting a session out of sight from the head, and taking it back: `del` asks twice and the
//! DAEMON does the write, so an archive outlives the head that made it; the archived rows come back
//! as a folded group of their own; `ctrl-z`, `/archive`, `/close` and `/forget` are the verbs, and
//! the row this head stands in is none of them.

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

/// The head with the built-in `sessions` layer actually installed: without it there is no host, and
/// a test meaning to ask the LAYER would be reading the head's own answer instead.
fn with_the_layer(a: &mut App) {
    let project = std::env::temp_dir().join(format!("letibot-archive-{}", std::process::id()));
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

/// The ids of the session rows the list draws, in order - a group's line is not a session.
fn drawn_ids(a: &App) -> Vec<String> {
    a.picker_rows()
        .iter()
        .filter_map(|r| r.session)
        .map(|i| a.sessions[i].session_id.clone())
        .collect()
}

/// Every id the list draws, group lines included.
fn drawn_rows(a: &App) -> Vec<String> {
    a.picker_rows().iter().map(|r| r.id.clone()).collect()
}

/// The row of a session by its id, or a panic naming what is drawn instead.
fn row_of(a: &App, id: &str) -> usize {
    a.picker_rows()
        .iter()
        .position(|r| r.id == id)
        .unwrap_or_else(|| panic!("`{id}` is drawn: {:?}", drawn_rows(a)))
}

/// What the status line says - the one sentence `say` keeps.
fn said(a: &App) -> String {
    a.notice.clone().unwrap_or_default()
}

/// Whether an archive or an unarchive went out on the wire. The seating's own bootstrap asks are
/// queued on the same list, so the question is which actions, not how many.
fn sent_archive(a: &mut App) -> bool {
    a.take_actions().iter().any(|x| {
        matches!(
            x,
            Action::Archive { .. } | Action::Unarchive { .. } | Action::Close { .. }
        )
    })
}

/// `del` is asked twice - the first press must archive nothing, which is the whole of asking twice -
/// and what the second press sends is the DAEMON's frame, not a list the head keeps.
#[test]
fn del_asks_before_it_archives() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
        child("s-sub-1", "read the repo", "s-habr"),
    ]);
    a.picker_sel = row_of(&a, "s-rust");
    a.take_actions();
    assert_eq!(a.key(Key::Delete), None, "the first press archives nothing");
    assert!(
        !sent_archive(&mut a),
        "and sends nothing: the second press is the confirmation"
    );
    assert_eq!(
        a.archive_armed.as_ref().map(|(id, _)| id.clone()),
        Some("s-rust".to_string()),
        "it arms the row it was pressed on"
    );
    let first = said(&a);
    assert!(first.contains("the rust rewrite"), "{first}");
    assert!(
        first.contains("s-rust"),
        "the sentence names the id: {first}"
    );
    assert!(first.contains("ctrl-z brings it back"), "{first}");
    assert!(
        first.contains("every row of it stays in the store"),
        "and what an archive keeps: {first}"
    );
    // The second press is the archive, and it is ONE frame that names the session.
    assert_eq!(
        a.key(Key::Delete),
        Some(Action::Archive {
            session_id: "s-rust".into(),
            reason: crate::app::archive::FROM_THE_LIST.into(),
        })
    );
    assert_eq!(a.archive_armed, None);
    assert_eq!(
        a.archived_last.as_deref(),
        Some("s-rust"),
        "the head remembers what it put away, to be able to bring it back"
    );
    // And the row is still drawn until the daemon says otherwise: the list is not this head's.
    assert_eq!(drawn_ids(&a), vec!["s-habr", "s-rust"]);
}

/// The first Done-when: Del-Del on a row survives quitting and reopening the head. The archive is
/// the store's, so a head that never saw it writes or draws the row - what a reopened head gets is
/// the daemon's list, and the row is in the archived half of it.
#[test]
fn an_archive_survives_the_head_quitting_and_opening_again() {
    let sessions = vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
    ];
    let mut first = a_head(sessions.clone());
    first.picker_sel = row_of(&first, "s-rust");
    first.key(Key::Delete);
    first.key(Key::Delete);
    // Quit here, with the frame written down as the daemon would hold it.
    let mut away = sessions[1].clone();
    away.live = false;
    let listed = vec![sessions[0].clone()];

    // A NEW head: nothing of the first one but what the daemon says.
    let mut reopened = a_head(listed.clone());
    reopened.apply(ServerFrame::Sessions {
        sessions: listed,
        current: "s-habr".into(),
        created: None,
        archived: vec![away],
    });
    with_the_layer(&mut reopened);
    assert!(
        !drawn_ids(&reopened).contains(&"s-rust".to_string()),
        "an archived row is not a row of the tree: {:?}",
        drawn_rows(&reopened)
    );
    let drawn = reopened.picker_lines(100).join("\n");
    assert!(
        drawn.contains("archived (1)"),
        "and the reopened head says where it went:\n{drawn}"
    );
    assert!(
        !drawn.contains("the rust rewrite"),
        "behind the closed group, like every other fold:\n{drawn}"
    );
    // The toggle is the person's, and it is still there to open after the restart.
    reopened.set_row_fold(letibot_plugin::sessions::ARCHIVED_KEY, true);
    let opened = reopened.picker_lines(100).join("\n");
    assert!(
        opened.contains("the rust rewrite"),
        "under the group, once it is opened:\n{opened}"
    );
}

/// The second and third Done-when, on the layer this binary carries: the toggle shows the row, and
/// Enter on it resumes it - and once the daemon has opened it, the row is back in the tree and out of
/// the archived group.
#[test]
fn the_archived_group_shows_the_row_and_enter_resumes_it() {
    let mut a = a_head(vec![in_tree("s-habr", "the habr article", false)]);
    with_the_layer(&mut a);
    let mut away = in_tree("s-rust", "the rust rewrite", false);
    away.live = false;
    a.apply(ServerFrame::Sessions {
        sessions: a.sessions.clone(),
        current: "s-habr".into(),
        created: None,
        archived: vec![away],
    });
    // Folded by default: the group is a LINE saying how many, and the row behind it is not drawn.
    let closed = a.picker_lines(100).join("\n");
    assert!(
        closed.contains("archived (1)"),
        "the group's count is on its line:\n{closed}"
    );
    assert!(
        !closed.contains("the rust rewrite"),
        "a group nobody opened is shut:\n{closed}"
    );
    // The toggle: Enter on the group's line opens it, and the row appears.
    a.picker_sel = row_of(&a, letibot_plugin::sessions::ARCHIVED_KEY);
    assert_eq!(
        a.key(Key::Enter),
        None,
        "a fold is the head's, not the daemon's"
    );
    let opened = a.picker_lines(100).join("\n");
    assert!(
        opened.contains("the rust rewrite"),
        "the toggle shows the row:\n{opened}"
    );
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr"],
        "and it is still not a row of the tree"
    );
    // Enter on the row: a stored session is RESUMED, which is what takes it out of the group - the
    // daemon opens it and clears the archive on the way.
    a.picker_sel = row_of(&a, "s-rust");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::ResumeSession("s-rust".into()))
    );
    // The daemon's answer: the session is live and in the list, and the archived half is empty.
    let mut back = in_tree("s-rust", "the rust rewrite", true);
    back.status.heads = 1;
    a.apply(ServerFrame::Sessions {
        sessions: vec![in_tree("s-habr", "the habr article", false), back],
        current: "s-habr".into(),
        created: Some("s-rust".into()),
        archived: Vec::new(),
    });
    assert_eq!(
        drawn_ids(&a),
        vec!["s-habr", "s-rust"],
        "the row is back in the tree: {:?}",
        drawn_rows(&a)
    );
    let after = a.picker_lines(100).join("\n");
    assert!(
        !after.contains("archived"),
        "and the group went with the last row in it:\n{after}"
    );
}

/// `ctrl-z` is the archive's undo, in the order they were made, and each press says what happened.
#[test]
fn ctrl_z_brings_the_last_archive_back() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
        in_tree("s-pkg", "make me a landing page", false),
    ]);
    a.picker_sel = row_of(&a, "s-rust");
    a.key(Key::Delete);
    a.key(Key::Delete);
    assert_eq!(
        a.key(Key::Undo),
        Some(Action::Unarchive {
            session_id: "s-rust".into()
        })
    );
    assert!(
        said(&a).contains("s-rust is back on the list"),
        "{}",
        said(&a)
    );
    assert_eq!(a.archived_last, None, "the undo is spent");
    // With nothing of this head's to take back, the key says so rather than doing nothing.
    assert_eq!(a.key(Key::Undo), None);
    assert!(
        said(&a).contains("nothing here was archived by this head"),
        "{}",
        said(&a)
    );
}

/// `del` on a row that is already archived is the way back, asked ONCE: putting a row back is not the
/// act that asks twice.
#[test]
fn del_on_an_archived_row_brings_it_back() {
    let mut a = a_head(vec![in_tree("s-habr", "the habr article", false)]);
    let mut away = in_tree("s-rust", "the rust rewrite", false);
    away.live = false;
    a.apply(ServerFrame::Sessions {
        sessions: a.sessions.clone(),
        current: "s-habr".into(),
        created: None,
        archived: vec![away],
    });
    a.set_row_fold(letibot_plugin::sessions::ARCHIVED_KEY, true);
    a.picker_sel = row_of(&a, "s-rust");
    assert_eq!(
        a.key(Key::Delete),
        Some(Action::Unarchive {
            session_id: "s-rust".into()
        }),
        "one press, and it is the other direction"
    );
}

/// A session that is not live is closed with `/close` and only hidden with `/archive`; the row this
/// head stands in is neither, because the daemon closes what it archives and a head left in a closed
/// session can neither be told nor tell.
#[test]
fn the_session_you_are_in_is_neither_archived_nor_closed() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
    ]);
    a.picker_sel = row_of(&a, "s-habr");
    a.key(Key::Delete);
    a.key(Key::Delete);
    assert!(!sent_archive(&mut a), "nothing was sent");
    assert_eq!(a.archive_armed, None, "and nothing was armed");
    let first = said(&a);
    assert!(
        first.contains("s-habr is the session you are in"),
        "{first}"
    );
    assert!(first.contains("Nothing was archived"), "{first}");
    // The verb is asked once and refuses for the same reason.
    assert_eq!(a.command("archive s-habr"), None);
    assert!(
        said(&a).contains("s-habr is the session you are in"),
        "{}",
        said(&a)
    );
    assert_eq!(a.command("close s-habr"), None);
    assert!(
        said(&a).contains("`/close` would leave this head"),
        "{}",
        said(&a)
    );
}

/// A name typed into the picker is a filter, letter by letter: the picker is the one surface whose
/// typed line is not a `/verb`, so a letter bound here would be the first character of a search.
/// `del` and `ctrl-z` are spellings a typed line cannot be.
#[test]
fn a_typed_name_is_never_an_archive() {
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
    assert_eq!(a.archive_armed, None, "and nothing was armed");
    assert_eq!(a.notice, before, "and no refusal was said: {:?}", a.notice);
    for (c, id) in [('f', "s-fix"), ('x', "s-x"), ('u', "s-u")] {
        let mut a = a_head(vec![
            in_tree("s-habr", "the habr article", false),
            in_tree(id, "a name", false),
        ]);
        let before = a.notice.clone();
        a.key(Key::Char(c));
        assert_eq!(a.editor.text(), c.to_string(), "`{c}` is a letter");
        assert_eq!(a.archive_armed, None, "`{c}` armed nothing");
        assert_eq!(a.notice, before, "`{c}` said nothing");
    }
}

/// A key pressed on a group's line says what the key is for rather than doing nothing.
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
    assert_eq!(a.archive_armed, None);
}

/// Another daemon's package is not this head's to archive, and the key says which fact it is: a row
/// of another store, not this list's group line.
#[test]
fn del_on_another_daemons_package_says_whose_row_it_is() {
    use letibot_sessionlog::{FleetRow, FleetState};
    let mut a = a_head(vec![in_tree("s-habr", "the habr article", false)]);
    a.fleet = vec![FleetRow {
        id: "s-far-1".into(),
        title: "work of another folder".into(),
        workspace: "/home/dead/Projects/letibot/wt-a".into(),
        model: "glm-coding/glm-4.7".into(),
        placement: Some("host:lab2x1".into()),
        state: FleetState::Running,
    }];
    a.set_row_fold("fleet/packages", true);
    a.picker_sel = row_of(&a, "s-far-1");
    a.key(Key::Delete);
    let said = said(&a);
    assert!(
        said.contains("another daemon's package"),
        "the sentence names whose row it is: {said}"
    );
    assert_eq!(a.archive_armed, None, "and nothing was armed");
}

/// Any other key on the pane disarms - the second press has to be a press, not a coincidence
/// minutes later.
#[test]
fn another_key_disarms_the_archive() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust", "the rust rewrite", false),
        in_tree("s-pkg", "make me a landing page", false),
    ]);
    a.picker_sel = row_of(&a, "s-rust");
    a.key(Key::Delete);
    assert!(a.archive_armed.is_some());
    a.key(Key::Down);
    assert_eq!(a.archive_armed, None, "an arrow is not the confirmation");
    a.key(Key::Delete);
    assert!(
        !sent_archive(&mut a),
        "so this `del` is a first press again"
    );
}

/// `/archive ID` is asked once because it was typed, and the id is resolved rather than guessed: an
/// ambiguous prefix and a miss are each said by name.
#[test]
fn archive_named_resolves_the_id_it_is_given() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-rust-1", "the rust rewrite", false),
        in_tree("s-rust-2", "the rust rewrite, again", false),
    ]);
    assert_eq!(a.command("archive rust"), None);
    assert!(
        said(&a).contains("2 sessions match \"rust\""),
        "an ambiguous prefix is refused by count: {}",
        said(&a)
    );
    assert!(said(&a).contains("will not guess"), "{}", said(&a));
    assert_eq!(a.command("archive nothing-like-this"), None);
    assert!(said(&a).contains("no session matches"), "{}", said(&a));
    // A unique one archives, in one press.
    assert_eq!(
        a.command("archive s-rust-1"),
        Some(Action::Archive {
            session_id: "s-rust-1".into(),
            reason: crate::app::archive::FROM_THE_LIST.into(),
        })
    );
    // And one that is archived already is said so rather than archived twice.
    let mut away = in_tree("s-old", "last week", false);
    away.live = false;
    a.apply(ServerFrame::Sessions {
        sessions: a.sessions.clone(),
        current: "s-habr".into(),
        created: None,
        archived: vec![away],
    });
    assert_eq!(a.command("archive s-old"), None);
    assert!(said(&a).contains("is archived already"), "{}", said(&a));
}

/// `/close` is the other way out: the daemon lets go of the session and the row stays in the list.
/// A session it is not holding has nothing to close, and says so instead of sending a frame.
#[test]
fn close_lets_go_of_a_session_without_hiding_it() {
    let mut a = a_head(vec![
        in_tree("s-habr", "the habr article", false),
        in_tree("s-busy", "a conversation still open", true),
        in_tree("s-old", "one nobody opened", false),
    ]);
    assert_eq!(
        a.command("close s-busy"),
        Some(Action::Close {
            session_id: "s-busy".into()
        })
    );
    assert!(said(&a).contains("stays on this list"), "{}", said(&a));
    assert_eq!(
        a.command("close s-old"),
        None,
        "a session the daemon is not holding has nothing to close"
    );
    assert!(said(&a).contains("is not live"), "{}", said(&a));
}

/// `/forget` deletes nothing and points at the verb that does exist: what it can name is what it
/// put out of sight, not what it took away.
#[test]
fn forget_points_at_archive_and_deletes_nothing() {
    let mut held = in_tree("s-old", "a conversation from last week", false);
    held.stored_items = 6;
    let mut a = a_head(vec![in_tree("s-habr", "the habr article", false), held]);
    assert_eq!(a.command("forget s-old"), None);
    let first = said(&a);
    assert!(
        first.contains("nothing here is forgotten"),
        "the destructive verb still refuses: {first}"
    );
    assert!(
        first.contains("`/archive s-old` is the door that exists"),
        "and points at the one that does: {first}"
    );
    assert!(
        first.contains("6 transcript row(s)"),
        "what a forget would take, so the refusal is not a shrug: {first}"
    );
    assert!(!sent_archive(&mut a), "and nothing was sent");
}

/// The bar offers both keys, in the archive's own words: a bar that said "removes" would be one
/// promising a delete this tree does not have.
#[test]
fn the_bar_offers_the_archive_keys() {
    let a = a_head(vec![in_tree("s-habr", "the habr article", false)]);
    let bar = a.hint_bar(200);
    assert!(
        bar.contains("del archives it, rows and all (twice)"),
        "{bar}"
    );
    assert!(
        bar.contains("ctrl-z brings the last archived back"),
        "{bar}"
    );
    // And nothing on the bar is a bare letter - the picker's typed line is a name.
    assert!(
        !bar.contains(" f ") && !bar.contains(" x ") && !bar.contains(" u "),
        "no letter of the alphabet is a key here: {bar}"
    );
}

/// The feed carries the archived rows with a bucket of their own and no project: the layer draws
/// what the feed holds, so this is the mechanism behind the group rather than a second opinion
/// about it.
#[test]
fn the_feed_carries_an_archived_row_outside_every_project() {
    let mut a = a_head(vec![in_tree("s-habr", "the habr article", false)]);
    let mut away = in_tree("s-rust", "the rust rewrite", false);
    away.live = false;
    a.apply(ServerFrame::Sessions {
        sessions: a.sessions.clone(),
        current: "s-habr".into(),
        created: None,
        archived: vec![away],
    });
    let feed = a.sessions_feed();
    let rows = feed["sessions"].as_array().expect("the feed's sessions");
    let rust = rows
        .iter()
        .find(|r| r["id"] == "s-rust")
        .expect("the archived row rides the feed");
    assert_eq!(rust["bucket"], letibot_plugin::sessions::ARCHIVED_BUCKET);
    assert_eq!(rust["project"], "", "an archived row is in no project");
    assert_eq!(
        rust["parent"],
        serde_json::Value::Null,
        "and under no parent"
    );
    let habr = rows
        .iter()
        .find(|r| r["id"] == "s-habr")
        .expect("the live row");
    assert_ne!(habr["bucket"], letibot_plugin::sessions::ARCHIVED_BUCKET);
}

/// A `/switch` to an archived id resumes it: the daemon takes it back into the list as it opens it,
/// which is why an archived row is reachable by name and not only through the group.
#[test]
fn an_archived_session_is_still_a_session_this_head_can_name() {
    let mut a = a_head(vec![in_tree("s-habr", "the habr article", false)]);
    let mut away = in_tree("s-rust", "the rust rewrite", false);
    away.live = false;
    a.apply(ServerFrame::Sessions {
        sessions: a.sessions.clone(),
        current: "s-habr".into(),
        created: None,
        archived: vec![away],
    });
    assert_eq!(
        a.pick("rust"),
        Some(Action::ResumeSession("s-rust".into())),
        "an archived row is not a lost one"
    );
}
