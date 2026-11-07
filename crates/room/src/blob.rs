//! The blob store: the pictures and files a chat message carries, kept as bytes beside the chat
//! store and named by the sha256 of what they hold. No database and no index beyond a sidecar of
//! metadata per blob: the id is the content, so a re-upload of the same bytes is the same blob.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::chat::Attachment;
use crate::room::Refused;

/// The base64 alphabet the whole tree spells once: a blob part travels through a frame that is one
/// line of JSON, and a second encoder is exactly the fact that gets written subtly wrong twice.
pub use letibot_transcript::media::{decode_base64, encode_base64};

/// The largest one blob may be. A message's four attachments are therefore 32 MiB at the very
/// most, which bounds what one picture can cost the coordinator's disk and its heap.
pub const MAX_BLOB: usize = 8 * 1024 * 1024;
/// The bytes one `put_blob` or `get_blob` part carries. Four of these are a frame's payload, so a
/// part plus its base64 and JSON stays under the server's 1 MiB line.
pub const MAX_PART: usize = 512 * 1024;
/// How long a blob nothing references is kept. A picture is uploaded and then said a moment later,
/// and the wider window is what stops a collection landing between the two.
pub const UNREFERENCED_MS: u64 = 60 * 60 * 1000;
/// The kinds of file a message may carry: pictures, and text a person wrote.
pub const MIMES: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "text/plain",
];

/// The bytes the store answers one `get_blob` with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub attachment: Attachment,
    pub offset: u64,
    pub data: Vec<u8>,
    /// No bytes follow this part.
    pub last: bool,
}

/// The store: one directory, `<dir>/<id>` for the bytes and `<dir>/<id>.json` for what they are.
/// A part being uploaded accumulates in `<dir>/.upload-<upload>` with its `.json` beside it, so an
/// interrupted upload is a file the next collection removes rather than a blob half the id of its own.
pub struct Blobs {
    dir: PathBuf,
}

impl Blobs {
    /// The store in `dir`. Nothing is created until the first part is put: a room with no pictures
    /// touches no disk.
    pub fn open(dir: &Path) -> Blobs {
        Blobs {
            dir: dir.to_path_buf(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Stage one part of an upload. `part` is the 0-based index the client means, so a lost or
    /// reordered frame is refused instead of assembled into an id nobody asked for; `last` finishes
    /// the upload and answers the attachment, and a part before it answers `None`.
    #[allow(clippy::too_many_arguments)]
    pub fn put_part(
        &mut self,
        upload: &str,
        name: &str,
        mime: &str,
        part: u32,
        data: &[u8],
        last: bool,
        now: u64,
    ) -> Result<Option<Attachment>, Refused> {
        if !MIMES.contains(&mime) {
            return Err(Refused::BadMime {
                mime: mime.to_string(),
            });
        }
        if data.len() > MAX_PART {
            return Err(Refused::BlobTooLarge {
                max: MAX_PART as u64,
                got: data.len() as u64,
            });
        }
        if upload.is_empty()
            || !upload
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            // The upload's name is a path component: anything else would let a frame choose where
            // this store writes.
            return Err(Refused::UnknownBlob {
                id: upload.to_string(),
            });
        }
        std::fs::create_dir_all(&self.dir).ok();
        let mut staged = self.staged(upload);
        let had = staged.parts;
        if part != had {
            return Err(Refused::BadPart {
                expected: had,
                got: part,
            });
        }
        let got = staged.bytes.len() as u64 + data.len() as u64;
        if got > MAX_BLOB as u64 {
            return Err(Refused::BlobTooLarge {
                max: MAX_BLOB as u64,
                got,
            });
        }
        staged.bytes.extend_from_slice(data);
        staged.parts += 1;
        staged.name = name.to_string();
        staged.mime = mime.to_string();
        if had == 0 {
            // The clock is the caller's, so a store kept for an hour is kept for an hour of the
            // coordinator's time, not of the file system's.
            staged.created_ms = now;
        }
        if !last {
            staged.save(&self.dir);
            return Ok(None);
        }
        let id = hex(&sha(&staged.bytes));
        let (w, h) = dimensions(mime, &staged.bytes);
        let attachment = Attachment {
            id: id.clone(),
            mime: mime.to_string(),
            name: name.to_string(),
            bytes: staged.bytes.len() as u64,
            w,
            h,
        };
        std::fs::write(self.blob(&id), &staged.bytes).map_err(|e| {
            eprintln!("room: blob store {} not written ({e})", self.dir.display());
            Refused::UnknownBlob { id: id.clone() }
        })?;
        let _ = std::fs::write(self.meta(&id), sidecar(&attachment, now));
        staged.remove(&self.dir);
        Ok(Some(attachment))
    }

    /// A stored blob's attachment, `None` when the store does not hold it. An id is a filename, so
    /// anything that is not the sha256 hex a `put_blob` answers is not looked for at all.
    pub fn get(&self, id: &str) -> Option<Attachment> {
        if !is_id(id) {
            return None;
        }
        let text = std::fs::read_to_string(self.meta(id)).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        Some(Attachment {
            id: id.to_string(),
            mime: v.get("mime")?.as_str()?.to_string(),
            name: v
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
            bytes: v.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0),
            w: v.get("w").and_then(|w| w.as_u64()).map(|w| w as u32),
            h: v.get("h").and_then(|h| h.as_u64()).map(|h| h as u32),
        })
    }

    /// The bytes at `offset` onwards, at most [`MAX_PART`] of them.
    pub fn read(&self, id: &str, offset: u64) -> Result<Part, Refused> {
        if !is_id(id) {
            return Err(Refused::UnknownBlob { id: id.to_string() });
        }
        let attachment = self
            .get(id)
            .ok_or_else(|| Refused::UnknownBlob { id: id.to_string() })?;
        let bytes = std::fs::read(self.blob(id))
            .map_err(|_| Refused::UnknownBlob { id: id.to_string() })?;
        let from = (offset as usize).min(bytes.len());
        let end = (from + MAX_PART).min(bytes.len());
        Ok(Part {
            attachment,
            offset: from as u64,
            data: bytes[from..end].to_vec(),
            last: end == bytes.len(),
        })
    }

    /// Drop every blob no kept message references, and every upload a whole window old. Answers
    /// how many went. Called when the chat store is rewritten, which is the one moment the ring's
    /// answer to *what does a message still name* is exact.
    pub fn gc(&mut self, kept: &BTreeSet<String>, now: u64) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut dropped = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(upload) = name.strip_prefix(".upload-") {
                if let Some(rest) = upload.strip_suffix(".json") {
                    let staged = self.staged(rest);
                    if now.saturating_sub(staged.created_ms) > UNREFERENCED_MS {
                        staged.remove(&self.dir);
                        dropped += 1;
                    }
                }
                continue;
            }
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if kept.contains(id) {
                continue;
            }
            let created = self.meta_created(id);
            if now.saturating_sub(created) < UNREFERENCED_MS {
                continue;
            }
            let _ = std::fs::remove_file(self.meta(id));
            let _ = std::fs::remove_file(self.blob(id));
            dropped += 1;
        }
        dropped
    }

    fn blob(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    fn meta(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn staged(&self, upload: &str) -> Staged {
        let path = self.dir.join(format!(".upload-{upload}"));
        let text = std::fs::read_to_string(self.dir.join(format!(".upload-{upload}.json")))
            .unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        Staged {
            upload: upload.to_string(),
            parts: v.get("parts").and_then(|p| p.as_u64()).unwrap_or(0) as u32,
            name: v
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
            mime: v
                .get("mime")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string(),
            created_ms: v
                .get("created_ms")
                .and_then(|c| c.as_u64())
                .unwrap_or(u64::MAX),
            bytes: std::fs::read(path).unwrap_or_default(),
        }
    }

    fn meta_created(&self, id: &str) -> u64 {
        std::fs::read_to_string(self.meta(id))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.get("created_ms").and_then(|c| c.as_u64()))
            .unwrap_or(u64::MAX)
    }
}

/// One upload's parts, as they accumulate.
struct Staged {
    upload: String,
    parts: u32,
    name: String,
    mime: String,
    created_ms: u64,
    bytes: Vec<u8>,
}

impl Staged {
    fn save(&self, dir: &Path) {
        let _ = std::fs::write(dir.join(format!(".upload-{}", self.upload)), &self.bytes);
        let _ = std::fs::write(
            dir.join(format!(".upload-{}.json", self.upload)),
            serde_json::json!({
                "parts": self.parts,
                "name": self.name,
                "mime": self.mime,
                "created_ms": self.created_ms,
            })
            .to_string(),
        );
    }

    fn remove(self, dir: &Path) {
        let _ = std::fs::remove_file(dir.join(format!(".upload-{}", self.upload)));
        let _ = std::fs::remove_file(dir.join(format!(".upload-{}.json", self.upload)));
    }
}

fn sidecar(attachment: &Attachment, now: u64) -> String {
    serde_json::json!({
        "mime": attachment.mime,
        "name": attachment.name,
        "bytes": attachment.bytes,
        "w": attachment.w,
        "h": attachment.h,
        "created_ms": now,
    })
    .to_string()
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

fn hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Is this an id this store minted: the sha256 hex of some bytes, and nothing that could be a path?
fn is_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The pixel size of a picture, read out of its header: a PNG's IHDR, a GIF's screen descriptor.
/// Anything else is `(None, None)` - a reader falls back to one row naming the picture.
fn dimensions(mime: &str, bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    let be = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let le = |at: usize| -> Option<u32> {
        Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u32)
    };
    match mime {
        "image/png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => (be(16), be(20)),
        "image/gif" => (le(6), le(8)),
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("letibot-blobs-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// The id is the content, so the same bytes uploaded twice are one blob and a reader can check
    /// what it fetched against the id the message named.
    #[test]
    fn a_blob_put_in_parts_hashes_to_its_id() {
        let d = dir("parts");
        let mut blobs = Blobs::open(&d);
        let whole = b"\x89PNG\r\n\x1a\n a small picture, in two parts";
        let cut = 14;
        blobs
            .put_part(
                "u1",
                "shot.png",
                "image/png",
                0,
                &whole[..cut],
                false,
                1_000,
            )
            .expect("part 0");
        let done = blobs
            .put_part("u1", "shot.png", "image/png", 1, &whole[cut..], true, 1_000)
            .expect("part 1 finishes it")
            .expect("a whole blob");
        assert_eq!(done.bytes, whole.len() as u64);
        assert_eq!(done.name, "shot.png");
        assert_eq!(done.mime, "image/png");
        assert_eq!(done.id, hex(&sha(whole)), "the id is the bytes");
        let read = blobs.read(&done.id, 0).expect("what it just stored");
        assert_eq!(read.data, whole);
        assert!(read.last);
        assert_eq!(blobs.get(&done.id), Some(done.clone()));
        // A part out of order is refused rather than appended where it does not belong.
        let err = blobs
            .put_part("u2", "x.png", "image/png", 1, b"x", false, 1_000)
            .unwrap_err();
        assert_eq!(
            err,
            Refused::BadPart {
                expected: 0,
                got: 1
            }
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_blob_over_the_cap_is_refused() {
        let d = dir("cap");
        let mut blobs = Blobs::open(&d);
        let chunk = vec![7u8; MAX_PART];
        let parts = MAX_BLOB / MAX_PART;
        for i in 0..parts {
            blobs
                .put_part(
                    "big",
                    "big.bin",
                    "image/png",
                    i as u32,
                    &chunk,
                    false,
                    1_000,
                )
                .expect("a part under the cap");
        }
        let err = blobs
            .put_part(
                "big",
                "big.bin",
                "image/png",
                parts as u32,
                &chunk,
                false,
                1_000,
            )
            .unwrap_err();
        assert!(
            matches!(err, Refused::BlobTooLarge { max, .. } if max == MAX_BLOB as u64),
            "{err:?}"
        );
        // One part larger than the wire allows is refused before it is written at all.
        let err = blobs
            .put_part(
                "one",
                "x.png",
                "image/png",
                0,
                &vec![0u8; MAX_PART + 1],
                false,
                1_000,
            )
            .unwrap_err();
        assert!(matches!(err, Refused::BlobTooLarge { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A picture whose message fell off the ring is a picture nobody can read, and the store is
    /// collected when the chat store is rewritten - but not before a window has passed, because the
    /// upload a message is about to name is precisely a blob nothing references yet.
    #[test]
    fn a_blob_no_message_keeps_is_deleted_at_rewrite() {
        let d = dir("gc");
        let mut blobs = Blobs::open(&d);
        let say = |blobs: &mut Blobs, upload: &str, bytes: &[u8]| -> Attachment {
            blobs
                .put_part(upload, "a.png", "image/png", 0, bytes, true, 1_000)
                .expect("a blob")
                .expect("a whole one")
        };
        let kept = say(&mut blobs, "k", b"the one a message names");
        let gone = say(&mut blobs, "g", b"the one no message names");
        // A collection inside the window is not a collection: the say may still be coming.
        assert_eq!(blobs.gc(&BTreeSet::from([kept.id.clone()]), 1_000), 0);
        assert!(blobs.get(&gone.id).is_some());
        let later = 1_000 + UNREFERENCED_MS + 1;
        let dropped = blobs.gc(&BTreeSet::from([kept.id.clone()]), later);
        assert_eq!(dropped, 1, "one blob, one drop");
        assert!(blobs.get(&kept.id).is_some(), "the referenced one stays");
        assert!(blobs.get(&gone.id).is_none(), "the unreferenced one goes");
        assert!(blobs.read(&gone.id, 0).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An id a frame chose is a path too, so a `get_blob` is held to the id shape: `../` names no
    /// blob this store minted, and reading outside it would hand a reader another file entirely.
    #[test]
    fn an_upload_name_cannot_walk_out_of_the_store() {
        let d = dir("name");
        let mut blobs = Blobs::open(&d);
        for bad in ["../x", "a/b", ""] {
            let err = blobs
                .put_part(bad, "x", "image/png", 0, b"x", false, 1_000)
                .unwrap_err();
            assert!(matches!(err, Refused::UnknownBlob { .. }), "{bad}: {err:?}");
        }
        // The store is in `d`, and its own sidecar is a file a wrong id would find.
        let blob = blobs
            .put_part("ok", "a.png", "image/png", 0, b"picture", true, 1_000)
            .expect("a blob")
            .expect("a whole one");
        assert!(blobs.get(&blob.id).is_some());
        for bad in [
            format!("../{}.json", blob.id),
            blob.id.to_uppercase(),
            blob.id[..63].to_string(),
        ] {
            assert!(blobs.get(&bad).is_none(), "{bad} is not an id");
            assert!(
                matches!(blobs.read(&bad, 0), Err(Refused::UnknownBlob { .. })),
                "{bad} names no bytes"
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
