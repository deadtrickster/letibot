//! Notes, warnings, the alarm and its counters, the notice.

use super::*;

/// **The negative assertion: a prompt is not retired by a row that merely
/// CONTAINS it.**
///
/// This is the failure that matters, and it is the one the whole-line rule
/// exists against. leticl's wording: a prompt `second thing` must not be retired
/// by a row reading `first thing\nsecond thing-guess`, or the head swallows a
/// prompt the daemon has not answered — the operator's sentence, gone from the
/// screen with nothing else holding it.
///
/// Three ways to be *contained but not a line*, because they fail differently: a
/// row that continues the echo's text, a row that precedes it, and a row that
/// holds the echo's words inside one longer line.
#[test]
fn a_row_that_only_contains_the_prompt_does_not_retire_it() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // **No turn**, deliberately: `App::submit` joins consecutive sends onto one
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

    // And the answer that IS one — a whole line — retires exactly that one.
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

/// **A line is spent once**, which is the property the old equality rule gave
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

/// **A blank line is not a claim.** A piece that is empty carries no words, so it
/// cannot say whether a prompt landed — and a row of blank lines must not retire
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

/// **R17: a head knows whether it has every row the daemon says it has.**
///
/// `seq` is the log's position, dense and assigned by the daemon alone — and this
/// head assigned it unconditionally (`self.seq = env.seq`), which is exactly what
/// makes a delivered row and a dropped one look the same. Measured 2026-09-22: a
/// prompt that was **in the ledger and acted on** never reached this head's screen,
/// and nothing in this head could have said so.
///
/// Three obligations, and the third is why the second is not enough: **say** it (a
/// note in the conversation, not a transient line — this is a fact about the
/// session), **repair** it (`/resync` exists for precisely this), and **count** it.
/// A gap repaired silently is indistinguishable from a session that never had one,
/// so the operator learns nothing about a daemon, a socket or a compaction that is
/// losing rows — which is the same argument `/status`'s `unreadable` bucket makes,
/// and this is the sixth of that family.
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
    // **Said in the conversation**, where it scrolls with everything else, and
    // naming the range so two gaps are two lines rather than one deduped one.
    let notes = a.notes_lines().join("\n");
    assert!(notes.contains("log_gap"), "nothing said the gap: {notes}");
    assert!(
        notes.contains("2..3"),
        "the lost seqs are not named: {notes}"
    );
    // **And repaired.** A resync is the one thing that can put the head back in
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

    // **Continuity is silent.** Ten events in a row say nothing at all, which is
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

/// **R17's other two numbers: a body with no row, and a daemon that is ahead.**
///
/// `gaps` is the wire losing events. These are the two that look identical to it
/// from inside and are not it:
///
/// * **`orphan_bodies`** — the row was announced, a snapshot replaced the rows
///   without it, and its words arrive with nowhere to go. `record_item` used to
///   `return` in silence, which is why the trailer of a lost row was that there
///   was no trailer. The row is not in `items`, so there is not even a
///   placeholder to notice; this count is the whole of what is left of it.
/// * **`behind`** — nothing is lost and nothing is orphaned, and the head is not
///   current: the events are in a queue, on a socket, or in this head's own
///   channel. This is the state the operator measured from outside on 2026-09-22
///   and could not name, and it is the reason a screen cannot be trusted to say
///   whether the session has gone quiet or the head has stopped being told.
#[test]
fn a_body_with_no_row_and_a_daemon_that_is_ahead_are_both_counted() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));

    // A body for a row this head never had: counted, said, and drawn nowhere —
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
    // **And it is not in the conversation**, because there is no row for it.
    let screen = a.screen(100, 40).join("\n");
    assert!(
        !screen.contains("a prompt that fell out of a snapshot"),
        "a body with no row was drawn anyway: {screen}"
    );

    // **A daemon that answers from further along than this head has read.** The
    // head is at seq 1 and the command's effect is visible at 9, so eight events
    // are in flight and none are lost — the head is behind, which is a different
    // fact and is why it is a different number.
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

    // A redelivery — a seq at or below this head's own — is not a distance.
    a.apply(ServerFrame::Accepted {
        client_request_id: "r2".into(),
        seq: 1,
        note: letibot_sessionlog::protocol::NOTE_PROMPT_QUEUED.into(),
    });
    assert_eq!(a.behind, 0);
}

/// **A refusal the harness made is one dim line, not a wall in red.**
///
/// The operator's report, about a `bash` one-liner the normaliser could not
/// read: *"i get what it tries to do, but it just throws up on my chat"*. Every
/// word of that explanation is addressed to the model, it was already on the
/// screen as the tool's own result one row above, and nothing in it is
/// answerable — no adjudicator was consulted and no grant lifts it.
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
    // One sentence, so at most a wrap of one. The thing being measured is that
    // it is not a paragraph per unresolved construct.
    assert!(
        lines.len() <= 2,
        "a wall again, {} lines: {lines:#?}",
        lines.len()
    );
    let l = lines.join(" ");
    let l = &l;
    assert!(l.contains("bash not_run"), "{l}");
    assert!(
        l.contains("meaning does not exist yet"),
        "the gist survived: {l}"
    );
    // Not the paragraph, not the id, not the loud register.
    assert!(
        !l.contains("parameter_expansion"),
        "the model's detail leaked: {l}"
    );
    assert!(!l.contains("adj-s-"), "an id nobody can use: {l}");
    assert!(!l.contains('!'), "still shouting: {l}");
}

/// **A program that dies at once leaves its sentence and its status, and they stay.**
///
/// The operator's defect, in their words: *"i typed `!term mc`, it flashed and was gone"*.
/// The pane was taken, one sentence was posted as a **notice** — which lives for
/// `NOTICE_MS` of wall time — and what the program had printed went with the rectangle. A
/// person who looked a minute later, or who came back to the session, saw nothing at all.
///
/// So the assertion is on the **frame**, not on a notice: the row is still there when the
/// notice is cleared, it names the line that ran, the daemon's own sentence with the exit
/// status in it, and the last thing the program printed.
#[test]
fn a_pane_that_dies_at_once_leaves_a_row_with_its_sentence_and_its_status() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    a.clock(1_000);
    // **One frame before the key**, because a pane's rectangle is the *last frame's* —
    // `App::submit` says so, and it is what a head that has drawn anything always has. A
    // test that opened a pane before drawing would be asking a 1×1 screen what the program
    // left on it.
    let _ = a.screen(80, 24);
    pane(&mut a, b"mc: not found\r\n");
    a.apply(ServerFrame::TermEnded {
        reason: "the program exited with 127".into(),
    });
    assert!(!a.pane_open());

    // **A row and not a notice.** `say` is what carried this, and it is the whole reason
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

    // **And a second pane that dies the same way is a second row.** The identity a note is
    // deduped by cannot tell two identical endings apart, so a pane's ending is filed without
    // the redelivery test — otherwise the second `!term mc` would be swallowed by the first.
    pane(&mut a, b"mc: not found\r\n");
    a.apply(ServerFrame::TermEnded {
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

/// **R10: a note the operator has read can be retired, and it stays retired.**
///
/// Four things, and they are one behaviour. A note is a **disclosure, not a
/// permanent record**: the session log holds the durable fact and the note is how a
/// head shows it once, so a reader must be able to retire one they have read. A
/// retired note must **stay** retired across a resync, a reattach and a **restart** —
/// a snapshot replanting what somebody just dismissed is the same defect as never
/// letting them dismiss it. And it must not **drop** the fact: hidden, counted and
/// findable, which is the rule `/status`'s own `filtered` counter already keeps.
///
/// **R19 moved the two halves of this test, and made the second of them harder.** The
/// note is filed **live** here, because a note that arrives with a snapshot is no
/// longer planted at all — so what has to be proved about the *resync* is now that a
/// retired note is not drawn, and what has to be proved about the *restart* is that a
/// retirement loaded from `head.toml` beats a **live** delivery, which is the one case
/// where nothing else would stop it. (The operator's own wall had none of this
/// retired: fault 1 is why it was up.)
/// **R22: clearing your own screen costs one key, and it keeps every rule R10 bought.**
///
/// The operator, on being told how to hide a note: *"typing `/notes dismiss all` is not
/// humane."* So the reflex is a chord — `ctrl-n` — and this test is the whole of what it
/// must not break: **retired is not deleted.** The notes stay, `/notes` prints them,
/// `/status` counts them, `/notes restore` brings them back, and the reflex survives a
/// restart (R19 part 3) so it does not have to be repeated.
///
/// The agreement with the other head is in `head-parity-2026-09-21.md` §R22 and is
/// asserted here as behaviour rather than restated: **all of them, not the newest**, and
/// the empty press says so.
#[test]
fn ctrl_n_retires_every_note_and_keeps_every_note() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    // Four notes, which is the shape the operator actually reported: R19's restart was
    // four notes at three lines each, and R10's wall was two.
    for (code, detail) in [
        (
            "gate_timeout",
            "denied: nobody answered before the deadline",
        ),
        ("daemon_stopping", "`dead` asked this daemon to stop"),
        ("compacted", "compacted: 940188 → 9181 tokens"),
        (
            "auto_compact",
            "938065 of 999999 tokens resident — compacting now",
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
    assert!(before.contains("auto_compact"), "{before}");

    // **One key, and the wall goes.**
    assert_eq!(a.key(Key::CtrlN), None, "the chord is not a daemon action");
    let after = a.screen(100, 30).join("\n");
    for code in [
        "gate_timeout",
        "daemon_stopping",
        "compacted",
        "auto_compact",
    ] {
        assert!(
            !after.contains(code),
            "`{code}` survived the press:\n{after}"
        );
    }
    // **And it says what it did, in the VERB's own words** — because it calls the verb
    // rather than composing a second sentence. That is the property worth asserting here:
    // a chord with its own wording is a chord that can come to disagree with `/notes
    // dismiss all` about what just happened. `(not saved: ...)` is in this frame because
    // this test's head has no `head.toml`; that suffix is the same writer's too.
    assert!(after.contains("retired 4 note(s)"), "{after}");
    assert!(after.contains("still counted on /status"), "{after}");

    // **Not deleted.** Every one is still held, still counted, still readable.
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

    // **A second press says there is nothing left to retire.** Not "retired 0 note(s)",
    // which is what the verb would say and is a sentence an operator should not be shown:
    // the notes are held but retired, so the count of what is LEFT is zero.
    assert_eq!(a.key(Key::CtrlN), None);
    let twice = a.screen(100, 30).join("\n");
    assert!(twice.contains("nothing to retire"), "{twice}");
    assert!(!twice.contains("retired 0 note(s)"), "{twice}");

    // **And back, if the reader was wrong** — the undo is exactly as cheap as the act.
    typed(&mut a, "/notes restore");
    a.key(Key::Enter);
    let back = a.screen(100, 30).join("\n");
    assert!(back.contains("gate_timeout"), "restore did nothing: {back}");
    assert!(
        back.contains("/notes restore") || back.contains("back on the screen"),
        "{back}"
    );
}

/// **The reflex survives a restart** (R22 + R19 part 3), which is the half that stops it
/// having to be repeated — and the half that used to be B's requirement and became A's
/// when the operator hit it.
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

    // A new head, the same file, the incident delivered **live** — the one delivery
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

#[test]
fn a_note_the_operator_has_read_can_be_retired_and_stays_retired() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    // **The snapshot from before the incident**, which is what the restarted head gets:
    // a head that attached and *then* watched it happen. Taken here rather than inside
    // the restart block below, because by then the hub carries the warning and a head
    // whose snapshot already holds it would not be the live case at all.
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

    // **Retired, and the wall goes — including its blank line.** The sentence said
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

    // **Hidden is not deleted.** The note is still this head's, `/status` counts it,
    // and `/notes` lists it with its text under a marker — the rule `/status`'s own
    // `filtered` counter keeps, which is what makes "I chose not to show this" a
    // different statement from "nothing happened".
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

    // **The two things that used to replant it.** A resync replaces the head's whole
    // note list from a snapshot — *everything in a snapshot is history and none of it
    // is anchored* — so before R10 the wall came back at position 0, above the whole
    // conversation, on an operation the operator did not ask for. The identity is
    // built from the log (code, ts, detail) and not from the screen, so the snapshot's
    // copy is the same key and stays retired.
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

    // **The restart, and the half R19 made harder.** The dismissals live in
    // `head.toml` and not in the process, and a head that has just started has shown
    // nothing — so the incident is delivered to `b` **live**, at a seam, which is the
    // one delivery nothing else would suppress: a snapshot's copy is not drawn for being
    // prior, and a redelivery is not filed twice. What stops it here is the retirement
    // this head read off the file, against the same `clean` snapshot (no warning in it)
    // that `a` attached to.
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

    // **And back, if the reader was wrong.** Two ways, and both of them are the
    // reader's: no dismissal is ever final in a way they cannot undo.
    typed(&mut b, "/notes restore");
    b.key(Key::Enter);
    let back = b.screen(100, 30).join("\n");
    assert!(back.contains("gate_timeout"), "restore did nothing: {back}");
    // **And the FILE, which this test used to skip.** Restoring on the screen while the
    // file kept the key is the defect leticl reported: the two disagreed and the next
    // save by any head put the dismissal back, so `restore` lasted until somebody else
    // saved. Asserted here because the screen alone could not tell the two apart.
    let (p, _) = crate::prefs::load(&path);
    assert!(
        !p.retired.iter().any(|k| k == &key),
        "restore did not reach the file: {:?}",
        p.retired
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A dismissal unions; a restore replaces** — leticl's report, as a test.
///
/// leticl found this rather than fixing it, and the shape of the fault is worth keeping:
/// a union can only ever ADD a key, so under a union a restore's save wrote the file's own
/// keys straight back, the head's `dismissed` went to `[]` while the file kept the key, and
/// the two disagreed — which is the SAME class of fault `merge_retired` was written to end,
/// arriving from the other direction. A restore is an assertion of removal, and only a
/// replacement can say it.
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

/// **The operator's own state, reproduced from the measurement rather than guessed.**
///
/// 2026-09-22, on his screen: `/notes` said `7 note(s), 1 retired` and only one entry
/// carried `[retired]`, while `head.toml` held keys for the notes that were listed.
/// `cargo run --example dump_notes` put the two lists side by side against the live log
/// and found six of the seven keys **identical — code, clock and detail hash** — so the
/// file and the log agreed, and the odd one out was the head's own `dismissed`.
///
/// The six facts below are the real ones: the real clock and the real sentence, each
/// asserted against the key `head.toml` actually held. A transcription slip fails here
/// rather than passing quietly as a different incident.
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

/// **A dismissal a SECOND head made is honoured by this one, without a restart.**
///
/// The operator's report of 2026-09-22 — *"i dismissed letibot notes but they stay"*
/// — and the half of it that a shared file makes possible. `~/.config/letibot/head.toml`
/// is one file for every head on the box, and `load_prefs` runs **once, at startup**:
/// two heads were running (measured: pids 2076943 and 2350959), so what one of them
/// retired after the other had loaded was invisible to it for the rest of its life.
///
/// The path here is the operator's exactly: `a` loads the file, *then* the file gains a
/// key, and `a` is asked for the listing. Before `refresh_retired` the note was still on
/// the screen; the assertion is that it is not.
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

    // **A second head retires it.** Not a call into `a` — a write to the file this
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

    // **Before it looks, this head does not know.** `dismissed` is what it loaded at
    // startup, and the file has moved since — which is the state the operator was in.
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
    // itself — the redelivery half of R10, now with the key read off the file
    // rather than out of this head's own memory.
    a.key(Key::Esc);
    a.apply(ServerFrame::Event(live.clone()));
    let screen = a.screen(120, 60).join("\n");
    assert!(
        !screen.contains("nobody answered"),
        "a note retired by another head came back at a seam:\n{screen}"
    );
}

/// **R19's first fault: history arrives as news.**
///
/// The operator restarted a head and was met by twelve red lines — four notes,
/// `daemon_stopping`, `compacted` and two `auto_compact`, folded correctly to three
/// lines each, at the top of a session that had just started: *"i dont want to see
/// that on restart."* **None of them had been dismissed**, which is why persisting a
/// retired set would not have helped. A head that has just attached has shown nothing,
/// so it was replaying hours of announcements as though they had just happened, above
/// a conversation they did not precede: *a warning is how a head shows a fact ONCE.*
///
/// Four assertions and a control: the screen has none of them; the head still holds
/// them; `/status` counts them; `/notes` lists them with their text; and a warning
/// that arrives **while the head is watching** is still drawn, because that is what a
/// note is for.
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

    // **Held, counted, listed** — the three things R19 asks for instead of drawing
    // them. `/notes` is where the fix puts them and `/status` is how a reader knows
    // there is something there.
    assert_eq!(a.notes.len(), 3, "{:?}", a.notes);
    assert_eq!(a.notes_before(), 3);
    assert_eq!(a.retired_notes(), 0);
    a.command("status");
    let stats = a.screen(120, 60).join("\n");
    assert!(
        stats.contains("3 from before this window"),
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

/// **R19's second fault: routine is painted as failure.**
///
/// `compacted` and `auto_compact` are the session doing exactly what it should, and
/// they arrived in the same red as a denial or a gate timeout — *the colour asserted a
/// severity the fact did not have*, which is why four notes read as a wall. **A
/// housekeeping notice and a refused call must not look alike.**
///
/// Asserted on the bytes rather than the look, and on both registers in one frame: a
/// routine code loses the `!` and the red, a failure keeps both. The split itself is
/// `letibot_sessionlog::warning`'s (the codes are the log's vocabulary and both heads
/// render them), so what this test pins is what *this* head does with it.
#[test]
fn a_routine_notice_is_not_drawn_in_the_failure_register() {
    const RED: &str = "\u{1b}[31m";
    let mut a = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    for (code, detail) in [
        ("compacted", "compacted: 940188 → 9181 tokens"),
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
    let routine = line("compacted");
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

/// The border is not silent about a counter that has **moved**.
///
/// The half of §13.2b that does belong on a resident row: a head that dropped
/// events is a head whose transcript has a hole in it, and that must not wait
/// for somebody to type a command.
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
    // The border says it with a triangle, pinned right — a fact that exists
    // only while it does, and never a resident sentence of bright yellow.
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains('⚠'), "{screen}");
    assert!(
        !screen.contains("dropped 12"),
        "the numbers are /status's, not the border's: {screen}"
    );
    // The unboxed composer — no border to pin a triangle to — names them on a
    // line of its own, read HERE, before the screen below acknowledges the alarm
    // (R51 item 17; the assertion used to sit after the `/status` read).
    let border = a.status_line(200);
    assert!(border.contains("dropped 12"), "{border}");
    assert!(border.contains("resync 1"), "{border}");
    assert!(
        border.contains("/status"),
        "and says where the rest is: {border}"
    );
    // …where they keep their names and their counts.
    a.command("status");
    // **A taller window, because the panel is taller now.** MEASURED while landing this
    // (2026-10-05): the `filtered` row's gloss names the profile the head is on, so several
    // rows gained a line of wrap and the counters below `dropped` sit past a 40-row screen —
    // the `resync` row is still on the panel (it is the tenth row of `status_lines`), it was
    // simply under the fold. The subject of this assertion is the row, so it is given room
    // to be drawn rather than asked for off-screen.
    let stats = a.screen(120, 60).join("\n");
    let dropped_row = stats
        .lines()
        .find(|l| l.contains("dropped"))
        .expect("the dropped row is on the /status screen");
    assert!(dropped_row.contains("12"), "{dropped_row}");
    // **The panel's own row, not the notice line** — MEASURED while landing this
    // (2026-10-05). At HEAD the resync notice is live but NOT on this screen: the composer's
    // height ladder deletes the most expendable row first, and at 24 rows the notice is the
    // one that goes. This rewrite's row budget is one row larger, so the notice now fits on
    // the screen (`  · resync: queue overflow`) and `find(|l| l.contains("resync"))` — which
    // this test used to do — hit THAT line instead of the counter row it means to assert.
    // The assertion's subject is the counter, so it looks for the row's own shape: the panel
    // indents its rows, and the number follows the label.
    let resync_row = stats
        .lines()
        .find(|l| {
            // Painted rows carry SGR codes before the label, so the shape is read off the
            // plain text — the same stripper the pane uses on a name.
            let plain = without_control_lines(l);
            plain.trim_start().starts_with("resync") && plain.chars().any(|c| c.is_ascii_digit())
        })
        .expect("the resync row is on the /status screen");
    assert!(resync_row.contains('1'), "{resync_row}");

    // **And READING it acknowledged the alarm** — R51 item 17, in the test that already had
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

    // **A counter that moves AFTER the reading is a new fact, and the mark comes back.**
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

/// **The acknowledgement is per counter and only up to what was read** — R51 item 17's
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
    // Read it: acknowledged, and STILL not news — the second reading of the same number is
    // what a reader does not need a second time.
    assert!(!one.exceeds(one));
    // A resync after the one that was read is a different fact.
    let two = Counters {
        resyncs: 2,
        ..Counters::default()
    };
    assert!(two.exceeds(one), "the second resync is news again");
    // **Per counter, not as a total.** A counter whose value went DOWN is not news — an alarm
    // that fired on a decrease would be a mark nobody could ever clear — and a DIFFERENT
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

/// **A frame this head cannot read is said, counted, and not fatal.**
///
/// Requirement R3, and the two failures it sits between: a head that exits on an
/// unparseable frame takes the session down and says nothing, and a head that steps
/// over one in silence is the same failure more quietly — *"this daemon is sending
/// me something I do not understand"* becomes indistinguishable from quiet. So:
/// the sentence is in the transcript, the counter moves, `/status` has the bucket
/// at zero **and** after, and nothing about it touches the read mark.
#[test]
fn an_unreadable_frame_is_said_counted_and_survived() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // **The bucket exists before anything lands in it.** §13.2b: an absent field
    // and a zero field must not look the same, and a head that does not count
    // these at all is what every head was until now.
    a.command("status");
    // **Taller than 40 rows on purpose.** `/status` is a scrolling pane, and R10
    // added a row to it (the notes the reader has retired) — so a test that asserts
    // a row is *on the screen* has to give the pane room for all of them rather than
    // depend on where the list happens to end.
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
    // **Nothing was parsed, so nothing is claimed.** The read mark is untouched
    // and neither ack counter moved: `filtered` is "events I chose not to show",
    // and inventing a seq here would rewind the mark over frames already read.
    assert_eq!((a.seq, a.rendered, a.filtered), before);

    // The sentence is in the conversation: what it was, that this is almost always
    // a newer daemon, and that this head is still attached.
    let screen = a.screen(120, 60).join("\n");
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
    // **Taller than a screen, for the reason stated above.** `/status` is a scrolling pane
    // and it has gained a row since this test was written (R10's retired notes, then the
    // unfilled-rows row); a test asserting a row is on the screen has to give the pane room
    // for all of them rather than depend on where the list happens to end.
    let stats = a.screen(120, 140).join("\n");
    let unreadable_rows: Vec<&str> = stats.lines().filter(|l| l.contains("unreadable")).collect();
    assert!(
        unreadable_rows.iter().any(|l| l.contains('2')),
        "{unreadable_rows:?}"
    );
}

/// **A head that outlives its daemon notices, says so, and re-asks.**
///
/// The operator's box: three `letibot-tui` processes alive (ages 15d, 1d18h, 21h) while the
/// daemon was replaced this afternoon. A head outlives the daemon that gave it its facts, and
/// the registry is in memory — so a daemon that comes back is not the one whose answers are
/// still on the screen. The head went on drawing them without ever saying the party it was
/// talking to had changed.
///
/// **The identity is `SO_PEERCRED` and the protocol**, which is what the head has: the pid is
/// the kernel's answer for *this socket*, re-read by the caller on every connection, and the
/// version rides the `Hello`. Both halves are asserted, because either alone can be wrong —
/// a pid is reused, and a rebuild that keeps its number is still a different daemon.
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

    // **A switch on the same socket, by the same process, is not a replacement.** This is the
    // half that keeps the note from firing on every keystroke-shaped `Hello`.
    a.apply(hello("s", a_family(), hub.snapshot()));
    assert!(
        !a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "daemon_replaced")),
        "the same daemon answered the same session twice: {:?}",
        a.notes
    );

    // **The socket died and the driver came back to a different process.** `head.rs` re-reads
    // `SO_PEERCRED` on the new connection before the `Hello` is folded — that is the caller's
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

    // **And the refetch ran**, which is the other half of the requirement: the head does not
    // keep drawing the old picture, it asks again. The same three reads as any seating.
    let asked = a.take_actions();
    for want in [Action::ListSessions, Action::ListJobs, Action::Settings] {
        assert!(
            asked.contains(&want),
            "a head under a new daemon did not re-ask for {want:?}: {asked:?}"
        );
    }

    // **A rebuild that moved the wire is a replacement too**, even at the same pid — which is
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

/// **A pid the kernel would not name is said as such, not as a number.** R30's rule on the
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

/// **R6: a named fill's line is the daemon's count, not a count of unfilled rows.**
///
/// The whole of the ruling: an indicator must be the fact, not a rendering of the
/// fact. Counting the rows still lacking a body would (a) name the wrong operation —
/// an ordinary reply is not an import (see `MIN_FILLING`) — and (b) draw *"never
/// filled in"* three seconds into a healthy generation. So the daemon names the
/// operation and counts it, and the head draws exactly that.
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
    assert!(start.contains("0 of 9570 parts"), "{start}");
    assert!(
        start.contains("importing an opencode conversation"),
        "{start}"
    );

    a.apply(ServerFrame::Event(env(2, tick(4_790))));
    let mid = a.screen(100, 30).join("\n");
    assert!(mid.contains("4790 of 9570 parts"), "{mid}");

    // **The last tick clears the line.** The daemon's durable finish note is what
    // says the fill finished, and a bar left at `total of total` would sit on the
    // screen for ever.
    a.apply(ServerFrame::Event(env(3, tick(9_570))));
    let done = a.screen(100, 30).join("\n");
    assert!(
        !done.contains("parts"),
        "the line goes when the fill is done: {done}"
    );
}

/// **The file is edited while the pane is open**, which is the one case an
/// open-time read cannot see. The operator: *"since the file can be updated,
/// dont cache it i guess or do a watcher with a nice syscall"*. One `stat`
/// per draw, not a watcher thread.
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
    // On its own line, above the composer — never *instead* of the composer,
    // which is what it used to be.
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
    // **Frames do not age it** (§11.3). Two hundred repaints at the same millisecond
    // leave the sentence exactly where it was — which is the whole point of moving the
    // clock off the frame and onto the wall: a notice's lifetime was 60 loop passes,
    // which is 6 s on a busy head, instant under `--replay`, and for ever on one whose
    // counter had already reached zero.
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

/// **A notice expires in TIME, on this head's clock** — §11.3, and R13's second
/// symptom. The mirror of leticl's `a-notice-expires-and-an-alarm-does-not`.
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
    // deadline, gone the millisecond it is due — and never `screen()` in between, so
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
    // **A key is the acknowledgement, and it moves the deadline rather than the text:**
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

/// **A fleeting notice goes when its time is up, and a failure's does not.**
///
/// The operator's report, in their words: *"also merge_queued notification sticks and jumps
/// slightly up when you actively reply and then bottom - hide it to near triangle with the
/// current queue stats … do it after time, say 30 seconds"*. So the row is drawn for
/// [`FLEETING_MS`] of wall time and then taken down — while `merge_not_queued`, its failure
/// twin (the same door's other verdict: a branch nothing will land), is not on a timer at all.
///
/// **The clock is the head's own and the unit is the wall**, which is `NOTICE_MS`'s test one
/// register over: 200 repaints at one millisecond age nothing, a millisecond before the
/// deadline the row is still there, and the millisecond it is due it is gone. A timer counted
/// in frames is a timer that stops when the frames stop.
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
    assert!(screen.contains("is in the merge queue"), "{screen}");
    assert!(screen.contains("nothing will land it"), "{screen}");

    // Frames do not age it (§11.3).
    for _ in 0..200 {
        a.screen(100, 24);
    }
    assert!(
        a.screen(100, 24)
            .join("\n")
            .contains("is in the merge queue"),
        "200 repaints is not a duration"
    );

    // The boundary, and not "eventually".
    a.clock(1_000 + FLEETING_MS - 1);
    assert!(
        a.screen(100, 24)
            .join("\n")
            .contains("is in the merge queue"),
        "still there a millisecond before"
    );
    a.clock(1_000 + FLEETING_MS);
    let screen = a.screen(100, 24).join("\n");
    assert!(
        !screen.contains("is in the merge queue"),
        "gone the millisecond it is due: {screen}"
    );
    assert!(
        screen.contains("nothing will land it"),
        "a failure is not on a timer, and the class table is what keeps it off one: {screen}"
    );

    // **Spent, not retired.** The sentence leaves this head rather than joining the reader's
    // own retired set — a timer may not write `head.toml`, because a key the reader restored
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

/// **One announcement is noted once, wherever it arrives from.**
///
/// A `Warning` can reach a head twice: `adopt` plants everything the snapshot
/// carries at anchor 0, and the live arm anchors at the CURRENT end of the
/// transcript. Two copies in two places — and the second sits under the
/// conversation, so new rows arrive beneath it and it reads as stuck to the
/// bottom. The operator, on three of them: *"sometimes new messages come
/// under those three but then those three again pinned to the bottom,
/// sometimes they just stay pinned"*.
///
/// The `ts` is taken from the snapshot rather than invented, and that is the
/// premise, not a detail. The view stamps `env.ts` and so does the live arm,
/// so the same announcement carries the same one by either route — a test
/// that made one up would be feeding the head a DIFFERENT warning and would
/// pass against a head that deduplicates nothing.
#[test]
fn one_warning_delivered_twice_is_noted_once() {
    let hub = letibot_sessionlog::hub::Hub::new("s");
    for i in 0..12 {
        hub.publish(letibot_sessionlog::SessionEvent::TranscriptAppended {
            item_id: format!("r{i}"),
            kind: "user".into(),
            ledger_head: String::new(),
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
    // And now the same envelope again, through the live arm — a redelivery.
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
    // **One announcement, one note, and it stays where it arrived** (R19). It
    // arrived in the snapshot, so it is not a seam of this conversation — the head was
    // not there — and the redelivery of the same envelope does not move it to a seam
    // at the end of the transcript, which would be the same fact drawn twice and in
    // the wrong place.
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
    // §4.5. Observed live before this event existed: the engine published a
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

/// **The ruling itself, driven end to end: the diagnostic moves to the edge and does not
/// stop existing.**
///
/// The operator, on `model_slow_first_byte`: *"it is important diagnostics - we have a
/// yellow triangle for that. both heads should not emit it inside conversation."* Both
/// halves have to hold at once, and either alone is a defect this tree has already paid
/// for: **kept and drawn as a row** is the wall they are reading, and **kept and drawn
/// nowhere** is a disclosure that has been deleted rather than moved.
///
/// So this drives the event and asserts the four things that have to be true together —
/// the arm's `Filtered` rather than `Rendered`, the sentence absent from the screen, the
/// counter moved, the triangle up, and the number named on `/status` — plus the control
/// that keeps them from being satisfied by a head that swallows every warning it is sent.
/// The arm's docstring is the argument; this is the assertion, because a docstring over
/// an arm nothing tests is exactly the shape R53 §1.2 found in this file.
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
        // **`Filtered`, not `Control` and not `Rendered`.** It is an event in the record —
        // it is read, and it is counted in the number that says what this head chose not to
        // show — and it is not a row. The arm's own comment argues it; `Control` would be
        // the third thing, and a frame this head is given is not a frame it is not given.
        Disposition::Filtered,
        "an edge-bound note is read and not drawn, and it is counted as filtered"
    );
    let screen = a.screen(80, 24).join("\n");
    assert!(
        !screen.contains(said),
        "the sentence is in the conversation, which is the ruling reversed:\n{screen}"
    );
    // **And it is still there.** The count moved, the triangle is up, and the reader is one
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

    // `/status`: the row is named, it is countable, and it carries the daemon's own code —
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

    // **The control: a code that changes the conversation is still a row in it.** Without
    // this, a head that swallowed every warning would pass everything above — and R19's own
    // argument is that compactions are the rows the operator asked to keep seeing.
    let mut b = App::new(plain_cfg(80));
    assert_eq!(
        b.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Warning {
                code: "compacted".into(),
                detail: "compacted: 940188 → 9181 tokens".into(),
                compaction: None,
            },
        ))),
        Disposition::Rendered,
        "a compaction changes the conversation and is a row in it"
    );
    assert!(
        b.screen(80, 24).join("\n").contains("940188"),
        "the compaction's own sentence is not drawn"
    );
    assert_eq!(b.slow_first_byte, 0, "a compaction is not the weather");
    assert!(
        !b.alarmed(),
        "and it raises no triangle, because nothing is wrong"
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

/// **The no-progress guard's warning leaves a RESIDENT state, not a note.**
///
/// The guard's finding is true until the session is replaced or the window is
/// raised, and the warning that announced it scrolls away with the
/// conversation — which is exactly where the 2026-10-09 wedge left it: one
/// warning line, scrolled past, and thirty provider refusals behind a session
/// that would not tidy itself and could not say why. The line above the
/// composer is what a person looks at on the way to typing; `/status` is the
/// whole-set read; and the sentence keeps the two doors apart, because the
/// daemon's recovery now does: pre-emptive tidying is off, a refused turn is
/// still compacted.
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

/// **The sentence a fold writes into `compacted`** — the daemon's own words, from
/// `sessions::compaction_said`, on the path where nobody watched the summary turn being
/// written: the whole record is pasted into the detail.
///
/// This is the text the operator was looking at. It is not a transcription slip: the format
/// strings are `compaction_said`'s, and the record under them is what the model reads in place
/// of everything before it.
fn a_compacted_said() -> String {
    format!(
        "compacted: 958397 → 100005 tokens, on transcript s#t25. Nothing of the summary turn \
         reached this screen — it ran over a scratch transcript — so here is what the model now \
         reads in place of everything before it:\n\n## Goal\n\n{}",
        "THE MODEL'S OWN RECORD ".repeat(20)
    )
}

/// **A compaction is ONE line in the rungs that show the conversation alone.**
///
/// The operator: *"compaction leads to too much noise in the conversation for me. in
/// conversation and show edits i want a one line - compacted blablabla"*. The two rungs he
/// named are `conversation` and `read-edits` — `Conversation`'s set with and without the
/// `Edits` flag — and the wall in both of them is the `compacted` warning, whose detail carries
/// the model's whole record on the fold path.
///
/// **The row the daemon appends for the fork is not what he is seeing**, and that is the thing
/// this test pins rather than assumes: the fork's `System` item is `Conversation`'s own
/// counter-example — *nothing the head did to produce them* — so the rung hides it, and what
/// reaches the glass is the warning. Both halves are asserted below.
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
        assert_eq!(
            carrying("958397 → 100005 tokens"),
            1,
            "a compaction's numbers are on ONE row at `{}`:\n{screen}",
            profile.name
        );
        assert_eq!(
            carrying("compacted"),
            1,
            "and the compaction is one row, not a block at `{}`:\n{screen}",
            profile.name
        );
        assert!(
            screen.contains("/notes"),
            "the row says where the whole sentence is at `{}`:\n{screen}",
            profile.name
        );
        // **The model's record is not on the screen.** It is the prompt, not anything that
        // happened, and this is the half the operator was complaining about.
        assert!(
            !screen.contains("THE MODEL'S OWN RECORD"),
            "the record the model reads was drawn in the conversation at `{}`:\n{screen}",
            profile.name
        );
        // **And nothing was swallowed**: the whole sentence, record and all, is on `/notes` —
        // the unfold the row's seam names.
        a.command("notes");
        let listing = a.screen(100, 60).join("\n");
        assert!(
            listing.contains("THE MODEL'S OWN RECORD"),
            "the record is not in the listing either, so the row cut it:\n{listing}"
        );
        assert!(
            listing.contains("ran over a scratch transcript"),
            "the daemon's sentence is not in the listing whole:\n{listing}"
        );
    }
}

/// **The fold is a property of the note and not of a rung**, and the fork's own row does not
/// vanish above them.
///
/// Two absences must not look like one. A rung-gated fold would make the ladder a revision —
/// `Verbosity::Loud`'s docstring: *"a rung that hid one would retroactively erase a warning
/// already read"* — so the compaction is one line at `loud` too; and the `System` row the fork
/// appends is drawn there as it always was, whole, because `loud` is the rung that draws what
/// the head was given.
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
            .filter(|l| l.contains("958397 → 100005 tokens"))
            .count(),
        1,
        "the warning is still one row at `loud`:\n{screen}"
    );
    assert!(
        screen.contains("This conversation was compacted"),
        "the fork's own row vanished from the rung that draws what the head was given:\n{screen}"
    );
}

/// **A warning in the same window still draws, whole.**
///
/// The fold's whole risk is the one `warning.rs` names: a head that folds a warning too
/// eagerly is a head whose failures nobody reads. A compaction is folded; a chain mismatch
/// beside it is not, and the two are asserted on one screen because that is the comparison a
/// reader makes.
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
            "ledger_chain_mismatch",
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
        .filter(|l| l.contains("ledger_chain_mismatch"))
        .collect();
    assert_eq!(mismatch.len(), 1, "the failure has its own row:\n{screen}");
    assert!(
        screen.contains("unverifiable"),
        "and its sentence is drawn, not folded away:\n{screen}"
    );
    assert!(
        !screen.contains("THE MODEL'S OWN RECORD"),
        "the compaction beside it is still one line:\n{screen}"
    );
}
