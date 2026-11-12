//! What a chip in the composer becomes in the transcript. A chip travels as a
//! [`letibot_sessionlog::protocol::Attachment`] and this module turns it into the
//! [`UserPart`] the same file would have produced through `read`.

use letibot_sessionlog::protocol::Attachment;
use letibot_transcript::UserPart;
use sha2::{Digest, Sha256};

/// A picture bigger than this is refused, as `read` refuses one: one number for both, or a
/// picture `read` would refuse and a chip would attach are two answers to how big is too big
/// and the second is discovered by the store.
pub const MAX_IMAGE_BYTES: u64 = letibot_tools::builtins::grep::FILE_CEILING * 16;

/// How much of a text file is inlined - 256 KiB, which is a long log, a whole source
/// file, a large CSV. Above it the part is a [`UserPart::FileRef`] instead, and the model
/// reads what it needs
pub const MAX_INLINE_TEXT: u64 = 256 * 1024;

/// The parts the attachments add, in the order they were chipped, plus one sentence per
/// attachment that could not be read. Pure with respect to the session
pub fn parts(attachments: &[Attachment]) -> (Vec<UserPart>, Vec<String>) {
    let mut parts = Vec::with_capacity(attachments.len());
    let mut refused = Vec::new();
    for (nth, a) in attachments.iter().enumerate() {
        let nth = nth + 1;
        match one(a) {
            Ok(Some(part)) => parts.extend(part),
            Ok(None) => {}
            Err(why) => {
                let said = format!("[attachment {nth} · {} — {why}]", name_of(a));
                refused.push(said.clone());
                parts.push(UserPart::Text { text: said });
            }
        }
    }
    (parts, refused)
}

/// What one attachment became, or why it did not.
fn one(a: &Attachment) -> Result<Option<Vec<UserPart>>, String> {
    let path = std::path::Path::new(&a.path);
    let meta = std::fs::metadata(path).map_err(|e| format!("could not be read: {e}"))?;
    if meta.is_dir() {
        return Err("that is a directory; attach a file".to_string());
    }
    let size = meta.len();
    // The picture is recognised from its BYTES and before the size cap is argued, because
    // the two outcomes have different ceilings: an image is refused above `read`'s (a `data:`
    // URI of it is carried whole), a text file rides its own.
    let bytes = std::fs::read(path).map_err(|e| format!("could not be read: {e}"))?;
    if let Some(m) = letibot_transcript::media::Media::of(&a.path, &bytes) {
        if size > MAX_IMAGE_BYTES {
            return Err(format!(
                "an image this size ({} MiB) is not attached — the ceiling is {} MiB, the \
                 same one `read` refuses above",
                size / (1024 * 1024),
                MAX_IMAGE_BYTES / (1024 * 1024)
            ));
        }
        return Ok(Some(vec![UserPart::Image {
            media_type: m.mime.clone(),
            data_ref: m.data_ref.clone(),
        }]));
    }
    // Not a picture this tree can hand a model. Text under the cap goes in whole; anything
    // else is a path the model may read itself.
    let text = match (size <= MAX_INLINE_TEXT).then(|| std::str::from_utf8(&bytes)) {
        Some(Ok(t)) => Some(t),
        // A file that is not UTF-8 is bytes, not text: a `FileRef`, never a lossy decode - the
        // same rule `read` keeps, and the reason a model can tell a screenshot from a shard.
        Some(Err(_)) | None => None,
    };
    let mut out = Vec::with_capacity(2);
    out.push(UserPart::FileRef {
        path: a.path.clone(),
        sha256: hex(Sha256::digest(&bytes).as_slice()),
    });
    if let Some(t) = text {
        out.push(UserPart::Text {
            text: format!(
                "--- {} · {} · attached ---\n{}",
                name_of(a),
                size_said(size),
                t
            ),
        });
    }
    Ok(Some(out))
}

/// The file's name for a sentence: the head's, or the path's tail when it sent none.
fn name_of(a: &Attachment) -> String {
    if !a.name.is_empty() {
        return a.name.clone();
    }
    std::path::Path::new(&a.path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| a.path.clone())
}

/// A byte count a person reads - `92 KiB`, `1.4 MiB`. The same shape `read`'s own sentences use
/// so a chip and a read of the same file report the same size.
fn size_said(bytes: u64) -> String {
    const K: u64 = 1024;
    if bytes >= K * K {
        format!("{:.1} MiB", bytes as f64 / (K * K) as f64)
    } else if bytes >= K {
        format!("{} KiB", bytes / K)
    } else {
        format!("{bytes} B")
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::protocol::AttachmentKind;
    /// A directory of this test's own, made fresh, named for the test so a rerun does not read
    /// the previous one's files.
    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "letibot-attach-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).expect("a temp dir");
        d
    }

    /// A real PNG, 640×480 - `letibot_transcript::testing`'s, because a header-only fixture is
    /// what let a 33-byte file be stored as a picture and wedge a session. `Media::of` now asks
    /// whether there is a picture in the bytes, not only what they claim to be.
    fn png() -> Vec<u8> {
        letibot_transcript::testing::png(640, 480)
    }

    fn chip(kind: AttachmentKind, path: &std::path::Path, name: &str) -> Attachment {
        Attachment {
            kind,
            path: path.display().to_string(),
            media_type: String::new(),
            bytes_len: 0,
            name: name.to_string(),
        }
    }

    /// A picture is handed over as a picture - a `data:` URI, the one form the server takes
    /// with the mime sniffed from the bytes.
    #[test]
    fn an_image_chip_becomes_an_image_part() {
        let d = dir("image");
        let p = d.join("shot.png");
        std::fs::write(&p, png()).unwrap();
        let (parts, refused) = parts(&[chip(AttachmentKind::Image, &p, "shot.png")]);
        assert!(refused.is_empty(), "{refused:?}");
        let UserPart::Image {
            media_type,
            data_ref,
        } = &parts[0]
        else {
            panic!("a png is not an image part: {parts:?}")
        };
        assert_eq!(media_type, "image/png");
        assert!(
            data_ref.starts_with("data:image/png;base64,"),
            "the form a bare path is refused in: {data_ref}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A type the head called a file, and the bytes say otherwise, is a picture. The name is
    /// a claim and the magic number is not - the rule `read` already keeps.
    #[test]
    fn the_bytes_decide_the_kind_and_not_the_chip() {
        let d = dir("misnamed");
        let p = d.join("really-a-picture.txt");
        std::fs::write(&p, png()).unwrap();
        let (parts, _) = parts(&[chip(AttachmentKind::File, &p, "really-a-picture.txt")]);
        assert!(
            matches!(&parts[0], UserPart::Image { media_type, .. } if media_type == "image/png"),
            "{parts:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// BESIDE the words, and never in front of them. The operator's sentence is the part a
    /// head retires its own echo by, so what an attachment adds goes after it - the property
    /// `App::retire_pending` and `row_words` both read.
    #[test]
    fn a_text_file_is_named_and_inlined_after_the_words() {
        let d = dir("text");
        let p = d.join("report.csv");
        std::fs::write(&p, "region,total\nemea,3\n").unwrap();
        let (extra, refused) = parts(&[chip(AttachmentKind::File, &p, "report.csv")]);
        assert!(refused.is_empty(), "{refused:?}");
        let mut all = vec![UserPart::Text {
            text: "[file 1 · report.csv · 0 B] what is this?".into(),
        }];
        all.extend(extra.clone());
        assert!(
            matches!(&all[0], UserPart::Text { text } if text.contains("what is this?")),
            "the words stay whole and first: {all:?}"
        );
        assert!(
            matches!(&extra[0], UserPart::FileRef { path, sha256 } if path == &p.display().to_string() && sha256.len() == 64),
            "a text file is also a path the model may read itself: {extra:?}"
        );
        let UserPart::Text { text } = &extra[1] else {
            panic!("the content is not a part: {extra:?}")
        };
        assert!(
            text.contains("report.csv"),
            "the header names the file: {text}"
        );
        assert!(text.contains("emea,3"), "and the content is there: {text}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Bytes that are not text are a path, never a lossy decode - a shard shown as U+FFFD is
    /// a file the model will conclude it cannot read.
    #[test]
    fn a_binary_that_is_not_an_image_is_only_a_file_ref() {
        let d = dir("binary");
        let p = d.join("model.bin");
        std::fs::write(&p, b"\xff\xfe\x00\x01not text at all\x80").unwrap();
        let (extra, refused) = parts(&[chip(AttachmentKind::File, &p, "model.bin")]);
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(extra.len(), 1, "one part and no inlined text: {extra:?}");
        assert!(matches!(&extra[0], UserPart::FileRef { .. }), "{extra:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A file that is gone is said, not skipped. The prompt still sends - the operator wrote
    /// a sentence - and both the model and the operator are told which chip did not arrive.
    #[test]
    fn a_missing_file_is_a_sentence_and_not_an_empty_attachment() {
        let d = dir("gone");
        let p = d.join("never-existed.csv");
        let (extra, refused) = parts(&[chip(AttachmentKind::File, &p, "never-existed.csv")]);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(extra.len(), 1);
        let UserPart::Text { text } = &extra[0] else {
            panic!("a refusal is a sentence: {extra:?}")
        };
        assert!(text.contains("never-existed.csv"), "{text}");
        assert!(text.contains("could not be read"), "{text}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A huge text file is a path and not an inline - the cap is what keeps one attachment
    /// from spending the prompt's budget.
    #[test]
    fn a_text_file_over_the_cap_is_not_inlined() {
        let d = dir("big");
        let p = d.join("huge.log");
        let body = "x".repeat((MAX_INLINE_TEXT + 16) as usize);
        std::fs::write(&p, body).unwrap();
        let (extra, refused) = parts(&[chip(AttachmentKind::File, &p, "huge.log")]);
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(
            extra.len(),
            1,
            "a file over the cap is a path and nothing else: {} parts",
            extra.len()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The door the daemon actually reads a mid-turn prompt through - `HubSteering` - turns
    /// the frame's attachment into a part, keeps the operator's words first and whole, and
    /// binds them to `Speaker::Operator`.
    #[test]
    fn a_prompt_queued_mid_turn_reaches_the_engine_with_its_picture() {
        use letibot_sessionlog::hub::Hub;
        use letibot_sessionlog::protocol::Caps;
        use letibot_turn::SteeringSource;

        let d = dir("mid-turn");
        let p = d.join("shot.png");
        std::fs::write(&p, png()).unwrap();
        let hub = Hub::new("s-attach");
        let head = hub.attach("tui", "dead", Caps::default(), 0);
        hub.submit(
            &head.head_id,
            "c1",
            0,
            letibot_sessionlog::CommandKind::Prompt {
                text: "what is in this picture?".into(),
                attachments: vec![chip(AttachmentKind::Image, &p, "shot.png")],
                context: None,
            },
        );

        let mut steering = crate::harness::HubSteering::new(hub.clone());
        let m = steering.try_next().expect("the prompt is steering");
        assert!(m.from_operator, "the chips rode somebody else's message");
        let item = m.to_item();
        let letibot_transcript::TranscriptItem::User { speaker, parts } = &item else {
            panic!("a prompt is a user row: {item:?}")
        };
        assert_eq!(*speaker, letibot_transcript::Speaker::Operator);
        let letibot_transcript::UserPart::Text { text } = &parts[0] else {
            panic!("the words must be the FIRST part: {parts:?}")
        };
        assert_eq!(text, "what is in this picture?", "the words stay whole");
        assert!(
            matches!(&parts[1], letibot_transcript::UserPart::Image { media_type, data_ref }
                if media_type == "image/png" && data_ref.starts_with("data:image/png;base64,")),
            "the picture did not reach the engine: {parts:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A file that vanished between the frame and the turn is said on the session's log, and
    /// the operator's words still go through - the same rule the head keeps one layer up.
    #[test]
    fn a_chip_whose_file_vanished_while_it_waited_is_warned_about() {
        use letibot_sessionlog::hub::Hub;
        use letibot_sessionlog::protocol::Caps;
        use letibot_turn::SteeringSource;

        let d = dir("mid-turn-gone");
        let p = d.join("never-there.png");
        let hub = Hub::new("s-attach-gone");
        let head = hub.attach("tui", "dead", Caps::default(), 0);
        hub.submit(
            &head.head_id,
            "c1",
            0,
            letibot_sessionlog::CommandKind::Prompt {
                text: "look".into(),
                attachments: vec![chip(AttachmentKind::Image, &p, "never-there.png")],
                context: None,
            },
        );
        let mut steering = crate::harness::HubSteering::new(hub.clone());
        let m = steering.try_next().expect("the prompt is steering");
        assert!(
            hub.retained().iter().any(|e| matches!(
                &e.event,
                letibot_sessionlog::SessionEvent::Warning { code, detail, .. }
                    if code == "attachment_refused" && detail.contains("never-there.png")
            )),
            "nothing on the log says the picture was not attached"
        );
        let item = m.to_item();
        let letibot_transcript::TranscriptItem::User { parts, .. } = &item else {
            panic!("a prompt is a user row")
        };
        assert_eq!(parts.len(), 2, "a sentence beside the words: {parts:?}");
        assert!(
            matches!(&parts[1], letibot_transcript::UserPart::Text { text }
                if text.contains("never-there.png")),
            "the model is not told which chip did not arrive: {parts:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
