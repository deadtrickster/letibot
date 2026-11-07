//! The seam P2 fills: a message drawn as a block. P0 has one plain line per message and the context
//! a block is drawn against, so what is proved here is that the seam carries a message and its width
//! through - and that a room with no pictures, no cursor and no search is drawn with no divider and
//! no highlight, which is the state P2 starts from.

use super::*;
use crate::app::rooms::{Attach, Msg};
use crate::ui::chat::{ChatCtx, chat_blocks, speaker_role};
use crate::ui::render::row_strings;
use serde_json::json;
use std::collections::BTreeMap;

fn msg(seq: u64, author: &str, text: &str) -> Msg {
    Msg {
        seq,
        ts: 0,
        author: author.into(),
        kind: "session".into(),
        to: None,
        thread: None,
        refs: Vec::new(),
        attachments: Vec::new(),
        text: text.into(),
    }
}

/// One message in, one block out, cut to the column it is drawn in - and the context's picture
/// lookup answering `None`, which is the one-row fallback P2 draws before P4's cache exists.
#[test]
fn a_message_is_one_block_cut_to_the_column() {
    let a = app();
    let names: BTreeMap<String, String> = [("s-a".to_string(), "gatekeeper".to_string())].into();
    let no_picture = |_: &str| -> Option<(u32, u32, u32)> { None };
    let ctx = ChatCtx {
        names: &names,
        me: "operator",
        width: 20,
        cfg: &a.cfg,
        picture: &no_picture,
        divider_after: None,
        selected: None,
        highlight: None,
    };
    let one = msg(14, "s-a", "merged `room: blob store`, gate green on lab2x1");
    let blocks = chat_blocks(&[&one], &ctx);
    assert_eq!(blocks.len(), 1, "one message, one block");
    assert_eq!(
        blocks[0].seq, 14,
        "the block is keyed by the seq that survives a poll"
    );
    assert_eq!(
        blocks[0].lines.len(),
        1,
        "P0 draws one row; P2 draws the block"
    );
    let drawn = row_strings(&blocks[0].lines, a.cfg.palette()).join("\n");
    assert!(drawn.contains("14"), "the seq is on the row: {drawn}");
    assert!(
        drawn.contains("gatekeeper"),
        "the roster's name, not the handle: {drawn}"
    );
    assert!(
        visible_width(&drawn) <= 20,
        "cut to the column it is drawn in: {drawn:?}"
    );
    // The picture lookup answers nothing yet, which is what draws the fallback row.
    assert_eq!((ctx.picture)("a".repeat(64).as_str()), None);
    // One colour for every handle until P2 hashes it, so a test that pins a palette pins it then.
    assert_eq!(speaker_role("s-a"), speaker_role("cc@letibot"));
}

/// The attachments a coordinator sends parse into the message, and a row written before they
/// existed still parses: the field is defaulted, so an old answer is a message with no pictures.
#[test]
fn a_message_with_attachments_parses_and_an_old_one_still_does() {
    let old: Msg = serde_json::from_value(json!({"seq": 1, "author": "s-a", "text": "plain"}))
        .expect("a row from before attachments existed");
    assert!(old.attachments.is_empty());
    let new: Msg = serde_json::from_value(json!({
        "seq": 2, "author": "s-a", "text": "![shot](blob:abc)",
        "attachments": [{"id": "abc", "mime": "image/png", "name": "shot.png", "bytes": 4096,
                         "w": 1280, "h": 720}],
    }))
    .expect("a row that carries a picture");
    assert_eq!(
        new.attachments,
        vec![Attach {
            id: "abc".into(),
            mime: "image/png".into(),
            name: "shot.png".into(),
            bytes: 4096,
            w: Some(1280),
            h: Some(720),
        }]
    );
}
