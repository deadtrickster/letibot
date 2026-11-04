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

/// The daemon's answer to one named request, as a head tells its own send's answer from a
/// poll's.
pub(super) fn reply_as(a: &mut App, id: &str, reply: Value) {
    a.apply(ServerFrame::RoomChat {
        client_request_id: id.into(),
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
            Action::RoomChat { op, .. } => Some(op),
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
            op: json!({"type": "rooms"}),
            id: None,
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
    let Some(Action::RoomChat { op, .. }) = sent else {
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
    let Some(Action::RoomChat { op, .. }) = a.key(Key::Enter) else {
        panic!("no chat request");
    };
    assert_eq!(op["room"], PROJECT);
}

/// The selection is the channel, not its index: a poll that lists a new room ahead of the
/// current one must not move what Enter posts to.
#[test]
fn a_room_arriving_ahead_of_the_selection_does_not_move_it() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 0, 0), (PROJECT, 2, 0, 0)]),
    );
    a.key(Key::Down);
    a.take_actions();
    assert_eq!(
        a.rooms.selected(),
        Some(crate::app::rooms::Chan::Room(PROJECT.into()))
    );
    // A new project lands first in the coordinator's list, which shifts every index.
    let other = "project/000000000001";
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 0, 0), (other, 1, 0, 0), (PROJECT, 2, 0, 0)]),
    );
    a.set_composer("still here?");
    let Some(Action::RoomChat { op, .. }) = a.key(Key::Enter) else {
        panic!("no chat request");
    };
    assert_eq!(op["room"], PROJECT, "the selection moved with the index");
    assert_eq!(
        a.rooms.selected(),
        Some(crate::app::rooms::Chan::Room(PROJECT.into()))
    );
}

/// A poll that drops the selected room selects nothing, says which channel went, and a line
/// goes nowhere rather than to whichever channel took its index.
#[test]
fn a_channel_that_disappears_selects_nothing_and_says_so() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 0, 0), (PROJECT, 2, 0, 0)]),
    );
    reply(
        &mut a,
        history(PROJECT, vec![msg(3, PROJECT, "s-a", "session", "hi")]),
    );
    a.key(Key::Down);
    a.take_actions();

    // The room is gone on the next poll: the project's last session left.
    reply(&mut a, rooms_reply(&[("host", 2, 0, 0)]));
    assert_eq!(
        a.rooms.selected(),
        None,
        "another channel was selected instead"
    );
    assert_eq!(a.rooms.gone.as_deref(), Some("#ab12cd34ef56"));
    let rows = screen(&mut a, 80);
    let all = rows.join("\n");
    assert!(all.contains("#ab12cd34ef56 is gone"), "{all}");

    // The line goes nowhere, and the poll asks only for the list.
    a.set_composer("anyone?");
    assert_eq!(a.key(Key::Enter), None);
    assert_eq!(a.input(), "anyone?", "the words are held, not swallowed");
    assert!(
        ops(&mut a).is_empty(),
        "a poll read a channel that is not selected"
    );
    // An explicit move lands somewhere real and clears the sentence.
    a.key(Key::Down);
    assert_eq!(a.rooms.gone, None);
    assert_eq!(
        a.rooms.selected(),
        Some(crate::app::rooms::Chan::Room("host".into()))
    );
}

#[test]
fn greater_than_and_a_number_answers_that_message() {
    let mut a = opened(1_000);
    a.set_composer(">14 agreed");
    let Some(Action::RoomChat { op, .. }) = a.key(Key::Enter) else {
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
    let Some(Action::RoomChat { op, .. }) = a.key(Key::Enter) else {
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
    let Some(Action::RoomChat { op, .. }) = a.key(Key::Enter) else {
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
    let Some(Action::RoomChat { op, .. }) = sent else {
        panic!("no chat request: {sent:?}");
    };
    assert_eq!(op["type"], "dm");
    assert_eq!(op["to"], "s-z");
    assert_eq!(op["text"], "please look");
    assert!(a.is_open(Surface::Rooms));
    assert_eq!(
        a.rooms.selected(),
        Some(crate::app::rooms::Chan::Dm("s-z".into()))
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
        Some(crate::app::rooms::Chan::Room("host".into()))
    );
    a.key(Key::Up);
    assert_eq!(
        a.rooms.selected(),
        Some(crate::app::rooms::Chan::Room(PROJECT.into()))
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

/// The unread messages another channel is holding may be OLDER than this channel's tail: `seq`
/// counts the whole log, so acking this channel's top would mark those read too. The cursor
/// waits until every channel this head knows has been drawn to its end.
#[test]
fn the_cursor_does_not_pass_older_unread_in_another_room() {
    let mut a = opened(1_000);
    // The project room is the busier one: host unread sits at 5, well under the project's 12.
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 5, 2), (PROJECT, 2, 12, 1)]),
    );
    a.key(Key::Down);
    reply(
        &mut a,
        history(PROJECT, vec![msg(12, PROJECT, "s-c", "session", "three")]),
    );
    assert!(
        ops(&mut a).is_empty(),
        "acked past the host room's older unread messages"
    );

    // The host room is drawn to its end; now nothing anywhere is undrawn, and the cursor moves
    // to the farthest this head has drawn in any channel - the project room's 12.
    a.key(Key::Up);
    reply(
        &mut a,
        history("host", vec![msg(5, "host", "s-b", "session", "two")]),
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

/// The send helper: one line posted to the host room, its request id handed back so a test can
/// answer it as the daemon would.
fn say(a: &mut App, text: &str) -> String {
    a.set_composer(text);
    match a.key(Key::Enter) {
        Some(Action::RoomChat { id: Some(id), .. }) => id,
        other => panic!("`{text}` did not send: {other:?}"),
    }
}

/// A failed send keeps its line and its reason in the channel it was aimed at, across the
/// polls that used to wipe the error within one interval.
#[test]
fn a_failed_send_keeps_its_line_and_reason_through_a_poll() {
    let mut a = opened(1_000);
    let id = say(&mut a, "who has the disk?");
    reply_as(
        &mut a,
        &id,
        json!({"type": "refused", "id": id, "why": "unknown_addressee"}),
    );
    // The poll that follows used to clear the error and leave the composer empty: the words
    // and the reason both went.
    reply(&mut a, rooms_reply(&[("host", 1, 0, 0)]));
    reply(&mut a, history("host", vec![]));
    let all = screen(&mut a, 80).join("\n");
    assert!(all.contains("✗ who has the disk?"), "{all}");
    assert!(all.contains("refused: unknown_addressee"), "{all}");
    assert!(all.contains("enter resends"), "{all}");
    assert_eq!(a.rooms.sends.len(), 1, "the record was dropped by a poll");
}

/// A send that never reached the coordinator is told from a poll's trouble by the request id,
/// and lands on its own line.
#[test]
fn a_transport_error_lands_on_the_send_it_names() {
    let mut a = opened(1_000);
    let id = say(&mut a, "status?");
    reply_as(
        &mut a,
        &id,
        json!({"error": "the host coordinator is not reachable"}),
    );
    let all = screen(&mut a, 80).join("\n");
    assert!(all.contains("✗ status?"), "{all}");
    assert!(all.contains("not sent:"), "{all}");
    // A poll's own trouble is nobody's send: it stays on the footer.
    reply(
        &mut a,
        json!({"error": "the host coordinator is not reachable"}),
    );
    assert!(a.rooms.error.is_some(), "a poll's error went to a send");
    assert_eq!(a.rooms.sends.len(), 1);
}

/// Only the send that landed goes. Another send's failure survives its neighbour's success,
/// and Enter resends the failed line while `d` dismisses it.
#[test]
fn a_landed_send_clears_only_itself_and_the_keys_deal_with_the_rest() {
    let mut a = opened(1_000);
    let first = say(&mut a, "first words");
    let second = say(&mut a, "second words");
    reply_as(
        &mut a,
        &second,
        json!({"type": "said", "id": second, "message": msg(9, "host", "operator", "operator", "second words")}),
    );
    let ids: Vec<&str> = a.rooms.sends.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec![first.as_str()], "a send that landed stayed");
    reply_as(
        &mut a,
        &first,
        json!({"type": "refused", "id": first, "why": "unknown_thread"}),
    );

    // Enter on the empty composer resends the failed line under a new id.
    a.set_composer("");
    let Some(Action::RoomChat { op, .. }) = a.key(Key::Enter) else {
        panic!("Enter did not resend the failed line");
    };
    assert_eq!(op["text"], "first words");
    assert!(
        a.rooms.sends.iter().all(|s| s.error.is_none()),
        "the superseded record stayed"
    );

    // It fails again, and `d` dismisses it: read, understood, not sent again.
    let again = a.rooms.sends[0].id.clone();
    reply_as(
        &mut a,
        &again,
        json!({"type": "refused", "id": again, "why": "unknown_thread"}),
    );
    a.set_composer("");
    a.key(Key::Char('d'));
    assert!(a.rooms.sends.is_empty(), "the failed line stayed after `d`");
    let all = screen(&mut a, 80).join("\n");
    assert!(!all.contains("✗ first words"), "{all}");
}

/// A `d` typed as part of a message is a letter, exactly as `r` is: the key is the composer's
/// while anything is in it.
#[test]
fn a_typed_d_is_a_letter_not_the_dismiss_key() {
    let mut a = opened(1_000);
    a.set_composer("a");
    a.key(Key::Char('d'));
    assert_eq!(a.editor.text(), "ad");
    assert!(a.rooms.sends.is_empty());
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

// -- drawing ----------------------------------------------------------------------------------

const WIDTHS: [usize; 2] = [80, 120];

fn screen(a: &mut App, cols: usize) -> Vec<String> {
    a.screen(cols, 24)
}

fn row_with<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter()
        .find(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("no row has {needle:?}:\n{}", rows.join("\n")))
}

fn fits(rows: &[String], cols: usize) {
    for r in rows {
        assert!(
            crate::ui::render::visible_width(r) <= cols,
            "{cols} columns, row of {}: {r:?}",
            crate::ui::render::visible_width(r)
        );
    }
}

/// The left column of a row, as wide as the frame drew it: the width is content-sized now, so
/// the test reads what the draw recorded rather than re-deriving it.
fn left_of(row: &str, lw: usize) -> String {
    // Two columns of gutter, then the list.
    row.chars().skip(2).take(lw).collect()
}

#[test]
fn before_any_answer_the_view_says_it_is_asking() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        let rows = screen(&mut a, cols);
        fits(&rows, cols);
        let all = rows.join("\n");
        assert!(all.contains("channels"), "{all}");
        assert!(all.contains("▸ host"), "{all}");
        assert!(all.contains("asking the coordinator"), "{all}");
    }
}

#[test]
fn an_empty_channel_says_so() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        reply(&mut a, rooms_reply(&[("host", 1, 0, 0)]));
        reply(&mut a, history("host", vec![]));
        let rows = screen(&mut a, cols);
        fits(&rows, cols);
        let all = rows.join("\n");
        assert!(all.contains("nothing here yet"), "{all}");
        assert!(all.contains("host room"), "{all}");
        assert!(all.contains("1 here"), "{all}");
    }
}

#[test]
fn one_channel_draws_its_messages_with_authors_and_a_divider() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        reply(&mut a, rooms_reply(&[("host", 2, 2, 0)]));
        reply(
            &mut a,
            history(
                "host",
                vec![
                    msg(1, "host", "s-a", "session", "who has the disk?"),
                    msg(2, "host", "operator", "operator", "s-b does"),
                ],
            ),
        );
        let rows = screen(&mut a, cols);
        fits(&rows, cols);
        let first = row_with(&rows, "<s-a>");
        assert!(first.contains("who has the disk?"), "{first}");
        assert!(first.contains(" │ "), "the divider: {first}");
        let second = row_with(&rows, "<operator>");
        assert!(second.contains("s-b does"), "{second}");
        assert!(
            rows.iter().position(|r| r.contains("<s-a>"))
                < rows.iter().position(|r| r.contains("<operator>")),
            "oldest first"
        );
    }
}

#[test]
fn many_channels_show_their_unread_counts_and_the_selected_one_shows_none() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        reply(
            &mut a,
            rooms_reply(&[
                ("host", 3, 20, 5),
                ("project/ab12cd34ef56", 2, 21, 3),
                ("project/ffeeddccbbaa", 1, 22, 12),
            ]),
        );
        let rows = screen(&mut a, cols);
        fits(&rows, cols);
        let lw = a.rooms_left_w;
        let host = left_of(row_with(&rows, "▸ host"), lw);
        assert_eq!(
            host.trim_end(),
            "▸ host",
            "the selected channel has no count"
        );
        let one = left_of(row_with(&rows, "#ab12cd34"), lw);
        assert!(one.trim_end().ends_with(" 3"), "{one:?}");
        let two = left_of(row_with(&rows, "#ffeeddcc"), lw);
        assert!(two.trim_end().ends_with("12"), "{two:?}");
    }
}

/// Two projects whose keys share a prefix are two channels: the column is sized from the
/// labels, so both whole keys fit at any ordinary width instead of both reading as the first
/// eight characters.
#[test]
fn two_projects_sharing_a_prefix_are_distinguishable() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        reply(
            &mut a,
            rooms_reply(&[
                ("host", 1, 0, 0),
                ("project/ab12cd34ef56", 2, 21, 3),
                ("project/ab12cd34ef99", 2, 22, 4),
            ]),
        );
        let rows = screen(&mut a, cols);
        fits(&rows, cols);
        let one = left_of(row_with(&rows, "#ab12cd34ef56"), a.rooms_left_w);
        let two = left_of(row_with(&rows, "#ab12cd34ef99"), a.rooms_left_w);
        assert!(
            one.trim_end().contains("#ab12cd34ef56"),
            "{one:?} at {cols}"
        );
        assert!(
            two.trim_end().contains("#ab12cd34ef99"),
            "{two:?} at {cols}"
        );
        assert_ne!(one.trim_end(), two.trim_end(), "the channels read as one");
    }
}

/// When the column must cut, it cuts the middle: both names keep their tail, so the two
/// channels stay distinguishable even at a width no whole key fits in.
#[test]
fn a_narrow_pane_cuts_names_keeping_their_ends() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[
            ("host", 1, 0, 0),
            ("project/ab12cd34ef56", 2, 21, 3),
            ("project/ab12cd34ef99", 2, 22, 4),
        ]),
    );
    let cols = 34;
    let rows = screen(&mut a, cols);
    fits(&rows, cols);
    let lw = a.rooms_left_w;
    assert!(lw < 13 + 3, "the pane is wide enough to cut nothing: {lw}");
    let one = left_of(row_with(&rows, "…4ef56"), lw);
    let two = left_of(row_with(&rows, "…4ef99"), lw);
    assert!(one.contains('…'), "the name was cut whole: {one:?}");
    assert_ne!(one.trim_end(), two.trim_end(), "both channels read as one");
    // The tail is what an end cut would have thrown away, and it is the distinguishing half.
    assert!(one.contains("ef56"), "{one:?}");
    assert!(two.contains("ef99"), "{two:?}");
}

/// The channel header carries the full name whatever the column did to it.
#[test]
fn the_channel_header_shows_the_whole_name() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 1, 0, 0), ("project/ab12cd34ef99", 2, 21, 3)]),
    );
    a.key(Key::Down);
    // The chat column is wide enough here to hold the whole key, where the cut column never
    // was: the header is where the full name lives.
    let rows = screen(&mut a, 80);
    let all = rows.join("\n");
    assert!(all.contains("project ab12cd34ef99"), "{all}");
}

#[test]
fn many_more_channels_than_rows_keep_the_selected_one_on_screen() {
    let mut a = opened(1_000);
    let ids: Vec<String> = (0..40).map(|i| format!("project/{i:08x}0000")).collect();
    let mut rows: Vec<(&str, u64, u64, u64)> = vec![("host", 1, 0, 0)];
    rows.extend(ids.iter().map(|i| (i.as_str(), 1, 0, 0)));
    reply(&mut a, rooms_reply(&rows));
    for _ in 0..30 {
        a.key(Key::Down);
    }
    let shown = screen(&mut a, 80);
    fits(&shown, 80);
    let all = shown.join("\n");
    assert!(all.contains("▸ #0000001d"), "{all}");
    assert!(!all.contains("#00000000"), "{all}");
}

#[test]
fn a_mention_is_marked_and_a_plain_message_is_not() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        reply(&mut a, rooms_reply(&[("host", 2, 2, 0)]));
        reply(
            &mut a,
            history(
                "host",
                vec![
                    msg(1, "host", "s-a", "session", "build is green"),
                    msg(2, "host", "s-b", "session", "@operator the disk is full"),
                ],
            ),
        );
        let rows = screen(&mut a, cols);
        let named = row_with(&rows, "the disk is full");
        assert!(named.contains("!"), "{named}");
        let plain = row_with(&rows, "build is green");
        assert!(!plain.contains("!"), "{plain}");
    }
}

#[test]
fn a_reply_carries_a_marker_with_the_number_it_answers() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        reply(&mut a, rooms_reply(&[("host", 2, 2, 0)]));
        let mut answer = msg(5, "host", "s-b", "session", "no, the other one");
        answer["thread"] = json!(3);
        reply(
            &mut a,
            history(
                "host",
                vec![msg(3, "host", "s-a", "session", "which disk?"), answer],
            ),
        );
        let rows = screen(&mut a, cols);
        let line = row_with(&rows, "no, the other one");
        assert!(line.contains("↳3 no, the other one"), "{line}");
        assert!(!row_with(&rows, "which disk?").contains('↳'));
    }
}

#[test]
fn a_todo_or_queue_line_is_one_quiet_row_with_its_ref() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        reply(&mut a, rooms_reply(&[("host", 2, 2, 0)]));
        let mut line = msg(2, "host", "todo", "system", "done: write the doc");
        line["refs"] = json!([{"kind": "todo", "id": "s-a/write the doc"}]);
        let mut queue = msg(3, "host", "queue", "system", "merged to main");
        queue["refs"] = json!([{"kind": "queue", "id": "m-41"}]);
        reply(&mut a, history("host", vec![line, queue]));
        let rows = screen(&mut a, cols);
        fits(&rows, cols);
        let todo = row_with(&rows, "write the doc");
        assert!(todo.contains("· todo: done: write the doc"), "{todo}");
        assert!(
            todo.contains("[todo s-a/write the doc]") || cols == 80,
            "{todo}"
        );
        assert!(
            !todo.contains("<todo>"),
            "a system line has no speaker brackets: {todo}"
        );
        let q = row_with(&rows, "merged to main");
        assert!(q.contains("[queue m-41]"), "{q}");
        assert_eq!(
            rows.iter().filter(|r| r.contains("merged to main")).count(),
            1
        );
    }
}

#[test]
fn a_long_message_wraps_inside_the_chat_column_and_loses_no_words() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        reply(&mut a, rooms_reply(&[("host", 2, 2, 0)]));
        let words: Vec<String> = (0..60).map(|i| format!("word{i}")).collect();
        let long = words.join(" ");
        reply(
            &mut a,
            history("host", vec![msg(1, "host", "s-a", "session", &long)]),
        );
        let rows = screen(&mut a, cols);
        fits(&rows, cols);
        let all = rows.join("\n");
        for w in &words {
            assert!(all.contains(w.as_str()), "{w} was lost at {cols}:\n{all}");
        }
        let used = rows.iter().filter(|r| r.contains("word")).count();
        assert!(used >= 3, "{used} rows at {cols}:\n{all}");
        // Continuation rows keep the chat column: the divider is still where it was.
        let at = |r: &str| r.chars().position(|c| c == '│');
        let first = at(row_with(&rows, "<s-a>"));
        for r in rows
            .iter()
            .filter(|r| r.contains("word") && !r.contains("<s-a>"))
        {
            assert_eq!(at(r), first, "{r:?}");
        }
    }
}

#[test]
fn a_dm_channel_is_listed_under_direct_messages_with_its_unread_count() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        reply(&mut a, rooms_reply(&[("host", 2, 0, 0)]));
        let mut first = msg(4, "dm", "s-a", "session", "psst");
        first["to"] = json!("operator");
        reply(&mut a, history("dm", vec![first]));
        let rows = screen(&mut a, cols);
        fits(&rows, cols);
        assert!(rows.join("\n").contains("direct messages"));
        let entry = left_of(row_with(&rows, "s-a"), a.rooms_left_w);
        assert!(entry.trim_end().ends_with('1'), "{entry:?}");

        a.key(Key::Down);
        let rows = screen(&mut a, cols);
        let all = rows.join("\n");
        assert!(all.contains("dm with s-a"), "{all}");
        assert!(row_with(&rows, "psst").contains("<s-a>"));
        assert_eq!(
            left_of(row_with(&rows, "▸ s-a"), a.rooms_left_w).trim_end(),
            "▸ s-a"
        );
    }
}

#[test]
fn the_roster_replaces_the_chat_and_goes_back() {
    for cols in WIDTHS {
        let mut a = opened(1_000);
        reply(&mut a, rooms_reply(&[("host", 2, 1, 0)]));
        reply(
            &mut a,
            history(
                "host",
                vec![msg(1, "host", "s-a", "session", "hello there")],
            ),
        );
        a.key(Key::Char('r'));
        reply(
            &mut a,
            json!({"type": "roster", "room": "host", "rows": [
                {"name": "operator", "kind": "operator", "connected": true, "project": "", "label": ""},
                {"name": "s-a", "kind": "session", "connected": false, "project": "", "label": "the doc writer"},
            ]}),
        );
        let rows = screen(&mut a, cols);
        fits(&rows, cols);
        let all = rows.join("\n");
        assert!(all.contains("2 in this channel"), "{all}");
        assert!(row_with(&rows, "operator  operator").contains('●'));
        assert!(row_with(&rows, "the doc writer").contains('○'));
        assert!(!all.contains("hello there"), "{all}");
        a.key(Key::Char('r'));
        assert!(screen(&mut a, cols).join("\n").contains("hello there"));
    }
}

#[test]
fn an_error_is_drawn_under_the_chat() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        json!({"error": "the room coordinator is not reachable"}),
    );
    let all = screen(&mut a, 80).join("\n");
    assert!(
        all.contains("the room coordinator is not reachable"),
        "{all}"
    );
}

#[test]
fn the_chat_stays_on_the_newest_message_and_page_up_goes_back() {
    let mut a = opened(1_000);
    reply(&mut a, rooms_reply(&[("host", 2, 60, 0)]));
    let rows: Vec<Value> = (1..=60)
        .map(|i| msg(i, "host", "s-a", "session", &format!("line number {i}")))
        .collect();
    reply(&mut a, history("host", rows));
    let all = screen(&mut a, 120).join("\n");
    assert!(all.contains("line number 60"), "{all}");
    assert!(!all.contains("line number 1 "), "{all}");
    a.key(Key::PageUp);
    let all = screen(&mut a, 120).join("\n");
    assert!(!all.contains("line number 60"), "{all}");
    // New news arrives while scrolled back: the view stays where it was.
    reply(
        &mut a,
        history(
            "host",
            vec![msg(61, "host", "s-a", "session", "line number 61")],
        ),
    );
    let all = screen(&mut a, 120).join("\n");
    assert!(!all.contains("line number 61"), "{all}");
    a.key(Key::PageDown);
    a.key(Key::PageDown);
    a.key(Key::PageDown);
    let all = screen(&mut a, 120).join("\n");
    assert!(all.contains("line number 61"), "{all}");
}

#[test]
fn the_view_survives_a_tiny_window() {
    let mut a = opened(1_000);
    reply(&mut a, rooms_reply(&[("host", 2, 1, 0)]));
    for (cols, h) in [(20, 8), (30, 4), (80, 3)] {
        let rows = a.screen(cols, h);
        fits(&rows, cols);
    }
}
