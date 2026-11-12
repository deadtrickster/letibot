//! Notes, warnings, the alarm and its counters, the notice.

use super::*;

/// The negative assertion: a prompt is not retired by a row that merely contains it. A prompt `second thing` must
/// not be retired by a row reading `first thing\nsecond thing-guess`, or the head swallows a prompt the daemon has
/// not answered, which is the failure the whole-line rule exists against.
#[test]
fn a_row_that_only_contains_the_prompt_does_not_retire_it() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // No turn, deliberately: `App::submit` joins consecutive sends onto one
    // entry only while it thinks a turn is running, and these two have to be two
    // entries for the rule under test to be about matching and not about joining.
    typed(&mut a, "second thing");
    a.key(Key::Enter);
    typed(&mut a, "fourth thing");
    a.key(Key::Enter);
    assert_eq!(a.pending_prompts.len(), 2, "two entries, two sends");

    for row in [
        // The echo is the FIRST LINE of a longer row: contained, not a line.
        "second thing-guess\nand another line",
        // The echo is preceded by other text in the same line.
        "first thing\nsecond thing and more",
        // The echo's words are inside one longer line.
        "first thing\nprefixed second thing suffixed",
    ] {
        a.record_item(
            "s.contains",
            TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text { text: row.into() }],
            },
        );
        assert_eq!(
            a.pending_prompts,
            vec!["second thing".to_string(), "fourth thing".to_string()],
            "`{row}` contains the echo and did not answer it"
        );
    }

    // And the answer that IS one - a whole line - retires exactly that one.
    a.record_item(
        "s.4",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "something else\nsecond thing".into(),
            }],
        },
    );
    assert_eq!(
        a.pending_prompts,
        vec!["fourth thing".to_string()],
        "the second is a whole line of that row, so it landed"
    );
}

/// A line is spent once, which is the property the old equality rule gave
/// and which a rule that retires every matching entry would lose: two prompts that
/// say the same thing stay queued separately until each of their rows lands.
#[test]
fn one_row_does_not_retire_two_identical_prompts() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // No turn: see the note in the test above about the head's own joining.
    typed(&mut a, "say it twice");
    a.key(Key::Enter);
    typed(&mut a, "say it twice");
    a.key(Key::Enter);
    assert_eq!(a.pending_prompts.len(), 2);
    a.record_item(
        "s.0",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "say it twice".into(),
            }],
        },
    );
    assert_eq!(
        a.pending_prompts,
        vec!["say it twice".to_string()],
        "one landing row answers one of the two"
    );
}

/// A blank line is not a claim. A piece that is empty carries no words, so it
/// cannot say whether a prompt landed - and a row of blank lines must not retire
/// the queue by matching them.
#[test]
fn blank_lines_claim_nothing() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    typed(&mut a, "a real prompt");
    a.key(Key::Enter);
    a.record_item(
        "s.0",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "\n\n\n".into(),
            }],
        },
    );
    assert_eq!(a.pending_prompts, vec!["a real prompt".to_string()]);
}

/// A head knows whether it has every row the daemon says it has: `seq` is dense and assigned by the daemon alone,
/// and this head assigned it unconditionally, which is exactly what makes a delivered row and a dropped one look
/// the same.
#[test]
fn a_gap_in_the_log_is_said_repaired_and_counted() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    assert_eq!(a.gaps, 0, "a head that has missed nothing says zero");

    // Two events are lost on the socket: 2 and 3 never arrive, and 4 does.
    a.apply(ServerFrame::Event(env(
        4,
        testing::delta("t1", "the answer to a prompt this head never saw"),
    )));
    assert_eq!(a.gaps, 1, "the gap is counted");
    assert_eq!(a.seq, 4, "and the read mark still follows what was read");
    // Said in the conversation, where it scrolls with everything else, and
    // naming the range so two gaps are two lines rather than one deduped one.
    let notes = a.notes_lines().join("\n");
    assert!(notes.contains("log_gap"), "nothing said the gap: {notes}");
    assert!(
        notes.contains("2..3"),
        "the lost seqs are not named: {notes}"
    );
    // And repaired. A resync is the one thing that can put the head back in
    // step, and the head cannot do it itself: the daemon owns the transcript.
    assert!(
        a.queued.iter().any(|x| matches!(x, Action::Resync)),
        "the gap was noted and not repaired"
    );

    // A second gap is a second line, not the first one restated.
    a.apply(ServerFrame::Event(env(9, testing::delta("t1", "later"))));
    assert_eq!(a.gaps, 2);
    let notes = a.notes_lines().join("\n");
    assert!(notes.contains("5..8"), "{notes}");

    // Continuity is silent. Ten events in a row say nothing at all, which is
    // the property that makes the line worth reading when it does appear.
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    for seq in 1..=10 {
        a.apply(ServerFrame::Event(env(seq, testing::turn_started("t1"))));
    }
    assert_eq!(a.gaps, 0);
    assert!(a.notes.is_empty(), "{:?}", a.notes);
    // And it is on the status screen, beside the five counters it belongs with.
    // A tall frame: `/status` is a long list and the row has to be reached rather
    // than scrolled for in a test.
    a.command("status");
    let status = a.screen(100, 100).join("\n");
    assert!(status.contains("gaps"), "no gap row on /status: {status}");
}

/// The other two numbers: a body with no row, and a daemon that is ahead. `gaps` is the wire losing events, and
/// these two look identical to it from inside: `orphan_bodies` is a row announced and then replaced away, so its
/// words have nowhere to go.
#[test]
fn a_body_with_no_row_and_a_daemon_that_is_ahead_are_both_counted() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));

    // A body for a row this head never had: counted, said, and drawn nowhere  -
    // there is nothing to draw it on.
    a.record_item(
        "s.gone",
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "a prompt that fell out of a snapshot".into(),
            }],
        },
    );
    assert_eq!(a.orphan_bodies, 1);
    let notes = a.notes_lines().join("\n");
    assert!(notes.contains("orphan_body"), "{notes}");
    assert!(
        notes.contains("s.gone"),
        "the note does not name the row: {notes}"
    );
    // And it is not in the conversation, because there is no row for it.
    let screen = a.screen(100, 40).join("\n");
    assert!(
        !screen.contains("a prompt that fell out of a snapshot"),
        "a body with no row was drawn anyway: {screen}"
    );

    // A daemon that answers from further along than this head has read: the command's effect is visible at 9 while
    // the head is at 1, so eight events are in flight and none are lost. Being behind is a different fact.
    a.apply(ServerFrame::Accepted {
        client_request_id: "r1".into(),
        seq: 9,
        note: letibot_sessionlog::protocol::NOTE_PROMPT_QUEUED.into(),
    });
    assert_eq!(a.behind, 8, "the distance the daemon stated was not kept");
    assert_eq!(
        a.gaps, 0,
        "being behind is not a gap, and must never move it"
    );
    assert!(
        !a.queued.iter().any(|x| matches!(x, Action::Resync)),
        "being behind is not a reason to resync"
    );

    // A redelivery - a seq at or below this head's own - is not a distance.
    a.apply(ServerFrame::Accepted {
        client_request_id: "r2".into(),
        seq: 1,
        note: letibot_sessionlog::protocol::NOTE_PROMPT_QUEUED.into(),
    });
    assert_eq!(a.behind, 0);
}

/// A refusal the harness made is one dim line and not a wall in red: every word of the explanation is addressed to
/// the model, and it was already on the screen as the tool's own result one row above.
#[test]
fn a_refusal_nobody_made_is_quiet_and_one_line() {
    let mut a = app();
    let long = "this command's meaning does not exist yet, so nothing can decide \
                    about it. The grammar read 315 bytes and 7 stage(s) and could not \
                    resolve:\n  parameter_expansion at 1:2 (bytes 2..4) decides the \
                    assignment: \"$$\"\n      `$$` decides what the variable will hold, \
                    and its value is not in this text.";
    let hub = Hub::new("s");
    let att = hub.attach("tui", "test", Caps::default(), 0);
    hub.publish(letibot_sessionlog::event::SessionEvent::DenialRaised {
        request_id: "adj-s-1789462738453908838-0001".into(),
        turn_id: "t1".into(),
        call_id: "c1".into(),
        tool: "bash".into(),
        summary: "`bash` wants exec access to `p=$$; for i in 1 2 3; do read -r ppid; done`".into(),
        baseline: "ask — intents [execute_code]".into(),
        by: "boundary:normaliser".into(),
        basis: long.into(),
        tier: "adjudicable".into(),
        outcome: "not_run".into(),
        repeat_count: 1,
        breaker_open: false,
        grant: "Nothing was executed and nothing changed. No grant applies.".into(),
    });
    feed(&mut a, &hub, &att.head_id);

    // A first refusal says nothing here at all: the refused call is a row in
    // the transcript one line below, with the same sentence on it.
    assert!(
        a.notes.is_empty(),
        "a single refusal is stated twice: {:?}",
        a.notes
    );

    // A SECOND attempt at the same direction is a fact about the session that
    // the row cannot carry, so that one does speak.
    let hub = Hub::new("s2");
    let att = hub.attach("tui", "test", Caps::default(), 0);
    hub.publish(letibot_sessionlog::event::SessionEvent::DenialRaised {
        request_id: "adj-2".into(),
        turn_id: "t1".into(),
        call_id: "c2".into(),
        tool: "bash".into(),
        summary: "`bash` wants exec access to `p=$$`".into(),
        baseline: "ask — intents [execute_code]".into(),
        by: "boundary:normaliser".into(),
        basis: long.into(),
        tier: "adjudicable".into(),
        outcome: "not_run".into(),
        repeat_count: 2,
        breaker_open: false,
        grant: "No grant applies.".into(),
    });
    let mut a = app();
    feed(&mut a, &hub, &att.head_id);
    let note = a.notes.last().expect("a repeat is worth saying");
    let lines = note_lines(&a.cfg, &note.1);
    // ONE line, and the head's one sentence a click away. The thing being measured
    // is that it is not a paragraph per unresolved construct.
    assert_eq!(
        lines.len(),
        1,
        "a wall again, {} lines: {lines:#?}",
        lines.len()
    );
    let l = lines.join(" ");
    let l = &l;
    assert!(l.contains("bash not_run"), "{l}");
    let (_, whole, _) = crate::ui::note_card(&note.1).expect("a card");
    assert!(
        whole.contains("meaning does not exist yet"),
        "the gist survived: {whole}"
    );
    assert!(
        !whole.contains("parameter_expansion"),
        "the model's detail leaked: {whole}"
    );
    // Not the paragraph, not the id, not the loud register.
    assert!(
        !l.contains("parameter_expansion"),
        "the model's detail leaked: {l}"
    );
    assert!(!l.contains("adj-s-"), "an id nobody can use: {l}");
    assert!(!l.contains('!'), "still shouting: {l}");
}

/// A program that dies at once leaves its sentence and its status, and they stay: the pane was taken and one
/// sentence posted as a notice, which lives for `NOTICE_MS`, so what the program printed went with the rectangle.
#[test]
fn a_pane_that_dies_at_once_leaves_a_row_with_its_sentence_and_its_status() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    a.clock(1_000);
    // One frame before the key, because a pane's rectangle is the last frame's: a test that opened a pane before
    // drawing would be asking a 1x1 screen what the program left on it.
    let _ = a.screen(80, 24);
    pane(&mut a, b"mc: not found\r\n");
    a.apply(ServerFrame::TermEnded {
        pane: Some(1),
        reason: "the program exited with 127".into(),
    });
    assert!(!a.pane_open());

    // A row and not a notice. `say` is what carried this, and it is the whole reason
    // the ending was invisible: the notice is gone in `NOTICE_MS` and the row is not.
    assert!(
        a.notice.is_none(),
        "an ending is not a notice: {:?}",
        a.notice
    );
    a.clock(1_000 + 60_000);
    let frame = a.screen(80, 24);
    assert!(
        frame.iter().any(|r| r.contains("!term mc")),
        "the row names the line that was run: {frame:?}"
    );
    assert!(
        frame
            .iter()
            .any(|r| r.contains("the program exited with 127")),
        "and the daemon's own sentence, with the status in it: {frame:?}"
    );
    assert!(
        frame.iter().any(|r| r.contains("mc: not found")),
        "and the last thing the program printed — which is the sentence a person needs \
             when a program dies instantly: {frame:?}"
    );

    // And a second pane that dies the same way is a second row. The identity a note is
    // deduped by cannot tell two identical endings apart, so a pane's ending is filed without
    // the redelivery test - otherwise the second `!term mc` would be swallowed by the first.
    pane(&mut a, b"mc: not found\r\n");
    a.apply(ServerFrame::TermEnded {
        pane: Some(1),
        reason: "the program exited with 127".into(),
    });
    let rows = a
        .screen(80, 24)
        .iter()
        .filter(|r| r.contains("!term mc"))
        .count();
    assert_eq!(
        rows,
        2,
        "two endings are two rows, however alike they are: {:?}",
        a.screen(80, 24)
    );
}

/// A note the person has read can be retired and stays retired: a note is a disclosure and not a permanent record,
/// since the log holds the durable fact, and a retired note must stay retired across a resync and a restart.
#[test]
fn ctrl_n_retires_every_note_and_keeps_every_note() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    // Four notes, which is the shape the operator actually reported: a restart was
    // four notes at three lines each, and a wall was two.
    for (code, detail) in [
        (
            "gate_timeout",
            "denied: nobody answered before the deadline",
        ),
        ("daemon_stopping", "`dead` asked this daemon to stop"),
        // Drawn codes: `compacted` and `auto_compact` twin the fork's own row and are filed
        // beside the conversation, so they would not be on the screen for this press to clear.
        (
            "workspace_gone",
            "this session's workspace is not there any more",
        ),
        (
            "boundary_absent",
            "this session has no boundary, so nothing confines the sandbox",
        ),
    ] {
        let e = hub.publish(SessionEvent::Warning {
            code: code.into(),
            detail: detail.into(),

            compaction: None,
        });
        a.apply(ServerFrame::Event(e));
    }
    let before = a.screen(100, 30).join("\n");
    assert!(before.contains("gate_timeout"), "{before}");
    assert!(before.contains("boundary_absent"), "{before}");

    // One key, and the wall goes.
    assert_eq!(a.key(Key::CtrlN), None, "the chord is not a daemon action");
    let after = a.screen(100, 30).join("\n");
    for code in [
        "gate_timeout",
        "daemon_stopping",
        "workspace_gone",
        "boundary_absent",
    ] {
        assert!(
            !after.contains(code),
            "`{code}` survived the press:\n{after}"
        );
    }
    // And it says what it did in the verb's own words, because it calls the verb rather than composing a second
    // sentence: a chord with its own wording can come to disagree with `/notes dismiss all` about what happened.
    assert!(after.contains("retired 4 note(s)"), "{after}");
    let notice = a.notice.clone().unwrap_or_default();
    assert!(notice.contains("still counted on /status"), "{notice}");

    // Not deleted. Every one is still held, still counted, still readable.
    assert_eq!(a.notes.len(), 4, "a note was dropped, not retired");
    assert_eq!(a.retired_notes(), 4);
    a.command("status");
    let stats = a.screen(120, 60).join("\n");
    assert!(stats.contains("4 retired"), "{stats}");
    a.key(Key::Esc);
    typed(&mut a, "/notes");
    a.key(Key::Enter);
    let listed = a.screen(120, 60).join("\n");
    assert!(listed.contains("nobody answered"), "{listed}");
    assert!(listed.contains("[retired]"), "{listed}");
    a.key(Key::Esc);

    // A second press says there is nothing left to retire. Not "retired 0 note(s)",
    // which is what the verb would say and is a sentence an operator should not be shown:
    // the notes are held but retired, so the count of what is LEFT is zero.
    assert_eq!(a.key(Key::CtrlN), None);
    let twice = a.screen(100, 30).join("\n");
    assert!(twice.contains("nothing to retire"), "{twice}");
    assert!(!twice.contains("retired 0 note(s)"), "{twice}");

    // And back, if the reader was wrong - the undo is exactly as cheap as the act.
    typed(&mut a, "/notes restore");
    a.key(Key::Enter);
    let back = a.screen(100, 30).join("\n");
    assert!(back.contains("gate_timeout"), "restore did nothing: {back}");
    // Said whole in the notice; the status row carries what fits after the turn's clock.
    let notice = a.notice.clone().unwrap_or_default();
    assert!(
        notice.contains("/notes restore") || notice.contains("back on the screen"),
        "{notice}"
    );
}

/// The reflex survives a restart, which is the half that stops it
/// having to be repeated - and the half the operator hit
/// in use.
#[test]
fn a_note_retired_with_ctrl_n_stays_retired_across_a_restart() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let clean = hub.snapshot();
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], clean.clone()));
    let live = hub.publish(SessionEvent::Warning {
        code: "gate_timeout".into(),
        detail: "denied: nobody answered before the deadline".into(),

        compaction: None,
    });
    a.apply(ServerFrame::Event(live.clone()));
    assert!(a.screen(100, 30).join("\n").contains("gate_timeout"));

    // A real file, because the property is about the file.
    let dir = std::env::temp_dir().join(format!("letibot-r22-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("head.toml");
    a.prefs_path = Some(path.clone());
    a.key(Key::CtrlN);
    assert!(
        !a.screen(100, 30).join("\n").contains("gate_timeout"),
        "the chord did not retire it"
    );
    assert!(path.is_file(), "the press did not reach head.toml");

    // A new head, the same file, the incident delivered live - the one delivery
    // nothing else suppresses (a snapshot's copy is prior, a redelivery is not filed
    // twice), so what stops it here is the persisted retirement and nothing else.
    let mut b = app();
    b.prefs_path = Some(path.clone());
    b.load_prefs();
    assert_eq!(b.dismissed.len(), 1, "{:?}", b.dismissed);
    b.apply(hello("s", vec![brief("s", "one", false)], clean));
    b.apply(ServerFrame::Event(live));
    let after = b.screen(100, 30).join("\n");
    assert!(
        !after.contains("gate_timeout"),
        "a restart replanted what ctrl-n retired:\n{after}"
    );
    assert_eq!(b.notes.len(), 1, "hidden, not dropped: {:?}", b.notes);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `ctrl-n` is a watermark and not a hide: a key set is a statement about copies, so a re-said retry or a
/// re-scrubbed replay is a different key and the wall comes back. The mark says *I have read everything up to
/// here*, per kind.
#[test]
fn ctrl_n_is_a_watermark_so_the_next_notice_arrives_alone() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    // Three of one kind, at three instants - the shape of a retry ladder. Explicit stamps
    // rather than `Hub::publish`, because the mark is an ORDER and a test may not depend on
    // two publishes not landing in the same millisecond.
    for (i, ts) in [(1u64, 1_000u64), (2, 2_000), (3, 3_000)] {
        a.apply(ServerFrame::Event(env_at(
            0,
            ts,
            SessionEvent::Warning {
                code: "model_endpoint_retry".into(),
                detail: format!("the model endpoint did not answer: attempt {i} of 5"),
                compaction: None,
            },
        )));
    }
    // **The premise, since bursts: three notices of one kind in one window are ONE counted
    // row** (`app::COALESCE_MS`) - so this is a count to clear, not a wall, and every assertion
    // below is about the mark rather than about how many lines there were.
    let wall = a.screen(120, 40).join("\n");
    assert_eq!(
        wall.matches("model_endpoint_retry").count(),
        1,
        "the premise — three notices, one counted row:\n{wall}"
    );
    assert!(
        wall.contains("3 model_endpoint_retry"),
        "the count does not say three:\n{wall}"
    );

    assert_eq!(a.key(Key::CtrlN), None);
    assert_eq!(
        a.screen(120, 40)
            .join("\n")
            .matches("model_endpoint_retry")
            .count(),
        0,
        "the press did not clear the wall"
    );
    // Hidden, not dropped, and the mark is what the head now holds of that kind.
    assert_eq!(a.notes.len(), 3, "a note was dropped, not retired");
    assert_eq!(a.retired_notes(), 3);
    assert_eq!(
        a.read_upto("model_endpoint_retry"),
        Some(3_000),
        "the mark is the newest one on the screen: {:?}",
        a.read_marks
    );

    // A re-render in between. A resync replaces `self.notes` wholesale from the daemon's
    // snapshot - which still carries all three, because nothing trims the source - so this is
    // the strongest form of the thing that could bring the wall back.
    a.apply(ServerFrame::Resync {
        reason: "queue overflow".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    assert_eq!(
        a.screen(120, 40)
            .join("\n")
            .matches("model_endpoint_retry")
            .count(),
        0,
        "a re-render resurrected the wall"
    );

    // And the next one of that kind arrives alone.
    a.apply(ServerFrame::Event(env_at(
        0,
        4_000,
        SessionEvent::Warning {
            code: "model_endpoint_retry".into(),
            detail: "the model endpoint did not answer: attempt 4 of 5".into(),
            compaction: None,
        },
    )));
    let after = a.screen(120, 40).join("\n");
    assert_eq!(
        after.matches("model_endpoint_retry").count(),
        1,
        "the wall came back plus one:\n{after}"
    );
    // And the line is the NEW one: the head's last note is the fourth, and it is live.
    let newest = a.notes.last().expect("the fourth was filed").1.clone();
    assert!(
        matches!(note_stamp(&newest), Some(4_000)),
        "the one line drawn is not the new notice: {:?}",
        note_stamp(&newest)
    );
    assert!(
        !a.is_retired(&newest),
        "the new notice is hidden, not drawn"
    );
}

/// A kind the watermark never covered is not swallowed by it, which is why the mark is per kind: one read-up-to
/// across the whole list would hide the first of a kind this reader has never seen.
#[test]
fn a_notice_of_a_kind_the_watermark_never_covered_is_drawn() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    for (i, ts) in [(1u64, 1_000u64), (2, 2_000)] {
        a.apply(ServerFrame::Event(env_at(
            0,
            ts,
            SessionEvent::Warning {
                code: "model_endpoint_retry".into(),
                detail: format!("did not answer: attempt {i} of 5"),
                compaction: None,
            },
        )));
    }
    assert_eq!(a.key(Key::CtrlN), None);
    assert_eq!(
        a.screen(120, 40)
            .join("\n")
            .matches("model_endpoint_retry")
            .count(),
        0
    );

    // Older than the mark, and of a kind nobody has read - the case a list-wide watermark
    // would swallow, and the one that must be drawn.
    a.apply(ServerFrame::Event(env_at(
        0,
        500,
        SessionEvent::Warning {
            code: "store_busy_wait".into(),
            detail: "the store is busy; the rows are in memory and the turn is waiting".into(),
            compaction: None,
        },
    )));
    let after = a.screen(120, 40).join("\n");
    assert!(
        after.contains("store_busy_wait"),
        "a kind the mark never covered was swallowed by it:\n{after}"
    );
    assert_eq!(
        a.read_upto("store_busy_wait"),
        None,
        "a kind with no mark was given one without being read"
    );
}

/// The same incident said again in other words is not news, which is the half a key set cannot do: `note_key` is
/// `(code, ts, hash(detail))`, so a copy whose words differ is a different key and the wall comes back.
#[test]
fn a_notice_of_a_kind_the_reader_has_read_past_is_not_news_in_other_words() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    for (i, ts) in [(1u64, 1_000u64), (2, 2_000), (3, 3_000)] {
        a.apply(ServerFrame::Event(env_at(
            0,
            ts,
            SessionEvent::Warning {
                code: "model_endpoint_retry".into(),
                detail: format!("did not answer: attempt {i} of 5"),
                compaction: None,
            },
        )));
    }
    assert_eq!(a.key(Key::CtrlN), None);
    assert_eq!(
        a.screen(120, 40)
            .join("\n")
            .matches("model_endpoint_retry")
            .count(),
        0
    );

    // The second one again, at its own instant and in different words. Not in the key
    // set (a different detail is a different key) and older than the mark.
    a.apply(ServerFrame::Event(env_at(
        0,
        2_000,
        SessionEvent::Warning {
            code: "model_endpoint_retry".into(),
            detail: "did not answer: attempt 2 of 5 — said again, and re-worded".into(),
            compaction: None,
        },
    )));
    let after = a.screen(120, 40).join("\n");
    assert_eq!(
        after.matches("model_endpoint_retry").count(),
        0,
        "a re-worded copy of a notice the reader had read was drawn:\n{after}"
    );
    // Hidden, and counted - the rule `/status` keeps for every retired note.
    assert_eq!(a.notes.len(), 4, "it was dropped instead of hidden");
    assert_eq!(a.retired_notes(), 4);
    // And a NEWER one is still news: the mark is not a mute.
    a.apply(ServerFrame::Event(env_at(
        0,
        9_000,
        SessionEvent::Warning {
            code: "model_endpoint_retry".into(),
            detail: "did not answer: attempt 9 of 9".into(),
            compaction: None,
        },
    )));
    let newer = a.screen(120, 40).join("\n");
    assert_eq!(
        newer.matches("model_endpoint_retry").count(),
        1,
        "the watermark became a mute:\n{newer}"
    );
}

/// The mark outlives the key set being reloaded from the file, the one moment the two records can disagree:
/// `refresh_retired` replaces `dismissed` with the file's list, and a key the mark added is not in that file.
#[test]
fn the_watermark_survives_the_key_set_being_reloaded_from_the_file() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    let dir = std::env::temp_dir().join(format!("letibot-r22-mark-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory for the file");
    let path = dir.join("head.toml");
    a.prefs_path = Some(path.clone());
    for (i, ts) in [(1u64, 1_000u64), (2, 2_000), (3, 3_000)] {
        a.apply(ServerFrame::Event(env_at(
            0,
            ts,
            SessionEvent::Warning {
                code: "model_endpoint_retry".into(),
                detail: format!("did not answer: attempt {i} of 5"),
                compaction: None,
            },
        )));
    }
    assert_eq!(a.key(Key::CtrlN), None);
    assert!(path.is_file(), "the press did not reach head.toml");

    // Another head saves the file with nothing retired in it - a restore, or a head that has
    // never dismissed anything. This head's own key set is replaced by it.
    std::fs::write(&path, "diff = split\nretired =\n").expect("write the file");
    a.refresh_retired();
    assert_eq!(
        a.dismissed.len(),
        3,
        "the reload did not happen, or the mark did not put its keys back: {:?}",
        a.dismissed
    );
    let after = a.screen(120, 40).join("\n");
    assert_eq!(
        after.matches("model_endpoint_retry").count(),
        0,
        "reloading the file dropped the mark and drew the wall again:\n{after}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A kind with no clock is never watermarked: `unreadable_frame` and `log_gap` happen on the socket, so `ts` is 0
/// and two of them cannot be ordered, and a mark of 0 would hide the second for ever.
#[test]
fn a_note_with_no_clock_is_never_hidden_by_a_watermark() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a.apply(ServerFrame::Event(env_at(
        0,
        1_000,
        SessionEvent::Warning {
            code: "model_endpoint_retry".into(),
            detail: "did not answer: attempt 1 of 5".into(),
            compaction: None,
        },
    )));
    a.unreadable(Unreadable {
        line: "{\"nonsense\":true}".into(),
        detail: "unknown variant `nonsense`".into(),
    });
    assert_eq!(a.key(Key::CtrlN), None);
    assert!(
        !a.screen(120, 40).join("\n").contains("unreadable_frame"),
        "the press did not clear it either"
    );
    // A second frame this build cannot read is a second fact, and it must be drawn: the mark
    // says nothing about a note with no clock.
    a.unreadable(Unreadable {
        line: "{\"still\":\"nonsense\"}".into(),
        detail: "unknown variant `still`".into(),
    });
    let after = a.screen(120, 40).join("\n");
    assert!(
        after.contains("unreadable_frame"),
        "a notice with no clock was muted by a watermark:\n{after}"
    );
    assert_eq!(
        a.read_upto("unreadable_frame"),
        None,
        "a note with no clock was given a mark"
    );
}

#[test]
fn a_note_the_operator_has_read_can_be_retired_and_stays_retired() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    // The snapshot from before the incident, which is what the restarted head gets. Taken here rather than inside
    // the restart block, since by then the hub carries the warning and the snapshot would hold it already.
    let clean = hub.snapshot();

    let mut a = app();
    a.clock(1_000);
    // Attached *before* the warning exists, so the head is there to see it happen:
    // this is the live path, which is the only path that plants a note now.
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    let published = hub.publish(SessionEvent::Warning {
        code: "gate_timeout".into(),
        detail: "denied: nobody answered before the deadline".into(),

        compaction: None,
    });
    a.apply(ServerFrame::Event(published.clone()));
    let screen = a.screen(100, 30).join("\n");
    assert!(screen.contains("gate_timeout"), "{screen}");

    // Retired, and the wall goes - including its blank line. The sentence said
    // `/notes`, so that is the verb; `/dismiss` is the same act under the word a
    // person types at a red block.
    typed(&mut a, "/dismiss");
    assert_eq!(a.key(Key::Enter), None);
    let hidden = a.screen(100, 30).join("\n");
    assert!(
        !hidden.contains("gate_timeout"),
        "still on the screen: {hidden}"
    );
    assert!(!hidden.contains("nobody answered"), "{hidden}");

    // Hidden is not deleted: the note is still this head's, `/status` counts it, and `/notes` lists it with its text
    // under a marker, which makes *I chose not to show this* a different statement from *nothing happened*.
    assert_eq!(a.notes.len(), 1, "the note was dropped, not retired");
    assert_eq!(a.retired_notes(), 1);
    a.command("status");
    let stats = a.screen(120, 60).join("\n");
    assert!(stats.contains("1 retired"), "{stats}");
    a.key(Key::Esc);
    typed(&mut a, "/notes");
    a.key(Key::Enter);
    let listed = a.screen(120, 60).join("\n");
    assert!(listed.contains("nobody answered"), "{listed}");
    assert!(listed.contains("[retired]"), "{listed}");
    a.key(Key::Esc);

    // The two things that could replant it: a resync replaces the head's whole note list from a snapshot, so
    // the wall would come back at position 0 above the whole conversation. The identity is built from the
    // note, so a copy does not come back.
    let key = note_key(&a.notes[0].1);
    a.apply(ServerFrame::Resync {
        reason: "queue overflow".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    let after_resync = a.screen(100, 30).join("\n");
    assert!(
        !after_resync.contains("gate_timeout"),
        "a resync replanted what was dismissed: {after_resync}"
    );
    // A reattach is the same event from the other direction: the same `Hello`, the
    // same snapshot, a new window.
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    assert!(
        !a.screen(100, 30).join("\n").contains("gate_timeout"),
        "a reattach replanted it"
    );

    // The restart: the dismissals live in `head.toml` and not in the process, and a
    // head that has just started has shown nothing, so the incident is delivered live, at a seam.
    let dir = std::env::temp_dir().join(format!("letibot-notes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("head.toml");
    crate::prefs::save(
        &path,
        &crate::prefs::HeadPrefs {
            retired: vec![key.clone()],
            ..Default::default()
        },
    )
    .unwrap();
    let mut b = app();
    b.prefs_path = Some(path.clone());
    b.load_prefs();
    assert_eq!(
        b.dismissed,
        vec![key.clone()],
        "the file's retired set did not reach the head"
    );
    b.apply(hello("s", vec![brief("s", "one", false)], clean));
    b.apply(ServerFrame::Event(published.clone()));
    assert!(
        !b.screen(100, 30).join("\n").contains("gate_timeout"),
        "a restart replanted it: dismissed={:?} key={key:?}",
        b.dismissed
    );
    assert_eq!(b.notes.len(), 1, "hidden, not dropped: {:?}", b.notes);

    // And back, if the reader was wrong. Two ways, and both of them are the
    // reader's: no dismissal is ever final in a way they cannot undo.
    typed(&mut b, "/notes restore");
    b.key(Key::Enter);
    let back = b.screen(100, 30).join("\n");
    assert!(back.contains("gate_timeout"), "restore did nothing: {back}");
    // And the file: restoring on the screen while the file kept the key is the
    // defect, since the next save by any head put the dismissal back, so restore lasted until somebody else saved.
    let (p, _) = crate::prefs::load(&path);
    assert!(
        !p.retired.iter().any(|k| k == &key),
        "restore did not reach the file: {:?}",
        p.retired
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dismissal unions and a restore replaces: a union can only add a key, so under a union a restore's save wrote
/// the file's own keys straight back and the head's `dismissed` went to empty while the file kept the key.
#[test]
fn a_restore_replaces_the_files_set_and_a_dismissal_unions_with_it() {
    let dir = std::env::temp_dir().join(format!("letibot-restore-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("head.toml");

    // Another head retired one key before this head ever looked.
    crate::prefs::save(
        &path,
        &crate::prefs::HeadPrefs {
            retired: vec!["w|theirs|1|aaaa".into()],
            ..Default::default()
        },
    )
    .unwrap();

    let mut a = app();
    a.prefs_path = Some(path.clone());
    a.dismissed = vec!["w|mine|2|bbbb".into()];

    // A dismissal unions: the other head's key survives this head's save.
    let _ = a.save_prefs(RetiredWrite::Union);
    let (p, _) = crate::prefs::load(&path);
    assert!(
        p.retired.iter().any(|k| k == "w|theirs|1|aaaa"),
        "a dismissal discarded another head's key: {:?}",
        p.retired
    );
    assert!(
        p.retired.iter().any(|k| k == "w|mine|2|bbbb"),
        "{:?}",
        p.retired
    );

    // A restore replaces: this head's set is the whole truth, and nothing comes back.
    a.dismissed.clear();
    let _ = a.save_prefs(RetiredWrite::Replace);
    let (q, _) = crate::prefs::load(&path);
    assert!(
        q.retired.is_empty(),
        "a restore did not remove the keys — the next save by any head would put them \
             back and `restore` would be a verb whose effect lasts until somebody else \
             saves: {:?}",
        q.retired
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The person's own state, reproduced from the measurement rather than guessed: `/notes` said `7 note(s), 1
/// retired` while only one entry carried `[retired]` and `head.toml` held keys for the notes that were listed.
#[test]
fn the_head_retires_every_note_the_file_it_wrote_says_it_retired() {
    let facts: [(&str, u64, &str); 6] = [
        (
            "mode_set",
            1790080806043,
            "this session is at `allow-all (this box, consented)` from the next call \
                 — nothing confines this box, and this point stands on your confirmation \
                 rather than on a boundary.",
        ),
        (
            "context_wall",
            1790104751092,
            "stopping this turn after 22 round(s)",
        ),
        (
            "auto_compact",
            1790104751092,
            "938785 of 999999 tokens resident",
        ),
        (
            "compacted",
            1790104765616,
            "compacted: 941052 → 8748 tokens",
        ),
        (
            "auto_compact",
            1790104765616,
            "compacted: 8748 tokens resident now",
        ),
        ("promote_idle", 1790108290718, "nothing was running to move"),
    ];

    // The shape the head's own file must have honoured: every one of them retired,
    // written as the file writes it and read back through the file's own parser.
    let dir = std::env::temp_dir().join(format!("letibot-operator-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("head.toml");
    let keys: Vec<String> = facts
        .iter()
        .map(|(code, ts, detail)| {
            note_key(&Note::Warned(Warned {
                code: (*code).into(),
                detail: (*detail).into(),
                ts: *ts,
            }))
        })
        .collect();
    crate::prefs::save(
        &path,
        &crate::prefs::HeadPrefs {
            retired: keys.clone(),
            ..Default::default()
        },
    )
    .unwrap();

    let mut a = app();
    a.prefs_path = Some(path.clone());
    a.load_prefs();
    assert_eq!(
        a.dismissed, keys,
        "the file's retired set did not reach the head"
    );

    // The same six, delivered as the log delivers them: a snapshot, with the clock
    // each envelope carried.
    let mut snapshot = Snapshot {
        session_id: "s".into(),
        seq: 1,
        dropped: 0,
        items_dropped: 0,
        items_lead: 0,
        items: Vec::new(),
        turn: None,
        open_decisions: Vec::new(),
        settled_decisions: Vec::new(),
        warnings: facts
            .iter()
            .map(|(code, ts, detail)| Warned {
                code: (*code).into(),
                detail: (*detail).into(),
                ts: *ts,
            })
            .collect(),
        heads: Vec::new(),
        subagents: Vec::new(),
        activity: letibot_sessionlog::activity::Activity::Quiet,
    };
    snapshot.warnings.truncate(facts.len());
    a.apply(hello("s", vec![brief("s", "one", false)], snapshot));

    assert_eq!(a.notes.len(), facts.len(), "{:?}", a.notes);
    assert_eq!(
        a.retired_notes(),
        facts.len(),
        "the file says {} retired and the head retired {}: dismissed={:?}",
        facts.len(),
        a.retired_notes(),
        a.dismissed
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dismissal a second head made is honoured by this one without a restart: `head.toml` is one file for every
/// head on the box and `load_prefs` runs once at startup, so a head up for hours saw only its own presses.
#[test]
fn a_dismissal_recorded_after_this_head_loaded_is_honoured_anyway() {
    let dir = std::env::temp_dir().join(format!("letibot-shared-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("head.toml");

    // One head, up first, attaching to a log that already holds one warning.
    let hub = Hub::new("s");
    let live = hub.publish(SessionEvent::Warning {
        code: "gate_timeout".into(),
        detail: "nobody answered within 300s".into(),

        compaction: None,
    });
    let mut a = app();
    a.prefs_path = Some(path.clone());
    a.load_prefs();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    assert_eq!(a.notes.len(), 1);
    assert_eq!(a.retired_notes(), 0, "nothing retired yet");

    // A second head retires it. Not a call into `a` - a write to the file this
    // head will look at, which is the only thing the two processes share.
    let key = note_key(&a.notes[0].1);
    crate::prefs::save(
        &path,
        &crate::prefs::HeadPrefs {
            retired: vec![key.clone()],
            ..Default::default()
        },
    )
    .unwrap();

    // Before it looks, this head does not know. `dismissed` is what it loaded at
    // startup, and the file has moved since - which is the state the operator was in.
    assert_eq!(
        a.retired_notes(),
        0,
        "the premise is a head that has not read the file since it changed, and this \
             one already had: dismissed={:?}",
        a.dismissed
    );
    typed(&mut a, "/notes");
    a.key(Key::Enter);
    let listed = a.screen(120, 60).join("\n");
    assert!(
        listed.contains("[retired"),
        "the listing showed a note the file says is retired as live:\n{listed}"
    );
    assert!(listed.contains("1 retired"), "{listed}");
    let _ = std::fs::remove_dir_all(&dir);

    // And the same envelope arriving again lands retired rather than planting
    // itself - the redelivery half, with the key read off the file
    // rather than out of this head's own memory.
    a.key(Key::Esc);
    a.apply(ServerFrame::Event(live.clone()));
    let screen = a.screen(120, 60).join("\n");
    assert!(
        !screen.contains("nobody answered"),
        "a note retired by another head came back at a seam:\n{screen}"
    );
}

/// First fault: history arrives as news. A restarted head was met by twelve red lines at the top of a
/// session that had just started, and none of them had been dismissed because none had been shown.
#[test]
fn a_fresh_attach_shows_the_conversation_and_not_what_came_before_it() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    for (code, detail) in [
        (
            "daemon_stopping",
            "`dead` asked this daemon to stop. Every head detaches",
        ),
        (
            "auto_compact",
            "938065 of 999999 tokens resident — compacting now",
        ),
        (
            "compacted",
            "compacted: 940188 → 9181 tokens, on transcript s#t25",
        ),
    ] {
        hub.publish(SessionEvent::Warning {
            code: code.into(),
            detail: detail.into(),

            compaction: None,
        });
    }

    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    let screen = a.screen(100, 30).join("\n");
    for code in ["daemon_stopping", "auto_compact", "compacted"] {
        assert!(
            !screen.contains(code),
            "`{code}` was planted above a conversation it did not precede:\n{screen}"
        );
    }
    assert!(
        !screen.contains("940188"),
        "the note's text is on the screen, so it is not only the code that leaks:\n{screen}"
    );

    // Held, counted, listed - the three things asked for instead of drawing
    // them. `/notes` is where the fix puts them and `/status` is how a reader knows
    // there is something there.
    assert_eq!(a.notes.len(), 3, "{:?}", a.notes);
    assert_eq!(a.notes_before(), 3);
    assert_eq!(a.retired_notes(), 0);
    a.command("status");
    let stats = a.screen(120, 60).join("\n");
    assert!(
        stats.contains("3 not drawn"),
        "`/status` must say why they are not on the screen:\n{stats}"
    );
    a.key(Key::Esc);
    typed(&mut a, "/notes");
    a.key(Key::Enter);
    let listed = a.screen(120, 60).join("\n");
    assert!(listed.contains("940188"), "{listed}");
    assert!(listed.contains("[before this window]"), "{listed}");
    a.key(Key::Esc);

    // **The control, and without it this test would pass on a head that draws no
    // notes at all.** A fact that happens while this head is watching is news, and
    // news is drawn where it happened.
    let live = hub.publish(SessionEvent::Warning {
        code: "context_wall".into(),
        detail: "stopping this turn: the window is full".into(),

        compaction: None,
    });
    a.apply(ServerFrame::Event(live));
    let after = a.screen(100, 30).join("\n");
    assert!(
        after.contains("context_wall"),
        "a warning that arrives live must still be drawn:\n{after}"
    );
    assert_eq!(a.notes.len(), 4);
    assert_eq!(a.notes_before(), 3, "only the snapshot's three are prior");
}

/// Second fault: routine is painted as failure. `compacted` and `auto_compact` are the session doing what
/// it should, and they arrived in the same red as a denial, so four notes read as a wall.
#[test]
fn a_routine_notice_is_not_drawn_in_the_failure_register() {
    const RED: &str = "\u{1b}[31m";
    let mut a = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    for (code, detail) in [
        // A routine code the head draws: `compacted` twins the fork's own row and is filed
        // beside the conversation, so the register this test is about would be off-screen.
        ("compact_half", "the fold read the first 90 items"),
        ("context_wall", "stopping this turn after 12 rounds"),
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
    let routine = line("compact_half");
    assert!(
        !routine.contains(RED),
        "a compaction was painted in the failure colour: {routine:?}"
    );
    assert!(
        routine.contains('·'),
        "not the routine register: {routine:?}"
    );
    assert!(
        !routine.contains('!'),
        "the alarm glyph is on a housekeeping line: {routine:?}"
    );
    let failure = line("context_wall");
    assert!(
        failure.contains(RED),
        "a wall must keep the red: {failure:?}"
    );
    assert!(
        failure.contains('!'),
        "a failure must keep the alarm glyph: {failure:?}"
    );
}

/// The border is not silent about a counter that has moved: a head that dropped events has a hole in its
/// transcript, and that must not wait for somebody to type a command.
#[test]
fn a_counter_that_has_moved_reaches_the_border_and_a_zero_one_does_not() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    assert_eq!(a.status_line(200), "");
    a.apply(ServerFrame::Resync {
        reason: "queue overflow".into(),
        dropped: 12,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    // The border says it with a triangle, pinned right - a fact that exists
    // only while it does, and never a resident sentence of bright yellow.
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains('⚠'), "{screen}");
    assert!(
        !screen.contains("dropped 12"),
        "the numbers are /status's, not the border's: {screen}"
    );
    // The unboxed composer - no border to pin a triangle to - names them on a
    // line of its own, read HERE, before the screen below acknowledges the alarm
    //.
    let border = a.status_line(200);
    assert!(border.contains("dropped 12"), "{border}");
    assert!(border.contains("resync 1"), "{border}");
    assert!(
        border.contains("/status"),
        "and says where the rest is: {border}"
    );
    // …where they keep their names and their counts.
    a.command("status");
    // A taller window, because the panel is taller now: a gloss names the profile the head is on, so several rows
    // gained a line of wrap and the counters below `dropped` sit past a 40-row screen.
    let stats = a.screen(120, 60).join("\n");
    let dropped_row = stats
        .lines()
        .find(|l| l.contains("dropped"))
        .expect("the dropped row is on the /status screen");
    assert!(dropped_row.contains("12"), "{dropped_row}");
    // The panel's own row and not the notice line: the composer's height ladder deletes the most expendable row
    // first, and at 24 rows the notice is the one that goes.
    let resync_row = stats
        .lines()
        .find(|l| {
            // Painted rows carry SGR codes before the label, so the shape is read off the
            // plain text - the same stripper the pane uses on a name.
            let plain = without_control_lines(l);
            plain.trim_start().starts_with("resync") && plain.chars().any(|c| c.is_ascii_digit())
        })
        .expect("the resync row is on the /status screen");
    assert!(resync_row.contains('1'), "{resync_row}");

    // And READING it acknowledged the alarm - the report, in the test that already had
    // the whole story of one counter arriving. The numbers stay on the screen (asserted
    // above, after the read), the mark goes, and the screen says what it did.
    assert!(
        stats.contains("acknowledged"),
        "the screen did not say what opening it just did: {stats}"
    );
    a.command("status"); // close it again
    let screen = a.screen(120, 24).join("\n");
    assert!(
        !screen.contains('⚠'),
        "the mark outlived the reading: {screen}"
    );
    assert_eq!(a.status_line(200), "", "nor on the unboxed path");

    // A counter that moves AFTER the reading is a new fact, and the mark comes back.
    // This is the half that makes acknowledging safe rather than a way to switch the alarm
    // off and forget it.
    a.apply(ServerFrame::Resync {
        reason: "queue overflow again".into(),
        dropped: 3,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    let screen = a.screen(120, 24).join("\n");
    assert!(
        screen.contains('⚠'),
        "a NEW incident did not raise the alarm again: {screen}"
    );
}

/// The acknowledgement is per counter and only up to what was read - the report's
/// *must not differ*, stated as arithmetic rather than through a screen.
#[test]
fn acknowledging_a_counter_silences_it_only_up_to_the_value_that_was_seen() {
    let none = Counters::default();
    // Nothing has moved: no alarm, and reading a clean screen acknowledges nothing.
    assert!(!none.exceeds(none));
    // One incident, and the alarm.
    let one = Counters {
        resyncs: 1,
        ..Counters::default()
    };
    assert!(one.exceeds(none), "the first resync is news");
    // Read it: acknowledged, and STILL not news - the second reading of the same number is
    // what a reader does not need a second time.
    assert!(!one.exceeds(one));
    // A resync after the one that was read is a different fact.
    let two = Counters {
        resyncs: 2,
        ..Counters::default()
    };
    assert!(two.exceeds(one), "the second resync is news again");
    // Per counter, not as a total. A counter whose value went DOWN is not news - an alarm
    // that fired on a decrease would be a mark nobody could ever clear - and a DIFFERENT
    // counter reaching a value this one already has is still its own news.
    assert!(!none.exceeds(one), "a decrease is not an incident");
    assert!(
        Counters {
            gaps: 1,
            ..Counters::default()
        }
        .exceeds(one),
        "`1 gap` is not silenced by having read `1 resync`"
    );
}

/// A frame this head cannot read is said, counted and not fatal: a head that exits takes the session down and
/// says nothing, and one that steps over it in silence leaves *this daemon is sending me something I do not
/// understand* indistinguishable from quiet.
#[test]
fn an_unreadable_frame_is_said_counted_and_survived() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // The bucket exists before anything lands in it. An absent field
    // and a zero field must not look the same, and a head that does not count
    // these at all is what an older head was.
    a.command("status");
    // Taller than 40 rows on purpose: `/status` is a scrolling pane and it gained a row, so a test asserting a row
    // is on the screen has to give the pane room for all of them.
    let zero = a.screen(120, 120).join("\n");
    let row = zero
        .lines()
        .find(|l| l.contains("unreadable"))
        .unwrap_or_else(|| panic!("/status has no bucket for this: {zero}"));
    assert!(row.contains('0'), "{row}");
    a.key(Key::Esc);
    assert_eq!(a.status_line(200), "", "a clean head has a clean border");

    // A daemon one version ahead sends a frame from the future.
    let before = (a.seq, a.rendered, a.filtered);
    a.unreadable(Unreadable {
        line: r#"{"frame":"peeked_v2","rows":[]}"#.into(),
        detail: "unknown variant `peeked_v2`, expected one of `hello`, `bye`".into(),
    });
    assert_eq!(a.unreadable, 1);
    // Nothing was parsed, so nothing is claimed. The read mark is untouched
    // and neither ack counter moved: `filtered` is "events I chose not to show",
    // and inventing a seq here would rewind the mark over frames already read.
    assert_eq!((a.seq, a.rendered, a.filtered), before);

    // The sentence is in the conversation: what it was, that this is almost always
    // a newer daemon, and that this head is still attached.
    // One line on the glass, the sentence whole in the popup.
    let screen = a.screen(120, 60).join("\n");
    assert!(screen.contains("unreadable_frame"), "{screen}");
    let screen = newest_notice(&mut a);
    assert!(screen.contains("peeked_v2"), "{screen}");
    assert!(screen.contains("cannot read"), "{screen}");
    assert!(screen.contains("newer"), "{screen}");
    assert!(
        screen.contains(&format!(
            "protocol {}",
            letibot_sessionlog::protocol::PROTOCOL_VERSION
        )),
        "{screen}"
    );

    // And the head is still running: the next frame applies as though nothing had
    // happened, which is the whole point of surviving one.
    a.apply(ServerFrame::Event(env(9, testing::turn_started("t1"))));
    assert!(a.turn_busy(), "the head kept working");

    // A second one counts twice, and the border names it now that it has moved.
    a.unreadable(Unreadable {
        line: r#"{"frame":"another"}"#.into(),
        detail: "unknown variant `another`".into(),
    });
    assert_eq!(a.unreadable, 2);
    let border = a.status_line(200);
    assert!(border.contains("unreadable 2"), "{border}");
    assert!(border.contains("/status"), "{border}");
    a.command("status");
    // Taller than a screen for the same reason: `/status` is a scrolling pane and it has gained rows since this
    // test was written, so the pane needs room for all of them.
    let stats = a.screen(120, 140).join("\n");
    let unreadable_rows: Vec<&str> = stats.lines().filter(|l| l.contains("unreadable")).collect();
    assert!(
        unreadable_rows.iter().any(|l| l.contains('2')),
        "{unreadable_rows:?}"
    );
}

/// A head that outlives its daemon notices, says so and re-asks: three heads were alive with ages in days while
/// the daemon had been replaced hours before, and the registry is in memory, so a daemon that comes back is not the
/// one whose answers are on the screen.
#[test]
fn a_replaced_daemon_is_noticed_said_and_re_asked() {
    let hub = Hub::new("s");
    // The daemon this head attached to.
    let mut a = app();
    a.set_daemon_pid(Some(4242));
    a.apply(hello("s", a_family(), hub.snapshot()));
    assert!(
        !a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "daemon_replaced")),
        "the first attach has nothing to compare against: {:?}",
        a.notes
    );
    a.take_actions();

    // A switch on the same socket, by the same process, is not a replacement. This is the
    // half that keeps the note from firing on every keystroke-shaped `Hello`.
    a.apply(hello("s", a_family(), hub.snapshot()));
    assert!(
        !a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "daemon_replaced")),
        "the same daemon answered the same session twice: {:?}",
        a.notes
    );

    // The socket died and the driver came back to a different process. `head.rs` re-reads
    // `SO_PEERCRED` on the new connection before the `Hello` is folded - that is the caller's
    // order and this test's, because the arm reads `self.daemon_pid`.
    a.link_down("the daemon closed the connection");
    a.reconnect_sent();
    a.set_daemon_pid(Some(5151));
    a.apply(hello("s", a_family(), hub.snapshot()));

    let said = a
        .notes
        .iter()
        .find_map(|(_, n)| match n {
            Note::Warned(w) if w.code == "daemon_replaced" => Some(w.detail.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a different daemon went unremarked: {:?}", a.notes));
    assert!(
        said.contains("pid 4242"),
        "it does not name the one that went: {said}"
    );
    assert!(
        said.contains("pid 5151"),
        "nor the one that arrived: {said}"
    );
    assert!(
        said.contains("in memory"),
        "it does not say WHY the old answers are no longer good: {said}"
    );

    // And the refetch ran, which is the other half of the requirement: the head does not
    // keep drawing the old picture, it asks again. The same three reads as any seating.
    let asked = a.take_actions();
    for want in [Action::ListSessions, Action::ListJobs, Action::Settings] {
        assert!(
            asked.contains(&want),
            "a head under a new daemon did not re-ask for {want:?}: {asked:?}"
        );
    }

    // A rebuild that moved the wire is a replacement too, even at the same pid - which is
    // the reason the identity is a pair rather than the number alone.
    let mut b = app();
    b.set_daemon_pid(Some(4242));
    b.apply(hello_at(
        "s",
        a_family(),
        hub.snapshot(),
        letibot_sessionlog::protocol::PROTOCOL_VERSION,
    ));
    b.apply(hello_at(
        "s",
        a_family(),
        hub.snapshot(),
        letibot_sessionlog::protocol::PROTOCOL_VERSION + 1,
    ));
    assert!(
        b.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "daemon_replaced")),
        "the same pid speaking a different protocol is still a different daemon: {:?}",
        b.notes
    );
}

/// A pid the kernel would not name is said as such, not as a number. The rule on the
/// farewell, kept on the other sentence that names a pid: a head that printed a zero would
/// send the operator to `ps` for a process that is not there.
#[test]
fn a_replacement_between_two_unnamed_peers_says_so_rather_than_inventing_a_pid() {
    let hub = Hub::new("s");
    let mut a = app();
    // No `set_daemon_pid` at all: the kernel declined to name the peer.
    a.apply(hello_at(
        "s",
        a_family(),
        hub.snapshot(),
        letibot_sessionlog::protocol::PROTOCOL_VERSION,
    ));
    a.apply(hello_at(
        "s",
        a_family(),
        hub.snapshot(),
        letibot_sessionlog::protocol::PROTOCOL_VERSION - 1,
    ));
    let said = a
        .notes
        .iter()
        .find_map(|(_, n)| match n {
            Note::Warned(w) if w.code == "daemon_replaced" => Some(w.detail.clone()),
            _ => None,
        })
        .expect("the protocol moved, so the daemon did");
    assert!(
        said.contains("the kernel did not name"),
        "an unnamed peer was rendered as a pid: {said}"
    );
    assert!(!said.contains("pid 0"), "a zero is not a pid: {said}");
}

/// A named fill's line is the daemon's count and not a count of unfilled rows: counting the rows still lacking a
/// body names the wrong operation, since an ordinary reply is not an import, and draws a number that is a
/// rendering of the fact rather than the fact.
#[test]
fn an_import_draws_the_importers_own_counter_and_clears_when_it_is_done() {
    let mut a = app();
    a.clock(1_000);
    let tick = |done: u64| SessionEvent::Filling {
        what: "importing an opencode conversation".into(),
        unit: "parts".into(),
        done,
        total: 9_570,
    };
    assert!(
        !a.screen(100, 30).join("\n").contains("parts"),
        "no fill line before one is running"
    );

    a.apply(ServerFrame::Event(env(1, tick(0))));
    let start = a.screen(100, 30).join("\n");
    assert!(start.contains("0/9570 parts"), "{start}");
    assert!(
        start.contains("Importing an opencode conversation"),
        "{start}"
    );

    a.apply(ServerFrame::Event(env(2, tick(4_790))));
    let mid = a.screen(100, 30).join("\n");
    assert!(mid.contains("4790/9570 parts"), "{mid}");

    // The last tick clears the line. The daemon's durable finish note is what
    // says the fill finished, and a bar left at `total of total` would sit on the
    // screen for ever.
    a.apply(ServerFrame::Event(env(3, tick(9_570))));
    let done = a.screen(100, 30).join("\n");
    assert!(
        !done.contains("parts"),
        "the line goes when the fill is done: {done}"
    );
}

/// The file is edited while the pane is open, which is the one case an open-time read cannot see: one `stat` per
/// draw and not a watcher thread, which would need a descriptor and a way into a head built around one loop.
#[test]
fn the_pane_notices_the_file_changing_under_it() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    let path = dir.join("TODO.md");
    std::fs::write(&path, "## Phase 0\n\n- [ ] **T1** open\n").expect("write");

    let mut a = app();
    a.wiring.workspace = dir.display().to_string();
    a.key(Key::CtrlT);
    assert!(a.screen(110, 40).join("\n").contains("[ ] Phase 0  [0/1]"));

    // The operator ticks it off in their editor, with the pane still up.
    // The length changes, so a second-granularity mtime cannot hide it.
    std::fs::write(&path, "## Phase 0\n\n- [x] **T1** open, now done\n").expect("write");
    let screen = a.screen(110, 40).join("\n");
    assert!(
        screen.contains("[x] Phase 0  [1/1]"),
        "the next draw sees it: {screen}"
    );

    // And an unchanged file is not re-read: the stat is the point of the
    // cache, not a decoration on it.
    let before = a.repo_todos_at;
    a.screen(110, 40);
    assert_eq!(a.repo_todos_at, before);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_rejection_shows_both_sequence_numbers() {
    let mut a = app();
    a.apply(ServerFrame::Rejected {
        client_request_id: "c1".into(),
        reason: "stale expected_seq".into(),
        expected_seq: 12,
        actual_seq: 40,
    });
    // On its own line, above the composer - never *instead* of the composer,
    // as an older layout had it.
    let screen = a.screen(200, 12);
    let joined = screen.join("\n");
    assert!(joined.contains("12") && joined.contains("40"), "{joined}");
    let (row, _) = a.cursor().unwrap();
    assert!(
        screen[row].contains('›'),
        "the composer survived the notice: {:?}",
        screen[row]
    );
}

#[test]
fn a_notice_does_not_outlive_its_welcome_or_hide_the_input() {
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Accepted {
        client_request_id: "c1".into(),
        seq: 3,
        note: "stale expected_seq: queued anyway as a follow-up user item".into(),
    });
    assert!(a.screen(80, 12).join("\n").contains("queued anyway"));
    // Frames do not age it: two hundred repaints at the same millisecond leave the sentence where it was, which is
    // the point of moving the clock onto the wall. As a count of loop passes it was 6 s on a busy head and forever
    // on one that went quiet.
    for _ in 0..200 {
        a.screen(80, 12);
    }
    assert!(
        a.screen(80, 12).join("\n").contains("queued anyway"),
        "200 repaints is not a duration"
    );
    a.clock(1_000 + NOTICE_MS);
    assert!(
        !a.screen(80, 12).join("\n").contains("queued anyway"),
        "a notice that never expires becomes furniture"
    );
}

/// A notice expires in TIME, on this head's clock; the second
/// symptom is the mirror of leticl's `a-notice-expires-and-an-alarm-does-not`.
#[test]
fn a_notice_expires_in_time_and_not_in_frames() {
    let mut a = app();
    a.clock(1_000);
    a.say("hello");
    assert_eq!(
        a.notice_until,
        Some(1_000 + NOTICE_MS),
        "the clock started, at the deadline — not a countdown"
    );
    assert!(a.screen(80, 12).join("\n").contains("hello"));
    // 200 repaints at one millisecond: a frame is a rendering of time, not a unit of it.
    for _ in 0..200 {
        a.screen(80, 12);
    }
    assert!(
        a.screen(80, 12).join("\n").contains("hello"),
        "frames do not age a notice"
    );
    // **The boundary, and not \"eventually\": present a millisecond before the
    // deadline, gone the millisecond it is due - and never `screen()` in between, so
    // the only thing that moved is the clock.
    a.clock(1_000 + NOTICE_MS - 1);
    assert!(
        a.screen(80, 12).join("\n").contains("hello"),
        "still there a millisecond before"
    );
    a.clock(1_000 + NOTICE_MS);
    assert!(
        !a.screen(80, 12).join("\n").contains("hello"),
        "gone the millisecond it is due"
    );
    assert_eq!(a.notice_until, None, "and its clock stopped with it");
    // A key is the acknowledgement, and it moves the deadline rather than the text:
    // the sentence survives the keypress and is taken down by the frame that follows
    // it, which is the behaviour the frame-count version had.
    a.clock(2_000);
    a.say("again");
    let _ = a.key(Key::Char('x'));
    assert!(
        a.notice.is_some(),
        "still there until the frame it is drawn on"
    );
    assert!(
        !a.screen(80, 12).join("\n").contains("again"),
        "and the next frame takes it down"
    );
}

/// A fleeting notice goes when its time is up and a failure's does not: the row is drawn for a duration, so a
/// queue's standings do not stick and jump as replies arrive, and a failure stays until it is retired.
#[test]
fn a_fleeting_notice_goes_after_its_time_and_a_failure_does_not() {
    let mut a = app();
    a.session_id = "s1".into();
    a.clock(1_000);
    for (seq, code, detail) in [
        (
            1,
            "merge_queued",
            "`agent/child-one` is in the merge queue as `c1`",
        ),
        (
            2,
            "merge_not_queued",
            "the store would not open, so nothing will land it",
        ),
    ] {
        a.apply(ServerFrame::Event(env_at(
            seq,
            1_000,
            SessionEvent::Warning {
                code: code.into(),
                detail: detail.into(),
                compaction: None,
            },
        )));
    }
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("is in the merge"), "{screen}");
    assert!(screen.contains("the store would not open"), "{screen}");

    // Frames do not age it.
    for _ in 0..200 {
        a.screen(100, 24);
    }
    assert!(
        a.screen(100, 24).join("\n").contains("is in the merge"),
        "200 repaints is not a duration"
    );

    // The boundary, and not "eventually".
    a.clock(1_000 + FLEETING_MS - 1);
    assert!(
        a.screen(100, 24).join("\n").contains("is in the merge"),
        "still there a millisecond before"
    );
    a.clock(1_000 + FLEETING_MS);
    let screen = a.screen(100, 24).join("\n");
    assert!(
        !screen.contains("is in the merge"),
        "gone the millisecond it is due: {screen}"
    );
    assert!(
        screen.contains("the store would not open"),
        "a failure is not on a timer, and the class table is what keeps it off one: {screen}"
    );

    // Spent, not retired. The sentence leaves this head rather than joining the reader's
    // own retired set - a timer may not write `head.toml`, because a key the reader restored
    // would then be re-retired by the clock on the next frame.
    assert_eq!(
        a.notes.len(),
        1,
        "one note left, and it is the failure: {:?}",
        a.notes
    );
    assert!(
        a.dismissed.is_empty(),
        "a timer does not write the reader's file: {:?}",
        a.dismissed
    );
}

/// One announcement is noted once, wherever it arrives from: `adopt` plants what the snapshot carries at anchor 0
/// and the live arm anchors at the current end, so the same `Warning` reached a head twice and the second copy
/// read as pinned under the conversation.
#[test]
fn one_warning_delivered_twice_is_noted_once() {
    let hub = letibot_sessionlog::hub::Hub::new("s");
    for i in 0..12 {
        hub.publish(letibot_sessionlog::SessionEvent::TranscriptAppended {
            item_id: format!("r{i}"),
            kind: "user".into(),
            ledger_head: String::new(),
            item: None,
            body_bytes: 0,
        });
        hub.record_item(
            &format!("r{i}"),
            letibot_transcript::TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: format!("row r{i}"),
                }],
            },
        );
    }
    let published = hub.publish(letibot_sessionlog::SessionEvent::Warning {
        code: "reseated".into(),
        detail: "THE-WARNING".into(),

        compaction: None,
    });

    let att = hub.attach("tui", "d", letibot_sessionlog::protocol::Caps::default(), 0);
    let snap = att.snapshot.clone().expect("a snapshot");
    let carried = snap
        .warnings
        .iter()
        .find(|w| w.detail == "THE-WARNING")
        .expect("the premise: the snapshot carries the warning");
    assert_eq!(
        carried.ts, published.ts,
        "the premise: one announcement has ONE ts, whichever route it takes"
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
    // And now the same envelope again, through the live arm - a redelivery.
    a.apply(ServerFrame::Event(published.clone()));

    assert_eq!(
        a.notes
            .iter()
            .filter(|(_, n)| matches!(n, Note::Warned(w) if w.detail == "THE-WARNING"))
            .count(),
        1,
        "one announcement, one note: {:?}",
        a.notes
    );
    // One announcement, one note, and it stays where it arrived: it came in the snapshot, so it is not a seam of
    // this conversation, and a redelivery does not move it to a seam at the end of the transcript.
    let (place, _) = a
        .notes
        .iter()
        .find(|(_, n)| matches!(n, Note::Warned(w) if w.detail == "THE-WARNING"))
        .expect("the note");
    assert_eq!(
        *place,
        Placed::Before,
        "the snapshot's kind is kept, not the replay's seam"
    );
}

#[test]
fn a_failed_turn_stops_the_spinner_instead_of_being_a_warning_and_a_hang() {
    // The engine published a
    // `Warning` and no terminal event, `TurnState` stayed `Running`, and the
    // head span at a dead session for as long as anyone left it open.
    let mut a = app();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    assert!(a.turn_busy());
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TurnFailed {
            turn_id: "t1".into(),
            error: "http io: Connection refused (os error 111)".into(),
            partial_kept: false,
        },
    )));
    assert!(!a.turn_busy(), "the turn is still marked running");
    assert!(a.turn_status(120).is_empty(), "the spinner is still there");
    let screen = a.screen(120, 16).join("\n");
    assert!(screen.contains("FAILED"), "{screen}");
    assert!(screen.contains("Connection refused"), "{screen}");
    // …and the daemon's grep-able warning is not the same sentence a second
    // time three lines away. Filtered, and counted as filtered.
    let before = a.filtered;
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::Warning {
            code: "turn_failed".into(),
            detail: "http io: Connection refused (os error 111)".into(),

            compaction: None,
        },
    )));
    assert_eq!(a.filtered - before, 1);
    assert_eq!(
        a.screen(120, 16)
            .join("\n")
            .matches("Connection refused")
            .count(),
        1,
        "the same failure is on the screen twice"
    );
}

/// The diagnostic moves to the edge and does not stop existing: `model_slow_first_byte` is worth a yellow
/// triangle and not a row in the conversation, and both halves have to hold at once, since either alone is a
/// defect.
#[test]
fn an_edge_bound_warning_raises_the_alarm_and_never_becomes_a_row() {
    let mut a = App::new(plain_cfg(80));
    assert_eq!(a.slow_first_byte, 0);
    assert!(!a.alarmed(), "the premise: a clean head has a clean edge");

    let said = "4210ms to the first byte (the provider, not the turn)";
    assert_eq!(
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Warning {
                code: "model_slow_first_byte".into(),
                detail: said.into(),
                compaction: None,
            },
        ))),
        // `Filtered` and not `Control` or `Rendered`: it is an event in the record, read and counted in the number
        // that says what this head chose not to show, and it is not a row.
        Disposition::Filtered,
        "an edge-bound note is read and not drawn, and it is counted as filtered"
    );
    let screen = a.screen(80, 24).join("\n");
    assert!(
        !screen.contains(said),
        "the sentence is in the conversation, which is the ruling reversed:\n{screen}"
    );
    // And it is still there. The count moved, the triangle is up, and the reader is one
    // verb from the number.
    assert_eq!(
        a.slow_first_byte, 1,
        "the diagnostic was dropped, not moved"
    );
    assert!(
        a.alarmed(),
        "a diagnostic on the edge must raise the ⚠, or nothing points at it"
    );
    assert!(
        screen.contains('⚠'),
        "the edge says nothing while a counter has moved:\n{screen}"
    );

    // `/status`: the row is named, it is countable, and it carries the daemon's own code  -
    // because moving the note off the screen took away the one place that spelling
    // appeared, and the code is the word a reader greps the session log for.
    a.command("status");
    // Tall, because `/status` is a scrolling pane and this row sits near its end.
    let stats = a.screen(120, 96).join("\n");
    let row = stats
        .lines()
        .find(|l| l.trim_start().starts_with("first byte"))
        .unwrap_or_else(|| panic!("`/status` does not carry the counter: {stats}"));
    assert!(
        row.contains('1'),
        "the row does not carry the count: {row:?}"
    );
    assert!(
        stats.contains("model_slow_first_byte"),
        "the code the reader greps the log for is not on the screen at all:\n{stats}"
    );

    // The control: a code that changes the conversation is still a row in it. Without
    // this, a head that swallowed every warning would pass everything above.
    let mut b = App::new(plain_cfg(80));
    assert_eq!(
        b.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Warning {
                code: "workspace_gone".into(),
                detail: "this session's workspace is not there any more: \
                         /home/dead/Projects/gone. Every call in it refuses."
                    .into(),
                compaction: None,
            },
        ))),
        Disposition::Rendered,
        "a daemon fact the model was not told is a row of the conversation"
    );
    let drawn = b.screen(80, 24).join("\n");
    assert!(
        drawn.contains("workspace_gone"),
        "the sentence is not drawn: {drawn}"
    );
    // And a twin of a model row is NOT one: the fold's own row is the model's, and the warning is
    // beside it. See `warning::TWIN`.
    let mut c = App::new(plain_cfg(80));
    assert_eq!(
        c.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Warning {
                code: "compacted".into(),
                detail: "compacted: 940188 → 9181 tokens, kept as a 9000-character summary, \
                         nothing dropped, on transcript s#t25."
                    .into(),
                compaction: None,
            },
        ))),
        Disposition::Filtered,
        "a compaction's row is the model's own summary row"
    );
    assert_eq!(b.slow_first_byte, 0, "neither is the weather");
    assert!(
        !b.alarmed(),
        "and it raises no triangle, because nothing is wrong"
    );
}

/// An edge-bound warning is moved, not deleted - and the two doors agree. The live arm must not filter the code
/// before filing, or the sentence survives only as a `/status` count while the snapshot arm files it: a head that
/// attached later would list the diagnostic under `/notes` and one present at the time would not.
#[test]
fn an_edge_bound_warning_is_listed_in_notes_rather_than_deleted() {
    let mut a = App::new(plain_cfg(80));
    let said = "4210ms to the first byte (the provider, not the turn)";
    assert_eq!(
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Warning {
                code: "model_slow_first_byte".into(),
                detail: said.into(),
                compaction: None,
            },
        ))),
        Disposition::Filtered,
        "the premise: it is not a row, so nothing below is about the transcript"
    );

    // Held, and held BESIDE the conversation. `Placed::Before` is what makes both halves true at
    // once: `/notes` lists every note whatever its placement, and the walk draws only a seam.
    let (place, _) = a
        .notes
        .iter()
        .find(|(_, n)| matches!(n, Note::Warned(w) if w.detail == said))
        .unwrap_or_else(|| panic!("the diagnostic was dropped, not moved: {:?}", a.notes));
    assert_eq!(
        *place,
        Placed::Before,
        "an edge-bound sentence is filed beside the conversation, not at a seam in it"
    );
    assert_eq!(
        a.slow_first_byte, 1,
        "and it is still counted: the edge points at the number"
    );

    // Where the operator reads it, the half the live arm must not lose.
    let listed = a.notes_lines().join("\n");
    assert!(
        listed.contains(said),
        "`/notes` does not carry the diagnostic, so the count is all that is left of it:\n{listed}"
    );
    // And `/status` says why it is not on the screen, in words that are true of an edge-bound note.
    a.command("status");
    let stats = a.screen(120, 96).join("\n");
    assert!(
        stats.contains("1 not drawn"),
        "`/status` must say what it is holding and not drawing:\n{stats}"
    );
}

/// The two silences are different facts, and a head that said one sentence for
/// both sent the operator looking for a command that had not started.
#[test]
fn ctrl_o_tells_a_generating_model_apart_from_an_idle_session() {
    let mut a = app();
    // Nothing at all.
    assert_eq!(a.key(Key::CtrlO), None);
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("nothing is running"),
        "{:?}",
        a.notice
    );

    // A turn running, but no call yet: the model is still generating.
    a.apply(ServerFrame::Event(env(0, testing::turn_started("t1"))));
    assert_eq!(a.key(Key::CtrlO), None);
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("still working"), "{said}");
    assert!(said.contains("no command running"), "{said}");
}

/// The no-progress guard's warning leaves a resident state and not a note: the finding is true until the session
/// is replaced or the window raised, and the warning that announced it scrolls away with the conversation.
#[test]
fn the_no_progress_warning_leaves_a_resident_line_and_a_status_row() {
    let hub = Hub::new("s");
    let mut a = app();
    a.clock(1_000);
    let e = hub.publish(SessionEvent::Warning {
        code: "auto_compact_no_progress".into(),
        detail: "compacted from 1849499 to 1530411 tokens and that is STILL within \
                 62500 of the 1000000 window, so automatic compaction is now off \
                 for this session rather than looping once per turn. The summary \
                 itself is near the wall: start a fresh session, or raise \
                 --context-window if the server really has more."
            .into(),
        compaction: None,
    });
    a.apply(ServerFrame::Event(e));

    // The resident line, above the composer, saying both halves: it is off,
    // AND a refused turn is still compacted.
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("auto-compaction is off"),
        "no resident line for the guard's finding: {screen}"
    );
    assert!(
        screen.contains("still compacted"),
        "the line keeps the pre-emptive and refused doors apart: {screen}"
    );

    // And `/status` carries it as a state row beside `session` and `head`, with
    // the daemon's own why as the gloss.
    a.command("status");
    let status = a.screen(120, 100).join("\n");
    assert!(
        status.contains("auto-compact") && status.contains("auto-compaction is off"),
        "no auto-compact row on /status: {status}"
    );
}

/// The sentence a fold writes into `compacted`: the daemon's own words from `sessions::compaction_said`, on the
/// path where nobody watched the summary turn being written, with the whole record pasted into the detail.
fn a_compacted_said() -> String {
    format!(
        "compacted: 958397 → 100005 tokens, on transcript s#t25. Nothing of the summary turn \
         reached this screen — it ran over a scratch transcript — so here is what the model now \
         reads in place of everything before it:\n\n## Goal\n\n{}",
        "THE MODEL'S OWN RECORD ".repeat(20)
    )
}

/// A compaction is one line in the rungs that show the conversation alone: `conversation` and `read-edits` drew a
/// wall of noise for it, and the operator asked for one line that says what happened.
#[test]
fn a_compaction_is_one_line_in_the_two_rungs_the_operator_named() {
    for profile in [Profile::CONVERSATION, Profile::READ_EDITS] {
        let mut a = app();
        a.visibility = Visibility::of(profile);
        a.apply(hello(
            "s",
            vec![brief("s", "one", false)],
            Hub::new("s").snapshot(),
        ));
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Warning {
                code: "compacted".into(),
                detail: a_compacted_said(),
                compaction: None,
            },
        )));
        let frame = a.screen(100, 30);
        let screen = frame.join("\n");
        let carrying = |needle: &str| frame.iter().filter(|l| l.contains(needle)).count();
        // `compacted` twins the fork's own `System` row, so it is filed beside the conversation:
        // no row of its own, at any rung. The fold's one row is the model's record, above.
        assert_eq!(
            carrying("958k → 100k tokens"),
            0,
            "the twinned warning drew a row at `{}`:\n{screen}",
            profile.name
        );
        // And nothing was swallowed: the daemon's whole sentence, the model's record and all,
        // is on `/notes` - and so is the note's own detail, verbatim.
        a.command("notes");
        let listing = a.screen(100, 60).join("\n");
        assert!(
            listing.contains("compacted"),
            "a compaction left no note at `{}`:\n{listing}",
            profile.name
        );
        let whole = a
            .notes
            .iter()
            .find_map(|(_, n)| match n {
                Note::Warned(w) if w.code == "compacted" => Some(w.detail.clone()),
                _ => None,
            })
            .expect("the compaction is held beside the conversation");
        assert!(
            whole.contains("958397 → 100005 tokens"),
            "the note lost the figures: {whole}"
        );
        assert!(
            whole.contains("THE MODEL'S OWN RECORD"),
            "the note lost the record: {whole}"
        );
        assert!(
            whole.contains("ran over a scratch transcript"),
            "the note lost the daemon's sentence: {whole}"
        );
    }
}

/// The fold is a property of the note and not of a rung, and the fork's own row does not vanish above them: a
/// rung-gated fold would retroactively erase a warning already read, so the compaction is one line at every rung.
#[test]
fn a_compaction_is_one_line_at_every_rung_and_the_forks_row_still_draws() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::LOUD);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Warning {
            code: "compacted".into(),
            detail: a_compacted_said(),
            compaction: None,
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("s#t25.0", "system"),
    )));
    a.record_item(
        "s#t25.0",
        TranscriptItem::System {
            text: format!(
                "This conversation was compacted: what was said before this point is replaced \
                 by the summary below, which was written over the full history of transcript \
                 s#t24 and proposed no tool calls.\n\n{}",
                "THE MODEL'S OWN RECORD ".repeat(20)
            ),
            origin: letibot_transcript::SystemOrigin::Update,
        },
    );
    let frame = a.screen(100, 40);
    let screen = frame.join("\n");
    assert_eq!(
        frame
            .iter()
            .filter(|l| l.contains("958k → 100k tokens"))
            .count(),
        0,
        "the twinned warning drew its own row at `loud`:\n{screen}"
    );
    // The fork's row is drawn, as ONE line (a System row was drawn whole, up to 190
    // screen rows of the model's own record), and a click - or alt-n - opens it whole.
    assert!(
        screen.contains("system · compacted summary ("),
        "the fork's own row vanished from the rung that draws what the head was given:\n{screen}"
    );
    assert!(!screen.contains("THE MODEL'S OWN RECORD"), "{screen}");
    assert!(
        newest_notice(&mut a).starts_with("This conversation was compacted"),
        "the popup holds the row whole"
    );
}

/// A warning in the same window still draws, whole: a head that folds a warning too eagerly is a head whose
/// failures nobody reads, so a compaction is folded and a chain mismatch beside it is not, on one screen.
#[test]
fn a_warning_beside_a_compaction_still_draws_whole() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    for (seq, code, detail) in [
        (1u64, "compacted", a_compacted_said()),
        (
            2,
            // A Failure that asks a move: the ledger's own codes are developer diagnostics and do
            // not take a row at all (`warning::is_developer`).
            "transcript_store",
            "row 41's head is not the one that was stored, so every row after it is \
             unverifiable: the chain broke at 41 and the store says nothing about how."
                .into(),
        ),
    ] {
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::Warning {
                code: code.into(),
                detail,
                compaction: None,
            },
        )));
    }
    let frame = a.screen(100, 30);
    let screen = frame.join("\n");
    let mismatch: Vec<&String> = frame
        .iter()
        .filter(|l| l.contains("transcript_store"))
        .collect();
    assert_eq!(mismatch.len(), 1, "the failure has its own row:\n{screen}");
    // Its register and its first clause are on the row, and its sentence is a click away
    //: one line a notice, a failure included - the mark and the red say what it is.
    assert!(
        mismatch[0].contains("! transcript_store · row 41's head is not the one"),
        "{screen}"
    );
    assert!(
        newest_notice(&mut a).contains("unverifiable"),
        "and its sentence is in the popup, not folded away"
    );
    assert!(
        !screen.contains("THE MODEL'S OWN RECORD"),
        "the compaction beside it is still one line:\n{screen}"
    );
}

/// The stale-prefix notice lands where the person already looks: the daemon's half is `Harness::open`'s, and this
/// is the half that decides whether anybody reads it, as an ordinary warning on the session's own log.
#[test]
fn a_stale_prefix_is_drawn_as_a_note_row_and_not_as_a_failure() {
    let detail = "session s-1789462738453908838 was seated 10-08 21:14 with a prompt this \
                  daemon would not compose now: the tool schemas have changed since. \
                  `/reseat` rebuilds the prompt from what is seated now, carrying this \
                  conversation across as it is; a new session gets the seated one from \
                  the start.";
    let hub = Hub::new("s");
    // A turn first, so the head is not drawing its empty-session placeholder - notes
    // live in the transcript's margin, and a session that has said nothing has no margin
    // to draw them in. Same preamble as `ctrl_n_retires_every_note_and_keeps_every_note`.
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    // It is live and not in the snapshot: a note that comes with a snapshot is not planted, so the delivery
    // that matters is the one a head attached to a running session gets.
    let e = hub.publish(SessionEvent::Warning {
        code: "prefix_stale".into(),
        detail: detail.into(),
        compaction: None,
    });
    a.apply(ServerFrame::Event(e));

    assert_eq!(
        letibot_sessionlog::warning::class("prefix_stale"),
        letibot_sessionlog::warning::Class::Routine,
        "the register is the whole of what a reader takes from the colour"
    );
    // One line now - the operator, of the wall this note
    // drew: *"these gray walls are hilarious … make them the usual one liners"*
    // (`stale_card`). The remedy is the card's (`alt-r`), and the sentence is one click away.
    let screen = a.screen(120, 40).join("\n");
    let row = screen
        .lines()
        .find(|l| l.contains("prompt stale · seated 10-08 21:14"))
        .unwrap_or_else(|| panic!("the notice is not on a note row at all:\n{screen}"));
    assert!(
        row.contains("tool schemas changed · click for details"),
        "what moved, and how to read the rest: {row:?}"
    );
    assert!(!row.contains('!'), "never the failure mark: {row:?}");
    let (y, _) = a.card_rows[0];
    a.key(Key::Click { x: 10, y: y as u16 });
    assert!(
        super::redcard::popup_text(&mut a).contains("`/reseat`"),
        "and the remedy is in the words one click away, not only in the daemon's event"
    );
    // A row, not a counter on the alarm. `ALARM_ONLY` is the list of codes that belong
    // on the triangle; this one is a fact about the conversation the operator is in, and a
    // reader who never opens `/status` is exactly the reader it is for.
    assert!(
        !letibot_sessionlog::warning::to_the_alarm("prefix_stale"),
        "a stale prefix changes what the model can call, so it belongs in the record"
    );
}

// ─────────────── a dismissed notice stays dismissed in every view ───────────────

/// A run folded into one counter, at the end of a conversation too long to walk whole - the
/// operator's frame: `[2 tool calls]` above the composer edge, and the notices he cleared above
/// the live pane. The filler is load-bearing: a short conversation never enters the tail walk
/// (`hist_floor` stays 0), so a test built on one pins nothing.
fn a_run_with_notices() -> App {
    let mut a = app();
    a.walk_limit = 1;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let mut seq = 0u64;
    // Longer than `room + TAIL_SLACK`, so the window really does start mid-conversation.
    for i in 0..120u64 {
        seq += 1;
        let id = format!("s.f{i}");
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(&id, "assistant"),
        )));
        seq += 1;
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptContent {
                item_id: id.clone(),
                item: Box::new(TranscriptItem::Assistant {
                    text: format!("earlier reply {i}"),
                    tool_calls: Vec::new(),
                    truncated: false,
                }),
            },
        )));
    }
    seq += 1;
    a.apply(ServerFrame::Event(env(
        seq,
        testing::appended("s.0", "assistant"),
    )));
    seq += 1;
    a.apply(ServerFrame::Event(env(
        seq,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "here is what I am about to do:".into(),
                tool_calls: vec![
                    letibot_transcript::ToolCall {
                        id: "c0".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"crates/tui/src/app.rs"}"#.into(),
                    },
                    letibot_transcript::ToolCall {
                        id: "c1".into(),
                        name: "grep".into(),
                        arguments: r#"{"pattern":"ctrl-t"}"#.into(),
                    },
                ],
                truncated: false,
            }),
        },
    )));
    for (i, (id, call, name)) in [("s.1", "c0", "read"), ("s.2", "c1", "grep")]
        .into_iter()
        .enumerate()
    {
        seq += 1;
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(id, "tool_result"),
        )));
        seq += 1;
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: call.into(),
                    name: name.into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!("CONTENTS-{i}"),
                    edits: Vec::new(),
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    // The notices, at three instants - explicit stamps, because a mark is an ORDER and two
    // publishes are not guaranteed to land in different milliseconds.
    for (i, ts) in [(1u64, 1_000u64), (2, 2_000), (3, 3_000)] {
        seq += 1;
        a.apply(ServerFrame::Event(env_at(
            seq,
            ts,
            SessionEvent::Warning {
                code: "model_endpoint_retry".into(),
                detail: format!("the model endpoint did not answer: attempt {i} of 5"),
                compaction: None,
            },
        )));
    }
    a.visibility = Visibility::of(Profile::CONVERSATION);
    // One frame first: it settles `marker_facts`, whose change invalidates the history and would
    // drop the tail back to a forward walk with no floor.
    a.screen(200, 40);
    a.invalidate_history();
    a
}

/// Where `needle` starts on the screen, as (x, y) - the frames these tests read are the head's own.
fn where_at(screen: &[String], needle: &str) -> (u16, u16) {
    for (y, l) in screen.iter().enumerate() {
        let cells: String = crate::app::keyhints::cells(l).into_iter().collect();
        if let Some(x) = cells.find(needle) {
            return (cells[..x].chars().count() as u16, y as u16);
        }
    }
    panic!("{needle:?} is not on the frame:\n{}", screen.join("\n"));
}

/// A notice whose row is not in the window is not drawn, which is the wall coming back at the door that drew it: a
/// tail walk renders the last N rows and `hist_upto` then stands at the end, so every seam is at or before it.
#[test]
fn a_notice_whose_row_is_above_the_window_is_not_drawn() {
    let mut a = app();
    // Forced tail mode: the head renders the end of the conversation and leaves the rest.
    a.walk_limit = 1;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let mut seq = 0u64;
    for i in 0..60u64 {
        let id = format!("s.{i}a");
        seq += 1;
        a.apply(ServerFrame::Event(env(
            seq,
            testing::appended(&id, "assistant"),
        )));
        seq += 1;
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptContent {
                item_id: id.clone(),
                item: Box::new(TranscriptItem::Assistant {
                    text: format!("reply {i}"),
                    tool_calls: Vec::new(),
                    truncated: false,
                }),
            },
        )));
        // One notice at the top of the conversation - its row is far above any window.
        if i == 1 {
            seq += 1;
            a.apply(ServerFrame::Event(env_at(
                seq,
                1_000,
                SessionEvent::Warning {
                    code: "model_endpoint_retry".into(),
                    detail: "the model endpoint did not answer: attempt 1 of 5".into(),
                    compaction: None,
                },
            )));
        }
    }
    let screen = a.screen(120, 40).join("\n");
    assert!(
        a.hist_floor > 0,
        "the premise, a tail walk, not a whole one (floor {}, upto {})",
        a.hist_floor,
        a.hist_upto
    );
    assert!(
        !screen.contains("model_endpoint_retry"),
        "a notice whose row is above the window was drawn anyway:\n{screen}"
    );
    // Held, not dropped, and not retired: it is out of this window, which is a third answer.
    assert_eq!(a.notes.len(), 1, "the note is gone, not out of the window");
    assert!(
        !a.is_retired(&a.notes[0].1),
        "the fixture retired the note, so it proves nothing"
    );
    // A note the walk steps over is hidden by the window, so `/status` must count it.
    assert_eq!(
        a.notes_before(),
        1,
        "a note above the window is counted as live: {} held, {} retired",
        a.notes.len(),
        a.retired_notes()
    );

    // And a notice at the END of the conversation is drawn - the rule is the seam, not the
    // silence: a note whose row IS in the window still arrives.
    a.apply(ServerFrame::Event(env_at(
        0,
        9_000,
        SessionEvent::Warning {
            code: "target_green".into(),
            detail: "target_green at 294af894".into(),
            compaction: None,
        },
    )));
    let screen = a.screen(120, 40).join("\n");
    assert!(
        screen.contains("target_green at 294af894"),
        "the notice filed at the end of the conversation is not on the screen:\n{screen}"
    );
    // The one in the window is not counted.
    assert_eq!(
        a.notes_before(),
        1,
        "a note that IS in the window is counted as out of it"
    );
}

/// `ctrl-n`, then the counter's expansion: the notices stay hidden and stay hidden the second time. The click
/// throws the rendered history away and rebuilds it, the strongest re-render the head has, and that must not draw
/// the wall.
#[test]
fn ctrl_n_then_the_counters_expansion_draws_no_notice() {
    let mut a = a_run_with_notices();
    // Premise: one counted row, and a tail window. The first frame carries the marker-facts
    // rebuild, so the second is the steady one every door below starts from.
    a.screen(200, 40);
    let before = a.screen(200, 40).join("\n");
    assert!(before.contains("model_endpoint_retry"), "{before}");
    assert!(before.contains("2 tool calls"), "{before}");
    assert!(
        a.hist_floor > 0,
        "the premise, a tail walk (floor {}, upto {})",
        a.hist_floor,
        a.hist_upto
    );

    a.key(Key::CtrlN);
    assert_eq!(a.read_upto("model_endpoint_retry"), Some(3_000));
    let cleared = a.screen(200, 40).join("\n");
    assert_eq!(
        cleared.matches("model_endpoint_retry").count(),
        0,
        "the press did not clear the wall:\n{cleared}"
    );

    // Open the run - the door the operator's click takes.
    let (x, y) = where_at(&a.screen(200, 40), "2 tool calls");
    a.key(Key::Click { x: x + 1, y });
    let open = a.screen(200, 40).join("\n");
    assert!(open.contains("CONTENTS-0"), "the run did not open:\n{open}");
    assert_eq!(
        open.matches("model_endpoint_retry").count(),
        0,
        "the expansion drew the retired wall:\n{open}"
    );

    // Fold it back and open it again: a second rebuild, and the same answer.
    a.key(Key::Esc);
    let (x, y) = where_at(&a.screen(200, 40), "2 tool calls");
    a.key(Key::Click { x: x + 1, y });
    let again = a.screen(200, 40).join("\n");
    assert!(again.contains("CONTENTS-0"), "{again}");
    assert_eq!(
        again.matches("model_endpoint_retry").count(),
        0,
        "the second expansion drew the retired wall:\n{again}"
    );
    // And the mark is still the mark - nothing a drawing path did moved it.
    assert_eq!(
        a.read_upto("model_endpoint_retry"),
        Some(3_000),
        "a drawing path moved the watermark: {:?}",
        a.read_marks
    );
    assert_eq!(a.notes.len(), 3, "the notes were dropped, not retired");
}

/// `ctrl-n`, then a row is queued: a prompt typed while a turn runs is an echo under the live pane and nothing
/// else, so nothing about the transcript changed and the notices must not come back.
#[test]
fn ctrl_n_then_a_queued_row_draws_no_notice() {
    let mut a = a_run_with_notices();
    // Two frames for the reason the counter's test gives: the first carries the marker-facts
    // rebuild, the second is the steady one.
    a.screen(200, 40);
    assert!(
        a.screen(200, 40)
            .join("\n")
            .contains("model_endpoint_retry"),
        "the fixture's notices are not on the frame"
    );
    assert!(
        a.hist_floor > 0,
        "the premise, a tail walk (floor {}, upto {})",
        a.hist_floor,
        a.hist_upto
    );
    a.key(Key::CtrlN);
    assert_eq!(
        a.screen(200, 40)
            .join("\n")
            .matches("model_endpoint_retry")
            .count(),
        0
    );

    typed(&mut a, "and what about the queue");
    let sent = a.key(Key::Enter);
    assert!(sent.is_some(), "the line was not sent: {sent:?}");
    let queued = a.screen(200, 40).join("\n");
    assert!(queued.contains("queued"), "no echo on the frame:\n{queued}");
    assert_eq!(
        queued.matches("model_endpoint_retry").count(),
        0,
        "the queued row brought the retired wall back:\n{queued}"
    );
    assert_eq!(
        a.read_upto("model_endpoint_retry"),
        Some(3_000),
        "the queued row moved the watermark: {:?}",
        a.read_marks
    );
}

/// K9's done-test: a warning kind `[warnings] mute` names is never drawn, and nothing else about
/// it changes - it is filed, counted and findable, exactly as a retired note is. The file half
/// (`[warnings] mute` -> this list) is `crates/leticode-config`'s own test; this is the half that
/// is the screen's.
#[test]
fn a_muted_warning_kind_is_never_drawn() {
    // A code the head still draws: `compacted` twins the model's own summary row and is filed
    // beside the conversation, so the drawing half of a mute would have nothing to prove.
    let said = "this session's workspace is not there any more: /home/dead/Projects/gone.";
    let gone = || {
        env(
            1,
            SessionEvent::Warning {
                code: "workspace_gone".into(),
                detail: said.into(),
                compaction: None,
            },
        )
    };

    // The premise: with nothing muted the code IS a row in the conversation.
    let mut plain = App::new(plain_cfg(80));
    assert_eq!(
        plain.apply(ServerFrame::Event(gone())),
        Disposition::Rendered
    );
    assert!(
        plain.screen(80, 24).join("\n").contains("workspace_gone"),
        "the premise: an unmuted row is drawn"
    );

    let mut muted = App::new(plain_cfg(80));
    muted.quiet = Quiet {
        muted: vec!["workspace_gone".to_string()],
        coalesce: true,
    };
    assert_eq!(
        muted.apply(ServerFrame::Event(gone())),
        // Read and filed as always: the mute is the DRAWING's, not the event's.
        Disposition::Rendered
    );
    let drawn = muted.screen(80, 24).join("\n");
    assert!(
        !drawn.contains("workspace_gone"),
        "a muted kind was drawn:\n{drawn}"
    );
    // And it is still there: held, counted, and one verb from being read.
    assert_eq!(muted.notes.len(), 1, "the note was dropped, not muted");
    assert_eq!(muted.retired_notes(), 1, "and it is not counted as hidden");
    let listed = muted.notes_lines().join("\n");
    assert!(
        listed.contains("workspace_gone"),
        "a muted note left `/notes` as well as the screen: {listed}"
    );

    // The kind and not the copy: a second copy of the same code is muted with the first, which
    // is the whole point of one line in a file instead of 127 `retired` keys.
    let mut other = App::new(plain_cfg(80));
    other.quiet = muted.quiet.clone();
    other.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Warning {
            code: "workspace_gone".into(),
            detail: "this session's workspace is not there any more: /home/dead/Projects/other."
                .into(),
            compaction: None,
        },
    )));
    assert!(
        !other.screen(80, 24).join("\n").contains("Projects/other"),
        "a second copy of a muted kind was drawn"
    );
}
