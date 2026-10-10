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

/// **The edge carries the queue's standings** — the operator's ask, in their words: *"hide it
/// to near triangle with the current queue stats - how many in review how many being merged
/// etc"*.
///
/// The counts are the daemon's own states, folded by [`App::queue_standings`], and the words
/// are three: `waiting` is *in review* (the gatekeeper's review is what it waits on), `taken`
/// is *being merged*, and the four the queue has stopped moving by itself are one word —
/// *parked*, which is the count a person has to act on.
#[test]
fn the_bottom_edge_counts_the_queue_by_state() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(queue_frame(
        vec![
            queue_entry("c1", MergeState::Waiting),
            queue_entry("c2", MergeState::Waiting),
            queue_entry("c3", MergeState::Taken),
            queue_entry("c4", MergeState::Failed),
            queue_entry("c5", MergeState::Conflict),
            queue_entry("c6", MergeState::Stale),
            queue_entry("c7", MergeState::Vetoed),
        ],
        Vec::new(),
    ));
    let rows = a.screen(100, 24);
    let screen = rows.join("\n");
    assert!(
        screen.contains("2 in review · 1 being merged · 4 parked"),
        "the machine's three and the person's one are the same standing here — `parked`, \
         the count somebody has to move: {screen}"
    );
    // **On the composer's bottom edge, not in the conversation** — the whole point of the
    // move is that this row is chrome and cannot reflow the transcript.
    let edge = rows
        .iter()
        .find(|l| l.contains("parked"))
        .expect("the standings row");
    assert!(
        edge.contains('╰') && edge.contains('╯'),
        "the standings are on the box's bottom edge, beside the ⚠: {edge:?}"
    );
    assert!(
        !a.notes.iter().any(|(_, n)| matches!(
            n,
            Note::Warned(w) if w.code == "merge_queued"
        )),
        "and nothing was said in the conversation: {:?}",
        a.notes
    );
}

/// **Nothing standing is not a row of zeroes.**
///
/// An empty queue and a queue whose entries have all landed are the same fact about what is
/// left to do — nothing — and the edge says nothing at all for both rather than
/// `0 in review · 0 being merged · 0 parked`, which would be a row of attention paid for ever
/// for a fact nobody has. The disclosure is `/queue`, which says *none* in words.
///
/// **A landed entry is the case that makes this a decision rather than an accident**: it is
/// still in the daemon's queue (a later entry can name it in `needs`), so a head that counted
/// every entry it held would show a row here and a head that folded `landed` into *parked* or
/// *in review* would show a lie about where the work is.
#[test]
fn the_edge_says_nothing_when_nothing_stands() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    let empty = a.screen(100, 24).join("\n");
    assert!(!empty.contains("in review"), "{empty}");

    a.apply(queue_frame(
        vec![
            queue_entry("c1", MergeState::Landed),
            queue_entry("c2", MergeState::Landed),
        ],
        Vec::new(),
    ));
    let landed = a.screen(100, 24).join("\n");
    assert!(
        !landed.contains("in review") && !landed.contains("parked"),
        "a landed entry has no standing and is not folded into one: {landed}"
    );
    // **The positive control**, so the two absences above are a statement about standing and
    // not a head that never draws the line at all: one entry that DOES stand, and the edge
    // says so in the same frame.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::MergeEntryAdded {
            entry: queue_entry("c3", MergeState::Waiting),
        },
    )));
    let standing = a.screen(100, 24).join("\n");
    assert!(
        standing.contains("1 in review · 0 being merged · 0 parked"),
        "the same edge, one entry later: {standing}"
    );
    // **And the pane still draws them**, which is what makes the silence above a statement
    // about standing and not a head that dropped two entries.
    a.command("queue");
    let pane = a.screen(100, 24).join("\n");
    assert!(pane.contains("landed"), "{pane}");
}

/// **The standings line is the same row, byte for byte, while a reply streams** — the
/// operator's report, and the bug this whole change is for: *"merge_queued notification sticks
/// and jumps slightly up when you actively reply and then bottom"*.
///
/// The notice's row was in the conversation, so a row appearing above the composer reflowed
/// the screen mid-reply and reflowed it back when the row landed. The fact lives on the edge
/// now, and the edge is composed from the queue and from nothing about the turn — so the test
/// pins the ROW and its bytes rather than a colour, because a colour that is right on a frame
/// that moved is the defect.
#[test]
fn the_standings_line_does_not_move_when_a_reply_does() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(queue_frame(
        vec![
            queue_entry("c1", MergeState::Waiting),
            queue_entry("c2", MergeState::Failed),
        ],
        Vec::new(),
    ));
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::delta("t1", "here is the "),
    )));
    let first = a.screen(100, 24);
    let at = first
        .iter()
        .position(|l| l.contains("parked"))
        .expect("the standings row");
    assert!(
        first[at].contains('╰') && first[at].contains('╯'),
        "the row this pins is the composer's bottom edge: {:?}",
        first[at]
    );

    a.apply(ServerFrame::Event(env(3, testing::delta("t1", "answer"))));
    let second = a.screen(100, 24);
    assert!(
        second.join("\n").contains("here is the answer"),
        "the premise: the reply grew between the two frames"
    );
    assert_eq!(
        second.iter().position(|l| l.contains("parked")),
        Some(at),
        "the standings moved down the screen while the reply grew: {}",
        second.join("\n")
    );
    assert_eq!(
        first[at], second[at],
        "the standings line changed while only the reply did"
    );
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

// ===== The merge half: the gate's steps =====

/// **The gate's steps draw in order, with their marks, and the landing under them** — the merge
/// half of the queue, which is the half that can be drawn well because *"a thing that is being run
/// can be drawn well"*, and the operator's complaint: *"it doesnt show me review queue state
/// transitions"*.
#[test]
fn the_gates_steps_draw_in_order_with_their_marks() {
    use letibot_sessionlog::event::{MergeGateOutcome, MergeState};
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    // **The merge half is its own view now** — `tab` moves to it, and this is the machine's.
    a.key(Key::Tab);
    let mut e = queue_entry("c1", MergeState::Landed);
    e.gate_steps = vec![
        gate_step(
            "sh scripts/check-fmt.sh main",
            MergeGateOutcome::Passed,
            "all formatted",
            1_200,
        ),
        gate_step(
            "cargo clippy --all-targets",
            MergeGateOutcome::Passed,
            "",
            4_000,
        ),
        gate_step(
            "cargo test --workspace --no-fail-fast",
            MergeGateOutcome::Passed,
            "test result: ok",
            61_000,
        ),
    ];
    e.landed_sha = Some("deadbee".into());
    a.apply(queue_frame(vec![e], Vec::new()));
    let rows = a.screen(140, 40);
    let at = |needle: &str| {
        rows.iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no row for {needle:?}: {rows:#?}"))
    };
    let first = at("sh scripts/check-fmt.sh main");
    let second = at("cargo clippy --all-targets");
    let third = at("cargo test --workspace --no-fail-fast");
    assert!(
        first < second && second < third,
        "the steps are drawn in the order `main` declared them: {rows:#?}"
    );
    // The mark is on the step's own row, and a step that ran green carries the tick.
    for needle in [
        "sh scripts/check-fmt.sh main",
        "cargo clippy --all-targets",
        "cargo test --workspace --no-fail-fast",
    ] {
        assert!(
            rows[at(needle)].contains('✓'),
            "{needle} is not marked green: {:?}",
            rows[at(needle)]
        );
    }
    // **The machine's own clock**, which is what tells a step that took a minute from one that
    // took a second — and, with the outcome, a real `0 ms` from a step that never ran.
    assert!(rows[first].contains("1.2s"), "{:?}", rows[first]);
    // **Where the branch ended up**, on the row the merge half is drawn on.
    assert!(
        rows.iter().any(|l| l.contains("landed deadbee")),
        "{rows:#?}"
    );
    // And the one sentence this entry must NOT carry: its gate has run.
    assert!(
        !rows.iter().any(|l| l.contains("the gate has not run")),
        "{rows:#?}"
    );
}

/// **A failing step shows its own output tail, and the steps the gate never reached say so** —
/// the step, what it printed and where it stopped, and the row that must not read as green: a
/// gate that stopped at the first failure is not a gate that ran everything.
#[test]
fn a_failing_step_shows_its_output_and_the_steps_after_it_did_not_run() {
    use letibot_sessionlog::event::{MergeGateOutcome, MergeState};
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    // The merge view, where the gate's rows are (`tab`).
    a.key(Key::Tab);
    let mut e = queue_entry("c1", MergeState::Failed);
    e.evidence = "the gate stopped at the second step".into();
    e.gate_steps = vec![
        gate_step(
            "sh scripts/check-fmt.sh main",
            MergeGateOutcome::Passed,
            "all formatted",
            1_200,
        ),
        gate_step(
            "cargo test --workspace --no-fail-fast",
            MergeGateOutcome::Failed,
            "… [4096 byte(s) dropped from the front]\nrunning 3 tests\n\
             thread 'a' panicked at src/lib.rs:9:\nassertion failed: 2 == 3\ntest result: FAILED",
            30_000,
        ),
        gate_step(
            "cargo build --release --bins",
            MergeGateOutcome::NotRun,
            "",
            0,
        ),
    ];
    a.apply(queue_frame(vec![e], Vec::new()));
    let rows = a.screen(140, 40);
    let failed = rows
        .iter()
        .position(|l| l.contains("cargo test --workspace --no-fail-fast"))
        .expect("the red step's row");
    assert!(rows[failed].contains('✗'), "{:?}", rows[failed]);
    // **The step's own words, and the END of them** — the last line is where it stopped.
    assert!(
        rows.iter()
            .any(|l| l.contains("thread 'a' panicked at src/lib.rs:9")),
        "the output is not drawn: {rows:#?}"
    );
    assert!(
        rows.iter().any(|l| l.contains("test result: FAILED")),
        "and not its end: {rows:#?}"
    );
    // **The step the gate never reached is drawn, in order, and it is not green.**
    let skipped = rows
        .iter()
        .position(|l| l.contains("cargo build --release --bins"))
        .expect("a step that never ran is still on the entry");
    assert!(skipped > failed, "the steps are in order: {rows:#?}");
    assert!(rows[skipped].contains("not run"), "{:?}", rows[skipped]);
    assert!(
        !rows[skipped].contains('✓'),
        "a step that never ran is not a step that was green: {:?}",
        rows[skipped]
    );
}

/// **An empty step list is *the gate has not run on this entry*** — what a daemon from before the
/// field sends, what an entry still waiting for its reviewer carries, and what a vetoed one
/// carries — and it must never read as *all steps passed*: a blank checklist under an entry is the
/// lie the empty list is drawn as a sentence to prevent.
#[test]
fn an_empty_step_list_reads_as_the_gate_has_not_run() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    // The merge view: *the gate has not run* is a sentence about the gate, and the review view
    // draws none of those.
    a.key(Key::Tab);
    a.apply(queue_frame(
        vec![queue_entry("c1", MergeState::Waiting)],
        Vec::new(),
    ));
    let rows = a.screen(120, 40);
    assert!(
        rows.iter()
            .any(|l| l.contains("the gate has not run on this entry")),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|l| l.contains('✓')),
        "an empty list is not a green gate: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|l| l.contains(" · not run")),
        "and it is not *a step that did not run* either — nothing was declared: {rows:#?}"
    );

    // **An entry the queue has taken says the merge is RUNNING.** The rows are written by the move
    // that ENDS a run, so a `taken` entry has none of this run's, and *running* is the honest
    // reading of the empty list there rather than *the gate has not run*.
    a.apply(queue_frame(
        vec![queue_entry("c2", MergeState::Taken)],
        Vec::new(),
    ));
    let rows = a.screen(120, 40);
    assert!(
        rows.iter().any(|l| l.contains("the merge is running")),
        "{rows:#?}"
    );
    assert!(!rows.iter().any(|l| l.contains('✓')), "{rows:#?}");
}

/// **A `no_gate` row is a repository with no gate declared** — one row rather than an empty list,
/// because *the gate declared nothing to run* and *the gate has not run* are different facts. It is
/// the honest and actionable case (the queue holds the branch until somebody writes the section),
/// so the queue's own sentence is on it, and it is never a step that passed.
#[test]
fn a_no_gate_row_reads_as_no_gate_declared() {
    use letibot_sessionlog::event::{MergeGateOutcome, MergeState};
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    // The merge view, where a `no_gate` row is drawn.
    a.key(Key::Tab);
    let mut e = queue_entry("c1", MergeState::Waiting);
    e.gate_steps = vec![gate_step(
        "",
        MergeGateOutcome::NoGate,
        "`main` has no `Merge gate` section in AGENTS.md, so there is no step to run",
        0,
    )];
    a.apply(queue_frame(vec![e], Vec::new()));
    let rows = a.screen(160, 40);
    assert!(
        rows.iter().any(|l| l.contains("no gate declared")),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|l| l.contains("no `Merge gate` section in AGENTS.md")),
        "the queue's own sentence is on the row: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|l| l.contains('✓') || l.contains('✗')),
        "a repository with no gate is not a gate that passed everything: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|l| l.contains("the gate has not run")),
        "and it is a row rather than the empty list: {rows:#?}"
    );
}

/// **The overlay is where the run is readable whole** — rano's overlay draws the entry, the ask
/// and the reviewer's verdict; the gate's steps and every line of the red step's output are under
/// them, wrapped rather than cut, because an overlay is read and scrolled rather than glanced at.
#[test]
fn the_overlay_draws_the_gates_steps_and_the_whole_output() {
    use letibot_sessionlog::event::{MergeGateOutcome, MergeState};
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut e = queue_entry("c1", MergeState::Failed);
    e.gate_steps = vec![
        gate_step(
            "sh scripts/check-fmt.sh main",
            MergeGateOutcome::Passed,
            "all formatted",
            1_000,
        ),
        gate_step(
            "cargo test --workspace",
            MergeGateOutcome::Failed,
            "first line of the tail\nsecond line\nthird line\nfourth line\nfifth line — where it stopped",
            9_000,
        ),
    ];
    a.apply(queue_frame(vec![e], Vec::new()));
    assert_eq!(a.key(Key::Enter), None);
    let screen = a.screen(120, 60).join("\n");
    assert!(screen.contains("the gate's steps"), "{screen}");
    assert!(
        screen.contains("✓ sh scripts/check-fmt.sh main"),
        "{screen}"
    );
    // **The lines the pane's own window cuts away** — the overlay draws all of the tail.
    assert!(screen.contains("first line of the tail"), "{screen}");
    assert!(screen.contains("fifth line — where it stopped"), "{screen}");
}

// ===== The two views: the review queue and the merge queue =====

/// **The merge view draws only the gate's rows** — the operator's ask, verbatim: *"nor there are
/// separate views for review and merge queues"*. The list is the same list (one entry, its branch
/// and its state word), and the row under it is the machine's: the steps, their marks, and where
/// the branch landed. **The tutor's row is not there** — it is the review view's, and the entry's
/// evidence under a gate row is the one list the operator asked to have split.
#[test]
fn the_merge_view_draws_only_the_gates_rows() {
    use letibot_sessionlog::event::{MergeGateOutcome, MergeState};
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut e = queue_entry("c1", MergeState::Landed);
    e.evidence = "the queue's own words".into();
    e.gate_steps = vec![gate_step(
        "sh scripts/check-fmt.sh main",
        MergeGateOutcome::Passed,
        "all formatted",
        1_200,
    )];
    e.landed_sha = Some("deadbee".into());
    a.apply(queue_frame(
        vec![e],
        vec![queue_review("c1", Some("accept"))],
    ));
    // Where the pane opens: the review view, and the tutor's row is on it.
    let review = a.screen(120, 40).join("\n");
    assert!(review.contains("reviewer: accept"), "{review}");
    assert!(
        !review.contains('✓'),
        "the review view drew a gate step: {review}"
    );
    // `tab` moves to the machine's half.
    assert_eq!(
        a.key(Key::Tab),
        None,
        "`tab` is the pane's, not the composer's"
    );
    let merge = a.screen(120, 40).join("\n");
    assert!(merge.contains("✓ sh scripts/check-fmt.sh main"), "{merge}");
    assert!(merge.contains("landed deadbee"), "{merge}");
    // The same list either side of the switch: the entry's own state word is still on it.
    assert!(merge.contains("· landed ·"), "{merge}");
    assert!(
        !merge.contains("reviewer: accept") && !merge.contains("the queue's own words"),
        "the merge view drew the tutor's row: {merge}"
    );
}

/// **The review view draws only the tutor's** — the verdict rano draws, the ask it was asked
/// against, and the four verbs a person has on the entry under the cursor, each with what it would
/// do. **No gate row**: not a step, not `not run`, and not the sentence an empty list draws. A view
/// that leaked the machine's rows would be the one list the operator asked to have split.
#[test]
fn the_review_view_draws_only_the_tutors() {
    use letibot_sessionlog::event::{MergeGateOutcome, MergeState};
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut e = queue_entry("c1", MergeState::Waiting);
    e.gate_steps = vec![gate_step(
        "cargo clippy --all-targets",
        MergeGateOutcome::Passed,
        "",
        4_000,
    )];
    a.apply(queue_frame(
        vec![e],
        vec![queue_review("c1", Some("accept"))],
    ));
    let screen = a.screen(120, 40).join("\n");
    assert!(screen.contains("reviewer: accept"), "{screen}");
    // **The ask the verdict was asked against**, which is what the verdict is a verdict ON.
    assert!(screen.contains("ask   make the widget blue"), "{screen}");
    for verb in ["a approve", "v veto", "r restart", "d rm"] {
        assert!(
            screen.contains(verb),
            "the row does not name `{verb}`: {screen}"
        );
    }
    // **What each would do**, read across the wrap the row is drawn with: the verbs are one
    // paragraph and the sentences break at spaces, so the words are joined before they are matched.
    let flat = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("the gate still runs"),
        "`a approve` does not say the gate still runs: {screen}"
    );
    assert!(
        flat.contains("the child is sent back"),
        "`v veto` does not say where the ball goes: {screen}"
    );
    for machine in ["✓", "✗", " · not run", "the gate has not run"] {
        assert!(
            !screen.contains(machine),
            "the review view drew the machine's `{machine}`: {screen}"
        );
    }
}

/// **An entry whose state has no merge half says so rather than showing an empty column** — a
/// `waiting` entry has no gate rows, so the merge view says *the gate has not run on this entry*
/// rather than drawing nothing under the entry, which is what reads as *all steps passed*. The
/// list above it is unchanged: the entry is still there with its state word.
#[test]
fn the_merge_view_says_when_an_entry_has_no_merge_half() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(
        vec![queue_entry("c1", MergeState::Waiting)],
        Vec::new(),
    ));
    a.key(Key::Tab);
    let rows = a.screen(120, 40);
    let screen = rows.join("\n");
    assert!(
        screen.contains("the gate has not run on this entry"),
        "{screen}"
    );
    assert!(
        rows.iter().any(|l| l.contains("· waiting ·")),
        "the list is the same list: {rows:#?}"
    );
    assert!(
        !screen.contains('✓') && !screen.contains('✗'),
        "an empty merge half is not a green gate: {screen}"
    );
    // **And an entry the queue has TAKEN is running**, not *has not run*: the rows ride the move
    // that ends a run, so an empty list under a `taken` entry is this run's.
    a.apply(queue_frame(
        vec![queue_entry("c2", MergeState::Taken)],
        Vec::new(),
    ));
    let taken = a.screen(120, 40).join("\n");
    assert!(taken.contains("the merge is running"), "{taken}");
}

/// **The thing that moves between the views does what its label says** — the strip names both
/// views, brackets the one that is on, and `tab` moves to the other; a click on a label is the
/// same move. **And the cursor does not move**: the two views are one list, so a reader who was on
/// the second entry is still on it — the defect the stop-row record exists to prevent, one view
/// over.
#[test]
fn the_strip_says_which_view_is_on_and_moves_between_them() {
    use letibot_sessionlog::event::MergeState;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut second = queue_entry("c2", MergeState::Waiting);
    second.branch = "agent/child-two".into();
    a.apply(queue_frame(
        vec![queue_entry("c1", MergeState::Waiting), second],
        Vec::new(),
    ));
    let strip = |rows: &[String]| {
        rows.iter()
            .find(|l| l.contains("tab switches"))
            .cloned()
            .expect("the pane draws the switch")
    };
    let rows = a.screen(120, 40);
    assert!(
        strip(&rows).contains("[review] merge"),
        "{:?}",
        strip(&rows)
    );
    assert!(rows.iter().any(|l| l.contains("nobody has reviewed it")));

    // The cursor on the second entry; `tab` moves the view and leaves the cursor where it was.
    a.key(Key::Down);
    assert!(
        a.screen(120, 40)
            .iter()
            .any(|l| l.contains('▸') && l.contains("agent/child-two")),
        "the cursor did not reach the second entry"
    );
    assert_eq!(a.key(Key::Tab), None);
    let rows = a.screen(120, 40);
    assert!(
        strip(&rows).contains("review [merge]"),
        "{:?}",
        strip(&rows)
    );
    assert!(
        rows.iter()
            .any(|l| l.contains('▸') && l.contains("agent/child-two")),
        "the switch moved the cursor off its entry: {rows:#?}"
    );

    // **And the label is a control**: a click on `review` is the move the key makes. The row is
    // the frame's own record, and `x` is a terminal cell — the gutter is two columns at this
    // width, so the label starts at four.
    let at = rows
        .iter()
        .position(|l| l.contains("tab switches"))
        .expect("the strip row") as u16;
    a.key(Key::Click { x: 5, y: at });
    let rows = a.screen(120, 40);
    assert!(
        strip(&rows).contains("[review] merge"),
        "{:?}",
        strip(&rows)
    );
    assert!(rows.iter().any(|l| l.contains("nobody has reviewed it")));
    assert!(
        rows.iter()
            .any(|l| l.contains('▸') && l.contains("agent/child-two")),
        "a click on the strip is not a click on an entry: {rows:#?}"
    );
}
