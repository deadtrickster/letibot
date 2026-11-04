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
    // The count belongs where the disclosure is READ - `/status` - and not on
    // the bottom border: the rule asks that it exist, be exact and be reachable,
    // never that a resident row of zeros sit beside the prompt.
    a.command("status");
    let screen = a.screen(120, 40).join("\n");
    assert!(screen.contains("filtered"), "{screen}");
    assert!(screen.contains("10 (terse)"), "{screen}");
    // And a head that has lost nothing says nothing on the border.
    assert_eq!(a.status_line(200), "", "a clean head has a clean border");
}

/// A turn whose only work is a running tool call can still be interrupted:
/// `TurnFinished` fires per ROUND, so the state name reads `finished` from the
/// instant a call starts, and a gate on it leaves esc esc dead throughout.
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

/// The status row is drawn for the whole life of the turn: through a call, and across the round boundary after it.
/// A round's `TurnFinished` says `ends_turn: false`, so nothing is written about an ending until the daemon marks
/// the PROMPT's end, or `Responding → Responded → Responding` flickers across every round boundary.
#[test]
fn the_status_row_survives_a_tool_call() {
    // `began_ms` is the PROMPT's stamp, the same on every round of it, which is
    // what makes the clock the turn's rather than a round's. The id is the ROUND's,
    // and a second `TurnStarted` with the same one is that round taken twice.
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
    // The round's generation ends, and the frame says which ending it is: a ROUND of a turn that
    // runs on, because the call it proposed is the caller's to run.
    a.apply(ServerFrame::Event(env(
        3,
        testing::turn_finished_round("t1"),
    )));
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
    // The call lands, so nothing is generating and no call is unfinished - the window the round
    // event alone cannot distinguish. The daemon said the turn runs on, so the row does.
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
        a.turn_busy(),
        "the round said the prompt was not over, and nothing else is outstanding"
    );
    assert!(
        !a.turn_status(120).is_empty(),
        "so the row is still drawn: {:?}",
        a.turn_status(120)
    );
    // And a second round does not move the base. Round
    // two's `TurnStarted` arrives three seconds later carrying the SAME `began_ms`, and the clock
    // must not restart at it - that is the `2.1s` a minute into a turn the operator reported.
    a.apply(started(5, 4_000, "t2", 1_000));
    a.clock(6_000);
    let line = a.turn_status(120);
    assert!(
        line.contains("5.0s"),
        "the round boundary did not restart it: {line:?}"
    );
    // And the MARKED end is the only frame that stands it down.
    a.apply(ServerFrame::Event(env(6, testing::turn_finished("t2"))));
    assert!(!a.turn_busy(), "the prompt is over");
    assert!(
        a.turn_status(120).is_empty(),
        "the row stands down with the turn: {:?}",
        a.turn_status(120)
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
    // the header cannot show - the expansion rate. 1,800 computed tokens in
    // 900 ms.
    assert!(line.contains("2000 tok/s"), "{line}");
    assert!(!line.contains("cached"), "{line}");
    // And it never wraps, at any width.
    for w in [24usize, 40, 60, 80, 120, 200] {
        assert!(line_width(&a.turn_status(w)) <= w, "w={w}");
    }
}

/// A turn that only thinks and writes calls still draws its row, and the row still moves.
///
/// MEASURED LIVE on the deepseek (`messages`) backend, and this test is that
/// measurement written down. The operator's screen read
///
/// ```text
///   ⠹ Responding · 2m19s · 145 chars
///   ⠼ Responding · 2m34s · 145 chars      header: 15.1s · 2826 out · 188 tok/s
/// ```
///
/// The number was frozen for two and a half minutes while the turn produced 2826 output tokens (*"still thinking glued to Responding"*).
/// Two answers: the number counts every channel (`107 chars` against `6384` on the same turn), and, the operator's ruling, the number
/// is gone entirely; the spinner and clock move on the head's own clock. So a turn of only reasoning and tool-call deltas is still
/// drawn, says `Responding`, carries no figure, and moves with nothing arriving.
#[test]
fn a_turn_that_only_thinks_and_writes_calls_still_draws_its_row() {
    let mut a = app();
    a.clock(1_000);
    // Stamped, so the clock has something to count from: `started_ms` falls back
    // to the envelope's `ts`, and a turn with 0 there is the snapshot case, whose
    // row says the turn started before this head attached and has no duration.
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    assert!(a.turn_status(160).contains("Responding"));
    assert!(!a.turn_status(160).contains("chars"));

    // The reasoning - the first thing a `messages` backend streams.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Reasoning,
            text: "check the caller before the callee".into(),
        },
    )));
    // The tool-call markup - the channel an agentic round is made of.
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

    // And it is alive on the clock, with no event at all in between - which is the whole of
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
    // One row, at every width - the property its history warns about, because it was once a
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
            ends_turn: true,
            prefix_check: None,
            dialect_template_sha: String::new(),
        },
    )));
    let screen = a.screen(120, 16).join("\n");
    assert!(screen.contains("CUT SHORT"), "{screen}");
}

/// The last line of a streaming answer must not sit against what is drawn under
/// it: the blank would otherwise appear only when the turn ends and the pane
/// stands down. The reasoning block and the call cards already carry that air.
#[test]
fn a_running_decode_keeps_a_blank_line_above_what_follows_it() {
    let mut a = app();
    // The window has to be full for this to mean anything: a short transcript is
    // padded to the room the body has, and that padding alone would pass the
    // assertion. Forty rows push the running answer to the bottom.
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

/// Every reset inside a reasoning row must hand the block's own style back:
/// `\x1b[0m` restores the terminal default, so the check is not "is every span
/// closed" but what each close leaves behind.
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

/// A tool call's raw markup is never on the screen by default: the engine keeps
/// it off the assistant-text channel
/// (`the_body_of_a_tool_call_is_never_announced_as_assistant_text`).
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

/// The chord is not Ctrl+R - the operator ruled that out by name - and it is
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

/// Where the ladder cannot draw a set, the verb says why instead of storing it:
/// `read-edits` is `{edits: open}` alone, so `thinking=open` there would be with
/// `tools=hidden`, and the refusal must name the switch and the way back.
#[test]
fn the_thinking_verb_in_read_edits_says_why_rather_than_storing_it() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::READ_EDITS);
    assert_eq!(a.command("verbosity thinking=open"), None);
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
    // The whole clause, a click away when it does not fit. `say` draws the sentence
    // cut to the frame, and a cut one ends `· click for details` and opens whole on a click  -
    // before, a refusal whose facts sat past the frame was a refusal that lost them.
    let hit = a
        .hits
        .iter()
        .find(|h| h.target == crate::app::hits::Target::FlashNotice)
        .unwrap_or_else(|| panic!("the cut notice is not a click target:\n{screen}"))
        .clone();
    assert!(screen.contains("· click for details"), "{screen}");
    a.key(Key::Click {
        x: 10,
        y: hit.rect.y as u16,
    });
    let whole = super::redcard::popup_text(&mut a);
    assert!(
        super::redcard::holds(&whole, "`thinking` is drawn while `tools` is off"),
        "the refusal must say which switch and which way round:\n{whole}"
    );
    a.key(Key::Esc);
    assert!(
        screen.contains("thinking=hidden"),
        "and the way back, in the verb's own words — INSIDE the notice's own width, which \
             is why the remedy is the first clause:\n{screen}"
    );
    // And only the switch BELOW it. `system` sits above `thinking` and is not something
    // `thinking=open` needs, so naming it would be a requirement that is not one - the third
    // fault this test found, and the reason the assertion is a negative as well.
    assert!(
        !screen.contains("`system`"),
        "the refusal named a switch ABOVE the one it is about:\n{screen}"
    );
}

/// Responding is green while the turn goes and yellow once it goes quiet, and
/// nothing more is said: the colour is the whole notice, so slowness never
/// becomes a sentence of its own.
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
    // The header's right half is the part you cannot get anywhere else, so a
    // `split_row` that drops it whole when both halves do not fit loses it with
    // nothing said: every width has to fit, and the parts go in a stated order.
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
            ..Default::default()
        },
        timings: Default::default(),
        ends_turn: true,
        prefix_check: None,
        dialect_template_sha: String::new(),
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
        // The operator's drop order: the out count, then the position counter,
        // then `% cached`, the model, the duration, tok/s, the spend, and `ctx` last - so the
        // counter going while `ctx` stays is the order, and `ctx` going first would break it.
        if l.contains("1/2") {
            assert!(
                l.contains("41.2k ctx"),
                "w={w}: the counter outlived ctx: {l}"
            );
        }
        if w >= 200 {
            assert!(l.contains("1/2") && l.contains("41.2k ctx"), "w={w}: {l}");
        }
    }
}

/// The question, answered on the head's own status: the ending says which check judged the
/// turn, so zero reuse is read as a slot or a divergence rather than guessed at, and the dialect
/// that rendered the prompt is named beside it.
#[test]
fn the_status_names_the_last_turns_prefix_check_and_dialect() {
    let ending = |check: letibot_sessionlog::event::PrefixCheck| SessionEvent::TurnFinished {
        turn_id: "t1".into(),
        finish_reason: letibot_sessionlog::event::FinishReason::Eos,
        usage: Usage::default(),
        timings: Default::default(),
        ends_turn: true,
        prefix_check: Some(check),
        dialect_template_sha: "3f2a9c".into(),
    };
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        ending(letibot_sessionlog::event::PrefixCheck::Held {
            expected_cached_min: 12_340,
            cached: 12_300,
            shortfall: 40,
        }),
    )));
    let status = a.status_lines(200).join("\n");
    assert!(status.contains("prefix"), "{status}");
    assert!(
        status.contains("short by 40"),
        "the shortfall is the number that says the server's cache and not ours: {status}"
    );
    assert!(status.contains("3f2a9c"), "the dialect: {status}");

    // A divergence is our own defect, and the row says which one it is rather than `held`.
    a.apply(ServerFrame::Event(env(
        3,
        ending(letibot_sessionlog::event::PrefixCheck::Violated {
            detail: "request N+1 does not extend request N".into(),
        }),
    )));
    let status = a.status_lines(200).join("\n");
    assert!(status.contains("DIVERGED"), "{status}");
    assert!(
        status.contains("does not extend"),
        "the engine's own sentence, not a paraphrase: {status}"
    );
}

#[test]
fn the_turns_numbers_live_in_the_header_and_an_ordinary_ending_has_no_footer() {
    // One fact, one place: the turn's numbers live in the header, and an ordinary
    // ending leaves the body with no footer at all - a settled fact should not
    // occupy the row a live fact has to earn.
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
                ..Default::default()
            },
            timings: letibot_sessionlog::event::Timings {
                prompt_ms: 900.0,
                predicted_ms: 2_000.0,
                wall_ms: 12_300,
            },
            ends_turn: true,
            prefix_check: None,
            dialect_template_sha: String::new(),
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

/// While a turn runs, the header shows no duration: the composer row is the one
/// clock. Running - no duration in the header; idle - the last turn's wall-ms,
/// which is a settled fact and not a live one.
#[test]
fn while_a_turn_runs_the_header_shows_no_clock() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    // A round finishes, so `last_timings` carries a SHORT wall-ms - the number that must not be
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
                ..Default::default()
            },
            timings: letibot_sessionlog::event::Timings {
                prompt_ms: 900.0,
                predicted_ms: 4_474.0,
                wall_ms: 4_474,
            },
            ends_turn: true,
            prefix_check: None,
            dialect_template_sha: String::new(),
        },
    )));
    // A call of that round is still running, which is what makes the turn BUSY across the round
    // boundary - the case the two clocks disagree in.
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

    // The row says the turn's elapsed, and it is the only clock on screen.
    // 151204 - 1000 = 150204 ms, spelled `2m30s`.
    assert!(row.contains("2m30s"), "the row's clock: {row}");
    assert!(
        !header.contains("4.5s"),
        "the header shows the last ROUND's wall-ms beside a running turn: {header}"
    );
    assert!(
        !header.contains("2m30s"),
        "the header repeats the composer row's clock: {header}"
    );

    // And the rate and the count did NOT move with it. `last_timings` is what they are for,
    // and the operator's earlier complaint about them was *"the rate comes and goes"* - so the
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
    // and cache fraction - live prefill numbers win there for the whole
    // turn - so the in-flight line says only what it alone knows.
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
    let _line = a.turn_status(120);
    // The header is where those numbers live.
    let header = a.header_line(200);
    assert!(header.contains("41.2k ctx"), "{header}");
    assert!(header.contains("92% cached"), "{header}");
    // This row repeats none of the header's numbers and carries no figure of its
    // own: it says the turn is alive and nothing more. The fields asserted absent
    // below are the ones a future change might put back.
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
    // The question this once settled is gone: both counters left the row, so what
    // is worth pinning now is their absence - neither the server's counter nor the
    // character fallback reaches this row, through any event that once put one there.
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
    // The counter arrives - and is not drawn, though it still counts as a frame the turn is
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
    // A spinner keyed off the last event's timestamp is a snapshot of one: a tool
    // running silent froze the glyph, and the frozen duration read as a dead turn.
    // Phase and duration both run on `now_ms`, which the driver advances every tick.
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
    // And it is a ROW of its own, immediately above the box - not inlaid in
    // the bottom border, where an edge truncates it.
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
    // And the bottom border no longer carries it - the edge is empty of
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

/// And the counts move as the round runs: a second call proposed on the same turn
/// takes the count to two with no new row landing, and the thinking streamed but
/// not committed is counted the same way.
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
            item: None,
            body_bytes: 0,
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
    // And the thinking the turn has streamed, which is also not a row yet.
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

/// The thinking count never falls, including when its reasoning lands as a row:
/// `reasoning_display_lines` sums `ceil(width / cols)`, so text arriving can only
/// add lines, and a count that drops is counting work that has already happened.
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

    // And its row lands. The transcript now holds the reasoning, so the live part must stop
    // counting it - while the TOTAL, which is row plus live, must not move.
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

    // And the next round rebuilds the pane, which is when the inflated number collapsed:
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

    // The count, read off the marker itself - the same arithmetic `live_work` does, taken
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
    // And it grows with the stream, a paragraph at a time - long enough that the wrapped
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

/// A boundary nobody measured - `TurnStarted` with `began_ms: None`, which is what
/// `Sessions::wake` publishes - is the same run: only a measured start opens a new
/// one, or the pane empties and the clock restarts mid-prompt.
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

    // The boundary, with no start in it at all.
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
    // And the thinking goes on across the boundary: the model is mid-thought and
    // keeps streaming under the new round's id, and the count is a different number
    // of lines, so it cannot come from a row the last frame cached.
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

/// The work in flight is ONE fold with the run it is about to join; the pair on the operator's screen was two folds:
///
/// Their screen, verbatim, and the pair is two folds where one belongs:
///
/// ```text
///   [36 tool calls, 869 thinking lines]
///   [94 thinking lines]
/// ```
///
/// The first is the walk's marker for the run the conversation rung hides; the second was the live pane's, for thinking still
/// streaming with no row of its own. `live_here` and `walk_carried_live` asked *does this run hold a row of the CURRENT TURN*, and a
/// turn that has committed no row yet has an empty `turn_rows`, so the pane drew the work BESIDE the run, not inside it. Not only a new
/// prompt: `Sessions::wake` opens a turn with no prompt row, which puts the two folds on ADJACENT lines, the shape this fixture drives.
///
/// Driven through the live path one frame at a time, rows appended and reasoning arriving as deltas in the daemon's order. See
/// [`run_carries_live`]: a run of an EARLIER turn's rows must not fold the live counts in.
#[test]
fn thinking_that_is_still_arriving_is_one_fold_with_the_run_it_will_join() {
    /// One display line at this frame's width.
    const WORKING: &str = "round 1: weighing up the reader and the caller";
    /// The same, arriving as two: the stream, and the row it becomes.
    const STREAMED: &str = "the call came back saying it is in the reader\n\
                          so the next thing to look at is who hands it over";
    /// A second round's working-out, which lands as a row of its own.
    const SECOND: &str = "and now the caller, which is where the value is constructed";

    let mut a = app();
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // The operator's own message, which is what a turn follows.
    a.apply(ServerFrame::Event(env(1, testing::appended("s.u", "user"))));
    a.record_item("s.u", operator_row("run the tests"));
    // ROUND 1: two calls answered and the model's own working-out. All three are rows this
    // rung hides, so they are ONE run - `[2 tool calls, 1 thinking line]`.
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
    let mut seq = 3;
    for (id, call) in [("s.1", "c1"), ("s.2", "c2")] {
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(id, "tool_result"),
        )));
        a.record_item(
            id,
            TranscriptItem::ToolResult {
                call_id: call.into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "SOMETHING LONG ENOUGH TO HIDE THE ROW".into(),
                edit: None,
                origin: None,
                media: None,
            },
        );
        seq += 1;
    }
    a.apply(ServerFrame::Event(env(
        seq,
        testing::appended("s.3", "reasoning"),
    )));
    a.record_item(
        "s.3",
        TranscriptItem::Reasoning {
            text: WORKING.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    seq += 1;
    // …and the round's own assistant row, which is the shape a tool-calling round has: no prose,
    // so it draws nothing and is part of the run.
    a.apply(ServerFrame::Event(env(
        seq,
        testing::appended("s.4", "assistant"),
    )));
    a.record_item(
        "s.4",
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    seq += 1;
    // The turn ends and the daemon WAKES the session - a settlement, a monitor, a
    // nag. `wake` stamps no clock and appends no prompt row, so this turn starts
    // with an empty `turn_rows` and nothing in front of the reader.
    a.apply(ServerFrame::Event(env_at(
        seq,
        2_000,
        SessionEvent::TurnFinished {
            turn_id: "r1".into(),
            finish_reason: letibot_sessionlog::event::FinishReason::Eos,
            usage: Usage::default(),
            timings: Default::default(),
            ends_turn: true,
            prefix_check: None,
            dialect_template_sha: String::new(),
        },
    )));
    seq += 1;
    a.apply(ServerFrame::Event(env_at(
        seq,
        3_000,
        SessionEvent::TurnStarted {
            turn_id: "r2".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: None,
        },
    )));
    seq += 1;
    // The premise, so a failure below is about the live work and not about the fixture: the
    // run is on the screen, and it is one fold.
    let before = a.screen(100, 30).join("\n");
    assert_eq!(markers(&before), 1, "one run, one fold: {before}");
    assert!(
        before.contains("[2 tool calls, 1 thinking line]"),
        "and it carries what is committed: {before}"
    );

    // THE BUG. The model thinks, with no row of its own yet - two display lines. That is the
    // work in flight, it is contiguous with the run above it, and it is the same fold: one
    // marker, and the counts are the SUM. The pane drew a second `[2 thinking lines]` here.
    a.apply(ServerFrame::Event(env(
        seq,
        SessionEvent::Delta {
            turn_id: "r2".into(),
            target: DeltaTarget::Reasoning,
            text: STREAMED.into(),
        },
    )));
    seq += 1;
    let streaming = a.screen(100, 30).join("\n");
    assert_eq!(
        markers(&streaming),
        1,
        "the thinking that is still arriving is the SAME fold as the run it will join, not a \
         second one beside it — the operator's `[36 tool calls, 869 thinking lines]` with \
         `[94 thinking lines]` under it: {streaming}"
    );
    assert!(
        streaming.contains("[2 tool calls, 3 thinking lines]"),
        "and the fold carries the sum, committed and in flight: {streaming}"
    );

    // Two thinking rows land, one after the other, and the merge does not move: the
    // streamed text becoming a row takes the in-flight count to zero and adds the
    // same lines, and a new round's working-out joins the same fold.
    for (id, text) in [("s.5", STREAMED), ("s.7", SECOND)] {
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(id, "reasoning"),
        )));
        a.record_item(
            id,
            TranscriptItem::Reasoning {
                text: text.into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
        );
        seq += 1;
        // The round's assistant row, with no prose: it draws nothing and is part of the run.
        let aid = format!("{id}a");
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(&aid, "assistant"),
        )));
        a.record_item(
            &aid,
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: Vec::new(),
                truncated: false,
            },
        );
        seq += 1;
    }
    let landed = a.screen(100, 30).join("\n");
    assert_eq!(
        markers(&landed),
        1,
        "consecutive thinking rows are ONE block, whatever order they arrive in: {landed}"
    );
    assert!(
        landed.contains("[2 tool calls, 4 thinking lines]"),
        "and the block counts the sum of them: {landed}"
    );

    // And the reply starting does not change the merge. The round's prose lands: it is a row
    // the reader can SEE, so it ends the run - which is what the run's marker must still obey  -
    // and the fold above it keeps the same counts and stays one fold.
    a.apply(ServerFrame::Event(env(
        seq,
        testing::appended("s.9", "assistant"),
    )));
    a.record_item(
        "s.9",
        TranscriptItem::Assistant {
            text: "here is what I found:".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    let replied = a.screen(100, 30).join("\n");
    assert_eq!(
        markers(&replied),
        1,
        "the reply is a drawn row and ends the run, and it is not a second fold: {replied}"
    );
    assert!(
        replied.contains("[2 tool calls, 4 thinking lines]"),
        "the fold above the reply keeps the number it had while the thinking was arriving: \
         {replied}"
    );
    assert!(
        replied.contains("here is what I found:"),
        "and the reply itself is on the screen: {replied}"
    );
}

/// A subagent whose whole product is prose is readable: the peek once rendered tool
/// results only, so a child that answers in text and calls no tools by design showed
/// "no tool output" and threw its report away.
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

/// A message taken from the queue mid-turn lands below the reply it interrupted, and
/// never above it first: the reply's text is already on screen while its row's body
/// arrives later, and every frame between must keep the reply on top.
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

/// What the daemon holds back from the model is drawn beside `Responding`, by kind,
/// and only while something is held: the counts ride the turn's status row, a
/// failure is named, and all zero draws nothing.
#[test]
fn the_held_counter_rides_the_responding_row_while_something_is_held() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    assert!(!a.turn_status(160).contains("held"), "nothing held yet");

    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::HeldResults {
            jobs: 2,
            subagents: 1,
            failed: 0,
        },
    )));
    let row = a.turn_status(160);
    assert!(
        row.contains("Responding") && row.contains("held: 2 jobs · 1 subagent"),
        "{row}"
    );
    assert!(row.find("Responding") < row.find("held:"), "{row}");

    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::HeldResults {
            jobs: 1,
            subagents: 0,
            failed: 1,
        },
    )));
    assert!(
        a.turn_status(160).contains("held: 1 job (failed)"),
        "{}",
        a.turn_status(160)
    );
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::HeldResults {
            jobs: 3,
            subagents: 0,
            failed: 1,
        },
    )));
    assert!(a.turn_status(160).contains("held: 3 jobs (1 failed)"));

    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::HeldResults {
            jobs: 0,
            subagents: 0,
            failed: 0,
        },
    )));
    let row = a.turn_status(160);
    assert!(row.contains("Responding") && !row.contains("held"), "{row}");
}

/// A snapshot drops the held counter this head last read: the counts are in no
/// snapshot and the daemon says them again right after one, so a count carried across a resync
/// or a reconnect is one whose zero this head may never have seen.
#[test]
fn a_snapshot_drops_the_held_counter_the_daemon_will_say_again() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::HeldResults {
            jobs: 2,
            subagents: 0,
            failed: 0,
        },
    )));
    assert!(a.turn_status(160).contains("held: 2 jobs"));

    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t2"));
    a.apply(ServerFrame::Resync {
        reason: "test".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    assert_eq!(a.held, None, "a stale count survived the snapshot");
    assert!(
        !a.turn_status(160).contains("held"),
        "{}",
        a.turn_status(160)
    );
}

/// The status row says what the session is WAITING on when no turn runs - a turn
/// that ended and a session that will act again on its own - drawn in the row a
/// turn's state uses, in the muted register, because the model did not say it.
#[test]
fn a_waiting_list_is_drawn_while_no_turn_runs() {
    use letibot_sessionlog::activity::{Activity, Waiting};
    let mut a = app();
    let waiting = |waiting_on: Vec<Waiting>| SessionEvent::ActivityChanged {
        activity: Activity::of(waiting_on),
    };
    a.apply(ServerFrame::Event(env(
        1,
        waiting(vec![
            Waiting::Job("j8032".into()),
            Waiting::Monitor("ci".into()),
        ]),
    )));
    let line = a.turn_status(120);
    assert!(
        line.contains("waiting on: job j8032 · monitor ci"),
        "the row says what it waits on: {line:?}"
    );

    // A turn of its own takes the row back. `Responding · …` is the turn's, and a list of what
    // is running beside it would say the same thing twice.
    a.apply(ServerFrame::Event(env(2, testing::turn_started("t1"))));
    let line = a.turn_status(120);
    assert!(!line.contains("waiting on:"), "{line:?}");

    // And a quiet session draws nothing at all - which is what makes this invisible to a head
    // talking to a daemon older than 52, whose value is always quiet.
    a.apply(ServerFrame::Event(env(3, testing::turn_finished("t1"))));
    a.apply(ServerFrame::Event(env(4, waiting(Vec::new()))));
    assert!(a.turn_status(120).is_empty(), "quiet draws nothing");
}

/// A child is named the way the subagents pane names it, and a kind that repeats is counted:
///
/// ```text
/// waiting on: monitor letibot-gate-after-wired · child s-1793010881933039000-sub-1793225939092557000 · child s-1793010881933039000-sub-1793225939285082000 · child s-1793010881933039000-sub-1793225939482158000
/// ```
///
/// Every fact in it is right and the line is unusable: a session id is not a name, and three of them push the monitor off a narrow row.
/// So the child is `child fix-login · …<tail>` ([`crate::app::child_name`]) and three of a kind is `3 subagents`. One of a kind keeps
/// the singular, since a reader who sees `1 subagent` learns nothing about *which*.
#[test]
fn a_waiting_list_names_its_children_and_counts_what_repeats() {
    use letibot_sessionlog::activity::{Activity, Waiting};
    let child = |a: &mut App, seq: u64, id: &str, slug: &str| {
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::Subagent {
                subagent_id: id.into(),
                state: "running".into(),
                prompt: "port the picker".into(),
                role: "coder".into(),
                task: String::new(),
                model: String::new(),
                answer: None,
                slug: slug.into(),
            },
        )));
    };
    let waiting = |waiting_on: Vec<Waiting>| SessionEvent::ActivityChanged {
        activity: Activity::of(waiting_on),
    };

    // The reported case: one monitor, three children.
    let mut a = app();
    let kids = [
        (
            "s-1793010881933039000-sub-1793225939092557000",
            "port the picker",
        ),
        (
            "s-1793010881933039000-sub-1793225939285082000",
            "port the table",
        ),
        (
            "s-1793010881933039000-sub-1793225939482158000",
            "port the tree",
        ),
    ];
    for (i, (id, slug)) in kids.iter().enumerate() {
        child(&mut a, i as u64 + 1, id, slug);
    }
    a.apply(ServerFrame::Event(env(
        10,
        waiting(
            std::iter::once(Waiting::Monitor("letibot-gate-after-wired".into()))
                .chain(kids.iter().map(|(id, _)| Waiting::Child((*id).into())))
                .collect(),
        ),
    )));
    let line = a.turn_status(200);
    assert!(
        line.contains("waiting on: monitor letibot-gate-after-wired · 3 subagents"),
        "the monitor named, the children counted: {line:?}"
    );
    // Nothing of the ids the report complained about survives: they name nothing a person can use.
    assert!(
        !line.contains("sub-1793"),
        "a session id is not a name: {line:?}"
    );

    // One child is named, not counted - its slug and the short id, exactly as the pane draws it.
    a.apply(ServerFrame::Event(env(
        11,
        waiting(vec![Waiting::Child(kids[0].0.into())]),
    )));
    let line = a.turn_status(200);
    assert_eq!(
        line.trim_end(),
        "waiting on: child port the picker · …92557000",
        "the pane's own name for a child"
    );

    // A child nobody named still has a name here - its short id - rather than an empty gap.
    a.apply(ServerFrame::Event(env(
        12,
        waiting(vec![Waiting::Child("s-unheard-of-child".into())]),
    )));
    let line = a.turn_status(200);
    assert_eq!(line.trim_end(), "waiting on: child …of-child");
}

/// The daemon's `Turn` is the PROMPT, so the row stays up across a round boundary:
/// every round-level fact says not busy between rounds, and the head asks the
/// activity before its own state or the row stands down mid-prompt.
#[test]
fn a_prompt_in_flight_holds_the_row_up_between_its_rounds() {
    use letibot_sessionlog::activity::{Activity, Waiting};
    let mut a = app();
    let activity = |waiting_on: Vec<Waiting>| SessionEvent::ActivityChanged {
        activity: Activity::of(waiting_on),
    };
    // A round that has finished, with its call finished too: the state once reported as *not
    // busy* over a turn that is plainly working.
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::proposed_on("t1", "c1", "bash", "\"sleep 1\""),
    )));
    a.apply(ServerFrame::Event(env(3, testing::turn_finished("t1"))));
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
        "every round-level fact says the round is over"
    );
    // And the daemon says the prompt is not.
    a.apply(ServerFrame::Event(env(5, activity(vec![Waiting::Turn]))));
    assert!(
        a.turn_busy(),
        "the daemon says a prompt is in flight, so the row is the turn's"
    );
    let line = a.turn_status(120);
    assert!(
        !line.contains("waiting on:"),
        "and the waiting list is not drawn over a running prompt: {line:?}"
    );
    // The prompt's loop returns: the row stands down, and the value says quiet.
    a.apply(ServerFrame::Event(env(6, activity(Vec::new()))));
    assert!(!a.turn_busy());
    assert!(a.turn_status(120).is_empty());
}
