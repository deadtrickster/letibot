//! **How many allocations a frame costs** — measured, because the claim is worth nothing without
//! a number.
//!
//! The head's loop calls `App::screen(w, h)` once per pass, tens of times a second, and then hands
//! the frame to `Terminal::draw`, which diffs it against a memory of the glass. Every `String`
//! built along that path exists to be compared and dropped, so this is the one place in the head
//! where an allocation is unambiguously hot — and the place a change is easiest to make on a hunch.
//!
//! # How it measures
//!
//! A counting global allocator: one `fetch_add` per `alloc`/`realloc`. Nothing else in this binary
//! allocates during a measured region, so the delta around one call is that call's own cost.
//!
//! # Why there is exactly ONE test in this file, and it is not tidiness
//!
//! The counter is global and the region is a wall-clock slice, so **a second test running
//! concurrently in this binary lands in the same count**. It did: three tests here reported 9
//! allocations for a frame that provably allocates none, and the frame numbers wandered by ±30
//! between runs. Cargo gives each test *file* its own process but runs the tests *within* one in
//! parallel threads, so the fix is one test measuring in sequence — not a thread-local allocator,
//! which would need care around TLS initialisation to be safe.
//!
//! # Why it is a test and not a benchmark
//!
//! A benchmark reports a time, which depends on the machine, the governor and whatever else is
//! running. This counts allocations, which is a property of the code. The numbers are exact and a
//! regression is a regression on any box.
//!
//! # The bounds, and what each one catches
//!
//! * **`App::screen`** must not build a second copy of the frame, and must not allocate per line
//!   for lines that have not changed. That is what the tail of `screen` used to do —
//!   `into_iter().map(…).collect()` built a new `Vec<String>` and dropped the old one, and the
//!   mapping called `trim_to` (which allocates even when a line already fits) and then `format!`
//!   to pad it.
//! * **`Terminal`** must not re-copy the glass. `paint_full` started from `shown.to_vec()` and
//!   returned the copy, so a `String` per screen row was allocated and freed on every frame —
//!   including frames that changed nothing and wrote no bytes at all.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn count() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

/// A head showing a settled conversation, sized like a real terminal.
///
/// The content matters: an empty screen is not the hot case. This drives the head the way the
/// driver does — a snapshot for `Hello`, then live frames — using the daemon's own test fixtures,
/// so the rows are the shapes a real frame has to draw rather than a hand-made approximation.
fn head() -> letibot_tui::app::App {
    use letibot_sessionlog::{Hub, ServerFrame, protocol, testing};
    use letibot_tui::render::RenderConfig;

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
    use letibot_tui::backend::terminal::paint_full;

    let glass: Vec<String> = (0..40)
        .map(|i| format!("row {i} of a settled screen"))
        .collect();
    let mut edited = glass.clone();
    edited[7] = "row 7, and this one moved".into();

    // **Warm twice, and the reason is that the buffers alternate.** `adopt_next` swaps them, so
    // after the first frame `shown` holds the rows and `scratch` is the empty vec `shown` used to
    // be; it is the second frame that refills `scratch`, and only from the third does the buffer
    // being filled arrive already holding this screen's rows. Measuring after one warm-up measures
    // the frame that legitimately allocates every row — which is how the first version of this
    // test reported 51 allocations for a frame that changes nothing. The test was wrong, not the
    // encoder.
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

    // ---- the scroll path, which never took the diff until now -----------------------------
    //
    // **The half this file never covered, and the one that was flickering.** A wheel notch is a
    // key like any other, so it went through the same loop — but the arm that handled it set
    // `App::redraw`, the driver read that before the next frame and called `Terminal::invalidate`,
    // and every frame after a notch was therefore a `full` one: `ESC[2J`, then all forty rows
    // rewritten with the row diff switched off. On a touchpad that is one erase per NOTCH: the
    // inertial scroll arrives over many reads and every read is its own tick.
    //
    // Two things are asserted, and they are the fix and its shape. **The flag stays clear** —
    // that is the whole change, and the assertion that fails on the old code. **And the frame a
    // notch produces takes the diff**: it writes exactly the rows whose text differs and no
    // screen erase at all, which is the claim that makes dropping the flag safe rather than
    // optimistic.
    use letibot_sessionlog::{Envelope, ServerFrame, testing};
    use letibot_tui::app::Key;

    let env = |seq: u64, event| Envelope {
        session_id: "s".into(),
        seq,
        ts: 0,
        event,
    };
    // **Tall enough for the window to MOVE**, or the test proves nothing: a screen whose rows do
    // not change takes the diff path and the full path identically, both writing nothing. Forty
    // rows of conversation against a forty-row terminal, added after the allocations above are
    // measured so the fixture's cost is where it was.
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

    // **Spend the flag the arrivals set, the way the head's loop does.** `take_redraw` is read at
    // the top of every pass and *before* the keys (`head.rs`), so the flag a key sets is spent by
    // the frame after it — and the flag an arriving row set is spent by the frame before any key
    // at all. Draining it here is what keeps the assertions below about the scroll rather than
    // about the forty rows that built the fixture.
    assert!(
        app.take_redraw(),
        "the fixture's own rows should have asked for a frame"
    );

    let mut before = app.screen(120, 40);
    assert!(before.len() > 20, "the scroll fixture draws no frame");
    put(&mut shown, &mut scratch, &before);

    // **The state change is kept, and the walk with it**, because dropping a repaint must not drop
    // a scroll: a notch up parks the reader on the anchor, and a notch down walks three lines back
    // toward the tail. This fixture parks three lines up, so the walk arrives in one notch — but
    // that is the geometry and not the rule. **One notch was the tail itself until 2026-10-06**, and
    // that is what the operator reported as *"one simple stroke gets me to the bottom immediately —
    // effectively like Esc"*; the walk is the reconciliation, and Esc and the parked `↓` are the
    // keys that still mean "follow again" in one press.
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

    // **Ctrl-L is the other half of the same decision, and it keeps its erase.** The flag means
    // *this head's memory of the glass is wrong*, which is what the operator is saying when they
    // press it — the one key whose whole job is to repaint. A resize is the other case and it is
    // in `term.rs`, because that is where the new size is known.
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
