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

#[test]
fn the_sidebar_starts_a_third_wide_and_keeps_the_transcripts_minimum() {
    let mut a = sidebar_with_rooms();
    a.screen(150, 24);
    assert_eq!(a.sidebar.last_w, 50);
    // Wide enough to ask for more than the transcript can spare: held at its minimum.
    a.sidebar.width = Some(400);
    a.screen(150, 24);
    assert_eq!(a.sidebar.last_w, 150 - MAIN_MIN - 1);
    // And no narrower than the list's own minimum.
    a.sidebar.width = Some(2);
    a.screen(150, 24);
    assert_eq!(a.sidebar.last_w, crate::app::sidebar::SIDE_MIN);
}

#[test]
fn the_border_drags_and_the_keys_resize() {
    let mut a = sidebar_with_rooms();
    a.screen(150, 24);
    let border = 150 - a.sidebar.last_w - 1;
    // The hand is over the border.
    a.hover_at(border as u16, 5);
    assert_eq!(a.take_pointer(), Some(true));
    a.key(Key::Press {
        x: border as u16,
        y: 5,
    });
    a.key(Key::Drag { x: 90, y: 5 });
    a.key(Key::Release { x: 90, y: 5 });
    a.screen(150, 24);
    assert_eq!(a.sidebar.last_w, 150 - 90 - 1);
    assert!(a.selection.is_none(), "the drag selected text");

    a.key(Key::AltWiden);
    a.screen(150, 24);
    assert_eq!(a.sidebar.last_w, 150 - 90 - 1 + 4);
    a.key(Key::AltNarrow);
    a.screen(150, 24);
    a.key(Key::AltNarrow);
    a.screen(150, 24);
    assert_eq!(a.sidebar.last_w, 150 - 90 - 1 - 4);
    let hint = a.screen(150, 24).last().cloned().unwrap();
    assert!(hint.contains("alt-< alt->"), "{hint}");
}

/// The width the operator chose is in `head.toml` and comes back at the next start. The configured
/// width is only the start.
#[test]
fn the_chosen_width_persists_and_the_configured_one_starts_it() {
    let dir = std::env::temp_dir().join(format!("letibot-sidebar-w-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("leticode.toml"),
        "[ui]\nrooms_sidebar = true\nrooms_sidebar_width = 30\n",
    )
    .unwrap();
    let path = dir.join("head.toml");

    let mut a = app();
    a.wiring.workspace = dir.display().to_string();
    a.prefs_path = Some(path.clone());
    a.load_prefs();
    a.screen(150, 24);
    assert_eq!(a.sidebar.last_w, 30, "the configured start");
    a.key(Key::AltWiden);
    assert_eq!(a.sidebar.width, Some(34));

    let mut b = app();
    b.wiring.workspace = dir.display().to_string();
    b.prefs_path = Some(path);
    b.load_prefs();
    b.screen(150, 24);
    assert_eq!(
        b.sidebar.last_w, 34,
        "the dragged width wins after a restart"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Sessions section lists the operator's own top-level sessions and nothing else: not the
/// queue's orchestrator, not a package's session, not a child.
#[test]
fn a_queue_orchestrator_and_a_package_session_are_not_listed() {
    let mut a = app();
    let mut gatekeeper = brief("s-gatekeeper-c2f63a0e6a17", "queue", false);
    gatekeeper.origin = letibot_sessionlog::SessionOrigin::Operator;
    let mut queue = brief("s-q", "queue", false);
    queue.origin = letibot_sessionlog::SessionOrigin::Queue;
    let mut package = brief("s-pkg", "package", false);
    package.origin = letibot_sessionlog::SessionOrigin::Package;
    let mut child = brief("s-child", "child", false);
    child.parent_session_id = Some("s-op".into());
    a.sessions = vec![
        brief("s-op", "mine", false),
        gatekeeper,
        queue,
        package,
        child,
    ];
    a.rooms_sync_sessions();
    let ids: Vec<&str> = a.rooms.sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["s-op"]);
}

fn cells_of(row: &str) -> Vec<char> {
    crate::app::keyhints::cells(row)
}

/// A hover is the surface's own: lighting a row of the transcript leaves the sidebar's cells
/// alone, and the other way round.
#[test]
fn a_hover_does_not_cross_the_sidebars_border() {
    let mut a = sidebar_with_rooms();
    a.screen(160, 24);
    let border = 160 - a.sidebar.last_w - 1;
    let plain = a.screen(160, 24);
    // A transcript hover with no columns of its own, on a row the sidebar also draws on.
    let y = plain.iter().position(|r| r.contains("alpha")).unwrap();
    a.pointer_at = Some((3, y as u16));
    a.hovered = Some(crate::app::pointer::Hovered {
        rows: vec![(y, None)],
    });
    let mut lit = plain.clone();
    a.paint_pointer(&mut lit);
    assert_ne!(lit[y], plain[y], "the hover painted nothing");
    let (c0, c1) = (cells_of(&plain[y]), cells_of(&lit[y]));
    assert_eq!(c0, c1, "same text");
    // The sidebar side of the row is byte for byte the unlit one.
    let side = |r: &str| r.split('│').next_back().unwrap_or("").to_string();
    assert_eq!(side(&lit[y]), side(&plain[y]), "border {border}");

    // And a hover on the sidebar row leaves the transcript side as it was.
    a.pointer_at = Some((border as u16 + 3, y as u16));
    a.hovered = Some(crate::app::pointer::Hovered {
        rows: vec![(y, None)],
    });
    let mut lit = plain.clone();
    a.paint_pointer(&mut lit);
    assert_ne!(lit[y], plain[y]);
    let main = |r: &str| r.rsplit_once('│').map_or(r, |(m, _)| m).to_string();
    assert_eq!(main(&lit[y]), main(&plain[y]));
}
