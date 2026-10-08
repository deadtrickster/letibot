//! Subagents: the pane, the tree, a child's output.

use super::*;

/// **A card that is a subagent's call says so, and names the child.**
///
/// The operator's ruling is that a subagent's ask surfaces at the root's head — *"who
/// asks subagents permissions? i think they should surface to the parent head all the way
/// to the root obviously"*. R58 gives a child no head, so its card arrives on THIS
/// session's screen, and **an unlabelled card is answered for the wrong thing**: the
/// question and the ladder are the same either way, and this clause is the only thing
/// that says whose call it is.
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
    // The handle — what `task_result` collects by and what a head attaches to — and the
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
        },
    )));
    // A spawn that the head saw. The running count lives in the pane —
    // the composer's border used to repeat it, and that border is plain now.
    assert_eq!(a.subagents.len(), 1);

    a.key(Key::CtrlG);
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("subagents"), "{screen}");
    assert!(screen.contains("summarize ~/bin/letibot"), "{screen}");
    assert!(screen.contains("running"), "{screen}");

    a.key(Key::Esc);
    assert!(!a.subagents_pane);
}

/// **A fresh head rebuilds the subagent pane AND the count out of the daemon's own
/// session list — no spawn event required.**
///
/// The operator's measured report, in two sentences: *"i just restarted the head and the
/// subagents list is gone"*, and (twice) *"when you started new subagents the subagents
/// pane refreshed and qwens showed up"*. So a head that attached after the spawns drew an
/// empty pane and no count, and the only thing that could fill it was a later spawn —
/// `SessionEvent::Subagent` carries exactly ONE child, so it cannot re-list an earlier
/// generation. Nothing folded the durable rows: `SessionEvent::Subagent` was folded into
/// nothing at all by the view (`letibot_sessionlog::view`), so the snapshot did not carry
/// them either, and the ONE push into `self.subagents` was the live event arm.
///
/// The durable half is `SessionBrief::parent_session_id`, which is on the wire in every
/// `Hello` and every `Sessions` frame — the registry's own words for it: *"A head draws a
/// subagent tree from this without reaching the store"*. This pins both halves of what the
/// pane must be able to say after an attach: **the rows**, and **the running count**, which
/// is the same list read through the state clause.
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
    // **The finished child is folded, and the pane says how many** — the group the operator
    // asked for. `s-sub-2` is `running: false` in the list, so it is a finished row.
    assert!(screen.contains("finished (1)"), "{screen}");
    assert!(
        !screen.contains("audit the store"),
        "a finished child is drawn though its group is folded:\n{screen}"
    );
    // **And the state word is not invented.** The list says whether a turn is generating;
    // it says nothing about how a settled child ended, so the row this head did not watch
    // says so rather than claiming `done` — unfolded, where a `done` would be a lie.
    a.key(Key::Down);
    a.key(Key::Enter);
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("audit the store"), "{screen}");
    assert!(screen.contains("state unknown"), "{screen}");
}

/// **THE ONE THAT IS RUNNING IS AT THE TOP OF THE PANE, NOT BELOW THE FOLD.**
///
/// The operator, 2026-10-06: *"i went to subagents panel and dont see it here"* — a subagent
/// just started, and a long list of finished ones. **It is not a delay**, which is the
/// question they asked next (*"after some time (which?) it appears"*): the daemon publishes the
/// child's `opening` state at the spawn and `running` once its harness is open — 0.3s for that
/// one, its own progress line — so the row is on the wire at once. It landed BELOW THE FOLD,
/// because a child this head watched spawn is appended after the durable rows, and the pane had
/// drawn nothing that would scroll it up. With the finished children folded, the running child
/// is the first row, which is the whole point.
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
        },
    )));
    a.key(Key::CtrlG);
    let screen = a.screen(214, 60);
    let joined = screen.join("\n");
    assert!(joined.contains("the one that is running"), "{joined}");
    assert!(joined.contains("finished (26)"), "{joined}");
    // **At the TOP** — the running child's row is above the group row, so it is the first
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

/// **THE ACCEPTANCE TEST: Enter into a subagent, then a single Esc back up to the
/// subagents list.**
///
/// The operator's own two sentences: *"when I \"Enter\" Subagent it is like completely
/// switching session with just one piece of info - a Label that it is a subagent and Esc
/// going up in the subagents tree"*, and *"so after o I couldnt just Esc from the subagent
/// - had to switch back here via session"*. One keystroke down, one keystroke up, the
/// parent's list on the screen when you land, and the label saying whose child you were in
/// while you were there.
#[test]
fn enter_into_a_subagent_and_one_esc_goes_back_up_to_the_list_it_came_from() {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.key(Key::CtrlG);

    // **Down a level: Enter IS the switch.**
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-1".into())));
    assert!(
        !a.subagents_pane,
        "switching closes the pane it was opened from"
    );
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    assert_eq!(a.session_id, "s-sub-1");
    assert!(
        a.subagents.is_empty(),
        "a subagent's own session shows its own children, not its parent's"
    );
    // **And the COUNT is the same list**, which is the reason the clear exists at all:
    // measured 2026-09-16, a carried row put `1 subagent running` on the composer of the
    // very subagent being looked at. The rebuild is filtered by `parent_session_id`, so a
    // head standing in the child draws none of the parent's running rows — and this is the
    // half that would come back if the fold ever read the daemon's list unfiltered.
    let screen = a.screen(100, 24).join("\n");
    assert!(
        !screen.contains("subagent running"),
        "the parent's running child is counted on the child's own composer:\n{screen}"
    );

    // **The ONE piece of info: this is a subagent, and whose.** On the row that already
    // names the session — the header — and nothing else added anywhere.
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
    assert!(a.subagents_pane, "esc goes up TO the subagents list");

    // The parent's `Hello` — which is what a switch is answered with — carries the
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

/// **Esc's other meanings, inside a subagent — and the two it gave up.**
///
/// The descent arm sits below every arm that already owns Esc, and this is the proof
/// rather than the claim: a pane the operator is standing in closes, a half-typed line
/// keeps Esc for the composer, an open decision keeps it away from the tree, and a
/// session with no parent has nowhere to go in the first place.
#[test]
fn esc_keeps_every_other_meaning_it_has_while_inside_a_subagent() {
    // A pane on the screen owns Esc — its own footer says `esc closes`, and it wins.
    let mut a = inside_a_subagent();
    a.key(Key::CtrlG);
    assert_eq!(a.key(Key::Esc), None);
    assert!(!a.subagents_pane, "esc closed the pane");
    assert_eq!(
        a.session_id, "s-sub-1",
        "esc also switched out of the session"
    );

    // **A half-typed line goes up WITH the operator** — it used to keep Esc for the composer,
    // which armed the interrupt, and the next Esc stopped the child the operator was only
    // leaving (2026-10-08). The composer is the head's, so the draft is still there above.
    let mut a = inside_a_subagent();
    a.clock(1_000);
    a.key(Key::Char('x'));
    assert_eq!(a.key(Key::Esc), Some(Action::Switch("s".into())));
    assert_eq!(a.editor.text(), "x", "the draft goes up with the operator");

    // A decision on the screen keeps the operator in the child — said, and NOT armed: a
    // second Esc there interrupts nothing.
    let mut a = inside_a_subagent();
    a.clock(1_000);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.open.push(decision_with(&[
        letibot_sessionlog::event::OptionKind::AllowOnce,
    ]));
    assert_eq!(a.key(Key::Esc), None);
    assert_eq!(a.session_id, "s-sub-1");
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("answer it, then esc goes up"), "{screen}");
    a.clock(1_200);
    assert_eq!(a.key(Key::Esc), None);
    assert!(
        !a.take_actions()
            .iter()
            .any(|x| matches!(x, Action::Interrupt(_))),
        "two Escs in a subagent interrupted it"
    );

    // And a conversation at the top of the tree has no parent to go to, so Esc is
    // exactly what it was there — the arm cannot fire at all.
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    assert!(a.parent_session().is_none());
    assert_eq!(a.key(Key::Esc), None, "one press arms, it does not fire");
    assert!(a.hint_bar(120).contains("again"), "{}", a.hint_bar(120));
}

/// **THE CONFLICT, PINNED RATHER THAN TAKEN IN SILENCE: inside a subagent, one Esc goes
/// up, and Esc-Esc no longer interrupts that child there.**
///
/// A single Esc that leaves the session and a first Esc that arms the interrupt frame are
/// the same keystroke, so no rule can honour both inside a subagent — and this is the one
/// the operator asked for (*"make sure a single Esc goes up to subagents list"*). What is
/// left of the frame is what the arm's own comment records: the child's turn can still be
/// interrupted from the child with `/interrupt`, and from the parent with `job_kill`,
/// which is how this tree already documents stopping one. In a session that is NOT a
/// subagent nothing moves at all — `esc_twice_interrupts_a_running_turn_…` above is that
/// half, unedited.
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

/// **ctrl-s inside a subagent is its family: the parent and its siblings.**
///
/// The operator's ask — *"make sure sessions list (ctrl-s) is filtered to the parent and
/// siblings. In fact it should also fix our sessions pane - it must be a tree"* — and the
/// tree was already here (`session_rows`/`push_children`: nested, folded, with the chain
/// to where you are always shown); what was wrong is that a head standing in a child drew
/// every conversation in the daemon around a row one level down. The siblings are **shown
/// whatever the collapse state says**, because a family view whose members were folded
/// away would be a list of one row.
#[test]
fn ctrl_s_inside_a_subagent_lists_the_parent_and_its_siblings() {
    let mut a = inside_a_subagent();
    let rows = a.session_rows();
    let shown: Vec<(&str, usize)> = rows
        .iter()
        .map(|r| (a.sessions[r.idx].session_id.as_str(), r.depth))
        .collect();
    assert_eq!(
        shown,
        vec![("s", 0), ("s-sub-1", 1), ("s-sub-2", 1)],
        "{shown:?}"
    );
    // Nothing was expanded and the siblings are on the list anyway.
    assert!(a.expanded.is_empty());
    // And the screen says the same thing the enumeration does — one list, read by the
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

/// **The one word the daemon's list is authoritative for, in both directions.**
///
/// `state` is the only field both halves can speak to, and the list speaks to exactly one
/// word: whether a turn is generating in that session *now*. A row this head watched keeps
/// every richer field — the role, the model, the answer — and the word `running` comes from
/// the list either way: a `false` retires a `running` the row is still claiming, because a
/// finish this head was not attached for otherwise leaves the composer counting a child
/// that is not running; and a `true` overrides a settled row, because a child that has
/// started a second turn is generating whatever it last finished.
///
/// **This is the merge, and it is where the two halves meet.** `fold_subagents` runs at
/// every `load` — the late-join and resync path, and the `Hello` a `Switch` is answered
/// with — so the list cannot flatten a watched row and the watched row cannot outlive the
/// list's live word.
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
    };
    a.apply(ServerFrame::Event(env(
        1,
        watched("done", Some("3529 files")),
    )));
    assert_eq!(a.subagents[0].state, "done");

    // **The list says a turn is generating in that child.** The word is the list's, and the
    // row's own richer facts survive it — a child asked for more work is a running child.
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
    // word — and the count above the composer stops lying about a child that is idle.
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

/// The subagent tree is the parent's fact. Measured 2026-09-16: switching
/// into a subagent carried "1 subagent running" onto ITS composer, and the
/// pane there offered the row of the very session being looked at. On the way
/// back the daemon replays the parent's retained events after `Hello`, so the
/// tree is rebuilt from the same events that built it the first time.
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
    };
    a.apply(ServerFrame::Event(env(1, spawn.clone())));
    a.key(Key::CtrlG);
    // **`o` is the alias and Enter is the key**, and both are the same act — the switch.
    // This test reads the pane's key for the way IN; the way here is the older spelling.
    assert_eq!(
        a.key(Key::Char('o')),
        Some(Action::Switch("s-sub-1".into()))
    );
    assert!(!a.subagents_pane, "switching closes the pane");

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
/// `opening`, and Enter on it goes nowhere — there is no session to go to.
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
    assert!(a.subagents_pane, "the pane stays where the operator was");
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

/// **Enter on a subagent row IS the switch into that subagent's session.**
///
/// Replaced behaviour, and the operator's own words for it: *"when I \"Enter\" Subagent
/// it is like completely switching session with just one piece of info … Which narrows the
/// subagent prompt - make \"o\" to \"Enter\""*. Enter used to `Peek` — a read that left the
/// head where it was — and `o` was the key that moved. The two acts are now where the rest
/// of this head puts them: Enter takes the row you are on.
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
        },
    )));
    a.key(Key::CtrlG);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Switch("s-sub-1".into())),
        "Enter read the row instead of going to it"
    );
    assert!(!a.subagents_pane, "switching closes the pane");
    assert!(
        a.sub_out_pending.is_none(),
        "Enter asked for the output; that is `p`'s job now"
    );
}

/// **The READ stays reachable, on a key that is neither Enter nor Esc — `p`.**
///
/// The peek is not a lesser act: it is the thing `ClientFrame::Peek` exists for (a child's
/// output read *without* moving the head, R20), and it was the behaviour Enter had, so
/// moving Enter must not lose it. `p` is `/peek ID`'s own key, and the ask is remembered so
/// a rejection has something to end.
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
        },
    )));
    a.key(Key::CtrlG);
    assert_eq!(a.key(Key::Char('p')), Some(Action::Peek("s-sub-1".into())));
    assert_eq!(a.sub_out_pending.as_deref(), Some("s-sub-1"));
    assert!(a.subagents_pane, "the tree stays open under the read");
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
    // **No snapshot, so this is the FALLBACK and it says so.** The daemon answered
    // with its event ring because that is all it had to answer with; a reader who
    // could not tell this from a session drawn from its rows would be reading a
    // different thing than they think — the whole reason `degraded` exists.
    assert!(v.degraded, "an event-ring answer is the fallback");
    let text = v.lines.join("\n");
    assert!(text.contains("· bash — ok"), "{text}");
    assert!(text.contains("line one"), "{text}");
    assert!(text.contains("line two"), "{text}");
    assert!(text.contains("/spill/c1"), "{text}");
    // The whole view is on disk, at a name a re-read overwrites — under the
    // head's own runtime dir, never a world-readable /tmp. The live write
    // went where the head puts things; the writer itself is exercised under
    // a directory this test owns.
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
    // **`running`, not `done`** — a finished child is under the folded `finished` group and is
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
        },
    )));
    a.key(Key::CtrlG);
    // `p` opens the read — the key that is neither Enter nor Esc.
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
    // Esc goes back to the tree — the tree, not everything closed. The payload's seam
    // prints `esc closes`, so Esc must mean that while it is up, and this pins that the
    // descent arm added below the panes did not take it.
    a.key(Key::Esc);
    assert!(a.sub_out.is_none());
    assert!(a.subagents_pane, "back to the tree");
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

/// **The pane's row is the TASK, and the child's answer is the subtitle beside it.**
///
/// The defect this pins, measured on the running head: a `done` row drew **122 characters
/// which were the child's ANSWER**, with the two-line task nowhere on the wire — because
/// `prompt` held a title on the opening states and the answer's first line on the finish,
/// and a field whose meaning depends on `state` cannot be read as the row. So the event
/// carries `task` whole (and never truncated by the daemon) and `answer` as its own field,
/// and this asserts both halves: the row is the question, the subtitle is the answer.
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
        },
    )));
    // **Unfold the `finished` group**, where a `done` child lives; the cursor starts on the
    // group row because it is the only stop.
    a.key(Key::Enter);
    let frame = a.screen(120, 40);
    let screen = frame.join("\n");
    // **The whole task, flattened to the row** — both of its lines are there, and *both*
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

/// **Stopping the daemon stops the work, and the card says so** — the operator's
/// ask, 2026-10-05. A running job is an hour of `cargo test --release`; a running
/// subagent is a session mid-task; both die with the daemon, and the stop row named
/// only the cold prefill — the cost that reverses itself — while omitting the one
/// that does not.
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
    // And the cheap row is unchanged by any of it — leaving the head stops
    // nothing, and a card that blurred that would be the defect this one exists
    // to prevent.
    assert!(
        !card[0].1.contains("stop with the daemon"),
        "the cheap row names no dying work: {:?}",
        card[0].1
    );
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
        },
    )));
    assert!(
        !a.screen(100, 24).join("\n").contains("subagent running"),
        "a fact that exists only while it does"
    );
}

/// **A child with a tool call in flight is a working child, and it is counted.**
///
/// The operator's standard, in their own words: *"claude code for example shows subagent as
/// alive until it finished turn with reply. not 'pausing it' on tool calls."* The daemon's
/// session list measures an **instant** — `SessionStatus::running` is *"a turn is generating
/// in this session at this instant"* — and a child executing a `cargo test` generates nothing
/// while it runs. So the list says `false` about a child that is plainly at work, and reading
/// that `false` as *finished* is what took live children out of the count.
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
    // **And the pane says which of the two facts it is** — up, with no turn generating this
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

/// **A child that stopped its turn with an interim message has not finished, and it counts.**
///
/// The operator's own child, verbatim: *"Waiting on `j228` (the workspace-wide test run) —
/// I'll report as soon as it ends"*, with two commits and a running suite behind it. That line
/// is not a completion — see [`child_event`]: `answer` rides the `done` and nothing else — so
/// the row holds no answer and the child has not ended. **The count must not move with the
/// list's instantaneous measurement**, however many times that measurement flaps; this is the
/// `4, 2, 3, 1` the operator watched, over children that were alive the whole time.
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

/// **The footer's number is the rows the pane draws, and not a second rule about them.**
///
/// The defect this pins: the footer counted `state == "running"` while the pane's active group
/// was built from [`SubagentState::is_finished`], so a child in `opening` was drawn as a live
/// row and left out of the number — two readings of one list, which is the two-enumerations
/// defect [`App::subagent_stops`] exists to prevent. Both now read the one lifecycle
/// predicate, and this asserts the agreement at three points: the pane's own enumeration, the
/// marks it drew on the glass, and the number on the composer's edge.
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
    // mark — `[~]` for `running`, `[…]` for `opening` — and the finished one is under the fold.
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
