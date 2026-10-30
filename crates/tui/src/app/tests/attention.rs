//! Notifications, the progress bar, the clipboard, images, light/dark.

use super::*;

/// **A PNG a tool returned is drawn inline where the terminal can, and uploaded once** — at
/// the size this window's width gives, and placed again at a new size when that changes.
/// The rows are placeholders in the image's id colour — ordinary text to the width count
/// and the diff — and the upload is the protocol's transmit plus a virtual placement of the
/// same size the rows draw.
#[test]
fn a_png_result_is_drawn_inline_and_uploaded_once() {
    // **Small enough to be ONE chunk of the kitty transmit** (rano cuts the base64 at 4096 bytes),
    // so this test is about the drawing and not about rano's chunking — and 8×4 is the same 2:1
    // box a 200×100 screenshot is, which is the arithmetic the assertions below are about.
    let media = letibot_transcript::media::Media::of("shot.png", &png_header(8, 4)).expect("a png");
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
                payload: "image/png 8×4".into(),
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
    let (cols, rows) = rano::term::graphics::image_cells(Some(8), Some(4), box_cols);
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
    let (c2, r2) = rano::term::graphics::image_cells(Some(8), Some(4), box2);
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

// ──────────────────── the resident line a question draws ────────────────────
//
// The operator, on a card that would not go: *"A detach clears the card … a row that is completed
// takes its question with it"*. The question's life is the ROW's (`TodoNeed::Operator`), and the
// head reads it from two places: the daemon's `asks` settings row — the durable half, for a head
// that attached after the question was asked — and the board itself. The board is the truth from
// the moment it is read; the settings row covers only the window before that.

use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoNeed, TodoStatus};

/// A row that asks the operator a question, with the two choices `todo_write` insists on.
fn asking(content: &str, question: &str) -> TodoEntry {
    TodoEntry {
        content: content.into(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: vec![TodoNeed::Operator {
            question: question.into(),
            options: vec!["one".into(), "two".into()],
        }],
    }
}

/// The same row with its wait taken off — what `todo_write`'s `detach` leaves on the board.
fn taken_back(content: &str) -> TodoEntry {
    TodoEntry {
        content: content.into(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }
}

/// The daemon's `asks` settings row — `2 — "…" on \`…\``, the durable half of the line.
fn asks_setting(a: &mut App, value: &str) {
    a.apply(ServerFrame::Settings {
        rows: vec![letibot_sessionlog::protocol::SettingRow {
            key: "asks".into(),
            value: value.into(),
            source: String::new(),
            editable: String::new(),
            choices: Vec::new(),
            tools: Vec::new(),
        }],
    });
}

/// The board, as the daemon publishes it after a row's wait moved.
fn board(a: &mut App, todos: Vec<TodoEntry>) {
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos,
    });
}

/// The line, as the operator reads it.
fn line(a: &mut App) -> String {
    a.waiting_line(200).join("")
}

/// **A question the head has READ the board for does not outlive its row** — the defect the
/// operator watched sit on his screen through three attempts to put it down: `detach` took the
/// wait off the row, the board came back without it, and the line went on saying
/// `WAITING ON YOU: 1 — "…" on \`…\`` because the only copy it had left was the `asks` settings row
/// read at attach. A row that is completed is the same fact by another route.
#[test]
fn a_question_the_head_has_read_the_board_for_dies_with_its_row() {
    let mut a = app();
    a.session_id = "s1".into();
    // The attach: the durable half says a question waits, so the head reads the board.
    asks_setting(&mut a, "1 — \"which db?\" on `deploy`");
    board(&mut a, vec![asking("deploy", "which db?")]);
    assert!(
        line(&mut a).contains("\"which db?\" on `deploy`"),
        "{}",
        line(&mut a)
    );

    // The model takes the wait back: the same row, the question gone.
    board(&mut a, vec![taken_back("deploy")]);
    assert!(
        line(&mut a).is_empty(),
        "a detach clears the line on the very next frame: {:?}",
        line(&mut a)
    );

    // A row that leaves the board takes its question with it, and so does a completed one.
    board(&mut a, vec![asking("deploy", "which db?")]);
    assert!(!line(&mut a).is_empty());
    // **And alt-a says `nothing`, not `still being read`** — the settings row still quotes the
    // question, and telling the operator to try again would be the same stale sentence twice.
    board(&mut a, vec![taken_back("deploy")]);
    a.key(Key::AltA);
    let said = a.notice.clone().unwrap_or_default();
    assert!(
        said.contains("nothing is waiting on you"),
        "the board was read, so there is nothing left to wait for: {said:?}"
    );
    board(&mut a, Vec::new());
    assert!(line(&mut a).is_empty(), "the row left the board");
    let mut done = asking("deploy", "which db?");
    done.status = TodoStatus::Completed;
    board(&mut a, vec![done]);
    assert!(
        line(&mut a).is_empty(),
        "a completed row asks nobody anything"
    );

    // And nothing brings it back: a redraw, a whole re-render, `/status`.
    assert!(
        a.screen(200, 40)
            .join("\n")
            .find("WAITING ON YOU")
            .is_none()
    );
    a.redraw = true;
    assert!(
        a.screen(200, 40)
            .join("\n")
            .find("WAITING ON YOU")
            .is_none()
    );
    let report = a.status_lines(200).join("\n");
    assert!(!report.contains("WAITING ON YOU"), "{report}");
    assert!(line(&mut a).is_empty());
}

/// **A second row's question still draws while the first is gone — the COUNT, not just the
/// presence.** The line's `(+1 more)` is read from the board too: a head that counted the
/// settings row's number would keep saying two after one was taken back.
#[test]
fn the_line_counts_the_questions_the_board_still_holds() {
    let mut a = app();
    a.session_id = "s1".into();
    asks_setting(&mut a, "2 — \"which db?\" on `deploy`");
    board(
        &mut a,
        vec![
            asking("deploy", "which db?"),
            asking("cut the release", "which version?"),
        ],
    );
    let said = line(&mut a);
    assert!(said.contains("\"which db?\" on `deploy`"), "{said}");
    assert!(
        said.contains("(+1 more)"),
        "two questions, one named: {said}"
    );

    // The first row's wait is taken back; the second one is untouched.
    board(
        &mut a,
        vec![
            taken_back("deploy"),
            asking("cut the release", "which version?"),
        ],
    );
    let said = line(&mut a);
    assert!(
        said.contains("\"which version?\" on `cut the release`"),
        "{said}"
    );
    assert!(
        !said.contains("more"),
        "one question left, so nothing is counted after it: {said}"
    );
}

/// **Answering still works, and the answer still reaches the model** — the path that must not
/// move: alt-a opens the card for the row the board names, Enter sends `AnswerTodo` for that row
/// and question, and the line is gone in the same breath (the board's own copy is one frame
/// behind, and `todo_answered` holds the row out until it arrives).
#[test]
fn answering_retires_the_line_and_the_answer_still_leaves() {
    use letibot_sessionlog::question::QuestionAnswer;
    let mut a = app();
    a.session_id = "s1".into();
    asks_setting(&mut a, "1 — \"which db?\" on `deploy`");
    board(&mut a, vec![asking("deploy", "which db?")]);
    assert!(!line(&mut a).is_empty());

    a.key(Key::AltA);
    let card = a
        .open
        .first()
        .expect("alt-a opens the row's question")
        .clone();
    assert_eq!(card.summary, "which db?");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::AnswerTodo {
            content: "deploy".into(),
            question: "which db?".into(),
            answer: QuestionAnswer::choosing(0),
        })
    );
    assert!(
        line(&mut a).is_empty(),
        "answered — the line goes with the card"
    );
    assert!(
        a.screen(200, 40)
            .join("\n")
            .find("WAITING ON YOU")
            .is_none(),
        "and nothing redraws it"
    );
}

/// **A reattach re-reads the board, and what it draws comes from that read** — a head that was
/// away while the question was taken back holds a board with the question on it and a settings row
/// with the question in it; neither may survive the attach. The attach asks for the board itself
/// (so the window in which the old copy is drawn is a round trip, not a session).
#[test]
fn a_reattach_asks_for_the_board_and_draws_nothing_the_board_no_longer_has() {
    let mut a = app();
    a.session_id = "s1".into();
    asks_setting(&mut a, "1 — \"which db?\" on `deploy`");
    board(&mut a, vec![asking("deploy", "which db?")]);
    assert!(!line(&mut a).is_empty(), "the premise: the line is up");

    // The daemon went away and came back; the row's wait was taken off meanwhile.
    a.queued.clear();
    a.apply(hello(
        "s1",
        vec![brief("s1", "the work", false)],
        Hub::new("s1").snapshot(),
    ));
    assert!(
        a.queued.contains(&Action::ListTodos),
        "the board is read on every attach, not only when the settings row says so: {:?}",
        a.queued
    );
    asks_setting(&mut a, "none");
    board(&mut a, vec![taken_back("deploy")]);
    assert!(
        line(&mut a).is_empty(),
        "nothing the head read before the reattach survives it: {:?}",
        line(&mut a)
    );
    assert!(
        a.screen(200, 40)
            .join("\n")
            .find("WAITING ON YOU")
            .is_none()
    );

    // **And a resync is the same fact said another way** — the daemon saying this head fell
    // behind. The board is not in a snapshot, so a head that dropped the events would keep the
    // copy it had; the resync asks for the board too.
    board(&mut a, vec![asking("deploy", "which db?")]);
    assert!(!line(&mut a).is_empty());
    a.queued.clear();
    a.apply(ServerFrame::Resync {
        reason: "the head fell behind".into(),
        dropped: 4,
        snapshot: Box::new(Hub::new("s1").snapshot()),
        scrubbed: Default::default(),
    });
    assert!(
        a.queued.contains(&Action::ListTodos),
        "a resync re-reads the board: {:?}",
        a.queued
    );
    board(&mut a, vec![taken_back("deploy")]);
    assert!(
        line(&mut a).is_empty(),
        "and what the resync draws comes from that read: {:?}",
        line(&mut a)
    );
}
