//! A message as a BLOCK rather than a line: header, body, quote, pictures, dividers. It draws the
//! pane language's `stream` block and knows nothing about rooms or `Msg` - what a message means is
//! [`crate::app::chat_adapter`]'s to say, which is the half a plugin replaces.
//!
//! Two vocabularies meet here: the pane language's `Role` names what a cell MEANS and `Paint` is
//! what the head draws it with. The renderer never takes an RGB from a layer.

mod block;
mod body;
pub(crate) mod colour;
mod divider;
mod mention;

use std::collections::BTreeMap;

use letibot_sessionlog::pane::{Mention, Stream, StreamEntry};
use rano::render::Line;

pub(crate) use crate::ui::render::md_options;
pub(crate) use colour::hue;

/// The columns a message's time and the gutter under it take: `14:02` and two spaces.
pub(crate) const GUTTER: usize = 7;

/// What a layer called one blob its window draws, whether it is a picture, and the size the wire
/// carried for it: a picture is drawn where the terminal can hold one, and a file is only named.
pub(crate) struct Blob {
    pub name: String,
    pub picture: bool,
    /// The pixels the store measured from the image's own header, when it could. `None` is *nobody
    /// measured it*, which is not 0x0.
    pub size: Option<(u32, u32)>,
}

/// A picture as the head's cache holds it: the image id it uploaded, and the pixels of it.
pub(crate) type Picture = (u32, u32, u32);

/// The picture cache, as the renderer asks it: blob id in, the picture when the bytes are here.
pub(crate) type Pictures<'a> = &'a dyn Fn(&str) -> Option<Picture>;

/// What one stream is drawn against: the column, how this head looks, which pictures it already
/// holds, which entry the reader stands on, and what they are looking for.
pub(crate) struct ChatCtx<'a> {
    pub width: usize,
    pub cfg: &'a crate::ui::render::RenderConfig,
    /// blob id -> (image id, pixel width, pixel height), from the head's picture cache. `None` is
    /// *the bytes are not here yet*, and draws the one-row fallback rather than a hole.
    pub picture: Pictures<'a>,
    /// The name each blob was sent under, so a picture row can say `shot.png`.
    pub blobs: &'a BTreeMap<String, Blob>,
    /// The entry id under the message cursor.
    pub selected: Option<&'a str>,
    /// The search needle, for its hits to be picked out.
    pub highlight: Option<&'a str>,
}

/// One entry's drawn rows, keyed by the entry's id - which for a room's stream is its `seq`, and is
/// whatever the layer said for any other.
pub(crate) struct Block {
    pub id: String,
    pub lines: Vec<Line>,
}

impl Block {
    /// The seq this entry is, when its id is one. A layer's ids are opaque; the room's are seqs,
    /// and that is what the pane's place-keeping orders by.
    pub(crate) fn seq(&self) -> Option<u64> {
        self.id.parse().ok()
    }
}

/// Every entry of a stream as its rows, in the order the layer listed them.
pub(crate) fn chat_blocks(s: &Stream, ctx: &ChatCtx<'_>) -> Vec<Block> {
    let cols = ctx.width.max(8);
    s.entries
        .iter()
        .map(|e| {
            let d = Draw {
                e,
                mentions: &s.mentions,
                ctx,
                cols,
            };
            Block {
                id: e.id().to_string(),
                lines: if e.is_divider() {
                    divider::lines(&d)
                } else {
                    block::lines(&d)
                },
            }
        })
        .collect()
}

/// One entry drawn against: the stream's tokens, the pane's context, and the columns it has.
pub(crate) struct Draw<'a> {
    pub e: &'a StreamEntry,
    pub mentions: &'a BTreeMap<String, Mention>,
    pub ctx: &'a ChatCtx<'a>,
    pub cols: usize,
}

impl Draw<'_> {
    /// The gutter a message is drawn under. A column too narrow to hold the time and a word of the
    /// body together has no gutter: the words matter more than the column they line up in.
    pub(crate) fn gutter(&self) -> usize {
        if self.cols >= GUTTER + 8 { GUTTER } else { 0 }
    }

    /// The columns a body has, after the gutter.
    pub(crate) fn body_cols(&self) -> usize {
        self.cols.saturating_sub(self.gutter()).max(8)
    }
}
