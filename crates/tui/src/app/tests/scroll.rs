//! Scrolling, holding the view, following the bottom, the history buffer.

use super::*;

/// **A line that names nothing is still the mid-typing courtesy.** The refusal above
/// must not swallow the case it was built beside: a permission that arrives under
/// somebody's half-thought holds their words and answers the marked row, so their
/// Enter is not a keystroke thrown away.
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
    // What the editor does with it is the editor's own readline semantics,
    // unchanged here: Up at the top row walks its submitted-line history,
    // which replaces the half-typed line — same as main, same as any shell.
    // The queue's part in this is only the negative one: no take-back rode
    // along, and the mirror still holds what the daemon still holds.
    assert_eq!(a.input(), "queued one");
    assert_eq!(a.pending_prompts.len(), 1, "the queue is untouched");
}

/// **R36: a scrolled viewport holds while the transcript grows under it.**
///
/// The operator: *"scroll must be preserved - if i scrolled i want my view to hold,
/// regardless of the new stuff below."* And the case that matters, which the row does not
/// say and the operator added: **they scroll to read something WHILE a turn is running.**
/// A scroll that holds on a still transcript and creeps on a live one is the bug they
/// will actually meet, so there is a running turn throughout — rows arriving above and
/// below, a tool row growing as its output lands, and an elision changing a row's height
/// under the viewport.
///
/// **What is asserted is the frame**, not a line count: R36's whole point is that a count
/// is derived and anything above changing height invalidates it, so a test that pinned
/// the count would be pinning the defect. The one line excluded is the banner that says
/// how much is below the reader — that number is *supposed* to change, and it is the
/// disclosure rather than the view.
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

    // **Six notches, not a page.** A page is the whole window here — the fixture is 84
    // body lines and the window is 25 — so a page up reaches the top and leaves no row
    // above the anchor to grow. The requirement's case is a reader who scrolled to read
    // *something*, not one who scrolled to the beginning.
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

    // 1. **Rows arrive BELOW the anchor** — the ordinary case: a turn commits what it
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

    // 2. **A row ABOVE the anchor grows** — R29's remedy line, a note, a tail that
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

    // 3. **A tool row GROWS as its output lands**, under the running turn — the third
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

/// **R36 across the replacements**: a resync, a snapshot on `hello`, a compaction.
///
/// This is where the row said it *would* break — *"a `resync`, a snapshot on `hello`, and
/// a compaction all replace the rows wholesale. If the anchored row survives, the view
/// holds. If it was summarised away, SAY SO rather than jumping."*
///
/// Both halves, because they are different obligations: a replacement that **carries**
/// the row has to hold the view, and one that **takes it away** has to say so — silently
/// jumping to the bottom would lose the reader's place twice, once to the replacement and
/// once to the head.
#[test]
fn a_replacement_holds_the_view_or_says_the_row_is_gone() {
    /// A snapshot holding `ids`, as the daemon sends one on `hello` or a resync.
    fn snapshot_with(ids: &[&str]) -> letibot_sessionlog::view::Snapshot {
        let mut snap = Hub::new("s").snapshot();
        snap.seq = 9;
        snap.items = ids
            .iter()
            .map(|id| letibot_sessionlog::view::SnapshotItem {
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
    // A replacement that carries only the FIRST few rows — which is what a compaction
    // looks like from here: the old transcript is gone, and a much shorter one stands in.
    // A replacement that carries the LAST twenty rows: the reader's row is gone and the
    // transcript is still long enough to be holding a place in, which is the shape a
    // compaction leaves behind — a summary in place of the old head, the recent tail
    // carried as itself.
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
        said.contains("follows the stream again"),
        "and carries the act that undoes the hold: {said}"
    );
    // **And it reaches the reader where they are looking.** The note lives at the seam
    // at the end of the replacement, which is *below* a reader who is holding a row near
    // the top — so the transient line above the composer carries it too, and that is the
    // one this asserts. A disclosure the reader cannot see is not a disclosure.
    assert!(
        b.notice
            .as_deref()
            .is_some_and(|n| n.contains(&lost.item_id)),
        "the reader must be told on screen, not only at a seam below them: {:?}",
        b.notice
    );
    let _ = frame;
    // **It lands on the nearest surviving row**, not at the bottom: the ordinal it held
    // is the only ordering both sides of a replacement agree on.
    let now = b.anchor.clone().expect("still holding, on its neighbour");
    assert!(
        now.ordinal < b.items.len(),
        "the hold was dropped instead of moved: {now:?}"
    );
    assert!(
        now.ordinal <= lost.ordinal,
        "the view jumped past where the reader was: {:?} from {:?}",
        now,
        lost
    );
    assert!(!b.following(), "the reader was thrown back into following");
}

/// **R56: while the view is held, the head writes NOTHING** — so a mouse selection survives
/// a streaming turn, which is the whole of why the hold exists.
///
/// The contract has three clauses and this drives all three: the frame is composed once
/// (with the marker on it, the single write the freeze owes), every later call returns it
/// **byte for byte** while a turn streams and rows land, and the release says what arrived.
/// The zero-bytes claim is asserted the only way a frame test can: `screen` is what the
/// terminal diffs against the glass, so a frame that does not change is a frame that writes
/// no bytes at all.
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

    // **The freeze, and the one write it owes** — the marker, on the hint bar's row.
    assert_eq!(a.key(Key::CtrlP), None, "the hold is not a daemon action");
    assert!(a.hold);
    let frozen = a.screen(100, 24);
    assert!(
        frozen.iter().any(|l| l.contains(HOLD_MARKER)),
        "the held frame does not say it is held:\n{}",
        frozen.join("\n")
    );

    // **And then nothing is written, however much arrives.** A turn streams, a round lands,
    // two rows are appended — the frame must not move by a byte.
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

    // **The release follows again, and says how much arrived — once**, which is what makes
    // the count honest: a live count while held would be an animation.
    assert_eq!(a.key(Key::CtrlP), None);
    assert!(!a.hold, "the second press releases");
    let released = a.screen(100, 24);
    assert_ne!(released, frozen, "the view did not come back");
    let said = released.join("\n");
    assert!(
        said.contains("the view follows again — 2 rows arrived while it was held"),
        "the release did not count what arrived:\n{said}"
    );
    assert!(
        !said.contains(HOLD_MARKER),
        "the marker outlived the hold:\n{said}"
    );
}

/// **THE ARROWS BRING THE CURSOR'S ROW INTO VIEW** — the operator's *"subagents panel doesnt
/// scroll"*. A pane longer than the screen was walkable only by PageDown or the wheel: the
/// arrows moved a cursor that then left the window, so a child below the fold looked
/// unreachable, which is how the running one stayed hidden even once it was the last row.
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
    // Up walks back, and the draw clamps at the beginning — sixty ups on a
    // fifty-line view stop at the top rather than scrolling into nothing.
    for _ in 0..60 {
        a.key(Key::Up);
    }
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("line 0"), "{screen}");
}

/// **A pane longer than the terminal was a pane whose tail could not be
/// read.** Every pane drew `rows.truncate(room)` and the scroll keys were
/// swallowed while one was open — right about not moving the view
/// underneath, and it left the pane itself unable to move at all.
/// `leticl`'s TODO.md renders 98 rows; on a 40-row terminal more than half
/// of it was unreachable.
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

    // PageDown reaches it. Enough presses to pass the end — the clamp is
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

/// The arrows belong to the decision only while the composer is empty.
///
/// A half-typed line still edits, so nothing was taken away from the person
/// who prefers typing — including `/command`, which shares Enter. Enter does
/// NOT send the line while an ask is open: the ask arrived while the line
/// was being typed, and Enter on a card means "answer this" (the operator,
/// 2026-09-17: *"when i hit enter my unfinished prompt gets sent first"*).
/// The line is held and the next Enter, with the ask settled, sends it.
#[test]
fn enter_on_an_ask_answers_it_and_holds_the_half_typed_line() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
    typed(&mut a, "some prose");
    // **The arrows reach the ladder even mid-line, and the line is untouched.**
    //
    // They used to belong to the composer here, so that "the ladder cursor does
    // not move under the operator's feet" — which sounded right and meant that
    // the only way to choose an option while typing was to empty the composer
    // first. The operator, 2026-09-20: *"until i press down arrow I wont get
    // into the permissions menu, by which time my prompt is erased and gone"*.
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

/// **Parked in the scrollback, the arrows are the scrollback's, and `↓` is the bottom.**
///
/// The operator, 2026-09-27, after hours in one head: *"on this letibot head scrolll is
/// broken - no way to scroll back to the bottom, stuck at holding"*. Measured in their
/// window: three presses of `↓` moved the banner's count by nothing while the stream added
/// lines underneath, and one press of `↑` put an old prompt — *"on this letibot head
/// scrolll is broken…"* — into the composer.
///
/// So this test has **history in the editor**, because history is what ate the keys: without
/// it the defect does not reproduce, which is why it survived to a head that had been up for
/// hours and not one that had just started.
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

    // **And with the reader following, ↑ is not this arm's at all** — the arm is about the
    // parked STATE and not about the key, so everything the head already did with ↑ still
    // happens. Here that is the take-back two arms above: the prompts this test submitted are
    // still queued, and ↑ hands them back for editing rather than scrolling anything.
    a.key(Key::Up);
    assert_eq!(
        a.editor.text(),
        "the first prompt\nthe second prompt",
        "↑ while following must still be the head's own key"
    );
}

/// **§6: a key that scrolls a pane moves THAT pane, and the two overlays are
/// tail-origin.**
///
/// Two defects of one shape — *the key moved something other than what the footer
/// under it promised* — and both are what leticl's tail-origin note is about:
///
/// * **The page keys and the wheel reached the transcript** while the job-output
///   overlay was open. The subagent view had been fixed for exactly this (`"a wheel
///   in the subagent output view scrolled the conversation underneath it"`), and the
///   job overlay — the same overlay, one arm down, opened by the same kind of Enter —
///   was left out of the arm. So a wheel over a job's output moved the conversation
///   behind it and Esc came back to a transcript parked somewhere else.
/// * **The slash listing's arrows were inverted.** `pane_scroll` counts rows hidden
///   **above the top** — `pane_window` is literally `skip(self.pane_scroll)` — so
///   adding to it walks *down* the document, and the arms added on Up. `↑` scrolled
///   a `/notes` listing toward its end while the footer said `up/down scrolls`.
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
    // **And what the reader sees follows**, because the offset is `skip(n)`. Three
    // rows in — the title, the blank and `row 0` — `row 1` is the first thing on
    // screen. Asserted on the text rather than on the number, because the number is
    // only interesting for what it does to the glass.
    b.pane_scroll = 3;
    let shown = b.screen(80, 24).join("\n");
    assert!(!shown.contains("row 0"), "row 0 was skipped: {shown}");
    assert!(shown.contains("row 1"), "{shown}");
    assert!(
        !shown.contains("esc closes"),
        "the footer is below the window: {shown}"
    );

    // ---- and the page keys reach the listing too, which they did not ----
    let mut c = App::new(plain_cfg(80));
    c.slash_out = Some((
        "/notes".into(),
        (0..40).map(|i| format!("row {i}")).collect(),
    ));
    c.screen(80, 24);
    assert_eq!(c.scroll, 0);
    c.key(Key::PageDown);
    assert!(c.pane_scroll > 0, "PageDown did not move the listing");
    assert_eq!(
        c.scroll, 0,
        "PageDown scrolled the transcript behind the listing"
    );
}

/// **The window and the conversation scroll as one** — the operator: *"say i scrolled to
/// the bottom of the ctrl-v view port it should keep scrolling the main convo"*. At the
/// window's first line an Up scrolls the conversation up to the rows above it; at its
/// last line a Down scrolls the conversation down, back to following.
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

/// **An open result window reaches its last line, comes back at once, and fits.**
///
/// The operator: *"i hit ctrl-v to read full command output and couldnt scroll bottom
/// anymore - only esc worked"*. Three faults, measured on a 300-line output at 80x24:
/// Down was unclamped, so the offset ran to 400 while the screen stood still and Up
/// then had to unwind it; the last page clamped to one line; and the window was the
/// budget's forty rows, so on a 24-row screen its first lines were above the top.
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

/// **Folding tools must not cost the ability to scroll.**
///
/// The operator: *"when i expand tools with Ct scroll stops working, even
/// after collapsing back. i have to switch byobu windows back and forth"* —
/// a window switch is a resize, which forces the frame this was waiting for.
#[test]
fn scroll_still_works_after_ctrl_t() {
    let mut a = app();
    // The tail walk is what this is about — `walk_limit = 1` is how the other
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

    // Expand the window on the newest long result. **This does not unfold the
    // conversation** (R10 moved that to `/t`), so the arrows now page the newest
    // payload rather than moving the transcript — which is the intended contract
    // and is asserted below rather than assumed.
    a.key(Key::CtrlV);
    a.screen(80, 24);
    assert!(
        a.payload_sel.is_some(),
        "ctrl-t did not open a payload view"
    );

    let at_refold = a.anchor.clone();
    // **One press, inside the window.** Five used to be pressed here; a Down past the
    // window's last line now carries on into the conversation (the operator: *"say i
    // scrolled to the bottom of the ctrl-v view port it should keep scrolling the main
    // convo"*), so pressing past a 30-line payload would move the transcript by design.
    a.key(Key::Down);
    a.screen(80, 24);
    // **The reader's PLACE, not a line count** (R36). The old assertion was
    // `a.scroll == at_refold`, and it passes only while a count is the whole truth:
    // paging the payload changes that row's height, so `total` moves and the count
    // with it — while the row under the reader's eye has not moved at all. That is the
    // defect the requirement is about, and asserting the count was asserting it.
    assert_eq!(
        a.anchor, at_refold,
        "the transcript moved while a payload view held the arrows"
    );
    assert!(
        a.payload_page > 0,
        "the arrows did not page the payload: {}",
        a.payload_page
    );

    // **Esc gives the arrows back**, and that is what this test is for: the
    // original bug was that expanding tools cost the ability to scroll at all.
    // A view that held the arrows for ever would be the same defect with a new
    // cause, so the escape hatch is part of the contract.
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

/// **The waiting frame: a cat dead centre, walking in place.**
///
/// Four properties, each of which an obvious implementation gets wrong:
///
/// - the cat is **horizontally centred**, measured on the cat's own row, so
///   "centred" is a statement about the cat rather than about whatever else is on
///   the line;
/// - it does not **slide**: the frames are different widths, so each is padded to
///   the widest before centring — otherwise the cat jitters sideways as it changes
///   expression, which reads as a drawing bug rather than a walk;
/// - it **moves**, from the clock, so the frame is a function of elapsed time and
///   not of a render-loop counter;
/// - it is **not at the top** — the middle of the conversation, not the row under
///   the header.
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

/// **The wait becomes impatient, and says so.**
///
/// The operator, on a daemon that accepted the connection and never answered:
/// *"stuck waiting for daemon and no way to exit"*. The screen said
/// `asking the daemon for this session` for three minutes and the hint bar under it
/// named `ctrl+c exit` — which did nothing, because the wait loop read no keys.
///
/// The keys are live now (`main`), and this is the other half: past
/// [`ATTACH_IMPATIENT`] the frame itself says how to get out, so the operator is not
/// left to discover that a wait can be abandoned.
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

/// **The tail frame is the full walk's frame.**
///
/// This is the whole licence for rendering only the end of a conversation, and it
/// is asserted against the other implementation rather than against a fixture: the
/// same rows, rendered from row 0 and rendered backward from the end, must produce
/// the same screen. A fixture would pin today's answer; this pins the agreement.
///
/// The forward walk is forced by raising `walk_limit` — see its doc for why that is
/// a field.
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

/// **The cost is what this exists for**: a tail frame does not render the rows above
/// the window.
///
/// Counted, not timed — `hist_renders` is the number of rows put through
/// `item_lines`, so it is the measurement and not a proxy for one.
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

/// **Scrolling up reaches the beginning.**
///
/// The operator: *"i want scroll back work"*. Two things made it crawl: `PageUp` moved a
/// constant ten lines on a 40-row terminal, and the back-fill happened on the frame
/// *after* the key — so the scroll was clamped against the rows rendered so far and a
/// press could never express "further up than I have drawn". Measured before the fix:
/// twelve rounds of eight PageUps reached message 143 of 400.
///
/// This asserts the end state a reader expects: page up enough and the first row of the
/// conversation is on screen.
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

/// **Scrolling up renders more, on demand.**
///
/// The operator's *"then some scroll up buffer if needed"*. A tail frame deliberately
/// does not render the rows above the window, so scrolling up has to fetch them —
/// and it has to be *able* to: the window clamp is computed from what is rendered, so
/// without the back-fill there would be nothing above the tail to scroll into.
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

/// **A change above the window is not left stale.**
///
/// A tail walk leaves no `hist_marks` — it does not pass the rows it skips — and
/// `invalidate_history_from` used "no mark" to mean "nothing to do". In tail mode
/// that would keep drawing a row that had *changed* as it was. The fix is the only
/// correct one available without marks: throw the history away and render the tail
/// again.
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

/// **The live `!` row walks the history once, not once a frame.**
///
/// `completions_line` runs on every frame and the live `!` row draws the candidates
/// out of it, so building them per frame meant walking the view and parsing every
/// `bash` call's arguments per frame. **Measured at 14.2 ms a frame** on a 2,000-row
/// session before the memo and 0.10 ms above the frame's own baseline after it, which
/// is the difference between a typing aid and a stall. `shell_walks` is the encoder —
/// a wall time is not something a test can assert on and a count is, the same rule
/// `hist_renders` follows.
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
    // **A body-less row is not evidence of anything.** It used to draw a progress
    // line claiming the conversation was being carried onto a new prompt — which
    // every ordinary turn produces, so the line appeared over ordinary replies. The
    // bar is now the daemon's (`Filling`); a row with no body draws nothing and the
    // row renders once its body lands.
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

/// **Nothing on the line may move sideways except the bar's own edge.**
///
/// The operator: *"move cat to the right most position or thngs jump
/// around"*. Two fields change width as the carry runs — the cat, whose
/// frames are 7 and 8 columns, and the numerator, which grows from `0` to its
/// denominator. Either one shifts whatever is drawn to its right, and a
/// progress indicator that shuffles reads as a fault.
///
/// Rendered width is the assertion, not the byte length: these rows are
/// painted, and `width::width` is what skips the escapes.
#[test]
fn the_fork_line_keeps_its_width_as_the_cat_and_the_count_change() {
    let cfg = RenderConfig {
        width: 100,
        ..Default::default()
    };
    // **The premise**: something on this line has to actually change across
    // the sample, or a constant width below proves nothing at all.
    //
    // The cat is no longer one of those things — every frame is one width now,
    // which `cat_frames_are_one_width_with_the_face_in_one_place` is what
    // guarantees, and this test leans on it rather than re-proving it. What is
    // asserted here is that the sample really does walk the animation, and
    // that the NUMERATOR changes width, which is the variable field this line
    // has to hold still around.
    let ticks: Vec<u64> = (0..8).map(|t| t * 400).collect();
    let mut frames: Vec<&str> = ticks.iter().map(|t| cat_frame(*t)).collect();
    frames.sort_unstable();
    frames.dedup();
    assert!(
        frames.len() > 1,
        "the sampled ticks must hit more than one cat frame, or the line is \
             never redrawn in this test: {frames:?}"
    );
    assert_ne!(
        progress::thousands(0).chars().count(),
        progress::thousands(2702).chars().count(),
        "and the numerator must change width across the run, or the layout \
             below says nothing about the count either"
    );

    let mut seen: Vec<usize> = Vec::new();
    // Across the whole run, and across a full cycle of cat frames.
    for done in [0u64, 7, 99, 100, 999, 1000, 2702] {
        for tick in &ticks {
            let line = filling_line("carrying", "rows", done, 2702, *tick, &cfg);
            seen.push(rano::width::text::width(&line[1]));
        }
    }
    seen.dedup();
    assert_eq!(
        seen.len(),
        1,
        "the line must be one width throughout; got {seen:?}"
    );
}

/// **A replaced item vector cannot invert the count.**
///
/// The shape this replaces kept a high-water mark of how many rows lacked a body and
/// reported `peak - pending`. A snapshot **replaces** `items` wholesale, so after a
/// larger carry was replaced by a smaller one the peak was stale and the line said
/// **`97 of 4 rows`** — a numerator larger than its own denominator. The count is now
/// how many have ARRIVED out of how many there are, both off one walk of the current
/// `items`, so it cannot.
#[test]
fn a_replaced_item_vector_cannot_invert_the_count() {
    let mut a = app();
    a.clock(0);
    a.apply(snapshot_hello("big", 40));
    let first = a.screen(80, 40).join("\n");
    assert!(first.contains("40 row(s) announced"), "{first}");

    // A second snapshot replaces the vector with a SMALLER carry. A remembered peak
    // would still say 40 here.
    a.apply(snapshot_hello("small", 4));
    let after = a.screen(80, 40).join("\n");
    assert!(
        after.contains("4 row(s) announced"),
        "the count follows the rows the head now holds: {after}"
    );
    assert!(
        !after.contains("40 row(s)") && !after.contains("44 row(s)"),
        "a replaced vector must not leave a number bigger than the whole: {after}"
    );
}

/// **A fork is one line, however many rows it carries** — and it is the daemon's
/// line, drawn from `Filling`, not a bar the head assembled out of body-less rows.
///
/// `/reseat` and `/compact` announce the whole conversation; drawn one-per-row that
/// was a screen of identical placeholders, and the instruction was to reuse the cat
/// and the prefill bar. The count is asserted, not just the word, because a test that
/// only looked for "carrying" would pass a screen that also had 300 grey rows under it.
#[test]
fn a_fork_announces_three_hundred_rows_and_draws_one_progress_line() {
    let mut a = app();
    // Above `MIN_FILLING`, so the bar is drawn — that is the thing under test here.
    let n = 300u64;
    // The rows the carry announces — still body-less, all of them.
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
    let screen = a.screen(80, 40).join("\n");
    assert_eq!(
        screen.matches("carrying the conversation").count(),
        1,
        "exactly one line for the whole carry: {screen}"
    );
    assert!(
        !screen.contains("waiting for the body"),
        "and not one placeholder among them: {screen}"
    );
    assert!(screen.contains(&format!("  0 of {n} rows")), "{screen}");

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
    let half = a.screen(80, 40).join("\n");
    assert!(
        half.contains(&format!("{} of {n} rows", n / 2)),
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
    let done = a.screen(80, 40).join("\n");
    assert!(
        !done.contains("carrying the conversation"),
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
    a.key(Key::WheelUp);
    a.key(Key::PageUp);
    // **A page is a screen here too.** The constant was ten, so this read `13`; a pager's
    // PageUp moves a page, and the subagent view is a pager. Asserted as the sum rather
    // than a literal so the two keys stay distinguishable — a PageUp that moved nothing
    // would still make this check meaningful.
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
    a.todos_pane = true;
    a.key(Key::WheelUp);
    assert!(a.following(), "a pane on top swallowed the wheel");
    a.todos_pane = false;
    a.key(Key::WheelUp);
    // **R36 made this the assertion**: the wheel takes the viewport OFF the stream, and
    // `following` is where that state lives. It used to be `scroll == 3` — a count, and
    // the count is exactly what the requirement stops trusting: it is derived by the
    // frame, so between a key and a paint it is the previous frame's number.
    assert!(
        !a.following(),
        "with nothing on top the wheel moves the conversation"
    );
}

/// **The window scrolls, and the choices never leave** (R20).
///
/// The ruling asks for a viewport that shrinks *and scrolls*, so the whole diff or
/// message can still be read without the options going anywhere. `pgup`/`pgdn` are the
/// keys — the ones an open card can take without stealing Up/Down from the ladder, and
/// the seam names them.
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

/// **A card that fits is exactly the card it always was**, and the transcript keeps
/// its own scroll.
///
/// The two halves are joined in the same order for the whole card, so nothing about a
/// short card changes — and `pgdn` on one still scrolls the conversation behind it,
/// because a card being up must not cost the transcript a key it has always had.
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

    // **The transcript's scroll keys are still the transcript's** — the card's content
    // fits, so nothing here takes them.
    a.items = (0..80)
        .map(|i| SnapshotItem {
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
    // previous frame's number — which is why `following` is a question about the anchor.
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
    // card on the screen (§1.4: oldest first) — a second `DecisionRequested` while the
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
            choices: Vec::new(),
            because: String::new(),
            advice: None,
            subagent: None,
            deadline: None,
            on_timeout: d.on_timeout,
        },
    )));
    // The reset is in `screen`, where the offset is used, so the property is what the
    // *frame* does with it — the same reason the clamp lives there.
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
        line("glm-coding/glm-5.3").contains(&format!("{}", sgr::GREEN)),
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

    // **A daemon that sends no such row greens nothing** — older than the field, and the head
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

/// **The marker joins the prose even when the tail walk rendered it first** — the case
/// the operator met on a real screen.
///
/// `fill_backward` renders the NEWEST rows first and stops on a line budget, and a run's
/// marker is drawn at the run's FIRST row — so the walk reached the marker, satisfied its
/// budget and broke, and **the narration line above it was never built**. The marker was
/// therefore emitted as a row of its own, on a screen the operator was reading, with the
/// colon of the sentence above it left dangling.
///
/// Asserted through the tail path on purpose: `hist_floor` non-zero is a transcript too
/// big to walk, which is every long session and is where this was found.
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
    // **The tail path, and the fill's budget already satisfied.** `walk_limit` is the
    // head's own threshold for "too big to walk from the beginning"; `fill_backward(1)`
    // is the fill the head makes for a reader one line short of the window, and it is
    // what makes the budget bite: the walk stops the moment it has a line, which is the
    // marker's own — so the narration above it is never built unless something insists.
    a.walk_limit = 1;
    let _ = a.screen(120, 24);
    // **The reader-one-line-short state, set exactly.** That is what a scroll produces:
    // the floor is up at the end of the session, the buffer is empty, and the fill is
    // asked for one line — so the walk stops the moment it has one, which is the marker's
    // own, and the narration above it is never built unless something insists.
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
    // **Enough rows to FILL the screen, which is what makes the shove visible.** The transcript
    // is bottom-anchored once it overflows: a chrome row appearing takes its space from the
    // transcript, every content row slides up by one, and that is the move the reader feels.
    // The first version of this test used three rows, they sat at the top, and it passed against
    // the reverted fix — the fixture could not express the defect.
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

/// **A wheel notch DOWN walks three lines, and a RUN of them is what reaches the tail.**
///
/// **What was true before, and why it changed.** The operator, 2026-10-05: *"I cant scroll back
/// to bottom with a mouse wheel - have to press escape"*. A notch walks three lines and
/// `following()` only comes back when the window REACHES the bottom, so against a live session —
/// which keeps adding rows — a notch is a step toward a target that runs away from it. The
/// answer then was to make ONE notch clear the anchor outright, and this test asserted that:
/// `a_wheel_notch_down_is_the_tail_even_when_the_stream_grew_under_the_reader`.
///
/// **The cure cost more than the complaint.** With a mouse, so it is not a touchpad artefact,
/// the operator: *"one simple stroke gets me to the bottom immediately — effectively like
/// Esc"* — the reader could not walk *down* through a conversation at all, because the first
/// notch left the conversation behind them. Both reports are one coin, and this is the
/// reconciliation: a notch down walks like a notch up, and *arriving* at the tail is what
/// resumes following. A run of notches still returns the reader to the bottom, which answers
/// October's need without the one-notch jump — and the deliberate act keeps its own meaning,
/// because Esc and the parked `↓` still mean "follow again" in one press.
///
/// The test above passes either way, because its transcript is STATIC. **The fixture is why
/// this survived**, so this one adds rows between the notches, thirty of them against three the
/// notch walks — the case that made 2026-10-05 look like a cure.
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
    // the thirty rows did — the head's loop reads it at the top of every pass.
    let _ = a.take_redraw();

    // **One notch down is a STEP, not the tail** — the opposite of what this test asserted
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

/// **Three notches down walk three lines each, and only the one that ARRIVES follows.**
///
/// The measurement the 2026-10-05 change stood in for. A notch is three lines, so a reader
/// walking down has to *see* three lines of conversation per notch, and the parked state has to
/// survive every notch that has not reached the bottom — otherwise a notch is a jump and the
/// conversation between the two positions is unreachable, which is the report this reverses.
/// Asserted on `view_top` rather than on `scroll`, because the line is what is on the glass.
///
/// **And the scroll path still does not ask for a repaint**, which is the flicker fix this
/// must not undo: `redraw` is not "rebuild the frame", it is *throw the glass away*, and a
/// window that slid is a diff.
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

    // Park nine lines up — three notches' worth — so the walk down has somewhere to go.
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
    // Found by pressing PageUp six times under tmux. The scroll was clamped to
    // `total - 1`, so the top of the history was a one-line window — and since
    // the scrollback banner overwrites the last line of the window, the whole
    // screen went blank with the banner alone on it.
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
