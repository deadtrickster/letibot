//! How many allocations a frame costs, counted on the allocating thread's own counter: `App::screen`
//! and `Terminal::draw` are the head's one hot path, and the delta around a call is that call's own
//! cost whatever else in the binary is running. A count is a property of the code, a time is not.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// This thread's allocations. See the module docs for why `const` and `Cell` are the point.
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

fn bump() {
    let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// The calling thread's allocations so far.
fn count() -> usize {
    ALLOCS.with(Cell::get)
}

/// A head showing a settled conversation, sized like a real terminal: an empty screen is not the hot
/// case. It is driven the way the driver drives it - a `Hello` snapshot, then live frames - over the
/// daemon's own fixtures, so the rows are the shapes a real frame draws.
fn head() -> letibot_tui::app::App {
    use letibot_sessionlog::{Hub, ServerFrame, protocol, testing};
    use letibot_tui::ui::render::RenderConfig;

    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(testing::delta(
        "t1",
        "The cache is keyed on the bytes the server saw, and the dialect replays prior reasoning \
         into the field the model expects; three things had to line up for it to hit, and only \
         two of them did. The prefix is frozen before the first turn, which is why a rewrite of \
         the prompt costs a full prefill and nothing else does. ",
    ));
    hub.publish(testing::proposed_on(
        "t1",
        "c1",
        "bash",
        "\"cargo test --release\"",
    ));
    let mut app = letibot_tui::app::App::new(RenderConfig {
        width: 120,
        color: true,
        ..RenderConfig::default()
    });
    app.apply(ServerFrame::Hello {
        protocol_version: protocol::PROTOCOL_VERSION,
        session_id: "s".into(),
        head_id: "h1".into(),
        dropped: 0,
        snapshot: Some(Box::new(hub.snapshot())),
        resumed_from: None,
        scrubbed: Default::default(),
        wiring: Default::default(),
        sessions: Vec::new(),
    });
    app
}

/// **A frame, and a glass, at a steady state** — the whole per-frame path, in one test.
#[test]
fn a_frame_costs_no_allocation_per_unchanged_line() {
    // ---- the head's own frame -------------------------------------------------------------
    let mut app = head();
    // Warm: the first frames lex and cache everything, and caching is a one-off cost the steady
    // state does not pay. Measuring it would flatter whatever change is being tested.
    for _ in 0..5 {
        std::hint::black_box(app.screen(120, 40));
    }
    let rows = app.screen(120, 40).len();
    assert!(
        rows > 20,
        "the fixture does not draw a real frame: {rows} rows"
    );

    let before = count();
    let frame = app.screen(120, 40);
    let per_frame = count() - before;
    let per_line = per_frame as f64 / frame.len() as f64;
    println!(
        "ALLOCS screen   one_frame={per_frame} rows={} per_line={per_line:.2}",
        frame.len()
    );
    assert!(
        per_line < 4.5,
        "one frame costs {per_frame} allocations for {} rows ({per_line:.2} per line)",
        frame.len()
    );

    // Ten in a row, so a cost that is really per-call cannot hide in one sample.
    let before = count();
    for _ in 0..10 {
        std::hint::black_box(app.screen(120, 40));
    }
    let ten = (count() - before) / 10;
    println!("ALLOCS screen   settled_per_frame={ten}");
    assert!(
        ten < 260,
        "a settled frame costs {ten} allocations, which is more than the frame's own rows"
    );

    // ---- the terminal's diff --------------------------------------------------------------
    use rano::term::terminal::paint_full;

    let glass: Vec<String> = (0..40)
        .map(|i| format!("row {i} of a settled screen"))
        .collect();
    let mut edited = glass.clone();
    edited[7] = "row 7, and this one moved".into();

    // Warm twice, because the buffers alternate: `adopt_next` swaps them, so only from the third
    // frame does the buffer being filled arrive holding this screen's rows. One warm-up measures the
    // frame that legitimately allocates every row.
    let mut shown: Vec<String> = Vec::new();
    let mut scratch: Vec<String> = Vec::new();
    let s = paint_full(&shown, &glass, &mut scratch, None, None, true);
    assert!(s.contains("row 0"), "the first paint is a full one");
    std::mem::swap(&mut shown, &mut scratch);
    let s = paint_full(&shown, &glass, &mut scratch, None, None, false);
    assert_eq!(s, "", "the second frame is already identical");
    std::mem::swap(&mut shown, &mut scratch);

    let before = count();
    let s = paint_full(&shown, &glass, &mut scratch, None, None, false);
    let quiet = count() - before;
    println!("ALLOCS terminal unchanged_frame={quiet}");
    assert_eq!(s, "", "an unchanged frame writes nothing");
    assert_eq!(
        quiet, 0,
        "an unchanged frame cost {quiet} allocations — the glass is being re-copied per frame"
    );

    // A frame that changed one row: the escape sequence and the row's text, and nothing per row.
    let before = count();
    let s = paint_full(&shown, &edited, &mut scratch, None, None, false);
    let one_row = count() - before;
    println!("ALLOCS terminal one_row_changed={one_row}");
    assert!(
        s.contains("row 7, and this one moved"),
        "the row was written"
    );
    assert!(
        one_row < 8,
        "changing one row of 40 cost {one_row} allocations — the per-row copy is back"
    );

    // ---- the scroll path, which never took the diff ---------------------------------------
    // Two things are asserted: the redraw flag stays clear after a notch (that is the whole change),
    // and the frame a notch produces writes exactly the rows that differ with no screen erase.
    use letibot_sessionlog::{Envelope, ServerFrame, testing};
    use letibot_tui::app::Key;

    let env = |seq: u64, event| Envelope {
        session_id: "s".into(),
        seq,
        ts: 0,
        event,
    };
    // Tall enough for the window to MOVE, or the test proves nothing: a screen whose rows do not
    // change takes the diff path and the full path identically, both writing nothing. Added after the
    // allocations above are measured, so the fixture's cost is where it was.
    for i in 0..40u64 {
        let id = format!("q.{i}");
        app.apply(ServerFrame::Event(env(
            1_000 + i * 2,
            testing::appended(&id, "user"),
        )));
        app.apply(ServerFrame::Event(env(
            1_001 + i * 2,
            testing::content(
                &id,
                &format!("row {i} of a conversation long enough to scroll"),
            ),
        )));
    }

    // Paint a frame onto the glass the way the head does, and hand back the bytes.
    let put = |shown: &mut Vec<String>, scratch: &mut Vec<String>, frame: &[String]| {
        let s = paint_full(shown, frame, scratch, None, None, false);
        std::mem::swap(shown, scratch);
        s
    };

    // Spend the flag the arrivals set, the way the head's loop does: `take_redraw` is read at the top
    // of every pass and before the keys (`head.rs`), so draining it here keeps the assertions below
    // about the scroll rather than about the forty rows that built the fixture.
    assert!(
        app.take_redraw(),
        "the fixture's own rows should have asked for a frame"
    );

    let mut before = app.screen(120, 40);
    assert!(before.len() > 20, "the scroll fixture draws no frame");
    put(&mut shown, &mut scratch, &before);

    // The state change is kept, and the walk with it, because dropping a repaint must not drop a
    // scroll: a notch up parks the reader and a notch down walks three lines back toward the tail,
    // so a fixture parked three lines up follows again in one notch. That is geometry, not the rule.
    assert!(app.following(), "the fixture starts on the stream");
    assert_eq!(app.key(Key::WheelUp), None);
    assert!(!app.following(), "a notch up parks the reader");
    assert!(!app.take_redraw(), "a notch up asked for a full repaint");
    assert_eq!(app.key(Key::WheelDown), None);
    assert!(
        app.following(),
        "the notch that walks into the tail is the one that follows again"
    );
    assert_eq!(app.scroll, 0, "and the count agrees with the anchor");
    assert!(
        !app.take_redraw(),
        "the notch down asked for a full repaint"
    );
    before = app.screen(120, 40);
    put(&mut shown, &mut scratch, &before);

    // Every key the conversation pane scrolls with, one at a time. The order matters only in that
    // the page keys have somewhere to move FROM — a notch up, a page up, a notch down, a page up
    // again, then a page down.
    for key in [
        Key::WheelUp,
        Key::PageUp,
        Key::WheelDown,
        Key::PageUp,
        Key::PageDown,
    ] {
        let name = format!("{key:?}");
        assert_eq!(app.key(key), None, "{name} is the head's own key");
        assert!(
            !app.take_redraw(),
            "{name} asked for the glass to be thrown away — that is the `ESC[2J` per notch a \
             touchpad flick was made of"
        );
        let after = app.screen(120, 40);
        let differing = (0..before.len().max(after.len()))
            .filter(|i| before.get(*i) != after.get(*i))
            .count();
        let painted = put(&mut shown, &mut scratch, &after);
        assert!(
            !painted.contains("\x1b[2J"),
            "the frame after {name} erased the screen"
        );
        assert_eq!(
            painted.matches("\x1b[K").count(),
            differing,
            "the frame after {name} did not write exactly the rows whose text differs: the \
             diff is the whole reason the erase is not needed"
        );
        before = after;
    }

    // Ctrl-L is the other half of the same decision, and it keeps its erase: the flag means this
    // head's memory of the glass is wrong, which is what pressing it says. A resize is the other case
    // and it lives in `term.rs`, where the new size is known.
    assert_eq!(app.key(Key::CtrlL), None);
    assert!(
        app.take_redraw(),
        "Ctrl-L no longer asks for a full repaint"
    );
    assert!(
        paint_full(&[], &before, &mut scratch, None, None, true).contains("\x1b[2J"),
        "and a full paint is still an erase, so the flag means what the scroll path no longer \
         wants"
    );
}
