//! **The payload window**: a result longer than the screen, opened with `ctrl-v`, and the four
//! ways a reader moves it — the arrows, the page keys, the wheel, and a click.
//!
//! The operator, on a 97-line `cat` in a 47-row terminal: *"it has 97 lines, 61 line elided and no
//! way for me to scroll that 'viewport' like it doesn respond to the mouse"*. The elision is the
//! budget doing its job — a large payload is capped on purpose and the count is honest — so what
//! these tests hold is the other half: **everything the window keeps is reachable, from the keys
//! and from the pointer, and the seam that says how much is not on the screen says it wherever the
//! reader is.**
//!
//! The numbers are the report's own: 97 lines of payload, a 100×47 frame. `WINDOW_CHROME` is ten
//! rows, so the window is 37 rows: one header, one seam and 36 lines of payload at the top —
//! `+61 lines` under it, which is exactly what the operator read off their screen.

use super::*;

/// The payload: `LINE-0 … LINE-{n-1}`, one line each, so a drawn line names itself.
fn payload(n: usize) -> String {
    (0..n).map(|i| format!("LINE-{i}\n")).collect()
}

fn result(a: &mut App, seq: u64, id: &str, call: &str, lines: usize) {
    shell_row(
        a,
        seq,
        id,
        "tool_result",
        TranscriptItem::ToolResult {
            call_id: call.into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: payload(lines),
            edit: None,
            origin: None,
            media: None,
        },
    );
}

/// **A session whose newest row is one long result** — what `ctrl-v` opens a window on.
fn long_result(lines: usize) -> App {
    let mut a = app();
    a.visibility = Visibility::of(Profile::NORMAL);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    result(&mut a, 1, "s.t1", "c0", lines);
    a
}

/// The payload line drawn on the screen, top to bottom — the frame's own answer to *where is the
/// window*, and never `App::payload_page`, which is what these tests are checking against.
fn drawn(screen: &str) -> Vec<usize> {
    screen
        .lines()
        .filter_map(|l| l.trim().strip_prefix("LINE-"))
        .filter_map(|n| n.parse().ok())
        .collect()
}

/// The number in `… +N lines` or `↑ N more lines above`, or zero when that seam is not drawn.
fn seam(screen: &str, needle: &str) -> usize {
    screen
        .lines()
        .find(|l| l.contains(needle))
        .and_then(|l| {
            let at = l.find(|c: char| c.is_ascii_digit())?;
            l[at..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// The screen row of a line the frame drew.
fn row_of(screen: &str, text: &str) -> u16 {
    screen
        .lines()
        .position(|l| l.trim() == text || l.contains(text))
        .unwrap_or_else(|| panic!("{text:?} is not on the screen:\n{screen}")) as u16
}

/// **`ctrl-v` opens the window on the newest result, at its top** — and it says both counts: how
/// long the payload is, and how much of it is under the seam.
#[test]
fn ctrl_v_opens_the_newest_result_at_its_top() {
    let mut a = long_result(97);
    a.key(Key::CtrlV);
    let screen = a.screen(100, 47).join("\n");
    assert!(a.payload_window_open(), "no window opened:\n{screen}");
    assert_eq!(a.payload_sel.as_deref(), Some("s.t1"));
    assert_eq!(a.payload_page, 0, "the window opened somewhere else");
    assert_eq!(
        drawn(&screen).first().copied(),
        Some(0),
        "the head of the payload is not what is drawn:\n{screen}"
    );
    assert!(screen.contains("97 lines"), "the header count:\n{screen}");
    // **The report's own numbers** — 36 lines drawn of 97, so 61 elided.
    assert_eq!(
        seam(&screen, "lines · ↓ pages down"),
        61,
        "the elided count is not the 61 the operator read:\n{screen}"
    );
}

/// **The arrows and the page keys move the window, each by its own step, and the count follows.**
#[test]
fn the_keys_page_the_window_and_the_elided_count_follows() {
    let mut a = long_result(97);
    a.key(Key::CtrlV);
    a.screen(100, 47);

    // The arrows are the transcript's own unit — ten lines.
    a.key(Key::Down);
    let screen = a.screen(100, 47).join("\n");
    assert_eq!(drawn(&screen).first().copied(), Some(10), "{screen}");
    assert_eq!(a.payload_page, 10);
    assert_eq!(
        seam(&screen, "more lines above"),
        10,
        "the seam above does not say how much is above it:\n{screen}"
    );
    assert_eq!(
        seam(&screen, "lines · ↓ pages down"),
        52,
        "the elided count did not come down with the page:\n{screen}"
    );

    // The page keys are half a screen plus one — 24 on a 47-row frame.
    a.key(Key::PageDown);
    let screen = a.screen(100, 47).join("\n");
    assert_eq!(drawn(&screen).first().copied(), Some(34), "{screen}");

    // And back up by the same steps.
    a.key(Key::PageUp);
    a.key(Key::Up);
    let screen = a.screen(100, 47).join("\n");
    assert_eq!(drawn(&screen).first().copied(), Some(0), "{screen}");
}

/// **The wheel pages the window under the pointer** — a notch is the house's three lines, the same
/// step the transcript takes, and the seam above it says so.
#[test]
fn the_wheel_pages_the_window_under_the_pointer() {
    let mut a = long_result(97);
    a.key(Key::CtrlV);
    let screen = a.screen(100, 47).join("\n");
    let at = row_of(&screen, "LINE-3");

    a.key(Key::Wheel {
        up: false,
        x: 10,
        y: at,
    });
    let screen = a.screen(100, 47).join("\n");
    assert_eq!(
        drawn(&screen).first().copied(),
        Some(3),
        "a notch down did not page the window:\n{screen}"
    );
    assert_eq!(a.payload_page, 3);

    a.key(Key::Wheel {
        up: false,
        x: 10,
        y: at,
    });
    a.key(Key::Wheel {
        up: true,
        x: 10,
        y: at,
    });
    let screen = a.screen(100, 47).join("\n");
    assert_eq!(
        drawn(&screen).first().copied(),
        Some(3),
        "a notch up did not page it back:\n{screen}"
    );
}

/// **The clamp is honest at both ends**: the last full page is the bottom and says so, the head is
/// the top and says nothing is above it, and a key pressed past either moves the window no further.
#[test]
fn the_window_clamps_at_both_ends_and_says_so() {
    let mut a = long_result(97);
    a.key(Key::CtrlV);
    a.screen(100, 47);

    a.key(Key::End);
    let max = a.payload_max.get();
    let screen = a.screen(100, 47).join("\n");
    assert_eq!(a.payload_page, max, "End did not reach the last page");
    assert!(
        screen.contains("… end of output"),
        "the end of the payload is not said:\n{screen}"
    );
    assert!(
        !screen.contains("pages down"),
        "the last page still offers a page that is not there:\n{screen}"
    );
    assert_eq!(drawn(&screen).last().copied(), Some(96), "{screen}");

    // Down at the end: the window stays and the conversation takes the rest of the scroll.
    a.key(Key::Down);
    assert_eq!(a.payload_page, max, "the window went past its last page");

    // **Up from the end moves at once** — no unwinding of invisible steps.
    a.key(Key::Up);
    assert_eq!(a.payload_page, max - 10, "Up had to unwind to move");

    a.key(Key::Home);
    let screen = a.screen(100, 47).join("\n");
    assert_eq!(a.payload_page, 0);
    assert_eq!(drawn(&screen).first().copied(), Some(0));
    assert!(
        !screen.contains("more lines above"),
        "the head still claims something is above it:\n{screen}"
    );
    a.key(Key::Up);
    assert_eq!(a.payload_page, 0, "Up at the head moved the window");
}

/// **A resize keeps the reader where they are and re-measures the window** — the body is sized
/// from the terminal's height, so a shorter screen draws fewer lines of the same payload from the
/// same offset.
#[test]
fn a_resize_keeps_the_window_where_it_is_and_re_measures_it() {
    let mut a = long_result(97);
    a.key(Key::CtrlV);
    a.screen(100, 47);
    a.key(Key::PageDown);
    let page = a.payload_page;
    assert_eq!(page, 24, "the premise: the window is a page down");

    for (w, h) in [(100, 60), (100, 24), (100, 47)] {
        let screen = a.screen(w, h).join("\n");
        assert_eq!(a.payload_page, page, "a resize moved the reader on {w}×{h}");
        assert_eq!(
            drawn(&screen).first().copied(),
            Some(page),
            "the window is not on the line it was on, on {w}×{h}:\n{screen}"
        );
    }

    // **And the body was measured again for each height** — a 24-row frame draws fewer lines of
    // the same payload than a 47-row one, and a stale body is what made a window overflow.
    let tall = drawn(&a.screen(100, 47).join("\n")).len();
    let short = drawn(&a.screen(100, 24).join("\n")).len();
    assert!(
        short < tall,
        "the shorter frame drew {short} lines and the taller {tall}: the window kept the body \
         it was given on the old screen"
    );
}

/// **A new result opens at its top** — the offset is where the reader was in ONE payload, and a
/// second payload inheriting it would skip the head of a result nobody has read.
#[test]
fn a_new_result_opens_at_its_top() {
    let mut a = long_result(97);
    a.key(Key::CtrlV);
    a.screen(100, 47);
    a.key(Key::End);
    assert!(
        a.payload_page > 0,
        "the premise: the reader paged to the end"
    );
    a.key(Key::Esc);
    assert!(!a.payload_window_open(), "the premise: the window closed");

    result(&mut a, 10, "s.t2", "c1", 50);
    a.key(Key::CtrlV);
    let screen = a.screen(100, 47).join("\n");
    assert_eq!(
        a.payload_sel.as_deref(),
        Some("s.t2"),
        "the window opened on the old result"
    );
    assert_eq!(
        a.payload_page, 0,
        "the new result inherited the last window's offset"
    );
    assert_eq!(drawn(&screen).first().copied(), Some(0), "{screen}");
    assert!(screen.contains("50 lines"), "{screen}");
}

/// **A click on a line reads the window from that line** — the same offset the keys move, so a key
/// pressed after a click continues from where the click left the reader.
#[test]
fn a_click_on_a_line_reads_the_window_from_that_line() {
    let mut a = long_result(97);
    a.key(Key::CtrlV);
    let screen = a.screen(100, 47).join("\n");
    let at = row_of(&screen, "LINE-6");

    a.key(Key::Click { x: 10, y: at });
    let screen = a.screen(100, 47).join("\n");
    assert_eq!(
        drawn(&screen).first().copied(),
        Some(6),
        "the clicked line is not the window's first:\n{screen}"
    );
    assert_eq!(a.payload_page, 6);

    // **One offset, two doors** — Down from where the click left it.
    a.key(Key::Down);
    let screen = a.screen(100, 47).join("\n");
    assert_eq!(
        drawn(&screen).first().copied(),
        Some(16),
        "the key did not continue from the click:\n{screen}"
    );
}

/// **A click that is not on a line of the payload is not a click on the window** — the header, the
/// `↑ N more lines above` seam and the `… +N lines` seam all act on nothing, so a reader pointing
/// at *"61 elided"* cannot move the window by accident.
#[test]
fn a_click_on_the_header_or_a_seam_acts_on_nothing() {
    let mut a = long_result(97);
    a.key(Key::CtrlV);
    a.screen(100, 47);
    a.key(Key::Down);
    let screen = a.screen(100, 47).join("\n");
    let page = a.payload_page;
    assert_eq!(page, 10, "the premise: a page in, so both seams are drawn");

    for (what, y) in [
        ("the header", row_of(&screen, "97 lines")),
        ("the seam above", row_of(&screen, "more lines above")),
        ("the seam below", row_of(&screen, "pages down")),
    ] {
        a.key(Key::Click { x: 10, y });
        assert_eq!(
            a.payload_page, page,
            "a click on {what} moved the window to {}",
            a.payload_page
        );
    }
}

/// **A drag over the window selects its cells and copies them** — the pointer's own selection,
/// which is drawn on the finished frame and so was never a per-row handler: what a drag does to a
/// diff's rows it does to these.
#[test]
fn a_drag_over_the_window_selects_and_copies_its_lines() {
    let mut a = long_result(97);
    // The terminal that speaks OSC 52 — the copy's door, and the reason a drag is a copy at all.
    let mut features = rano::term::Features::default();
    features.clipboard = true;
    a.set_features(features);
    a.key(Key::CtrlV);
    let screen = a.screen(100, 47).join("\n");
    let first = row_of(&screen, "LINE-0");
    let last = row_of(&screen, "LINE-2");

    a.key(Key::Press { x: 6, y: first });
    a.key(Key::Drag { x: 14, y: last });
    a.key(Key::Release { x: 14, y: last });
    let copied = a
        .clipboard_out
        .clone()
        .expect("a drag over the window copied nothing");
    let lines: Vec<&str> = copied.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "the drag did not span three lines: {copied:?}"
    );
    assert!(lines[0].starts_with("LINE-0"), "the copy: {copied:?}");
    assert!(lines[2].contains("LINE-2"), "the copy: {copied:?}");
}

/// **The seam tells the truth at every page**: what is drawn, what is above and what is elided add
/// up to the payload, so *"97 lines, 61 elided"* is arithmetic a reader can check.
#[test]
fn the_counts_add_up_to_the_payload_at_every_page() {
    const TOTAL: usize = 97;
    let mut a = long_result(TOTAL);
    a.key(Key::CtrlV);
    for _ in 0..4 {
        let screen = a.screen(100, 47).join("\n");
        let shown = drawn(&screen).len();
        let above = seam(&screen, "more lines above");
        let below = seam(&screen, "lines · ↓ pages down");
        assert_eq!(
            shown + above + below,
            TOTAL,
            "shown {shown} + above {above} + elided {below} is not the {TOTAL} the header says:\n{screen}"
        );
        assert!(
            screen.contains(&format!("{TOTAL} lines")),
            "the header count left the screen while scrolling:\n{screen}"
        );
        a.key(Key::PageDown);
    }
}
