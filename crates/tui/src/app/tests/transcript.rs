//! The conversation: rows, folds, markers, visibility.

use super::*;

/// **`/cells` sends the rows this head drew, with the operator's message.**
///
/// The model can ask for a screen (`harness what=screen`); this is the other
/// direction, and it is captured at Enter rather than when the turn gets round
/// to it — by then the screen has moved.
#[test]
fn cells_sends_the_screen_with_the_message() {
    let mut a = app();
    // Draw once, so the head has a size and a frame. Nothing is sent before
    // that: an unrendered head has no cells, and an empty block would read as a
    // blank terminal.
    assert!(
        matches!(a.submit("/cells look at this".into()), None),
        "a head that has drawn nothing must refuse rather than send emptiness"
    );
    let _ = a.screen(100, 12);

    let Some(Action::Prompt(text)) = a.submit("/cells look at this".into()) else {
        panic!("/cells did not send");
    };
    assert!(text.starts_with("look at this\n\n"), "{text}");
    assert!(text.contains("100x12"), "the head's real size: {text}");
    // Delimited at both ends, so the model can see where the picture stops.
    assert!(
        text.contains(CELLS_OPEN) && text.contains(CELLS_CLOSE),
        "{text}"
    );

    // And the transcript shows the words plus one line, not the screen again.
    let folded = fold_cells(&text).expect("a cells message folds");
    assert!(folded.starts_with("look at this"), "{folded}");
    assert!(folded.contains("rows of this screen (100x12)"), "{folded}");
    assert!(
        !folded.contains(CELLS_OPEN),
        "the marker leaked into the fold: {folded}"
    );
    assert_eq!(folded.lines().count(), 2, "one message, one note: {folded}");
    // An ordinary message is left exactly alone.
    assert_eq!(fold_cells("just a message"), None);
    // The rows themselves, not a summary of them.
    let drawn = a.screen(100, 12);
    let last = drawn.last().expect("a frame has rows");
    assert!(
        text.contains(last.as_str()),
        "the rows are not in the message"
    );

    // The pending row holds what was SENT, byte for byte — that is what the
    // transcript's user item will match when it lands.
    let echo = a.pending_prompts.last().expect("the message is echoed");
    assert_eq!(echo, &text);
    // And it is DRAWN folded: the operator's words and one line, never a copy
    // of the screen inside the screen.
    // **Unfolded**, because this test is about what the words ARE — the fold's own
    // behaviour is pinned by `an_echo_is_one_elided_headline…` below.
    let drawn = queued_lines(echo, &a.cfg, QUEUED, true);
    assert!(
        drawn.iter().any(|l| l.contains("look at this")),
        "{drawn:#?}"
    );
    assert!(
        drawn.iter().any(|l| l.contains("rows of this screen")),
        "{drawn:#?}"
    );
    assert!(
        drawn.len() < 6,
        "the queued row is painting the whole screen: {} lines",
        drawn.len()
    );
}

#[test]
fn queued_prompts_behind_a_running_turn_are_one_message() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut a, "also fix the parser");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Prompt("also fix the parser".into()))
    );
    typed(&mut a, "and add a test for it");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Prompt("and add a test for it".into()))
    );
    assert_eq!(
        a.pending_prompts,
        vec!["also fix the parser\nand add a test for it".to_string()],
        "one echo, the way the engine holds one message"
    );
    // Idle submits land each as their own row within a tick, so they stay
    // separate entries.
    a.apply(ServerFrame::Event(env(2, testing::turn_finished("t1"))));
    typed(&mut a, "fresh question");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Prompt("fresh question".into()))
    );
    assert_eq!(a.pending_prompts.len(), 2);
}

/// **The shape that actually happens on this box, and it retired nothing.**
///
/// Measured 2026-09-23 on the live head: six `queued ·` echoes, one per paragraph
/// of the R27 instruction, every one of them answered — and the row that answered
/// them was their JOIN. Six sends while the head did not think a turn was running
/// became six separate entries (`App::submit`'s coalescing is conditional on the head
/// thinking a turn is RUNNING, which is `turn_busy`'s question — see it for why the state
/// name was the wrong instrument), the engine merged them into ONE user item joined by
/// newlines, and against a whole-string rule every comparison was *no*.
///
/// Then the seventh thing: the operator's next message was sent while a turn WAS
/// running, so the head appended it to the last entry — making an entry that is a
/// *superset* of one row and a *prefix* of another. Every one of these three shapes
/// is here, in one test, against the text lengths the real session had.
#[test]
fn the_engine_join_of_several_prompts_retires_every_echo_it_answered() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // The first six: idle submits (the turn state is not Running), so they stay
    // separate entries.
    let six = [
        "THE OPERATOR'S RULING, and it came with a split",
        "My reasoning for why that split is right",
        "One consequence to build in rather than discover",
        "What is still yours to rule",
        "And two corrections you owe your own documentation",
        "Finally, one thing I did NOT establish",
    ];
    for p in six {
        typed(&mut a, p);
        a.key(Key::Enter);
    }
    assert_eq!(a.pending_prompts.len(), 6, "one entry per send");

    // Then a seventh send while the turn IS running: the head joins it onto the
    // last entry, which is the whole reason one entry can be a superset of one
    // row and a prefix of another.
    a.apply(ServerFrame::Event(env(2, testing::turn_started("t2"))));
    let seventh = "The operator is looking at six queued echoes";
    typed(&mut a, seventh);
    a.key(Key::Enter);
    assert_eq!(a.pending_prompts.len(), 6, "joined, not pushed");
    assert_eq!(
        a.pending_prompts[5],
        format!("{}\n{seventh}", six[5]),
        "the last entry is the sixth prompt with the seventh under it"
    );

    // The row the six landed as: the engine's join, one item, six lines.
    a.record_item(
        "s.0",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: six.join("\n"),
            }],
        },
    );
    assert_eq!(
        a.pending_prompts,
        vec![seventh.to_string()],
        "all six are answered by that one row; only the seventh is still owed"
    );

    // And the seventh's own row, which is where its echo goes.
    a.record_item(
        "s.1",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: seventh.to_string(),
            }],
        },
    );
    assert!(
        a.pending_prompts.is_empty(),
        "every echo is retired: {:?}",
        a.pending_prompts
    );

    // The screen agrees — no `queued` tag left anywhere.
    let screen = a.screen(120, 40).join("\n");
    assert!(!screen.contains("queued"), "{screen}");
}

/// **R33: an echo is ONE elided headline, and the usual key opens it.**
///
/// The operator, looking at letibot with three of their own messages queued:
/// *"three giant messages queued"* — a 63-row pane filled with the reader's own words,
/// the conversation pushed off the screen. *"It is a thing waiting, not content to
/// read — the reader wrote it and does not need it read back."*
///
/// The unit of the count is **screen rows**, not source lines, because that is what
/// the reader is paying: the model writes one enormous paragraph, and `+0 lines`
/// beside a row that fills the pane answers the wrong question.
#[test]
fn an_echo_is_one_elided_headline_until_it_is_unfolded() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A message the length of the R31 instruction: 30 lines, ~9.6k characters — the
    // real row that filled the operator's pane.
    let long: String = (0..30)
        .map(|i| format!("paragraph {i} {}", "word ".repeat(40)))
        .collect::<Vec<_>>()
        .join("\n");
    typed(&mut a, &long);
    a.key(Key::Enter);

    let rows = a.screen(120, 60);
    let echo: Vec<&String> = rows
        .iter()
        .filter(|l| l.contains("queued ·") || l.contains("… +"))
        .collect();
    assert_eq!(echo.len(), 1, "the echo is one row, not thirty: {rows:#?}");
    assert!(echo[0].contains("queued · paragraph 0"), "{echo:?}");
    assert!(
        echo[0].contains("· /t opens it"),
        "the seam names the key that opens it: {echo:?}"
    );
    assert!(
        a.pending_prompts[0].starts_with("paragraph 0"),
        "the queue holds the words, not the rendering"
    );
    assert_eq!(
        a.pending_prompts[0], long,
        "byte for byte: the fold is a rendering and nothing else"
    );

    // **And the usual key opens it.** `/t` is this head's unfold-the-long-rows verb;
    // a reader who wants the long things shown whole asks once.
    a.command("t");
    let open = a.screen(120, 200);
    assert!(
        open.iter().filter(|l| l.contains("paragraph 29")).count() >= 1,
        "`/t` must show the rest of it"
    );
    assert!(
        !open.iter().any(|l| l.contains("/t opens it")),
        "and the seam goes with it"
    );
    assert_eq!(a.pending_prompts[0], long, "still byte for byte");
}

/// **A short echo is one row with no seam at all**, because there is nothing hidden —
/// a row carrying `… +0 lines` would be furniture.
#[test]
fn a_short_echo_has_no_seam() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    typed(&mut a, "a short one");
    a.key(Key::Enter);
    let rows = a.screen(120, 40);
    assert!(
        rows.iter().any(|l| l.contains("queued · a short one")),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|l| l.contains("opens it")),
        "nothing is hidden, so nothing is claimed: {rows:#?}"
    );
}

/// **An echo queued AFTER the snapshot keeps saying `queued`.**
///
/// The mark is not a property of the session or of the text — it is a property of one
/// echo's history, and a prompt typed after the snapshot has a row coming that no
/// snapshot has replaced. Marking by time-of-load rather than per echo would take the
/// honest word away from every prompt the operator sends next.
#[test]
fn an_echo_queued_after_the_snapshot_still_says_queued() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    typed(&mut a, "before the snapshot");
    a.key(Key::Enter);
    // A resync whose rows do not carry it.
    let mut snap = Hub::new("s").snapshot();
    snap.seq = 9;
    a.apply(hello("s", vec![brief("s", "one", false)], snap));
    assert_eq!(a.unconfirmed, vec!["before the snapshot".to_string()]);

    // Now one sent after.
    typed(&mut a, "after the snapshot");
    a.key(Key::Enter);
    assert_eq!(
        a.unconfirmed,
        vec!["before the snapshot".to_string()],
        "the new echo is not marked by a snapshot that predates it"
    );
    let screen = a.screen(140, 30).join("\n");
    assert!(screen.contains("queued · before the snapshot"), "{screen}");
    assert!(screen.contains("queued · after the snapshot"), "{screen}");
}

/// **An unconfirmed echo whose row lands INSIDE A MERGED ITEM leaves the set.**
///
/// This is the leak leticl measured on its own head, and the structural reason is the
/// same here: the set is keyed by the echo's text and the landing row's text is the
/// engine's JOIN, so `unconfirmed` could never be cleared by comparing texts. It wants
/// an **intersection** — keep the marks whose echo is still in the queue — which is
/// what `retire_pending` now does, and the first version of this fix got the
/// intersection's TIMING wrong (before the loop rather than after), which is the same
/// leak with the opposite sign. Both are pinned here.
#[test]
fn an_unconfirmed_echo_retires_when_its_row_lands_inside_a_merged_item() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    typed(&mut a, "one of mine");
    a.key(Key::Enter);
    typed(&mut a, "and another");
    a.key(Key::Enter);
    // A snapshot that carries neither, so both are unconfirmed.
    let mut snap = Hub::new("s").snapshot();
    snap.seq = 4;
    a.apply(hello("s", vec![brief("s", "one", false)], snap));
    assert_eq!(a.unconfirmed.len(), 2, "{:?}", a.unconfirmed);

    // **The row that lands is the JOIN of both**, which is what the engine appends
    // for consecutive queued prompts.
    a.record_item(
        "s.0",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "one of mine\nand another".into(),
            }],
        },
    );
    assert!(a.pending_prompts.is_empty(), "{:?}", a.pending_prompts);
    assert!(
        a.unconfirmed.is_empty(),
        "a mark outlived the echo it was on: {:?}",
        a.unconfirmed
    );
}

/// **The live path and the snapshot path retire the same things.**
///
/// `App::record_item` read only the FIRST text part while `App::load` read every one,
/// so the same row could stand down one echo on one path and not the other — leticl
/// measured exactly this on its own head (*"the live arm read only the FIRST text part
/// where the snapshot path reads every part"*). Two ways into one rule is two rules;
/// this is the assertion that they are one.
#[test]
fn the_live_and_snapshot_paths_retire_the_same_echoes() {
    // Two echoes, so the two paths can differ about one without the whole thing
    // passing.
    let setup = || {
        let mut a = app();
        a.apply(hello(
            "s",
            vec![brief("s", "one", false)],
            Hub::new("s").snapshot(),
        ));
        typed(&mut a, "first of mine");
        a.key(Key::Enter);
        typed(&mut a, "second of mine");
        a.key(Key::Enter);
        a
    };
    let row = || TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text {
            text: "first of mine\nsecond of mine".into(),
        }],
    };

    let mut live = setup();
    live.record_item("s.0", row());
    let mut snap = setup();
    let mut s = Hub::new("s").snapshot();
    s.seq = 3;
    s.items.push(letibot_sessionlog::view::SnapshotItem {
        item_id: "s.0".into(),
        kind: "user".into(),
        ledger_head: "beef".into(),
        ts: 0,
        item: Some(row()),
    });
    snap.apply(hello("s", vec![brief("s", "one", false)], s));

    assert_eq!(
        live.pending_prompts, snap.pending_prompts,
        "the two paths disagreed about what a row answered"
    );
    assert!(
        live.pending_prompts.is_empty(),
        "{:?}",
        live.pending_prompts
    );
}

/// The piece arithmetic itself, where it is easier to read than through an `App`.
#[test]
fn strip_landed_reads_lines_not_strings() {
    fn go(entry: &str, row: &str) -> Option<String> {
        let lines: Vec<&str> = row.split('\n').collect();
        let mut claimed = vec![false; lines.len()];
        let mut cursor = 0;
        strip_landed(entry, &lines, &mut claimed, &mut cursor)
    }
    // Equality, by lines.
    assert_eq!(go("one", "one"), Some(String::new()));
    assert_eq!(go("one\ntwo", "one\ntwo"), Some(String::new()));
    // The row is the front run of the echo.
    assert_eq!(go("one\ntwo", "one"), Some("two".into()));
    // The row is a middle and a tail run.
    assert_eq!(go("one\ntwo\nthree", "two\nthree"), Some("one".into()));
    // Contained but not a line: untouched, every one of them.
    assert_eq!(go("two", "one\ntwo-guess"), None);
    assert_eq!(go("two", "one\nxtwo x"), None);
    assert_eq!(go("two", "one\ntwo is a longer line"), None);
    // A blank piece claims nothing, so an echo of blank lines is not retired by
    // a row of blank lines.
    assert_eq!(go("\n", "\n\n"), None);
}

#[test]
fn a_landing_row_retires_its_echo_and_a_piece_strips_the_front() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut a, "first");
    a.key(Key::Enter);
    typed(&mut a, "second");
    a.key(Key::Enter);
    assert_eq!(a.pending_prompts, vec!["first\nsecond".to_string()]);
    // The whole coalesced message lands: the echo stands down whole.
    a.record_item(
        "s.0",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "first\nsecond".into(),
            }],
        },
    );
    assert!(a.pending_prompts.is_empty());
    // A notice split the run, so the pieces land around it: the front piece
    // strips itself off the echo, the rest waits for its own row.
    typed(&mut a, "third");
    a.key(Key::Enter);
    typed(&mut a, "fourth");
    a.key(Key::Enter);
    a.record_item(
        "s.1",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "third".into(),
            }],
        },
    );
    assert_eq!(a.pending_prompts, vec!["fourth".to_string()]);
    a.record_item(
        "s.2",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "fourth".into(),
            }],
        },
    );
    assert!(a.pending_prompts.is_empty());
}

/// **The waiting words sit below the working, and move up when the daemon takes them** — two
/// operator reports, and they are the two halves of one rule.
///
/// *"queued above thinking"* (their screen, 2026-10-05) is the half that was wrong. Their
/// words, and directly under them the running turn's reasoning — and nothing answers those
/// words, because the daemon has not been given them, so nothing under them may look like an
/// answer. While nothing has taken them they belong at the tail, and the reply to the
/// PREVIOUS prompt is above them, where it belongs.
///
/// *"a message was queued to harnessd, delivered to model, reply started streaming above the
/// queued message and then some tick goes off and queued message dequeued and rendered
/// rightfully above the reply. pure ui desync."* is the half that must not come back — and it
/// is why the move happens at the moment the daemon TAKES the words: from then on the echo is
/// above the pane by construction, so no reply to it can appear over it.
///
/// **The move is asserted as a move, because the move is the ruling.** What is deliberately
/// not asserted is that the frame is the same frame — it is not, and it must not be: the two
/// facts are different facts, and the frame is the one place that says which one holds.
#[test]
fn a_queued_echo_waits_below_the_working_until_the_daemon_takes_it() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // The reply starts streaming FIRST, which is the latency the whole defect is about:
    // `Delta` carries its text and `TranscriptAppended` carries only an id.
    a.apply(ServerFrame::Event(env(
        2,
        testing::delta("t1", "R2 the reply being streamed now"),
    )));
    typed(&mut a, "Q2 the message queued mid-turn");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Prompt("Q2 the message queued mid-turn".into()))
    );
    let before = a.screen(100, 24);
    let before_text = before.join("\n");
    assert!(
        before_text.contains("▌ queued · Q2 the message queued mid-turn"),
        "{before_text}"
    );
    let echo = before
        .iter()
        .position(|l| l.contains("queued · Q2 the message"))
        .expect("the echo is on the screen");
    let reply = before
        .iter()
        .position(|l| l.contains("R2 the reply"))
        .expect("the reply is on the screen");
    assert!(
        reply < echo,
        "**the waiting words are BELOW the working**: the daemon has not taken them, so the \
             reply to the PREVIOUS prompt must not read as an answer to them — and the operator read \
             exactly that as *queued above thinking*:\n{before_text}"
    );

    // The daemon appends the row at its step boundary and announces it.
    a.apply(ServerFrame::Event(env(3, testing::appended("s.9", "user"))));
    let after = a.screen(100, 24);
    let after_text = after.join("\n");
    assert!(
        after_text.contains("▌ Q2 the message queued mid-turn"),
        "the row is drawn in the echo's place, from the words the head already had: \
             {after_text}"
    );
    // **And it no longer claims to be queued.** This assertion used to be `before == after` —
    // *not one row changes across the announcement* — and that was true only while the bound
    // row wore the same `queued` mark as the echo, which is the very thing R2 forbids: the
    // daemon had the words and the model could already be answering them. The rows beneath do
    // not move; the mark does, because the row has landed. The operator's own order, which is
    // what this test now pins: *"my message | your line | and only then unqueued."*
    assert!(
        // The MARK, not the word: this fixture's own message text contains *queued*, so a
        // bare `contains("queued")` would pass on the words and say nothing about the label.
        !after_text.contains("▌ queued · ") && !after_text.contains("▌ unconfirmed · "),
        "the announced row still carries a queued claim: {after_text}"
    );
    // **And the move is the ruling.** From this moment the words are above the reply, which is
    // the half of the report that must not come back: *"reply started streaming above the
    // queued message … pure ui desync."*
    let words_after = after
        .iter()
        .position(|l| l.contains("Q2 the message"))
        .expect("the words are on the screen");
    let reply_after = after
        .iter()
        .position(|l| l.contains("R2 the reply"))
        .expect("the reply is on the screen");
    assert!(
        words_after < reply_after,
        "once the daemon has taken the words they sit ABOVE the reply, so no reply to them can \
             appear over them:\n{after_text}"
    );
    // **The move is a move and not a second drawing.** Whatever else changes, the reader's own
    // sentence is on the screen exactly once, in each frame.
    assert_eq!(
        before_text.matches("Q2 the message").count(),
        1,
        "{before_text}"
    );
    assert_eq!(
        after_text.matches("Q2 the message").count(),
        1,
        "{after_text}"
    );
}

/// **The prompt is on the screen before the reply to it — requirement R2.**
///
/// Two channels with different latencies. Assistant text arrives as `Delta`,
/// carrying its text, and renders as it streams; a user row arrives as
/// `TranscriptAppended` — an id and a kind, **no text** — and only later as
/// `TranscriptContent`. So the reply is always faster to display than the prompt
/// that caused it: the operator watched their own sentence sit under the answer
/// to it, tagged `queued`, and then jump above it when the body finally landed.
///
/// The row is drawn from the echo the head already holds, in the row's own place,
/// from the announcement.
#[test]
fn a_queued_prompt_is_drawn_in_the_transcript_the_moment_its_row_is_announced() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut a, "also fix the parser");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Prompt("also fix the parser".into()))
    );
    // Before anything lands: the echo, tagged as what it is.
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("queued · also fix the parser"), "{screen}");

    // The daemon appends the row at its step boundary and announces it. No text
    // on the event — the head's own echo is all there is.
    a.apply(ServerFrame::Event(env(2, testing::appended("s.9", "user"))));
    // And the model's reply starts streaming, which is what used to reach the
    // screen first.
    a.apply(ServerFrame::Event(env(
        3,
        testing::delta("t1", "I'll fix the parser."),
    )));

    let screen = a.screen(100, 24).join("\n");
    let prompt = screen
        .find("also fix the parser")
        .unwrap_or_else(|| panic!("the prompt is not on the screen: {screen}"));
    let reply = screen
        .find("I'll fix the parser")
        .unwrap_or_else(|| panic!("the reply is not on the screen: {screen}"));
    assert!(
        prompt < reply,
        "the reply is above the prompt that caused it: {screen}"
    );
    // **And once.** The row is carrying the words now, so the tail echo must not
    // draw them a second time.
    assert_eq!(
        screen.matches("also fix the parser").count(),
        1,
        "the prompt is on the screen twice: {screen}"
    );

    // The body arrives and confirms the binding: the row becomes a settled user
    // block, in place, and the echo goes with it.
    a.apply(ServerFrame::Event(env(
        4,
        testing::content("s.9", "also fix the parser"),
    )));
    assert!(
        a.pending_prompts.is_empty(),
        "confirmed, so the echo retires"
    );
    assert!(a.bound_prompts.is_empty(), "and the binding is spent");
    let screen = a.screen(100, 24).join("\n");
    assert!(
        !screen.contains("queued"),
        "the row is settled and still says queued: {screen}"
    );
    assert_eq!(screen.matches("also fix the parser").count(), 1, "{screen}");
    // Still above the reply, now from its own body.
    assert!(
        screen.find("also fix the parser").unwrap() < screen.find("I'll fix the parser").unwrap(),
        "{screen}"
    );
}

/// **Not every announced `User` row is this head's prompt — the hazard.**
///
/// A harness steering notice and a §5.7 salvage notice are the same item shape as
/// a prompt (`harnessd::harness` says so in as many words), and so is another
/// attached head's prompt. The announcement carries nothing that tells them apart,
/// so the binding is a guess and **the retire waits for content matched by text**:
/// a head that retired on the announcement would lose the echo of a prompt still
/// sitting in the hub's queue, and the operator would watch their own sentence
/// vanish off the screen.
#[test]
fn a_notice_announced_while_a_prompt_is_queued_does_not_retire_its_echo() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut a, "also fix the parser");
    a.key(Key::Enter);

    // A steering notice is appended — the SAME shape as a prompt, and it steals
    // the optimistic binding, because the announcement cannot say it is not ours.
    a.apply(ServerFrame::Event(env(2, testing::appended("s.9", "user"))));
    assert!(
        a.bound_prompts.contains_key("s.9"),
        "the guess is made; there is nothing else to guess from"
    );
    // Content arrives and contradicts it: this row was never this head's words.
    a.apply(ServerFrame::Event(env(
        3,
        testing::content("s.9", "the spec changed - RFC 2812 rather than 1459"),
    )));
    assert_eq!(
        a.pending_prompts,
        vec!["also fix the parser".to_string()],
        "the announcement retired an echo whose prompt is still queued"
    );
    assert!(a.bound_prompts.is_empty(), "the wrong guess is dropped");
    // And both correct themselves: the notice renders from its own content, and
    // the operator's words are back at the tail, still saying `queued`, which is
    // still true.
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("RFC 2812"), "{screen}");
    assert!(
        screen.contains("queued · also fix the parser"),
        "the operator's own words left the screen: {screen}"
    );
    assert_eq!(screen.matches("also fix the parser").count(), 1, "{screen}");
}

/// **Two prompts in the air bind two rows, in the order they were sent.**
///
/// Oldest-first, and never an echo already bound: two rows announced while both
/// are queued must not both wear the first prompt's words. Idle submits land each
/// as their own entry (`queued_prompts_behind_a_running_turn_are_one_message`
/// covers the coalesced case), and both rows are in flight until their bodies
/// arrive.
#[test]
fn two_queued_prompts_bind_the_two_announcements_in_order() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    typed(&mut a, "first");
    a.key(Key::Enter);
    typed(&mut a, "second");
    a.key(Key::Enter);
    assert_eq!(
        a.pending_prompts,
        vec!["first".to_string(), "second".to_string()]
    );

    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s.10", "user"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("s.11", "user"),
    )));
    assert_eq!(
        a.bound_prompts.get("s.10").map(String::as_str),
        Some("first")
    );
    assert_eq!(
        a.bound_prompts.get("s.11").map(String::as_str),
        Some("second")
    );
    let screen = a.screen(100, 24).join("\n");
    // **Both rows are on the screen and neither claims to be queued** — they were announced, so
    // what the head owes is their bodies, not their rows. See [`echo_mark`]. The assertion this
    // replaces counted `queued · first` and `queued · second`, which was true only while a bound
    // row wore the echo's own mark.
    assert_eq!(screen.matches("▌ first").count(), 1, "{screen}");
    assert_eq!(screen.matches("▌ second").count(), 1, "{screen}");
    assert!(
        !screen.contains("▌ queued · "),
        "an announced row still claims to be queued: {screen}"
    );
    assert!(
        screen.find("first").unwrap() < screen.find("second").unwrap(),
        "the rows are drawn in the order they were announced: {screen}"
    );
}

/// **A binding dies with the row it was for.** A snapshot replaces every row, and
/// a binding whose row is gone has nothing left to stand for — otherwise the echo
/// would be suppressed at the tail for ever, on the strength of a row nobody has
/// any more.
#[test]
fn a_snapshot_keeps_only_the_bindings_whose_rows_are_still_bodyless() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut a, "still queued");
    a.key(Key::Enter);
    a.apply(ServerFrame::Event(env(2, testing::appended("s.9", "user"))));
    assert!(a.bound_prompts.contains_key("s.9"));

    // A resync of the same session whose rows do not include that one.
    let mut snap = Hub::new("s").snapshot();
    snap.seq = 5;
    snap.items.push(letibot_sessionlog::view::SnapshotItem {
        item_id: "s.10".into(),
        kind: "user".into(),
        ledger_head: "beef".into(),
        ts: 0,
        item: Some(TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "from the snapshot".into(),
            }],
        }),
    });
    a.apply(hello("s", vec![brief("s", "one", false)], snap));
    assert!(
        a.bound_prompts.is_empty(),
        "a row that is gone takes its guess with it"
    );
    assert_eq!(
        a.pending_prompts,
        vec!["still queued".to_string()],
        "and the words are still this head's to show"
    );
    let screen = a.screen(100, 24).join("\n");
    assert!(
        screen.contains("still queued"),
        "the echo is suppressed by a binding nothing can see: {screen}"
    );
    // **And it says `unconfirmed`, not `queued`** — R16's third mark.
    //
    // This test WAS the case the mark is for, and it asserted the wrong word until
    // the mark existed: a snapshot arrived, it did not carry this echo's row, and the
    // head went on saying *queued* — a claim about the daemon's queue that it can no
    // longer support, because it cannot tell *still coming* from *replaced by a fork*.
    // The operator, from leticl's screen and true here: an echo the head cannot resolve
    // must stop saying `queued`, *"which is a claim the head can actually support"*.
    assert!(
        screen.contains("queued · still queued"),
        "a snapshot that does not carry the row must stop claiming the daemon owes it: \
             {screen}"
    );
    assert_eq!(a.unconfirmed, vec!["still queued".to_string()]);

    // **And it retires the ordinary way when its row does land**, taking its mark
    // with it — the intersection. `unconfirmed` is a mark ON an echo, not a copy of
    // its text, so an echo that stands down must not leave the mark behind for ever.
    a.record_item(
        "s.11",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "still queued".into(),
            }],
        },
    );
    assert!(a.pending_prompts.is_empty());
    assert!(
        a.unconfirmed.is_empty(),
        "the mark outlived the echo it was on: {:?}",
        a.unconfirmed
    );
}

/// **R37: `Conversation` is the conversation and nothing the head did to produce it.**
///
/// The operator: *"and i want a special mode that hides tool calls and thinking
/// completely."* Measured before it was built: the ladder's bottom rung was `Terse`,
/// *"assistant text and tool outcomes only"* — so **terse still drew every tool row**, and
/// there was no rung that gave the conversation alone.
///
/// Asserted as a difference against `Terse` rather than as a list, because the list is
/// what a later edit forgets: the same fixture is drawn twice, once per rung, and what is
/// gone is exactly the working.
#[test]
fn the_conversation_rung_shows_the_conversation_and_nothing_else() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // One exchange of each kind: the operator's words, the model's answer, its reasoning,
    // a tool call with its result, and a head arrival.
    for (seq, event) in [
        (1, testing::appended("s.0", "user")),
        (2, testing::content("s.0", "please read the file")),
        (
            3,
            SessionEvent::TranscriptAppended {
                item_id: "s.1".into(),
                kind: "reasoning".into(),
                ledger_head: String::new(),
            },
        ),
        (
            4,
            SessionEvent::TranscriptContent {
                item_id: "s.1".into(),
                item: Box::new(letibot_transcript::TranscriptItem::Reasoning {
                    text: "I should look at the file first".into(),
                    field: letibot_transcript::ReasoningField::ReasoningContent,
                    truncated: false,
                }),
            },
        ),
        (5, testing::appended("s.2", "assistant")),
        (
            6,
            SessionEvent::TranscriptContent {
                item_id: "s.2".into(),
                item: Box::new(letibot_transcript::TranscriptItem::Assistant {
                    text: "the file says a thing".into(),
                    tool_calls: Vec::new(),
                    truncated: false,
                }),
            },
        ),
        (7, testing::appended("s.3", "tool_result")),
        (
            8,
            SessionEvent::TranscriptContent {
                item_id: "s.3".into(),
                item: Box::new(letibot_transcript::TranscriptItem::ToolResult {
                    call_id: "c1".into(),
                    name: "bash".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "MAGIC-TOOL-PAYLOAD".into(),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        ),
        (
            9,
            SessionEvent::HeadAttached {
                head_id: "h2".into(),
                kind: "tui".into(),
                identity: "somebody".into(),
            },
        ),
    ] {
        a.apply(ServerFrame::Event(env(seq, event)));
    }
    a.visibility = Visibility::of(Profile::LOUD);
    let loud = a.screen(100, 40).join("\n");
    assert!(loud.contains("please read the file"), "{loud}");
    assert!(loud.contains("the file says a thing"), "{loud}");

    // **And the same fixture under the new rung.** The conversation is all that is left.
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let quiet = a.screen(100, 40).join("\n");
    assert!(
        quiet.contains("please read the file"),
        "the operator's own words are the conversation: {quiet}"
    );
    assert!(
        quiet.contains("the file says a thing"),
        "and so is the model's answer: {quiet}"
    );
    for hidden in [
        "MAGIC-TOOL-PAYLOAD",
        "I should look at the file first",
        "bash",
        "h2",
    ] {
        assert!(
            !quiet.contains(hidden),
            "`{hidden}` is the working and must not be drawn: {quiet}"
        );
    }
    // **And the name is on the screen.** R29's remedy rule takes the form R37 gives it:
    // the MODE is named rather than a placeholder per hidden row — which is the thing the
    // operator asked to be rid of.
    assert!(
        quiet.contains("conversation"),
        "the rung must be named on the screen: {quiet}"
    );
    // **It is a view.** Switching back draws every row again, including the span it was
    // on, and nothing was dropped from the head's own copy.
    // **And the reasoning under the new rung is hidden by the RUNGS, not by the fold.**
    // Two different mechanisms want it off the screen — `thinking: folded` folds the
    // block, and this rung removes it — so the assertion above is only worth anything if
    // the fold is open. Otherwise it passes for the wrong reason, which is the defect
    // this test exists against.
    a.visibility = Visibility::of(Profile::LOUD);
    a.reasoning = Fold::Open;
    a.invalidate_history();
    let loud_open = a.screen(100, 200).join("\n");
    assert!(
        loud_open.contains("I should look at the file first"),
        "the fold is open, so the text is reachable: {loud_open}"
    );
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    assert!(
        !a.screen(100, 200)
            .join("\n")
            .contains("I should look at the file first"),
        "and the rung is what removes it, with the fold open"
    );

    // **It is a view.** Switching back draws every row again, including the span it was
    // on, and nothing was dropped from the head's own copy.
    a.visibility = Visibility::of(Profile::LOUD);
    a.invalidate_history();
    let back = a.screen(100, 200).join("\n");
    assert!(
        back.contains("MAGIC-TOOL-PAYLOAD") && back.contains("I should look at the file first"),
        "a rung is a view: switching back must restore every row\n{back}"
    );
    // **Four ITEMS**: the operator's message, the reasoning, the answer and the result.
    // A `HeadAttached` is a note rather than a row, which is why the assertion is four —
    // the first draft said five and the failure named the miscount rather than the
    // mechanism, which is the useful direction.
    assert_eq!(a.items.len(), 4, "nothing left the head's copy");
    // **And an arrival is a fact the head holds, not a row it draws.** `HeadAttached` is
    // counted on every rung and drawn only at `Loud`, which is what the rung's own doc
    // says — so the assertion is the COUNT, because a rung that dropped the fact rather
    // than the row would make the header wrong about how many heads are on the session.
    // The seat's own snapshot carries no heads, so the one that is counted here is the
    // arrival the fixture sent — held where it is not drawn, which is what this asserts.
    assert_eq!(a.heads, 1, "the arrival is held even where it is not drawn");
}

/// **What this rung may NOT hide, and each for its own reason** (R37).
///
/// Three, and only one of them is about this head's taste: warnings were ruled by
/// `Verbosity::Terse`'s own docstring, decision cards make the session unanswerable if
/// they go, and liveness is this rung's specific risk — *a ten-minute tool-heavy turn
/// draws nothing at all*.
#[test]
fn the_conversation_rung_hides_no_warning_no_card_and_no_liveness() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A warning, which is a fact the daemon chose to interrupt with.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Warning {
            code: "context_wall".into(),
            detail: "WARNING-MAGIC-SENTENCE".into(),
            compaction: None,
        },
    )));
    // A turn in flight, with a tool call whose output is arriving.
    a.apply(ServerFrame::Event(env(2, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: "exec".into(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        testing::tool_progress("c1", "still going"),
    )));
    // And a gate card waiting for an answer.
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::DecisionRequested {
            write_targets: Vec::new(),
            req_id: "r1".into(),
            kind: "permission".into(),
            call_id: Some("c1".into()),
            access: String::new(),
            summary: "run the thing".into(),
            target: String::new(),
            detail: String::new(),
            options: Vec::new(),
            choices: Vec::new(),
            because: String::new(),
            advice: None,
            subagent: None,
            deadline: None,
            on_timeout: letibot_sessionlog::event::OnTimeout::Deny,
        },
    )));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let frame = a.screen(100, 40);
    let all = frame.join("\n");
    assert!(
        all.contains("WARNING-MAGIC-SENTENCE"),
        "a warning is not on the ladder: {all}"
    );
    assert!(
        all.contains("?") && all.contains("permission"),
        "the decision card is the question the session is waiting on: {all}"
    );
    // **Liveness.** With the tool rows hidden, the only thing on screen that says a turn
    // is running is the composer's border — so it must still say so, and the elapsed
    // time with it. R13: a reader who cannot tell working from wedged is the confusion
    // this document has spent the day correcting in its own instruments.
    assert!(
        all.contains("Responding"),
        "the turn's own status line must survive the rung: {all}"
    );
}

/// **R37's consequence for R36**: a rung that hides rows can hide the one the reader is
/// holding, and the view moves to the nearest surviving row **and says so**.
#[test]
fn a_rung_that_hides_the_held_row_moves_the_view_and_says_so() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A transcript whose rows alternate: conversation, tool result, conversation, …
    for i in 0..30u64 {
        let (a_id, b_id) = (format!("s.a{i}"), format!("s.b{i}"));
        a.apply(ServerFrame::Event(env(
            i * 4 + 1,
            testing::appended(&a_id, "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 4 + 2,
            testing::content(&a_id, &format!("answer {i}")),
        )));
        a.apply(ServerFrame::Event(env(
            i * 4 + 3,
            testing::appended(&b_id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 4 + 4,
            SessionEvent::TranscriptContent {
                item_id: b_id,
                item: Box::new(letibot_transcript::TranscriptItem::ToolResult {
                    call_id: format!("c{i}"),
                    name: "bash".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "tool payload".into(),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    a.screen(100, 30);
    // **The anchor is put on a TOOL row deliberately**, because that is the case being
    // tested and a wheel notch lands wherever the arithmetic takes it — the first draft
    // scrolled and happened to stop on an assistant row, so the re-anchor never fired and
    // the test asserted that a thing which had not happened had not said anything.
    let tool_row = a
        .items
        .iter()
        .position(|it| it.kind == "tool_result")
        .expect("the fixture has tool rows");
    a.anchor = Some(Held {
        item_id: a.items[tool_row].item_id.clone(),
        ordinal: tool_row,
        into: 0,
    });
    let held = a.anchor.clone().expect("a held row");

    // Switch to the rung, which hides every other row here — and the assertion that the
    // held row IS one of them comes after the switch, because `hidden_by_rung` is a
    // question about the rung and asking it under `Normal` answers `false` for
    // everything. (The first draft asked it before the switch and passed on the wrong
    // side of the same mistake.)
    a.visibility = Visibility::of(Profile::CONVERSATION);
    assert!(
        a.hidden_by_rung(held.ordinal),
        "the fixture must hold a hidden row"
    );
    a.reanchor_off_hidden();
    let now = a
        .anchor
        .clone()
        .expect("still holding, on a row that shows");
    assert!(
        !a.hidden_by_rung(now.ordinal),
        "the view is holding a row the rung hides: {now:?}"
    );
    assert!(
        a.notice.as_deref().is_some_and(|n| n.contains("hides")),
        "and it says so rather than jumping quietly: {:?}",
        a.notice
    );
    // **The nearest** surviving row, not an arbitrary one: within one row of where the
    // reader was, since this fixture hides every other row.
    let distance = (now.ordinal as isize - held.ordinal as isize).unsigned_abs();
    assert!(
        distance <= 1,
        "the view jumped {distance} rows instead of to the neighbour: {held:?} -> {now:?}"
    );
}

/// **The rung is a rung**: the ladder cycles through it, and the verb says which rung is
/// on. The name is letibot's to choose and both heads use it (R37) — this pins the
/// spelling so it cannot drift into a second word for one view.
#[test]
fn the_ladder_cycles_through_the_conversation_rung_by_name() {
    assert_eq!(Verbosity::Conversation.as_str(), "conversation");
    let mut v = Verbosity::Conversation;
    for want in ["terse", "normal", "loud", "conversation"] {
        v = v.next();
        assert_eq!(v.as_str(), want);
    }
    // And only the one rung is a scope.
    assert!(Verbosity::Conversation.hides_the_working());
    for r in [Verbosity::Terse, Verbosity::Normal, Verbosity::Loud] {
        assert!(!r.hides_the_working(), "{r:?}");
    }
    // `keeps` is the item-level question, and it is the same line: the conversation in,
    // everything the head made out.
    use letibot_transcript::{ReasoningField, TranscriptItem as T};
    let user = T::User {
        speaker: Default::default(),
        parts: vec![letibot_transcript::UserPart::Text { text: "hi".into() }],
    };
    let answer = T::Assistant {
        text: "hello".into(),
        tool_calls: Vec::new(),
        truncated: false,
    };
    let thought = T::Reasoning {
        text: "hmm".into(),
        field: ReasoningField::ReasoningContent,
        truncated: false,
    };
    for kept in [&user, &answer] {
        assert!(Verbosity::Conversation.keeps(kept), "{kept:?}");
    }
    assert!(!Verbosity::Conversation.keeps(&thought));
    // And the registers keep everything, which is what makes this a rung rather than a
    // change to the ladder's meaning.
    for r in [Verbosity::Terse, Verbosity::Normal, Verbosity::Loud] {
        assert!(r.keeps(&thought), "{r:?}");
    }
}

/// **R16: an echo whose prompt landed under a transcript that was then replaced.**
///
/// Measured on this head 2026-09-22: three prompts still rendering `queued ·`
/// while the tree was clean, no turn was running, and the work they asked for was
/// already committed. R2 says exactly why the mark can outlive the fact — an echo
/// binds to an announced row and retires on `TranscriptContent` **matched by
/// text** — and a compaction **replaces the transcript**, so the row those echoes
/// were waiting for was summarised away and the retiring event never arrives.
///
/// The fix is at the fork and it is exact rather than approximate: the echoes that
/// were in the air **when the fork began** are resolved, and the ones queued after
/// it are left alone, because those belong to the new transcript and their rows
/// are still coming. Both halves are asserted here, and the second matters as much
/// as the first: retiring an echo whose row has not landed takes a sentence off
/// the screen, which is §4.2's defect with a new cause.
///
/// **Why the shape is "mark on the way in, resolve on the way out".** A fork
/// announces itself twice — `auto_compact` *before* it happens and `compacted`
/// *after* — and only the first of those can say what was in the air. A manual
/// `/compact` has no "before" warning at all, so the head takes its own mark as
/// it sends the request. Both are the head's own bookkeeping on facts it already
/// has; no daemon change is involved.
#[test]
fn a_compaction_resolves_the_echoes_it_supersedes() {
    let hub = Hub::new("s");
    let att = hub.attach("tui", "test", Caps::default(), 0);
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    hub.publish(testing::turn_started("t1"));
    feed(&mut a, &hub, &att.head_id);
    assert!(a.turn_busy(), "the premise: a turn is running");

    // The operator types and sends. The hub takes the prompt as a follow-up user
    // item and the head echoes it — the R2 window, and the only place the words
    // exist until the step boundary.
    typed(&mut a, "also fix the parser");
    assert!(matches!(a.key(Key::Enter), Some(Action::Prompt(_))));
    let queued = a.screen(100, 24).join("\n");
    assert!(queued.contains("queued · also fix the parser"), "{queued}");

    // **The fork, in the order the daemon publishes it.** `auto_compact` says the
    // conversation is about to be replaced; `compacted` says it has been; and the
    // new base arrives as rows. The prompt's own row was in the transcript that
    // was summarised away, so no `TranscriptContent` for it ever comes — which is
    // the defect, and it is this test's own premise.
    hub.publish(SessionEvent::Warning {
        code: "auto_compact".into(),
        detail: "938065 of 999999 tokens resident — compacting now".into(),

        compaction: None,
    });
    hub.publish(SessionEvent::Warning {
        code: "compacted".into(),
        detail: "compacted: 940188 → 9181 tokens, on transcript s#t25".into(),

        compaction: None,
    });
    hub.publish(SessionEvent::TranscriptAppended {
        item_id: "s#t25.0".into(),
        kind: "system".into(),
        ledger_head: String::new(),
    });
    hub.record_item(
        "s#t25.0",
        TranscriptItem::System {
            text: "This conversation was compacted: everything said before this point \
                       is replaced by the summary below…"
                .into(),
            origin: letibot_transcript::SystemOrigin::Update,
        },
    );
    feed(&mut a, &hub, &att.head_id);

    // **The mark is gone, and it is gone because the fork resolved it.** The
    // prompt's words are still in the conversation — as prose in the summary — and
    // what left the screen is the claim that the daemon had not taken them yet.
    let after = a.screen(100, 24).join("\n");
    assert!(
        !after.contains("queued ·"),
        "the mark outlived the transcript it was waiting for: {after}"
    );
    assert!(
        a.pending_prompts.is_empty(),
        "the echo is still held: {:?}",
        a.pending_prompts
    );

    // **And one queued after the fork is left alone**, because its row is still
    // coming. This is the half that must not be lost, and the next fork is what
    // takes it — proving the mark is re-taken per fork rather than spent once.
    typed(&mut a, "and now the next thing");
    assert!(matches!(a.key(Key::Enter), Some(Action::Prompt(_))));
    let still = a.screen(100, 24).join("\n");
    assert!(still.contains("queued · and now the next thing"), "{still}");

    hub.publish(SessionEvent::Warning {
        code: "auto_compact".into(),
        detail: "compacting again".into(),

        compaction: None,
    });
    hub.publish(SessionEvent::Warning {
        code: "compacted".into(),
        detail: "compacted again".into(),

        compaction: None,
    });
    feed(&mut a, &hub, &att.head_id);
    let last = a.screen(100, 24).join("\n");
    assert!(
        !last.contains("queued ·"),
        "the second fork left its own echoes behind: {last}"
    );

    // **A manual fork takes its mark on the way out**, because the daemon only
    // says `compacted` — after the fact — for one the head asked for.
    let mut b = app();
    b.session_id = "s".into();
    b.pending_prompts.push("typed while the turn ran".into());
    assert_eq!(b.command("compact"), Some(Action::Compact));
    assert_eq!(
        b.fork_pending,
        vec!["typed while the turn ran".to_string()],
        "a manual compaction marked nothing"
    );
    b.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Warning {
            code: "compacted".into(),
            detail: "compacted".into(),

            compaction: None,
        },
    )));
    assert!(
        b.pending_prompts.is_empty(),
        "the manual fork resolved nothing"
    );
}

/// **A refusal's reasoning folds, and the envelope never shows.** Both halves
/// of *"it just throws up on my chat"*: layer A's reason is a document, and the
/// row under it was carrying the marker the model reads.
#[test]
fn a_reason_that_is_a_document_folds_to_its_first_sentence() {
    assert!(is_envelope("<<<TOOL_ERROR 5ebfdef6>>>"));
    assert!(is_envelope("  <<<END_TOOL_ERROR 5ebfdef6>>>  "));
    assert!(
        !is_envelope("< < <TOOL_ERROR 5ebfdef6>>>"),
        "a neutralised body line"
    );
    assert!(!is_envelope("error: could not find `Cargo.toml`"));

    let doc = "this command's meaning does not exist yet, so nothing can decide \
                   about it. The grammar read 315 bytes and could not resolve:\n  \
                   parameter_expansion at 1:2 decides the assignment";
    let gist = first_sentence(doc);
    assert_eq!(
        gist,
        "this command's meaning does not exist yet, so nothing can decide about it."
    );
    assert!(!gist.contains("parameter_expansion"));
    // A reason that IS a sentence is left exactly alone.
    let one = "the workspace has no writable backend";
    assert_eq!(first_sentence(one), one);
}

/// **The chord is the head's, is named where it is advertised, and moves nothing else.**
///
/// Three separate things a chord has to get right, and each has already been got wrong
/// somewhere in this file: it must not type into the composer, it must be on the hint bar
/// where the operator can actually see it (the bar is longer than a terminal and trimmed),
/// and it must not move a fold or open a pane.
#[test]
fn the_notes_chord_is_advertised_where_it_can_be_seen_and_reaches_nothing_else() {
    let mut a = app();
    typed(&mut a, "hello");
    a.key(Key::CtrlN);
    assert_eq!(a.input(), "hello", "ctrl-n typed into the composer");
    assert_eq!(a.reasoning, Fold::Folded, "ctrl-n moved the thinking fold");
    assert_eq!(a.tools, Fold::Folded, "ctrl-n moved the tool-output fold");
    assert!(
        !a.jobs_pane && !a.todos_pane && !a.picker,
        "ctrl-n opened a pane"
    );
    assert!(!a.raw_calls, "ctrl-n toggled the raw view");

    // **On the bar, at 80 columns.** The bar is 151 columns now and the frame trims it,
    // so a chord appended to the end is a chord nobody can see — measured, not assumed.
    let bar = a.screen(80, 24).join("\n");
    assert!(
        bar.contains("ctrl-n notes"),
        "the chord is advertised but not visible at 80 columns:\n{bar}"
    );
    // And it does not push `ctrl-s` off, which is what the bar opens with.
    assert!(bar.contains("ctrl-s sessions"), "{bar}");

    // `/help` names it, with the rule that matters on the line.
    a.command("help");
    let help = a.screen(120, 60).join("\n");
    assert!(help.contains("ctrl-n"), "{help}");
    assert!(
        help.contains("Retired is not deleted"),
        "the row must carry the rule R10 bought: {help}"
    );
}

/// **The operator's own sentence, as the head draws it.**
///
/// The other half of `sudo_ask_live.rs`'s assertion, and it is here because this is where
/// the head is. That file measures the daemon — that `operator_run_unreadable` is
/// **published** on the session log for a run it may not look at — and cannot measure what
/// a screen does with it: `letibot-harnessd` has no head in it. So the one thing the
/// operator's report turns on is pinned on this side: the warning is not one of the codes
/// that is counted on the edge and never drawn, and it is not filtered as a duplicate of
/// something the turn already says.
///
/// The text is the real sentence, at its real length, because length is half of what could
/// go wrong: a note longer than the screen folds to its head, and a fold that lost the code
/// would leave the person with a paragraph and no name for it.
#[test]
fn the_sentence_for_a_run_the_daemon_may_not_look_at_is_drawn() {
    let sentence = "`sudo apt install mc` has been quiet for a beat and this daemon cannot \
                        tell whether it is waiting for a line: one of its processes belongs to \
                        another user, so `/proc` refuses for it. If it is waiting — `sudo` \
                        reaching `apt`'s `Continue? [Y/n]` is the case this was measured on — \
                        the way in is `!send <line>`, which needs no card. Until the command ends \
                        it keeps a thread of its own; the rest of the daemon runs on, and another \
                        `!` line of yours waits for this one rather than overlapping it.";
    let mut a = App::new(plain_cfg(120));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Warning {
            code: "operator_run_unreadable".into(),
            detail: sentence.into(),
            compaction: None,
        },
    )));
    let screen = a.screen(120, 20).join("\n");
    assert!(
        screen.contains("operator_run_unreadable"),
        "the sentence never reached the screen — which is the operator's own report, and \
             the whole of what this test is for:\n{screen}"
    );
    // **And it is a ROW, not a counter.** `ALARM_ONLY` moves a code out of the conversation
    // and onto the triangle; a fault drawn only as a number in `/status` is a fault nobody
    // reads, and this one is the pointer to `!send`.
    assert!(
        !letibot_sessionlog::warning::to_the_alarm("operator_run_unreadable"),
        "the code is edge-bound, so a head would never draw the way in"
    );
    // **And the way in survives the fold.** The sentence is a document and folds to its
    // head; what may not be folded away is the verb the person needs.
    assert!(
        screen.contains("!send") || a.notes_lines().iter().any(|l| l.contains("!send")),
        "neither the screen nor `/notes` kept the way in:\n{screen}"
    );
}

/// **A note that is a document folds to its head, and `/notes` still has all of it.**
///
/// The other half of R10: the operator's wall was *two* gate timeouts at about
/// thirteen lines each — *"how to remove this red wall?"*. The instrument is the one
/// every other long thing on this screen already uses (a card's diff rows, an
/// elision row naming where the rest is), and the fold must not be a cap on the
/// record: the whole sentence is one verb away.
#[test]
fn a_note_that_is_a_document_folds_to_its_head_and_the_verb_has_the_rest() {
    let mut a = app();
    let long = format!(
        "denied: the gate timed out before anybody answered, so the call did not run.\n{}",
        (1..12)
            .map(|i| format!("  line {i} of the rule it cites"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    a.note(Note::Warned(Warned {
        code: "gate_timeout".into(),
        detail: long.clone(),
        ts: 7,
    }));
    let screen = a.screen(100, 40).join("\n");
    // The head of it, and a seam that names the verb.
    assert!(screen.contains("gate_timeout"), "{screen}");
    assert!(screen.contains("anybody answered"), "{screen}");
    assert!(screen.contains("· /notes"), "the seam is missing: {screen}");
    assert!(
        !screen.contains("line 11 of the rule it cites"),
        "the whole document is on the screen: {screen}"
    );
    // **Twelve lines is three plus nine**, so the seam counts what it took out.
    assert!(screen.contains("+9 lines"), "{screen}");
    // And the record is whole where the seam points.
    let listed = a.notes_lines().join("\n");
    assert!(listed.contains("line 11 of the rule it cites"), "{listed}");
    assert!(listed.contains("line 1 of the rule it cites"), "{listed}");
}

/// **R10's ruling on `ctrl-v`: one row, not the conversation.**
///
/// The chord flips the tool fold for the **whole** conversation while the pager's
/// own seam advertises it as `… +N lines · ctrl-v`, which reads per-row. The
/// operator was surprised *"that ctrl-t triggered the wall AT ALL"*, and the fix is
/// to keep the meaning a per-row seam can honestly name: the chord opens one row's
/// window, and the conversation-wide unfold keeps the verb it already had.
#[test]
fn ctrl_v_opens_one_rows_window_and_the_whole_folds_are_the_verb() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    let long: String = (0..40).map(|i| format!("line {i}\n")).collect();
    a_result_row(&mut a, 2, "i1", &long);

    // **The seam names the chord on the row the chord reaches**, and it is the only
    // long row here.
    let folded = a.screen(100, 40).join("\n");
    assert!(folded.contains("ctrl-v opens it"), "{folded:?}");

    // The chord opens that row's window and **does not unfold the other rows**: the
    // fold is the verb's, and one chord doing both is what made this a wall.
    a.key(Key::CtrlV);
    assert_eq!(a.payload_sel.as_deref(), Some("i1"));
    assert!(
        !a.tools.is_open(),
        "ctrl-t unfolded the whole conversation as well"
    );
    let open = a.screen(100, 40).join("\n");
    assert!(
        open.contains("pages down"),
        "the window did not open: {open:?}"
    );

    // Pressing it again closes the window and leaves everything else alone.
    a.key(Key::CtrlV);
    assert_eq!(a.payload_sel, None);
    assert!(!a.tools.is_open());

    // **And the conversation-wide unfold is `/t`**, which is where it already lived.
    assert_eq!(a.command("t"), None);
    assert!(a.tools.is_open(), "/t no longer unfolds tool output");
    assert_eq!(
        a.command("tools"),
        None,
        "`/tools` is the listing verb, not a second fold"
    );
}

/// **R40: the bar says what `ctrl-t` does, and names the verb that does the rest.**
///
/// The operator, reading a real screen: *"als why Ct stopped expanding tools??? at least in
/// letibt"*. Two defects, and only one of them was letibot's.
///
/// **This one was letibot's, and it is R29's rule failing on the bar instead of on a
/// note.** `ctrl-t` was narrowed deliberately (R10, above) — it opens one row's window and
/// the conversation-wide unfold is `/t` — and the bar still read `ctrl-t long output`,
/// which is the reading the ruling removed. So the key advertised at the bottom of the
/// screen did not do what the bar said, and `/t` was on the bar nowhere: the remedy the
/// operator needed was real, existed, and was not offered.
///
/// **The bar's own arithmetic is R22's and it is measured here rather than asserted.**
/// This bar is longer than 80 columns whatever it says — nine entries and room for about
/// five — so what is *visible* is a decision. Two of them are pinned (R22: `ctrl-s` first,
/// `ctrl-n` second, because a reflex worth advertising has to be inside the frame) and the
/// rest are ordered by what a reader cannot find out any other way. That is the test's last
/// assertion, and it is the reason `tab completes /commands` is the entry that gave way.
#[test]
fn the_bar_says_what_ctrl_v_does_and_names_the_verb_that_does_the_rest() {
    let mut a = app();
    let bar = a.screen(210, 24).pop().unwrap_or_default();

    // The words are true: one row, not the conversation.
    assert!(bar.contains("ctrl-v"), "the chord left the bar: {bar}");
    assert!(
        !bar.contains("long output"),
        "the bar still says the conversation-wide thing ctrl-t stopped doing: {bar}"
    );
    // And the verb that DOES fold the conversation is on it.
    assert!(
        bar.contains("/t"),
        "`/t` is the whole fold and the bar does not name it: {bar}"
    );
    // Adjacent, because they are the two a reader confuses: one result, all of them.
    assert!(
        bar.contains("ctrl-v newest result · /t all tool rows"),
        "the pair is not stated as a pair: {bar}"
    );

    // **Both halves of the claim are driven, on one screen.** A bar that is right about a
    // chord that does something else is the defect respelled.
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    let long: String = (0..40).map(|i| format!("line {i}\n")).collect();
    a_result_row(&mut a, 2, "i1", &long);
    a.key(Key::CtrlV);
    assert_eq!(
        a.payload_sel.as_deref(),
        Some("i1"),
        "the bar says `newest result` and ctrl-t opened no window"
    );
    assert!(
        !a.tools.is_open(),
        "the bar says one result and ctrl-t folded them all"
    );
    a.key(Key::CtrlV);
    assert_eq!(a.command("t"), None);
    assert!(
        a.tools.is_open(),
        "the bar says `/t all tool rows` and /t did not unfold them"
    );
    assert!(
        a.payload_sel.is_none(),
        "`/t` is the conversation-wide fold and it opened a row's window"
    );

    // **Tab gave up its space, and gets to keep its fact.** The bar no longer names it —
    // it is the entry that answers before it is named — and `/help` still does, on the row
    // a reader is looking at when they go there. Dropping it from both would have been a
    // different change.
    assert!(
        !bar.contains("tab completes"),
        "the bar still spends 23 columns on the one entry that needs no advertisement: {bar}"
    );
    assert!(
        !bar.contains("tab"),
        "tab is still on the bar under another spelling: {bar}"
    );
    let help = help_lines(&a.cfg, 140).join("\n");
    assert!(
        help.contains("tab"),
        "tab left the bar AND the help screen, which is not the trade: {help}"
    );
    assert!(
        bar.contains("/help"),
        "the index has to stay on the bar — R29's remedy rule needs it there: {bar}"
    );

    // **The measurement, recorded.** R22's arithmetic is that a chord past the frame's
    // width is a chord nobody has, so the numbers are pinned rather than remembered.
    // (`trim` is the frame's gutter, which is not the bar's own width.)
    //
    // **Re-measured 2026-10-03 for R56's rework**, which added `ctrl-p hold` and moved the
    // todos pane to `ctrl-t` and the payload window to `ctrl-v`: 146 → 160, the new entry's
    // own width. What is visible at 80 and at 120 is unchanged (the two pinned entries at 80,
    // the `ctrl-v newest result · /t all` pair whole at 120), which the assertions below keep.
    assert_eq!(
        bar.trim().chars().count(),
        160,
        "the bar's width changed; re-measure what is visible at 80: {bar}"
    );
    let at80 = a.screen(80, 24).pop().unwrap_or_default();
    assert!(
        at80.contains("ctrl-s sessions") && at80.contains("ctrl-n notes"),
        "R22's two pinned entries left the first 80 columns: {at80}"
    );
    assert!(
        !at80.contains("ctrl-v"),
        "the pair now fits at 80 — better than the measurement, so update it: {at80}"
    );
    let at120 = a.screen(120, 24).pop().unwrap_or_default();
    assert!(
        at120.contains("ctrl-v newest result · /t all"),
        "the pair is not whole at 120, which is where it was measured to be: {at120}"
    );
}

#[test]
fn a_recorded_session_renders_without_a_daemon() {
    // W8 is a leaf: give it a recorded log and it is built and demoed before a
    // turn engine exists.
    let mut a = app();
    for e in testing::recorded_session() {
        a.apply(ServerFrame::Event(letibot_sessionlog::event::Envelope {
            session_id: "s".into(),
            seq: a.seq + 1,
            ts: 0,
            event: e,
        }));
    }
    for (id, item) in testing::recorded_items() {
        a.record_item(&id, item);
    }
    let screen = a.screen(80, 30);
    assert_eq!(screen.len(), 30);
    assert!(screen.iter().all(|l| line_width(l) <= 80));
    let joined = screen.join("\n");
    assert!(joined.contains("cached"), "the status of the turn is shown");
}

/// **The answer to the re-ask is APPLIED.** A `Sessions` frame replaces `self.sessions`, and
/// `self.sessions` is what the subagent rows and the composer's count are folded from — so a
/// list that lands without a fold moves the picker and leaves the pane and the count standing
/// on the list that was just thrown away.
///
/// That is the defect the arm had, and it is what made a re-ask useless: the answer to *what
/// are my children now* arrived and was not read. A re-ask whose reply is dropped is worse
/// than no re-ask, because it looks like one.
#[test]
fn a_sessions_reply_re_folds_the_rows_and_the_count() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "parent", false)],
        Hub::new("s").snapshot(),
    ));
    assert!(
        a.subagents.is_empty(),
        "the premise: no child is listed yet"
    );
    assert!(
        !count_row(&mut a).contains("subagent running"),
        "the premise: and none is counted"
    );

    // The daemon's answer to the `ListSessions` the seating asked for. **A `Sessions` frame,
    // not a `Hello`** — this is the reply the re-ask actually produces, and it is the one the
    // arm used to read only for the picker.
    a.apply(ServerFrame::Sessions {
        sessions: a_family(),
        current: "s".into(),
        created: None,
    });
    let ids: Vec<&str> = a.subagents.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(ids, vec!["s-sub-1", "s-sub-2"], "{ids:?}");
    let row = count_row(&mut a);
    assert!(
        row.contains("1 subagent running"),
        "the list reply did not reach the count: {row}"
    );
}

/// **The family view draws the fold glyph its own rule implies.**
///
/// The rows are on the list because this head is standing among them ([`App::family_open`]),
/// not because the operator pressed `→` — and the glyph beside the parent came from the
/// collapse list alone, so a family view drew `▸` directly above the three rows it was
/// claiming to hide. One rule, read by the enumeration and by the glyph: what the tree draws
/// and what the tree lists are the same fact.
#[test]
fn the_family_views_fold_glyph_says_open_because_its_rows_are_shown() {
    let mut a = inside_a_subagent();
    a.key(Key::CtrlS);
    let screen = a.screen(100, 30).join("\n");
    // The parent's ROW, not the header above it — the header names this head's session,
    // which is the child, and it carries the `subagent of <parent>` label.
    let parent = screen
        .lines()
        .find(|l| l.contains("parent") && (l.contains("+ ") || l.contains("- ")))
        .unwrap_or_else(|| panic!("the parent's row is on the list:\n{screen}"));
    assert!(
        parent.contains('-'),
        "the parent is not drawn open over its own visible children (fold `-`): {parent}"
    );
    // And nothing was expanded to make that true — the rule is the view's, not the
    // operator's act.
    assert!(a.expanded.is_empty());
    // A conversation at the top of the tree keeps the ordinary rule: not expanded is `▸`.
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.key(Key::CtrlS);
    let screen = a.screen(100, 30).join("\n");
    let parent = screen
        .lines()
        .find(|l| l.contains("parent") && (l.contains("+ ") || l.contains("- ")))
        .unwrap_or_else(|| panic!("the parent's row is on the list:\n{screen}"));
    assert!(
        parent.contains('+'),
        "a collapsed conversation is not drawn open — its fold is `+`, not `-`: {parent}"
    );
    assert!(!screen.contains("audit the store"), "{screen}");
}

/// **Tab unfolds, like enter.** The operator, after using the pane: *"i also
/// feel like I want Tab to expand the todo row"*. It completes a `/command`
/// while one is being typed, and the composer is empty here — the same
/// condition enter already carried.
#[test]
fn tab_unfolds_an_item_and_still_completes_a_command() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-tab-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::write(
        dir.join("TODO.md"),
        concat!(
            "## Phase 0\n",
            "\n",
            "- [ ] **T1** head\n",
            "  the body line\n"
        ),
    )
    .expect("write");
    let mut a = app();
    a.wiring.workspace = dir.display().to_string();
    a.key(Key::CtrlT);
    assert!(!a.screen(110, 30).join("\n").contains("the body line"));
    // **The cursor starts on the ADD CONTROL**, which is the head of `todos_stops` and
    // leticl's own starting position — so one Down is what puts it on the first repo item. The
    // row under the cursor is the row Enter and Tab act on, which is the whole point of one
    // enumeration; before this change the cursor had nowhere else to be and these keys were
    // reading a different list from the one the pane drew.
    a.key(Key::Down);
    a.key(Key::Tab);
    assert!(
        a.screen(110, 30).join("\n").contains("the body line"),
        "tab unfolds"
    );
    a.key(Key::Tab);
    assert!(
        !a.screen(110, 30).join("\n").contains("the body line"),
        "and folds"
    );

    // With something typed, Tab is still the completion key: the pane does
    // not get to eat it just because it is open.
    for c in "/mod".chars() {
        a.key(Key::Char(c));
    }
    a.key(Key::Tab);
    assert!(
        a.editor.text().starts_with("/mode"),
        "completion still works: {:?}",
        a.editor.text()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **An item's detail is under it in the file and was thrown away.**
///
/// `- [x] **T2** vendor yason …, pinned in` continues on the next line with
/// the commits it pins; the pane showed the first line and stopped, so the
/// item trailed off mid-sentence. The operator: *"if a todo has some
/// associated text? should i be able to expand it somehow?"*
#[test]
fn an_items_continuation_lines_are_kept_and_unfold_on_enter() {
    // Written with explicit newlines rather than `\`-continuations: that
    // escape eats the leading whitespace of the next line, which is exactly
    // the indentation this test is about.
    let body = concat!(
        "## Phase 0\n",
        "\n",
        "- [x] **T1** vendor the deps, pinned in\n",
        "  `scripts/bootstrap.sh` (yason `0c84b29`).\n",
        "  Deps: none.\n",
        "- [ ] **T2** no body at all\n",
        "\n",
        "Prose after a blank line belongs to nobody.\n",
    );
    // The parse keeps it.
    let all = todo_plain(body);
    assert!(
        all.contains("scripts/bootstrap.sh (yason 0c84b29)."),
        "{all}"
    );
    assert!(all.contains("Deps: none."), "{all}");
    assert!(
        !all.contains("Prose after a blank"),
        "a blank line closes the item: {all}"
    );

    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-body-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::write(dir.join("TODO.md"), body).expect("write");
    let mut a = app();
    a.wiring.workspace = dir.display().to_string();
    a.key(Key::CtrlT);

    // Folded: the first line, and a mark that there is more. T2 has no body
    // and so carries no mark — `···` means "there is more", not "this is an
    // item".
    let screen = a.screen(110, 40).join("\n");
    assert!(
        screen.contains("T1 vendor the deps, pinned in ···"),
        "{screen}"
    );
    assert!(screen.contains("T2 no body at all"), "{screen}");
    assert!(!screen.contains("T2 no body at all ···"), "{screen}");
    assert!(
        !screen.contains("bootstrap.sh"),
        "folded hides it: {screen}"
    );

    // **The cursor starts on the `[+]` control**, which is the head of the one enumeration —
    // leticl's `todos-stops` — and NOT on the first item. It was on the first item while the
    // cursor's position came from an index into the repo's rows; the operator's two reports,
    // *"arrows dont go here"* and *"mouse doesnt click"*, were both that one defect, because
    // the pane drew the mark from one list and the keys acted on another. There is one list.
    let screen = a.screen(110, 40).join("\n");
    let on = |s: &str, what: &str| {
        s.lines()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains(what))
    };
    assert!(
        on(&screen, "[+] add todo item"),
        "starts on the control: {screen}"
    );
    // The model's rows and your own are not stops either, so the arrows never park on a row
    // where no key acts — the same reason the repo's headings are skipped.
    assert!(
        !screen.contains("▸ T1"),
        "the cursor is not on an item yet: {screen}"
    );

    // One Down is what reaches the first item, and Enter unfolds it.
    a.key(Key::Down);
    let screen = a.screen(110, 40).join("\n");
    assert!(
        on(&screen, "T1 vendor the deps"),
        "down reached T1: {screen}"
    );
    assert_eq!(a.key(Key::Enter), None);
    let screen = a.screen(110, 40).join("\n");
    assert!(
        on(&screen, "T1 vendor the deps"),
        "Enter does not move the cursor: {screen}"
    );
    assert!(
        screen.contains("scripts/bootstrap.sh (yason 0c84b29)."),
        "{screen}"
    );
    assert!(screen.contains("Deps: none."), "{screen}");
    assert!(
        !screen.contains("pinned in ···"),
        "unfolded drops the mark: {screen}"
    );

    // Enter again folds it; an arrow moves on and folds what it leaves.
    a.key(Key::Enter);
    assert!(!a.screen(110, 40).join("\n").contains("bootstrap.sh"));
    a.key(Key::Enter);
    a.key(Key::Down);
    let screen = a.screen(110, 40).join("\n");
    assert!(
        !screen.contains("bootstrap.sh"),
        "moving off an item folds it: {screen}"
    );
    assert!(
        on(&screen, "T2 no body at all"),
        "the arrow moved on to T2: {screen}"
    );

    // Down past the last item wraps — and the head of the list is the `[+]` control, because
    // that is where the enumeration starts. Up from the control wraps to the LAST item, so it
    // never lands on the `## Phase 0` heading, which has nothing to unfold.
    a.key(Key::Down);
    let screen = a.screen(110, 40).join("\n");
    assert!(
        on(&screen, "[+] add todo item"),
        "wrapped to the control: {screen}"
    );
    a.key(Key::Up);
    let screen = a.screen(110, 40).join("\n");
    assert!(
        on(&screen, "T2 no body at all"),
        "up from the control wraps to the last ITEM, not to the heading: {screen}"
    );
    assert_eq!(
        a.repo_sel, 2,
        "and the repo cursor follows, so the two cannot disagree"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A `!` line is the operator's own shell command, and nothing else stops being
/// what it was.**
///
/// The operator's ask: *"when prompt starts with ! it is going to be a shell command
/// from me"*. Pinned with the contrast cases beside it, because a recogniser is only
/// honest next to what it must NOT take: an ordinary line is still a prompt, and a
/// line with whitespace before the bang is prose too — the same rule `/`-verbs
/// follow, so the two sigils behave the same way and `  ! ls .` cannot become a
/// command by an indentation nobody can see.
#[test]
fn a_bang_line_is_a_shell_command_and_an_ordinary_line_is_still_a_prompt() {
    let mut a = app();
    typed(&mut a, "! ls .");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::OperatorShell {
            line: "! ls .".into()
        }),
        "the line travels verbatim, bang included"
    );
    assert_eq!(
        a.pending_prompts,
        vec!["! ls .".to_string()],
        "the echo is held like a prompt's, until the daemon's row lands"
    );
    // No space needed: `!ls` is the same command as `! ls`.
    typed(&mut a, "!ls");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::OperatorShell { line: "!ls".into() })
    );
    // An ordinary line is a prompt, unchanged.
    typed(&mut a, "what changed here?");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Prompt("what changed here?".into()))
    );
    // Whitespace before the bang is prose, exactly as it is before a `/` verb.
    typed(&mut a, "  ! ls .");
    assert_eq!(a.key(Key::Enter), Some(Action::Prompt("  ! ls .".into())));
}

/// **A bang with nothing after it is refused here, and the words are kept.**
///
/// `!` and `!   ` carry no command; sending one would make the daemon answer a
/// refusal for something this head could see was empty. The refusal is local and
/// the line goes back to the composer, which is the same courtesy a held prompt
/// gets — nothing is sent, so nothing can be shown as queued and then evaporate.
#[test]
fn a_bang_with_nothing_after_it_is_refused_and_the_words_are_kept() {
    let mut a = app();
    for line in ["!", "!   "] {
        typed(&mut a, line);
        assert_eq!(a.key(Key::Enter), None, "`{line}` must not be sent");
        assert_eq!(a.input(), line, "the line goes back to the composer");
        assert!(a.pending_prompts.is_empty(), "nothing was queued");
        a.key(Key::CtrlC);
    }
}

/// **The operator's command gets the tool-output treatment, the whole of it.**
///
/// A long output is *collapsed with an honest count* — `… +N lines` naming N as the
/// rows that are not shown — and the seam names the key that opens the row's own
/// window (`ctrl-v`), with `/t` the verb that unfolds every tool row. Opening it
/// pages through the payload without unfolding the conversation. And the operator's
/// own line is drawn as THEIR row — `speaker: Operator`, the block a prompt gets.
#[test]
fn a_bang_rows_output_is_collapsed_with_an_honest_count_and_opens_one_row() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut a, "! seq 1 60");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::OperatorShell {
            line: "! seq 1 60".into()
        })
    );
    let body: String = (0..60).map(|i| format!("line {i}\n")).collect();
    bang_rows(&mut a, 2, "i1", "! seq 1 60", &body);

    // The echo stood down: the daemon's `User` row IS the line, by construction.
    assert!(a.pending_prompts.is_empty(), "the echo retired on the row");

    let folded = a.screen(100, 40).join("\n");
    assert!(
        folded.contains("! seq 1 60"),
        "the operator's line is drawn: {folded:?}"
    );
    // The honest count: 60 lines of payload, the head of it shown, so 59 below —
    // the seam says exactly that, and `60 lines` on the header agrees with it.
    assert!(
        folded.contains("+59 lines"),
        "the count must state what is hidden, not round it away: {folded:?}"
    );
    assert!(
        folded.contains("ctrl-v opens it"),
        "the newest long row names the chord that opens its window: {folded:?}"
    );

    // The chord opens THAT row's window and does not unfold the conversation.
    a.key(Key::CtrlV);
    assert_eq!(a.payload_sel.as_deref(), Some("i1"));
    assert!(!a.tools.is_open(), "ctrl-v unfolded every row as well");
    let open = a.screen(100, 60).join("\n");
    assert!(
        open.contains("pages down"),
        "the window opened and says how to move in it: {open:?}"
    );
    // And `/t` is still the verb that unfolds every tool row — pinned beside the
    // chord so a seam that names one cannot lose the other.
    a.key(Key::Esc);
    assert_eq!(a.command("t"), None);
    assert!(a.tools.is_open(), "/t no longer unfolds tool output");
}

/// **§3.1, on this path: a control byte in the command's output never reaches the
/// frame as a control byte.**
///
/// The operator's own `ls` is still a program's bytes — `grep --color` emits SGR,
/// and a payload carrying `ESC[?1002h` turns the operator's mouse reporting off,
/// which is the exact report this sanitisation exists for. Sanitised at render, in
/// the store's own words *the record is what the tool wrote and must stay that* —
/// so the assertion is on the FRAME's bytes, colourless, where the head emits no
/// escapes of its own and any `ESC` is somebody else's.
#[test]
fn no_control_byte_from_a_bang_rows_output_reaches_the_frame() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // The store's own vocabulary of hostile bytes (`HOSTILE`, in the §3.1 suite):
    // SGR, a mode string, an OSC title, C1 controls, DEL.
    let hostile = " A\u{1b}[31mred\u{1b}[0m \u{1b}[8m(hidden) \u{1b}[2J \u{1b}[?1002h \u{1b}[?1006h \u{1b}[?1049h \u{1b}[?2004h \u{1b}[?2026h \u{1b}]0;pwned\u{7} \u{9b}31m \u{9c} \u{7f} end";
    bang_rows(
        &mut a,
        2,
        "i1",
        "! grep --color rn foo",
        &format!("src/a.rs{hostile}\nsrc/b.rs"),
    );
    for w in [60usize, 100, 160] {
        let rows = a.screen(w, 40);
        for (n, row) in rows.iter().enumerate() {
            assert!(
                !row.contains('\u{1b}'),
                "an ESC reached the frame at {w} cols, row {n}: {row:?}"
            );
            assert!(
                !row.chars()
                    .any(|c| ('\u{80}'..='\u{9f}').contains(&c) || c == '\u{7f}'),
                "a C1/DEL byte reached the frame at {w} cols, row {n}: {row:?}"
            );
        }
    }
    // And the fold still works over the sanitised text: the payload is long enough
    // to count, and the count is of lines the operator can read.
    let folded = a.screen(100, 40).join("\n");
    assert!(
        folded.contains("ctrl-v opens it") || folded.contains("+1 line"),
        "the row is still a folded tool row: {folded:?}"
    );
}

/// **The fold counts the TEXT, not the escape bytes.**
///
/// The seam's number is the whole reason a long row is readable at all — *"the count must
/// state what is hidden, not round it away"* — and a head that counted a payload line by its
/// bytes would report a different number for a coloured `ls` than for a plain one of the same
/// length. So the two are rendered and their seams compared: same payload, same count, with
/// and without the SGR.
#[test]
fn the_fold_counts_a_coloured_payload_exactly_as_it_counts_a_plain_one() {
    let plain: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let coloured: String = (0..60)
        .map(|i| format!("\u{1b}[01;34mline {i}\u{1b}[0m\n"))
        .collect();
    let mut seen = Vec::new();
    for body in [&plain, &coloured] {
        let mut a = App::new(RenderConfig {
            width: 100,
            color: true,
            ..RenderConfig::default()
        });
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        bang_rows(&mut a, 2, "i1", "! ls -la", body);
        let folded = a.screen(100, 40).join("\n");
        assert!(
            folded.contains("+59 lines"),
            "sixty payload lines, one shown, so 59 below: {folded:?}"
        );
        assert!(
            folded.contains("60 lines"),
            "and the header agrees with the seam: {folded:?}"
        );
        seen.push(folded);
    }
    // **The two counts are the same number**, which is the assertion: not that each
    // contains a string, but that the escape bytes changed nothing about it.
    assert_eq!(
        seen[0].matches("+59 lines").count(),
        seen[1].matches("+59 lines").count(),
        "a coloured payload folded differently from a plain one"
    );
}

/// **An act of the person at the keyboard is never hidden by verbosity.**
///
/// MEASURED on a live head, and this is the whole of the report: the operator typed `! ls`,
/// the store held the right two rows — their own `User` line and the `bash` result with
/// `origin: CallOrigin::Operator` — the turn started correctly, and **the operator never saw
/// the result on the screen**. Their own guess was the right one: *"i guess it was eaten by
/// verbosity level"*, and their verbosity is `read-edits`, whose rung is the bottom of the
/// ladder.
///
/// So both rows are drawn at every rung of [`Verbosity::ALL`], and at `read-edits` — the
/// profile the report came from, which is `conversation`'s set with `edits` turned up and so
/// the same rung — and the ruling is about the FILTER only: the result is still a folded
/// tool row, elided with an honest count, and the payload is not dumped whole. The rungs are
/// taken by name through the verb, so what is pinned is the ladder a reader has and not a set
/// built by hand.
#[test]
fn the_operators_own_act_is_drawn_at_every_rung() {
    // Long enough to fold: the row shows a line or two and counts the rest, so the last line
    // of the payload is the one that must NOT be on the screen.
    let body: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let mut sets: Vec<(String, Visibility)> = Verbosity::ALL
        .into_iter()
        .map(|r| {
            let vis = Visibility::of(Profile::parse(r.as_str()).expect("a rung is a profile"));
            assert_eq!(vis.rung(), r, "the premise: `{}` is that rung", r.as_str());
            (r.as_str().to_string(), vis)
        })
        .collect();
    // **And the profile the operator is actually on.** `read-edits` is not a rung — it is
    // `conversation`'s set with the `edits` switch turned up — so its rung is `conversation`'s,
    // which is why the two rows were eaten by a filter that was only ever about the model.
    let read_edits = Visibility::of(Profile::READ_EDITS);
    assert_eq!(read_edits.rung(), Verbosity::Conversation, "the premise");
    sets.push(("read-edits".to_string(), read_edits));

    for (name, vis) in sets {
        let mut a = app();
        a.visibility = vis;
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        typed(&mut a, "! seq 1 60");
        assert!(matches!(
            a.key(Key::Enter),
            Some(Action::OperatorShell { .. })
        ));
        bang_rows(&mut a, 2, "i1", "! seq 1 60", &body);

        let screen = a.screen(100, 40).join("\n");
        assert!(
            screen.contains("! seq 1 60"),
            "the operator's own line is hidden at `{name}`:\n{screen}"
        );
        assert!(
            screen.contains("+59 lines"),
            "the result of the operator's own command is hidden at `{name}`:\n{screen}"
        );
        assert!(
            !screen.contains("line 59"),
            "the payload was dumped whole at `{name}` — kept is not unfolded:\n{screen}"
        );
    }
}

/// **The three rows an answered question leaves on the log**, in the order the session writes
/// them — the call's result as the person's act, the question as the session's, and the answer
/// as the operator's own words.
///
/// Written from the shapes `Harness::settle_asked` produces, so the drawing these tests are
/// about is the drawing the daemon's own rows get.
fn asked_rows(a: &mut App, seq: u64, id: &str, payload: &str) {
    a.apply(ServerFrame::Event(env(
        seq,
        testing::appended(id, "tool_result"),
    )));
    a.record_item(
        id,
        TranscriptItem::ToolResult {
            call_id: format!("{id}c"),
            name: "ask_user_question".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: payload.into(),
            edit: None,
            origin: Some(letibot_transcript::CallOrigin::Operator { who: "dead".into() }),
            media: None,
        },
    );
    for (n, speaker, text) in [
        (
            1u64,
            letibot_transcript::Speaker::Agent,
            "asked: which database should the migration target?\n\
             options offered: postgres | sqlite",
        ),
        (
            2u64,
            letibot_transcript::Speaker::Operator,
            "sqlite\nonly for the CUDA box",
        ),
    ] {
        let item_id = format!("{id}u{n}");
        a.apply(ServerFrame::Event(env(
            seq + n,
            testing::appended(&item_id, "user"),
        )));
        a.record_item(
            &item_id,
            TranscriptItem::User {
                speaker,
                parts: vec![UserPart::Text {
                    text: text.to_string(),
                }],
            },
        );
    }
}

/// **An answered question leaves something on the screen, at every rung.**
///
/// The operator's report, verbatim: *"by the way i dont see my answer to the selector"*. The
/// selector is a live overlay — `DecisionRequested` opens it and `DecisionAnswered` closes it —
/// so once the card goes, the only thing that can hold the exchange is the transcript. The
/// answer used to be a tool payload and nothing else, and at `read-edits` — the operator's own
/// profile, whose rung is the bottom of the ladder — the tool rows are not drawn at all.
///
/// So all three rows are drawn at every rung: the question as a `User` row of the session's (a
/// `User` row is the conversation and is kept everywhere), the answer as the operator's own,
/// and the result because `origin: CallOrigin::Operator` is the mark that says a person acted —
/// which [`the_operators_own_act_is_drawn_at_every_rung`] already holds for the `!` line, and
/// which this pins for the other way a person acts.
#[test]
fn an_answered_question_is_on_the_screen_at_every_rung() {
    // Long enough to fold: the row shows a line or two and counts the rest, so the last line of
    // the payload is the one that must NOT be on the screen.
    let body: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let mut sets: Vec<(String, Visibility)> = Verbosity::ALL
        .into_iter()
        .map(|r| {
            let vis = Visibility::of(Profile::parse(r.as_str()).expect("a rung is a profile"));
            assert_eq!(vis.rung(), r, "the premise: `{}` is that rung", r.as_str());
            (r.as_str().to_string(), vis)
        })
        .collect();
    // **And the profile the operator is actually on**, which is not a rung: `read-edits` is
    // `conversation`'s set with the `edits` switch turned up, so its rung is the bottom one.
    let read_edits = Visibility::of(Profile::READ_EDITS);
    assert_eq!(read_edits.rung(), Verbosity::Conversation, "the premise");
    sets.push(("read-edits".to_string(), read_edits));

    for (name, vis) in sets {
        let mut a = app();
        a.visibility = vis;
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        asked_rows(&mut a, 2, "q1", &body);

        let screen = a.screen(100, 40).join("\n");
        assert!(
            screen.contains("which database should the migration target?"),
            "the question is hidden at `{name}`:\n{screen}"
        );
        assert!(
            screen.contains("sqlite") && screen.contains("only for the CUDA box"),
            "the operator's answer is hidden at `{name}`:\n{screen}"
        );
        assert!(
            screen.contains("+59 lines"),
            "the result of the person's answer is hidden at `{name}`:\n{screen}"
        );
        assert!(
            !screen.contains("line 59"),
            "the payload was dumped whole at `{name}` — kept is not unfolded:\n{screen}"
        );
    }
}

/// **The control that makes the ruling mean *who acted* and not *tool rows are drawn*.**
///
/// A `ToolResult` the MODEL proposed — `origin: None`, which is also every row written before
/// the field existed — is the head's working and follows the ladder exactly as it always did:
/// hidden at the bottom rung, where the screen carries the run marker and the count instead,
/// and drawn (folded) above it. Beside the test above, this pair is what says the clause reads
/// the operator's `origin` rather than widening the filter for tool rows in general.
#[test]
fn a_models_tool_row_still_obeys_the_rung() {
    let body: String = (0..60).map(|i| format!("line {i}\n")).collect();
    for rung in Verbosity::ALL {
        let mut a = app();
        assert_eq!(a.command(&format!("verbosity {}", rung.as_str())), None);
        assert_eq!(a.visibility.rung(), rung, "the premise: that is this rung");
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        model_bash_rows(&mut a, 2, "i1", &body);

        let screen = a.screen(100, 40).join("\n");
        if rung.hides_the_working() {
            assert!(
                !screen.contains("+59 lines"),
                "a model's tool row was drawn at `{}`:\n{screen}",
                rung.as_str()
            );
            assert!(
                screen.contains("[1 tool call]"),
                "the row the rung hides is not even counted at `{}`:\n{screen}",
                rung.as_str()
            );
        } else {
            assert!(
                screen.contains("+59 lines"),
                "a model's tool row is not drawn at `{}`:\n{screen}",
                rung.as_str()
            );
            assert!(
                !screen.contains("line 59"),
                "and it is not folded either at `{}`:\n{screen}",
                rung.as_str()
            );
        }
    }
}

/// **Two prompts typed behind a running CALL are ONE echo** (R51 item 14, the preamble's second
/// gate).
///
/// The daemon merges everything typed during a round into one held user item, so the head must
/// join them the same way. This gate read the state name, so during a tool call it did not join
/// — and the operator got two `queued` rows for one message, where the engine produced one row.
#[test]
fn two_prompts_behind_a_running_call_are_one_echo() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::proposed_on("t1", "c1", "bash", "\"cargo test\""),
    )));
    a.apply(ServerFrame::Event(env(3, testing::turn_finished("t1"))));
    assert!(a.submit("first thing".into()).is_some());
    assert!(a.submit("second thing".into()).is_some());
    assert_eq!(
        a.pending_prompts,
        vec!["first thing\nsecond thing".to_string()],
        "both lines joined into the one message the engine will read"
    );
}

/// **A row's payload is reachable past its first screenful.**
///
/// The operator's framing: a tool result is a **logical** string that wraps to
/// thousands of display lines, of which the window shows a few dozen. The fold drew
/// the head and reported the rest as `… +N lines · ctrl-t`, and `ctrl-t` revealed
/// none of them — it raised the *budget* (which rows may be long), never the
/// *offset*. So the rest of a 418 KB payload could not be read at all.
///
/// This asserts what makes it reachable: the view opens on `ctrl-t`, the arrows move
/// the window, the end is reachable, and the seam says which key does what.
///
/// A **tall** screen on purpose: the card draws up to the body budget when it is
/// open, so on a 30-row terminal only the block's own tail is on screen and "did the
/// head move" is unanswerable. Sixty rows fits the whole block and the chrome under
/// it, which is what makes the assertions below about the *payload* rather than about
/// which part of it the terminal happened to show.
#[test]
fn a_long_payload_can_be_paged_to_its_end() {
    let mut a = app();
    // A payload of 200 numbered lines, so "which part is on screen" is decidable.
    let body: String = (0..200).map(|i| format!("line {i}\n")).collect();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a_result_row(&mut a, 2, "i1", &body);

    // Folded: the head, a count, and the chord that is supposed to reveal the rest.
    // **The newest long row names the chord; this is that row.** R10 moved the
    // conversation-wide unfold to `/t`, so the seam says `opens it` — the window,
    // not the whole conversation.
    let folded = a.screen(100, 60).join("\n");
    assert!(folded.contains("line 0"), "{folded:?}");
    assert!(folded.contains("ctrl-v opens it"), "{folded:?}");

    // Ctrl-T opens a view on the newest payload row. **It does not touch the fold**
    // any more: one chord, one meaning, and this one is the per-row window the seam
    // above just named.
    a.key(Key::CtrlV);
    assert!(a.payload_sel.is_some(), "ctrl-t opened no view");
    let head = a.screen(100, 60).join("\n");
    assert!(
        head.contains("line 0"),
        "the head is what is drawn: {head:?}"
    );
    assert!(head.contains("pages down"), "{head:?}");

    // Down pages: the window moves off the head, and the seam above says so.
    a.key(Key::Down);
    let paged = a.screen(100, 60).join("\n");
    assert!(
        !paged.contains("line 0\n"),
        "the window did not move: {paged:?}"
    );
    assert!(
        paged.contains("more lines above"),
        "the seam above says there is more: {paged:?}"
    );

    // Up goes back, and enough Down reaches the end rather than running off it.
    a.key(Key::Up);
    let back = a.screen(100, 60).join("\n");
    assert!(back.contains("line 0"), "up did not come back: {back:?}");
    for _ in 0..60 {
        a.key(Key::Down);
    }
    let end = a.screen(100, 60).join("\n");
    assert!(
        end.contains("end of output"),
        "the end must be reachable and said so: {end:?}"
    );

    // And Esc closes the view. **The fold is not touched either way**: R10 took the
    // conversation-wide unfold off this chord and gave it to `/t`, so a window that
    // opened and closed leaves every other row exactly as it was.
    a.key(Key::Esc);
    assert_eq!(a.payload_sel, None, "esc did not close the view");
    assert!(
        !a.tools.is_open(),
        "a payload window must not unfold the whole conversation"
    );
}

/// **Every chord in the table is reachable from a press, and a press IS a `/verbosity`.**
///
/// `Show::chord` is the one entry that advertises a key AND binds it, and this is what keeps it
/// that way: `Key::show` is the lookup the dispatch uses, so a chord added to the table that no
/// key press reaches — or a key bound by hand with nothing in the table — fails here. The two
/// hand-written arms this replaces are the reason: each named its key twice, and one of them
/// moved the MIRROR the drawing reads (`self.reasoning`) instead of the set, so the picture
/// changed while the status row, `head.toml`, `keeps` and `rung()` went on saying the old set.
#[test]
fn a_chords_key_is_found_from_the_table_and_a_press_moves_the_set() {
    for s in Show::ALL {
        let Some((_, key)) = s.chord() else {
            continue;
        };
        assert_eq!(key.show(), Some(s), "{} is advertised as {key:?}", s.name());
    }
    let mut a = app();
    assert_eq!(
        a.visibility.level(Show::Thinking),
        Level::Folded,
        "normal starts with the thinking folded"
    );
    a.key(Key::CtrlR);
    assert_eq!(
        a.visibility.level(Show::Thinking),
        Level::Open,
        "the SET moved"
    );
    assert_eq!(
        a.reasoning,
        Fold::Open,
        "and the mirror the drawing reads moved with it — the defect was these two disagreeing"
    );
    assert!(
        a.visibility.as_str().contains("thinking=open"),
        "the set reads as what it is, so it can be typed back: {}",
        a.visibility.as_str()
    );
    a.key(Key::CtrlR);
    assert_eq!(
        a.visibility.level(Show::Thinking),
        Level::Folded,
        "and back"
    );
    assert_eq!(a.reasoning, Fold::Folded);
}

/// **The card names the set in force when no profile is it.**
///
/// The fixture is `loud` with the edit cards off, and it is chosen because the assertion needs a
/// set that is no profile: every switch is on except `edits`, which no row of `Profile::ALL`
/// has. The row the card must grow for it is the set's own `custom …` name — which is also what
/// `pick_current` reads, so it is the row the marker and the cursor land on rather than
/// matching nothing.
#[test]
fn the_card_names_the_set_in_force_a_reader_typed() {
    let mut a = app();
    a.pick = Some(Pick::Verbosity);

    // A profile: the five, and no extra row.
    a.visibility = Visibility::of(Profile::CONVERSATION);
    let rows = a.pick_values();
    assert_eq!(rows.len(), Profile::ALL.len(), "{rows:?}");
    assert!(
        !rows.iter().any(|(v, _)| v.starts_with("custom")),
        "a profile needs no custom row: {rows:?}"
    );

    // **A set no profile is** — `loud` with the edit cards off.
    a.visibility = Visibility::of(Profile::LOUD).with(Show::Edits, Level::Hidden);
    let mine = a.visibility.as_str();
    assert!(mine.starts_with("custom"), "the set in force: {mine}");
    let rows = a.pick_values();
    assert_eq!(rows.len(), Profile::ALL.len() + 1, "{rows:?}");
    assert!(
        rows.iter().any(|(v, _)| v == &mine),
        "the card must name the set in force: {rows:?}"
    );
    // **And it is the row the marker and the cursor take**, because the row's name IS what
    // `pick_current` reads — so the cursor starts on the state the reader is in rather than on
    // the head's start.
    assert_eq!(a.pick_current(), mine);
    a.seed_pick();
    assert_eq!(
        a.mode_sel,
        rows.len() - 1,
        "the cursor did not land on the set in force: {rows:?}"
    );
}

#[test]
fn thinking_is_folded_by_default_and_the_fold_says_which_key_opens_it() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    for _ in 0..40 {
        a.apply(ServerFrame::Event(env(
            2,
            testing::reasoning("t1", "a line of working out\n"),
        )));
    }
    let folded = a.screen(80, 24).join("\n");
    assert!(folded.contains("ctrl-r"), "the affordance is on the screen");
    assert!(
        folded.matches("a line of working out").count() <= 1,
        "folded thinking shows the live line and no more:\n{folded}"
    );
    a.key(Key::CtrlR);
    let open = a.screen(80, 24).join("\n");
    assert!(
        open.matches("a line of working out").count() > 1,
        "ctrl-r opened nothing:\n{open}"
    );
}

#[test]
fn the_log_alone_is_enough_to_render_the_conversation() {
    // T13.1's other half: *"the log should be a sufficient record of a
    // session"*. It was not. `letibot-tui --replay` reads exactly these
    // envelopes and nothing else, and before the body travelled on the log it
    // showed a placeholder for every row — the transcript existed only inside a
    // snapshot nobody had written down.
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(testing::appended("s.0", "user"));
    hub.record_item(
        "s.0",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "why did the cache miss".into(),
            }],
        },
    );
    hub.publish(testing::appended("t1.0", "assistant"));
    hub.record_item(
        "t1.0",
        TranscriptItem::Assistant {
            text: "because reasoning_content was replayed into the wrong field".into(),
            tool_calls: vec![],
            truncated: false,
        },
    );
    hub.publish(testing::turn_finished("t1"));

    // Round-trip through the wire form, which is what `--replay` reads.
    let jsonl: Vec<String> = hub
        .retained()
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    let mut a = app();
    for line in &jsonl {
        let env: letibot_sessionlog::event::Envelope = serde_json::from_str(line).unwrap();
        a.apply(ServerFrame::Event(env));
    }
    let screen = a.screen(100, 30).join("\n");
    assert!(screen.contains("why did the cache miss"), "{screen}");
    assert!(screen.contains("wrong field"), "{screen}");
    assert!(!screen.contains("waiting for the body"), "{screen}");
}

#[test]
fn a_tool_call_the_head_never_saw_proposed_still_names_its_target() {
    // The reattach case, which is the common one: a head that joins after a
    // turn has no proposal event to have learned the target from, and the
    // arguments on the settled row are the only place it survives. It uses the
    // *same* function the wire does, so both renderings agree.
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("t1.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "t1.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"crates/ui/src/style.rs"}"#.into(),
                }],
                truncated: false,
            }),
        },
    )));
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("→ Read crates/ui/src/style.rs"), "{screen}");
    assert!(
        !screen.contains("(c1)"),
        "the id is not shown when a name is: {screen}"
    );
}

/// Two rounds of one turn, both numbering their calls from `call_0`, which is
/// what `letibot_turn::items` does whenever the wire format carries no id.
///
/// Taken from the operator's own session (`s-1788987496351498881`, fourteen
/// rounds of `call_0`/`call_1`/`call_2`) and reduced to the two rows that make
/// the defect: the head kept one session-wide table keyed on the call id, so
/// the later round's paths overwrote the earlier round's and every settled card
/// read back the survivor. On screen that was `▸ Read TODO.md · ok · 143 lines`
/// above a body beginning `# rano` — the payload of `README.md`.
///
/// It is a correctness test, not a layout one. A card that names a file the
/// tool did not open is the head telling the operator something false about
/// what a tool returned.
#[test]
fn a_second_round_of_calls_does_not_relabel_the_first_rounds_results() {
    fn call(id: &str, name: &str, arguments: &str) -> letibot_transcript::ToolCall {
        letibot_transcript::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }
    fn assistant(a: &mut App, seq: u64, id: &str, calls: Vec<letibot_transcript::ToolCall>) {
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(id, "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            seq + 1,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(TranscriptItem::Assistant {
                    text: String::new(),
                    tool_calls: calls,
                    truncated: false,
                }),
            },
        )));
    }
    fn result(a: &mut App, seq: u64, id: &str, call_id: &str, name: &str, payload: &str) {
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            seq + 1,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: call_id.into(),
                    name: name.into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: payload.into(),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }

    let mut a = app();
    assistant(
        &mut a,
        1,
        "r1.a",
        vec![call("call_0", "read", r#"{"path":"README.md"}"#)],
    );
    // Two lines each, so the payload is a body under a header rather than
    // inlined onto it — the pairing is what is under test, and it is only
    // visible when the two are separate rows.
    result(
        &mut a,
        3,
        "r1.t",
        "call_0",
        "read",
        "FIRST-ROUND-PAYLOAD\nmore\n",
    );
    assistant(
        &mut a,
        5,
        "r2.a",
        vec![call("call_0", "read", r#"{"path":"TODO.md"}"#)],
    );
    result(
        &mut a,
        7,
        "r2.t",
        "call_0",
        "read",
        "SECOND-ROUND-PAYLOAD\nmore\n",
    );

    // A tall enough screen that both rounds are on it at once, which is the
    // only way the pairing is visible at all.
    let lines = a.screen(120, 60);
    // The card a payload is sitting under: the nearest header above it.
    let label = |needle: &str| -> String {
        let i = lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is not on the screen:\n{}", lines.join("\n")));
        lines[..i]
            .iter()
            .rev()
            .find(|l| l.contains('▸') || l.contains('▾'))
            .cloned()
            .unwrap_or_default()
    };
    let screen = lines.join("\n");
    assert!(
        label("FIRST-ROUND-PAYLOAD").contains("README.md"),
        "round one's payload is under `{}`:\n{screen}",
        label("FIRST-ROUND-PAYLOAD")
    );
    assert!(
        label("SECOND-ROUND-PAYLOAD").contains("TODO.md"),
        "round two's payload is under `{}`:\n{screen}",
        label("SECOND-ROUND-PAYLOAD")
    );
}

/// The same claim, with a row above the round so the narrowed invalidation
/// cannot fall back on "rewind to zero".
///
/// `a_settled_call_is_one_row_…` starts at the assistant row, so its round
/// head is index 0 and every invalidation in it is a full rebuild — which is
/// exactly the path that was never narrowed. Put a user row in front and the
/// rewind is a real one. Found by diffing the byte stream of a replay against
/// the same replay with the narrowing switched off: 59 frames of 1028 showed
/// a different screen, and the first of them had `→ Listed * · no result`
/// sitting above `▸ Listed * · ok`.
#[test]
fn a_settled_call_is_one_row_when_the_round_does_not_start_at_row_zero() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::appended("u", "user"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "u".into(),
            item: Box::new(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "what is in the tree".into(),
                }],
            }),
        },
    )));
    // A pane, because `drawn_live` and `superseded` are inputs to the walk and
    // a test without a turn exercises neither. The daemon's order, from
    // `docs/tui-testing.md`: the round's own rows, then the terminal event,
    // then the result rows.
    a.apply(ServerFrame::Event(env(3, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        4,
        testing::appended("r.a", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::TranscriptContent {
            item_id: "r.a".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "call_0".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"TODO.md"}"#.into(),
                }],
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(6, testing::turn_finished("t1"))));
    // The walk has to have RENDERED the round before the result lands, or the
    // rewind has nothing to rewind and the bug hides.
    let before = a.screen(120, 40).join("\n");
    assert!(before.contains("no result"), "{before}");

    a.apply(ServerFrame::Event(env(
        7,
        testing::appended("r.t", "tool_result"),
    )));
    let _ = a.screen(120, 40);
    a.apply(ServerFrame::Event(env(
        8,
        SessionEvent::TranscriptContent {
            item_id: "r.t".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "call_0".into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "# rano TODO\n".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    let after = a.screen(120, 40).join("\n");
    assert_eq!(
        after.matches("TODO.md").count(),
        1,
        "the proposal does not stay beside its own answer:\n{after}"
    );
    assert!(
        !after.contains("no result"),
        "a call that returned does not still read as one that did not:\n{after}"
    );
}

/// A message the operator sends while the calls run lands BETWEEN the calls
/// and their results. It is not the end of the round.
///
/// Measured in the store 2026-09-17: `assistant (5 calls), user, user,
/// tool_result ×10`. The results after the user rows still answer the calls
/// above them — a user cannot produce a tool result — and stopping at the
/// user row left every call `→ no result` after the model had moved on.
#[test]
fn a_message_sent_mid_round_does_not_orphan_the_rounds_results() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("r.a", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "r.a".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "call_0".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"TODO.md"}"#.into(),
                }],
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(3, testing::appended("u", "user"))));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::TranscriptContent {
            item_id: "u".into(),
            item: Box::new(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "continue".into(),
                }],
            }),
        },
    )));
    let _ = a.screen(120, 40);
    a.apply(ServerFrame::Event(env(
        5,
        testing::appended("r.t", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        6,
        SessionEvent::TranscriptContent {
            item_id: "r.t".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "call_0".into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "# rano TODO\n".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    let after = a.screen(120, 40).join("\n");
    assert!(
        !after.contains("no result"),
        "the result after the operator's message still answers the call:\n{after}"
    );
    assert_eq!(after.matches("TODO.md").count(), 1, "{after}");
}

/// One turn, with all four of its levels on the screen at once.
///
/// The operator's report was that a turn has no shape: *"user message, then a
/// flat wall of cards"*. What answers it is a step, not a glyph — the
/// question and the answer at the body's own column, the working one step in
/// under them.
#[test]
fn a_turn_is_a_question_a_step_of_working_and_an_answer_back_at_the_margin() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::appended("u", "user"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "u".into(),
            item: Box::new(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "what crates are in this workspace".into(),
                }],
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        3,
        testing::appended("t", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::TranscriptContent {
            item_id: "t".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "call_0".into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "one\ntwo\nthree\n".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        5,
        testing::appended("s", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        6,
        SessionEvent::TranscriptContent {
            item_id: "s".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "There are twelve.".into(),
                tool_calls: vec![],
                truncated: false,
            }),
        },
    )));
    let screen = a.screen(120, 40);
    let at = |needle: &str| -> usize {
        let l = screen
            .iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing:\n{}", screen.join("\n")));
        l.len() - l.trim_start().len()
    };
    let question = at("what crates are in this workspace");
    let working = at("Read (call_0)");
    let answer = at("There are twelve.");
    assert_eq!(
        question, answer,
        "the question and the answer share a column"
    );
    assert_eq!(
        working,
        question + card::REASONING_RAIL_WIDTH,
        "and the working is one step in under them:\n{}",
        screen.join("\n")
    );
}

#[test]
fn a_prompt_typed_mid_turn_stays_on_screen_marked_queued() {
    // The complaint this whole queue answers: enter pressed while a turn runs,
    // the hub accepts the prompt as a follow-up user item, and until the step
    // boundary appends it the words were on no part of the screen. Now they
    // wait at the tail of the body, marked, where their row will land.
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    for c in "also bump the retry budget".chars() {
        a.key(Key::Char(c));
    }
    assert!(matches!(a.key(Key::Enter), Some(Action::Prompt(_))));
    assert_eq!(a.input(), "", "the composer handed the words off");
    let screen = a.screen(80, 24).join("\n");
    assert!(
        screen.contains("queued · also bump the retry budget"),
        "{screen}"
    );
}

#[test]
fn the_queued_echo_stands_down_when_the_row_lands() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    for c in "also bump the retry budget".chars() {
        a.key(Key::Char(c));
    }
    a.key(Key::Enter);
    assert!(a.screen(80, 24).join("\n").contains("queued ·"));
    // The step boundary appends the follow-up user item: the transcript has
    // taken the words over, so the dim echo must go — one row retires one
    // entry, and the settled row is what remains.
    a.apply(ServerFrame::Event(env(2, testing::appended("u2", "user"))));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "u2".into(),
            item: Box::new(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "also bump the retry budget".into(),
                }],
            }),
        },
    )));
    let screen = a.screen(80, 24).join("\n");
    assert!(!screen.contains("queued ·"), "{screen}");
    assert!(
        screen.contains("also bump the retry budget"),
        "the words left with the echo: {screen}"
    );
}

#[test]
fn switching_sessions_leaves_the_queue_behind_and_a_resync_keeps_what_is_still_queued() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    for c in "also bump the retry budget".chars() {
        a.key(Key::Char(c));
    }
    a.key(Key::Enter);
    // A switch is a replacement: the queue belongs to the session it was typed
    // at, and the hub drains it into *that* transcript.
    a.apply(hello(
        "s2",
        vec![brief("s2", "other", false)],
        Hub::new("s2").snapshot(),
    ));
    assert!(
        !a.screen(80, 24).join("\n").contains("queued ·"),
        "an echo of another session's queue"
    );
    // Back to the first session. A resync of the *same* session keeps what is
    // still queued — the hub's queue survived — but a transcript that already
    // holds the words stands the echo down, the way the live row would have.
    let hub = Hub::new("s");
    hub.publish(testing::appended("u1", "user"));
    hub.record_item(
        "u1",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "also bump the retry budget".into(),
            }],
        },
    );
    a.apply(hello("s", vec![brief("s", "", true)], hub.snapshot()));
    assert!(
        !a.screen(80, 24).join("\n").contains("queued ·"),
        "the snapshot already holds the words"
    );
}

#[test]
fn a_row_with_no_timestamp_shows_none_rather_than_the_epoch() {
    // A log recorded before `SnapshotItem::ts` existed replays with zeros, and
    // `01:00:00` would be a measurement nobody took rendered as one they did.
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::appended("s.0", "user"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::content("s.0", "no clock here"),
    )));
    let screen = a.screen(80, 16);
    let row = screen.iter().find(|l| l.contains("no clock here")).unwrap();
    assert!(!row.contains(':'), "a fabricated timestamp: {row:?}");
}

/// **Rendering the same state twice must produce the same frame** — the invariant that
/// catches a render path that mutates the state it is drawing.
///
/// The operator's report: *"empty space increases"*. A frame is a pure function of the head's
/// state on a given tick; if drawing twice with no event in between differs, the draw is
/// writing back into what it reads, and every later frame inherits it.
#[test]
fn two_renders_of_one_state_are_the_same_frame() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // **The operator's own rung**, which is what makes the marker live at all: the counts are
    // drawn only where the working is hidden.
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // Prose committed as a row, so the marker has a sentence to continue, and then a call in
    // flight — the shape the counts exist for.
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("s.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "Now the last two.".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        testing::proposed_on("t1", "c1", "bash", "\"cargo test\""),
    )));
    let first = a.screen(120, 30);
    let second = a.screen(120, 30);
    let third = a.screen(120, 30);
    // What the operator sees: the marker's text, counted. Three renders of one state must not
    // give three markers on the line.
    let count = |frame: &[String]| markers(&frame.join("\n"));
    assert_eq!(
        count(&first),
        count(&second),
        "the marker multiplied between two renders of one state"
    );
    assert_eq!(first, second, "the second render differs from the first");
    assert_eq!(second, third, "the third differs from the second");
}

/// **R38: a setting with more than one value is CHOSEN from a card, never cycled.**
///
/// The operator's own shape: `/verbosity` bare used to walk the rung, so the reader learnt
/// the list by changing it and discovered the current value the same way. With R37's fourth
/// rung that was up to three presses and three repaints to learn four words that fit on one
/// card. The card states every value **and what it means** — `conversation`, `terse`,
/// `normal`, `loud` are names, and a reader choosing between them is choosing between *what
/// will be on my screen*, which the name does not say.

/// **R37 AMENDED: a run of hidden rows is ONE line, and it is the sentence's
/// continuation.**
///
/// The operator, reading a real screen where the model's prose ended in a colon:
/// *"if toolcalls and thinking are just hidden completely the narrative breaks."*
/// The colon pointed at work that was not there. So a contiguous run of hidden rows
/// collapses to one line carrying **how much** (`N tool calls`, `N thinking lines`) and
/// **what it touched** (the verbs and targets the head already computed), drawn at the
/// prose's own column with no blank in front of it.
#[test]
fn a_run_of_hidden_rows_draws_one_line_that_continues_the_sentence() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // **The operator's own shape**: an answer that ENDS IN A COLON, then the work. An
    // assistant row is the conversation, so it is kept — and the marker has to read as
    // the continuation of the sentence it points at.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    // **The row carries the calls as well as the prose**, which is the shape the daemon
    // writes and the only place a row's *target* survives: `targets_before` reads the
    // nearest assistant row's `tool_calls` arguments, so a fixture that published only
    // proposals would leave every target empty — and this test would then be asserting
    // that a marker with no target is a marker with the right target.
    let calls: Vec<(&str, &str, &str)> = vec![
        ("c0", "read", "crates/tui/src/app.rs"),
        ("c1", "grep", "ctrl-t"),
        ("c2", "edit", "crates/tui/src/chrome.lisp"),
        ("c3", "read", "crates/tui/src/term.rs"),
    ];
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "here is what I am about to do:".into(),
                tool_calls: calls
                    .iter()
                    .map(|(id, name, arg)| letibot_transcript::ToolCall {
                        id: (*id).into(),
                        name: (*name).into(),
                        arguments: format!("{{\"path\": \"{arg}\"}}"),
                    })
                    .collect(),
                truncated: false,
            }),
        },
    )));
    // One reasoning block, then four tool results — all hidden, all contiguous.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptAppended {
            item_id: "s.1".into(),
            kind: "reasoning".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::Reasoning {
                text: "let me check the file\nand then the other one\nand then the tests".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            }),
        },
    )));
    for (i, (call, name, _arg)) in calls.iter().enumerate() {
        let id = format!("s.{}", i + 2);
        a.apply(ServerFrame::Event(env(
            (i as u64) * 2 + 5,
            testing::appended(&id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            (i as u64) * 2 + 6,
            SessionEvent::TranscriptContent {
                item_id: id.clone(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: (*call).into(),
                    name: (*name).into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!("CONTENTS-{i}"),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    // Wide enough for the whole marker: at 110 columns it is trimmed, which is its own
    // rule (one line, always) and not a missing fact — but then the assertion below would
    // be about the trim rather than about what the marker says.
    let quiet = a.screen(200, 40).join("\n");

    // **One line for the whole run**, carrying both counts and the verbs.
    assert_eq!(
        quiet.matches("[4 tool calls").count(),
        1,
        "the run must draw exactly one marker:\n{quiet}"
    );
    assert!(quiet.contains("3 thinking lines"), "{quiet}");
    // **The counts and nothing else** — the operator's final shape. The verbs and the
    // distinct targets were built, measured and superseded: with prose on both sides of
    // the marker, the counts are the only fact the two neighbours do not already carry.
    // Asserted as the *absence* of the superseded half, so it cannot creep back in.
    for gone in ["Read", "Edited", "Searched", "Ran"] {
        assert!(
            !quiet.contains(gone),
            "`{gone}` is the superseded summary half and must not be drawn: {quiet}"
        );
    }
    // The rows themselves are still gone — the point of the rung is that the reader does
    // not read them.
    for hidden in ["CONTENTS-0", "CONTENTS-3", "let me check the file"] {
        assert!(
            !quiet.contains(hidden),
            "`{hidden}` is the working: {quiet}"
        );
    }

    // **It is the continuation of the sentence, not a row underneath it.** The prose ends
    // in a colon and the marker is the very next line.
    // **It is punctuation inside the sentence, not a row underneath it.** The marker is
    // on the prose's OWN line, after the colon — which is the operator's exemplar:
    // `…has to give: [11 tool calls, 246 thinking lines]`.
    let line = quiet
        .lines()
        .find(|l| l.contains("here is what I am about to do:"))
        .expect("the prose is on the screen");
    assert!(
        line.contains("[4 tool calls, 3 thinking lines]"),
        "the marker is not on the line the colon points at: {line:?}"
    );
    assert!(
        line.trim_end()
            .ends_with("[4 tool calls, 3 thinking lines]"),
        "the counts come last on the sentence they continue: {line:?}"
    );
}

/// **A tool-heavy turn draws EXACTLY ONE marker, whatever rows the daemon
/// interleaved** — R37 AMENDED, and this is the form to write rather than the case.
///
/// The operator ran the rung and got **eight markers in a row with no prose between
/// any of them**. Every one of their CONTENTS was right; what was wrong was how many
/// there were. So this asserts the **number**, not the content — a test that checked
/// the wording would have passed on all eight — and it asserts it over the shapes the
/// daemon really writes between one call and the next:
///
/// * an assistant row with **no text** carrying the round's calls (the shape of a
///   tool-calling round, and invisible at this rung);
/// * an assistant row whose text is **whitespace only**;
/// * a **tool_result** row;
/// * a row **announced with no body** — *"a zero-length text row"*, and invisible
///   by design.
///
/// Each of those ended a run under the old boundary, and the count is what says so.
#[test]
fn a_tool_heavy_turn_draws_one_marker_however_the_rows_are_interleaved() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let mut seq = 1u64;
    let mut append = |a: &mut App, id: &str, kind: &str, item: Option<TranscriptItem>| {
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptAppended {
                item_id: id.into(),
                kind: kind.into(),
                ledger_head: String::new(),
            },
        )));
        seq += 1;
        if let Some(item) = item {
            a.apply(ServerFrame::Event(env(
                seq,
                SessionEvent::TranscriptContent {
                    item_id: id.into(),
                    item: Box::new(item),
                },
            )));
            seq += 1;
        }
    };

    // The prose that introduces the work — the sentence with the colon.
    append(
        &mut a,
        "s.0",
        "assistant",
        Some(TranscriptItem::Assistant {
            text: "let me check all of it:".into(),
            tool_calls: Vec::new(),
            truncated: false,
        }),
    );
    // Round one: an assistant row with NO text, its three calls, and their results.
    let calls = |from: usize, n: usize| -> Vec<letibot_transcript::ToolCall> {
        (from..from + n)
            .map(|i| letibot_transcript::ToolCall {
                id: format!("c{i}"),
                name: "bash".into(),
                arguments: format!("{{\"command\": \"step {i}\"}}"),
            })
            .collect()
    };
    append(
        &mut a,
        "s.1",
        "assistant",
        Some(TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: calls(0, 3),
            truncated: false,
        }),
    );
    for i in 0..3 {
        let id = format!("s.{}", 2 + i);
        append(&mut a, &id, "tool_result", None);
        append(
            &mut a,
            &id,
            "tool_result",
            Some(TranscriptItem::ToolResult {
                call_id: format!("c{i}"),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: format!("output {i}"),
                edit: None,
                origin: None,
                media: None,
            }),
        );
    }
    // A reasoning block, and a second round announced with NO body at all.
    append(
        &mut a,
        "s.20",
        "reasoning",
        Some(TranscriptItem::Reasoning {
            text: "three down, more to go".into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        }),
    );
    append(&mut a, "s.21", "assistant", None);
    append(
        &mut a,
        "s.21",
        "assistant",
        Some(TranscriptItem::Assistant {
            text: "   \n  ".into(),
            tool_calls: calls(3, 4),
            truncated: false,
        }),
    );
    for i in 3..7 {
        let id = format!("s.{}", 22 + i);
        append(&mut a, &id, "tool_result", None);
        append(
            &mut a,
            &id,
            "tool_result",
            Some(TranscriptItem::ToolResult {
                call_id: format!("c{i}"),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: format!("output {i}"),
                edit: None,
                origin: None,
                media: None,
            }),
        );
    }
    // And the prose that reports it, which is what ENDS the run.
    append(
        &mut a,
        "s.40",
        "assistant",
        Some(TranscriptItem::Assistant {
            text: "and that is what all of it says.".into(),
            tool_calls: Vec::new(),
            truncated: false,
        }),
    );
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let screen = a.screen(110, 200).join("\n");

    assert_eq!(
        markers(&screen),
        1,
        "seven calls and two rounds with no prose between them are ONE run:\n{screen}"
    );
    assert!(
        screen.contains("[7 tool calls") && screen.contains("1 thinking line"),
        "the counts aggregate over the whole run:\n{screen}"
    );
    // The positive control, and the reason the number above means anything: **prose
    // ends a run.** A second visible sentence in the middle makes it two.
    //
    // **Applied in transcript order.** The events go through `apply` in the order they
    // are written and `env`'s seq is only a stamp, so a control written out of order
    // puts the rows in the wrong order and counts one marker for the wrong reason.
    let mut b = app();
    b.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let mut seq = 1u64;
    // One closure rather than two, because both would capture `seq` and the second is
    // then a second mutable borrow. It takes the whole item, so the two shapes this
    // control needs — prose and a result — are one call with different arguments.
    let mut put = |b: &mut App, id: &str, kind: &str, item: TranscriptItem| {
        b.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptAppended {
                item_id: id.into(),
                kind: kind.into(),
                ledger_head: String::new(),
            },
        )));
        seq += 1;
        b.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(item),
            },
        )));
        seq += 1;
    };
    for (id, text) in [("s.0", "first:"), ("s.2", "second:")] {
        put(
            &mut b,
            id,
            "assistant",
            TranscriptItem::Assistant {
                text: text.into(),
                tool_calls: Vec::new(),
                truncated: false,
            },
        );
        let at = if id == "s.0" { "s.1" } else { "s.3" };
        put(
            &mut b,
            at,
            "tool_result",
            TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "output".into(),
                edit: None,
                origin: None,
                media: None,
            },
        );
    }
    b.visibility = Visibility::of(Profile::CONVERSATION);
    b.invalidate_history();
    let two = b.screen(110, 200).join("\n");
    assert_eq!(
        markers(&two),
        2,
        "prose the reader can see is what ends a run:\n{two}"
    );
}

/// **The marker the walk could not reach.** A run longer than the line budget used to
/// lose its marker entirely when it was filled from the bottom: the marker is drawn at
/// the run's FIRST row and the backward walk renders the newest first, so stopping on
/// the budget left the rows it had passed showing nothing at all.
#[test]
fn a_run_longer_than_the_window_still_draws_its_marker() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // Sixty hidden rows and no prose anywhere, so the whole transcript is one run.
    for i in 0..60u64 {
        let id = format!("s.{i}");
        a.apply(ServerFrame::Event(env(
            i * 2 + 1,
            testing::appended(&id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 2,
            SessionEvent::TranscriptContent {
                item_id: id.clone(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: format!("c{i}"),
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
    // A window far too short to hold the run, and the tail path taken on purpose by
    // walking backward over it — which is the path that used to stop short.
    a.invalidate_history();
    a.fill_backward(10);
    let screen = a.screen(110, 24).join("\n");
    assert_eq!(
        markers(&screen),
        1,
        "a run with no prose in it is one marker, however tall:\n{screen}"
    );
    assert!(
        screen.contains("[60 tool calls"),
        "and it counts every row it stands for:\n{screen}"
    );
}

/// **The rung survives a head restart** — the operator's *"make versbosity a config option
/// so it persists headrestarts."*
///
/// Asserted through `load_prefs` **and nothing else** — the loader is the thing a fresh
/// head runs, so driving it is the whole of what "persists a restart" means. A test that
/// set `a.verbosity` by hand after loading would be asserting that a field can be
/// assigned, which is the shape of test that passes while the file says nothing.
#[test]
fn the_rung_is_written_down_and_comes_back_on_the_next_head() {
    let dir = std::env::temp_dir().join(format!("letibot-rung-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("head.toml");

    // The head the reader is in: they choose a rung.
    let mut a = app();
    a.prefs_path = Some(path.clone());
    assert_eq!(
        a.visibility.profile(),
        Some(Profile::NORMAL),
        "the head starts here"
    );
    assert_eq!(a.command("verbosity conversation"), None);
    assert_eq!(a.visibility.profile(), Some(Profile::CONVERSATION));
    assert!(
        path.is_file(),
        "choosing a rung did not write it down: {}",
        path.display()
    );
    // The confirmation says where it went, like every other persisted setting here.
    let notice = a.notice.clone().unwrap();
    assert!(!notice.contains("not saved"), "{notice}");

    // The next head: a fresh app, the same file, and `load_prefs` is all it runs.
    let mut fresh = app();
    fresh.prefs_path = Some(path.clone());
    fresh.load_prefs();
    assert_eq!(
        fresh.visibility.profile(),
        Some(Profile::CONVERSATION),
        "the rung did not come back on a restarted head"
    );

    // **And a rung the file names that this build does not know is REPORTED, not obeyed.**
    // §13.2b's rule for a setting: silently starting at the default would make a typo and
    // a deliberate `normal` the same screen.
    std::fs::write(&path, "verbosity = \"chatty\"\n").unwrap();
    let mut third = app();
    third.prefs_path = Some(path.clone());
    third.load_prefs();
    assert_eq!(
        third.visibility.profile(),
        Some(Profile::NORMAL),
        "an unknown rung was obeyed"
    );
    assert!(
        third.notice.clone().unwrap_or_default().contains("chatty"),
        "an unreadable rung was swallowed: {:?}",
        third.notice
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A set the card can show is a set the file keeps** — the operator's report, and both halves
/// of it: *"it is not saved - when i do /verbosity there is no custom"*.
///
/// Two faults, and they compounded into one symptom:
///
///  * the card's rows came from a hand-written table of **four** profiles, so `read-edits` —
///    *conversation plus the edit cards* — had no row at all, and neither had a set off the
///    ladder. A rung `/v` cycles onto was a rung the card could not show;
///  * `prefs::load` validated the saved word against that same four-name list, so `read-edits`
///    and every `custom …` set were written by this head and then **refused when the file was
///    read back** — the reader chose a set, the file kept it, and the next start fell back to
///    `normal` in silence.
///
/// The assertion is the round trip the operator was making: choose it, write it, read it with a
/// fresh head. Both spellings, because they fail in the same place — one is a profile the table
/// never had and the other is no profile at all.
#[test]
fn a_set_the_card_can_choose_is_a_set_the_file_keeps() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-rung-roundtrip-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("head.toml");

    // **`read-edits`, which is a profile the card must be able to draw.**
    let mut a = app();
    a.prefs_path = Some(path.clone());
    assert_eq!(a.command("verbosity read-edits"), None);
    assert_eq!(a.visibility.profile(), Some(Profile::READ_EDITS));
    let mut fresh = app();
    fresh.prefs_path = Some(path.clone());
    fresh.load_prefs();
    assert_eq!(
        fresh.visibility.profile(),
        Some(Profile::READ_EDITS),
        "`read-edits` was written and then refused when the file was read back"
    );
    assert!(
        fresh.notice.is_none(),
        "and nothing was reported about it: {:?}",
        fresh.notice
    );

    // **And a set no profile is** — the `custom …` spelling, which is the one the card writes for
    // just this case and the one the operator was looking for.
    let mut b = app();
    b.prefs_path = Some(path.clone());
    assert_eq!(b.command("verbosity custom edits=hidden"), None);
    assert!(
        b.visibility.profile().is_none(),
        "the premise: this set is no profile: {}",
        b.visibility.as_str()
    );
    let mut fresh = app();
    fresh.prefs_path = Some(path.clone());
    fresh.load_prefs();
    assert_eq!(
        fresh.visibility,
        b.visibility,
        "a custom set did not survive the round trip: {}",
        fresh.visibility.as_str()
    );
    assert!(
        fresh.notice.is_none(),
        "the set came back and was reported as unreadable: {:?}",
        fresh.notice
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A completion notice is folded to one line per settlement, and R7's promise to the    /// **A completion notice is folded to one line per settlement, and R7's promise to the
/// model is not drawn.**
///
/// The operator's ask, and leticl's half of it: *"i dont want to see that message to you
/// 'This is the completion…'"*. The two strings below are the daemon's own text, copied out
/// of the store, so this asserts against what is actually on the wire and not against a
/// paraphrase — the trailing paragraph is the part being dropped, and it is dropped because
/// it is addressed to the model.
#[test]
fn a_completion_notice_folds_to_one_line_per_settlement() {
    const JOB: &str = "[job] a job you backgrounded has ended:\n  - `j57` exited 0 after 7m06s, wrote 508 bytes: sleep 3; echo done\nThis is the completion arriving on its own — you do not need to wait for it, and `job_wait` would only block you for a result you already have.";
    const TASK: &str = "[task] a subagent you started has finished:\n  - `s-1791017230755743833-sub-1791119445423` done: 3529\nThis is the completion arriving on its own — you do not need to wait for it, and calling `task_result` to block would only hold you for a result you already have.";

    let folded = folded_notice(JOB, &[]).expect("a job notice folds");
    assert_eq!(
        folded, "Job j57 exited 0 after 7m06s, wrote 508 bytes: sleep 3; echo done",
        "every fact is kept and the heading is not"
    );
    assert!(
        !folded.contains("you do not need to wait for it"),
        "the promise is to the model: {folded}"
    );
    // The same shape, the other noun, and the child's own answer kept whole.
    assert_eq!(
        folded_notice(TASK, &[]).expect("a child notice folds"),
        "Agent s-1791017230755743833-sub-1791119445423 · done: 3529"
    );

    // **Three settlements are three lines, not a heading and a count.** The daemon's
    // heading is `3 jobs you backgrounded have ended:`, and a folded line names its own
    // kind — which is exactly the argument for dropping it.
    const THREE: &str = "[job] 3 jobs you backgrounded have ended:\n  - `j11` exited 0 after 12.0s, wrote 13 bytes: a\n  - `j12` exited 0 after 16.0s, wrote 13 bytes: b\n  - `j13` exited 0 after 20.0s, wrote 13 bytes: c\nThis is the completion arriving on its own — you do not need to wait for it.";
    let three = folded_notice(THREE, &[]).expect("a group of three folds");
    let lines: Vec<&str> = three.lines().collect();
    assert_eq!(lines.len(), 3, "one line per settlement: {three}");
    assert!(
        lines.iter().all(|l| l.starts_with("Job ")),
        "each line names its own kind: {three}"
    );
    assert!(
        !three.contains("3 jobs"),
        "the count was the heading's job, and the heading is gone: {three}"
    );
}

/// **A named job folds to the same line, with the name in it.**
///
/// The label is `release-build · j65` — spaces and all — and the fold took the bullet's first
/// word for the handle, closed the backtick on it and failed, which dropped the WHOLE row to raw
/// prose: the operator's line would have lost its fold, and the paragraph R7 addresses to the
/// model would have come back, on exactly the notice the name was asked for. So the handle is
/// the whole span between the backticks, and this is the test that says so.
#[test]
fn a_named_jobs_settlement_folds_with_its_name() {
    const NAMED: &str = "[job] a job you backgrounded has ended:\n  - `release-build · j65` exited 0 after 54.9s, wrote 1030 bytes: cargo test --workspace\nThis is the completion arriving on its own — you do not need to wait for it, and `job_wait` would only block you for a result you already have.";
    let folded = folded_notice(NAMED, &[]).expect("a named job's notice folds");
    assert_eq!(
        folded,
        "Job release-build · j65 exited 0 after 54.9s, wrote 1030 bytes: cargo test --workspace",
        "the name is kept; the heading and the promise are not"
    );
    assert!(
        !folded.contains("you do not need to wait for it"),
        "the promise is to the model: {folded}"
    );
}

/// **The plan's nudge is the same shape — and the operator asked for its HEAD alone.**
///
/// Their words, on the row they have been reading all evening: *"in read verbosity todo nag
/// shouldnt show me model prompt only todo head."* The row is `[todo check] …` and its closing
/// paragraph is advice **addressed to the model** — *do this one, or mark it done, or drop
/// it* — so it folds like a completion notice does, to the item and whose row it is.
///
/// **Both closings are named**, because the operator's row and the model's own are different
/// sentences (`unfinished_plan`), and a nag the fold cannot account for is drawn whole rather
/// than halved.
#[test]
fn the_todo_nag_folds_to_the_item_and_not_to_the_advice() {
    const MINE: &str = "[todo check] this turn is finished and one item is not done (1 more open):\n  - T2 · the launcher — yours\ndo this one, or mark it done, or drop it — a plan left open is a plan nobody is following.";
    assert_eq!(
        folded_notice(MINE, &[]).unwrap(),
        "Todo T2 · the launcher — yours"
    );
    const THEIRS: &str = "[todo check] this turn is finished and one item is not done:\n  - git status in the status line — look how leticl did it — the operator's\nthe operator asked for this one, so do it — or mark it done with `todo_write`'s `operator` field, quoting the text above exactly.";
    assert_eq!(
        folded_notice(THEIRS, &[]).unwrap(),
        "Todo git status in the status line — look how leticl did it — the operator's"
    );
    // **No advice reaches the screen**, which is the whole of the ask.
    for nag in [MINE, THEIRS] {
        let folded = folded_notice(nag, &[]).expect("a nag folds");
        for advice in [
            "mark it done",
            "plan nobody is following",
            "quoting the text above",
            "do this one",
        ] {
            assert!(
                !folded.contains(advice),
                "`{advice}` is addressed to the model, not to the reader: {folded}"
            );
        }
    }
    // And a nag whose closing is not one this head knows is drawn whole.
    const STRANGE: &str = "[todo check] this turn is finished and one item is not done:\n  - T2 · the launcher — yours\nSomething new the daemon has started saying.";
    assert!(folded_notice(STRANGE, &[]).is_none());
}

/// **One row can hold both kinds** — the daemon coalesces what it submits, so a job group and
/// a child group arrive as one item — and both fold, in the order they were written.
#[test]
fn a_notice_holding_a_job_and_a_child_folds_to_both() {
    const BOTH: &str = "[job] a job you backgrounded has ended:\n  - `j57` exited 0 after 3.0s, wrote 5 bytes: sleep 3; echo done\nThis is the completion arriving on its own — you do not need to wait for it.\n\n[task] a subagent you started has finished:\n  - `s-p-sub-1` done: 3529\nThis is the completion arriving on its own — you do not need to wait for it.";
    let folded = folded_notice(BOTH, &[]).expect("both kinds fold");
    let lines: Vec<&str> = folded.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "one line per settlement, across both: {folded}"
    );
    assert_eq!(
        lines[0], "Job j57 exited 0 after 3.0s, wrote 5 bytes: sleep 3; echo done",
        "the job first, as the daemon wrote it"
    );
    assert_eq!(lines[1], "Agent s-p-sub-1 · done: 3529");
}

/// **A child's own task rides the folded line** — the piece leticl has and this head did
/// not, and it is LOOKED UP rather than parsed.
///
/// The notice carries only what the child answered; what it was asked is on the subagent
/// row, so this is `subagent_asked`'s own words and not a second reading of the notice. The
/// two fallbacks are asserted with it, because both are real: a child this head never
/// watched spawn (a head that attached after the spawn), and a daemon older than the `task`
/// field, whose `prompt` still holds the row.
#[test]
fn the_folded_agent_line_names_the_task_it_was_asked() {
    const TASK: &str = "[task] a subagent you started has finished:\n  - `s-p-sub-1` done: 3529\nThis is the completion arriving on its own — you do not need to wait for it.";

    let sub = SubagentState {
        session_id: "s-p-sub-1".into(),
        state: "done".into(),
        generating: false,
        // The legacy meaning on the finish: the ANSWER's first line. Which is why the task
        // field exists, and why the fallback below is not this.
        prompt: "ready.".into(),
        role: "coder".into(),
        task: "Answer with one word:\n  ready.".into(),
        model: String::new(),
        answer: Some("ready".into()),
        spawned_ms: 0,
    };
    assert_eq!(
        folded_notice(TASK, std::slice::from_ref(&sub)).unwrap(),
        "Agent s-p-sub-1 · Answer with one word: ready. · done: 3529",
        "the task is flattened to one line, the answer kept whole"
    );

    // **No row for the handle**: the line still names the handle and the answer. A fold that
    // dropped the line instead would lose a settlement for being un-describable.
    assert_eq!(
        folded_notice(TASK, &[]).unwrap(),
        "Agent s-p-sub-1 · done: 3529"
    );

    // **A daemon older than `task`**: `prompt` is the fallback, and this is the one place a
    // reader could be shown the answer twice — which is why the task is preferred when it is
    // there and `prompt` only stands in when it is not.
    let older = SubagentState {
        task: String::new(),
        ..sub
    };
    assert_eq!(
        folded_notice(TASK, std::slice::from_ref(&older)).unwrap(),
        "Agent s-p-sub-1 · ready. · done: 3529"
    );
}

/// **A call in flight is counted before its row exists** — the operator's *"display looks
/// frozen, while in fact it is just say cargo testing with yellow dot."*
///
/// The structural reason is in `harnessd::harness`: a round's result rows are appended
/// **after every call in it has run**, so between the narration and the first result the
/// transcript holds no work at all. A marker built from rows therefore counted nothing and
/// said nothing, while the reader watched a `cargo test` that had been running for a
/// minute — a screen that reads as a head which has stopped.
///
/// The turn is what knows. This drives a turn whose call is proposed and running with no
/// result yet, and asks for the counts.
#[test]
fn a_call_in_flight_is_counted_before_its_row_exists() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // The narration, and then the work — with no rows to stand for it.
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Text,
            text: "let me run the tests:".into(),
        },
    )));
    // The row the prose will become, and the call proposed on the turn.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "let me run the tests:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        5,
        testing::proposed_on("t1", "c1", "bash", "cargo test"),
    )));
    a.apply(ServerFrame::Event(env(
        6,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: "exec".into(),
        },
    )));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("[1 tool call]"),
        "the call in flight is not counted, so the screen says the head stopped:\n{screen}"
    );
    assert!(
        !screen.contains("cargo test"),
        "the call's own card is the working and this rung does not draw it:\n{screen}"
    );
}

/// **The counts are plain and the seam is faint** — the operator's split, ruled across
/// the two heads.
///
/// This head painted the whole marker faint and leticl painted only the seam; **leticl's
/// reading is the one that stands.** The counts are *punctuation inside a sentence* — the
/// marker sits on the end of the prose the colon points at — and nothing in prose is dimmed
/// mid-sentence except an aside, which the counts are not: they are the only content the
/// marker carries. The seam is the aside, and R29's register rule says so: **dim is for a
/// sentence you could delete with the reader no worse off.**
///
/// Asserted on the escapes at both ends, because the words are identical either way and
/// the register is the whole of what changed: the `[` must NOT be preceded by the faint
/// code, and the dot must be inside the faint run with the seam.
///
/// **The earlier ruling still stands and is the other half of this**: the seam is gray,
/// dot included. This test is where it is kept.
/// **An entry that GREW after a row was bound to it is not drawn twice** — R51 item 15.
///
/// The shape that defeats a whole-string comparison: the operator types `A`, and it is bound to
/// the body-less row the daemon announced. Then they keep typing (behind a running turn, so item
/// 14's coalescing puts `B` and `C` into the SAME entry), and the entry is now `"A\nB\nC"` while
/// the row is still drawing `"A"`. The tail's question used to be *is this entry already on
/// screen* — answered NO for every such entry — so the WHOLE thing was drawn again and `A`
/// appeared twice.
///
/// The rule is that the unit of DRAWING is the entry and the unit of CLAIMING is the piece: the
/// line the row is drawing is spent, and what is left of the entry is drawn as one block.
#[test]
fn an_echo_that_grew_after_it_was_bound_is_not_drawn_twice() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // `A` is sent and queued; the daemon announces its row with no body yet; the head binds
    // the echo it holds to that row, which is where the words belong.
    typed(&mut a, "A");
    a.key(Key::Enter);
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "user".into(),
            ledger_head: String::new(),
        },
    )));
    assert_eq!(
        a.bound_prompts.get("s.0").map(String::as_str),
        Some("A"),
        "the premise: the row is drawing the words it was bound to"
    );
    // …and then `B` and `C`, which coalesce ONTO that entry.
    typed(&mut a, "B");
    a.key(Key::Enter);
    typed(&mut a, "C");
    a.key(Key::Enter);
    assert_eq!(
        a.pending_prompts,
        vec!["A\nB\nC".to_string()],
        "the premise: one entry, three lines"
    );
    let screen = a.screen(100, 30).join("\n");
    // **The claim, and it is the whole of this item.** The bound row draws `A`; the tail must
    // draw only what that does not — not the whole entry a second time.
    //
    // **The bound row carries no mark and the remainder does** — see the tail's own comment
    // for why those are two questions rather than one spelling. This assertion used to count
    // `queued · A`, which was true only while a landed row wore the echo's mark: the defect
    // R2 names, visible here as `A` reading `queued` under a reply that was already streaming.
    assert_eq!(
        screen.matches("▌ A").count(),
        1,
        "`A` is on the screen more than once: {screen:?}"
    );
    assert_eq!(
        screen.matches("▌ queued · B").count(),
        1,
        "the leftover of a claimed entry is drawn once, hedged: {screen:?}"
    );
    // And the remainder is drawn as ONE block — the coalescing item 14 requires — so `B` and
    // `C` are together in one row and not one row each. R33's shape: the elided headline and
    // its seam, which is what says there is more under it.
    assert_eq!(
        screen.matches("… +1 lines · /t opens it").count(),
        1,
        "the remainder is not one elided block: {screen:?}"
    );
    assert_eq!(
        screen.matches("▌").count(),
        2,
        "one bar for the row and one for the remainder, and no third: {screen:?}"
    );
}

/// **An entry that grew is `unconfirmed` still, by its ORIGINAL text** — the mark rides on the
/// entry.
///
/// The near-miss shape of item 15: the piece-claiming walk hands back a REMAINDER, and asking
/// `unconfirmed` about the remainder rather than about the entry would silently drop the mark for
/// exactly the case that item is about — a snapshot marked the echo, and then it grew.
#[test]
fn a_grown_entry_keeps_the_mark_its_original_text_carried() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut a, "A");
    a.key(Key::Enter);
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "user".into(),
            ledger_head: String::new(),
        },
    )));
    // The mark is on the entry as it was, which is the entry's identity.
    a.unconfirmed.push("A".into());
    typed(&mut a, "B");
    a.key(Key::Enter);
    let screen = a.screen(100, 30).join("\n");
    assert!(
        screen.contains("queued · A"),
        "the row is drawing the words it was bound to: {screen:?}"
    );
    // **And the tail's remainder carries the same word**, because it is the same prompt: the
    // row above asked `unconfirmed` under the text it is drawing, and so does this.
    assert!(
        screen.contains("queued · B"),
        "the remainder disagreed with its own row about one prompt: {screen:?}"
    );
}

/// **ONE marker for the work, not one per kind of work.**
///
/// Caught in a tmux sample of the live head, 2026-09-26, at second resolution:
///
/// ```text
/// ...a call finished, its result row landed, and the next round is THINKING...
///
/// [1 tool call] · ctrl-t opens it          <- the walk's marker for the hidden run
///
///   [1 thinking line] · ctrl-t opens it    <- the PANE's marker, a second one
///     Responding · 3m10s
/// ```
///
/// The operator: *"[] statistics is broken essentially - print tools and thinking separately, and
/// when it is just thinking doesnt advance the counter - [] disappears and then - when it is
/// thinking without response it prints it right above Responding."*
///
/// **Two markers, because two things draw one.** The walk draws a marker for a hidden RUN; the
/// live pane draws a marker for the work IN FLIGHT; and the function whose entire job is to say
/// *the walk has already counted this* — `live_tail_covered` — answers **false** in exactly the
/// case it exists for. Its docstring: *"true when the transcript's last row is invisible, because
/// then the stretch it sits in runs to the end and `unseen_run_at` has already folded the tail
/// into its counts."* Its body: `!rung.hides_the_working() && …` — so when the rung DOES hide
/// the working (the only case where a run exists at all) the first term is false and the
/// function can never return true.
///
/// The state below is the sampled one: a round whose call has finished with its result row
/// landed (so the walk has a run to count), and thinking streaming with no call outstanding.
/// **The marker is drawn ONCE per line, however many frames draw it.**
///
/// The operator's screen, twice: `…half-fixed: [1 tool call] · ctrl-t opens it [1 tool call] ·
/// ctrl-t opens it` — the SAME marker twice on one line, which is the append problem again in
/// the other join.
///
/// There are two places a marker is glued to a line, and only one of them was made undoable:
///
///   · the end-of-walk join (`App::live_join`) — recorded, restored next frame;
///   · **the in-walk join, at a run's first row** — which appends to `hist_lines[at]` with no
///     record at all, and `hist_lines` is the cache of RENDERED rows.
///
/// So a frame that re-walks the run's row glues a second marker onto the line the first one is
/// already in. This renders the same state twice, which is the smallest thing that can catch it.
#[test]
fn a_marker_is_glued_to_a_line_once_however_many_frames_draw_it() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("s.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "Running the tests:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        testing::proposed_on("t1", "c1", "bash", "\"cargo test\""),
    )));
    // **CONSECUTIVE CALLS, which is the shape the operator named** — *"sometimes on consequtive
    // tool calls and things"*. Each round: a call finishes with its result row, and the next is
    // proposed, so between two renders the run grows and the marker's text changes.
    for n in 0..4u64 {
        let seq = 10 + n * 8;
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::ToolFinished {
                turn_id: "t1".into(),
                call_id: format!("c{n}"),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload_digest: "d".into(),
                inline_bytes: 1,
                full_bytes: 1,
                spill: None,
                repairs: 0,
                edit: None,
            },
        )));
        let rid = format!("s.{}", 10 + n);
        a.apply(ServerFrame::Event(env(
            seq + 1,
            testing::appended(&rid, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            seq + 2,
            SessionEvent::TranscriptContent {
                item_id: rid,
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: format!("c{n}"),
                    name: "bash".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!("result {n}"),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
        // …and the next call is proposed, which is the live work of the next round.
        a.apply(ServerFrame::Event(env(
            seq + 3,
            testing::proposed_on("t1", &format!("c{}", n + 1), "bash", "\"cargo test\""),
        )));
        let screen = a.screen(100, 30).join("\n");
        assert_eq!(
            markers(&screen),
            1,
            "render {} drew the marker {} times:\n{screen}",
            n + 1,
            markers(&screen)
        );
    }
}

#[test]
fn the_work_in_flight_is_counted_by_one_marker_not_two() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // The prose that introduced the call, and the call itself.
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("s.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "Running the tests:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        testing::proposed_on("t1", "c1", "bash", "\"cargo test\""),
    )));
    // **The call FINISHES and its result row lands** — that row is what the walk counts, and it
    // is hidden under this rung. This is the half the marker is for.
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "d".into(),
            inline_bytes: 1,
            full_bytes: 1,
            spill: None,
            repairs: 0,
            edit: None,
        },
    )));
    a.apply(ServerFrame::Event(env(
        6,
        testing::appended("s.1", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        7,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "test result: ok".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    // **AND THE OPERATOR'S OWN MESSAGE IS THE LAST ROW** — which is the half that matters, and
    // the operator named it: *"again it happens when we have my queued messages."* The pane's
    // marker only joins the sentence above it when that sentence is the MODEL's prose; an
    // operator row is not, so the pane must draw its marker on its own line — and that is the
    // second marker, beside the walk's.
    a.apply(ServerFrame::Event(env(8, testing::appended("s.2", "user"))));
    a.apply(ServerFrame::Event(env(
        9,
        SessionEvent::TranscriptContent {
            item_id: "s.2".into(),
            item: Box::new(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "and what about the cache?".into(),
                }],
            }),
        },
    )));
    // **And the next round is THINKING** — no call outstanding, reasoning streaming.
    a.apply(ServerFrame::Event(env(
        10,
        testing::reasoning("t1", "the counts are only"),
    )));

    let live = a.live_work_now();
    assert_eq!(live.calls, 0, "the premise: no call is outstanding");
    assert!(live.think_lines > 0, "the premise: thinking is streaming");

    let screen = a.screen(100, 30);
    let text = screen.join("\n");
    let markers = markers(&text);
    assert_eq!(
        markers, 1,
        "the work in flight is drawn by {markers} markers — the walk's and the pane's, for one \
             run:\n{text}"
    );
    // **And the one marker carries BOTH numbers**, which is the operator's *"when it is just
    // thinking doesnt advance the counter"*: the run's finished call and the thinking still
    // arriving, in the same brackets.
    //
    // Asserted as the two counts rather than as one exact spelling, because WHICH rung of the
    // ladder a marker wears is a function of the room the line had — see
    // `the_marker_steps_down_its_ladders_instead_of_growing` for the ladder itself. What must
    // not vary is that both facts are there and there is one marker.
    assert!(
        text.contains("1 tool") && text.contains("1 thinking"),
        "the single marker does not carry both counts:\n{text}"
    );
}

/// **A FACT THE MARKER DRAWS IS A FACT THE CACHE KEY HOLDS** — the property the two
/// omissions were missing made checkable, and the reason [`MarkerFacts`] exists at all.
///
/// It is what [`hidden_run_marker`] reads *and* what `App::marker_facts` compares, so the
/// table below is the whole of the marker's dependence on NOW: three fields it draws, and
/// `run`, which it does not draw but the rebuild needs — the run the work has LEFT has to be
/// rebuilt without the colour. **Two assertions per row, because the two halves must not come
/// apart:** a fact that moves the painting but not the key is a stale row (the colour, then
/// the run — both shipped), and a fact that moves the key but not the painting is a rebuild
/// for nothing.
#[test]
fn every_fact_the_marker_draws_is_a_fact_the_key_holds() {
    let cfg = RenderConfig {
        color: true,
        ..plain_cfg(110)
    };
    let vis = Visibility::of(Profile::CONVERSATION);
    let paint = |facts: MarkerFacts| {
        hidden_run_marker(&[], 0, 0, vis, &cfg, true, facts, true).painted(&cfg)
    };
    let live = |calls: usize, running: usize, think_lines: usize| LiveWork {
        calls,
        running,
        think_lines,
    };
    let base = MarkerFacts::of(live(1, 1, 0), Some(0));
    // The three the marker draws, and the one it does not.
    let cases: [(&str, MarkerFacts, bool); 4] = [
        (
            "the calls count",
            MarkerFacts::of(live(2, 1, 0), Some(0)),
            true,
        ),
        (
            "the thinking count",
            MarkerFacts::of(live(1, 1, 3), Some(0)),
            true,
        ),
        (
            "the yellow (`running`)",
            MarkerFacts::of(live(1, 0, 0), Some(0)),
            true,
        ),
        (
            "which run the work is in (not drawn)",
            MarkerFacts::of(live(1, 1, 0), Some(4)),
            false,
        ),
    ];
    assert!(
        paint(base).contains("\u{1b}[33m"),
        "the premise: a running call paints its count pending"
    );
    for (what, facts, drawn) in cases {
        assert!(
            facts != base,
            "{what} does not move the key — that row stays stale"
        );
        assert_eq!(
            paint(facts) != paint(base),
            drawn,
            "{what}: the painting and the key disagree about whether this is drawn"
        );
    }
}

/// **ONE TURN OF MANY ROUNDS WEARS ONE YELLOW MARKER, NOT FORTY** — the operator,
/// on this session's own screen while a long turn ran: *"old tool calls stayed yellow
/// for some reason."*
///
/// The `live_here` question was *does this run hold a row of THIS TURN*, and a turn of
/// forty rounds is forty runs, every one of them holding this turn's rows — so every
/// marker folded the in-flight counts (inflating each, so the numbers were wrong as
/// well as the colour) and every marker was painted pending. The work in flight happens
/// after every committed row, so it continues the NEWEST run and no other; the turn
/// clause stays, because a run of an EARLIER turn is still not this turn's.
///
/// Both directions are asserted here: two rounds of one turn, a call in flight, exactly
/// one yellow marker with the in-flight count folded in and the other run plain.
#[test]
fn only_the_newest_runs_marker_is_yellow_in_a_turn_of_many_rounds() {
    let mut a = App::new(RenderConfig {
        color: true,
        ..plain_cfg(110)
    });
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // **Two rounds, each: the model's prose, then a call and its result row.** At the
    // conversation rung the prose is drawn and the result row is hidden, so each round
    // is its own run of hidden rows with its own marker — which is the shape the defect
    // needed and the shape a long turn has.
    for (i, (seq, row_id, text, call_id)) in [
        (2u64, "s.0", "round one says a thing", "c1"),
        (8, "s.2", "round two says a thing", "c2"),
    ]
    .into_iter()
    .enumerate()
    {
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(&format!("{row_id}.a"), "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            seq + 1,
            testing::content(&format!("{row_id}.a"), text),
        )));
        a.apply(ServerFrame::Event(env(
            seq + 2,
            testing::proposed("t1", call_id, "bash"),
        )));
        a.apply(ServerFrame::Event(env(
            seq + 3,
            SessionEvent::ToolStarted {
                turn_id: "t1".into(),
                call_id: call_id.into(),
                name: "bash".into(),
                access: "exec".into(),
            },
        )));
        a.apply(ServerFrame::Event(env(
            seq + 4,
            SessionEvent::ToolFinished {
                turn_id: "t1".into(),
                call_id: call_id.into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload_digest: "d".into(),
                inline_bytes: 12,
                full_bytes: 12,
                spill: None,
                repairs: 0,
                edit: None,
            },
        )));
        a.apply(ServerFrame::Event(env(
            seq + 5,
            testing::appended(row_id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            seq + 6,
            SessionEvent::TranscriptContent {
                item_id: row_id.into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: call_id.into(),
                    name: "bash".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!("result for round {i}"),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    // **Work in flight: a third round's call, proposed and running, with no row yet.**
    // That is what the newest marker carries, and what no other marker may.
    a.apply(ServerFrame::Event(env(
        20,
        testing::proposed("t1", "c3", "bash"),
    )));
    a.apply(ServerFrame::Event(env(
        21,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c3".into(),
            name: "bash".into(),
            access: "exec".into(),
        },
    )));
    let live = a.live_work_now();
    assert!(live.running > 0, "the premise: a call is executing");

    let frame = a.screen(110, 40);
    let yellow = "\u{1b}[33m";
    let marker_rows: Vec<&String> = frame
        .iter()
        .filter(|l| l.contains("tool call") || l.contains("tool calls"))
        .collect();
    assert!(
        marker_rows.len() >= 2,
        "the fixture must draw two runs' markers, got {marker_rows:?}"
    );
    let yellow_rows: Vec<&&String> = marker_rows.iter().filter(|l| l.contains(yellow)).collect();
    assert_eq!(
        yellow_rows.len(),
        1,
        "exactly the newest run's marker is pending; the rest are settled history:\
             \n{}",
        frame.join("\n")
    );
    // The NUMBER is `calls` (the run's own row plus the one in flight), and `running`
    // is what lights the colour — see `LiveWork`. So the yellow marker carries two and
    // the settled one carries one, and that difference is the in-flight work.
    assert!(
        yellow_rows[0].contains('2'),
        "the yellow marker does not carry the in-flight call: {:?}",
        yellow_rows[0]
    );
    // **And the numbers are not double-counted**: the older run's marker carries only
    // its own one call, which is the other half of the same defect.
    let older = marker_rows
        .iter()
        .find(|l| !l.contains(yellow))
        .expect("an older marker");
    assert!(
        older.contains("[1 tool call]") || older.contains("1 tool"),
        "an older run's marker is inflated by the in-flight work: {older:?}"
    );
}

/// **A YELLOW THAT STAYS ON A RUN THE WORK HAS LEFT** — the operator's own screen, read back to
/// me: *"look how many tools are yellow"*, with **five** markers lit at once on one turn.
///
/// The invariant is that ONE marker carries the yellow: it says *a call of THIS number is
/// executing*, so it belongs to the run the work is in, and every other run is settled history
/// that draws plain. `only_the_newest_runs_marker_is_yellow_in_a_turn_of_many_rounds` asserts
/// exactly that and passes — because it renders the FINISHED state in one walk, where the
/// decision is consistent by construction.
///
/// **What breaks it is the frames in between.** A marker is painted into `hist_lines`, the cache
/// of RENDERED rows, and the yellow is a fact about NOW rather than about the row — so each run
/// is yellow while it IS the newest, and the invalidation that should take the colour away keys
/// on the COUNTS, which can be identical between two rounds (the operator's screen carried
/// `[2 tool calls, 6 thinking lines]` five times) and only ever rebuilds the newest run's row.
/// The runs above keep the yellow for the rest of the turn.
///
/// So this drives the turn a frame at a time, which is what a person watches.
#[test]
fn the_yellow_does_not_stay_on_a_run_the_work_has_left() {
    let mut a = App::new(RenderConfig {
        color: true,
        ..plain_cfg(110)
    });
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r0".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            // One prompt's stamp, carried by every round of it: the only thing on the wire that
            // says these rounds are one turn.
            began_ms: Some(1_000),
        },
    )));
    // **Three rounds, each driven as the daemon drives them** — the model's prose, a call
    // proposed and started, its result row — with a FRAME at every step, because a framing is
    // what bakes a marker into the cache.
    let mut seq = 2u64;
    for round in 0..3u64 {
        let prose = format!("p.{round}");
        a.apply(ServerFrame::Event(env_at(
            seq,
            1_000,
            testing::appended(&prose, "assistant"),
        )));
        a.record_item(
            &prose,
            TranscriptItem::Assistant {
                text: format!("round {round} of the work:"),
                tool_calls: Vec::new(),
                truncated: false,
            },
        );
        seq += 1;
        a.apply(ServerFrame::Event(env(
            seq,
            testing::proposed_on("r0", &format!("c{round}"), "bash", "\"cargo test\""),
        )));
        seq += 1;
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::ToolStarted {
                turn_id: "r0".into(),
                call_id: format!("c{round}"),
                name: "bash".into(),
                access: Default::default(),
            },
        )));
        seq += 1;
        // **A frame with this round's call executing**, which is the moment the yellow belongs
        // to this run — and the moment the marker is painted with it.
        a.screen(110, 40);
        // It returns, and its row lands: this round's run is now settled history.
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::ToolFinished {
                turn_id: "r0".into(),
                call_id: format!("c{round}"),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload_digest: "d".into(),
                inline_bytes: 20,
                full_bytes: 20,
                spill: None,
                repairs: 0,
                edit: None,
            },
        )));
        seq += 1;
        let result = format!("t.{round}");
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(&result, "tool_result"),
        )));
        seq += 1;
        a.record_item(
            &result,
            TranscriptItem::ToolResult {
                call_id: format!("c{round}"),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "test result: ok".into(),
                edit: None,
                origin: None,
                media: None,
            },
        );
    }
    // **And the work is in flight, which is what lights anything at all**: a fourth call
    // proposed and started, with no row of its own. One marker may be yellow — this one.
    a.apply(ServerFrame::Event(env(
        seq,
        testing::proposed_on("r0", "c3", "bash", "\"cargo test\""),
    )));
    a.apply(ServerFrame::Event(env(
        seq + 1,
        SessionEvent::ToolStarted {
            turn_id: "r0".into(),
            call_id: "c3".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    assert!(
        a.live_work_now().running > 0,
        "the premise: a call is executing, so some marker must carry the yellow"
    );

    let screen = a.screen(110, 40);
    // **Counted by the words, not by `is_marker`** — that helper reads a digit straight after the
    // `[`, which is true only of a frame drawn with no colour: a pending count has an escape
    // where the digit is (`[\u{1b}[33m2\u{1b}[0m tool calls]`), which is the very thing this test
    // is about. `tool call` is on a marker and nowhere else on this screen.
    let drawn = screen.iter().filter(|l| l.contains("tool call")).count();
    let lit = screen
        .iter()
        .filter(|l| l.contains("tool call") && l.contains("\u{1b}[33m"))
        .count();
    assert!(
        drawn >= 3,
        "the fixture must draw a marker per round, and it drew {drawn}:\n{}",
        screen.join("\n")
    );
    assert_eq!(
        lit,
        1,
        "the work is in ONE run and {lit} markers are yellow — every run the work has left is \
             keeping the colour it was painted with:\n{}",
        screen.join("\n")
    );
}

/// **THE REPLY MUST NOT BE DRAWN ABOVE THE QUEUED MESSAGE IT ANSWERS.**
///
/// The operator, twice — once on a real screen and once naming it exactly: *"a message was
/// queued to harnessd, delivered to model, reply started streaming above the queued message and
/// then some tick goes off and queued message dequeued and rendered rightfully above the reply.
/// pure ui desync."* And again today: *"when i see your response to my queued message just
/// before that message is dequeued."*
///
/// **The ordering the head must draw**, whatever tick it is on:
///
/// ```text
/// the operator's words          <- queued, bound to the row the daemon announced
/// the model's reply             <- streaming, or committed
/// ```
///
/// The window that makes it hard is the one between the daemon taking the prompt and the row's
/// body arriving: `TranscriptAppended` carries an id and a kind and no text, so the head draws
/// that row from its own echo (`App::bound_prompts`) and must NOT also draw the echo at the
/// tail. Two rows for one message, in two different places, is the desync.
#[test]
fn the_reply_is_never_drawn_above_the_queued_message_it_answers() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // Prose so the turn has a live pane with a sentence of its own.
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("s.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "Working on it:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    // The operator queues a message, which is what puts the echo on screen.
    typed(&mut a, "and what about the cache?");
    assert!(a.key(Key::Enter).is_some(), "the line was sent");
    // The prompt is in the hub now — a turn behind it — so the echo is the only place it exists.
    assert!(
        a.screen(100, 30).join("\n").contains("queued ·"),
        "the premise: the echo is drawn while the prompt is queued"
    );

    // **The step boundary: the daemon takes it and announces the user row.** No body yet — the
    // announcement carries an id and a kind and nothing else, which is the R2 window.
    a.apply(ServerFrame::Event(env(4, testing::appended("s.1", "user"))));

    // **And the model starts answering it.** The reply is a delta on the live pane.
    a.apply(ServerFrame::Event(env(
        5,
        testing::delta("t1", "the cache is keyed on bytes"),
    )));
    let screen = a.screen(100, 30);
    let text = screen.join("\n");

    // The words are on the screen ONCE, and they are the operator's row.
    assert_eq!(
        text.matches("and what about the cache?").count(),
        1,
        "the queued message is drawn twice — once as the row and once at the tail:\n{text}"
    );
    // **And the reply is BELOW them**, which is the operator's actual complaint.
    let words = screen
        .iter()
        .position(|l| l.contains("and what about the cache?"))
        .expect("the operator's row is on the screen");
    let reply = screen
        .iter()
        .position(|l| l.contains("the cache is keyed on bytes"))
        .unwrap_or_else(|| panic!("the reply is not on the screen:\n{text}"));
    assert!(
        reply > words,
        "THE REPLY IS ABOVE THE MESSAGE IT ANSWERS — row {reply} vs {words}:\n{text}"
    );
}

/// **The waiting words are the only row that moves when the daemon takes them** — R51 item 13's
/// third *must not differ*, as it now stands, and the clause most able to rot quietly.
///
/// An echo is words plus the air a landed row has, and a committed row is words plus the
/// separator's blank — so the two occupy the same rows and **no other row of the frame changes**
/// when the announcement arrives. Get it wrong — put the air before the echo, or let the wait
/// carry furniture of its own — and every row between the echo and the composer shifts at the
/// moment the operator's own message lands, which is exactly when they are looking at it.
///
/// **The words themselves DO move, by the ruling in `body_window`**: while nothing has taken
/// them they wait below the live pane, and the announcement is the daemon taking them, so they
/// go up into their row's own place. Both halves are asserted here — the move, and that it is
/// the only thing that moves — because either one alone is satisfied by a frame that is wrong.
#[test]
fn the_waiting_words_are_the_only_row_that_moves_when_the_daemon_takes_them() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // Some prose first, so the echo has committed rows above it and the frame is not the
    // trivial empty one.
    a_result_row(&mut a, 2, "s.0", "a settled row");
    typed(&mut a, "a queued line");
    a.key(Key::Enter);
    let before = a.screen(100, 30);
    assert!(
        before.iter().any(|l| l.contains("queued ·")),
        "the premise: the echo is drawn as a queued block"
    );
    // **Whose row to measure, and this is the whole of the measurement.** The live pane is
    // pinned to the bottom of the frame — measured first, it cannot move whatever the echo
    // does, so asserting on it proves nothing. What moves is the WORDS: with the air in front
    // of the echo, they sit one row lower than the row that replaces them, so every committed
    // row between them shifts when the announcement lands.
    let words_row = |screen: &[String]| {
        screen
            .iter()
            .position(|l| l.contains("a queued line"))
            .expect("the words are on the screen")
    };
    let was = words_row(&before);
    // The step boundary: the announcement, then the body. The transcript takes the words over.
    a.apply(ServerFrame::Event(env(3, testing::appended("u2", "user"))));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::TranscriptContent {
            item_id: "u2".into(),
            item: Box::new(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "a queued line".into(),
                }],
            }),
        },
    )));
    let after = a.screen(100, 30);
    assert!(
        !after.iter().any(|l| l.contains("queued ·")),
        "the echo stood down: {after:?}"
    );
    assert!(
        after.iter().any(|l| l.contains("a queued line")),
        "and the words are still on the screen, as a settled row: {after:?}"
    );
    // **Every other row, in order and in content.** Blank rows are dropped, and that is a
    // deliberate weakening: the wait's leading air and the landed row's separator are blanks in
    // different places, the subject here is whether a ROW moved, and a comparison that counted
    // blanks would be measuring the padding a tall screen adds.
    let others = |screen: &[String]| -> Vec<String> {
        screen
            .iter()
            .filter(|l| !l.trim().is_empty() && !l.contains("a queued line"))
            .cloned()
            .collect()
    };
    assert_eq!(
        others(&before),
        others(&after),
        "a row other than the waiting words moved when the daemon took them:\n\
             before: {before:?}\nafter: {after:?}"
    );
    // **And the move itself is the ruling**, so it is asserted rather than tolerated: the words
    // were below the pane and are now above it. A test that pinned the index would be pinning
    // the ordering the operator had removed.
    assert!(
        words_row(&after) < was,
        "the words did not move up when the daemon took them: row {was} → row {}",
        words_row(&after)
    );
}

/// **Only the CALLS number goes yellow, and only on the marker that carries the live work** —
/// R51 items 7 and 8, the two halves of one colour.
///
/// Two operator corrections are baked in here. The first: *"you should yellow only tool call
/// number, not the whole [] thing"* — so the thinking count and both brackets are asserted
/// PLAIN, not merely unasserted. The second: this colour has been wrong in three directions,
/// and the direction this test exists for is the third — a marker for a PREVIOUS turn's run
/// must not light up because the current turn has a call in flight.
#[test]
fn the_calls_count_goes_pending_and_nothing_else_in_the_marker_does() {
    let mut a = app();
    a.cfg.color = true;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // An earlier turn's settled work, hidden by the rung, with its own marker.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "an earlier turn:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    for i in 0..2 {
        let id = format!("s.{}", 1 + i);
        a.apply(ServerFrame::Event(env(
            3 + i * 2,
            SessionEvent::TranscriptAppended {
                item_id: id.clone(),
                kind: "tool_result".into(),
                ledger_head: String::new(),
            },
        )));
        a.apply(ServerFrame::Event(env(
            4 + i * 2,
            SessionEvent::TranscriptContent {
                item_id: id,
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: format!("old{i}"),
                    name: "bash".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "settled".into(),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    // The prose that ends that run, and then the CURRENT turn, whose work is in flight.
    a.apply(ServerFrame::Event(env(
        7,
        SessionEvent::TranscriptAppended {
            item_id: "s.9".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        8,
        SessionEvent::TranscriptContent {
            item_id: "s.9".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "that was the easy part".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(9, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        10,
        testing::proposed_on("t1", "c1", "bash", "\"cargo test\""),
    )));
    // **And the daemon STARTS it**, which is what makes it executing rather than merely
    // proposed. This is the distinction the colour turns on: a call the model has written and
    // the daemon has not begun is work the marker COUNTS and is not work that is happening, so
    // the number is there and the yellow is not.
    a.apply(ServerFrame::Event(env(
        11,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    a.apply(ServerFrame::Event(env(12, testing::turn_finished("t1"))));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();

    // The premise: one call EXECUTING, so the marker that carries it is the one to colour.
    assert!(
        marker_carries_live(a.live_work_now()),
        "the premise: a call is executing"
    );
    assert_eq!(
        a.live_work_now().calls,
        a.live_work_now().running,
        "and it is the only call, so the number and the colour agree here"
    );
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("\x1b[33m1\x1b[0m tool call"),
        "the live count is not pending: {screen:?}"
    );
    // **And the brackets are not.** The reversal of the operator's first cut, so a future
    // decision to colour the whole thing fails here rather than passing on a substring.
    assert!(
        !screen.contains("\x1b[33m["),
        "the bracket went yellow with the number: {screen:?}"
    );
    // **The earlier turn's marker is plain**, which is the third wrong direction: it is a
    // marker in the same walk, and it must not be lit by work that is not its own.
    let earlier = screen
        .lines()
        .find(|l| l.contains("[2 tool calls]"))
        .expect("the earlier turn's marker is on the screen");
    assert!(
        !earlier.contains("\x1b[33m"),
        "a settled marker went yellow while new work was in flight: {earlier:?}"
    );
}

/// **A marker whose work is a THOUGHT alone colours nothing** — the other half of item 7.
///
/// The styled thing is the CALLS count, so a marker carrying only thinking lines has nothing to
/// colour even while the model is plainly working. This is the case that would tempt a reader to
/// colour the brackets or the whole marker, and the operator ruled against both.
/// **A call that has FINISHED keeps its number and loses the yellow** — the stuck counter.
///
/// The operator: *"one of your yellow tool calls stuck at yellow."* leticl has the rule and its
/// words are the diagnosis: *"`:calls` is now the calls with no result row yet, which includes a
/// call that has FINISHED and whose row is a frame away. That one keeps the number (it is still
/// work the marker counts) and must not keep the colour — the yellow says EXECUTING, and nothing
/// is."*
///
/// **The two facts come apart in a one-frame window**, which is why this is a test about a
/// window rather than about a state: `ToolFinished` arrives, and the row that takes the call over
/// is announced and filled on later frames. In between, the count must hold — a number that is a
/// count of work done cannot go down — and the colour must go.
#[test]
fn a_finished_call_keeps_its_number_and_loses_the_yellow() {
    let mut a = app();
    // Asserted on the ESCAPES, so the palette has to be on — the words are the same either way
    // and the register is the whole of what is being tested.
    a.cfg.color = true;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("s.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "Running the tests:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        testing::proposed_on("t1", "c1", "bash", "\"cargo test\""),
    )));
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));

    // While it runs, the number is there and the colour is on.
    assert_eq!(a.live_work_now().calls, 1, "the premise: one call to count");
    assert!(
        marker_carries_live(a.live_work_now()),
        "the premise: executing"
    );
    let running = a.screen(100, 30).join("\n");
    assert!(
        running.contains("\x1b[33m1\x1b[0m tool call"),
        "the executing count is not pending:\n{running}"
    );

    // **It finishes, and the row has not landed.** The number holds; the colour goes.
    a.apply(ServerFrame::Event(env(
        6,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "d".into(),
            inline_bytes: 1,
            full_bytes: 1,
            spill: None,
            repairs: 0,
            edit: None,
        },
    )));
    let live = a.live_work_now();
    assert_eq!(live.calls, 1, "the number went down with the call's finish");
    assert_eq!(live.running, 0, "and nothing is executing");
    assert!(
        !marker_carries_live(live),
        "a finished call is still lighting the yellow — the stuck counter"
    );
    let settled = a.screen(100, 30).join("\n");
    assert!(
        !settled.contains("\x1b[33m1\x1b[0m tool call"),
        "the counter is still yellow with nothing executing:\n{settled}"
    );
    assert!(
        settled.contains("1 tool call"),
        "and the number left with the colour:\n{settled}"
    );
}

/// **A call whose result ROW this head holds is not executing, whatever the pane says.**
///
/// The operator, on a long round: *"yellow tool calls are not resolved unfortunately"* — a marker
/// whose digits stay pending on a turn whose calls have all finished and whose daemon has published
/// every row, and it never clears. The pane says `Running` and only a `ToolFinished` corrects it, so
/// a finish this head never received leaves one call pending **for the rest of the session**: no
/// later event clears it, `App::turn_busy` goes on saying `Responding` over it, and the daemon's own
/// view holds the same `running` because the finish it never published is the finish nobody heard.
///
/// **What this head does hold is the result row** — and a result row for a call is the call being
/// over. This is the half `live_work`'s `running` was missing: the NUMBER already counts only the
/// calls the transcript has not taken over, so the colour must be counted over those same calls or
/// it goes on colouring a number that has stopped counting them.
///
/// **The state is deliberately NOT fixed up here**, and the assertion in the middle says so: the
/// pane is the daemon's to correct. This is the colour reading the head's own record of the same
/// fact, not a head inventing a `ToolFinished` it never received.
#[test]
fn a_call_the_transcript_has_answered_is_not_drawn_executing() {
    const PENDING: &str = "\u{1b}[33m1\u{1b}[0m tool call";
    let mut a = app();
    a.cfg.color = true;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("s.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "Running the tests:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        testing::proposed_on("t1", "c1", "bash", "\"cargo test\""),
    )));
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    // The control, so the assertions below are about the colour leaving and not about a marker
    // that never drew: one call executing, and both registers say so.
    let running = a.screen(100, 30).join("\n");
    assert!(
        running.contains(PENDING),
        "the premise: a call that is executing is drawn pending:\n{running}"
    );

    // **The row the daemon appended when the call returned — and NO `ToolFinished`.** This is the
    // whole premise: the event this head never receives is the one thing that could have corrected
    // the pane, and the row is everything it has left to read.
    a.apply(ServerFrame::Event(env(
        6,
        testing::appended("s.1", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        7,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "test result: ok. 481 passed".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    // **The pane is still `Running`, and that stays true** — the state is the daemon's to correct
    // and this head has not been told. It is why these assertions cannot be passing because the
    // call was quietly finished off.
    assert!(
        matches!(
            a.turn.as_ref().expect("the turn").calls[0].state,
            CallState::Running
        ),
        "this test is not about a call whose state was corrected: {:?}",
        a.turn.as_ref().expect("the turn").calls[0].state
    );
    // And the transcript's own record of the same call: the row landed, so the call is answered.
    assert_eq!(
        a.turn.as_ref().expect("the turn").settled_calls,
        1,
        "the row that landed did not hand the call over to the transcript"
    );
    assert_eq!(
        a.live_work_now().running,
        0,
        "a call the transcript has answered is still counted as executing"
    );
    let settled = a.screen(100, 30).join("\n");
    assert!(
        !settled.contains(PENDING),
        "the transcript has answered this call and the marker is still pending:\n{settled}"
    );
    // **And the number did not go with the colour** — the operator's requirement is that the count
    // of work done never falls. The row claims the call and the marker counts the row.
    assert!(
        settled.contains("1 tool call"),
        "the number left with the colour:\n{settled}"
    );
}

/// **A settling diff card is handed over in ONE frame — it never leaves the screen.**
///
/// The operator, in a session of noop `edit`/`write` calls: *"periodic flicker while diff card
/// settles - even green caret appears briefly inside the diff card"*. Measured on this fixture,
/// the flicker was a **gap frame**: the live card was dropped from the pane when the result row
/// was *announced* (`TurnPane::settled_calls` advanced at `TranscriptAppended`), while the row
/// could not draw itself until its *body* landed (`TranscriptContent`) — a row with no body
/// renders zero rows, so for that frame the call was in **neither half**: the card's rows were
/// erased, the whole window re-derived around the hole, and the next frame drew the settled row
/// in its place. A terminal that does not composite the paint (no mode 2026 — byobu, older
/// tmux) shows the hardware caret wherever the last row-write ended, so the erase sweep put the
/// green block **inside the card's rows** for the duration of the write. In a loop of edit
/// calls that is the periodic flicker, once per settle.
///
/// The fix is the boundary this file already claims for the marker
/// (`live_work`: *"it advances when the row's BODY lands, not when the call finishes"* — and
/// leticl measured the same window one event earlier: *"for that window the call was in neither
/// half"*): the pane keeps the card until the row can replace it, so the handover is one frame
/// with the card on screen throughout.
///
/// The assertions, on every frame of the transition — the finished call, the announced row, the
/// landed body:
///
/// 1. **the card's diff is on screen** — no frame of a settling card is a frame without it;
/// 2. **the caret is never on one of the card's rows** — the composer's cursor is parked on the
///    composer, and a settling card must not move it (`App::screen`'s cursor clamp is the
///    hypothesis this test closes: measured, the clamp never engaged at any realistic size);
/// 3. **two renders of one state are the same frame** — the property
///    `two_renders_of_one_state_are_the_same_frame` keeps for a still transcript, held across
///    the transition too.
///
/// `on_the_tail_walk` is the same transition on a conversation too big to walk from the
/// beginning — the operator's session, resumed far over `SELF_WALK_LIMIT`, where every arriving
/// row invalidates the rendered history wholesale. The gap frame reproduced on both walks; the
/// tail one is here because that is the shape that was reported.
#[test]
fn a_settling_diff_card_never_leaves_the_screen() {
    settling_diff_card_never_leaves_the_screen(false);
}

/// The tail-walk half of the report — see the test above for what a frame is asserted on.
#[test]
fn a_settling_diff_card_never_leaves_the_screen_on_the_tail_walk() {
    settling_diff_card_never_leaves_the_screen(true);
}

fn settling_diff_card_never_leaves_the_screen(tail: bool) {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    if tail {
        // A conversation big enough that the tail walk cannot reach the beginning — the same
        // fixture the other tail tests use (`walk_limit = 1` is how they get into it without
        // building a megabyte), entered honestly: the conversation is walked once first, so
        // the assertion below can say the tail path was really taken.
        for i in 0..400u64 {
            let body = format!(
                "line {i} of the conversation\n\n```rust\nfn f{i}() {{ let n = {i}; }}\n```\n\nand some prose to wrap, with `code` in it.\n"
            );
            a.apply(ServerFrame::Event(env(
                i * 2 + 1,
                testing::appended(&format!("s.{i}"), "user"),
            )));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("s.{i}"), &body),
            )));
        }
        a.walk_limit = 1;
        let _ = a.screen(100, 24);
        assert!(
            a.hist_floor > 0,
            "this fixture never entered tail mode, so it cannot test the tail walk"
        );
    }
    // The round: prose, then an `edit` call that finished carrying both sides of a small
    // change — the card the operator watches settle. Seqs run on from the fixture above so
    // the head's own gap detector has nothing to say: a `log_gap` note is a row, and a row
    // this test did not put there is a frame this test is not about.
    let mut seq = if tail { 801 } else { 1 };
    let mut next = |e: SessionEvent| {
        let s = seq;
        seq += 1;
        env(s, e)
    };
    let turn_id = if tail { "t9" } else { "t1" };
    a.apply(ServerFrame::Event(next(testing::turn_started(turn_id))));
    let assistant_id = if tail { "s.900" } else { "s.0" };
    a.apply(ServerFrame::Event(next(testing::appended(
        assistant_id,
        "assistant",
    ))));
    a.apply(ServerFrame::Event(next(SessionEvent::TranscriptContent {
        item_id: assistant_id.into(),
        item: Box::new(TranscriptItem::Assistant {
            text: "Now I will edit the file.".into(),
            tool_calls: Vec::new(),
            truncated: false,
        }),
    })));
    a.apply(ServerFrame::Event(next(testing::proposed_on(
        turn_id, "c1", "edit", "a.rs",
    ))));
    a.apply(ServerFrame::Event(next(SessionEvent::ToolStarted {
        turn_id: turn_id.into(),
        call_id: "c1".into(),
        name: "edit".into(),
        access: Default::default(),
    })));
    let result_id = if tail { "s.901" } else { "s.1" };

    // Every frame of the settling, asserted on as the doc above says. The frames are taken
    // one event at a time because that is how the daemon's two events arrive: the
    // announcement and the body are separate frames on the wire, and a head that renders
    // between them is the head the operator was looking at.
    let mut frame = |a: &mut App, e: SessionEvent, label: &str| {
        a.apply(ServerFrame::Event(next(e)));
        let f = a.screen(100, 24);
        assert!(
            f.iter().any(|l| l.contains("@@ -1,1 +1,3")),
            "{label}: the diff card is not on the screen:\n{}",
            f.join("\n")
        );
        let card_rows: Vec<usize> = f
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains("Edited") || l.contains("@@") || l.contains("fn a()"))
            .map(|(i, _)| i)
            .collect();
        assert!(
            !card_rows.is_empty(),
            "{label}: no card rows found, so the caret assertion below would assert nothing"
        );
        if let Some((row, _)) = a.cursor() {
            assert!(
                !card_rows.contains(&row),
                "{label}: the caret is parked inside the diff card, at row {row} of {:?}",
                card_rows
            );
        }
        assert_eq!(
            a.screen(100, 24),
            f,
            "{label}: two renders of one state are different frames"
        );
        f
    };

    // 1. The call finished: the live card, diff and all.
    let finished = frame(
        &mut a,
        SessionEvent::ToolFinished {
            turn_id: turn_id.into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "fnv1a:1".into(),
            inline_bytes: 64,
            full_bytes: 64,
            spill: None,
            repairs: 0,
            edit: Some(edit_excerpt()),
        },
        "finished",
    );
    // 2. The result row announced, its body not yet landed. **This is the frame that
    //    flickered**: the card is the only thing on screen that knows the call, and the row
    //    cannot draw yet — so the card stays. And the frame is the SAME frame, not a frame
    //    with the card in a different place: a byte-identical frame is the one the painter
    //    writes zero bytes for, which is what "no flicker" is at the terminal.
    let announced = frame(
        &mut a,
        testing::appended(result_id, "tool_result"),
        "announced",
    );
    assert_eq!(
        announced, finished,
        "the announcement moved the frame, and a frame that moves is a frame that repaints"
    );
    // 3. The body lands: the settled row draws itself and the pane stands down — one frame,
    //    card on screen throughout.
    frame(
        &mut a,
        SessionEvent::TranscriptContent {
            item_id: result_id.into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "edit".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: String::new(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
        "landed",
    );
    // And the handover is complete: the pane no longer draws the call.
    assert_eq!(
        a.turn.as_ref().expect("the turn").settled_calls,
        1,
        "the landed row did not take the call over from the pane"
    );
}

/// **A call id reused in the next round does not inherit the last round's answer.**
///
/// The claim has to be read of THIS ROUND's rows, and this is the shape that punishes asking the
/// whole transcript. The harness assigns `call_0`, `call_1`, … to a model whose wire format
/// carries no call id (`crates/turn/src/items.rs`: *"GLM's wire format carries no call id, so the
/// harness assigns one. Positional and stable within the turn"*), and the head's own handover says
/// so: *"the ids repeat"* ([`TurnPane::settled_calls`]). So a rule that asks the transcript *has
/// any row ever carried this id* answers YES for a call that has just started in the round after
/// the last one's row landed — and the yellow, which is the only thing on the screen that says a
/// command is running, would go out exactly while it ran. That is a report this colour has already
/// had once: *"running tool is no longer yellow the counter, wtf why it regressed."*
///
/// The rows a claim may be read from are the ones [`TurnPane::appended`] names — this round's —
/// which `TurnStarted` empties for the next one.
#[test]
fn a_reused_call_id_does_not_inherit_the_last_rounds_answer() {
    /// A round's turn start, with the `began_ms` the emitter stamps once per prompt — which is
    /// what tells the head that a second round is the SAME turn and not a new one.
    fn round(turn_id: &str) -> SessionEvent {
        SessionEvent::TurnStarted {
            turn_id: turn_id.into(),
            model: "m".into(),
            ledger_head: String::new(),
            began_ms: Some(1_000),
        }
    }
    let mut a = app();
    a.cfg.color = true;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(ServerFrame::Event(env(1, round("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("s.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "First round:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    // Round 1's call, with the positional id an id-less wire format gets — and its row.
    a.apply(ServerFrame::Event(env(
        4,
        testing::proposed_on("t1", "call_0", "bash", "\"cargo test\""),
    )));
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "call_0".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        6,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "call_0".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "d".into(),
            inline_bytes: 1,
            full_bytes: 1,
            spill: None,
            repairs: 0,
            edit: None,
        },
    )));
    a.apply(ServerFrame::Event(env(
        7,
        testing::appended("s.1", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        8,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "call_0".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "test result: ok".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(9, testing::turn_finished("t1"))));

    // Round 2 of the SAME turn: the same id, and a call that is executing right now.
    a.apply(ServerFrame::Event(env(10, round("t2"))));
    a.apply(ServerFrame::Event(env(
        11,
        testing::proposed_on("t2", "call_0", "bash", "\"cargo test\""),
    )));
    a.apply(ServerFrame::Event(env(
        12,
        SessionEvent::ToolStarted {
            turn_id: "t2".into(),
            call_id: "call_0".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    let live = a.live_work_now();
    assert_eq!(live.calls, 1, "the premise: one call to count");
    assert_eq!(
        live.running, 1,
        "a call in the new round is executing, and round 1's row is not its answer"
    );
    assert!(
        marker_carries_live(live),
        "the executing count is not pending"
    );
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("\u{1b}[33m2\u{1b}[0m tool calls"),
        "a call executing in round 2 is drawn plain because round 1 used the same id:\n{screen}"
    );
}

/// **After the operator's own message the counts stand on their own line, with air.**
///
/// The operator: *"the [] thing comes right after my message if my message arrives your
/// mid turn. add an empty line between them"* — and then, reading it again: *"literally
/// just happened without mid turns."* They were right twice: the count was gluing to
/// **their** line, not the model's, and it did so on any turn where the model worked
/// without narrating first.
///
/// The rule is that the marker continues **the model's sentence** — the one the colon
/// points at. Anything else above it, their message included, and it stands as a line of
/// its own with the blank prose gets.
#[test]
fn the_counts_after_the_operators_own_message_stand_on_their_own_line() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // Their message, and then work — with no narration at all, which is the shape that
    // reproduced it.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "user".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: "make verbosity a config option".into(),
                }],
            }),
        },
    )));
    for i in 0..2u64 {
        let id = format!("s.{}", i + 1);
        a.apply(ServerFrame::Event(env(
            3 + i * 2,
            testing::appended(&id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            4 + i * 2,
            SessionEvent::TranscriptContent {
                item_id: id,
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: format!("c{i}"),
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
    a.invalidate_history();
    let screen = a.screen(120, 30);
    let lines: Vec<&str> = screen.iter().map(String::as_str).collect();
    let mine = lines
        .iter()
        .position(|l| l.contains("make verbosity a config option"))
        .expect("the message is on the screen");
    assert!(
        !lines[mine].contains("[2 tool calls]"),
        "the counts are glued to the operator's own line: {:?}",
        lines[mine]
    );
    assert!(
        lines[mine + 1].is_empty(),
        "no empty line between their message and the counts: {screen:?}"
    );
    assert!(
        lines[mine + 2].contains("[2 tool calls]"),
        "the counts are not on the line after the blank: {screen:?}"
    );
}

/// **And the model's own sentence still takes them glued** — the other half of the rule,
/// asserted beside it so the two cannot drift apart.
#[test]
fn the_counts_after_the_models_own_sentence_are_glued_to_it() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "first the helpers:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    // **Contiguous seqs.** `a_result_row` publishes three events, and a gap in the
    // numbering makes the head file a `log_gap` NOTE — which is a row, which is drawn
    // between the prose and the counts, which is a different scene from the one this test
    // is about. Found by debug print, after the assertion failed for that reason.
    a_result_row(&mut a, 3, "s.1", "one");
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let screen = a.screen(120, 30).join("\n");
    let line = screen
        .lines()
        .find(|l| l.contains("first the helpers:"))
        .expect("the prose is on the screen");
    assert!(
        line.trim_end().ends_with("helpers: [1 tool call]"),
        "the model's sentence did not take the counts: {line:?}"
    );
}

/// **A sentence that fills its line still carries the counts** — the operator's own
/// *"sometimes you do it same line - sometimes dont"*, reproduced.
///
/// The join was refused for want of room whenever the prose's last line ran to the frame's
/// edge, so the same transcript read two ways depending on the width and on where the line
/// broke. Measured on their screen: every joined marker sat on a short last line and every
/// lone one on a full one. The room is now reserved before the sentence is wrapped, so the
/// prose breaks a little earlier and the counts land on its last line.
///
/// **The prose is deliberately long enough to fill the line.** A short sentence would pass
/// whether the reservation worked or not — which is exactly how the first three versions
/// of this test passed while the bug was on the operator's screen.
#[test]
fn the_counts_land_on_a_sentence_that_fills_its_line() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let long = "the last piece of it, and the one where the arithmetic has to give, which \
                    is a sentence long enough to reach the edge of any frame it is read in and \
                    then some, and the part that matters is here:";
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: long.into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    for i in 0..3u64 {
        let id = format!("s.{}", i + 1);
        a.apply(ServerFrame::Event(env(
            3 + i * 2,
            testing::appended(&id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            4 + i * 2,
            SessionEvent::TranscriptContent {
                item_id: id,
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: format!("c{i}"),
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
    // **Three widths, because the defect was width-dependent.** A narrow frame and a wide
    // one take different paths through the wrap, and the operator's own terminal is wider
    // than either of the widths the earlier versions of this test used.
    for width in [80usize, 100, 210] {
        a.invalidate_history();
        let screen = a.screen(width, 30).join("\n");
        assert_eq!(
            markers(&screen),
            1,
            "no marker at all at {width} columns:\n{screen}"
        );
        let at = screen
            .lines()
            .position(|l| l.contains("[3 tool calls]"))
            .unwrap_or_else(|| panic!("the counts are missing at {width} columns:\n{screen}"));
        // **The marker is glued to the sentence when the sentence's last line has room, and
        // stands on its own line when it has none** — leticl's `%marker-onto-last-line`, whose
        // own docstring names this exact case: *"a last line with no room left at all is the
        // one case where the marker goes to its own line, and that is a line that was already
        // full of the sentence."*
        //
        // **What this test used to demand was the defect.** It required the counts to be on the
        // sentence's own line at every width, and the only way to satisfy that is to wrap the
        // sentence SHORT — reserve the marker's room from every line before knowing whether the
        // last one needed it. The operator measured the consequence on their own terminal at
        // 210 columns: *"there is no need to have the line break here because the whole tail
        // fits. you didnt try the 'tool calls' -> 'tools' -> 't' progressing. so I complain
        // about line wrapping here."*
        let on_the_sentence = screen.lines().any(|l| l.contains(": [3 tool calls]"));
        let sentence_full = screen.lines().any(|l| {
            l.trim_end().ends_with(':')
                && visible_width(l) + 1 + visible_width("[3 tool calls]") > width
        });
        assert!(
            on_the_sentence || sentence_full,
            "the counts are neither on the sentence that points at them nor on their own line \
                 because that sentence filled the frame, at {width} columns:\n{screen}"
        );
    }
}

/// **The marker steps DOWN its ladders rather than getting wider** — R51 item 7's neighbour,
/// and the operator's own ask: *"you didnt try the 'tool calls' -> 'tools' -> 't' progressing."*
///
/// Three properties, and each was a separate defect:
///
///  1. **The room is fixed for the frame.** It is a function of the width and nothing else, so
///     a count gaining a digit cannot re-wrap the sentence above it. leticl's
///     `hidden-run-marker-room`, and the operator's *"i dont like jumps"*.
///  2. **A marker too wide for the room loses WORDS, not layout.** `2 tool calls` → `2 tools`
///     → `2 calls` → `2t`, and the seam gives way before the counts reach their last rung.
///  3. **The seam goes before the counts do**, because the counts are the fact the line exists
///     to carry and the seam is the head talking about its own keys.
#[test]
fn the_marker_steps_down_its_ladders_instead_of_growing() {
    // (1) The room depends on the WIDTH alone — never on what the counts say.
    assert_eq!(marker_room(210), MARKER_ROOM_MAX);
    assert_eq!(marker_room(80), 40, "at most half the frame");
    assert_eq!(marker_room(40), 22, "and never below the floor");
    // **Below the floor the FRAME wins, and the docstring's "never less than the floor" is the
    // part that is imprecise** — leticl's own `(min cols …)` binds first, so a 20-column frame
    // gets a 20-column room and a 4-column frame gets 4. That is the right answer (a room wider
    // than the line it is on is not a room) and the arithmetic is what to keep, not the prose.
    assert_eq!(marker_room(20), 20);
    assert_eq!(marker_room(4), 4);

    // (2) The counts, at each rung, for a call count that forces the question.
    let at = |rung: usize| {
        let c = Counts::at_rung(11, 246, 0, rung);
        format!("{}{}", c.plain(), marker_seam_rung(true, 0))
    };
    assert_eq!(at(0), "[11 tool calls, 246 thinking lines]");
    assert_eq!(at(1), "[11 tools, 246 thinking]");
    assert_eq!(at(2), "[11 calls, 246 lines]");
    assert_eq!(at(3), "[11t, 246l]");
    // **And the seam is empty at every rung while `MARKER_SEAM` is off** — the ladder keeps
    // its slots so turning it back on cannot move a line, but nothing is drawn in them.
    assert_eq!(marker_seam_rung(true, 0), "");
    assert_eq!(marker_seam_rung(false, 0), "");

    // **And the whole thing steps down to fit a room**, which is the property and not the
    // spelling: every rung is narrower than the one before, and the last one always fits.
    let widths: Vec<usize> = (0..=3)
        .map(|r| visible_width(&Counts::at_rung(11, 246, 0, r).plain()))
        .collect();
    assert!(
        widths.windows(2).all(|w| w[0] > w[1]),
        "the rungs do not get narrower: {widths:?}"
    );
    assert!(
        widths[3] <= marker_room(80) - 1,
        "the last rung does not fit a narrow frame's room: {} > {}",
        widths[3],
        marker_room(80) - 1
    );

    // (3) The seam is spent BEFORE the counts reach their last rung — the ladder's shape.
    assert_eq!(
        MARKER_LADDER,
        [(0, 0), (1, 0), (2, 0), (3, 0), (3, 1), (3, 2)]
    );
    // …and a marker is BUILT through it, which is where the order matters: the ladder stops at
    // the first rung that fits, so a room that admits `[11t, 246l] · ctrl-t` keeps the short
    // chord rather than dropping it.
    // **With the seam off, a room that held back the counts now shows them in full.** At 26
    // columns the room is 25 and `[11 tools, 246 thinking]` is 24 — so the ladder stops at rung
    // 1, where with the seam it had to reach rung 3 to make room for ` · ctrl-t`. That is the
    // seam's cost, paid in the reader's words, and it is why removing it is worth more than a
    // tidy line.
    let short = Marker::new(11, 246, 0, true, false, 26);
    assert_eq!(short.counts.plain(), "[11 tools, 246 thinking]");
    assert_eq!(short.seam, "", "the seam is off by ruling");
    // A room too small even for that drops the seam ENTIRELY, and the counts stay: the counts
    // are the fact the line exists to carry, and the seam is the head talking about its own
    // keys — which is the whole of the ladder's order.
    let counts_only = Marker::new(11, 246, 0, true, false, 16);
    assert_eq!(counts_only.seam, "", "the seam is off by ruling");
    assert_eq!(counts_only.counts.plain(), "[11t, 246l]");
    // And a room with space keeps the whole thing spelled out.
    let roomy = Marker::new(11, 246, 0, true, false, 56);
    assert_eq!(roomy.seam, "", "the seam is off by ruling");
    assert_eq!(roomy.counts.plain(), "[11 tool calls, 246 thinking lines]");
}

/// **A run of one row is one count**, and the *"adaptivity"* that once made it say what
/// the row was is superseded by the operator's final shape: the marker is the counts and
/// nothing else, at every size. `[1 tool call]` — not `[Read src/app.rs]`, which was a
/// summary wearing a count's clothes.
#[test]
fn a_run_of_one_row_is_one_count() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "reading it now:".into(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "c0".into(),
                    name: "read".into(),
                    arguments: "{\"path\": \"crates/tui/src/app.rs\"}".into(),
                }],
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        3,
        testing::appended("s.1", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "c0".into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "CONTENTS".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let quiet = a.screen(110, 40).join("\n");
    assert!(
        quiet.contains("[1 tool call]"),
        "one row is one count, whatever size the run: {quiet}"
    );
    assert!(
        !quiet.contains("Read crates/tui/src/app.rs"),
        "the superseded summary half is not drawn even for one row: {quiet}"
    );

    // **And a single reasoning row keeps its count**, because the count is all there is to
    // say about it — the clause is dropped when a subject replaces it, not always.
    let mut b = app();
    b.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    b.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "reasoning".into(),
            ledger_head: String::new(),
        },
    )));
    b.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Reasoning {
                text: "one line".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            }),
        },
    )));
    b.visibility = Visibility::of(Profile::CONVERSATION);
    b.invalidate_history();
    let quiet = b.screen(110, 40).join("\n");
    assert!(
        quiet.contains("[1 thinking line]"),
        "a single reasoning row has only its count to report: {quiet}"
    );
}

/// **The chord is named only on the run it acts on**, and every other run names the verb
/// that does reach it — `ctrl-t` opens the newest run because that is the only one a head
/// with no cursor can address (R10's rule, one level up).
#[test]
fn the_chord_is_named_on_one_run_and_the_verb_on_the_others() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // Two runs, separated by an assistant row — which is kept, and is what makes them two
    // runs rather than one.
    a_result_row(&mut a, 1, "s.0", "first output");
    a.apply(ServerFrame::Event(env(
        20,
        SessionEvent::TranscriptAppended {
            item_id: "s.1".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        21,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "and now the second thing:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    a_result_row(&mut a, 30, "s.2", "second output");
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let quiet = a.screen(110, 40).join("\n");
    // **The seam is OFF** — the operator: *"dont print \" dot /verbosity\" or ctrl-t opens
    // it - we dont need that."* What this test used to check is therefore gone by ruling, and
    // what replaces it is the ruling itself: neither spelling is on the screen, on any run.
    assert!(
        !quiet.contains("ctrl-v opens it"),
        "the chord is still advertised on a marker: {quiet}"
    );
    assert!(
        !quiet.contains("/verbosity"),
        "the verb is still advertised on a marker: {quiet}"
    );
    // And the two runs are still two markers, told apart by their counts — which is what the
    // seam was standing in for.
    assert_eq!(markers(&quiet), 2, "the runs are not two markers: {quiet}");
}

/// **A marker that cannot be opened is the elision this document refuses everywhere else.**
/// `ctrl-t` opens the newest run — and opening it is **the rung lifted for its rows**, so
/// the reader gets the very rows the rung was hiding rather than a second rendering of them.
#[test]
fn the_marker_opens_and_the_run_draws_its_own_rows() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a_result_row(&mut a, 1, "s.0", "first output");
    a_result_row(&mut a, 20, "s.1", "second output");
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let closed = a.screen(110, 40).join("\n");
    assert!(closed.contains("[2 tool calls"), "{closed}");
    assert!(!closed.contains("first output"), "{closed}");

    // **The chord.** It opens the newest run — both rows here, since they are contiguous.
    a.key(Key::CtrlV);
    let open = a.screen(110, 40).join("\n");
    assert!(
        !open.contains("[2 tool calls"),
        "the marker is replaced by the run it stood for: {open}"
    );
    assert!(
        open.contains("second output"),
        "opening a run draws its rows: {open}"
    );
    // **And it draws them as ROWS** — the head's own headline for a settled tool result,
    // which is the same thing `/verbosity normal` gives, reached without leaving the rung
    // the reader chose.
    assert!(
        open.contains("Ran") && open.contains("second output"),
        "the rows are the head's own, with their headlines and payloads: {open}"
    );
    // The same key closes it, and the marker comes back.
    a.key(Key::CtrlV);
    let shut = a.screen(110, 40).join("\n");
    assert!(shut.contains("[2 tool calls"), "{shut}");
    assert!(!shut.contains("second output"), "{shut}");
}

/// **The rung is still a view, and the marker does not change that.** Switching back draws
/// every row again — and the marker is drawn at no other rung, because there is nothing
/// hidden for it to stand for.
#[test]
fn the_marker_exists_only_at_the_rung_that_hides_what_it_stands_for() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a_result_row(&mut a, 1, "s.0", "the payload");
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    assert!(a.screen(110, 40).join("\n").contains("[1 tool call]"));
    for rung in [Profile::TERSE, Profile::NORMAL, Profile::LOUD] {
        a.visibility = Visibility::of(rung);
        a.invalidate_history();
        let shown = a.screen(110, 40).join("\n");
        assert!(
            !shown.contains("[1 tool call]"),
            "a marker at {} would be a line about nothing hidden: {shown}",
            rung.name
        );
        assert!(
            shown.contains("Ran") && shown.contains("the payload"),
            "the row itself is drawn at {}: {shown}",
            rung.name
        );
    }
}

/// **Changing the rung closes what was open, because it was open in the other
/// rendering.** `payload_sel` names a payload window under every other rung and the run
/// `ctrl-t` opened under this one, so carrying the same id across the change put a window
/// on the screen that the reader had not asked for.
#[test]
fn changing_the_rung_closes_the_run_it_opened() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a_result_row(&mut a, 1, "s.0", "the payload");
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    a.key(Key::CtrlV);
    assert!(a.payload_sel.is_some(), "the run is open");
    assert!(a.screen(110, 40).join("\n").contains("the payload"));
    // Now leave the rung. The run is not a run any more, and the id must not be read as
    // a payload window on the row that wore it.
    assert_eq!(a.command("verbosity normal"), None);
    assert!(
        a.payload_sel.is_none(),
        "a run left open across a rung change came back as a payload window"
    );
    let shown = a.screen(110, 40).join("\n");
    assert!(!shown.contains("pages down"), "no window is open: {shown}");
}

/// **One turn's work is counted once, even while its own counts are still moving.**
///
/// The operator, watching a live turn: *"just saw = [1 tool call] [1 tool call] that later
/// merge to [2 tool calls]"*. Two markers for one turn, which then became one — so the reader
/// was shown a number that was wrong and then corrected, on the line whose whole job is to be
/// the fact.
///
/// **The mechanism is the design and the probe is the defect.** Only ONE marker may carry the
/// in-flight counts, and this file says so three times — `walk_carried_live` beside it, the
/// duplicate caught in an earlier tmux sample, and the two-marker screen. Every one of those
/// guards asks *has a run ALREADY carried it*, and that question is answered from the walk,
/// which folds the live work into a run only while that run is still the last thing in the
/// transcript (`end == items.len()`, or a run holding a row of this turn). The live pane's own
/// marker joins the prose the tail walk rendered, and it is drawn when no run carried the
/// counts — so the two will disagree for exactly as long as a round's rows are being committed
/// underneath: the walk's run stops reaching the end, the guard reads *not carried*, and the
/// pane draws its own copy beside the one already on the screen.
///
/// **And the state that actually splits them is a ROUND BOUNDARY.** The operator's screen:
///
/// ```text
///   ▌ their message
///                    ← blank
///   [2 tool calls]
///                    ← blank
///   [N thinking lines]
/// ```
///
/// — two markers, and their question is the right one: *"why not `[2 tool calls, N thinking
/// lines]` on a single row?"*
///
/// **Because round 2 starts with an empty `appended`.** A `TurnStarted` builds a fresh
/// `TurnPane`, so `appended` — *which rows this turn produced* — is emptied every round, while
/// the run on screen still holds the PREVIOUS round's two result rows. `live_here` asks
/// whether the run holds a row of `appended`, and the answer is now no: the run is the last
/// thing in the transcript, it is this turn's work, and the head has forgotten it. So the walk
/// draws `[2 tool calls]` with nothing folded in, `walk_carried_live` reads false for the same
/// reason, and the pane draws its own copy of the reasoning as `[N thinking lines]` — for one
/// turn.
/// **The thinking count moves as the thinking streams, not when the answer starts.**
///
/// The operator: *"thinking counter is not realtime. it updated and shown once your proper
/// reply lines appear."* A count of the working that only appears once the working is over is
/// the same defect as the yellow that only lit on settle — the row's whole job is to say the
/// turn is alive *now*.
///
/// So this pins the count against the deltas rather than against the end state: each reasoning
/// chunk that arrives must be able to move it, with no answer text anywhere in the fixture.
/// **The count baked into the rendered history moves with the stream** — the operator's
/// backfill, reproduced.
///
/// Their report: *"you started replying with `[1 tool call]`. then response line appeared and
/// then that `[1 tool call]` became `[1 tool call, 52 thinking lines]` — so unlike in leticl
/// thinking count is not live and backfilled after the first non-thinking line."*
///
/// **Backfilled is the word for it, and the cause is the cache.** The walk bakes its marker
/// into `hist_lines` — the cache of RENDERED rows — and that cache is only rebuilt from the row
/// something changed at. A reasoning delta changes no row, so every frame served the line with
/// the number it had been rendered with; what finally filled it in was a ROW landing, which
/// invalidated from there and re-rendered the marker.
///
/// So this drives exactly that: a turn whose work is inside a run of hidden rows, a frame
/// drawn, more reasoning, another frame — and the number on the row must have moved **without
/// any row arriving**.
/// **The yellow arrives when the call starts running, not when some row lands.**
///
/// The operator, right after the backfill: *"and btw - i didnt see yellow toolcalls for a
/// while. maybe the same problem"* — and it was. `marker_carries_live` is `live.running > 0`
/// while the counts are `live.calls` and `live.think_lines`, so a call that STARTS executing
/// changes what the marker paints without changing either number. The invalidation keyed on
/// the numbers alone, so the plain marker stayed in the cache and the yellow had nothing to
/// rebuild it — the same cached-row defect as the backfilled count, in the half that is a
/// colour rather than a digit.
///
/// The fixture draws a frame with the work PROPOSED (no yellow), then starts the call, and
/// asserts the colour on the next frame with no row in between.
#[test]
fn the_yellow_arrives_when_the_call_starts_not_when_a_row_lands() {
    let mut a = app();
    // **Colour on, or the assertions below test nothing** — `app()` is `Palette::None`, where
    // every register paints the same empty string. The sibling yellow test does the same.
    a.cfg.color = true;
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s.0", "assistant"),
    )));
    a.record_item(
        "s.0",
        TranscriptItem::Assistant {
            text: "let me check:".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    a.apply(ServerFrame::Event(env_at(
        2,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    // **A run of hidden rows of THIS turn**, so the WALK draws the marker and `live_here` is
    // true for it. The sibling test explains why the pane's own marker would make this pass
    // without the fix; the ordering here explains the other half — a row that landed before
    // the turn started is not the turn's row, and the walk's marker would then be plain for a
    // reason that has nothing to do with the cache.
    a.apply(ServerFrame::Event(env(
        3,
        testing::appended("s.1", "tool_result"),
    )));
    a.record_item(
        "s.1",
        TranscriptItem::ToolResult {
            call_id: "c0".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "SOMETHING LONG ENOUGH TO HIDE THE ROW".into(),
            edit: None,
            origin: None,
            media: None,
        },
    );
    // **Proposed, so the call is counted and not executing.** `marker_carries_live` asks for
    // `running`, and a proposal is not running.
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::ToolCallProposed {
            turn_id: "r1".into(),
            call_id: "c1".into(),
            name: "read".into(),
            args_digest: "d".into(),
            target: "crates/tui/src/app.rs".into(),
        },
    )));
    // **The marker's own digits, not the colour's mere presence** — the spinner and the
    // composer's edge are painted yellow too, so `contains("\u{1b}[33m")` passed on them and
    // said nothing about the count. This is the shape the sibling yellow test uses.
    //
    // **`2`, and the digit is the fixture's own arithmetic**: the run holds one hidden
    // result row (`s.1`, a call no pane ever proposed — the deposit shape) and the pane
    // holds one call in flight (`c1`), which are two calls and were counted as one while
    // the handover advanced on the announcement: `settled_calls` ran to 1 on an empty call
    // list and the next call of the round was claimed for a row it never answered — the
    // exact defect `round_answered`'s doc names (*"a counter that ran ahead would claim the
    // next call of the round for it"*). The handover is id-read now, so the landed row and
    // the live call are both counted, once each.
    const PENDING: &str = "\u{1b}[33m2\u{1b}[0m tool calls";
    let screen = a.screen(120, 30).join("\n");
    assert!(
        !screen.contains(PENDING),
        "a proposed call is not executing, so the count is not pending: {screen:?}"
    );
    // **And it starts.** One event, no row — the colour must be there on the next frame.
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::ToolStarted {
            turn_id: "r1".into(),
            call_id: "c1".into(),
            name: "read".into(),
            access: "read".into(),
        },
    )));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains(PENDING),
        "the call is executing and the count is not pending — the plain marker is still in \
             the cache: {screen:?}"
    );
}

#[test]
fn a_delta_changes_the_counts_and_nothing_else_in_the_rendered_history() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    for i in 0..8u64 {
        let (id, text) = (
            format!("s.{i}"),
            format!("settled row {i} of the conversation"),
        );
        a.apply(ServerFrame::Event(env_at(
            i + 1,
            1_000,
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
    a.apply(ServerFrame::Event(env_at(
        9,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    a.apply(ServerFrame::Event(env(
        10,
        SessionEvent::ToolCallProposed {
            turn_id: "r1".into(),
            call_id: "c1".into(),
            name: "read".into(),
            args_digest: "d".into(),
            target: "crates/tui/src/app.rs".into(),
        },
    )));
    let before = a.screen(100, 30);
    a.apply(ServerFrame::Event(env(
        11,
        SessionEvent::Delta {
            turn_id: "r1".into(),
            target: DeltaTarget::Reasoning,
            text: "x".repeat(300),
        },
    )));
    let after = a.screen(100, 30);

    assert_eq!(
        before.len(),
        after.len(),
        "the frame changed height on a delta: {} lines → {}",
        before.len(),
        after.len()
    );
    let differing: Vec<usize> = (0..before.len())
        .filter(|i| before[*i] != after[*i])
        .collect();
    assert_eq!(
        differing.len(),
        1,
        "a delta changed {} lines and the rule allows one — the marker's counts. \
             \nbefore:\n{}\nafter:\n{}",
        differing.len(),
        before.join("\n"),
        after.join("\n")
    );
    let at = differing[0];
    assert!(
        after[at].contains("tool call") && after[at].contains("thinking line"),
        "the line that changed is not the marker: {:?} → {:?}",
        before[at],
        after[at]
    );
}

#[test]
fn a_prompt_stops_claiming_to_be_queued_when_its_row_is_announced() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.session_id = "s".into();

    // 1. They send it, and the head echoes it locally. `submit` is the real door.
    a.set_composer("the retry budget needs a bump");
    let _ = a.key(Key::Enter);
    assert!(
        !a.pending_prompts.is_empty(),
        "the echo is not queued at all, so this test measures nothing"
    );
    let before = a.screen(100, 30).join("\n");
    assert!(
        before.contains("queued"),
        "the echo says it is queued: {before}"
    );

    // 2. **The daemon announces the row — no body yet**, which is the moment the model has the
    //    words and the transcript can draw them.
    a.apply(ServerFrame::Event(env(1, testing::appended("s.9", "user"))));
    let after = a.screen(100, 30).join("\n");
    assert!(
        after.contains("the retry budget needs a bump"),
        "the announced row is not drawn from the echo it was bound to: {after}"
    );
    assert!(
        !after.contains("queued"),
        "the prompt still claims to be queued after the model has been given it — which is \
             how the answer appears above a question the screen has not drawn: {after}"
    );
}

#[test]
fn the_marker_in_the_rendered_history_is_not_a_backfilled_count() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s.0", "assistant"),
    )));
    a.record_item(
        "s.0",
        TranscriptItem::Assistant {
            text: "let me check:".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    // A prop that needs no model: a proposed call, so `live.calls > 0` and a marker is drawn.
    a.apply(ServerFrame::Event(env_at(
        2,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::ToolCallProposed {
            turn_id: "r1".into(),
            call_id: "c1".into(),
            name: "read".into(),
            args_digest: "d".into(),
            target: "crates/tui/src/app.rs".into(),
        },
    )));
    // The count as a reader reads it, off the whole screen.
    let shown = |a: &mut App| -> usize {
        a.screen(100, 24)
            .join("\n")
            .lines()
            .find_map(|l| {
                let (_, rest) = l.split_once('[')?;
                let body = rest.split(']').next()?;
                body.split(", ").find_map(|p| {
                    p.trim()
                        .strip_suffix(" thinking lines")
                        .or_else(|| p.trim().strip_suffix(" thinking line"))
                        .and_then(|n| n.trim().parse().ok())
                })
            })
            .unwrap_or(0)
    };
    assert_eq!(shown(&mut a), 0, "nothing has streamed yet");
    // **Three frames, three chunks, and no row in between** — the number must climb on each.
    let mut last = 0;
    for i in 0..3 {
        a.apply(ServerFrame::Event(env(
            4 + i,
            SessionEvent::Delta {
                turn_id: "r1".into(),
                target: DeltaTarget::Reasoning,
                text: "x".repeat(400),
            },
        )));
        let now = shown(&mut a);
        assert!(
            now > last,
            "the count was not rebuilt on frame {i}: {last} → {now} — a count baked into \
                 the rendered history is a backfilled count"
        );
        last = now;
    }
}

#[test]
fn a_new_round_does_not_forget_the_rows_the_turn_already_produced() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // The prose that introduces the work — this is what the pane's marker joins.
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s.0", "assistant"),
    )));
    a.record_item(
        "s.0",
        TranscriptItem::Assistant {
            text: "let me check that for you:".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    // **ROUND 1.** The turn starts and proposes two calls; both run; both results land as
    // rows. `appended` now holds the narration and the two results — this turn's rows.
    let round = |id: &str| SessionEvent::TurnStarted {
        turn_id: id.into(),
        model: "qwen3-next-80b".into(),
        ledger_head: "0000".into(),
        // **The one field that says these rounds are one turn.** The shared
        // `testing::turn_started` leaves it `None`, which is *nobody measured it* — and with
        // `None` this test proved nothing: the reverted fix left it green, because the carry it
        // is meant to exercise is keyed on this.
        began_ms: Some(1_000),
    };
    a.apply(ServerFrame::Event(env_at(2, 1_000, round("r1"))));
    for (seq, id) in [(3u64, "s.1"), (4, "s.2")] {
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(id, "tool_result"),
        )));
        a.record_item(
            id,
            TranscriptItem::ToolResult {
                call_id: format!("c{seq}"),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "SOMETHING LONG ENOUGH TO HIDE THE ROW".into(),
                edit: None,
                origin: None,
                media: None,
            },
        );
    }

    // **ROUND 2, same turn.** The daemon stamps every round of one prompt with the same
    // `began_ms`, which is the only thing on the wire that says these two `TurnStarted`s are
    // one turn — every other field, `turn_id` included, is per ROUND.
    a.apply(ServerFrame::Event(env_at(5, 1_000, round("r2"))));
    // And this round's work so far is thinking, with no result row yet: the pane's own count.
    a.apply(ServerFrame::Event(env(
        6,
        SessionEvent::Delta {
            turn_id: "r2".into(),
            target: DeltaTarget::Reasoning,
            text: "the first call told me it is in the reader, so let me check the caller\n\
                       and the place it is constructed before I touch anything at all"
                .into(),
        },
    )));

    let screen = a.screen(100, 30).join("\n");
    // **Counted as MARKERS, not as lines.** The first version of this test counted lines that
    // contained a count, and it passed against the reverted fix — because the two markers were
    // drawn *on one line*: `let me check that for you: [2 tool calls] [2 thinking lines]`. One
    // line, two markers, and a line counter cannot tell that from one. What the operator has
    // seen both ways is the pair, so the discriminator is the pair's own shape.
    assert!(
        screen.contains("2 tool calls, 2 thinking lines"),
        "one turn's work is ONE marker carrying both halves — the operator's own question: \
             *why not [2 tool calls, N thinking lines] on a single row?* — got: {screen}"
    );
    assert!(
        !screen.contains("] ["),
        "and not two markers side by side, which is the pair they caught as `[1 tool call] \
             [1 tool call]`: {screen}"
    );
}

/// **A boundary the head has ALREADY HAD, taken again, does not open a second run** — and does
/// not throw the round's own stream away.
///
/// The same `turn_id` is the same round: `run_turn_steered` mints `{transcript}#{turn_seq}` and
/// a restored session starts that counter at the item count so the ids never repeat within one
/// transcript. So a second copy of one boundary is a duplicate event, and the head's answer to
/// a duplicate has to be *nothing* — the round's text, its thinking and its proposed calls are
/// state the event does not describe and cannot restore, so rebuilding the pane here is how a
/// count of work done goes DOWN.
#[test]
fn a_boundary_the_head_has_already_had_does_not_open_a_second_run() {
    let mut a = app();
    a_turn_mid_run(&mut a);
    a.clock(5_000);
    // The same round again: same `turn_id`, same stamp, one frame later.
    a.apply(ServerFrame::Event(env_at(
        5,
        5_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    let after = a.screen(100, 30).join("\n");
    assert_eq!(markers(&after), 1, "one run, one marker: {after}");
    assert!(
        after.contains("1 tool call, 2 thinking lines"),
        "the round's own stream survives a second copy of its boundary: {after}"
    );
    assert!(
        a.turn_status(120).contains("4.0s"),
        "and its clock is not restarted: {}",
        a.turn_status(120)
    );
}

/// **A prompt that IS new still opens a run** — the control for the two above, so the fix
/// cannot be *never split*.
///
/// What makes it new is two facts and both are on the screen: it carries a `began_ms` of its
/// own, and the operator's message stands between it and the run before it — a drawn row, which
/// ends a run whatever any `TurnStarted` says. So the old run's marker stays where it is, the
/// new turn's work is its own marker, and the clock is the new prompt's.
#[test]
fn a_prompt_that_is_actually_new_still_opens_a_run() {
    let mut a = app();
    a_turn_mid_run(&mut a);
    // The operator's own message: a row the reader can see, and therefore the border.
    a.apply(ServerFrame::Event(env(5, testing::appended("s.2", "user"))));
    a.record_item(
        "s.2",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "and now the other one".into(),
            }],
        },
    );
    a.apply(ServerFrame::Event(env_at(
        6,
        20_000,
        SessionEvent::TurnStarted {
            turn_id: "r2".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(20_000),
        },
    )));
    a.apply(ServerFrame::Event(env(
        7,
        testing::appended("s.3", "tool_result"),
    )));
    a.record_item(
        "s.3",
        TranscriptItem::ToolResult {
            call_id: "c2".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "SOMETHING LONG ENOUGH TO HIDE THE ROW".into(),
            edit: None,
            origin: None,
            media: None,
        },
    );
    a.apply(ServerFrame::Event(env(
        8,
        SessionEvent::Delta {
            turn_id: "r2".into(),
            target: DeltaTarget::Reasoning,
            text: "the second question is about the caller, so let me look at it here\n\
                       and then at the place the value is handed over"
                .into(),
        },
    )));
    a.clock(22_000);
    let screen = a.screen(100, 30).join("\n");
    assert_eq!(
        markers(&screen),
        2,
        "the finished run and the new one, and nothing else: {screen}"
    );
    assert!(
        screen.contains("1 tool call, 2 thinking lines"),
        "the new run's marker carries its own work: {screen}"
    );
    let status = a.turn_status(120);
    assert!(
        status.contains("2.0s"),
        "and the clock is the NEW prompt's, not the one before it: {status}"
    );
}

#[test]
fn a_run_of_rows_no_count_describes_says_what_it_is_rather_than_nothing() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    // A system row, which the rung hides and neither count can describe.
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s.0", "system"),
    )));
    a.record_item(
        "s.0",
        TranscriptItem::System {
            text: "the model was reconfigured".into(),
            origin: letibot_transcript::SystemOrigin::Update,
        },
    );
    let screen = a.screen(100, 24).join("\n");
    assert!(
        !screen.contains("[]"),
        "an empty marker is not a marker: {screen}"
    );
    assert!(
        screen.contains("[1 head event]"),
        "the run says what it is: {screen}"
    );

    // **And it steps down the ladder like the other clauses**, so a narrow frame does not get
    // a marker wider than its room.
    let mut narrow = app();
    narrow.cfg.width = 40;
    narrow.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    narrow.visibility = Visibility::of(Profile::CONVERSATION);
    narrow.apply(ServerFrame::Event(env(
        1,
        testing::appended("s.0", "system"),
    )));
    narrow.record_item(
        "s.0",
        TranscriptItem::System {
            text: "the model was reconfigured".into(),
            origin: letibot_transcript::SystemOrigin::Update,
        },
    );
    let narrow_screen = narrow.screen(40, 24).join("\n");
    assert!(
        !narrow_screen.contains("[]"),
        "and still not empty when the room is tight: {narrow_screen}"
    );

    // **The fallback is a branch, not a third clause.** A run of calls describes itself, so
    // the system row beside it is not mentioned — leticl draws `parts`, or the fallback.
    let mut with_calls = app();
    with_calls.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    with_calls.visibility = Visibility::of(Profile::CONVERSATION);
    for (seq, id, item) in [
        (
            1u64,
            "s.0",
            TranscriptItem::System {
                text: "the model was reconfigured".into(),
                origin: letibot_transcript::SystemOrigin::Update,
            },
        ),
        (
            2,
            "s.1",
            TranscriptItem::ToolResult {
                call_id: "c0".into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "SOMETHING LONG ENOUGH TO HIDE".into(),
                edit: None,
                origin: None,
                media: None,
            },
        ),
    ] {
        let kind = match &item {
            TranscriptItem::System { .. } => "system",
            _ => "tool_result",
        };
        with_calls.apply(ServerFrame::Event(env(seq, testing::appended(id, kind))));
        with_calls.record_item(id, item);
    }
    let screen = with_calls.screen(100, 24).join("\n");
    assert!(
        screen.contains("1 tool call"),
        "a run of calls describes itself: {screen}"
    );
    assert!(
        !screen.contains("head event"),
        "and does not mention the row beside it: {screen}"
    );
}

#[test]
fn the_marker_reads_as_one_paragraph() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    for (seq, item_id, kind, item) in [
        (
            1u64,
            "s.0",
            "assistant",
            TranscriptItem::Assistant {
                text: "let me look at the four places this has to give:".into(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "c0".into(),
                    name: "read".into(),
                    arguments: "{\"path\": \"crates/tui/src/app.rs\"}".into(),
                }],
                truncated: false,
            },
        ),
        (
            3,
            "s.1",
            "tool_result",
            TranscriptItem::ToolResult {
                call_id: "c0".into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "SOMETHING LONG ENOUGH TO HIDE".into(),
                edit: None,
                origin: None,
                media: None,
            },
        ),
        (
            5,
            "s.2",
            "assistant",
            TranscriptItem::Assistant {
                text: "and that is what it says.".into(),
                tool_calls: Vec::new(),
                truncated: false,
            },
        ),
    ] {
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptAppended {
                item_id: item_id.into(),
                kind: kind.into(),
                ledger_head: String::new(),
            },
        )));
        a.apply(ServerFrame::Event(env(
            seq + 1,
            SessionEvent::TranscriptContent {
                item_id: item_id.into(),
                item: Box::new(item),
            },
        )));
    }
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let screen = a.screen(100, 30);
    eprintln!("\n{}", screen.join("\n"));
    let lines: Vec<&str> = screen.iter().map(String::as_str).collect();
    let at = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("`{needle}` is not on the screen:\n{}", screen.join("\n")))
    };
    let prose = at("this has to give:");
    let report = at("and that is what it says.");
    // **The marker is ON the sentence's own line**, which is the operator's own exemplar
    // — `…has to give: [11 tool calls, 246 thinking lines]`. It is punctuation inside a
    // sentence, so there is no second line to find and nothing that could be mistaken for
    // a row: `at` would panic if `[1 tool call]` were anywhere else, and this says where
    // the sentence has to end.
    assert!(
        lines[prose]
            .trim_end()
            .ends_with("this has to give: [1 tool call]"),
        "the counts are not the end of the sentence that points at the work: {:?}",
        lines[prose]
    );
    // **And the report is the paragraph after it**, with the air prose gets — the marker
    // is the model's own working and this is the model speaking again. That air is also
    // the economy: the conclusion is a neighbour the marker never has to state.
    assert_eq!(
        report,
        prose + 2,
        "the report is not the next paragraph:\n{}",
        screen.join("\n")
    );
    assert!(
        lines[prose + 1].is_empty(),
        "paragraphs of prose are separated by a blank line: {lines:?}"
    );
}

/// **R37's own invariant, still true: a warning is not the working.** The amendment adds a
/// line back to the screen; it must not have let anything else back in or out.
#[test]
fn the_marker_does_not_hide_a_warning() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.note(Note::Warned(Warned {
        code: "gate_timeout".into(),
        detail: "nobody answered the ask".into(),
        ts: 3,
    }));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let quiet = a.screen(110, 40).join("\n");
    assert!(
        quiet.contains("nobody answered the ask"),
        "a warning is not the working and the rung does not hide it: {quiet}"
    );
}

#[test]
fn the_verbosity_card_shows_every_rung_with_its_meaning_and_marks_the_current_one() {
    let mut a = app();
    assert_eq!(a.visibility.profile(), Some(Profile::NORMAL));
    // Bare: the card, and nothing sent anywhere — a rung is a local setting.
    assert_eq!(a.command("verbosity"), None);
    assert!(a.pick == Some(Pick::Verbosity));
    // **The cursor's row read off the table rather than written down.** This was `2`, from when
    // the card listed four profiles and `normal` was the third — and that hand-written list is
    // what the card no longer keeps: `read-edits` sits after `conversation`, so every later rung
    // moved a row down, and a literal here has to be re-counted by hand each time one is added.
    let normal_at = Profile::ALL
        .iter()
        .position(|p| *p == Profile::NORMAL)
        .expect("`normal` is a profile");
    assert_eq!(
        a.mode_sel, normal_at,
        "the cursor starts on the rung in force"
    );
    let screen = a.screen(120, 40).join("\n");
    for p in Profile::ALL {
        assert!(
            screen.contains(p.name),
            "`{}` is not on the card:\n{screen}",
            p.name
        );
        // The meaning, as a prefix of the sentence — long enough to be the sentence and
        // short enough that the card's wrap cannot have split it.
        let lead: String = p.why.chars().take(30).collect();
        assert!(
            screen.contains(&lead),
            "the card does not say what `{}` gives you:\n{screen}",
            p.name
        );
    }
    let row = screen
        .lines()
        .find(|l| l.contains("normal") && l.contains('▸'))
        .expect("the rung in force is on the card");
    assert!(
        row.contains("← now"),
        "the card says which rung is live: {row}"
    );
    // **The fact that surprises people**, in the card and not only in a notice: the ladder
    // applies to the whole transcript, already drawn.
    assert!(
        screen.contains("the WHOLE transcript, already drawn"),
        "{screen}"
    );
}

/// The card's `Enter` and the typed verb are the same act — one function behind both, so a
/// rung taken with the arrow keys and a rung typed cannot disagree about anything, down to
/// the sentence the reader is left with.
#[test]
fn taking_a_rung_from_the_card_is_the_same_act_as_typing_it() {
    let mut card = app();
    assert_eq!(card.command("verbosity"), None);
    card.key(Key::Up); // normal -> terse
    // One row up the card's own order, which is `Profile::ALL`'s — the literal `1` was that
    // order as it stood with four profiles in it.
    let terse_at = Profile::ALL
        .iter()
        .position(|p| *p == Profile::TERSE)
        .expect("`terse` is a profile");
    assert_eq!(card.mode_sel, terse_at);
    assert_eq!(card.key(Key::Enter), None);
    assert!(card.pick.is_none(), "taking a value closes the card");

    let mut typed_app = app();
    assert_eq!(typed_app.command("verbosity terse"), None);

    assert_eq!(card.visibility.profile(), Some(Profile::TERSE));
    assert_eq!(typed_app.visibility.profile(), card.visibility.profile());
    assert_eq!(card.notice, typed_app.notice);
}

/// **`/v` is the one spelling that still cycles**, and it keeps doing so across R38.
///
/// It is an alias in `HEAD_COMMAND_ALIASES` — taken, deliberately not offered by tab — and
/// the old `"verbosity" | "v"` arm folded both spellings into one cycle. R38 split the long
/// spelling into card-bare / rung-named, and `strip_prefix("verbosity")` does not match `v`,
/// so the alias fell through to the daemon until it got its own arm. One key's worth of
/// cycling is a promise this head made; what R38 rules out is having to cycle to *learn* the
/// values, and the card is where they are read.
#[test]
fn the_v_alias_still_cycles_the_ladder() {
    let mut a = app();
    assert_eq!(a.visibility.profile(), Some(Profile::NORMAL));
    assert_eq!(
        a.command("v"),
        None,
        "a rung is a local setting: nothing is sent"
    );
    assert_eq!(
        a.visibility.profile(),
        Some(Profile::LOUD),
        "`v` is the next rung"
    );
    assert!(
        a.pick.is_none(),
        "`v` cycles in place; it does not open the card"
    );
    // The same function as the card and the long spelling, so the three cannot disagree.
    let mut named = app();
    assert_eq!(named.command("verbosity loud"), None);
    assert_eq!(named.visibility.profile(), a.visibility.profile());
    assert_eq!(named.notice, a.notice);
}

/// **The operator's own example: `read-edits` draws the edits, and `conversation` does not.**
///
/// Their words, and the reason the ladder was abandoned: *"for example leticl has read-edits
/// verbosity levels when all is hidden except edits"*. A ladder could not be told this — *"all
/// is hidden except edits"* is not a rung between two others — so the test is the pair: the
/// same turn's screen at `read-edits` carries the diff, and at `conversation` the diff is gone
/// while the conversation stays. That pair is what proves the edit SWITCH is what draws it,
/// rather than one of the ladder's rungs happening to.
#[test]
fn read_edits_draws_the_edits_and_conversation_does_not() {
    let screen_for = |vis: Visibility| {
        let mut a = app();
        a.visibility = vis;
        // The narration, so there is a conversation for the switch NOT to touch.
        // **A turn first**: a call lives in a `TurnPane`, and one without a `TurnStarted`
        // is not in `t.calls` at all — the state this test was written blind to, and found
        // by printing them (`calls=[]` while `shows(Edits)` was already true).
        a.apply(ServerFrame::Event(env_at(
            0,
            1_000,
            testing::turn_started("t1"),
        )));
        a.apply(ServerFrame::Event(env(
            1,
            testing::appended("s.0", "assistant"),
        )));
        a.record_item(
            "s.0",
            TranscriptItem::Assistant {
                text: "let me change it:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            },
        );
        // One call that changed a file — the card the operator's rung exists for.
        a.apply(ServerFrame::Event(env(
            2,
            testing::proposed("t1", "c1", "edit"),
        )));
        a.apply(ServerFrame::Event(env(
            3,
            SessionEvent::ToolFinished {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload_digest: "fnv1a:1".into(),
                inline_bytes: 64,
                full_bytes: 64,
                spill: None,
                repairs: 0,
                edit: Some(edit_excerpt()),
            },
        )));
        a.screen(120, 30).join("\n")
    };
    let read_edits = screen_for(Visibility::of(Profile::READ_EDITS));
    assert!(
        read_edits.contains("1 - fn a() {}"),
        "`read-edits` hid the edit card it exists for:\n{read_edits}"
    );
    assert!(
        read_edits.contains("let me change it:"),
        "and it is still a conversation:\n{read_edits}"
    );
    let conversation = screen_for(Visibility::of(Profile::CONVERSATION));
    assert!(
        !conversation.contains("1 - fn a() {}"),
        "`conversation` drew an edit card — the empty set is not empty:\n{conversation}"
    );
    assert!(
        conversation.contains("let me change it:"),
        "the conversation itself must survive the emptiest profile:\n{conversation}"
    );
}

/// **A composer line that could not be completed pays no row at all.**
///
/// The slot is not a permanent row: it exists while the composer holds a `!` or `/` line,
/// and not otherwise. So an ordinary frame is the frame it was before this existed — the
/// conversation keeps that row, and the transcript's own last line sits one row lower than
/// it sits under a `!` line, which is where every frame drew it. An unconditional
/// reservation would be a row of screen taken from the transcript for a hint nobody is
/// going to be offered.
#[test]
fn a_composer_line_that_could_not_be_completed_pays_no_row() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    shell_row(&mut a, 1, "u1", "user", operator_row("! cargo test 199"));
    for i in 0..30u64 {
        // Contiguous sequence numbers: a gap here is a resync notice in the middle of
        // the transcript, and this fixture wants the transcript and nothing else.
        let (seq, id, text) = (
            i + 2,
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
    let composer_at = |f: &[String]| {
        f.iter()
            .position(|l| l.contains('╭'))
            .expect("the composer's top edge")
    };
    let last_row_at = |f: &[String]| {
        f.iter()
            .position(|l| l.contains("row 29 of the transcript"))
            .expect("the newest line of the transcript")
    };
    let conversation = |f: &[String]| {
        f.iter()
            .filter(|l| l.contains(" of the transcript"))
            .count()
    };

    // Three frames of one session and one transcript: an EMPTY composer — the frame this
    // head drew before any of this existed — an ordinary line, and the same line with the
    // `!` that makes it completable.
    let empty = a.screen(100, 24);
    typed(&mut a, "cargo test 199");
    let plain = a.screen(100, 24);
    a.set_composer("! cargo test 199");
    let bang = a.screen(100, 24);

    assert_eq!(
        composer_at(&plain),
        composer_at(&empty),
        "an ordinary line did not move the composer:\n{}",
        plain.join("\n")
    );
    assert_eq!(
        last_row_at(&plain),
        last_row_at(&empty),
        "**and it did not take a row from the conversation** — an ordinary line is the frame \
             this head always drew:\nplain:\n{}\nempty:\n{}",
        plain.join("\n"),
        empty.join("\n")
    );
    assert_eq!(conversation(&plain), conversation(&empty));

    assert_eq!(
        last_row_at(&bang),
        last_row_at(&plain),
        "**a completable line pays no row either**: its suggestions are in the composer's \
             bottom edge, so the conversation keeps every row it had:\n\
             bang:\n{}\nplain:\n{}",
        bang.join("\n"),
        plain.join("\n")
    );
    assert_eq!(
        composer_at(&bang),
        composer_at(&plain),
        "{}",
        bang.join("\n")
    );
}

/// **Every code the tree says belongs on the edge has a register in this head.**
///
/// `warning::ALARM_ONLY` moves a code off the conversation; this head has to have
/// somewhere for it to land, or the diagnostic is drawn nowhere at all. The two halves
/// live in different crates and cannot be checked by the compiler, so the check is a test
/// — and it is written to fail when somebody adds a row to the classification without
/// wiring it here, which is the only moment the omission is cheap to fix.
#[test]
fn every_edge_bound_code_has_a_counter_in_this_head() {
    for code in letibot_sessionlog::warning::ALARM_ONLY {
        let mut a = App::new(plain_cfg(80));
        assert!(
            a.count_edge_note(code),
            "`{code}` is classified edge-bound and this head has no counter for it, so \
                 the diagnostic would be drawn nowhere"
        );
    }
    // And the register it moved is the one the alarm reads: a counter outside
    // `Counters` would be a number nobody is pointed at.
    let mut a = App::new(plain_cfg(80));
    a.count_edge_note("model_slow_first_byte");
    assert!(
        a.alarmed(),
        "counting an edge-bound note must raise the ⚠ — the mark is the whole reason it \
             is not a row in the conversation"
    );
    // **And a code this head has no register for is REFUSED, not counted into a
    // neighbour's cell.** The false half is the one the arm acts on: it is what makes
    // `alarm_only_unregistered` a sentence rather than a silent deletion.
    assert!(
        !a.count_edge_note("something_this_head_has_never_heard_of"),
        "an unknown code must not be counted into a counter that means something else"
    );
}

/// **THE JOBS PANE GROUPS FINISHED, LIKE THE SUBAGENTS PANE** — the operator's own row:
/// *"jobs panel - same as subagents - show list of running, group finished"*.
///
/// Running first, one folded `finished (N)` row, and Enter on that row unfolds it. The
/// reason it matters is the subagents pane's: a settled row the reader has stopped caring
/// about must not push a running one off the bottom of the screen.
#[test]
fn the_jobs_pane_shows_the_running_ones_first_and_folds_the_finished_ones() {
    let mut a = App::new(plain_cfg(100));
    a.session_id = "s1".into();
    a.apply(jobs_frame(
        "s1",
        vec![
            daemon_job("j1", "cargo build", false),
            daemon_job("j2", "cargo test", true),
            daemon_job("j3", "sleep 9", false),
        ],
    ));
    a.jobs_pane = true;
    let frame = a.jobs_lines(100);
    let joined = frame.join("\n");
    assert!(joined.contains("finished (2)"), "{joined}");
    // **The running one is ABOVE the group row**, which is the whole point of the fold.
    let running_at = frame
        .iter()
        .position(|l| l.contains("cargo test"))
        .expect("the running job's row");
    let fold_at = frame
        .iter()
        .position(|l| l.contains("finished (2)"))
        .expect("the group row");
    assert!(
        running_at < fold_at,
        "the running job is below the fold:\n{joined}"
    );
    assert!(
        !joined.contains("cargo build"),
        "a settled job is drawn though its group is folded:\n{joined}"
    );
    // **Enter on the group row unfolds it** — one running stop, then the fold.
    a.key(Key::Down);
    assert_eq!(a.jobs_sel, 1, "the cursor is not on the group row");
    assert_eq!(
        a.key(Key::Enter),
        None,
        "the group row is a fold, not a read"
    );
    let joined = a.jobs_lines(100).join("\n");
    assert!(joined.contains("cargo build"), "{joined}");
    assert!(joined.contains("sleep 9"), "{joined}");
}
