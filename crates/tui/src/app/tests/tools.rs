//! Tool calls and results: cards, diffs, payloads, outcomes.

use super::*;

/// **The control: a model's row is unmarked, and its paint has not moved.**
///
/// Beside the test above, this pair is what says the mark reads `origin` rather than
/// appearing on tool rows in general — `origin: None` is a call the MODEL proposed, and it
/// is also every row written before the field existed.
///
/// **The header is pinned literally**, because *a model-origin row is unchanged* is a claim
/// about bytes and there is no other way to hold one: a change that moves the operator's row
/// is expected to leave this one exactly as it is, and a change that moves this one is the
/// defect this test exists to catch.
#[test]
fn a_models_tool_row_is_unmarked_and_its_paint_has_not_moved() {
    let body: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let theirs = item_rows(
        true,
        TranscriptItem::ToolResult {
            call_id: "bang-1".into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: body,
            edit: None,
            origin: None,
            media: None,
        },
    );
    assert!(
        !theirs[0].contains('▌'),
        "a call the MODEL proposed is not the person's act: {:?}",
        theirs[0]
    );
    assert_eq!(
        theirs[0],
        "  \u{1b}[2m▾\u{1b}[0m\u{1b}[2m Ran \u{1b}[0m(bang-1)\u{1b}[2m · ok\u{1b}[0m\
             \u{1b}[2m\u{1b}[0m\u{1b}[1m · 60 lines\u{1b}[0m",
        "the model's header is the one it has always been"
    );
}

#[test]
fn a_running_tool_call_shows_how_long_it_has_been_running_and_what_it_last_said() {
    // All three were derivable from events the head was already reading;
    // `Envelope::ts` is on every one of them and `ToolProgress { note }` was
    // being dropped on the floor.
    let mut a = app();
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    a.apply(ServerFrame::Event(env_at(
        2,
        1_000,
        testing::proposed("t1", "c1", "bash"),
    )));
    a.apply(ServerFrame::Event(env_at(
        3,
        2_000,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    a.apply(ServerFrame::Event(env_at(
        4,
        6_200,
        SessionEvent::ToolProgress {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            note: "compiling letibot-tui".into(),
        },
    )));
    let screen = a.screen(120, 24).join("\n");
    // The tense says "wait" without a colour or a glyph.
    assert!(screen.contains("Running"), "{screen}");
    assert!(screen.contains("4.2s"), "{screen}");
    assert!(screen.contains("compiling letibot-tui"), "{screen}");

    a.apply(ServerFrame::Event(env_at(
        5,
        9_500,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "fnv1a:1".into(),
            inline_bytes: 214,
            full_bytes: 214,
            spill: None,
            repairs: 0,
            edit: None,
        },
    )));
    let screen = a.screen(120, 24).join("\n");
    assert!(
        screen.contains("Ran"),
        "past tense once it is done: {screen}"
    );
    assert!(screen.contains("7.5s"), "{screen}");
}

#[test]
fn content_this_head_did_not_write_cannot_reconfigure_the_terminal() {
    use letibot_transcript::{
        ReasoningField, SystemOrigin, ToolCall, ToolOutcome, TranscriptItem as T, UserPart,
    };
    let rows: Vec<(&str, String)> = vec![
        (
            "model prose, settled",
            render_one(T::Assistant {
                text: format!("an answer {EVIL} with a sequence in it\nand a second line"),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        ),
        (
            "model reasoning",
            render_one(T::Reasoning {
                text: format!("thinking {EVIL} about it\nsecond line"),
                field: ReasoningField::ReasoningContent,
                truncated: false,
            }),
        ),
        (
            "the operator's own paste",
            render_one(T::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: format!("I pasted {EVIL} out of a log\nsecond line"),
                }],
            }),
        ),
        (
            "a system row",
            render_one(T::System {
                text: format!("bootstrap {EVIL}\nsecond line"),
                origin: SystemOrigin::Bootstrap,
            }),
        ),
        (
            "a tool payload",
            render_one(T::ToolResult {
                call_id: "call_0".into(),
                name: "bash".into(),
                outcome: ToolOutcome::Ok,
                payload: format!("$ ls\n{EVIL}\nfile.rs"),
                edit: None,
                origin: None,
                media: None,
            }),
        ),
        (
            "a tool's failure reason",
            render_one(T::ToolResult {
                call_id: "call_1".into(),
                name: "bash".into(),
                outcome: ToolOutcome::Failed {
                    reason: format!("the command wrote {EVIL} to stderr"),
                },
                payload: "$ false".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        ),
        (
            "the raw markup behind ctrl-x",
            render_one(T::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_2".into(),
                    name: "read".into(),
                    arguments: format!("{{\"path\":\"{EVIL}\"}}"),
                }],
                truncated: false,
            }),
        ),
        (
            "both sides of a diff read off disk",
            render_one(T::ToolResult {
                call_id: "call_3".into(),
                name: "edit".into(),
                outcome: ToolOutcome::Ok,
                payload: "1 replacement".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        ),
    ];
    for (what, row) in rows {
        assert!(
            !row.contains(EVIL_HEAD),
            "{what}: a window-title request reached the row: {row:?}"
        );
        assert!(
            !row.contains('\u{7}'),
            "{what}: a BEL survived into the row: {row:?}"
        );
        // …and the row is still a row: the sequence went **whole**, leaving the text
        // standing rather than blanking the line.
        assert!(
            !row.trim().is_empty(),
            "{what}: the row was blanked by the sanitiser: {row:?}"
        );
        assert!(
            !row.contains("]0;pwned"),
            "{what}: an OSC's body was left as text: {row:?}"
        );
    }
}

/// **The half of §3.1 that is easy to get wrong** now lives with the functions, in
/// `letibot_ui::text`'s own tests — a single-line sanitiser applied to a document
/// collapses every paragraph, and the pair of functions is where that is pinned. It
/// used to be duplicated here.

/// **R13: the number beside a running call comes from a clock that keeps moving.**
///
/// The operator: *"when a tool call takes time — say `cargo build` — it is frozen
/// at 0ms until it finishes. A live coarse timer would be nice, say 1/10th of a
/// second."*
///
/// **And my head's defect is not leticl's, which is worth writing down because the
/// two look identical on a screen.** leticl's number was already read from its own
/// clock; what was frozen was the *asking*, because its loop painted only when a
/// frame or a key marked the head dirty. This head composes a frame on every tick
/// — `Terminal::events()` returns after ~100 ms of quiet under `VMIN=0 VTIME=1`, and
/// `App::screen` is called unconditionally after it — so the asking was never the
/// problem here. What was wrong is that **the row read a different clock from the
/// spinner two hundred lines below it**: the spinner is `App::now_ms`, which the
/// driver advances every tick, and the running card's elapsed was
/// `TurnPane::last_ms`, the last event's timestamp — a clock that *stops*
/// whenever the daemon stops talking, which is precisely what a silent build does.
///
/// So the assertion is the pair: with no event in between, thirty seconds of
/// silence, the number moves and the spinner moves with it.
#[test]
fn a_running_call_counts_up_while_the_daemon_says_nothing() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    a.apply(ServerFrame::Event(env_at(
        2,
        1_000,
        testing::proposed_on("t1", "c1", "bash", "\"cargo build\""),
    )));
    a.clock(2_000);
    a.apply(ServerFrame::Event(env_at(
        3,
        2_000,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));

    // **The premise: nothing else arrives.** Every assertion below is about a
    // clock that moved with no event to carry it, so an event in between would
    // make this test pass against a head that never fixed anything. (The head's
    // clock and the events' `ts` are advanced together, the way a live session's
    // are — same machine, same clock, one tick apart at most.)
    let last_event = a.last_event_at;

    // Two hundred milliseconds in, nothing said: the call counts from when it
    // actually started, not from the last thing the daemon said.
    a.clock(2_200);
    let early = a.screen(120, 24).join("\n");
    assert!(early.contains("Running \"cargo build\""), "{early}");
    assert!(
        early.contains("200ms"),
        "a running call did not count up with the head's own clock: {early}"
    );
    assert_eq!(a.last_event_at, last_event, "the premise: no event arrived");

    // And thirty seconds of a silent build, which is the case the operator was
    // looking at. Coarse on purpose — tenths, because that is what a person reads.
    a.clock(32_000);
    let late = a.screen(120, 24).join("\n");
    assert!(
        late.contains("30.0s"),
        "thirty silent seconds and the number did not move: {late}"
    );
    assert_eq!(a.last_event_at, last_event, "and still nothing arrived");

    // **The spinner and the number are one clock now.** They were not before, and
    // that is the whole of this defect: a row whose spinner turns while its
    // duration sits still reads as a stalled turn with a working clock beside it.
    assert!(
        late.contains("Responding"),
        "the border's own spinner is the comparison: {late}"
    );

    // **A call with no anchor keeps the log's clock**, and this is the half that
    // must not be lost: a `--replay` applies every frame *before* it sets a
    // clock, so there is no anchor to measure against and the recorded span is
    // the only honest number. Here the log says the call ran 1.0s.
    let mut b = app();
    b.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    b.apply(ServerFrame::Event(env_at(
        2,
        1_000,
        testing::proposed_on("t1", "c1", "bash", "\"cargo build\""),
    )));
    b.apply(ServerFrame::Event(env_at(
        3,
        2_000,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    assert_eq!(b.now_ms, 0, "the premise: nobody told this head the time");
    b.apply(ServerFrame::Event(env_at(
        4,
        3_000,
        SessionEvent::ToolProgress {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            note: "Compiling".into(),
        },
    )));
    let replayed = b.screen(120, 24).join("\n");
    assert!(
        replayed.contains("1.0s"),
        "a replayed call keeps the log's own span: {replayed}"
    );
}

#[test]
fn a_call_from_a_snapshot_has_no_duration_rather_than_a_zero_one() {
    // A snapshot carries no timestamps. `0.0s` is a measurement that was never
    // taken rendered as one that was.
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(testing::proposed("t1", "c1", "read"));
    hub.publish(SessionEvent::ToolFinished {
        turn_id: "t1".into(),
        call_id: "c1".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload_digest: "fnv1a:1".into(),
        inline_bytes: 40,
        full_bytes: 40,
        spill: None,
        repairs: 0,
        edit: None,
    });
    let mut a = app();
    a.apply(ServerFrame::Resync {
        reason: "test".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    let card: Vec<String> = a
        .screen(120, 24)
        .into_iter()
        .filter(|l| l.trim_start().starts_with('\u{25cf}'))
        .collect();
    assert_eq!(card.len(), 1, "{card:?}");
    assert!(card[0].contains("Read"), "{card:?}");
    assert!(
        !card[0].contains("0ms") && !card[0].contains("0.0s"),
        "a snapshot has no clock, so it must show no duration: {card:?}"
    );
}

/// **A tool's output cannot reconfigure the operator's terminal.**
///
/// A payload is whatever the command wrote. Rendering it straight put its
/// escapes on the wire — and in the operator's own store, 2026-09-20, 44
/// `tool_result` rows carry an escape and 20 carry a MODE string: `?1002`
/// and `?1006` are mouse reporting, `?1049` the alternate screen, `?2004`
/// bracketed paste.
///
/// Turning mouse reporting off is why *"when i expand tools with Ct scroll
/// stops working"*: ctrl-t renders payloads that were folded away, one of
/// them disables the wheel, and folding back cannot undo what the terminal
/// was already told. Switching byobu windows fixes it because tmux
/// re-asserts its modes on focus.
#[test]
fn an_expanded_tool_payload_cannot_turn_the_mouse_off() {
    let mut a = app();
    a.tools = Fold::Open;
    let esc = '\u{1b}';
    // Exactly the shapes found in the store: a mode reset, a cursor move and
    // an SGR colour.
    let payload = format!("before{esc}[?1002l{esc}[?1006l\n{esc}[1;1Hmoved\n{esc}[0;90mdim\nafter");
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("i1", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
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
    )));
    let screen = a.screen(100, 30).join("\n");
    // **The ESC is what makes a sequence a command.** `[?1002l` as literal
    // text is four harmless characters; it is `ESC` in front of it that the
    // terminal acts on. So this asserts the pairing, not the substring —
    // and deliberately not "no ESC anywhere", because the head's own colour
    // is made of them.
    for bad in ["[?1002", "[?1006", "[?1049", "[?2004", "[1;1H"] {
        let seq = format!("{esc}{bad}");
        assert!(
            !screen.contains(&seq),
            "a payload's `ESC{bad}` reached the terminal: {screen:?}"
        );
    }
    // And the text itself survives, which is the point of showing it at all.
    assert!(screen.contains("before"), "{screen}");
    assert!(screen.contains("moved"), "{screen}");
    assert!(screen.contains("after"), "{screen}");
}

#[test]
fn a_spilled_result_reads_as_the_harness_working_not_as_damage() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::proposed("t1", "c1", "grep"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "fnv1a:1".into(),
            inline_bytes: 8192,
            full_bytes: 480_000,
            spill: Some("9fa3c1".into()),
            repairs: 0,
            edit: None,
        },
    )));
    let screen = a.screen(160, 12).join("\n");
    assert!(screen.contains("8.0 KB of 468.8 KB"), "{screen}");
    assert!(screen.contains("read_spill hash=9fa3c1"), "{screen}");
    assert!(screen.contains("the rest is kept"), "{screen}");
}

#[test]
fn a_running_tool_call_says_what_it_is_running_on() {
    // §4.1, fixed. The whole difference between a tool list that is useful and
    // one that is decorative.
    let mut a = app();
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    a.apply(ServerFrame::Event(env_at(
        2,
        1_000,
        testing::proposed_on("t1", "c1", "bash", "\"cargo test --workspace\""),
    )));
    a.apply(ServerFrame::Event(env_at(
        3,
        2_000,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    let screen = a.screen(120, 24).join("\n");
    assert!(
        screen.contains("Running \"cargo test --workspace\""),
        "a running call renders its argument, not just its verb:\n{screen}"
    );
}

/// A settled call is **one** row, not two.
///
/// It used to be two: `→ Read TODO.md` from the assistant row, then
/// `▸ Read TODO.md · ok · 129 lines · ctrl-t` from the result row three lines
/// below, saying the same thing with an outcome attached. Four calls cost
/// eight rows of a thirty-four-row terminal before any output was on it.
#[test]
fn a_settled_call_is_one_row_and_the_row_is_the_one_with_the_result_on_it() {
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
    let before = a.screen(120, 40).join("\n");
    assert_eq!(
        before.matches("TODO.md").count(),
        1,
        "a call with no result yet is announced exactly once:\n{before}"
    );
    assert!(
        before.contains("no result"),
        "and says it has none:\n{before}"
    );

    a.apply(ServerFrame::Event(env(
        3,
        testing::appended("r.t", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        4,
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
        "and once the result lands the proposal does not stay beside it:\n{after}"
    );
    assert!(after.contains("▸ Read TODO.md · ok"), "{after}");
    assert!(
        !after.contains("no result"),
        "a call that returned does not still read as one that did not:\n{after}"
    );
}

/// A subject that does not fit takes the shortening, and the outcome does not.
#[test]
fn a_subject_too_long_for_the_row_never_pushes_the_outcome_off_it() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("t", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "t".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "call_0".into(),
                name: "ask_code".into(),
                outcome: letibot_transcript::ToolOutcome::NotRun {
                    why: "no retrieval backend is attached to this session".into(),
                },
                payload: "a\nb\nc\n".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    // The subject comes from the round's assistant row; here there is none, so
    // it is the correlation id — long enough to matter once the row narrows.
    let screen = a.screen(60, 24).join("\n");
    assert!(screen.contains("not run"), "{screen}");
    assert!(
        screen.contains("no retrieval backend"),
        "the reason wraps in the body rather than being cut off a header:\n{screen}"
    );
}

/// The other half of the same table: a result whose round is not on the screen
/// borrows nothing. `(call_0)` is a correlation key and reads as one; a
/// neighbour's path reads as a fact.
#[test]
fn a_result_whose_round_the_head_cannot_see_says_so_rather_than_borrowing() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("t.0", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "t.0".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "call_0".into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "hello\n".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("(call_0)"), "{screen}");
}

/// **A snapshot that carries a finished call is seated, and counts as nothing executing.**
///
/// The late-join and resync path, and the brief for this work named it as the lead: *"the head
/// learns each call's state at the one moment the daemon hands it over"*. `App::load` does exactly
/// that — `self.turn = s.turn.map(…)` seats the pane's state and every call's own state out of the
/// daemon's folded turn — so this is a RECORD of that seating rather than a fix for it. The daemon
/// half is asserted first, because the head half would pass on a snapshot that carried nothing:
/// a `Hub` that published `ToolFinished` reports the call `Finished` in its own view.
///
/// The second half is the control and it is not decoration — a snapshot whose call is genuinely
/// executing, with no result row yet, must count as executing, or the first half would pass on a
/// colour that never lights.
#[test]
fn a_snapshot_that_carries_a_finished_call_is_seated_and_counts_as_nothing_executing() {
    /// The call as the daemon proposes and starts it.
    fn started(hub: &Hub) {
        hub.publish(testing::turn_started("r1"));
        hub.publish(testing::proposed_on("r1", "c1", "bash", "\"cargo test\""));
        hub.publish(SessionEvent::ToolStarted {
            turn_id: "r1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        });
    }

    /// **The daemon's own session, whose one call has finished** — the `ToolFinished` the runtime
    /// publishes from inside the call (`crates/tools/src/runtime.rs`), with no result row appended
    /// yet, which is the frame a call's row is one frame away from.
    let hub = Hub::new("s");
    started(&hub);
    hub.publish(SessionEvent::ToolFinished {
        turn_id: "r1".into(),
        call_id: "c1".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload_digest: "d".into(),
        inline_bytes: 36,
        full_bytes: 36,
        spill: None,
        repairs: 0,
        edit: None,
    });
    let snap = hub.snapshot();
    let turn = snap.turn.as_ref().expect("the daemon keeps the turn");
    assert!(
        matches!(turn.calls[0].state, CallState::Finished { .. }),
        "the daemon did not fold the finish: {:?}",
        turn.calls[0].state
    );
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", true)], snap));
    assert!(
        matches!(
            a.turn.as_ref().expect("the seated pane").calls[0].state,
            CallState::Finished { .. }
        ),
        "the snapshot's call state was not seated at all"
    );
    assert_eq!(
        a.live_work_now().running,
        0,
        "a finished call out of a snapshot is counted as executing"
    );

    // The control: the same daemon one event earlier, with the call still executing.
    let hub = Hub::new("s");
    started(&hub);
    let mut b = app();
    b.apply(hello("s", vec![brief("s", "one", true)], hub.snapshot()));
    assert_eq!(
        b.live_work_now().running,
        1,
        "a snapshot's executing call is not counted, so the assertion above is vacuous"
    );
}

/// **A settled row takes its register from the ONE outcome mapping** — R51 item 9.
///
/// The operator, looking at a command the harness had just backgrounded: *"why on earth
/// backgrounding message is in red"*. The row asked `bad = !matches!(outcome, Ok)` and drew
/// every non-`ok` outcome in `Failure`, so a job still working read as something to retry and a
/// refusal read as a malfunction. This asks the mapping for each outcome, and only `ok` is
/// overruled — faint, because a green line under every command says nothing.
///
/// **And the register a backgrounded call gets changed afterwards, by a ruling of its own:**
/// *"wait, color change can mean some rerenders, so lets make it white as soon as job starts"*.
/// So it is no longer one of the *attention* outcomes — the CALL is over, and the job behind it
/// is counted on the composer's edge and listed in the jobs pane. See the two arms' notes in
/// `card::Outcome`.
#[test]
fn a_settled_rows_register_comes_from_the_outcome_not_from_a_not_ok_test() {
    use letibot_transcript::ToolOutcome as O;
    // The two the operator named that are still *something to look at*: neither is a failure.
    for waiting in [
        O::Denied {
            req_id: "d1".into(),
        },
        O::Abstained {
            reason: "nothing to do".into(),
        },
    ] {
        assert_eq!(
            outcome_role(&waiting),
            Role::Attention,
            "{waiting:?} is something to look at, not something that failed"
        );
    }
    // **A backgrounded call is neither loud nor lit**: finished work whose product is a job.
    assert_eq!(
        outcome_role(&O::Backgrounded {
            handle: "j1".into(),
            ran_for_ms: 1,
            how: letibot_transcript::Backgrounding::Asked,
            next: "read it".into(),
        }),
        Role::Faint,
        "the call is over the moment it is backgrounded — the JOB is what runs on"
    );
    // A real failure is still loud, and `ok` is the quiet case this head chose.
    assert_eq!(
        outcome_role(&O::Failed {
            reason: "boom".into()
        }),
        Role::Failure
    );
    assert_eq!(outcome_role(&O::Ok), Role::Faint);
    // A `not run` is the mapping's own answer, whatever it is — the point is that it comes
    // from there rather than from a second spelling here.
    assert_eq!(
        outcome_role(&O::NotRun {
            why: "the scope closed".into()
        }),
        display_outcome(&O::NotRun {
            why: "the scope closed".into()
        })
        .role()
    );
}

/// **The three lines read as one paragraph** — the requirement's own claim, asserted in
/// the only form that can: the order.
///
/// The operator's shape is *prompt → narration → work → report*, and the marker sits
/// between the last two. So the assertion is that the screen carries the prose, then the
/// marker, then the report, **with no blank line anywhere between them** — which is what
/// "the marker does not have to say what was concluded" is worth in practice. Run with
/// `--nocapture` to see it: the paragraph is the evidence and the three positions are the
/// claim.
/// **A run of hidden rows neither count can describe says `[1 head event]`, never `[]`.**
///
/// R53 §1.5, and it is a defect this head could put on the screen: the counts walked a run
/// counting `ToolResult` and `Reasoning` and nothing else (`_ => {}`), while the rung also
/// hides `System` and `SegmentMark`. A run made only of those had no calls and no thinking
/// lines, so its marker had no body — `[]`, on the line whose whole job is to be the fact the
/// rung was hiding.
///
/// leticl's fallback is the answer and its wording is the one used: *"a run neither count can
/// describe … falls back to their count, because `[]` is not a marker"* — `%hidden-run-counts`
/// ends `(t (incf events))`. The clause is drawn **only** when the other two are empty, which
/// the second half of this test pins: a run of real calls must not grow a `, 1 head event`.
/// **One call, one word, live or settled** — R53 §1.5, and the second spelling this tree kept.
///
/// The live card's word came from `card::Outcome`, the settled transcript row's from a function
/// in this file, and they disagreed. MEASURED on the same call before the fix:
///
/// ```text
///   live       refused            failed · timed out        failed · not run — {why}
///   settled    REFUSED            timeout                   not run
/// ```
///
/// — the word changed as the row landed, twice into a different word and once only in case.
/// leticl paid for this once already (`bceef58`: *"the two spellings disagreed about `not_run`
/// and about backgrounded"*), and its docstring for its own `%outcome-word` names this head's
/// spelling as the reference. So the card moved, and this test is the table that says so: one
/// place decides, and it is pinned for every outcome rather than for the three that were wrong.
///
/// **What would fail it.** Putting the word back in either renderer — a `timeout` that reads
/// `failed`, a `refused` in lower case, a `not run` collapsed into `Failed`. The table has to be
/// edited, which is the point: the words are a vocabulary and not a coincidence.
#[test]
fn one_call_reads_the_same_word_whatever_row_draws_it() {
    use letibot_transcript::ToolOutcome as O;
    let cases: Vec<(O, &str, Option<&str>)> = vec![
        (O::Ok, "ok", None),
        (
            O::Abstained {
                reason: "no answer in the corpus".into(),
            },
            "ABSTAINED",
            Some("no answer in the corpus"),
        ),
        (
            O::Failed {
                reason: "exit 101".into(),
            },
            "failed",
            Some("exit 101"),
        ),
        (
            O::Denied {
                req_id: "req_1".into(),
            },
            "REFUSED",
            Some("the call was denied (req_1)"),
        ),
        (O::Timeout, "timeout", None),
        (
            O::NotRun {
                why: "the turn was interrupted".into(),
            },
            "not run",
            Some("the turn was interrupted"),
        ),
        (
            O::Backgrounded {
                handle: "j4".into(),
                ran_for_ms: 400,
                how: letibot_transcript::Backgrounding::Operator {
                    identity: "dead".into(),
                },
                next: "`/job j4 out` to read it".into(),
            },
            "backgrounded",
            Some("as `j4` after 0.4s — `/job j4 out` to read it"),
        ),
    ];
    for (outcome, word, why) in cases {
        // The live card's word, which is the card's own one list.
        assert_eq!(
            display_outcome(&outcome).word(),
            word,
            "the live card's word for {outcome:?}"
        );
        // The settled row's, which now asks the same place.
        assert_eq!(
            outcome_word(&outcome),
            word,
            "the settled row's word for {outcome:?}"
        );
        assert_eq!(
            outcome_why(&outcome).as_deref(),
            why,
            "and the reason behind it, for {outcome:?}"
        );
        assert_eq!(
            display_outcome(&outcome).reason(),
            why,
            "the card's reason is the row's reason, for {outcome:?}"
        );
    }
}

/// **R38's second setting, and the one that was `slash_refused` until today.** `/diff` bare
/// is a card; `/diff NAME` is the same function the card calls.
#[test]
fn the_diff_card_offers_both_styles_and_sets_the_one_taken() {
    let mut a = app();
    assert!(a.diff_split, "side by side is the default");
    assert_eq!(a.command("diff"), None);
    assert!(a.pick == Some(Pick::Diff));
    assert_eq!(a.mode_sel, 1, "the cursor starts on the style in force");
    let screen = a.screen(120, 40).join("\n");
    for (value, why) in DIFF_VALUES {
        assert!(
            screen.contains(value),
            "`{value}` is not on the card:\n{screen}"
        );
        let lead: String = why.chars().take(30).collect();
        assert!(
            screen.contains(&lead),
            "the card does not say what `{value}` looks like:\n{screen}"
        );
    }
    let row = screen
        .lines()
        .find(|l| l.contains("split") && l.contains('▸'))
        .expect("the style in force is on the card");
    assert!(row.contains("← now"), "{row}");
    assert!(
        screen.contains("every edit card, drawn and future"),
        "the card does not say what it reaches:\n{screen}"
    );
    a.key(Key::Up); // split -> unified
    assert_eq!(a.mode_sel, 0);
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.pick.is_none(), "taking a value closes the card");
    assert!(!a.diff_split, "the card set the style");
    let notice = a.notice.clone().unwrap();
    assert!(notice.contains("diff unified"), "{notice}");
}

/// **A change reaches the rows already drawn**, which is the difference between a setting
/// and a preference for what comes next: the history holds rendered rows, so the whole buffer
/// is stale at the moment the style changes.
#[test]
fn the_diff_verb_sets_the_style_and_redraws_what_is_already_drawn() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
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
    let split = a.screen(120, 30).join("\n");
    assert!(
        split.contains("1 - fn a() {}"),
        "the two-panel diff is not drawn:\n{split}"
    );
    // The typed name, not the card: the same setting through the same function.
    assert_eq!(a.command("diff unified"), None);
    assert!(!a.diff_split);
    let unified = a.screen(120, 30).join("\n");
    assert!(
        !unified.contains("1 - fn a() {}"),
        "the row already drawn kept its old style:\n{unified}"
    );
    assert!(unified.contains("x();"), "{unified}");
    assert!(unified.contains('+'), "{unified}");
    // A name that is not a style is refused BY NAME, listing the two — the shape `/verbosity`
    // already had, so a typo reads as a refusal and not as silence.
    assert_eq!(a.command("diff sideways"), None);
    let notice = a.notice.clone().unwrap();
    assert!(notice.contains("not a diff style"), "{notice}");
    assert!(
        notice.contains("unified") && notice.contains("split"),
        "the refusal does not name what it would have taken: {notice}"
    );
    assert!(!a.diff_split, "a refusal changed the setting");
    // **And the spellings the SHARED preference file accepts are taken.** The file is
    // read by both heads, and leticl's parser takes `split` / `side-by-side` / `auto`
    // and `unified` / `single`. A verb with a list of its own would refuse a value the
    // file the two of them share had already accepted — so this is `DiffPref::parse`,
    // and the same word reaching either door lands the same way. Asserted through the
    // preference parser's own answer rather than by repeating the list here.
    for (typed, want) in [
        ("auto", crate::prefs::DiffPref::Split),
        ("side-by-side", crate::prefs::DiffPref::Split),
        ("single", crate::prefs::DiffPref::Unified),
        ("unified", crate::prefs::DiffPref::Unified),
    ] {
        assert_eq!(
            crate::prefs::DiffPref::parse(typed),
            Some(want),
            "`{typed}` is a spelling the shared file takes"
        );
        a.diff_split = matches!(want, crate::prefs::DiffPref::Unified);
        assert_eq!(a.command(&format!("diff {typed}")), None);
        assert_eq!(
            a.diff_split,
            matches!(want, crate::prefs::DiffPref::Split),
            "`/diff {typed}` did not set what the file's parser says it means"
        );
    }
}

/// **The file is named once.** The operator, on `▸ Wrote …/pr-body-align.md · ok` with the
/// same path on the line under it: *"why two times?"*. A header that names the file drops
/// the diff's own name line; a header that could not name it (a call id in its place) keeps
/// it, which is what that line was added for.
#[test]
fn an_edit_card_names_its_file_once() {
    let count = |rows: &[String]| rows.iter().filter(|l| l.contains("a.rs")).count();
    let named = call_card(
        &edit_row(Some(edit_excerpt())),
        &plain_cfg(120),
        0,
        Fold::Open,
        true,
    );
    assert_eq!(count(&named), 1, "{}", named.join("\n"));

    let mut unnamed_row = edit_row(Some(edit_excerpt()));
    unnamed_row.target = String::new();
    let unnamed = call_card(&unnamed_row, &plain_cfg(120), 0, Fold::Open, true);
    assert_eq!(
        count(&unnamed),
        1,
        "a header with no file keeps the diff's name line:\n{}",
        unnamed.join("\n")
    );

    assert!(header_names_the_file("a.rs", "a.rs"));
    assert!(header_names_the_file("src/a.rs", "/w/src/a.rs"));
    assert!(!header_names_the_file("a.rs", "/w/ba.rs"));
    assert!(!header_names_the_file("", "a.rs"));
}

#[test]
fn an_edit_call_renders_the_two_panel_diff_when_it_is_on() {
    let rows = call_card(
        &edit_row(Some(edit_excerpt())),
        &plain_cfg(120),
        0,
        Fold::Open,
        true,
    );
    let joined = rows.join("\n");
    assert!(
        joined.contains('│'),
        "two panels with a separator: {joined}"
    );
    assert!(
        joined.contains('-') && joined.contains("fn a() {}"),
        "{joined}"
    );
    assert!(joined.contains('+') && joined.contains("x();"), "{joined}");
    // The removed and added first lines share one row — the change reads
    // across — and the two added lines that have no old counterpart get
    // their own rows with an empty left panel.
    assert!(
        rows.iter()
            .any(|r| r.contains('-') && r.contains('+') && r.contains('│')),
        "{joined}"
    );
    assert!(
        rows.iter().filter(|r| r.contains('│')).any(|r| r
            .split_once('│')
            .unwrap()
            .0
            .trim()
            .is_empty()
            && r.contains("x();")),
        "{joined}"
    );
}

/// The toggle is the whole gate. Off is a unified diff at any width; on is
/// a split at any width — a narrow pane gets a narrow split, not the byte
/// count, because an edit drawn cramped is still an edit the operator can
/// read, and the width gate's other answer was *no diff at all*.
#[test]
fn the_diff_toggle_alone_picks_split_or_unified_at_any_width() {
    let off = call_card(
        &edit_row(Some(edit_excerpt())),
        &plain_cfg(120),
        0,
        Fold::Open,
        false,
    );
    let text = off.join("\n");
    assert!(!text.contains('│'), "switched off: {off:?}");
    assert!(
        text.contains("+    x();") || text.contains("+x();"),
        "no unified diff: {off:?}"
    );
    assert!(
        !text.contains("64 B"),
        "the byte count came back instead of a diff: {off:?}"
    );

    // Narrow and switched on: still a split. The panels are cramped; the
    // renderer wraps and degrades, and the change is on the screen.
    let narrow = call_card(
        &edit_row(Some(edit_excerpt())),
        &plain_cfg(80),
        0,
        Fold::Open,
        true,
    );
    let text = narrow.join("\n");
    assert!(text.contains('│'), "narrow pane drew no split: {narrow:?}");
    assert!(
        text.contains("x();"),
        "narrow pane lost the change: {narrow:?}"
    );
}

/// **A restarted head still draws the last turn's edit panels.**
///
/// The excerpt rides the `ToolFinished` event, and the snapshot's turn
/// carries the turn's calls with it. Live, the head copied the excerpt into
/// `call_edits` as the row landed, and history rows render their panels from
/// that map; the map was memory-only, so a restart drew every landed edit
/// panel-less even though the snapshot had just handed the head the turn's
/// calls with the excerpt still on them (operator, 2026-09-17). The fix
/// seeds the map from the snapshot, positionally, the way the live hand-off
/// matched rows to calls. This walks the whole restart: a hub whose turn
/// edited a file and then finished, a fresh head attaching to its snapshot —
/// attach replays no events, so the snapshot's turn is the excerpt's only
/// carrier — and the panel on the screen.
///
/// The row here deliberately carries **no** excerpt of its own: rows written
/// before the field existed read as `None`, and this is the case where the
/// seeded map is the only thing between the operator and a panel-less edit.
/// The row-borne path has its own test below.
#[test]
fn a_restarted_head_still_draws_the_last_turns_edit_panels() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(testing::proposed("t1", "c1", "edit"));
    hub.publish(SessionEvent::ToolStarted {
        turn_id: "t1".into(),
        call_id: "c1".into(),
        name: "edit".into(),
        access: "write".into(),
    });
    hub.publish(SessionEvent::ToolFinished {
        turn_id: "t1".into(),
        call_id: "c1".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload_digest: "fnv1a:1".into(),
        inline_bytes: 12,
        full_bytes: 12,
        spill: None,
        repairs: 0,
        edit: Some(edit_excerpt()),
    });
    hub.publish(testing::appended("s.1", "tool_result"));
    hub.record_item(
        "s.1",
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "edit".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "done".into(),
            // A row from before the field existed: the seeded map is the
            // only carrier here, which is the point of this test.
            edit: None,
            origin: None,
            media: None,
        },
    );
    hub.publish(testing::turn_finished("t1"));

    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    // The operator watches tool output open; the fold is a preference, not
    // the bug. The bug is the excerpt the snapshot carried and the head
    // dropped.
    a.tools = Fold::Open;

    let screen = a.screen(120, 40).join("\n");
    assert!(
        screen.contains("x();"),
        "the edit's after-side must render after a restart:\n{screen}"
    );
}

/// **A resumed daemon's rows draw their panels with no turn on the wire.**
///
/// The seed above needs the snapshot's turn: it matches rows to calls
/// positionally through `t.appended`. The operator's second report
/// (2026-09-18) was a daemon that had *resumed from the store* — its view
/// was rebuilt from rows alone, the resume publishes no turn events, and
/// the snapshot it handed a fresh head had `turn: None` with the rows in
/// it. Nothing to seed from, and every landed edit drew panel-less again
/// even after the first fix. The row now carries the excerpt itself, so
/// the row is the carrier: no turn, no event, no map — and the panel
/// still renders.
#[test]
fn a_row_from_a_resumed_daemon_draws_its_panel_without_a_turn() {
    let hub = Hub::new("s");
    // Rows only, as a resumed daemon's republish() emits them: no
    // TurnStarted, no ToolStarted, no ToolFinished. The store has the
    // transcript, and the excerpt rides the row.
    hub.publish(testing::appended("s.1", "tool_result"));
    hub.record_item(
        "s.1",
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "edit".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "done".into(),
            edit: Some(edit_excerpt()),
            origin: None,
            media: None,
        },
    );

    let mut a = app();
    let snap = hub.snapshot();
    assert!(
        snap.turn.is_none(),
        "a resumed daemon's snapshot has no turn"
    );
    a.apply(hello("s", vec![brief("s", "one", false)], snap));
    a.tools = Fold::Open;

    let screen = a.screen(120, 40).join("\n");
    assert!(
        screen.contains("x();"),
        "the edit's after-side must render from the row alone:\n{screen}"
    );
}

/// **The clip the operator photographed.** The card built its diff for the
/// body width, and the turn block then stepped the whole card in by the
/// activity indent — *after* the card had rendered — so every split row
/// left the frame two columns wider than the frame trims to, and a
/// full-width panel line lost its tail at the terminal edge. The panels
/// are built for the width the row will actually have, which is the
/// arithmetic the transcript's own diff arm already does at its `let w`.
#[test]
fn the_split_diff_fits_after_the_indent_the_caller_applies() {
    let w = 209; // a 211-column terminal minus the gutter
    let e = letibot_sessionlog::event::ToolEdit {
        before: format!("{}\n", "y".repeat(300)),
        after: format!("{}\n", "x".repeat(300)),
        before_lines: 1,
        after_lines: 1,
        ..edit_excerpt()
    };
    let cfg = plain_cfg(w);
    let rows = step_in(
        call_card(&edit_row(Some(e)), &cfg, 0, Fold::Open, true),
        activity_indent(w),
    );
    let joined = rows.join("\n");
    assert!(
        rows.iter().all(|r| r.chars().count() <= w),
        "a diff row outgrew the frame the card is drawn in: {joined}"
    );
    // And the width was not paid for by the content: both panels wrap
    // their long line whole, so the change is on the screen, not cut.
    assert_eq!(joined.matches('x').count(), 300, "{joined}");
    assert_eq!(joined.matches('y').count(), 300, "{joined}");
}

#[test]
fn a_created_file_renders_as_all_right_panel_and_a_cap_says_so() {
    let created = letibot_sessionlog::event::ToolEdit {
        created: true,
        before: String::new(),
        before_lines: 0,
        ..edit_excerpt()
    };
    let rows = call_card(
        &edit_row(Some(created)),
        &plain_cfg(120),
        0,
        Fold::Open,
        true,
    );
    let body: Vec<&str> = rows
        .iter()
        .filter(|r| r.contains('│'))
        .map(String::as_str)
        .collect();
    assert!(!body.is_empty(), "{rows:?}");
    for r in &body {
        let (left, _right) = r.split_once('│').unwrap();
        assert!(left.trim().is_empty(), "created: no left panel: {r:?}");
    }

    let capped = letibot_sessionlog::event::ToolEdit {
        truncated: true,
        ..edit_excerpt()
    };
    let rows = call_card(
        &edit_row(Some(capped)),
        &plain_cfg(120),
        0,
        Fold::Open,
        true,
    );
    assert!(
        rows.iter().any(|r| r.contains("the excerpt was capped")),
        "{rows:?}"
    );
}

/// **The verb decides the HEADER; the pair decides the BODY.**
///
/// This read *"a non-edit call never grows a second panel"*, and that was right while `edit`
/// and `write` were the only calls that carried an excerpt: the name was the whole signal,
/// and a `grep` carrying one was a shape that could not happen. `crates/tools/src/detect.rs`
/// made a `bash` call a **second carrier** — a shell that rewrites a file hands the head both
/// sides deliberately, so that the change can be read — and the gate on the name then drew
/// the note and no diff. The operator, watching a `python3` heredoc rewrite a file: *"right,
/// but i didnt see the diff"*.
///
/// So what survives of the old rule is the half that was ever about the NAME: a call draws a
/// panel for the pair it holds and for nothing else, and it is still drawn under its own
/// verb — `Ran` for a shell, which is the label `crates/tools/src/detect.rs` refuses to
/// falsify by calling the change an `edit`.
#[test]
fn a_call_draws_a_panel_for_the_pair_it_carries_and_never_for_one_it_does_not() {
    // No pair, whatever the verb: no panel.
    let mut row = edit_row(None);
    row.name = "grep".into();
    let rows = call_card(&row, &plain_cfg(120), 0, Fold::Open, true);
    assert!(!rows.join("\n").contains('│'), "{rows:?}");

    // A pair on a call whose verb is not an edit — the shape `bash` arrives in — draws it,
    // and the header still says what the call was rather than claiming `Edited`.
    let mut row = edit_row(Some(edit_excerpt()));
    row.name = "bash".into();
    row.target = "python3 - <<'PYEOF' …".into();
    let rows = call_card(&row, &plain_cfg(120), 0, Fold::Open, true);
    let text = rows.join("\n");
    assert!(text.contains('│'), "no panel for the pair: {rows:?}");
    assert!(
        text.contains("x();"),
        "the change is not on screen: {rows:?}"
    );
    assert!(
        text.contains("Ran"),
        "the verb is still the call's own: {rows:?}"
    );
}

#[test]
fn the_live_event_path_feeds_the_two_panel_view() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
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
    let screen = a.screen(120, 24).join("\n");
    // The diff body, not the byte-count fallback: the added line is on
    // the screen. (The screen always contains `│` — the composer's box —
    // so the separator proves nothing; the code does.)
    assert!(screen.contains("x();"), "{screen}");
    assert!(screen.contains("fn a() {}"), "{screen}");
    // And the setting the pane flips is the one the card reads: off, the
    // same change is drawn as a unified diff — signed, still there.
    flip_diff_view(&mut a);
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("x();"), "{screen}");
    assert!(screen.contains("+"), "{screen}");
    assert!(
        !screen.contains("64 B"),
        "the byte count came back: {screen}"
    );
}

/// **The bug the operator reported.** The diff was drawn only by the live
/// card, and the transcript takes a call over the moment its result row
/// lands — so the diff existed for the gap between `ToolFinished` and
/// `TranscriptAppended`, which is to say never. The settled row must draw
/// it, folded and open, and must keep drawing it after the next turn starts.
#[test]
fn a_settled_edit_row_draws_the_diff_and_keeps_it_across_turns() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
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
    // The transcript takes the call over: the row lands, then its body.
    a.apply(ServerFrame::Event(env(
        4,
        testing::appended("t1.r1", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
            5,
            SessionEvent::TranscriptContent {
                item_id: "t1.r1".into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: "c1".into(),
                    name: "edit".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "a.rs: 1 replacement(s). lines 1-3\n\n     1| fn a() {\n     2|     x();\n     3| }\n".into(),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("x();"),
        "settled row lost the diff:\n{screen}"
    );
    assert!(
        !screen.contains("1 replacement(s)"),
        "the tool's prose was drawn instead of the diff:\n{screen}"
    );

    // The next turn takes the pane away; the settled row still has its pair.
    a.apply(ServerFrame::Event(env(6, testing::turn_started("t2"))));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("x();"),
        "the diff vanished when the next turn started:\n{screen}"
    );

    // Narrow: unified, and still the change.
    let screen = a.screen(80, 30).join("\n");
    assert!(
        screen.contains("x();"),
        "narrow settled row lost the change:\n{screen}"
    );
    // The toggle reaches the settled row too, not only the live pane.
    let split = a.screen(120, 30).join("\n");
    assert!(
        split.contains('│') && split.contains("1 - fn a() {}"),
        "{split}"
    );
    flip_diff_view(&mut a);
    let unified = a.screen(120, 30).join("\n");
    assert!(
        unified.contains("-fn a() {}") || unified.contains("- fn a() {}"),
        "{unified}"
    );
    assert!(
        !unified.contains("1 - fn a() {}                                          │"),
        "still split after the flip:\n{unified}"
    );
    if std::env::var("LETIBOT_SHOW").is_ok() {
        eprintln!("=== 120 unified ===\n{unified}");
    }
}

/// **Ctrl+O guards the command, not the turn.** The operator, looking at a
/// `◐ Running "cargo test …"` card while the head refused: *"nothing is
/// running to move to the background"* / *"how come"*. The gate asked whether
/// the TURN was running, and a terminal turn state can still hold a call the
/// daemon is executing.
#[test]
fn ctrl_o_promotes_a_running_command_even_after_the_turn_went_terminal() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(0, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        1,
        testing::proposed("t1", "c1", "bash"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    // Mid-call, with the turn still running: promote, as it always did.
    assert_eq!(a.key(Key::CtrlO), Some(Action::Promote));

    // Now the turn goes terminal with the call still executing — the state the
    // `TurnFinished` arm's own comment describes, and which the engine reaches
    // on every interrupt path.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TurnInterrupted {
            turn_id: "t1".into(),
            reason: "steering_urgent".into(),
            partial_kept: false,
        },
    )));
    // **Two different questions, and this is the test that pins them apart.** The turn is
    // terminal — nothing is generating — and the head is still BUSY, because the call the
    // daemon is executing belongs to it. That gap is R51's whole root cause: every gate that
    // asked the state name for *is the model working* got `false` here, which is why esc-esc
    // was dead through every command and the status row vanished through every call.
    assert!(!a.turn_generating(), "the turn is terminal");
    assert!(a.turn_busy(), "and the head is still working on its call");
    assert!(
        a.running_call().is_some(),
        "and the command is still running"
    );
    assert_eq!(
        a.key(Key::CtrlO),
        Some(Action::Promote),
        "the command is what gets promoted, so the command is what is asked about"
    );
}
