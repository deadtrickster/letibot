//! The terminal pane.

use super::*;

/// **`!term <command>` is the pane, and nothing else stops being what it was.**
///
/// The verb is a **whole word**: `!terminal x` and `!terms x` are ordinary `!` lines, which
/// is what they always were, and `! ls .` is untouched. A recogniser is only honest next to
/// what it must NOT take, which is why the contrast cases are in the same test.
///
/// **And this is the refusal no longer firing.** `! mc` is refused by name
/// (`letibot_tools::exec::terminal`) because a plain `!` run's pty is a *capture* — one
/// transcript row and `/dev/null` on stdin — so a screen program draws into a row and waits
/// for a keystroke that cannot arrive. `!term mc` never reaches that path: it is a different
/// action, on a different frame, to a different daemon half, and the refusal's own remedy
/// now names the verb.
#[test]
fn a_term_line_opens_the_pane_and_an_ordinary_bang_line_is_still_a_shell_command() {
    let mut a = app();
    assert_eq!(
        pane(&mut a, b"mc"),
        Action::TermOpen {
            line: "!term mc".into()
        }
    );
    assert!(a.pane_open());

    for line in ["!terminal x", "!terms x", "!term-x", "! ls ."] {
        let mut b = app();
        typed(&mut b, line);
        assert_eq!(
            b.key(Key::Enter),
            Some(Action::OperatorShell { line: line.into() }),
            "`{line}` is not the verb and is still the operator's shell line"
        );
        assert!(!b.pane_open(), "`{line}` opened a pane");
    }

    // **The verb with nothing after it ATTACHES**, and it is not a request for `$SHELL`
    // either: it is the pane this session already has. The head opens the rectangle and sends
    // the bare line; the daemon answers with what is running and with the screen it kept —
    // see `a_bare_term_line_attaches_and_the_daemon_says_what_is_running`.
    let mut c = app();
    typed(&mut c, "!term");
    assert_eq!(
        c.key(Key::Enter),
        Some(Action::TermOpen {
            line: "!term".into()
        }),
        "a bare `!term` is an attach, and the line goes over as typed"
    );
    assert!(
        c.pane_open(),
        "the rectangle is up before the daemon answers"
    );
    assert_eq!(c.input(), "", "the line left the composer — it was sent");
}

/// **The pane takes the conversation's rectangle and gives it back, exactly.**
///
/// This is the whole of *"the conversation's rectangle given to the program with the
/// composer keeping its rows"*, and the property `letibot_ui::ansi::pane_rows` keeps:
/// the pane is resized to the rectangle it is given and returns **exactly** that many rows.
/// So the header keeps its line, the chrome under the pane keeps its rows, and nothing above
/// the pane moves when it opens.
///
/// Asserted against the frame this head drew **without** a pane, at the same size and the
/// same state — so the comparison is not against a hand-written expectation of what the
/// chrome looks like, which would rot, but against the frame the pane is supposed to be
/// occupying a slice of.
#[test]
fn the_pane_keeps_the_header_the_composer_and_the_status_their_rows() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
    shell_row(
        &mut a,
        3,
        "u2",
        "user",
        operator_row("what is in this tree?"),
    );

    let before = a.screen(80, 24);
    pane(&mut a, b"\x1b[2J\x1b[Hhello from mc\r\n");
    let during = a.screen(80, 24);
    // **`size().0`, and not `rows()`** — on this screen `rows()` hands back the cells of
    // every row (`Chunks<Cell>`), and the number the pane was given is the size. The
    // rectangle is what this test is about, so it is asked for by name.
    let room = a.term.as_ref().expect("a pane").screen.size().0;

    assert_eq!(
        during.len(),
        before.len(),
        "the frame is the terminal's height, pane or no pane"
    );
    assert_eq!(during[0], before[0], "the header keeps its row");
    assert_eq!(
        &during[1 + room..],
        &before[1 + room..],
        "the composer and the status keep their rows"
    );
    assert!(
        during.iter().any(|r| r.contains("hello from mc")),
        "the program is drawn in the rectangle: {during:?}"
    );
    assert!(!before.iter().any(|r| r.contains("hello from mc")));
    assert!(room >= 1, "the pane got a rectangle");
}

/// **The pane's rows are exactly `room`, at every size** — the property `vt-one` is
/// preserving, asserted from the head's side rather than from the screen's.
#[test]
fn the_pane_draws_exactly_the_rows_it_is_given() {
    for h in [8usize, 24, 40] {
        let mut a = app();
        a.session_id = "s".into();
        shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
        pane(&mut a, b"\x1b[2Jone\r\ntwo\r\n");
        let frame = a.screen(80, h);
        assert_eq!(frame.len(), h, "a {h}-row terminal is a {h}-row frame");
        let room = a.term.as_ref().unwrap().screen.size().0;
        assert_eq!(
            a.term.as_ref().unwrap().screen.size().1,
            80 - 2 * App::gutter(80),
            "the pane is the conversation's own width, gutter excluded"
        );
        assert!(
            room <= h,
            "the pane cannot be taller than the frame: {room} of {h}"
        );
    }
}

/// **Keys are forwarded as bytes, and the way out is not one of them.**
///
/// The pane's keyboard is the program's, and what reaches it is the bytes the operator's
/// terminal sent — **not this head's reading of them**. `ESC O A` is the application-cursor
/// spelling of *up*, and a head that decoded it into `Key::Up` and re-encoded it would send
/// `ESC [ A`, a different string to a program that asked for the first. The assertion is on
/// the bytes, including a control byte and half a UTF-8 character.
///
/// **`ctrl-\` (0x1c) is found in that stream before anything is forwarded**, so the program
/// never receives it and cannot trap it. It is also a byte `key_of` maps to nothing, so it
/// could never have arrived as a `Key` at all — see `Terminal::raw_input`.
///
/// # And the act is a DETACH — the operator's *"but i dont want it to exit"*
///
/// This test used to assert `Action::TermClose` here, which is the defect the whole split
/// exists to remove: leaving a pane ended its cgroup, so leaving `nano` killed it. What is
/// asserted now is the negative the requirement is made of — **nothing leaves the head** —
/// and the positive that makes it a detach rather than a disappearance: the pane is still
/// held, so the program is still running and `!term` has something to come back to.
#[test]
fn the_panes_keys_go_down_verbatim_and_ctrl_backslash_detaches() {
    let mut a = app();
    pane(&mut a, b"mc");

    let keys = vec![0x1b, b'O', b'A', 0x03, b'x', 0xe2];
    assert_eq!(
        a.pane_keys(&keys),
        vec![Action::TermInput {
            bytes: keys.clone()
        }],
        "the bytes, and not a key this head understood"
    );

    assert_eq!(
        a.pane_keys(b"hi\x1cz"),
        vec![Action::TermInput {
            bytes: b"hi".to_vec()
        }],
        "the bytes before the way out are the last the program gets, the key itself is never \
             one of them, and NOTHING ELSE is sent — no frame of any kind ends anything"
    );
    assert!(
        !a.pane_open(),
        "the rectangle comes back: the pane is hidden"
    );
    assert!(
        a.term.is_some(),
        "and the pane is KEPT — the program is still running, which is what `!term` attaches \
             back to"
    );
    assert_eq!(a.input(), "", "the composer saw none of it");
    // **The disclosure is the chrome line, and it is drawn while it is true** — see
    // [`App::detach`] for why the sentence is not a notice as well: a notice cannot be taken
    // back, and this one would have outlived the pane.
    let frame = a.screen(80, 24);
    assert!(
        frame.iter().any(|r| r.contains("a pane is running")
            && r.contains("!term mc")
            && r.contains("`!term close` ends it")),
        "the screen says the program is still running, and names both ways on: {frame:?}"
    );
    assert!(
        a.notes.is_empty(),
        "a detach files no row: it ends nothing, so there is no ending to disclose"
    );

    // **A detached pane forwards nothing**, which is the other half of *it is not on the
    // screen*: the keys belong to the composer again, so this is never even asked — and a
    // byte written into a pane nobody is drawing would be a keystroke the operator aimed at
    // the composer.
    assert!(
        a.pane_keys(b"more").is_empty(),
        "a detached pane takes no keys"
    );

    // The control that makes the interception mean something: with no pane, the same bytes
    // are the composer's and `0x1c` is a way out of nothing.
    let mut b = app();
    assert!(!b.pane_open());
    assert!(b.pane_keys(&keys).is_empty());
    assert!(b.pane_keys(b"\x1c").is_empty());
}

/// **Leaving gives the conversation back, and the ending stays as a row.**
///
/// This test used to assert that the frame came back **byte for byte**, with the ending
/// cleared as a notice — and *that assertion was the defect*. A pane that ended left nothing
/// behind but a sentence that faded, so a program that died at once was invisible a few
/// seconds later and there was nothing to scroll back to. What is true now is the half that
/// matters and the half that changed:
///
/// * **the program's own drawing is gone** — the rectangle is the conversation's again, and
///   the rows the program painted are not in the frame;
/// * **the transcript is back**, which is the property this test has always been about;
/// * **and the ending is a row in it**, with the line that was run and the daemon's own
///   sentence — see [`Note::Pane`].
///
/// **The ending here is the deliberate one** (`!term close`, confirmed), which is why the
/// sentence is *"you closed the terminal"* and the register is the dim `·` — a detach would
/// have filed no row at all, and a program's own exit would have been the `×`.
#[test]
fn ending_the_pane_gives_the_conversation_back_and_the_ending_stays_as_a_row() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    shell_row(&mut a, 1, "u1", "user", operator_row("! ls -la"));
    shell_row(
        &mut a,
        3,
        "u2",
        "user",
        operator_row("what is in this tree?"),
    );
    let before = a.screen(80, 24);

    pane(&mut a, b"\x1b[2J\x1b[Hmc's screen\r\n");
    assert_ne!(
        a.screen(80, 24),
        before,
        "the pane really did replace the conversation"
    );

    // The operator ends it deliberately: `!term close` is typed at the composer (which a
    // drawn pane owns, so this is the DETACHED case — the head has the pane and the card
    // names it), then confirmed with the one key that means yes.
    a.detach();
    typed(&mut a, "!term close");
    assert_eq!(
        a.key(Key::Enter),
        None,
        "the verb asks; it does not end anything on its own"
    );
    assert!(a.term_ask.is_some(), "the confirmation is up");
    assert_eq!(
        a.key(Key::Char('y')),
        Some(Action::TermClose),
        "and `y` is the yes"
    );

    a.apply(ServerFrame::TermEnded {
        reason: "you closed the terminal".into(),
    });
    assert!(!a.pane_open(), "the pane is over");
    assert!(
        a.notice.is_none(),
        "an ending is not a notice: it is a row that is still there a minute later, and \
             the notice is what used to carry it and fade: {:?}",
        a.notice
    );
    let after = a.screen(80, 24);
    // **The program's own drawing is gone from the rectangle, and the only copy of it is
    // the ending's own quote of its last rows.** Asserted that way rather than as *the text
    // is nowhere*, because the row deliberately carries what the program printed — that is
    // the fix — and *the rectangle is the conversation's again* is the property.
    let heading = after
        .iter()
        .position(|r| r.contains("!term mc"))
        .expect("the ending is a row");
    assert!(
        after[heading].contains("you closed the terminal"),
        "the ending row carries the daemon's own sentence: {after:?}"
    );
    assert_eq!(
        after[heading + 1].trim(),
        "mc's screen",
        "and the program's last rows are quoted under it, indented — not drawn as the \
             rectangle they filled a moment ago: {after:?}"
    );
    assert_eq!(
        after.iter().filter(|r| r.contains("mc's screen")).count(),
        1,
        "one copy of what the program printed, and it is the ending's: {after:?}"
    );
    assert!(
        after.iter().any(|r| r.contains("! ls -la")),
        "and the transcript is back: {after:?}"
    );
}

/// **`!term close` asks before it ends anything, and only a deliberate yes ends it.**
///
/// The operator's rule, in their words: *"yeah it is pretty much a terminal emulator - if a
/// process runs then `ending` must ask"*. So the verb raises a card and sends **nothing**;
/// the frame leaves on `y` and on nothing else.
///
/// **Enter is deliberately not the yes**, and the two reasons are asserted here rather than
/// argued: Enter is the prompt card's own key (an empty line is a real answer there, so the
/// two cards would be answerable by the same keystroke) and it is the composer's, so a stray
/// one must not be able to kill a program. Esc cancels, and so does every other key — *the
/// safe default* is the whole shape of a destructive confirmation.
///
/// **And a detach never asks**, because it ends nothing: that is the point of it being the
/// default. Asserted beside the rest so the two acts are read together.
#[test]
fn a_term_close_asks_first_and_only_a_deliberate_yes_ends_anything() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    let _ = a.screen(80, 24);
    pane(&mut a, b"nano's screen\r\n");
    // The operator leaves first — a detach, and it raises no card at all.
    a.pane_keys(b"\x1c");
    assert!(!a.pane_open());
    assert!(
        a.term_ask.is_none(),
        "a detach ends nothing, so it asks nothing"
    );

    typed(&mut a, "!term close");
    assert_eq!(
        a.key(Key::Enter),
        None,
        "the verb itself sends nothing: it is the ASK"
    );
    let ask = a.term_ask.as_ref().expect("the confirmation is up");
    assert_eq!(
        ask.line, "!term mc",
        "the card names the program in the operator's own spelling"
    );
    let card = a.screen(80, 24);
    assert!(
        card.iter()
            .any(|r| r.contains("end the pane") && r.contains("!term mc")),
        "and it says what it is about to end: {card:?}"
    );
    assert!(
        card.iter().any(|r| r.contains("y ends it")),
        "both keys, and which one is the default: {card:?}"
    );

    // **Every key that is not a deliberate yes cancels**, Enter included — and the program
    // is still running afterwards.
    for k in [Key::Enter, Key::Esc, Key::Char('n'), Key::Up] {
        let mut b = app();
        b.session_id = "s".into();
        let _ = b.screen(80, 24);
        pane(&mut b, b"mc");
        // Detached first, because that is the only state `!term close` is reachable from:
        // a drawn pane owns the keyboard.
        b.pane_keys(b"\x1c");
        typed(&mut b, "!term close");
        assert_eq!(
            b.key(Key::Enter),
            None,
            "the verb itself sends nothing: it is the ASK"
        );
        assert!(b.term_ask.is_some(), "the confirmation is up");
        assert_eq!(
            b.key(k.clone()),
            None,
            "`{k:?}` must not end a program: the safe default is the cancel"
        );
        assert!(b.term_ask.is_none(), "and the card is down");
        assert!(
            b.term.is_some(),
            "`{k:?}`: the pane is still here — nothing was ended"
        );
    }

    // And the yes: the frame leaves, and the pane's keys stop while the kill is in flight.
    assert_eq!(a.key(Key::Char('Y')), Some(Action::TermClose));
    assert!(a.term_ask.is_none());
    assert!(
        a.pane_keys(b"x").is_empty(),
        "an ending is on its way: a byte written into a pty whose program is being \
             signalled is a byte nobody will read"
    );
}

/// **A close the head cannot answer from what it holds is ASKED FOR, not guessed.**
///
/// Two cases, and they are the reason [`PaneFact`] is a three-state: a session with no pane
/// (a sentence, and **nothing sent** — `TermClose` is quiet about there being nothing to
/// end, so a head that sent it would look like it had done something) and a pane the daemon
/// says is running in a head that does not hold it (a card that names `!term <command>`, the
/// string the daemon was handed).
///
/// The third state — *not asked yet* — is the one that would otherwise be read as *no pane*,
/// and it is asserted as the hold it is: the line goes out, and the answer runs the same
/// decision.
#[test]
fn a_close_the_head_cannot_answer_is_asked_for_and_not_guessed() {
    // No pane at all: the daemon said so.
    let mut a = app();
    a.session_id = "s".into();
    a.apply(ServerFrame::TermStatus { command: None });
    typed(&mut a, "!term close");
    assert_eq!(a.key(Key::Enter), None, "nothing is sent");
    assert!(a.term_ask.is_none(), "and no card: there is nothing to end");
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("no pane to end")),
        "the head says why rather than staying silent: {:?}",
        a.notice
    );

    // A pane the DAEMON holds and this head does not: the card names it.
    let mut b = app();
    b.session_id = "s".into();
    b.apply(ServerFrame::TermStatus {
        command: Some("mc /etc".into()),
    });
    typed(&mut b, "!term close");
    assert_eq!(b.key(Key::Enter), None);
    assert_eq!(
        b.term_ask.as_ref().map(|a| a.line.as_str()),
        Some("!term mc /etc"),
        "the card names what the daemon says is running"
    );
    assert_eq!(b.key(Key::Char('y')), Some(Action::TermClose));

    // **Not asked yet**: the read goes out and the line is HELD, then the same decision runs
    // on the answer. A head that read this as *no pane* would refuse to end a program the
    // operator can see.
    let mut c = app();
    c.session_id = "s".into();
    assert_eq!(c.term_fact, PaneFact::Unasked, "nothing has answered yet");
    typed(&mut c, "!term close");
    assert_eq!(
        c.key(Key::Enter),
        Some(Action::TermStatus),
        "the head asks rather than guessing"
    );
    assert!(c.close_pending, "and it holds the line");
    c.apply(ServerFrame::TermStatus {
        command: Some("nano notes.txt".into()),
    });
    assert!(!c.close_pending, "the answer runs the decision");
    assert_eq!(
        c.term_ask.as_ref().map(|a| a.line.as_str()),
        Some("!term nano notes.txt"),
        "and the card is the one the answer implies"
    );
}

/// **The pane's existence is drawn from the status read, and it is not a row.**
///
/// The operator's rule: a detach leaves no ending row, and *the pane's existence is a fact
/// the head draws from `TermStatus`*. So a head that is not drawing a pane the daemon says
/// is running draws **one line** above the composer, naming the program and both verbs, and
/// files nothing in the conversation; when the answer becomes `None` the line goes with it,
/// because it is a fact about now and not a disclosure about a moment.
#[test]
fn the_pane_line_is_drawn_from_the_status_read_and_is_not_a_row() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    let _ = a.screen(80, 24);
    assert!(
        !a.screen(80, 24)
            .iter()
            .any(|r| r.contains("a pane is running")),
        "nothing is drawn before the daemon has said anything"
    );

    a.apply(ServerFrame::TermStatus {
        command: Some("mc /etc".into()),
    });
    let frame = a.screen(80, 24);
    assert!(
        frame.iter().any(|r| r.contains("a pane is running")
            && r.contains("!term mc /etc")
            && r.contains("!term close")),
        "the line names what is running and both verbs: {frame:?}"
    );
    assert!(
        a.notes.is_empty(),
        "and it is NOT a row: a detach is not an event, so there is no ending to file"
    );

    // The daemon says there is nothing running any more: the line stops being true and goes.
    a.apply(ServerFrame::TermStatus { command: None });
    assert!(
        !a.screen(80, 24)
            .iter()
            .any(|r| r.contains("a pane is running")),
        "a fact that is no longer true is no longer drawn"
    );
}

/// **A program that ends on its own is the same frame as a refusal to start**, and the
/// sentence is the whole of the difference.
#[test]
fn a_pane_that_ends_on_its_own_closes_the_same_way_a_refusal_does() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    pane(&mut a, b"bye\r\n");
    a.apply(ServerFrame::TermEnded {
        reason: "the program exited with 3".into(),
    });
    assert!(!a.pane_open());
    assert!(
        a.screen(80, 24)
            .iter()
            .any(|r| r.contains("the program exited with 3")),
        "the ending is a row, and the reason the daemon gave is on it: {:?}",
        a.screen(80, 24)
    );

    // And a `TermEnded` for a pane this head never opened — a second head attached to the
    // same session, which the frames are fanned out to — is dropped rather than said: it is
    // not this head's pane and it has no rectangle to restore.
    let mut b = app();
    b.session_id = "s".into();
    b.apply(ServerFrame::TermEnded {
        reason: "the program exited with 0".into(),
    });
    assert!(b.notice.is_none(), "{:?}", b.notice);
    // The same for the bytes: a pane nobody opened is not opened by them.
    b.apply(ServerFrame::TermOutput {
        bytes: b"nobody's screen".to_vec(),
    });
    assert!(!b.pane_open());
}

/// **The sessions pane's tree, as a picture** — an instrument, not an assertion.
///
/// MEASURED on this box 2026-10-05, because the numbers are not what the tests' fixtures
/// imply: **174 of the store's 280 sessions carry a `parent_session_id`**, this session has
/// 95 children of its own, the header reads `1/106`, and every one of those 174 is **one
/// level down** — there is no depth-2 row in the store at all. So the tree is the pane's
/// majority case rather than a corner, and the two things the fixtures do not exercise are
/// the ones an instrument is for: the **grandchild** (reached by a child's own `task`, which
/// no fixture on this box has produced) and the **three-digit row number** (106 roots, where
/// `{:>2}` gives rows 100+ one column more than rows 1–9).
///
/// Print it, do not assert it:
///
/// ```text
/// cargo test -p letibot-tui --lib -- --ignored --nocapture show_the_sessions_pane_tree
/// ```
#[test]
#[ignore]
fn show_the_sessions_pane_tree() {
    let mut grand = brief("s-grand", "the grandchild", false);
    grand.parent_session_id = Some("s-child-a".into());
    let mut child_a = brief("s-child-a", "the first child", true);
    child_a.parent_session_id = Some("s-root".into());
    let mut child_b = brief("s-child-b", "the second child", false);
    child_b.parent_session_id = Some("s-root".into());
    let mut far = brief("s-other-sub", "somebody else's child", false);
    far.parent_session_id = Some("s-other".into());
    // A hundred roots, so the numbering reaches three digits — the state this box is in.
    //
    // **`grand` IS IN THIS LIST, and the first draft of this instrument forgot it** — the
    // fixture built the grandchild, set its parent, and then never pushed it, so the depth-2
    // frame drew the whole daemon and the family frame drew 104 roots: two frames that looked
    // like pane defects and were the fixture lying. Which is the argument for an instrument
    // that prints rather than asserts — an assertion would have been written to match.
    let mut family = vec![
        brief("s-root", "this conversation", false),
        child_a,
        grand,
        child_b,
        brief("s-other", "another conversation", false),
        far,
    ];
    for i in 0..100 {
        family.push(brief(&format!("s-filler-{i:03}"), "a conversation", false));
    }

    let mut a = app();
    a.apply(hello(
        "s-root",
        family.clone(),
        Hub::new("s-root").snapshot(),
    ));
    a.picker = true;
    // **A ruler, because the question here is COLUMNS.** *Does the pane render the tree*
    // is a question about which column a row's name starts in, and an eyeball on a frame
    // answers it wrongly at one column per level — which is what the first look did.
    let ruler: String = (0..96)
        .map(|i| char::from_digit((i % 10) as u32, 10).unwrap())
        .collect();
    let show = |label: &str, a: &App| {
        eprintln!("\n──── {label} ────");
        eprintln!("|{ruler}");
        for l in a.picker_lines(96) {
            eprintln!("|{l}");
        }
    };

    show(
        "collapsed — 104 roots, and this one has two children folded into it",
        &a,
    );
    a.expanded.push("s-child-a".into());
    show("s-child-a expanded — the grandchild at depth 2", &a);
    a.expanded.push("s-root".into());
    show(
        "the root expanded too — both children, then the grandchild",
        &a,
    );
    a.expanded.clear();
    a.session_id = "s-grand".into();
    show(
        "standing IN the grandchild — the family view, not the daemon",
        &a,
    );
}

/// **The newest agent is on top** — the operator's ask, 2026-10-05: *"fix agents pane - the
/// ordering is off - most recent agents must be on top"*.
///
/// The daemon's list is in creation order, **oldest first** (the store's own `created_ms`),
/// and the children this head watched spawn were *appended* after it — so the pane drew the
/// child that had just been started at the BOTTOM of its group, which for an agent pane is the
/// one row anybody opened it to see.
///
/// **Two running children, so the groups do the ordering for nobody.** The pane already draws
/// running rows above the finished group (`the_running_subagent_is_drawn_above_…`), so a
/// fixture that mixed the two would pass on the group's own rule and say nothing at all about
/// recency. Both are `running` in the list — the one thing a brief states about a live child —
/// and both the list's order and the rows' order on the glass are asserted.
#[test]
fn the_newest_agent_is_at_the_top_of_the_pane() {
    let mut a = app();
    let mut older = brief("s-sub-old", "the older one", true);
    older.parent_session_id = Some("s".into());
    older.created_ms = 1_000;
    let mut newer = brief("s-sub-new", "the newer one", true);
    newer.parent_session_id = Some("s".into());
    newer.created_ms = 2_000;
    // **The daemon's order, which is the order the pane must not keep**: oldest first.
    a.apply(hello(
        "s",
        vec![brief("s", "parent", true), older, newer],
        Hub::new("s").snapshot(),
    ));
    assert_eq!(
        a.subagents
            .iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["s-sub-new", "s-sub-old"],
        "the newest child is not first in the pane's list"
    );
    a.key(Key::CtrlG);
    let screen = a.screen(100, 24);
    let at = |needle: &str| {
        screen
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is not on the pane:\n{}", screen.join("\n")))
    };
    assert!(
        at("the newer one") < at("the older one"),
        "the pane drew the older agent above the newer one:\n{}",
        screen.join("\n")
    );

    // **A row nobody can date sorts LAST**, not first: `created_ms` is `0` for a replay and for
    // a brief from a daemon that does not stamp the row, and an undated row belongs at the
    // bottom rather than at the top pretending to be the newest thing in the pane.
    let mut b = app();
    let mut dated = brief("s-sub-dated", "the dated one", true);
    dated.parent_session_id = Some("s".into());
    dated.created_ms = 5;
    let mut undated = brief("s-sub-undated", "the undated one", true);
    undated.parent_session_id = Some("s".into());
    // `created_ms` stays `0`, which is the whole of this half.
    b.apply(hello(
        "s",
        vec![brief("s", "parent", true), undated, dated],
        Hub::new("s").snapshot(),
    ));
    assert_eq!(
        b.subagents
            .iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["s-sub-dated", "s-sub-undated"],
        "a row nobody can date sorted above one somebody can"
    );
}

/// **A named fill draws the bar, an unnamed bulk announcement is said, and a live
/// append does neither.**
///
/// Three sources, three rules, and the whole of R9:
///
/// * the **bar** is the daemon naming an operation it is running (`Filling`), and only
///   one big enough is worth a cat (`MIN_FILLING`);
/// * the **sentence** is the head's own, for a *bulk announcement* a snapshot left
///   outstanding — and it is **not** gated by `MIN_FILLING`, because a three-row batch
///   that never lands is exactly what it is for;
/// * a **live** `TranscriptAppended` triggers neither. It is body-less for the R2
///   window of every ordinary message, so a trigger built on "some row lacks a body"
///   announced a carry that was not happening — the defect this replaces.
#[test]
fn a_named_fill_draws_the_bar_and_an_unnamed_snapshot_is_said_but_a_live_append_is_not() {
    // --- the bar, from the daemon.
    let mut a = app();
    a.clock(0);
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Filling {
            what: "carrying the conversation onto the new prompt".into(),
            unit: "rows".into(),
            done: 0,
            total: 2_702,
        },
    )));
    let carried = a.screen(80, 20).join("\n");
    assert!(
        carried.contains("carrying the conversation") && carried.contains("of 2702 rows"),
        "a named fill draws the bar: {carried}"
    );
    // **A small one draws no bar** — under `MIN_FILLING` — and still no sentence, because
    // the daemon did name it.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Filling {
            what: "importing an opencode conversation".into(),
            unit: "rows".into(),
            done: 0,
            total: 40,
        },
    )));
    let small = a.screen(80, 20).join("\n");
    assert!(
        !small.contains("of 40 rows") && !small.contains("announced"),
        "a 40-row fill gets no bar and needs no sentence: {small}"
    );

    // --- the sentence, from an unnamed SNAPSHOT.
    let mut b = app();
    b.clock(1_000);
    b.apply(snapshot_hello("snap", 4));
    let waiting = b.screen(80, 20).join("\n");
    assert!(
        waiting.contains("4 row(s) announced, waiting for the daemon to send them"),
        "a snapshot with no bodies is a bulk announcement and is said: {waiting}"
    );
    // **Past the patience it stops claiming to be progress** — and this is the sentence
    // that found the operator a real daemon-side hole, so it is not gated by the bar's
    // threshold either.
    b.clock(1_000 + BODY_PATIENCE + 1);
    let stalled = b.screen(80, 20).join("\n");
    assert!(
        stalled.contains("4 row(s) announced to this head and never filled"),
        "after the patience it says they are not coming: {stalled}"
    );
    // **And it NAMES them.** A count is the least useful form of this fact and the ids are
    // what make it checkable; the head held them all along and printed only the number.
    assert!(
        stalled.contains("snap.0, snap.1, snap.2"),
        "the ids must be on the line, sorted, so the claim can be checked: {stalled}"
    );
    // **And ALL of them on `/status`, because the line is trimmed to the frame.** An
    // 80-column line holds a count and about three names; a pane holds the rest, and the
    // operator asked for the names rather than the count.
    typed(&mut b, "/status");
    b.key(Key::Enter);
    // Tall on purpose: the pane is longer than a screen and scrolls, and the row this
    // asserts is near its end. A short screen would assert the frame's height rather than
    // the pane's content.
    let pane = b.screen(120, 200).join("\n");
    assert!(
        pane.contains("unfilled"),
        "`/status` must carry the pair `orphan` was missing: {pane}"
    );
    for i in 0..4 {
        assert!(
            pane.contains(&format!("snap.{i}")),
            "every announced row must be named in the pane, not just counted: {pane}"
        );
    }
    b.key(Key::Esc);
    // And it must not claim a cause it cannot see.
    assert!(
        !stalled.contains("did not send them"),
        "a head sees what arrived and cannot know whether the daemon withheld or the \
             content was lost: {stalled}"
    );
    // And a body landing takes its id off the count, so the line goes.
    for i in 0..4 {
        b.apply(ServerFrame::Event(env(
            100 + i as u64,
            testing::content(&format!("snap.{i}"), "a row"),
        )));
    }
    let filed = b.screen(80, 20).join("\n");
    assert!(
        !filed.contains("announced"),
        "the bulk announcement is complete, so the line goes: {filed}"
    );

    // --- **THE DEFECT: the sentence counts rows the snapshot never announced.**
    //
    // The trigger is the SNAPSHOT's body-less ids ([`Bulk`]); the number drawn is
    // `items.len() - arrived`, which is every body-less row the head holds. Those are
    // different sets, so a live row with no body inflates a count that the sentence
    // then attributes to the daemon — and on the operator's own screen it read
    // `2 row(s) announced and never filled in`, where a fresh snapshot of the same
    // session has **0** rows with no body. Measured on the live daemon 2026-09-23.
    //
    // This asserts the inflation directly: a good snapshot (every id filled) PLUS one
    // live row whose body never arrives. `bulk` is `None` — the snapshot announced
    // nothing — and nothing here should ever be called an announcement.
    let mut d = app();
    d.clock(1_000);
    d.apply(snapshot_hello("good", 3));
    for i in 0..3 {
        d.apply(ServerFrame::Event(env(
            100 + i as u64,
            testing::content(&format!("good.{i}"), "a row"),
        )));
    }
    // The snapshot is complete, so the trigger is gone.
    let clean = d.screen(80, 20).join("\n");
    assert!(!clean.contains("announced"), "premise: {clean}");

    // Now one live row, body-less, and no body ever comes.
    d.apply(ServerFrame::Event(env(
        200,
        testing::appended("live.0", "assistant"),
    )));
    d.clock(1_000 + BODY_PATIENCE + 1);
    let after = d.screen(80, 20).join("\n");
    assert!(
        !after.contains("announced and never filled in"),
        "a live body-less row is the R2 window, and the daemon never announced it in a \
             snapshot — the sentence must not blame the daemon for it:\n{after}"
    );

    // --- and the mixed case, which is the one that reached the operator's screen: one
    // snapshot row that never landed AND one live row with no body. The sentence's count
    // must be the snapshot's, not the total, or it attributes the live row to the daemon.
    let mut e = app();
    e.clock(2_000);
    e.apply(snapshot_hello("mixed", 2));
    // One of the two lands.
    e.apply(ServerFrame::Event(env(
        300,
        testing::content("mixed.0", "a row"),
    )));
    // And a live row with no body joins it.
    e.apply(ServerFrame::Event(env(
        301,
        testing::appended("live.1", "assistant"),
    )));
    e.clock(2_000 + BODY_PATIENCE + 1);
    let mixed = e.screen(80, 20).join("\n");
    assert!(
        mixed.contains("1 row(s) announced to this head and never filled"),
        "the count must be the SNAPSHOT's unlanded rows (1), not every body-less row the \
             head holds (2) — the extra one was never announced:\n{mixed}"
    );

    // --- and a live append is not a bulk announcement at all, however long it sits.
    let mut c = app();
    c.clock(0);
    for i in 0..4 {
        c.apply(ServerFrame::Event(env(
            i + 1,
            testing::appended(&format!("s.{i}"), "user"),
        )));
    }
    let ordinary = c.screen(80, 20).join("\n");
    assert!(
        !ordinary.contains("announced"),
        "a live body-less row is the R2 window, not a carry: {ordinary}"
    );
    c.clock(BODY_PATIENCE + 1);
    let ordinary_late = c.screen(80, 20).join("\n");
    assert!(
        !ordinary_late.contains("never filled in"),
        "and it never becomes a stall however long it sits: {ordinary_late}"
    );
}

/// A call the turn was cut short in the middle of does not leave the screen.
///
/// The hazard in "one row per call": while the live pane is drawing a turn,
/// that turn's assistant rows deliberately draw none of their own unsettled
/// calls. If the pane then stands down with a call still unanswered, nobody is
/// drawing it — and the operator is looking at a turn that asked for three
/// files with no sign it ever did.
#[test]
fn a_call_the_turn_was_interrupted_in_the_middle_of_still_says_it_asked() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("t1.0", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "t1.0".into(),
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
    // While it is running the pane owns it and the row says nothing.
    assert!(
        !a.screen(120, 24).join("\n").contains("no result"),
        "not while it is still running"
    );
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::TurnInterrupted {
            turn_id: "t1".into(),
            reason: "operator pressed esc twice".into(),
            partial_kept: true,
        },
    )));
    let screen = a.screen(120, 24).join("\n");
    assert!(
        screen.contains("→ Read TODO.md · no result"),
        "and once nothing is drawing it, the row does:\n{screen}"
    );
}

/// **A pane's Enter belongs to the pane, even with words in the composer.**
///
/// The operator, 2026-10-03, having gone to the jobs pane and pressed Enter with a
/// stray character sitting in the composer: *"the pane own keyboard in a way, so enter
/// is a pane thing."* Every pane arm used to gate its own Enter on
/// `editor.text().is_empty()`, so **a keystroke aimed at the pane became "send what I
/// was typing"** — and what was in the composer was a `\`, which is what reached the
/// model as a prompt.
///
/// Both halves are asserted, because both are the point: Enter is not a submit, and the
/// words are still there to be sent deliberately — held, not eaten. The second is the
/// property that makes this safe to do to a half-written line at all.
#[test]
fn a_panes_enter_is_the_panes_even_with_words_in_the_composer() {
    let mut a = App::new(plain_cfg(80));
    a.session_id = "s1".into();
    a.apply(jobs_frame(
        "s1",
        vec![daemon_job("j1", "cargo test --workspace", true)],
    ));
    a.jobs_pane = true;
    a.set_composer("\\");
    let act = a.key(Key::Enter);
    assert!(
        !matches!(act, Some(Action::Prompt(_))),
        "Enter in the jobs pane sent the composer instead of opening the job: {act:?}"
    );
    assert_eq!(
        a.editor.text(),
        "\\",
        "and the line is held where it was — a pane that ate it would lose the words \
             the operator had typed"
    );
    // **And the pane's own act happened**, which is the half that makes this ownership
    // rather than mere suppression: Enter is not "swallowed while a pane is up", it is
    // the pane's. Asserted because a blanket swallow would pass the two checks above
    // and would be a worse answer — a key that does nothing where the operator asked
    // for something.
    assert!(
        a.job_out.as_ref().is_some_and(|j| j.job == "j1"),
        "Enter opened the job's output, which is what the pane's Enter means"
    );

    // **The other half: with no pane up, Enter is still the composer's.** A rule that
    // stopped a bare prompt from sending would be a worse defect than the one it fixed.
    //
    // A *fresh* head rather than the one above, because that one now has the
    // job-output view open — and the view owning Enter is the same rule working, not
    // an exception to it. Asserting the control on a head with an overlay up would be
    // asserting that the rule does not apply to overlays.
    let mut b = App::new(plain_cfg(80));
    b.session_id = "s1".into();
    b.set_composer("\\");
    assert!(
        matches!(b.key(Key::Enter), Some(Action::Prompt(t)) if t == "\\"),
        "with no pane open, Enter sends the line"
    );
}
