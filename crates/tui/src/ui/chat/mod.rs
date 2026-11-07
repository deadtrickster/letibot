//! A message as a BLOCK rather than a line: the header, the body under it, the quote of what it
//! answers, and the rows a picture takes. P0 defines the seam and draws today's plain line; P2
//! replaces [`chat_blocks`] with the block the design draws, and nothing else has to move.

#![allow(dead_code)] // The seam is P0's and its readers are P2's: every field here is drawn then.

use std::collections::BTreeMap;

use rano::agent::text::clean_line;
use rano::render::{Line, Span};
use rano::style::Role;

use crate::app::rooms::Msg;
use crate::ui::render::{RenderConfig, trim_to};

/// What one message is drawn against: who the handles are, how wide the column is, which row is
/// picked out, and what the reader has already seen.
pub(crate) struct ChatCtx<'a> {
    /// handle -> display name, from the roster.
    pub names: &'a BTreeMap<String, String>,
    /// The name the reader speaks as.
    pub me: &'a str,
    pub width: usize,
    pub cfg: &'a RenderConfig,
    /// blob id -> (image id, w, h), from P4's picture cache. `None` is the one-row fallback.
    pub picture: &'a dyn Fn(&str) -> Option<(u32, u32, u32)>,
    /// The `seq` the unread divider follows, when the reader opened the channel with news.
    pub divider_after: Option<u64>,
    /// The `seq` under the message cursor.
    pub selected: Option<u64>,
    /// The search needle, for its hits to be picked out.
    pub highlight: Option<&'a str>,
}

/// One message's drawn rows, keyed by the `seq` that survives a poll.
pub(crate) struct Block {
    pub seq: u64,
    pub lines: Vec<Line>,
}

/// One block per message. P0 draws the line `/rooms` draws today - `seq author: text` - because
/// the scroll math is already written against these rows; P2 turns each into the designed block.
pub(crate) fn chat_blocks(msgs: &[&Msg], ctx: &ChatCtx<'_>) -> Vec<Block> {
    let cols = ctx.width.max(8);
    msgs.iter()
        .map(|m| {
            let who = ctx.names.get(&m.author).unwrap_or(&m.author);
            let text = format!("{} {}: {}", m.seq, clean_line(who), clean_line(&m.text));
            Block {
                seq: m.seq,
                lines: vec![Line::new(vec![Span::raw(trim_to(&text, cols))])],
            }
        })
        .collect()
}

/// The colour a handle speaks in. P0 answers one role for every handle; P2 hashes the handle and
/// picks from the palette, so a rename keeps the colour.
pub(crate) fn speaker_role(_handle: &str) -> Role {
    Role::Strong
}
