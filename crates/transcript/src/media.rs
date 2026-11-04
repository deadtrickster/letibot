//! Bytes a tool read that are not text, kept as bytes: the structured channel on
//! `letibot_tools::runtime::Invocation` that is sent, beside display-only `letibot_tools::edit::FileEdit`.
//! The bytes live on the row, the durable source of a rebuilt prompt.

use serde::{Deserialize, Serialize};

/// The size a `data:` reference states, read out of the header only, so a head can cut a
/// picture's cell box without decoding the pixels. The mime comes from the reference's own
/// prefix rather than a name, because a name is the weaker claim anyway.
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

/// Standard base64, decoded - [`encode_base64`] backwards. Tolerant on purpose: a payload
/// cut mid-quantum, or written by another version, must not turn an image into no image at
/// all. It does not guess - a character outside the alphabet ends the decode.
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

/// The base64 alphabet, spelled once for the whole tree: `media` encodes a `data:` URI and
/// the tool layer's `firecode` backend encodes a file to fetch it, and a second copy is
/// exactly the fact that gets spelled subtly wrong in one of the two places.
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

/// The image types this tree will hand a model. Not a policy list: it is what the model
/// server's chat template can read. A type outside it is not refused, it is simply not an
/// image, and `read` shows it the way it shows any other bytes.
pub const SUPPORTED_IMAGE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Bytes a call produced that are not text, with the metadata a row needs to describe them.
/// `media_type` alone answers the wrong question - a reader wants to know how big and what
/// shape - and `bytes` is the file's size, a number a person can compare with `ls`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    /// `image/png`. Sniffed from the bytes; see [`sniff_mime`].
    pub mime: String,
    /// The file's own size in bytes, NOT the size of [`Media::data_ref`] - base64 is 4/3 of it and
    /// the difference is an encoding detail rather than a fact about the picture.
    pub bytes: usize,
    /// Pixels, when the header carries them. `None` is not `0` - a format whose header this
    /// module does not parse has an unknown size, and the same argument holds for the row:
    /// *absent is not 0x0*.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `data:image/png;base64,…` - the form the model server takes, verbatim.
    pub data_ref: String,
    /// Whether a model was actually given these bytes. A dropped attachment and an ignored
    /// one look identical on a transcript that does not say which, and only one of them is
    /// the operator's to correct. Set by the engine, which builds the request.
    #[serde(default)]
    pub delivered: bool,
}

impl Media {
    /// A picture this call read, or `None` when the bytes are not one of the supported
    /// images. The mime comes from the bytes first and the path only as a fallback, because a
    /// file's name is a claim and its magic number is not. See [`complete_image`].
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

    /// The base64 a multimodal request carries - the payload WITHOUT the `data:` prefix,
    /// because llama.cpp decodes each element of `multimodal_data` whole and the prefix would
    /// be decoded as image bytes. A reference that is already bare is carried through.
    pub fn wire_base64(&self) -> &str {
        wire_payload(&self.data_ref)
    }

    /// What this image costs the prompt, in vision tokens, or `None` when the header gave no
    /// size: one token per 1024 pixels, capped at 4096. It lives on the type because the
    /// accounting for a row that carries media is the one caller and it needs the number.
    pub fn vision_tokens(&self) -> Option<u64> {
        let (w, h) = (self.width?, self.height?);
        Some((u64::from(w) * u64::from(h) / 1024).min(4096))
    }

    /// The one line a head draws in place of the bytes. Text, carrying no data at all: a
    /// payload that inlined the base64 would put a megabyte of it through every renderer,
    /// every `grep` over the log, and every scrollback.
    pub fn summary(&self) -> String {
        match (self.width, self.height) {
            (Some(w), Some(h)) => {
                format!("image {} {w}×{h} · {} KiB", self.mime, self.bytes / 1024)
            }
            _ => format!("image {} · {} KiB", self.mime, self.bytes / 1024),
        }
    }
}

/// The image type of these bytes, by magic number, with the path as a fallback - a name is
/// a claim and a magic number is not. `None` means not an image this tree will hand a model,
/// which includes a PDF: there is no PDF path to put one on.
pub fn sniff_mime(path: &str, bytes: &[u8]) -> Option<&'static str> {
    // Magic first. Every one of these is a fixed prefix, and none is ambiguous with another.
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    // RIFF….WEBP - the four bytes after `RIFF` are a little-endian length, so the tag is at 8.
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    // Then the name, which is where opencode puts it too: a file whose *extension* says image and
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

/// Is this a COMPLETE image of the type it claims? - judged structurally, per format,
/// without decoding a pixel. A magic number says what a file is trying to be; this says
/// whether there is a picture in it, which is the one thing a structural check can refuse.
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

/// The same predicate for a picture a row already carries - a `data:` reference, decoded
/// just far enough to ask [`complete_image`]. This is the half that lets a fixed binary
/// reopen a session a broken one wedged, because the bad picture is still in the history.
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
        let len =
            u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
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
/// `None` for a format this does not parse, which is honest rather than a zero: absent is
/// not 0x0, and GIF and WebP are omitted rather than guessed.
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
    // `i + 4 <= len` to read a marker and its length, and `i + 9 <= len` to read a frame
    // header's last byte at `i + 8` - the off-by-one that a fixture ending exactly at the width
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

/// The `data:` prefix off a reference, or the reference unchanged when it has none. One rule
/// for two carriers, because a second spelling of "strip the prefix" is a second chance to
/// fail with nothing but `Failed to load image or audio file` to show for it.
pub fn wire_payload(data_ref: &str) -> &str {
    data_ref
        .split_once(";base64,")
        .map(|(_, b)| b)
        .unwrap_or(data_ref)
}

/// Every image in a transcript, in the order a prompt places them, ready for the wire. A
/// server substitutes `multimodal_data` at the markers in order, so this list and the
/// renderer's markers have to be one sequence; base64 with no prefix, as the wire wants it.
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

/// Mark every picture in these rows as having reached a model. Called by the engine and
/// nothing else, because it builds the request and so is the only thing that knows. A tool
/// result's media is marked; a user row's image part has no `Media` to mark yet.
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

    /// A real PNG, `w`×`h` - the shared fixture, so this crate's tests cannot drift back into
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
        // Named wrongly, sniffed rightly - the file is a PNG whatever it is called.
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

    /// A user row's image part carries a reference, and the size still comes out of the
    /// header - with the base64 decoded no further than the IHDR needs.
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

        // A JPEG: SOI, an APP0 segment to walk over, then SOF0 with the size. The segment
        // lengths are the point - a fixed offset would read the comment's bytes as
        // dimensions - and the fixture ends with EOI, because a JPEG without one is not a picture.
        let m = Media::of("a.jpg", &crate::testing::jpeg(800, 600)).expect("a jpeg");
        assert_eq!((m.width, m.height), (Some(800), Some(600)));
    }

    /// The vision cost is the measured one, at the sizes the server was actually asked: one
    /// token per 1024 pixels, capped at 4096. Pinned here because the accounting defect needs
    /// a number to be fixed with, not a paragraph.
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
        // Absent rather than zero - see the method's own note.
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

    /// `delivered` starts false and one call flips it. False is a fact rather than a guess for
    /// every row that predates the field, because no such row can carry a `Media` at all.
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
        // And a row with no media is untouched - the walk does not invent one.
        let crate::TranscriptItem::ToolResult { media, .. } = &items[1] else {
            panic!("the fixture is a tool result")
        };
        assert!(media.is_none());
    }

    #[test]
    fn an_unparsed_header_says_unknown_rather_than_zero() {
        // Same rule, one layer down: *absent is not 0x0*. A GIF's size is not parsed
        // here, and a zero would read as a picture with no pixels.
        let m = Media::of("a.gif", &crate::testing::gif(2, 2)).expect("a gif");
        assert_eq!((m.width, m.height), (None, None));
        assert!(m.summary().contains("image/gif"), "{}", m.summary());
        assert!(!m.summary().contains("0×0"), "{}", m.summary());
    }

    /// A PNG with no `IDAT` is not an image, whatever its header says: these are the bytes
    /// that a provider refused on every turn for ever, in a session no restart could revive,
    /// because the row is history.
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

    /// A picture cut off mid-chunk is not a picture either - the walk refuses a chunk whose
    /// length runs past the end, which is the truncated case a `IDAT`-and-`IEND` check alone would
    /// miss when the truncation is what removed them.
    #[test]
    fn a_truncated_png_is_not_an_image() {
        let full = png(2, 2);
        assert!(
            Media::of("a.png", &full).is_some(),
            "the fixture is complete"
        );
        // Every prefix of it, up to the last byte of the IEND, is incomplete.
        for cut in [8usize, 20, 33, 40, full.len() - 1] {
            assert!(
                Media::of("a.png", &full[..cut]).is_none(),
                "a PNG cut to {cut} of {} bytes was called an image",
                full.len()
            );
        }
    }

    /// The four formats, complete and not - one table, so a format added to
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
