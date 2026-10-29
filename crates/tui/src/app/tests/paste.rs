//! **Pasting pictures and files into the composer** — `PROTOCOL_VERSION` 50.
//!
//! The operator: *"how i can paste an image to letibot?"*. These tests are the three doors stated
//! as behaviour: the clipboard, a drag-and-drop paste of paths, and the chip going out with the
//! message — plus the two refusals that keep it honest (a paste that is not files stays text, and a
//! chip whose file is gone is not sent empty).
//!
//! **Nothing here reads the real clipboard.** `app()` starts on the fake
//! (`app::attachments::default_clipboard`), and a test that wants a picture sets one with
//! `a.set_clipboard(FakeClipboard(..))`.

use super::*;
use crate::app::attachments::{FakeClipboard, chip_text};
use letibot_sessionlog::protocol::AttachmentKind;

/// A directory of this test's own, named for the case so a rerun does not read the last one's
/// files.
fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "letibot-paste-tui-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&d).expect("a temp dir");
    d
}

/// A **real** PNG, 2×2 — `letibot_transcript::testing`'s, because a fixture that only *looks* like
/// an image is what this module's incident was made of: a header-only PNG went in as a chip, was
/// sent, was stored, and a provider refused every turn that carried it.
fn png() -> Vec<u8> {
    letibot_transcript::testing::png(2, 2)
}

/// **The bytes the incident wrote** — a signature, an `IHDR`, and no picture.
fn png_header_only() -> Vec<u8> {
    letibot_transcript::testing::png_header_only(640, 480)
}

/// A head with a picture on the fake clipboard AND a directory of its own to write it in.
///
/// **The directory is not optional.** Without it the head writes into the operator's real paste
/// directory — `$TMPDIR/letibot-<uid>/attachments`, with `session_id: ""` making the name `-1.png`
/// — which is exactly what happened on 2026-10-30 and what `set_attachments_dir` exists to stop.
fn a_picture_with_clipboard(tag: &str) -> App {
    let mut a = app();
    a.set_attachments_dir(dir(tag));
    a.set_clipboard(FakeClipboard(Ok(Some(png()))));
    a
}

/// **Ctrl-V takes the picture off the clipboard and puts a chip in the composer.**
#[test]
fn ctrl_v_with_an_image_on_the_clipboard_makes_a_chip() {
    let mut a = a_picture_with_clipboard("ctrl-v");
    a.key(Key::CtrlV);
    let input = a.input().to_string();
    assert!(
        input.starts_with("[image 1 · png · "),
        "no chip in the composer: {input:?}"
    );
    assert_eq!(a.chips.len(), 1, "one chip in the ledger");
    assert_eq!(a.chips[0].attachment.kind, AttachmentKind::Image);
    assert!(
        std::path::Path::new(&a.chips[0].attachment.path).is_file(),
        "the chip points at nothing: {}",
        a.chips[0].attachment.path
    );
    // The bytes on disk are the picture, not its base64 or a truncated copy.
    let written = std::fs::read(&a.chips[0].attachment.path).expect("the saved picture");
    assert_eq!(written, png());
}

/// **The terminal's paste key with an image** arrives as an EMPTY bracketed paste — there is no
/// text on the clipboard — and it is the same act as ctrl-v.
#[test]
fn an_empty_paste_with_an_image_on_the_clipboard_makes_a_chip() {
    let mut a = a_picture_with_clipboard("empty-paste");
    a.key(Key::Paste(String::new()));
    assert_eq!(a.chips.len(), 1, "{:?}", a.input());
    assert!(
        a.input().starts_with("[image 1 · png · "),
        "{:?}",
        a.input()
    );
}

/// **A clipboard that hands over a PNG header is refused, not chipped.**
///
/// These are the incident's bytes — 33 of them, a signature and an `IHDR` — and the whole chain
/// started with a head chipping them. A chip is a promise that the file is a picture, and this one
/// is not; saying so here is the only place in the chain where the person can paste again.
#[test]
fn ctrl_v_with_a_png_header_attaches_nothing_and_says_so() {
    let mut a = app();
    let where_it_would_go = dir("header-only");
    a.set_attachments_dir(where_it_would_go.clone());
    a.set_clipboard(FakeClipboard(Ok(Some(png_header_only()))));
    a.key(Key::CtrlV);
    assert!(a.chips.is_empty(), "a header was chipped: {:?}", a.input());
    assert_eq!(a.input(), "", "something went into the composer");
    let said = a.notice.clone().unwrap_or_default();
    assert!(
        said.contains("not a complete PNG"),
        "no sentence said why: {said}"
    );
    // And nothing was written either — the bytes are refused BEFORE the file, so a refused paste
    // leaves no `-1.png` for `read` to find later, which is how the incident began.
    let wrote = std::fs::read_dir(&where_it_would_go)
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(wrote, 0, "the refused bytes left a file behind");
    let _ = std::fs::remove_dir_all(&where_it_would_go);
}

/// **No head a test builds can name the operator's paste directory.**
///
/// MEASURED 2026-10-30: a `letibot-tui` unit test pasted a fake screenshot and the head wrote it
/// into `$TMPDIR/letibot-<uid>/attachments/-1.png` — the operator's own directory, because
/// `App::new` starts with `session_id: ""`. `read` then called that file an image, and the session
/// it was read in was dead for good.
///
/// So this is the guard the fix is held to: the directory a TEST build resolves is not the
/// operator's, and a head that has not been given one still does not get the operator's.
#[test]
fn a_test_build_cannot_name_the_operators_attachment_directory() {
    let operators = crate::app::panes::head_runtime_dir().join("attachments");
    let mine = crate::app::attachments::attachments_dir();
    assert_ne!(mine, operators, "a test build resolved the operator's own");
    assert!(
        !mine.starts_with(crate::app::panes::head_runtime_dir()),
        "a test build resolved something inside the operator's runtime directory: {}",
        mine.display()
    );
    // And the head a test builds without saying anything lands there too, not on the operator's.
    let a = app();
    assert!(
        a.attachments_dir.is_none(),
        "the default is the helper's answer, not a field a test had to remember to set"
    );
}

/// **And with no picture on the clipboard ctrl-v is the window it always was** — the regression
/// this feature must not cause. `app()`'s fake holds nothing, which is the ordinary case.
#[test]
fn ctrl_v_with_no_image_still_opens_the_payload_window() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    let long: String = (0..40).map(|i| format!("line {i}\n")).collect();
    bang_rows(&mut a, 2, "i1", "! cat the-log", &long);
    a.screen(100, 30);
    a.key(Key::CtrlV);
    assert!(
        a.payload_sel.is_some(),
        "the key stopped opening a long result's window"
    );
}

/// **A bracketed paste that is exactly a file path becomes a chip** — the drag-and-drop door.
#[test]
fn a_pasted_path_is_a_chip_and_not_text() {
    let d = dir("one-path");
    let p = d.join("report.csv");
    std::fs::write(&p, "region,total\nemea,3\n").unwrap();
    let mut a = app();
    a.key(Key::Paste(p.display().to_string()));
    assert_eq!(a.chips.len(), 1, "{:?}", a.input());
    let input = a.input().to_string();
    assert!(input.starts_with("[file 1 · report.csv · "), "{input:?}");
    assert!(
        !input.contains(&p.display().to_string()),
        "the path was inserted as text as well: {input:?}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **Several files at once, with an escaped space** — what dragging three files into a terminal
/// pastes.
#[test]
fn a_paste_of_several_paths_makes_several_chips() {
    let d = dir("several");
    let one = d.join("one file.txt");
    let two = d.join("shot.png");
    std::fs::write(&one, "hello").unwrap();
    std::fs::write(&two, png()).unwrap();
    let pasted = format!(
        "{} {}",
        one.display().to_string().replace(' ', "\\ "),
        two.display()
    );
    let mut a = app();
    a.key(Key::Paste(pasted));
    assert_eq!(a.chips.len(), 2, "{:?}", a.input());
    assert_eq!(a.chips[0].attachment.kind, AttachmentKind::File);
    assert_eq!(a.chips[1].attachment.kind, AttachmentKind::Image);
    assert!(
        a.input().contains("[file 1 · one file.txt · "),
        "{:?}",
        a.input()
    );
    assert!(a.input().contains("[image 2 · png · "), "{:?}", a.input());
    let _ = std::fs::remove_dir_all(&d);
}

/// **A paste that is not files stays text** — the rule that keeps a sentence from being rewritten.
#[test]
fn a_paste_of_words_is_still_words() {
    let mut a = app();
    a.key(Key::Paste("what does README.md say about retries?".into()));
    assert!(a.chips.is_empty(), "{:?}", a.input());
    assert_eq!(a.input(), "what does README.md say about retries?");
}

/// **Backspace takes the chip whole**, one press, and leaves the sentence around it.
#[test]
fn backspace_deletes_a_chip_as_one_unit() {
    let d = dir("backspace");
    let p = d.join("notes.txt");
    std::fs::write(&p, "some notes").unwrap();
    let mut a = app();
    typed(&mut a, "read this ");
    a.key(Key::Paste(p.display().to_string()));
    assert!(
        a.input().ends_with("[file 1 · notes.txt · 10 B] "),
        "{:?}",
        a.input()
    );
    a.key(Key::Backspace);
    assert_eq!(a.input(), "read this", "the chip did not go in one press");
    assert!(a.chips.is_empty(), "the ledger kept a chip the text lost");
    let _ = std::fs::remove_dir_all(&d);
}

/// **Enter sends the words WITH the attachments**, and the words keep the chip the operator saw.
#[test]
fn the_prompt_is_sent_with_its_attachments() {
    let d = dir("send");
    let p = d.join("shot.png");
    std::fs::write(&p, png()).unwrap();
    let mut a = app();
    typed(&mut a, "what is in this picture?");
    a.key(Key::Paste(p.display().to_string()));
    let Some(Action::Prompt(prompt)) = a.key(Key::Enter) else {
        panic!("enter did not send: {:?}", a.input())
    };
    assert_eq!(prompt.attachments.len(), 1, "{prompt:?}");
    assert_eq!(prompt.attachments[0].kind, AttachmentKind::Image);
    assert!(
        prompt.text.contains("what is in this picture?"),
        "the operator's words are not in the message: {prompt}"
    );
    assert!(
        prompt.text.contains("[image 1 · png · "),
        "the chip the person saw is not in what they sent: {prompt}"
    );
    assert!(a.chips.is_empty(), "the ledger outlived the send");
    let _ = std::fs::remove_dir_all(&d);
}

/// **A chip whose file is gone is refused, and the line is kept.** Nothing is sent empty, and
/// nothing is sent as though the picture had gone with it.
#[test]
fn a_chip_whose_file_vanished_is_refused_and_nothing_is_sent() {
    let d = dir("gone");
    let p = d.join("vanishing.png");
    std::fs::write(&p, png()).unwrap();
    let mut a = app();
    typed(&mut a, "look at this");
    a.key(Key::Paste(p.display().to_string()));
    let before = a.input().to_string();
    assert!(before.contains("[image 1 · png · "), "{before:?}");
    std::fs::remove_file(&p).unwrap();
    a.chips[0].attachment.path = p.display().to_string();
    assert_eq!(a.key(Key::Enter), None, "a dead chip was sent anyway");
    assert_eq!(a.input(), before, "the operator's line was lost");
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("is gone"), "no sentence said why: {said}");
    let _ = std::fs::remove_dir_all(&d);
}

/// **The user row in the transcript shows the chips** — the operator's own words, with the chip in
/// them, and no second marker for the part beside it and no bytes.
#[test]
fn the_transcript_row_draws_the_chips_and_not_the_bytes() {
    let mut a = app();
    let chip = chip_text(
        1,
        &letibot_sessionlog::Attachment {
            kind: AttachmentKind::Image,
            path: "/tmp/shot.png".into(),
            media_type: "image/png".into(),
            bytes_len: 94_208,
            name: "shot.png".into(),
        },
    );
    a.apply(ServerFrame::Event(env(1, testing::appended("i1", "user"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "i1".into(),
            item: Box::new(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![
                    UserPart::Text {
                        text: format!("what is this? {chip}"),
                    },
                    UserPart::Image {
                        media_type: "image/png".into(),
                        data_ref: "data:image/png;base64,AAAA".into(),
                    },
                ],
            }),
        },
    )));
    let frame = a.screen(100, 30).join("\n");
    assert!(frame.contains("what is this?"), "{frame}");
    assert!(frame.contains(&chip), "the chip is not on the row: {frame}");
    assert!(
        !frame.contains("[image image/png]"),
        "the row named the picture twice, in two shapes: {frame}"
    );
    assert!(!frame.contains("base64"), "the row drew the bytes: {frame}");
}
