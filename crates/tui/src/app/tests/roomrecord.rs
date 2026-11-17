//! A room post long enough for the room to file it as a record reaches the session as its first
//! line and a pointer:
//!
//!   #221 [dm] from cc@etcd-a36 (external) to dm
//!   > Tranche, in order of value: … [record #221, 1645 bytes: room read #221]
//!
//! The row is what the model was sent, and the model reads the rest with `room_read`. The details
//! popup resolves that pointer instead: it asks the daemon for the message in this session's own
//! name and shows it whole. See `app::diff_popup`.

use super::*;

/// The row the room sent, as `room::chat::record_line` spells a filed post.
const STUB: &str = "#221 [dm] from cc@etcd-a36 (external) to dm\n> Tranche, in order of value: … \
                    [record #221, 1645 bytes: room read #221]";

/// The message the room holds under #221 - what only a read can answer.
const WHOLE: &str = "Tranche, in order of value:\n\n1. the parser takes the caller's shape\n2. \
                     the caller keeps its own copy until the parser is green";

/// A head whose one row is `row`, spoken by the harness in place of whoever sent it.
fn with_row(row: &str) -> App {
    let hub = Hub::new("s");
    hub.publish(testing::appended("s.1", "user"));
    hub.record_item(
        "s.1",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text: row.into() }],
        },
    );
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a
}

/// The room-chat op this head queued, if any: the one door to a record's text.
fn asked(a: &App) -> Option<serde_json::Value> {
    a.queued.iter().find_map(|act| match act {
        Action::RoomChat { op, .. } if op["type"] == "record" => Some(op.clone()),
        _ => None,
    })
}

/// The popup's text, opened by the click the row's line stands for.
fn popup(a: &mut App) -> String {
    let screen = a.screen(140, 40).join("\n");
    assert!(
        screen.contains("fetched from the room") || screen.contains("the room could not"),
        "no popup is on the screen:\n{screen}"
    );
    screen
}

/// A record row's popup asks for the message by its seq, and the room's answer is shown whole:
/// the row on the conversation still carries the pointer, because it is what the model was sent.
#[test]
fn a_record_rows_details_are_the_whole_message_from_the_room() {
    let mut a = with_row(STUB);
    a.open_notice_popup(0);
    let op = asked(&a).expect("the popup asks the room for the record");
    assert_eq!(op["seq"], 221, "{op}");
    assert_eq!(op["room"], "dm", "{op}");
    assert_eq!(op["session"], "s", "{op}");

    a.on_room_chat(
        "record:dm:221",
        serde_json::json!({"type": "record", "room": "dm", "seq": 221, "text": WHOLE}),
    );
    let screen = popup(&mut a);
    assert!(
        screen.contains("the caller keeps its own copy"),
        "the whole message is in the popup:\n{screen}"
    );
    assert!(
        screen.contains("the model was sent this row's first line and a pointer"),
        "the popup says what it is:\n{screen}"
    );
    // The row itself is untouched: the conversation still holds the pointer the model was sent,
    // and only the popup's copy carries the whole text.
    assert_eq!(
        crate::ui::transcript::user::agent_row_text(&a.items[0]).as_deref(),
        Some(STUB),
        "the row the model was sent is unchanged"
    );
}

/// A read that cannot be answered keeps the row's stub and says why, in one sentence: the popup
/// is never a blank box and never a claim that the message was read.
#[test]
fn a_record_the_room_cannot_answer_shows_the_stub_and_why() {
    let mut a = with_row(STUB);
    a.open_notice_popup(0);
    a.on_room_chat(
        "record:dm:221",
        serde_json::json!({
            "type": "record",
            "seq": 221,
            "error": "the room no longer holds #221",
        }),
    );
    let screen = popup(&mut a);
    assert!(
        screen.contains("the room could not give #221 back: the room no longer holds #221"),
        "the reason is on the popup:\n{screen}"
    );
    assert!(
        screen.contains("Tranche, in order of value"),
        "the row's own stub is still there:\n{screen}"
    );
    assert!(
        !screen.contains("fetched from the room"),
        "a failed read claims nothing was fetched:\n{screen}"
    );
}

/// A room message the room carried whole has no pointer, so the popup is the row's text and the
/// room is not asked anything.
#[test]
fn a_short_room_message_opens_as_it_was_sent() {
    let short = "#41 [project] from dead (say) to all\n> the gate is green, landing it now";
    let mut a = with_row(short);
    a.open_notice_popup(0);
    assert!(
        asked(&a).is_none(),
        "a row with no pointer asks the room nothing"
    );
    let text = match a.diff_popup.as_ref().map(|p| &p.body) {
        Some(crate::app::diff_popup::PopupBody::Notice { text, .. }) => text.clone(),
        _ => panic!("the short row opens no popup"),
    };
    assert_eq!(text, short);
}
