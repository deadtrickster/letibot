//! The link, attaching, reconnecting, stopping, the session picker.

use super::*;

/// **A bare `!term` attaches: the rectangle comes up, the daemon says what is running, and
/// the screen it kept is drawn in it.**
///
/// This is the operator's way back, and the whole of it is three facts:
///
/// * the head sends the **bare verb** — it does not know what is running, and cannot: the
///   program is the session's and the line that started it may have been typed by another
///   head, or by this one before a session switch;
/// * the daemon answers with [`ServerFrame::TermAttached`], and the head **says** what it
///   was told — *"saying what is running in it"* is the requirement, and a screen alone
///   does not say it (`mc`'s panels look like `mc`'s panels);
/// * and the bytes that follow are the **daemon's replay** of a screen the head never drew,
///   which is what proves the attach rather than a redraw: the pane the head is holding was
///   created empty a moment ago, so everything on it came from the daemon.
#[test]
fn a_bare_term_line_attaches_and_the_daemon_says_what_is_running() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    // One frame first: a pane's rectangle is the last frame's, which is all this layer
    // knows until it draws one.
    let _ = a.screen(80, 24);

    typed(&mut a, "!term");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::TermOpen {
            line: "!term".into()
        })
    );
    assert!(a.pane_open());
    // **Nothing has been drawn yet** — the rectangle is empty because the head has never
    // seen this program. That is the state the replay has to fill.
    assert!(
        !a.screen(80, 24).iter().any(|r| r.contains("mc's screen")),
        "the head cannot have this screen: it never drew it"
    );

    a.apply(ServerFrame::TermAttached {
        command: "mc /etc".into(),
    });
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("!term mc /etc")),
        "the operator is told what is running in the pane they attached to: {:?}",
        a.notice
    );
    a.apply(ServerFrame::TermOutput {
        bytes: b"\x1b[2J\x1b[Hmc's screen\r\n".to_vec(),
    });
    assert!(
        a.screen(80, 24).iter().any(|r| r.contains("mc's screen")),
        "the daemon's replay is drawn in the rectangle: {:?}",
        a.screen(80, 24)
    );

    // **And the line the daemon named is what an ending names**, not the bare verb the head
    // typed: the row a person reads has to say which pane ended. (The reason is the daemon's
    // own sentence; this fixture uses the one a deliberate close gets, which is the daemon's
    // wording since protocol 34 — see `harnessd`'s `Terminals::CLOSED`.)
    a.apply(ServerFrame::TermEnded {
        reason: "you closed the terminal".into(),
    });
    assert!(
        a.screen(80, 24)
            .iter()
            .any(|r| r.contains("!term mc /etc") && r.contains("you closed the terminal")),
        "the ending names the pane the daemon said was running: {:?}",
        a.screen(80, 24)
    );
}

/// **A session with no pane says so, and the sentence is a row.**
///
/// The other half of the attach: `!term` with nothing running is not silence and not a
/// rectangle left standing empty — the daemon answers with the same `TermEnded` every pane
/// that could not start uses, and the head files it like any other ending.
#[test]
fn attaching_to_a_session_with_no_pane_says_so() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    let _ = a.screen(80, 24);
    typed(&mut a, "!term");
    assert!(a.key(Key::Enter).is_some());
    assert!(a.pane_open());
    a.apply(ServerFrame::TermEnded {
        reason: "this session has no pane to attach to — `!term COMMAND` starts one. \
                     Nothing was attached."
            .into(),
    });
    assert!(!a.pane_open(), "the rectangle comes back");
    assert!(
        a.screen(80, 24)
            .iter()
            .any(|r| r.contains("no pane to attach to")),
        "and the daemon's sentence is a row: {:?}",
        a.screen(80, 24)
    );
}

/// **A program that exits while the operator is away still leaves its row.**
///
/// The second half of *a detach must not hide anything*: the head keeps the pane while it
/// is detached (so it keeps feeding the screen from the frames that are still arriving),
/// and the ending therefore arrives with the last rows the program left — the row they
/// would have seen had they been looking.
///
/// **The register is the difference and it is asserted too**: this ending was not the
/// operator's act, so it is the notice register — the `×` a `!term close` does not get.
#[test]
fn a_program_that_exits_while_detached_still_leaves_its_ending_row() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    let _ = a.screen(80, 24);
    pane(&mut a, b"mc: starting\r\n");

    // The operator leaves: nothing is sent, the rectangle goes, the program keeps running.
    assert_eq!(
        a.pane_keys(b"\x1c"),
        Vec::new(),
        "a detach sends nothing at all"
    );
    assert!(!a.pane_open());

    // It writes while they are away — the frames still arrive, and the head still feeds the
    // screen it is holding. Then it exits.
    a.apply(ServerFrame::TermOutput {
        bytes: b"mc: not found\r\n".to_vec(),
    });
    a.apply(ServerFrame::TermEnded {
        reason: "the program exited with 127".into(),
    });

    assert!(!a.pane_open());
    assert!(a.term.is_none(), "the pane is over and dropped");
    let frame = a.screen(80, 24);
    assert!(
        frame
            .iter()
            .any(|r| r.contains("!term mc") && r.contains("the program exited with 127")),
        "the ending is a row, with the daemon's own sentence on it: {frame:?}"
    );
    assert!(
        frame.iter().any(|r| r.contains("mc: not found")),
        "and with the last thing the program printed — the rows a detach must not hide: \
             {frame:?}"
    );
    // **`×` and not `·`**: nobody chose this ending, so it is the answer to what the
    // operator is about to read rather than housekeeping.
    assert!(
        frame
            .iter()
            .any(|r| r.trim_start().starts_with('×') && r.contains("!term mc")),
        "an ending nobody asked for is the notice register: {frame:?}"
    );
}

/// **A bare `!term` after a detach attaches back to the SAME run.**
///
/// The operator's way back, and the whole of it is that the pane was never ended: the head
/// sends the bare verb, the daemon answers with what is running in the pane it kept and
/// replays the screen, and the head draws that screen again. A head that had *ended* the
/// pane on `ctrl-\` would be opening a new one here — or being refused.
#[test]
fn a_bare_term_line_after_a_detach_attaches_back_to_the_same_run() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    let _ = a.screen(80, 24);
    pane(&mut a, b"\x1b[2J\x1b[Hmc's screen\r\n");
    a.pane_keys(b"\x1c");
    assert!(!a.pane_open(), "detached");

    typed(&mut a, "!term");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::TermOpen {
            line: "!term".into()
        }),
        "a bare `!term` is an attach: the daemon is asked for the pane it has"
    );
    assert!(
        a.pane_open(),
        "the rectangle is back before the daemon answers"
    );
    // The daemon's answer: the same command, and the screen it kept.
    a.apply(ServerFrame::TermAttached {
        command: "mc /etc".into(),
    });
    a.apply(ServerFrame::TermOutput {
        bytes: b"\x1b[2J\x1b[Hmc's screen\r\n".to_vec(),
    });
    assert_eq!(
        a.term.as_ref().map(|p| p.line.clone()),
        Some("!term mc /etc".into()),
        "and the line is the daemon's own name for the program — the same run"
    );
    assert!(
        a.screen(80, 24).iter().any(|r| r.contains("mc's screen")),
        "with the screen replayed into the rectangle"
    );
}

/// **A sub-session is listed under the conversation that spawned it, and it switches like any
/// other** — the operator's correction of 2026-10-03, and the half of it that is letibot's.
///
/// *"yes subagents are not even scratch session they are session, just sub sessions"*, and
/// *"why readonly? subagent session is more like you driving others via tmux"*. Two sites used to
/// drop every row with a parent — `Hello` and `Sessions` — so a session a head can post to, and
/// get an answer from, was thrown away on the way in. What that filter was protecting against was
/// noise, and the answer to noise is nesting rather than silence.
///
/// Four properties, and the third is the one a two-enumerations regression would break:
///
/// * **Kept**: both rows are in `sessions` after a `Hello`.
/// * **Collapsed by default**: one row drawn, so this screen looks exactly as it did before
///   sub-sessions were listed at all.
/// * **The listing and the keys agree**: the child is indented under its parent, and the cursor
///   that reaches it by `↓` is the row `Enter` switches to — both read from [`App::session_rows`].
/// * **`←` folds it back**, on whichever of the two rows the cursor is on.
#[test]
fn a_sub_session_is_listed_under_its_parent_and_switches_like_any_other() {
    let mut child = brief("s-child", "the child", false);
    child.parent_session_id = Some("s-root".into());
    let mut a = app();
    a.apply(hello(
        "s-root",
        vec![brief("s-root", "root", false), child],
        Hub::new("s-root").snapshot(),
    ));
    // **Kept.** The daemon told this head about two sessions and it holds two.
    assert_eq!(
        a.sessions.len(),
        2,
        "a sub-session was discarded on the way in"
    );
    a.picker = true;

    // **Collapsed by default**, which is the whole reason the default is not noisy.
    assert_eq!(a.session_rows().len(), 1, "the child is shown unexpanded");
    let closed = a.screen(100, 30).join("\n");
    assert!(
        !closed.contains("the child"),
        "a collapsed child was drawn anyway:\n{closed}"
    );

    // **The fold, on the row the cursor is on** — it starts on the session this head is in.
    assert_eq!(a.key(Key::Right), None, "expanding is not a daemon action");
    assert_eq!(a.session_rows().len(), 2, "expanding shows the child");
    let open = a.screen(100, 30).join("\n");
    assert!(
        open.contains("the child"),
        "the child is not drawn:\n{open}"
    );

    // **The cursor reaches it, and the row it reaches is the row Enter takes.** One
    // enumeration, so the indentation the eye sees and the row the key acts on cannot drift.
    assert_eq!(a.key(Key::Down), None);
    let picked = a.screen(100, 30).join("\n");
    let mark_at = |needle: &str| {
        picked
            .lines()
            .find(|l| l.contains(needle))
            .and_then(|l| l.find('\u{25b8}'))
    };
    assert!(
        mark_at("the child") > mark_at("root"),
        "the child is not indented under its parent:\n{picked}"
    );
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Switch("s-child".into())),
        "Enter on a sub-session must be an ordinary attach"
    );

    // **`←` folds it back** — and a child row closes its PARENT, so the key means *back up the
    // tree* on the row you just arrived at rather than nothing at all.
    a.key(Key::Left);
    assert_eq!(a.session_rows().len(), 1, "left did not fold the tree");
}

/// **The version the daemon is TOLD to be is checked at the handshake, and a skew is
/// said rather than fatal.**
///
/// leticl does this on `Hello` and this head did not, which left the skew announcing
/// itself on the first frame that failed to parse — the expensive moment, one frame
/// late, and phrased as a decoder's complaint. The `Hello` is the cheap moment: nothing
/// has been read yet and the direction is known.
///
/// Three things are asserted together, because they are one behaviour: a matching
/// version says **nothing at all** (a line here would be furniture), a differing one
/// says which way round it is in the transcript, and **the head stays attached** — a
/// skew is usually survivable, which is R3's whole argument, so exiting here would be
/// the failure framing the fix.
#[test]
fn a_protocol_skew_is_said_at_the_handshake_and_the_head_stays_attached() {
    let hub = Hub::new("s");

    // The normal case: both halves the same build, and nothing is said. §13.2b cuts
    // both ways — a field that is always zero is no more readable than one that is
    // absent, and a head that announces "versions match" on every attach teaches the
    // operator that this line can be skipped.
    let mut a = app();
    a.apply(hello_at(
        "s",
        vec![brief("s", "one", false)],
        hub.snapshot(),
        letibot_sessionlog::protocol::PROTOCOL_VERSION,
    ));
    assert_eq!(
        a.daemon_protocol,
        Some(letibot_sessionlog::protocol::PROTOCOL_VERSION)
    );
    assert!(
        a.notes.is_empty(),
        "nothing to say, so nothing said: {:?}",
        a.notes
    );
    a.command("status");
    // **Taller than it was, again.** `/status` is a scrolling pane, and the row this test
    // reads is near its end: R17 added three rows (gaps, behind, orphan) and this change
    // added a fourth (`first byte`, the counter `model_slow_first_byte` now lands in) plus
    // its gloss. A frame that used to reach the bottom of the list does not, and the
    // failure is a missing row rather than a wrong one.
    let status = a.screen(120, 96).join("\n");
    let row = status
        .lines()
        .find(|l| l.contains("protocol"))
        .unwrap_or_else(|| panic!("/status does not name the version: {status}"));
    assert!(row.contains("same build"), "{row}");
    a.key(Key::Esc);

    // A NEWER daemon: a reading problem, and this head can survive it.
    let newer = letibot_sessionlog::protocol::PROTOCOL_VERSION + 3;
    let mut a = app();
    a.apply(hello_at(
        "s",
        vec![brief("s", "one", false)],
        hub.snapshot(),
        newer,
    ));
    let said = a
        .notes
        .last()
        .map(|(_, n)| note_lines_unfolded(&a.cfg, n).join(" "))
        .unwrap_or_default();
    assert!(said.contains(&newer.to_string()), "{said}");
    assert!(said.contains("NEWER"), "{said}");
    assert!(said.contains("Restarting the daemon"), "{said}");
    // It says what it will do about them, which is the part the operator needs to
    // know before the first one arrives.
    assert!(said.contains("skipped"), "{said}");
    // And the head is still here, still drawing, still able to take a frame.
    assert!(!a.should_quit());
    assert_eq!(a.session_id, "s");
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    assert!(a.turn_busy(), "the head kept working");

    // An OLDER daemon: a writing problem, and quiet until it is fatal — so this is the
    // sentence that matters most.
    let older = letibot_sessionlog::protocol::PROTOCOL_VERSION - 1;
    let mut a = app();
    a.apply(hello_at(
        "s",
        vec![brief("s", "one", false)],
        hub.snapshot(),
        older,
    ));
    let said = a
        .notes
        .last()
        .map(|(_, n)| note_lines_unfolded(&a.cfg, n).join(" "))
        .unwrap_or_default();
    assert!(said.contains("OLDER"), "{said}");
    assert!(
        said.contains("closing the socket"),
        "it must say the session can end on the next command, not merely that the \
             daemon is old: {said}"
    );
    assert!(!a.should_quit());
    // `/status` names the direction too, and has it after the note has scrolled away.
    a.command("status");
    // **Taller than it was**: `/status` gained three rows in R17 (gaps, behind,
    // orphan), and a fixed-height frame that used to reach the bottom of the list
    // no longer does.
    // **And the same height for the same reason.**
    let status = a.screen(120, 96).join("\n");
    let row = status
        .lines()
        .find(|l| l.contains("protocol"))
        .unwrap_or_else(|| panic!("{status}"));
    assert!(row.contains("OLDER"), "{row}");
    assert!(row.contains(&older.to_string()), "{row}");
}

/// **A `Switch` re-reads the same `Hello`, so the sentence is filed once.** The
/// daemon answers a switch with a second `Hello` on the same connection — that is the
/// whole of this head's switch path — and a skew announced twice would read as two
/// skews. `note` dedupes on `(code, detail, ts)`, which is what makes that true, and
/// this pins it rather than trusting it.
#[test]
fn a_version_skew_is_said_once_however_many_hellos_arrive() {
    let hub = Hub::new("s");
    let mut a = app();
    let newer = letibot_sessionlog::protocol::PROTOCOL_VERSION + 1;
    for _ in 0..3 {
        a.apply(hello_at(
            "s",
            vec![brief("s", "one", false)],
            hub.snapshot(),
            newer,
        ));
    }
    assert_eq!(a.notes.len(), 1, "said three times: {:?}", a.notes);
}

/// **A head whose daemon goes away says so, keeps its screen, and does not exit.**
///
/// The defect this is against: `client.ack(...)?` propagated out of the driver's
/// loop, through `main`, and the process was gone — the operator's view of a
/// conversation that was still on disk, taken with a socket that had merely been
/// closed. The half a test can hold on this side is the head's: the *state* is on
/// the head, the conversation is not cleared, the line says what happened, and a
/// prompt typed at it is refused rather than shown as sent.
#[test]
fn a_head_with_no_daemon_says_so_keeps_its_screen_and_holds_the_line() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(testing::delta("t1", "the answer I already have"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    let before = a.screen(100, 24).join("\n");
    assert!(before.contains("the answer I already have"), "{before}");

    // The socket goes: the reader notices, and says what it can.
    a.link_down("the daemon closed the connection");
    assert!(a.detached());
    assert!(
        !a.should_quit(),
        "a dropped socket is not a reason to leave"
    );

    // **The screen it had is still the screen it has.** The transcript is not
    // cleared, the turn is not forgotten, and nothing is redrawn as if the session
    // had ended — plus one line saying what is wrong, above the composer where the
    // eye crosses on the way to typing.
    let after = a.screen(100, 24).join("\n");
    assert!(
        after.contains("the answer I already have"),
        "the conversation was taken away: {after}"
    );
    assert!(
        after.contains("daemon connection is down"),
        "nothing said the link was down: {after}"
    );
    assert!(after.contains("reconnecting"), "{after}");
    // And the keys still work: this is a head, not a corpse.
    assert!(!after.contains("esc closes this"), "{after}");

    // **A line typed here must not look sent.** The trap is the echo: `submit`
    // pushes it into `pending_prompts`, the screen draws `queued · <their words>`,
    // and there is no queue anywhere — so when the daemon comes back the sentence is
    // gone and it was on the screen as held the whole time.
    typed(&mut a, "did that go anywhere?");
    assert_eq!(a.key(Key::Enter), None, "nothing may leave a dead link");
    assert!(
        a.pending_prompts.is_empty(),
        "shown as queued with no queue behind it: {:?}",
        a.pending_prompts
    );
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("no daemon connection"), "{said}");
    // The words are still theirs, in the field they typed them into.
    assert_eq!(a.input(), "did that go anywhere?");

    // **The daemon comes back, and the recovery is the protocol's own.** The `Hello`
    // is what says the link is up — it is the frame that seats the connection — and
    // it says so into the conversation, with the seq it asked from, because the gap
    // is about to be filled with events and unexplained events are noise.
    a.clock(1_000 + 4_400);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    assert!(!a.detached());
    let back = a.screen(100, 24).join("\n");
    assert!(!back.contains("daemon connection is down"), "{back}");
    assert!(back.contains("daemon is back after 4.4s"), "{back}");
    assert!(back.contains("Resuming from seq"), "{back}");
    // The ordinary reconnect succeeds on its first try, so there is no attempt count
    // to show — and the sentence must not show an empty one.
    assert!(
        back.contains("the daemon is back after 4.4s."),
        "no empty brackets: {back}"
    );
}

/// **`reconnecting…` is a state with a clock, not a word.**
///
/// Under the impatient threshold the line says what happened; past it the operator is
/// told how long, how many attempts, and what they can do — because a head that has
/// been saying one word for a minute has told them nothing they could not see, and is
/// indistinguishable from a head that is about to come back.
#[test]
fn a_link_that_stays_down_stops_saying_reconnecting_and_starts_saying_how_long() {
    let mut a = app();
    a.clock(1_000);
    a.link_down("the daemon closed the connection");

    // Twelve seconds in: still "reconnecting", and no advice yet — under the
    // threshold a drop is usually over before the sentence is read.
    a.clock(13_000);
    let soon = a.screen(120, 24).join("\n");
    assert!(soon.contains("reconnecting"), "{soon}");
    assert!(
        !soon.contains("letibot --status"),
        "advice nobody needs yet: {soon}"
    );

    // Past it: the elapsed time, the attempts, and the two commands that answer
    // "is the daemon gone".
    a.clock(1_000 + LINK_IMPATIENT_MS + 1);
    a.reconnect_failed("connection refused");
    a.reconnect_failed("connection refused");
    let mut a2 = a;
    a2.clock(1_000 + 72_000);
    let long = a2.screen(120, 24).join("\n");
    assert!(long.contains("trying for 1m12s"), "{long}");
    assert!(long.contains("2 attempts"), "{long}");
    assert!(long.contains("letibot --status"), "{long}");
    assert!(
        long.contains("keeps trying"),
        "it must not read as having given up: {long}"
    );

    // And when it *does* get back after those two failures, the count is shown —
    // once, in brackets, not an empty pair.
    a2.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let up = a2.screen(120, 24).join("\n");
    assert!(
        up.contains("daemon is back after 1m12s (2 attempts)"),
        "{up}"
    );
}

/// **The backoff is the head's, and it is flat.** A caller asks once per pass and the
/// head answers whether the two seconds are up; two attempts inside one window would
/// be a busy loop against a socket that is not there.
#[test]
fn the_head_asks_for_a_reconnect_only_after_the_backoff() {
    let mut a = app();
    a.clock(1_000);
    assert!(!a.should_reconnect(), "attached heads reconnect never");
    a.link_down("the daemon closed the connection");
    assert!(
        !a.should_reconnect(),
        "the first attempt waits out the same backoff"
    );
    a.clock(1_000 + RECONNECT_BACKOFF_MS);
    assert!(a.should_reconnect());

    // A failed attempt pushes the window out again and counts itself.
    a.reconnect_failed("connection refused");
    assert!(!a.should_reconnect());
    a.clock(1_000 + RECONNECT_BACKOFF_MS + RECONNECT_BACKOFF_MS);
    assert!(a.should_reconnect());
    let drawn = a.screen(120, 24).join("\n");
    assert!(drawn.contains("connection refused"), "{drawn}");
}

/// **A `Bye` does not answer a stop this head asked for, and it does not silence the
/// farewell.**
///
/// The operator's second report, in one test: *"it reports the server exited within a
/// second — while `harnessd` is in fact hung and has to be killed with `--force`."*
///
/// The `Bye` on this path is published by the daemon's **connection thread** the moment
/// the `Stop` is taken (`registry.close()`), and the **worker** running the operator's
/// command is a different thread that has not ended. MEASURED on a live daemon,
/// 2026-10-06: `Bye` at 519 µs after the stop, the daemon's process still in `/proc`.
///
/// So both halves are asserted here, and both were wrong before the change: the head
/// used to leave on the `Bye` (`should_quit` true), and it used to say **nothing at
/// all** on the way out (`stop_farewell` `None`), which a person reads as *it stopped*.
#[test]
fn a_bye_does_not_report_a_stop_that_has_not_happened() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // The head asks, and the daemon does what it does: the ack, then the goodbye from
    // the connection thread — with the worker still inside the command.
    a.stop_began("dead", true, Some(4242), 1_000);
    a.apply(ServerFrame::Bye {
        reason: "daemon shutting down".into(),
    });
    a.clock(1_200);

    assert!(
        !a.should_quit(),
        "the daemon's process has NOT gone — `gone` is false and nothing has observed \
             it leave — so the head must not leave on a frame that the connection thread \
             published before the worker ended. This is the lie: it reported the server \
             exited within a second."
    );

    // And when the deadline passes with the process still there, the head says what it
    // saw rather than nothing.
    a.clock(1_000 + STOP_DEADLINE_MS + 1);
    assert!(a.should_quit(), "the deadline is the head's own way out");
    let said = a
        .stop_farewell()
        .expect("a stop that did not happen must be said on the way out");
    assert!(
        said.contains("began shutting down"),
        "the daemon DID hear the request — that is what the goodbye is evidence for, \
             and it is the useful half: {said}"
    );
    assert!(
        said.contains("still there"),
        "the sentence is about the PROCESS, and the process is still there: {said}"
    );
    assert!(said.contains("4242"), "and it names it: {said}");
    assert!(
        said.contains("--force"),
        "`--force` is the next move and it is said as what it is: {said}"
    );
}

/// **And when the process really has gone, there is nothing to say.** The correction
/// above must not turn every orderly stop into a warning.
#[test]
fn a_stop_whose_process_is_gone_says_nothing_even_after_a_bye() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.stop_began("dead", true, Some(4242), 1_000);
    a.apply(ServerFrame::Bye {
        reason: "daemon shutting down".into(),
    });
    // What `Link::watch_stop` observes on every tick, and the only thing that is a fact
    // about the process.
    a.stopping_mut().expect("a stop is in flight").gone = true;
    a.clock(1_200);
    assert!(
        a.should_quit(),
        "the process has gone, so the question is answered"
    );
    assert_eq!(
        a.stop_farewell(),
        None,
        "a stop that worked is not worth a sentence"
    );
}

/// **A `Bye` is final and a dropped socket is not.** The daemon saying goodbye is the
/// end of the conversation — a refusal, a skew, a shutdown — and the head leaves with
/// the reason on it. Turning that into a retry is leticl's measured defect: a refusal
/// the daemon meant as final became a two-second loop under a head that never
/// attached and never exited.
///
/// **And it is still final for a head that did not ask for the stop**, which is the
/// half the correction above must not break: a skew, a refusal and another head's stop
/// all arrive this way, with no `stopping` in flight, and none of them is a question
/// this head is waiting on.
#[test]
fn a_bye_leaves_and_never_looks_like_a_link_to_reconnect() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Bye {
        reason: "protocol version 19, this daemon speaks 22".into(),
    });
    assert!(a.should_quit(), "a goodbye is the end");
    assert_eq!(
        a.farewell().map(str::to_string),
        Some("protocol version 19, this daemon speaks 22".into()),
        "and the reason outlives the screen"
    );
    // The pump dies right after the `Bye`, and the driver reports that as well: it
    // must not turn a head that is leaving into one that is reconnecting.
    a.link_down("the daemon closed the connection");
    assert!(!a.detached(), "a head on its way out does not reconnect");
    assert!(
        a.screen(100, 24)
            .join("\n")
            .contains("daemon: protocol version")
    );
}

#[test]
fn a_late_head_shows_the_accumulated_text_and_then_increments() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    for w in ["Hello", " ", "world"] {
        hub.publish(testing::delta("t1", w));
    }
    let att = hub.attach("tui", "test", Caps::default(), 0);
    let mut a = app();
    a.apply(ServerFrame::Hello {
        protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
        session_id: "s".into(),
        head_id: att.head_id.clone(),
        dropped: att.dropped,
        snapshot: att.snapshot.map(Box::new),
        resumed_from: att.resumed_from,
        scrubbed: att.scrubbed,
        wiring: Default::default(),
        sessions: Vec::new(),
    });
    hub.publish(testing::delta("t1", "!"));
    feed(&mut a, &hub, &att.head_id);
    assert_eq!(a.turn.as_ref().unwrap().text.raw(), "Hello world!");
}

#[test]
fn a_compact_is_asked_of_the_session_this_head_is_in() {
    // Not attached: refused with a line, not an action the daemon would have
    // to guess about.
    let mut a = app();
    typed(&mut a, "/compact");
    assert!(a.key(Key::Enter).is_none());
    // Attached: the action, naming nobody — the daemon compacts the session
    // the head is sitting in, which is the one /compact can reach.
    let hub = letibot_sessionlog::hub::Hub::new("s");
    let att = hub.attach(
        "tui",
        "test",
        letibot_sessionlog::protocol::Caps::default(),
        0,
    );
    let mut a = app();
    a.apply(ServerFrame::Hello {
        protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
        session_id: "s".into(),
        head_id: att.head_id.clone(),
        dropped: att.dropped,
        snapshot: att.snapshot.map(Box::new),
        resumed_from: att.resumed_from,
        scrubbed: att.scrubbed,
        wiring: Default::default(),
        sessions: Vec::new(),
    });
    typed(&mut a, "/compact");
    assert_eq!(a.key(Key::Enter), Some(Action::Compact));
}

/// **A head that switches away and back RE-ASKS — the frames it sends are the assertion.**
///
/// The operator's report is a drawn state that came back *on its own*: *"so the counter is
/// gone"*, then, minutes later, *"yep and now it is back. wtf"*, over a subagent that ran
/// throughout. A state restored by a frame nobody asked for is a state whose restore depends
/// on when an unrelated round-trip lands, and this is the half that removes the dependence:
/// the head puts the question at the one moment it knows it needs the answer.
///
/// **Asserted on the actions, not on a helper.** `refetch_session_facts` is private and
/// calling it directly would prove that a function the head never calls does what it says —
/// which is the shape of test this file keeps deleting. The actions here are what the driver
/// turns into frames on the wire, so this is *the head asked*.
#[test]
fn a_switch_away_and_back_re_asks_for_the_rows_the_jobs_and_the_settings() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    let attached = a.take_actions();
    for want in [Action::ListSessions, Action::ListJobs, Action::Settings] {
        assert!(
            attached.contains(&want),
            "the attach did not ask for {want:?}: {attached:?}"
        );
    }

    // **Away.** The daemon answers a `Switch` with a second `Hello` on the same connection,
    // and that is the whole of the switch path — so the ask rides the same arm.
    a.key(Key::CtrlG);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    assert!(
        a.take_actions().contains(&Action::ListSessions),
        "the switch into a child did not re-ask for its children"
    );

    // **And back**, which is the return the operator was watching.
    assert_eq!(a.key(Key::Esc), Some(Action::Switch("s".into())));
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    let back = a.take_actions();
    for want in [Action::ListSessions, Action::ListJobs, Action::Settings] {
        assert!(
            back.contains(&want),
            "the return from the switch did not re-ask for {want:?}: {back:?}"
        );
    }
}

/// **THE REGRESSION: the counter does not go away on a switch and come back later.**
///
/// This is the operator's report, reproduced. A child is watched alive; the head switches
/// into it and back; and at the instant it comes back the daemon's session list says
/// `running: false` — because the child is parked on its own background job, which is
/// `SessionStatus::running`'s own definition of *no turn is generating in it right now*.
///
/// **Before the fix the count was gone here**, and stayed gone until some later list reply
/// happened to catch the child generating: a switch sends `since_seq = 0`, so nothing is
/// replayed, the rows were cleared, and the list's one bit was the only evidence left — and
/// the fold read that bit as *finished*. The parent's view now carries its children on every
/// snapshot, which is the events' own conclusion, so the row survives the round trip.
///
/// **The control is the second half, and it is what makes the first mean something.** A
/// snapshot with no children in it is a head with no evidence, and there the list's `false`
/// is read as finished — the honest answer for a child nobody watched, and the reason the
/// fix is *the snapshot carries them* rather than *the count never drops*.
#[test]
fn a_running_child_is_still_counted_after_a_switch_away_and_back() {
    // The parent's log, with one child watched come alive. **Through a real `Hub`**, so the
    // snapshot is the daemon's own fold of the event and not a fixture written to agree
    // with the head.
    let hub = Hub::new("s");
    hub.publish(SessionEvent::Subagent {
        subagent_id: "s-sub-1".into(),
        state: "running".into(),
        prompt: "find the bug in the reader".into(),
        role: "coder".into(),
        task: "find the bug in the reader".into(),
        model: String::new(),
        answer: None,
    });
    let snapshot = hub.snapshot();
    assert_eq!(
        snapshot.subagents.len(),
        1,
        "the premise: the parent's view carries its child"
    );

    let mut a = app();
    a.apply(hello("s", a_family(), snapshot.clone()));
    assert!(
        count_row(&mut a).contains("1 subagent running"),
        "the premise: the child is counted while the head is at home"
    );

    // Away, and back. **The list says `running: false` at the instant of return** — the
    // child is between turns — and it says it in both directions, so the only thing that
    // differs between the two halves of this test is the snapshot.
    let mut parked = a_family();
    parked[1].status.running = false;
    a.key(Key::CtrlG);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    assert_eq!(a.session_id, "s-sub-1");
    a.apply(hello("s", parked.clone(), snapshot.clone()));
    assert_eq!(a.session_id, "s");

    // **The count is still there.** The child is alive: the head watched it come alive and
    // has seen no completion, and the list's `false` is not one.
    let row = count_row(&mut a);
    assert!(
        row.contains("1 subagent running"),
        "the counter went away on a switch and would have to come back on its own: {row}"
    );
    assert_eq!(
        a.subagents[0].state, "running",
        "the row, not a second number"
    );
    assert_eq!(a.subagents[0].task, "find the bug in the reader");

    // **THE CONTROL.** The same round trip with a snapshot that carries no children — a head
    // that never watched the spawn. There the list is all there is, and its `false` is read
    // as finished, because a head that cannot tell *parked* from *ended* must not count a
    // child it cannot vouch for.
    let mut blind = app();
    blind.apply(hello("s", parked.clone(), Hub::new("s").snapshot()));
    assert_eq!(
        blind.subagents.len(),
        2,
        "the list still lists the children: the rows are drawn, they are just not vouched for"
    );
    assert!(
        !count_row(&mut blind).contains("subagent running"),
        "a child nobody watched is counted on the list's instantaneous word alone"
    );
}

/// **The jobs pane is NOT the subagent pane's defect, and this is the measurement.**
///
/// `App::load` clears `self.jobs` on a switch for the same reason it clears the subagent
/// rows — a carried row is a question about a job that was never in this session — but the
/// jobs have a bootstrap read and the subagents did not: the `Hello` arm queues `ListJobs`
/// on **every** attachment, which a `Switch` is answered with, and `ServerFrame::Jobs`
/// refills the table for the session that answered. So a switched head re-asks and the pane
/// comes back on the daemon's own word, where the subagent pane had nothing to ask with.
#[test]
fn a_switch_re_asks_for_the_jobs_and_the_daemons_answer_refills_the_pane() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.take_actions();
    assert_eq!(a.key(Key::CtrlG), None);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
    // The switch's own `Hello` asks again — this is the half that was missing for
    // subagents, which had no frame to ask with.
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    assert!(
        a.take_actions().iter().any(|a| *a == Action::ListJobs),
        "the switched head did not re-ask for its jobs"
    );
    a.jobs = Vec::new();
    a.apply(ServerFrame::Jobs {
        session_id: "s-sub-1".into(),
        jobs: vec![daemon_job("j1", "cargo test", true)],
    });
    assert_eq!(a.jobs.len(), 1, "the daemon's table did not land");
}

/// **R58's other half: a peek answered with ROWS draws a session, not a dump.**
///
/// The daemon's `Peeked::snapshot` is the child's own transcript — the same rows the
/// transcript renders — so the pane goes through `item_lines`, and what a reader sees is a
/// **tool card** (`▸ Ran ls -la /etc · ok · …`), not the plain `· bash — ok` strings the
/// event-ring fallback is stuck with. The two must not look alike, and the way this pins
/// that is a word only one of the two renderers produces: the verb the card maps `bash` to,
/// with the target its own arguments seed.
///
/// And it asserts the absence of the degraded sentence, because a render that shows the
/// card and still claims it could not read the rows is the other half of the same defect.
#[test]
fn a_peek_drawn_from_the_session_rows_shows_a_tool_card_and_does_not_claim_degrading() {
    let mut a = app();
    let mut snap = Hub::new("s-sub-1").snapshot();
    snap.items = vec![
        letibot_sessionlog::view::SnapshotItem {
            item_id: "s-sub-1.a".into(),
            kind: "assistant".into(),
            ledger_head: "beef".into(),
            ts: 0,
            item: Some(TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    arguments: r#"{"command":"ls -la /etc"}"#.into(),
                }],
                truncated: false,
            }),
        },
        letibot_sessionlog::view::SnapshotItem {
            item_id: "s-sub-1.r".into(),
            kind: "tool_result".into(),
            ledger_head: "beef".into(),
            ts: 0,
            item: Some(TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "total 0".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    ];
    a.apply(ServerFrame::Peeked {
        session_id: "s-sub-1".into(),
        dropped: 0,
        events: vec![],
        snapshot: Some(Box::new(snap)),
    });
    let v = a.sub_out.as_ref().expect("the view opened");
    assert!(
        !v.degraded,
        "rows were asked for and sent: this is not the fallback"
    );
    assert_eq!(v.session_id, "s-sub-1");
    let text = v.lines.join("\n");
    // The card, and the target the snapshot's own turn seeded — neither word is in the
    // plain event-ring fallback, which is the property being pinned. The verb is the one
    // the card maps `bash` to (`Ran`, not the tool's own name), and the subject is quoted
    // because the argument has a space in it — both are `item_lines`' doing, not this
    // pane's, which is exactly the point: one renderer, not two.
    assert!(text.contains(r#"▸ Ran "ls -la /etc" · ok"#), "{text}");
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains(r#"Ran "ls -la /etc""#), "{screen}");
    assert!(
        !screen.contains("event ring"),
        "a card was drawn; the pane must not also claim it could not read the rows:\n{screen}"
    );
}

/// **A reading of a directory that is not a repository is ABSENT, and the head remembers
/// which directory it read** — so a session switch re-reads at once and the interval applies
/// only within one workspace.
#[test]
fn a_refresh_of_something_that_is_not_a_repository_draws_nothing_and_is_remembered() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-git-none-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    let mut a = app();
    a.clock(1_000);
    a.wiring.workspace = dir.display().to_string();
    a.refresh_git();
    assert!(
        a.git.is_none(),
        "a directory that is not a repository must draw nothing, not a bare field: {:?}",
        a.git
    );
    assert_eq!(a.git_read.0, dir.display().to_string());
    assert_eq!(
        a.git_read.1, 1_000,
        "the reading is stamped with the loop's clock"
    );
    // A second call inside the interval does not re-read — which is the point of the stamp —
    // and a workspace that CHANGED is read at once rather than waiting it out.
    a.clock(1_500);
    a.refresh_git();
    assert_eq!(a.git_read.1, 1_000, "re-read inside the interval");
    a.clock(1_600);
    a.wiring.workspace = "/home/dead/Projects/letibot".into();
    a.refresh_git();
    assert_eq!(
        a.git_read.1, 1_600,
        "a workspace that changed is read at once"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The head asks for the settings on attach.** The daemon answers
/// `ClientFrame::Settings` and never pushes the rows, so a head that had not
/// opened `/mode` or `/config` had none — and the header, which reads the
/// live `model` row, fell back to the model `Hello` named. The operator, on a
/// session answered by deepseek: *"restarted the letibot - still qwen"*.

#[test]
fn attaching_asks_for_the_settings_so_the_header_is_not_stale() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    assert!(
        a.take_actions().contains(&Action::Settings),
        "attach asks for them"
    );
    // Before the answer, the header falls back to Hello's model — which is
    // what it always did, and is right until something better arrives.
    assert!(a.header_line(200).contains("qwen-3.8-flash-next"));
    a.apply(model_settings("deepseek/deepseek-flash", &["local"]));
    assert!(a.header_line(200).contains("deepseek/deepseek-flash"));
}

/// A daemon serving more than this head says so, because stopping it is
/// then somebody else's business too.
#[test]
fn the_card_names_the_other_heads_that_would_lose_the_daemon() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::HeadAttached {
            head_id: "h2".into(),
            kind: "tui".into(),
            identity: "someone".into(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::HeadAttached {
            head_id: "h3".into(),
            kind: "tui".into(),
            identity: "another".into(),
        },
    )));
    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("other head"), "{screen}");
}

/// **The two events a head answers without drawing are COUNTED, not `Control`** — R53 §1.2.
///
/// Both are `SessionEvent`s, so both are session content: read, deliberately not drawn as a
/// row of their own, and counted. `Control` is documented as *"Not an event: a Hello, a
/// Resync, a command reply"*, which is what a head sends *about the connection* rather than
/// what the session did.
///
/// **Why this test exists rather than only the fix.** R53's own words: *"nothing asserts the
/// count for these two events, and the comment reads as the specification, so a reader
/// checking the file finds an argument for the correct behaviour sitting on top of the
/// incorrect one."* That is the failure mode this file already has one instance of — a
/// docstring describing three colours over a function that returns a plain string — and the
/// protection is not a better comment, it is an assertion that fails when the word changes.
#[test]
fn the_two_events_a_head_answers_without_drawing_are_counted_not_control() {
    let mut a = app();
    assert_eq!(
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::OperatorCallAllowed {
                call_id: "oc1".into(),
                name: "bash".into(),
                who: "dead".into(),
                arguments: "\"true\"".into(),
            },
        ))),
        Disposition::Filtered,
        "an admitted operator call is read and not drawn — it must count, or \"the event \
             never came\" and \"this head drops it\" look the same"
    );
    assert_eq!(
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::ScreenRequested {
                req_id: "r1".into(),
            },
        ))),
        Disposition::Filtered,
        "a screen request is answered by the frame, not by a row — and it is still counted"
    );
    // And the queue the answer rides on is untouched by the disposition: the ask is still
    // there for the driver to answer after the paint.
    assert_eq!(a.screen_requests, vec!["r1".to_string()]);
}

/// The same rule one layer up: **rendering** the history does not grow with
/// the session either.
///
/// `the_body_a_frame_builds_…` asserts the frame is the window. It says
/// nothing about how many rows were rendered to build it, and that was the
/// hole: every row whose body arrived threw the whole rendered transcript
/// away, so a session of `n` rows cost `n²/2` renders. At 400 rows that is
/// 80,000 against 400 — and the count, not a stopwatch, is the thing a test
/// can hold.
///
/// The bound is deliberately loose (`4n`): a row is legitimately re-rendered
/// when its own round changes under it, and pinning this to the exact number
/// would make it a test of the current round shape rather than of the rule.
/// The waiting frame, as the operator sees it. Ignored; `--ignored --nocapture`.
#[test]
#[ignore]
fn show_the_attach_frame() {
    let mut a = app();
    a.begin_attach_at(0);
    for t in [0u64, 120, 240, 360] {
        a.clock(t);
        eprintln!("--- t = {t} ms");
        for (i, l) in a.screen(60, 20).iter().enumerate() {
            if !l.trim().is_empty() {
                eprintln!("{i:>2} |{l}|");
            }
        }
    }
}

/// **The frame drawn before the daemon has answered claims nothing.**
///
/// A head takes the screen and draws before `HeadClient::attach` returns, because
/// that round trip carries the whole snapshot and can be a fifth of a second on a
/// busy daemon. What it draws must not be the empty-transcript banner: "this
/// session has said nothing yet" is false when the truth is "nobody has told this
/// head yet", and a head that asserts it on attach to a full session is lying on
/// its first frame.
#[test]
fn the_frame_before_the_attach_claims_nothing_about_the_session() {
    let mut a = app();
    a.begin_attach();
    let frame = a.screen(80, 24);
    let flat = frame.join("\n");
    assert!(
        !flat.contains("has said nothing yet"),
        "the pre-attach frame asserts the session is empty: {flat:?}"
    );
    assert!(
        !flat.contains("letibot\n"),
        "the pre-attach frame draws the empty-session banner anyway"
    );
    // The chrome is there, which is the whole point of drawing early: the
    // composer's own hint bar is the row that says the program is alive. It is
    // asserted on the *tail* — `ctrl-s sessions` — rather than on the keys that
    // used to open it (`ctrl+c exit`), because those are deliberately gone now:
    // see `Editor::hint`.
    assert!(
        flat.contains("ctrl-s sessions"),
        "no chrome at all: {flat:?}"
    );
    assert_eq!(frame.len(), 24);

    // And once the daemon has answered, a genuinely empty session *does* say so.
    a.apply(hello(
        "s",
        Vec::new(),
        Snapshot {
            session_id: "s".into(),
            seq: 0,
            dropped: 0,
            items_dropped: 0,
            items: Vec::new(),
            turn: None,
            open_decisions: Vec::new(),
            settled_decisions: Vec::new(),
            warnings: Vec::new(),
            heads: Vec::new(),
            subagents: Vec::new(),
        },
    ));
    let frame = a.screen(80, 24);
    let flat = frame.join("\n");
    assert_eq!(a.attaching, false, "the Hello did not clear it");
    assert!(
        flat.contains("has said nothing yet"),
        "an empty session after the Hello must say so: {flat:?}"
    );
}

/// **A snapshot full of body-less rows draws nothing, and renders as bodies land.**
///
/// The old shape drew a bar here by *counting* the rows with no body — which, being
/// a count of a symptom, also fired on every ordinary turn. The bar is the daemon's
/// now, so a snapshot that carries {n} body-less rows draws no bar at all (nothing
/// has named an operation), and each row renders the moment its body arrives.
///
/// The premise is asserted first and it is the whole test: the snapshot must really
/// arrive with body-less rows in it.
#[test]
fn a_snapshot_full_of_bodiless_rows_draws_no_bar_and_renders_as_bodies_land() {
    let hub = letibot_sessionlog::hub::Hub::new("s");
    let n = 40usize;
    for i in 0..n {
        hub.publish(letibot_sessionlog::SessionEvent::TranscriptAppended {
            item_id: format!("s.{i}"),
            kind: "user".into(),
            ledger_head: String::new(),
        });
    }
    let att = hub.attach(
        "tui",
        "test",
        letibot_sessionlog::protocol::Caps::default(),
        0,
    );
    let snap = att.snapshot.expect("a snapshot");
    assert_eq!(
        snap.items.iter().filter(|i| i.item.is_none()).count(),
        n,
        "the premise: the snapshot carries {n} rows with no body, which is what \
             a fork delivers"
    );
    let mut a = app();
    a.apply(ServerFrame::Hello {
        protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
        session_id: "s".into(),
        head_id: att.head_id.clone(),
        dropped: att.dropped,
        snapshot: Some(Box::new(snap)),
        resumed_from: att.resumed_from,
        scrubbed: att.scrubbed,
        wiring: Default::default(),
        sessions: Vec::new(),
    });
    let first = a.screen(80, 40).join("\n");
    // **No bar.** The head has been told nothing about an operation, and a count of
    // body-less rows is exactly the proxy this design removed.
    assert!(
        !first.contains("carrying the conversation"),
        "a body-less snapshot named a carry nothing asked for: {first}"
    );
    assert!(!first.contains("of {n} rows"), "{first}");

    // And the rows render as their bodies land.
    for i in 0..10 {
        a.apply(ServerFrame::Event(env(
            100 + i as u64,
            testing::content(&format!("s.{i}"), "a line of conversation"),
        )));
    }
    let moved = a.screen(80, 40).join("\n");
    assert!(
        moved.contains("a line of conversation"),
        "the bodies did not render: {moved}"
    );
}

/// **A fill's bar goes when the view that was measuring it is replaced.**
///
/// Measured on the operator's own head, 2026-10-04: it stood at `897 of 2000 rows —
/// restoring the stored conversation` and did not move.
///
/// A `Filling` tick's only exit is a tick whose `done` has reached `total`, and a republish
/// of 2000 rows **overruns the head's 1024-event queue**. The hub does not block — it
/// demotes the head (`Inner::append_and_fan`) and stops writing to it altogether, so every
/// later tick is skipped, the completing one included. The head is handed a snapshot
/// instead. Without this, the head sat waiting for a tick it had already been told would
/// not come, and the bar outlived the operation by the rest of the session.
#[test]
fn a_snapshot_ends_a_fill_whose_ticks_were_thrown_away() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Filling {
            what: "restoring the stored conversation".into(),
            unit: "rows".into(),
            done: 897,
            total: 2000,
        },
    )));
    assert!(a.filling.is_some(), "the tick armed the bar");
    let armed = a.screen(100, 30).join("\n");
    assert!(armed.contains("897 of 2000 rows"), "{armed}");

    // **The demotion's own delivery**: a snapshot, with the reason it happened.
    a.apply(ServerFrame::Resync {
        reason: "queue of 1024 overflowed at seq 1800".into(),
        dropped: 0,
        snapshot: Box::new(Hub::new("s").snapshot()),
        scrubbed: Default::default(),
    });
    assert!(
        a.filling.is_none(),
        "the bar must not outlive the stream that fed it"
    );
    let after = a.screen(100, 30).join("\n");
    assert!(
        !after.contains("of 2000 rows"),
        "and it must leave the screen: {after}"
    );
    // A fill that is still running re-arms itself on its next tick, which is what makes
    // clearing here safe rather than lossy.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Filling {
            what: "restoring the stored conversation".into(),
            unit: "rows".into(),
            done: 960,
            total: 2000,
        },
    )));
    let again = a.screen(100, 30).join("\n");
    assert!(
        again.contains("960 of 2000 rows"),
        "a fill still going comes back on its next tick: {again}"
    );
}

#[test]
fn the_header_can_name_what_the_session_is_talking_to_before_any_turn() {
    // §4.4. It read `no turn yet` for a freshly attached head, because
    // `TurnStarted { model }` was the only one of the three facts that reached a
    // head and it only arrives when a turn starts — so nothing could say what
    // the session was about to talk to at the one moment somebody was about
    // to. The daemon has known the model since it parsed its own command
    // line; it says so on `Hello`, and the header — not the composer's
    // border, which the eye crosses on every return to the field — is where
    // it renders. The dialect and the endpoint do not ride along.
    let mut a = app();
    let empty = Hub::new("s").snapshot();
    a.apply(hello("s", vec![brief("s", "", false)], empty));
    let header = a.header_line(200);
    assert!(header.contains("qwen-3.8-flash-next"), "{header}");
    assert!(
        !header.contains("qwen3.8"),
        "the dialect is not the model's double: {header}"
    );
    assert!(
        !header.contains("127.0.0.1:8080"),
        "the endpoint is the daemon's business: {header}"
    );
}

#[test]
fn a_turn_joined_from_a_snapshot_shows_no_elapsed_rather_than_an_epoch() {
    // Found by switching into a session that was mid-turn — the case the whole
    // switch feature exists for. A snapshot has no timestamps, `started_ms` is
    // 0, and `last_ms - 0` is a Unix epoch in milliseconds: the line read
    // `Responding · 496940h16m`.
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(testing::delta("t1", "half an answer"));
    let mut a = app();
    a.apply(ServerFrame::Resync {
        reason: "switch".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    a.apply(ServerFrame::Event(env_at(
        99,
        1_788_984_000_000,
        testing::delta("t1", " more"),
    )));
    let line = a.turn_status(120);
    assert!(!line.is_empty(), "the turn is running");
    assert!(line.contains("started before this head attached"), "{line}");
    // No `NNNh` anywhere: that shape is what an epoch renders as.
    let chars: Vec<char> = line.chars().collect();
    assert!(
        !chars
            .windows(2)
            .any(|w| w[0].is_ascii_digit() && w[1] == 'h'),
        "an epoch rendered as a duration: {line}"
    );
}

#[test]
fn a_head_that_attaches_after_a_restart_shows_the_context_from_the_session_row() {
    // The daemon restarted: the view was rebuilt from the transcript and
    // `TurnFinished` is ephemeral, so the snapshot has no turn state to read
    // the context from. The number the last turn left behind is on the
    // session's own row, and the brief carries it — the header must show it
    // on the first frame, not wait for the next turn.
    let hub = Hub::new("s");
    let mut a = app();
    let mut b = brief("s", "the cache question", false);
    b.context_tokens = Some(44_700);
    b.context_cached = Some(40_000);
    a.apply(hello("s", vec![b], hub.snapshot()));
    assert_eq!(
        a.usage,
        Some(Usage {
            prompt_tokens: 44_700,
            cached_tokens: 40_000,
            predicted_tokens: 0,
            cost_micros_usd: None
        })
    );
    let header = a.header_line(200);
    assert!(header.contains("44.7k ctx"), "{header}");
    assert!(header.contains("89% cached"), "{header}");
    // A snapshot that DOES carry the turn state wins over the row: the live
    // number is the one the head just saw, and the row is the fallback for
    // the daemon that lost it.
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(SessionEvent::TurnFinished {
        turn_id: "t1".into(),
        finish_reason: letibot_sessionlog::event::FinishReason::Eos,
        usage: Usage {
            prompt_tokens: 51_000,
            cached_tokens: 44_700,
            predicted_tokens: 300,
            cost_micros_usd: None,
        },
        timings: Default::default(),
    });
    let mut a = app();
    let mut b = brief("s", "the cache question", false);
    b.context_tokens = Some(44_700);
    b.context_cached = Some(40_000);
    a.apply(hello("s", vec![b], hub.snapshot()));
    assert_eq!(
        a.usage,
        Some(Usage {
            prompt_tokens: 51_000,
            cached_tokens: 44_700,
            predicted_tokens: 300,
            cost_micros_usd: None
        }),
        "the snapshot's turn state is newer than the row"
    );
    // A backfilled row carries the size but not the fraction: the migration
    // sums the prefix and the items for a store being upgraded, and the cache
    // of a prompt that was never sent is not a measurement. The header shows
    // the size and says nothing about the cache, rather than a `0%` nobody took.
    let hub = Hub::new("s");
    let mut a = app();
    let mut b = brief("s", "the cache question", false);
    b.context_tokens = Some(44_700);
    b.context_cached = None;
    a.apply(hello("s", vec![b], hub.snapshot()));
    assert_eq!(
        a.usage,
        Some(Usage {
            prompt_tokens: 44_700,
            cached_tokens: 0,
            predicted_tokens: 0,
            cost_micros_usd: None
        })
    );
    let header = a.header_line(200);
    assert!(header.contains("44.7k ctx"), "{header}");
    assert!(
        !header.contains("% cached"),
        "no fraction was measured, so none is shown: {header}"
    );
    // And a row with no number seeds nothing: a session that has run no
    // turn shows no context rather than a zero.
    let hub = Hub::new("s");
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "", false)], hub.snapshot()));
    assert!(a.usage.is_none());
}

#[test]
fn a_head_that_joins_mid_turn_draws_the_turn_it_missed() {
    // The live frames carry the increments; the snapshot carries what a late head never saw.
    // **What it owes that head now is the ROW**, not a count: the counter was removed from the
    // display on 2026-09-27, and the fact a joiner still needs is that a turn is running and
    // how long it has been (which the snapshot cannot measure, so it says so rather than
    // inventing one).
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(testing::tokens_generated("t1", 42));
    let mut a = app();
    a.apply(hello("s", vec![], hub.snapshot()));
    let line = a.turn_status(120);
    assert!(line.contains("Responding"), "{line}");
    assert!(
        line.contains("started before this head attached"),
        "a snapshot measured no duration, and says so: {line}"
    );
    assert!(
        !line.contains("42"),
        "the counter is not on this row: {line}"
    );
}

#[test]
fn the_picker_lists_the_sessions_and_a_number_switches_to_one() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![
            brief("s", "the cache question", true),
            brief("s2", "scratch", false),
        ],
        Hub::new("s").snapshot(),
    ));
    assert_eq!(a.key(Key::CtrlS), Some(Action::ListSessions));
    let screen = a.screen(110, 24).join("\n");
    assert!(screen.contains("the cache question"), "{screen}");
    assert!(screen.contains("scratch"), "{screen}");
    assert!(
        screen.contains("generating"),
        "the busy one says so:\n{screen}"
    );
    typed(&mut a, "2");
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s2".into())));
}

#[test]
fn the_picker_moves_with_arrows_and_enter_takes_the_marked_row() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![
            brief("s", "one", true),
            brief("s2", "two", false),
            brief("s3", "three", false),
        ],
        Hub::new("s").snapshot(),
    ));
    assert_eq!(a.key(Key::CtrlS), Some(Action::ListSessions));
    // The cursor starts on the session this head is in, so the mark and the
    // bold name are on the same row until an arrow moves it.
    assert_eq!(a.picker_sel, 0);
    let row = a
        .screen(110, 24)
        .into_iter()
        .find(|l| l.contains("one") && l.contains('▸'))
        .unwrap();
    assert!(row.contains('▸'), "the mark is what enter takes: {row}");
    a.key(Key::Down);
    a.key(Key::Down);
    assert_eq!(a.picker_sel, 2);
    let row = a
        .screen(110, 24)
        .into_iter()
        .find(|l| l.contains("three") && l.contains('▸'))
        .unwrap();
    assert!(row.contains('▸'), "the mark moved with the arrows: {row}");
    // Up wraps past the top; Up again wraps in from the bottom.
    a.key(Key::Up);
    a.key(Key::Up);
    assert_eq!(a.picker_sel, 0);
    a.key(Key::Up);
    assert_eq!(a.picker_sel, 2);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s3".into())));
}

#[test]
fn a_click_picks_the_row_under_the_pointer_and_enter_still_switches() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![
            brief("s", "one", true),
            brief("s2", "two", false),
            brief("s3", "three", false),
        ],
        Hub::new("s").snapshot(),
    ));
    assert_eq!(a.key(Key::CtrlS), Some(Action::ListSessions));
    a.screen(110, 24);
    // The session header takes row 0, the picker's title and blank take
    // two more, so the first session row is y=3 — 0-based, the decoder
    // having taken the wire's one off.
    a.key(Key::Click { x: 10, y: 5 });
    assert_eq!(a.picker_sel, 2);
    let row = a
        .screen(110, 24)
        .into_iter()
        .find(|l| l.contains("three") && l.contains('▸'))
        .unwrap();
    assert!(
        row.contains('▸'),
        "the mark moved to the clicked row: {row}"
    );
    a.key(Key::Click { x: 0, y: 3 });
    assert_eq!(a.picker_sel, 0);
    // A click into the blank space under the list moves nothing: the row
    // was truncated away, so selecting it would switch to a session
    // nobody saw.
    a.key(Key::Click { x: 4, y: 12 });
    assert_eq!(a.picker_sel, 0);
    // Select and confirm stay two acts: the click only moves the mark.
    a.key(Key::Click { x: 4, y: 5 });
    assert_eq!(a.picker_sel, 2);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s3".into())));
}

/// **A provider switch reaches a head that was already attached.**
///
/// This is the operator's report, reproduced: attach while the daemon is local,
/// switch to a provider, and the top row went on saying `qwen-3.8-27b`.
///
/// The cause is in the protocol rather than in either end's logic.
/// `ServerFrame::Settings` has exactly **one** send site and it answers a request —
/// `set_settings` fills the registry's mailbox and nothing pushes. So the attached
/// head's `model` row is what it read at attach, for the life of the connection,
/// however many switches happen. `TurnStarted` *does* arrive unprompted and names
/// what is answering, so the header now takes whichever it was told more recently,
/// by `seq`.
#[test]
fn a_provider_switch_reaches_an_already_attached_head() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // Attached while local: the row the head will hold.
    a.apply(model_settings("local (qwen-3.8-27b)", &["local"]));
    assert!(
        a.header_line(200).contains("qwen-3.8-27b"),
        "the attach state: {}",
        a.header_line(200)
    );

    // The operator switches. The daemon republishes to its registry, and the *turns*
    // arrive naming the new model — but no `Settings` frame is sent to this head.
    a.apply(ServerFrame::Event(env(
        9,
        SessionEvent::TurnStarted {
            turn_id: "t1".into(),
            model: "deepseek/deepseek-flash".into(),
            ledger_head: "0000".into(),
            // The turn's own start, stamped once per prompt; `None` is a row from a
            // snapshot, which has no timestamps.
            began_ms: None,
        },
    )));
    let h = a.header_line(200);
    assert!(
        h.contains("deepseek/deepseek-flash"),
        "the switch did not reach the header: {h}"
    );
    assert!(
        !h.contains("qwen-3.8-27b"),
        "the stale row outranked the live turn: {h}"
    );

    // And a fresh `/config` answer still wins, because it is later still.
    a.apply(model_settings("grok/grok-4", &["local"]));
    let h = a.header_line(200);
    assert!(h.contains("grok/grok-4"), "{h}");
    assert!(!h.contains("deepseek"), "{h}");
}

/// **The top row names what answers NOW.** `wiring.model` is the daemon's
/// word from `Hello`, sent once at attach, so a session switched to a provider
/// mid-conversation kept its old label: the operator switched leticl to
/// deepseek and *"top row still says qwen"*.
#[test]
fn the_header_follows_a_mid_session_model_switch() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // What `Hello` said, which is all the header used to read.
    assert!(
        a.header_line(200).contains("qwen-3.8-flash-next"),
        "{}",
        a.header_line(200)
    );
    // The daemon republishes the row on a switch; the header takes it.
    a.apply(model_settings(
        "deepseek/deepseek-flash",
        &["local", "deepseek/deepseek-flash"],
    ));
    let h = a.header_line(200);
    assert!(h.contains("deepseek/deepseek-flash"), "{h}");
    assert!(!h.contains("qwen-3.8-flash-next"), "and not both: {h}");

    // Back to local, where the row carries the alias for the config pane and
    // the header wants only the name.
    a.apply(model_settings("local (glm-5.3-flash)", &["local"]));
    let h = a.header_line(200);
    assert!(h.contains("glm-5.3-flash"), "{h}");
    assert!(
        !h.contains("local ("),
        "the header takes the name alone: {h}"
    );
}

/// **Taking a keyless row from the picker ASKS for the key** — the operator's row, second
/// sentence: *"if i choose a model without key picker should ask for the key."*
///
/// The ask is a card and a masked buffer of their own — never the composer, so the key
/// cannot land in a prompt, and never the sudo path, whose `req_id` belongs to the daemon.
/// Enter does both things in one verb, `models CHOICE --key K`, which is the round trip the
/// typed spelling already is: store (mode 600) and switch. Esc cancels with nothing stored
/// and the row not taken; an EMPTY enter is a cancelled ask rather than a stored empty key.
///
/// Two honesty rules, both the same rules the greening keeps:
/// * **No ask when the daemon did not name the keyless row** (`models.keys` absent is *no
///   greening*, never *no keys*), so an older daemon never has its switches blocked behind
/// a prompt for a key the box may hold.
/// * **`local` never asks** — it needs no credential, and `choice_ready` says so.
#[test]
fn taking_a_keyless_row_asks_for_the_key_and_enter_stores_and_switches() {
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
                &["local", "deepseek/deepseek-flash"],
            ),
            // The daemon names NO provider as keyed, so `deepseek` is keyless by its word.
            row(MODEL_KEYS_KEY, "", &[]),
        ],
    });
    assert_eq!(a.command("models"), Some(Action::Settings));
    // Take the keyless row by its number: Enter on the card, the row the daemon named keyless.
    assert_eq!(a.key(Key::Down), None);
    assert_eq!(a.key(Key::Enter), None);
    assert!(
        a.key_ask.is_some(),
        "a row the daemon named keyless opens the ask rather than switching"
    );
    let screen = a.screen(110, 30).join("\n");
    assert!(
        screen.contains("deepseek needs a key this box does not hold"),
        "the card names the provider the ask is for:\n{screen}"
    );
    // **Typed characters are dots and never on the screen as text** — the key cannot be
    // read off the glass by anything else that sees it.
    for c in "sk-live-12345".chars() {
        a.key(Key::Char(c));
    }
    let masked = a.screen(110, 30).join("\n");
    assert!(
        masked.contains("•••"),
        "the composer masks the key: no dots on screen"
    );
    assert!(
        !masked.contains("sk-live-12345"),
        "the key itself is never rendered:\n{masked}"
    );
    // **Enter does both things in one verb**, and nothing is sent before it.
    let act = a.key(Key::Enter).expect("enter acts");
    assert!(a.key_ask.is_none(), "the ask is closed");
    match act {
        Action::Slash { line } => assert_eq!(
            line, "models deepseek/deepseek-flash --key sk-live-12345",
            "one verb stores the key and takes the row"
        ),
        other => panic!("enter on the ask sends a slash, not {other:?}"),
    }

    // **Esc cancels with nothing stored and the row not taken.**
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
    b.apply(ServerFrame::Settings {
        rows: vec![
            row(
                "model",
                "local (glm-5.3-flash)",
                &["local", "deepseek/deepseek-flash"],
            ),
            row(MODEL_KEYS_KEY, "", &[]),
        ],
    });
    b.command("models");
    b.key(Key::Down);
    b.key(Key::Enter);
    assert!(b.key_ask.is_some());
    b.key(Key::Char('k'));
    assert_eq!(b.key(Key::Esc), None);
    assert!(b.key_ask.is_none(), "esc closes the ask");
    // **Nothing was SENT for the key** — the queue may hold the attach's own asks
    // (`Settings`, `ListJobs`, planted by the `Hello` arm), but no `Slash` left the head.
    assert!(
        b.take_actions()
            .into_iter()
            .all(|act| !matches!(act, Action::Slash { .. })),
        "esc sent nothing for the key"
    );

    // **An older daemon — no `models.keys` row at all — never asks**, and the switch goes
    // through as it always did. Absent is *no greening*, never *no keys*.
    let mut c = App::new(RenderConfig {
        width: 110,
        color: true,
        ..RenderConfig::default()
    });
    c.session_id = "s1".into();
    c.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    c.apply(ServerFrame::Settings {
        rows: vec![row(
            "model",
            "local (glm-5.3-flash)",
            &["local", "deepseek/deepseek-flash"],
        )],
    });
    c.command("models");
    c.key(Key::Down);
    match c.key(Key::Enter).expect("the switch still goes out") {
        Action::Slash { line } => assert_eq!(line, "models deepseek/deepseek-flash"),
        other => panic!("no row, no ask: {other:?}"),
    }
    assert!(c.key_ask.is_none(), "an absent keys row blocks nothing");

    // **And `local` never asks** — it needs no credential, which is `choice_ready`'s own rule.
    let mut d = App::new(RenderConfig {
        width: 110,
        color: true,
        ..RenderConfig::default()
    });
    d.session_id = "s1".into();
    d.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    d.apply(ServerFrame::Settings {
        rows: vec![
            row("model", "local (glm-5.3-flash)", &["local"]),
            row(MODEL_KEYS_KEY, "", &[]),
        ],
    });
    d.command("models");
    match d.key(Key::Enter).expect("local switches straight through") {
        Action::Slash { line } => assert_eq!(line, "models local"),
        other => panic!("local needs no key: {other:?}"),
    }
    assert!(d.key_ask.is_none());
}

/// **`/models` is a menu now.** It printed a wall of provider rows in which
/// the switch — the thing anybody types it for — was the least visible part.
/// The operator: *"for starters i want it to be usual menu, like /mode"*.
#[test]
fn slash_models_opens_a_picker_seeded_on_what_answers_now() {
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(model_settings(
        "glm/glm-5.3-flash",
        &[
            "local",
            "deepseek/deepseek-flash",
            "glm/glm-5.3-flash",
            "grok/grok-4.3",
        ],
    ));
    // Bare `/models` opens the list AND asks the daemon, so a session whose
    // model moved in another head does not draw a stale `← now`.
    // It refreshes the SETTINGS, not the `models` verb: that verb answers
    // with the whole provider listing, which would land on the log under the
    // card and is the wall of text this picker replaces.
    assert_eq!(a.command("models"), Some(Action::Settings));
    assert!(a.pick == Some(Pick::Model));
    assert_eq!(a.mode_sel, 2, "seeded on the row that answers now");

    // Tall enough that the card's trailing hints survive the fit loop, which
    // drops them last-first when the screen is short.
    let screen = a.screen(110, 40).join("\n");
    assert!(
        screen.contains("what answers this conversation"),
        "{screen}"
    );
    assert!(screen.contains("grok/grok-4.3"), "{screen}");
    assert!(screen.contains("← now"), "{screen}");
    // The card says what it does and names the verb for the other thing,
    // because one verb doing both is what sent the operator looking.
    assert!(screen.contains("this conversation only"), "{screen}");
    assert!(screen.contains("/default-model"), "{screen}");

    // Arrow and Enter send the switch as the daemon verb.
    a.key(Key::Down);
    match a.key(Key::Enter) {
        Some(Action::Slash { line }) => assert_eq!(line, "models grok/grok-4.3"),
        other => panic!("{other:?}"),
    }
    assert!(a.pick.is_none(), "taking a model closes the list");
}

/// The mode picker is unchanged by sharing its machinery — Esc still closes,
/// and the two never open at once.
#[test]
fn the_two_pickers_do_not_open_together() {
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    a.apply(model_settings("local", &["local", "glm/glm-5.3-flash"]));

    assert_eq!(a.command("mode"), Some(Action::Settings));
    assert!(a.pick == Some(Pick::Mode));
    a.command("models");
    assert!(a.pick == Some(Pick::Model), "the second closes the first");
    a.key(Key::Esc);
    assert!(a.pick.is_none(), "esc closes it");
}

/// **A picker that opened before its own list arrived seeds when the list does** — the second
/// cause of the operator's report, and the one leticl had already fixed on its side.
///
/// `/mode` and `/models` draw the card AND send the ask in the same breath, so a head whose
/// rows have not landed seeds from an empty list: `position` answers nothing, the fallback is
/// `0`, and the cursor sits on the first row while `← now` marks the real current one further
/// down. leticl's `head.lisp` records the operator hitting exactly this — *"mode selectors has
/// selection on the first not on the current again"* — and its word *again* is the tell: it
/// works on the SECOND open, settings being known by then, so a test that sets the rows up
/// first never sees it. This one does not set them up first.
///
/// **And the reader's own cursor is not snapped back.** leticl reconciles the two with a
/// one-way flag, and this asserts both halves: an untouched cursor follows the answer, a moved
/// one does not.
#[test]
fn a_picker_that_opened_before_its_rows_landed_seeds_when_they_do() {
    const CHOICES: &str =
        r#"["read-only","always-ask","writes allowed","automode","automode-edits","allow-all"]"#;
    let choices: Vec<String> = serde_json::from_str(CHOICES).unwrap();
    let answer = move || ServerFrame::Settings {
        rows: vec![letibot_sessionlog::protocol::SettingRow {
            key: "mode".into(),
            value: "automode-edits".into(),
            source: "project store (modes.tsv)".into(),
            editable: "/mode NAME".into(),
            choices: choices.clone(),
            tools: Vec::new(),
        }],
    };
    // The head has heard nothing yet — the ask is out, the answer is not.
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.command("mode");
    assert_eq!(a.mode_sel, 0, "the premise: nothing to seed on yet");
    a.apply(answer());
    assert_eq!(
        a.mode_sel, 4,
        "the rows landed and the cursor did not go to the mode the session is under"
    );

    // **The cursor the reader moved is left alone.** Two Downs is a choice they made, and an
    // answer landing a moment later must not undo it.
    //
    // The rows have to be present for there to be anything to move THROUGH — with no choices
    // the card has no rows and the arrows are refused, which is the first half's premise and
    // not this one's.
    let mut b = app();
    b.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    b.apply(answer());
    b.command("mode");
    assert_eq!(b.mode_sel, 4, "the premise: seeded on the current mode");
    b.key(Key::Down);
    assert_eq!(b.mode_sel, 5, "the premise: the reader has moved it");
    // A fresh answer — the one `/mode` itself asked for — arriving after that.
    b.apply(answer());
    assert_eq!(
        b.mode_sel, 5,
        "the answer snapped the reader's cursor back to where it would have been"
    );

    // **And the rows can be another SESSION's, not merely absent.** `self.settings` is only
    // ever assigned by a settings frame — a switch does not clear it — so a picker opened
    // between a switch and that session's answer seeds from the session the reader just left.
    // The symptom is the same one and so is the fix: the re-seed runs on the frame, so the
    // card corrects itself the moment the right rows land.
    let mut c = app();
    c.apply(hello(
        "s1",
        vec![brief("s1", "one", false)],
        Hub::new("s1").snapshot(),
    ));
    c.apply(answer());
    assert_eq!(c.mode_sel, 0, "the premise: no picker is open yet");
    // The reader moves to another session, which sits at `read-only`.
    c.apply(hello(
        "s2",
        vec![brief("s2", "two", false)],
        Hub::new("s2").snapshot(),
    ));
    c.command("mode");
    assert_eq!(
        c.mode_sel, 4,
        "the premise: the card opened on the session that was just LEFT, because its rows \
             are the only ones the head has"
    );
    // That session's rows land, and the card corrects itself.
    c.apply(mode_settings(
        "read-only",
        &[
            "read-only",
            "always-ask",
            "writes allowed",
            "automode",
            "automode-edits",
            "allow-all",
        ],
    ));
    assert_eq!(
        c.mode_sel, 0,
        "the card kept the previous session's mode after the new rows arrived"
    );
    let screen = c.screen(110, 30).join("\n");
    let marked = screen
        .lines()
        .find(|l| l.contains('\u{2190}'))
        .map(str::trim)
        .expect("a row is marked as current");
    assert!(
        marked.contains("read-only"),
        "the card marks the wrong session's mode: {marked:?}"
    );
}

#[test]
fn an_ambiguous_pick_is_refused_with_the_count_rather_than_resolved() {
    // Switching to the wrong session is not a keystroke you can take back: the
    // prompt you type next lands there.
    let mut a = app();
    a.apply(hello(
        "s",
        vec![
            brief("s", "cache one", false),
            brief("s2", "cache two", false),
            brief("s3", "other", false),
        ],
        Hub::new("s").snapshot(),
    ));
    a.key(Key::CtrlS);
    typed(&mut a, "cache");
    assert_eq!(
        a.key(Key::Enter),
        None,
        "an ambiguous prefix must not switch"
    );
    let screen = a.screen(110, 24).join("\n");
    assert!(screen.contains("2 sessions match"), "{screen}");
}

#[test]
fn switching_replaces_the_previous_sessions_facts_and_does_not_carry_them_over() {
    // Seen under tmux: a brand-new empty session claiming `4470 ctx · 34%
    // cached`, because the head assigned `session_id` before `load` compared
    // the two and so `load` never saw that it had moved.
    let mut a = app();
    let one = Hub::new("s1");
    one.publish(testing::turn_started("t1"));
    one.publish(SessionEvent::TurnFinished {
        turn_id: "t1".into(),
        finish_reason: letibot_sessionlog::event::FinishReason::Eos,
        usage: Usage {
            prompt_tokens: 4_470,
            cached_tokens: 1_500,
            predicted_tokens: 10,
            cost_micros_usd: None,
        },
        timings: Default::default(),
    });
    a.apply(hello("s1", vec![brief("s1", "one", false)], one.snapshot()));
    assert!(a.header_line(200).contains("4470 ctx"));

    a.apply(hello(
        "s2",
        vec![brief("s1", "one", false), brief("s2", "two", false)],
        Hub::new("s2").snapshot(),
    ));
    let h = a.header_line(200);
    assert!(
        !h.contains("4470"),
        "the old session's token count came along: {h}"
    );
    assert!(h.contains("2/2"), "{h}");
    assert!(a.turn.is_none(), "the old session's turn came along");
}

/// **A head asked for a stored session keeps the cat until it is IN that session.**
///
/// `letibot --continue` and the picker's resume are two steps: the daemon seats the head
/// somewhere first — the fresh session a new head gets — and only then answers the resume.
/// On a long session that second step is seconds, and the first `Hello` used to end the
/// wait: the screen said "this session has said nothing yet" about the placeholder, and the
/// operator, who asked for a conversation with two thousand rows in it, started typing into
/// the empty one. The wait is over when the head is where it was asked to go.
#[test]
fn a_resume_keeps_the_cat_until_the_asked_for_session_arrives() {
    fn empty(id: &str) -> Snapshot {
        Snapshot {
            session_id: id.into(),
            seq: 0,
            dropped: 0,
            items_dropped: 0,
            items: Vec::new(),
            turn: None,
            open_decisions: Vec::new(),
            settled_decisions: Vec::new(),
            warnings: Vec::new(),
            heads: Vec::new(),
            subagents: Vec::new(),
        }
    }
    let waiting = |a: &mut App, when: &str| {
        let flat = a.screen(80, 24).join("\n");
        assert!(
            !flat.contains("has said nothing yet"),
            "{when}: the placeholder is drawn as if it were the session asked for: {flat:?}"
        );
        assert!(
            a.attaching,
            "{when}: the wait ended before the session arrived"
        );
    };

    // `--continue`: asked for before the attach, seated in a fresh session first.
    let mut a = app();
    a.request_session("s-long");
    a.begin_attach_at(0);
    a.apply(hello("s-fresh", Vec::new(), empty("s-fresh")));
    waiting(&mut a, "--continue, seated in the placeholder");
    a.apply(hello("s-long", Vec::new(), empty("s-long")));
    assert!(
        !a.attaching,
        "the session asked for arrived and the cat stayed"
    );

    // The picker: a session that is on disk and not live.
    let mut b = app();
    b.apply(hello("s-fresh", Vec::new(), empty("s-fresh")));
    let mut stored = brief("s-long", "long", false);
    stored.live = false;
    b.sessions = vec![brief("s-fresh", "fresh", false), stored];
    assert_eq!(
        b.switch_to("s-long".into()),
        Some(Action::ResumeSession("s-long".into()))
    );
    waiting(&mut b, "picker resume, before the daemon answers");

    // And a resume the daemon refuses ends the wait: the refusal is said, and the head is
    // where it was.
    b.apply(ServerFrame::Rejected {
        client_request_id: String::new(),
        reason: format!(
            "{}: \"s-long\"",
            letibot_sessionlog::protocol::REJECT_NOT_IN_STORE
        ),
        expected_seq: 0,
        actual_seq: 0,
    });
    assert!(!b.attaching, "a refused resume left the cat walking");
    assert!(
        b.screen(80, 24)
            .join("\n")
            .contains("no such session in the store"),
        "the refusal was not said"
    );
}
