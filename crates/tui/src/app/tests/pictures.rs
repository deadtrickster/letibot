//! **A pasted picture, drawn** — the operator, from a fresh session with an image pasted and sent:
//! *"pasted, sent, model started toolcalling (yellow counter), clicked on the tools counter —
//! empty panel, Esc brings me back. image doesnt render"*.
//!
//! Two halves: the thumbnail under the chip (and the terminal's own gate on it), and the panel a
//! click on the tool-call counter opens **while the calls are still running**.

use super::*;

/// A PNG-shaped reference: signature, then a real IHDR carrying `w` and `h`. The bytes need not
/// decode — nothing under test decodes a PNG; the head reads the header and hands the rest to the
/// terminal, exactly as it does for a real paste.
fn png_data_ref(w: u32, h: u32) -> String {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 2, 0, 0, 0]);
    v.extend_from_slice(&[0, 0, 0, 0]);
    format!(
        "data:image/png;base64,{}",
        letibot_transcript::media::encode_base64(&v)
    )
}

/// The placeholder a kitty terminal replaces with a slice of the picture.
const PLACEHOLDER: char = '\u{10EEEE}';

/// **The operator's row as the daemon stores it**: their words with the chip in them, and the
/// picture's bytes beside them as an `Image` part (protocol 50).
fn a_prompt_with_a_picture() -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::appended("s.0", "user"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![
                    UserPart::Text {
                        text: "what is this? [image 1 · png · 92 KiB]".into(),
                    },
                    UserPart::Image {
                        media_type: "image/png".into(),
                        data_ref: png_data_ref(320, 200),
                    },
                ],
            }),
        },
    )));
    a
}

/// Where `PLACEHOLDER` appears, as screen rows.
fn picture_rows_on(screen: &[String]) -> Vec<usize> {
    screen
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(PLACEHOLDER))
        .map(|(y, _)| y)
        .collect()
}

#[test]
fn a_picture_under_the_chip_is_drawn_where_the_terminal_can_fill_it() {
    let mut a = a_prompt_with_a_picture();
    a.set_proto(rano::term::Proto::Kitty);
    let screen = a.screen(100, 30);
    let rows = picture_rows_on(&screen);
    assert!(!rows.is_empty(), "no thumbnail:\n{}", screen.join("\n"));
    // **Under the chip**, which is on the operator's own line: the thumbnail's first row is below
    // it and the rows are contiguous.
    let words = screen
        .iter()
        .position(|l| l.contains("what is this?"))
        .expect("the operator's row");
    assert!(rows[0] > words, "the picture is above the words: {rows:?}");
    assert_eq!(
        rows.windows(2).filter(|w| w[1] != w[0] + 1).count(),
        0,
        "the rows are not contiguous: {rows:?}"
    );
    // **Its own aspect, kept**: 320×200 in a box half of 100 columns wide and never under 20.
    let (cols, want_rows) = rano::term::graphics::image_cells(
        Some(320),
        Some(200),
        rano::term::graphics::image_box(a.cfg.width),
    );
    assert_eq!(rows.len(), want_rows as usize, "{rows:?}");
    assert_eq!(
        cols,
        rano::term::graphics::image_box(a.cfg.width),
        "half the frame's inner width, which is what the upload placed"
    );

    // **And the bytes went out, once**, under the id the placeholders name.
    let uploads = a.take_image_uploads();
    // The image is the SECOND part — the chip's words are the first.
    let id = rano::term::graphics::image_id(&format!("s.0#{}1", crate::ui::render::USER_PART));
    assert_eq!(uploads.len(), 2, "an upload and a placement");
    let reference = png_data_ref(320, 200);
    assert_eq!(
        uploads[0],
        rano::term::graphics::image_upload(id, letibot_transcript::media::wire_payload(&reference)),
        "the upload is the picture's bytes under the row's id"
    );
    assert_eq!(
        uploads[1],
        rano::term::graphics::image_place(id, cols, want_rows),
        "the placement is the box the rows draw"
    );
    // And a frame that changed nothing sends nothing: the id is in the terminal already.
    let _ = a.screen(100, 30);
    assert!(a.take_image_uploads().is_empty(), "the bytes went twice");
}

#[test]
fn a_terminal_without_pictures_draws_the_chip_alone() {
    let mut a = a_prompt_with_a_picture();
    // Nothing decided: no protocol, which is what a test, a pipe and Terminal.app all are.
    let screen = a.screen(100, 30);
    assert!(
        screen.iter().any(|l| l.contains("what is this?")),
        "{}",
        screen.join("\n")
    );
    assert_eq!(
        screen
            .iter()
            .filter(|l| l.contains("[image 1 · png · 92 KiB]"))
            .count(),
        1,
        "the chip is the picture, once"
    );
    assert!(
        picture_rows_on(&screen).is_empty(),
        "placeholders on a terminal that cannot fill them:\n{}",
        screen.join("\n")
    );
    assert!(a.take_image_uploads().is_empty(), "bytes were sent anyway");
}

#[test]
fn enter_opens_the_picture_larger_and_esc_deletes_it() {
    let mut a = a_prompt_with_a_picture();
    a.set_proto(rano::term::Proto::Kitty);
    a.screen(100, 30);
    let _ = a.take_image_uploads();
    // **Enter on an empty composer, over a picture the frame drew.**
    assert_eq!(a.editor.text(), "");
    assert!(!a.picture_rows.is_empty(), "no picture on the screen");
    a.key(Key::Enter);
    let open = a.screen(100, 30).join("\n");
    assert!(
        matches!(
            a.diff_popup.as_ref().map(|p| &p.body),
            Some(crate::app::diff_popup::PopupBody::Picture { .. })
        ),
        "enter opened no picture:\n{open}"
    );
    // **Larger than the thumbnail**: the box the popup places is wider than half the frame, and
    // the bytes are the same picture under the same id — a placement, not a second upload.
    let uploads = a.take_image_uploads();
    assert_eq!(uploads.len(), 1, "one placement: {uploads:?}");
    let placed = String::from_utf8_lossy(&uploads[0]).to_string();
    let cols: u32 = placed
        .split("c=")
        .nth(1)
        .and_then(|r| r.split(',').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let thumb = rano::term::graphics::image_box(a.cfg.width);
    assert!(
        cols > thumb,
        "the picture opened at {cols} columns, no wider than the {thumb} it was already drawn at: {placed}"
    );
    assert!(
        String::from_utf8_lossy(&uploads[0]).contains("a=p"),
        "the picture was uploaded again rather than placed: {:?}",
        String::from_utf8_lossy(&uploads[0])
    );
    // **Esc closes it, and the terminal is told to free the bytes** — a picture nothing draws is
    // memory with no owner.
    a.key(Key::Esc);
    assert!(a.diff_popup.is_none(), "esc left the picture open");
    let after = a.take_image_uploads();
    assert!(
        after
            .iter()
            .any(|b| String::from_utf8_lossy(b).contains("a=d,d=I")),
        "nothing was deleted: {after:?}"
    );
}

#[test]
fn a_click_on_the_thumbnail_opens_the_same_picture() {
    let mut a = a_prompt_with_a_picture();
    a.set_proto(rano::term::Proto::Kitty);
    let screen = a.screen(100, 30);
    let y = *picture_rows_on(&screen).first().expect("a thumbnail row");
    a.key(Key::Click { x: 3, y: y as u16 });
    assert!(
        matches!(
            a.diff_popup.as_ref().map(|p| &p.body),
            Some(crate::app::diff_popup::PopupBody::Picture { .. })
        ),
        "the click opened nothing"
    );
}

/// **The frame's own record decides what a click opens** — a row of the operator's words is not a
/// picture's row, so a click there still falls through to the conversation.
#[test]
fn the_operators_words_are_not_the_picture() {
    let mut a = a_prompt_with_a_picture();
    a.set_proto(rano::term::Proto::Kitty);
    let screen = a.screen(100, 30);
    let words = screen
        .iter()
        .position(|l| l.contains("what is this?"))
        .expect("the operator's row");
    a.key(Key::Click {
        x: 3,
        y: words as u16,
    });
    assert!(
        a.diff_popup.is_none(),
        "a click on the operator's words opened the picture"
    );
}

/// **A picture behind tmux crosses as the wrapped form** — a kitty sequence inside a multiplexer
/// goes as a DCS payload with its escapes doubled, and an unwrapped one is swallowed with nothing
/// said. The one place that decides is `Proto::wrap`, and every upload this head makes passes
/// through it.
#[test]
fn behind_tmux_a_picture_is_uploaded_as_the_wrapped_form() {
    let mut a = a_prompt_with_a_picture();
    a.set_proto(rano::term::Proto::KittyPassthrough);
    let screen = a.screen(100, 30);
    assert!(
        !picture_rows_on(&screen).is_empty(),
        "the placeholders are ordinary text and cross tmux as they are"
    );
    let uploads = a.take_image_uploads();
    assert_eq!(uploads.len(), 2);
    for cmd in &uploads {
        assert!(
            cmd.starts_with(b"\x1bPtmux;") && cmd.ends_with(b"\x1b\\"),
            "not the wrapped form: {:?}",
            String::from_utf8_lossy(cmd)
        );
    }
}
