//! Scrolling, holding the view, following the bottom, the history buffer.

use super::*;

/// A line that names nothing is still the mid-typing courtesy: a permission that arrives under
/// somebody's half-thought holds their words and answers the marked row, so their Enter is not wasted.
#[test]
fn a_line_that_names_nothing_holds_the_words_and_answers_the_marked_row() {
    use letibot_sessionlog::event::OptionKind;
    let mut a = app();
    a.open.push(decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::RejectOnce,
    ]));
    let action = a.submit("why did the cache miss?".into());
    assert!(
        matches!(action, Some(Action::Answer { ref option_id, .. }) if option_id == "allow_once"),
        "the marked row is the answer: {action:?}"
    );
    assert_eq!(a.input(), "why did the cache miss?", "the words are held");
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("answered the ask"),
        "and it says which of the two things just happened: {:?}",
        a.notice
    );
}

#[test]
fn up_with_a_half_typed_line_still_belongs_to_the_editor() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut a, "queued one");
    a.key(Key::Enter);
    typed(&mut a, "half a thought");
    assert_eq!(a.key(Key::Up), None, "the editor keeps its own Up");
    // What the editor does with it is readline's own semantics, unchanged: Up at the top row walks its
    // submitted-line history. The queue's part is only the negative one, so no take-back rides along.
    assert_eq!(a.input(), "queued one");
    assert_eq!(a.pending_prompts.len(), 1, "the queue is untouched");
}

/// A scrolled viewport holds while the transcript grows under it, and the case that matters is a reader
/// who scrolls to read something WHILE a turn runs: rows arrive above and below and a row's height
/// changes under the viewport. What is asserted is the frame, since a line count is the derived thing.
#[test]
fn a_scrolled_viewport_holds_while_the_transcript_grows() {
    fn visible(a: &mut App) -> Vec<String> {
        a.screen(100, 30)
            .into_iter()
            .filter(|l| !l.contains("holding your place"))
            .collect()
    }

    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A conversation to scroll in, with a turn running so the live pane draws at the
    // same time as history arrives.
    for i in 0..40u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i} says a thing")),
        )));
    }
    a.apply(ServerFrame::Event(env(200, testing::turn_started("t1"))));
    a.screen(100, 30);

    // Six notches, not a page: the window is 25 body lines here, so a page up reaches the top and leaves
    // no row above the anchor to grow. The requirement's case is a reader who scrolled to read something.
    for _ in 0..6 {
        a.key(Key::WheelUp);
        a.screen(100, 30);
    }
    assert!(!a.following(), "six notches up did not leave the stream");
    let held = a.anchor.clone().expect("a held row");
    assert!(
        held.ordinal > 0,
        "the fixture scrolled to the very top, so there is no row above the anchor to \
             grow: {held:?}"
    );
    let under = visible(&mut a);
    let top_before = a.view_top;

    // 1. **Rows arrive BELOW the anchor** - the ordinary case: a turn commits what it
    //    has done and the transcript grows under a reader who is reading.
    for i in 100..108u64 {
        a.apply(ServerFrame::Event(env(
            i,
            testing::appended(&format!("s.new{i}"), "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            i + 1000,
            testing::content(&format!("s.new{i}"), "new content below the reader"),
        )));
    }
    assert_eq!(
        visible(&mut a),
        under,
        "content arriving below moved the held view"
    );
    assert_eq!(a.anchor, Some(held.clone()), "and moved the held row");

    // 2. **A row ABOVE the anchor grows** - R29's remedy line, a note, a tail that
    //    expanded. This is the case a line count cannot survive: every line below it
    //    shifts, and a count moves with them.
    {
        let first = a.items.first_mut().expect("a first row");
        first.kind = "assistant".into();
        first.item = Some(letibot_transcript::TranscriptItem::Assistant {
            text: "row 0 says a thing\nand three more lines\nthat were not there \
                       before\nat all"
                .into(),
            tool_calls: Vec::new(),
            truncated: false,
        });
    }
    a.invalidate_history();
    assert_eq!(
        visible(&mut a),
        under,
        "a row above the anchor gaining lines moved the held view"
    );
    assert_eq!(a.anchor, Some(held.clone()), "and moved the held row");

    // 3. **A tool row GROWS as its output lands**, under the running turn - the third
    //    way a line count creeps, and the one the live case is for.
    a.apply(ServerFrame::Event(env(
        3000,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: "exec".into(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        3003,
        testing::tool_progress("c1", "30 lines so far"),
    )));
    a.apply(ServerFrame::Event(env(
        3001,
        SessionEvent::TranscriptAppended {
            item_id: "s.grow".into(),
            kind: "tool_result".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        3002,
        SessionEvent::TranscriptContent {
            item_id: "s.grow".into(),
            item: Box::new(letibot_transcript::TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: (0..30)
                    .map(|i| format!("output line {i}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    let grown = a.screen(100, 30);
    assert_eq!(
        a.anchor,
        Some(held.clone()),
        "a growing tool row moved the held row"
    );
    assert!(
        grown.iter().any(|l| l.contains("holding your place")),
        "the screen must still say the viewport is held: {grown:#?}"
    );
    // The window's own lines are the ones it drew before, minus that banner.
    let window: Vec<String> = grown
        .into_iter()
        .filter(|l| !l.contains("holding your place"))
        .collect();
    assert_eq!(window, under, "a growing tool row moved the held view");

    // 4. **The hold is a PLACE, not a count**: the window's top line is where it was,
    //    which a count could not have kept with 38 lines added below it.
    assert_eq!(
        a.view_top, top_before,
        "the window's top line moved; a hold is a place, not a count"
    );
    assert!(a.body_len > 84, "nothing was added to the body at all");

    // 5. **Nothing that arrived returned the reader to the stream.** Only an explicit
    //    act does, and it is the one the banner names.
    assert!(!a.following());
    a.key(Key::Esc);
    assert!(a.following(), "esc did not resume following");
    assert_eq!(
        a.scroll, 0,
        "resuming following left a scroll behind: {}",
        a.scroll
    );
}

/// A replacement that carries the row holds the view, and one that takes it away says so: the nearest
/// surviving row is chosen by an index measured in the transcript that just went, so the tail is where a
/// reader goes.
#[test]
fn a_replacement_holds_the_view_or_says_the_row_is_gone() {
    /// A snapshot holding `ids`, as the daemon sends one on `hello` or a resync.
    fn snapshot_with(ids: &[&str]) -> letibot_sessionlog::view::Snapshot {
        let mut snap = Hub::new("s").snapshot();
        snap.seq = 9;
        snap.items = ids
            .iter()
            .map(|id| letibot_sessionlog::view::SnapshotItem {
                citations: Vec::new(),
                item_id: (*id).to_string(),
                kind: "assistant".into(),
                ledger_head: "beef".into(),
                ts: 0,
                item: Some(letibot_transcript::TranscriptItem::Assistant {
                    text: format!("row {id} says a thing"),
                    tool_calls: Vec::new(),
                    truncated: false,
                }),
            })
            .collect();
        snap
    }

    fn scrolled() -> App {
        let mut a = app();
        a.apply(hello(
            "s",
            vec![brief("s", "one", false)],
            Hub::new("s").snapshot(),
        ));
        for i in 0..40u64 {
            a.apply(ServerFrame::Event(env(
                i * 2 + 1,
                testing::appended(&format!("s.{i}"), "assistant"),
            )));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("s.{i}"), &format!("row {i} says a thing")),
            )));
        }
        a.screen(100, 30);
        for _ in 0..6 {
            a.key(Key::WheelUp);
            a.screen(100, 30);
        }
        assert!(!a.following(), "the fixture never scrolled");
        a
    }

    // ---- the row SURVIVES the replacement ----
    let mut a = scrolled();
    let held = a.anchor.clone().expect("a held row");
    // A snapshot that carries every row, which is what a plain resync of an unchanged
    // session is.
    let ids: Vec<String> = a.items.iter().map(|i| i.item_id.clone()).collect();
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        snapshot_with(&ids),
    ));
    a.screen(100, 30);
    assert_eq!(
        a.anchor,
        Some(held.clone()),
        "a replacement that carries the row moved the view"
    );
    assert!(
        !a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "anchor_lost")),
        "and it said nothing, because nothing was lost"
    );

    // ---- the row is GONE, and it is said ----
    let mut b = scrolled();
    let lost = b.anchor.clone().expect("a held row");
    // A replacement that carries the last twenty rows: the reader's row is gone and the transcript is
    // still long enough to hold a place in, which is what a compaction leaves behind. The place is not
    // held by index, which is the whole of what the tail rule refuses.
    let tail: Vec<String> = (20..40).map(|i| format!("s.{i}")).collect();
    let tail: Vec<&str> = tail.iter().map(String::as_str).collect();
    b.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        snapshot_with(&tail),
    ));
    let frame = b.screen(100, 30);
    let said = b
        .notes
        .iter()
        .find_map(|(_, n)| match n {
            Note::Warned(w) if w.code == "anchor_lost" => Some(w.detail.clone()),
            _ => None,
        })
        .expect("a row that is gone must be said, not jumped over");
    assert!(
        said.contains(&lost.item_id),
        "the sentence names the row that went: {said}"
    );
    assert!(
        said.contains("follows the tail again"),
        "and says where the reader is now, which is the whole of what it can say: {said}"
    );
    // It reaches the reader where they are looking: the note lives at the seam below a reader holding a
    // row near the top, so the transient line above the composer carries it too. A disclosure nobody sees
    // is not one.
    assert!(
        b.notice
            .as_deref()
            .is_some_and(|n| n.contains(&lost.item_id)),
        "the reader must be told on screen, not only at a seam below them: {:?}",
        b.notice
    );
    let _ = frame;
    // It goes to the tail, not to the nearest surviving row: the row at the old ordinal is chosen by an
    // index measured in the transcript that just went, and a shorter replacement makes that integer a
    // different place.
    assert!(
        b.anchor.is_none(),
        "the reader was landed on a row by index rather than sent to the tail: {:?}",
        b.anchor
    );
    assert!(
        b.following(),
        "the view must follow the tail when the row it was on is not in the transcript"
    );
}

/// While the view is held the head writes nothing, so a mouse selection survives a streaming turn: the
/// frame is composed once, every later call returns it byte for byte while rows land, and the release
/// says what arrived. A frame that does not change is a frame that writes no bytes.
#[test]
fn a_held_view_draws_the_same_frame_while_the_turn_streams() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::delta("t1", "the answer is"),
    )));
    assert!(!a.hold, "the premise: the view is following");

    // **The freeze, and the one write it owes** - the marker, on the hint bar's row.
    assert_eq!(a.key(Key::CtrlP), None, "the hold is not a daemon action");
    assert!(a.hold);
    let frozen = a.screen(100, 24);
    assert!(
        frozen.iter().any(|l| l.contains(HOLD_MARKER)),
        "the held frame does not say it is held:\n{}",
        frozen.join("\n")
    );

    // **And then nothing is written, however much arrives.** A turn streams, a round lands,
    // two rows are appended - the frame must not move by a byte.
    for i in 0..20u64 {
        a.apply(ServerFrame::Event(env(
            3 + i,
            testing::delta("t1", " more"),
        )));
    }
    a.apply(ServerFrame::Event(env(
        23,
        testing::appended("s.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        24,
        testing::content("s.0", "an answer"),
    )));
    a.apply(ServerFrame::Event(env(
        25,
        testing::appended("u.1", "user"),
    )));
    assert_eq!(
        a.screen(100, 24),
        frozen,
        "the held frame moved while the turn streamed"
    );

    // **`redraw` cannot force a repaint behind the hold's back**, which is the other way
    // bytes could reach the glass: a full repaint writes every row.
    a.mark_redraw();
    assert!(
        !a.take_redraw(),
        "a held view let the glass be thrown away, so a full repaint is one frame away"
    );
    assert_eq!(a.screen(100, 24), frozen);

    // **The release follows again, and says how much arrived - once**, which is what makes
    // the count honest: a live count while held would be an animation.
    assert_eq!(a.key(Key::CtrlP), None);
    assert!(!a.hold, "the second press releases");
    let released = a.screen(100, 24);
    assert_ne!(released, frozen, "the view did not come back");
    let said = released.join("\n");
    // Said in the status row, after the turn's clock - cut to the room it has there, and whole
    // in the notice a click opens.
    assert!(said.contains("the view follows again"), "{said}");
    let notice = a.notice.clone().unwrap_or_default();
    assert!(
        notice.contains("the view follows again — 2 rows arrived while it was held"),
        "the release did not count what arrived: {notice}"
    );
    assert!(
        !said.contains(HOLD_MARKER),
        "the marker outlived the hold:\n{said}"
    );
}

/// The arrows bring the cursor's row into view: a pane longer than the screen was walkable only by
/// PageDown or the wheel, so a child below the fold looked unreachable.
#[test]
fn the_arrows_scroll_the_subagent_cursor_into_view() {
    let mut a = app();
    let mut fam = vec![brief("s", "parent", false)];
    for i in 0..40 {
        let mut b = brief(&format!("s-sub-{i:02}"), &format!("child {i}"), true);
        b.parent_session_id = Some("s".into());
        fam.push(b);
    }
    a.apply(hello("s", fam, Hub::new("s").snapshot()));
    a.key(Key::CtrlG);
    // All forty are generating, so all forty are stops and nothing is folded.
    a.screen(100, 20);
    assert_eq!(a.pane_scroll, 0, "the pane opens at its head");
    for _ in 0..30 {
        a.key(Key::Down);
    }
    let screen = a.screen(100, 20).join("\n");
    assert!(a.pane_scroll > 0, "the window never followed the cursor");
    assert!(
        screen.contains("child 30"),
        "the cursor walked off the bottom and the pane did not follow:\n{screen}"
    );
}

#[test]
fn arrows_walk_the_subagent_output_back_to_its_beginning() {
    let mut a = app();
    let payload: String = (0..50).map(|i| format!("line {i}\n")).collect();
    // The head asks before the daemon answers (a reply nobody asked for is dropped).
    a.ask_peek("s-sub-1");
    a.apply(ServerFrame::Peeked {
        session_id: "s-sub-1".into(),
        dropped: 0,
        snapshot: None,
        events: vec![env(
            1,
            SessionEvent::TranscriptContent {
                item_id: "i1".into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: "c1".into(),
                    name: "bash".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload,
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )],
    });
    // A terminal: the tail shows by default, the beginning does not.
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("line 49"), "{screen}");
    assert!(!screen.contains("line 0"), "{screen}");
    // Up walks back, and the draw clamps at the beginning - sixty ups on a
    // fifty-line view stop at the top rather than scrolling into nothing.
    for _ in 0..60 {
        a.key(Key::Up);
    }
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("line 0"), "{screen}");
}

/// A pane longer than the terminal was a pane whose tail could not be read: every pane drew
/// `rows.truncate(room)` and the scroll keys were swallowed while one was open, so most of a 98-row
/// TODO could not be reached.
#[test]
fn a_pane_taller_than_the_screen_scrolls_to_its_end() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-scroll-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    let mut body = String::from("## Phase 0\n\n");
    for i in 0..60 {
        body.push_str(&format!("- [ ] **T{i}** item number {i}\n"));
    }
    std::fs::write(dir.join("TODO.md"), &body).expect("write");

    let mut a = app();
    a.wiring.workspace = dir.display().to_string();
    a.key(Key::CtrlT);
    let top = a.screen(110, 20).join("\n");
    assert!(top.contains("T0 item number 0"), "{top}");
    assert!(
        !top.contains("T59 item number 59"),
        "the tail is off-screen: {top}"
    );

    // PageDown reaches it. Enough presses to pass the end - the clamp is
    // what stops it, and scrolling past into blank rows would be its own bug.
    for _ in 0..20 {
        a.key(Key::PageDown);
        a.screen(110, 20);
    }
    let end = a.screen(110, 20).join("\n");
    assert!(end.contains("T59 item number 59"), "{end}");
    assert!(!end.contains("T0 item number 0"), "{end}");
    // The last screenful, not past it: the final row is still drawn.
    assert!(
        a.pane_scroll == a.pane_len.saturating_sub(a.pane_room),
        "clamped to the last screenful: {} of {}/{}",
        a.pane_scroll,
        a.pane_len,
        a.pane_room
    );

    // And back up.
    for _ in 0..20 {
        a.key(Key::PageUp);
        a.screen(110, 20);
    }
    assert_eq!(a.pane_scroll, 0);
    assert!(a.screen(110, 20).join("\n").contains("T0 item number 0"));

    // A reopened pane starts at the top rather than where it was left.
    a.key(Key::PageDown);
    a.screen(110, 20);
    a.key(Key::Esc);
    a.key(Key::CtrlT);
    assert_eq!(a.pane_scroll, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The cursor keeps itself on screen: an arrow that walks the selection out
/// of the window reads as a key that does nothing.
#[test]
fn the_cursor_scrolls_itself_into_view() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-cursor-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    let mut body = String::from("## Phase 0\n\n");
    for i in 0..40 {
        body.push_str(&format!("- [ ] **T{i}** item {i}\n"));
    }
    std::fs::write(dir.join("TODO.md"), &body).expect("write");
    let mut a = app();
    a.wiring.workspace = dir.display().to_string();
    a.key(Key::CtrlT);
    a.screen(110, 20);

    for _ in 0..30 {
        a.key(Key::Down);
    }
    let screen = a.screen(110, 20).join("\n");
    assert!(screen.contains("▸"), "the cursor is on screen: {screen}");
    assert!(a.pane_scroll > 0, "which took scrolling: {}", a.pane_scroll);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The arrows belong to the decision only while the composer is empty. A half-typed line still edits, and
/// Enter does not send it while an ask is open: the line is held and the next Enter, with the ask
/// settled, sends it.
#[test]
fn enter_on_an_ask_answers_it_and_holds_the_half_typed_line() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
    typed(&mut a, "some prose");
    // The arrows reach the ladder even mid-line, and the line is untouched. They used to belong to the
    // composer, so choosing an option while typing meant emptying it first.
    a.key(Key::Down);
    assert_eq!(a.sel, 1, "the ladder moved");
    assert_eq!(a.input(), "some prose", "and the line is still there");
    a.key(Key::Up);
    assert_eq!(a.sel, 0);
    assert_eq!(a.input(), "some prose");
    // Enter answers the ask with the marked row and holds the line.
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "r1".into(),
            option_id: "allow".into(),
            pattern: None,
            note: None
        })
    );
    assert_eq!(a.input(), "some prose", "the words are held, not sent");
    assert!(a.pending_prompts.is_empty(), "nothing was sent");
    // With the ask settled, the next Enter sends the held line.
    a.apply(ServerFrame::Event(env(2, testing::answered("r1", "allow"))));
    assert_eq!(a.key(Key::Enter), Some(Action::Prompt("some prose".into())));
}

/// Parked in the scrollback the arrows are the scrollback's, and `↓` is the bottom: three presses of it
/// moved the banner's count by nothing while the stream added lines, and one `↑` put an old prompt into
/// the composer. The fixture has editor history, because history is what ate the keys.
#[test]
fn parked_in_the_scrollback_the_arrows_are_the_scrollbacks_and_down_is_the_bottom() {
    let mut a = app();
    for i in 0..40u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i} says a thing")),
        )));
    }
    a.screen(100, 30);

    // Two prompts, submitted, so the composer has a history to walk.
    for line in ["the first prompt", "the second prompt"] {
        for c in line.chars() {
            a.key(Key::Char(c));
        }
        a.key(Key::Enter);
    }
    assert!(
        a.editor.text().is_empty(),
        "a submit leaves the composer empty: {:?}",
        a.editor.text()
    );

    // Park the way a wheel does, and check the banner this is all about.
    a.key(Key::WheelUp);
    let screen = a.screen(100, 30);
    assert!(!a.following(), "the wheel left the stream");
    assert!(
        screen.iter().any(|l| l.contains("holding your place")),
        "the banner is up: {screen:#?}"
    );

    // **↑ scrolls the transcript and does NOT walk history.** The composer is where a
    // recalled prompt appeared, so it is what the assertion is about.
    let top = a.view_top;
    a.key(Key::Up);
    assert!(
        a.editor.text().is_empty(),
        "↑ put the composer's history in the box: {:?}",
        a.editor.text()
    );
    a.screen(100, 30);
    assert!(!a.following(), "↑ returned the reader to the stream");
    assert!(a.view_top <= top, "↑ did not move the view up");

    // **↓ is the bottom in ONE press**, which is what the banner says and what a key moving
    // one line can never be against a stream that adds lines faster.
    a.key(Key::Down);
    assert!(a.following(), "↓ did not return the reader to the stream");
    let screen = a.screen(100, 30);
    assert!(
        !screen.iter().any(|l| l.contains("holding your place")),
        "the banner is still up after ↓: {screen:#?}"
    );

    // With the reader following, `↑` is not this arm's at all: it is the take-back two arms above, so the
    // queued prompts come back for editing rather than scrolling anything.
    a.key(Key::Up);
    assert_eq!(
        a.editor.text(),
        "the first prompt\nthe second prompt",
        "↑ while following must still be the head's own key"
    );
}

/// A key that scrolls a pane moves that pane, and the two overlays are tail-origin: the page keys and the
/// wheel reached the transcript while the job-output overlay was open, and the slash listing's arms were
/// inverted, since `pane_scroll` counts rows hidden above the top.
#[test]
fn a_key_that_scrolls_a_pane_moves_that_pane_and_not_the_transcript_behind_it() {
    // ---- the job-output overlay: tail-origin, and it must take the page keys ----
    let mut a = App::new(plain_cfg(100));
    a.session_id = "s1".into();
    a.apply(jobs_frame(
        "s1",
        vec![daemon_job("j3", "cargo build", false)],
    ));
    a.key(Key::CtrlQ);
    // **The job is settled**, so Enter on the group row unfolds it and one Down lands on
    // it; then Enter opens the overlay the rest of this test drives.
    a.key(Key::Enter);
    a.key(Key::Down);
    a.key(Key::Enter);
    let lines: Vec<String> = (0..60).map(|i| format!("line {i}")).collect();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::JobOutput {
            job: "j3".into(),
            from: 0,
            to: 10,
            produced: 60,
            dropped: 0,
            state: "exited 0".into(),
            never_ran: false,
            lines,
            next: None,
        },
    )));
    // One draw, so the pane's own clamp has run and the transcript has a length.
    a.screen(100, 24);
    assert_eq!(a.scroll, 0, "the premise: the transcript is at its tail");

    a.key(Key::PageUp);
    assert!(
        a.job_out.as_ref().unwrap().scroll > 0,
        "PageUp did not move the overlay's window"
    );
    assert_eq!(
        a.scroll, 0,
        "the transcript scrolled behind the overlay: {}",
        a.scroll
    );

    let was = a.job_out.as_ref().unwrap().scroll;
    a.screen(100, 24);
    a.key(Key::PageDown);
    assert!(
        a.job_out.as_ref().unwrap().scroll < was,
        "PageDown did not come back toward the tail"
    );
    assert_eq!(a.scroll, 0, "and the transcript stayed put");

    // **The wheel is the key the operator actually reaches for**, three lines a
    // notch and toward the beginning for a `WheelUp`.
    let before = a.job_out.as_ref().unwrap().scroll;
    a.screen(100, 24);
    a.key(Key::WheelUp);
    assert_eq!(
        a.job_out.as_ref().unwrap().scroll,
        before + 3,
        "a notch is three lines"
    );
    assert_eq!(
        a.scroll, 0,
        "the wheel scrolled the transcript behind the overlay"
    );

    // ---- the slash listing: head-origin, arrows were inverted ----
    let mut b = App::new(plain_cfg(80));
    b.slash_out = Some((
        "/notes".into(),
        (0..40).map(|i| format!("row {i}")).collect(),
    ));
    b.push_surface(Surface::Listing);
    b.screen(80, 24);
    assert_eq!(b.pane_scroll, 0);
    // Up moves toward the beginning; at the beginning it stays there.
    b.key(Key::Up);
    assert_eq!(b.pane_scroll, 0, "Up walked forward from the top");
    // Down moves into the listing, and Up comes back out of it.
    b.key(Key::Down);
    assert_eq!(b.pane_scroll, 1, "Down did not move the listing");
    b.key(Key::Down);
    assert_eq!(b.pane_scroll, 2);
    b.key(Key::Up);
    assert_eq!(b.pane_scroll, 1, "Up did not come back");
    // And what the reader sees follows, because the offset is `skip(n)`. Asserted on the text rather than
    // the number, since the number is only interesting for what it does to the glass.
    b.pane_scroll = 3;
    let shown = b.screen(80, 24).join("\n");
    assert!(!shown.contains("row 0"), "row 0 was skipped: {shown}");
    assert!(shown.contains("row 1"), "{shown}");
    // The listing's own footer, not the hint bar's `esc closes this`, which names the same key
    // on the bar for as long as the listing is up.
    assert!(
        !shown.contains("esc closes · up/down scrolls"),
        "the footer is below the window: {shown}"
    );

    // ---- and the page keys reach the listing too, which they did not ----
    let mut c = App::new(plain_cfg(80));
    c.slash_out = Some((
        "/notes".into(),
        (0..40).map(|i| format!("row {i}")).collect(),
    ));
    c.push_surface(Surface::Listing);
    c.screen(80, 24);
    assert_eq!(c.scroll, 0);
    c.key(Key::PageDown);
    assert!(c.pane_scroll > 0, "PageDown did not move the listing");
    assert_eq!(
        c.scroll, 0,
        "PageDown scrolled the transcript behind the listing"
    );
}

/// The window and the conversation scroll as one: at the window's first line an Up scrolls the
/// conversation up, and at its last line a Down scrolls it back to following.
#[test]
fn an_open_window_hands_scrolling_on_to_the_conversation_at_its_edges() {
    let mut a = app();
    let mut seq = 0u64;
    let mut push = |a: &mut App, id: &str, kind: &str, item: TranscriptItem| {
        seq += 2;
        a.apply(ServerFrame::Event(env(
            seq - 1,
            testing::appended(id, kind),
        )));
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(item),
            },
        )));
    };
    for i in 0..40 {
        push(
            &mut a,
            &format!("u.{i}"),
            "user",
            TranscriptItem::User {
                parts: vec![UserPart::Text {
                    text: format!("earlier row {i}"),
                }],
                speaker: letibot_transcript::Speaker::Operator,
            },
        );
    }
    let payload: String = (0..300).map(|n| format!("output line {n}\n")).collect();
    push(
        &mut a,
        "r.0",
        "tool_result",
        TranscriptItem::ToolResult {
            call_id: "c0".into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload,
            edit: None,
            origin: None,
            media: None,
        },
    );
    a.screen(80, 24);
    a.key(Key::CtrlV);
    a.screen(80, 24);
    let has = |a: &mut App, needle: &str| a.screen(80, 24).iter().any(|l| l.contains(needle));
    assert!(has(&mut a, "output line 0"), "the window opens at its head");
    assert!(
        !has(&mut a, "earlier row 30"),
        "the fixture starts with row 30 out of view"
    );
    // At the window's first line, Up moves the conversation: the rows above come into view.
    for _ in 0..3 {
        a.key(Key::Up);
        a.screen(80, 24);
    }
    assert_eq!(a.payload_page, 0, "the window stays at its head");
    assert!(
        has(&mut a, "earlier row 30"),
        "Up at the window's top did not scroll the conversation"
    );
    assert!(
        !a.following(),
        "the conversation is parked above the bottom now"
    );
    // At the window's last line, Down moves the conversation back down to following.
    a.key(Key::End);
    a.screen(80, 24);
    for _ in 0..10 {
        a.key(Key::Down);
        a.screen(80, 24);
    }
    assert!(has(&mut a, "output line 299"), "the window is at its end");
    assert!(
        a.following(),
        "Down past the window's end did not reach the conversation's bottom"
    );
}

/// An open result window reaches its last line, comes back at once, and fits. Down was unclamped, so the
/// offset ran past the screen and Up had to unwind it; the last page clamped to one line; and the window
/// was the budget's forty rows, so on a 24-row screen its head was above the top.
#[test]
fn an_open_result_window_reaches_its_end_comes_back_at_once_and_fits() {
    let mut a = app();
    let payload: String = (0..300).map(|n| format!("output line {n}\n")).collect();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s.0", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "c0".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload,
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    a.screen(80, 24);
    a.key(Key::CtrlV);
    let shown = |a: &mut App| {
        a.screen(80, 24)
            .into_iter()
            .filter(|l| l.contains("output line"))
            .collect::<Vec<_>>()
    };
    // It fits: the first line of the output is on the screen when it opens.
    let open = shown(&mut a);
    assert!(
        open.first().is_some_and(|l| l.ends_with("output line 0")),
        "{open:?}"
    );
    // Down past the end stops on a FULL last page that ends on the last line.
    for _ in 0..40 {
        a.key(Key::Down);
    }
    let end = shown(&mut a);
    assert!(
        end.last().is_some_and(|l| l.ends_with("output line 299")),
        "{end:?}"
    );
    assert!(end.len() > 5, "the last page is one line: {end:?}");
    // And the first Up moves.
    a.key(Key::Up);
    let back = shown(&mut a);
    assert_ne!(
        back.last(),
        end.last(),
        "Up did not move after paging past the end"
    );
    // End and Home jump; the wheel and PageDown page the window, not the transcript.
    a.key(Key::Home);
    assert!(
        shown(&mut a)
            .first()
            .is_some_and(|l| l.ends_with("output line 0"))
    );
    a.key(Key::End);
    assert!(
        shown(&mut a)
            .last()
            .is_some_and(|l| l.ends_with("output line 299"))
    );
    a.key(Key::Home);
    a.key(Key::WheelDown);
    assert!(
        shown(&mut a)
            .first()
            .is_some_and(|l| l.ends_with("output line 3")),
        "the wheel did not page the window"
    );
}

/// Folding tools must not cost the ability to scroll: with tools expanded the scroll keys stopped working
/// until a resize forced the frame.
#[test]
fn scroll_still_works_after_ctrl_t() {
    let mut a = app();
    // The tail walk is what this is about - `walk_limit = 1` is how the other
    // tail tests in this file get into it without building a megabyte.
    a.walk_limit = 1;
    // Tool results, because ctrl-t is what expands THEM: a fixture of plain
    // user rows folds to the same thing either way and cannot show this.
    let payload: String = (0..30).map(|n| format!("output line {n}\n")).collect();
    for i in 0..400u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            SessionEvent::TranscriptContent {
                item_id: format!("s.{i}"),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: format!("c{i}"),
                    name: "bash".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: payload.clone(),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    a.screen(80, 24);
    // The bug is about the TAIL walk, so the fixture has to be in it.
    assert!(
        a.hist_floor > 0,
        "this fixture never enters tail mode, so it cannot show the bug"
    );

    // Scrolling works to begin with.
    for _ in 0..5 {
        a.key(Key::Up);
        a.screen(80, 24);
    }
    let before = a.scroll;
    assert!(before > 0, "scroll did not move at all to begin with");

    // Expand the window on the newest long result. This does not unfold the conversation, so the arrows
    // page the newest payload rather than moving the transcript, which is asserted below.
    a.key(Key::CtrlV);
    a.screen(80, 24);
    assert!(
        a.payload_sel.is_some(),
        "ctrl-t did not open a payload view"
    );

    let at_refold = a.anchor.clone();
    // One press, inside the window: a Down past its last line carries on into the conversation, so
    // pressing past a 30-line payload would move the transcript by design.
    a.key(Key::Down);
    a.screen(80, 24);
    // The reader's place, not a line count: paging the payload changes that row's height, so `total` moves
    // while the row under the reader's eye has not.
    assert_eq!(
        a.anchor, at_refold,
        "the transcript moved while a payload view held the arrows"
    );
    assert!(
        a.payload_page > 0,
        "the arrows did not page the payload: {}",
        a.payload_page
    );

    // Esc gives the arrows back, and that is what this test is for: a view that held them for ever would
    // be the same defect with a new cause.
    a.key(Key::Esc);
    a.screen(80, 24);
    assert_eq!(a.payload_sel, None, "esc did not close the payload view");
    let freed = a.scroll;
    for _ in 0..5 {
        a.key(Key::Up);
        a.screen(80, 24);
    }
    assert!(
        a.scroll > freed,
        "scroll is stuck after ctrl-t: {freed} -> {}, body_len {}",
        a.scroll,
        a.body_len
    );

    // Close the window, and it still scrolls. **The chord toggles the window and
    // nothing else now** (R10), so this is one press to open and one to close.
    a.key(Key::CtrlV);
    a.screen(80, 24);
    assert!(a.payload_sel.is_some(), "ctrl-t did not open the window");
    a.key(Key::CtrlV);
    a.screen(80, 24);
    assert_eq!(a.payload_sel, None, "ctrl-t did not close the window");
    let at_collapse = a.scroll;
    for _ in 0..5 {
        a.key(Key::Up);
        a.screen(80, 24);
    }
    assert!(
        a.scroll > at_collapse,
        "scroll is stuck after collapsing back: {at_collapse} -> {}",
        a.scroll
    );
}

/// The waiting frame: a cat dead centre, walking in place. It is centred on its own row, padded to the
/// widest frame so it does not jitter sideways, moved from the clock rather than a render counter, and
/// drawn in the middle of the conversation rather than under the header.
#[test]
fn the_waiting_frame_is_a_centred_cat_that_walks_in_place() {
    let mut a = app();
    a.begin_attach_at(0);
    a.clock(0);
    let cat_row = |a: &mut App, cols: usize| -> (usize, String) {
        let f = a.screen(cols, 24);
        f.iter()
            .position(|l| l.contains("(=^"))
            .map(|i| (i, f[i].clone()))
            .expect("the cat is on the screen")
    };

    let (row, first) = cat_row(&mut a, 80);
    let cat_at = first.find("(=^").expect("the cat");
    // The **slot**, not the cat: see `CAT_SLOT`. A 7-wide and an 8-wide frame
    // cannot both be exactly centred, and the choice made here is that the slot is
    // centred and the cat sits at its left edge.
    let left = width::width(&first[..cat_at]);
    let right = 80usize.saturating_sub(left + CAT_SLOT);
    assert!(
        left.abs_diff(right) <= 2,
        "the cat's slot is not centred: {left} left, {right} right in {first:?}"
    );
    // Not pinned to the top of the conversation.
    assert!(row >= 4, "the cat is on row {row}, at the top of the frame");

    // It walks, from the clock.
    a.clock(120);
    let (_, later) = cat_row(&mut a, 80);
    assert_ne!(first.trim(), later.trim(), "the cat did not move at 120 ms");

    // And it walks **in place**: the same left margin at every frame.
    for t in [0u64, 120, 240, 360, 480, 600, 720, 840] {
        a.clock(t);
        let (_, r) = cat_row(&mut a, 80);
        let at = r.find("(=^").expect("the cat");
        assert_eq!(
            at, cat_at,
            "the cat slid across at {t} ms: {r:?} against the first frame"
        );
    }

    // Narrow screens and tiny heights must not panic or lose the chrome.
    for (w, h) in [(1usize, 6usize), (8, 6), (20, 8), (200, 60)] {
        a.clock(0);
        let f = a.screen(w, h);
        assert_eq!(f.len(), h, "{w}x{h}");
    }
}

/// The wait becomes impatient and says so: the screen said `asking the daemon for this session` for three
/// minutes with `ctrl+c exit` under it that did nothing. The keys are live now, and past
/// [`ATTACH_IMPATIENT`] the frame itself says how to get out.
#[test]
fn the_waiting_frame_names_the_way_out_once_it_has_gone_on() {
    let mut a = app();
    a.begin_attach_at(0);

    // Immediately: a cat, and no instructions. A wait that is usually over in a few
    // hundred milliseconds should not open with advice.
    a.clock(0);
    let soon = a.screen(80, 24).join("\n");
    assert!(soon.contains("(=^"), "the cat: {soon:?}");
    assert!(
        !soon.contains("not answered"),
        "advice on a wait that has just started: {soon:?}"
    );

    // Past the threshold it says so, and names the key that now works.
    a.clock(ATTACH_IMPATIENT);
    let late = a.screen(80, 24).join("\n");
    assert!(late.contains("(=^"), "the cat is still there: {late:?}");
    assert!(
        late.contains("has not answered"),
        "a long wait must say something is wrong: {late:?}"
    );
    assert!(late.contains("ctrl-c"), "and how to get out: {late:?}");
}

/// The tail frame is the full walk's frame, which is the whole licence for rendering only the end of a
/// conversation: the same rows rendered from row 0 and backward from the end must produce the same screen.
/// The forward walk is forced by raising `walk_limit`.
#[test]
fn a_tail_frame_equals_the_full_walks_frame() {
    let mut tail = app();
    let mut full = app();
    // Enough to be worth tailing, small enough to run in a test.
    for i in 0..400u64 {
        let body = format!(
            "line {i} of the conversation\n\n```rust\nfn f{i}() {{ let n = {i}; }}\n```\n\nand some prose to wrap, with `code` in it.\n"
        );
        for a in [&mut tail, &mut full] {
            a.apply(ServerFrame::Event(env(
                i * 2 + 1,
                testing::appended(&format!("s.{i}"), "user"),
            )));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("s.{i}"), &body),
            )));
        }
    }
    full.walk_limit = usize::MAX;

    // The tail path must actually have been taken, or this asserts nothing.
    tail.screen(100, 40);
    assert_eq!(
        tail.hist_floor, 0,
        "under the limit the whole thing is walked"
    );
    tail.walk_limit = 1;
    tail.hist_floor = 0;
    tail.hist_lines.clear();
    tail.hist_first_class = None;
    tail.hist_upto = 0;
    tail.hist_marks.clear();
    let t = tail.screen(100, 40);
    assert!(
        tail.hist_floor > 0,
        "the tail path was not taken, so nothing is being compared"
    );

    // And the two agree. The *whole* frame, not a slice: a tail that drew the
    // right rows with the wrong separators would pass a slice test.
    let f = full.screen(100, 40);
    assert_eq!(t, f, "the tail frame is not the full walk's frame");
}

/// The cost is what this exists for: a tail frame does not render the rows above the window. Counted and
/// not timed, since `hist_renders` is the measurement and not a proxy for one.
#[test]
fn a_tail_frame_does_not_render_the_whole_conversation() {
    let mut a = app();
    let n = 600u64;
    for i in 0..n {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(
                &format!("s.{i}"),
                "a line of conversation\n\n```rust\nfn f() {}\n```\n",
            ),
        )));
    }
    a.walk_limit = 1;
    let _ = a.screen(100, 40);
    let rendered = a.hist_renders;
    assert!(
        rendered < n,
        "the tail path rendered {rendered} rows of {n}, so it walked the lot"
    );
    // And what it rendered is bounded by the window plus its slack, not by the
    // conversation.
    let budget = (40 + TAIL_SLACK + 2) as u64;
    assert!(
        rendered <= budget,
        "rendered {rendered} rows; the window plus slack is {budget}"
    );
}

/// A frame that has drawn the tail cannot be *wrong* about the rows above it: the
/// reader is told they are there.
#[test]
fn a_tail_frame_says_there_is_more_above() {
    let mut a = app();
    for i in 0..400u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), "a line of conversation\n"),
        )));
    }
    a.walk_limit = 1;
    a.screen(100, 40);
    assert!(a.hist_floor > 0, "the tail path was not taken");
    assert!(
        a.hist_floor < a.items.len(),
        "nothing was left above the window to be counted"
    );
    // The rows above are the ones that were skipped, and the count is what a
    // scrolled-back banner needs.
    assert_eq!(
        a.hist_floor,
        a.items.len() - a.rendered_rows(),
        "the floor and the rendered rows must account for every row"
    );
}

/// Scrolling up reaches the beginning: `PageUp` moved a constant ten lines on a 40-row terminal and the
/// back-fill happened a frame after the key, so a press could never express "further up than I have drawn".
/// Page up enough and the first row is on screen.
#[test]
fn scrolling_up_reaches_the_beginning() {
    let mut a = app();
    a.walk_limit = 1;
    for i in 0..400u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("message {i}\n")),
        )));
    }
    a.screen(80, 24);
    assert!(a.hist_floor > 0, "this fixture never enters tail mode");

    // Enough pages for 400 messages on a 24-row screen, with room to spare.
    for _ in 0..80 {
        a.key(Key::PageUp);
        a.screen(80, 24);
    }
    let top = a.screen(80, 24).join("\n");
    assert!(
        top.contains("message 0"),
        "the beginning of the conversation is not reachable: {top:?}"
    );
    assert_eq!(
        a.hist_floor, 0,
        "the head reached the first row but says rows are missing above it"
    );
    // And the way back down still works.
    for _ in 0..80 {
        a.key(Key::PageDown);
        a.screen(80, 24);
    }
    assert_eq!(
        a.scroll, 0,
        "scrolling back to the tail did not land at the tail"
    );
}

/// Scrolling up renders more, on demand: a tail frame does not render the rows above the window, and the
/// clamp is computed from what is rendered, so without the back-fill there is nothing to scroll into.
#[test]
fn scrolling_up_back_fills() {
    let mut a = app();
    for i in 0..400u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), "a line of conversation\n"),
        )));
    }
    a.walk_limit = 1;
    let _ = a.screen(100, 20);
    let floor0 = a.hist_floor;
    let rows0 = a.rendered_rows();
    assert!(floor0 > 0, "the tail path was not taken");

    // Scroll up: the head must render rows it had skipped, so the floor moves down
    // and the number of rendered rows grows.
    a.scroll = 15;
    let _ = a.screen(100, 20);
    assert!(
        a.hist_floor < floor0,
        "scrolling up did not render anything above the window: floor {} -> {}",
        floor0,
        a.hist_floor
    );
    assert!(
        a.rendered_rows() > rows0,
        "the rendered rows did not grow: {rows0} -> {}",
        a.rendered_rows()
    );

    // And it is still bounded by what was asked for, not by the conversation.
    assert!(
        a.rendered_rows() <= 15 + 20 + TAIL_SLACK + 4,
        "scrolling up rendered the whole conversation"
    );

    // The floor reaches zero when the top is reached, which is the state a
    // scrolled-to-the-top frame needs to be honest about.
    a.scroll = 100_000;
    let _ = a.screen(100, 20);
    assert_eq!(
        a.hist_floor, 0,
        "scrolling to the top never reached the beginning"
    );
}

/// A change above the window is not left stale: a tail walk leaves no `hist_marks`, and "no mark" used to
/// mean "nothing to do", so the only correct answer is to throw the history away and render the tail again.
#[test]
fn a_change_above_the_window_is_not_left_stale() {
    let mut a = app();
    for i in 0..400u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), "a line of conversation\n"),
        )));
    }
    a.walk_limit = 1;
    let _ = a.screen(100, 20);
    assert!(a.hist_floor > 0, "the tail path was not taken");
    assert!(a.hist_marks.is_empty(), "a tail walk should leave no marks");

    // A row *above* the floor changes.
    a.invalidate_history_from(3);
    assert_eq!(
        a.hist_floor, 0,
        "a change above the window left the tail in place, so the changed row would \
             go on being drawn as it was"
    );
}

/// **The number R19.1 exists for**: a tail frame against a full walk, on a transcript
/// big enough to matter. Ignored because it is slow.
#[test]
#[ignore]
fn a_tail_frame_is_far_cheaper_than_a_full_walk() {
    // A row shape with real markdown in it, because the lex is the cost.
    let body = "Here is some prose with `code` and a [link](x) in it, long enough to wrap a line.\n\n\
                    ```rust\nfn f() { let n = 1; }\n```\n\nMore prose, and a list:\n\n- one\n- two\n\n";
    for rows in [400usize, 1200] {
        let mut a = app();
        for i in 0..rows as u64 {
            a.apply(ServerFrame::Event(env(
                i * 2 + 1,
                testing::appended(&format!("s.{i}"), "user"),
            )));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("s.{i}"), body),
            )));
        }
        let bytes = transcript_bytes(&a.items);

        // The full walk.
        a.walk_limit = usize::MAX;
        let t = std::time::Instant::now();
        let full = a.screen(100, 40);
        let full_ms = t.elapsed().as_secs_f64() * 1e3;

        // And the tail, from a cold history.
        a.walk_limit = 1;
        a.hist_floor = 0;
        a.hist_lines.clear();
        a.hist_first_class = None;
        a.hist_upto = 0;
        a.hist_marks.clear();
        let t = std::time::Instant::now();
        let tail = a.screen(100, 40);
        let tail_ms = t.elapsed().as_secs_f64() * 1e3;

        eprintln!(
            "{rows:>5} rows, {:.1} MB: full walk {full_ms:>7.1} ms, tail {tail_ms:>6.1} ms \
                 ({:.1}%), rendered {} rows, floor {}",
            bytes as f64 / 1e6,
            tail_ms * 100.0 / full_ms,
            a.rendered_rows(),
            a.hist_floor,
        );
        assert_eq!(tail, full, "the two frames disagree");
        assert!(tail_ms < full_ms, "the tail was not the cheaper path");
    }
}

/// **How much of an attach is the history walk.** The cold frame over a big
/// session is the number the operator waited for; the warm frames are the steady
/// state. Ignored because timing is not an assertion.
#[test]
#[ignore]
fn a_big_attach_costs_the_walk() {
    for n in [500usize, 2000, 6000] {
        let mut a = app();
        for i in 0..n {
            a.apply(ServerFrame::Event(env(
                (i * 2 + 1) as u64,
                testing::appended(&format!("s.{i}"), "user"),
            )));
            let body = format!(
                "a line of conversation {i}\n\n```rust\nfn f() {{ let n = {i}; }}\n```\n\nsome prose with `code` and a [link](x) in it, long enough to wrap a couple of times.\n"
            );
            a.apply(ServerFrame::Event(env(
                (i * 2 + 2) as u64,
                testing::content(&format!("s.{i}"), &body),
            )));
        }
        let t = std::time::Instant::now();
        let first = a.screen(100, 40);
        let cold = t.elapsed();
        let t = std::time::Instant::now();
        for _ in 0..20 {
            let _ = a.screen(100, 40);
        }
        let warm = t.elapsed() / 20;
        eprintln!(
            "{n} rows: first frame {:>8.1} ms, later frames {:>6.2} ms ({} lines)",
            cold.as_secs_f64() * 1e3,
            warm.as_secs_f64() * 1e3,
            first.len()
        );
        assert!(first.len() <= 40);
    }
}

/// The live `!` row walks the history once, not once a frame: `completions_line` runs every frame, and
/// building the candidates walks the view and parses every `bash` call. `shell_walks` is the encoder,
/// since a wall time is not something a test can assert on and a count is.
#[test]
fn the_bang_row_walks_the_history_once_not_once_a_frame() {
    let mut a = app();
    for i in 0..200u64 {
        shell_row(
            &mut a,
            i * 2 + 1,
            &format!("s.{i}"),
            "assistant",
            bash_row(&format!("cargo test {i}")),
        );
    }
    typed(&mut a, "! cargo");
    for _ in 0..20 {
        let _ = a.screen(100, 40);
    }
    assert_eq!(
        a.shell_walks, 1,
        "one walk for twenty frames of one transcript"
    );
    // **A row landing is a new walk.** The list is the transcript's and not this
    // head's, so a command that has just run has to appear in it.
    shell_row(&mut a, 1000, "a1", "assistant", bash_row("cargo build"));
    let _ = a.screen(100, 40);
    assert_eq!(a.shell_walks, 2, "and again when the rows move");
    assert!(
        a.shell_candidates().iter().any(|l| l == "! cargo build"),
        "the command that just ran is in the list: {:?}",
        a.shell_candidates()
    );
}

#[test]
fn a_row_without_a_body_draws_nothing_and_renders_its_body_when_it_lands() {
    // A body-less row is not evidence of anything: it drew a progress line claiming the conversation was
    // being carried, which every ordinary turn produces. The bar is the daemon's; a row with no body draws
    // nothing and renders once its body lands.
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::appended("s.0", "user"))));
    let waiting = a.screen(80, 12).join("\n");
    assert!(
        !waiting.contains("waiting for the body"),
        "no per-row placeholder: {waiting}"
    );
    assert!(
        !waiting.contains("carrying the conversation"),
        "and no bar — a body-less row does not name an operation: {waiting}"
    );
    a.apply(ServerFrame::Event(env(
        2,
        testing::content("s.0", "the operator's own prompt"),
    )));
    let screen = a.screen(80, 12).join("\n");
    assert!(screen.contains("the operator's own prompt"), "{screen}");
    assert!(!screen.contains("waiting for the body"), "{screen}");
}

/// Nothing on the line may move sideways except the bar's own edge: the animation and the numerator both
/// change width as the carry runs, and whatever is drawn to their right shifts. Rendered width is the
/// assertion, not the byte length.
#[test]
fn the_fork_line_keeps_its_width_as_the_cat_and_the_count_change() {
    // The premise: something on this line has to change across the sample, or a constant width proves
    // nothing. The animation is the spinner, and the numerator's width is the field the line holds still.
    let ticks: Vec<u64> = (0..8).map(|t| t * 120).collect();
    let mut frames: Vec<char> = ticks
        .iter()
        .map(|t| rano::agent::text::spinner(*t))
        .collect();
    frames.sort_unstable();
    frames.dedup();
    assert!(
        frames.len() > 1,
        "the sampled ticks must hit more than one spinner frame, or the line is \
             never redrawn in this test: {frames:?}"
    );
    assert_ne!(
        progress::thousands(0).chars().count(),
        progress::thousands(2702).chars().count(),
        "and the numerator must change width across the run, or the layout \
             below says nothing about the count either"
    );

    // **The line is the status row's now** (`ui::progress_row`): one line, label · bar ·
    // count · clock, and the rule is the same - the count must not shove the clock about.
    let mut a = app();
    let mut seen: Vec<usize> = Vec::new();
    // Across the whole run, and across a full cycle of spinner frames.
    for done in [0u64, 7, 99, 100, 999, 1000, 2701] {
        for tick in &ticks {
            a.filling = Some(("carrying".into(), "rows".into(), done, 2702));
            a.clock(*tick);
            // The clock reads the same at every sample, so what is measured is the count.
            a.filling_since = *tick;
            seen.push(rano::width::text::width(&a.status_row(100).0));
        }
    }
    seen.dedup();
    assert_eq!(
        seen.len(),
        1,
        "the line must be one width throughout; got {seen:?}"
    );
}

/// A replaced item vector cannot invert the count: a high-water mark of rows lacking a body reported
/// `peak - pending`, and after a larger carry was replaced by a smaller one the line said `97 of 4 rows`.
/// The count is now how many have arrived out of how many there are, off one walk of `items`.
#[test]
fn a_replaced_item_vector_cannot_invert_the_count() {
    let mut a = app();
    a.clock(0);
    a.apply(snapshot_hello("big", 40));
    let first = a.screen(80, 40).join("\n");
    assert!(first.contains("0/40 rows"), "{first}");

    // A second snapshot replaces the vector with a SMALLER carry. A remembered peak
    // would still say 40 here.
    a.apply(snapshot_hello("small", 4));
    let after = a.screen(80, 40).join("\n");
    assert!(
        after.contains("0/4 rows"),
        "the count follows the rows the head now holds: {after}"
    );
    assert!(
        !after.contains("/40 rows") && !after.contains("/44 rows"),
        "a replaced vector must not leave a number bigger than the whole: {after}"
    );
}

/// A fork is one line however many rows it carries, drawn from the daemon's `Filling`: announced per row
/// it was a screen of identical placeholders. The count is asserted, not just the word.
#[test]
fn a_fork_announces_three_hundred_rows_and_draws_one_progress_line() {
    let mut a = app();
    // Above `MIN_FILLING`, so the bar is drawn - that is the thing under test here.
    let n = 300u64;
    // The rows the carry announces - still body-less, all of them.
    for i in 0..n {
        a.apply(ServerFrame::Event(env(
            i + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
    }
    // **And the daemon names the operation.** This is the half that makes the line
    // honest: without it there is no bar at all, because a body-less row is not
    // evidence of a carry.
    a.apply(ServerFrame::Event(env(
        n + 1,
        SessionEvent::Filling {
            what: "carrying the conversation onto the new prompt".into(),
            unit: "rows".into(),
            done: 0,
            total: n,
        },
    )));
    let screen = a.screen(120, 40).join("\n");
    assert_eq!(
        screen.matches("arrying the conversation").count(),
        1,
        "exactly one line for the whole carry: {screen}"
    );
    assert!(
        !screen.contains("waiting for the body"),
        "and not one placeholder among them: {screen}"
    );
    assert!(screen.contains(&format!("  0/{n} rows")), "{screen}");

    // Bodies land and the daemon's count follows them.
    for i in 0..(n / 2) {
        a.apply(ServerFrame::Event(env(
            2 * n + i + 1,
            SessionEvent::Filling {
                what: "carrying the conversation onto the new prompt".into(),
                unit: "rows".into(),
                done: i + 1,
                total: n,
            },
        )));
    }
    let half = a.screen(120, 40).join("\n");
    assert!(
        half.contains(&format!("{}/{n} rows", n / 2)),
        "the numerator is what the daemon says has arrived: {half}"
    );

    // And the last tick takes the line away entirely.
    a.apply(ServerFrame::Event(env(
        4 * n + 1,
        SessionEvent::Filling {
            what: "carrying the conversation onto the new prompt".into(),
            unit: "rows".into(),
            done: n,
            total: n,
        },
    )));
    let done = a.screen(120, 40).join("\n");
    assert!(
        !done.contains("arrying the conversation"),
        "the fill is done, so nothing is drawn: {done}"
    );
}

#[test]
fn a_long_line_is_broken_by_the_head_and_not_by_the_terminal() {
    // A 200-character path in an 80-column terminal. If the head emits it long,
    // the terminal wraps it, the head's line count is wrong, and the frame
    // fights the scroll region for the rest of the session.
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::appended("s.0", "user"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::content("s.0", &"/very/long/path".repeat(20)),
    )));
    for l in a.screen(80, 24) {
        assert!(line_width(&l) <= 80, "{} cols: {l}", line_width(&l));
    }
}

/// A wheel in the subagent output view scrolls that view, and leaves the
/// conversation under it where it was.
#[test]
fn the_wheel_in_the_subagent_view_does_not_move_the_conversation_underneath() {
    let mut a = app();
    for i in 0..60u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("u.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("u.{i}"), &format!("line {i}")),
        )));
    }
    let _ = a.screen(80, 24);
    assert_eq!(a.scroll, 0, "following the stream");
    a.sub_out = Some(SubOut {
        session_id: "sub-1".into(),
        lines: (0..100).map(|i| format!("out {i}")).collect(),
        degraded: false,
        scroll: 0,
        spill: None,
        dropped: 0,
    });
    a.push_surface(Surface::SubOut);
    a.key(Key::WheelUp);
    a.key(Key::PageUp);
    // A page is a screen here too: the constant was ten, so this read 13, and the subagent view is a pager.
    // Asserted as the sum so the two keys stay distinguishable.
    let page = a.screen_rows;
    assert!(
        page > 10,
        "the fixture's screen is not tall enough to tell the two apart"
    );
    assert_eq!(
        a.sub_out.as_ref().unwrap().scroll,
        3 + page,
        "the view scrolled a wheel notch and a page"
    );
    assert_eq!(a.scroll, 0, "the conversation did not");
    a.key(Key::Esc);
    assert!(a.sub_out.is_none());
    assert_eq!(
        a.scroll, 0,
        "and coming back lands at the tail, not in the scrollback"
    );
    // A screen on top with no scroll of its own swallows the wheel.
    a.push_surface(Surface::Todos);
    a.key(Key::WheelUp);
    assert!(a.following(), "a pane on top swallowed the wheel");
    a.close_surface(Surface::Todos);
    a.key(Key::WheelUp);
    // The wheel takes the viewport off the stream, and `following` is where that state lives. It used to be
    // `scroll == 3`, and a count is what the requirement stops trusting: between a key and a paint it is
    // the previous frame's number.
    assert!(
        !a.following(),
        "with nothing on top the wheel moves the conversation"
    );
}

/// The window scrolls and the choices never leave: the viewport shrinks and scrolls so the whole diff or
/// message can be read with the options still there. `pgup`/`pgdn` are the keys, since an open card must
/// not steal Up/Down from the ladder, and the seam names them.
#[test]
fn the_card_body_scrolls_without_the_ladder_moving() {
    let mut a = tall_card(true);
    let first = a.screen(80, 24).join("\n");
    assert!(first.contains("line 0 of layer A"), "{first}");

    // A frame has been drawn, so the key handler knows there is something out of view.
    a.key(Key::PageDown);
    let scrolled = a.screen(80, 24).join("\n");
    assert!(
        !scrolled.contains("line 0 of layer A"),
        "pgdn did not move the window:\n{scrolled}"
    );
    assert!(
        scrolled.contains("lines below") || scrolled.contains("above,"),
        "the seam must say there are now two ends hidden:\n{scrolled}"
    );
    assert!(scrolled.contains("pgup/pgdn scrolls"), "{scrolled}");

    // **The ladder is where it was**, which is the point of the whole item.
    for opt in ["allow_once", "allow_session", "deny"] {
        assert!(
            scrolled.contains(opt),
            "scrolling lost `{opt}`:\n{scrolled}"
        );
    }
    assert_eq!(
        first.find("allow_once").map(|_| "in"),
        scrolled.find("allow_once").map(|_| "in"),
        "the ladder moved"
    );

    // **Walk to the end**, and the seam moves to the top of the window with it.
    for _ in 0..8 {
        a.key(Key::PageDown);
    }
    let end = a.screen(80, 24).join("\n");
    assert!(end.contains("line 39 of layer A"), "{end}");
    assert!(
        end.contains("line(s) out of view · pgup scrolls"),
        "at the tail the seam is above the window and names the way back:\n{end}"
    );
    assert!(end.contains("allow_once"), "{end}");

    // And back to the head, where it all started.
    for _ in 0..8 {
        a.key(Key::PageUp);
    }
    let home = a.screen(80, 24).join("\n");
    assert_eq!(home, first, "pgup did not come back to where it started");
}

/// A card that fits is exactly the card it always was, and the transcript keeps its own scroll: `pgdn` on a
/// short card still scrolls the conversation behind it, because a card up must not cost the transcript a key.
#[test]
fn a_card_that_fits_is_whole_and_leaves_the_transcript_its_scroll() {
    let mut a = tall_card(false);
    let whole = a.screen(80, 24).join("\n");
    assert!(
        !whole.contains("out of view"),
        "a card that fits must draw no seam:\n{whole}"
    );
    // Byte-for-byte the two halves in order: what `decision_lines` returns is what the
    // screen shows, wrapped to the same width.
    let d = a.open.first().cloned().expect("the card");
    let expected: Vec<String> = a
        .decision_lines(&d, 76)
        .into_iter()
        .flat_map(|l| wrap(&l, 76))
        .map(|l| l.trim().to_string())
        .collect();
    let shown: Vec<String> = whole
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    for e in &expected {
        assert!(
            shown.contains(e),
            "the card lost a line it used to draw: {e:?}\n{whole}"
        );
    }

    // **The transcript's scroll keys are still the transcript's** - the card's content
    // fits, so nothing here takes them.
    a.items = (0..80)
        .map(|i| SnapshotItem {
            citations: Vec::new(),
            item_id: format!("i{i}"),
            kind: "row".into(),
            ledger_head: String::new(),
            ts: 0,
            item: Some(letibot_transcript::TranscriptItem::Assistant {
                text: format!("row {i}"),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        })
        .collect();
    a.invalidate_history();
    a.screen(80, 24);
    a.key(Key::PageUp);
    // **R36: the assertion is the HOLD, not a count.** `scroll` is derived and is only
    // recomputed by a paint, so a reader of it between a key and a frame sees the
    // previous frame's number - which is why `following` is a question about the anchor.
    assert!(
        !a.following(),
        "the card stole the transcript's page key with nothing of its own to page"
    );
}

/// **A new card starts at its head.** The offset is reset in one place, so a second
/// permission cannot inherit the first one's scroll and open in the middle of its wall.
#[test]
fn a_second_card_does_not_inherit_the_first_ones_scroll() {
    let mut a = tall_card(true);
    a.screen(80, 24);
    a.key(Key::PageDown);
    a.screen(80, 24);
    assert!(a.dec_scroll > 0, "the premise: the window moved");

    // **Answer the first one**, because an open set is a queue and `open[0]` is the
    // card on the screen (§1.4: oldest first) - a second `DecisionRequested` while the
    // first is unanswered is a second card behind it, not a replacement.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::DecisionAnswered {
            req_id: "d1".into(),
            outcome: letibot_sessionlog::event::DecisionOutcome::Selected {
                option_id: "allow_once".into(),
            },
            by: letibot_sessionlog::event::Decider {
                kind: "operator".into(),
                identity: "dead".into(),
            },
            basis: "at the head".into(),
            late: false,
        },
    )));
    assert!(a.open.is_empty(), "the premise: the first card closed");
    let mut d = decision_with(&[letibot_sessionlog::event::OptionKind::AllowOnce]);
    d.req_id = "d2".into();
    d.detail = (0..40)
        .map(|i| format!("  line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::DecisionRequested {
            write_targets: Vec::new(),
            req_id: d.req_id.clone(),
            kind: d.kind.clone(),
            call_id: None,
            access: "exec".into(),
            summary: d.summary.clone(),
            target: d.target.clone(),
            detail: d.detail.clone(),
            options: d.options.clone(),
            options_note: d.options_note.clone(),
            choices: Vec::new(),
            because: String::new(),
            advice: None,
            subagent: None,
            deadline: None,
            on_timeout: d.on_timeout,
        },
    )));
    // The reset is in `screen`, where the offset is used, so the property is what the
    // *frame* does with it - the same reason the clamp lives there.
    let screen = a.screen(80, 24).join("\n");
    assert_eq!(
        a.dec_scroll, 0,
        "the new card inherited the old card's offset"
    );
    assert!(
        screen.contains("line 0"),
        "the new card opened mid-wall:\n{screen}"
    );
}

#[test]
fn the_model_picker_greens_the_providers_this_box_holds_a_key_for() {
    use letibot_sessionlog::protocol::{MODEL_KEYS_KEY, SettingRow};
    let row = |r: &str, v: &str, choices: &[&str]| SettingRow {
        key: r.into(),
        value: v.into(),
        source: String::new(),
        editable: "/models PROVIDER/MODEL".into(),
        choices: choices.iter().map(|s| (*s).to_string()).collect(),
        tools: Vec::new(),
    };
    let mut a = App::new(RenderConfig {
        width: 110,
        color: true,
        ..RenderConfig::default()
    });
    a.session_id = "s1".into();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Settings {
        rows: vec![
            row(
                "model",
                "local (glm-5.3-flash)",
                &["local", "glm-coding/glm-5.3", "deepseek/deepseek-flash"],
            ),
            row(MODEL_KEYS_KEY, "glm-coding", &[]),
        ],
    });
    assert_eq!(a.command("models"), Some(Action::Settings));
    let screen = a.screen(110, 30).join("\n");
    let line = |needle: &str| {
        screen
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no row with {needle:?} in:\n{screen}"))
            .to_string()
    };
    assert!(
        line("glm-coding/glm-5.3").contains(&sgr::GREEN.to_string()),
        "a provider this box holds a key for is not greened: {:?}",
        line("glm-coding/glm-5.3")
    );
    assert!(
        !line("deepseek/deepseek-flash").contains(sgr::GREEN),
        "a provider with no key is greened: {:?}",
        line("deepseek/deepseek-flash")
    );
    // **`local` is green for a reason of its own**, and the row is found by its number rather
    // than by the words: the colour sits between the number and the name, so a needle spanning
    // both does not exist in the bytes.
    let local_row = screen
        .lines()
        .find(|l| l.contains(" 1  ") && l.contains("local"))
        .unwrap_or_else(|| panic!("the local row is drawn:\n{screen}"))
        .to_string();
    // The cursor sits on `local`, so its green comes with the highlight's inverse.
    assert!(
        local_row.contains(sgr::GREEN) || local_row.contains("\x1b[7;32m"),
        "`local` needs no key, so it must not read as one of the unkeyed rows: {local_row:?}"
    );
    assert!(
        screen.contains("green: this box holds a key for it"),
        "the meaning is not on the card in words, so a colourless head is told nothing:\n{screen}"
    );

    // **A daemon that sends no such row greens nothing** - older than the field, and the head
    // reads an absent row as *no greening* rather than as *no keys*. `local` stays green,
    // which is its own rule and not this row's.
    let mut b = App::new(RenderConfig {
        width: 110,
        color: true,
        ..RenderConfig::default()
    });
    b.session_id = "s1".into();
    b.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    b.apply(model_settings(
        "local (glm-5.3-flash)",
        &["local", "glm-coding/glm-5.3"],
    ));
    b.command("models");
    let older = b.screen(110, 30).join("\n");
    let provider = older
        .lines()
        .find(|l| l.contains("glm-coding/glm-5.3"))
        .expect("the row is drawn")
        .to_string();
    assert!(
        !provider.contains(sgr::GREEN),
        "an absent keys row greened a provider anyway: {provider:?}"
    );
}

/// **`unclaimed_prompts` is the walk, stated as the three answers it gives.**
#[test]
fn the_claiming_walk_keeps_the_entries_the_rows_are_not_drawing() {
    let entry = |s: &str| s.to_string();
    // A row drawing `text`, in the shape the walk takes.
    let drawing = |text: &str| {
        (
            text.to_string(),
            text.split('\n').map(str::to_string).collect::<Vec<_>>(),
        )
    };
    // Nothing bound: every entry is drawn as it stands, and there is no claiming row.
    assert_eq!(
        unclaimed_prompts(&[entry("A")], &[]),
        vec![(0, "A".into(), None)]
    );
    // The whole entry is being drawn by a row: nothing for the tail.
    assert!(unclaimed_prompts(&[entry("A")], &[drawing("A")]).is_empty());
    // A piece is: the remainder, joined as one block. This is the case the whole-string
    // comparison got wrong, and the claiming row is named so the remainder can carry its mark.
    assert_eq!(
        unclaimed_prompts(&[entry("A\nB\nC")], &[drawing("A")]),
        vec![(0, "B\nC".into(), Some("A".into()))]
    );
    // **A blank line in an entry claims nothing**, so an entry whose words are all claimed but
    // which carries a blank is not drawn as a row of nothing.
    assert!(unclaimed_prompts(&[entry("A\n")], &[drawing("A")]).is_empty());
    // **A line is spent once.** Two entries that read the same, and one line on screen: the
    // first claims it, the second keeps its own.
    assert_eq!(
        unclaimed_prompts(&[entry("A"), entry("A")], &[drawing("A")]),
        vec![(1, "A".into(), None)]
    );
    // And an entry the rows say nothing about is drawn whole, beside one they do.
    assert_eq!(
        unclaimed_prompts(&[entry("A\nB"), entry("C")], &[drawing("A")]),
        vec![(0, "B".into(), Some("A".into())), (1, "C".into(), None)]
    );
}

/// The marker joins the prose even when the tail walk rendered it first: `fill_backward` renders the newest
/// rows first and stops on a line budget, so it reached the marker, broke, and never built the narration
/// above it - which was drawn as a row of its own with the colon dangling. Asserted through the tail path.
#[test]
fn the_marker_joins_the_prose_the_tail_walk_rendered_before_it() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // Enough prose above that the fill stops short of it, and the narration last.
    for i in 0..30u64 {
        let id = format!("p.{i}");
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            SessionEvent::TranscriptAppended {
                item_id: id.clone(),
                kind: "assistant".into(),
                ledger_head: String::new(),
            },
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            SessionEvent::TranscriptContent {
                item_id: id,
                item: Box::new(TranscriptItem::Assistant {
                    text: format!("paragraph {i} of the narration,"),
                    tool_calls: Vec::new(),
                    truncated: false,
                }),
            },
        )));
    }
    let seq = 100u64;
    a.apply(ServerFrame::Event(env(
        seq,
        SessionEvent::TranscriptAppended {
            item_id: "n.0".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        seq + 1,
        SessionEvent::TranscriptContent {
            item_id: "n.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "Now I'll write the implementation. First the helpers:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    for i in 0..8u64 {
        let id = format!("c.{i}");
        a.apply(ServerFrame::Event(env(
            seq + 2 + i * 2,
            testing::appended(&id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            seq + 3 + i * 2,
            SessionEvent::TranscriptContent {
                item_id: id,
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: format!("k{i}"),
                    name: "bash".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!("output {i}"),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    a.visibility = Visibility::of(Profile::CONVERSATION);
    // The tail path, with the fill's budget already satisfied: `fill_backward(1)` is the fill made for a
    // reader one line short, so the walk stops the moment it has a line, which is the marker's own.
    a.walk_limit = 1;
    let _ = a.screen(120, 24);
    // The reader-one-line-short state, set exactly: the floor is up at the end of the session, the buffer
    // is empty, and the fill is asked for one line.
    a.hist_floor = a.items.len();
    a.hist_upto = a.items.len();
    a.hist_lines.clear();
    a.spans.clear();
    a.fill_backward(1);
    let screen = a.screen(120, 24).join("\n");
    let line = screen
        .lines()
        .find(|l| l.contains("First the helpers:"))
        .unwrap_or_else(|| panic!("the narration is not on the screen:\n{screen}"));
    assert!(
        line.trim_end().ends_with("helpers: [8 tool calls]"),
        "the marker did not join the sentence above it:\n{screen}"
    );
}

#[test]
fn a_turn_starting_does_not_shove_the_transcript_up_a_row() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // Enough rows to fill the screen, which is what makes the shove visible: once the transcript is
    // bottom-anchored, a chrome row takes its space and every content row slides up by one. Three rows sat
    // at the top and passed against the reverted fix.
    for i in 0..30u64 {
        let (seq, id, text) = (
            i * 2 + 1,
            format!("s.{i}"),
            format!("row {i} of the transcript"),
        );
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(&id, "assistant"),
        )));
        a.record_item(
            &id,
            TranscriptItem::Assistant {
                text,
                tool_calls: Vec::new(),
                truncated: false,
            },
        );
    }
    // **Idle first**, which is the frame the row has to be reserved in: nothing is running, so
    // the status text is empty and only the reservation puts a row there.
    let idle = a.screen(100, 24);
    assert!(
        idle.iter().any(|l| l.contains("row 29 of the transcript")),
        "the newest row is on the screen: {idle:#?}"
    );
    assert!(
        !idle.iter().any(|l| l.contains("row 0 of the transcript")),
        "and the oldest has scrolled off, so the view is bottom-anchored: {idle:#?}"
    );
    let row_of = |v: &[String], what: &str| v.iter().position(|l| l.contains(what));

    // **And a turn starts**, which is the moment the shove happened.
    a.apply(ServerFrame::Event(env_at(
        7,
        1_000,
        testing::turn_started("t1"),
    )));
    let running = a.screen(100, 24);
    assert!(
        running.iter().any(|l| l.contains("Responding")),
        "the turn's row is drawn while it runs: {running:#?}"
    );
    for what in ["row 25 of the transcript", "row 29 of the transcript"] {
        assert_eq!(
            row_of(&idle, what),
            row_of(&running, what),
            "{what:?} moved when the turn started — the reserved row is what stops this: \
                 \nidle:\n{}\nrunning:\n{}",
            idle.join("\n"),
            running.join("\n")
        );
    }
}

#[test]
fn the_wheel_scrolls_and_wheeling_back_follows_the_stream() {
    let mut a = app();
    for i in 0..40u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("line {i}")),
        )));
    }
    a.screen(80, 24);
    assert_eq!(a.key(Key::WheelUp), None);
    assert_eq!(a.key(Key::WheelUp), None);
    // A notch is three body lines, because one line of body can be two screen
    // rows after wrapping and a notch that moves one wrapped row reads as
    // nothing happened.
    a.screen(80, 24);
    assert!(
        !a.following(),
        "two notches up left the reader on the stream"
    );
    // **Six lines above the bottom**, which is the measurement the count was standing
    // in for: two notches of three, where a notch is three lines because one body line
    // can be two screen rows after wrapping.
    assert_eq!(
        a.view_top,
        a.body_len - a.view_room - 6,
        "two notches of three lines did not move the window six lines"
    );
    assert_eq!(a.key(Key::WheelDown), None);
    assert_eq!(a.key(Key::WheelDown), None);
    assert!(
        a.following(),
        "wheeling back to the bottom follows the stream"
    );
}

/// A wheel notch down walks three lines, and a run of them is what reaches the tail: one notch clearing the
/// anchor outright made the conversation between the two positions unreachable, so a notch walks like a
/// notch up and arriving at the tail resumes following. Esc and the parked `↓` still mean it in one press.
#[test]
fn a_wheel_notch_down_walks_and_a_run_of_notches_is_what_reaches_the_tail() {
    let mut a = app();
    let rows = |a: &mut App, from: u64, to: u64| {
        for i in from..to {
            a.apply(ServerFrame::Event(env(
                i * 2 + 1,
                testing::appended(&format!("s.{i}"), "user"),
            )));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("s.{i}"), "a line of conversation"),
            )));
        }
    };
    rows(&mut a, 0, 60);
    a.screen(80, 24);
    assert!(
        a.following(),
        "the premise: the reader starts on the stream"
    );

    // Park it the way a wheel does, and check it really is parked.
    a.key(Key::WheelUp);
    a.screen(80, 24);
    assert!(!a.following(), "the notch parked the reader");

    // **And the stream keeps arriving while the reader is parked.**
    rows(&mut a, 60, 90);
    a.screen(80, 24);
    let above = a.view_top;
    // Drain the flag, so what is asserted below is about the notch rather than about anything
    // the thirty rows did - the head's loop reads it at the top of every pass.
    let _ = a.take_redraw();

    // **One notch down is a STEP, not the tail** - the opposite of what this test asserted
    // before, and the whole of the reversal.
    assert_eq!(a.key(Key::WheelDown), None);
    a.screen(80, 24);
    assert!(
        !a.following(),
        "one notch down jumped to the bottom — that is the `effectively like Esc` report, and \
             it is what leaves the reader unable to walk down through the conversation"
    );
    assert_eq!(
        a.view_top,
        above + 3,
        "a notch down is three lines of conversation, not a jump to the end"
    );
    assert!(
        !a.take_redraw(),
        "the notch asked for the glass to be thrown away — that is the erase the flicker fix \
             removed from the scroll path"
    );

    // **A RUN of notches is what returns the reader to the live stream**, because arriving at
    // the bottom is the one act that resumes following. Bounded, so a walk that never gets
    // there fails here rather than looping.
    let mut notches = 1;
    while !a.following() && notches < 40 {
        assert_eq!(a.key(Key::WheelDown), None);
        a.screen(80, 24);
        notches += 1;
    }
    assert!(
        a.following(),
        "{notches} notches down never reached the tail of a stream that had stopped growing"
    );
    assert!(
        notches > 1,
        "one notch was the tail again — the one-notch jump is back"
    );
    assert_eq!(
        a.scroll, 0,
        "and the count agrees with the anchor, because the two are one state"
    );
}

/// Three notches down walk three lines each, and only the one that arrives follows: the parked state
/// survives every notch short of the bottom, so nothing between the two positions is unreachable.
#[test]
fn three_notches_down_walk_three_lines_each_and_only_arriving_follows() {
    let mut a = app();
    for i in 0..40u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("line {i}")),
        )));
    }
    a.screen(80, 24);
    // Drain the flag, so the assertions below are about the notches and not about the fixture.
    let _ = a.take_redraw();
    let bottom = a.body_len - a.view_room;

    // Park nine lines up - three notches' worth - so the walk down has somewhere to go.
    for _ in 0..3 {
        a.key(Key::WheelUp);
        a.screen(80, 24);
    }
    assert!(!a.following(), "three notches up parked the reader");
    assert_eq!(
        a.view_top,
        bottom - 9,
        "three notches up did not walk nine lines"
    );

    // **The walk down, three lines a notch**, and every notch short of the bottom leaves the
    // reader parked exactly where the notch put them.
    for (n, want) in [(1, bottom - 6), (2, bottom - 3)] {
        assert_eq!(a.key(Key::WheelDown), None);
        a.screen(80, 24);
        assert!(
            !a.following(),
            "notch {n} down was still above the bottom and left the stream"
        );
        assert_eq!(a.view_top, want, "notch {n} down did not walk three lines");
        assert!(
            !a.take_redraw(),
            "notch {n} down asked for the glass to be thrown away"
        );
    }

    // **And the third one arrives, which is the one act that resumes following.**
    assert_eq!(a.key(Key::WheelDown), None);
    a.screen(80, 24);
    assert!(
        a.following(),
        "the notch that reached the bottom did not resume following"
    );
    assert_eq!(a.scroll, 0, "and the count agrees with the anchor");
    assert!(
        !a.take_redraw(),
        "the arriving notch asked for the glass to be thrown away"
    );
}

#[test]
fn scrolling_to_the_top_leaves_a_full_screen_rather_than_a_blank_one() {
    // Found by pressing PageUp six times under tmux: the scroll was clamped to `total - 1`, so the top of
    // the history was a one-line window, and the banner that overwrites the last line left the screen blank.
    let mut a = app();
    for i in 0..40u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("line {i}")),
        )));
    }
    // A frame first: `body_len` is what `PageUp` clamps against and it is only
    // known once something has been laid out.
    a.screen(80, 24);
    for _ in 0..30 {
        a.key(Key::PageUp);
        a.screen(80, 24);
    }
    let screen = a.screen(80, 24);
    let filled = screen.iter().filter(|l| !l.trim().is_empty()).count();
    assert!(
        filled > 6,
        "scrolled to the top and the screen is empty:\n{}",
        screen.join("\n")
    );
    assert!(
        screen.iter().any(|l| l.contains("line 0")),
        "{}",
        screen.join("\n")
    );
}

/// A park far up on a stream that keeps arriving, and the wheel still gets the reader back: a notch down
/// walks three lines and arriving at the tail resumes following.
#[test]
fn a_deep_park_on_a_living_stream_is_reachable_from_the_wheel() {
    let mut a = app();
    a.walk_limit = 1;
    let mut seq = 0u64;
    let mut rows = |a: &mut App, n: u64| {
        for _ in 0..n {
            seq += 2;
            a.apply(ServerFrame::Event(env(
                seq - 1,
                testing::appended(&format!("s.{seq}"), "user"),
            )));
            a.apply(ServerFrame::Event(env(
                seq,
                testing::content(&format!("s.{seq}"), "a line of conversation"),
            )));
        }
    };
    rows(&mut a, 400);
    a.screen(80, 24);
    assert!(
        a.hist_floor > 0,
        "the tail path was not taken, so this is not the park"
    );

    // Park the way the operator did: wheel up until the banner counts 190 lines below.
    while a.scroll < 190 {
        a.key(Key::WheelUp);
        a.screen(80, 24);
    }
    let parked = a.scroll;
    assert!(
        parked >= 190,
        "the wheel-up walk never got 190 lines up: {parked}"
    );

    // The stream keeps arriving while the reader wheels down, two rows a notch at a monitor's pace; four
    // notches a burst, eight bursts, bounded so a failure fails rather than loops.
    let mut notches = 0usize;
    for _ in 0..8 {
        for _ in 0..4 {
            rows(&mut a, 2);
            a.key(Key::WheelDown);
            a.screen(80, 24);
            notches += 1;
        }
        a.clock(a.now_ms + 200);
        if a.following() {
            break;
        }
    }
    assert!(
        a.following(),
        "{notches} notches down in eight flicks never reached the tail of a stream that \
             kept growing — parked {parked} lines up, ended {} lines up. `esc` should not be \
             the only way back from a deep park",
        a.scroll
    );
}

/// Scrolled back, the prompt of the turn on screen stays pinned at the top, one line: it is the person's
/// prompt that opened the turn the window's top is in, cut to one line, and only while the view is held and
/// the prompt has scrolled off.
#[test]
fn scrolled_back_the_turns_prompt_is_pinned_on_top() {
    use letibot_transcript::{TranscriptItem, UserPart};
    let mut a = app();
    a.session_id = "s".into();
    let mut seq = 0u64;
    let mut row = |a: &mut App, id: &str, kind: &str, item: TranscriptItem| {
        seq += 1;
        a.apply(ServerFrame::Event(env(seq, testing::appended(id, kind))));
        seq += 1;
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(item),
            },
        )));
    };
    for t in 0..3 {
        let prompt = format!(
            "PROMPT-{t} please look at the reader and tell me why it drops the last record \
             when the file has no trailing newline, and fix it with a test"
        );
        row(
            &mut a,
            &format!("u{t}"),
            "user",
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![UserPart::Text { text: prompt }],
            },
        );
        let answer: String = (0..40)
            .map(|i| format!("answer {t} line {i}\n\n"))
            .collect();
        row(
            &mut a,
            &format!("a{t}"),
            "assistant",
            TranscriptItem::Assistant {
                text: answer,
                tool_calls: vec![],
                truncated: false,
            },
        );
    }
    let (w, h) = (80, 24);
    let following = a.screen(w, h);
    assert!(
        !following.iter().any(|l| l.contains("PROMPT-")),
        "following the tail, nothing is pinned: {following:#?}"
    );
    // Up into the middle of turn 1's answer.
    while !a
        .screen(w, h)
        .iter()
        .any(|l| l.contains("answer 1 line 20"))
    {
        a.key(Key::PageUp);
    }
    let screen = a.screen(w, h);
    let pinned = &screen[1];
    assert!(
        pinned.contains("PROMPT-1"),
        "the top body row is the turn's prompt: {screen:#?}"
    );
    assert!(pinned.contains('…'), "cut to one line: {pinned:?}");
    assert!(
        !screen[2].contains("tell me why"),
        "one line, not the wrapped prompt: {screen:#?}"
    );
    // Further up, into turn 0: the pin follows the turn.
    while !a
        .screen(w, h)
        .iter()
        .any(|l| l.contains("answer 0 line 20"))
    {
        a.key(Key::PageUp);
    }
    let screen = a.screen(w, h);
    assert!(screen[1].contains("PROMPT-0"), "{screen:#?}");
    // At the very top the prompt is itself on screen, so it is not drawn twice.
    for _ in 0..40 {
        a.key(Key::PageUp);
    }
    let screen = a.screen(w, h).join("\n");
    assert_eq!(screen.matches("PROMPT-0").count(), 1, "{screen}");
}

// A carry: the rows are replaced and the place is not. A re-seat forks the transcript, so every row
// arrives under a new id and a viewport named by an id has nothing to hold; the words are what both
// sides agree on, and a bare offset must never cross a transcript boundary.

/// The conversation the fixtures scroll in: `n` rows, each saying `row {i} says a thing`.
fn forty_rows(a: &mut App, n: u64) {
    for i in 0..n {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i} says a thing")),
        )));
    }
}

/// A head in that conversation with the viewport scrolled up - six notches, which on this
/// fixture leaves the reader holding `s.19` with nothing above it in the window.
fn scrolled_up(n: u64) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    forty_rows(&mut a, n);
    a.screen(100, 30);
    for _ in 0..6 {
        a.key(Key::WheelUp);
        a.screen(100, 30);
    }
    assert!(!a.following(), "the fixture never scrolled");
    a
}

/// A snapshot of the rows a carry delivers: `ids` in order, each body from `text(i)`. The ids are the
/// fixture's point, since a carry's rows arrive under the new transcript's.
fn carried_snapshot(ids: &[String], text: impl Fn(usize) -> String) -> Snapshot {
    let mut snap = Hub::new("s").snapshot();
    snap.seq = 999;
    snap.items = ids
        .iter()
        .enumerate()
        .map(|(i, id)| SnapshotItem {
            citations: Vec::new(),
            item_id: id.clone(),
            kind: "assistant".into(),
            ledger_head: "beef".into(),
            ts: 0,
            item: Some(TranscriptItem::Assistant {
                text: text(i),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        })
        .collect();
    snap
}

/// The words a row says, for an assertion - `row_words` is the head's own reading, so a test
/// asking *what is on this row* asks it the way the carry does.
fn words_at(a: &App, row: usize) -> String {
    a.items
        .get(row)
        .and_then(row_words)
        .unwrap_or_else(|| panic!("no words at row {row}"))
}

/// A carry that lengthens the transcript leaves the reader on their row, not their index: the fork's note
/// goes in front of the rows it carried, so the integer the viewport held names a different row.
#[test]
fn a_carry_that_lengthens_the_transcript_leaves_the_reader_on_their_row() {
    let mut a = scrolled_up(40);
    let held = a.anchor.clone().expect("a held row");
    let was = words_at(&a, held.ordinal);
    assert_eq!(was, "row 19 says a thing", "the fixture's held row moved");

    // The carry: the conversation carried across under the new transcript's ids, with the
    // re-seat's own note in front of it and twenty rows arrived after it.
    let ids: Vec<String> = (0..62).map(|i| format!("t.{i}")).collect();
    let snap = carried_snapshot(&ids, |i| match i {
        0 => "This conversation was re-seated onto a new prompt.".to_string(),
        1 => "The tool list was rebuilt.".to_string(),
        _ if i < 42 => format!("row {} says a thing", i - 2),
        _ => format!("row {} arrived after the re-seat", i - 2),
    });
    a.apply(hello("s", vec![brief("s", "one", false)], snap));
    let frame = a.screen(100, 30);

    // **The index is not the place.** Row `held.ordinal` in the new transcript is two rows
    // above the reader's, and it is a different row - which is what the old fallback would
    // have held, silently, as though nothing had happened.
    assert_ne!(
        words_at(&a, held.ordinal),
        was,
        "the fixture does not shift the rows, so this test could not tell a row from an index"
    );
    // **The row is.** Two rows down, under a new id, saying what it said.
    let now = a.anchor.clone().expect("the reader is still holding a row");
    assert_eq!(
        now.ordinal,
        held.ordinal + 2,
        "the viewport did not move with the row"
    );
    assert_eq!(
        now.item_id, "t.21",
        "and it is holding the new transcript's id"
    );
    assert_eq!(
        words_at(&a, now.ordinal),
        was,
        "and the row it holds says the same thing"
    );
    // **And the reader is looking at it.** The frame is the contract, not the field.
    let screen = frame.join("\n");
    assert!(
        screen.contains(&was),
        "the row the reader was reading is not on the screen: {screen}"
    );
    // **Nothing was said, because nothing was lost.** A row that is found again is not a row
    // that went missing, and the old code announced it as lost on the way past.
    assert!(
        a.notice.is_none(),
        "a row that was carried across is not news: {:?}",
        a.notice
    );
    assert!(
        !a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "anchor_lost")),
        "and it is not filed as lost either"
    );
}

/// A row's words are what the carry matches it by, and the two readers are one rule: [`row_words`] builds
/// the string and [`row_says`] compares against it per row. A `User` row is often several parts, and a match
/// reading only the first would lose every merged row.
#[test]
fn a_rows_words_are_what_the_carry_matches_it_by() {
    fn row(item: Option<TranscriptItem>, kind: &str) -> SnapshotItem {
        SnapshotItem {
            citations: Vec::new(),
            item_id: "x".into(),
            kind: kind.into(),
            ledger_head: "beef".into(),
            ts: 0,
            item,
        }
    }

    let merged = row(
        Some(TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![
                UserPart::Text {
                    text: "first".into(),
                },
                UserPart::Text {
                    text: "second".into(),
                },
            ],
        }),
        "user",
    );
    let words = row_words(&merged).expect("a row with words");
    assert_eq!(
        words, "first\nsecond",
        "the engine's JOIN is the row's words"
    );
    assert!(row_says(&merged, &words), "and the row says them back");
    // **The whole of it, and not a piece.** A row is found by what it says entire - a
    // prefix match would put the reader on the wrong row of a conversation that repeats
    // itself, which is the residue this match has to refuse.
    assert!(!row_says(&merged, "first"), "a piece is not the row");
    assert!(
        !row_says(&merged, "first\nsecond\nthird"),
        "nor is more than it"
    );
    assert!(!row_says(&merged, "second"), "nor is a later part alone");

    // **A body-less row has no words**, which is what the carry reports rather than
    // guessing: an empty string would match the first empty row in the next transcript.
    let silent = row(None, "assistant");
    assert_eq!(
        row_words(&silent),
        None,
        "nothing has arrived to be found by"
    );
    assert!(!row_says(&silent, ""), "and an empty row says nothing");
    // A row that arrived and says nothing is the same answer, for the same reason.
    let blank = row(
        Some(TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: Vec::new(),
            truncated: false,
        }),
        "assistant",
    );
    assert_eq!(row_words(&blank), None);
    assert!(!row_says(&blank, ""));
}

/// A fork that arrives as appends is not a transcript boundary, and the head says nothing about it: the
/// rows the head already holds are still there under their own ids, so the boundary rule has nothing to fire
/// on. How many rows it holds afterwards is deliberately not asserted, since that number is the doubling.
#[test]
fn a_fork_that_arrives_as_appends_leaves_the_view_where_it_was_and_says_nothing() {
    let mut a = scrolled_up(40);
    let held = a.anchor.clone().expect("a held row");
    let was_top = a.view_top;

    // The fork, as an attached head receives it: the warning that says it happened, then
    // every carried row re-announced under the new transcript's ids, bodies and all.
    a.apply(ServerFrame::Event(env(
        1000,
        SessionEvent::Warning {
            code: "reseated".into(),
            detail: "re-seated".into(),
            compaction: None,
        },
    )));
    for i in 0..60u64 {
        a.apply(ServerFrame::Event(env(
            1001 + i * 2,
            testing::appended(&format!("t.{i}"), "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            1002 + i * 2,
            SessionEvent::TranscriptContent {
                item_id: format!("t.{i}"),
                item: Box::new(TranscriptItem::Assistant {
                    text: format!("row {i} says a thing"),
                    tool_calls: Vec::new(),
                    truncated: false,
                }),
            },
        )));
    }
    a.screen(100, 30);

    // **The row is still here under its own id.** Nothing was re-anchored, and nothing
    // needed to be: this path never crosses the boundary the rule is about.
    assert_eq!(
        a.anchor,
        Some(held.clone()),
        "the view moved off a row that is still in the transcript"
    );
    assert_eq!(a.view_top, was_top, "and the window moved with it");
    assert!(
        a.carry.is_none(),
        "a carry was taken with no snapshot to take it from"
    );
    assert!(a.notice.is_none(), "and nothing was said: {:?}", a.notice);
    assert!(
        !a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "anchor_lost")),
        "and nothing was filed as lost either"
    );
}

/// A carry that does not carry the row goes to the tail and says so: the ordinal was measured in the
/// transcript that went, and a reader who was reading is never moved in silence.
#[test]
fn a_carry_that_drops_the_row_follows_the_tail_and_says_so() {
    let mut a = scrolled_up(40);
    let held = a.anchor.clone().expect("a held row");
    let was = words_at(&a, held.ordinal);

    // The compaction's carry: a summary, then the recent tail - the reader's row is in the
    // part that was summarised, and it is nowhere in what arrives.
    let ids: Vec<String> = (0..20).map(|i| format!("t.{i}")).collect();
    let snap = carried_snapshot(&ids, |i| match i {
        0 => "This conversation was compacted: what was said before this point is replaced by \
              the summary below."
            .to_string(),
        _ => format!("row {} says a thing", i + 19),
    });
    a.apply(hello("s", vec![brief("s", "one", false)], snap));
    let frame = a.screen(100, 30);

    // **The reader's row is not in the new transcript**, so the place cannot be held - and
    // the fixture says so rather than leaving it to the code: no row here says `was`.
    assert!(
        !a.items.iter().any(|it| row_says(it, &was)),
        "the fixture carries the reader's row, so this test is not about a lost one"
    );
    assert!(
        a.anchor.is_none(),
        "a row that is not in the transcript must not be held by index: {:?}",
        a.anchor
    );
    assert!(
        a.following(),
        "and the view follows the tail, which is where it went"
    );

    // **And it is SAID** - on the screen, where the reader is looking, and named for the row
    // so the sentence is checkable rather than a shrug.
    let said = a
        .notice
        .as_deref()
        .expect("a reader who was moved must be told, not moved in silence");
    assert!(
        said.contains("not in the transcript that replaced it"),
        "the sentence says what happened: {said}"
    );
    assert!(
        said.contains("follows the tail again"),
        "and where the reader is now: {said}"
    );
    assert!(
        said.contains(&held.item_id),
        "and names the row that was not carried: {said}"
    );
    // **The durable half too**, so `/notes` can answer *why did my view move* after the
    // transient line has gone.
    let noted = a
        .notes
        .iter()
        .find_map(|(_, n)| match n {
            Note::Warned(w) if w.code == "anchor_lost" => Some(w.detail.clone()),
            _ => None,
        })
        .expect("a row that is gone is filed as well as said");
    assert!(
        noted.contains(&held.item_id),
        "the note names the row: {noted}"
    );

    // **And the tail is what is on the screen**: the last row the new transcript carries.
    let screen = frame.join("\n");
    assert!(
        screen.contains("row 38 says a thing"),
        "the view did not go to the tail: {screen}"
    );
    assert!(
        !screen.contains(&was),
        "and it is not showing a row the new transcript does not have: {screen}"
    );
}

/// A carry that has not finished arriving has not lost the row: a fork announces every row before a body
/// follows, so a row that cannot be matched yet is not gone, and the window holds its line until it lands.
#[test]
fn a_carry_whose_bodies_are_still_coming_holds_the_window_and_then_the_row() {
    let mut a = scrolled_up(40);
    let held = a.anchor.clone().expect("a held row");
    let was = words_at(&a, held.ordinal);
    let was_top = a.view_top;

    // The carry, announced: sixty rows under new ids and not one body yet.
    let ids: Vec<String> = (0..60).map(|i| format!("t.{i}")).collect();
    let mut snap = carried_snapshot(&ids, |i| format!("row {i} says a thing"));
    for it in snap.items.iter_mut() {
        it.item = None;
    }
    a.apply(hello("s", vec![brief("s", "one", false)], snap));
    a.screen(100, 30);

    // **Nothing has been decided and nothing has been said**: the row is named, not lost.
    assert_eq!(
        a.anchor.as_ref().map(|h| h.item_id.as_str()),
        Some(held.item_id.as_str()),
        "a carry with no bodies yet moved the viewport"
    );
    assert!(a.carry.is_some(), "and the carry is still in flight");
    assert!(
        a.bulk.is_some(),
        "the fixture announced rows with no bodies"
    );
    assert!(
        a.notice.is_none(),
        "a row that may still be coming is not announced as gone: {:?}",
        a.notice
    );

    // The bodies, the way the daemon publishes them - every one but the reader's, and the
    // ones below it first, so the buffer grows under the held window before the row that
    // names it has anything to match.
    let body = |i: u64| SessionEvent::TranscriptContent {
        item_id: format!("t.{i}"),
        item: Box::new(TranscriptItem::Assistant {
            text: format!("row {i} says a thing"),
            tool_calls: Vec::new(),
            truncated: false,
        }),
    };
    for i in (20..60u64).chain(0..19) {
        a.apply(ServerFrame::Event(env(2000 + i, body(i))));
        a.screen(100, 30);
    }

    // **The window is still where the reader left it**, and the row is still only named:
    // fifty-nine bodies have landed and not one of them is the reader's.
    assert!(a.carry.is_some(), "the reader's row has no body yet");
    assert_eq!(
        a.anchor.as_ref().map(|h| h.item_id.as_str()),
        Some(held.item_id.as_str()),
        "a row that cannot be matched yet was decided about anyway"
    );
    assert_eq!(
        a.view_top, was_top,
        "the window walked down the arriving content instead of holding the line the reader \
         was at"
    );
    assert!(a.notice.is_none(), "and nothing was said: {:?}", a.notice);

    // **The reader's own row lands**, and it is placed under the new transcript's id - at the
    // line it was at, because the conversation carried across is the same one.
    a.apply(ServerFrame::Event(env(2100, body(19))));
    let frame = a.screen(100, 30);
    let now = a
        .anchor
        .clone()
        .expect("the reader is holding the row again");
    assert_eq!(now.item_id, "t.19", "placed under the new transcript's id");
    assert_eq!(
        words_at(&a, now.ordinal),
        was,
        "and it is the row they were on"
    );
    assert!(a.carry.is_none(), "and the carry is spent");
    assert!(a.bulk.is_none(), "every announced row has its body");
    assert_eq!(
        a.view_top, was_top,
        "the viewport moved for a row that did not"
    );
    assert!(
        a.notice.is_none(),
        "the row was carried, so there is nothing to say: {:?}",
        a.notice
    );
    assert!(
        frame.join("\n").contains(&was),
        "and the row is on the screen"
    );
}

// A live fork: the rows are replaced under an attached head, and a `/reseat` on a live session sends
// no snapshot - the carried rows arrive as appends and the boundary is stated after them - so a carry
// taken only in `load` is one a live fork never takes, and the ordinal fallback lands one row off.

/// The carried rows, published the way the daemon publishes them: an announcement, and then
/// its body, under the **new** transcript's ids.
fn carried_appends(a: &mut App, seq: u64, ids: &[String], text: impl Fn(usize) -> String) {
    for (i, id) in ids.iter().enumerate() {
        a.apply(ServerFrame::Event(env(
            seq + (i as u64) * 2,
            testing::appended(id, "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            seq + (i as u64) * 2 + 1,
            SessionEvent::TranscriptContent {
                item_id: id.clone(),
                item: Box::new(TranscriptItem::Assistant {
                    text: text(i),
                    tool_calls: Vec::new(),
                    truncated: false,
                }),
            },
        )));
    }
}

/// **A fork as an ATTACHED head receives one**: the rows, and then the statement that names
/// the boundary - `harness.rs`'s own order, which is the one a live `/reseat` is.
fn fork_live(
    a: &mut App,
    seq: u64,
    parent: &str,
    new: &str,
    ids: &[String],
    text: impl Fn(usize) -> String,
) {
    carried_appends(a, seq, ids, text);
    a.apply(ServerFrame::Event(env(
        seq + (ids.len() as u64) * 2,
        testing::forked(new, parent),
    )));
}

/// **A fork re-bases the window on the transcript the session now speaks.** The head kept the rows
/// before the first one that went, and `items_base` with them, so after a compaction the ordinal it
/// asked a page for named a row of the transcript the fork replaced.
#[test]
fn a_fork_re_bases_the_window_on_the_transcript_it_left() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A head that attached to a window the daemon had trimmed 500 rows of, holding rows of the
    // transcript behind the one the session was speaking.
    a.items_base = 500;
    let ancestor: Vec<String> = (0..5).map(|i| format!("s#t0.{i}")).collect();
    carried_appends(&mut a, 1, &ancestor, |i| format!("row {i}"));
    let speaking: Vec<String> = (0..15).map(|i| format!("s#t1.{i}")).collect();
    carried_appends(&mut a, 100, &speaking, |i| format!("row {i}"));
    a.walk_limit = 1;
    let _ = a.screen(100, 20);

    // The compaction: the conversation carried under the new transcript's ids, then the statement.
    let carried: Vec<String> = (0..12).map(|i| format!("s#t2.{i}")).collect();
    fork_live(&mut a, 1000, "s#t1", "s#t2", &carried, |i| {
        format!("row {i}")
    });
    let _ = a.screen(100, 20);

    assert_eq!(
        a.items_base, 0,
        "the window's base still names an ordinal of the transcript the fork replaced"
    );
    let held: Vec<&str> = a.items.iter().map(|r| r.item_id.as_str()).collect();
    assert!(
        !held.iter().any(|id| id.starts_with("s#t1.")),
        "a row of the transcript the fork replaced is still held: {held:?}"
    );
    // And the rows the resume republished above the base are still here, above it: the reader
    // draws them, and no ordinal names them.
    assert_eq!(
        a.items_lead, 5,
        "the head does not know where the base begins: {held:?}"
    );
    assert_eq!(
        a.items[a.items_lead].item_id, "s#t2.0",
        "the base does not start at the first row of the new transcript"
    );
}

/// A live fork leaves the reader on the row they were on: its id is gone and its ordinal names a different
/// row, so the two things left are its own words, which is what a carry takes.
#[test]
fn a_live_fork_leaves_the_reader_on_the_row_they_were_on() {
    let mut a = scrolled_up(40);
    let held = a.anchor.clone().expect("a held row");
    let was = words_at(&a, held.ordinal);
    assert_eq!(was, "row 19 says a thing", "the fixture's held row moved");

    // The carry: the same conversation under the new transcript's ids, with the re-seat's own
    // note in front of it - so the integer the viewport holds names a different row.
    let ids: Vec<String> = (0..62).map(|i| format!("t.{i}")).collect();
    fork_live(&mut a, 1000, "s", "t", &ids, |i| match i {
        0 => "This conversation was re-seated onto a new prompt.".to_string(),
        1 => "The tool list was rebuilt.".to_string(),
        _ if i < 42 => format!("row {} says a thing", i - 2),
        _ => format!("row {} arrived after the re-seat", i - 2),
    });
    let frame = a.screen(100, 30);

    // **The index is not the place.** Row `held.ordinal` in the transcript that arrived is two
    // above the reader's and says something else - which is what the old fallback would have
    // held, silently, as though nothing had happened.
    assert_ne!(
        words_at(&a, held.ordinal),
        was,
        "the fixture does not shift the rows, so this test could not tell a row from an index"
    );
    // **The row is**, two down, under the new transcript's id, saying what it said.
    let now = a.anchor.clone().expect("the reader is still holding a row");
    assert_eq!(
        now.ordinal,
        held.ordinal + 2,
        "a live fork moved the viewport off the row the reader was on"
    );
    assert_eq!(now.item_id, "t.21", "and it holds the new transcript's id");
    assert_eq!(
        words_at(&a, now.ordinal),
        was,
        "and the row it holds says the same thing"
    );
    // **And the reader is looking at it.** The frame is the contract, not the field.
    let screen = frame.join("\n");
    assert!(
        screen.contains(&was),
        "the row the reader was reading is not on the screen: {screen}"
    );
    // **Nothing was said, because nothing was lost.** A row that is found again is not a row
    // that went missing, and the ordinal fallback announced it as lost on the way past.
    assert!(
        a.notice.is_none(),
        "a row that was carried across is not news: {:?}",
        a.notice
    );
    assert!(
        !a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "anchor_lost")),
        "and it is not filed as lost either"
    );
}

/// A live fork that does not carry the row goes to the tail and says so: a compaction's shape leaves the
/// reader's row in the part that was summarised, and the head must neither count nor move them in silence.
#[test]
fn a_live_fork_that_does_not_carry_the_row_follows_the_tail_and_says_so() {
    let mut a = scrolled_up(40);
    let held = a.anchor.clone().expect("a held row");
    let was = words_at(&a, held.ordinal);

    // The carry: the note, then the newest twenty rows. The reader's row is in the part that
    // was summarised.
    let ids: Vec<String> = (0..20).map(|i| format!("t.{i}")).collect();
    fork_live(&mut a, 1000, "s", "t", &ids, |i| match i {
        0 => "This conversation was compacted: what was said before this point is replaced by \
              the summary below."
            .to_string(),
        _ => format!("row {} says a thing", i + 19),
    });
    let frame = a.screen(100, 30);

    // **The reader's row is not in the new transcript**, and the fixture says so rather than
    // leaving it to the code: no row here says `was`.
    assert!(
        !a.items.iter().any(|it| row_says(it, &was)),
        "the fixture carries the reader's row, so this test is not about a lost one"
    );
    assert!(
        a.anchor.is_none(),
        "a row that is not in the transcript must not be held by index: {:?}",
        a.anchor
    );
    assert!(
        a.following(),
        "and the view follows the tail, which is where it went"
    );

    // **And it is SAID** - on the screen, where the reader is looking, and named for the row.
    let said = a
        .notice
        .as_deref()
        .expect("a reader who was moved must be told, not moved in silence");
    assert!(
        said.contains("not in the transcript that replaced it"),
        "the sentence says what happened, and not that the row had no body — a live fork's \
         rows have arrived: {said}"
    );
    assert!(
        said.contains("follows the tail again"),
        "and where the reader is now: {said}"
    );
    assert!(
        said.contains(&held.item_id),
        "and names the row that was not carried: {said}"
    );
    // **The durable half too**, so `/notes` can answer *why did my view move* afterwards.
    let noted = a
        .notes
        .iter()
        .find_map(|(_, n)| match n {
            Note::Warned(w) if w.code == "anchor_lost" => Some(w.detail.clone()),
            _ => None,
        })
        .expect("a row that is gone is filed as well as said");
    assert!(
        noted.contains(&held.item_id),
        "the note names the row: {noted}"
    );

    // **And the tail is what is on the screen**: the last row the new transcript carries.
    let screen = frame.join("\n");
    assert!(
        screen.contains("row 38 says a thing"),
        "the view did not go to the tail: {screen}"
    );
    assert!(
        !screen.contains(&was),
        "and it is not showing a row the new transcript does not have: {screen}"
    );
}

/// A fork stated before its rows has not lost the reader's row, and does not say it has: a head that hears
/// the statement first has nothing to match, and saying the row is gone would be a sentence about a row that
/// has not arrived.
#[test]
fn a_fork_stated_before_its_rows_waits_rather_than_calling_the_row_gone() {
    let mut a = scrolled_up(40);
    let held = a.anchor.clone().expect("a held row");
    let was = words_at(&a, held.ordinal);

    // The statement, and then a frame - with not one row of the carry here yet.
    a.apply(ServerFrame::Event(env(1000, testing::forked("t", "s"))));
    a.screen(100, 30);
    assert!(
        a.carry.as_ref().is_some_and(|c| c.arriving),
        "a fork stated before its rows must take a carry that knows they are coming"
    );
    assert!(
        a.notice.is_none(),
        "nothing may be said about a row that has not arrived: {:?}",
        a.notice
    );
    assert!(
        !a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "anchor_lost")),
        "and nothing may be filed as lost either"
    );

    // The rows, and the reader is placed on the row they were on.
    let ids: Vec<String> = (0..60).map(|i| format!("t.{i}")).collect();
    carried_appends(&mut a, 1001, &ids, |i| format!("row {i} says a thing"));
    a.screen(100, 30);
    let now = a
        .anchor
        .clone()
        .expect("the reader is holding the row again");
    assert_eq!(now.item_id, "t.19", "placed under the new transcript's id");
    assert_eq!(
        words_at(&a, now.ordinal),
        was,
        "and it is the row they were on"
    );
    assert!(a.carry.is_none(), "and the carry is spent");
    assert!(
        a.notice.is_none(),
        "the row was carried, so there is nothing to say: {:?}",
        a.notice
    );
}

/// Eighty rows, then `n` more from `from`.
fn rows(a: &mut App, from: u64, n: u64) {
    for i in from..from + n {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i} says a thing")),
        )));
    }
}

/// The scrollback banner's count, as drawn.
fn below(a: &mut App) -> Option<String> {
    a.screen(100, 30)
        .iter()
        .find(|l| l.contains("holding your place"))
        .and_then(|l| l.split("· ").nth(1))
        .map(|s| s.split(' ').next().unwrap_or("").to_string())
}

/// A held view still moves for the reader's own keys: the hold is against the STREAM moving the view, so a
/// key draws it once, held again. Otherwise the wheel moves the view and nothing on the screen does.
#[test]
fn a_held_view_moves_for_the_readers_keys_and_not_for_the_stream() {
    let mut a = app();
    rows(&mut a, 0, 80);
    a.screen(100, 30);
    a.key(Key::WheelUp);
    assert_eq!(below(&mut a).as_deref(), Some("3"));
    a.key(Key::CtrlP);
    assert!(a.hold, "the premise: held");
    a.key(Key::WheelUp);
    assert_eq!(
        below(&mut a).as_deref(),
        Some("6"),
        "the wheel moved the held view"
    );
    a.key(Key::WheelDown);
    assert_eq!(
        below(&mut a).as_deref(),
        Some("3"),
        "and back down, counted down"
    );
    // The stream does not move it.
    let before = a.screen(100, 30);
    rows(&mut a, 80, 5);
    assert_eq!(
        a.screen(100, 30),
        before,
        "rows arriving left the held frame as it was"
    );
}

/// **A hold a selection took ends when the reader acts elsewhere** - it was the selection's, so
/// that the text under it stayed put while it was made; a wheel afterwards is the reader moving
/// on, and leaves the view moving and following as it did before the drag.
#[test]
fn a_selections_hold_ends_with_the_next_key() {
    let mut a = app();
    rows(&mut a, 0, 80);
    a.screen(100, 30);
    a.key(Key::Press { x: 4, y: 5 });
    a.key(Key::Drag { x: 12, y: 6 });
    a.key(Key::Release { x: 12, y: 6 });
    assert!(a.hold, "the premise: the drag held the view");
    a.key(Key::WheelUp);
    assert!(!a.hold, "the wheel ended the selection's hold");
    assert!(a.selection.is_none(), "and the selection with it");
    assert_eq!(
        below(&mut a).as_deref(),
        Some("3"),
        "and the wheel moved the view"
    );
}

// ---------------------------------------------------------------------------
// **The reply drawn twice, once on each side of the question** - 2026-10-30.
//
// The operator's photograph, in order:
//
// ```text
//   so, update the docs ?                                          10:19:38
// [2 tool calls, 53 thinking lines]
// The doc is stale in six places - … Rewriting it to what the code does:
//   » Wrote rano/docs/pdf.md · ok · 7ms · 147 lines
// [1 tool call, 19 thinking lines]
// Different thread, and a useful one - §20.8 has been waiting on a real *-json.log:
//   also maybe we have docker on this host now? …                  10:20:10
// [1 tool call, 18 thinking lines]
// Different thread, and a useful one - §20.8 has been waiting on a real *-json.log: [1 tool call, 36 thinking lines]
// ```
//
// Their words: *"the reply started before my question was visible and then duplicated after."*
//
// **Which copy is which, from the code and not from the look.** The counts are drawn AFTER the
// reply in the pane (`ui::transcript::live`, whose own comment records the operator's ruling
// *"all of it after the reply"*), and the pane is drawn after every row - so the copy carrying
// `[1 tool call, 36 thinking lines]` is the LIVE one, and the copy above the question is the
// stored row. That is the opposite of what the shape suggests, and it is what makes the search
// land in the right place.
//
// **The two copies are two buffers, not one list written twice.** `items` holds the row;
// `TurnPane::text` holds the prose the deltas streamed into; the frame draws the rows and then
// the pane. Nothing in the item list is duplicated, so a rule about the list cannot see this.
//
// **The live path already hands the prose over** - at the row's BODY (`App::record_item`), which
// is why the first test below passes on the tree before this change. What it cannot cover is a
// row that arrives already holding its body: a snapshot. A snapshot is a *replacement*, and it
// carries the turn's accumulated text (`Snapshot::turn`) beside rows whose bodies are in `items`
// - and the daemon's view never trims that text when a row lands, so a head that re-attaches,
// resyncs or switches back mid-turn is handed one paragraph twice, with no later event to take
// the pane's copy away: the body it waits for has already happened. The second test is that door,
// and it is the one that failed before the fix.

/// **How many times the frame draws this text**, which is the assertion a duplicate needs - a
/// count of rows can be right while the screen shows a paragraph twice.
fn drawn_times(frame: &str, words: &str) -> usize {
    frame.matches(words).count()
}

/// The line index of the first row of the frame that contains `words`.
fn at(frame: &str, words: &str) -> usize {
    frame
        .lines()
        .position(|l| l.contains(words))
        .unwrap_or_else(|| panic!("`{words}` is not on the frame:\n{frame}"))
}

/// A reply streamed while the person's own prompt lands mid-stream is drawn once, below it: the rows above
/// are byte-identical to what they were, since a head that re-flowed history to make room would pass a
/// "once" assertion and still be wrong.
#[test]
fn a_reply_streamed_while_a_prompt_lands_is_drawn_once_below_it() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    // The operator's first prompt, and the round that answered it.
    shell_row(
        &mut a,
        1,
        "s.u1",
        "user",
        operator_row("so, update the docs ?"),
    );
    a.apply(ServerFrame::Event(env_at(
        3,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    a.apply(ServerFrame::Event(env_at(
        4,
        1_001,
        testing::delta("r1", "The doc is stale in six places."),
    )));
    shell_row(
        &mut a,
        5,
        "s.a1",
        "assistant",
        TranscriptItem::Assistant {
            text: "The doc is stale in six places.".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    // Round 2, streaming - and the operator types into the middle of it.
    a.apply(ServerFrame::Event(env_at(
        7,
        2_000,
        SessionEvent::TurnStarted {
            turn_id: "r2".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(2_000),
        },
    )));
    a.apply(ServerFrame::Event(env_at(
        8,
        2_001,
        testing::delta("r2", "Different thread, and the useful one:"),
    )));
    let before = a.screen(90, 40).join("\n");
    assert_eq!(
        drawn_times(&before, "Different thread, and the useful one:"),
        1,
        "the premise: the streaming reply is on the frame once:\n{before}"
    );
    // The prompt lands while the round is still speaking.
    shell_row(
        &mut a,
        9,
        "s.u2",
        "user",
        operator_row("also maybe we have docker on this host now?"),
    );
    // And the round finishes: the row, then its body.
    shell_row(
        &mut a,
        11,
        "s.a2",
        "assistant",
        TranscriptItem::Assistant {
            text: "Different thread, and the useful one:".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    let frame = a.screen(90, 40).join("\n");
    // **Once**, and after the question it answers.
    assert_eq!(
        drawn_times(&frame, "Different thread, and the useful one:"),
        1,
        "the reply is drawn {} time(s) — the row and the live pane's copy are both on the \
         screen:\n{frame}",
        drawn_times(&frame, "Different thread, and the useful one:")
    );
    assert!(
        at(&frame, "Different thread, and the useful one:")
            > at(&frame, "also maybe we have docker on this host now?"),
        "the reply is drawn above the question that prompted it:\n{frame}"
    );
    // And the rows above it did not move: everything up to the first answer's row is the same on both
    // frames. Cut there rather than at the question, because the pane's copy sits between them on one frame.
    let kept = |f: &str| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in f.lines() {
            out.push(l.to_string());
            if l.contains("The doc is stale in six places.") {
                break;
            }
        }
        out
    };
    assert_eq!(
        kept(&before),
        kept(&frame),
        "the rows above the question changed when it landed"
    );
    assert_eq!(
        drawn_times(&frame, "The doc is stale in six places."),
        1,
        "the first answer is still drawn once:\n{frame}"
    );
}

/// A head that re-attaches mid-turn does not draw the reply it already has a row for: the snapshot carries
/// the turn's accumulated text beside the rows that hold it, and the body that would have taken it away
/// happened before this head existed.
#[test]
fn a_reattach_mid_turn_does_not_draw_the_reply_twice() {
    // The daemon's own log, folded into a snapshot: the answer to the first prompt has landed, and
    // the operator's next prompt was appended after it while the model was still speaking.
    let hub = Hub::new("s");
    hub.publish(testing::appended("s.u1", "user"));
    hub.record_item("s.u1", operator_row("so, update the docs ?"));
    hub.publish(testing::turn_started("r1"));
    hub.publish(testing::delta(
        "r1",
        "Different thread, and the useful one:",
    ));
    hub.publish(testing::appended("s.a1", "assistant"));
    hub.record_item(
        "s.a1",
        TranscriptItem::Assistant {
            text: "Different thread, and the useful one:".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    hub.publish(testing::appended("s.u2", "user"));
    hub.record_item(
        "s.u2",
        operator_row("also maybe we have docker on this host now?"),
    );

    let mut a = app();
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello("s", vec![brief("s", "one", true)], hub.snapshot()));
    let frame = a.screen(90, 40).join("\n");
    assert_eq!(
        drawn_times(&frame, "Different thread, and the useful one:"),
        1,
        "the reply the rows already hold is drawn {} time(s): the row, and the pane's copy of \
         it below the operator's own prompt:\n{frame}",
        drawn_times(&frame, "Different thread, and the useful one:")
    );
    assert!(
        at(&frame, "Different thread, and the useful one:")
            < at(&frame, "also maybe we have docker on this host now?"),
        "the answer to the first prompt is drawn above the second prompt:\n{frame}"
    );

    // **And the next round streams where it belongs** - under the question, not above it, and
    // still without a second copy of the sentence that landed before the attach.
    a.apply(ServerFrame::Event(env_at(
        100,
        3_000,
        SessionEvent::TurnStarted {
            turn_id: "r2".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(3_000),
        },
    )));
    a.apply(ServerFrame::Event(env_at(
        101,
        3_001,
        testing::delta(
            "r2",
            "Docker is here — 29.1.3, and this user can talk to it.",
        ),
    )));
    let frame = a.screen(90, 40).join("\n");
    assert_eq!(
        drawn_times(&frame, "Different thread, and the useful one:"),
        1,
        "the landed reply is drawn twice once the next round streams:\n{frame}"
    );
    assert!(
        at(&frame, "Docker is here — 29.1.3")
            > at(&frame, "also maybe we have docker on this host now?"),
        "the round that answers the second prompt is drawn under it:\n{frame}"
    );
}

/// Two complete turns in a row draw each reply once, the control case: a duplicate here means a row was
/// announced twice or the pane's prose survived a round boundary.
#[test]
fn two_complete_turns_draw_each_reply_once() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let mut seq = 1;
    for (n, (prompt, answer)) in [
        ("so, update the docs ?", "The doc is stale in six places."),
        ("and the docker one?", "Docker is here — 29.1.3."),
    ]
    .into_iter()
    .enumerate()
    {
        let turn = format!("r{n}");
        shell_row(
            &mut a,
            seq,
            &format!("s.u{n}"),
            "user",
            operator_row(prompt),
        );
        seq += 2;
        a.apply(ServerFrame::Event(env_at(
            seq,
            1_000 * (n as u64 + 1),
            SessionEvent::TurnStarted {
                turn_id: turn.clone(),
                model: "qwen3-next-80b".into(),
                ledger_head: "0000".into(),
                began_ms: Some(1_000 * (n as u64 + 1)),
            },
        )));
        seq += 1;
        a.apply(ServerFrame::Event(env(seq, testing::delta(&turn, answer))));
        seq += 1;
        shell_row(
            &mut a,
            seq,
            &format!("s.a{n}"),
            "assistant",
            TranscriptItem::Assistant {
                text: answer.into(),
                tool_calls: Vec::new(),
                truncated: false,
            },
        );
        seq += 2;
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TurnFinished {
                turn_id: turn,
                finish_reason: letibot_sessionlog::event::FinishReason::Eos,
                usage: Default::default(),
                timings: Default::default(),
                ends_turn: true,
            },
        )));
        seq += 1;
    }
    let frame = a.screen(90, 40).join("\n");
    for (prompt, answer) in [
        ("so, update the docs ?", "The doc is stale in six places."),
        ("and the docker one?", "Docker is here — 29.1.3."),
    ] {
        assert_eq!(
            drawn_times(&frame, answer),
            1,
            "`{answer}` is drawn {} time(s):\n{frame}",
            drawn_times(&frame, answer)
        );
        assert!(
            at(&frame, answer) > at(&frame, prompt),
            "`{answer}` is drawn above `{prompt}`:\n{frame}"
        );
    }
}

/// A re-attach mid-round does not count the working it already has rows for: `settled_calls` and the
/// reasoning handover are the marks a snapshot does not carry, so a head that re-attached started both at
/// zero and counted a call and a block of thinking that were rows above it.
#[test]
fn a_reattach_mid_round_does_not_count_the_working_it_has_rows_for() {
    let hub = Hub::new("s");
    hub.publish(testing::appended("s.u1", "user"));
    hub.record_item("s.u1", operator_row("so, update the docs ?"));
    hub.publish(testing::turn_started("r1"));
    hub.publish(testing::reasoning(
        "r1",
        "a thought that already landed as a row",
    ));
    hub.publish(SessionEvent::ToolCallProposed {
        turn_id: "r1".into(),
        call_id: "call_0".into(),
        name: "bash".into(),
        args_digest: "d".into(),
        target: "cargo test".into(),
    });
    // The round's thinking lands as a row, and then the call's result.
    hub.publish(testing::appended("s.r1", "reasoning"));
    hub.record_item(
        "s.r1",
        TranscriptItem::Reasoning {
            text: "a thought that already landed as a row".into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    hub.publish(testing::appended("s.t1", "tool_result"));
    hub.record_item(
        "s.t1",
        TranscriptItem::ToolResult {
            call_id: "call_0".into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "the output of a call that already landed".into(),
            edit: None,
            origin: None,
            media: None,
        },
    );

    let mut a = app();
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello("s", vec![brief("s", "one", true)], hub.snapshot()));
    let t = a.turn.as_ref().expect("the snapshot carried a turn");
    assert_eq!(
        t.settled_calls, 1,
        "the pane still believes the transcript owes it a card for `call_0`"
    );
    assert!(
        t.reasoning.is_empty(),
        "the pane still holds thinking the reasoning row holds: {:?}",
        t.reasoning.raw()
    );
    // And the marker the reader sees counts each once - one call, one thinking line, both of them
    // the rows' own.
    let frame = a.screen(90, 40).join("\n");
    assert!(
        frame.contains("[1 tool call, 1 thinking line]"),
        "the marker counts the landed work twice:\n{frame}"
    );
    assert!(
        !frame.contains("2 tool calls") && !frame.contains("2 thinking lines"),
        "the marker counts work the transcript already holds:\n{frame}"
    );
}

// And with the fold open the thinking itself was the second copy: `reasoned_upto` trimmed the count and
// nothing trimmed the text, so a landed thought was drawn as its row and again in the pane. The pane's
// buffer is now the thinking no row holds.

/// The state `ctrl-r` leaves a `loud` head in: the thinking switch open and the fold open. Written as the
/// two fields `set_verbosity` writes, since the chord also writes the preference file.
fn thinking_open(a: &mut App) {
    a.visibility = Visibility::of(Profile::LOUD).with(Show::Thinking, Level::Open);
    a.reasoning = Fold::Open;
}

/// The rows of the frame that carry a reasoning body - the ones drawn under the `┃` rail.
fn railed(frame: &str) -> usize {
    frame.lines().filter(|l| l.contains('┃')).count()
}

/// **The number in `[N thinking lines]`, read off the frame** - what a reader sees, not the field.
fn thinking_count(frame: &str) -> Option<usize> {
    frame.lines().find_map(|l| {
        let (_, rest) = l.split_once('[')?;
        let body = rest.split(']').next()?;
        body.split(", ").find_map(|p| {
            p.trim()
                .strip_suffix(" thinking lines")
                .or_else(|| p.trim().strip_suffix(" thinking line"))
                .and_then(|n| n.trim().parse().ok())
        })
    })
}

/// A thought that has landed as a row is drawn once, and the pane draws none of it: the mid-stream half is
/// asserted first, so a fix that stopped drawing the pane's thinking would fail here rather than pass.
#[test]
fn landed_thinking_is_drawn_once_when_the_fold_is_open() {
    const THOUGHT: &str = "THINK-ALPHA one short thought";
    let mut a = app();
    thinking_open(&mut a);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    shell_row(&mut a, 1, "s.u1", "user", operator_row("what changed?"));
    a.apply(ServerFrame::Event(env_at(
        3,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    // It thinks, and nothing has landed yet: the pane is the only copy.
    a.apply(ServerFrame::Event(env_at(
        4,
        1_100,
        testing::reasoning("r1", THOUGHT),
    )));
    let streaming = a.screen(90, 40).join("\n");
    assert_eq!(
        drawn_times(&streaming, THOUGHT),
        1,
        "the thinking the pane is streaming is not drawn exactly once:\n{streaming}"
    );
    assert!(
        streaming.contains("Thinking…"),
        "the pane is drawing no thinking block at all:\n{streaming}"
    );

    // The round's thinking lands as a row, and the pane has nothing left to draw.
    shell_row(
        &mut a,
        5,
        "s.r1",
        "reasoning",
        TranscriptItem::Reasoning {
            text: THOUGHT.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    let landed = a.screen(90, 40).join("\n");
    assert_eq!(
        drawn_times(&landed, THOUGHT),
        1,
        "the thought is drawn {} time(s) with the fold open:\n{landed}",
        drawn_times(&landed, THOUGHT)
    );
    assert!(
        landed.contains("Thought"),
        "the row is not drawing it either:\n{landed}"
    );
    assert!(
        !landed.contains("Thinking…"),
        "the pane is still drawing a thinking block of its own:\n{landed}"
    );
}

/// The number and the text it is a number about agree, in both states on one frame: the count used to be
/// taken over a different string than the fold drew, and then over a different width, since the body is
/// drawn four columns narrower.
#[test]
fn the_thinking_count_equals_what_the_fold_draws() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    shell_row(&mut a, 1, "s.u1", "user", operator_row("what changed?"));
    a.apply(ServerFrame::Event(env_at(
        3,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    // **Long enough that the arithmetic and the drawing can part company**: 169 columns of
    // five-column words at an eighty-six-column frame is two lines by the frame's width and three
    // by the eighty-two the step and the rail leave - which is what the fold actually draws.
    let thought = format!("{}word", "word ".repeat(33));
    a.apply(ServerFrame::Event(env_at(
        4,
        1_100,
        testing::reasoning("r1", &thought),
    )));
    // The marker, clicked - which is how a reader opens the working at this rung. The click is
    // what a person does; this writes what it writes: the one switch (`App::lift`) and the panel
    // that carries its exit (`payload_sel`, by the live tail's sentinel).
    a.lift = Some(Show::Thinking);
    a.payload_sel = Some(crate::app::runclick::LIVE_RUN.to_string());
    let screen = a.screen(90, 40).join("\n");
    let counted = thinking_count(&screen).unwrap_or_else(|| panic!("no marker:\n{screen}"));
    assert!(
        counted > 1,
        "the fixture's thought is one line, so it cannot tell the two arithmetics apart:\n{screen}"
    );
    assert_eq!(
        railed(&screen),
        counted,
        "the marker says {counted} thinking line(s) and the fold draws {}:\n{screen}",
        railed(&screen)
    );
}

/// A re-attach mid-thought does not redraw the part that landed before it: the snapshot carries both
/// blocks, so the pane spends what the rows hold and draws the rest, which is a prefix walk and not an
/// equality.
#[test]
fn a_reattach_mid_thinking_draws_it_once() {
    const LANDED: &str = "THINK-ONE landed as a row";
    const ALSO_LANDED: &str = "THINK-TWO the second block, also a row";
    const STREAMING: &str = "THINK-THREE still arriving";
    let hub = Hub::new("s");
    hub.publish(testing::appended("s.u1", "user"));
    hub.record_item("s.u1", operator_row("what changed?"));
    hub.publish(testing::turn_started("r1"));
    // The model thought, spoke, thought again - `turn::items`' three-item shape - and both
    // thoughts are rows.
    hub.publish(testing::reasoning("r1", LANDED));
    hub.publish(testing::appended("s.r1", "reasoning"));
    hub.record_item(
        "s.r1",
        TranscriptItem::Reasoning {
            text: LANDED.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    hub.publish(testing::reasoning("r1", ALSO_LANDED));
    hub.publish(testing::appended("s.r2", "reasoning"));
    hub.record_item(
        "s.r2",
        TranscriptItem::Reasoning {
            text: ALSO_LANDED.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    // And a third block is still arriving when the head joins.
    hub.publish(testing::reasoning("r1", STREAMING));

    let mut a = app();
    thinking_open(&mut a);
    a.apply(hello("s", vec![brief("s", "one", true)], hub.snapshot()));
    let frame = a.screen(90, 40).join("\n");
    for (what, words) in [
        ("the first landed block", LANDED),
        ("the second landed block", ALSO_LANDED),
        ("the block still streaming", STREAMING),
    ] {
        assert_eq!(
            drawn_times(&frame, words),
            1,
            "{what} is drawn {} time(s) after a re-attach:\n{frame}",
            drawn_times(&frame, words)
        );
    }
}

/// A round that thinks in two blocks hands each one over as its row lands: the handover walks the round's
/// rows in order and remembers how far it got, or the second landing finds no prefix and leaves the second
/// block on the screen twice.
#[test]
fn a_round_that_thinks_in_two_blocks_hands_each_over() {
    const FIRST: &str = "THINK-FIRST before the words";
    const SECOND: &str = "THINK-SECOND after them";
    let mut a = app();
    thinking_open(&mut a);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    shell_row(&mut a, 1, "s.u1", "user", operator_row("what changed?"));
    a.apply(ServerFrame::Event(env_at(
        3,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    for (seq, text) in [(4, FIRST), (5, SECOND)] {
        a.apply(ServerFrame::Event(env_at(
            seq,
            1_100 + seq,
            testing::reasoning("r1", text),
        )));
    }
    // Both blocks are on the screen once, from the pane.
    let streaming = a.screen(90, 40).join("\n");
    assert_eq!(drawn_times(&streaming, FIRST), 1, "{streaming}");
    assert_eq!(drawn_times(&streaming, SECOND), 1, "{streaming}");

    // And the round's rows land, in the order the daemon appends them.
    shell_row(
        &mut a,
        6,
        "s.r1",
        "reasoning",
        TranscriptItem::Reasoning {
            text: FIRST.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    shell_row(
        &mut a,
        8,
        "s.r2",
        "reasoning",
        TranscriptItem::Reasoning {
            text: SECOND.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    let landed = a.screen(90, 40).join("\n");
    for (what, words) in [("the first block", FIRST), ("the second block", SECOND)] {
        assert_eq!(
            drawn_times(&landed, words),
            1,
            "{what} is drawn {} time(s) once both rows hold it:\n{landed}",
            drawn_times(&landed, words)
        );
    }
    assert!(
        !landed.contains("Thinking…"),
        "the pane is still drawing a thinking block of its own:\n{landed}"
    );
}

/// **The head holds a window of the conversation and not the buffer.** A session that runs for hours
/// appends rows for ever, and a body it holds is text: without a bound the head's copy of a long
/// session grows with the session, and so does every line it has ever drawn.
#[test]
fn the_head_holds_a_window_of_a_long_session_and_not_the_whole_of_it() {
    let mut a = app();
    a.walk_limit = 1;
    // A window a test can actually reach: 2000 rows of a few hundred bytes is over it in a moment.
    a.held_limit = 64 * 1024;
    let body = "a line of conversation, and enough of them to make a row cost its bytes\n\n\
                ```rust\nfn f() { let n = 1; }\n```\n";
    let rows = 2000u64;
    for i in 0..rows {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i}\n{body}")),
        )));
        // A frame now and then, as a head draws one per tick rather than per row.
        if i % 100 == 0 {
            let _ = a.screen(100, 40);
        }
    }
    let frame = a.screen(100, 40);

    // The tail is what it draws: the newest row, whole.
    assert!(
        frame
            .iter()
            .any(|l| l.contains(&format!("row {}", rows - 1))),
        "the newest row is not on the screen:\n{}",
        frame.join("\n")
    );
    // And what it holds is bounded, in the place the memory actually is.
    assert!(
        transcript_bytes(&a.items) <= a.held_limit,
        "the head holds {} bytes of {} rows, over its {} byte window",
        transcript_bytes(&a.items),
        a.items.len(),
        a.held_limit
    );
    assert!(
        a.items_base > 0,
        "nothing was let go, so this asserts nothing about a window"
    );
    // The count is still the session's: a row dropped above the window is counted, not lost.
    assert_eq!(
        a.items_base + a.items.len(),
        rows as usize,
        "the window no longer accounts for every row of the session"
    );
    // The lines are bounded too, and by the window rather than by the session: a rendered buffer that
    // kept every row it ever drew would be the memory this test is about.
    assert!(
        a.hist_lines.len() < rows as usize / 4,
        "the head kept {} rendered lines of a {} row session",
        a.hist_lines.len(),
        rows
    );
}

/// A page of rows the daemon answers goes ABOVE the window and is drawn there, so the reader who
/// asked for older rows gets them instead of a page the head keeps out of sight.
#[test]
fn an_answered_page_lands_above_the_window() {
    let mut a = app();
    // A head that attached to a session the daemon had already trimmed: its first row is row 60.
    a.items_base = 60;
    for i in 0..20u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i}\n")),
        )));
    }
    a.walk_limit = 1;
    let _ = a.screen(100, 20);
    let floor = a.hist_floor;
    assert!(floor > 0, "the tail path was not taken");

    // The page the daemon would answer for the rows above, oldest first.
    let page: Vec<SnapshotItem> = (40..60u64)
        .map(|i| SnapshotItem {
            item_id: format!("p.{i}"),
            kind: "user".into(),
            ledger_head: String::new(),
            ts: 0,
            item: Some(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: format!("older row {i}\n"),
                }],
            }),
            citations: Vec::new(),
        })
        .collect();
    a.apply(ServerFrame::RowsFetched {
        session_id: "s".into(),
        from: 40,
        rows: page,
    });

    assert_eq!(a.items_base, 40, "the window's base moved to the page");
    assert_eq!(a.items.len(), 40, "the page joined the rows it came above");
    assert_eq!(
        a.hist_floor,
        floor + 20,
        "the rendered rows moved down with them"
    );

    // And scrolling into them draws them: the page is reachable, not merely held.
    a.scroll = 100_000;
    let drawn = a.screen(100, 40).join("\n");
    assert_eq!(
        a.hist_floor, 0,
        "the top of the window is still the page, so it was held and not drawn"
    );
    assert!(
        drawn.contains("older row 40"),
        "the fetched rows are not on the screen:\n{drawn}"
    );
}

/// Scrolling above the oldest row this head holds asks the daemon for the rows it is missing, in
/// pages, and asks once rather than once a frame.
#[test]
fn scrolling_above_the_window_asks_for_the_rows_it_is_missing() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A window the daemon trimmed before this head attached: 400 rows lived above it.
    a.items_base = 400;
    for i in 0..20u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i}\n")),
        )));
    }
    a.walk_limit = 1;
    let _ = a.screen(100, 20);
    // Nothing is asked while the reader is at the tail: the rows above are not being read.
    assert!(
        !a.queued
            .iter()
            .any(|x| matches!(x, Action::FetchRows { .. })),
        "a head following the tail asked for rows nobody is reading"
    );

    // Scroll to the top of what it holds: now the missing rows are the next thing to draw.
    a.scroll = 100_000;
    let _ = a.screen(100, 20);
    let asked: Vec<(usize, usize)> = a
        .queued
        .iter()
        .filter_map(|x| match x {
            Action::FetchRows { from, count, .. } => Some((*from, *count)),
            _ => None,
        })
        .collect();
    assert_eq!(asked.len(), 1, "one page, not one per frame: {asked:?}");
    let (from, count) = asked[0];
    assert_eq!(
        from + count,
        400,
        "the page must end where the window begins, or rows are skipped"
    );

    // The answer lands above the window, and the next screen can scroll into it.
    let page: Vec<SnapshotItem> = (from..from + count)
        .map(|i| SnapshotItem {
            item_id: format!("p.{i}"),
            kind: "user".into(),
            ledger_head: String::new(),
            ts: 0,
            item: Some(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: format!("older row {i}\n"),
                }],
            }),
            citations: Vec::new(),
        })
        .collect();
    a.apply(ServerFrame::RowsFetched {
        session_id: "s".into(),
        from,
        rows: page,
    });
    assert_eq!(a.items_base, from, "the window's base moved to the page");
    assert!(a.fetch_asked.is_none(), "the ask was not cleared");

    // And the window it holds now reaches further up: the head knows where to ask next.
    a.scroll = 100_000;
    let _ = a.screen(100, 20);
    let next: Option<usize> = a.queued.iter().rev().find_map(|x| match x {
        Action::FetchRows { from, .. } => Some(*from),
        _ => None,
    });
    assert_eq!(
        next,
        Some(from.saturating_sub(count)),
        "the next page does not continue where the last one ended"
    );
}

/// Rows a daemon would answer a page with, oldest first.
fn page_of(from: usize, count: usize) -> Vec<SnapshotItem> {
    (from..from + count)
        .map(|i| SnapshotItem {
            item_id: format!("p.{i}"),
            kind: "user".into(),
            ledger_head: String::new(),
            ts: 0,
            item: Some(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: format!("older row {i}\n"),
                }],
            }),
            citations: Vec::new(),
        })
        .collect()
}

/// A head holding a window the daemon trimmed `base` rows above, with `rows` rows in it and the
/// reader scrolled to the top of what it holds: the state the page ask exists for.
fn at_the_top_of_a_window(base: usize, rows: u64) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.items_base = base;
    for i in 0..rows {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i}\n")),
        )));
    }
    a.walk_limit = 1;
    let _ = a.screen(100, 20);
    a.scroll = 100_000;
    let _ = a.screen(100, 20);
    a
}

/// The page the head asked for, as `(from, count)`, if it asked for one.
fn asked_for(a: &App) -> Option<(usize, usize)> {
    a.queued.iter().rev().find_map(|x| match x {
        Action::FetchRows { from, count, .. } => Some((*from, *count)),
        _ => None,
    })
}

/// **A page that does not join the window is the last one.** The daemon answers short when a row
/// in the page is one this build cannot read, and the head dropped such a page and then asked for
/// it again on every frame, for ever: nothing drawn, nothing said.
#[test]
fn a_page_that_does_not_join_the_window_is_not_asked_for_again() {
    let mut a = at_the_top_of_a_window(300, 20);
    let (from, count) = asked_for(&a).expect("the fixture never asked for a page");
    assert_eq!(
        (from, count),
        (200, 100),
        "the fixture asked for another page"
    );
    // One row short of the window: the row the daemon could not read was left out.
    a.apply(ServerFrame::RowsFetched {
        session_id: "s".into(),
        from,
        rows: page_of(from, count - 1),
    });
    a.queued.clear();
    a.scroll = 100_000;
    let _ = a.screen(100, 20);
    assert!(
        asked_for(&a).is_none(),
        "the head asked again for the page it just dropped: {:?}",
        asked_for(&a)
    );
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("did not join")),
        "the head dropped a page and said nothing: {:?}",
        a.notice
    );
}

/// **An empty page says where the transcript begins.** At `from == 0` the head cleared the ask and
/// set nothing else, so the same request went out on every frame and the reader was told nothing
/// about why the rows above are not there.
#[test]
fn an_empty_page_says_the_window_holds_the_beginning() {
    let mut a = at_the_top_of_a_window(100, 20);
    let (from, count) = asked_for(&a).expect("the fixture never asked for a page");
    assert_eq!(
        (from, count),
        (0, 100),
        "the fixture asked for another page"
    );
    a.apply(ServerFrame::RowsFetched {
        session_id: "s".into(),
        from,
        rows: Vec::new(),
    });
    a.queued.clear();
    a.scroll = 100_000;
    let _ = a.screen(100, 20);
    assert!(
        asked_for(&a).is_none(),
        "the head asked again for the page it just dropped: {:?}",
        asked_for(&a)
    );
    assert!(
        a.notice.as_deref().is_some_and(|n| n.contains("above")),
        "an empty page at row 0 said nothing: {:?}",
        a.notice
    );
}

/// **The byte bound is a bound.** `trim_held` capped its drop at the row the reader was holding, so
/// one `PageUp` to the beginning of a live session kept every row that arrived afterwards for the
/// life of the head.
#[test]
fn a_reader_holding_the_top_row_does_not_keep_the_whole_session() {
    let mut a = app();
    a.walk_limit = 1;
    a.held_limit = 8 * 1024;
    let body = "a line of conversation, and enough of them to make a row cost its bytes\n\n\
                ```rust\nfn f() { let n = 1; }\n```\n";
    for i in 0..60u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i}\n{body}")),
        )));
    }
    let _ = a.screen(100, 40);
    // The reader goes up to the very beginning of the session, which is the row no trim may pass.
    for _ in 0..20 {
        a.key(Key::PageUp);
        let _ = a.screen(100, 40);
    }
    let held = a.anchor.clone().expect("the fixture is not holding a row");
    assert_eq!(held.ordinal, 0, "the fixture did not reach the top row");
    // And the session runs on under the held view.
    for i in 60..260u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i}\n{body}")),
        )));
    }
    let _ = a.screen(100, 40);
    assert!(
        transcript_bytes(&a.items) <= a.held_limit,
        "the head holds {} bytes of {} rows, over its {} byte window",
        transcript_bytes(&a.items),
        a.items.len(),
        a.held_limit
    );
}

/// **And the bound is a bound when there is nothing below to let go.** A reader at the oldest row with
/// no tail under it leaves the trim only their own row to take, so it takes it and holds the survivor.
#[test]
fn a_window_with_nothing_below_takes_the_held_row_rather_than_overshoot_its_bound() {
    let mut a = app();
    a.walk_limit = 1;
    a.held_limit = u64::MAX as usize;
    let body = "a line of conversation, and enough of them to make a row cost its bytes\n\n\
                ```rust\nfn f() { let n = 1; }\n```\n";
    for i in 0..60u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i}\n{body}")),
        )));
    }
    let _ = a.screen(100, 40);
    for _ in 0..20 {
        a.key(Key::PageUp);
        let _ = a.screen(100, 40);
    }
    let held = a.anchor.clone().expect("the fixture is not holding a row");
    assert_eq!(held.ordinal, 0, "the fixture did not reach the top row");
    // The bound the head must now hold, with no row arriving below to be let go instead.
    assert_eq!(
        a.items_below, 0,
        "the fixture let rows go below the window, so the tail is somewhere to free"
    );
    a.held_limit = 2 * 1024;
    let _ = a.screen(100, 40);
    assert!(
        transcript_bytes(&a.items) <= a.held_limit,
        "the head holds {} bytes of {} rows, over its {} byte window",
        transcript_bytes(&a.items),
        a.items.len(),
        a.held_limit
    );
    assert_eq!(
        a.anchor.as_ref().map(|h| h.ordinal),
        Some(0),
        "the reader is not held on the oldest row this head kept"
    );
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("above the window")),
        "the trim took the held row and said nothing: {:?}",
        a.notice
    );
}

/// **A head at the tail asks for nothing.** `hist_floor == 0` means *everything this head holds is
/// rendered*, which is true at the tail of every session short enough to walk whole - and the page
/// ask read it as *the reader has scrolled up*, so a head that had just attached pulled the rows
/// above its window a hundred at a time, for ever.
#[test]
fn a_head_following_the_tail_asks_for_no_rows() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // The daemon trimmed 300 rows before this head attached, and what is left of the window is
    // under the walk limit, so the whole of it renders and `hist_floor` is 0 at the tail.
    a.items_base = 300;
    for i in 0..200u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i}\n")),
        )));
    }
    for _ in 0..3 {
        let _ = a.screen(100, 20);
    }
    let asked: Vec<(usize, usize)> = a
        .queued
        .iter()
        .filter_map(|x| match x {
            Action::FetchRows { from, count, .. } => Some((*from, *count)),
            _ => None,
        })
        .collect();
    assert!(
        asked.is_empty(),
        "a head following the tail asked for rows nobody is reading: {asked:?}"
    );
    assert_eq!(
        a.hist_floor, 0,
        "the fixture did not take the full-walk path"
    );
}

/// **Attaching to a long session draws its end.** The daemon sends a window of a session it has
/// trimmed 4500 rows of, and the head must draw the last screen of that window without lexing the
/// rows above it: the first frame is the current one, not a walk of the conversation.
#[test]
fn a_fresh_attach_to_a_long_session_draws_its_tail() {
    let mut a = app();
    // The walk limit is the one knob the fixture bends: the same shape with row bodies big enough
    // to pass the real limit would be megabytes of markdown per test, and the head's rule is the
    // same at either value.
    a.walk_limit = 8 * 1024;
    let window = 400usize;
    let mut snap = Hub::new("s").snapshot();
    snap.seq = 3;
    snap.items_dropped = 4_500;
    snap.items = (0..window)
        .map(|i| letibot_sessionlog::view::SnapshotItem {
            citations: Vec::new(),
            item_id: format!("s.{i}"),
            kind: "user".into(),
            ledger_head: "beef".into(),
            ts: 0,
            item: Some(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: format!("row {i} of a long conversation\n"),
                }],
            }),
        })
        .collect();
    a.apply(hello("s", vec![brief("s", "one", false)], snap));

    let first = a.screen(100, 40);
    let drawn = first.join("\n");
    assert!(
        drawn.contains(&format!("row {}", window - 1)),
        "the newest row is not on the first frame:\n{drawn}"
    );
    assert!(
        !drawn.contains("row 0 of"),
        "the first frame walked back to the oldest row it holds:\n{drawn}"
    );
    // The rows were not lexed: the tail is the window plus its slack, not the window.
    assert!(
        (a.hist_renders as usize) <= 40 + TAIL_SLACK + 4,
        "the first frame rendered {} of {} rows",
        a.hist_renders,
        window
    );
    // And the head's copy is the window the daemon sent: the rows it left out are a count, not
    // something this head went and read.
    assert_eq!(a.items.len(), window, "the head holds more than the window");
    assert_eq!(
        a.items_base, 4_500,
        "the rows above the window are not counted"
    );
}

/// **The window has two ends.** A reader parked far above the tail is not reading what arrives
/// below them, so those rows are let go and counted; coming back down asks for them a page at a
/// time, in the direction the session runs, and the tail follows again once the window reaches it.
/// The row bodies are generated from the ordinal, so a page the fixture answers is the row the
/// session really had there.
#[test]
fn rows_below_a_parked_reader_are_let_go_and_fetched_back() {
    const ROWS: u64 = 6_000;
    const PARKED_AT: u64 = 1_000;
    let body = "a line of conversation, and enough of them to make a row cost its bytes\n\n\
                ```rust\nfn f() { let n = 1; }\n```\n";
    let words = |i: u64| format!("row {i} says a thing\n{body}");
    let row_at = |i: u64| SnapshotItem {
        citations: Vec::new(),
        item_id: format!("s.{i}"),
        kind: "user".into(),
        ledger_head: String::new(),
        ts: 0,
        item: Some(TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![letibot_transcript::UserPart::Text { text: words(i) }],
        }),
    };
    let mut a = app();
    a.walk_limit = 1;
    // A window a test can reach: a few hundred bytes a row is over this in a dozen rows.
    a.held_limit = 64 * 1024;
    let arrive = |a: &mut App, i: u64| {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &words(i)),
        )));
    };
    for i in 0..PARKED_AT {
        arrive(&mut a, i);
        if i % 200 == 0 {
            let _ = a.screen(100, 40);
        }
    }
    let _ = a.screen(100, 40);
    // The reader goes up into the window and stays there while the session runs on below them: up
    // by a screenful of notches, so there are rows above them the window has not rendered and
    // nothing asks for the rows above a window it is still drawing.
    a.scroll_up(400);
    let _ = a.screen(100, 40);
    assert!(
        a.anchor.is_some(),
        "the reader is following the tail, so nothing is parked above it"
    );
    assert!(
        a.hist_floor > 0,
        "the whole of the window is rendered, so this is not the window this test is about"
    );
    for i in PARKED_AT..ROWS {
        arrive(&mut a, i);
        if i % 100 == 0 {
            let _ = a.screen(100, 40);
        }
    }
    let _ = a.screen(100, 40);
    // **And the wheel still knows the session is moving.** The window stopped growing when it became
    // a window, so a count of what this head holds would call a living session still: a park the
    // tail keeps running away from is walked out of, not notched out of at three lines.
    let mut step: Vec<usize> = Vec::new();
    for i in ROWS..ROWS + 3 {
        arrive(&mut a, i);
        let _ = a.screen(100, 40);
        step.push(a.wheel_down_notch());
    }
    assert!(
        step[2] > step[0],
        "the wheel stopped gathering speed under a stream it should be walking out of: {step:?}"
    );
    let total = ROWS + 3;

    // **The window is bounded at both ends.** The rows arriving below the reader are not the rows
    // the reader is reading, so they are a count and not a copy.
    assert!(
        a.items_below > 0,
        "the tail was never let go, so this asserts nothing about a window"
    );
    assert!(
        transcript_bytes(&a.items) <= a.held_limit,
        "the head holds {} bytes of {} rows, over its {} byte window",
        transcript_bytes(&a.items),
        a.items.len(),
        a.held_limit
    );
    // A window and not the tail: the count is the window's, which is the limit over a row's size,
    // and not the session's. Every row of a session that ran for another hour would still be here.
    assert!(
        a.items.len() < 1_000,
        "the head kept {} rows of a {} row session (base {}, below {}, anchor {:?})",
        a.items.len(),
        total,
        a.items_base,
        a.items_below,
        a.anchor.as_ref().map(|h| h.ordinal)
    );
    // And every row is accounted for exactly once: above the window, in it, or below it.
    assert_eq!(
        a.rows_in_session(),
        total as usize,
        "the window lost or double-counted a row"
    );

    // The reader comes down to the end, and what is below the window is fetched back a page at a
    // time: every page starts where the window ends, so nothing is skipped and nothing is doubled.
    a.hold(100_000);
    assert!(a.anchor.is_none(), "the reader is not at the tail");
    let mut pages = 0;
    let mut above = 0;
    for _ in 0..400 {
        let _ = a.screen(100, 40);
        let Some(at) = a
            .queued
            .iter()
            .position(|x| matches!(x, Action::FetchRows { .. }))
        else {
            break;
        };
        let Action::FetchRows { from, count, .. } = a.queued.remove(at) else {
            unreachable!("the position was of a FetchRows")
        };
        // The head asks in both directions and the daemon answers both: a page above ends where
        // the window begins, a page below starts where it ends, and neither leaves a hole.
        let below = a.items_base + a.items.len();
        if from != below {
            assert_eq!(
                from + count,
                a.items_base,
                "a page above does not end where the window begins"
            );
            above += 1;
        }
        let end = (from as u64 + count as u64).min(total) as usize;
        let rows: Vec<SnapshotItem> = (from..end).map(|i| row_at(i as u64)).collect();
        a.apply(ServerFrame::RowsFetched {
            session_id: "s".into(),
            from,
            rows,
        });
        pages += 1;
    }
    // The paging here is the tail's: the head asks above once, when the reader's own row is the
    // oldest one it holds, and a page per frame above would be a loop rather than a fetch.
    assert!(
        above < 5,
        "the head asked for {above} pages above the window while fetching the tail"
    );
    assert_eq!(
        a.items_below,
        0,
        "the window never reached the tail (base {}, {} rows, below ask {:?}, {pages} pages, {above} above)",
        a.items_base,
        a.items.len(),
        a.fetch_below,
    );
    assert_eq!(
        a.items_base + a.items.len(),
        total as usize,
        "the window does not end where the session does"
    );
    assert!(
        pages > 1,
        "one page covered the whole tail, so this asserts nothing about paging"
    );

    // The newest row is on the screen, and the rows drawn above it are in order: a page placed
    // wrongly draws a row twice or leaves one out.
    let frame = a.screen(100, 40);
    let drawn = frame.join("\n");
    assert!(
        drawn.contains(&format!("row {} says a thing", total - 1)),
        "the newest row is not on the screen:\n{drawn}"
    );
    let numbers: Vec<u64> = frame
        .iter()
        .filter_map(|l| {
            let at = l.find("row ")? + 4;
            let n: String = l[at..].chars().take_while(char::is_ascii_digit).collect();
            n.parse().ok()
        })
        .collect();
    assert!(
        numbers.windows(2).all(|w| w[0] < w[1]),
        "the window drew a row twice or out of order: {numbers:?}"
    );

    // And the live tail resumes at the bottom: a row arriving now is held, drawn, and is not
    // something the reader has to ask for a page at a time.
    arrive(&mut a, total);
    arrive(&mut a, total + 1);
    let live = a.screen(100, 40).join("\n");
    assert!(
        live.contains(&format!("row {} says a thing", total + 1)),
        "the tail did not resume at the bottom:\n{live}"
    );
    assert_eq!(
        a.items_below, 0,
        "a row arriving below the window was let go"
    );
}

/// **The count below the window is a guess and the daemon's answer is the truth.** A page that
/// comes back empty says the session ends where the window does, and asking again would be a
/// loop on a number nothing can pay.
#[test]
fn an_empty_page_below_the_window_ends_the_asking() {
    let mut a = app();
    a.walk_limit = 1;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    for i in 0..20u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            testing::content(&format!("s.{i}"), &format!("row {i}\n")),
        )));
    }
    // A window whose end the head believes is 40 rows short of the session's.
    a.items_below = 40;
    let _ = a.screen(100, 20);
    a.hold(100_000);
    let _ = a.screen(100, 20);
    let at = a
        .queued
        .iter()
        .position(|x| matches!(x, Action::FetchRows { .. }))
        .expect("no page below the window was asked for");
    let Action::FetchRows { from, .. } = a.queued.remove(at) else {
        unreachable!("the position was of a FetchRows")
    };

    // The daemon has nothing at that ordinal.
    a.apply(ServerFrame::RowsFetched {
        session_id: "s".into(),
        from,
        rows: Vec::new(),
    });
    assert_eq!(a.items_below, 0, "the empty answer did not end the count");
    let _ = a.screen(100, 20);
    assert!(
        !a.queued
            .iter()
            .any(|x| matches!(x, Action::FetchRows { .. })),
        "a page was asked for below a window the daemon said ends the session"
    );
    // And a row arriving now is held rather than counted: the window reaches the end again.
    a.apply(ServerFrame::Event(env(
        99,
        testing::appended("s.new", "user"),
    )));
    assert_eq!(a.items.last().map(|r| r.item_id.as_str()), Some("s.new"));
}
