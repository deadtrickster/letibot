//! Subagents: the pane, the tree, a child's output.

use super::*;

/// A card that is a subagent's call says so and names the child: a child has no head, so its card
/// arrives on this session's screen, and the question and the ladder are the same either way - the
/// clause is the only thing that says whose call it is.
#[test]
fn the_card_says_when_the_call_is_a_subagents_and_names_it() {
    use letibot_sessionlog::event::{OptionKind, SubagentAsk};
    let a = app();

    // This session's own call: no clause at all. A `None` that still drew a line would be
    // furniture on every ordinary card.
    let mine = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    let drawn = a.decision_lines(&mine, 100).join("\n");
    assert!(!drawn.contains("a subagent's call"), "{drawn}");

    let mut child = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    child.subagent = Some(SubagentAsk {
        handle: "s-sub-3".into(),
        task: "count the rows the store never reads".into(),
        root: "s-root".into(),
    });
    let drawn = a.decision_lines(&child, 100).join("\n");
    // The handle - what `task_result` collects by and what a head attaches to - and the
    // task, so two children of one session are told apart.
    assert!(drawn.contains("a subagent's call — s-sub-3"), "{drawn}");
    assert!(
        drawn.contains("count the rows the store never reads"),
        "{drawn}"
    );
    // **Above the ladder**, so it is read on the same pass of the eye as the question:
    // under the wall it would be read after the answer was already chosen.
    let clause = drawn.find("a subagent's call").unwrap();
    let ladder = drawn.find("allow_once").unwrap();
    assert!(clause < ladder, "{drawn}");
    // In the faint register, like every other clause on this card. Checked on a head that
    // emits colour, since these layout tests deliberately do not.
    let painted = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    let line = painted
        .decision_lines(&child, 100)
        .into_iter()
        .find(|l| l.contains("a subagent's call"))
        .expect("the row");
    assert!(line.contains(sgr::DIM), "not in the dim register: {line:?}");
    // A child whose task the daemon never sent draws the handle alone, rather than a
    // dangling separator after it.
    let mut bare = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    bare.subagent = Some(SubagentAsk {
        handle: "s-sub-4".into(),
        task: String::new(),
        root: "s-root".into(),
    });
    let drawn = a.decision_lines(&bare, 100).join("\n");
    assert!(drawn.contains("a subagent's call — s-sub-4"), "{drawn}");
    assert!(
        !drawn.contains("— s-sub-4 ·"),
        "a task nobody sent was invented: {drawn}"
    );
}

#[test]
fn ctrl_g_opens_the_subagent_tree_and_esc_closes_it() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "summarize ~/bin/letibot".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    // A spawn that the head saw. The running count lives in the pane  -
    // the composer's border used to repeat it, and that border is plain now.
    assert_eq!(a.subagents.len(), 1);

    a.key(Key::CtrlG);
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("subagents"), "{screen}");
    assert!(screen.contains("summarize ~/bin/letibot"), "{screen}");
    assert!(screen.contains("running"), "{screen}");

    a.key(Key::Esc);
    assert!(!a.is_open(Surface::Subagents));
}

/// A fresh head rebuilds the subagent pane AND the count from the daemon's own session list, with
/// no spawn event: `SessionEvent::Subagent` carries one child, so it cannot re-list an earlier
/// generation, and `SessionBrief::parent_session_id` is the durable half every `Hello` carries.
#[test]
fn a_fresh_head_rebuilds_the_subagent_rows_and_the_count_from_the_session_list() {
    let mut a = app();
    // Not one `Subagent` event is applied anywhere in this test.
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));

    // The children, in the daemon's order, and neither the stranger's child nor the
    // parent's own row.
    let ids: Vec<&str> = a.subagents.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(ids, vec!["s-sub-1", "s-sub-2"], "{ids:?}");

    // **The count**, which is the same defect: `running` is the list's own fact about
    // whether a turn is generating in that session right now.
    let screen = a.screen(100, 24);
    let row = screen
        .iter()
        .find(|l| l.contains("subagent"))
        .expect("the count is on the screen");
    assert!(row.contains("1 subagent running"), "{row}");

    // **The pane**, with the subtask the daemon named it by.
    a.key(Key::CtrlG);
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("find the bug in the reader"), "{screen}");
    assert!(screen.contains("running"), "{screen}");
    assert!(
        !screen.contains("someone else's child"),
        "another conversation's child is in this session's tree:\n{screen}"
    );
    // **The finished child is folded, and the pane says how many** - the group the operator
    // asked for. `s-sub-2` is `running: false` in the list, so it is a finished row.
    assert!(screen.contains("finished (1)"), "{screen}");
    assert!(
        !screen.contains("audit the store"),
        "a finished child is drawn though its group is folded:\n{screen}"
    );
    // **And the state word is not invented.** The list says whether a turn is generating;
    // it says nothing about how a settled child ended, so the row this head did not watch
    // says so rather than claiming `done` - unfolded, where a `done` would be a lie.
    a.key(Key::Down);
    a.key(Key::Enter);
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("audit the store"), "{screen}");
    assert!(screen.contains("state unknown"), "{screen}");
}

/// After a daemon restart the pane says how each child ended, from the store, and in words rather
/// than markdown: the `Subagent` event lived in the parent's memory and went with the daemon,
/// while the child's stored conversation still has a last row to answer with - or not.
#[test]
fn a_rebuilt_child_says_how_its_stored_conversation_ended() {
    use letibot_sessionlog::StoredEnd;
    let child = |id: &str, title: &str, live: bool, end: Option<StoredEnd>| {
        let mut b = brief(id, title, false);
        b.parent_session_id = Some("s".into());
        b.live = live;
        b.stored_end = end;
        b
    };
    let family = vec![
        brief("s", "parent", false),
        child(
            "s-sub-answered",
            "**Fix the pane.** The `[?]` rows",
            false,
            Some(StoredEnd::Answered {
                first_line: "Done. **Report:**".into(),
            }),
        ),
        child(
            "s-sub-cut",
            "cut off by a restart",
            false,
            Some(StoredEnd::MidTurn),
        ),
        child(
            "s-sub-parked",
            "parked on its own job",
            true,
            Some(StoredEnd::MidTurn),
        ),
    ];
    let mut a = app();
    a.apply(hello("s", family, Hub::new("s").snapshot()));
    a.key(Key::CtrlG);
    a.key(Key::Enter);
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("finished (3)"), "{screen}");
    assert!(
        screen.contains("[x] Fix the pane. The [?] rows"),
        "an answered child is done, and its title is words:\n{screen}"
    );
    assert!(
        screen.contains("done · Done. Report:"),
        "the answer is the subtitle, in words:\n{screen}"
    );
    assert!(
        screen.contains("stopped mid-turn"),
        "a child cut off in a session the daemon no longer holds says so:\n{screen}"
    );
    // A live child between rounds is not called stopped: its last row is a result because
    // it is waiting on something, and the list's `running: false` is not an ending.
    let parked = screen
        .lines()
        .skip_while(|l| !l.contains("parked on its own job"))
        .nth(1)
        .unwrap_or_default();
    assert!(parked.contains("state unknown"), "{parked}\n{screen}");
    assert!(
        !screen.contains("**"),
        "markdown markers in the pane:\n{screen}"
    );
}

/// The one that is running is at the TOP of the pane, not below the fold. It is not a delay: the
/// daemon publishes `opening` at the spawn, so the row is on the wire at once, and it landed below
/// the fold because a watched spawn is appended after the durable rows.
#[test]
fn the_running_subagent_is_drawn_above_the_folded_finished_ones() {
    let mut a = app();
    let mut fam = vec![brief("s", "parent", false)];
    for i in 0..26 {
        let mut b = brief(&format!("s-sub-{i:02}"), &format!("finished {i}"), false);
        b.parent_session_id = Some("s".into());
        fam.push(b);
    }
    a.apply(hello("s", fam, Hub::new("s").snapshot()));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-run".into(),
            state: "running".into(),
            prompt: "the one that is running".into(),
            role: "coder".into(),
            task: "the one that is running".into(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    a.key(Key::CtrlG);
    let screen = a.screen(214, 60);
    let joined = screen.join("\n");
    assert!(joined.contains("the one that is running"), "{joined}");
    assert!(joined.contains("finished (26)"), "{joined}");
    // **At the TOP** - the running child's row is above the group row, so it is the first
    // thing on the pane rather than the twenty-seventh.
    let run_at = screen
        .iter()
        .position(|l| l.contains("the one that is running"))
        .expect("the running child's row");
    let fold_at = screen
        .iter()
        .position(|l| l.contains("finished (26)"))
        .expect("the group row");
    assert!(
        run_at < fold_at,
        "the running child is below the fold:\n{joined}"
    );
    // And the finished ones are not drawn, which is what makes the whole list fit.
    assert!(!joined.contains("finished 25"), "{joined}");
}

/// The acceptance test: Enter into a subagent, then a single Esc back up to the subagents list -
/// one keystroke down, one up, the parent's list on the screen when you land, and the label saying
/// whose child you were in while you were there.
#[test]
fn enter_into_a_subagent_and_one_esc_goes_back_up_to_the_list_it_came_from() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.key(Key::CtrlG);

    // **Down a level: Enter IS the switch.**
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
    assert!(
        !a.is_open(Surface::Subagents),
        "switching closes the pane it was opened from"
    );
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    assert_eq!(a.session_id, "s-sub-1");
    assert!(
        a.subagents.is_empty(),
        "a subagent's own session shows its own children, not its parent's"
    );
    // And the COUNT is the same list, which is the reason the clear exists: a carried row put
    // `1 subagent running` on the composer of the very subagent being looked at. The rebuild is
    // filtered by `parent_session_id`, so a head standing in the child draws none of them.
    let screen = a.screen(100, 24).join("\n");
    assert!(
        !screen.contains("subagent running"),
        "the parent's running child is counted on the child's own composer:\n{screen}"
    );

    // **The ONE piece of info: this is a subagent, and whose.** On the row that already
    // names the session - the header - and nothing else added anywhere.
    assert!(
        screen.contains("subagent of parent"),
        "the header does not say whose child this is:\n{screen}"
    );

    // **And one Esc goes back up**, to the parent, with the list open.
    assert_eq!(
        a.key(Key::Esc),
        Some(Action::Switch("s".into())),
        "a single Esc did not climb out of the subagent"
    );
    assert!(
        a.is_open(Surface::Subagents),
        "esc goes up TO the subagents list"
    );

    // The parent's `Hello` - which is what a switch is answered with - carries the
    // list, so the pane and the count are rebuilt rather than wait for an event.
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    assert_eq!(a.session_id, "s");
    assert_eq!(a.subagents.len(), 2, "the list is back");
    assert_eq!(
        a.subagents[a.subagents_sel].session_id, "s-sub-1",
        "the cursor is not on the child this head came out of"
    );
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("find the bug in the reader"), "{screen}");
    assert!(screen.contains("1 subagent running"), "{screen}");
}

/// Esc's other meanings inside a subagent, and the two it gave up: the descent arm sits below every
/// arm that already owns Esc, so a pane closes, a half-typed line keeps Esc for the composer, and
/// an open decision keeps it away from the tree.
#[test]
fn esc_keeps_every_other_meaning_it_has_while_inside_a_subagent() {
    // A pane on the screen owns Esc - its own footer says `esc closes`, and it wins.
    let mut a = inside_a_subagent();
    a.key(Key::CtrlG);
    assert_eq!(a.key(Key::Esc), None);
    assert!(!a.is_open(Surface::Subagents), "esc closed the pane");
    assert_eq!(
        a.session_id, "s-sub-1",
        "esc also switched out of the session"
    );

    // **A half-typed line goes up WITH the operator** - it used to keep Esc for the composer,
    // which armed the interrupt, and the next Esc stopped the child the operator was only
    // leaving (2026-10-08). The composer is the head's, so the draft is still there above.
    let mut a = inside_a_subagent();
    a.clock(1_000);
    a.key(Key::Char('x'));
    assert_eq!(a.key(Key::Esc), Some(Action::Switch("s".into())));
    assert_eq!(a.editor.text(), "x", "the draft goes up with the operator");

    // A decision on the screen keeps the operator in the child - said, and NOT armed: a
    // second Esc there interrupts nothing.
    let mut a = inside_a_subagent();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.open.push(decision_with(&[
        letibot_sessionlog::event::OptionKind::AllowOnce,
    ]));
    // The first Esc puts the card away (R90.5) and the second is kept from leaving with it
    // unanswered (R90.15) - and neither arms the interrupt.
    assert_eq!(a.key(Key::Esc), None);
    assert_eq!(a.session_id, "s-sub-1");
    assert!(a.open.is_empty() && a.away.len() == 1, "put away");
    a.clock(1_100);
    assert_eq!(a.key(Key::Esc), None);
    assert_eq!(a.session_id, "s-sub-1");
    let screen = a.screen(120, 30).join("\n");
    // In the status row after the turn's clock, cut there and whole in the notice.
    assert!(screen.contains("a decision put away"), "{screen}");
    let notice = a.notice.clone().unwrap_or_default();
    assert!(notice.contains("answer it, then esc goes up"), "{notice}");
    a.clock(1_200);
    assert_eq!(a.key(Key::Esc), None);
    assert!(
        !a.take_actions()
            .iter()
            .any(|x| matches!(x, Action::Interrupt(_))),
        "two Escs in a subagent interrupted it"
    );

    // And a conversation at the top of the tree has no parent to go to, so Esc is
    // exactly what it was there - the arm cannot fire at all.
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    assert!(a.parent_session().is_none());
    assert_eq!(a.key(Key::Esc), None, "one press arms, it does not fire");
    assert!(a.hint_bar(120).contains("again"), "{}", a.hint_bar(120));
}

/// The conflict, pinned rather than taken in silence: inside a subagent one Esc goes up, so Esc-Esc
/// no longer interrupts that child there. The child's turn is still interruptible with
/// `/interrupt`, and from the parent with `job_kill`.
#[test]
fn esc_inside_a_busy_subagent_goes_up_and_the_pair_does_not_interrupt_it_there() {
    let mut a = inside_a_subagent();
    a.clock(1_000);
    // The child is mid-turn, which is the state the pair exists for.
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    assert!(a.turn_busy(), "the child is not busy, so this pins nothing");
    assert_eq!(a.key(Key::Esc), Some(Action::Switch("s".into())));
    // The second press lands in the parent, where there is no pair to fire: the editor
    // arms and nothing is interrupted.
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    assert_eq!(a.key(Key::Esc), None);
    assert!(
        !a.take_actions()
            .iter()
            .any(|a| matches!(a, Action::Interrupt(_))),
        "the pair interrupted a session the operator had already left"
    );
}

/// ctrl-s inside a subagent is its family: the parent and its siblings, drawn as the tree it
/// already is. The siblings are shown whatever the collapse state says, because a family view
/// whose members were folded away would be a list of one row.
#[test]
fn ctrl_s_inside_a_subagent_lists_the_parent_and_its_siblings() {
    let mut a = inside_a_subagent();
    let rows = a.session_rows();
    let shown: Vec<(&str, usize)> = rows
        .iter()
        .map(|r| {
            let i = r.idx.expect("a family view has no group lines");
            (a.sessions[i].session_id.as_str(), r.depth)
        })
        .collect();
    assert_eq!(
        shown,
        vec![("s", 0), ("s-sub-1", 1), ("s-sub-2", 1)],
        "{shown:?}"
    );
    // Nothing was expanded and the siblings are on the list anyway.
    assert!(a.expanded.is_empty());
    // And the screen says the same thing the enumeration does - one list, read by the
    // drawings and by the keys.
    a.key(Key::CtrlS);
    let screen = a.screen(100, 30).join("\n");
    assert!(screen.contains("parent"), "{screen}");
    assert!(screen.contains("audit the store"), "{screen}");
    assert!(
        !screen.contains("other conversation"),
        "another conversation's tree is in a subagent's session list:\n{screen}"
    );
}

/// The one word the daemon's list is authoritative for, in both directions: `state`. A row this
/// head watched keeps every richer field, but a `false` retires a `running` it still claims and a
/// `true` overrides a settled row. `fold_subagents` runs at every `load`, so neither wins twice.
#[test]
fn the_session_list_owns_the_word_running_and_the_event_owns_everything_else() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "parent", false)],
        Hub::new("s").snapshot(),
    ));
    let watched = |state: &str, answer: Option<&str>| SessionEvent::Subagent {
        subagent_id: "s-sub-1".into(),
        state: state.into(),
        prompt: "find the bug".into(),
        role: "coder".into(),
        task: "audit the store".into(),
        model: "qwen-3.8-flash-next".into(),
        answer: answer.map(str::to_string),
        slug: String::new(),
    };
    a.apply(ServerFrame::Event(env(
        1,
        watched("done", Some("3529 files")),
    )));
    assert_eq!(a.subagents[0].state, "done");

    // **The list says a turn is generating in that child.** The word is the list's, and the
    // row's own richer facts survive it - a child asked for more work is a running child.
    let mut live = a_family();
    live[1].status.running = true;
    a.apply(hello("s", live, Hub::new("s").snapshot()));
    assert_eq!(
        a.subagents[0].state, "running",
        "the list's live word lost to a row"
    );
    assert_eq!(a.subagents[0].role, "coder", "the event's row was replaced");
    assert_eq!(a.subagents[0].task, "audit the store");
    assert_eq!(a.subagents[0].answer.as_deref(), Some("3529 files"));

    // **And the other way round: the child has stopped, and the row still claims it is
    // running.** The finish happened before the list was cut, so the list is the later
    // word - and the count above the composer stops lying about a child that is idle.
    let mut stopped = a_family();
    stopped[1].status.running = false;
    a.apply(hello("s", stopped, Hub::new("s").snapshot()));
    assert_eq!(
        a.subagents[0].state, "",
        "a stale `running` outlived the list that denied it"
    );
    assert!(
        !a.screen(100, 24).join("\n").contains("subagent running"),
        "a child that is not generating is counted as running"
    );
    // What the list cannot speak to is kept as the event said it: the answer was `done`.
    assert_eq!(a.subagents[0].answer.as_deref(), Some("3529 files"));
}

/// The subagent tree is the parent's fact: a head standing in a child draws neither the parent's
/// running rows nor the row of the session it is looking at, and on the way back the daemon
/// replays the parent's retained events after `Hello`, so the tree is rebuilt from the same events.
#[test]
fn switching_into_a_subagent_drops_the_parents_tree_and_coming_back_rebuilds_it() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "parent", true)],
        Hub::new("s").snapshot(),
    ));
    let spawn = SessionEvent::Subagent {
        subagent_id: "s-sub-1".into(),
        state: "running".into(),
        prompt: "find the bug".into(),
        role: "coder".into(),
        task: String::new(),
        model: String::new(),
        answer: None,
        slug: String::new(),
    };
    a.apply(ServerFrame::Event(env(1, spawn.clone())));
    a.key(Key::CtrlG);
    // **`o` is the alias and Enter is the key**, and both are the same act - the switch.
    // This test reads the pane's key for the way IN; the way here is the older spelling.
    assert_eq!(
        a.key(Key::Char('o')),
        Some(Action::Switch("s-sub-1".into()))
    );
    assert!(!a.is_open(Surface::Subagents), "switching closes the pane");

    a.apply(hello(
        "s-sub-1",
        vec![brief("s", "parent", true)],
        Hub::new("s-sub-1").snapshot(),
    ));
    assert_eq!(a.session_id, "s-sub-1");
    assert!(a.subagents.is_empty(), "the parent's tree came along");
    let screen = a.screen(100, 24).join("\n");
    assert!(!screen.contains("subagent running"), "{screen}");

    // Back to the parent: Hello, then the replayed backlog.
    a.apply(hello(
        "s",
        vec![brief("s", "parent", true)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(1, spawn)));
    assert_eq!(a.subagents.len(), 1);
    a.key(Key::CtrlG);
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("find the bug"), "{screen}");
}

/// A child that is still copying its workspace or booting its VM is listed as
/// `opening`, and Enter on it goes nowhere - there is no session to go to.
#[test]
fn an_opening_subagent_is_listed_but_cannot_be_entered() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "parent", true)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "opening".into(),
            prompt: "find the bug".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    a.key(Key::CtrlG);
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("not attachable yet"), "{screen}");
    assert_eq!(
        a.key(Key::Enter),
        None,
        "Enter switched into a session that is not open"
    );
    assert_eq!(
        a.key(Key::Char('o')),
        None,
        "`o` switched into a session that is not open"
    );
    assert_eq!(
        a.key(Key::Char('p')),
        None,
        "`p` read a session that is not open"
    );
    assert!(
        a.is_open(Surface::Subagents),
        "the pane stays where the operator was"
    );
    assert!(a.sub_out_pending.is_none(), "nothing was asked for");

    // Open now: the same row, and Enter goes there.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "find the bug".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    assert_eq!(a.subagents.len(), 1);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
    // ...and that closed the pane, because the head is leaving the session the pane
    // belongs to. Put it back for the alias and the read, which are the other two keys.
    a.key(Key::CtrlG);
    assert_eq!(a.key(Key::Char('p')), Some(Action::Peek("s-sub-1".into())));
    a.sub_out_pending = None;
    assert_eq!(
        a.key(Key::Char('o')),
        Some(Action::Switch("s-sub-1".into()))
    );
}

/// Enter on a subagent row IS the switch into that subagent's session, which is where the rest of
/// this head puts the moving act; the read that leaves the head where it was has its own key (`p`).
#[test]
fn enter_on_a_subagent_row_switches_into_its_session() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "summarize ~/bin/letibot".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    a.key(Key::CtrlG);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Switch("s-sub-1".into())),
        "Enter read the row instead of going to it"
    );
    assert!(!a.is_open(Surface::Subagents), "switching closes the pane");
    assert!(
        a.sub_out_pending.is_none(),
        "Enter asked for the output; that is `p`'s job now"
    );
}

/// The READ stays reachable, on a key that is neither Enter nor Esc - `p`. It is not a lesser act:
/// it is what `ClientFrame::Peek` exists for, a child's output read without moving the head, and
/// the ask is remembered so a rejection has something to end.
#[test]
fn p_reads_a_subagents_output_without_moving_the_head() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "summarize ~/bin/letibot".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    a.key(Key::CtrlG);
    assert_eq!(a.key(Key::Char('p')), Some(Action::Peek("s-sub-1".into())));
    assert_eq!(a.sub_out_pending.as_deref(), Some("s-sub-1"));
    assert!(
        a.is_open(Surface::Subagents),
        "the tree stays open under the read"
    );
    assert!(a.session_id.is_empty(), "reading moved the head");
}

#[test]
fn a_peeked_subagent_shows_its_tool_output_and_spills_the_whole_view() {
    let mut a = app();
    a.sub_out_pending = Some("s-sub-1".into());
    a.apply(ServerFrame::Peeked {
        session_id: "s-sub-1".into(),
        dropped: 3,
        snapshot: None,
        events: vec![
            env(
                1,
                SessionEvent::TranscriptContent {
                    item_id: "i1".into(),
                    item: Box::new(TranscriptItem::ToolResult {
                        call_id: "c1".into(),
                        name: "bash".into(),
                        outcome: letibot_transcript::ToolOutcome::Ok,
                        payload: "line one\nline two".into(),
                        edit: None,
                        origin: None,
                        media: None,
                    }),
                },
            ),
            env(
                2,
                SessionEvent::ToolFinished {
                    turn_id: "t1".into(),
                    call_id: "c1".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload_digest: "d".into(),
                    inline_bytes: 18,
                    full_bytes: 400,
                    spill: Some("/spill/c1".into()),
                    repairs: 0,
                    edit: None,
                },
            ),
        ],
    });
    // The ask is answered; the view is the tool results, verbatim, with the
    // spill locator named and the drop disclosed.
    assert!(a.sub_out_pending.is_none());
    let v = a.sub_out.as_ref().expect("the view opened");
    assert_eq!(v.session_id, "s-sub-1");
    assert_eq!(v.dropped, 3);
    // No snapshot, so this is the FALLBACK and it says so: the daemon answered with its event
    // ring because that is all it had, and a reader who could not tell this from a session drawn
    // from its rows would be reading a different thing than they think.
    assert!(v.degraded, "an event-ring answer is the fallback");
    let text = v.lines.join("\n");
    assert!(text.contains("· bash — ok"), "{text}");
    assert!(text.contains("line one"), "{text}");
    assert!(text.contains("line two"), "{text}");
    assert!(text.contains("/spill/c1"), "{text}");
    // The whole view is on disk, at a name a re-read overwrites, under the head's own runtime dir
    // and never a world-readable /tmp. The writer is exercised under a directory this test owns.
    let spill = v.spill.as_ref().expect("spilled");
    assert!(
        spill.ends_with("/letibot/subagent-s-sub-1.log") || spill.contains("/letibot-"),
        "{spill}"
    );
    assert!(
        !spill.starts_with("/tmp/letibot-subagent"),
        "spilled to a predictable /tmp name: {spill}"
    );
    let _ = std::fs::remove_file(spill);
    let dir = std::env::temp_dir().join(format!("letibot-peek-test-{}", std::process::id()));
    let under = spill_sub_out_under(&dir, "s-sub-1", &v.lines).expect("spilled");
    assert!(under.starts_with(dir.to_str().unwrap()), "{under}");
    let on_disk = std::fs::read_to_string(&under).expect("read");
    assert!(on_disk.contains("line two"), "{on_disk}");
    let _ = std::fs::remove_dir_all(&dir);
    // And the pane draws, header and disclosure included.
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("subagent output"), "{screen}");
    assert!(screen.contains("3 earlier events"), "{screen}");
    // And the degraded sentence reaches the screen, naming the way to the real draw.
    assert!(screen.contains("event ring"), "{screen}");
}

#[test]
fn esc_leaves_the_output_and_enter_and_o_both_switch_into_the_subagent() {
    let mut a = app();
    // **`running`, not `done`** - a finished child is under the folded `finished` group and is
    // not a stop, and this test is about the keys acting on a child that is one.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "summarize ~/bin/letibot".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    a.key(Key::CtrlG);
    // `p` opens the read - the key that is neither Enter nor Esc.
    a.key(Key::Char('p'));
    a.apply(ServerFrame::Peeked {
        session_id: "s-sub-1".into(),
        dropped: 0,
        snapshot: None,
        events: vec![],
    });
    // An empty scrollback says so; it does not look like a missing session.
    // "Neither an answer nor tool output", because a subagent whose whole
    // product is prose has no tool output by design and that is not a fault.
    let v = a.sub_out.as_ref().expect("the view opened");
    assert!(
        v.lines[0].contains("neither an answer nor tool output"),
        "{}",
        v.lines[0]
    );
    // Esc goes back to the tree - the tree, not everything closed. The payload's seam
    // prints `esc closes`, so Esc must mean that while it is up, and this pins that the
    // descent arm added below the panes did not take it.
    a.key(Key::Esc);
    assert!(a.sub_out.is_none());
    assert!(a.is_open(Surface::Subagents), "back to the tree");
    // Enter is the way in now, and `o` is the same act under the key this pane has
    // always used for it.
    assert!(matches!(
        a.key(Key::Enter),
        Some(Action::Switch(id)) if id == "s-sub-1"
    ));
    a.key(Key::CtrlG);
    assert!(matches!(
        a.key(Key::Char('o')),
        Some(Action::Switch(id)) if id == "s-sub-1"
    ));
}

#[test]
fn a_running_subagent_row_becomes_done_rather_than_a_second_line() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "summarize ~/bin/letibot".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "done".into(),
            prompt: "Here is the summary.".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    assert_eq!(a.subagents.len(), 1, "done replaces running, not appends");
    assert_eq!(a.subagents[0].state, "done");
    a.key(Key::CtrlG);
    // A `done` child is finished, so it is under the folded group: the pane says how many
    // before it says which, and Enter on that row is what shows them.
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("finished (1)"), "{screen}");
    a.key(Key::Enter);
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("done"), "{screen}");
    assert!(screen.contains("Here is the summary."), "{screen}");
}

/// The pane's row is the TASK and the child's answer is the subtitle beside it: `prompt` held a
/// title on the opening states and the answer's first line on the finish, and a field whose
/// meaning depends on `state` cannot be read as the row. So the event carries both, separate.
#[test]
fn a_subagent_row_shows_the_task_and_the_answer_beside_it() {
    let mut a = app();
    a.key(Key::CtrlG);
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "done".into(),
            // The LEGACY meaning on the finish: the answer's first line. Kept, so an old
            // head is unchanged.
            prompt: "twelve rows have no reader".into(),
            role: "coder".into(),
            task: "audit the session store\nand say which rows are never read".into(),
            model: String::new(),
            answer: Some("twelve rows have no reader".into()),
            slug: String::new(),
        },
    )));
    // **Unfold the `finished` group**, where a `done` child lives; the cursor starts on the
    // group row because it is the only stop.
    a.key(Key::Enter);
    let frame = a.screen(120, 40);
    let screen = frame.join("\n");
    // **The whole task, flattened to the row** - both of its lines are there, and *both*
    // words of it, which is the difference from the legacy field's first line alone.
    assert!(
        screen.contains("audit the session store and say which rows are never read"),
        "{screen}"
    );
    // **And the answer is its own clause**, the subtitle, not the row.
    assert!(screen.contains("twelve rows have no reader"), "{screen}");
    let row = frame
        .iter()
        .find(|l| l.contains("audit the session store"))
        .expect("the task's row");
    assert!(
        !row.contains("twelve rows have no reader"),
        "the answer was drawn AS the task — the field-that-means-two-things defect is \
             back: {row}"
    );
}

/// Stopping the daemon stops the work, and the card says so: a running job is an hour of `cargo
/// test`, a running subagent is a session mid-task, both die with the daemon - and the stop row
/// named only the cold prefill, the cost that reverses itself, while omitting the one that does not.
#[test]
fn the_stop_row_names_the_jobs_and_subagents_that_die_with_the_daemon() {
    let mut a = app();
    a.clock(1_000);
    a.session_id = "s".into();
    // A running job and a running subagent, beside a settled one of each: only
    // the RUNNING count is the warning's, and settled work must not inflate it.
    a.apply(jobs_frame(
        "s",
        vec![
            daemon_job("j1", "cargo test --release", true),
            daemon_job("j2", "wc -l notes", false),
        ],
    ));
    a.subagents = vec![
        SubagentState {
            session_id: "s-sub-1".into(),
            state: "running".into(),
            generating: true,
            prompt: "audit the store".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            spawned_ms: 0,
            slug: String::new(),
        },
        SubagentState {
            session_id: "s-sub-2".into(),
            state: "done".into(),
            generating: false,
            prompt: "finished".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            spawned_ms: 0,
            slug: String::new(),
        },
    ];
    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    assert!(a.quit_card, "two presses open the card");
    let card = a.quit_choices();
    assert!(
        card[1]
            .1
            .contains("1 job and 1 subagent are running and stop with the daemon"),
        "the running pair is named on the stop row:\n{:?}",
        card[1].1
    );
    // And drawn: the card is where the operator reads it, and a warning that
    // wraps off the glass is a warning nobody saw.
    let screen = a.screen(110, 30).join("\n");
    assert!(
        screen.contains("stop with the daemon"),
        "the warning reached the glass:\n{screen}"
    );
    // And the cheap row is unchanged by any of it - leaving the head stops
    // nothing, and a card that blurred that would be the defect this one exists
    // to prevent.
    assert!(
        !card[0].1.contains("stop with the daemon"),
        "the cheap row names no dying work: {:?}",
        card[0].1
    );
}

/// **The quit card says what else a stop kills**: another session's running turn, a reviewer
/// mid-verdict and a gate on a target. The card's own counts are this session's jobs and subagents,
/// so on a daemon with four busy sessions the cheap row and the killing row read alike.
#[test]
fn the_quit_card_says_which_other_work_a_stop_kills() {
    let mut a = app();
    a.apply(ServerFrame::Sessions {
        sessions: vec![
            brief("s-1", "the other one", true),
            brief("s-2", "quiet", false),
        ],
        current: "s-1".into(),
        created: None,
    });
    a.apply(queue_frame(queues(
        vec![queue_review_row("r-1", "reviewing")],
        vec![queue_merge_row("m-1", "gating")],
    )));
    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    assert!(a.quit_card, "two presses open the card");
    // The rows, with the card's own indent folded out of the comparison: the sentence wraps at the
    // card's width, and what it says is the assertion.
    let said = said_on_the_card(&a);
    assert!(
        said.contains(
            "stopping also kills 1 other session's turn, the reviewer's judgement and the gate \
             on `main`."
        ),
        "the card names the work that dies outside this session:\n{said}"
    );

    // And a daemon with nothing else running says nothing about it.
    let mut quiet = app();
    quiet.key(Key::CtrlC);
    quiet.key(Key::CtrlC);
    let said = said_on_the_card(&quiet);
    assert!(!said.contains("stopping also kills"), "{said}");
}

/// The quit card's rows as one sentence: whitespace folded, so a row the card wrapped reads the same
/// as one it did not.
fn said_on_the_card(a: &App) -> String {
    a.quit_card_lines(120)
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn a_running_subagent_is_counted_on_the_top_border_and_a_done_one_is_not() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "summarize ~/bin/letibot".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    let screen = a.screen(100, 24);
    let row = screen
        .iter()
        .find(|l| l.contains("subagent"))
        .expect("the count is on the screen");
    assert!(row.contains("1 subagent running"), "{row}");
    assert!(row.contains('╭'), "pinned to the top edge: {row}");
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "done".into(),
            prompt: "summarize ~/bin/letibot".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    assert!(
        !a.screen(100, 24).join("\n").contains("subagent running"),
        "a fact that exists only while it does"
    );
}

/// A child with a tool call in flight is a working child, and it is counted. The list's
/// `SessionStatus::running` is an instant - a turn generating now - and a child executing a
/// `cargo test` generates nothing, so reading that `false` as finished drops live children.
#[test]
fn a_child_with_a_tool_call_in_flight_is_counted() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.apply(ServerFrame::Event(env(
        1,
        child_event("s-sub-1", "running", None),
    )));
    // **The call is executing, so nothing is generating.** The list is cut now, and its word
    // about this instant is the only thing it can say.
    let mut list = a_family();
    list[1].status.running = false;
    a.apply(hello("s", list, Hub::new("s").snapshot()));
    assert_eq!(
        a.subagents[0].state, "running",
        "the list's `false` retired a child that has not finished"
    );
    assert!(
        !a.subagents[0].is_finished(),
        "a child with a call in flight is a live row"
    );
    let row = count_row(&mut a);
    assert!(row.contains("1 subagent running"), "{row}");
    // **And the pane says which of the two facts it is** - up, with no turn generating this
    // instant, which is exactly what a call in flight looks like from here. The word is the
    // head's, like `state unknown` beside it; the child's own words are the four states.
    a.key(Key::CtrlG);
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("[~]"), "the live mark is gone:\n{screen}");
    assert!(
        screen.contains("waiting"),
        "the pane does not say the child is up and not generating:\n{screen}"
    );
}

/// A child that stopped its turn with an interim message has not finished, and it counts: `answer`
/// rides the `done` and nothing else, so a row with no answer is a child that has not ended, and
/// the count must not flap with the list's instantaneous measurement.
#[test]
fn a_child_that_stopped_its_turn_with_an_interim_message_still_counts() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.apply(ServerFrame::Event(env(
        1,
        child_event("s-sub-1", "running", None),
    )));
    assert_eq!(
        a.subagents[0].answer, None,
        "an interim line is not an answer, and the daemon says so by publishing no `answer`"
    );
    // The list is cut between the child's turns, so its `running` alternates. The count must
    // read 1 at every one of them.
    for (i, generating) in [false, true, false, false, true].into_iter().enumerate() {
        let mut list = a_family();
        list[1].status.running = generating;
        a.apply(hello("s", list, Hub::new("s").snapshot()));
        let row = count_row(&mut a);
        assert!(
            row.contains("1 subagent running"),
            "frame {i} (generating={generating}) dropped a live child: {row}"
        );
    }
}

/// The footer's number is the rows the pane draws, not a second rule about them: it counted
/// `state == "running"` while the pane's group was built from [`SubagentState::is_finished`], so a
/// child in `opening` was drawn live and left out of the number. Both read one predicate now.
#[test]
fn the_footers_number_is_the_rows_the_pane_draws() {
    let mut a = app();
    // One child of each kind the pane can draw: up and generating, still opening, and ended.
    let mut live = brief("s-sub-1", "the one that is up", true);
    live.parent_session_id = Some("s".into());
    let mut opening = brief("s-sub-2", "the one still opening", false);
    opening.parent_session_id = Some("s".into());
    let mut done = brief("s-sub-3", "the one that finished", false);
    done.parent_session_id = Some("s".into());
    a.apply(hello(
        "s",
        vec![brief("s", "parent", false), live, opening, done],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        child_event("s-sub-1", "running", None),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        child_event("s-sub-2", "opening", None),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        child_event("s-sub-3", "done", Some("3529 files")),
    )));
    assert!(
        !a.subagents_finished_open,
        "the premise: the finished group is folded, so every `Agent` stop is a live row"
    );
    // **The pane's own enumeration**, read the way the pane and its keys read it.
    let drawn = a
        .subagent_stops()
        .iter()
        .filter(|s| matches!(s, SubStop::Agent(_)))
        .count();
    assert_eq!(drawn, 2, "the two live children are the pane's active rows");
    let row = count_row(&mut a);
    assert!(row.contains("2 subagents running"), "{row}");
    // **And the same number, counted off the marks the pane drew.** The two live rows wear a
    // mark - `[~]` for `running`, `[…]` for `opening` - and the finished one is under the fold.
    a.key(Key::CtrlG);
    let screen = a.screen(100, 24).join("\n");
    let marks = screen
        .lines()
        .filter(|l| l.contains("[~]") || l.contains("[\u{2026}]"))
        .count();
    assert_eq!(
        marks, drawn,
        "the drawn live marks and the pane's stops disagree:\n{screen}"
    );
    assert_eq!(
        drawn, 2,
        "and the footer said `2 subagents running` for exactly these:\n{screen}"
    );
}

/// The merge queue's gatekeeper is not one of this session's subagents: the daemon runs the
/// reviewer as a child of this session so its approvals reach this head, but nobody here started
/// it, so it stays out of the pane and the count.
#[test]
fn a_gatekeeper_child_is_not_counted_or_listed() {
    let mut a = app();
    a.apply(hello("s", Vec::new(), Hub::new("s").snapshot()));
    // A normal child and the reviewer, both live.
    a.apply(ServerFrame::Event(env(
        1,
        child_event("s-sub-1", "running", None),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Subagent {
            subagent_id: "s-sub-gk".into(),
            state: "running".into(),
            prompt: letibot_sessionlog::GATEKEEPER_TITLE_PREFIX.into(),
            role: "gatekeeper".into(),
            task: format!(
                "{} judge the branch",
                letibot_sessionlog::GATEKEEPER_TITLE_PREFIX
            ),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    let ids: Vec<&str> = a.subagents.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(ids, vec!["s-sub-1"], "the reviewer is in the tree: {ids:?}");
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("1 subagent running"), "{screen}");

    // And rebuilt from the session list, where only the title can say what it is.
    let mut gk = brief(
        "s-sub-gk",
        &format!(
            "{} A subagent has finished work",
            letibot_sessionlog::GATEKEEPER_TITLE_PREFIX
        ),
        true,
    );
    gk.parent_session_id = Some("s".into());
    let mut child = brief("s-sub-1", "find the bug", true);
    child.parent_session_id = Some("s".into());
    a.apply(ServerFrame::Sessions {
        sessions: vec![brief("s", "parent", false), child, gk],
        current: "s".into(),
        created: None,
    });
    let ids: Vec<&str> = a.subagents.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["s-sub-1"],
        "rebuilt with the reviewer in it: {ids:?}"
    );
}

/// The finished fold survives the round trip down into a child and back up: `fold_subagents` must
/// not write `subagents_finished_open` to land its cursor, or a folded group of two hundred
/// children comes back open. The cursor lands on the `finished` group row instead.
#[test]
fn the_fold_survives_going_down_into_a_child_and_back_up() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    // The premise: `s-sub-1` is running and `s-sub-2` is not, so the pane has one live row and
    // a folded group of one.
    assert!(
        !a.subagents_finished_open,
        "the premise: the finished group starts folded"
    );
    a.key(Key::CtrlG);

    // Down: Enter on the live child is the switch, and the child's own session has no children
    // of its own.
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    assert_eq!(a.session_id, "s-sub-1");

    // **The child ENDS while the operator is inside it** - the event, and the list that no
    // longer measures a turn generating in it. This is the whole of the reproduction.
    a.apply(ServerFrame::Event(env(
        5,
        child_event("s-sub-1", "done", Some("the answer")),
    )));
    let mut ended = a_family();
    ended[1].status.running = false;

    // Up: one Esc, then the parent's `Hello` - which carries the list `fold_subagents` reads.
    assert_eq!(a.key(Key::Esc), Some(Action::Switch("s".into())));
    a.apply(hello("s", ended, Hub::new("s").snapshot()));

    assert!(
        !a.subagents_finished_open,
        "the round trip re-opened the group the operator had folded"
    );
    // And it is drawn folded: the finished children are under the group row and not on the
    // glass. This is the operator's *"it doesnt group finished subagents"* read off the screen
    // rather than off the flag.
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("finished (2)"), "{screen}");
    assert!(
        !screen.contains("find the bug in the reader"),
        "the child the operator left is drawn, so the group is open:\n{screen}"
    );
    assert!(!screen.contains("audit the store"), "{screen}");
}

/// A child ending does not re-open the group the operator folded: the fold is the operator's, and a
/// row landing under it is not a reason to open it - whether the re-open would arrive with the
/// round trip or with a later list frame.
#[test]
fn a_child_ending_does_not_re_open_the_fold() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.apply(ServerFrame::Event(env(
        1,
        child_event("s-sub-1", "running", None),
    )));
    a.key(Key::CtrlG);

    // Fold the group the way the operator folds it: Enter on the group row. The cursor starts
    // on the live child, so one Down lands on the group.
    a.key(Key::Down);
    assert_eq!(
        a.subagent_stops().get(a.subagents_sel),
        Some(&SubStop::Finished),
        "the cursor is not on the group row"
    );
    a.key(Key::Enter);
    assert!(
        a.subagents_finished_open,
        "Enter on the group row unfolds it"
    );
    a.key(Key::Enter);
    assert!(!a.subagents_finished_open, "and Enter folds it again");

    // The live child ends: the event, and a list that no longer measures a turn in it.
    a.apply(ServerFrame::Event(env(
        2,
        child_event("s-sub-1", "done", Some("the answer")),
    )));
    let mut ended = a_family();
    ended[1].status.running = false;
    a.apply(ServerFrame::Sessions {
        sessions: ended,
        current: "s".into(),
        created: None,
    });

    assert!(
        !a.subagents_finished_open,
        "a child ending re-opened the group the operator folded"
    );
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("finished (2)"), "{screen}");
    assert!(
        !screen.contains("find the bug in the reader"),
        "the ended child is drawn, so the group is open:\n{screen}"
    );
}

/// **A subagent's two lines are one entry**: the pointer over its facts lights both, and a click
/// there selects it - the same block, under the pointer and under the click.
#[test]
fn a_two_line_entry_is_one_block_under_the_pointer_and_the_click() {
    let mut a = app();
    let mut fam = vec![brief("s", "parent", false)];
    for (id, title) in [("s-sub-a", "first task"), ("s-sub-b", "second task")] {
        let mut b = brief(id, title, true);
        b.parent_session_id = Some("s".into());
        fam.push(b);
    }
    a.apply(hello("s", fam, Hub::new("s").snapshot()));
    a.key(Key::CtrlG);
    let screen = a.screen(120, 30);
    let task = screen
        .iter()
        .position(|l| l.contains("second task"))
        .expect("the second entry");
    let facts = task + 1;
    assert!(
        !screen[facts].trim().is_empty(),
        "the premise: a facts line under it"
    );
    a.key(Key::Hover {
        x: 8,
        y: facts as u16,
    });
    let lit = a.screen(120, 30);
    assert!(
        lit[task].contains("\x1b[22;1m"),
        "the task line is not lit: {:?}",
        lit[task]
    );
    assert!(
        lit[facts].contains("\x1b[22;1m"),
        "the facts line is not lit"
    );
    let before = a.subagents_sel;
    a.key(Key::Click {
        x: 8,
        y: facts as u16,
    });
    assert_ne!(
        a.subagents_sel, before,
        "a click on the facts line selected nothing"
    );
    let picked = a.screen(120, 30);
    assert!(
        picked[task].contains('▸'),
        "the second entry is not the selected one: {:?}",
        picked[task]
    );
}

// ─────────────────── the wait a switch is, and what is drawn for it ───────────────────

/// Entering a subagent is a round trip, and the head says so while it happens: both arms of
/// `App::switch_to` share one state (`fetching` + `attaching`), so before the `Hello` the status
/// row names where the head is going and the conversation it still draws is not passed off as that.
#[test]
fn entering_a_subagent_says_that_a_switch_is_in_flight() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.key(Key::CtrlG);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));

    // **The wait, which is the whole finding.** Nothing has answered yet.
    assert!(
        a.attaching,
        "the head asked for a session and draws as though it had arrived"
    );
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("switching to find the bug in the reader"),
        "the status row says nothing about the switch:\n{screen}"
    );
    // The cat too - the same placeholder a first attach draws, from the same field: *the head
    // has been told nothing yet*. A spinner is the honest answer here and a banner is not.
    assert!(
        screen.contains("asking the daemon"),
        "the conversation does not show the wait:\n{screen}"
    );

    // The daemon answers with the second `Hello`, and the state ends with it.
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    assert!(!a.attaching, "the Hello did not end the wait");
    assert_eq!(a.session_id, "s-sub-1");
    let screen = a.screen(120, 30).join("\n");
    assert!(
        !screen.contains("switching to"),
        "the sentence outlived the switch:\n{screen}"
    );
}

/// A `Hello` for some OTHER session does not end the wait for the one asked for, which is why
/// `fetching` is a session id rather than a flag: the daemon may seat the head somewhere first, and
/// a pending state cleared on the wrong `Hello` would take the sentence down mid-move.
#[test]
fn a_hello_for_another_session_leaves_the_switch_pending() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.key(Key::CtrlG);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
    // The answer to something else entirely - the daemon seating this head where it already was.
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    assert!(
        a.attaching,
        "a Hello for the session being LEFT ended the wait"
    );
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("switching to"), "{screen}");
}

/// `esc` out of a subagent draws the list it came out of, not the child's empty one: the pane lists
/// the CURRENT session's children, so for as long as the switch took it drew `none spawned yet`
/// over a parent with two of them. `fetching` names the session the list belongs to.
#[test]
fn esc_out_of_a_subagent_lists_the_parent_before_the_switch_lands() {
    let mut a = inside_a_subagent();
    assert!(
        a.subagents.is_empty(),
        "the premise: the child has no children"
    );
    assert_eq!(a.key(Key::Esc), Some(Action::Switch("s".into())));
    assert!(a.is_open(Surface::Subagents));

    // **Before the parent's `Hello`** - the state the operator sat in.
    let ids: Vec<&str> = a.subagents.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["s-sub-1", "s-sub-2"],
        "the pane drew the child's children while going up to the parent: {ids:?}"
    );
    let screen = a.screen(100, 24).join("\n");
    assert!(
        !screen.contains("none spawned yet"),
        "the pane claims nothing was spawned, over the list it came out of:\n{screen}"
    );
    assert!(screen.contains("find the bug in the reader"), "{screen}");
    // And the cursor is on the row the operator climbed out of, which is the row the pane was
    // opened FOR - `up_from`, read once the list it names is the one on the screen.
    assert_eq!(
        a.subagents[a.subagents_sel].session_id, "s-sub-1",
        "the cursor is not on the child this head came out of"
    );
    // The move is still in flight, and the screen says that too.
    assert!(a.attaching);
    assert!(screen.contains("switching to"), "{screen}");

    // The parent's `Hello` rebuilds the same list - one answer, not two.
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    assert_eq!(a.session_id, "s");
    let ids: Vec<&str> = a.subagents.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(ids, vec!["s-sub-1", "s-sub-2"], "{ids:?}");
    assert_eq!(a.subagents[a.subagents_sel].session_id, "s-sub-1");
}

/// Entering a subagent stops drawing the conversation it is LEAVING: `body_window` pushed the
/// attach cat after every row of the session being left, so the pane showed the parent's
/// conversation with a spinner squeezed under it.
#[test]
fn entering_a_subagent_stops_drawing_the_conversation_it_is_leaving() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    shell_row(&mut a, 1, "s.0", "user", operator_row("PARENT-QUESTION"));
    shell_row(
        &mut a,
        3,
        "s.1",
        "assistant",
        TranscriptItem::Assistant {
            text: "PARENT-ANSWER".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    // The premise: the parent's conversation is what the pane is drawing.
    let before = a.screen(100, 24).join("\n");
    assert!(
        before.contains("PARENT-QUESTION") && before.contains("PARENT-ANSWER"),
        "the premise: the parent's conversation is on the screen\n{before}"
    );

    a.key(Key::CtrlG);
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));

    // **Nothing has answered yet.** The pane is the one the operator reported: a full screen
    // of the session he asked to leave.
    let during = a.screen(100, 24).join("\n");
    assert!(
        !during.contains("PARENT-ANSWER"),
        "the conversation pane still draws the session being left:\n{during}"
    );
    assert!(
        !during.contains("PARENT-QUESTION"),
        "the conversation pane still draws the session being left:\n{during}"
    );
    // And what it draws instead says what is happening, in the place he is looking.
    assert!(
        during.contains("asking the daemon"),
        "the pane shows neither the wait nor the conversation:\n{during}"
    );
    assert!(
        during.contains("leaving parent for find the bug in the reader"),
        "the pane does not say which session it left or which it is waiting for:\n{during}"
    );
    assert!(
        during.contains("switching to find the bug in the reader"),
        "nothing on the screen says where this head is going:\n{during}"
    );

    // The child's `Hello`: its conversation, and nothing of the parent's.
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    shell_row(
        &mut a,
        1,
        "s-sub-1.0",
        "user",
        operator_row("CHILD-QUESTION"),
    );
    let after = a.screen(100, 24).join("\n");
    assert!(!a.attaching, "the Hello did not end the wait");
    assert!(after.contains("CHILD-QUESTION"), "{after}");
    assert!(!after.contains("PARENT-ANSWER"), "{after}");
    assert!(
        !after.contains("leaving parent"),
        "the sentence outlived the switch it describes:\n{after}"
    );
}

/// **The resume arm is the same rule** - a session the daemon does not hold is fetched from the
/// store, which is the slower wait of the two, and the conversation being left must not be
/// passed off as the one asked for while it happens.
#[test]
fn resuming_a_stored_session_also_stops_drawing_the_one_being_left() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    shell_row(
        &mut a,
        1,
        "s.0",
        "assistant",
        TranscriptItem::Assistant {
            text: "PARENT-ANSWER".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    let mut stored = brief("s-stored", "a conversation on disk", false);
    stored.live = false;
    a.sessions.push(stored);
    assert_eq!(
        a.switch_to("s-stored".into()),
        Some(Action::ResumeSession("s-stored".into()))
    );
    let during = a.screen(100, 24).join("\n");
    assert!(
        !during.contains("PARENT-ANSWER"),
        "the pane draws the session being left while the store is read:\n{during}"
    );
    assert!(
        during.contains("leaving parent for a conversation on disk"),
        "{during}"
    );
    assert!(
        during.contains("switching to a conversation on disk"),
        "{during}"
    );
}
