//! The pictures a message carries: fetching a blob once and keeping it, drawing one at the row a
//! message says, and the chip a pasted file becomes on its way out as a `put_blob`.
//!
//! The bytes never come from a layer and never leave this head: a stream entry names a blob id
//! (`StreamEntry::pictures`), the head holds or fetches the bytes, and the terminal is handed an
//! image id. That is why a plugin's stream can draw pictures without a plugin ever seeing a byte.
//!
//! The cache is bounded - [`PICTURES`] pictures or [`PICTURE_BYTES`] - and drops the one drawn
//! longest ago, because a chat scrolled through a morning is more pictures than a terminal wants.

#![allow(dead_code)] // The upload and the chip lookup are P3's readers; the drawing is P2's.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use crate::app::rooms::Attach;
use crate::app::*;

/// How many pictures the cache holds at once.
pub(crate) const PICTURES: usize = 32;
/// How many bytes of pictures the cache holds at once.
pub(crate) const PICTURE_BYTES: usize = 64 * 1024 * 1024;
/// The bytes one `put_blob` or `get_blob` part carries, which is the coordinator's own `MAX_PART`:
/// a part plus its base64 and JSON stays under the frame the room server accepts.
pub(crate) const PART: usize = 512 * 1024;
/// The one type the terminal is handed. A jpeg or a webp a message carries is drawn as the
/// one-row fallback naming it, because the kitty protocol carries a PNG here and converting one
/// would be an image decoder this head does not have.
pub(crate) const DRAWABLE: &str = "image/png";

/// One picture the head holds: the terminal's image id, the pixels the coordinator read out of the
/// header, the bytes the overlay opens whole, and the clock of the frame that last drew it.
struct Held {
    id: u32,
    px: (u32, u32),
    bytes: Vec<u8>,
    used: Cell<u64>,
}

/// An upload in flight: the parts queued for it, and what the chip names.
struct Upload {
    name: String,
    /// The chip text as the composer holds it - what `rooms_outgoing` finds and swaps.
    chip: String,
    /// The blob id the coordinator answered with, once it has.
    blob: Option<String>,
}

/// An answer half in: the bytes of the parts so far, and the size the coordinator read out of the
/// header (which only the first part carries).
#[derive(Default)]
struct Half {
    bytes: Vec<u8>,
    px: (Option<u32>, Option<u32>),
}

/// What P4 keeps between two frames: the pictures held, the blobs asked for, the parts of an
/// answer half in, and the uploads on their way out.
#[derive(Default)]
pub(crate) struct Media {
    held: BTreeMap<String, Held>,
    /// Blob ids asked for and not answered: one ask per id, since a poll draws the same picture
    /// every two seconds.
    asked: BTreeSet<String>,
    /// blob id -> the parts of its answer so far.
    parts: BTreeMap<String, Half>,
    /// The clock the cache is ordered by, on the head's own frame count.
    tick: Cell<u64>,
    /// The uploads this head made, oldest first, and the blob each became.
    uploads: Vec<Upload>,
}

impl Media {
    fn now(&self) -> u64 {
        let t = self.tick.get() + 1;
        self.tick.set(t);
        t
    }

    /// Drop the pictures drawn longest ago until the cache is inside both caps. The terminal is
    /// told too: an id it still holds is memory nothing will free once the rows that named it are
    /// gone.
    fn evict(
        &mut self,
        uploads: &mut Vec<Vec<u8>>,
        sent: &mut std::collections::HashMap<u32, (Option<u32>, Option<u32>)>,
    ) {
        loop {
            let over = self.held.len() > PICTURES
                || self.held.values().map(|h| h.bytes.len()).sum::<usize>() > PICTURE_BYTES;
            if !over {
                return;
            }
            let Some(oldest) = self
                .held
                .iter()
                .min_by_key(|(_, h)| h.used.get())
                .map(|(id, h)| (id.clone(), h.id))
            else {
                return;
            };
            self.held.remove(&oldest.0);
            sent.remove(&oldest.1);
            uploads.push(rano::term::graphics::image_delete(oldest.1));
        }
    }
}

impl App {
    /// The image a blob is already drawn from: `(image id, width, height)`. `None` is *not fetched
    /// yet*, which draws the one-row fallback rather than a hole.
    pub(crate) fn room_picture(&self, blob: &str) -> Option<(u32, u32, u32)> {
        let held = self.media.held.get(blob)?;
        // Drawing is what the cache is ordered by: a picture on the screen now is not the one to
        // drop when the next arrives.
        held.used.set(self.media.now());
        Some((held.id, held.px.0, held.px.1))
    }

    /// Ask for every blob of these a message draws and the cache has not seen.
    pub(crate) fn room_pictures_wanted(&mut self, blobs: &[&Attach]) {
        for a in blobs {
            if a.mime != DRAWABLE || self.media.held.contains_key(&a.id) {
                continue;
            }
            if !self.media.asked.insert(a.id.clone()) {
                continue;
            }
            self.queued.push(Action::RoomChat {
                op: serde_json::json!({"type": "get_blob", "id": a.id, "offset": 0}),
                id: Some(format!("{BLOB}{}", a.id)),
            });
        }
    }

    /// A pasted or dropped picture: the chip that goes in the composer, and the `put_blob` parts it
    /// is uploaded as. An upload goes out at once rather than at send, so the id a `say` names is
    /// usually already answered when the operator presses Enter.
    pub(crate) fn room_upload(&mut self, png: Vec<u8>, name: &str) -> String {
        let nth = self.media.uploads.len() + 1;
        let chip = format!(
            "[image {nth} · png · {}]",
            crate::app::attachments::size_said(png.len() as u64)
        );
        let upload = format!("upload-{}-{nth}", self.now_ms);
        let parts = png.len().div_ceil(PART).max(1);
        for part in 0..parts {
            let from = part * PART;
            let to = (from + PART).min(png.len());
            let data = letibot_transcript::media::encode_base64(&png[from..to]);
            self.queued.push(Action::RoomChat {
                op: serde_json::json!({
                    "type": "put_blob",
                    "upload": upload,
                    "name": name,
                    "mime": DRAWABLE,
                    "part": part,
                    "last": part + 1 == parts,
                    "data": data,
                }),
                id: Some(format!("{PUT}{upload}")),
            });
        }
        self.media.uploads.push(Upload {
            name: name.to_string(),
            chip: chip.clone(),
            blob: None,
        });
        chip
    }

    /// The chip text in a line swapped for the reference the message carries, and the blob ids the
    /// `say` names. A chip whose upload has not been answered yet keeps its words and names no
    /// blob: sending a reference to an id the coordinator does not hold is a refusal, and losing the
    /// words to it would be worse than the picture arriving late.
    pub(crate) fn room_outgoing_chips(&self, text: &str) -> (String, Vec<String>) {
        let mut out = text.to_string();
        let mut ids = Vec::new();
        for u in &self.media.uploads {
            let Some(blob) = &u.blob else { continue };
            if !out.contains(&u.chip) {
                continue;
            }
            out = out.replace(&u.chip, &format!("![{}](blob:{blob})", u.name));
            ids.push(blob.clone());
        }
        (out, ids)
    }

    /// The answer to a request this head made for a blob or an upload. `true` when the reply was
    /// one of those, so `/rooms` does not read it as a poll's.
    pub(crate) fn on_room_blob(&mut self, reply: &serde_json::Value) -> bool {
        match reply.get("type").and_then(serde_json::Value::as_str) {
            Some("blob_part") => {
                let att = reply.get("attachment").cloned().unwrap_or_default();
                let Some(id) = att.get("id").and_then(serde_json::Value::as_str) else {
                    return true;
                };
                let id = id.to_string();
                let data = reply
                    .get("data")
                    .and_then(serde_json::Value::as_str)
                    .map(letibot_transcript::media::decode_base64)
                    .unwrap_or_default();
                let last = reply.get("last").and_then(serde_json::Value::as_bool) == Some(true);
                let pixels = |k: &str| {
                    att.get(k)
                        .and_then(serde_json::Value::as_u64)
                        .map(|v| v as u32)
                };
                let px = (pixels("w"), pixels("h"));
                let half = self.media.parts.entry(id.clone()).or_default();
                half.bytes.extend_from_slice(&data);
                if px.0.is_some() {
                    half.px = px;
                }
                if !last {
                    // The next part, from where this one ended: a blob is up to 8 MiB and a part is
                    // half a megabyte, so the ask is repeated rather than asked for whole.
                    let offset = half.bytes.len();
                    self.queued.push(Action::RoomChat {
                        op: serde_json::json!({
                            "type": "get_blob",
                            "id": id,
                            "offset": offset,
                        }),
                        id: Some(format!("{BLOB}{id}")),
                    });
                    return true;
                }
                let half = self.media.parts.remove(&id).unwrap_or_default();
                self.media.asked.remove(&id);
                self.keep(&id, half.bytes, half.px);
                true
            }
            Some("blob") => {
                let Some(blob) = reply
                    .get("attachment")
                    .and_then(|a| a.get("id"))
                    .and_then(serde_json::Value::as_str)
                else {
                    return true;
                };
                // The upload still waiting on its answer is this one: a head uploads one chip at a
                // time and the parts of one upload answer in order.
                if let Some(u) = self.media.uploads.iter_mut().find(|u| u.blob.is_none()) {
                    u.blob = Some(blob.to_string());
                }
                true
            }
            _ => false,
        }
    }

    /// A blob's bytes are whole: into the cache, and onto the terminal under the image id the
    /// renderer draws its rows with.
    fn keep(&mut self, blob: &str, bytes: Vec<u8>, px: (Option<u32>, Option<u32>)) {
        if bytes.is_empty() {
            return;
        }
        let id = rano::term::graphics::image_id(&format!("blob:{blob}"));
        let px = (px.0.unwrap_or(0), px.1.unwrap_or(0));
        let mut uploads = Vec::new();
        if self
            .images_sent
            .insert(id, (Some(px.0), Some(px.1)))
            .is_none()
        {
            let box_cols = rano::term::graphics::image_box(self.cfg.width);
            let px_in = ((px.0 > 0).then_some(px.0), (px.1 > 0).then_some(px.1));
            let (cols, rows) = rano::term::graphics::image_cells(px_in.0, px_in.1, box_cols);
            let b64 = letibot_transcript::media::encode_base64(&bytes);
            uploads.push(
                self.proto
                    .wrap(&rano::term::graphics::image_upload(id, &b64)),
            );
            uploads.push(
                self.proto
                    .wrap(&rano::term::graphics::image_place(id, cols, rows)),
            );
        }
        self.media.held.insert(
            blob.to_string(),
            Held {
                id,
                px,
                bytes,
                used: Cell::new(self.media.now()),
            },
        );
        self.media.evict(&mut uploads, &mut self.images_sent);
        self.image_uploads.extend(uploads);
        // The rows of a picture are drawn from the cache, so the frame this filled is stale.
        self.invalidate_history();
    }

    /// The picture whole, in the overlay Enter and a click open: the same `PopupBody::Picture` a
    /// conversation's thumbnail opens, with the bytes this head fetched rather than a row's.
    /// `false` when the blob is not held - a picture still on its way is not one to open.
    pub(crate) fn room_open_picture(&mut self, blob: &str) -> bool {
        let Some((id, px, png)) = self.media.held.get(blob).map(|h| {
            (
                h.id,
                h.px,
                letibot_transcript::media::encode_base64(&h.bytes),
            )
        }) else {
            return false;
        };
        let bytes = self.media.held[blob].bytes.len();
        self.open_popup(crate::app::diff_popup::PopupBody::Picture {
            id,
            png,
            media_type: DRAWABLE.to_string(),
            px: ((px.0 > 0).then_some(px.0), (px.1 > 0).then_some(px.1)),
            bytes,
        });
        true
    }
}

/// Request id prefixes: an answer is told by the id it comes back with, since a poll's, a send's
/// and a picture's all arrive on the same frame.
pub(crate) const BLOB: &str = "blob:";
pub(crate) const PUT: &str = "put:";
