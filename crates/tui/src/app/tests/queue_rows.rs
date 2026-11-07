//! The Queues section of the rooms list: health, counts, unread queue moves, open and mute.

use super::rooms::{history, msg, opened, reply, rooms_reply, screen};
use super::*;
use crate::app::queue_rows::Health;
use crate::app::rooms::Chan;
use crate::ui::panes::queue::QueueScope;
use serde_json::json;

const REPO: &str = "/home/dead/Projects/letibot";
const PROJECT: &str = "project/ab12cd34ef56";

fn review(id: &str, state: &str) -> serde_json::Value {
    json!({"id": id, "repository": REPO, "target": "main", "kind": "branch",
        "session_id": "s-r", "branch": format!("b-{id}"), "state": state, "evidence": "", "created_ms": 1, "state_ms": 1})
}

fn snapshot(at: u64, moves: &[u64]) -> letibot_sessionlog::event::QueueSnapshot {
    let recent: Vec<_> = moves
        .iter()
        .map(|t| {
            json!({"queue": "review", "entry_id": "r1", "repository": REPO, "from": "waiting",
                "to": "reviewing", "at_ms": t, "by": "x", "why": "w"})
        })
        .collect();
    serde_json::from_value(json!({
        "at_ms": at,
        "reviews": [review("r1", "waiting"), review("r2", "reviewing"), review("r3", "blocked")],
        "targets": [{"repository": REPO, "target": "main", "sha": "abc", "state": "red",
            "checked_ms": at, "evidence": "e"}],
        "recent": recent
    }))
    .expect("a snapshot")
}

fn muted_row(on: bool) -> letibot_sessionlog::protocol::SettingRow {
    letibot_sessionlog::protocol::SettingRow {
        key: "listen.letibot".into(),
        value: if on { "[x] muted" } else { "[ ] listened" }.into(),
        source: String::new(),
        editable: "/listen".into(),
        choices: Vec::new(),
        tools: Vec::new(),
    }
}

fn head(moves: &[u64]) -> App {
    let mut a = opened(1_000);
    a.queues_all = snapshot(5_000, &[]);
    a.queue_baseline();
    a.queues_all = snapshot(6_000, moves);
    a
}

fn pos(rows: &[String], needle: &str) -> (u16, u16) {
    let y = rows
        .iter()
        .position(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("no `{needle}` in {rows:#?}"));
    let x = rows[y]
        .find(needle)
        .map(|i| rows[y][..i].chars().count())
        .unwrap();
    (x as u16, y as u16)
}

#[test]
fn the_section_shows_health_and_counts_below_rooms_and_sessions() {
    let mut a = head(&[]);
    a.sessions = vec![brief("s-x", "alpha", false)];
    a.rooms_sync_sessions();
    let repos = a.queue_repos();
    assert_eq!(repos.len(), 1);
    let r = &repos[0];
    assert_eq!(
        (r.health, r.waiting, r.gating, r.person),
        (Health::Red, 1, 1, 1)
    );
    let rows = screen(&mut a, 120);
    let (_, queues) = pos(&rows, "Queues");
    let (_, sessions) = pos(&rows, "Sessions");
    let (_, name) = pos(&rows, "● letibot");
    assert!(sessions < queues && queues < name, "{rows:#?}");
    assert!(
        rows[usize::from(name) + 1].contains("1w 1g 1p"),
        "{rows:#?}"
    );
    assert!(rows[usize::from(name)].contains('●'), "{rows:#?}");
}

#[test]
fn unread_counts_moves_since_the_row_was_opened_and_resets_on_open() {
    let mut a = head(&[5_500, 5_600, 6_000]);
    assert_eq!(a.queue_repos()[0].unread, 3);
    let rows = screen(&mut a, 120);
    let (x, y) = pos(&rows, "letibot");
    a.key(Key::Click { x, y });
    assert_eq!(a.queue_repos()[0].unread, 0);
    // A later move counts again.
    a.queues_all = snapshot(7_000, &[5_500, 5_600, 6_000, 6_900]);
    assert_eq!(a.queue_repos()[0].unread, 1);
}

#[test]
fn a_click_on_the_row_opens_that_repositorys_queue() {
    let mut a = head(&[]);
    let rows = screen(&mut a, 120);
    let (x, y) = pos(&rows, "letibot");
    a.key(Key::Click { x, y });
    assert!(a.is_open(Surface::Queue));
    assert_eq!(a.queue_scope, QueueScope::Path(REPO.into()));
}

#[test]
fn the_checkbox_sends_listen_minus_and_plus() {
    let mut a = head(&[]);
    a.settings = vec![muted_row(false)];
    let rows = screen(&mut a, 120);
    let (x, y) = pos(&rows, "[ ]");
    assert_eq!(
        a.key(Key::Click { x, y }),
        Some(Action::Slash {
            line: "listen -letibot".into()
        })
    );
    a.settings = vec![muted_row(true)];
    let rows = screen(&mut a, 120);
    let (x, y) = pos(&rows, "[x]");
    assert_eq!(
        a.key(Key::Click { x, y }),
        Some(Action::Slash {
            line: "listen +letibot".into()
        })
    );
}

/// A gatekeeper's post is a queue event: it is not a message of the room, and reading the room to
/// its end leaves nothing unread.
#[test]
fn a_queue_post_does_not_land_in_a_room() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 0, 0), (PROJECT, 2, 9, 2)]),
    );
    a.key(Key::Down);
    reply(
        &mut a,
        history(
            PROJECT,
            vec![
                msg(7, PROJECT, "s-a", "session", "a human word"),
                msg(
                    9,
                    PROJECT,
                    "s-gatekeeper-ab12cd34ef56",
                    "session",
                    "landed #412",
                ),
            ],
        ),
    );
    let c = Chan::Room(PROJECT.into());
    let texts: Vec<_> = a
        .rooms
        .messages(&c)
        .iter()
        .map(|m| m.text.clone())
        .collect();
    assert_eq!(texts, vec!["a human word".to_string()]);
    assert!(!screen(&mut a, 120).join("\n").contains("landed #412"));
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 0, 0), (PROJECT, 2, 9, 2)]),
    );
    assert_eq!(a.rooms.seen.get(PROJECT).copied(), Some(9));
    a.key(Key::Up);
    assert_eq!(a.rooms.unread(&c), 0);
}
