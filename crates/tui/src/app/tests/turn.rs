//! The turn in flight: streaming, tokens, reasoning, the footer, a stuck turn.

use super::*;

#[test]
fn a_head_that_filters_everything_still_says_so() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::TERSE);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    let before = a.filtered;
    for i in 0..10 {
        a.apply(ServerFrame::Event(env(
            2 + i,
            testing::reasoning("t1", "thinking "),
        )));
    }
    assert_eq!(a.filtered - before, 10);
    // Where it says so is `/status`, not the bottom border. §13.2b's rule is
    // about the moment the disclosure is READ — the count has to exist, be
    // exact, and be reachable without restarting anything. It was never an
    // argument for a resident row of zeros next to the prompt.
    a.command("status");
    let screen = a.screen(120, 40).join("\n");
    assert!(screen.contains("filtered"), "{screen}");
    assert!(screen.contains("10 (terse)"), "{screen}");
    // And a head that has lost nothing says nothing on the border.
    assert_eq!(a.status_line(200), "", "a clean head has a clean border");
}

/// **A turn whose only work is a running tool call can be interrupted** (R51 item 16, and the
/// preamble's first gate).
///
/// The measured defect: `TurnFinished` fires per ROUND, so the state name reads `finished` from
/// the instant a call starts — and this gate read the state name, so esc esc was dead for the
/// whole of every command. leticl measured the same thing on its own head: esc esc during a
/// `sleep 60`, and forty seconds later still `Responding · 42.0s` with the call running.
#[test]
fn esc_twice_interrupts_a_turn_that_is_only_waiting_on_a_call() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::proposed_on("t1", "c1", "bash", "\"sleep 60\""),
    )));
    // The round's generation ends, which is exactly when the call starts.
    a.apply(ServerFrame::Event(env(3, testing::turn_finished("t1"))));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    // The premise, and it is the whole bug: nothing is generating, and the model is working.
    assert!(!a.turn_generating(), "the round is over");
    assert!(a.turn_busy(), "and the call is running");
    assert_eq!(a.key(Key::Esc), None, "one press arms, it does not fire");
    assert!(
        matches!(a.key(Key::Esc), Some(Action::Interrupt(_))),
        "esc esc reaches the daemon while a command runs"
    );
}

/// **The status row is drawn for the whole life of the turn — including through a call** (R51
/// item 3).
///
/// The measured defect was not the wrong tense but NO LINE AT ALL: the gate read the state
/// name, so `Responding · 4.2s` vanished the moment a call started and the screen became
/// indistinguishable from a head that had stopped. The row says what the model is doing, and a
/// model waiting on a command is still doing something.
#[test]
fn the_status_row_survives_a_tool_call() {
    // `began_ms` is the PROMPT's stamp and the daemon sends the same one on every round of it
    // (`harnessd::sessions::run_prompt` → `begin_turn_clock`), which is what makes the clock a
    // clock for the turn rather than for a round.
    //
    // **The id is the ROUND's, and it is a parameter because two rounds cannot share one.**
    // `run_turn_steered` mints `{transcript}#{turn_seq}` with `turn_seq += 1` per round, so a
    // second `TurnStarted` carrying the first round's id is not a round at all — it is that
    // round taken twice, which the head now refuses to re-open (see
    // `a_boundary_the_head_has_already_had_does_not_open_a_second_run`). This fixture used to
    // hand both rounds `t1`, which no daemon does.
    let started = |seq, ts, id: &str, began: u64| {
        ServerFrame::Event(env_at(
            seq,
            ts,
            SessionEvent::TurnStarted {
                turn_id: id.into(),
                model: "m".into(),
                ledger_head: "0000".into(),
                began_ms: Some(began),
            },
        ))
    };
    let mut a = app();
    a.clock(1_000);
    a.apply(started(1, 1_000, "t1", 1_000));
    a.apply(ServerFrame::Event(env(
        2,
        testing::proposed_on("t1", "c1", "bash", "\"cargo test\""),
    )));
    a.apply(ServerFrame::Event(env(3, testing::turn_finished("t1"))));
    a.clock(5_000);
    let line = a.turn_status(120);
    assert!(
        line.contains("Responding"),
        "the row is still drawn: {line:?}"
    );
    assert!(
        line.contains("4.0s"),
        "and it counts from the prompt, through the call: {line:?}"
    );
    // The call lands and nothing else is outstanding, so the work is over and the row stands
    // down. **This is the tense's own question**: not *is there a turn open* but *is the model
    // working*, and here the answer is no until the next round starts.
    a.apply(ServerFrame::Event(env(
        4,
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
    assert!(
        !a.turn_busy(),
        "the call finished and nothing is generating"
    );
    assert!(
        a.turn_status(120).is_empty(),
        "the row stands down with the work"
    );
    // **And a second round does not move the base** (R51 item 2's *must not differ*). Round
    // two's `TurnStarted` arrives three seconds later carrying the SAME `began_ms`, and the clock
    // must not restart at it — that is the `2.1s` a minute into a turn the operator reported.
    a.apply(started(5, 4_000, "t2", 1_000));
    a.clock(6_000);
    let line = a.turn_status(120);
    assert!(
        line.contains("5.0s"),
        "the round boundary did not restart it: {line:?}"
    );
}

#[test]
fn the_composer_survives_a_narrow_terminal_and_a_short_one() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(
        &mut a,
        "a question long enough to wrap in a narrow terminal",
    );
    for w in [20usize, 30, 40, 60, 80, 200] {
        for h in [3usize, 5, 8, 12, 24, 60] {
            let screen = a.screen(w, h);
            assert_eq!(screen.len(), h, "w={w} h={h}");
            for l in &screen {
                assert!(
                    line_width(l) <= w,
                    "w={w} h={h}: {} cols: {l}",
                    line_width(l)
                );
            }
            let (row, col) = a.cursor().unwrap();
            assert!(row < h, "the caret is off the screen at w={w} h={h}");
            assert!(col < w, "the caret is off the right edge at w={w} h={h}");
            // The box is either whole or gone; never one wall of it.
            let top = screen.iter().filter(|l| l.contains('╭')).count();
            let bot = screen.iter().filter(|l| l.contains('╰')).count();
            assert_eq!(
                top,
                bot,
                "half a box at w={w} h={h}:\n{}",
                screen.join("\n")
            );
        }
    }
}

#[test]
fn the_prefill_line_reads_as_nearly_done_when_the_prompt_was_mostly_cached() {
    // The number this harness exists to move, on the screen while it is being
    // moved. Neither surveyed project can draw this line at all.
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::PromptProgress {
            turn_id: "t1".into(),
            progress: letibot_sessionlog::event::PromptProgress {
                total: 41_233,
                cache: 38_100,
                processed: 39_900,
                time_ms: 900,
            },
        },
    )));
    let line = a.turn_status(120);
    assert!(line.contains("prefill 97%"), "{line}");
    // The counts live in the header's ctx/cached readout; the line keeps what
    // the header cannot show — the expansion rate. 1,800 computed tokens in
    // 900 ms.
    assert!(line.contains("2000 tok/s"), "{line}");
    assert!(!line.contains("cached"), "{line}");
    // And it never wraps, at any width.
    for w in [24usize, 40, 60, 80, 120, 200] {
        assert!(line_width(&a.turn_status(w)) <= w, "w={w}");
    }
}

/// **A turn that only thinks and writes calls still draws its row, and the row still moves.**
///
/// MEASURED LIVE on the deepseek (`messages`) backend, 2026-09-27, and this test is that
/// measurement written down. The operator's screen read
///
/// ```text
///   ⠹ Responding · 2m19s · 145 chars
///   ⠼ Responding · 2m34s · 145 chars      header: 15.1s · 2826 out · 188 tok/s
/// ```
///
/// — the number frozen for two and a half minutes across a window in which the turn produced
/// 2826 output tokens. Their report: *"still thinking glued to Responding"*.
///
/// **Two answers came out of that, and this test pins the second.** The first was to make the
/// number count every channel instead of the answer's prose alone — that landed in `2aada71`
/// and was measured against the previous build (`107 chars` against `6384` on the same turn at
/// the same instant, `docs/evidence/turn-row-liveness-2026-09-27/`). The second is the
/// operator's ruling on what the row is for: *"i dont care about those chars"* / *"just dont
/// show me them"* — so **the number is gone entirely**, and what says the turn is alive is the
/// spinner and the clock, which move on the head's own clock whether or not any event arrives.
///
/// So the assertions below are that a turn made only of reasoning and tool-call deltas is still
/// drawn, still says `Responding`, carries no figure, and moves with nothing arriving — the
/// four things that have to hold together for that row to be honest on a backend that sends no
/// token counter at all.
#[test]
fn a_turn_that_only_thinks_and_writes_calls_still_draws_its_row() {
    let mut a = app();
    a.clock(1_000);
    // **Stamped, so the clock has something to count from.** `started_ms` comes from the
    // envelope's `ts` when the prompt's own `began_ms` is absent, and a turn with 0 there is
    // the snapshot case — the row says *"started before this head attached"* and has no
    // duration to move, which is honest but not what is under test.
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    assert!(a.turn_status(160).contains("Responding"));
    assert!(!a.turn_status(160).contains("chars"));

    // The reasoning — the first thing a `messages` backend streams.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Reasoning,
            text: "check the caller before the callee".into(),
        },
    )));
    // The tool-call markup — the channel an agentic round is made of.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::ToolCall,
            text: "{\"command\":\"cargo test\"}".into(),
        },
    )));
    // And the answer, for the ordinary case.
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Text,
            text: "here is the answer".into(),
        },
    )));
    let line = a.turn_status(160);
    assert!(line.contains("Responding"), "{line}");
    assert!(!line.contains("chars"), "the count is not drawn: {line}");
    assert!(!line.contains(" tok"), "nor the server's: {line}");

    // **And it is alive on the clock, with no event at all in between** — which is the whole of
    // what replaced the number: a spinner and a duration that move because the head reads its
    // own clock, rather than because something arrived.
    let glyph = |s: &str| s.chars().find(|c| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(*c));
    let first = a.turn_status(160);
    a.clock(4_000);
    let later = a.turn_status(160);
    assert_ne!(
        glyph(&first),
        glyph(&later),
        "the glyph moves with no event: {first} → {later}"
    );
    assert!(later.contains("3.0s"), "and the clock with it: {later}");
    // One row, at every width — the property its history warns about, because it was once a
    // legend on the composer's border where the edge truncated it.
    for w in [24usize, 40, 60, 80, 120, 200] {
        assert!(!a.turn_status(w).contains('\n'), "one row at w={w}");
    }
}

#[test]
fn a_turn_that_was_cut_short_does_not_read_like_one_that_finished() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::delta("t1", "half an ans"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TurnFinished {
            turn_id: "t1".into(),
            finish_reason: letibot_sessionlog::event::FinishReason::Length,
            usage: Default::default(),
            timings: Default::default(),
        },
    )));
    let screen = a.screen(120, 16).join("\n");
    assert!(screen.contains("CUT SHORT"), "{screen}");
}

/// Found by watching a decode run: the last line of the streaming answer sat
/// directly against whatever was drawn under it, and the blank appeared only
/// when the turn ended and the pane stood down. The reasoning block and the
/// call cards already carry their own trailing air, so the text block does
/// too now, and this pins it.
#[test]
fn a_running_decode_keeps_a_blank_line_above_what_follows_it() {
    let mut a = app();
    // The window has to be full for this to mean anything: a short transcript
    // is padded to the room the body has, and that padding would pass the
    // assertion with or without the fix. Forty settled rows push the running
    // answer to the bottom of the window, which is where the defect lived.
    let mut seq = 1;
    for i in 0..40 {
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(&format!("s.{i}"), "user"),
        )));
        seq += 1;
        a.apply(ServerFrame::Event(env(
            seq,
            testing::content(&format!("s.{i}"), "a line of earlier transcript"),
        )));
        seq += 1;
    }
    a.apply(ServerFrame::Event(env(seq, testing::turn_started("t1"))));
    seq += 1;
    a.apply(ServerFrame::Event(env(
        seq,
        testing::delta("t1", "half an answer"),
    )));
    let rows = a.screen(80, 24);
    let last_text = rows
        .iter()
        .rposition(|l| l.contains("half an answer"))
        .expect("the streaming answer is on the screen");
    let under = &rows[last_text + 1];
    assert!(
        under.trim().is_empty(),
        "a running decode is padded by a blank line, but this row follows it \
             directly: {under:?}"
    );
}

/// The defect the operator found by looking: *"thinking color rendering has
/// something unclosed in escapes — it tries to be gray, then say goes green
/// and becomes white for several rows and then gray again."*
///
/// It was not an unclosed escape and it was not a delta split mid-sequence. It
/// was a **closed** one: a styled span inside the reasoning block closed with
/// `\x1b[0m`, which restores the terminal default rather than the block. So
/// the check is not "is everything closed" — it is "does every close hand the
/// block's own style back".
///
/// A screenshot would not have stopped this coming back; this does.
#[test]
fn every_reset_in_a_reasoning_row_restores_the_reasoning_style() {
    let mut a = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::reasoning("t1", REASONING_WITH_MARKDOWN),
    )));
    a.key(Key::CtrlR);
    let rows = a.screen(100, 40);

    let reopen = rano::style::Palette::Colour.open(Role::Reasoning);
    let reset = rano::width::text::RESET;
    let rail: Vec<&String> = rows.iter().filter(|l| l.contains('┃')).collect();
    assert!(
        rail.len() >= 4,
        "the fixture must reach the screen as several rail rows:\n{}",
        rows.join("\n")
    );
    // The heading has to actually be styled, or this test would pass on a
    // renderer that had simply stopped colouring anything.
    assert!(
        rail.iter()
            .any(|l| l.contains(&rano::style::Palette::Colour.open(Role::Subheading))),
        "no heading was styled inside the reasoning; the fixture is not exercising the bug"
    );
    for l in rail {
        assert!(
            l.ends_with(reset),
            "a reasoning row ended with the block still open: {l:?}"
        );
        // The row's own close is a reset (and `wrap` may have added one of its
        // own), so the trailing run of them is the end of the row, not a leak.
        let body = l.trim_end_matches(reset);
        let mut at = 0;
        while let Some(hit) = body[at..].find(reset) {
            let after = at + hit + reset.len();
            assert!(
                body[after..].starts_with(&reopen),
                "a reset inside the reasoning left the block: the text after \
                     it is {:?}\nwhole row: {:?}",
                &body[after..body.len().min(after + 24)],
                l
            );
            at = after;
        }
    }
}

/// The third defect, from using it: *"tool calls — i see `<function…` like
/// strings first, then closing tag arrives and it becomes a toolcall."*
///
/// The head's half of the fix. The engine's half — that the markup arrives on
/// a channel of its own at all — is
/// `letibot-turn`'s `the_body_of_a_tool_call_is_never_announced_as_assistant_text`.
#[test]
fn the_raw_markup_of_a_tool_call_is_never_on_the_screen_by_default() {
    const MARKUP: &str = "\n<function=read>\n<parameter=path>\nsrc/main.rs\n</parameter>\n";
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::delta("t1", "Reading it now.\n\n"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::ToolCall,
            text: MARKUP.into(),
        },
    )));
    let default = a.screen(100, 30).join("\n");
    assert!(
        !default.contains("<function="),
        "the raw markup reached the default view:\n{default}"
    );
    assert!(
        default.contains("Reading it now."),
        "the answer went missing with it:\n{default}"
    );
    // …and the reader is told something is happening, which is the only thing
    // the markup was accidentally conveying.
    assert!(
        default.contains("writing a tool call"),
        "nothing stood in for the call being written:\n{default}"
    );

    // The operator kept the raw form deliberately: "I want to save the ability
    // to see raw tool calls but it should be behind some chord, different to
    // C-r."
    a.key(Key::CtrlX);
    let raw = a.screen(100, 30).join("\n");
    assert!(
        raw.contains("<function=read>") && raw.contains("<parameter=path>"),
        "ctrl-x revealed nothing:\n{raw}"
    );
    a.key(Key::CtrlX);
    assert!(
        !a.screen(100, 30).join("\n").contains("<function="),
        "ctrl-x does not toggle back off"
    );
}

/// The chord is not Ctrl+R — the operator ruled that out by name — and it is
/// not one the composer or the terminal already owns.
#[test]
fn the_raw_chord_is_its_own_key_and_reaches_nothing_else() {
    let mut a = app();
    typed(&mut a, "hello");
    a.key(Key::CtrlX);
    assert_eq!(a.input(), "hello", "ctrl-x typed into the composer");
    assert!(a.raw_calls, "ctrl-x did not toggle the raw view");
    assert_eq!(a.reasoning, Fold::Folded, "ctrl-x moved the thinking fold");
    assert_eq!(a.tools, Fold::Folded, "ctrl-x moved the tool-output fold");
}

/// **And where the ladder cannot draw the result, the chord says why instead of lying** — the
/// operator's rule for the whole rewrite: a state that changes nothing is worse than an
/// unfinished one, because it lies about the screen.
///
/// `read-edits` is `{edits: open}` and nothing else, so the thinking's chord there would be
/// `thinking=open` with `tools=hidden` — no rung draws it, and the set must not be stored. The
/// refusal has to name the switch AND the way back, which is one word of the same verb.
#[test]
fn the_thinking_chord_in_read_edits_says_why_rather_than_storing_it() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::READ_EDITS);
    a.key(Key::CtrlR);
    assert_eq!(
        a.visibility,
        Visibility::of(Profile::READ_EDITS),
        "a set no rung can draw must not be stored"
    );
    assert_eq!(
        a.reasoning,
        Fold::Folded,
        "and the mirror must not move either"
    );
    let screen = a.screen(100, 30).join("\n");
    // **The whole clause, and it has to fit inside the notice's 100 columns.** `say` draws the
    // sentence through `trim_to(…, w)`, so a refusal whose facts sit past the frame is a
    // refusal that lost them — measured here first, when the ladder came before the remedy.
    assert!(
        screen.contains("`thinking` is drawn while `tools` is off"),
        "the refusal must say which switch and which way round, inside the frame:\n{screen}"
    );
    assert!(
        screen.contains("thinking=hidden"),
        "and the way back, in the verb's own words — INSIDE the notice's own width, which \
             is why the remedy is the first clause:\n{screen}"
    );
    // **And only the switch BELOW it.** `system` sits above `thinking` and is not something
    // `thinking=open` needs, so naming it would be a requirement that is not one — the third
    // fault this test found, and the reason the assertion is a negative as well.
    assert!(
        !screen.contains("`system`"),
        "the refusal named a switch ABOVE the one it is about:\n{screen}"
    );
}

/// **Responding is green while the turn goes and yellow when it goes quiet, and nothing else
/// is said.** The operator, 2026-10-08: *"we have Responding in yellow which is a warning color
/// … I dont want notification that it is slow yet we continue … let usual Responding be green
/// and when we detect delays - yellow it"*. The sentence this replaced (*"nothing received for
/// 40.0s"*) dated from failed turns that never ended; they end with `TurnFailed` now.
#[test]
fn a_quiet_turn_turns_responding_yellow_and_says_nothing_more() {
    let mut a = App::new(RenderConfig {
        width: 120,
        color: true,
        ..RenderConfig::default()
    });
    a.clock(1_000);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    let responding = |a: &mut App| {
        a.screen(120, 12)
            .into_iter()
            .find(|l| l.contains("Responding"))
            .unwrap_or_default()
    };
    let going = responding(&mut a);
    assert!(
        going.contains(sgr::GREEN),
        "a turn that is going is green: {going:?}"
    );
    assert!(!going.contains(sgr::YELLOW), "{going:?}");
    a.clock(1_000 + 40_000);
    let quiet = responding(&mut a);
    assert!(
        quiet.contains(sgr::YELLOW),
        "a quiet turn is yellow: {quiet:?}"
    );
    let screen = a.screen(120, 12).join("\n");
    assert!(
        !screen.contains("nothing received"),
        "and says nothing more:\n{screen}"
    );
}

#[test]
fn a_resync_replaces_state_rather_than_appending_to_it() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(testing::delta("t1", "abc"));
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(2, testing::delta("t1", "abc"))));
    a.apply(ServerFrame::Resync {
        reason: "queue overflow".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    assert_eq!(a.turn.as_ref().unwrap().text.raw(), "abc", "not abcabc");
    assert_eq!(a.resyncs, 1);
}

#[test]
fn a_rounds_prose_moves_into_the_transcript_rather_than_being_copied_into_it() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    for w in ["I'll take ", "a look ", "at the tree first."] {
        a.apply(ServerFrame::Event(env(2, testing::delta("t1", w))));
    }
    // Streaming: the pane is the only place it exists, and it is showing.
    let live = a.screen(120, 30).join("\n");
    assert_eq!(live.matches("at the tree first.").count(), 1, "{live}");
    a.apply(ServerFrame::Event(env(
        3,
        testing::appended("t1.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::TranscriptContent {
            item_id: "t1.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "I'll take a look at the tree first.".into(),
                tool_calls: vec![],
                truncated: false,
            }),
        },
    )));
    // Settled: still once, and now in the transcript where it belongs.
    let settled = a.screen(120, 30).join("\n");
    assert_eq!(
        settled.matches("at the tree first.").count(),
        1,
        "the pane kept a copy of what the transcript took over:\n{settled}"
    );
    // The next round's prose still streams into the pane.
    a.apply(ServerFrame::Event(env(
        5,
        testing::delta("t1", "And now the answer."),
    )));
    let next = a.screen(120, 30).join("\n");
    assert!(next.contains("And now the answer."), "{next}");
    assert_eq!(next.matches("at the tree first.").count(), 1, "{next}");
}

#[test]
fn the_session_header_degrades_by_deletion_and_never_wraps() {
    // It handed both halves to `split_row`, which drops the **whole** right one
    // when they do not both fit — correct for the in-flight line and wrong here,
    // where the right half is the part you cannot get anywhere else. Measured
    // under tmux at 110 columns: an 82-column path plus a 27-column tail is 111,
    // and the entire tail vanished with nothing to say it had.
    let mut a = app();
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(SessionEvent::TurnFinished {
        turn_id: "t1".into(),
        finish_reason: letibot_sessionlog::event::FinishReason::Eos,
        usage: Usage {
            prompt_tokens: 41_233,
            cached_tokens: 38_100,
            predicted_tokens: 200,
            cost_micros_usd: None,
        },
        timings: Default::default(),
    });
    a.apply(hello(
        "s",
        vec![
            brief("s", "the cache question", false),
            brief("s2", "", false),
        ],
        hub.snapshot(),
    ));
    for w in [40usize, 60, 80, 110, 200] {
        let l = a.header_line(w);
        assert!(line_width(&l) <= w, "w={w}: {} cols: {l}", line_width(&l));
        // The session index is the field that survives longest: with several
        // sessions, "which one is this" is the question the header exists for.
        assert!(l.contains("1/2"), "w={w}: {l}");
        if w >= 80 {
            assert!(l.contains("41.2k ctx"), "w={w}: {l}");
        }
    }
}

#[test]
fn the_turns_numbers_live_in_the_header_and_an_ordinary_ending_has_no_footer() {
    // One fact, one place. The footer used to close with `45 tok/s · 12.3s ·
    // 1.2k out` on a line that also repeated the header's context and cache
    // numbers; the duplicates went, the measurements moved up, and an
    // ordinary ending now leaves the body with no footer line at all — a
    // settled fact was occupying the row a live fact used to have to earn.
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TurnFinished {
            turn_id: "t1".into(),
            finish_reason: letibot_sessionlog::event::FinishReason::Eos,
            usage: Usage {
                prompt_tokens: 41_233,
                cached_tokens: 38_100,
                predicted_tokens: 1_200,
                cost_micros_usd: None,
            },
            timings: letibot_sessionlog::event::Timings {
                prompt_ms: 900.0,
                predicted_ms: 2_000.0,
                wall_ms: 12_300,
            },
        },
    )));
    let header = a.header_line(200);
    assert!(header.contains("600 tok/s"), "{header}");
    assert!(header.contains("12.3s"), "{header}");
    assert!(header.contains("1200 out"), "{header}");
    assert!(header.contains("41.2k ctx"), "{header}");
    // An ordinary ending says nothing at all on the body.
    let screen = a.screen(120, 30);
    assert!(
        !screen.iter().any(|l| l.contains("── ")),
        "an ordinary ending has no footer: {screen:?}"
    );
}

/// **While a turn runs, the header's duration IS the turn's — the two clocks are one number.**
///
/// MEASURED on the other head's live screen, and this head had the identical pair: the composer
/// row showed `Responding · 151s` (the turn, from `TurnStarted.began_ms`) while the header showed
/// `· 4.5s ·` — `last_timings.wall_ms`, which is measured when a ROUND ends. A round ends at every
/// tool call and every job, so the header's number restarted three times in one turn, and it is
/// the number sitting next to the word *Responding*, where a bare duration reads as a clock.
/// The operator read it that way: *"its responding timer resets not at the turn end but on jobs
/// and tool calls."*
///
/// **The ruling is the operator's**: running → the turn's elapsed; idle → the last turn's
/// wall-ms. The alternative was proposed and refused twice in other shapes, so it is recorded
/// rather than re-derived.
///
/// # The assertion is the AGREEMENT, not a value
///
/// Both timers derive from the turn's start while a turn runs, so what has to hold is that they
/// are **equal**. The disagreement is what the operator saw — 151s beside 4.5s — so equality is
/// the fix stated as a property rather than as a number somebody has to keep in step. A test
/// asserting `4.0s` would pass while the two drifted apart on any other clock.
#[test]
fn while_a_turn_runs_the_header_and_the_turn_row_show_the_same_clock() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    // A round finishes, so `last_timings` carries a SHORT wall-ms — the number that used to be
    // the header's, and the whole of the defect.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TurnFinished {
            turn_id: "t1".into(),
            finish_reason: letibot_sessionlog::event::FinishReason::Eos,
            usage: Usage {
                prompt_tokens: 41_233,
                cached_tokens: 38_100,
                predicted_tokens: 900,
                cost_micros_usd: None,
            },
            timings: letibot_sessionlog::event::Timings {
                prompt_ms: 900.0,
                predicted_ms: 4_474.0,
                wall_ms: 4_474,
            },
        },
    )));
    // A call of that round is still running, which is what makes the turn BUSY across the round
    // boundary — the case the two clocks disagree in.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::ToolCallProposed {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            args_digest: "d".into(),
            target: "cargo test".into(),
        },
    )));
    a.clock(151_204);

    assert!(a.turn_busy(), "the premise: a turn is running");
    let row = a.turn_status(200);
    let header = a.header_line(200);

    // **The row says the turn's elapsed**, and it is the number the header must agree with.
    // 151204 - 1000 = 150204 ms, and both renderers spell that `2m30s`.
    assert!(row.contains("2m30s"), "the row's clock: {row}");
    // The header used to say the round's `4.5s` here. It must agree with the row instead — and
    // the assertion that matters is the AGREEMENT, so the string is taken from the row rather
    // than typed twice: two literals that happen to match today are two literals to keep in step.
    assert!(
        !header.contains("4.5s"),
        "the header still shows the last ROUND's wall-ms beside a running turn: {header}"
    );
    assert!(
        header.contains("2m30s"),
        "the header does not show the turn's elapsed: {header}"
    );

    // **And the rate and the count did NOT move with it.** `last_timings` is what they are for,
    // and the operator's earlier complaint about them was *"the rate comes and goes"* — so the
    // rate stays on the round that just ended while the duration becomes the turn's.
    assert!(
        header.contains("tok/s"),
        "the rate went missing with the duration: {header}"
    );
    assert!(header.contains("900 out"), "{header}");
}

#[test]
fn the_decode_line_does_not_repeat_the_prompts_cache_numbers() {
    // While the answer decodes, the header already carries the prompt's size
    // and cache fraction — live prefill numbers win there for the whole
    // turn — so the in-flight line says only what it alone knows.
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::PromptProgress {
            turn_id: "t1".into(),
            progress: letibot_sessionlog::event::PromptProgress {
                total: 41_233,
                cache: 38_100,
                processed: 41_233,
                time_ms: 900,
            },
        },
    )));
    let line = a.turn_status(120);
    // The header is where those numbers live.
    let header = a.header_line(200);
    assert!(header.contains("41.2k ctx"), "{header}");
    assert!(header.contains("92% cached"), "{header}");
    // **And this row repeats none of them, and carries no figure of its own** — the operator's
    // ruling of 2026-09-27 is that the row says the turn is alive and nothing more: *"i dont
    // care about those chars"*. The assertions are kept for the fields that were here
    // (`5 chars` after a delta, `42 tok` from the server's counter) precisely because those
    // are the ones a future change might put back.
    a.apply(ServerFrame::Event(env(3, testing::delta("t1", "hello"))));
    a.apply(ServerFrame::Event(env(
        4,
        testing::tokens_generated("t1", 42),
    )));
    let line = a.turn_status(120);
    assert!(!line.contains("chars"), "{line}");
    assert!(!line.contains(" tok"), "{line}");
    assert!(!line.contains("prompt"), "{line}");
    assert!(!line.contains("cached"), "{line}");
    assert!(!line.contains("  "), "no justification padding: {line}");
}

#[test]
fn no_counter_rides_the_status_line_and_a_dead_one_never_did() {
    // **This test used to be called `…wins_over_the_char_count`, and the question it settled
    // is gone.** Both counters were removed from the row on 2026-09-27 — the operator's ruling,
    // *"i dont care about those chars"* — so what is worth pinning now is the absence: neither
    // the server's counter nor the character fallback reaches this row, through every event
    // that used to put one there.
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::PromptProgress {
            turn_id: "t1".into(),
            progress: letibot_sessionlog::event::PromptProgress {
                total: 100,
                cache: 0,
                processed: 100,
                time_ms: 10,
            },
        },
    )));
    let line = a.turn_status(120);
    assert!(!line.contains("tok"), "{line}");
    assert!(!line.contains("chars"), "{line}");
    // The counter arrives — and is not drawn, though it still counts as a frame the turn is
    // alive on, which is why `TokensGenerated` is still `Rendered` in the fold.
    assert_eq!(
        a.apply(ServerFrame::Event(env(
            3,
            testing::tokens_generated("t1", 1234),
        ))),
        Disposition::Rendered,
        "a generated-token frame is still a frame in which the row is alive"
    );
    let line = a.turn_status(120);
    assert!(!line.contains("1234"), "{line}");
    // And text arriving moves nothing onto this row either.
    a.apply(ServerFrame::Event(env(4, testing::delta("t1", "hello"))));
    let line = a.turn_status(120);
    assert!(!line.contains("tok"), "{line}");
    assert!(!line.contains("chars"), "{line}");
    // What it DOES carry, whatever arrives: the word, the clock, the spinner.
    assert!(line.contains("Responding"), "{line}");
    assert!(line.contains('·'), "{line}");
}

#[test]
fn the_spinner_spins_on_the_heads_clock_not_on_the_daemons_events() {
    // A spinner keyed off the last event's timestamp is not a spinner, it is
    // a snapshot of one: a tool running thirty silent seconds froze it on one
    // glyph, and the frozen duration beside it read as a dead turn. The phase
    // and the duration both run on `now_ms`, which the driver advances every
    // tick whether or not anything arrived.
    let mut a = app();
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    a.clock(1_000);
    let first = a.turn_status(120);
    assert!(first.contains("Responding"), "{first}");
    assert!(first.contains("0ms"), "{first}");
    a.clock(1_160);
    let second = a.turn_status(120);
    assert!(second.contains("160ms"), "the duration ticks: {second}");
    let glyph = |s: &str| s.chars().find(|c| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(*c));
    assert_ne!(
        glyph(&first),
        glyph(&second),
        "the glyph moved with no event in between: {first} → {second}"
    );
    // **And it is a ROW of its own, immediately above the box** (R51 item 1) — not inlaid in
    // the bottom border, which is where it used to be and where an edge truncated it.
    let screen = a.screen(100, 24);
    let at = screen
        .iter()
        .position(|l| l.contains("Responding"))
        .expect("the turn's status is on the screen");
    let row = &screen[at];
    assert!(!row.contains('╰'), "not inlaid in the bottom edge: {row}");
    assert!(
        !row.contains('╭'),
        "not inlaid in the top edge either: {row}"
    );
    // The row above it is the box's top edge, so `Responding` sits ON the composer.
    let below = &screen[at + 1];
    assert!(
        below.contains('╭'),
        "the status row's next line is the box top: {row} / {below}"
    );
    // And the bottom border no longer carries it — the edge that used to hold it is empty of
    // the turn's words, which is what makes this a moved row rather than a second copy.
    let bottom = screen
        .iter()
        .find(|l| l.contains('╰'))
        .expect("the box is closed");
    assert!(
        !bottom.contains("Responding"),
        "the turn's status left the bottom edge: {bottom}"
    );
}

/// **And the counts move as the round runs** — *"obviously be updated earlier."*
///
/// A second call proposed on the same turn takes the count to two with no new row landing,
/// and the thinking streamed but not committed is counted the same way.
#[test]
fn the_counts_move_as_the_round_runs() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Text,
            text: "working:".into(),
        },
    )));
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
                text: "working:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    let count = |a: &mut App| {
        a.invalidate_history();
        let s = a.screen(120, 30).join("\n");
        s.lines()
            .find(|l| l.contains("tool call") || l.contains("thinking line"))
            .map(str::to_string)
    };
    a.apply(ServerFrame::Event(env(
        5,
        testing::proposed_on("t1", "c1", "bash", "cargo test"),
    )));
    assert!(
        count(&mut a).is_some_and(|l| l.contains("[1 tool call]")),
        "one call proposed is one count: {:?}",
        count(&mut a)
    );
    a.apply(ServerFrame::Event(env(
        6,
        testing::proposed_on("t1", "c2", "bash", "cargo build"),
    )));
    assert!(
        count(&mut a).is_some_and(|l| l.contains("[2 tool calls]")),
        "the second call moved it, with no row landing: {:?}",
        count(&mut a)
    );
    // **And the thinking the turn has streamed**, which is also not a row yet.
    a.apply(ServerFrame::Event(env(
        7,
        SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Reasoning,
            text: "I should check the tests first".into(),
        },
    )));
    assert!(
        count(&mut a).is_some_and(|l| l.contains("2 tool calls") && l.contains("thinking line")),
        "the streamed reasoning is not counted: {:?}",
        count(&mut a)
    );
}

#[test]
fn a_marker_of_thinking_alone_goes_pending_nowhere() {
    let mut a = app();
    a.cfg.color = true;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Delta {
            turn_id: "t1".into(),
            target: letibot_sessionlog::event::DeltaTarget::Reasoning,
            text: "working it out".into(),
        },
    )));
    assert!(
        a.live_work_now().think_lines > 0,
        "the premise: there is thinking to count"
    );
    assert!(
        !marker_carries_live(a.live_work_now()),
        "a thought is not a call, so there is no number to colour"
    );
}

/// **Nothing the model says reaches the screen before the prompt that caused it** — R2.
///
/// The operator, twice in one session: *"i also saw my queued message go up before being
/// dequeued"* and then, with the order written out: *"my message | your line | and only then
/// unqueued"*. Which is a claim about THREE events, and only one of them is the head's:
///
///   1. their prompt goes to the daemon and the echo says `queued`;
///   2. the answer streams — `Delta`, which carries its text;
///   3. the prompt's row is announced — `TranscriptAppended`, which carries NO text — and its
///      body follows later on `TranscriptContent`.
///
/// The model's reply streams while the prompt's row is still only announced, so a head that
/// waits for the BODY to stop saying `queued` is showing the answer to a question it has not
/// drawn yet. R2's fix is optimistic binding: on the body-less `user` row, bind the oldest
/// pending echo to it and draw the row from that text.
///
/// **What this test measures is the mark, because that is what the operator saw.** Their words
/// are the assertion: after the row is announced and before its body arrives, the surface must
/// not still be saying `queued` about words the model has already been given.
/// **The turn's own row is reserved, so a turn starting does not move the screen.**
///
/// The operator: *"keep the line reserved for `Responding...` always free, or we have these ugly
/// jumps"*. The row exists only while a turn does, so without the reservation every row above
/// it — the transcript a reader is reading — moves by one the moment a turn begins, and moves
/// back when it ends.
///
/// **What is pinned is the MOVE, not the row.** Asserting *the row is always there* would pass
/// on a frame that reserved it in the wrong place; what the reader feels is the shove, so the
/// test compares the rows that are not the status line across the turn boundary.
/// **A delta changes the counts and nothing else** — the operator's rule for the rendered
/// history, as a measurement.
///
/// Their words: *"once something is rendered nothing left to it except tool calls / thinking
/// lines count shouldn't ever change by itself, without say me toggling verbosity."* So this
/// renders a frame, streams one reasoning chunk, renders again, and compares **line by line**:
/// every line but one must be byte-identical, and the one that differs must be the marker.
///
/// **What it would catch.** A mark that changes as a fact settles, a line that grows, a count
/// that appears late — each of them shows up here as a second differing line, which is the
/// assertion's whole point. The three defects this window has had (the backfilled count, the
/// yellow that needed a row, the echo that kept saying `queued`) were all *one row changing
/// without being asked*, and each would fail this.
///
/// Not asserted here: the verbosity toggle, which is the one case where every row may change.
/// **The thinking count never falls** — including when its reasoning lands as a row.
///
/// The operator: *"lol, just saw how thinking lines count went from 22 to 15."* The arithmetic
/// cannot produce that: `reasoning_display_lines` sums `ceil(width / cols)`, so text arriving can
/// only add lines. What the number was counting was the reasoning **plus the rows that reasoning
/// had already become**, and the round boundary — which rebuilds the pane, empty — took the
/// inflation away. A count of work done fell, which is the defect leticl measured on the calls
/// side, quoted in `LiveWork`'s own doc.
///
/// So this drives the whole life of one piece of reasoning: streamed, then landed as a row, then
/// a new round. The count must climb and then hold — never fall.
#[test]
fn the_thinking_count_never_falls_not_even_when_its_reasoning_lands() {
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
            text: "let me think:".into(),
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
    let shown = |a: &mut App| -> usize {
        a.screen(100, 30)
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
    // Stream four paragraphs, watching each frame.
    let mut last = 0;
    for i in 0..4u64 {
        a.apply(ServerFrame::Event(env(
            3 + i,
            SessionEvent::Delta {
                turn_id: "r1".into(),
                target: DeltaTarget::Reasoning,
                text: "y".repeat(300),
            },
        )));
        let now = shown(&mut a);
        assert!(
            now >= last,
            "the count fell while streaming: {last} → {now}"
        );
        last = now;
    }
    assert!(last > 0, "the stream was never counted at all");

    // **And its row lands.** The transcript now holds the reasoning, so the live part must stop
    // counting it — while the TOTAL, which is row plus live, must not move.
    a.apply(ServerFrame::Event(env(
        20,
        testing::appended("s.1", "reasoning"),
    )));
    a.record_item(
        "s.1",
        TranscriptItem::Reasoning {
            text: "y".repeat(1200),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    let after_landing = shown(&mut a);
    assert!(
        after_landing >= last,
        "the count fell when its reasoning landed as a row — it was counting the same work \
             twice: {last} → {after_landing}"
    );

    // **And the next round rebuilds the pane**, which is when the inflated number collapsed:
    // the committed row still counts, and nothing else disappears with the pane.
    a.apply(ServerFrame::Event(env_at(
        21,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r2".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    let next_round = shown(&mut a);
    assert!(
        next_round >= last,
        "the count fell at the round boundary: {last} → {next_round}"
    );
}

#[test]
fn the_thinking_count_moves_while_the_thinking_streams() {
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
            text: "let me look:".into(),
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

    // **The count, read off the marker itself** — the same arithmetic `live_work` does, taken
    // from the row a reader is looking at rather than from the field behind it.
    let shown = |a: &mut App| -> usize {
        let screen = a.screen(100, 24).join("\n");
        screen
            .lines()
            .find_map(|l| {
                let (_, rest) = l.split_once('[')?;
                let body = rest.split(']').next()?;
                body.split(", ")
                    .find_map(|p| p.trim().strip_suffix(" thinking line"))
                    .or_else(|| {
                        body.split(", ")
                            .find_map(|p| p.trim().strip_suffix(" thinking lines"))
                    })
                    .and_then(|n| n.trim().parse().ok())
            })
            .unwrap_or(0)
    };

    // Nothing yet.
    assert_eq!(shown(&mut a), 0, "no thinking has arrived");
    // **And it grows with the stream**, a paragraph at a time — long enough that the wrapped
    // count must change, and with NO answer text at all.
    let mut last = 0;
    for i in 0..6 {
        let chunk = "x".repeat(200);
        a.apply(ServerFrame::Event(env(
            3 + i,
            SessionEvent::Delta {
                turn_id: "r1".into(),
                target: DeltaTarget::Reasoning,
                text: chunk,
            },
        )));
        let now = shown(&mut a);
        assert!(
            now > last,
            "the count did not move on chunk {i}: {last} → {now}"
        );
        last = now;
    }
    assert!(last >= 6, "and it is the stream's own size: {last}");
}

/// **A boundary NOBODY MEASURED is the same run, not a new one.**
///
/// This is the frame the operator's screen was built from, and it is the daemon's own shape:
/// `harnessd::sessions::run_prompt` stamps the whole turn's start (`begin_turn_clock`) and
/// **`Sessions::wake` does not** — so the turn a job's settlement or a monitor's firing opens
/// publishes `TurnStarted { began_ms: None }`, which is the one value the head used to read as
/// *a new prompt*.
///
/// Read that way it emptied `turn_rows` and moved `started_ms` to the event's `ts`, so the run
/// the work is still in stopped reading as this turn's (`live_here` and `walk_carried_live`
/// both no), the pane drew its own marker beside the walk's, and the `Responding` clock
/// restarted — *"it looks like turn end or some other border is misinterpreted and Responding
/// timer resets and I get new line with `[N thinking lines]` which then gets merged to the
/// previous `[N tools, M thinking]`"*. The merge is the next row landing.
#[test]
fn a_boundary_nobody_measured_does_not_split_the_run() {
    let mut a = app();
    a_turn_mid_run(&mut a);
    a.clock(5_000);
    let before = a.screen(100, 30).join("\n");
    // The premise, so a failure below is about the boundary and not about the fixture.
    assert_eq!(markers(&before), 1, "one run, one marker: {before}");
    assert!(
        before.contains("1 tool call, 2 thinking lines"),
        "and the run carries the work in flight: {before}"
    );
    assert!(
        a.turn_status(120).contains("4.0s"),
        "counting from the prompt: {}",
        a.turn_status(120)
    );

    // **The boundary, with no start in it at all.**
    a.apply(ServerFrame::Event(env_at(
        5,
        9_000,
        SessionEvent::TurnStarted {
            turn_id: "r2".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: None,
        },
    )));
    // **And the thinking goes on across the boundary**, which is the whole point of it: the
    // model is mid-thought when the event arrives and keeps streaming under the new round's
    // id. It is also a DIFFERENT number of lines from the thinking before the boundary, so
    // the assertion below cannot be satisfied by a row the last frame had already cached —
    // the count has to be rebuilt from the run the work is in.
    a.apply(ServerFrame::Event(env(
        6,
        SessionEvent::Delta {
            turn_id: "r2".into(),
            target: DeltaTarget::Reasoning,
            text: "the call came back saying it is in the reader\n\
                       and the caller is the place it is constructed\n\
                       so the next thing to look at is who hands it over"
                .into(),
        },
    )));
    a.clock(9_000);
    let after = a.screen(100, 30).join("\n");
    assert!(
        !after.contains("] ["),
        "one run is ONE marker: the pane drew a second one beside the walk's, which is the \
             pair the operator reported (`[N thinking lines]` beside `[N tools, M thinking]`): \
             {after}"
    );
    assert!(
        after.contains("1 tool call, 3 thinking lines"),
        "and the run's counts carry the thinking that is in flight NOW, not the thinking that \
             was in flight when the frame before it was drawn: {after}"
    );
    let status = a.turn_status(120);
    assert!(
        status.contains("8.0s"),
        "the clock keeps counting from the prompt that is running rather than restarting at \
             the frame that arrived: {status}"
    );
}

/// **A subagent whose whole product is prose is readable.**
///
/// The peek rendered tool results and nothing else, so a `digest` subagent —
/// handed a slice of transcript in its prompt, answering in text, calling no
/// tools by design — showed "no tool output" and threw its report away.
/// Measured in the operator's store: six of them, one user item, one
/// reasoning item and one assistant item each, zero tool results. *"i see
/// them and i see their output but when i enter - no output"*.
#[test]
fn a_prose_only_subagent_shows_its_answer_and_not_its_thinking() {
    let content = |id: &str, item: TranscriptItem| {
        env(
            1,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(item),
            },
        )
    };
    let events = vec![
        content(
            "i0",
            TranscriptItem::Reasoning {
                text: "let me weigh this up at length".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
        ),
        content(
            "i1",
            TranscriptItem::Assistant {
                text: "**Nothing in this part bears on (a) or (c).**\nThe only lazy-loading \
                           language is about background-job output."
                    .into(),
                tool_calls: Vec::new(),
                truncated: false,
            },
        ),
    ];
    let lines = subagent_out_lines(&events).join("\n");
    assert!(lines.contains("Nothing in this part bears"), "{lines}");
    assert!(lines.contains("background-job output"), "{lines}");
    assert!(
        !lines.contains("weigh this up"),
        "the thinking is not the answer: {lines}"
    );
}

/// **A message taken from the queue mid-turn lands BELOW the reply it interrupted, and never
/// above it first.** The operator, 2026-10-08: *"when it dequeued during active turn it can go
/// above the most recent piece of reply and then get reordered to the bottom. very strange
/// feeling."* At the step boundary the daemon announces the round's assistant row and then the
/// taken prompt's row; the reply's TEXT is already on screen (it streamed), its row's BODY
/// arrives a moment later. Every frame in between must keep the reply above the message.
#[test]
fn a_message_taken_mid_turn_never_jumps_above_the_reply_it_follows() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::delta("t1", "the reply so far."),
    )));
    // The operator types a follow-up while the reply streams: it is queued.
    typed(&mut a, "a follow-up");
    a.key(Key::Enter);
    let order = |a: &mut App, when: &str| {
        let s = a.screen(120, 30).join("\n");
        let reply = s.find("the reply so far.");
        let msg = s.find("a follow-up");
        if let (Some(r), Some(m)) = (reply, msg) {
            assert!(
                r < m,
                "{when}: the message is drawn ABOVE the reply it follows:\n{s}"
            );
        }
        s
    };
    order(&mut a, "queued");
    // The boundary: the round's assistant row is announced (no body yet)…
    a.apply(ServerFrame::Event(env(
        3,
        testing::appended("t1.0", "assistant"),
    )));
    order(&mut a, "assistant row announced");
    // …then the taken prompt's row, with its body.
    a.apply(ServerFrame::Event(env(
        4,
        testing::appended("t1.1", "user"),
    )));
    order(&mut a, "user row announced");
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::TranscriptContent {
            item_id: "t1.1".into(),
            item: Box::new(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: "a follow-up".into(),
                }],
            }),
        },
    )));
    order(&mut a, "user row's body landed");
    // …and last, the assistant row's body.
    a.apply(ServerFrame::Event(env(
        6,
        SessionEvent::TranscriptContent {
            item_id: "t1.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "the reply so far.".into(),
                tool_calls: vec![],
                truncated: false,
            }),
        },
    )));
    let settled = order(&mut a, "settled");
    assert_eq!(settled.matches("the reply so far.").count(), 1, "{settled}");
    assert_eq!(settled.matches("a follow-up").count(), 1, "{settled}");
}
