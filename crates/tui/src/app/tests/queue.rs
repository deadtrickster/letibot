//! The merge queue.

use super::*;

/// **The bootstrap read, and the two halves the answer carries.** `/queue` asks the daemon
/// for the whole queue and draws nothing until it answers — the jobs pane's shape, one pane
/// along. The verdict is drawn beside the entry it is about, which is why it travels in the
/// same frame rather than inside the entry.
#[test]
fn the_queue_pane_asks_for_the_queue_and_draws_what_comes_back() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    assert_eq!(a.command("queue"), Some(Action::ListMergeQueue));
    assert!(a.queue_pane);
    // Nothing has arrived: the pane says so rather than drawing a queue nobody sent.
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("merge queue"), "{screen}");
    assert!(screen.contains("none. A branch lands here"), "{screen}");

    a.apply(queue_frame(
        vec![queue_entry("c1", MergeState::Waiting)],
        vec![queue_review("c1", Some("accept"))],
    ));
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("agent/child-one"), "{screen}");
    assert!(screen.contains("waiting"), "{screen}");
    assert!(screen.contains("reviewer: accept"), "{screen}");

    // Closing it asks for nothing and puts the conversation back.
    assert_eq!(a.command("queue"), None);
    assert!(!a.queue_pane);
    assert!(!a.screen(100, 24).join("\n").contains("merge queue"));
}

/// **Esc in the entry overlay goes back to the LIST** — the overlay's own words, and the
/// jobs pane's rule one pane along: the queue is still behind it, so Esc must not take the
/// pane down with the overlay.
///
/// This is the one thing the pane slice got wrong, and it was invisible on paper: the
/// overlay's key block sat *below* the block that closes every pane on Esc, so the first
/// Esc closed the pane and left the overlay standing (the overlay is drawn before the pane,
/// so the screen did not change) and the second Esc closed the overlay over an empty
/// screen. One press did nothing, two presses left the queue.
#[test]
fn esc_leaves_the_entry_overlay_with_the_queue_still_behind_it() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    // A review that has been ASKED FOR and has not answered — the overlay says which,
    // rather than showing a decision nobody made.
    a.apply(queue_frame(
        vec![queue_entry("c1", MergeState::Waiting)],
        vec![queue_review("c1", None)],
    ));
    assert_eq!(a.key(Key::Enter), None);
    assert_eq!(a.queue_open.as_deref(), Some("c1"), "enter opens the entry");
    let screen = a.screen(100, 60).join("\n");
    assert!(screen.contains("merge queue · entry"), "{screen}");
    assert!(screen.contains("(asked, no answer yet)"), "{screen}");

    a.key(Key::Esc);
    assert!(a.queue_open.is_none(), "esc leaves the overlay");
    assert!(
        a.queue_pane,
        "esc goes back to the queue, not out of everything"
    );
    assert!(!a.screen(100, 24).join("\n").contains("merge queue · entry"));
}

/// **A failed review is legible on the pane's row** — the operator's other half of the report,
/// and the one that makes the restart a decision about something: *"Today the four entries are
/// invisible as failures unless you go to the store."*
///
/// The quota sentence is the thing to show rather than the word *failed*, and it has to survive
/// two things the pane does to it: rano draws `{id}{review}{evidence}` truncated at the pane's
/// width, and the review half used to say *the reviewer has been asked and has not answered*
/// over an attempt that had already died. So this asserts both halves of the fix — the review
/// says `no verdict` rather than *still waiting*, and the entry's own reason reaches the row.
#[test]
fn a_failed_review_says_so_on_the_panes_row() {
    use letibot_sessionlog::event::MergeState;
    let quota = "http 429: Weekly/Monthly Limit Exhausted. Your limit will reset at \
                 2026-10-12 15:01:48";
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut entry = queue_entry("c1", MergeState::Failed);
    // **The daemon's own words on the entry**, which is where the row a person scans reads the
    // reason: `mergequeue::review_exhausted_evidence` puts the failure FIRST for exactly this.
    entry.evidence = format!(
        "{quota} — the gatekeeper's attempt failed 3 times and the queue has stopped asking. \
         Nothing judged this branch. `/queue restart c1` asks again."
    );
    a.apply(queue_frame(
        vec![entry],
        vec![queue_review_failed("c1", quota)],
    ));
    let screen = a.screen(120, 24).join("\n");
    assert!(
        screen.contains("http 429: Weekly/Monthly Limit Exhausted"),
        "the quota sentence is on the row, not the word `failed`: {screen}"
    );
    assert!(
        screen.contains("reviewer: no verdict"),
        "and the review says no verdict rather than still waiting: {screen}"
    );
    assert!(
        !screen.contains("the reviewer has been asked and has not answered"),
        "an attempt that has died is not an attempt that is running: {screen}"
    );
    // **And a verdict still reads as one.** The change above must not have flattened the three
    // review states into two.
    a.apply(queue_frame(
        vec![queue_entry("c2", MergeState::Failed)],
        vec![queue_review("c2", Some("reject"))],
    ));
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("reviewer: reject"), "{screen}");
}

/// **The row draws the SHORT id, because the reason is what the line is for.** rano draws
/// `{id}{review}{evidence}` truncated at the pane's width, and a `task_start` entry's id is
/// forty-odd columns of `s-…-sub-…` — so a full id spent the line on itself and the failure a
/// person needed to read was cut off the end. The tail is the part that differs, and the overlay
/// still prints the id in full.
#[test]
fn the_row_draws_the_short_id_so_the_reason_fits() {
    use letibot_sessionlog::event::MergeState;
    let long = "s-1789919514688401228-sub-1791569790219697204";
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut entry = queue_entry(long, MergeState::Failed);
    entry.evidence = "http 429: Weekly/Monthly Limit Exhausted".into();
    a.apply(queue_frame(vec![entry], Vec::new()));
    let screen = a.screen(100, 24).join("\n");
    // `registry::short_id`: an ellipsis and the last eight characters, the part that differs.
    assert!(
        screen.contains(&format!("…{}", &long[long.len() - 8..])),
        "the row draws the tail: {screen}"
    );
    assert!(
        !screen.contains(long),
        "and not the forty-character id that would truncate the reason away: {screen}"
    );
    assert!(
        screen.contains("http 429: Weekly/Monthly Limit Exhausted"),
        "so the failure fits: {screen}"
    );
}

/// **`r` on a parked row asks the daemon to restart that entry's review** — the operator's ask,
/// in their words: *"so merge queue has 4 failed items, we need a way to restart them"*. The key
/// and the typed spelling send ONE verb, so the two cannot drift.
#[test]
fn the_restart_key_sends_the_verb_for_the_row_under_the_cursor() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(
        vec![
            queue_entry("c1", MergeState::Waiting),
            queue_entry("c2", MergeState::Failed),
        ],
        Vec::new(),
    ));
    // The cursor starts on the first row, which is `waiting`: nothing to restart, said rather
    // than sent, and nothing reaches the daemon.
    assert_eq!(
        a.key(Key::Char('r')),
        None,
        "a waiting row is not restarted"
    );
    let screen = a.screen(100, 24).join("\n");
    assert!(
        screen.contains("nothing to restart") || screen.contains("is `waiting`"),
        "{screen}"
    );
    // Down onto the failed row, and `r` is the restart.
    a.key(Key::Down);
    assert_eq!(
        a.key(Key::Char('r')),
        Some(Action::Slash {
            line: "queue restart c2".into()
        }),
        "the key sends the same verb `/queue restart ID` sends"
    );
}

/// **Folded, never invented** — the jobs pane's rule, and the two arrivals it is about.
/// A move for an entry this head was never told about adds nothing (the next snapshot
/// carries it), and the same id twice is the same row, because the enqueue is idempotent
/// by the child's own handle.
#[test]
fn an_entry_is_folded_from_the_events_and_never_invented() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::MergeEntryMoved {
            id: "c9".into(),
            state: MergeState::Landed,
            evidence: "merged as deadbee".into(),
        },
    )));
    assert!(a.merge.is_empty(), "a move is not an entry");

    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::MergeEntryAdded {
            entry: queue_entry("c1", MergeState::Waiting),
        },
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::MergeEntryMoved {
            id: "c1".into(),
            state: MergeState::Landed,
            evidence: "merged as deadbee".into(),
        },
    )));
    assert_eq!(a.merge.len(), 1);
    assert_eq!(a.merge[0].state, MergeState::Landed);
    assert_eq!(a.merge[0].evidence, "merged as deadbee");

    // The same id again is the same row, with the newer word on it.
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::MergeEntryAdded {
            entry: queue_entry("c1", MergeState::Taken),
        },
    )));
    assert_eq!(a.merge.len(), 1, "one child, one entry");
    assert_eq!(a.merge[0].state, MergeState::Taken);
}

/// **The ask is on the row and the overlay shows it.** It is what the reviewer reviews
/// against — the gatekeeper's protocol is brief-first and has no field for the child's
/// report — so a pane that could not show it would leave the operator reading a verdict
/// against an ask they cannot see. The verdict travels with its reasons, the files it was
/// based on and the commands that were run, because a word without those is an opinion.
#[test]
fn the_overlay_shows_the_ask_the_verdict_was_asked_against() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(
        vec![queue_entry("c1", MergeState::Waiting)],
        vec![queue_review("c1", Some("reject"))],
    ));
    a.key(Key::Enter);
    let screen = a.screen(100, 60).join("\n");
    assert!(screen.contains("the ask it was built from"), "{screen}");
    assert!(screen.contains("make the widget blue"), "{screen}");
    assert!(screen.contains("the reviewer's verdict"), "{screen}");
    assert!(screen.contains("reject"), "{screen}");
    assert!(
        screen.contains("the ask is met and the tests pass"),
        "{screen}"
    );
    assert!(screen.contains("crates/widget.rs"), "{screen}");
    assert!(screen.contains("cargo test -p widget"), "{screen}");
}

/// **`nobody has asked` and `asked and silent` are different facts**, and the pane says
/// which of the two it is looking at. Reading the first as the second would make a queue
/// nobody has looked at look like one that is being looked at now.
#[test]
fn nobody_has_asked_is_not_the_same_as_asked_and_silent() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(
        vec![queue_entry("c1", MergeState::Waiting)],
        Vec::new(),
    ));
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("nobody has reviewed it"), "{screen}");

    a.apply(queue_frame(
        vec![queue_entry("c1", MergeState::Waiting)],
        vec![queue_review("c1", None)],
    ));
    let screen = a.screen(100, 24).join("\n");
    assert!(
        screen.contains("the reviewer has been asked and has not answered"),
        "{screen}"
    );
    assert!(!screen.contains("nobody has reviewed it"), "{screen}");
}

/// **An entry with no review at all says so in the overlay too** — *nobody has asked*, which
/// is the state the gate waits in rather than a review with no verdict.
#[test]
fn an_entry_with_no_review_says_nobody_has_asked() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(
        vec![queue_entry("c1", MergeState::Waiting)],
        Vec::new(),
    ));
    a.key(Key::Enter);
    let screen = a.screen(100, 60).join("\n");
    assert!(screen.contains("nobody has asked"), "{screen}");
    assert!(
        screen.contains("An entry with no review does not land"),
        "{screen}"
    );
}

/// **`a`, `v` and `d` are the person's three verbs** — the operator's ask, in their words: *"i
/// want to be able to approve / veto / delete"*. Each key sends the SAME verb its typed spelling
/// sends, so the key and `/queue approve|veto|rm ID` cannot drift; and a row the daemon would
/// refuse is said rather than sent, `r`'s own rule one state over.
#[test]
fn the_person_verbs_send_the_verb_for_the_row_under_the_cursor() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(
        vec![
            queue_entry("c1", MergeState::Failed),
            queue_entry("c2", MergeState::Vetoed),
            queue_entry("c3", MergeState::Landed),
        ],
        Vec::new(),
    ));
    assert_eq!(
        a.key(Key::Char('a')),
        Some(Action::Slash {
            line: "queue approve c1".into()
        }),
        "`a` sends the same verb `/queue approve ID` sends"
    );
    a.key(Key::Down);
    assert_eq!(
        a.key(Key::Char('v')),
        Some(Action::Slash {
            line: "queue veto c2".into()
        }),
        "`v` sends the child back"
    );
    // **A vetoed row is a person's own decision, and `d` takes it away** — the way out of a
    // decision is a person's, one verb over.
    assert_eq!(
        a.key(Key::Char('d')),
        Some(Action::Slash {
            line: "queue rm c2".into()
        })
    );
    // **A landed row is said, not sent.** The head already knows the daemon refuses it, so a
    // round trip to be told no is a round trip wasted — and the same for a row the queue has
    // taken and is mid-merge on.
    a.key(Key::Down);
    for k in ['a', 'v', 'd'] {
        assert_eq!(a.key(Key::Char(k)), None, "`{k}` on a landed row");
    }
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("is `landed`"), "{screen}");
}

/// **A vetoed entry does not draw as red** — the operator's requirement, and the whole reason a
/// veto is a state rather than a sentence on a `failed` row: a person's decision and a machine's
/// must not look alike.
///
/// Asserted against the FAILED row in the same frame, because *not red* alone would pass on a
/// head that drew no marks at all.
#[test]
fn a_vetoed_entry_does_not_draw_as_red() {
    use letibot_sessionlog::event::MergeState;
    let mut a = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    a.session_id = "s1".into();
    a.command("queue");
    let mut vetoed = queue_entry("c1", MergeState::Vetoed);
    vetoed.evidence = "vetoed by the operator: it lands nothing".into();
    let mut failed = queue_entry("c2", MergeState::Failed);
    failed.evidence = "the gate is red".into();
    a.apply(queue_frame(vec![vetoed, failed], Vec::new()));
    let rows = a.screen(100, 24);
    let red = a.cfg.palette().open(Role::Failure);
    // **Two lines an entry** (rano's shape): the first carries the mark and the state word, the
    // second the id, the review and the reason.
    let vetoed_row = rows
        .iter()
        .find(|l| l.contains("· vetoed ·"))
        .expect("the vetoed row is drawn");
    assert!(
        !vetoed_row.contains(&red),
        "a person's veto is not a failure: {vetoed_row:?}"
    );
    assert!(
        rows.iter().any(|l| l.contains("vetoed by the operator")),
        "and the row says who decided, in their own words: {rows:?}"
    );
    let failed_row = rows
        .iter()
        .find(|l| l.contains("· failed ·"))
        .expect("the failed row is drawn");
    assert!(
        failed_row.contains(&red),
        "and the contrast holds — a gate failure IS the loud one: {failed_row:?}"
    );
}

/// **The pane's keys are on the bar that describes the keys** — four verbs, and a person who has
/// not read the source has to be able to find them.
#[test]
fn the_hint_line_names_the_person_verbs() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let bar = a.hint_bar(160);
    for want in ["a approve", "v veto", "d drop", "r restart"] {
        assert!(bar.contains(want), "the bar names `{want}`: {bar}");
    }
    // And it names them only while the pane has the keys: the bar is what the keys DO now.
    a.command("queue");
    assert!(!a.hint_bar(160).contains("v veto"), "{}", a.hint_bar(160));
}

/// **A removed entry leaves the pane, and its verdict goes with it** — the queue's one event
/// about an ABSENCE, which a head that folded the entry in needs or it goes on drawing a row the
/// queue no longer holds.
#[test]
fn a_removed_entry_leaves_the_pane_and_its_verdict_with_it() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(
        vec![
            queue_entry("c1", MergeState::Failed),
            queue_entry("c2", MergeState::Waiting),
        ],
        vec![queue_review("c1", Some("reject"))],
    ));
    assert_eq!(a.merge.len(), 2, "both rows are here to start with");
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::MergeEntryRemoved {
            id: "c1".into(),
            evidence: "removed from the queue by the operator".into(),
        },
    )));
    assert_eq!(a.merge.len(), 1, "the row is gone");
    assert_eq!(a.merge[0].id, "c2");
    assert!(
        a.review_of("c1").is_none(),
        "and its verdict with it: a verdict for an entry that is not there is a row nothing \
         reads, and the overlay would point at a reviewer of a deleted entry"
    );
    let screen = a.screen(100, 24).join("\n");
    assert!(!screen.contains("reviewer: reject"), "{screen}");
    // **And a removal for an id this head never had is not an entry invented** — the jobs pane's
    // rule, one arrival over.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::MergeEntryRemoved {
            id: "c9".into(),
            evidence: "removed".into(),
        },
    )));
    assert_eq!(a.merge.len(), 1, "nothing was added");
}
