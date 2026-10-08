//! The frame: header, hint bar, the fit, the gutter, the cursor, the title.

use super::*;

/// **The window title is the session's name and the folder**, follows a rename, falls
/// back to the short id for an untitled session, and carries no control character from
/// a hostile title — the terminal would execute it.
#[test]
fn the_window_title_names_the_session_and_follows_a_rename() {
    let mut a = app();
    assert_eq!(
        a.window_title(),
        "letibot",
        "before a session there is only the program"
    );
    a.apply(hello(
        "s-1791387240666068000",
        vec![brief("s-1791387240666068000", "", false)],
        Hub::new("s-1791387240666068000").snapshot(),
    ));
    // After the hello, which carries the daemon's own workspace.
    a.wiring.workspace = "/Users/dead/Projects/thing".into();
    let untitled = a.window_title();
    assert!(
        untitled.starts_with("thing · "),
        "the folder leads until there is a name: {untitled}"
    );
    assert!(!untitled.contains("leticode"), "{untitled}");
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::SessionRenamed {
            title: "port the parser".into(),
        },
    )));
    assert_eq!(a.window_title(), "port the parser · thing");
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::SessionRenamed {
            title: "evil\u{1b}]0;pwned\u{7}\u{9b}31m title".into(),
        },
    )));
    let written = rano::term::terminal::window_title_text(&a.window_title());
    assert!(
        !written.chars().any(|c| c.is_control()),
        "a control character would reach the terminal: {written:?}"
    );
    assert!(written.starts_with("evil]0;pwned"), "{written:?}");
}

/// **The keymap line is centred** — the operator's ask of 2026-10-04: *"please center the
/// keymap bottom line."*
///
/// The bar arrives already painted, so the centring is measured on the visible width and the
/// assertion is a BALANCE within one column — an odd remainder has to sit on one side, and
/// naming a side would be a rule nobody asked for. **An over-long bar centres to itself**
/// (pad 0) and keeps its head, which is the half naming the first keys: the same degradation
/// the left-aligned bar had, so a narrow screen loses the same end it always did.
#[test]
fn the_keymap_line_is_centred() {
    let mut a = app();
    for w in [40usize, 60, 80, 100, 120, 210] {
        let bar = a.hint_bar(w);
        let left = bar.chars().take_while(|c| *c == ' ').count();
        let right = w.saturating_sub(visible_width(&bar));
        assert!(
            left.abs_diff(right) <= 1,
            "w={w}: {left} left, {right} right — the bar is not centred: {bar:?}"
        );
    }
    // And the over-long case keeps its head rather than losing both ends to the middle.
    let narrow = a.hint_bar(20);
    assert_eq!(visible_width(&narrow), 20, "{narrow:?}");
    assert!(narrow.starts_with("ctrl-s"), "the head went: {narrow:?}");
}

/// **The branch is on the header, beside the workspace it is a fact about** — the operator's
/// ask of 2026-10-04, and leticl's row (`src/chrome.lisp:644`) matched rather than reinvented.
///
/// Both halves, because the second is the one that matters: a field that is set draws, and a
/// repository this head could not read draws **nothing** rather than a blank that reads like a
/// clean tree — *"a header that said `main` over a directory that is not a repository would be
/// a lie in the one row nobody checks"*.
#[test]
fn the_header_carries_the_workspace_branch_and_nothing_when_there_is_none() {
    let mut a = app();
    a.session_id = "s".into();
    a.wiring.workspace = "/home/dead/Projects/letibot".into();
    // The field as the READER leaves it: the state, rendered through the format in force.
    a.git_state = Some(crate::gitfield::GitState {
        branch: "main".into(),
        detached: false,
        behind: None,
        ahead: Some(2),
        stash: None,
        action: None,
        conflict: None,
        staged: None,
        unstaged: Some(1),
        untracked: None,
    });
    a.apply_git_format();
    let header = a.header_line(200);
    assert!(
        header.contains("main") && header.contains("⇡2") && header.contains("!1"),
        "the segments are on the row, gitstatus's own glyphs: {header}"
    );
    // Beside the path and not instead of it: the path is still on the row.
    assert!(header.contains("letibot"), "{header}");

    // **Absent draws nothing at all**, which is the whole rule: no marks, no placeholder.
    let mut b = app();
    b.session_id = "s".into();
    b.wiring.workspace = "/home/dead/Projects/letibot".into();
    b.git = None;
    assert!(
        !b.header_line(200).contains('⇡'),
        "a header with no reading claims nothing: {}",
        b.header_line(200)
    );
    // And a workspace that is not set draws none of it either.
    let mut c = app();
    c.session_id = "s".into();
    c.git_state = a.git_state.clone();
    c.apply_git_format();
    assert!(
        !c.header_line(200).contains("main"),
        "{}",
        c.header_line(200)
    );
}

/// **The money meter.** `micros_usd` was computed by the daemon and read in
/// exactly one place — the one-shot `--prompt` printer — so a session driven
/// from a head never saw it. The operator, on a metered conversation: *"still
/// no money"*. Correct: the meter existed for a surface they were not using.
#[test]
fn a_metered_turn_puts_its_cost_on_the_header_and_the_total_accumulates() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A free turn lights nothing: free and unpriced are both "no number",
    // and a `$0.0000` on a local session would be noise on every header.
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(2, finished_costing("t1", None))));
    assert!(!a.header_line(200).contains('$'), "{}", a.header_line(200));

    // Two metered turns, summed.
    a.apply(ServerFrame::Event(env(3, testing::turn_started("t2"))));
    a.apply(ServerFrame::Event(env(4, finished_costing("t2", Some(33)))));
    a.apply(ServerFrame::Event(env(5, testing::turn_started("t3"))));
    a.apply(ServerFrame::Event(env(
        6,
        finished_costing("t3", Some(12_345)),
    )));
    let h = a.header_line(200);
    assert!(h.contains("$0.0124"), "33 + 12345 micro-USD: {h}");

    // The total is the conversation's, not the head's: switching sessions
    // must not carry one session's bill onto another's header.
    a.apply(hello(
        "s2",
        vec![brief("s", "one", false)],
        Hub::new("s2").snapshot(),
    ));
    assert!(!a.header_line(200).contains('$'), "{}", a.header_line(200));
}

#[test]
fn the_quit_card_can_stop_the_daemon_and_can_be_taken_back() {
    let mut a = app();
    a.clock(1_000);
    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    a.key(Key::Down);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::StopDaemon),
        "the second row"
    );

    // By number, without moving the cursor.
    let mut b = app();
    b.clock(1_000);
    b.key(Key::CtrlC);
    b.key(Key::CtrlC);
    assert_eq!(b.key(Key::Char('2')), Some(Action::StopDaemon));

    // **Esc takes it back.** A card opened by a habit keystroke has to have
    // an answer that is not an exit.
    let mut c = app();
    c.clock(1_000);
    c.key(Key::CtrlC);
    c.key(Key::CtrlC);
    assert!(c.quit_card);
    assert_eq!(c.key(Key::Esc), None);
    assert!(!c.quit_card, "esc closed it");
    assert!(!c.quit, "and the head is staying");
}

#[test]
fn esc_twice_interrupts_a_running_turn_and_says_so_when_nothing_is_running() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    assert_eq!(a.key(Key::Esc), None, "one press arms, it does not fire");
    // …and the hint changes, which is the entire mechanism by which anybody
    // learns the double-tap exists.
    assert!(a.hint_bar(120).contains("again"), "{}", a.hint_bar(120));
    assert!(matches!(a.key(Key::Esc), Some(Action::Interrupt(_))));

    let mut a = app();
    a.clock(1_000);
    a.key(Key::Esc);
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.screen(100, 20).join("\n").contains("nothing is running"));
}

#[test]
fn the_composer_is_a_field_with_a_caret_in_it_and_no_prose() {
    // The complaint, as an assertion: *"the chat prompt is basically not
    // empty — 'ask something...' and it just stays without any visual cues it
    // is actually a text input"*. Three things have to be true of an empty
    // composer: it is inside a container, the caret is in it, and there is no
    // prose in the field telling you to type.
    let mut a = app();
    let screen = a.screen(80, 20);
    let (row, col) = a.cursor().expect("a composer always has a caret");
    assert!(screen[row].contains('│'), "walls: {:?}", screen[row]);
    assert!(screen[row - 1].contains('╭'), "top: {:?}", screen[row - 1]);
    assert!(
        screen[row + 1].contains('╰'),
        "bottom: {:?}",
        screen[row + 1]
    );
    assert!(
        !screen[row].contains("ask something"),
        "no prose inside the field: {:?}",
        screen[row]
    );
    // The caret sits just past the prompt glyph, in an otherwise empty field —
    // and the field itself sits one gutter in from the terminal's edge.
    assert_eq!(col, 4 + App::GUTTER, "{:?}", screen[row]);

    // And it moves with the text.
    typed(&mut a, "why did the cache miss");
    let screen = a.screen(80, 20);
    let (row2, col2) = a.cursor().unwrap();
    assert_eq!(col2, 4 + App::GUTTER + "why did the cache miss".len());
    assert!(screen[row2].contains("why did the cache miss"));
}

/// **The caret is on the body row even when a row has been added above the box** — R51
/// item 11, and the order constraint it exists as.
///
/// leticl broke this by adding a status row: the box moved down from row 0 to row 1 and the
/// body from 1 to 2, and the caret — *the one consumer that computes against the composer's
/// rows WITHOUT composing them* — stayed where it was. The operator, one keystroke later:
/// *"hmm cursor now goes above the text i type lol."*
///
/// **This head computes it from one notion rather than a `+1` per call site** — the caret's
/// offset is `chrome.len() + caret_row`, read *after* every row above the box has been pushed —
/// and this test is what holds that property: it asserts the status row, the box's top edge and
/// the body row are three consecutive rows, in that order, with the caret on the third.
///
/// It exists because cluster 1 of this port DID add a row above the box (the turn's status),
/// which is exactly the change that broke it on the other head.
#[test]
fn the_caret_is_on_the_body_row_with_a_status_row_above_the_box() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "t1".into(),
            model: "m".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    let screen = a.screen(100, 30);
    let (row, _) = a.cursor().expect("a composer always has a caret");
    // The premise, and it is the whole point of the test: the extra row IS above the box.
    assert!(
        screen[row - 2].contains("Responding"),
        "the premise: the status row is up there: {:?}",
        screen[row - 2]
    );
    assert!(
        screen[row - 1].contains('╭'),
        "and the box's top edge is between it and the body: {:?}",
        screen[row - 1]
    );
    assert!(
        screen[row].contains('│'),
        "the caret is on the body row, between the walls: {:?}",
        screen[row]
    );
    assert!(
        screen[row + 1].contains('╰'),
        "and the bottom edge is below it: {:?}",
        screen[row + 1]
    );
}

/// The second defect: *"no margins for the main output — things are hard left
/// with literally zero space."*
///
/// Asserted on **every** row rather than on the transcript, because the failure
/// mode of fixing only the body is a header and a composer inset differently
/// from the answer, which reads worse than no margin at all.
#[test]
fn every_row_of_the_frame_starts_one_gutter_in() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::delta("t1", "a paragraph of answer that is long enough to wrap\n"),
    )));
    a.apply(ServerFrame::Event(env(3, testing::appended("s.0", "user"))));
    a.apply(ServerFrame::Event(env(4, testing::content("s.0", "why"))));
    for w in [60usize, 80, 100, 110, 120] {
        let rows = a.screen(w, 24);
        for l in rows.iter().filter(|l| !l.trim().is_empty()) {
            assert!(
                l.starts_with(&" ".repeat(App::GUTTER)),
                "at {w} columns a row is hard left: {l:?}"
            );
            assert!(
                line_width(l) <= w - App::GUTTER,
                "at {w} columns a row overran the right gutter ({}): {l:?}",
                line_width(l)
            );
        }
    }
}

/// A terminal too narrow to spare four columns gives the gutter up before it
/// gives up any content.
#[test]
fn a_very_narrow_terminal_keeps_its_content_and_drops_the_gutter() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(2, testing::delta("t1", "answer\n"))));
    let rows = a.screen(30, 20);
    assert!(
        rows.iter().any(|l| l.starts_with("answer")),
        "the gutter survived a 30-column terminal: {rows:?}"
    );
}

/// `esc` is "I did not mean to change that", which a card of values has to mean: the arrow
/// keys move the cursor and nothing else, and leaving has to leave the setting alone.
#[test]
fn esc_leaves_the_setting_alone() {
    let mut a = app();
    assert_eq!(a.command("verbosity"), None);
    a.key(Key::Down);
    a.key(Key::Down);
    assert_eq!(a.mode_sel, 0, "the cursor moved to the other end");
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.pick.is_none(), "esc closes the card");
    assert_eq!(
        a.visibility.profile(),
        Some(Profile::NORMAL),
        "esc changed the setting it was moving a cursor over"
    );
}
