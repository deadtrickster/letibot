//! The `/rooms` view: what it asks the daemon, what it does with the answers, and what a typed
//! line becomes. The source is a fake: replies are built by hand, as the daemon would send them.

use super::*;
use serde_json::{Value, json};

const PROJECT: &str = "project/ab12cd34ef56";

pub(super) fn reply(a: &mut App, reply: Value) {
    a.apply(ServerFrame::RoomChat {
        client_request_id: "c1".into(),
        reply,
    });
}

pub(super) fn msg(seq: u64, room: &str, author: &str, kind: &str, text: &str) -> Value {
    json!({"seq": seq, "ts": 1_793_735_794_000u64 + seq * 60_000, "room": room, "author": author,
           "kind": kind, "text": text})
}

pub(super) fn history(room: &str, rows: Vec<Value>) -> Value {
    let last = rows.last().and_then(|m| m["seq"].as_u64()).unwrap_or(0);
    json!({"type": "history", "room": room, "rows": rows, "last_seq": last, "trimmed": false})
}

pub(super) fn rooms_reply(rows: &[(&str, u64, u64, u64)]) -> Value {
    let rows: Vec<Value> = rows
        .iter()
        .map(|(id, members, last_seq, unread)| {
            json!({"id": id, "kind": if *id == "host" { "host" } else { "project" },
                   "project": "", "members": members, "last_seq": last_seq, "unread": unread})
        })
        .collect();
    json!({"type": "rooms", "rows": rows})
}

/// The chat requests among the queued actions, as the JSON the daemon would receive.
pub(super) fn ops(a: &mut App) -> Vec<Value> {
    a.take_actions()
        .into_iter()
        .filter_map(|x| match x {
            Action::RoomChat { op } => Some(op),
            _ => None,
        })
        .collect()
}

fn types(ops: &[Value]) -> Vec<&str> {
    ops.iter()
        .map(|o| o["type"].as_str().unwrap_or(""))
        .collect()
}

/// An app with the view open at `t` ms and the opening requests drained.
pub(super) fn opened(t: u64) -> App {
    let mut a = app();
    a.clock(t);
    let first = a.command("rooms");
    assert!(matches!(first, Some(Action::RoomChat { .. })), "{first:?}");
    a.take_actions();
    a
}

#[test]
fn opening_asks_for_the_rooms_and_the_host_room() {
    let mut a = app();
    a.clock(1_000);
    let first = a.command("rooms");
    assert_eq!(
        first,
        Some(Action::RoomChat {
            op: json!({"type": "rooms"})
        })
    );
    let rest = ops(&mut a);
    assert_eq!(
        rest,
        vec![
            json!({"type": "read", "room": "host", "since": 0}),
            json!({"type": "read", "room": "dm", "since": 0}),
        ]
    );
    assert!(a.is_open(Surface::Rooms));
}

#[test]
fn a_second_rooms_closes_the_view() {
    let mut a = opened(1_000);
    assert_eq!(a.command("rooms"), None);
    assert!(!a.is_open(Surface::Rooms));
}

#[test]
fn the_view_polls_only_while_it_is_open() {
    let mut a = app();
    a.clock(1_000);
    a.clock(10_000);
    assert!(ops(&mut a).is_empty(), "closed view polled");

    a.command("rooms");
    a.take_actions();
    a.clock(11_500);
    assert!(ops(&mut a).is_empty(), "polled before the interval");
    a.clock(12_100);
    assert_eq!(types(&ops(&mut a)), ["rooms", "read", "read"]);
    a.clock(12_200);
    assert!(ops(&mut a).is_empty(), "polled twice in one interval");

    a.pop_surface();
    a.clock(60_000);
    assert!(ops(&mut a).is_empty(), "closed view kept polling");
}

#[test]
fn a_poll_reads_from_where_the_last_one_ended() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        history(
            "host",
            vec![
                msg(4, "host", "s-a", "session", "one"),
                msg(7, "host", "s-b", "session", "two"),
            ],
        ),
    );
    a.take_actions();
    a.clock(4_000);
    let polled = ops(&mut a);
    assert!(
        polled.contains(&json!({"type": "read", "room": "host", "since": 7})),
        "{polled:?}"
    );
}

#[test]
fn a_line_posts_to_the_selected_channel_as_a_say() {
    let mut a = opened(1_000);
    a.set_composer("build is green");
    let sent = a.key(Key::Enter);
    let Some(Action::RoomChat { op }) = sent else {
        panic!("not a chat request: {sent:?}");
    };
    assert_eq!(op["type"], "say");
    assert_eq!(op["room"], "host");
    assert_eq!(op["text"], "build is green");
    assert_eq!(op["thread"], Value::Null);
    assert!(op["id"].as_str().is_some_and(|i| !i.is_empty()));
}

#[test]
fn a_line_in_a_project_room_goes_to_that_room() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 0, 0), (PROJECT, 2, 0, 0)]),
    );
    a.key(Key::Down);
    a.take_actions();
    a.set_composer("who has the disk?");
    let Some(Action::RoomChat { op }) = a.key(Key::Enter) else {
        panic!("no chat request");
    };
    assert_eq!(op["room"], PROJECT);
}

#[test]
fn greater_than_and_a_number_answers_that_message() {
    let mut a = opened(1_000);
    a.set_composer(">14 agreed");
    let Some(Action::RoomChat { op }) = a.key(Key::Enter) else {
        panic!("no chat request");
    };
    assert_eq!(op["thread"], 14);
    assert_eq!(op["text"], "agreed");

    a.set_composer(">14");
    assert_eq!(
        a.key(Key::Enter)
            .map(|x| matches!(x, Action::RoomChat { .. })),
        Some(true)
    );
    a.set_composer(">x not a reply");
    let Some(Action::RoomChat { op }) = a.key(Key::Enter) else {
        panic!("no chat request");
    };
    assert_eq!(op["thread"], Value::Null);
    assert_eq!(op["text"], ">x not a reply");
}

#[test]
fn a_line_in_a_dm_channel_is_a_dm_to_that_peer() {
    let mut a = opened(1_000);
    reply(&mut a, rooms_reply(&[("host", 1, 0, 0)]));
    reply(
        &mut a,
        history(
            "dm",
            vec![{
                let mut m = msg(3, "dm", "s-a", "session", "psst");
                m["to"] = json!("operator");
                m
            }],
        ),
    );
    // host, then the DM with s-a.
    a.key(Key::Down);
    a.take_actions();
    a.set_composer("heard");
    let Some(Action::RoomChat { op }) = a.key(Key::Enter) else {
        panic!("no chat request");
    };
    assert_eq!(op["type"], "dm");
    assert_eq!(op["to"], "s-a");
    assert_eq!(op["text"], "heard");
}

#[test]
fn slash_dm_sends_and_selects_the_peer() {
    let mut a = app();
    a.clock(1_000);
    let sent = a.command("dm @s-z please look");
    let Some(Action::RoomChat { op }) = sent else {
        panic!("no chat request: {sent:?}");
    };
    assert_eq!(op["type"], "dm");
    assert_eq!(op["to"], "s-z");
    assert_eq!(op["text"], "please look");
    assert!(a.is_open(Surface::Rooms));
    assert_eq!(
        a.rooms.selected(),
        crate::app::rooms::Chan::Dm("s-z".into())
    );
}

#[test]
fn switching_channel_reads_the_new_one_at_once() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 0, 0), (PROJECT, 2, 0, 0)]),
    );
    a.key(Key::Down);
    a.clock(1_100);
    let next = ops(&mut a);
    assert!(
        next.contains(&json!({"type": "read", "room": PROJECT, "since": 0})),
        "{next:?}"
    );
    // Wraps at the end.
    a.key(Key::Up);
    assert_eq!(
        a.rooms.selected(),
        crate::app::rooms::Chan::Room("host".into())
    );
    a.key(Key::Up);
    assert_eq!(
        a.rooms.selected(),
        crate::app::rooms::Chan::Room(PROJECT.into())
    );
}

#[test]
fn the_roster_key_asks_for_the_roster_of_the_selected_channel() {
    let mut a = opened(1_000);
    a.key(Key::Char('r'));
    a.clock(1_100);
    let next = ops(&mut a);
    assert!(
        next.contains(&json!({"type": "roster", "room": "host"})),
        "{next:?}"
    );
    a.key(Key::Char('r'));
    a.clock(1_200);
    assert!(!types(&ops(&mut a)).contains(&"roster"));
}

#[test]
fn a_typed_r_is_a_letter_not_the_roster_key() {
    let mut a = opened(1_000);
    a.set_composer("a");
    a.key(Key::Char('r'));
    assert!(!a.rooms.roster_open);
    assert_eq!(a.editor.text(), "ar");
}

#[test]
fn what_was_shown_is_acked() {
    let mut a = opened(1_000);
    reply(&mut a, rooms_reply(&[("host", 2, 7, 2)]));
    reply(
        &mut a,
        history(
            "host",
            vec![
                msg(4, "host", "s-a", "session", "one"),
                msg(7, "host", "s-b", "session", "two"),
            ],
        ),
    );
    assert_eq!(ops(&mut a), vec![json!({"type": "ack", "seq": 7})]);
    // Nothing new, nothing acked again.
    reply(&mut a, history("host", vec![]));
    assert!(ops(&mut a).is_empty());
}

#[test]
fn the_cursor_does_not_pass_a_channel_nobody_looked_at() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 7, 1), (PROJECT, 2, 12, 3)]),
    );
    reply(
        &mut a,
        history("host", vec![msg(7, "host", "s-b", "session", "two")]),
    );
    assert!(
        ops(&mut a).is_empty(),
        "acked past unread messages elsewhere"
    );

    // Looking at the project room clears its count, and then the cursor can pass both.
    a.key(Key::Down);
    reply(
        &mut a,
        history(PROJECT, vec![msg(12, PROJECT, "s-c", "session", "three")]),
    );
    assert_eq!(ops(&mut a), vec![json!({"type": "ack", "seq": 12})]);
}

#[test]
fn a_refusal_and_a_daemon_error_are_kept_for_the_view() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        json!({"type": "refused", "id": "x", "why": "unknown_addressee"}),
    );
    assert_eq!(a.rooms.error.as_deref(), Some("refused: unknown_addressee"));
    reply(
        &mut a,
        json!({"error": "the room coordinator is not reachable"}),
    );
    assert_eq!(
        a.rooms.error.as_deref(),
        Some("the room coordinator is not reachable")
    );
    reply(&mut a, rooms_reply(&[("host", 1, 0, 0)]));
    assert_eq!(a.rooms.error, None);
}

#[test]
fn a_said_reply_polls_at_once() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        json!({"type": "said", "id": "x", "message": msg(9, "host", "operator", "operator", "hi")}),
    );
    a.clock(1_050);
    assert!(types(&ops(&mut a)).contains(&"read"));
}

#[test]
fn an_unread_count_survives_until_the_channel_is_shown() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 5, 0), (PROJECT, 2, 9, 4)]),
    );
    let project = crate::app::rooms::Chan::Room(PROJECT.into());
    assert_eq!(a.rooms.unread(&project), 4);
    a.key(Key::Down);
    reply(
        &mut a,
        history(PROJECT, vec![msg(9, PROJECT, "s-c", "session", "x")]),
    );
    assert_eq!(a.rooms.unread(&project), 0);
}

#[test]
fn a_mention_is_a_name_not_a_prefix() {
    use crate::app::rooms::{Msg, mentions_operator};
    let m = |author: &str, to: Option<&str>, text: &str| Msg {
        seq: 1,
        ts: 0,
        author: author.into(),
        kind: "session".into(),
        to: to.map(String::from),
        thread: None,
        refs: Vec::new(),
        text: text.into(),
    };
    assert!(mentions_operator(&m("s-a", None, "ping @operator, please")));
    assert!(mentions_operator(&m("s-a", Some("operator"), "hi")));
    assert!(mentions_operator(&m("s-a", None, "@Operator")));
    assert!(!mentions_operator(&m(
        "s-a",
        None,
        "@operators of the farm"
    )));
    assert!(!mentions_operator(&m("s-a", None, "mail me@operator")));
    assert!(!mentions_operator(&m("operator", None, "@operator")));
}
