//! Todos: the plan, the todo card, the repository queue.

use super::*;

/// The todos pane's bar names the keys that pane answers: the footer was rano's pinned sentence, and *enter or tab
/// unfolds* was false on two of the three stops the cursor could be on.
#[test]
fn the_todos_pane_footer_names_the_keys_the_pane_answers() {
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
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated {
            todos: vec![row("my own row", TodoBy::Operator)],
        },
    )));
    a.push_surface(Surface::Todos);

    // **The add control**, which is where the cursor starts.
    a.todos_sel = 0;
    let bar = a.hint_bar(200);
    assert!(bar.contains("↑↓ moves"), "the movement key is named: {bar}");
    assert!(
        bar.contains("enter or tab opens the card"),
        "the add control's act is opening the card, not unfolding anything: {bar}"
    );
    assert!(
        bar.contains("esc closes"),
        "and the way out is named, from the tail: {bar}"
    );
    assert!(!bar.contains("unfolds"), "nothing here unfolds: {bar}");

    // **One of the operator's own rows** — one Down from the add control. TAB toggles it, which
    // is the sentence rano's pinned footer got wrong, and `/todo rm` is named because deletion is
    // the operator's own act and this is the only place it is spelled.
    a.todos_sel = 1;
    let bar = a.hint_bar(200);
    assert!(
        bar.contains("tab toggles it done"),
        "the act on the operator's row is the toggle: {bar}"
    );
    assert!(
        bar.contains("enter reads it whole"),
        "and enter, which reads the row, is named beside it: {bar}"
    );
    assert!(
        !bar.contains("unfolds"),
        "**the lie is gone**: the old footer said `unfolds` on a row where enter marks it done: {bar}"
    );
    assert!(
        bar.contains("/todo rm N"),
        "and the operator's own delete verb is discoverable: {bar}"
    );

    // **A repo item unfolds**, which is the one stop where the old sentence was true — and it is
    // still said, so the fix is a correction rather than a deletion. Enter reads it; Tab unfolds it.
    a.repo_todos = Some(vec![TodoRow {
        indent: 0,
        mark: Some(TodoMark::Open),
        text: "an item in the file".into(),
        body: Vec::new(),
        item: true,
    }]);
    a.todos_sel = 2;
    let bar = a.hint_bar(200);
    assert!(
        bar.contains("tab unfolds the item"),
        "the repo's own act is still named: {bar}"
    );

    // **A model's row: the read and nothing else.** No key of this pane moves one — the R44
    // boundary — and the bar says only what it does do rather than promising an act.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TodosUpdated {
            todos: vec![row("a row the model wrote", TodoBy::Model)],
        },
    )));
    a.todos_sel = 1;
    let bar = a.hint_bar(200);
    assert!(
        bar.contains("enter reads it whole"),
        "the model's row can be read: {bar}"
    );
    assert!(
        !bar.contains("toggles") && !bar.contains("unfolds"),
        "and the bar promises no act on it: {bar}"
    );
}

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
    // The fact, not the word: this asserted `read-only`, which was the old heading's wording, so a wording change
    // broke a test about a fact that had not changed.
    assert!(
        screen.contains("this pane never writes it"),
        "the pane does not say it leaves the file alone: {screen}"
    );
    // And Esc is "go back", before the composer sees it.
    a.key(Key::Esc);
    assert!(!a.is_open(Surface::Todos));
    // Toggling twice does not ask twice without opening in between.
    assert_eq!(a.key(Key::CtrlT), Some(Action::ListTodos));
    assert_eq!(a.key(Key::CtrlT), None);
}

/// A click on the pane moves the cursor to the row it is on: letibot had no click arm for this pane at all, since
/// the picker and the mode card had one and this pane did not.
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

/// The pane says which of its two lists the model is told about: two sections that look alike and differ in whether
/// the model ever hears about them is the conflation this pins, and prose that says so needs a test.
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
    // **Enter does not confirm the click — it READS the row it landed on**, which is the same
    // two-acts rule said one key over: the click moved the cursor, and the read is what Enter does
    // with it. Esc puts the reader back on the same row.
    assert_eq!(a.key(Key::Enter), None);
    assert_eq!(a.todos_open, Some(TodoStop::Repo(2)));
    a.key(Key::Esc);
    let screen = a.screen(110, 40);
    assert!(
        marked(&screen, "T2 second item"),
        "the click did not confirm, and esc came back to the row:\n{}",
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
    // **The board is read UNFOLDED here**, because the subject is that both sources are drawn and
    // named: the folded default takes a `[x]` row out of both halves, and `d` has its own tests.
    a.todos_fold_done = false;
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

/// The new-todo card: title, Tab for the detail, Enter adds it as yours, Esc cancels. The composer is the field, so
/// Tab stores what was typed into the field being left and puts the other one in the composer, and the field under
/// the cursor is never a keystroke behind.
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
        Some(Action::SetOperatorTodos { .. })
    ));
    assert!(a.todo_draft.is_none(), "the card came down");
    assert_eq!(a.input(), "", "and the composer is empty again");

    // **An empty title is refused and the card STAYS UP**, which is the one field rule.
    a.command("todo");
    assert!(a.key(Key::Enter).is_none());
    assert!(
        a.todo_draft.is_some(),
        "the card came down on an empty title"
    );
    assert!(
        a.notice.as_deref().unwrap_or("").contains("needs a title"),
        "{:?}",
        a.notice
    );

    // Tab cycles the three fields and comes back to the title, keeping every one: the composer carries the focused
    // field and the draft the other two. The middle one is `when`, which a two-field cycle could not have named.
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

    // Enter adds both, tagged as the person's. The list carries two rows now, since the head echoes its own send
    // (`echo_operator_todos`) and a second add sees the first.
    let act = a.key(Key::Enter);
    match act {
        Some(Action::SetOperatorTodos { items, .. }) => {
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

/// The person can add a row of their own and it goes to the daemon as their half: `TodoBy::Operator` was
/// constructible only in tests, and nothing in the tree could create one, since `SetOperatorTodos` had no sender.
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
        Some(Action::SetOperatorTodos { items, .. }) => {
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
        Some(Action::SetOperatorTodos { items, .. }) => {
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
    // `rm 1` strikes it off and the row stays: the row is marked `cancelled` and the daemon keeps it, because
    // everything that was ever created stays in history.
    match a.command("todo rm 1") {
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items.len(), 1, "the row is still there: {items:?}");
            assert_eq!(items[0].content, "ship the parity row");
            assert_eq!(
                items[0].status,
                TodoStatus::Cancelled,
                "**struck off, not removed**: {items:?}"
            );
        }
        other => panic!("expected a write of the operator's half, got {other:?}"),
    }
    // **And the model cannot do the same.** The word is not one `todo_write` takes, and the
    // refusal names the three it does — so a row can never be retired by the half that wrote it,
    // which is the whole of the ruling.
    assert!(
        a.notice.as_deref().unwrap_or("").contains("struck off"),
        "the act is said out loud, and says the row stays: {:?}",
        a.notice
    );
    // **A bare `/todo` opens the CARD** — the shape `/mode` and `/models` keep, where an act
    // with more than one part is chosen from a card rather than typed blind.
    assert_eq!(a.command("todo"), None);
    assert!(
        a.todo_draft.is_some(),
        "bare /todo did not open the card: {:?}",
        a.notice
    );
}

/// The header counts the rows the pane draws: one list, counted once. The defect is a pane that disagrees with
/// itself, a total maintained beside the rows it counts or derived from the wire while the rows come from the head.
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
    // **Every state is drawn, so the board is read UNFOLDED** — the numbers below are counted
    // against the marks the pane paints, and the folded default paints no `[x]` at all.
    a.todos_fold_done = false;
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

/// A row is set aside and lifted by number, and it is the person's act: the row keeps its handle, does not fire
/// while set aside, and lifting it puts the same question back in front of the check.
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
        Some(Action::SetOperatorTodos { items, .. }) => {
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
        Some(Action::SetOperatorTodos { items, .. }) => {
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

/// A row the model wrote can be set aside by the person: the verb numbered only the person's own rows and
/// `todo_write` refuses `postponed`, so an accurate row waiting on a person had no name any verb could use.
#[test]
fn a_row_the_model_wrote_can_be_set_aside_by_its_words() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoState, TodoStatus};
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![
            TodoEntry {
                content: "operator: paste the token".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
            TodoEntry {
                content: "mine".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ],
    });
    a.key(Key::CtrlT);

    match a.command("todo postpone operator: paste the token") {
        Some(Action::SetOperatorTodos { items, moved }) => {
            assert_eq!(
                items.len(),
                1,
                "the operator's half is unchanged: {items:?}"
            );
            assert_eq!(items[0].content, "mine");
            assert_eq!(
                moved,
                vec![TodoState {
                    content: "operator: paste the token".into(),
                    status: TodoStatus::Postponed,
                }],
                "the move goes out beside the half, named by the row's own words"
            );
        }
        other => panic!("expected a state move, got {other:?}"),
    }
    // **And it is on the pane at once**, the same trust the operator's own rows get: the write
    // publishes when it lands, and until then the head draws what it sent.
    assert_eq!(
        a.todos
            .iter()
            .find(|t| t.content == "operator: paste the token")
            .map(|t| t.status),
        Some(TodoStatus::Postponed),
        "the model's row is set aside in the head's own view: {:?}",
        a.todos
    );
    assert!(
        a.notice.as_deref().unwrap_or("").contains("set aside"),
        "the act is said out loud: {:?}",
        a.notice
    );

    // **And the lift**, by the same words — the state is reversible, which is what makes it a
    // silence rather than a way to drop a row.
    match a.command("todo resume operator: paste the token") {
        Some(Action::SetOperatorTodos { moved, .. }) => {
            assert_eq!(moved.len(), 1);
            assert_eq!(moved[0].status, TodoStatus::Pending);
        }
        other => panic!("expected a state move, got {other:?}"),
    }

    // **A NUMBER still means the operator's own row**, even though the model's row is first in
    // the union — the pane prints numbers for that half and for nothing else.
    match a.command("todo postpone 1") {
        Some(Action::SetOperatorTodos { items, moved }) => {
            assert_eq!(items[0].content, "mine", "row 1 of YOUR rows: {items:?}");
            assert_eq!(items[0].status, TodoStatus::Postponed);
            assert!(moved.is_empty(), "a number never reaches the model's half");
        }
        other => panic!("expected a write of the operator's half, got {other:?}"),
    }

    // **A name that resolves to nothing is REFUSED**, and it says what it could have named — a
    // guess here would set aside a row nobody meant.
    assert_eq!(a.command("todo postpone nobody says this"), None);
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("no row this head knows says"),
        "the refusal names what it looked for: {:?}",
        a.notice
    );

    // **A parent's row is refused by name**, because a parent owns its own rows' states and a
    // silence written here would be taken back by the parent's next write.
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![TodoEntry {
            content: "told by the parent".into(),
            status: TodoStatus::Pending,
            by: TodoBy::parent_of("s-1789462738453908838"),
            when: None,
            needs: Vec::new(),
        }],
    });
    assert_eq!(a.command("todo postpone told by the parent"), None);
    assert!(
        a.notice.as_deref().unwrap_or("").contains("Parent s-"),
        "the refusal says whose row it is: {:?}",
        a.notice
    );
}

/// A cancelled row stays on the board, marked, and is not open work: the row is drawn and kept, since everything
/// created stays in history, and the counts do not see it.
#[test]
fn a_cancelled_row_stays_on_the_board_and_is_not_open_work() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
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
                content: "push leticl to github".into(),
                status: TodoStatus::Cancelled,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ],
    });
    a.key(Key::CtrlT);
    let screen = a.screen(120, 40).join("\n");
    assert!(
        screen.contains("push leticl to github"),
        "**the row is still on the board**: {screen}"
    );
    assert!(
        screen.contains("push leticl to github (cancelled)  — you"),
        "and it still wears its author: {screen}"
    );
    assert_eq!(
        todo_counts(&a.todos),
        (1, 0),
        "one open row (the model's), and the cancelled one is neither open nor postponed"
    );
    assert!(
        screen.contains("1 open") && !screen.contains("postponed"),
        "the header counts only what is owed: {screen}"
    );

    // **And `resume` is the way back**, which is the other half of *kept in history*: the row was
    // never lost, so it can be picked up again.
    match a.command("todo resume 1") {
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items[0].status, TodoStatus::Pending);
        }
        other => panic!("expected a write of the operator's half, got {other:?}"),
    }
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
        Some(Action::SetOperatorTodos { items, .. }) => {
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
    a.push_surface(Surface::Todos);
    let screen = a.screen(110, 40).join("\n");
    // **Once each.** The duplicate is the defect, so a count is the assertion.
    for content in ["seat the tool", "push leticl to github"] {
        assert_eq!(
            screen.matches(content).count(),
            1,
            "`{content}` is drawn more than once: {screen}"
        );
    }
    // Each row wears its own author as a tag: a heading was the earlier reading, and a pane that re-derives
    // authorship from which section a row landed in is the drift the `by` field exists to prevent.
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

/// An empty half says which half is empty: a plan the person took over has no model rows, and one the model wrote
/// has none of theirs, so `none written yet` over either is a different fact.
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
    a.push_surface(Surface::Todos);
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

/// The items are the queue and they were never drawn: the pane rendered one line per heading with counts beside it
/// and stopped.
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

/// A heading with no checkboxes under it is not done: it is a section nobody has filled in, and claiming it as
/// finished work is claiming prose as done.
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

/// Both halves of the pane speak one vocabulary: the items were not painted at all while the jobs pane two keys
/// away had been painting the same three states all along.
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
    // **Both halves are read UNFOLDED**: the subject is that a finished row is painted GREEN in
    // the session's list and in the file's, and the folded default draws neither of them.
    a.todos_fold_done = false;
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

/// A block of calls with a slow one in the middle: the fast ones go green as they finish, not when the block does.
/// The pane used to retire when the assistant row's body landed, which is before the calls run.
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

/// A person's todo is on the pane from the moment it is sent and not from the step boundary: the write was never
/// lost, but the command reaches the worker behind a running turn, so the pane drew nothing for the whole of it.
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
        Some(Action::SetOperatorTodos { .. })
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

/// A condition is attached to a row by a verb, by number, which is what makes a conditioned row something a person
/// can file. The third claim is what keeps a form a form: the row keeps the handle the reader typed.
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
        Some(Action::SetOperatorTodos { items, .. }) => assert_eq!(
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
        Some(Action::SetOperatorTodos { items, .. }) => {
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

/// A row filed from the card can carry a condition, which is why the third field exists: the handle the reader
/// typed is on the row the card files, and the fields are not read as a command line.
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
        Some(Action::SetOperatorTodos { items, .. }) => {
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
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items[0].when, None, "{items:?}")
        }
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
        Some(Action::SetOperatorTodos { items, .. }) => {
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
    assert!(
        a.queued.iter().any(|q| matches!(q, Action::ListTodos)),
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
    let Some(Action::SetOperatorTodos { items: sent, .. }) = a
        .queued
        .iter()
        .find(|q| matches!(q, Action::SetOperatorTodos { .. }))
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
        .filter(|q| matches!(q, Action::SetOperatorTodos { .. }))
        .count();
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: Vec::new(),
    });
    assert_eq!(
        a.queued
            .iter()
            .filter(|q| matches!(q, Action::SetOperatorTodos { .. }))
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

/// A starter-todo seed writes the retired set back and does not write it away: `head.toml` is one file for every
/// head and it is where a dismissal is remembered, so a save carrying only this head's list would drop the rest.
#[test]
fn a_seed_does_not_write_another_heads_dismissals_away() {
    let dir = std::env::temp_dir().join(format!("letibot-seed-retired-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("todo-template.md"),
        "## Phase 0\n\n- [ ] only row\n",
    )
    .unwrap();
    let path = dir.join("head.toml");
    let key = "w|model_endpoint_retry|3000|deadbeefdeadbeef".to_string();

    // **Two heads up at once, which is the arrangement the shared file exists for** (measured
    // 2026-09-22: two `letibot-tui` processes against one `head.toml`). The second one is already
    // running — it has read the file, and it dismissed nothing — when the first retires a notice.
    let mut b = app();
    b.prefs_path = Some(path.clone());
    b.load_prefs();
    assert!(
        b.dismissed.is_empty(),
        "the premise: it has retired nothing"
    );

    // One head retired a notice and saved it. The file is the record.
    let mut a = app();
    a.prefs_path = Some(path.clone());
    a.load_prefs();
    a.retire(vec![key.clone()]);
    a.save_prefs(RetiredWrite::Union);
    assert!(
        crate::prefs::load(&path).0.retired.contains(&key),
        "the premise: the dismissal is on disk"
    );

    // And the second head now attaches to a project it has never seeded — a write nobody asked
    // for, carrying its own (empty) list into a file it shares.
    b.todo_template = crate::prefs::TodoTemplate::Default;
    b.apply(ServerFrame::Hello {
        protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
        session_id: "s2".into(),
        head_id: "h2".into(),
        dropped: 0,
        snapshot: Some(Box::new(Hub::new("s2").snapshot())),
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
    // The seed waits for the board: the head keeps no second list and must not send a half of one
    // it has not read (`todo_seed_pending`).
    b.apply(ServerFrame::Todos {
        session_id: "s2".into(),
        todos: Vec::new(),
    });
    assert!(
        b.todo_seed
            .contains(&todo_seed_key("/home/dead/Projects/x")),
        "the premise: the seed ran"
    );
    assert!(
        crate::prefs::load(&path).0.retired.contains(&key),
        "the seed wrote the other head's dismissals out of the file: {:?}",
        crate::prefs::load(&path).0.retired
    );
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
    // The board read is the attach's own and not the seed's: every attach asks for the board now, so what the
    // switch governs is whether the template's rows are filed, and nothing was sent to the person's half.
    assert!(
        !a.queued
            .iter()
            .any(|q| matches!(q, Action::SetOperatorTodos { .. })),
        "off is off: no row is filed for a seed that is not due"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The title cap, at the person's own doors: `/todo TEXT` and the card, which are the two ways they write a row. A
/// title longer than the pane's row is cut at the edge and read by nobody, so it is refused by name instead.
#[test]
fn a_title_over_the_cap_is_refused_at_the_operators_own_doors() {
    use letibot_sessionlog::event::TODO_TITLE_MAX;
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));

    // **The typed door.** `/todo <a long title>` says why and sends nothing at all.
    let long = "x".repeat(TODO_TITLE_MAX + 1);
    assert_eq!(a.command(&format!("todo {long}")), None, "nothing was sent");
    let said = a.notice.clone().unwrap_or_default();
    assert!(
        said.contains(&format!("at most {TODO_TITLE_MAX}")),
        "the refusal names the cap: {said}"
    );
    assert!(
        said.contains("put it after ` — `"),
        "and where the rest of the row goes: {said}"
    );
    assert!(a.operator_todos().is_empty(), "no row was added: {said}");
    assert!(
        !a.queued
            .iter()
            .any(|q| matches!(q, Action::SetOperatorTodos { .. })),
        "and no half was queued: {:?}",
        a.queued
    );

    // **A title AT the cap passes** — the bound is a bound and not a shave.
    let exact = "y".repeat(TODO_TITLE_MAX);
    match a.command(&format!("todo {exact}")) {
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].content, exact);
        }
        other => panic!("expected the add, got {other:?}"),
    }

    // **The card**, which measures the COMPOSED row — so a long title is refused with the card still
    // up, and what was typed is not lost.
    a.command("todo");
    typed(&mut a, &long);
    assert!(a.key(Key::Enter).is_none(), "nothing was added");
    assert!(a.todo_draft.is_some(), "the card stayed up");
    let said = a.notice.clone().unwrap_or_default();
    assert!(
        said.contains(&format!("at most {TODO_TITLE_MAX}")),
        "the card's refusal names the cap too: {said}"
    );
    assert_eq!(
        a.operator_todos().len(),
        1,
        "still the one row that fitted: {:?}",
        a.operator_todos()
    );
}

/// A row written before the cap is still theirs to move: `SetOperatorTodos` replaces the half wholesale, so a cap
/// checked there would refuse `/todo done 2` because some old row is long. The check is at creation.
#[test]
fn an_operator_row_written_before_the_cap_is_still_movable() {
    use letibot_sessionlog::event::TODO_TITLE_MAX;
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let legacy = "L".repeat(TODO_TITLE_MAX * 2);
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated {
            todos: vec![TodoEntry {
                content: legacy.clone(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            }],
        },
    )));
    match a.command("todo done 1") {
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items.len(), 1, "the old row is sent back: {items:?}");
            assert_eq!(items[0].content, legacy, "with every character of it");
            assert_eq!(items[0].status, TodoStatus::Completed);
        }
        other => panic!("expected the write of the operator's half, got {other:?}"),
    }
}

/// A board row is one line, newline and all: the pane flattens a newline, so a writer that put a second line in
/// `content` has not made a detail, it has made words that run together. A detail has a separator of its own.
#[test]
fn a_board_row_with_a_newline_is_drawn_on_one_line() {
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
                content: "first line\nsecond line".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            }],
        },
    )));
    let lines = a.todos_lines(120);
    assert!(
        lines.iter().any(|l| l.contains("first line second line")),
        "the row is drawn as ONE line, with the newline flattened: {lines:#?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.trim_start().starts_with("second line")),
        "and the second line is not a row of its own: {lines:#?}"
    );
}

/// A template item whose title is over the cap is not seeded, and it is said: from the person's side a row that
/// silently did not arrive is indistinguishable from a file never read. The rest of the template still lands.
#[test]
fn a_template_item_over_the_cap_is_not_seeded_and_is_said() {
    use letibot_sessionlog::event::TODO_TITLE_MAX;
    let dir = std::env::temp_dir().join(format!("letibot-seed-cap-{}", std::process::id()));
    let long = "z".repeat(TODO_TITLE_MAX + 1);
    let mut a = seeded_head(
        &dir,
        &format!("## Phase 0\n\n- [ ] {long}\n- [ ] a row that fits\n"),
        "/home/dead/Projects/cap",
    );
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: Vec::new(),
    });
    assert_eq!(
        a.operator_todos()
            .iter()
            .map(|t| t.content.as_str())
            .collect::<Vec<_>>(),
        vec!["a row that fits"],
        "the long row was not seeded and the rest was"
    );
    let said = a.notice.clone().unwrap_or_default();
    assert!(
        said.contains("1 item of the template not seeded"),
        "the refusal is a note: {said}"
    );
    assert!(
        said.contains(&long),
        "and it names the row rather than the count alone: {said}"
    );
    assert!(
        said.contains("1 starter todo from"),
        "the count of what DID land travels with it, or `say` would have dropped one of the two: \
         {said}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ===== One row, whole: the overlay the pane's Enter opens =====

/// A head with the todos pane open and one row per `(author, content)`, the cursor parked on the
/// FIRST of them — the add control is where the cursor starts, so one Down is what reaches it.
fn todos_head(rows: &[(letibot_sessionlog::event::TodoBy, &str)]) -> App {
    use letibot_sessionlog::event::{TodoEntry, TodoStatus};
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated {
            todos: rows
                .iter()
                .map(|(by, content)| TodoEntry {
                    by: by.clone(),
                    content: (*content).to_string(),
                    status: TodoStatus::Pending,
                    when: None,
                    needs: Vec::new(),
                })
                .collect(),
        },
    )));
    a.key(Key::CtrlT);
    a.key(Key::Down);
    a
}

/// Enter reads the row under the cursor, and Esc goes back to the list with the cursor on it: the pane draws every
/// row to its own width, so a row longer than the pane is cut off with nothing behind it.
#[test]
fn enter_reads_the_row_under_the_cursor_and_esc_goes_back_to_it() {
    use letibot_sessionlog::event::TodoBy;
    let mut a = todos_head(&[
        (TodoBy::Model, "the model's first row"),
        (TodoBy::Operator, "the operator's own row"),
        (TodoBy::Model, "the model's second row"),
    ]);
    assert_eq!(a.key(Key::Enter), None);
    assert_eq!(
        a.todos_open,
        Some(TodoStop::Theirs("the model's first row".into())),
        "enter opens the row the cursor is on"
    );
    let screen = a.screen(100, 30).join("\n");
    assert!(screen.contains("todos — one row"), "{screen}");
    assert!(
        screen.contains("the model · open"),
        "the author and the state are part of reading the row: {screen}"
    );
    assert!(screen.contains("the model's first row"), "{screen}");
    assert!(
        !screen.contains("the model's second row") && !screen.contains("the operator's own row"),
        "the overlay is ONE row and not the list: {screen}"
    );

    // Esc goes back to the LIST — the pane is still there — and the cursor is where it was.
    a.key(Key::Esc);
    assert!(a.todos_open.is_none(), "esc leaves the row");
    assert!(
        a.is_open(Surface::Todos),
        "esc goes back to the list, not out of everything"
    );
    let screen = a.screen(100, 30).join("\n");
    assert!(
        !screen.contains("todos — one row"),
        "the overlay is gone: {screen}"
    );
    assert!(
        screen
            .lines()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains("the model's first row")),
        "the cursor is on the row it was on: {screen}"
    );
    // And a second Esc closes the pane, which is the pair the queue entry's overlay keeps.
    a.key(Key::Esc);
    assert!(!a.is_open(Surface::Todos), "the second esc closes the pane");
}

/// Enter reads one of the person's own rows and Tab toggles it: the toggle is what the typed door sends, and a read
/// that took Enter without leaving the toggle somewhere would have made this pane's own act unreachable.
#[test]
fn tab_toggles_one_of_the_operators_rows_and_enter_reads_it() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = todos_head(&[(TodoBy::Operator, "the operator's own row")]);
    // The board is read unfolded, since the second Tab has to find the row the first one moved: under the folded
    // default the row leaves the drawn list, the cursor falls back to the add control, and the toggle is one-way.
    a.todos_fold_done = false;
    // The pane's own open (`ListTodos`) is drained first, so what is left is what the keys sent.
    let _ = a.take_actions();
    // Enter READS it — the same overlay, and nothing is sent by the read.
    assert_eq!(a.key(Key::Enter), None);
    assert_eq!(
        a.todos_open,
        Some(TodoStop::Mine("the operator's own row".into()))
    );
    assert!(a.take_actions().is_empty(), "reading a row sends nothing");
    a.key(Key::Esc);

    // Tab toggles it, and again puts it back — a cursor that can only go one way is one you cannot
    // correct.
    match a.key(Key::Tab) {
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items[0].status, TodoStatus::Completed, "{items:?}");
        }
        other => panic!("expected the toggle, got {other:?}"),
    }
    match a.key(Key::Tab) {
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items[0].status, TodoStatus::Pending, "{items:?}");
        }
        other => panic!("expected the row to be reopened, got {other:?}"),
    }
    assert!(
        a.todos_open.is_none(),
        "and Tab does not open the overlay as a side effect"
    );
}

/// The cursor walks every row of the board, the model's and the person's, and the mark is on the row the keys are
/// about: rano records a stop only for the rows it treats as stops, so the head completes the record, and a wrong
/// completion shifts every stop after it.
#[test]
fn the_cursor_walks_every_row_and_the_mark_follows_it() {
    use letibot_sessionlog::event::TodoBy;
    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-walk-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::write(
        dir.join("TODO.md"),
        "## Phase 0\n\n- [ ] the file's own item\n",
    )
    .expect("write");
    let mut a = todos_head(&[
        (TodoBy::Model, "the model's first row"),
        (TodoBy::Operator, "the operator's own row"),
        (TodoBy::Model, "the model's second row"),
    ]);
    a.wiring.workspace = dir.display().to_string();
    a.refresh_repo_todos();

    // Down from the first row to the last, one press at a time: the mark is on the row the key
    // moved to, every time.
    for want in [
        "the model's first row",
        "the operator's own row",
        "the model's second row",
        "the file's own item",
    ] {
        let screen = a.screen(110, 40);
        let marked = screen
            .iter()
            .find(|l| l.contains('▸'))
            .unwrap_or_else(|| panic!("nothing is marked in:\n{}", screen.join("\n")));
        assert!(
            marked.contains(want),
            "the mark is on the wrong row — wanted {want:?}, drawn {marked:?}"
        );
        a.key(Key::Down);
    }
    // And the wrap goes back to the control, which is the head of the one enumeration.
    let screen = a.screen(110, 40);
    assert!(
        screen
            .iter()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains("[+] add todo item")),
        "the walk wraps to the control: {}",
        screen.join("\n")
    );

    // A click is the record's other reader and the one a wrong completion breaks: a click on a row's own screen row
    // must land the cursor on that row, and clicking the person's row is the case a record missing the model's rows
    // gets wrong by one.
    let screen = a.screen(110, 40);
    let y = screen
        .iter()
        .position(|l| l.contains("the operator's own row"))
        .expect("the row is drawn");
    a.key(Key::Click {
        x: 6,
        y: u16::try_from(y).expect("fits"),
    });
    assert_eq!(a.todos_sel, 2, "the click landed on the row it was on");
    let screen = a.screen(110, 40);
    assert!(
        screen
            .iter()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains("the operator's own row")),
        "and the mark is there: {}",
        screen.join("\n")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A row that carries newlines is shown WHOLE** — the shape a model writes, and the one the pane
/// cannot draw: rano's row is `clean_line(content)`, which turns the newline into a space, so the
/// row's own lines are not rows on the pane at all.
#[test]
fn a_row_that_carries_newlines_is_shown_whole() {
    use letibot_sessionlog::event::TodoBy;
    let content = format!(
        "{}the second line, which the pane cannot show in full",
        "first line of the row ".repeat(4)
    );
    let mut a = todos_head(&[(TodoBy::Model, &content)]);
    // The pane's own row: one line, trimmed to the pane's width, so the tail is not on it.
    let pane = a.screen(60, 30).join("\n");
    assert!(
        !pane.contains("the pane cannot show in full"),
        "the pane is the reason the overlay exists: {pane}"
    );

    a.key(Key::Enter);
    let screen = a.screen(60, 30).join("\n");
    assert!(screen.contains("first line of the row"), "{screen}");
    assert!(
        screen.contains("the pane cannot show in full"),
        "the row's second line is on the overlay: {screen}"
    );
    // **The newline is a break and not a space**: the row's second line begins a row of its own,
    // which is the shape the author wrote — rano's own row is `clean_line(content)`, where that
    // newline is a space and the two lines are one run of text.
    assert!(
        screen
            .lines()
            .any(|l| l.trim() == "which the pane cannot show in full"),
        "the row's own line begins its own row: {screen}"
    );
}

/// A row longer than the screen pages rather than being cut to the window: the row can be longer than any
/// terminal, so a bigger box alone would not answer it. The page keys move the overlay and the transcript stays put.
#[test]
fn a_row_longer_than_the_screen_pages_rather_than_truncating() {
    use letibot_sessionlog::event::TodoBy;
    let long = (0..120)
        .map(|i| format!("word{i:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut a = todos_head(&[(TodoBy::Model, &long)]);
    a.key(Key::Enter);
    let first = a.screen(100, 16).join("\n");
    assert!(first.contains("word000"), "{first}");
    assert!(
        !first.contains("word119"),
        "the tail is below the fold, which is the whole point: {first}"
    );

    for _ in 0..40 {
        a.key(Key::PageDown);
    }
    let last = a.screen(100, 16).join("\n");
    assert!(
        last.contains("word119"),
        "the rest of the row is reachable by paging: {last}"
    );
    assert!(
        a.following(),
        "and the page keys moved the overlay, not the transcript underneath it"
    );
}

/// The overlay draws the board as it stands now, and a row that is gone says so: the row is looked up by identity
/// at draw time and never carried, and the board's identity is its words. A `TodosUpdated` can move a state, which
/// draws the new one, or take the row away.
#[test]
fn a_row_that_moves_under_the_overlay_is_drawn_as_it_stands_now() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
    let mut a = todos_head(&[(TodoBy::Model, "the row as it was written")]);
    a.key(Key::Enter);
    let screen = a.screen(100, 30).join("\n");
    assert!(screen.contains("the model · open"), "{screen}");

    // The model marks its own row done. The overlay shows the new state, not the one it had when
    // Enter was pressed.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TodosUpdated {
            todos: vec![TodoEntry {
                by: TodoBy::Model,
                content: "the row as it was written".into(),
                status: TodoStatus::Completed,
                when: None,
                needs: Vec::new(),
            }],
        },
    )));
    let screen = a.screen(100, 30).join("\n");
    assert!(
        screen.contains("the model · done"),
        "the state is read at draw time: {screen}"
    );

    // And a row renamed is a DIFFERENT row — the board has no ids — so the overlay says so rather
    // than showing words that are not on the board any more.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TodosUpdated {
            todos: vec![TodoEntry {
                by: TodoBy::Model,
                content: "the row, rewritten".into(),
                status: TodoStatus::Pending,
                when: None,
                needs: Vec::new(),
            }],
        },
    )));
    let screen = a.screen(100, 30).join("\n");
    assert!(
        screen.contains("is not on the board any more"),
        "a renamed row is a different row: {screen}"
    );
    // **And the row that took its place is not drawn as though it were it.** The words appear once,
    // quoted in the sentence that says they are gone; what must NOT appear is the new row's text or
    // a facts line, which is what a lookup that had fallen back to the index would have drawn.
    assert!(
        !screen.contains("the row, rewritten") && !screen.contains("the model ·"),
        "the overlay does not draw whatever took the row's place: {screen}"
    );
}

/// **A parent's row says who wrote it, in the words it arrived with** — the fourth author on the
/// wire, and the one a reader cannot guess: `Parent <session-id>` is the string the operator
/// specified, and the overlay shows it rather than re-spelling it as *the model's*.
#[test]
fn a_parents_row_names_its_author() {
    use letibot_sessionlog::event::TodoBy;
    let mut a = todos_head(&[(
        TodoBy::parent_of("s-1789462738453908838"),
        "audit the store",
    )]);
    a.key(Key::Enter);
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("Parent s-1789462738453908838 · open"),
        "{screen}"
    );
}

/// **A row's `needs` are part of reading it** — the edges that say what has to land first, which
/// the pane's one-line row cannot carry at all.
#[test]
fn the_overlay_shows_what_the_row_needs() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoNeed, TodoStatus};
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
                by: TodoBy::Model,
                content: "land the port".into(),
                status: TodoStatus::Pending,
                when: None,
                needs: vec![
                    TodoNeed::Row {
                        content: "seat the tool".into(),
                    },
                    TodoNeed::Job {
                        handle: "j89".into(),
                    },
                ],
            }],
        },
    )));
    a.key(Key::CtrlT);
    a.key(Key::Down);
    a.key(Key::Enter);
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("needs row `seat the tool`, job j89"),
        "{screen}"
    );
}

/// Keys typed in the overlay do not reach the composer: nothing on it is a field, so a key it does not name has no
/// meaning there, and a draft growing under a row somebody is reading is a message they did not write.
#[test]
fn keys_typed_in_the_overlay_do_not_reach_the_composer() {
    use letibot_sessionlog::event::TodoBy;
    let mut a = todos_head(&[(TodoBy::Model, "a row")]);
    a.key(Key::Enter);
    assert!(a.todos_open.is_some());

    a.key(Key::Char('x'));
    assert_eq!(
        a.editor.text(),
        "",
        "a typed key is not the composer's while a row is open"
    );
    assert!(
        a.todos_open.is_some(),
        "and it did not close the row either"
    );

    // Up is the overlay's — it scrolls — and NOT the queued line's recall.
    a.pending_prompts = vec!["a queued line".into()];
    a.key(Key::Up);
    assert_eq!(a.editor.text(), "", "the queued line was not recalled");
    assert_eq!(a.pending_prompts, vec!["a queued line".to_string()]);

    // And Enter in the overlay does not send anything: there is nothing to send, and the row stays
    // open — a second Enter is not a way out of a screen whose way out is named on it.
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.todos_open.is_some());
    assert_eq!(a.editor.text(), "");
}

/// A wait `wait_on` armed shows on the row in the list and not only in its card: the list's slot read only `when`,
/// so a row the model had parked on a job looked exactly like a row nobody had touched.
#[test]
fn an_armed_wait_shows_on_the_row() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoNeed, TodoStatus};
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![
            TodoEntry {
                content: "make tier 1 green".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: vec![TodoNeed::Job {
                    handle: "j55".into(),
                }],
            },
            TodoEntry {
                content: "the criterion row".into(),
                status: TodoStatus::InProgress,
                by: TodoBy::Model,
                when: None,
                needs: vec![TodoNeed::Child {
                    id: "s-1-sub-9".into(),
                }],
            },
            TodoEntry {
                content: "nothing armed".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: vec![TodoNeed::Row {
                    content: "make tier 1 green".into(),
                }],
            },
        ],
    });
    a.key(Key::CtrlT);
    let lines = a.todos_lines(160);
    let row = |words: &str| {
        lines
            .iter()
            .find(|l| l.contains(words))
            .cloned()
            .unwrap_or_default()
    };
    assert!(
        row("make tier 1 green").contains("waits on j55"),
        "{lines:#?}"
    );
    assert!(
        row("the criterion row").contains("waits on s-1-sub-9"),
        "{lines:#?}"
    );
    assert!(
        !row("nothing armed").contains("waits on"),
        "an edge on a row is not a wait on a job: {lines:#?}"
    );
}

// ─────────────────────── a row that waits on the OPERATOR ───────────────────────

/// The board a session waiting on the operator shows: one row asking, one ready row.
fn waiting_board(a: &mut App) {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoNeed, TodoStatus};
    a.session_id = "s1".into();
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![
            TodoEntry {
                content: "deploy the staging box".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: vec![TodoNeed::Operator {
                    question: "which database should staging use?".into(),
                    options: vec!["postgres".into(), "sqlite".into()],
                }],
            },
            TodoEntry {
                content: "write the release notes".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
        ],
    });
}

/// The resident line says the person is waited on and goes when nobody is: drawn from the board, quoting the
/// question and the row and naming alt-a, and from the `asks` settings row for a head that has not read the board.
#[test]
fn a_board_waiting_on_the_operator_draws_a_resident_line() {
    let mut a = app();
    assert!(a.waiting_line(160).is_empty(), "nobody is waited on yet");
    a.session_id = "s1".into();
    a.apply(ServerFrame::Settings {
        rows: vec![letibot_sessionlog::protocol::SettingRow {
            key: "asks".into(),
            value: "1 — \"which database should staging use?\" on `deploy the staging box`".into(),
            source: String::new(),
            editable: String::new(),
            choices: Vec::new(),
            tools: Vec::new(),
        }],
    });
    let line = a.waiting_line(200).join("");
    assert!(line.contains("WAITING ON YOU"), "{line}");
    assert!(
        a.queued.contains(&Action::ListTodos),
        "the board is read so the card can offer the options"
    );
    waiting_board(&mut a);
    let line = a.waiting_line(200).join("");
    assert!(
        line.contains("\"which database should staging use?\" on `deploy the staging box`"),
        "{line}"
    );
    assert!(line.contains("alt-a to answer"), "{line}");
    assert!(
        a.screen(200, 40).join("\n").contains("WAITING ON YOU"),
        "the line is on the screen"
    );
    a.apply(ServerFrame::Settings { rows: Vec::new() });
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: Vec::new(),
    });
    assert!(
        a.waiting_line(200).is_empty(),
        "the line goes when nobody is waited on"
    );
}

/// The todo pane's row says `waits on you`, and Enter on it opens the question card, the one `ask_user_question`
/// draws, with the row's choices. Answering it goes to the row and takes the card down.
#[test]
fn the_pane_marks_the_row_and_enter_opens_its_question_card() {
    use letibot_sessionlog::question::QuestionAnswer;
    let mut a = app();
    waiting_board(&mut a);
    a.key(Key::CtrlT);
    let lines = a.todos_lines(160);
    let row = lines
        .iter()
        .find(|l| l.contains("deploy the staging box"))
        .cloned()
        .unwrap_or_default();
    assert!(row.contains("waits on you"), "{lines:#?}");

    // The cursor starts on the add control; one Down is the waiting row.
    a.key(Key::Down);
    assert_eq!(
        a.key(Key::Enter),
        None,
        "the card opens; nothing is sent yet"
    );
    let card = a.open.first().expect("the question card is up").clone();
    assert_eq!(card.kind, "question");
    assert_eq!(card.summary, "which database should staging use?");
    assert_eq!(
        card.choices,
        vec!["postgres".to_string(), "sqlite".to_string()]
    );

    let expect = |answer: QuestionAnswer| Action::AnswerTodo {
        content: "deploy the staging box".into(),
        question: "which database should staging use?".into(),
        answer,
    };
    // An option, by Enter on the marked row.
    assert_eq!(a.key(Key::Enter), Some(expect(QuestionAnswer::choosing(0))));
    assert!(a.open.is_empty(), "the card comes down once answered");

    // alt-a opens it again, from anywhere; the other three shapes are typed.
    for (typed_line, answer) in [
        (
            "sqlite -- only for tests",
            QuestionAnswer::choosing(1).with_note("only for tests"),
        ),
        (
            "the one already running",
            QuestionAnswer::free("the one already running"),
        ),
        ("abstain", QuestionAnswer::abstaining()),
    ] {
        // The board says the row still waits (as it would after a refused answer): an answer
        // from this head leaves its row out until the board comes back.
        waiting_board(&mut a);
        a.key(Key::AltA);
        assert!(!a.open.is_empty(), "alt-a opens the card");
        typed(&mut a, typed_line);
        assert_eq!(a.key(Key::Enter), Some(expect(answer)), "{typed_line}");
        assert!(a.open.is_empty());
    }
}

/// **The picker marks a session that is waiting on you** — from its brief, so a person can see it
/// from the list without attaching to every session.
#[test]
fn the_picker_marks_a_session_waiting_on_the_operator() {
    let mut asking = brief("s-asking", "the asking one", false);
    asking.waits_on_operator = 2;
    let other = brief("s-quiet", "the quiet one", false);
    let mut a = app();
    a.apply(hello(
        "s-quiet",
        vec![other, asking],
        Hub::new("s-quiet").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    let lines = a.picker_lines(120).join("\n");
    assert!(
        lines.contains("the asking one · WAITING ON YOU (2)"),
        "{lines}"
    );
    assert!(!lines.contains("the quiet one · WAITING"), "{lines}");
}

/// The model taking its wait on the person back takes the card down: a `detach` removes the question from the row,
/// and the board that comes back no longer asks it, so the card and the resident line go with it.
#[test]
fn a_question_the_model_takes_back_takes_its_card_and_its_line_down() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
    let mut a = app();
    waiting_board(&mut a);
    a.open_operator_ask(None);
    assert!(
        a.open
            .iter()
            .any(|d| d.req_id.starts_with(crate::app::todos::TODO_ASK)),
        "the premise: the card is open"
    );
    // The model detaches: the same rows, the question gone.
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: vec![
            TodoEntry {
                content: "deploy the staging box".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
            TodoEntry {
                content: "write the release notes".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
        ],
    });
    assert!(
        !a.open
            .iter()
            .any(|d| d.req_id.starts_with(crate::app::todos::TODO_ASK)),
        "the card went with the question"
    );
    assert!(
        a.waiting_line(200).is_empty(),
        "and so did the resident line"
    );

    // Put away with Esc first, then taken back: gone from the away list too.
    waiting_board(&mut a);
    a.open_operator_ask(None);
    a.key(Key::Esc);
    a.apply(ServerFrame::Todos {
        session_id: "s1".into(),
        todos: Vec::new(),
    });
    assert!(
        a.away.iter().all(|w| !w.is_todo()),
        "the put-away card went too"
    );
}
// ───────────────────── the done rows, in a group of their own ─────────────────────

/// **A head on a board of the statuses given** — `todos_head`'s sibling, for the tests about rows
/// that are DONE, which `todos_head` never makes (it is the walk's, and the walk is over open work).
fn status_head(
    rows: &[(
        &str,
        letibot_sessionlog::event::TodoBy,
        letibot_sessionlog::event::TodoStatus,
    )],
) -> App {
    use letibot_sessionlog::event::TodoEntry;
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated {
            todos: rows
                .iter()
                .map(|(content, by, status)| TodoEntry {
                    by: by.clone(),
                    content: (*content).to_string(),
                    status: *status,
                    when: None,
                    needs: Vec::new(),
                })
                .collect(),
        },
    )));
    a.key(Key::CtrlT);
    a
}

/// The screen row a piece of a row's words was drawn on, in the frame the caller just drew: by words and never by
/// arithmetic, since a test that computed where a row must be would assert its own arithmetic back at itself.
fn at(screen: &[String], needle: &str) -> usize {
    screen
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("`{needle}` is not on the screen:\n{}", screen.join("\n")))
}

/// The done rows are gathered under a line of their own, below the rows still owed: the order is the board's, so
/// both halves keep their relative order, which is all *newest last* can be here since the wire carries no
/// completion time.
#[test]
fn the_done_rows_are_gathered_under_a_line_of_their_own_below_the_rows_owed() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = status_head(&[
        ("done first", TodoBy::Model, TodoStatus::Completed),
        ("still owed", TodoBy::Model, TodoStatus::Pending),
        ("done second", TodoBy::Operator, TodoStatus::Completed),
        ("set aside", TodoBy::Operator, TodoStatus::Postponed),
        ("in progress", TodoBy::Model, TodoStatus::InProgress),
        (
            "done third",
            TodoBy::Parent("Parent s-sub-1".into()),
            TodoStatus::Completed,
        ),
    ]);
    // **The group is read OPEN**, which is what this test is about: the folded default draws the
    // line where the rows are, and there would be nothing to compare the order of.
    a.todos_fold_done = false;
    let screen = a.screen(110, 40);
    let group = at(&screen, "done (3)");
    let owed = ["still owed", "set aside", "in progress"].map(|r| at(&screen, r));
    let done = ["done first", "done second", "done third"].map(|r| at(&screen, r));

    assert!(
        owed.iter().all(|r| *r < group),
        "a row still owed is drawn under the group line: {}",
        screen.join("\n")
    );
    // **The rows owed keep the board's own order between themselves** — the grouping is a split
    // into two halves, not a sort.
    assert!(
        owed[0] < owed[1] && owed[1] < owed[2],
        "the rows owed were reordered: {owed:?}\n{}",
        screen.join("\n")
    );
    assert!(
        done.iter().all(|r| *r > group),
        "a done row is drawn above the group line: {}",
        screen.join("\n")
    );
    assert!(
        done[0] < done[1] && done[1] < done[2],
        "the done rows were reordered among themselves: {done:?}\n{}",
        screen.join("\n")
    );
    // **The line names the count and nothing else while the group is open** — the key that folds
    // it is the bar's to name (`d folds done`), and the folded line is where the reminder belongs.
    let line = screen[group].trim();
    assert_eq!(line, "done (3)", "the group line reads: {line:?}");
}

/// A row's number is its place on the whole board, so a row moving into the done group keeps it: the verbs take
/// that number, and a counter that walked the drawn order would renumber every row the moment a done one moved.
#[test]
fn the_numbers_are_the_boards_and_a_row_moving_into_the_group_keeps_its_own() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = status_head(&[
        ("the first", TodoBy::Operator, TodoStatus::Pending),
        ("the second, done", TodoBy::Operator, TodoStatus::Completed),
        ("the third", TodoBy::Operator, TodoStatus::Pending),
    ]);
    // **The board is read UNFOLDED**, because a number is a property of a DRAWN row: the folded
    // default draws `done (1)` where the second row's number used to stand.
    a.todos_fold_done = false;
    let screen = a.screen(110, 40);
    assert!(
        screen.iter().any(|l| l.contains("1  [ ] the first")),
        "the first row lost its number: {}",
        screen.join("\n")
    );
    assert!(
        screen.iter().any(|l| l.contains("3  [ ] the third")),
        "the third row's number followed it into the drawn order: {}",
        screen.join("\n")
    );
    let done = at(&screen, "2  [x] the second, done");
    assert!(
        done > at(&screen, "done (1)"),
        "the done row is in the group and still says 2: {}",
        screen.join("\n")
    );

    // **And the verb takes that number.** Row 3 is drawn SECOND of the operator's rows — it is the
    // third row of the BOARD — and `/todo done 3` must move the third and not the one above it.
    match a.command("todo done 3") {
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items[2].content, "the third", "{items:?}");
            assert_eq!(items[2].status, TodoStatus::Completed, "{items:?}");
            assert_eq!(
                items[1].content, "the second, done",
                "the verb moved another row: {items:?}"
            );
        }
        other => panic!("expected the write of the operator's half, got {other:?}"),
    }
    let screen = a.screen(110, 40);
    assert!(
        screen.iter().any(|l| l.contains("3  [x] the third")),
        "the row that moved into the group kept its number: {}",
        screen.join("\n")
    );
}

/// Tab on one of the person's rows keeps the cursor on that row when the toggle moves it into the done group:
/// marking it done changes its state and the grouping draws it elsewhere, so the index is a place and not a row
/// from that frame on. The key carries it by identity.
#[test]
fn toggling_a_row_into_the_group_takes_the_cursor_with_it() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = status_head(&[
        ("the first", TodoBy::Operator, TodoStatus::Pending),
        (
            "the one being marked",
            TodoBy::Operator,
            TodoStatus::Pending,
        ),
        ("the last", TodoBy::Operator, TodoStatus::Pending),
    ]);
    // **The group is read OPEN**: the row this test marks done has to stay DRAWN for the cursor to
    // be found on it, and under the folded default it would leave the list the moment it is marked.
    a.todos_fold_done = false;
    a.screen(110, 40);
    a.key(Key::Down);
    a.key(Key::Down);
    assert!(
        matches!(a.key(Key::Tab), Some(Action::SetOperatorTodos { .. })),
        "the toggle is the operator's row's own act"
    );
    let screen = a.screen(110, 40);
    let marked = screen
        .iter()
        .find(|l| l.contains('▸'))
        .unwrap_or_else(|| panic!("no mark at all: {}", screen.join("\n")));
    assert!(
        marked.contains("the one being marked"),
        "the cursor stayed on an index while the row moved into the group: {}",
        screen.join("\n")
    );
    assert!(
        at(&screen, "the one being marked") > at(&screen, "done (1)"),
        "the row was not marked done, so this proves nothing: {}",
        screen.join("\n")
    );
}

/// `d` opens the folded group and `d` again folds it to the one line that names it, which is where a head nobody
/// has asked starts. The folded line says how many rows it holds and which key shows them.
#[test]
fn d_opens_the_folded_group_and_d_again_folds_it() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = status_head(&[
        ("the model's done row", TodoBy::Model, TodoStatus::Completed),
        ("the model's open row", TodoBy::Model, TodoStatus::Pending),
        ("your done row", TodoBy::Operator, TodoStatus::Completed),
        ("your open row", TodoBy::Operator, TodoStatus::InProgress),
        (
            "a subagent's done row",
            TodoBy::Parent("Parent s-sub-1".into()),
            TodoStatus::Completed,
        ),
        (
            "a row you set aside",
            TodoBy::Operator,
            TodoStatus::Postponed,
        ),
    ]);
    assert!(
        a.todos_fold_done,
        "a head nobody has asked starts with the done group folded"
    );
    // The header's own line, so the count is compared with ITSELF rather than with a number this
    // test would have to keep in step with the fixture.
    let head_of = |screen: &str| {
        screen
            .lines()
            .find(|l| l.trim_start().starts_with("todos — "))
            .map(|l| l.trim().to_string())
            .unwrap_or_else(|| panic!("no header: {screen}"))
    };
    let before = a.screen(110, 40).join("\n");
    assert!(
        before.contains("done (3) · d shows them"),
        "the folded line does not name the count and the key: {before}"
    );
    for row in [
        "the model's done row",
        "your done row",
        "a subagent's done row",
    ] {
        assert!(
            !before.contains(row),
            "{row} is drawn with the group folded: {before}"
        );
    }
    for row in [
        "the model's open row",
        "your open row",
        "a row you set aside",
    ] {
        assert!(
            before.contains(row),
            "{row} went with the done ones: {before}"
        );
    }

    // **The first press is the SHOWING half**, because the head opens folded.
    a.key(Key::Char('d'));
    let after = a.screen(110, 40).join("\n");
    for row in [
        "the model's done row",
        "your done row",
        "a subagent's done row",
    ] {
        assert!(after.contains(row), "{row} did not come back: {after}");
    }
    assert!(
        after.contains("done (3)") && !after.contains("d shows them"),
        "the group is open but its line still reads folded: {after}"
    );
    assert_eq!(
        head_of(&before),
        head_of(&after),
        "the header counts what it counted before"
    );
    assert!(
        a.notice.clone().unwrap_or_default().contains("are back"),
        "the key says what it did: {:?}",
        a.notice
    );

    // **And `d` again folds it** — the fold is not a one-way door, and the rows go back under the
    // same line and the same count.
    a.key(Key::Char('d'));
    let back = a.screen(110, 40).join("\n");
    for row in [
        "the model's done row",
        "your done row",
        "a subagent's done row",
    ] {
        assert!(
            !back.contains(row),
            "{row} is still drawn with the group folded: {back}"
        );
    }
    for row in [
        "the model's open row",
        "your open row",
        "a row you set aside",
    ] {
        assert!(back.contains(row), "{row} went with the done ones: {back}");
    }
    assert!(
        back.contains("done (3) · d shows them"),
        "the folded line does not name the count and the key: {back}"
    );
    assert!(
        a.notice
            .clone()
            .unwrap_or_default()
            .contains("3 done folded away"),
        "the second press says what it did: {:?}",
        a.notice
    );
}

/// The cursor keeps its row and not its index when the group folds under it: `todos_sel` indexes the one
/// enumeration, and folding takes entries out, so an index kept across the switch lands on whichever row slid into
/// the hole.
#[test]
fn the_cursor_keeps_the_row_it_was_on_when_the_group_folds() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = status_head(&[
        (
            "done above the cursor",
            TodoBy::Model,
            TodoStatus::Completed,
        ),
        ("done above it too", TodoBy::Operator, TodoStatus::Completed),
        (
            "the row under the cursor",
            TodoBy::Model,
            TodoStatus::Pending,
        ),
        ("the last row", TodoBy::Model, TodoStatus::Pending),
    ]);
    // **The group is read OPEN**: this test walks INTO it and then folds it shut under the cursor,
    // and the folded default would have the first press opening rather than folding.
    a.todos_fold_done = false;
    // The add control, then the FIRST row still owed: onto the row this test is about.
    a.key(Key::Down);
    let screen = a.screen(110, 40);
    assert!(
        screen
            .iter()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains("the row under the cursor")),
        "the cursor is not on its row before the switch: {}",
        screen.join("\n")
    );

    a.key(Key::Char('d'));
    let screen = a.screen(110, 40);
    assert!(
        screen
            .iter()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains("the row under the cursor")),
        "the cursor slid onto another row when the group folded: {}",
        screen.join("\n")
    );

    // **And a cursor INSIDE the group is the other half**: folding takes the row out from under it,
    // and the index it had is then a place rather than a row.
    a.key(Key::Char('d'));
    // The add control, the two owed rows, then the group's two: the second of them is the last stop
    // of the list — three Downs from the row the cursor is on.
    a.key(Key::Down);
    a.key(Key::Down);
    a.key(Key::Down);
    let screen = a.screen(110, 40);
    assert!(
        screen
            .iter()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains("done above it too")),
        "the walk does not reach the done rows: {}",
        screen.join("\n")
    );
    a.key(Key::Char('d'));
    let screen = a.screen(110, 40);
    assert_eq!(
        screen.iter().filter(|l| l.contains('▸')).count(),
        1,
        "one mark, or the fixture repeats itself: {}",
        screen.join("\n")
    );
    assert!(
        screen
            .iter()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains("the last row")),
        "a folded row under the cursor did not fall back to a row that is drawn: {}",
        screen.join("\n")
    );
}

/// The done group's line is not a row and not a stop: nothing is drawn where a cursor can rest that has no act, and
/// `d` is the pane's key, so a stop there would be a second door to a key nobody is told about. The stops follow
/// the DRAWN order.
#[test]
fn the_arrows_and_a_click_walk_the_rows_in_the_order_they_are_drawn() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = status_head(&[
        ("done below", TodoBy::Model, TodoStatus::Completed),
        ("owed first", TodoBy::Model, TodoStatus::Pending),
        ("owed second", TodoBy::Model, TodoStatus::InProgress),
        ("done above", TodoBy::Model, TodoStatus::Completed),
    ]);
    // **The stops are the DRAWN order**, so the group is read OPEN — the folded default draws
    // three stops where this test counts five.
    a.todos_fold_done = false;
    let _ = a.screen(110, 40);
    // The add control, then the two rows owed, then — past the group line — the two done ones.
    assert_eq!(
        a.todos_stops(),
        vec![
            TodoStop::Add,
            TodoStop::Theirs("owed first".into()),
            TodoStop::Theirs("owed second".into()),
            TodoStop::Theirs("done below".into()),
            TodoStop::Theirs("done above".into()),
        ],
        "the stops are not the drawn order"
    );

    // **A click on a done row lands on that row**, one row below the group's line: the row's
    // PANE place is what a click reads, and the grouping has moved it away from its place on the
    // board — which is the whole defect a click could have kept while the drawing was fixed.
    let screen = a.screen(110, 40);
    let row = at(&screen, "done above");
    a.key(Key::Click {
        x: 10,
        y: row as u16,
    });
    let drawn = a.screen(110, 40);
    assert!(
        drawn
            .iter()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains("done above")),
        "a click on a done row landed somewhere else: {}",
        drawn.join("\n")
    );
    // And on the one above it, which is the pair that would swap places under arithmetic over the
    // board's order.
    let row = at(&screen, "done below");
    a.key(Key::Click {
        x: 10,
        y: row as u16,
    });
    let drawn = a.screen(110, 40);
    assert!(
        drawn
            .iter()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains("done below")),
        "a click on the other done row landed somewhere else: {}",
        drawn.join("\n")
    );

    // **And the arrows walk the same order**: four Downs from the control reach the second done
    // row, which is the fourth stop — not the second row of the board.
    let mut a = status_head(&[
        ("done below", TodoBy::Model, TodoStatus::Completed),
        ("owed first", TodoBy::Model, TodoStatus::Pending),
        ("owed second", TodoBy::Model, TodoStatus::InProgress),
        ("done above", TodoBy::Model, TodoStatus::Completed),
    ]);
    // Open here too: four Downs reach the second done row only while the group is drawn.
    a.todos_fold_done = false;
    for _ in 0..4 {
        a.key(Key::Down);
    }
    let screen = a.screen(110, 40);
    assert!(
        screen
            .iter()
            .find(|l| l.contains('▸'))
            .is_some_and(|l| l.contains("done above")),
        "the arrows did not walk into the done group: {}",
        screen.join("\n")
    );
}

/// **A board with nothing done draws no group line at all** — `done (0)` is furniture about
/// nothing, and the ordinary board is the one with work left.
#[test]
fn a_board_with_nothing_done_draws_no_group_line() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = status_head(&[
        ("still owed", TodoBy::Model, TodoStatus::Pending),
        ("set aside", TodoBy::Operator, TodoStatus::Postponed),
    ]);
    let screen = a.screen(110, 40).join("\n");
    assert!(
        !screen.contains("done ("),
        "a group line is drawn over no group: {screen}"
    );
    assert_eq!(
        a.todos_stops().len(),
        3,
        "the stops are the control and the two rows"
    );
}

/// A board whose rows are all done: the group is the whole list, and rano's `none yet` must not stand over it,
/// since that sentence is true of the empty argument and false of the board. The first look is the folded one.
#[test]
fn a_board_of_only_done_rows_draws_the_group_and_does_not_say_none_yet() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = status_head(&[
        (
            "the model's only row, done",
            TodoBy::Model,
            TodoStatus::Completed,
        ),
        (
            "your only row, done",
            TodoBy::Operator,
            TodoStatus::Completed,
        ),
    ]);
    let folded = a.screen(110, 40).join("\n");
    assert!(
        !folded.contains("none yet"),
        "the pane says there is nothing on a board with two rows folded away: {folded}"
    );
    assert!(
        folded.contains("done (2) · d shows them"),
        "the folded line does not stand in its place: {folded}"
    );

    // **And the line opens onto the rows it counts**, under the same words and the same number.
    a.key(Key::Char('d'));
    let screen = a.screen(110, 40).join("\n");
    let line = screen
        .lines()
        .find(|l| l.contains("done (2)"))
        .unwrap_or_else(|| panic!("no group line: {screen}"));
    assert_eq!(line.trim(), "done (2)", "the open group's line: {line:?}");
    assert!(
        screen.contains("the model's only row, done") && screen.contains("your only row, done"),
        "the group is open and holds both rows: {screen}"
    );
}

/// **The repo's `[x]` items are gathered the same way, one section at a time** — a `TODO.md` is
/// sections, and an item belongs to the heading over it, so the grouping stops at the heading and
/// the heading keeps the file's own cookie (`[1/2]`), which counts the file and not this pane.
#[test]
fn the_files_done_items_are_gathered_under_a_line_in_their_own_section() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let dir = std::env::temp_dir().join(format!(
        "letibot-todo-group-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::write(
        dir.join("TODO.md"),
        "## Phase 0 — the plan\n\n- [x] **T1** the finished item\n- [ ] **T2** the open item\n\
         \n## Phase 1 — after it\n\n- [ ] **T3** another open one\n",
    )
    .expect("write");
    let mut a = status_head(&[("a session row", TodoBy::Model, TodoStatus::Pending)]);
    // **The sections are read UNFOLDED**: the subject is where the file's `[x]` item is drawn, and
    // the folded default draws its section's line in its place.
    a.todos_fold_done = false;
    a.wiring.workspace = dir.display().to_string();
    a.refresh_repo_todos();

    let screen = a.screen(110, 40);
    let heading = at(&screen, "Phase 0 — the plan  [1/2]");
    let open = at(&screen, "T2 the open item");
    let group = at(&screen, "done (1)");
    let done = at(&screen, "T1 the finished item");
    assert!(
        heading < open && open < group && group < done,
        "the section is not heading, owed, group, done: {}",
        screen.join("\n")
    );
    // **The next section is untouched** — a section with nothing done gets no line of its own.
    let next = at(&screen, "another open one");
    assert!(next > done, "{}", screen.join("\n"));
    assert_eq!(
        screen.iter().filter(|l| l.contains("done (1)")).count(),
        1,
        "one group line per section that has one: {}",
        screen.join("\n")
    );

    // **And the fold takes the file's item away with the session's**, under its own section's line.
    a.key(Key::Char('d'));
    let folded = a.screen(110, 40).join("\n");
    assert!(
        !folded.contains("T1 the finished item"),
        "the file's done item is still drawn: {folded}"
    );
    assert!(folded.contains("T2 the open item"), "{folded}");
    assert!(
        folded.contains("Phase 0 — the plan  [1/2]"),
        "the heading's cookie is the file's, and it moved: {folded}"
    );
    assert!(
        folded.contains("done (1) · d shows them"),
        "the section's folded line does not name the count: {folded}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Folding the group under the cursor scrolls the window to where the cursor landed: the draw that folded them is
/// the first that knew the new numbers, so `todos_follow` makes THAT draw scroll rather than the keypress scroll
/// against the record of the draw before it.
#[test]
fn folding_the_group_under_the_cursor_keeps_the_cursor_on_the_screen() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    // **Distinct words matter** — a board's identity is its text ([`TodoStop`]), so sixty rows
    // saying the same thing would all draw the mark and prove nothing about which one it is on.
    let contents: Vec<String> = (0..60)
        .map(|i| {
            if i < 30 {
                format!("open row {i}")
            } else {
                format!("done row {i}")
            }
        })
        .collect();
    let rows: Vec<(&str, TodoBy, TodoStatus)> = contents
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let status = if i < 30 {
                TodoStatus::Pending
            } else {
                TodoStatus::Completed
            };
            (text.as_str(), TodoBy::Model, status)
        })
        .collect();
    let mut a = status_head(&rows);
    // **The group is read OPEN**, because the cursor has to be able to walk INTO it: the case is a
    // cursor five rows inside the group when it shuts, and the folded default has no such rows.
    a.todos_fold_done = false;
    // **Drawn between presses**, because the arrows scroll by the rows the last DRAW recorded: a
    // walk with no frames in it never moves the window, and the case this test is about is a
    // window that HAS moved.
    a.screen(110, 20);
    // The add control, then the thirty rows owed, then five rows into the group.
    for _ in 0..(1 + 30 + 5) {
        a.key(Key::Down);
        a.screen(110, 20);
    }
    assert!(
        a.pane_scroll > 0,
        "the window never moved, so this proves nothing"
    );

    a.key(Key::Char('d'));
    let screen = a.screen(110, 20);
    let marked: Vec<&String> = screen.iter().filter(|l| l.contains('▸')).collect();
    assert_eq!(
        marked.len(),
        1,
        "one mark, or the fixture repeats itself: {marked:?}"
    );
    assert!(
        marked[0].contains("open row 29"),
        "the cursor is not on the row it fell back to, or is not on the screen at all: {}",
        screen.join("\n")
    );
}

/// **`d` says which way it stands** — a bar that went on offering `folds done` over a group that is
/// already shut names a key whose press does the opposite of what it says. **The head opens folded**
/// (`HeadPrefs::default`), so the first sentence a reader meets is the showing one.
#[test]
fn the_bar_says_d_shows_the_group_and_then_that_it_folds_it() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = status_head(&[
        ("done already", TodoBy::Model, TodoStatus::Completed),
        ("still open", TodoBy::Model, TodoStatus::Pending),
    ]);
    let bar = a.hint_bar(200);
    assert!(bar.contains("d shows done"), "{bar}");
    assert!(!bar.contains("d folds done"), "{bar}");

    a.key(Key::Char('d'));
    let bar = a.hint_bar(200);
    assert!(bar.contains("d folds done"), "{bar}");
    assert!(
        !bar.contains("d shows done"),
        "the bar still offers the act that already happened: {bar}"
    );
    // The help lists both, because it is read without a list in front of it.
    let (_, help) = App::surface_help(Surface::Todos).expect("the todos pane is in the help");
    assert!(
        help.contains("folds the done group, or shows it again"),
        "{help}"
    );
}

/// `d` is the composer's while a line is typed: the letters are the person's to type first, and the empty-composer
/// guard is what keeps the key from eating one in the middle of a word.
#[test]
fn a_word_typed_over_the_todos_pane_reaches_the_composer_even_with_a_d_in_it() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let mut a = status_head(&[("done already", TodoBy::Model, TodoStatus::Completed)]);
    // The head opens folded, so the first `d` OPENS the group — the key is still the pane's, which
    // is what this test is about.
    assert!(
        a.todos_fold_done,
        "the fixture does not start where a head does"
    );
    a.key(Key::Char('d'));
    assert!(!a.todos_fold_done, "the first `d` is the pane's");
    assert!(a.editor.text().is_empty(), "{}", a.editor.text());

    let mut sent = Vec::new();
    for c in "legacy deploy".chars() {
        sent.extend(a.key(Key::Char(c)));
    }
    assert!(sent.is_empty(), "typing sent {sent:?}");
    assert_eq!(a.editor.text(), "legacy deploy");
    assert!(
        !a.todos_fold_done,
        "a `d` inside a typed line flipped the switch"
    );
}

/// The fold outlives the head: `todos_hide_done` in `head.toml`, where every other switch lives, since a switch
/// that forgets is one the reader keeps re-making. The key is the old spelling because every file on the box
/// carries it.
#[test]
fn the_fold_is_remembered_by_the_next_head() {
    use letibot_sessionlog::event::{TodoBy, TodoStatus};
    let dir = std::env::temp_dir().join(format!("letibot-todo-fold-prefs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("head.toml");

    let mut a = status_head(&[("done already", TodoBy::Model, TodoStatus::Completed)]);
    a.prefs_path = Some(path.clone());
    assert!(
        a.todos_fold_done,
        "the switch does not start where `HeadPrefs::default` puts it"
    );
    a.key(Key::Char('d'));
    assert!(!a.todos_fold_done, "the key did not flip the switch");
    let on_disk = std::fs::read_to_string(&path).expect("head.toml written");
    assert!(
        on_disk.contains("todos_hide_done = false"),
        "the switch was not written down under the key every head already has: {on_disk}"
    );
    assert!(
        !a.notice.clone().unwrap_or_default().contains("not saved"),
        "the confirmation says it was not saved: {:?}",
        a.notice
    );

    // The next head: a fresh app, the same file, and `load_prefs` is all it runs.
    let mut fresh = app();
    fresh.prefs_path = Some(path.clone());
    fresh.load_prefs();
    assert!(
        !fresh.todos_fold_done,
        "the switch did not come back on a restarted head"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
