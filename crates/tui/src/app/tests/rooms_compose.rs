//! The composer in a room: the `@` completion and the room's `/` verbs, the history and the drafts
//! of an input, the reply target, the chips a paste leaves and what a submitted line becomes.
//!
//! The completion, the history and the drafts are reached through `app::chat_input`'s entry
//! points, which are the seam the key table calls; the send goes through `rooms_outgoing` and then
//! `rooms_say`, which is the pair the composer's dispatch makes. The daemon is a fake: replies are
//! built by hand, as `rooms.rs` does.

use super::rooms::{history as chat_history, msg, opened, reply, rooms_reply};
use super::*;
use crate::app::chat_input::complete::Complete;
use crate::app::rooms::Chan;
use letibot_plugin::complete::{Candidate, Completion};
use rano::style::Palette;
use serde_json::json;

const PROJECT: &str = "project/ab12cd34ef56";
/// The blob id a chip carries once P4's upload has answered. A fixture, not a hash.
const BLOB: &str = "9f2c4e0d";

/// The send as the composer's dispatch makes it: the line goes through `rooms_outgoing` for its
/// reply prefix and its blob references, the composer is emptied, and `rooms_say` threads it.
fn send(a: &mut App, text: &str) -> Option<Action> {
    let (line, _blobs) = a.rooms_outgoing(text);
    a.set_composer("");
    a.rooms_say(&line)
}

/// The host room's roster: one member here and connected, one here and away, the operator, and a
/// member of another room.
fn host_roster(a: &mut App) {
    reply(
        a,
        json!({"type": "roster", "room": "host", "rows": [
            {"name": "s-a", "kind": "session", "connected": true, "project": "", "label": "fix-room-push"},
            {"name": "s-b", "kind": "session", "connected": false, "project": "", "label": ""},
            {"name": "operator", "kind": "operator", "connected": true, "project": "", "label": ""},
        ]}),
    );
    reply(
        a,
        json!({"type": "roster", "room": PROJECT, "rows": [
            {"name": "s-z", "kind": "session", "connected": true, "project": "letibot", "label": "the doc writer"},
        ]}),
    );
}

/// A drawn line as the screen would carry it, plain: the row's words, not its colour.
fn drawn(line: rano::render::Line) -> String {
    crate::ui::render::row(&line, Palette::None)
}

// ─────────────────────────────── `@` completion ───────────────────────────────

/// Tab on an `@` word offers the room's roster, and the members the room can hear come before the
/// ones it cannot.
#[test]
fn at_completes_from_the_room_roster_connected_first() {
    let mut a = opened(1_000);
    host_roster(&mut a);
    a.set_composer("@");
    assert!(a.rooms_complete(), "tab opens the roster");
    let answer = a.rooms_completion().expect("an answer").clone();
    assert_eq!((answer.from, answer.to), (0, 1), "the span is the fragment");
    let inserts: Vec<&str> = answer
        .candidates
        .iter()
        .map(|c| c.insert.as_str())
        .collect();
    let people: Vec<&str> = inserts.iter().copied().filter(|i| *i != "@cc").collect();
    assert_eq!(
        people.len(),
        4,
        "the host room's three and the other room's one: {inserts:?}"
    );
    let away = answer
        .candidates
        .iter()
        .position(|c| c.insert == "@s-b")
        .expect("the away member is offered too");
    let elsewhere = answer
        .candidates
        .iter()
        .position(|c| c.insert == "@s-z")
        .expect("the other room's member is offered too");
    assert_eq!(answer.candidates[away].note.as_deref(), Some("away"));
    assert!(
        answer.candidates[..away]
            .iter()
            .all(|c| c.note.as_deref() != Some("away")),
        "every connected member ranks before the away one: {inserts:?}"
    );
    assert!(
        away < elsewhere,
        "this room before another one: {inserts:?}"
    );
    assert_eq!(
        a.input(),
        answer.candidates[0].insert,
        "the first is in the line"
    );
}

/// A member whose name starts with the fragment ranks before one whose name merely contains it.
#[test]
fn a_prefix_match_ranks_before_a_substring_match() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        json!({"type": "roster", "room": "host", "rows": [
            {"name": "s-1", "kind": "session", "connected": true, "project": "", "label": "prefixfix"},
            {"name": "s-2", "kind": "session", "connected": true, "project": "", "label": "fixer"},
        ]}),
    );
    a.set_composer("@fix");
    assert!(a.rooms_complete());
    let answer = a.rooms_completion().expect("an answer");
    assert_eq!(
        answer.candidates[0].insert, "@s-2",
        "the prefix match is first"
    );
    assert_eq!(
        answer.candidates[1].insert, "@s-1",
        "the substring match follows"
    );
}

/// The fragment is matched against the display name, and the handle is what gets inserted: the
/// server resolves handles, so a display name would post as nobody.
#[test]
fn completion_matches_a_display_name_and_inserts_the_handle() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        json!({"type": "roster", "room": "host", "rows": [
            {"name": "s-9f2c", "kind": "session", "connected": true, "project": "", "label": "the doc writer"},
        ]}),
    );
    a.set_composer("ask @doc");
    assert!(a.rooms_complete());
    let answer = a.rooms_completion().expect("an answer");
    let first = &answer.candidates[0];
    assert_eq!(first.insert, "@s-9f2c", "the handle is inserted");
    assert_eq!(
        first.label.as_deref(),
        Some("the doc writer"),
        "the display name is what the row shows"
    );
    assert_eq!(answer.from, 4, "the span starts at the `@`");
    assert_eq!(a.input(), "ask @s-9f2c");
}

/// A member of another room is offered, marked `elsewhere`, and behind the room's own.
#[test]
fn a_member_of_another_room_is_marked_elsewhere() {
    let mut a = opened(1_000);
    host_roster(&mut a);
    a.set_composer("@s-");
    assert!(a.rooms_complete());
    let answer = a.rooms_completion().expect("an answer");
    let elsewhere = answer
        .candidates
        .iter()
        .find(|c| c.insert == "@s-z")
        .expect("the other room's member is offered");
    assert_eq!(elsewhere.note.as_deref(), Some("elsewhere"));
    assert_eq!(elsewhere.label.as_deref(), Some("the doc writer"));
    let here = answer
        .candidates
        .iter()
        .position(|c| c.insert == "@s-a")
        .expect("this room's member");
    let there = answer
        .candidates
        .iter()
        .position(|c| c.insert == "@s-z")
        .expect("the other room's");
    assert!(here < there, "this room first: {:?}", answer.candidates);
}

/// Tab walks the candidates and Esc closes the row without touching the line it wrote.
#[test]
fn tab_cycles_and_esc_closes_the_completion() {
    let mut a = opened(1_000);
    host_roster(&mut a);
    a.set_composer("@");
    assert!(a.rooms_compose_key(&Key::Tab), "tab is the composer's here");
    let answer = a.rooms_completion().expect("an answer").clone();
    let expected: Vec<String> = answer.candidates.iter().map(|c| c.insert.clone()).collect();
    assert_eq!(
        expected,
        ["@operator", "@s-a", "@s-b", "@s-z", "@cc"],
        "here and connected, here and away, elsewhere, the bridge alias"
    );
    assert_eq!(
        a.input(),
        expected[0],
        "the first candidate is written on the first tab"
    );
    for next in expected.iter().skip(1) {
        assert!(a.rooms_compose_key(&Key::Tab));
        assert_eq!(a.input(), *next);
    }
    assert!(a.rooms_compose_key(&Key::Tab));
    assert_eq!(a.input(), expected[0], "and round again");
    assert!(a.rooms_completion().is_some(), "the row is still up");
    assert!(a.rooms_compose_key(&Key::Esc));
    assert!(a.rooms_completion().is_none(), "esc closed it");
    assert_eq!(a.input(), expected[0], "and left the line alone");
}

/// A `/` in a room offers the room's own verbs before the head's.
#[test]
fn slash_in_a_room_offers_room_verbs_first() {
    let mut a = opened(1_000);
    a.set_composer("/");
    assert!(a.rooms_complete());
    let answer = a.rooms_completion().expect("an answer");
    let inserts: Vec<&str> = answer
        .candidates
        .iter()
        .map(|c| c.insert.as_str())
        .collect();
    assert_eq!(
        &inserts[..4],
        ["/dm ", "/invite ", "/leave ", "/reply "],
        "the room's verbs first: {inserts:?}"
    );
    assert_eq!(
        a.input(),
        "/dm ",
        "the verb and the space its argument is typed on from"
    );
    // A needle both halves answer finds the room's verb first, because the list is the room's
    // ahead of the head's and the rank only reorders within one prefix.
    a.set_composer("/r");
    assert!(a.rooms_complete());
    let answer = a.rooms_completion().expect("an answer");
    assert_eq!(answer.candidates[0].insert, "/reply ");
}

// ─────────────────────────── history and drafts ───────────────────────────

/// Up on an empty composer recalls the channel's last line; Down walks back to the empty line.
#[test]
fn up_on_an_empty_composer_recalls_this_channels_last_line() {
    let mut a = opened(1_000);
    send(&mut a, "build is green");
    assert_eq!(a.input(), "", "the send emptied the composer");
    assert!(a.rooms_compose_key(&Key::Up));
    assert_eq!(a.input(), "build is green");
    assert!(a.rooms_compose_key(&Key::Down));
    assert_eq!(
        a.input(),
        "",
        "past the newest line is the line being typed"
    );
    // Up on a typed line is the editor's: a wrapped line moves by a row, not by a message.
    a.set_composer("half a thought");
    assert!(!a.rooms_history(true), "not ours while something is typed");
    assert_eq!(a.input(), "half a thought");
}

/// Each channel recalls its own lines.
#[test]
fn history_is_per_channel() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 0, 0), (PROJECT, 2, 0, 0)]),
    );
    let host = Chan::Room("host".into());
    let project = Chan::Room(PROJECT.into());
    send(&mut a, "in the host room");
    a.rooms_switched(Some(&host), &project);
    send(&mut a, "in the project room");
    a.rooms_switched(Some(&project), &host);
    assert!(a.rooms_compose_key(&Key::Up));
    assert_eq!(a.input(), "in the host room");
    a.set_composer("");
    a.rooms_switched(Some(&host), &project);
    assert!(a.rooms_compose_key(&Key::Up));
    assert_eq!(a.input(), "in the project room");
}

/// A switch keeps the unsent line of the channel being left and restores it on return.
#[test]
fn a_draft_survives_a_channel_switch() {
    let mut a = opened(1_000);
    let host = Chan::Room("host".into());
    let project = Chan::Room(PROJECT.into());
    a.set_composer("half a thought");
    a.rooms_switched(Some(&host), &project);
    assert_eq!(a.input(), "", "the other channel has nothing unsent");
    a.set_composer("another");
    a.rooms_switched(Some(&project), &host);
    assert_eq!(a.input(), "half a thought");
    a.rooms_switched(Some(&host), &project);
    assert_eq!(
        a.input(),
        "another",
        "and the project's line is still its own"
    );
}

/// The two are keyed by the input's `key`, not by a channel: an input that is not a channel at all
/// keeps its own history and its own draft, which is how a plugin's chat pane gets them.
#[test]
fn history_and_drafts_are_keyed_by_the_input_key() {
    let mut a = opened(1_000);
    a.rooms_input_key_set("plugin/chat");
    a.set_composer("a draft for the layer");
    a.rooms_input_key_set("host");
    assert_eq!(a.input(), "", "the room's input is not the layer's");
    send(&mut a, "said in the room");
    a.rooms_input_key_set("plugin/chat");
    assert_eq!(
        a.input(),
        "a draft for the layer",
        "the layer's draft came back"
    );
    a.set_composer("");
    assert!(
        !a.rooms_history(true),
        "the layer has sent nothing: the room's lines are not its history"
    );
    send(&mut a, "said in the layer");
    assert!(a.rooms_compose_key(&Key::Up));
    assert_eq!(a.input(), "said in the layer");
    a.set_composer("");
    a.rooms_input_key_set("host");
    assert!(a.rooms_compose_key(&Key::Up));
    assert_eq!(a.input(), "said in the room");
}

// ─────────────────────────── the reply target ───────────────────────────

/// `R` on a selected message sets the reply target, the composer shows what is being answered, and
/// Esc clears it. A sent line carries the target as the `>N` thread.
#[test]
fn r_on_a_selected_message_replies_and_esc_clears_it() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        chat_history(
            "host",
            vec![msg(14, "host", "gk", "session", "merged the blob store")],
        ),
    );
    // What `Act::RoomReply` does with the message under the cursor.
    let seq = a.rooms_message(14).expect("the message is loaded").seq;
    a.rooms_set_reply(Some(seq));
    assert_eq!(a.rooms_reply_target(), Some(14));
    let strip = drawn(a.rooms_reply_row(80).expect("the strip is up"));
    assert!(strip.contains("#14"), "{strip}");
    assert!(strip.contains("gk"), "{strip}");
    assert!(strip.contains("merged the blob store"), "{strip}");

    let Some(Action::RoomChat { op, .. }) = send(&mut a, "agreed") else {
        panic!("the line did not go");
    };
    assert_eq!(op["text"], "agreed", "the body is what was typed");
    assert_eq!(op["thread"], 14, "the reply target is the thread");
    assert_eq!(a.rooms_reply_target(), None, "one send answers one message");

    a.rooms_set_reply(Some(14));
    assert!(a.rooms_compose_key(&Key::Esc));
    assert_eq!(a.rooms_reply_target(), None);
    assert!(a.rooms_reply_row(80).is_none(), "and the strip is gone");
}

// ─────────────────────────── the line and the paste ───────────────────────────

/// Alt+Enter adds a line to the composer and Enter sends both: a room message is one message, not
/// two, so the newline is the message's and not the send's.
#[test]
fn alt_enter_adds_a_line_and_enter_sends_both() {
    let mut a = opened(1_000);
    typed(&mut a, "the gate is green");
    a.key(Key::SoftEnter);
    typed(&mut a, "and the queue is empty");
    assert_eq!(a.input(), "the gate is green\nand the queue is empty");
    let Some(Action::RoomChat { op, .. }) = a.key(Key::Enter) else {
        panic!("enter did not send");
    };
    assert_eq!(op["type"], "say");
    assert_eq!(op["text"], "the gate is green\nand the queue is empty");
}

/// A pasted picture becomes a chip in the line and a `![name](blob:ID)` reference in what goes
/// out, with the id on the say.
#[test]
fn a_pasted_picture_becomes_a_chip_and_a_blob_reference() {
    let mut a = opened(1_000);
    // P4's upload queues the parts and answers the chip at once; its blob id arrives with the
    // room's reply, so the chip is in the line before it names anything.
    assert!(a.rooms_paste_picture(vec![0x89, b'P', b'N', b'G'], "shot.png"));
    assert!(a.input().starts_with("[image 1 · png"), "{}", a.input());
    assert_eq!(a.compose.chips.len(), 1);

    // A chip whose id is known goes out as a blob reference with the id on the say.
    let mut a = opened(1_000);
    a.rooms_chip("[shot.png]", Some(BLOB), "shot.png");
    assert_eq!(a.input(), "[shot.png]", "the chip is in the line");
    let (line, blobs) = a.rooms_outgoing("[shot.png] the gate");
    assert_eq!(line, format!("![shot.png](blob:{BLOB}) the gate"));
    assert_eq!(blobs, vec![BLOB.to_string()], "the say carries the id");
}

// ─────────────────────────── the completion row ───────────────────────────

/// An implementation of `Complete` that is not the room's: the row draws what it answers, so any
/// layer's `complete_` hook gets the same row.
struct Fake;

impl Complete for Fake {
    fn complete(&self, text: &str, cursor: usize) -> Completion {
        Completion {
            from: 0,
            to: cursor.min(text.len()),
            candidates: vec![
                Candidate {
                    insert: "@s-a".into(),
                    label: Some("fix-room-push".into()),
                    note: Some("here".into()),
                    hue: Some(3),
                },
                Candidate {
                    insert: "@cc".into(),
                    ..Candidate::default()
                },
            ],
        }
    }
}

/// The row the composer's edge draws is the answer it was handed: the label, the note, and the
/// insert of a candidate that named no label. Nothing here reads the roster.
#[test]
fn the_completion_row_draws_what_a_complete_impl_answers() {
    let answer = Fake.complete("@fi", 3);
    answer.validate("@fi").expect("the host can apply it");
    let row = crate::ui::chat_compose::completion_row(&answer, 0, 80).expect("a row");
    let text = drawn(row);
    assert!(text.contains("fix-room-push here"), "{text}");
    assert!(
        text.contains("@cc"),
        "a candidate with no label draws its insert: {text}"
    );
    assert!(text.contains(" · "), "the candidates are one row: {text}");
    assert!(
        !text.contains("@s-a fix-room-push"),
        "the label is drawn, not both: {text}"
    );

    // A dozen columns cut the note before the name, because the name is what is being chosen.
    let narrow = drawn(crate::ui::chat_compose::completion_row(&answer, 0, 12).expect("a row"));
    assert!(
        crate::ui::render::visible_width(&narrow) <= 12,
        "{narrow:?} is wider than 12"
    );
    assert!(
        narrow.contains("fix-room"),
        "the name survives the cut: {narrow:?}"
    );

    // The row is the room's too: what Tab left open is what the edge would draw.
    let mut a = opened(1_000);
    host_roster(&mut a);
    a.set_composer("@");
    assert!(a.rooms_complete());
    let text = drawn(a.rooms_completion_row(80).expect("the room's row"));
    assert!(text.contains("fix-room-push"), "{text}");
}

/// A line typed in a room goes out as itself: `rooms_outgoing` adds no blob reference and names no
/// attachment, because there is no chip.
#[test]
fn a_typed_line_goes_out_unchanged() {
    let mut a = opened(1_000);
    let (text, blobs) = a.rooms_outgoing(">14 the gate is green");
    assert_eq!(text, ">14 the gate is green", "byte for byte");
    assert!(blobs.is_empty(), "nothing to upload: {blobs:?}");
}
