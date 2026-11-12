//! P4: moving through the chat rather than through the channels. The message cursor, the jump to
//! the tail and to the unread divider, the `N new` count, `Ctrl+K`, `/search`, and the pictures a
//! message carries - fetched once, kept in a bounded cache, opened whole, and uploaded in parts.
//!
//! Every one of them acts on the entries of a STREAM rather than on the rows a message was drawn
//! on, which is what makes the last test here possible: a plugin's `Block::Stream` is walked by the
//! same arrows, with no line of the pane knowing what a room is.

use super::*;
use crate::app::chat_media::{PART, PICTURES};
use crate::app::chat_nav::Jump;
use serde_json::json;

use super::rooms::{history, msg, opened, ops, reply, reply_as, rooms_reply};
use crate::app::rooms::Chan;

/// The host room with `n` messages, oldest first, and the poll's answer already in.
fn channels(a: &mut App, rows: &[(&str, u64, u64, u64)]) {
    reply(a, rooms_reply(rows));
}

/// `seq`s 1..=n said in the host room, at `t` seconds apart.
fn said(seqs: std::ops::RangeInclusive<u64>) -> Vec<serde_json::Value> {
    seqs.map(|s| msg(s, "host", "s-a", "chat", &format!("line {s}")))
        .collect()
}

/// The selected channel's messages, as the head has them.
fn loaded(a: &App) -> Vec<u64> {
    let c = a.rooms.selected().expect("a channel");
    a.rooms.messages(&c).iter().map(|m| m.seq).collect()
}

/// One blob's bytes, as a `get_blob` answer's last part: what the coordinator sends.
fn blob_part(id: &str, bytes: &[u8], last: bool, offset: u64) -> serde_json::Value {
    json!({
        "type": "blob_part",
        "attachment": {"id": id, "mime": "image/png", "name": "shot.png",
                       "bytes": bytes.len(), "w": 640, "h": 320},
        "offset": offset,
        "data": letibot_transcript::media::encode_base64(bytes),
        "last": last,
    })
}

/// A PNG's worth of bytes that is not a PNG: nothing in the cache decodes them, so a fixture only
/// has to be the right length.
fn bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// A picture message: one attachment and the `![…](blob:…)` the body names it by.
fn picture(seq: u64, id: &str) -> serde_json::Value {
    json!({"seq": seq, "ts": 1_793_735_794_000u64 + seq * 60_000, "room": "host",
           "author": "s-a", "kind": "chat",
           "attachments": [{"id": id, "mime": "image/png", "name": "shot.png",
                            "bytes": 12, "w": 640, "h": 320}],
           "text": format!("look ![shot](blob:{id})")})
}

// ─────────────────────────── the message cursor ───────────────────────────

/// Tab on an empty composer selects a message, and the newest one: the reader joins the chat at its
/// tail. With no line typed, the key is the view's; with one, it is the composer's and Tab is
/// still the completion.
#[test]
fn tab_on_an_empty_composer_selects_the_newest_message() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 5, 0)]);
    reply(&mut a, history("host", said(1..=5)));
    assert_eq!(loaded(&a), vec![1, 2, 3, 4, 5]);
    assert_eq!(a.rooms_cursor(), None, "no cursor until Tab");

    a.key(Key::Tab);
    assert_eq!(a.rooms_cursor(), Some(5), "tab selects the newest message");
    assert_eq!(a.editor.text(), "", "and nothing reached the composer");

    // Esc gives the keys back to the composer without leaving the view.
    a.key(Key::Esc);
    assert_eq!(a.rooms_cursor(), None);
    assert!(
        a.is_open(Surface::Rooms),
        "esc left the cursor, not the view"
    );

    // With a line typed, Tab is the composer's completion again - the guard the binding carries.
    typed(&mut a, "half a li");
    a.key(Key::Tab);
    assert_eq!(a.rooms_cursor(), None, "a typed line keeps tab");
}

/// The arrows move by MESSAGE: a wall of five messages is five steps, not the twenty-odd rows the
/// five of them are drawn on. And with a cursor up they stop moving the channel.
#[test]
fn up_and_down_move_by_message_not_by_line() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 6, 0), ("project/ab12", 2, 6, 0)]);
    reply(&mut a, history("host", said(1..=6)));
    let width = 80;
    let mut text = String::new();
    for i in 1..=6 {
        text.push_str(&format!(
            "line {i} — a body long enough to wrap onto a second row here.\n"
        ));
    }
    reply(
        &mut a,
        history(
            "host",
            (1..=6)
                .map(|s| msg(s, "host", "s-a", "chat", &text))
                .collect(),
        ),
    );

    a.key(Key::Tab);
    assert_eq!(a.rooms_cursor(), Some(6));
    a.key(Key::Up);
    assert_eq!(a.rooms_cursor(), Some(5), "one message, not one row");
    a.key(Key::Up);
    a.key(Key::Up);
    assert_eq!(a.rooms_cursor(), Some(3));
    a.key(Key::Down);
    assert_eq!(a.rooms_cursor(), Some(4));
    // A cursor at an end stays there rather than wrapping the chat and losing the reader's place.
    for _ in 0..8 {
        a.key(Key::Down);
    }
    assert_eq!(a.rooms_cursor(), Some(6));
    // And the channel did not move: the arrows were the cursor's.
    assert_eq!(a.rooms.sel, Some(Chan::Room("host".into())));
    // The chat is drawn on `width` columns with the cursor on the newest message: the frame still
    // fits, so a selected block is not a row that overflowed.
    let rows = a.screen(width, 24);
    for r in &rows {
        assert!(
            crate::ui::render::visible_width(r) <= width,
            "{width} columns: {r:?}"
        );
    }
}

// ─────────────────────────── jumps ───────────────────────────

/// `u` goes to the unread divider - the first message newer than the mark the channel had when it
/// was opened - and `End` and `G` go back to the newest.
#[test]
fn u_jumps_to_the_unread_divider() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 4, 0), ("project/ab12", 2, 0, 0)]);
    reply(&mut a, history("host", said(1..=4)));
    // Read to the end of the host room, then leave it and come back: the divider's place is the
    // seen mark the channel had at the moment it was opened, and four messages have been seen.
    a.key(Key::Down);
    a.key(Key::Up);
    assert_eq!(
        a.rooms.opened_seen.get("host").copied(),
        Some(4),
        "the divider sits before the first message newer than what was seen at the open"
    );

    // Four more arrive while the reader is there.
    reply(&mut a, history("host", said(5..=8)));
    let _ = a.rooms_jump(Jump::Unread);
    assert_eq!(a.rooms_cursor(), Some(5), "the first unseen message");

    let _ = a.rooms_jump(Jump::Tail);
    assert_eq!(a.rooms_cursor(), Some(8), "end goes to the newest");

    a.key(Key::Char('u'));
    assert_eq!(a.rooms_cursor(), Some(5), "`u` is the divider again");
    a.key(Key::Char('G'));
    assert_eq!(a.rooms_cursor(), Some(8), "`G` is the tail");
    a.key(Key::End);
    assert_eq!(a.rooms_cursor(), Some(8));
    assert_eq!(a.editor.text(), "", "none of those reached the composer");
}

/// Scrolled back, what lands is counted and named - `N new - End jumps` - and the count is the
/// messages after the ones the reader had at the tail. Jumping to the tail ends it.
#[test]
fn new_messages_while_scrolled_back_say_how_many() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 30, 0)]);
    reply(&mut a, history("host", said(1..=30)));
    // Drawn once at the tail: the baseline is the newest message, and there is nothing behind the
    // reader yet.
    a.screen(80, 12);
    assert_eq!(a.rooms_new_footer(), None);

    // The reader scrolls back, and the next draw is the one that knows they did.
    a.key(Key::PageUp);
    a.screen(80, 12);
    assert!(a.pane_scroll < a.rooms.prev_max, "scrolled back");

    reply(&mut a, history("host", said(31..=33)));
    assert_eq!(a.rooms_new_count(), Some(3));
    assert_eq!(a.rooms_new_footer().as_deref(), Some("3 new - End jumps"));

    // At the tail again, the count is over: the chat follows them.
    a.key(Key::End);
    assert_eq!(a.rooms_new_count(), None);
    assert_eq!(a.rooms_new_footer(), None);
    assert_eq!(a.rooms_cursor(), Some(33), "end landed on the newest");
}

// ─────────────────────────── ctrl-k ───────────────────────────

/// `Ctrl+K` lists the channels, narrows as a name is typed, puts what is unread first, and Enter
/// switches to the row under the cursor.
#[test]
fn ctrl_k_filters_channels_and_enter_switches() {
    let mut a = opened(1_000);
    channels(
        &mut a,
        &[
            ("host", 2, 9, 0),
            ("project/ab12", 3, 4, 3),
            ("project/shipyard", 1, 2, 0),
        ],
    );
    reply(&mut a, history("host", said(1..=2)));

    assert!(matches!(
        a.rooms_switcher(),
        std::ops::ControlFlow::Break(None)
    ));
    let rows = crate::app::chat_nav::shown(&a);
    assert_eq!(rows[0].label, "#ab12", "unread first: {rows:?}");
    assert_eq!(rows[0].unread, 3);

    // Typed into the switcher, not into the composer.
    typed(&mut a, "ship");
    assert_eq!(a.editor.text(), "", "the query is the switcher's");
    let rows = crate::app::chat_nav::shown(&a);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].label, "#shipyard");

    a.key(Key::Enter);
    assert!(a.nav.switcher.is_none(), "enter closed the list");
    assert_eq!(
        a.rooms.sel,
        Some(Chan::Room("project/shipyard".into())),
        "enter switched to the row under the cursor"
    );

    // A fuzzy match still finds a row, and Esc puts the list away without switching.
    a.key(Key::KillToEnd);
    typed(&mut a, "hs");
    let rows = crate::app::chat_nav::shown(&a);
    assert!(
        rows.iter().any(|r| r.label == "host"),
        "the letters in order find `host`: {rows:?}"
    );
    a.key(Key::Esc);
    assert!(a.nav.switcher.is_none());
    assert_eq!(a.rooms.sel, Some(Chan::Room("project/shipyard".into())));
}

// ─────────────────────────── search ───────────────────────────

/// `/search` finds the hits among the messages the channel has loaded, the chat is drawn with the
/// needle for its highlight, and `n` and `N` step through the hits - wrapping at both ends.
#[test]
fn search_highlights_hits_and_n_steps_through_them() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 6, 0)]);
    let rows = vec![
        msg(1, "host", "s-a", "chat", "morning"),
        msg(2, "host", "s-b", "chat", "the gate was green"),
        msg(3, "host", "s-a", "chat", "lunch"),
        msg(4, "host", "s-b", "chat", "gate again, red this time"),
        msg(5, "host", "s-a", "chat", "evening"),
    ];
    reply(&mut a, history("host", rows));

    a.command("search gate");
    assert_eq!(a.rooms_search_needle(), Some("gate"), "the needle is kept");
    assert_eq!(a.rooms_cursor(), Some(2), "the first hit");

    a.key(Key::Char('n'));
    assert_eq!(a.rooms_cursor(), Some(4), "the second hit");
    a.key(Key::Char('n'));
    assert_eq!(a.rooms_cursor(), Some(2), "n wraps to the first");
    a.key(Key::Char('N'));
    assert_eq!(a.rooms_cursor(), Some(4), "N walks back, wrapping");

    // A needle nothing carries says so, and closing the search takes the needle with it.
    a.command("search nothing-matches-this");
    assert_eq!(a.rooms_search_needle(), Some("nothing-matches-this"));
    a.command("search");
    assert_eq!(a.rooms_search_needle(), None);
}

/// A search is a read of the room's LOG, not of the messages this head happens to hold: the room
/// is asked, its hits merge into the channel like any other history, and the cursor lands on one
/// the window never had.
#[test]
fn a_search_asks_the_room_and_its_answer_loads_what_the_window_dropped() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 6, 0)]);
    reply(
        &mut a,
        history("host", vec![msg(6, "host", "s-a", "chat", "lately")]),
    );

    a.command("search gate");
    // The room is asked, because the loaded rows have no hit; the answer comes back under the id.
    let asked = a.take_actions().into_iter().find_map(|x| match x {
        Action::RoomChat { op, id } if op["type"] == "search" => Some((op, id)),
        _ => None,
    });
    assert_eq!(
        asked,
        Some((
            json!({"type": "search", "room": "host", "text": "gate", "last": 200}),
            Some("search:host".into())
        ))
    );

    // The answer is an ordinary history frame, under the search's own id.
    reply_as(
        &mut a,
        "search:host",
        history(
            "host",
            vec![msg(2, "host", "s-b", "chat", "the gate was green")],
        ),
    );
    assert_eq!(
        loaded(&a),
        vec![2, 6],
        "the log's hit is loaded beside the ring's"
    );
    assert_eq!(a.rooms_cursor(), Some(2), "and the cursor lands on it");
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("the room's log"), "{said}");
}

// ─────────────────────────── pictures ───────────────────────────

/// A blob is asked for once per id, however many frames draw it, and once the parts are whole it is
/// drawn from the cache - no second `get_blob`.
#[test]
fn a_picture_is_fetched_once_and_drawn_from_the_cache() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 1, 0)]);
    reply(&mut a, history("host", vec![picture(1, "abc123")]));
    a.take_actions();

    let blob = a.rooms.messages(&Chan::Room("host".into()))[0].attachments[0].clone();
    for _ in 0..3 {
        a.room_pictures_wanted(&[&blob]);
    }
    let asks = ops(&mut a);
    assert_eq!(asks.len(), 1, "one ask for one id: {asks:?}");
    assert_eq!(asks[0]["type"], "get_blob");
    assert_eq!(asks[0]["id"], "abc123");
    assert_eq!(asks[0]["offset"], 0);
    assert_eq!(a.room_picture("abc123"), None, "not fetched yet");

    let data = bytes(4);
    reply_as(&mut a, "blob:abc123", blob_part("abc123", &data, true, 0));
    let (id, w, h) = a.room_picture("abc123").expect("drawn from the cache");
    assert_ne!(id, 0);
    assert_eq!((w, h), (640, 320), "the size the coordinator read");

    // Frames go on drawing it, and nothing is asked for a second time.
    for _ in 0..3 {
        a.room_pictures_wanted(&[&blob]);
        assert_eq!(a.room_picture("abc123").map(|p| p.0), Some(id));
    }
    assert!(
        ops(&mut a).is_empty(),
        "the cache answered, so nothing asked"
    );
}

/// A blob over one part comes in parts, each asking for the offset after it - and only the last one
/// finishes the picture.
#[test]
fn a_picture_arrives_in_parts_and_only_the_last_one_draws_it() {
    let mut a = opened(1_000);
    let blob = crate::app::rooms::Attach {
        id: "big".into(),
        mime: "image/png".into(),
        name: "big.png".into(),
        bytes: (PART as u64) + 8,
        w: Some(80),
        h: Some(40),
    };
    a.room_pictures_wanted(&[&blob]);
    assert_eq!(ops(&mut a).len(), 1);

    let first = bytes(PART);
    reply_as(&mut a, "blob:big", blob_part("big", &first, false, 0));
    let asks = ops(&mut a);
    assert_eq!(asks.len(), 1, "the next part is asked for");
    assert_eq!(asks[0]["offset"], PART, "from where the first ended");
    assert_eq!(a.room_picture("big"), None, "half a picture is no picture");

    reply_as(
        &mut a,
        "blob:big",
        blob_part("big", &bytes(8), true, PART as u64),
    );
    assert!(a.room_picture("big").is_some(), "the last part finishes it");
}

/// The cache is bounded and drops the picture drawn longest ago, so a chat scrolled through a
/// morning does not keep every picture it passed.
#[test]
fn the_picture_cache_drops_the_least_recently_drawn() {
    let mut a = opened(1_000);
    for i in 0..PICTURES {
        let id = format!("b{i}");
        reply_as(&mut a, "blob:x", blob_part(&id, &bytes(8), true, 0));
    }
    for i in 0..PICTURES {
        assert!(a.room_picture(&format!("b{i}")).is_some(), "b{i} is held");
    }
    // The first is drawn again, so it is no longer the one drawn longest ago.
    a.room_picture("b0");
    reply_as(&mut a, "blob:x", blob_part("fresh", &bytes(8), true, 0));
    assert!(a.room_picture("fresh").is_some());
    assert!(a.room_picture("b1").is_none(), "b1 went: drawn longest ago");
    assert!(a.room_picture("b0").is_some(), "b0 stayed: just drawn");
}

/// An upload leaves in parts no larger than the coordinator's: base64 of half a megabyte each, one
/// per frame, `last` on the final one.
#[test]
fn an_upload_goes_in_parts_under_the_frame_limit() {
    let mut a = opened(1_000);
    let png = bytes(PART * 2 + 7);
    let chip = a.room_upload(png.clone(), "shot.png");
    assert!(
        chip.contains("shot.png") || chip.contains("png"),
        "{chip:?}"
    );

    let puts = ops(&mut a);
    assert_eq!(puts.len(), 3, "three parts for two and a bit");
    let mut whole = Vec::new();
    for (i, op) in puts.iter().enumerate() {
        assert_eq!(op["type"], "put_blob");
        assert_eq!(op["part"], i);
        assert_eq!(op["last"], i == 2, "only the last finishes the upload");
        assert_eq!(op["mime"], "image/png");
        let data = op["data"].as_str().unwrap();
        let part = letibot_transcript::media::decode_base64(data);
        assert!(
            part.len() <= PART,
            "part {i} is {} bytes, over the cap",
            part.len()
        );
        assert!(
            data.len() < 1024 * 1024,
            "a part stays under the frame limit"
        );
        whole.extend_from_slice(&part);
    }
    assert_eq!(whole, png, "the parts are the bytes, in order");

    // The answer names the blob, and the chip the composer holds becomes the reference a say
    // carries.
    reply_as(
        &mut a,
        "put:u",
        json!({"type": "blob", "attachment": {"id": "deadbeef", "mime": "image/png",
                                               "name": "shot.png", "bytes": png.len()}}),
    );
    let (text, ids) = a.room_outgoing_chips(&format!("look {chip}"));
    assert_eq!(ids, vec!["deadbeef".to_string()]);
    assert!(text.contains("![shot.png](blob:deadbeef)"), "{text}");
}

/// Enter on a message carrying a picture opens it whole, in the overlay the conversation's
/// thumbnails open in.
#[test]
fn enter_on_a_picture_message_opens_the_overlay() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 1, 0)]);
    reply(&mut a, history("host", vec![picture(1, "abc123")]));
    a.take_actions();
    reply_as(
        &mut a,
        "blob:abc123",
        blob_part("abc123", &bytes(24), true, 0),
    );

    a.key(Key::Tab);
    assert_eq!(a.rooms_cursor(), Some(1));
    a.key(Key::Enter);
    match a.diff_popup.as_ref().map(|p| &p.body) {
        Some(crate::app::diff_popup::PopupBody::Picture { px, .. }) => {
            assert_eq!(*px, (Some(640), Some(320)), "the picture, at its own size");
        }
        other => panic!("enter opened no picture: {other:?}"),
    }
    assert_eq!(a.editor.text(), "", "enter did not reach the composer");
}

/// A picture that has not arrived is not one to open, and Enter on a message with nothing in it says
/// so rather than doing nothing.
#[test]
fn enter_on_a_picture_that_has_not_arrived_says_what_it_can_do() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 1, 0)]);
    reply(&mut a, history("host", vec![picture(1, "abc123")]));
    a.take_actions();
    a.key(Key::Tab);
    a.key(Key::Enter);
    assert!(
        a.diff_popup.is_none(),
        "nothing was fetched, so nothing opened"
    );
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("nothing to open")),
        "{:?}",
        a.notice
    );
}

// ─────────────────────────── the thread ───────────────────────────

/// `t` shows the thread of the message under the cursor: it asks the coordinator for the thread and
/// keeps the root, so the replies are what the view draws next.
#[test]
fn t_shows_the_thread_of_the_selected_message() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 3, 0)]);
    let mut reply3 = msg(3, "host", "s-b", "chat", "an answer");
    reply3["thread"] = json!(1);
    reply(
        &mut a,
        history(
            "host",
            vec![msg(1, "host", "s-a", "chat", "the question"), reply3],
        ),
    );
    a.take_actions();

    a.key(Key::Tab);
    assert_eq!(a.rooms_cursor(), Some(3));
    a.key(Key::Char('t'));
    let asks = ops(&mut a);
    assert_eq!(asks.len(), 1, "one read: {asks:?}");
    assert_eq!(asks[0]["type"], "read");
    assert_eq!(asks[0]["thread"], 1, "a reply's thread is its root");
    assert_eq!(a.nav.thread, Some(1));
    assert_eq!(a.rooms_cursor(), Some(1), "the cursor rests on the root");
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("thread of #1")),
        "{:?}",
        a.notice
    );
}

/// `y` puts the message under the cursor on the terminal's clipboard, and says what it copied.
#[test]
fn y_copies_the_selected_message() {
    let mut a = opened(1_000);
    channels(&mut a, &[("host", 2, 1, 0)]);
    reply(
        &mut a,
        history(
            "host",
            vec![msg(1, "host", "s-a", "chat", "the deploy is green")],
        ),
    );
    a.key(Key::Tab);
    a.key(Key::Char('y'));
    assert_eq!(a.clipboard_out.as_deref(), Some("the deploy is green"));
    assert!(a.notice.as_deref().is_some_and(|n| n.contains("copied #1")));
}

// ─────────────────────────── the same cursor in a plugin pane ───────────────────────────

/// The cursor, the jumps and the search act on the entries of a STREAM. A plugin answers one - three
/// entries and a divider, of which two carry an id - and the arrows walk its entries, not the pane's
/// rows: the same rule `/rooms` keeps, with no line of the pane knowing what a room is.
#[test]
fn the_cursor_moves_by_stream_entry_in_a_plugin_pane() {
    use letibot_sessionlog::pane as pv;

    let mut a = app();
    let entries: Vec<pv::StreamEntry> = vec![
        pv::StreamEntry {
            id: Some("e1".into()),
            body: Some("the first thing said".into()),
            ..Default::default()
        },
        // A divider is drawn and is not a stop: nothing can be done with it.
        pv::StreamEntry {
            divider: Some(pv::Divider::Unread),
            text: "3 new".into(),
            ..Default::default()
        },
        pv::StreamEntry {
            id: Some("e2".into()),
            body: Some("the second".into()),
            ..Default::default()
        },
        pv::StreamEntry {
            id: Some("e3".into()),
            body: Some("the third".into()),
            ..Default::default()
        },
    ];
    let view = pv::View {
        title: "chat".into(),
        panels: vec![pv::Panel {
            id: "chat".into(),
            title: "the room".into(),
            summary: vec![pv::Block::Stream(pv::Stream {
                entries,
                ..Default::default()
            })],
            ..Default::default()
        }],
        ..Default::default()
    };
    a.apply(letibot_sessionlog::protocol::ServerFrame::Event(
        letibot_sessionlog::event::Envelope {
            session_id: "s".into(),
            seq: 1,
            ts: 0,
            event: letibot_sessionlog::event::SessionEvent::PluginPane {
                pane: "chat".into(),
                view: serde_json::to_value(view).unwrap(),
            },
        },
    ));
    a.open_plugin("chat");
    assert_eq!(a.focused_surface(), Some(Surface::PluginPane));
    assert_eq!(
        a.plugin_stops().len(),
        3,
        "three entries with an id, and no divider"
    );

    // The cursor starts on the first stop, and each arrow is one ENTRY: the divider between the
    // first and the second is drawn and is never a step.
    assert_eq!(selected_id(&a), "e1");
    a.key(Key::Down);
    assert_eq!(selected_id(&a), "e2", "by entry, over the divider");
    a.key(Key::Down);
    assert_eq!(selected_id(&a), "e3");
    a.key(Key::Down);
    assert_eq!(selected_id(&a), "e1", "wrapping, as every list here does");
    a.key(Key::Up);
    assert_eq!(selected_id(&a), "e3");

    // And the entries are drawn, one row each, so the cursor rests on something a reader sees.
    let rows = a.screen(80, 20).join("\n");
    for said in ["the first thing said", "the second", "the third"] {
        assert!(rows.contains(said), "{said} is not drawn:\n{rows}");
    }
}

/// The id of the stop under a plugin pane's cursor.
fn selected_id(a: &App) -> String {
    a.plugin_selected()
        .map(|t| t.id().to_string())
        .unwrap_or_default()
}
