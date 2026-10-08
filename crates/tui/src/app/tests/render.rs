//! Rendering: markdown, the palette, escapes this head did not author.

use super::*;

/// **R29 part two: the reader's own input refused is a THIRD register**, and it is
/// neither of the two above.
///
/// The operator, on a red note for a mistyped `/qwe` sitting in the same colour as
/// `ledger_chain_mismatch`: *"red is stop the world event … a mistyped /qwe is not a
/// session in trouble."* The census is 7 of 46 failure codes (`Class`'s docs), and this
/// asserts what the *head* does with them: its own mark, the notice colour, and neither
/// of the two things the other registers use.
///
/// **All three in one frame**, because the defect is a *comparison* — a reader learns the
/// register by seeing two notes side by side — and a test that drew them one at a time
/// could pass while they looked identical.
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
    // The three marks, and they are three DIFFERENT marks — which is what survives a
    // `--replay`, a pipe and a light theme, where the colours do not.
    assert!(routine.contains("· compacted"), "{routine:?}");
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

/// **The operator's real payload is painted in the palette's own roles, and not one byte of
/// `ls`'s sequence reaches the glass as text.**
///
/// The first cut's tests all used a synthetic payload, and the real one differs in the two
/// ways the fixture could not show: a line with no escape on it (`total 124`), and `ls`'s
/// own reset *before* the colour (`\u{1b}[0m\u{1b}[01;34m`) rather than only a close after it.
/// Neither defeats the painter — which is the assertion — and the point of pinning it here
/// is that the *proof* is now on the bytes the operator was looking at.
///
/// `ctrl-v` is pressed because the fold draws the payload's **first** line and `ls -la`'s
/// first line is `total 124`, the one line of the payload with no colour in it; the window
/// is what puts the directory rows on the screen. That is a property of the fold and not of
/// the paint, and it is why the header says `4 lines` beside one shown row.
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
    // The colour arrived — as **this head's** role for `ls`'s `01;34`, built from the palette
    // rather than spelled as an escape here: a test that knew the sequence would pass on the
    // day the role moved.
    assert!(
        frame.contains(letibot_ui::style::Palette::Colour.open(Role::Subheading)),
        "`ls`'s directory colour must be drawn in the palette's own role: {frame:?}"
    );
    // And the text is kept, **line for line and column for column**: the visible frame, with
    // this head's own sequences taken off, is the payload's own text with nothing in it — no
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

/// **An escape at the START of a payload line, and one mid-line** — the two shapes named,
/// and neither defeats the painter.
///
/// A line that opens with a colour is the case a fixture built as `"text\u{1b}[31mred…"`
/// never produces, and it is the shape `ls` writes for a *run* of coloured names. The
/// mid-line case is the same colour after text, which is `grep --color`'s shape. Both go
/// through one `item_lines` call with the fold open, so the body path is the one under test.
#[test]
fn a_colour_at_the_start_of_a_payload_line_and_one_mid_line_both_become_roles() {
    let blue = letibot_ui::style::Palette::Colour.open(Role::Subheading);
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
            frame.contains(blue),
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

/// **The same real payload, and everything that is NOT SGR is still removed whole.**
///
/// The colour path is the new reader; §3.1's guarantee is the old one and it is not
/// weakened by the new one existing. A mode string, an OSC title, a C1 CSI/ST and a DEL
/// are all still gone — as escapes *and* as text — on the very payload the operator was
/// looking at, with `ls`'s own `\u{1b}[0m` in the middle of a line where a parser that
/// mis-read a reset would leave `[0m` behind.
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
        frame.contains(letibot_ui::style::Palette::Colour.open(Role::Subheading)),
        "the payload's own colour went with the hostile bytes: {frame:?}"
    );
}

/// **The same hostile payload on a head that DOES emit colour** — and this is the half the
/// colourless test cannot reach.
///
/// The operator's own `! ls -la` is meant to look on the screen as it looks in their console,
/// so the SGR their command wrote has to survive as **a role this head already has** rather
/// than as a byte from the program. That is the one thing §3.1's sanitiser could not be asked
/// for before: the old guarantee was *no escape reaches the frame*, and it is now *no escape
/// reaches the frame that this head did not choose* — which is the same guarantee stated in
/// the only form that can be true of a head that colours its own rows.
///
/// So the assertions are three: the payload's `\u{1b}[01;34m` arrives as [`Role::Subheading`]'s
/// own sequence (the same bytes `ls`'s directory colour means here); every other escape on
/// every row is in [`head_vocabulary`]; and not one byte of the hostile sequences — the mode
/// strings, the OSC title, the C1 controls, the DEL — is anywhere in the frame, which is the
/// existing guarantee re-asserted where the colour now enters.
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
        frame.contains(letibot_ui::style::Palette::Colour.open(Role::Subheading)),
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

/// **A colour that runs to the end of a payload line does not tint the next one.**
///
/// A terminal carries SGR state across a newline; a row list must not. Each payload line is
/// drawn with this head's own two-column gutter in front of it, and a colour that leaked would
/// paint that gutter — and then the whole of the next line — in a colour the command never
/// asked for there. The assertion is on the row that holds the second line: it must carry
/// none of the first line's colour.
#[test]
fn a_colour_that_runs_to_the_end_of_a_payload_line_does_not_tint_the_next_one() {
    let mut a = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    // The first line opens a colour and never closes it — which is what a program that
    // is killed mid-write does, and what `ls` does at the end of its last entry.
    // Three lines, so the row has a rest to open: a folded row draws one line and the
    // seam, and this test needs the line AFTER the coloured one on the screen.
    bang_rows(
        &mut a,
        2,
        "i1",
        "! ls -la",
        "\u{1b}[31mred to the end of the line\nplain second line\nthird",
    );
    a.key(Key::CtrlV);
    let rows = a.screen(100, 40);
    let red = letibot_ui::style::Palette::Colour.open(Role::Failure);
    let coloured = rows
        .iter()
        .find(|r| r.contains("red to the end"))
        .expect("the first line is drawn");
    assert!(
        coloured.contains(red),
        "the first line's own colour is drawn: {coloured:?}"
    );
    let next = rows
        .iter()
        .find(|r| r.contains("plain second line"))
        .expect("the second line is drawn");
    assert!(
        !next.contains(red),
        "the first line's colour leaked into the next row: {next:?}"
    );
    assert!(
        only_the_heads_own_escapes(next),
        "and that row is still only this head's own sequences: {next:?}"
    );
}

/// **A row the person ran is the model's row with their own mark on it** — the one fact a
/// settled tool row could not say, and the whole of what the operator was missing.
///
/// MEASURED on a live head, and it is why this is a test about the PAINT: `! ls` and
/// `! ls -la` produced their two rows, the result was drawn, folded and visible at every
/// rung, and the report on it was *"no colors tho?"*. Read rather than assumed, the answer
/// is that every role on that header is chosen from `outcome`, `name`, the payload and the
/// fold and **nothing in the arm ever read `origin`** — so the operator's row and the
/// model's were already byte for byte the same, and what was absent is the one fact only
/// `origin` carries: who acted.
///
/// So two properties, and the first is what makes the second mean anything:
///
///  * **The registers are the model's, byte for byte.** Take the provenance off the
///    operator's header and it IS the model's header — same `Faint` glyph and verb, same
///    `Plain` subject, same outcome role, same count, same fold, same body — asserted on the
///    painted rows, because the words are identical either way and the register is the whole
///    of the claim.
///  * **The provenance is readable with and without colour.** The `▌` bar in
///    [`Role::UserAccent`]: the glyph and the role `user_block` already gives the operator's
///    own words, and the one their `!` line is wearing two rows above this one. Under
///    [`Palette::None`] the bar is what survives, which is the whole reason the mark is a
///    glyph and not only a colour.
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
    let bar = letibot_ui::style::Palette::Colour.paint(Role::UserAccent, "▌");
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

    // **And with no palette at all** — the pipe, `--replay` and CI case. The provenance is a
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

/// **And it is on the SCREEN, which is where the report came from.**
///
/// The two tests above hold the row `item_lines` builds; this one holds the row the operator
/// actually met — the same two rows fed the way the daemon appends them, through the walk and
/// the frame, with the palette on because the mark is a role as well as a glyph. **Both
/// halves**, because one half alone is the defect: the operator's row must wear the bar, and
/// the model's row must not, or the mark is decoration on every tool row rather than
/// provenance on this one.
#[test]
fn the_persons_mark_reaches_the_screen_and_only_their_own_row_wears_it() {
    let body: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let bar = letibot_ui::style::Palette::Colour.paint(Role::UserAccent, "▌");
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

/// **§3.1's falsification half, and the reason it exists is that this head had the
/// fix and none of the tests.**
///
/// The operator: *"leticl has two that would catch a regression and you have none,
/// so nothing proves YOUR sanitising would notice if it stopped working."* So this
/// feeds the real vocabulary through **every** surface that renders text this head
/// did not author and asserts what a terminal would be handed.
///
/// # Why the colourless half is the sharper one
///
/// With `color: false` the head emits **no escapes at all** — that is the
/// `--replay`/pipe/CI case and it is asserted elsewhere — so any `ESC` in a row is
/// proof that somebody else's text got through, SGR pair and all. With colour on,
/// the head's own sequences are present by design and the assertion narrows to the
/// ones it never writes.
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
    // A running call's progress note — the channel the gate's own sentence rides.
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

    b.jobs_pane = true;
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
        },
    )));
    b.subagents_pane = true;
    b.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TodosUpdated {
            todos: vec![letibot_sessionlog::event::TodoEntry {
                by: letibot_sessionlog::event::TodoBy::Model,
                when: None,
                content: format!("tidy up{HOSTILE}"),
                status: letibot_sessionlog::event::TodoStatus::Pending,
            }],
        },
    )));
    b.todos_pane = true;
    b.apply(ServerFrame::Event(env(
        3,
        SessionEvent::SessionRenamed {
            title: format!("my session{HOSTILE}"),
        },
    )));
    // **A note with hostile text in it, drawn through the real producer.** The
    // listing's contract is that its rows are *already-composed lines* — a note's row
    // is built by `note_lines_unfolded`, which guards the daemon's `detail` as it
    // composes it — so a test that pushed raw foreign text into `slash_out` would be
    // testing a contract no producer has. (The first version of this did exactly that,
    // and the regression commit is what exposed it.)
    b.note(Note::Warned(Warned {
        code: "gate".into(),
        detail: format!("refused{HOSTILE}"),
        ts: 0,
    }));
    // **One pane at a time, because `screen` draws one.** The first version of this
    // loop set four panes at once and said "the jobs pane" for all four, while
    // `slash_out` won the screen every time — a label that described the loop rather
    // than the frame, which is the same defect the test exists to catch.
    for (name, only) in [
        ("jobs", 0usize),
        ("subagents", 1),
        ("todos", 2),
        ("slash listing", 3),
    ] {
        b.jobs_pane = only == 0;
        b.subagents_pane = only == 1;
        b.todos_pane = only == 2;
        b.slash_out = (only == 3).then(|| ("/notes".into(), b.notes_lines()));
        // The header carries the renamed session's title whatever pane is up, so
        // every one of these frames also exercises the title.
        let shown = b.screen(160, 40);
        assert_eq!(unauthored(&shown), None, "the {name} pane");
        assert!(
            !shown.iter().any(|l| l.contains('\u{1b}')),
            "the {name} pane emitted an escape with no colour configured"
        );
        // **And the pane really was on the screen**, or the assertion above is about
        // a frame that never drew it — the mistake this loop made first time.
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

/// **A carried row is done, not pending, and the colour has to say so.**
///
/// `progress::bar` paints `processed` with `Role::Pending` and `cache` with
/// `Role::Success`. That split is the prefill's, where yellow means being
/// computed now and costing you. A row carry spends nothing — a filled cell
/// is a row that has arrived — so yellow was the wrong register: *"that one
/// is yellow"*.
///
/// Asserted on the glyphs rather than on escapes, because `Palette::None` is
/// the replay and CI case and the split survives it: `bar` uses a different
/// GLYPH per band for exactly that reason. `█` is the settled band, `▓` the
/// pending one.
#[test]
fn landed_rows_paint_as_settled_and_not_as_pending() {
    let cfg = RenderConfig {
        width: 100,
        ..Default::default()
    };
    let line = filling_line("carrying", "rows", 3, 4, 0, &cfg).join("");
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

/// [`Palette::None`] is not a monochrome theme, it is the `--replay`, pipe and
/// CI case: **no sequences at all**. The accent glyphs are what survive it, and
/// they are why the hierarchy above is carried by a step and a word rather than
/// by a colour.
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

/// Size tells you what matters before you read a word — by **attribute**, so
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

/// **R18's regression, reproduced on the bytes: a listing must not strip the colour
/// the head composed into it.**
///
/// `81990b3` applied the §3.1 sanitiser to the slash listing's rows. But a `/notes`
/// row is **the head's own composed text** — `note_lines_unfolded` paints it with
/// `sgr::RED` — so the sanitiser stripped the head's own escape. And because
/// `without_control` replaced the `ESC` with a space and left the rest behind, the
/// row read ` [31m… [0m`: an invisible sequence turned into five columns of visible
/// garbage, which is also why the wrapping broke.
///
/// Asserted on the bytes rather than the look, and **both halves**: the body of an
/// escape must not appear as text, and the head's own colour must still be there —
/// stripping it would be a different bug from leaving garbage.
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

/// **The header is facts: no accent bar, no emphasis, both halves in one quiet register** —
/// R51 item 10, and it SETTLES a deliberate divergence rather than adding a preference.
///
/// leticl recorded this as its own choice against letibot (`4110e7b`: *"This DEPARTS from
/// letibot, whose `header_line` paints the bar `UserAccent` and the title `Strong`"*). The
/// operator is now asking for it here too, so the divergence goes in favour of the new register.
///
/// **`▌` in the user-accent register is how both heads say *a person said this*.** On the row
/// the reader crosses on every return to the field, it made the session's own name read as
/// somebody's sentence — the operator: *"the project directory and session name are pinned in the
/// first row with the same blue bar we use for my messages. very confusing. just make both gray
/// and remove the bar."*
///
/// Asserted on the ESCAPES, because the words are identical either way and the register is the
/// whole of what changed: the row must carry the faint code and neither the bold nor the accent.
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
    // **The bar is DELETED, not recoloured** — a grey bar is still a bar and still claims a
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
    // The name is still *there* — this is a register change and not a deletion of the row.
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
    // The block is padded to the full **content** width, or the background
    // stops mid-row and reads as damage. Content width is the terminal less
    // both gutters; the right one is empty by design, and `term::paint` erases
    // it with the row.
    assert_eq!(line_width(row), 80 - App::GUTTER, "{row:?}");
}

/// **The model picker greens the rows this box can actually take** — the operator's ask of
/// 2026-10-04: *"model peeker should green models we have keys for."*
///
/// The fact is the daemon's (`MODEL_KEYS_KEY`), because whether a preset resolves a key is a
/// fact about this box's files and environment — a head that guessed would green exactly the
/// rows that refuse at the first turn. Beyond the colour, two honesty rules are asserted:
/// **`local` is green for a reason of its own** (nothing to authenticate, and an uncoloured
/// `local` would read as *no key* about the one row that never wanted one), and **the meaning
/// is said in words**, because `Palette::None` is the `--replay`, pipe and CI case where a
/// colour says nothing at all.

/// **A declared local model is ready, and the daemon is what says so.**
///
/// The operator, 2026-10-05: *"dense78 needs a key this box does not hold."* It
/// needs none — a LAN box, no key, no meter. `local` used to be greened by a
/// hardcoded literal, which worked exactly as long as it was the only keyless row.
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

/// **A light background, once the terminal says so, renders the light palette** — and only
/// where the feature is on: an answer the head did not ask for changes nothing.
#[test]
fn a_light_background_reply_switches_the_palette() {
    let mut a = app();
    a.cfg.color = true;
    a.key(Key::Background { light: true });
    a.screen(80, 24);
    assert_eq!(a.cfg.palette(), letibot_ui::style::Palette::Colour);
    a.set_features(rano::term::Features::ALL);
    a.screen(80, 24);
    assert_eq!(a.cfg.palette(), letibot_ui::style::Palette::Light);
    a.key(Key::Background { light: false });
    a.screen(80, 24);
    assert_eq!(a.cfg.palette(), letibot_ui::style::Palette::Colour);
}
