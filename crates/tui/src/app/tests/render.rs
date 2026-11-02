//! Rendering: markdown, the palette, escapes this head did not author.

use super::*;

/// The reader's own refused input is a third register, neither the failure nor the routine one:
/// its own mark, the notice colour, and neither of the two things those use. All three are drawn
/// in one frame, because the defect is a comparison between notes.
#[test]
fn the_three_registers_are_three_marks() {
    const RED: &str = "\u{1b}[31m";
    const YELLOW: &str = "\u{1b}[33m";
    let mut a = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    for (code, detail) in [
        ("compacted", "compacted: 940188 → 9181 tokens"),
        (
            "slash_refused",
            "/qwe is not a daemon verb; /help lists the head's",
        ),
        (
            "ledger_chain_mismatch",
            "row 41's head is not the one that was stored",
        ),
    ] {
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Warning {
                code: code.into(),
                detail: detail.into(),
                compaction: None,
            },
        )));
    }
    let frame = a.screen(100, 30);
    let line = |needle: &str| {
        frame
            .iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("nothing drew `{needle}`:\n{}", frame.join("\n")))
            .clone()
    };
    let (routine, refused, failure) = (
        line("compacted"),
        line("slash_refused"),
        line("ledger_chain_mismatch"),
    );
    // Three different marks, which is what survives `--replay`, a pipe and a light theme where
    // the colours do not. The routine register's mark is the separator of the frame's own
    // `session` line, and a second one there would read as a stutter.
    assert!(routine.contains("session · "), "{routine:?}");
    assert!(
        !routine.contains("! compacted") && !routine.contains("× compacted"),
        "{routine:?}"
    );
    assert!(refused.contains("× slash_refused"), "{refused:?}");
    assert!(failure.contains("! ledger_chain_mismatch"), "{failure:?}");
    // The colours: a typo is not red, and it is not the housekeeping dim either.
    assert!(
        !refused.contains(RED),
        "a typo in the failure colour: {refused:?}"
    );
    assert!(
        refused.contains(YELLOW),
        "not the notice register: {refused:?}"
    );
    assert!(
        failure.contains(RED),
        "the chain mismatch lost its red: {failure:?}"
    );
    // And the reason the register exists, in one assertion: the reader's own mistake is
    // not the same event as a session in trouble, and neither is housekeeping.
    assert_ne!(refused, failure);
    assert_ne!(refused, routine);
}

/// The operator's real `ls -la` payload is painted in the palette's own roles, and not one byte
/// of `ls`'s sequence reaches the glass as text - the two shapes a synthetic fixture never makes:
/// an uncoloured line, and `ls`'s own reset before the colour rather than only a close after it.
#[test]
fn the_operators_own_ls_payload_is_drawn_in_the_palettes_own_roles() {
    let mut a = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    bang_rows(&mut a, 2, "i1", "! ls -la", OPERATOR_LS_LA);
    a.key(Key::CtrlV);
    let rows = a.screen(100, 40);
    let frame = rows.join("\n");
    // The colour arrived - as **this head's** role for `ls`'s `01;34`, built from the palette
    // rather than spelled as an escape here: a test that knew the sequence would pass on the
    // day the role moved.
    assert!(
        frame.contains(&rano::style::Palette::Colour.open(Role::Subheading)),
        "`ls`'s directory colour must be drawn in the palette's own role: {frame:?}"
    );
    // And the text is kept, **line for line and column for column**: the visible frame, with
    // this head's own sequences taken off, is the payload's own text with nothing in it - no
    // `[0m`, no `[01;34m`, and no space where a sequence used to be.
    let visible = rows.iter().map(|r| seen(r)).collect::<Vec<_>>().join("\n");
    for kept in [
        "total 124",
        "drwxrwxr-x 22 dead dead  4096 Oct  6 09:46 .",
        "drwxrwxr-x  3 dead dead  4096 Oct  4 22:22 crates",
        "drwxrwxr-x 14 dead dead  4096 Oct  6 11:11 letibot",
    ] {
        assert!(
            visible.contains(kept),
            "{kept:?} is not on the glass as text: {visible:?}"
        );
    }
    // **No sequence body survives as text**, and every escape that does reach the frame is
    // one this head chose. The `[` in the paste is the thing being asserted absent.
    for (n, row) in rows.iter().enumerate() {
        assert!(
            !seen(row).contains('['),
            "a sequence's body reached the glass as text, row {n}: {:?}",
            seen(row)
        );
        assert!(
            only_the_heads_own_escapes(row),
            "an escape reached the frame that the palette did not put there, row {n}: {row:?}"
        );
    }
}

/// An escape at the START of a payload line and one mid-line: the first is `ls`'s shape for a run
/// of coloured names and the second is `grep --color`'s. Both go through one `item_lines` call
/// with the fold open, so the body path is the one under test.
#[test]
fn a_colour_at_the_start_of_a_payload_line_and_one_mid_line_both_become_roles() {
    let blue = rano::style::Palette::Colour.open(Role::Subheading);
    for (what, payload, coloured) in [
        (
            "at the start of the line",
            "\u{1b}[01;34mfirst\u{1b}[0m\nplain second",
            "first",
        ),
        (
            "mid-line",
            "first \u{1b}[01;34msecond\u{1b}[0m tail\nplain third",
            "second",
        ),
    ] {
        let rows = item_rows(true, bash_result(payload));
        let frame = rows.join("\n");
        assert!(
            frame.contains(&blue),
            "{what}: the colour was not drawn: {frame:?}"
        );
        assert!(
            frame.contains(coloured),
            "{what}: {coloured:?} was lost: {frame:?}"
        );
        assert!(
            frame.contains("plain"),
            "{what}: the next line is gone: {frame:?}"
        );
        for (n, row) in rows.iter().enumerate() {
            assert!(
                !seen(row).contains('['),
                "{what}: a sequence's body reached the glass as text, row {n}: {:?}",
                seen(row)
            );
            assert!(
                only_the_heads_own_escapes(row),
                "{what}: an escape reached the frame that the palette did not put there, \
                     row {n}: {row:?}"
            );
        }
    }
}

/// The same real payload, and everything that is not SGR is still removed whole: a mode string,
/// an OSC title, a C1 CSI/ST and a DEL are gone as escapes and as text, on a payload whose own
/// reset mid-line is where a parser that mis-read it would leave `[0m` behind.
#[test]
fn the_real_payload_still_loses_every_byte_that_is_not_a_colour() {
    let mut a = App::new(RenderConfig {
        width: 120,
        color: true,
        ..RenderConfig::default()
    });
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    let hostile =
        "\u{1b}[2J \u{1b}[?1002h \u{1b}[?1049h \u{1b}]0;pwned\u{7} \u{9b}31m \u{9c} \u{7f}";
    bang_rows(
        &mut a,
        2,
        "i1",
        "! ls -la",
        &format!("{}\n{}{hostile}", OPERATOR_LS_LA.trim_end(), "crates/"),
    );
    a.key(Key::CtrlV);
    let rows = a.screen(120, 40);
    let frame = rows.join("\n");
    for gone in [
        "[2J", "[?1002h", "[?1049h", "]0;", "pwned", "\u{7}", "\u{9b}", "\u{9c}", "\u{7f}",
    ] {
        assert!(!frame.contains(gone), "{gone:?} survived: {frame:?}");
    }
    // And the colour the payload legitimately carries is still there, so this is not
    // "everything was dropped": the assertion above is about the other families.
    assert!(
        frame.contains(&rano::style::Palette::Colour.open(Role::Subheading)),
        "the payload's own colour went with the hostile bytes: {frame:?}"
    );
}

/// The same hostile payload on a head that DOES emit colour, which is the half the colourless
/// test cannot reach: the SGR the command wrote has to survive as a role this head already has,
/// never as a byte from the program - no escape reaches the frame that this head did not choose.
#[test]
fn on_a_head_that_emits_colour_a_foreign_escape_arrives_as_a_role_and_nothing_else() {
    let mut a = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // `ls`'s own default for a directory, and then the rest of the vocabulary of
    // hostile bytes the colourless test already uses.
    let hostile = " A\u{1b}[31mred\u{1b}[0m \u{1b}[8m(hidden) \u{1b}[2J \u{1b}[?1002h \u{1b}]0;pwned\u{7} \u{9b}31m \u{9c} \u{7f} end";
    bang_rows(
        &mut a,
        2,
        "i1",
        "! ls -la",
        &format!("src/\u{1b}[01;34mthe-dir\u{1b}[0m{hostile}\nplain"),
    );
    let rows = a.screen(100, 40);
    let frame = rows.join("\n");
    // The colour arrived, as this head's role for it.
    assert!(
        frame.contains(&rano::style::Palette::Colour.open(Role::Subheading)),
        "the payload's own colour must be drawn in the palette's role for it: {frame:?}"
    );
    assert!(frame.contains("the-dir"), "and the text is kept: {frame:?}");
    assert!(frame.contains("red"), "{frame:?}");
    // And every escape that reaches the glass is one this head chose.
    for (n, row) in rows.iter().enumerate() {
        assert!(
            only_the_heads_own_escapes(row),
            "an escape reached the frame that the palette did not put there, row {n}: {row:?}"
        );
    }
    // The existing guarantee, re-asserted: not one byte of a hostile sequence, as an
    // escape or as text.
    for gone in [
        "[2J", "[?1002h", "[8m", "]0;", "pwned", "\u{9b}", "\u{9c}", "\u{7f}",
    ] {
        assert!(!frame.contains(gone), "{gone:?} survived: {frame:?}");
    }
}

/// A colour that runs to the end of a payload line does not tint the next one: a terminal carries
/// SGR state across a newline and a row list must not, or the head's own gutter and the whole of
/// the next line get painted in a colour the command never asked for there.
#[test]
fn a_colour_that_runs_to_the_end_of_a_payload_line_does_not_tint_the_next_one() {
    let mut a = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // The first line opens a colour and never closes it, which is what a program killed mid-write
    // does. Three lines, so the row has a rest to open: a folded row draws one line and the seam,
    // and this test needs the line after the coloured one on the screen.
    bang_rows(
        &mut a,
        2,
        "i1",
        "! ls -la",
        "\u{1b}[31mred to the end of the line\nplain second line\nthird",
    );
    a.key(Key::CtrlV);
    let rows = a.screen(100, 40);
    let red = rano::style::Palette::Colour.open(Role::Failure);
    let coloured = rows
        .iter()
        .find(|r| r.contains("red to the end"))
        .expect("the first line is drawn");
    assert!(
        coloured.contains(&red),
        "the first line's own colour is drawn: {coloured:?}"
    );
    let next = rows
        .iter()
        .find(|r| r.contains("plain second line"))
        .expect("the second line is drawn");
    assert!(
        !next.contains(&red),
        "the first line's colour leaked into the next row: {next:?}"
    );
    assert!(
        only_the_heads_own_escapes(next),
        "and that row is still only this head's own sequences: {next:?}"
    );
}

/// A row the person ran is the model's row with their own mark on it: every role on that header
/// comes from `outcome`, `name`, the payload and the fold, and nothing in the arm reads `origin`.
/// So the registers stay the model's byte for byte, and a bar in the user-accent role says who.
#[test]
fn an_operators_tool_row_is_the_models_row_with_the_persons_own_mark_on_it() {
    let body: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let row = |origin: Option<letibot_transcript::CallOrigin>| TranscriptItem::ToolResult {
        call_id: "bang-1".into(),
        name: "bash".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload: body.clone(),
        edit: None,
        origin,
        media: None,
    };
    let mine = item_rows(
        true,
        row(Some(letibot_transcript::CallOrigin::Operator {
            who: "dead".into(),
        })),
    );
    let theirs = item_rows(true, row(None));

    // The person's own mark, built from the palette rather than spelled as an escape: the
    // assertion is about the ROLE, and a test that knew the sequence would pass on the day
    // the role moved.
    let bar = rano::style::Palette::Colour.paint(Role::UserAccent, "▌");
    let indent = theirs[0].len() - theirs[0].trim_start().len();
    assert!(
        mine[0].starts_with(&format!("{}{bar} ", " ".repeat(indent))),
        "the row does not say the person ran it: {:?}",
        mine[0]
    );
    assert_eq!(
        mine[0],
        format!("{}{bar} {}", " ".repeat(indent), &theirs[0][indent..]),
        "the operator's header is not the model's with the mark in front of it"
    );
    assert_eq!(
        mine[1..],
        theirs[1..],
        "the count, the fold and the body must be the model's, byte for byte"
    );

    // **And with no palette at all** - the pipe, `--replay` and CI case. The provenance is a
    // glyph, so it survives the sequences going away, and nothing else on the row is painted.
    let plain = item_rows(
        false,
        row(Some(letibot_transcript::CallOrigin::Operator {
            who: "dead".into(),
        })),
    );
    assert!(
        plain[0].contains('▌'),
        "the provenance went with the colour: {:?}",
        plain[0]
    );
    assert!(
        !plain.iter().any(|l| l.contains('\u{1b}')),
        "[`Palette::None`] emits no sequences at all: {plain:?}"
    );
}

/// And it is on the SCREEN, where the report came from: both rows fed the way the daemon appends
/// them, through the walk and the frame, with the palette on. Both halves, because one alone is
/// the defect - the operator's row must wear the bar and the model's must not.
#[test]
fn the_persons_mark_reaches_the_screen_and_only_their_own_row_wears_it() {
    let body: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let bar = rano::style::Palette::Colour.paint(Role::UserAccent, "▌");
    // The settled tool row is the one line that names the call; ` Ran ` is the verb a `bash`
    // result is drawn with, and the operator's own line above it is not a tool row.
    let header = |a: &mut App| {
        a.screen(100, 40)
            .into_iter()
            .find(|l| l.contains(" Ran "))
            .expect("the result row is drawn")
    };

    let mut mine = app();
    mine.cfg.color = true;
    mine.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut mine, "! seq 1 60");
    assert!(matches!(
        mine.key(Key::Enter),
        Some(Action::OperatorShell { .. })
    ));
    bang_rows(&mut mine, 2, "i1", "! seq 1 60", &body);
    let row = header(&mut mine);
    assert!(
        row.contains(&bar),
        "the row the person ran does not say so on the screen: {row:?}"
    );

    let mut theirs = app();
    theirs.cfg.color = true;
    theirs.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    model_bash_rows(&mut theirs, 2, "i1", &body);
    let row = header(&mut theirs);
    assert!(
        !row.contains('▌'),
        "a call the MODEL proposed is wearing the person's mark: {row:?}"
    );
}

/// The falsification half: the real vocabulary through every surface that renders text this head
/// did not author. The colourless run is the sharper one - with `color: false` the head emits no
/// escapes at all, so any `ESC` in a row is somebody else's text getting through.
#[test]
fn no_escape_from_content_this_head_did_not_author_reaches_the_terminal() {
    use letibot_transcript::{
        ReasoningField, SystemOrigin, ToolCall, ToolOutcome, TranscriptItem as T, UserPart,
    };

    // ---- the transcript surfaces, colourless: not one ESC may survive ----
    let rows: Vec<(&str, Vec<String>)> = vec![
        (
            "model prose",
            item_rows(
                false,
                T::Assistant {
                    text: format!("an answer{HOSTILE}\nand a second line"),
                    tool_calls: Vec::new(),
                    truncated: false,
                },
            ),
        ),
        (
            "a fence body",
            item_rows(
                false,
                T::Assistant {
                    text: format!("before\n\n```rust\nlet x = 1;{HOSTILE}\n```\n\nafter"),
                    tool_calls: Vec::new(),
                    truncated: false,
                },
            ),
        ),
        (
            "model reasoning",
            item_rows(
                false,
                T::Reasoning {
                    text: format!("thinking{HOSTILE}\nsecond line"),
                    field: ReasoningField::ReasoningContent,
                    truncated: false,
                },
            ),
        ),
        (
            "the operator's own paste",
            item_rows(
                false,
                T::User {
                    speaker: Default::default(),
                    parts: vec![UserPart::Text {
                        text: format!("I pasted{HOSTILE} out of a log"),
                    }],
                },
            ),
        ),
        (
            "a system row",
            item_rows(
                false,
                T::System {
                    text: format!("bootstrap{HOSTILE}"),
                    origin: SystemOrigin::Bootstrap,
                },
            ),
        ),
        (
            "a tool payload",
            item_rows(
                false,
                T::ToolResult {
                    call_id: "call_0".into(),
                    name: "bash".into(),
                    outcome: ToolOutcome::Ok,
                    payload: format!("$ ls{HOSTILE}\nfile.rs"),
                    edit: None,
                    origin: None,
                    media: None,
                },
            ),
        ),
        (
            "a tool's failure reason",
            item_rows(
                false,
                T::ToolResult {
                    call_id: "call_1".into(),
                    name: "bash".into(),
                    outcome: ToolOutcome::Failed {
                        reason: format!("the command wrote{HOSTILE} to stderr"),
                    },
                    payload: "$ false".into(),
                    edit: None,
                    origin: None,
                    media: None,
                },
            ),
        ),
        (
            "the raw markup behind ctrl-x",
            item_rows(
                false,
                T::Assistant {
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_2".into(),
                        name: "read".into(),
                        arguments: format!("{{\"path\":\"{HOSTILE}\"}}"),
                    }],
                    truncated: false,
                },
            ),
        ),
        (
            "both sides of a diff and its path",
            item_rows(
                false,
                T::ToolResult {
                    call_id: "call_3".into(),
                    name: "edit".into(),
                    outcome: ToolOutcome::Ok,
                    payload: "1 replacement".into(),
                    edit: None,
                    origin: None,
                    media: None,
                },
            ),
        ),
    ];
    for (what, r) in &rows {
        assert!(
            !r.iter().any(|l| l.contains('\u{1b}')),
            "{what}: an ESC reached a row with no colour configured: {r:?}"
        );
        assert_eq!(unauthored(r), None, "{what}");
    }

    // ---- and the stateful surfaces: panes, chrome, and the live stream ----
    let mut a = app();
    a.session_id = "s1".into();
    // A live turn streaming the payload, which is the one surface that never goes
    // through `item_lines`.
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(2, testing::delta("t1", HOSTILE))));
    a.apply(ServerFrame::Event(env(
        3,
        testing::reasoning("t1", HOSTILE),
    )));
    // A running call's progress note - the channel the gate's own sentence rides.
    a.apply(ServerFrame::Event(env(
        4,
        testing::proposed("t1", "c1", "bash"),
    )));
    a.apply(ServerFrame::Event(env(
        5,
        testing::tool_progress("c1", HOSTILE),
    )));
    // A prompt this head is holding, which renders at the tail until its row lands.
    a.pending_prompts.push(format!("queued{HOSTILE}"));
    let live = a.screen(160, 40);
    assert_eq!(unauthored(&live), None, "a live turn");
    assert!(
        !live.iter().any(|l| l.contains('\u{1b}')),
        "a live turn emitted an escape with no colour configured"
    );

    // The panes: each renders a fact from the daemon or the model.
    let mut b = App::new(plain_cfg(120));
    b.session_id = "s1".into();
    b.apply(jobs_frame(
        "s1",
        vec![daemon_job("j1", &format!("cargo test{HOSTILE}"), false)],
    ));

    b.push_surface(Surface::Jobs);
    b.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "sub-1".into(),
            state: "running".into(),
            prompt: format!("summarise{HOSTILE}"),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    b.push_surface(Surface::Subagents);
    b.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TodosUpdated {
            todos: vec![letibot_sessionlog::event::TodoEntry {
                by: letibot_sessionlog::event::TodoBy::Model,
                when: None,
                needs: Vec::new(),
                content: format!("tidy up{HOSTILE}"),
                status: letibot_sessionlog::event::TodoStatus::Pending,
            }],
        },
    )));
    b.push_surface(Surface::Todos);
    b.apply(ServerFrame::Event(env(
        3,
        SessionEvent::SessionRenamed {
            title: format!("my session{HOSTILE}"),
        },
    )));
    // A note with hostile text in it, through the real producer: a note's row is built by
    // `note_lines_unfolded`, which guards the daemon's `detail` as it composes it, so pushing raw
    // foreign text into `slash_out` would test a contract no producer has.
    b.note(Note::Warned(Warned {
        code: "gate".into(),
        detail: format!("refused{HOSTILE}"),
        ts: 0,
    }));
    // One pane at a time, because `screen` draws one: four panes at once and `slash_out` wins the
    // screen every time, which is a label describing the loop rather than the frame.
    for (name, only) in [
        ("jobs", 0usize),
        ("subagents", 1),
        ("todos", 2),
        ("slash listing", 3),
    ] {
        b.close_all_surfaces();
        match only {
            0 => b.push_surface(Surface::Jobs),
            1 => b.push_surface(Surface::Subagents),
            2 => b.push_surface(Surface::Todos),
            _ => {
                b.slash_out = Some(("/notes".into(), b.notes_lines()));
                b.push_surface(Surface::Listing);
            }
        }
        // The header carries the renamed session's title whatever pane is up, so
        // every one of these frames also exercises the title.
        let shown = b.screen(160, 40);
        assert_eq!(unauthored(&shown), None, "the {name} pane");
        assert!(
            !shown.iter().any(|l| l.contains('\u{1b}')),
            "the {name} pane emitted an escape with no colour configured"
        );
        // **And the pane really was on the screen**, or the assertion above is about
        // a frame that never drew it - the mistake this loop made first time.
        let flat = shown.join("\n");
        let drawn = match only {
            0 => flat.contains("background jobs"),
            1 => flat.contains("subagents"),
            2 => flat.contains("todos"),
            _ => flat.contains("esc closes"),
        };
        assert!(drawn, "the {name} pane was not drawn at all:\n{flat}");
    }
}

/// A carried row is done, not pending, and the colour has to say so: a row carry spends nothing,
/// so the prefill's yellow - computed now, costing you - is the wrong register. Asserted on the
/// glyphs, because `Palette::None` is the replay and CI case and the split survives it.
#[test]
fn landed_rows_paint_as_settled_and_not_as_pending() {
    // The fill's one line in the status row (`ui::progress_row`): three of four rows landed.
    let mut a = app();
    a.filling = Some(("carrying".into(), "rows".into(), 300, 400));
    let (line, _) = a.status_row(100);
    assert!(
        line.contains('█'),
        "three landed rows are settled: {line:?}"
    );
    assert!(
        !line.contains('▓'),
        "and none of them is still being worked on: {line:?}"
    );
    assert!(
        line.contains('░'),
        "the one outstanding is not filled at all"
    );
}

/// [`Palette::None`] is not a monochrome theme, it is the `--replay`, pipe and CI case: no
/// sequences at all. The accent glyphs are what survive it, and they are why the hierarchy is
/// carried by a step and a word rather than by a colour.
#[test]
fn the_plain_palette_emits_no_escapes_and_keeps_the_glyphs() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::appended("u", "user"))));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "u".into(),
            item: Box::new(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "hello".into(),
                }],
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        3,
        testing::appended("r", "reasoning"),
    )));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::TranscriptContent {
            item_id: "r".into(),
            item: Box::new(TranscriptItem::Reasoning {
                text: "working it out".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        5,
        testing::appended("t", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        6,
        SessionEvent::TranscriptContent {
            item_id: "t".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "call_0".into(),
                name: "grep".into(),
                outcome: letibot_transcript::ToolOutcome::Failed {
                    reason: "no such path".into(),
                },
                payload: "a\nb\n".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    a.reasoning = Fold::Open;
    let screen = a.screen(120, 40).join("\n");
    assert!(!screen.contains('\x1b'), "{screen:?}");
    for glyph in ["▌", "▸", "┃", "╭", "│", "╰"] {
        assert!(screen.contains(glyph), "{glyph} is missing:\n{screen}");
    }
    assert!(
        screen.contains("failed"),
        "and the word survives too:\n{screen}"
    );
    assert!(screen.contains("no such path"), "{screen}");
}

/// Size tells you what matters before you read a word - by **attribute**, so
/// it survives a terminal-native theme, and never by a cube colour.
#[test]
fn a_big_result_weighs_more_than_a_small_one_and_costs_no_cube_colour() {
    let mut a = App::new(RenderConfig {
        width: 120,
        color: true,
        ..RenderConfig::default()
    });
    let mut add = |seq: u64, id: &str, n: usize| {
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            seq + 1,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: "call_0".into(),
                    name: "grep".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "x\n".repeat(n),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    };
    add(1, "small", 3);
    add(3, "big", 236);
    let screen = a.screen(120, 40);
    let row = |needle: &str| {
        screen
            .iter()
            .find(|l| l.contains(needle))
            .cloned()
            .unwrap_or_else(|| panic!("{needle}: {}", screen.join("\n")))
    };
    assert!(
        row("236 lines").contains("\x1b[1m"),
        "a big result is bold: {:?}",
        row("236 lines")
    );
    assert!(
        !row("3 lines").contains("\x1b[1m"),
        "a small one is not: {:?}",
        row("3 lines")
    );
    let joined = screen.join("\n");
    for cube in ["38;5;", "48;5;", "38;2;"] {
        assert!(
            !joined.contains(cube),
            "{cube} is not a theme slot: {joined:?}"
        );
    }
}

/// A listing must not strip the colour the head composed into it: a `/notes` row is the head's
/// own composed text, so the sanitiser removed the head's own escape and, leaving the rest
/// behind, turned it into visible garbage. Both halves - no escape as text, and the colour kept.
#[test]
fn a_slash_listing_does_not_strip_the_colour_the_head_composed_into_it() {
    let mut a = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    a.session_id = "s1".into();
    a.note(Note::Warned(Warned {
        code: "gate".into(),
        detail: "a refusal".into(),
        ts: 0,
    }));
    // The premise: the row this head composes for its own listing IS coloured.
    let rows = a.notes_lines();
    assert!(
        rows.iter().any(|l| l.contains("\u{1b}[31m")),
        "the premise: `notes_lines` paints a warned note with the head's own red: {rows:?}"
    );
    // Now draw it through the listing, which is where the sanitiser was applied.
    a.slash_out = Some(("/notes".into(), rows));
    a.push_surface(Surface::Listing);
    a.pane_scroll = 0;
    let shown = a.screen(100, 30).join("\n");
    assert!(
        !shown.contains("[31m") || shown.contains("\u{1b}[31m"),
        "an escape's BODY was left as printable text:\n{shown}"
    );
    assert!(
        shown.contains("\u{1b}[31m"),
        "the head's own colour was stripped from its own listing:\n{shown}"
    );
}

/// The header is facts: no accent bar, no emphasis, both halves in one quiet register. The accent
/// bar is how both heads say a person said this, and on the row the reader crosses on every
/// return to the field it made the session's own name read as somebody's sentence.
#[test]
fn the_header_has_no_bar_and_no_emphasis() {
    let mut a = app();
    a.cfg.color = true;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    let screen = a.screen(100, 24);
    let head = screen
        .iter()
        .find(|l| l.contains("▌") || l.contains("one"))
        .expect("the header is drawn");
    // **The bar is DELETED, not recoloured** - a grey bar is still a bar and still claims a
    // person said this. The glyph is what says it; the register only underlines it.
    assert!(
        !head.contains('▌'),
        "the header still opens with the user accent bar: {head:?}"
    );
    // And nothing on the row is emphasised or accented.
    assert!(
        !head.contains(sgr::BOLD),
        "the name is still bold: {head:?}"
    );
    assert!(
        !head.contains("\x1b[34m"),
        "something is still in the user-accent register: {head:?}"
    );
    assert!(
        head.contains("\x1b[2m"),
        "the header is not in the quiet register at all: {head:?}"
    );
    // The name is still *there* - this is a register change and not a deletion of the row.
    assert!(
        head.contains("one"),
        "the session name went with the bar: {head:?}"
    );
}

#[test]
fn a_users_own_message_is_a_block_with_a_bar_and_the_time_it_was_sent() {
    let mut a = app();
    a.apply(ServerFrame::Event(env_at(
        1,
        1_788_984_000_000,
        testing::appended("s.0", "user"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        testing::content("s.0", "why did the cache miss"),
    )));
    let screen = a.screen(80, 16);
    let row = screen
        .iter()
        .find(|l| l.contains("why did the cache miss"))
        .expect("the prompt is on the screen");
    // The bar is the signal that survives with no colour and survives a
    // copy-paste, which is why it is a glyph and not only a colour.
    assert!(
        row.starts_with(&format!("{}▌", " ".repeat(App::GUTTER))),
        "{row:?}"
    );
    // A wall-clock time, from the log's own clock. Which one depends on the
    // box's zone, so the assertion is on the shape.
    assert!(
        row.split_whitespace()
            .last()
            .is_some_and(|t| t.len() == 8 && t.contains(':')),
        "no timestamp on the row: {row:?}"
    );
    // The block is padded to the full content width, or the background stops mid-row and reads as
    // damage. Content width is the terminal less both gutters; the right one is empty by design.
    assert_eq!(line_width(row), 80 - App::GUTTER, "{row:?}");
}

// The model picker greens the rows this box can take. The fact is the daemon's (`MODEL_KEYS_KEY`),
// because whether a preset resolves a key is a fact about this box's files and environment, and a
// head that guessed would green exactly the rows that refuse at the first turn.

/// A declared local model is ready, and the daemon is what says so: `local` needs no key and no
/// authentication, so greening it by a hardcoded literal worked exactly as long as it was the
/// only keyless row.
#[test]
fn a_declared_local_model_is_ready_without_a_key() {
    use letibot_sessionlog::protocol::{MODEL_KEYLESS_KEY, MODEL_KEYS_KEY, SettingRow};
    let row = |r: &str, v: &str| SettingRow {
        key: r.into(),
        value: v.into(),
        source: String::new(),
        editable: String::new(),
        choices: Vec::new(),
        tools: Vec::new(),
    };
    let mut a = App::new(RenderConfig {
        width: 110,
        color: true,
        ..RenderConfig::default()
    });
    a.settings = vec![
        row(MODEL_KEYS_KEY, "deepseek,glm"),
        row(MODEL_KEYLESS_KEY, "local,dense78"),
    ];

    assert!(a.choice_ready("local"), "the daemon's own server");
    assert!(
        a.choice_ready("dense78"),
        "a declared local model needs no key"
    );
    assert!(
        a.choice_ready("dense78 (qwen-3.8-27b at http://192.168.1.78:8082)"),
        "the row for a session already on it -- the first WORD is the name, and \
             splitting on `/` would land inside the url"
    );
    assert!(
        a.choice_ready("deepseek/deepseek-flash"),
        "a key this box holds"
    );
    assert!(
        !a.choice_ready("grok/grok-4.3"),
        "a preset with no key is still not ready"
    );
    assert!(
        !a.choice_ready("dense97"),
        "a name nobody declared is not ready either"
    );
}

/// **A light background, once the terminal says so, renders the light palette** - and only
/// where the feature is on: an answer the head did not ask for changes nothing.
#[test]
fn a_light_background_reply_switches_the_palette() {
    let mut a = app();
    a.cfg.color = true;
    a.key(Key::Background { light: true });
    a.screen(80, 24);
    assert_eq!(a.cfg.palette(), rano::style::Palette::Colour);
    a.set_features(rano::term::Features::ALL);
    a.screen(80, 24);
    assert_eq!(a.cfg.palette(), rano::style::Palette::Light);
    a.key(Key::Background { light: false });
    a.screen(80, 24);
    assert_eq!(a.cfg.palette(), rano::style::Palette::Colour);
}
