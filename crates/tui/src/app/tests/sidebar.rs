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
