//! Notifications, the progress bar, the clipboard, images, light/dark.

use super::*;

/// **A PNG a tool returned is drawn inline where the terminal can, and uploaded once** — at
/// the size this window's width gives, and placed again at a new size when that changes.
/// The rows are placeholders in the image's id colour — ordinary text to the width count
/// and the diff — and the upload is the protocol's transmit plus a virtual placement of the
/// same size the rows draw.
#[test]
fn a_png_result_is_drawn_inline_and_uploaded_once() {
    let media =
        letibot_transcript::media::Media::of("shot.png", &png_header(200, 100)).expect("a png");
    let row = |a: &mut App| {
        a.apply(ServerFrame::Event(env(
            1,
            testing::appended("r.img", "tool_result"),
        )));
        a.record_item(
            "r.img",
            TranscriptItem::ToolResult {
                call_id: "c0".into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "image/png 200×100".into(),
                edit: None,
                origin: None,
                media: Some(media.clone()),
            },
        );
    };

    let mut off = app();
    row(&mut off);
    assert_eq!(
        placeholders(&off.screen(100, 40)),
        0,
        "no feature, no placeholders"
    );
    assert!(off.take_image_uploads().is_empty());

    let mut a = app();
    a.set_features(rano::term::Features::ALL);
    row(&mut a);
    let screen = a.screen(160, 40);
    let id = rano::term::graphics::image_id("r.img");
    let box_cols = rano::term::graphics::image_box(a.cfg.width);
    let (cols, rows) = rano::term::graphics::image_cells(Some(200), Some(100), box_cols);
    assert_eq!(cols, box_cols, "as wide as the box");
    assert_eq!(rows, box_cols.div_ceil(4), "a 2:1 picture in 2:1 cells");
    assert_eq!(placeholders(&screen), rows as usize, "{screen:#?}");
    let drawn = screen.iter().find(|l| l.contains('\u{10EEEE}')).unwrap();
    assert!(drawn.contains(&format!(
        "38;2;{};{};{}m",
        (id >> 16) & 255,
        (id >> 8) & 255,
        id & 255
    )));
    let ups = a.take_image_uploads();
    assert_eq!(ups.len(), 2, "the bytes, then the placement");
    let up = String::from_utf8_lossy(&ups[0]);
    assert!(
        up.starts_with(&format!("\x1b_Ga=t,f=100,i={id},q=2,m=0;")),
        "{up}"
    );
    assert_eq!(ups[1], rano::term::graphics::image_place(id, cols, rows));
    a.screen(160, 40);
    assert!(
        a.take_image_uploads().is_empty(),
        "uploaded once, not every frame"
    );

    // **A resize places it again at the new size**, without sending the bytes again — and
    // the rows drawn at the new width are the new placement's.
    let narrow = a.screen(60, 40);
    let box2 = rano::term::graphics::image_box(a.cfg.width);
    assert_ne!(
        box2, box_cols,
        "the fixture has to change the box to show anything"
    );
    let (c2, r2) = rano::term::graphics::image_cells(Some(200), Some(100), box2);
    assert_eq!(
        a.take_image_uploads(),
        vec![rano::term::graphics::image_place(id, c2, r2)]
    );
    assert_eq!(placeholders(&narrow), r2 as usize);
}

/// **A PNG the reply's markdown names is drawn at the reference** — the operator's own first
/// try: a model wrote `/Users/dead/sunset.png` and said `![sunset](…)`, and the head drew
/// the alt text; then, drawn, it sat below the reply's last paragraph rather than under the
/// line that named it. And a row whose content arrives a frame after it was appended still
/// gets its picture: the upload pass waits for it rather than walking past.
#[test]
fn a_png_the_reply_names_is_drawn_at_the_reference() {
    let dir = std::env::temp_dir().join(format!(
        "lb-mdimg-{}-{}",
        std::process::id(),
        letibot_sessionlog::event::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sunset.png");
    std::fs::write(&file, png_header(400, 100)).unwrap();

    let mut a = app();
    a.set_features(rano::term::Features::ALL);
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("a.img", "assistant"),
    )));
    a.screen(100, 40);
    assert!(
        a.take_image_uploads().is_empty(),
        "no content yet, nothing to send"
    );
    a.record_item(
        "a.img",
        TranscriptItem::Assistant {
            text: format!(
                "Done.\n\n```\n![sunset over mountains]({})\n```\n\nThe last paragraph.\n",
                file.display()
            ),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    a.screen(100, 40);
    assert_eq!(
        a.take_image_uploads().len(),
        2,
        "the content arrived after the row: still uploaded"
    );
    let screen = a.screen(100, 40);
    let (_, rows) = rano::term::graphics::image_cells(
        Some(400),
        Some(100),
        rano::term::graphics::image_box(a.cfg.width),
    );
    assert_eq!(placeholders(&screen), rows as usize, "{screen:#?}");
    let first = screen
        .iter()
        .position(|l| l.contains('\u{10EEEE}'))
        .unwrap();
    let reference = screen
        .iter()
        .position(|l| l.contains("![sunset over mountains]"))
        .expect("the reference is drawn");
    assert!(
        reference < first && screen[first - 1].contains('└'),
        "under the frame its reference is in:\n{screen:#?}"
    );
    let last_para = screen
        .iter()
        .position(|l| l.contains("The last paragraph."))
        .expect("the reply");
    assert!(
        first < last_para,
        "under its reference, above what follows:\n{screen:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // A URL is not fetched, and a title is not part of the path.
    assert_eq!(
        crate::ui::render::markdown_images("![a](https://x/y.png) ![b](nope.png \"t\")"),
        vec![("b".to_string(), "nope.png".to_string())]
    );
}

/// **`/copy` takes the open window's output, else the last reply** — and says so when the
/// terminal is not known to take OSC 52 rather than pretending.
#[test]
fn copy_takes_the_open_window_else_the_last_reply() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("a.0", "assistant"),
    )));
    a.record_item(
        "a.0",
        TranscriptItem::Assistant {
            text: "the answer".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    assert_eq!(a.command("copy"), None);
    assert_eq!(a.take_clipboard(), None, "no OSC 52, nothing written");

    a.set_features(rano::term::Features::ALL);
    a.command("copy");
    assert_eq!(a.take_clipboard().as_deref(), Some("the answer"));

    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("r.0", "tool_result"),
    )));
    a.record_item(
        "r.0",
        TranscriptItem::ToolResult {
            call_id: "c0".into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "line 1\nline 2\n".into(),
            edit: None,
            origin: None,
            media: None,
        },
    );
    a.payload_sel = Some("r.0".into());
    a.command("copy");
    assert_eq!(a.take_clipboard().as_deref(), Some("line 1\nline 2\n"));
}

/// **The tab says what the session is doing, and a person who is away is told when it
/// needs them.** Progress follows the state: busy while the turn works, waiting (the paused
/// colour) while a card is open, idle after. A notification is an edge — the card arriving,
/// the turn ending — and only for a window known to be unfocused.
#[test]
fn progress_follows_the_turn_and_notifications_go_to_an_absent_reader() {
    use rano::term::Progress;
    let mut a = app();
    a.set_features(rano::term::Features::ALL);
    assert_eq!(a.take_notification(), None, "the first look is a baseline");
    assert_eq!(a.progress(), Progress::Idle);

    a.key(Key::FocusOut);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    assert_eq!(a.progress(), Progress::Busy);
    assert_eq!(a.take_notification(), None, "starting work needs nobody");

    a.apply(ServerFrame::Event(env(
        2,
        testing::requested("r1", "run cargo test"),
    )));
    assert_eq!(a.progress(), Progress::Waiting, "a card outranks busy");
    let n = a
        .take_notification()
        .expect("a card for somebody who is away");
    assert!(
        n.contains("permission needed") && n.contains("run cargo test"),
        "{n}"
    );
    assert_eq!(a.take_notification(), None, "said once, not every tick");

    a.apply(ServerFrame::Event(env(3, testing::answered("r1", "allow"))));
    a.apply(ServerFrame::Event(env(4, testing::turn_finished("t1"))));
    assert_eq!(a.progress(), Progress::Idle, "{:?}", a.progress());
    let n = a
        .take_notification()
        .expect("the turn ended while they were away");
    assert!(n.ends_with(": done"), "{n}");

    // Looking at the window: the same edges say nothing.
    a.key(Key::FocusIn);
    a.apply(ServerFrame::Event(env(5, testing::turn_started("t2"))));
    a.apply(ServerFrame::Event(env(
        6,
        testing::requested("r2", "write a file"),
    )));
    assert_eq!(
        a.take_notification(),
        None,
        "a focused reader is not notified"
    );

    // And a terminal without the feature never is.
    let mut b = app();
    b.take_notification();
    b.key(Key::FocusOut);
    b.apply(ServerFrame::Event(env(1, testing::requested("r1", "x"))));
    assert_eq!(b.take_notification(), None);
}
