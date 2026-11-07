//! The rooms sidebar: off by default, on from config or a key, a column beside the transcript, and
//! a strip that says so when the terminal is too narrow for it.

use super::rooms::{opened, reply, rooms_reply};
use super::*;
use crate::app::rooms::Chan;
use crate::app::sidebar::{Fit, MAIN_MIN};

fn with_sessions(a: &mut App) {
    a.sessions = vec![brief("s-x", "alpha", true)];
    a.rooms_sync_sessions();
}

#[test]
fn the_sidebar_is_off_until_asked() {
    let mut a = opened(1_000);
    reply(&mut a, rooms_reply(&[("host", 2, 7, 0)]));
    a.key(Key::Esc);
    assert_eq!(a.sidebar_fit(160), Fit::Off);
}

#[test]
fn config_turns_the_sidebar_on() {
    let dir = std::env::temp_dir().join(format!("letibot-sidebar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("leticode.toml"), "[ui]\nrooms_sidebar = true\n").unwrap();
    let mut a = app();
    a.wiring.workspace = dir.display().to_string();
    a.load_rules();
    assert!(a.sidebar.on, "[ui] rooms_sidebar = true was not read");
}

/// Rooms and sessions stand beside the transcript, on every row, and a click on a session switches.
#[test]
fn the_sidebar_stands_beside_the_transcript() {
    let mut a = app();
    reply(&mut a, rooms_reply(&[("host", 2, 7, 3)]));
    with_sessions(&mut a);
    a.sidebar_command("on");
    let rows = a.screen(160, 24);
    assert!(rows.iter().any(|r| r.contains("Rooms")), "{rows:?}");
    let y = rows
        .iter()
        .position(|r| r.contains("alpha"))
        .expect("alpha");
    assert!(
        rows.iter().take(y).any(|r| r.contains("Sessions")),
        "{rows:?}"
    );
    // The list is right of the divider the frame draws after the transcript.
    let cut = rows[y].rfind("│").expect("a divider");
    let x = rows[y][cut..]
        .find("alpha")
        .map(|i| rows[y][..cut + i].chars().count())
        .unwrap();
    assert!(x > MAIN_MIN, "the list is on the right: {x}");
    assert_eq!(
        a.key(Key::Click {
            x: x as u16,
            y: y as u16
        }),
        Some(Action::Switch("s-x".into()))
    );
}

/// The toggle key and `/rooms sidebar off` both put it away.
#[test]
fn the_toggle_and_the_command_flip_it() {
    let mut a = app();
    a.key(Key::AltL);
    assert!(a.sidebar.on);
    a.key(Key::AltL);
    assert!(!a.sidebar.on);
    a.command("rooms sidebar on");
    assert!(a.sidebar.on);
    a.command("rooms sidebar off");
    assert!(!a.sidebar.on);
}

/// Focus goes in with alt-j, the arrows walk, Enter on a room shows it, Esc leaves.
#[test]
fn focus_walks_the_list_and_esc_leaves() {
    let mut a = app();
    reply(&mut a, rooms_reply(&[("host", 2, 7, 0)]));
    with_sessions(&mut a);
    a.sidebar_command("on");
    a.screen(160, 24);
    a.key(Key::AltJ);
    assert!(a.sidebar.focus);
    a.key(Key::Down);
    a.key(Key::Down);
    assert_eq!(a.rooms.selected(), Some(Chan::Session("s-x".into())));
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-x".into())));
    a.key(Key::Esc);
    assert!(!a.sidebar.focus);
}

/// Too narrow: one strip row says it is collapsed, and nothing is silently dropped.
#[test]
fn a_narrow_terminal_collapses_to_a_strip_that_says_so() {
    let mut a = app();
    a.sidebar_command("on");
    assert_eq!(a.sidebar_fit(80), Fit::Collapsed);
    let rows = a.screen(80, 24);
    assert!(rows[0].contains("collapsed"), "{:?}", rows[0]);
    assert!(matches!(a.sidebar_fit(160), Fit::Wide(_)));
}

fn pos(rows: &[String], needle: &str, from_right: bool) -> (u16, u16) {
    let y = rows
        .iter()
        .position(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("no `{needle}` in {rows:#?}"));
    let i = if from_right {
        rows[y].rfind(needle)
    } else {
        rows[y].find(needle)
    }
    .unwrap();
    (rows[y][..i].chars().count() as u16, y as u16)
}

fn sidebar_with_rooms() -> App {
    let mut a = app();
    a.session_id = "s-cur".into();
    a.sessions = vec![
        brief("s-cur", "current", false),
        brief("s-x", "alpha", false),
    ];
    a.rooms_sync_sessions();
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 7, 0), ("project/ab12cd34ef56", 2, 9, 0)]),
    );
    a.sidebar_command("on");
    a
}

/// A room picked in the sidebar does not trap the head: a session row leaves it, the one already
/// on screen included, and Esc from the list is back to the transcript.
#[test]
fn a_session_row_leaves_a_room_and_so_does_esc() {
    let mut a = sidebar_with_rooms();
    let rows = a.screen(160, 24);
    let (x, y) = pos(&rows, "#ab12cd34ef56", true);
    a.key(Key::Click { x, y });
    assert!(a.is_open(Surface::Rooms), "the room did not open");

    // The session this head is on: no switch to make, and the room view goes.
    let rows = a.screen(160, 24);
    let (x, y) = pos(&rows, "│  current", true);
    let x = x + 3;
    assert_eq!(a.key(Key::Click { x, y }), None);
    assert!(
        !a.is_open(Surface::Rooms),
        "still in the room: {x},{y} {rows:#?}"
    );

    // Another session: switched to, and the room view goes.
    let rows = a.screen(160, 24);
    let (x, y) = pos(&rows, "#ab12cd34ef56", true);
    a.key(Key::Click { x, y });
    assert!(a.is_open(Surface::Rooms));
    let rows = a.screen(160, 24);
    let (x, y) = pos(&rows, "alpha", true);
    assert_eq!(
        a.key(Key::Click { x, y }),
        Some(Action::Switch("s-x".into()))
    );
    assert!(!a.is_open(Surface::Rooms));

    // Esc from the focused list.
    a.rooms_go(Chan::Room("host".into()));
    a.sidebar_act(Chan::Room("host".into()));
    a.screen(160, 24);
    a.key(Key::AltJ);
    assert!(a.is_open(Surface::Rooms) && a.sidebar.focus);
    a.key(Key::Esc);
    assert!(!a.is_open(Surface::Rooms) && !a.sidebar.focus);
}

/// With the sidebar beside it `/rooms` draws no list of its own, and without it the list stays.
#[test]
fn the_rooms_view_has_no_list_of_its_own_beside_the_sidebar() {
    let mut a = sidebar_with_rooms();
    a.sidebar_act(Chan::Room("host".into()));
    let rows = a.screen(160, 24);
    let headings = rows.iter().filter(|r| r.contains("Sessions")).count();
    assert_eq!(headings, 1, "{rows:#?}");
    // The room's title starts at the left edge, where its own list used to be.
    let (x, _) = pos(&rows, "host room", false);
    assert!(x < 8, "{x}: {rows:#?}");

    a.sidebar_command("off");
    let rows = a.screen(160, 24);
    let (x, _) = pos(&rows, "host room", false);
    assert!(x > 8, "the list is back: {x}");
}
