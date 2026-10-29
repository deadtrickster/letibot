//! **Fixtures that are real images** — because a fixture that only *looked* like one hid a bug
//! that cost a session.
//!
//! MEASURED 2026-10-30: every picture fixture in this tree was a PNG signature, an `IHDR`, and
//! nothing else — 33 bytes, no `IDAT`, no `IEND`, a zeroed CRC. Every test that used one asserted
//! that `read` had called it an image, and it had: [`crate::media::Media::of`] read the magic and
//! the header and asked nothing more. Then a `read` of exactly those bytes was stored as an image
//! and sent to a provider, which refused the **whole request** —
//! `.messages[1861].image[0]: You have uploaded an unsupported image` — on every turn, for ever,
//! because a transcript row is append-only and the picture was history.
//!
//! So the fixtures here are **complete** images: [`png`] is a real RGBA picture whose bytes
//! CoreGraphics (`sips -g pixelWidth`) decodes, and [`png_header_only`] is the incident's 33 bytes,
//! kept on purpose so a test can assert that they are refused.
//!
//! Public rather than `#[cfg(test)]` for the reason `letibot_sessionlog::testing` is public: the
//! crates that need a picture in a fixture — the head, the daemon, `read`'s own test — are not this
//! one, and a second copy of a PNG builder is a second copy that can drift back into a stub.
//!
//! # What each fixture claims
//!
//! [`png`] is a decodable picture: the scanlines are there, the CRCs are right, the deflate stream
//! is a real one (a stored block, so the bytes are visible rather than compressed).
//!
//! [`jpeg`], [`gif`] and [`webp`] are **structurally complete** — the header, the payload chunks
//! and the trailer [`crate::media::complete_image`] reads — and their pixels are stubs, because
//! nothing in this tree decodes them and a hand-written baseline JPEG would be a page of tables in
//! the middle of a test fixture. Said here rather than implied.

/// **A real PNG**, `w`×`h`, opaque black RGBA.
///
/// Built rather than embedded so a caller can ask for the dimensions it needs and read them back
/// out of the header. The `IDAT` is one zlib stream made of stored (uncompressed) deflate blocks,
/// which is why the bytes are four times the pixel count plus a header — the fixture is a picture,
/// not a compression test.
pub fn png(w: u32, h: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity(h as usize * (1 + 4 * w as usize));
    for _ in 0..h {
        // Filter type 0 (none), then the row's pixels.
        raw.push(0);
        for _ in 0..w {
            raw.extend_from_slice(&[0x00, 0x00, 0x00, 0xff]);
        }
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    // Bit depth 8, colour type 6 (RGBA), deflate, adaptive filtering, no interlace.
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// **The bytes that wedged a session** — a PNG signature and an `IHDR` for `w`×`h`, a zeroed CRC,
/// and no picture at all.
///
/// Kept as a fixture because the refusal it produces is the behaviour under test: a file that
/// announces itself as a PNG and holds no image is not an image, and the cost of treating it as
/// one is a session no restart could revive.
pub fn png_header_only(w: u32, h: u32) -> Vec<u8> {
    let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
    p.extend_from_slice(&13u32.to_be_bytes());
    p.extend_from_slice(b"IHDR");
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    p.extend_from_slice(&[8, 6, 0, 0, 0]);
    // The CRC nobody computed: this file was never written by an encoder.
    p.extend_from_slice(&[0, 0, 0, 0]);
    p
}

/// **A structurally complete JPEG** — `SOI`, a JFIF `APP0`, a `SOF0` carrying `w`×`h`, a `DQT`, an
/// `SOS` header, a few bytes of entropy-coded data and `EOI`.
///
/// The entropy-coded data is a stub: nothing here decodes a JPEG, and [`crate::media::jpeg_size`]
/// is the only reader, which is the header walk this fixture exists to exercise.
pub fn jpeg(w: u16, h: u16) -> Vec<u8> {
    let mut out = b"\xff\xd8".to_vec();
    let app0 = b"JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00";
    jpeg_segment(&mut out, 0xe0, app0);
    let mut sof = vec![8u8];
    sof.extend_from_slice(&h.to_be_bytes());
    sof.extend_from_slice(&w.to_be_bytes());
    sof.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
    jpeg_segment(&mut out, 0xc0, &sof);
    jpeg_segment(&mut out, 0xdb, &[0u8; 65]);
    jpeg_segment(&mut out, 0xda, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0x00, 0x3f, 0x00]);
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(b"\xff\xd9");
    out
}

/// **A structurally complete GIF** — `GIF89a`, a logical screen descriptor, a two-entry colour
/// table, one image descriptor with a data sub-block, and the trailer.
pub fn gif(w: u16, h: u16) -> Vec<u8> {
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    // A global colour table of two entries, 8-bit colour resolution.
    out.extend_from_slice(&[0x80, 0, 0]);
    out.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    out.push(0x2c); // image descriptor
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.push(0); // no local colour table
    out.push(2); // LZW minimum code size
    out.extend_from_slice(&[2, 0x44, 0x01, 0x00]); // one sub-block, then its terminator
    out.push(0x3b); // trailer
    out
}

/// **A structurally complete WEBP** — a `RIFF`/`WEBP` container with one `VP8L` chunk.
pub fn webp(w: u32, h: u32) -> Vec<u8> {
    let mut vp8l = vec![0x2f]; // the VP8L signature byte
    vp8l.extend_from_slice(&((w - 1) | ((h - 1) << 14)).to_le_bytes());
    vp8l.extend_from_slice(&[0u8; 5]);
    let mut chunk = b"VP8L".to_vec();
    chunk.extend_from_slice(&(vp8l.len() as u32).to_le_bytes());
    chunk.extend_from_slice(&vp8l);
    let mut body = b"WEBP".to_vec();
    body.extend_from_slice(&chunk);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// **One image as the `data:` reference a row carries** — the shape both a `Media` and a
/// `UserPart::Image` hold, so a fixture does not spell the prefix a second time.
pub fn data_ref(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        crate::media::encode_base64(bytes)
    )
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn jpeg_segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
}

/// **A zlib stream of stored blocks** — a real deflate stream, and one whose bytes are the input,
/// so the fixture needs no compressor. A stored block carries at most 65535 bytes.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // CMF/FLG: deflate, 32 KiB window, no dictionary
    let mut chunks: Vec<&[u8]> = data.chunks(65_535).collect();
    if chunks.is_empty() {
        chunks.push(&[]);
    }
    let last = chunks.len() - 1;
    for (i, c) in chunks.iter().enumerate() {
        out.push(if i == last { 0x01 } else { 0x00 }); // BFINAL, BTYPE=00 (stored)
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut c: u32 = 0xffff_ffff;
    for b in bytes {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xedb8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for x in bytes {
        a = (a + u32::from(*x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::complete_image;

    /// **Every fixture passes the predicate it exists for**, and the incident's bytes do not.
    #[test]
    fn the_fixtures_are_complete_and_the_header_only_png_is_not() {
        assert!(complete_image("image/png", &png(2, 2)));
        assert!(complete_image("image/jpeg", &jpeg(2, 2)));
        assert!(complete_image("image/gif", &gif(2, 2)));
        assert!(complete_image("image/webp", &webp(2, 2)));
        assert!(
            !complete_image("image/png", &png_header_only(640, 480)),
            "the fixture that wedged a session is being called an image again"
        );
    }

    /// **The CRCs are computed, not zero** — the whole difference between the fixture this module
    /// replaces and a picture: an encoder writes them and a decoder checks them.
    #[test]
    fn the_png_is_a_picture_and_not_a_header() {
        let p = png(2, 2);
        assert!(p.windows(4).any(|w| w == b"IDAT"), "no IDAT");
        assert!(p.ends_with(b"IEND\xae\x42\x60\x82"), "no IEND with its CRC");
        // The IHDR's own CRC is the one over `IHDR` and its thirteen bytes.
        let ihdr_crc = u32::from_be_bytes([p[29], p[30], p[31], p[32]]);
        assert_eq!(ihdr_crc, crc32(&p[12..29]), "the IHDR's CRC is wrong");
        assert_ne!(ihdr_crc, 0, "the fixture this replaces had a zeroed CRC");
    }
}
