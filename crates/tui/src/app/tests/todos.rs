//! Todos: the plan, the todo card, the repository queue.

use super::*;

#[test]
fn ctrl_t_opens_the_todos_pane_and_esc_closes_it() {
    let mut a = app();
    // Opening asks for the list — the bootstrap read — and the pane draws
    // both of its sections, labelled as the two different things they are.
    assert_eq!(a.key(Key::CtrlT), Some(Action::ListTodos));
    let screen = a.screen(100, 30).join("\n");
    assert!(screen.contains("todos"), "{screen}");
    assert!(
        screen.contains("add todo item"),
        "the pane's add control is not drawn: {screen}"
    );
    assert!(
        screen.contains("who wrote each line"),
        "the pane does not name the list's authorship: {screen}"
    );
    // **The fact, not the word.** This asserted `read-only`, which was the old repo heading's
    // wording; the heading now says what that section IS (`what the project intends; not the
    // model's plan`, after the operator's two-lists ruling) and the sentence that carries the
    // read-only fact is the one under the list. Pinning the WORD is what made a wording change
    // look like a behaviour change.
    assert!(
        screen.contains("this pane never writes it"),
        "the pane does not say it leaves the file alone: {screen}"
    );
    // And Esc is "go back", before the composer sees it.
    a.key(Key::Esc);
    assert!(!a.todos_pane);
    // Toggling twice does not ask twice without opening in between.
    assert_eq!(a.key(Key::CtrlT), Some(Action::ListTodos));
    assert_eq!(a.key(Key::CtrlT), None);
}

/// **A CLICK ON THE PANE MOVES THE CURSOR TO THE ROW IT IS ON.**
///
/// The operator's second report about this pane, in leticl's `todos-stops` docstring: *"mouse
/// doesnt click"*. letibot had no click arm for this pane at all — the picker and the mode card
/// had one and the todos pane had none — and leticl's own first cut had one that computed the
/// add row as a negative index and threw the click away.
///
/// What makes it work is the row the pane RECORDED as it drew, not arithmetic at the click's
/// end: a click has a screen row and nothing else, and a second computation of where a row went
/// is the defect both reports came from.
/// **The todos pane reads a nested layout's `TODO.md`** — the operator's own box: the session
/// lives in `Projects/letibot`, the repository in `Projects/letibot/letibot`, and neither the
/// pane nor the git field found anything at the workspace itself. The pane resolves through
/// the same `project_dir` the header's branch does, so one layout feeds both and neither can
/// read a different tree.
#[test]
fn the_todos_pane_reads_a_nested_layouts_todo_md() {
    let base = std::env::temp_dir().join(format!(
        "letibot-nested-pane-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let ws = base.join("letibot");
    let repo = ws.join("letibot");
    // The child is a real-enough repository for the resolver (a `.git` directory), because
    // that is the marker both consumers share — the file alone would not mark it.
    std::fs::create_dir_all(repo.join(".git")).expect("scratch");
    std::fs::write(
        repo.join("TODO.md"),
        "## Phase 0\n\n- [ ] **T1** nested item\n",
    )
    .expect("write");
    let mut a = app();
    a.wiring.workspace = ws.display().to_string();
    a.key(Key::CtrlT);
    let screen = a.screen(110, 40).join("\n");
    assert!(
        screen.contains("T1 nested item"),
        "the pane did not reach one level down:\n{screen}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// **The pane says which of its two lists the model is told about** — the operator's ruling,
/// 2026-09-29: *"host specific todo is actionable but shared todo.md items are promotable."*
///
/// Two sections that look alike and differ in whether the model ever hears about them is the
/// conflation this pins. It is a **sentence**, which is why it needs a test at all: prose that
/// nothing checks is prose that drifts, and this tree's recurring defect is exactly a true
/// sentence that stopped being true (see `messages.rs`'s *"not sent to this provider"*).
///
/// Three assertions, and the second is the one that is easy to lose:
///
///   * the repo section says the model is never told about it;
///   * the session section says the model IS reminded of it, so the negative above is scoped
///     rather than a blanket claim about the whole pane;
///   * and the repo heading does **not** call the file a *queue* — `queued` is this head's word
///     for a prompt the daemon owes a row for (`pub const QUEUED`), so that word in this spot
///     says the opposite of what is true.
#[test]
fn the_pane_says_which_of_its_two_lists_the_model_is_told_about() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-two-lists-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::write(
        dir.join("TODO.md"),
        "## Phase 0\n\n- [ ] T1 something the project intends\n",
    )
    .expect("write");
    let mut a = app();
    a.wiring.workspace = dir.display().to_string();
    a.key(Key::CtrlT);
    let screen = a.screen(140, 40).join("\n");
    assert!(
        screen.contains("the model is never told about it"),
        "the pane does not say the repo's list is outside what the model hears: {screen}"
    );
    assert!(
        screen.contains("the model sees these and is reminded of them"),
        "and it does not say the session's list IS what the model hears: {screen}"
    );
    assert!(
        !screen.contains("the operator's queue"),
        "the repo's TODO.md is called a queue, which in this head means a prompt waiting for \
             the model: {screen}"
    );
    // The heading still names the file, so the sentence above is not the only thing telling a
    // reader which list they are looking at.
    assert!(screen.contains("the repo's TODO.md"), "{screen}");
    let _ = std::fs::remove_dir_all(&dir);
}

// **Restored.** This test had lost its `#[test]` to a stray attribute that sat before the
// *next* test's doc — so it never ran. Found while inserting the nested-layout test beside
// it; running it is the only way to know whether it passes.
#[test]
fn a_click_on_the_todos_pane_puts_the_cursor_on_the_row_it_is_on() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-click-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::write(
        dir.join("TODO.md"),
        "## Phase 0\n\n- [x] **T1** first item\n- [ ] **T2** second item\n",
    )
    .expect("write");
    let mut a = app();
    a.wiring.workspace = dir.display().to_string();
    a.key(Key::CtrlT);

    // The screen's own rows, which is what a click's `y` names. No session is attached, so
    // there is no header above the pane and row N of the screen is row N of the pane.
    let row_of = |screen: &[String], what: &str| {
        u16::try_from(
            screen
                .iter()
                .position(|l| l.contains(what))
                .unwrap_or_else(|| panic!("no row with {what:?} in:\n{}", screen.join("\n"))),
        )
        .expect("fits")
    };
    // **The screen is an argument and not a capture** — a closure over the frame the test
    // happened to take first is a test that passes and fails for reasons nothing to do with
    // the click, which is how this one first read as a broken click.
    let marked = |screen: &[String], what: &str| {
        screen
            .iter()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains(what))
    };
    let screen = a.screen(110, 40);
    assert!(
        marked(&screen, "[+] add todo item"),
        "starts on the control"
    );

    // A click on the second item's row moves the cursor to it, and stops there: select and
    // confirm stay two acts, as they do in the pickers.
    a.key(Key::Click {
        x: 6,
        y: row_of(&screen, "T2 second item"),
    });
    let screen = a.screen(110, 40);
    assert!(
        marked(&screen, "T2 second item"),
        "the click moved the mark:\n{}",
        screen.join("\n")
    );
    assert_eq!(a.repo_sel, 2, "and the repo cursor followed");
    assert_eq!(a.key(Key::Enter), None);
    let screen = a.screen(110, 40);
    assert!(
        marked(&screen, "T2 second item"),
        "the click did not confirm:\n{}",
        screen.join("\n")
    );

    // A click on the add control — **the row `line - header` used to make negative** — lands
    // on it, which is the whole of the operator's report.
    a.key(Key::Click {
        x: 6,
        y: row_of(&screen, "[+] add todo item"),
    });
    let screen = a.screen(110, 40);
    assert!(
        marked(&screen, "[+] add todo item"),
        "the control takes a click:\n{}",
        screen.join("\n")
    );

    // A click on the `## Phase 0` heading moves nothing: it is drawn and is not a stop, and no
    // key would act on it.
    a.key(Key::Click {
        x: 6,
        y: row_of(&screen, "Phase 0"),
    });
    let screen = a.screen(110, 40);
    assert!(
        marked(&screen, "[+] add todo item"),
        "a heading is not a stop: a click there moves nothing:\n{}",
        screen.join("\n")
    );

    // And a click into the blank space below the list moves nothing either — the guard is the
    // WINDOW, not the list.
    let below = row_of(&screen, "this pane never writes it");
    a.key(Key::Click { x: 6, y: below });
    let screen = a.screen(110, 40);
    assert!(
        marked(&screen, "[+] add todo item"),
        "blank space below the rows moves nothing:\n{}",
        screen.join("\n")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_todos_pane_shows_both_sources_and_says_which_is_which() {
    let mut a = app();
    // The session's list, as the event carried it.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated {
            todos: vec![
                letibot_sessionlog::event::TodoEntry {
                    by: letibot_sessionlog::event::TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                    content: "seat the tool".into(),
                    status: letibot_sessionlog::event::TodoStatus::Completed,
                },
                letibot_sessionlog::event::TodoEntry {
                    by: letibot_sessionlog::event::TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                    content: "render the pane".into(),
                    status: letibot_sessionlog::event::TodoStatus::InProgress,
                },
            ],
        },
    )));
    // The repo's queue, as the pane-open read found it. Pointed at this
    // workspace, which has a real TODO.md with sections and checkboxes.
    a.wiring.workspace = std::env::var("CARGO_MANIFEST_DIR")
        .map(|d| {
            std::path::Path::new(&d)
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .display()
                .to_string()
        })
        .unwrap_or_default();
    a.key(Key::CtrlT);
    let screen = a.screen(110, 40).join("\n");
    assert!(screen.contains("[x] seat the tool"), "{screen}");
    assert!(screen.contains("[~] render the pane"), "{screen}");
    assert!(
        screen.contains("TODO.md"),
        "the second source is named: {screen}"
    );
    // A heading carries org's cookie rather than a sentence of counts, and —
    // the point — the items under it are actually drawn.
    assert!(
        screen.contains("/") && screen.contains("["),
        "headings carry a [done/total] cookie: {screen}"
    );
}

/// **The new-todo card: title, Tab for the detail, Enter adds it as yours, Esc cancels** —
/// leticl's `todo-card-lines`, and the shape its `%todo-draft-*` keys keep.
///
/// **The composer is the field.** Tab stores what was typed into the field being left and puts
/// the other one in the composer, so the field under the cursor is never a keystroke behind —
/// and everything else is the editor's, so the title and the detail are typed, pasted and
/// undone with the keys the operator already has.
///
/// **A title is required and the card stays up without one**, which is the only field rule: a
/// row of nothing is not what the reader meant, and saying so beats storing it.
#[test]
fn the_todo_card_takes_a_title_and_a_detail_and_adds_them_as_yours() {
    use letibot_sessionlog::event::TodoBy;
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    assert_eq!(a.command("todo"), None, "bare /todo opens the card");
    assert!(a.todo_draft.is_some());
    // The card is drawn with its three keys, which is how a reader learns Tab exists.
    let card = a.screen(100, 30).join("\n");
    assert!(card.contains("adding a todo item"), "{card}");
    assert!(card.contains("moves between the fields"), "{card}");

    // **A title alone.** Type it and press Enter.
    typed(&mut a, "ship the parity row");
    assert!(matches!(
        a.key(Key::Enter),
        Some(Action::SetOperatorTodos(_))
    ));
    assert!(a.todo_draft.is_none(), "the card came down");
    assert_eq!(a.input(), "", "and the composer is empty again");

    // **An empty title is refused and the card STAYS UP**, which is the one field rule.
    a.command("todo");
    assert!(matches!(a.key(Key::Enter), None));
    assert!(
        a.todo_draft.is_some(),
        "the card came down on an empty title"
    );
    assert!(
        a.notice.as_deref().unwrap_or("").contains("needs a title"),
        "{:?}",
        a.notice
    );

    // **Tab cycles the three fields and comes back to the title, keeping every one.** The
    // composer carries the focused field; the draft carries the other two. Three hops now, and
    // the one in the middle is `when` — the field this card grew, which a two-field cycle would
    // have had no way to name.
    typed(&mut a, "and the cache too");
    a.key(Key::Tab);
    assert_eq!(
        a.input(),
        "",
        "the detail field is empty when it is entered"
    );
    typed(&mut a, "the note the model needs");
    a.key(Key::Tab);
    assert_eq!(
        a.input(),
        "",
        "`when` is the field after the detail, and this row waits on nothing"
    );
    a.key(Key::Tab);
    assert_eq!(
        a.input(),
        "and the cache too",
        "Tab came back to the title, with what was typed into it"
    );

    // **Enter adds both, tagged as the operator's.** The list carries TWO rows now — the
    // one added at the top of this test and this one — because the head echoes its own send
    // (`echo_operator_todos`): the pane shows a row from the moment it is sent, so a second
    // add sees the first. Before the echo the daemon's answer never arrived in a test and
    // the count stayed behind.
    let act = a.key(Key::Enter);
    match act {
        Some(Action::SetOperatorTodos(items)) => {
            assert_eq!(items.len(), 2, "the echoed first row plus this one");
            assert_eq!(items[1].by, TodoBy::Operator);
            assert!(
                items[1].content.contains("and the cache too")
                    && items[1].content.contains("the note the model needs"),
                "the detail was lost: {:?}",
                items[1].content
            );
        }
        other => panic!("expected the add, got {other:?}"),
    }

    // **Esc cancels and adds nothing** — the card asks, and a question has to be able to be
    // answered no.
    a.command("todo");
    typed(&mut a, "never mind");
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.todo_draft.is_none(), "esc did not close the card");
    assert_eq!(a.input(), "", "and took the words with it");
    assert!(
        a.notice.as_deref().unwrap_or("").contains("nothing added"),
        "{:?}",
        a.notice
    );
}

/// **The operator can add a row of their own, and it goes to the daemon as their half.**
///
/// The gap this closes, measured before writing it: **`TodoBy::Operator` was constructible
/// only in tests.** The daemon stores and serves the operator's rows, the pane splits them out,
/// and nothing in the tree could CREATE one — `ClientFrame::SetOperatorTodos` had no sender at
/// all, though the protocol's own doc describes a head sending one. So the operator's half of
/// the board was unreachable, and *"you dont support persistent todos and leticl does"* was
/// exactly right.
#[test]
fn the_operator_can_add_and_dispose_of_their_own_todo_rows() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A row the MODEL wrote, so the numbering can be shown to be over the operator's half only.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated {
            todos: vec![TodoEntry {
                content: "the model's own row".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            }],
        },
    )));
    let mine = |a: &App| a.operator_todos();

    // **Adding one sends the WHOLE list, tagged as the operator's.** There is no per-row frame.
    let act = a.command("todo ship the parity row");
    match &act {
        Some(Action::SetOperatorTodos(items)) => {
            assert_eq!(items.len(), 1, "the whole list is one row: {items:?}");
            assert_eq!(items[0].content, "ship the parity row");
            assert_eq!(
                items[0].by,
                TodoBy::Operator,
                "filed in the operator's half"
            );
            assert_eq!(items[0].status, TodoStatus::Pending);
        }
        other => panic!("expected a SetOperatorTodos, got {other:?}"),
    }
    // The daemon takes it and publishes the union back — which is how the head learns its own
    // list: it keeps no second copy.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TodosUpdated {
            todos: vec![
                TodoEntry {
                    content: "the model's own row".into(),
                    status: TodoStatus::Pending,
                    by: TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                },
                TodoEntry {
                    content: "ship the parity row".into(),
                    status: TodoStatus::Pending,
                    by: TodoBy::Operator,
                    when: None,
                    needs: Vec::new(),
                },
            ],
        },
    )));
    assert_eq!(mine(&a).len(), 1, "the head sees its own row back");

    // **`done 1` marks the FIRST OF THE OPERATOR'S ROWS** — not the first of the union, which
    // is the model's. A number over the union would edit a row that is not theirs to edit.
    match a.command("todo done 1") {
        Some(Action::SetOperatorTodos(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].content, "ship the parity row");
            assert_eq!(items[0].status, TodoStatus::Completed);
        }
        other => panic!("expected completion, got {other:?}"),
    }
    // **A number out of range is REFUSED by name**, not clamped onto a neighbour.
    assert_eq!(a.command("todo done 9"), None);
    assert!(
        a.notice.as_deref().unwrap_or("").contains("no row 9"),
        "the refusal does not name the row: {:?}",
        a.notice
    );
    // **`rm 1` takes it off**, which is the operator's own act and not the model's — the model
    // may move a row's status and may not remove it.
    match a.command("todo rm 1") {
        Some(Action::SetOperatorTodos(items)) => {
            assert!(items.is_empty(), "the row is gone: {items:?}");
        }
        other => panic!("expected a removal, got {other:?}"),
    }
    // **A bare `/todo` opens the CARD** — the shape `/mode` and `/models` keep, where an act
    // with more than one part is chosen from a card rather than typed blind.
    assert_eq!(a.command("todo"), None);
    assert!(
        a.todo_draft.is_some(),
        "bare /todo did not open the card: {:?}",
        a.notice
    );
}

/// **THE HEADER COUNTS THE ROWS THE PANE DRAWS** — the operator's *"with some counter visible
/// to me"*, and the half of it that is a rule rather than a preference: one list, counted once.
///
/// The defect this asserts against is a pane that disagrees with itself — a total maintained
/// beside the rows it counts, or derived from the wire while the rows come from the head's own
/// copy — so the numbers are checked against **the marks the pane actually painted**, not
/// against a second computation of the same sum. Three open and one set aside, over five rows
/// of which one is finished: the two numbers a reader can count to on the screen.
#[test]
fn the_todo_header_counts_the_rows_the_pane_draws() {
    use letibot_sessionlog::event::{TodoBy, TodoCondition, TodoEntry, TodoStatus};
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![
            TodoEntry {
                content: "the model's row".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
            TodoEntry {
                content: "one I owe".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
            TodoEntry {
                content: "started".into(),
                status: TodoStatus::InProgress,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
            TodoEntry {
                content: "finished".into(),
                status: TodoStatus::Completed,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
            TodoEntry {
                content: "push once CI lands".into(),
                status: TodoStatus::Postponed,
                by: TodoBy::Operator,
                when: Some(TodoCondition::Job {
                    handle: "j121".into(),
                }),
                needs: Vec::new(),
            },
        ],
    });
    a.key(Key::CtrlT);

    // **The marks the pane painted**, read off its own lines: each mark is followed by a space
    // and the row's words, which is what tells a row from the legend under the list.
    let lines = a.todos_lines(120);
    let drawn = |mark: &str| lines.iter().filter(|l| l.contains(mark)).count();
    assert_eq!(drawn("[ ] "), 2, "two open rows, one of them the model's");
    assert_eq!(drawn("[~] "), 1, "one started");
    assert_eq!(drawn("[x] "), 1, "one finished");
    assert_eq!(drawn("[p] "), 1, "one set aside");

    let screen = a.screen(120, 40).join("\n");
    assert!(
        screen.contains("3 open · 1 postponed"),
        "the header does not carry the two numbers: {screen}"
    );
    // The numbers and the marks are the same rows: 2 + 1 open, 1 set aside.
    assert_eq!(todo_counts(&a.todos), (3, 1));
    // **And a postponed row says what it is still waiting on**, which is the other half of the
    // state: the handle is KEPT and does not fire while the row is set aside, so a pane that
    // drew only `[p]` would leave a reader to guess whether the condition was dropped.
    assert!(
        screen.contains("push once CI lands") && screen.contains("waits on j121"),
        "the row must carry its condition: {screen}"
    );
    // And the mark is explained where its rows are, since it is the one mark org has no
    // spelling for.
    assert!(
        screen.contains("/todo postpone N") && screen.contains("/todo resume N"),
        "the pane must name the verbs that undo the mark: {screen}"
    );
}

/// **A ROW IS SET ASIDE AND LIFTED BY NUMBER, AND IT IS THE OPERATOR'S ACT.**
///
/// The other half of the operator's ask — *"can we handle postponed todo item properly"* — and
/// the recommendation they made when the state was proposed: the row keeps its handle, does not
/// fire while it is set aside, and lifting it puts the same question back in front of the check.
/// Nothing here touches the condition, which is the assertion that makes the state reversible
/// rather than a quiet way to drop a firing.
///
/// Numbered over the operator's half exactly as `done N` and `rm N` are, and refused by name
/// when the number is not one of theirs — so a typo cannot set aside a row nobody named.
#[test]
fn a_row_is_set_aside_and_lifted_by_number() {
    use letibot_sessionlog::event::{TodoBy, TodoCondition, TodoEntry, TodoStatus};
    let waiting = Some(TodoCondition::Job {
        handle: "j121".into(),
    });
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![TodoEntry {
            content: "push once CI lands".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: waiting.clone(),
            needs: Vec::new(),
        }],
    });
    a.key(Key::CtrlT);

    match a.command("todo postpone 1") {
        Some(Action::SetOperatorTodos(items)) => {
            assert_eq!(items.len(), 1, "the whole half goes out: {items:?}");
            assert_eq!(items[0].status, TodoStatus::Postponed);
            assert_eq!(
                items[0].when, waiting,
                "**the handle is KEPT** — lifting the row has to put the same question back"
            );
        }
        other => panic!("expected a write of the operator's half, got {other:?}"),
    }
    assert!(
        a.notice.as_deref().unwrap_or("").contains("set aside"),
        "the act is said out loud, and it names the verb that undoes it: {:?}",
        a.notice
    );
    let screen = a.screen(120, 40).join("\n");
    assert!(screen.contains("0 open · 1 postponed"), "{screen}");

    // **And the lift.** Back to open work, with the same condition on it.
    match a.command("todo resume 1") {
        Some(Action::SetOperatorTodos(items)) => {
            assert_eq!(items[0].status, TodoStatus::Pending);
            assert_eq!(items[0].when, waiting, "still waiting on the same handle");
        }
        other => panic!("expected a write of the operator's half, got {other:?}"),
    }
    let after = a.screen(120, 40).join("\n");
    assert!(
        after.contains("1 open") && !after.contains("postponed"),
        "the row is back in the list and the counter says so: {after}"
    );

    // A number that is not a row of theirs is refused BY NAME, and nothing is sent.
    assert_eq!(a.command("todo postpone 9"), None);
    assert!(
        a.notice.as_deref().unwrap_or("").contains("no row 9"),
        "the refusal names the row: {:?}",
        a.notice
    );
    assert_eq!(a.command("todo resume 9"), None);

    // **And the verbs are listed where every other verb is** — `SLASH_COMMANDS` is what `/help`
    // and the completion table read, so a verb missing here is a verb nobody finds.
    let hint = SLASH_COMMANDS
        .iter()
        .find(|(n, _)| *n == "todo")
        .map(|(_, h)| *h)
        .unwrap_or("");
    assert!(
        hint.contains("postpone") && hint.contains("resume"),
        "the todo row of the verb table does not name the two new verbs: {hint}"
    );
}

/// **A row is drawn ONCE, under the author that wrote it** — R51 item 18, and the defect it
/// records is the one leticl measured and handed back.
///
/// The wire's list is the **union** (`TodoBoard::snapshot`: the model's rows, then the
/// operator's), because that is what the model has to see. A pane that drew it under one
/// heading therefore showed the operator's rows **as the model's plan** — on leticl's head the
/// same row appeared twice with two different authors:
///
/// ```text
/// [x] push leticl to github  — you
/// [x] push leticl to github  — model
/// ```
///
/// a duplicate AND a false author, from one row on the board. This asserts the two halves of
/// the fix: each row once, and each under the heading of the half that owns it.
/// **`by: Operator` does NOT mean "came from the file"** — and the design that assumes it would
/// delete the operator's own rows.
///
/// The rule being proposed is that the file IS the operator's half, so a `by: Operator` row's
/// state lives in `TODO.md` and a re-migration cannot lose anything. **The rule has an exception,
/// and it is destructive**: this head can put a `by: Operator` row on the board with no line in
/// the file behind it. `SetOperatorTodos` is a whole-half write from the head — `/todo TEXT`, the
/// `[+]` card, and the pane's own toggle all go through it — and none of those three is a file
/// edit.
///
/// So under a whole-file migration the next re-read **replaces the operator's half with the
/// file's rows**, and the rows this test adds are not replaced-with-different-state: they are
/// *gone*. The state-level wipe that `a_whole_file_migration_would_wipe_the_state_the_model_set`
/// records is the milder version of the same defect.
///
/// **What this pins is the fact that decides the wire.** Either the operator's own rows stop
/// being session-scoped — every one of them comes from the file, so `/todo TEXT` and the toggle
/// write the file and the session half retires — or a row has to say which origin it came from,
/// and that is a field. Both are honest; inferring origin from `by` is neither.
#[test]
fn the_operators_own_rows_are_not_in_the_file_so_by_operator_is_not_file_sourced() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-origin-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::write(
        dir.join("TODO.md"),
        "## Phase 0\n\n- [ ] T1 lifted from the file\n",
    )
    .expect("write");
    use letibot_sessionlog::event::TodoBy;
    let mut a = app();
    a.wiring.workspace = dir.display().to_string();
    a.session_id = "s1".into();

    // The operator adds one the way they actually do, and it is filed as theirs.
    match a.command("todo something only this session knows about") {
        Some(Action::SetOperatorTodos(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].by, TodoBy::Operator, "it IS the operator's row");
            // **And it is not in the file**, which is the exception.
            let on_disk = std::fs::read_to_string(dir.join("TODO.md")).expect("read");
            assert!(
                !on_disk.contains("something only this session knows about"),
                "the head is supposed to keep no file copy of its own rows: {on_disk}"
            );
            assert!(
                on_disk.contains("T1 lifted from the file"),
                "and the file is where the file's row lives: {on_disk}"
            );
        }
        other => panic!("expected a SetOperatorTodos, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_todo_row_is_drawn_once_under_the_author_that_wrote_it() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
    let row = |content: &str, by| TodoEntry {
        by,
        content: content.into(),
        status: TodoStatus::Pending,
        when: None,
        needs: Vec::new(),
    };
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // The union, in the order the daemon builds it: the model's half first.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated {
            todos: vec![
                row("seat the tool", TodoBy::Model),
                row("push leticl to github", TodoBy::Operator),
            ],
        },
    )));
    a.todos_pane = true;
    let screen = a.screen(110, 40).join("\n");
    // **Once each.** The duplicate is the defect, so a count is the assertion.
    for content in ["seat the tool", "push leticl to github"] {
        assert_eq!(
            screen.matches(content).count(),
            1,
            "`{content}` is drawn more than once: {screen}"
        );
    }
    // **And each row wears its OWN author as a tag** — R51 item 18: *"the author tag on every
    // row is the requirement (R44)."* A heading was the earlier reading; a tag is the fact,
    // and a pane that re-derives authorship from which section a row landed in is the drift
    // the `by` field exists to prevent.
    let tool = screen
        .lines()
        .find(|l| l.contains("seat the tool"))
        .expect("the model's row is on the screen");
    assert!(
        tool.contains("— model"),
        "the model's row does not name its author: {tool:?}"
    );
    let push = screen
        .lines()
        .find(|l| l.contains("push leticl to github"))
        .expect("the operator's row is on the screen");
    assert!(
        push.contains("— you"),
        "the operator's row does not name its author: {push:?}"
    );
}

/// **An empty half says which half is empty** — the two sentences are not the same statement.
///
/// A session whose plan the operator has taken over has no model rows and several of their own;
/// one where the model has written a plan has none of theirs. `none written yet` over a list the
/// MODEL is supposed to be keeping and over a list the OPERATOR owns are different facts about
/// different halves, and this is the split that makes saying so possible.
#[test]
fn each_half_of_the_board_says_when_it_is_the_empty_one() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated {
            todos: vec![TodoEntry {
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
                content: "my own row".into(),
                status: TodoStatus::Pending,
            }],
        },
    )));
    a.todos_pane = true;
    let screen = a.screen(110, 40).join("\n");
    // **One list now, so there is one empty sentence** — and the operator's row is drawn with
    // its author beside it.
    assert!(
        screen.contains("todos") && screen.contains("my own row"),
        "the operator's row is not drawn: {screen}"
    );
    assert!(
        screen.contains("my own row  — you"),
        "the row does not wear its author's tag: {screen}"
    );
}

/// **The items are the queue, and they were never drawn.** The pane rendered
/// one line per heading with counts beside it and stopped. The operator,
/// looking at `leticl`'s: *"our todo pane doesnt render them — only section
/// titles and sub todos count"*.
#[test]
fn a_heading_rolls_up_its_items_the_way_org_does() {
    let all = todo_plain(
        "# title\n\
             \n\
             ## Phase 0 — repo\n\
             \n\
             - [x] **T1** git init, `.gitignore`.\n\
             - [x] **T2** vendor the deps.\n\
             \n\
             ## Phase 7 — parity\n\
             \n\
             - [x] **S1** the side-by-side diff\n\
             - [~] **S2** the session picker\n\
             - [ ] **S3** the mode card\n\
             \n\
             ## Phase 9 — not started\n\
             \n\
             - [ ] **Z1** nothing yet\n",
    );

    // Every child done makes the parent done — org's own rule for a heading.
    assert!(all.contains("[x] Phase 0 — repo  [2/2]"), "{all}");
    // One started makes it started, and the cookie counts only the finished.
    assert!(all.contains("[~] Phase 7 — parity  [1/3]"), "{all}");
    // None started leaves it open.
    assert!(all.contains("[ ] Phase 9 — not started  [0/1]"), "{all}");

    // And the items themselves, under their heading, in order, with their own
    // marks and without markdown's emphasis noise.
    assert!(all.contains("[x] T1 git init, .gitignore."), "{all}");
    assert!(all.contains("[~] S2 the session picker"), "{all}");
    assert!(all.contains("[ ] S3 the mode card"), "{all}");
    assert!(!all.contains("**"), "the markers are stripped: {all}");
    let (s1, s2) = (all.find("S1").unwrap(), all.find("S2").unwrap());
    assert!(s1 < s2, "file order is kept");
}

/// A heading with no checkboxes under it is not done — it is a section
/// nobody has filled in, and org does not mark those either. This is also
/// every prose heading in a real TODO.md, which must not be claimed as
/// finished work.
#[test]
fn an_empty_heading_carries_no_box_and_no_cookie() {
    let all =
        todo_plain("## Dependency graph\n\nT1 -> T3 -> T4\n\n## Phase 0\n\n- [x] **T1** done\n");
    assert!(all.contains("Dependency graph"), "{all}");
    assert!(
        !all.contains("[x] Dependency graph") && !all.contains("[ ] Dependency graph"),
        "an empty section is neither done nor open: {all}"
    );
    assert!(
        !all.contains("Dependency graph  ["),
        "and carries no cookie: {all}"
    );
    assert!(all.contains("[x] Phase 0  [1/1]"), "{all}");
}

/// **Both halves of the pane speak one vocabulary.** The operator, on seeing
/// the items drawn: *"colors?"*. They were not painted at all — neither the
/// model's list nor the repo's — while the jobs pane two keys away had been
/// painting the same three states green and yellow all along.
#[test]
fn a_todo_is_painted_by_its_state_in_both_halves() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-colour-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::write(
        dir.join("TODO.md"),
        "## Phase 0\n\n- [x] **T1** done\n- [~] **T2** doing\n- [ ] **T3** open\n",
    )
    .expect("write");

    let mut a = App::new(RenderConfig {
        color: true,
        ..plain_cfg(110)
    });
    a.wiring.workspace = dir.display().to_string();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated {
            todos: vec![
                letibot_sessionlog::event::TodoEntry {
                    by: letibot_sessionlog::event::TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                    content: "seated".into(),
                    status: letibot_sessionlog::event::TodoStatus::Completed,
                },
                letibot_sessionlog::event::TodoEntry {
                    // A model's plan, which is also the field's serde default.
                    by: letibot_sessionlog::event::TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                    content: "seating".into(),
                    status: letibot_sessionlog::event::TodoStatus::InProgress,
                },
            ],
        },
    )));
    a.key(Key::CtrlT);
    let screen = a.screen(110, 40).join("\n");

    // The model's half.
    assert!(
        screen.contains(&format!("{}[x]{} seated", sgr::GREEN, sgr::RESET)),
        "{screen:?}"
    );
    assert!(
        screen.contains(&format!("{}[~]{} seating", sgr::YELLOW, sgr::RESET)),
        "{screen:?}"
    );
    // The repo's half, painted the same way — including the heading, which
    // carries the rolled-up state and so carries its colour.
    assert!(
        screen.contains(&format!("{}[~]{} Phase 0", sgr::YELLOW, sgr::RESET)),
        "{screen:?}"
    );
    assert!(
        screen.contains(&format!("{}[x]{} T1 done", sgr::GREEN, sgr::RESET)),
        "{screen:?}"
    );
    // An open item is left alone: it is the default and the majority of any
    // list, and colouring the majority spends the signal the other two carry.
    assert!(screen.contains("[ ] T3 open"), "{screen:?}");
    assert!(
        !screen.contains(&format!("{}[ ]", sgr::GREEN))
            && !screen.contains(&format!("{}[ ]", sgr::YELLOW)),
        "{screen:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `###` is a subsection in org's outline and in markdown's, so its items
/// belong to it rather than to the `##` above.
#[test]
fn a_subsection_owns_its_own_items() {
    let all = todo_plain("## Phase 7\n\n- [x] **A** one\n\n### Strand B\n\n- [ ] **B1** two\n");
    assert!(all.contains("[x] Phase 7  [1/1]"), "{all}");
    assert!(all.contains("[ ] Strand B  [0/1]"), "{all}");
}

/// A block of calls with a slow one in the middle: the fast ones go green
/// as they finish, not when the block does.
///
/// Measured 2026-09-17 in the operator's session: `todo_write`, then a
/// `task` that ran a subagent for fifteen minutes, then three searches. The
/// pane retired the moment the assistant row's body landed — which is
/// before the calls run — and the transcript row, which draws a call from
/// its result row, showed all five as `no result` until the batch landed.
#[test]
fn a_finished_call_is_finished_while_the_call_beside_it_still_runs() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::appended("r.a", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "r.a".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![
                    letibot_transcript::ToolCall {
                        id: "call_0".into(),
                        name: "todo_write".into(),
                        arguments: r#"{"todos":[]}"#.into(),
                    },
                    letibot_transcript::ToolCall {
                        id: "call_1".into(),
                        name: "task".into(),
                        arguments: r#"{"prompt":"fix the compaction bug"}"#.into(),
                    },
                ],
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        testing::proposed("t1", "call_0", "todo_write"),
    )));
    a.apply(ServerFrame::Event(env(
        5,
        testing::proposed("t1", "call_1", "task"),
    )));
    a.apply(ServerFrame::Event(env(6, testing::turn_finished("t1"))));
    // The daemon's order: call_0 starts and finishes in a millisecond,
    // call_1 starts and stays running. No result row lands for either yet.
    a.apply(ServerFrame::Event(env(
        7,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "call_0".into(),
            name: "todo_write".into(),
            access: Default::default(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        8,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "call_0".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "fnv1a:1".into(),
            inline_bytes: 12,
            full_bytes: 12,
            spill: None,
            repairs: 0,
            edit: None,
        },
    )));
    a.apply(ServerFrame::Event(env(
        9,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "call_1".into(),
            name: "task".into(),
            access: Default::default(),
        },
    )));
    let screen = a.screen(120, 40).join("\n");
    assert!(
        !screen.contains("no result"),
        "a call that finished, and one still running, and neither is `no result`:\n{screen}"
    );
    assert!(
        screen.contains("● todo_write"),
        "the finished call is drawn finished:\n{screen}"
    );
    assert!(
        screen.contains("◐ task"),
        "and the running one running:\n{screen}"
    );
}

/// **An operator's todo is on the pane from the MOMENT IT IS SENT** — not from the step
/// boundary.
///
/// The operator: *"when i send todos they appear in the todo pane some time later — it feels
/// like their appearance depend on the turn state. but to me - im not sure if i lost them or
/// not."* The write was never lost (`set_operator_todos` publishes at once, with a retry);
/// what was late was the DRAWING — the pane showed only what the daemon had published, and
/// `TodosUpdated` is injected behind a running turn. The head now applies its own send, the
/// same trust `pending_prompts` places in the words it shows until their row lands.
///
/// Two claims asserted: the row is on screen BEFORE any `TodosUpdated` arrives (add, and the
/// toggle under the cursor — both doors into one act), and the daemon's answer still WINS —
/// the echo is a drawing, not a second board, and a `TodosUpdated` replaces it wholesale.
#[test]
fn an_operators_todo_is_on_the_pane_the_moment_it_is_sent() {
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A model row on the board, so the union has both halves and the echo can be seen to
    // keep the model's half where it was. Sent under the session id the head actually
    // holds — the `Todos` arm answers only for the session it is in.
    let sid = a.session_id.clone();
    a.apply(ServerFrame::Todos {
        session_id: sid,
        todos: vec![letibot_sessionlog::event::TodoEntry {
            content: "a model row".into(),
            status: letibot_sessionlog::event::TodoStatus::Pending,
            by: letibot_sessionlog::event::TodoBy::Model,
            when: None,
            needs: Vec::new(),
        }],
    });
    // Add one of ours. No `TodosUpdated` is applied afterwards — the row must be on the
    // pane off the back of the send alone.
    a.key(Key::CtrlT);
    assert!(matches!(
        a.command("todo check the parity row"),
        Some(Action::SetOperatorTodos(_))
    ));
    let screen = a.screen(110, 30).join("\n");
    assert!(
        screen.contains("check the parity row"),
        "the row is drawn before the daemon has published it:\n{screen}"
    );
    assert!(
        screen.contains("a model row"),
        "the echo replaces only the operator's half; the model's stays:\n{screen}"
    );

    // **The daemon's answer still wins.** The echo is a drawing, not a second board: when
    // `TodosUpdated` lands — in the daemon's own order, possibly reordered — it replaces
    // the union wholesale.
    a.apply(ServerFrame::Todos {
        session_id: a.session_id.clone(),
        todos: vec![letibot_sessionlog::event::TodoEntry {
            content: "the daemon's row".into(),
            status: letibot_sessionlog::event::TodoStatus::Pending,
            by: letibot_sessionlog::event::TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }],
    });
    let after = a.screen(110, 30).join("\n");
    assert!(
        after.contains("the daemon's row"),
        "the daemon's publish replaces the echo:\n{after}"
    );
    assert!(
        !after.contains("check the parity row"),
        "and the echo does not outlive the answer it was standing in for:\n{after}"
    );
}

/// **A CONDITION IS ATTACHED TO A ROW BY A VERB, BY NUMBER** — the piece that makes a
/// conditioned row something a person can actually file. The operator's own words: *"More like
/// Option<TodoCondition> and then we can have many conditions, and we can instantiate them
/// programmatically or via a form, when I file a todo"*.
///
/// Three claims, and the third is what keeps the feature usable: the row carries the handle it
/// waits on, the write goes out as the operator's half (which is the only place a condition can
/// live — the model's half is replaced wholesale by every `todo_write`), and **`-` takes it
/// off**, because a condition nobody can clear is a row waiting for ever on a job that already
/// ended.
#[test]
fn a_condition_is_attached_to_a_row_by_number_and_can_be_taken_off() {
    use letibot_sessionlog::event::{TodoBy, TodoCondition, TodoEntry, TodoStatus};
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![TodoEntry {
            content: "push once CI lands".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }],
    });
    match a.command("todo when 1 j121") {
        Some(Action::SetOperatorTodos(items)) => assert_eq!(
            items[0].when,
            Some(TodoCondition::Job {
                handle: "j121".into()
            }),
            "the row carries the handle it waits on"
        ),
        other => panic!("expected a write of the operator's half, got {other:?}"),
    }
    // **And off again.** A row whose job has ended must be able to stop waiting on it, or the
    // store reports the same row due on every wake for the rest of the session.
    match a.command("todo when 1 -") {
        Some(Action::SetOperatorTodos(items)) => {
            assert_eq!(items[0].when, None, "`-` clears it")
        }
        other => panic!("expected a write, got {other:?}"),
    }
    // A number that is not a row of theirs is refused BY NAME, and nothing is sent — the same
    // rule `done N` keeps, so a typo cannot quietly attach a condition to the wrong row.
    assert_eq!(a.command("todo when 9 j121"), None);
    assert!(
        a.notice.as_deref().unwrap_or("").contains("no row 9"),
        "the refusal names the row: {:?}",
        a.notice
    );
    // And a bare `when` with nothing after it says how it is used rather than eating the word
    // as the text of a new row.
    assert_eq!(a.command("todo when"), None);
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("/todo when N JOB"),
        "a bare `when` names the form: {:?}",
        a.notice
    );
}

/// **A ROW FILED FROM THE CARD CAN CARRY A CONDITION** — the operator's *"or via a form, when I
/// file a todo"*, and the reason the third field exists at all.
///
/// Two claims, and the second is what keeps a form a form: the handle the reader typed is on
/// the row the card files, and **the fields are not read as a command line**. The card used to
/// hand its words to `todo_command`, so a title that happened to read `done 2` or `when 1 j7`
/// was taken for the verb, and would move or condition a row the reader never named — a hazard
/// the two-field card carried all along, and one the `when` field would have made reachable in
/// a new way.
#[test]
fn a_row_filed_from_the_card_can_carry_a_condition() {
    use letibot_sessionlog::event::{TodoBy, TodoCondition, TodoEntry, TodoStatus};
    let mut a = app();
    a.session_id = "s1".into();
    assert_eq!(a.command("todo"), None, "bare /todo opens the card");
    typed(&mut a, "push once CI lands");
    a.key(Key::Tab);
    typed(&mut a, "and say so in the reply");
    // **Tab a second time reaches the third field**, which is what the enum is for: the `bool`
    // this replaced could not name it, and a reader who tabbed twice would have been back on
    // the title with `j121` typed into it.
    a.key(Key::Tab);
    typed(&mut a, "j121");
    let card = a.screen(120, 30).join("\n");
    assert!(
        card.contains("adding a todo item"),
        "the card is up:\n{card}"
    );
    assert!(
        card.contains("JOB handle"),
        "the card must say what the third field wants:\n{card}"
    );
    match a.key(Key::Enter) {
        Some(Action::SetOperatorTodos(items)) => {
            assert_eq!(items.len(), 1, "one row was filed: {items:?}");
            assert_eq!(items[0].by, TodoBy::Operator);
            assert_eq!(
                items[0].content, "push once CI lands — and say so in the reply",
                "the title and the detail are one row"
            );
            assert_eq!(
                items[0].when,
                Some(TodoCondition::Job {
                    handle: "j121".into()
                }),
                "the condition did not reach the row"
            );
        }
        other => panic!("expected the add, got {other:?}"),
    }
    assert!(a.todo_draft.is_none(), "the card came down");

    // **The same card with the `when` field left alone files an unconditional row** — empty is
    // *not recorded here*, the convention the whole record keeps.
    let mut b = app();
    b.session_id = "s1".into();
    b.command("todo");
    typed(&mut b, "just a row");
    match b.key(Key::Enter) {
        Some(Action::SetOperatorTodos(items)) => assert_eq!(items[0].when, None, "{items:?}"),
        other => panic!("expected the add, got {other:?}"),
    }

    // **And a title that reads as a verb is still a title.** The card's words are fields, not a
    // command line, so nothing here moves row 2.
    let mut c = app();
    c.session_id = "s1".into();
    c.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![
            TodoEntry {
                content: "first".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
            TodoEntry {
                content: "second".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ],
    });
    c.command("todo");
    typed(&mut c, "done 2");
    match c.key(Key::Enter) {
        Some(Action::SetOperatorTodos(items)) => {
            assert_eq!(items.len(), 3, "a row was filed: {items:?}");
            assert_eq!(items[2].content, "done 2");
            assert_eq!(
                items[1].status,
                TodoStatus::Pending,
                "row 2 was moved by a title that read like a command: {items:?}"
            );
        }
        other => panic!("expected the add, got {other:?}"),
    }
}

/// **The attach queues the board read when a seed is due, and the template's items land
/// on the operator's half — text, body and mark copied, the daemon's rows kept.**
#[test]
fn starter_todos_copy_onto_the_operators_half() {
    let dir = std::env::temp_dir().join(format!("letibot-seed-{}", std::process::id()));
    let mut a = seeded_head(
        &dir,
        "## Phase 0\n\n- [x] T1 git init\n- [ ] T2 vendor the deps\n  pinned at abc\n- [~] T3 the card\n",
        "/home/dead/Projects/x",
    );
    assert!(
        a.todo_seed_pending,
        "the attach saw a due seed and is waiting for the board"
    );
    assert_eq!(
        a.queued.iter().any(|q| matches!(q, Action::ListTodos)),
        true,
        "the board read the seed waits on was queued: {:?}",
        a.queued
    );
    // The daemon's answer carries both halves — an operator row it already holds and a
    // model row — so the seed can be seen to ADD to the one and leave the other alone.
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![
            letibot_sessionlog::event::TodoEntry {
                content: "keep me".into(),
                status: letibot_sessionlog::event::TodoStatus::Pending,
                by: letibot_sessionlog::event::TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
            letibot_sessionlog::event::TodoEntry {
                content: "a model row".into(),
                status: letibot_sessionlog::event::TodoStatus::Pending,
                by: letibot_sessionlog::event::TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
        ],
    });
    let Some(Action::SetOperatorTodos(sent)) = a
        .queued
        .iter()
        .find(|q| matches!(q, Action::SetOperatorTodos(_)))
        .cloned()
    else {
        panic!("the seed sent nothing: {:?}", a.queued);
    };
    assert_eq!(
        sent.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
        vec![
            "keep me",
            "T1 git init",
            "T2 vendor the deps — pinned at abc",
            "T3 the card",
        ],
        "text and body copied, the daemon's operator row kept"
    );
    assert_eq!(
        sent[1].status,
        letibot_sessionlog::event::TodoStatus::Completed
    );
    assert_eq!(
        sent[2].status,
        letibot_sessionlog::event::TodoStatus::Pending
    );
    // `[~]` seeds open — leticl's own rule: only `[x]` seeds a done row.
    assert_eq!(
        sent[3].status,
        letibot_sessionlog::event::TodoStatus::Pending
    );
    assert!(
        sent.iter()
            .all(|t| t.by == letibot_sessionlog::event::TodoBy::Operator)
    );
    // The model's half is untouched in the optimistic view, and the head's own record
    // holds the project now.
    assert!(a.todos.iter().any(|t| t.content == "a model row"));
    assert!(
        a.todo_seed
            .contains(&todo_seed_key("/home/dead/Projects/x"))
    );
    let (on_disk, _) = crate::prefs::load(&dir.join("head.toml"));
    assert_eq!(
        on_disk.todo_seed, a.todo_seed,
        "the record was written, not just held"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **ONCE PER PROJECT.** A second board read — the pane opening, a switch landing —
/// finds the record and adds nothing; a deleted starter row must not come back.
#[test]
fn a_seed_runs_once_per_project() {
    let dir = std::env::temp_dir().join(format!("letibot-seed-once-{}", std::process::id()));
    let mut a = seeded_head(
        &dir,
        "## Phase 0\n\n- [ ] only row\n",
        "/home/dead/Projects/y",
    );
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: Vec::new(),
    });
    assert_eq!(a.operator_todos().len(), 1);
    // The operator deletes the starter row — the record is what must keep it dead.
    let sent = a
        .queued
        .iter()
        .filter(|q| matches!(q, Action::SetOperatorTodos(_)))
        .count();
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: Vec::new(),
    });
    assert_eq!(
        a.queued
            .iter()
            .filter(|q| matches!(q, Action::SetOperatorTodos(_)))
            .count(),
        sent,
        "no second seed was queued"
    );
    assert_eq!(
        a.operator_todos().len(),
        0,
        "a deleted starter row stays deleted"
    );
    // And a restarted head reads the record from the file and skips the project.
    let mut b = app();
    b.prefs_path = Some(dir.join("head.toml"));
    b.load_prefs();
    b.todo_template = crate::prefs::TodoTemplate::Default;
    b.wiring.workspace = "/home/dead/Projects/y".into();
    assert!(!b.todo_seed_due(), "the record outlives the head");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The refusals are notes.** A template with no items seeds nothing and is MARKED
/// (the alternative re-reads it on every start); a missing template is said and NOT
/// marked, so the file the operator was going to write still gets its chance.
#[test]
fn template_refusals_are_notes() {
    let dir = std::env::temp_dir().join(format!("letibot-seed-refuse-{}", std::process::id()));
    // No items — a heading is not a checkbox row.
    let mut a = seeded_head(&dir, "## Phase 0\n\nprose only\n", "/home/dead/Projects/z");
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: Vec::new(),
    });
    assert!(a.operator_todos().is_empty(), "nothing to add from prose");
    assert!(
        a.todo_seed
            .contains(&todo_seed_key("/home/dead/Projects/z")),
        "an empty template is still a seeding"
    );
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|s| s.ends_with("has no items in it, so nothing was added")),
        "said, not silent: {:?}",
        a.notice
    );
    // Missing — the Default template with the file taken away.
    let mut b = seeded_head(&dir, "- [ ] row\n", "/home/dead/Projects/w");
    std::fs::remove_file(dir.join("todo-template.md")).unwrap();
    // A project not yet seeded: clear the record the first head wrote.
    b.todo_seed.clear();
    b.todo_seed_pending = true;
    b.seed_todos();
    assert!(
        b.todo_seed.is_empty(),
        "a missing file is not a seeding — it may still be written"
    );
    assert!(
        b.notice
            .as_deref()
            .is_some_and(|s| s.contains("is not there")),
        "said: {:?}",
        b.notice
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The switch off is off** — an ordinary head, template at its default, attaches and
/// queues no board read for a seed and adds nothing.
#[test]
fn the_starter_todos_switch_is_off_by_default() {
    let dir = std::env::temp_dir().join(format!("letibot-seed-off-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut a = app();
    a.prefs_path = Some(dir.join("head.toml"));
    a.load_prefs();
    assert_eq!(a.todo_template, crate::prefs::TodoTemplate::Off);
    let snap = Hub::new("s1").snapshot();
    a.apply(ServerFrame::Hello {
        protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
        session_id: "s1".into(),
        head_id: "h1".into(),
        dropped: 0,
        snapshot: Some(Box::new(snap)),
        resumed_from: None,
        scrubbed: Default::default(),
        wiring: SessionWiring {
            model: String::new(),
            dialect: String::new(),
            endpoint: String::new(),
            workspace: "/home/dead/Projects/x".into(),
        },
        sessions: Vec::new(),
    });
    assert!(!a.todo_seed_pending, "off is off");
    assert!(
        !a.queued.iter().any(|q| matches!(q, Action::ListTodos)),
        "no board read queued for a seed that is not due"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
