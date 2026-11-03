//! The link, attaching, reconnecting, stopping, the session picker.

use super::*;

/// A bare `!term` attaches: the rectangle comes up, the daemon says what is running, and the screen it kept is
/// drawn in it. The head sends the bare verb because it cannot know what is running - the program is the
/// session's, and the line may have been typed by another head or before a switch.
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
    // **Nothing has been drawn yet** - the rectangle is empty because the head has never
    // seen this program. That is the state the replay has to fill.
    assert!(
        !a.screen(80, 24).iter().any(|r| r.contains("mc's screen")),
        "the head cannot have this screen: it never drew it"
    );

    a.apply(ServerFrame::TermAttached {
        pane: 1,
        command: "mc /etc".into(),
        opened: false,
    });
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("!term mc /etc")),
        "the operator is told what is running in the pane they attached to: {:?}",
        a.notice
    );
    a.apply(ServerFrame::TermOutput {
        pane: 1,
        bytes: b"\x1b[2J\x1b[Hmc's screen\r\n".to_vec(),
    });
    assert!(
        a.screen(80, 24).iter().any(|r| r.contains("mc's screen")),
        "the daemon's replay is drawn in the rectangle: {:?}",
        a.screen(80, 24)
    );

    // And the line the daemon named is what an ending names, not the bare verb the head typed: the row a
    // person reads has to say which pane ended.
    a.apply(ServerFrame::TermEnded {
        pane: Some(1),
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

/// A session with no pane says so, and the sentence is a row: `!term` with nothing running is not silence and
/// not a rectangle left standing empty, since the daemon answers with the same `TermEnded` every pane that
/// could not start uses, and the head files it like any other ending.
#[test]
fn attaching_to_a_session_with_no_pane_says_so() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    let _ = a.screen(80, 24);
    typed(&mut a, "!term");
    assert!(a.key(Key::Enter).is_some());
    assert!(a.pane_open());
    // A refusal is written to the head that asked, before any pane was numbered.
    a.apply(ServerFrame::TermEnded {
        pane: None,
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

/// A program that exits while the person is away still leaves its row: the head keeps the pane while detached
/// and keeps feeding it, so the ending arrives with the last rows the program left. The register is the
/// difference and is asserted too.
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

    // It writes while they are away - the frames still arrive, and the head still feeds the
    // screen it is holding. Then it exits.
    a.apply(ServerFrame::TermOutput {
        pane: 1,
        bytes: b"mc: not found\r\n".to_vec(),
    });
    a.apply(ServerFrame::TermEnded {
        pane: Some(1),
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

/// A bare `!term` after a detach attaches back to the same run: the pane was never ended, so the daemon answers
/// with what is running in the pane it kept and replays the screen. A head that had ended it would be opening a
/// new one, or being refused.
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
        pane: 1,
        command: "mc /etc".into(),
        opened: false,
    });
    a.apply(ServerFrame::TermOutput {
        pane: 1,
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

/// A sub-session is listed, reachable and switchable, folded under the session that spawned it. Two sites used
/// to drop every row with a parent, so a session a head can post to and get an answer from was thrown away on
/// the way in.
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
    a.push_surface(Surface::Picker);

    // **The project's line and the conversation, the child folded away** - the whole reason the
    // default is not noisy.
    let folded: Vec<String> = a
        .session_rows()
        .iter()
        .map(|r| match r.idx {
            Some(i) => a.sessions[i].title.clone(),
            None => format!("[{}]", r.node.title()),
        })
        .collect();
    assert_eq!(folded, vec!["[letibot (2)]", "root"], "{folded:?}");
    let closed = a.screen(100, 30).join("\n");
    assert!(
        !closed.contains("the child"),
        "a folded session's child was drawn anyway:\n{closed}"
    );

    // **The fold, on the parent's row** - the cursor starts on the session this head is in, so
    // set it on that row and open it. Expanding is a fold, not a daemon action.
    a.picker_sel = a
        .session_rows()
        .iter()
        .position(|r| r.idx.is_some_and(|i| a.sessions[i].session_id == "s-root"))
        .expect("the conversation's row");
    assert_eq!(a.key(Key::Right), None, "expanding is not a daemon action");
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

    // **`←` on the child row closes the TREE EDGE it sits on** - the parent stays open in the
    // project, and the child goes back under it.
    assert_eq!(
        a.session_rows().len(),
        3,
        "the project and its tree are open"
    );
    a.key(Key::Left);
    assert_eq!(
        a.session_rows().len(),
        2,
        "left did not fold the child back into its parent"
    );
}

/// **A child is a session the head DRIVES** - under its parent on the picker's list, taken by the
/// same pick, typed into by the same composer, and left by the same way up. Nothing on that path
/// reads the parent edge, which is the point: the filter that hid these rows must not come back.
#[test]
fn a_child_is_listed_attached_to_sent_to_and_left_like_any_other_session() {
    let mut child = brief("s-child", "the child", true);
    child.parent_session_id = Some("s-root".into());
    let root = brief("s-root", "root", true);
    let mut a = app();
    a.apply(hello(
        "s-root",
        vec![root.clone(), child.clone()],
        Hub::new("s-root").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    a.set_row_fold("s-root", true);

    // **Listed one level under its parent, and numbered like any other row** - the number is the
    // drawn list's, so a child a person can see is a child a number can name.
    let rows: Vec<(String, usize)> = a
        .session_rows()
        .iter()
        .filter_map(|r| r.idx.map(|i| (a.sessions[i].session_id.clone(), r.depth)))
        .collect();
    assert_eq!(
        rows,
        vec![("s-root".to_string(), 1), ("s-child".to_string(), 2)],
        "the child is a row one level under the session that spawned it"
    );
    assert_eq!(
        a.session_at_number(2)
            .map(|i| a.sessions[i].session_id.clone()),
        Some("s-child".to_string()),
        "the second session on the list is the child"
    );

    // **And the drawing is that list** - whichever of the two drew it, the child is a row a person
    // reads, since a row the eyes cannot find is a row the keys might as well not reach.
    let picture = a.picker_lines(100).join("\n");
    assert!(
        picture.contains("the child"),
        "the picker does not draw the child:\n{picture}"
    );

    // **The pick on it is an ordinary attach.** No refusal, no second verb.
    assert_eq!(
        a.pick("2"),
        Some(Action::Switch("s-child".into())),
        "picking a child must switch to it"
    );
    a.apply(hello(
        "s-child",
        vec![root, child],
        Hub::new("s-child").snapshot(),
    ));
    assert_eq!(a.session_id, "s-child");

    // **A line typed while standing in it is a prompt to the child**, and one Esc is the way back:
    // this is the same send and the same detach every other session gets.
    typed(&mut a, "and the sizes");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Prompt("and the sizes".into())),
        "a child takes a message like any other session"
    );
    assert_eq!(
        a.key(Key::Esc),
        Some(Action::Switch("s-root".into())),
        "leaving a child must not need a second door"
    );
}

/// The version the daemon is told to be is checked at the handshake, and a skew is said rather than fatal: the
/// `Hello` is the cheap moment, since nothing has been read yet and the direction is known, where the first
/// frame that failed to parse was one frame late and phrased as a decoder's complaint.
#[test]
fn a_protocol_skew_is_said_at_the_handshake_and_the_head_stays_attached() {
    let hub = Hub::new("s");

    // The normal case: both halves the same build and nothing is said, since a field always zero is no more
    // readable than one absent, and a line saying versions match on every attach teaches that it can be skipped.
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
    // Taller than it was: `/status` is a scrolling pane and this row is near its end, so a frame that used to
    // reach the bottom no longer does, and the failure is a missing row rather than a wrong one.
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

    // An OLDER daemon: a writing problem, and quiet until it is fatal - so this is the
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
    // Taller than it was: `/status` gained three rows in R17, so a fixed-height frame that used to reach the
    // bottom no longer does.
    let status = a.screen(120, 96).join("\n");
    let row = status
        .lines()
        .find(|l| l.contains("protocol"))
        .unwrap_or_else(|| panic!("{status}"));
    assert!(row.contains("OLDER"), "{row}");
    assert!(row.contains(&older.to_string()), "{row}");
}

/// A `Switch` re-reads the same `Hello`, so the sentence is filed once: a skew announced twice would read as
/// two skews, and `note` dedupes on `(code, detail, ts)`. This pins that rather than trusting it.
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
/// An older daemon seats this head read-only: nothing is sent, and the refusal says the whole informed
/// sentence. Three facts are one behaviour: the head stays attached and keeps reading, nothing leaves, and the
/// person is told why.
#[test]
fn an_older_daemon_seats_this_head_read_only_and_the_refusal_says_the_sentence() {
    let hub = Hub::new("s");
    // The operator's own numbers: a 17-day-old daemon speaking 22, this head speaking 36.
    let older = letibot_sessionlog::protocol::PROTOCOL_VERSION.saturating_sub(14);
    let mut a = app();
    a.apply(hello_at(
        "s",
        vec![brief("s", "one", false)],
        hub.snapshot(),
        older,
    ));
    assert!(a.skew_locked(), "the OLDER direction is the locked one");
    // The seating's own asks did not go out, which is the assertion the feature stands on: `TermStatus` and
    // `ListJobs` against a daemon that has never heard of them would spend the session before a key is pressed.
    assert!(
        a.take_actions().is_empty(),
        "a locked head queues nothing at the seating"
    );
    // The head still reads: a frame lands and the head takes it.
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    assert!(a.turn_busy(), "reading is what a read-only attach is for");

    // The composer refuses, says the sentence, and holds the line.
    assert_eq!(a.submit("hello?".into()), None, "nothing was sent");
    assert_eq!(a.input(), "hello?", "the line is held, not dropped");
    let said = a.notice.as_deref().unwrap_or("");
    assert!(said.contains("OLDER build"), "{said}");
    // The load-bearing clause of the informed sentence, not merely the numbers.
    assert!(
        said.contains("closing the socket"),
        "the refusal must say what sending costs: {said}"
    );
    assert!(
        said.contains("ctrl-^"),
        "the refusal must spell the way out: {said}"
    );
    assert!(said.contains("Nothing was sent"), "{said}");
    // And the resident line says the state and the chord where the person is looking.
    // The chord is asserted on the whole screen because the line wraps - at 120 columns
    // `ctrl-^` is on its second visual row, and a wrapped sentence is still one line.
    let screen = a.screen(120, 40).join("\n");
    let lock_rows: Vec<&str> = screen.lines().filter(|l| l.contains("read-only")).collect();
    assert_eq!(lock_rows.len(), 1, "one resident line: {lock_rows:?}");
    assert!(
        screen.contains("ctrl-^"),
        "the chord is spelled on the screen"
    );
}

/// **`ctrl-^` is the deliberate override: the lock lifts, the seating's asks go out, and
/// the composer sends again.** A head that only knew how to refuse would have implemented
/// half the ruling.
#[test]
fn ctrl_caret_lifts_the_read_only_and_pays_the_seatings_asks() {
    let hub = Hub::new("s");
    let older = letibot_sessionlog::protocol::PROTOCOL_VERSION.saturating_sub(14);
    let mut a = app();
    a.apply(hello_at(
        "s",
        vec![brief("s", "one", false)],
        hub.snapshot(),
        older,
    ));
    a.key(Key::CtrlCaret);
    assert!(!a.skew_locked(), "the chord is the override");
    // The asks the lock skipped are the head's own obligations, and lifting the lock is
    // when they come due - an overridden head is not a head with a stale header forever.
    let asked = a.take_actions();
    for want in [
        Action::TermStatus,
        Action::Settings,
        Action::ListJobs,
        Action::ListSessions,
    ] {
        assert!(asked.contains(&want), "missing {want:?} in {asked:?}");
    }
    // The lock line came down with the lock.
    assert!(!a.screen(120, 40).join("\n").contains("read-only"));
    // And the composer sends again - the person's decision, made deliberately.
    assert_eq!(
        a.submit("hello?".into()),
        Some(Action::Prompt("hello?".into())),
        "an overridden head sends"
    );

    // **The override belongs to the seat it was given on.** A second `Hello` from the same
    // daemon - what a `Switch` produces - must not re-lock a head the operator unlocked,
    // while the same pid speaking a *different* older protocol is a new party and re-locks.
    a.apply(hello_at(
        "s",
        vec![brief("s", "one", false)],
        hub.snapshot(),
        older,
    ));
    assert!(!a.skew_locked(), "the same seat keeps the override");
    a.apply(hello_at(
        "s",
        vec![brief("s", "one", false)],
        hub.snapshot(),
        older - 1,
    ));
    assert!(
        a.skew_locked(),
        "a replaced daemon is a new decision — the lock returns"
    );
}

/// **A NEWER daemon disables nothing**: the skew is said and counted, but the seating's
/// asks go out and the composer sends, exactly as on a matching build.
#[test]
fn a_newer_daemon_attaches_with_nothing_disabled() {
    let hub = Hub::new("s");
    let newer = letibot_sessionlog::protocol::PROTOCOL_VERSION + 3;
    let mut a = app();
    a.apply(hello_at(
        "s",
        vec![brief("s", "one", false)],
        hub.snapshot(),
        newer,
    ));
    assert!(
        !a.skew_locked(),
        "the reading direction is survivable as-is"
    );
    // The seating's asks went out as on any attach.
    let asked = a.take_actions();
    assert!(
        asked.contains(&Action::TermStatus) && asked.contains(&Action::ListSessions),
        "nothing withheld: {asked:?}"
    );
    // And the composer sends.
    assert_eq!(
        a.submit("hello?".into()),
        Some(Action::Prompt("hello?".into()))
    );
    // The skew is still said - the sentence is the fact, the lock is only the OLDER half's.
    let said = a
        .notes
        .last()
        .map(|(_, n)| note_lines_unfolded(&a.cfg, n).join(" "))
        .unwrap_or_default();
    assert!(said.contains("NEWER"), "{said}");
    // No lock line - nothing is locked.
    assert!(!a.screen(120, 40).join("\n").contains("read-only"));
}

/// A head whose daemon goes away says so, keeps its screen, and does not exit. The defect was `ack(...)?`
/// propagating out of the driver's loop, through `main`, so the process was gone over a socket that had merely
/// been closed. The state is on the head and the conversation is not cleared.
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

    // The screen it had is still the screen it has: the transcript is not cleared, the turn is not forgotten,
    // plus one line saying what is wrong, above the composer where the eye crosses.
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

    // A line typed here must not look sent: the echo is pushed into `pending_prompts` and drawn `queued`, and
    // there is no queue anywhere, so the sentence is gone when the daemon comes back.
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

    // The daemon comes back and the recovery is the protocol's own: the `Hello` seats the connection and says
    // so into the conversation with the seq it asked from, since the gap is about to be filled with events.
    a.clock(1_000 + 4_400);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    assert!(!a.detached());
    let back = a.screen(100, 24).join("\n");
    assert!(!back.contains("daemon connection is down"), "{back}");
    assert!(back.contains("daemon is back after 4.4s"), "{back}");
    // One line on the glass (R89); the sentence whole a click - or alt-n - away.
    let back = newest_notice(&mut a);
    assert!(back.contains("Resuming from seq"), "{back}");
    // The ordinary reconnect succeeds on its first try, so there is no attempt count
    // to show - and the sentence must not show an empty one.
    assert!(
        back.contains("the daemon is back after 4.4s."),
        "no empty brackets: {back}"
    );
}

/// `reconnecting…` is a state with a clock and not a word: past the impatient threshold the person is told how
/// long, how many attempts and what they can do, because one word for a minute is indistinguishable from a head
/// about to come back.
#[test]
fn a_link_that_stays_down_stops_saying_reconnecting_and_starts_saying_how_long() {
    let mut a = app();
    a.clock(1_000);
    a.link_down("the daemon closed the connection");

    // Twelve seconds in: still "reconnecting", and no advice yet - under the
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

    // And when it *does* get back after those two failures, the count is shown  -
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

/// A `Bye` does not answer a stop this head asked for, and it does not silence the farewell: the `Bye` on this
/// path is published by the connection thread the moment the `Stop` is taken, while the worker is still running
/// the person's command.
#[test]
fn a_bye_does_not_report_a_stop_that_has_not_happened() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // The head asks, and the daemon does what it does: the ack, then the goodbye from
    // the connection thread - with the worker still inside the command.
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

/// A `Bye` is final and a dropped socket is not: the daemon saying goodbye is the end of the conversation, and
/// turning that into a retry is a refusal meant as final becoming a two-second loop under a head that never
/// attached. It is still final for a head that did not ask for the stop.
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
    // Attached: the action, naming nobody - the daemon compacts the session
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

/// A head that switches away and back re-asks, and the frames it sends are the assertion: a state restored by a
/// frame nobody asked for depends on when an unrelated round-trip lands, which is the counter coming back on
/// its own.
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
    // and that is the whole of the switch path - so the ask rides the same arm.
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

/// The regression: the counter does not go away on a switch and come back later. A child is watched alive, the
/// head switches in and back, and the daemon's list says `running: false` because the child is parked on a job.
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
        slug: String::new(),
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

    // Away, and back. **The list says `running: false` at the instant of return** - the
    // child is between turns - and it says it in both directions, so the only thing that
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

    // The control: the same round trip with a snapshot carrying no children, where a head that never watched
    // the spawn has only the list, and its `false` is read as finished.
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

/// The jobs pane is not the subagent pane's defect: `App::load` clears `self.jobs` on a switch, but the `Hello`
/// arm queues `ListJobs` on every attachment, so the table is refilled and a job already running is not lost.
#[test]
fn a_switch_re_asks_for_the_jobs_and_the_daemons_answer_refills_the_pane() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.take_actions();
    assert_eq!(a.key(Key::CtrlG), None);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
    // The switch's own `Hello` asks again - this is the half that was missing for
    // subagents, which had no frame to ask with.
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    assert!(
        a.take_actions().contains(&Action::ListJobs),
        "the switched head did not re-ask for its jobs"
    );
    a.jobs = Vec::new();
    a.apply(ServerFrame::Jobs {
        monitors: Vec::new(),
        session_id: "s-sub-1".into(),
        jobs: vec![daemon_job("j1", "cargo test", true)],
    });
    assert_eq!(a.jobs.len(), 1, "the daemon's table did not land");
}

/// A peek answered with rows draws a session, not a dump: `Peeked::snapshot` is the child's own transcript, so
/// the pane goes through `item_lines` and a reader sees a tool card rather than the plain strings the event-ring
/// fallback is stuck with. The two must not look alike.
#[test]
fn a_peek_drawn_from_the_session_rows_shows_a_tool_card_and_does_not_claim_degrading() {
    let mut a = app();
    let mut snap = Hub::new("s-sub-1").snapshot();
    snap.items = vec![
        letibot_sessionlog::view::SnapshotItem {
            citations: Vec::new(),
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
            citations: Vec::new(),
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
    // The head asks before the daemon answers (a reply nobody asked for is dropped).
    a.ask_peek("s-sub-1");
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
    // The card and the target the snapshot's own turn seeded, neither of which is in the plain event-ring
    // fallback: the verb is the card's, the subject is quoted, and both are one renderer's doing rather than two.
    assert!(text.contains(r#"▸ Ran "ls -la /etc" · ok"#), "{text}");
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains(r#"Ran "ls -la /etc""#), "{screen}");
    assert!(
        !screen.contains("event ring"),
        "a card was drawn; the pane must not also claim it could not read the rows:\n{screen}"
    );
}

/// **Tick until the git reading in flight is collected** - the reading runs on its own thread
/// ([`App::refresh_git`]), so a test that wants its answer drives the loop's half until the
/// job is gone. Bounded, so a reader that never answers fails the test instead of hanging it.
fn collect_git(a: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while a.git_job.is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "the git reading was never collected"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        a.refresh_git();
    }
}

/// **A reading of a directory that is not a repository is ABSENT, and the head remembers
/// which directory it read** - so a session switch re-reads at once and the interval applies
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
    collect_git(&mut a);
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
    // A second call inside the interval does not re-read - which is the point of the stamp  -
    // and a workspace that CHANGED is read at once rather than waiting it out.
    a.clock(1_500);
    a.refresh_git();
    assert!(a.git_job.is_none(), "re-read inside the interval");
    assert_eq!(a.git_read.1, 1_000, "re-read inside the interval");
    // A repository of the test's own and not a path on this box: a directory holding several repositories reads
    // as "N repos" and starts no reading at all, and the operator's own folder is exactly that.
    let repo = dir.with_extension("repo");
    std::fs::create_dir_all(&repo).expect("scratch repo");
    let init = letibot_tools::testing::git(&repo)
        .args(["init", "-q"])
        .status()
        .expect("git init");
    assert!(init.success(), "git init");
    letibot_tools::testing::repo_config(&repo);
    a.clock(1_600);
    a.wiring.workspace = repo.display().to_string();
    a.refresh_git();
    assert!(
        a.git_job.is_some(),
        "a workspace that changed is read at once"
    );
    collect_git(&mut a);
    assert_eq!(
        a.git_read.1, 1_600,
        "a workspace that changed is read at once"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&repo);
}

/// The loop never waits for `git status`: it ran inline on the loop and one repository took 1.9 s, so every key
/// read during it waited and the head showed under 20 ms of CPU for a body that took seconds to move.
#[test]
fn a_slow_git_reading_does_not_hold_the_loop_and_is_never_doubled() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    let started = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(gate));
    let mut a = app();
    {
        let (started, finished, gate) = (started.clone(), finished.clone(), gate.clone());
        a.git_reader = Arc::new(move |_ws: &str| {
            started.fetch_add(1, Ordering::SeqCst);
            // Held until the test releases it - the 1.9 s `git status`, made as long as the
            // test needs. The timeout is a backstop so a broken loop fails rather than hangs.
            let _ = gate
                .lock()
                .expect("gate")
                .recv_timeout(std::time::Duration::from_secs(30));
            finished.fetch_add(1, Ordering::SeqCst);
            Some(crate::gitfield::GitState {
                branch: "main".into(),
                detached: false,
                behind: None,
                ahead: None,
                stash: None,
                action: None,
                conflict: None,
                staged: None,
                unstaged: None,
                untracked: None,
            })
        });
    }
    a.wiring.workspace = "/somewhere/slow".into();
    a.clock(1_000);
    a.refresh_git();
    assert_eq!(
        finished.load(Ordering::SeqCst),
        0,
        "refresh_git returned only after the reading finished: the loop waited for git"
    );
    assert!(a.git_job.is_some(), "a reading is in flight");
    // Ticks while the reading runs, the clock well past the interval: still one reading.
    for t in 1..=20u64 {
        a.clock(1_000 + t * crate::gitfield::GIT_REFRESH_MS);
        a.refresh_git();
    }
    // The thread may not have been scheduled yet; it is started once whatever the timing.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while started.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(started.load(Ordering::SeqCst), 1, "one reading at a time");
    assert_eq!(finished.load(Ordering::SeqCst), 0);
    assert!(
        a.git.is_none(),
        "nothing to draw before the reading answers"
    );
    let collected_at = 1_000 + 21 * crate::gitfield::GIT_REFRESH_MS;
    a.clock(collected_at);
    release.send(()).expect("release the reader");
    collect_git(&mut a);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    assert!(a.git.is_some(), "the reading is drawn once it is collected");
    assert_eq!(
        a.git_read.1, collected_at,
        "the interval counts from when the reading was collected, not from when it began"
    );
    // Inside the interval after the collection: no second reading, however slow the first was.
    a.clock(collected_at + crate::gitfield::GIT_REFRESH_MS - 1);
    a.refresh_git();
    assert!(
        a.git_job.is_none(),
        "a reading came due before its interval"
    );
    a.clock(collected_at + crate::gitfield::GIT_REFRESH_MS);
    a.refresh_git();
    assert!(a.git_job.is_some(), "and after it, the next reading starts");
    release.send(()).expect("release the second reading");
    collect_git(&mut a);
    assert_eq!(started.load(Ordering::SeqCst), 2);
}

/// The head asks for the settings on attach: the daemon answers `Settings` and never pushes the rows, so a head
/// that had opened neither `/mode` nor `/config` had none, and the header fell back to the model `Hello` named.

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
    // Before the answer, the header falls back to Hello's model - which is
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

/// The two events a head answers without drawing are counted and not `Control`: both are `SessionEvent`s, so
/// both are session content, read and deliberately not drawn as a row. `Control` is what a head sends about the
/// connection rather than what the session did.
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

/// Rendering the history does not grow with the session either: the frame is the window, but every row whose
/// body arrived threw the rendered transcript away, so a session of `n` rows cost `n²/2` renders.
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

/// The frame drawn before the daemon has answered claims nothing: the `Hello` carries the whole snapshot and can
/// be a fifth of a second, and *this session has said nothing yet* is false when the truth is that nobody has
/// told this head yet.
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
    // The chrome is there, which is the point of drawing early: the composer's hint bar says the program is
    // alive. Asserted on its tail, since the keys that used to open it are deliberately gone.
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
            activity: letibot_sessionlog::activity::Activity::Quiet,
        },
    ));
    let frame = a.screen(80, 24);
    let flat = frame.join("\n");
    assert!(!a.attaching, "the Hello did not clear it");
    assert!(
        flat.contains("has said nothing yet"),
        "an empty session after the Hello must say so: {flat:?}"
    );
}

/// A snapshot full of body-less rows draws nothing and renders as bodies land. The old shape drew a bar by
/// counting rows with no body, which being a count of a symptom fired on every ordinary turn; the bar is the
/// daemon's now, and nothing has named an operation here.
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

/// A fill's bar goes when the view that measured it is replaced: a `Filling` tick's only exit is a tick whose
/// `done` reached `total`, and a republish that overruns a head's queue demotes the head, so the final tick is
/// never written.
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
    assert!(armed.contains("897/2000 rows"), "{armed}");

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
        !after.contains("/2000 rows"),
        "and it must leave the screen: {after}"
    );
    // A fill that is still running re-arms itself on its next tick, which is what makes
    // clearing here safe rather than lossy. (Past the resync's flash notice: while it is up the
    // progress gives up its counts to keep the notice whole - `App::status_row`.)
    a.clock(1);
    a.clock(2 + NOTICE_MS);
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
        again.contains("960/2000 rows"),
        "a fill still going comes back on its next tick: {again}"
    );
}

#[test]
fn the_header_can_name_what_the_session_is_talking_to_before_any_turn() {
    // It read `no turn yet` for a freshly attached head, since `TurnStarted { model }` was the only fact that
    // reached a head and only when a turn starts. The daemon has known the model since it parsed its command
    // line, and now says so on `Hello`.
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
fn a_running_call_joined_from_a_snapshot_shows_no_epoch() {
    // Seen live as `◐ Editing jobs.rs · 498235h48m`: the card measured from 0.
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    hub.publish(testing::proposed_on("t1", "c1", "edit", "jobs.rs"));
    hub.publish(SessionEvent::ToolStarted {
        turn_id: "t1".into(),
        call_id: "c1".into(),
        name: "edit".into(),
        access: "write".into(),
    });
    let snap = hub.snapshot();
    let started = snap.turn.as_ref().unwrap().calls[0].started_ts;
    assert!(started > 0, "the daemon sends the call's start");
    let mut a = app();
    a.now_ms = started;
    a.apply(hello("s", vec![], snap));
    a.apply(ServerFrame::Event(env_at(
        99,
        started + 3_000,
        testing::tool_progress("c1", "half"),
    )));
    a.now_ms += 3_000;
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("jobs.rs"), "{screen}");
    let chars: Vec<char> = screen.chars().collect();
    assert!(
        !chars
            .windows(2)
            .any(|w| w[0].is_ascii_digit() && w[1] == 'h'),
        "an epoch rendered as a duration: {screen}"
    );
    assert!(
        screen.contains("3.0s"),
        "measured from the call's start: {screen}"
    );
}

#[test]
fn a_turn_joined_from_a_snapshot_shows_no_elapsed_rather_than_an_epoch() {
    // Found by switching into a session mid-turn: a snapshot has no timestamps, so `started_ms` is 0 and
    // `last_ms - 0` is a Unix epoch in milliseconds, and the line read 496940 hours.
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
    // The daemon restarted: the view was rebuilt from the transcript and `TurnFinished` is ephemeral, so the
    // snapshot has no turn state. The number the last turn left is on the session's own row and the header shows
    // it on the first frame.
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
            cost_micros_usd: None,
            ..Default::default()
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
            ..Default::default()
        },
        timings: Default::default(),
        ends_turn: true,
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
            cost_micros_usd: None,
            ..Default::default()
        }),
        "the snapshot's turn state is newer than the row"
    );
    // A backfilled row carries the size but not the fraction: the cache of a prompt that was never sent is not a
    // measurement, so the header shows the size and says nothing about the cache rather than a `0%` nobody took.
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
            cost_micros_usd: None,
            ..Default::default()
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
    // The live frames carry the increments and the snapshot carries what a late head never saw. What it owes
    // that head is the row, not a count: the fact is that a turn is running and how long, which the snapshot
    // cannot measure.
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
    // The cursor starts on the session this head is in - the row under the project's line, which
    // is the first row and the one Enter folds.
    assert_eq!(a.picker_sel, 1);
    let row = a
        .screen(110, 24)
        .into_iter()
        .find(|l| l.contains("one") && l.contains('▸'))
        .unwrap();
    assert!(row.contains('▸'), "the mark is what enter takes: {row}");
    a.key(Key::Down);
    a.key(Key::Down);
    assert_eq!(a.picker_sel, 3);
    let row = a
        .screen(110, 24)
        .into_iter()
        .find(|l| l.contains("three") && l.contains('▸'))
        .unwrap();
    assert!(row.contains('▸'), "the mark moved with the arrows: {row}");
    // Up wraps past the top; Up again wraps in from the bottom.
    a.key(Key::Up);
    a.key(Key::Up);
    assert_eq!(a.picker_sel, 1);
    // **One more Up is the project's line**, which is a stop of its own - Enter folds it - and the
    // one after that is the last session again.
    a.key(Key::Up);
    assert_eq!(a.picker_sel, 0);
    a.key(Key::Up);
    assert_eq!(a.picker_sel, 3);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s3".into())));
}

/// A click picks the session DRAWN under the pointer. This test used to encode the bug: it clicked a row and
/// expected an index counted one screen row per session, while rano draws two. The rows are found on the frame
/// now, so the test cannot drift from the drawing.
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
    let frame = a.screen(110, 24);
    let at = |needle: &str| {
        frame
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is drawn: {frame:#?}")) as u16
    };
    // The header takes row 0, the title and blank two more, then the project's line and legend, so two rows per
    // session: that is the arithmetic the handler used to get wrong. The first row is found by its number.
    assert_eq!(at("two"), at("1  one") + 2, "{frame:#?}");
    a.key(Key::Click {
        x: 10,
        y: at("two"),
    });
    assert_eq!(a.picker_sel, 2, "that row is the second session's");
    let row = a
        .screen(110, 24)
        .into_iter()
        .find(|l| l.contains("two") && l.contains('▸'))
        .unwrap();
    assert!(
        row.contains('▸'),
        "the mark moved to the clicked row: {row}"
    );
    // **The id line under a session picks THAT session**, not the next one.
    a.key(Key::Click { x: 8, y: at("s3") });
    assert_eq!(a.picker_sel, 3, "a click on three's id line picks three");
    a.key(Key::Click {
        x: 0,
        y: at("1  one"),
    });
    assert_eq!(a.picker_sel, 1);
    // A click on the footer, or on a line of it that is nobody's row, moves nothing.
    a.key(Key::Click {
        x: 4,
        y: at("enter switches"),
    });
    assert_eq!(a.picker_sel, 1);
    a.key(Key::Click {
        x: 4,
        y: at("switching does not stop anything"),
    });
    assert_eq!(a.picker_sel, 1);
    // Select and confirm stay two acts: the click only moves the mark.
    a.key(Key::Click {
        x: 4,
        y: at("three"),
    });
    assert_eq!(a.picker_sel, 3);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s3".into())));
}

/// **The picker scrolls to keep its cursor on the screen**, and a click is measured against the
/// scrolled frame. It used to be cut at the window with no scrolling, so the arrows walked a
/// cursor below the cut and Enter switched to a session nobody could see.
#[test]
fn the_picker_scrolls_its_cursor_into_view_and_clicks_follow() {
    let mut a = app();
    let mut list = vec![brief("s", "zero", true)];
    for i in 1..20 {
        list.push(brief(&format!("s{i}"), &format!("name{i:02}"), false));
    }
    a.apply(hello("s", list, Hub::new("s").snapshot()));
    a.key(Key::CtrlS);
    a.screen(110, 16);
    for _ in 0..15 {
        a.key(Key::Down);
    }
    // The project's line is row 0, so the fifteenth Down from the seeded cursor is the sixteenth
    // row of the list.
    assert_eq!(a.picker_sel, 16);
    let frame = a.screen(110, 16);
    let y = frame
        .iter()
        .position(|l| l.contains("name15"))
        .unwrap_or_else(|| panic!("the cursor's row is on the screen: {frame:#?}"));
    assert!(frame[y].contains('▸'), "{frame:#?}");
    // A click two sessions up in the scrolled frame picks that session.
    let up = frame.iter().position(|l| l.contains("name13")).unwrap() as u16;
    a.key(Key::Click { x: 10, y: up });
    assert_eq!(a.picker_sel, 14);
}

/// A provider switch reaches a head that was already attached: `Settings` has one send site and it answers a
/// request, so a head attached before the switch went on naming the model `Hello` carried.
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
    // arrive naming the new model - but no `Settings` frame is sent to this head.
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

/// The top row names what answers now. `wiring.model` is the daemon's word from `Hello`, sent once at attach, so
/// a session switched to a provider mid-conversation kept its old label.
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

/// **A switch somebody else made reaches this head.** `Settings` has one send site and it answers a
/// request, so a head that did not type `/models` went on naming the model `Hello` carried until its
/// next turn. The daemon names the model on the session's hub; the header takes that word, and a
/// model is not a warning, so no note is filed for it.
#[test]
fn a_switch_reaches_the_heads_that_did_not_type_it() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(model_settings("local (qwen-3.8-27b)", &["local"]));
    assert!(
        a.header_line(200).contains("qwen-3.8-27b"),
        "the attach state: {}",
        a.header_line(200)
    );
    let notes = a.notes.len();

    a.apply(ServerFrame::Event(env(
        9,
        SessionEvent::Warning {
            code: "model_set".into(),
            detail: "deepseek/deepseek-flash".into(),
            compaction: None,
        },
    )));

    let h = a.header_line(200);
    assert!(
        h.contains("deepseek/deepseek-flash"),
        "the switch reached nobody: {h}"
    );
    assert!(!h.contains("qwen"), "the old name is still there: {h}");
    assert_eq!(
        a.notes.len(),
        notes,
        "a model is state, not a warning to read: {:?}",
        a.notes
    );
}

/// Taking a keyless row from the picker asks for the key: the ask is a card with a masked buffer of its own,
/// never the composer, so the key cannot land in a prompt, and never the sudo path, whose `req_id` is the
/// daemon's. Enter does both in one verb.
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
    // **Typed characters are dots and never on the screen as text** - the key cannot be
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
    // **Nothing was SENT for the key** - the queue may hold the attach's own asks
    // (`Settings`, `ListJobs`, planted by the `Hello` arm), but no `Slash` left the head.
    assert!(
        b.take_actions()
            .into_iter()
            .all(|act| !matches!(act, Action::Slash { .. })),
        "esc sent nothing for the key"
    );

    // **An older daemon - no `models.keys` row at all - never asks**, and the switch goes
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

    // **And `local` never asks** - it needs no credential, which is `choice_ready`'s own rule.
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
/// the switch - the thing anybody types it for - was the least visible part.
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
    // Bare `/models` opens the list and asks the daemon, so a session whose model moved in another head does
    // not draw a stale `← now`. It refreshes the settings and not the verb, whose whole listing would land on
    // the log under the card.
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

/// The mode picker is unchanged by sharing its machinery - Esc still closes,
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

/// `/sessions` and `ctrl-s` open the picker the same way: both close a setting card that is up and seed the
/// cursor on the session this head is in. `/sessions` used to set the picker and nothing else, leaving two lists
/// on the screen.
#[test]
fn slash_sessions_opens_the_picker_the_way_ctrl_s_does() {
    let sessions = || {
        vec![
            brief("s1", "one", false),
            brief("s2", "two", false),
            brief("s3", "three", false),
        ]
    };
    for open in ["/sessions", "ctrl-s"] {
        let mut a = app();
        a.apply(hello("s2", sessions(), Hub::new("s2").snapshot()));
        a.apply(mode_settings("read-only", MODES));
        a.picker_sel = 2;
        a.command("mode");
        assert!(a.pick == Some(Pick::Mode));
        let sent = if open == "ctrl-s" {
            a.key(Key::CtrlS)
        } else {
            a.command("sessions")
        };
        assert_eq!(sent, Some(Action::ListSessions), "{open}");
        assert!(a.is_open(Surface::Picker), "{open}");
        assert!(a.pick.is_none(), "{open} closes the setting card");
        assert_eq!(
            a.picker_sel, 2,
            "{open} seeds the cursor on this session, under the project's line"
        );
    }
}

/// A picker that opened before its own list arrived seeds when the list does: `/mode` and `/models` draw the card
/// and send the ask in the same breath, so the seed reads an empty list and the cursor sits on the first row.
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
    // The head has heard nothing yet - the ask is out, the answer is not.
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

    // The cursor the reader moved is left alone: two Downs is a choice they made, and an answer landing later
    // must not undo it. The rows have to be present for there to be anything to move through.
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
    // A fresh answer - the one `/mode` itself asked for - arriving after that.
    b.apply(answer());
    assert_eq!(
        b.mode_sel, 5,
        "the answer snapped the reader's cursor back to where it would have been"
    );

    // And the rows can be another session's, not merely absent: `self.settings` is only assigned by a settings
    // frame, so a picker opened between a switch and that session's answer seeds from the session just left. The
    // re-seed runs on the frame, so the card corrects itself.
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
            ..Default::default()
        },
        timings: Default::default(),
        ends_turn: true,
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

/// A head asked for a stored session keeps the cat until it is in that session: the daemon seats the head
/// somewhere first and answers the resume after, which on a long session is seconds, and the first `Hello` used
/// to end the wait.
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
            activity: letibot_sessionlog::activity::Activity::Quiet,
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

/// A force path in flight holds the head, and the farewell says what it did whether or not it worked: a forced
/// stop that ended the daemon is still said, and one that did not keeps the verb.
#[test]
fn a_forced_stop_holds_the_head_and_is_said_either_way() {
    let escalated = || {
        let mut a = app();
        a.stop_began("dead", true, Some(4242), 1_000);
        a.clock(1_000 + STOP_DEADLINE_MS + 1);
        a.force_stop_escalated(
            ForceWhy::DidNotStop {
                waited_ms: STOP_DEADLINE_MS,
            },
            1_000 + STOP_DEADLINE_MS + 1,
        );
        a
    };
    let mut a = escalated();
    let later = 1_000 + STOP_DEADLINE_MS + 60_000;
    assert!(
        !a.stopping().unwrap().resolved(later),
        "past the deadline, but the force path has not answered"
    );
    let line = a
        .stopping()
        .unwrap()
        .waiting_line(1_000 + STOP_DEADLINE_MS + 500);
    assert!(
        line.contains("the daemon did not stop within 15s — stopping it the forced way"),
        "{line}"
    );
    // Escalated once: a second call does not restart it.
    a.force_stop_escalated(ForceWhy::DidNotStop { waited_ms: 1 }, 99_999);
    assert!(matches!(
        a.stopping().unwrap().forced.as_ref().unwrap().why,
        ForceWhy::DidNotStop { waited_ms } if waited_ms == STOP_DEADLINE_MS
    ));

    let mut failed = escalated();
    a.force_stop_finished(ForceOutcome {
        gone: true,
        said: "no turns were in flight; SIGTERM ended it (pid 4242)".into(),
    });
    assert!(a.stopping().unwrap().resolved(later));
    let said = a.stop_farewell().expect("a forced stop is said");
    assert!(said.contains("stopping it the forced way"), "{said}");
    assert!(
        said.contains("done: no turns were in flight; SIGTERM ended it"),
        "{said}"
    );
    assert!(!said.contains("letibot --stop --force"), "{said}");

    failed.force_stop_finished(ForceOutcome {
        gone: false,
        said: "SIGKILL did not end it within 2s".into(),
    });
    assert!(failed.stopping().unwrap().resolved(later));
    let said = failed
        .stop_farewell()
        .expect("a stop that did not happen is said");
    assert!(
        said.contains("the forced way did not finish it either"),
        "{said}"
    );
    assert!(
        said.contains("4242") && said.contains("letibot --stop --force"),
        "{said}"
    );
}

/// **An older daemon: the line names both protocols, and no stop frame is claimed.**
#[test]
fn an_older_daemon_is_said_as_one() {
    let mut a = app();
    a.force_stop_began(
        "dead",
        Some(4242),
        ForceWhy::OlderDaemon {
            daemon: 45,
            head: 47,
        },
        1_000,
    );
    let s = a.stopping().unwrap();
    assert!(!s.sent);
    assert!(
        s.waiting_line(1_200).contains(
            "the daemon is an older build (protocol 45, this head speaks 47) — stopping it \
             the forced way: in-flight turns are interrupted first"
        ),
        "{}",
        s.waiting_line(1_200)
    );
    assert!(
        !a.stopping().unwrap().resolved(u64::MAX),
        "held until the force path answers"
    );
    a.force_stop_finished(ForceOutcome {
        gone: false,
        said: "SIGKILL did not end it within 2s".into(),
    });
    let said = a.stop_farewell().unwrap();
    assert!(
        said.contains("was not sent as a stop frame, because the daemon is an older build"),
        "{said}"
    );
}

// ──────────────── `/sessions`, Enter, and where the head thinks it is ────────────────

/// Enter on the parent of the session you are in goes there: the picker's Enter is `switch_to`, which refuses the
/// session the head is already on, so this pins the path - standing in a child, the family list is not `already
/// here`.
#[test]
fn a_picker_enter_on_the_parent_of_the_session_you_are_in_goes_there() {
    let mut a = app();
    a.apply(hello(
        "s-sub-1",
        vec![
            brief("s", "the main conversation", true),
            {
                let mut child = brief("s-sub-1", "find the bug in the reader", true);
                child.parent_session_id = Some("s".into());
                child
            },
            {
                let mut sibling = brief("s-sub-2", "audit the store", false);
                sibling.parent_session_id = Some("s".into());
                sibling
            },
        ],
        Hub::new("s-sub-1").snapshot(),
    ));
    assert_eq!(a.key(Key::CtrlS), Some(Action::ListSessions));
    let at = a
        .picker_rows()
        .iter()
        .position(|r| r.id == "s")
        .expect("the parent is on the family list");
    a.picker_sel = at;
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Switch("s".into())),
        "Enter on the parent's row did not ask for the parent"
    );
    // And the head is waiting for it rather than sitting on the child's screen as though the key
    // had been ignored - see `entering_a_subagent_says_that_a_switch_is_in_flight`.
    assert!(a.attaching);
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("switching to the main conversation"),
        "{screen}"
    );
}

/// A list does not seat a head: a `Sessions` frame naming another session asks for the rows, and the arm used to
/// write `current` over `self.session_id`, so a head could claim a session whose transcript it did not hold.
#[test]
fn a_session_list_does_not_move_the_head_without_seating_it() {
    let mut a = app();
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    let _ = a.take_actions();
    // The daemon's answer to the `ListSessions` every seating asks for, naming a session this
    // head is not drawing.
    a.apply(ServerFrame::Sessions {
        sessions: a_family(),
        current: "s".into(),
        created: None,
    });
    assert_eq!(
        a.session_id, "s-sub-1",
        "a list moved the head off the session it is drawing"
    );
    assert!(
        a.take_actions().contains(&Action::Resync),
        "the head took the daemon's word about the seat and asked for nothing"
    );
    // And the list itself was applied - the rows, which is what a list is FOR.
    assert_eq!(a.sessions.len(), 5);
    // A list naming the session this head IS in asks for nothing at all.
    a.apply(ServerFrame::Sessions {
        sessions: a_family(),
        current: "s-sub-1".into(),
        created: None,
    });
    assert!(
        !a.take_actions().contains(&Action::Resync),
        "the ordinary answer was read as a disagreement"
    );
}

/// **A replayed `model_set` is history, not news.** A snapshot's warning window carries switches from
/// before the head attached; stamped as if they had just arrived, an old one outranked the settings
/// row the head already held, and the header drew the model the session had left.
#[test]
fn a_replayed_model_set_does_not_outrank_the_settings_row() {
    let mut a = app();
    let mut hub = Hub::new("s");
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a.apply(model_settings("glm/glm-4.6", &["local"]));
    assert!(
        a.header_line(200).contains("glm-4.6"),
        "the premise: {}",
        a.header_line(200)
    );
    // The switch it moved AWAY from, still inside the snapshot's window.
    hub.publish(SessionEvent::Warning {
        code: "model_set".into(),
        detail: "deepseek/deepseek-chat".into(),
        compaction: None,
    });
    a.apply(ServerFrame::Resync {
        reason: "test".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    let h = a.header_line(200);
    assert!(
        h.contains("glm-4.6") && !h.contains("deepseek"),
        "a switch from before the attach outranked the row the head holds: {h}"
    );
}
