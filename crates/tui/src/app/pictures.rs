//! **The pictures an operator's own rows carry, and the one key that opens one whole.**
//!
//! The conversation draws each attached image as a thumbnail under the chip that names it
//! (`ui::transcript::user`), which is what `app::attention::queue_image_uploads` makes possible:
//! the bytes are on the row, the head sends them to the terminal once, and the renderer draws the
//! rows the terminal fills.
//!
//! What is here is the other half of that — **the picture a reader can point at**:
//!
//! * **which row is a picture's**, recorded by the draw rather than recomputed by the key. The
//!   thumbnail's height depends on the width and on the picture's own aspect, and a key handler
//!   that did the same arithmetic a second time would be a second opinion about the screen — the
//!   defect `App::todos_stop_rows` and every other `*_stop_rows` record exists to prevent;
//! * **what Enter and a click open**: the picture, larger, in the popup the change and the notice
//!   already open in (`app::diff_popup`) — one overlay with one set of keys, rather than a second
//!   screen with its own Esc.
//!
//! Why an overlay at all, when the thumbnail is already the picture: the thumbnail is a couple of
//! dozen rows of a 40-column box, and a screenshot at that size is not readable. Four fifths of the
//! frame is.

use super::*;
use letibot_transcript::{TranscriptItem, UserPart};

/// **One picture the conversation drew**, as the key and the click name it — and all of it read
/// from the row, so a click cannot name a picture that is not the one it was drawn for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PictureRef {
    /// The row that carries it — the only durable name a picture has.
    pub(crate) item_id: String,
    /// Which image part of that row, so two pictures in one prompt are two pictures.
    pub(crate) part: usize,
    /// The id the bytes were uploaded under (`ui::render::USER_PART` of the key).
    pub(crate) id: u32,
    /// Pixels, when the header gave them — what the popup's box keeps its aspect from.
    pub(crate) px: (Option<u32>, Option<u32>),
    /// The PNG's own size, for the popup's edge.
    pub(crate) bytes: usize,
}

impl App {
    /// **Tell the head what the terminal speaks about pictures** — one decision, taken beside
    /// `set_features` because it comes from the same environment.
    ///
    /// It is [`rano::term::Proto::of`] and not `detect`: the capability query is a write to the
    /// terminal and a bounded wait on its answer, which is a thing to do *before* a head commits to
    /// reading input and not something to do twice. Where the names say nothing, this head draws no
    /// picture and says nothing — the chip is the picture's name, which is what it draws today.
    pub fn set_proto(&mut self, proto: rano::term::Proto) {
        if self.proto == proto {
            return;
        }
        // **Whatever the old protocol held is deleted in the old protocol's terms** — an id the
        // terminal was holding is memory nothing else can free once the rows that named it are
        // gone.
        for bytes in self.pictures.set_proto(proto) {
            self.image_uploads.push(bytes);
        }
        self.proto = proto;
        self.invalidate_history();
        self.redraw = true;
    }

    /// **The pictures among the conversation rows on this window**, as `(screen row, picture)` —
    /// the sibling of `file_rows_in`, and read the same way.
    ///
    /// A thumbnail is the LAST `rows` lines of its row's span (the block, then the pictures, in
    /// part order), so a click on the operator's own words stays a click on nothing while a click
    /// on the picture opens it. The arithmetic is the renderer's own — `user_picture_box` of the
    /// width — because a box measured any other way is a box the frame did not draw.
    pub(crate) fn picture_rows_in(&self, start: usize, end: usize) -> Vec<(usize, PictureRef)> {
        let end = end.min(self.hist_lines.len());
        let mut out = Vec::new();
        for line in start..end {
            let i = self.spans.partition_point(|sp| sp.at <= line);
            let Some(sp) = i.checked_sub(1).map(|i| self.spans[i]) else {
                continue;
            };
            if line >= sp.at + sp.lines {
                continue;
            }
            let Some((it, item)) = self.items.get(sp.row).and_then(|it| {
                let item = it.item.as_ref()?;
                Some((it, item))
            }) else {
                continue;
            };
            let TranscriptItem::User {
                parts,
                speaker: letibot_transcript::Speaker::Operator,
            } = item
            else {
                continue;
            };
            // From the row's last line backwards, one picture at a time: the renderer appends
            // them in part order under the block, so the last picture is the row's last lines.
            let mut back = sp.at + sp.lines;
            for (k, p) in parts.iter().enumerate().rev() {
                if !matches!(p, UserPart::Image { .. }) {
                    continue;
                }
                let Some((id, _cols, rows, px)) =
                    crate::ui::render::user_picture_box(&it.item_id, k, self.cfg.width)
                else {
                    continue;
                };
                let at = back.saturating_sub(rows as usize);
                if line >= at && line < back {
                    out.push((
                        line - start,
                        PictureRef {
                            item_id: it.item_id.clone(),
                            part: k,
                            id,
                            px,
                            bytes: bytes_of(p),
                        },
                    ));
                    break;
                }
                back = at;
            }
        }
        out
    }

    /// **Open a picture whole** — the popup the change and the notice open in, with the picture cut
    /// to the frame and its bytes sent at that box (see `ui::diff_popup`'s `Picture` arm).
    pub(crate) fn open_picture(&mut self, r: &PictureRef) {
        let part = self
            .items
            .iter()
            .find(|it| it.item_id == r.item_id)
            .and_then(|it| it.item.as_ref())
            .and_then(|item| match item {
                TranscriptItem::User { parts, .. } => parts.get(r.part),
                _ => None,
            })
            .cloned();
        let Some(UserPart::Image {
            media_type,
            data_ref,
        }) = part
        else {
            return;
        };
        self.open_popup(crate::app::diff_popup::PopupBody::Picture {
            id: r.id,
            png: letibot_transcript::media::wire_payload(&data_ref).to_string(),
            media_type,
            px: r.px,
            bytes: r.bytes,
        });
        // **After the popup is open**, because opening one closes the last and closing gives back
        // what that view held — this picture's bytes are not the previous view's to free.
        if let Some((_, cols, rows, _)) =
            crate::ui::render::user_picture_box(&r.item_id, r.part, self.cfg.width)
        {
            self.pictures.adopt(r.id, (cols, rows));
        }
    }

    /// **The newest picture the last frame drew**, by the row it is on — `Enter`'s answer where a
    /// click has a position and a key has none.
    pub(crate) fn newest_picture_ref(&self) -> Option<PictureRef> {
        self.picture_rows
            .last()
            .and_then(|(_, k)| self.picture_refs.get(*k))
            .cloned()
    }
}

/// **A picture's size in bytes**, out of its `data:` reference — the base64's own arithmetic, and
/// no decode: four characters are three bytes, less the padding. The popup's edge says how big the
/// picture is, and a number that came from decoding a megabyte to count it would be a number this
/// head paid for.
fn bytes_of(part: &UserPart) -> usize {
    let UserPart::Image { data_ref, .. } = part else {
        return 0;
    };
    let b64 = letibot_transcript::media::wire_payload(data_ref);
    let pad = b64.ends_with('=') as usize + b64.ends_with("==") as usize;
    (b64.len() / 4) * 3 - pad.min(2)
}
