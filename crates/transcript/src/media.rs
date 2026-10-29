//! **Bytes a tool read that are not text, kept as bytes** — the second structured channel on
//! `letibot_tools::runtime::Invocation`, beside `letibot_tools::edit::FileEdit`.
//!
//! # What this is for, in the operator's words
//!
//! *"lets just do it opencode way"*, *"read is read there is nothing to settle"*, *"reading an image
//! is no different to reading a rust file."* So this module adds no verb, no gate question and no
//! permission: `read` is asked for a path, and the only thing that differs is what comes back.
//!
//! # Why the field is here rather than in an event
//!
//! `FileEdit`'s docstring makes the argument and it transfers whole: **the row is the durable
//! artifact**. A head that attaches after a restart is handed rows, not events — and where the bytes
//! have to survive for a *second* reason, the same one: a prompt rebuilt on the next round renders
//! the transcript again, so an image that lived only in an event would be visible to the model once
//! and then gone, which is the worst of the possible failures because it looks like a model that
//! forgot rather than a head that dropped it.
//!
//! # Where it DIFFERS from `FileEdit`, and the difference is the design
//!
//! `FileEdit` is **display-only** — *"the prompt builders read `payload` and never this"* — and this
//! is the opposite: it exists to be SENT. That is why [`Media::data_ref`] is spelled the way a
//! renderer needs it rather than the way a head needs it, and why the mime is sniffed from the bytes
//! instead of taken on trust from the path.
//!
//! # The shape a renderer needs
//!
//! MEASURED, 2026-09-27, against the local `llama-server` (`qwen-3.8-27b`, `--mmproj`):
//!
//! * `data:image/png;base64,…` **works** — the model named a red square `Red` and a green one
//!   `Green`.
//! * a bare **path** does not: `HTTP 400 · Failed to load image or audio file`. The URL is passed
//!   verbatim to the server, which fetches it, and a path on this box is not something it can fetch.
//!   So `data_ref` MUST be a `data:` URI, and the renderers are right to stay pure `parts → JSON`.
//!
//! # What is deliberately NOT here
//!
//! **No size cap, and that is a measurement rather than an omission.** The local server resizes
//! above **4096 vision tokens (4.19 MP — 2048×2048)** and never refuses for pixels; it also accepted
//! a 16.8 MB request body. A cap here would be a head discarding what the server would have taken,
//! which is the failure the operator named: *"a head that ignores it will attach images the server
//! discards, which would then look exactly like a model ignoring a picture it received."* The
//! numbers live in `docs/leticode.md` so a future change argues with them rather than with a guess.

use serde::{Deserialize, Serialize};

/// **The size a `data:` reference states, read out of the header only** — so a head can cut a
/// picture's cell box without decoding the pixels.
///
/// A tool result carries a [`Media`], which already has `width`/`height`; a **user row's image part
/// carries the reference and nothing else** ([`crate::UserPart::Image`]), and the head that draws it
/// needs the aspect the box keeps. So this decodes a bounded prefix of the base64 — 4 KiB, which is
/// more than enough for every header walked below — and asks [`dimensions`], the same function
/// [`Media::of`] uses. One walk over one format table, and a format whose size this module does not
/// parse answers `(None, None)` here exactly as it does there.
///
/// The mime comes from the reference's own `data:<mime>;base64,` prefix and not from a file name:
/// there is no name here, and a name is the weaker claim anyway.
pub fn size_of_data_ref(data_ref: &str) -> (Option<u32>, Option<u32>) {
    let Some((head, _)) = data_ref.split_once(";base64,") else {
        return (None, None);
    };
    let mime = head.trim_start_matches("data:");
    let payload = wire_payload(data_ref);
    // 4 KiB of bytes needs 5462 base64 characters, and the padding never crosses the walk.
    let take = payload.len().min(5 * 1024);
    let bytes = decode_base64(&payload[..take]);
    dimensions(mime, &bytes)
}

/// **Standard base64, decoded** — [`encode_base64`] backwards, and the alphabet is the same
/// constant read the other way.
///
/// **Tolerant on purpose**: padding, a stray character, a payload cut mid-quantum. It is used to
/// read a header out of bytes a head did not build (a pasted picture, a row written by another
/// version), and refusing there would turn *an image of an unknown shape* into *no image at all*.
/// What it does not do is guess: a character outside the alphabet ends the decode, so a corrupted
/// payload yields a short buffer and `(None, None)` rather than wrong pixels.
pub fn decode_base64(text: &str) -> Vec<u8> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in text.bytes() {
        let Some(v) = A.iter().position(|a| *a == c) else {
            if c == b'=' {
                break;
            }
            // Whitespace is padding a line-wrapped encoder leaves; anything else is corruption.
            if c.is_ascii_whitespace() {
                continue;
            }
            break;
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// **The base64 alphabet, spelled once for the whole tree.**
///
/// It is here — in the crate both the tool layer and the log can see — because two callers need it
/// for two different reasons: `media` encodes an image for a `data:` URI, and `letibot_tools`'
/// `firecode` backend encodes a file to fetch it over the wire. **A base64 alphabet is exactly the
/// kind of fact that gets spelled right in one place and subtly wrong in another** — which is this
/// tree's whole ledger of defects for one afternoon — so the second copy is gone and the tool layer
/// calls this one.
pub fn encode_base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// **The image types this tree will hand a model**, matching the server's own supported set.
///
/// opencode's `SUPPORTED_IMAGE_MIMES` is the same four, and the model server's chat template
/// accepts `image`/`image_url` parts — so this is not a policy list, it is what the far end can
/// read. A type outside it is not refused as a security matter; it is simply not an image, and
/// `read` shows it the way it shows any other bytes.
pub const SUPPORTED_IMAGE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// **Bytes a call produced that are not text**, with the metadata a row needs to describe them.
///
/// The fields are R54 §5's, which argues each one: `media_type` alone answers the wrong question (a
/// reader wants to know *how big* and *what shape*), and `bytes` is the size of the underlying file
/// rather than of the base64 — a number a person can compare with `ls`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    /// `image/png`. Sniffed from the bytes; see [`sniff_mime`].
    pub mime: String,
    /// The file's own size in bytes, NOT the size of [`Media::data_ref`] — base64 is 4/3 of it and
    /// the difference is an encoding detail rather than a fact about the picture.
    pub bytes: usize,
    /// Pixels, when the header carries them. **`None` is not `0`** — a format whose header this
    /// module does not parse has an unknown size, and R54 §5 makes the same argument for the row:
    /// *absent is not 0x0*.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `data:image/png;base64,…` — the form the model server takes, verbatim.
    pub data_ref: String,
    /// **Whether a model was actually given these bytes.**
    ///
    /// leticl's argument, and it is the reason the field exists rather than the count being left to
    /// the reader: *"an attachment that the daemon dropped and one the model ignored look identical
    /// on a transcript that does not say which, and only one of them is the operator's mistake to
    /// correct."* The payload's own sentence tells the MODEL the image is attached; nothing told the
    /// READER whether it arrived.
    ///
    /// `false` is the honest default and it is a fact rather than a guess: **no row that predates
    /// this field can carry a `Media` at all**, because `UserPart::Image` had no producer until
    /// 2026-09-27 — the tree could carry a picture in its types and could not make one. So for every
    /// row written before it, `false` is true.
    ///
    /// Set by whoever sends it, which is the engine and only the engine: it builds the request, so
    /// it is the only place that knows whether the bytes went out. Two ways they do not — the
    /// endpoint has no media marker (a metered provider, or a server without `mtmd`), or the
    /// renderer's markers and the attachments disagreed and the engine refused to send pictures it
    /// could not place.
    #[serde(default)]
    pub delivered: bool,
}

impl Media {
    /// **A picture this call read**, or `None` when the bytes are not one of the supported images.
    ///
    /// The mime is taken from the **bytes** first and the path only as a fallback, which is
    /// opencode's order (`read.ts:304`, `sniffAttachmentMime(sample, FSUtil.mimeType(filepath))`) and
    /// is the right way round for a reason this tree has already been bitten by: a file's name is a
    /// claim and its magic number is not. A `.png` that is really a PDF, or a screenshot saved with
    /// no extension at all, both arrive here.
    ///
    /// **A signature is not a picture.** The bytes must also be a COMPLETE image of the type they
    /// claim — see [`complete_image`], which is the second half of this function and the reason it
    /// can answer `None` for a file that sniffs perfectly. MEASURED 2026-10-30: a 33-byte PNG
    /// header and nothing else was called an image here, stored as one, and sent to a provider that
    /// refused the whole request on every turn for ever, because the row is append-only.
    pub fn of(path: &str, bytes: &[u8]) -> Option<Media> {
        let mime = sniff_mime(path, bytes)?;
        if !complete_image(mime, bytes) {
            return None;
        }
        let (width, height) = dimensions(mime, bytes);
        Some(Media {
            mime: mime.to_string(),
            bytes: bytes.len(),
            width,
            height,
            data_ref: format!("data:{mime};base64,{}", encode_base64(bytes)),
            // Not delivered yet: nobody has sent it. See the field.
            delivered: false,
        })
    }

    /// **The base64 a multimodal request carries** — the payload WITHOUT the `data:` prefix.
    ///
    /// `data_ref` is `data:image/png;base64,AAA…` because that is what the OpenAI-compatible image
    /// URL takes. llama.cpp's `multimodal_data` is the opposite: `base64_decode(entry)` on each
    /// element (`server-common.cpp:1015`), so the prefix would be decoded as image bytes and the
    /// file would fail to load. Two shapes for one fact, and the prefix is the whole difference —
    /// which is exactly the kind of thing that fails as `Failed to load image or audio file` with
    /// nothing pointing at the colon.
    ///
    /// Returns the whole string when there is no prefix: a `data_ref` that is already bare is
    /// carried through rather than mangled.
    pub fn wire_base64(&self) -> &str {
        wire_payload(&self.data_ref)
    }

    /// **What this image costs the prompt, in vision tokens** — or `None` when the header did not
    /// give a size.
    ///
    /// MEASURED against the local `llama-server`, 2026-09-27: **one token per 1024 pixels**, on a
    /// 32×32 patch grid, capped at **4096**. 2048×2048 (4.19 MP) is the last size that is not
    /// downscaled — 2048², 2304² and 4096² all cost exactly 4151 prompt tokens, which is 4096 plus
    /// the framing.
    ///
    /// **Why this lives on the type rather than in the caller.** The number is needed by exactly one
    /// thing today — the token accounting for a row that carries media — and that accounting is
    /// currently WRONG: `plan_tail` sums the ledger's per-row token counts, and the ledger renders
    /// the *payload*, which is a twenty-token sentence about the picture. So a compacted tail
    /// "planned to fit the budget" can be two orders of magnitude over it. See
    /// `docs/compaction.md` §8, which files the fix and this arithmetic with it.
    ///
    /// `None` for an unparsed header rather than a guess: R54 §5's rule — *absent is not 0x0* — is
    /// the same rule here, and a zero would count a picture as free.
    pub fn vision_tokens(&self) -> Option<u64> {
        let (w, h) = (self.width?, self.height?);
        Some((u64::from(w) * u64::from(h) / 1024).min(4096))
    }

    /// The one line a head draws in place of the bytes.
    ///
    /// Text, carrying no data at all — opencode's second channel: *"output: 'Image read
    /// successfully'"* while the bytes ride beside it. A payload that inlined the base64 would put a
    /// megabyte of it through every renderer, every `grep` over the log, and every scrollback.
    pub fn summary(&self) -> String {
        match (self.width, self.height) {
            (Some(w), Some(h)) => {
                format!("image {} {w}×{h} · {} KiB", self.mime, self.bytes / 1024)
            }
            _ => format!("image {} · {} KiB", self.mime, self.bytes / 1024),
        }
    }
}

/// **The image type of these bytes**, by magic number, with the path as a fallback.
///
/// The order is opencode's and the comment above [`Media::of`] gives the reason. `None` means *not
/// an image this tree will hand a model* — which includes a PDF, deliberately: PDFs ride the same
/// attachment path in opencode, and this tree has no PDF path to put one on yet, so claiming one
/// here would be a mime it cannot honour.
pub fn sniff_mime(path: &str, bytes: &[u8]) -> Option<&'static str> {
    // **Magic first.** Every one of these is a fixed prefix, and none is ambiguous with another.
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    // RIFF….WEBP — the four bytes after `RIFF` are a little-endian length, so the tag is at 8.
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    // **Then the name**, which is where opencode puts it too: a file whose *extension* says image and
    // whose header this function does not know is still worth trying, because the far end is the
    // authority on what it can read and a refusal from it names the file.
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// **Is this a COMPLETE image of the type it claims?** — judged structurally, per format, without
/// decoding a pixel.
///
/// The rule this exists for, MEASURED 2026-10-30: a `read` of a 33-byte file that was a PNG
/// signature and an `IHDR` and nothing else was called an image, stored as one, and sent to a
/// provider that refused the WHOLE request —
/// `.messages[1861].image[0]: You have uploaded an unsupported image. Please make sure your image is
/// valid and has one of the following formats: webp, png, jpeg, and gif.` — on every turn, for ever.
/// The session could not be revived by any restart, because the row is history and history does not
/// change. A magic number says what a file is *trying* to be; this says whether there is a picture
/// in it.
///
/// # What each format has to have, and why nothing is decoded
///
/// * **PNG** — an `IHDR` first, at least one `IDAT`, and an `IEND` that ends the chunk walk. The
///   walk is the whole check: every chunk's length must fit in the file, so a file cut off
///   mid-chunk is not an image either. Bytes after `IEND` are ignored, which is what a decoder does.
/// * **JPEG** — `SOI` at the front and `EOI` at the end. A JPEG's markers are not length-prefixed
///   the way PNG's chunks are, and a walk to `EOI` is what a decoder does anyway.
/// * **GIF** — its header and its trailer (`0x3b`), which is where every decoder stops.
/// * **WEBP** — a `RIFF`/`WEBP` container holding a `VP8 `/`VP8L`/`VP8X` chunk that fits in the
///   file. `VP8X` alone is a header for the extended form; the same walk finds the image chunk
///   beside it, and a file with neither is not a picture.
///
/// **Nothing here decodes**, so it cannot say whether the pixels are valid — only that the file is
/// not truncated and not a header pretending to be a picture. That is the line the incident drew:
/// the provider's complaint was about a file with no image in it at all, which is exactly what a
/// structural check can refuse, and doing it here costs a walk rather than a decoder.
///
/// A mime outside the four is not an image this tree hands a model ([`SUPPORTED_IMAGE_MIMES`]), so
/// it answers `false` rather than guessing.
pub fn complete_image(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => png_complete(bytes),
        "image/jpeg" => {
            bytes.len() >= 4 && bytes.starts_with(b"\xff\xd8") && bytes.ends_with(b"\xff\xd9")
        }
        "image/gif" => {
            (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"))
                && bytes.last() == Some(&0x3b)
        }
        "image/webp" => webp_complete(bytes),
        _ => false,
    }
}

/// **The same predicate for a picture a row already carries** — a `data:` reference, decoded just
/// far enough to ask [`complete_image`].
///
/// This is the half that makes a fixed binary able to reopen a session a broken one wedged: the
/// transcript is append-only, so the picture that a provider refused is still in the history, and
/// the renderer has to be able to tell that it is not one. [`decode_base64`] is tolerant, so a
/// reference that is itself corrupt decodes short and answers `false` — which is the right answer
/// for bytes nobody can read.
///
/// The whole payload is decoded, because two of the four formats state their completeness at the
/// END. That is O(the picture) per render, the same order as the request this is building.
pub fn data_ref_is_image(data_ref: &str) -> bool {
    let Some((head, payload)) = data_ref.split_once(";base64,") else {
        return false;
    };
    let mime = head.trim_start_matches("data:");
    complete_image(mime, &decode_base64(payload))
}

/// A PNG's chunk walk: `IHDR` first, an `IDAT` somewhere, and an `IEND` that fits.
fn png_complete(bytes: &[u8]) -> bool {
    const SIG: usize = 8;
    if bytes.len() < SIG + 12 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return false;
    }
    let mut at = SIG;
    let mut idat = false;
    let mut first = true;
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
        let kind = &bytes[at + 4..at + 8];
        // Length, type, data and CRC all have to be IN the file: a chunk that runs past the end is
        // a truncated picture, which is the case this whole function is about.
        let Some(end) = at.checked_add(12).and_then(|e| e.checked_add(len)) else {
            return false;
        };
        if end > bytes.len() {
            return false;
        }
        if first {
            if kind != b"IHDR" {
                return false;
            }
            first = false;
        }
        match kind {
            b"IDAT" => idat = true,
            b"IEND" => return idat,
            _ => {}
        }
        at = end;
    }
    false
}

/// A WEBP's chunk walk: `RIFF`/`WEBP`, then the first `VP8 `/`VP8L`/`VP8X` chunk that fits.
fn webp_complete(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || !bytes.starts_with(b"RIFF") || &bytes[8..12] != b"WEBP" {
        return false;
    }
    let mut at = 12usize;
    while at + 8 <= bytes.len() {
        let kind = &bytes[at..at + 4];
        let len = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
            as usize;
        let Some(end) = at.checked_add(8).and_then(|e| e.checked_add(len)) else {
            return false;
        };
        if end > bytes.len() {
            return false;
        }
        if kind == b"VP8 " || kind == b"VP8L" || kind == b"VP8X" {
            return true;
        }
        // RIFF pads every chunk to an even length.
        at = end + (len & 1);
    }
    false
}

/// Width and height from the header, for the two formats that carry them in a fixed place.
///
/// **`None` for a format this does not parse**, and that is the honest answer rather than a zero:
/// R54 §5's argument about the row — *absent is not 0x0* — is the same argument here. GIF and WebP
/// are omitted rather than guessed; adding one is a header walk with its own test, not a change to
/// this function's shape.
fn dimensions(mime: &str, bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    match mime {
        // IHDR is the first chunk: 8 bytes of signature, 4 of length, 4 of type, then w and h.
        "image/png" if bytes.len() >= 24 => {
            let be = |at: usize| {
                u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
            };
            (Some(be(16)), Some(be(20)))
        }
        // A JPEG is a walk: SOI, then a chain of markers, and the frame header (SOF0…SOF3, SOF5…SOF7,
        // SOF9…SOF11) is the first marker that carries the size. Everything before it is metadata of
        // some length, and each segment states its own.
        "image/jpeg" => jpeg_size(bytes),
        _ => (None, None),
    }
}

fn jpeg_size(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    let mut i = 2usize; // past SOI
    // **`i + 4 <= len` to read a marker and its length**, and `i + 9 <= len` to read a frame
    // header's last byte at `i + 8` — the off-by-one that a fixture ending exactly at the width
    // caught, which is the whole reason the test builds one.
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xff {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        // Standalone markers carry no length.
        if (0xd0..=0xd9).contains(&marker) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        // SOF0-SOF15, minus the three that are not frame headers (DHT=C4, JPG=C8, DAC=CC).
        let is_sof = (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc);
        if is_sof && len >= 7 && i + 9 <= bytes.len() {
            let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
            return (Some(w), Some(h));
        }
        // A zero-length segment would spin for ever; the spec forbids one.
        i += 2 + len.max(2);
    }
    (None, None)
}

/// **The `data:` prefix off a reference**, or the reference unchanged when it has none.
///
/// One rule, two carriers: [`Media::wire_base64`] is this applied to a `Media`, and a user row's
/// image part holds its own `data_ref` — so a second spelling of *strip the prefix* is a second
/// chance to get it wrong in the one failure mode that has no symptom except
/// `Failed to load image or audio file`.
pub fn wire_payload(data_ref: &str) -> &str {
    data_ref
        .split_once(";base64,")
        .map(|(_, b)| b)
        .unwrap_or(data_ref)
}

/// **Every image in a transcript, in the order a prompt places them, ready for the wire.**
///
/// The other half of a multimodal request: a server substitutes the pictures in `multimodal_data` at
/// the markers **in the order they appear**, so this list and the renderer's markers have to be the
/// same sequence. `letibot-dialect`'s `media_spans` counts the markers a span sequence places, and a
/// test in `letibot-turn` holds the two to each other over a fixture that carries media — because
/// two walks of one fact is precisely how this tree has been bitten, and here the failure would be a
/// picture substituted into the wrong place, which is worse than a missing one.
///
/// **Returned as the wire wants it** — base64 with no `data:` prefix — so no caller has a chance to
/// pass the prefixed form to `multimodal_data`.
///
/// Both carriers are covered: a tool result's `media` (what `read` returns) and a user row's image
/// part (what a head attaches). A row with neither contributes nothing, which is every row of every
/// conversation with no pictures in it.
pub fn media_in_order(items: &[crate::TranscriptItem]) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        match item {
            crate::TranscriptItem::ToolResult { media: Some(m), .. } => {
                out.push(m.wire_base64().to_string())
            }
            crate::TranscriptItem::User { parts, .. } => {
                for p in parts {
                    if let crate::UserPart::Image { data_ref, .. } = p {
                        out.push(wire_payload(data_ref).to_string());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// **Mark every picture in these rows as having reached a model.**
///
/// Called by the engine and nothing else, because the engine is the only thing that knows: it builds
/// the request, so it is the only place that can say whether the bytes went out. See
/// [`Media::delivered`] for why the flag exists and why `false` is the honest default.
///
/// **A tool result's media and a user row's image part are the same fact with two carriers**, and
/// this marks the first only — the type that carries a `Media`. A user row's image is a
/// `UserPart::Image`, which has no `Media` to mark, so a head attaching a picture has no
/// `delivered` report today. Said rather than implied: that is the half still missing, and it is a
/// shape problem (the part carries a ref, not a `Media`) rather than an oversight.
pub fn mark_delivered(items: &mut [crate::TranscriptItem]) {
    for item in items {
        if let crate::TranscriptItem::ToolResult { media, .. } = item
            && let Some(m) = media
        {
            m.delivered = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A **real** PNG, `w`×`h` — the shared fixture, so this crate's tests cannot drift back into
    /// a header that is not a picture. See [`crate::testing`] for why that matters.
    fn png(w: u32, h: u32) -> Vec<u8> {
        crate::testing::png(w, h)
    }

    #[test]
    fn an_image_is_recognised_by_its_bytes_and_not_by_its_name() {
        // The magic number is the claim; the extension is only a fallback. A `.png` that is really
        // something else is the case this order exists for, and opencode's is the same order.
        let png = png(7, 9);
        assert_eq!(sniff_mime("picture.png", &png), Some("image/png"));
        // **Named wrongly, sniffed rightly** — the file is a PNG whatever it is called.
        assert_eq!(sniff_mime("picture.dat", &png), Some("image/png"));
        assert_eq!(sniff_mime("no-extension", &png), Some("image/png"));

        // And the fallback: a header this function does not know, under a name that says image.
        let unknown = b"\x00\x01\x02\x03 not-a-header-i-know";
        assert_eq!(sniff_mime("shot.jpeg", unknown), Some("image/jpeg"));
        assert_eq!(sniff_mime("shot.webp", unknown), Some("image/webp"));
        // A name that says nothing gets nothing.
        assert_eq!(sniff_mime("notes.txt", unknown), None);
        assert_eq!(sniff_mime("Makefile", unknown), None);
    }

    /// **A user row's image part carries a reference, and the size still comes out of the
    /// header** — with the base64 decoded no further than the IHDR needs.
    #[test]
    fn a_data_ref_states_its_size_without_decoding_the_picture() {
        use super::{decode_base64, encode_base64, size_of_data_ref};
        let png = png(640, 480);
        let reference = format!("data:image/png;base64,{}", encode_base64(&png));
        assert_eq!(size_of_data_ref(&reference), (Some(640), Some(480)));
        // The same bytes with no prefix at all: the mime is the only thing that came from the
        // header's spelling, so a bare payload has no format to read and no size to give.
        assert_eq!(size_of_data_ref(&encode_base64(&png)), (None, None));
        // A format whose header this module does not walk answers absent, not 0x0.
        assert_eq!(
            size_of_data_ref("data:image/gif;base64,R0lGODlh"),
            (None, None)
        );
        // Truncated payloads decode to what they have; a header cut short is absent, not wrong.
        assert_eq!(size_of_data_ref("data:image/png;base64,"), (None, None));
        // And the decoder is `encode_base64` backwards over the same alphabet.
        for bytes in [&b""[..], b"f", b"fo", b"foo", &png[..]] {
            assert_eq!(decode_base64(&encode_base64(bytes)), bytes);
        }
        assert_eq!(
            decode_base64("Zm9v\n"),
            b"foo",
            "a wrapped encoder's newline"
        );
        assert_eq!(decode_base64("Zg=="), b"f");
        assert_eq!(
            decode_base64("Zm9v!!"),
            b"foo",
            "corruption ends the decode"
        );
    }

    #[test]
    fn the_other_three_supported_types_sniff_by_magic() {
        assert_eq!(
            sniff_mime("a", b"\xff\xd8\xff\xe0something"),
            Some("image/jpeg")
        );
        assert_eq!(sniff_mime("a", b"GIF89a______"), Some("image/gif"));
        assert_eq!(sniff_mime("a", b"GIF87a______"), Some("image/gif"));
        // RIFF has a length between the tag and the form, so the check cannot be a prefix.
        assert_eq!(
            sniff_mime("a", b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_mime("a", b"RIFF\x24\x00\x00\x00WAVEfmt "), None);
    }

    #[test]
    fn png_dimensions_come_out_of_the_header_and_jpegs_out_of_its_own_walk() {
        let m = Media::of("a.png", &png(320, 200)).expect("a png");
        assert_eq!((m.width, m.height), (Some(320), Some(200)));
        assert_eq!(m.mime, "image/png");
        assert_eq!(
            m.bytes,
            png(320, 200).len(),
            "the size is the FILE's, not the base64's"
        );
        assert!(
            m.data_ref.starts_with("data:image/png;base64,"),
            "{}",
            m.data_ref
        );
        assert!(m.summary().contains("320×200"), "{}", m.summary());

        // A JPEG: SOI, an APP0 segment to be walked over, then SOF0 with the size. The segment
        // lengths are the point of the walk — a parser that assumed a fixed offset would read the
        // comment's bytes as dimensions. The fixture ends with EOI, because a JPEG without one is
        // not a picture and `Media::of` now says so.
        let m = Media::of("a.jpg", &crate::testing::jpeg(800, 600)).expect("a jpeg");
        assert_eq!((m.width, m.height), (Some(800), Some(600)));
    }

    /// **The vision cost is the measured one**, at the sizes the server was actually asked.
    ///
    /// The numbers are from `docs/leticode.md`'s table — one token per 1024 pixels, capped at 4096 —
    /// and the point of pinning them here is that the accounting defect filed in
    /// `docs/compaction.md` §8 needs a number to be fixed with, not a paragraph.
    #[test]
    fn the_vision_cost_is_one_token_per_kilopixel_capped_at_4096() {
        let at = |w, h| {
            Media {
                mime: "image/png".into(),
                bytes: 0,
                width: Some(w),
                height: Some(h),
                data_ref: String::new(),
                delivered: false,
            }
            .vision_tokens()
        };
        assert_eq!(at(64, 64), Some(4), "4 kilopixels");
        assert_eq!(at(1024, 1024), Some(1024), "1.05 MP");
        assert_eq!(at(2048, 2048), Some(4096), "4.19 MP is exactly the cap");
        assert_eq!(
            at(4096, 4096),
            Some(4096),
            "and above it the server RESIZES, so it stays 4096"
        );
        // Absent rather than zero — see the method's own note.
        let no_size = Media {
            mime: "image/gif".into(),
            bytes: 0,
            width: None,
            height: None,
            data_ref: String::new(),
            delivered: false,
        };
        assert_eq!(no_size.vision_tokens(), None);
    }

    /// **`delivered` starts false, and one call flips it — the two states it has.**
    ///
    /// leticl's argument is why it exists: *"an attachment that the daemon dropped and one the model
    /// ignored look identical on a transcript that does not say which."* And `false` is a fact rather
    /// than a guess for every row that predates the field, because no such row can carry a `Media` at
    /// all — `UserPart::Image` had no producer until 2026-09-27.
    #[test]
    fn an_image_is_not_delivered_until_something_sends_it() {
        let m = Media::of("a.png", &png(2, 2)).expect("a png");
        assert!(!m.delivered, "a picture nothing has sent is not delivered");

        let mut items = vec![
            crate::TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                outcome: crate::ToolOutcome::Ok,
                payload: "shot.png".into(),
                edit: None,
                origin: None,
                media: Some(m),
            },
            crate::TranscriptItem::ToolResult {
                call_id: "c2".into(),
                name: "read".into(),
                outcome: crate::ToolOutcome::Ok,
                payload: "fn main() {}".into(),
                edit: None,
                origin: None,
                media: None,
            },
        ];
        mark_delivered(&mut items);
        let crate::TranscriptItem::ToolResult { media, .. } = &items[0] else {
            panic!("the fixture is a tool result")
        };
        assert!(
            media.as_ref().is_some_and(|m| m.delivered),
            "the picture rode a request and the row does not say so"
        );
        // And a row with no media is untouched — the walk does not invent one.
        let crate::TranscriptItem::ToolResult { media, .. } = &items[1] else {
            panic!("the fixture is a tool result")
        };
        assert!(media.is_none());
    }

    #[test]
    fn an_unparsed_header_says_unknown_rather_than_zero() {
        // R54 §5's rule, applied one layer down: *absent is not 0x0*. A GIF's size is not parsed
        // here, and a zero would read as a picture with no pixels.
        let m = Media::of("a.gif", &crate::testing::gif(2, 2)).expect("a gif");
        assert_eq!((m.width, m.height), (None, None));
        assert!(m.summary().contains("image/gif"), "{}", m.summary());
        assert!(!m.summary().contains("0×0"), "{}", m.summary());
    }

    /// **A PNG with no `IDAT` is not an image, whatever its header says.**
    ///
    /// MEASURED 2026-10-30: these are the bytes — 33 of them, a signature and an `IHDR` — that a
    /// test wrote into the operator's paste directory, that `read` called an image, and that a
    /// provider then refused on every turn for ever. The session it wedged could not be revived by
    /// any restart, because the row is history.
    #[test]
    fn a_png_with_no_idat_is_not_an_image() {
        let header = crate::testing::png_header_only(640, 480);
        assert_eq!(header.len(), 33, "the incident's file, to the byte");
        assert_eq!(
            sniff_mime("shot.png", &header),
            Some("image/png"),
            "it still sniffs as a PNG — that is why the magic number was not enough"
        );
        assert!(
            Media::of("shot.png", &header).is_none(),
            "a header with no picture in it was called an image again"
        );
        // And a signature alone is the same answer, not a third one.
        assert!(Media::of("a.png", b"\x89PNG\r\n\x1a\n").is_none());
    }

    /// **A picture cut off mid-chunk is not a picture either** — the walk refuses a chunk whose
    /// length runs past the end, which is the truncated case a `IDAT`-and-`IEND` check alone would
    /// miss when the truncation is what removed them.
    #[test]
    fn a_truncated_png_is_not_an_image() {
        let full = png(2, 2);
        assert!(Media::of("a.png", &full).is_some(), "the fixture is complete");
        // Every prefix of it, up to the last byte of the IEND, is incomplete.
        for cut in [8usize, 20, 33, 40, full.len() - 1] {
            assert!(
                Media::of("a.png", &full[..cut]).is_none(),
                "a PNG cut to {cut} of {} bytes was called an image",
                full.len()
            );
        }
    }

    /// **The four formats, complete and not** — one table, so a format added to
    /// [`SUPPORTED_IMAGE_MIMES`] without a completeness rule is a test that fails rather than a
    /// picture a provider refuses.
    #[test]
    fn every_supported_format_has_a_completeness_rule() {
        use crate::testing::{gif, jpeg, png, webp};
        let complete: [(&str, Vec<u8>); 4] = [
            ("image/png", png(2, 2)),
            ("image/jpeg", jpeg(2, 2)),
            ("image/gif", gif(2, 2)),
            ("image/webp", webp(2, 2)),
        ];
        for (mime, bytes) in &complete {
            assert!(complete_image(mime, bytes), "{mime} is not recognised");
            assert!(
                data_ref_is_image(&crate::testing::data_ref(mime, bytes)),
                "{mime} is not recognised through a data_ref"
            );
        }
        // The same file with its tail cut: a JPEG without `EOI`, a GIF without its trailer, a WEBP
        // whose only chunk does not fit, a PNG without its `IEND`.
        for (mime, bytes) in &complete {
            let cut = &bytes[..bytes.len() - 2];
            assert!(!complete_image(mime, cut), "a truncated {mime} passed");
        }
        // A type this tree does not hand a model is not an image, whatever the bytes are.
        assert!(!complete_image("image/tiff", &png(2, 2)));
        // And a reference that is not one decodes to nothing and answers `false`.
        assert!(!data_ref_is_image("not a reference"));
        assert!(!data_ref_is_image("data:image/png;base64,"));
    }
}
