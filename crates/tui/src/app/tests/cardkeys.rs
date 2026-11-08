//! Keys, cards and popups agree: each case a key sequence that must not send an answer, a `/reseat`
//! or a quit the operator did not choose, pinned so it cannot come back.

use super::*;
use letibot_sessionlog::question::QuestionAnswer;

fn ask(a: &mut App, seq: u64, e: SessionEvent) {
    a.apply(ServerFrame::Event(env(seq, e)));
}

fn answer(req_id: &str, option_id: &str) -> Option<Action> {
    Some(Action::Answer {
        req_id: req_id.into(),
        option_id: option_id.into(),
        pattern: None,
        note: None,
    })
}

// ─────────────────────── the highlight follows the front card ───────────────────────

/// ↓ to deny, a second permission arrives behind the card on screen, Enter still denies.
#[test]
fn a_decision_arriving_behind_the_card_does_not_move_its_highlight() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    assert_eq!(a.key(Key::Down), None);
    ask(&mut a, 2, testing::requested("r2", "rm -rf"));
    assert_eq!(a.open[0].req_id, "r1", "the new card queues behind");
    assert_eq!(a.key(Key::Enter), answer("r1", "deny"));
}

/// Card A answered on its second row; card B comes up at its first, not A's.
#[test]
fn the_next_card_up_starts_at_its_own_top() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    ask(&mut a, 2, testing::requested("r2", "rm -rf"));
    a.key(Key::Down);
    assert_eq!(a.key(Key::Enter), answer("r1", "deny"));
    ask(&mut a, 3, testing::answered("r1", "deny"));
    assert_eq!(a.open[0].req_id, "r2");
    assert_eq!(a.key(Key::Enter), answer("r2", "allow"));
}

/// A click marks a row of the card the FRAME drew, and of no other: a key earlier in the same
/// read can have answered that card and put another in the slot, and the row the frame
/// recorded then names a row the operator never saw.
#[test]
fn a_click_aimed_at_a_decision_marks_no_row_of_the_card_that_took_the_slot() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    let frame = a.screen(100, 30);
    let y = frame
        .iter()
        .position(|l| l.contains("2  Deny"))
        .expect("the second rung is drawn") as u16;
    ask(&mut a, 2, testing::answered("r1", "deny"));
    ask(&mut a, 3, testing::requested("r2", "rm -rf"));
    assert_eq!(a.open[0].req_id, "r2");
    assert_eq!(a.key(Key::Click { x: 6, y }), None);
    assert_eq!(a.sel, 0, "a row index from r1's card marked a row of r2's");
}

// ───────────────── a question over typed words does not take them ─────────────────

/// A question that arrives while a prompt is half typed: the words are held, the card's field is
/// empty, Enter answers the marked row, and the words come back when the card is gone.
#[test]
fn a_question_arriving_over_typed_words_holds_them_instead_of_taking_them() {
    let mut a = app();
    typed(&mut a, "check the logs first");
    ask(
        &mut a,
        1,
        testing::asked("q1", "which branch?", &["main", "release"], ""),
    );
    assert_eq!(a.input(), "", "the card's field starts empty");
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("your line is held"),
        "{:?}",
        a.notice
    );
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::AnswerQuestion {
            req_id: "q1".into(),
            answer: QuestionAnswer::choosing(0),
        }),
        "Enter answers the marked row, not the operator's prompt"
    );
    ask(&mut a, 2, testing::answered("q1", "main"));
    assert!(a.open.is_empty());
    assert_eq!(a.input(), "check the logs first", "the line is back");
}

/// Esc the question away, start typing, alt-a to reread it, Enter: the question is answered with
/// its marked row and the typed line survives.
#[test]
fn a_question_brought_back_over_typed_words_holds_them() {
    let mut a = app();
    ask(
        &mut a,
        1,
        testing::asked("q1", "which branch?", &["main", "release"], ""),
    );
    a.key(Key::Esc);
    assert!(a.open.is_empty(), "put away");
    typed(&mut a, "check the logs first");
    a.key(Key::AltA);
    assert_eq!(a.open[0].req_id, "q1", "it came back");
    assert_eq!(a.input(), "");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::AnswerQuestion {
            req_id: "q1".into(),
            answer: QuestionAnswer::choosing(0),
        })
    );
    // Put away again instead: the words still come back.
    let mut b = app();
    ask(
        &mut b,
        1,
        testing::asked("q1", "which branch?", &["main", "release"], ""),
    );
    b.key(Key::Esc);
    typed(&mut b, "my prompt");
    b.key(Key::AltA);
    typed(&mut b, "rel");
    b.key(Key::Esc);
    assert_eq!(
        b.input(),
        "my prompt",
        "the held line, not the card's draft"
    );
    b.key(Key::AltA);
    assert_eq!(b.input(), "rel", "the card's own draft comes back with it");
}

// ──────────────── a row number marks a permission's row ────────────────

/// On a permission a row number marks the row it names - the numbers are drawn and the bar says
/// so - and only Enter answers. A question's rows carry no number: its digits are its answer.
#[test]
fn a_digit_on_a_permission_marks_its_numbered_row_and_enter_answers() {
    let mut b = app();
    ask(&mut b, 1, testing::requested("r1", "rm"));
    let card = b.decision_lines(&b.open[0].clone(), 80).join("\n");
    assert!(
        card.contains("▸ 1  Allow once") && card.contains("2  Deny"),
        "{card}"
    );
    assert!(b.hint_bar(200).contains("a row number marks"));
    assert_eq!(b.key(Key::Char('2')), None, "a digit answers nothing");
    assert_eq!(b.sel, 1, "it moved the mark");
    assert_eq!(b.key(Key::Enter), answer("r1", "deny"));
}

/// A question with no choices has no ladder to walk; an empty choice is not a blank row.
#[test]
fn a_question_without_choices_offers_no_arrows_and_an_empty_choice_is_named() {
    let mut a = app();
    ask(
        &mut a,
        1,
        testing::asked("q1", "what is it called?", &[], ""),
    );
    let card = a.decision_lines(&a.open[0].clone(), 80).join("\n");
    assert!(!card.contains("↑↓"), "{card}");
    assert!(card.contains("type your answer"), "{card}");
    assert!(!a.hint_bar(200).contains("↑↓"), "{}", a.hint_bar(200));
    typed(&mut a, "an old prompt");
    a.key(Key::CtrlC);
    assert_eq!(a.key(Key::Up), None);
    assert_eq!(a.input(), "", "↑ does not recall history into the answer");
    assert_eq!(a.key(Key::Enter), None);
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("type your answer")
    );

    let mut b = app();
    ask(&mut b, 1, testing::asked("q1", "suffix?", &["", "-v2"], ""));
    let card = b.decision_lines(&b.open[0].clone(), 80).join("\n");
    assert!(card.contains("▸ (an empty answer)"), "{card}");
    assert!(!card.contains("1  (an empty answer)"), "no number: {card}");
}

// ───────────────────── the prompt card has a way back ─────────────────────

/// Esc puts the prompt card away; a resident line says the command waits, and alt-a brings the
/// card back.
#[test]
fn the_prompt_card_put_away_has_a_resident_line_and_alt_a_brings_it_back() {
    let mut a = app();
    ask(
        &mut a,
        1,
        SessionEvent::PromptRequested {
            req_id: "p1".into(),
            job: "j1".into(),
            command: "sudo apt install mc".into(),
            question: Some("Continue? [Y/n]".into()),
            reading: letibot_sessionlog::PromptReading::Blocked,
        },
    );
    a.key(Key::Esc);
    assert!(a.prompt_away);
    let line = a.waiting_line(200).join("");
    assert!(
        line.contains("YOUR COMMAND IS WAITING") && line.contains("alt-a"),
        "{line}"
    );
    a.key(Key::AltA);
    assert!(!a.prompt_away, "alt-a brought the card back");
    assert!(a.waiting_line(200).is_empty());
    typed(&mut a, "y");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::PromptAnswer {
            req_id: "p1".into(),
            line: "y".into()
        })
    );
}

// ─────────────── the quit card and what it can be answered by ───────────────

/// ctrl-c ctrl-c over a permission card does not open the quit card (whose `1` would answer the
/// permission); a decision arriving over an open quit card closes it.
#[test]
fn the_quit_card_never_stands_over_a_decision() {
    let mut a = app();
    a.clock(1_000);
    ask(&mut a, 1, testing::requested("r1", "rm"));
    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    assert!(!a.quit_card, "not over a decision");
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("a decision is up")
    );
    assert_eq!(a.open[0].req_id, "r1");

    let mut b = app();
    b.clock(1_000);
    b.key(Key::CtrlC);
    b.key(Key::CtrlC);
    assert!(b.quit_card);
    ask(&mut b, 1, testing::requested("r1", "rm"));
    assert!(!b.quit_card, "the decision closed it");
    assert_eq!(b.key(Key::Char('1')), None, "1 marks the decision's row");
    assert!(!b.quit);
}

/// Words typed after the quit card opened are not lost on an Enter that quits.
#[test]
fn the_quit_cards_enter_keeps_the_empty_composer_guard() {
    let mut a = app();
    a.clock(1_000);
    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    assert!(a.quit_card);
    typed(&mut a, "wait");
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.quit_card && !a.quit, "still up, nothing quit");
    assert_eq!(a.input(), "wait");
}

/// A digit on a numbered card marks the row; the answer is the deliberate Enter. One rule for every
/// numbered card: a digit typed as the first character of a line must not leave the head, and the bar
/// under the card promises `1/2 or arrows then enter`.
#[test]
fn a_digit_on_the_quit_card_marks_the_row_and_stops_nothing() {
    let mut a = app();
    a.clock(1_000);
    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    assert!(a.quit_card);
    assert_eq!(a.key(Key::Char('2')), None, "a move, not an answer");
    assert_eq!(a.quit_sel, 1, "the stop row is marked");
    assert!(!a.quit && a.quit_card, "nothing left, the card is still up");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::StopDaemon),
        "and Enter answers it"
    );

    // The line being typed when the card opened is not an answer either: the first digit marks,
    // the rest is the operator's, and Enter refuses while the composer holds words.
    let mut b = app();
    b.clock(1_000);
    b.key(Key::CtrlC);
    b.key(Key::CtrlC);
    assert_eq!(b.key(Key::Char('2')), None, "the first digit is the mark");
    typed(&mut b, " minutes");
    assert_eq!(b.key(Key::Enter), None, "words in the composer");
    assert!(!b.quit && b.quit_card, "still up, nothing quit");
}

// ──────────── one order for the slot, the keys and the bar ────────────

/// The todo card is open and a permission arrives: the todo card stays drawn and keeps its keys,
/// the permission's ladder does not move under it, and the bar is the todo card's.
#[test]
fn a_draft_card_in_front_of_a_decision_is_drawn_keyed_and_barred_as_one() {
    let mut a = app();
    a.session_id = "s".into();
    a.open_todo_card();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        !screen.contains("Allow once"),
        "the decision waits behind: {screen}"
    );
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("waiting behind this card"),
        "{:?}",
        a.notice
    );
    let bar = a.hint_bar(200);
    assert!(bar.contains("enter adds it"), "{bar}");
    assert_eq!(a.key(Key::Down), None);
    assert_eq!(a.sel, 0, "↓ did not move a ladder nobody sees");
    a.key(Key::Esc);
    assert!(a.todo_draft.is_none(), "Esc was the todo card's");
    assert_eq!(a.open.len(), 1, "and not the decision's");
    assert!(a.screen(120, 30).join("\n").contains("Allow once"));
}

/// The setting picker opened while a decision is up: the decision is drawn and keyed, so the bar
/// is the decision's, not the picker's.
#[test]
fn the_bar_names_the_decision_over_a_picker() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    a.pick = Some(Pick::Mode);
    let bar = a.hint_bar(200);
    assert!(bar.contains("a row number marks"), "{bar}");
}

/// Cards that own every key have a bar of their own, without the conversation's chords.
#[test]
fn a_card_that_owns_the_keyboard_has_its_own_bar() {
    let mut a = app();
    a.mode_confirm = Some("allow-all".into());
    let bar = a.hint_bar(200);
    assert!(bar.contains("esc puts it away"), "{bar}");
    assert!(bar.contains("n refuses"), "{bar}");
    assert!(!bar.contains("ctrl-t"), "{bar}");
    let mut b = app();
    ask(
        &mut b,
        1,
        SessionEvent::SecretRequested {
            req_id: "s1".into(),
            prompt: "password".into(),
            command: "sudo ls".into(),
            deadline: 0,
        },
    );
    let bar = b.hint_bar(200);
    assert!(bar.contains("type the password"), "{bar}");
    assert!(bar.contains("ctrl-c refuses"), "{bar}");
    assert!(!bar.contains("ctrl-s"), "{bar}");
}

/// Esc puts a card away and sends nothing, on every card: the refusal is an explicit key's
/// (n on the allow-all card, ctrl-c on the password field), and the card says which. A card
/// put away is not gone - the daemon's request keeps waiting, and alt-a brings it back.
#[test]
fn esc_puts_a_card_away_and_the_explicit_key_refuses() {
    let mut a = app();
    a.apply(mode_settings(
        "writes allowed",
        &["writes allowed", "allow-all"],
    ));
    a.session_id = "s-1".into();
    a.apply(ServerFrame::ModeConfirm {
        session_id: "s-1".into(),
        req_id: "h2:req-1".into(),
        name: "allow-all".into(),
        asked_by: "mcp".into(),
    });
    assert_eq!(a.key(Key::Esc), None, "esc sent something");
    assert!(a.mode_confirm.is_none(), "the card is still up");
    assert_eq!(
        a.mode_away.as_ref().map(|m| m.req_id.as_str()),
        Some("h2:req-1"),
        "the request was not kept for alt-a"
    );
    // And the card comes back armed, and settles when answered.
    a.key(Key::AltA);
    assert!(
        a.mode_confirm.is_some(),
        "alt-a did not bring the card back"
    );
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Char('n')),
        Some(Action::ModeConsent {
            req_id: "h2:req-1".into(),
            yes: false
        })
    );

    // The password card: esc puts it away with nothing sent, ctrl-c is the refusal, and
    // alt-a is the way back to a sudo that keeps waiting.
    let mut b = app();
    ask(
        &mut b,
        1,
        SessionEvent::SecretRequested {
            req_id: "secret-s".into(),
            prompt: "[sudo] password for dead: ".into(),
            command: "sudo ls".into(),
            deadline: 1_000_000,
        },
    );
    assert_eq!(b.key(Key::Esc), None, "esc refused the ask");
    assert!(b.secret.is_some() && b.secret_away, "the ask was closed");
    let away = b.screen(100, 20).join("\n");
    assert!(!away.contains("sudo wants a password"), "{away}");
    b.key(Key::AltA);
    assert!(!b.secret_away, "alt-a did not bring the card back");
    assert_eq!(
        b.key(Key::CtrlC),
        Some(Action::Secret {
            req_id: "secret-s".into(),
            secret: None,
        })
    );
}

// ──────────────────── the panes' bars ────────────────────

fn waiting_board(a: &mut App) {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoNeed, TodoStatus};
    a.session_id = "s".into();
    a.apply(ServerFrame::Todos {
        seats: Vec::new(),
        session_id: "s".into(),
        todos: vec![TodoEntry {
            content: "pick the db".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
            needs: vec![TodoNeed::Operator {
                question: "postgres or sqlite?".into(),
                options: vec!["postgres".into(), "sqlite".into()],
            }],
        }],
    });
}

/// Enter on a row waiting on you opens its question; the bar says so instead of "reads it whole".
#[test]
fn the_todos_bar_says_enter_answers_a_waiting_rows_question() {
    let mut a = app();
    waiting_board(&mut a);
    a.key(Key::CtrlT);
    // Walk to the waiting row, wherever the pane puts it.
    for _ in 0..8 {
        if a.hint_bar(300).contains("answers its question") {
            break;
        }
        a.key(Key::Down);
    }
    let bar = a.hint_bar(300);
    assert!(bar.contains("enter answers its question"), "{bar}");
    a.key(Key::Enter);
    assert_eq!(a.open[0].summary, "postgres or sqlite?");
    let pane = a.screen(160, 40).join("\n");
    assert!(!pane.contains("or alt-a, answers it"), "{pane}");
}

/// ↑↓ scroll /help instead of recalling history into the composer.
#[test]
fn the_arrows_scroll_a_reading_surface() {
    let a = app();
    for s in [Surface::Help, Surface::Stats] {
        let b = a.binding_for(s, &Key::Up).expect("↑ is bound");
        assert_eq!(b.act, crate::app::keys::bindings::Act::Scroll, "{s:?}");
    }
    let mut a = app();
    a.push_surface(Surface::Help);
    assert_eq!(a.key(Key::Up), None);
    assert_eq!(a.input(), "");
    assert!(a.hint_bar(300).contains("↑↓"), "{}", a.hint_bar(300));
}

/// Enter over a reading pane with a line typed is swallowed - and now said.
#[test]
fn a_swallowed_enter_over_a_reading_pane_is_said() {
    let mut a = app();
    a.push_surface(Surface::Help);
    typed(&mut a, "hello");
    assert_eq!(a.key(Key::Enter), None);
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("this pane takes enter"),
        "{:?}",
        a.notice
    );
    assert_eq!(a.input(), "hello");
}

/// `/peek ID` with no pane open: Esc goes to the conversation and the bar says so.
#[test]
fn a_peek_over_no_pane_names_the_conversation_as_where_esc_goes() {
    let mut a = app();
    a.push_surface(Surface::SubOut);
    let bar = a.hint_bar(400);
    assert!(bar.contains("esc back to the conversation"), "{bar}");
    let mut b = app();
    b.push_surface(Surface::Jobs);
    b.push_surface(Surface::SubOut);
    assert!(b.hint_bar(400).contains("esc back to the jobs"));
}

// ──────────── todo cards follow the board ────────────

/// Another head answered the row: the board no longer has it waiting, so this head's card closes
/// and what was typed on it does not become a prompt.
#[test]
fn a_todo_card_closes_when_its_row_stops_waiting() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
    let mut a = app();
    waiting_board(&mut a);
    a.key(Key::AltA);
    assert!(a.open[0].req_id.starts_with("todo-ask:"));
    typed(&mut a, "sqli");
    let done = TodoEntry {
        content: "pick the db".into(),
        status: TodoStatus::Completed,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    };
    a.apply(ServerFrame::Todos {
        seats: Vec::new(),
        session_id: "s".into(),
        todos: vec![done.clone()],
    });
    assert!(a.open.is_empty(), "the stale card closed");
    assert_eq!(a.input(), "", "its words are not left to become a prompt");
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("no longer waits")
    );

    // A put-away card for a row that stopped waiting is forgotten too, and the tab stops waiting.
    let mut b = app();
    waiting_board(&mut b);
    b.key(Key::AltA);
    b.key(Key::Esc);
    assert_eq!(b.away.len(), 1);
    b.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated { todos: vec![done] },
    )));
    assert!(b.away.is_empty());
    assert_eq!(b.progress(), rano::term::Progress::Idle);
}

/// A question put away in session A does not keep session B's tab on Waiting.
#[test]
fn the_tab_waits_only_on_what_waits_in_this_session() {
    let mut a = app();
    a.session_id = "a".into();
    ask(&mut a, 1, testing::asked("q1", "which?", &["x", "y"], ""));
    a.key(Key::Esc);
    assert_eq!(a.progress(), rano::term::Progress::Waiting);
    a.session_id = "b".into();
    assert_ne!(a.progress(), rano::term::Progress::Waiting);
}

/// A resync while answering a todo row: the card is put away with its draft, not dropped with the
/// draft left to be sent as a prompt.
#[test]
fn a_resync_puts_the_todo_card_away_with_its_draft() {
    let hub = Hub::new("s");
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    waiting_board(&mut a);
    a.key(Key::AltA);
    typed(&mut a, "sqli");
    a.apply(ServerFrame::Resync {
        reason: "test".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    assert!(a.open.is_empty());
    assert_eq!(a.input(), "", "the draft did not stay behind as a prompt");
    a.key(Key::AltA);
    assert!(a.open[0].req_id.starts_with("todo-ask:"));
    assert_eq!(a.input(), "sqli", "alt-a brings it back with the draft");
}

/// A model's question dropped by a resync while its answer is being typed: the words were an
/// answer, not a prompt, so they go with the card rather than being sent to the model by the next
/// Enter. A permission's words are the operator's own and stay.
#[test]
fn a_question_a_resync_drops_takes_its_draft_with_it() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    ask(
        &mut a,
        1,
        testing::asked("q1", "which branch?", &["main"], ""),
    );
    typed(&mut a, "the release one");
    a.apply(ServerFrame::Resync {
        reason: "test".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    assert!(
        a.open.is_empty(),
        "the snapshot no longer holds the question"
    );
    assert_eq!(a.input(), "", "the words were an answer, not a prompt");
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("was cleared (ctrl-z brings it back)"),
        "{:?}",
        a.notice
    );
    assert_eq!(a.key(Key::Enter), None, "nothing is sent");

    // A permission has no field: what is typed under it is the operator's own line.
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut b = app();
    b.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    ask(&mut b, 1, testing::requested("r1", "rm"));
    typed(&mut b, "half a line");
    b.apply(ServerFrame::Resync {
        reason: "test".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    assert_eq!(b.input(), "half a line", "not the card's to take");
}

/// A switch carries the line being typed. `load` is the switch path as well as the resync
/// path, and the question it read as gone was the OUTGOING session's card: nothing about that
/// card's answer is in this line, so leaving takes nothing with it.
#[test]
fn a_switch_away_from_a_question_keeps_the_typed_line() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    ask(
        &mut a,
        1,
        testing::asked("q1", "which branch?", &["main"], ""),
    );
    typed(&mut a, "the release one");
    let two = Hub::new("two");
    a.apply(hello(
        "two",
        vec![brief("s", "one", false), brief("two", "two", false)],
        two.snapshot(),
    ));
    assert_eq!(a.session_id, "two", "the switch happened");
    assert!(
        a.open.is_empty(),
        "the other session's card did not come along"
    );
    assert_eq!(
        a.input(),
        "the release one",
        "leaving a session is not the card's answer going away"
    );
    assert!(
        !a.notice.as_deref().unwrap_or("").contains("was cleared"),
        "and nothing says the line was cleared: {:?}",
        a.notice
    );
}

/// The control for the switch rule: a switch with no question up carries the line, which is what
/// the head did before the draft rule and must keep doing.
#[test]
fn a_switch_with_no_question_up_keeps_the_typed_line() {
    let hub = Hub::new("s");
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    typed(&mut a, "half a line");
    let two = Hub::new("two");
    a.apply(hello(
        "two",
        vec![brief("s", "one", false), brief("two", "two", false)],
        two.snapshot(),
    ));
    assert_eq!(a.input(), "half a line", "the line is the operator's");
}

/// A command is never a card's answer: `/`, `!`, `!term` and `!send` are taken by
/// [`App::submit`] before the open card is consulted, so a verb typed under a question that goes
/// is the operator's line and stays.
#[test]
fn a_command_under_a_dropped_question_stays_in_the_composer() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    ask(
        &mut a,
        1,
        testing::asked("q1", "which branch?", &["main"], ""),
    );
    typed(&mut a, "/queue");
    a.apply(ServerFrame::Resync {
        reason: "test".into(),
        dropped: 0,
        snapshot: Box::new(hub.snapshot()),
        scrubbed: Default::default(),
    });
    assert!(
        a.open.is_empty(),
        "the snapshot no longer holds the question"
    );
    assert_eq!(
        a.input(),
        "/queue",
        "a verb is the operator's line, not the card's answer"
    );
}

/// A model's question notifies as a question, a todo card as a todo row asking.
#[test]
fn a_notification_names_the_kind_of_card() {
    let mut a = app();
    a.set_features(rano::term::Features::ALL);
    a.take_notification();
    a.key(Key::FocusOut);
    ask(&mut a, 1, testing::asked("q1", "which branch?", &["x"], ""));
    let n = a.take_notification().expect("notified");
    assert!(n.contains("question for you"), "{n}");
}

// ─────────────────────── the popup and narrow widths ───────────────────────

fn notice_popup(a: &mut App) {
    a.diff_popup = Some(crate::app::diff_popup::DiffPopup {
        body: crate::app::diff_popup::PopupBody::Notice {
            text: "a long notice line that has to wrap somewhere inside the box".into(),
            jobs: Vec::new(),
            answer: None,
            title: None,
        },
        scroll: 0,
        placed: false,
        laid: None,
        room: 0,
    });
}

/// With a decision open, the popup's edge offers only the keys that still scroll it.
#[test]
fn the_popup_edge_drops_the_keys_a_decision_takes() {
    let mut a = app();
    notice_popup(&mut a);
    let edge = a.diff_popup_rows(80, 12).join("\n");
    assert!(edge.contains("↑↓ pgup/pgdn home/end"), "{edge}");
    ask(&mut a, 1, testing::requested("r1", "rm"));
    let edge = a.diff_popup_rows(80, 12).join("\n");
    assert!(!edge.contains("↑↓") && edge.contains("pgup/pgdn"), "{edge}");
    assert!(a.hint_bar(200).contains("the popup: pgup/pgdn"));
}

/// Below ~24 columns the popup's rows overflowed the box; none is wider than the screen now.
#[test]
fn the_popup_fits_a_narrow_screen() {
    let mut a = app();
    notice_popup(&mut a);
    for w in [12usize, 18, 22] {
        for r in a.diff_popup_rows(w, 10) {
            let plain = without_control_lines(&r);
            assert!(rano::width::text::width(&plain) <= w, "{w}: {plain:?}");
        }
    }
}

// ──────────── what waits, said and clickable ────────────

/// The card says how many more wait behind it, and whether one has a deadline.
#[test]
fn the_front_card_counts_what_waits_behind_it() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    let mut q = testing::asked("q2", "which?", &["x"], "");
    if let SessionEvent::DecisionRequested { deadline, .. } = &mut q {
        *deadline = Some(u64::MAX / 2);
    }
    ask(&mut a, 2, q);
    let card = a.decision_lines(&a.open[0].clone(), 120).join("\n");
    assert!(
        card.contains("+1 more waiting behind this card — one has a deadline"),
        "{card}"
    );
}

/// The waiting line does not offer alt-a for the card already open, counts every card put away,
/// and its key hint is a button.
#[test]
fn the_waiting_line_is_about_what_is_not_on_the_screen() {
    let mut a = app();
    waiting_board(&mut a);
    assert!(a.waiting_line(200).join("").contains("alt-a to answer"));
    a.key(Key::AltA);
    assert!(a.waiting_line(200).is_empty(), "{:?}", a.waiting_line(200));

    let mut b = app();
    b.session_id = "s".into();
    ask(&mut b, 1, testing::asked("q1", "one?", &["x"], ""));
    b.key(Key::Esc);
    ask(&mut b, 2, testing::asked("q2", "two?", &["x"], ""));
    b.key(Key::Esc);
    let l = b.waiting_line(200).join("");
    assert!(l.contains("+1 more put away"), "{l}");
    let cs: Vec<char> = without_control_lines(&l).chars().collect();
    let keys = crate::app::keyhints::scan_row(0, &cs);
    assert!(
        keys.iter().any(|k| k.bytes == b"\x1ba".to_vec()),
        "alt-a is a button on the line: {l}"
    );
}

/// /help names the card keys, alt-r/alt-p, and that ctrl-c twice asks rather than quits.
#[test]
fn the_help_names_the_card_keys() {
    let a = app();
    let help = help_lines(&a.cfg, 200, &a.key_overrides).join("\n");
    for words in [
        "alt-a brings",
        "alt-r reseats",
        "row number does too",
        "asks whether to",
    ] {
        assert!(help.contains(words), "{words}: {help}");
    }
}

/// A late `Peeked` after the pane that asked was closed does not open over what came next.
#[test]
fn a_late_peek_reply_after_its_pane_closed_is_not_opened() {
    let mut a = app();
    a.push_surface(Surface::Todos);
    a.ask_peek("s-sub-1");
    a.key(Key::Esc);
    assert_eq!(a.top_surface(), None);
    a.apply(ServerFrame::Peeked {
        session_id: "s-sub-1".into(),
        dropped: 0,
        events: vec![],
        snapshot: None,
    });
    assert_eq!(
        a.top_surface(),
        None,
        "nothing opened over the conversation"
    );
    assert!(a.sub_out.is_none());
}

// ──────────── an answered card closes at once, and comes back if refused ────────────

/// Enter closes the permission card the moment the answer leaves, so a second Enter sends
/// nothing; a refusal of that answer puts the card back with its marked row; the daemon's
/// settlement of it settles the card this head closed.
#[test]
fn an_answered_card_closes_at_once_and_a_refusal_reopens_it() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    a.key(Key::Down);
    assert_eq!(a.key(Key::Enter), answer("r1", "deny"));
    assert!(a.open.is_empty(), "the card closed when the answer left");
    assert_eq!(a.key(Key::Enter), None, "a second Enter cannot resend");
    a.answer_sent("r1", "c7".into());
    a.apply(ServerFrame::Rejected {
        client_request_id: "c7".into(),
        reason: "the answer was malformed".into(),
        expected_seq: 1,
        actual_seq: 1,
    });
    assert_eq!(
        a.open.first().map(|d| d.req_id.as_str()),
        Some("r1"),
        "refused: back"
    );
    assert_eq!(
        a.key(Key::Enter),
        answer("r1", "deny"),
        "with the row it had"
    );
    a.answer_sent("r1", "c8".into());
    ask(&mut a, 2, testing::answered("r1", "deny"));
    assert!(a.open.is_empty() && a.answering.is_empty(), "settled");
    // A refusal that says the decision is gone puts nothing back.
    ask(&mut a, 3, testing::requested("r2", "rm"));
    a.key(Key::Enter);
    a.answer_sent("r2", "c9".into());
    a.apply(ServerFrame::Rejected {
        client_request_id: "c9".into(),
        reason: letibot_sessionlog::protocol::REJECT_UNKNOWN_DECISION.into(),
        expected_seq: 3,
        actual_seq: 3,
    });
    assert!(a.open.is_empty(), "settled elsewhere: not reopened");
}

// ──────── A question's digits are its answer, and an answer in flight is not a card ────────

/// Typing `16` as the free answer to "how many workers?" marked row 1 and left `6` in the field,
/// so the answer sent was not the one typed. A question's digits are its answer; the arrows mark.
#[test]
fn a_question_cards_digit_is_typed_into_its_answer() {
    let mut a = app();
    ask(
        &mut a,
        1,
        testing::asked("q1", "how many workers?", &["2", "4", "8"], ""),
    );
    typed(&mut a, "16");
    assert_eq!(a.input(), "16", "both characters are the answer's");
    assert_eq!(a.sel, 0, "no digit moved the mark");
    let card = a.decision_lines(&a.open[0].clone(), 80).join("\n");
    assert!(!card.contains("a row number"), "{card}");
    assert!(card.contains("type your own"), "{card}");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::AnswerQuestion {
            req_id: "q1".into(),
            answer: QuestionAnswer::free("16"),
        })
    );
}

/// The ctrl-v window's own seam prints `esc closes` and it owns the arrows while it is open, so a
/// decision arriving over it must not take the arrows off the row being read.
#[test]
fn the_payload_window_keeps_the_arrows_from_a_decision_card() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::NORMAL);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    let payload: String = (0..40).map(|i| format!("LINE-{i}\n")).collect();
    shell_row(
        &mut a,
        1,
        "s.t1",
        "tool_result",
        TranscriptItem::ToolResult {
            call_id: "c0".into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload,
            edits: Vec::new(),
            origin: None,
            media: None,
        },
    );
    a.key(Key::CtrlV);
    assert!(a.payload_window_open(), "the window opened");
    ask(&mut a, 3, testing::requested("r1", "rm"));
    a.key(Key::Down);
    assert_eq!(a.sel, 0, "the arrow did not move a ladder nobody aimed at");
    assert_eq!(a.payload_page, 10, "it paged the window under the card");
}

/// An answer has left and its settlement has not: the same ask said again - a replay, a second
/// head's resync - must not put the card back, or the next Enter sends the answer twice.
#[test]
fn a_replayed_decision_does_not_reopen_the_card_it_answered() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    assert_eq!(a.key(Key::Enter), answer("r1", "allow"));
    assert!(a.open.is_empty(), "the card closed when the answer left");
    ask(&mut a, 2, testing::requested("r1", "rm"));
    assert!(a.open.is_empty(), "the answered card did not come back");
    assert_eq!(a.key(Key::Enter), None, "and a second Enter sends nothing");
}

// ────────── a notification names what needs the person, a stale prompt too ──────────

/// The kinds are named for what they are - a permission, a model's question, a todo row's question -
/// and the reseat card notifies too: it is this head's own, and nothing else says it is waiting.
#[test]
fn a_notification_names_what_needs_the_person_including_a_stale_prompt() {
    let mut a = app();
    a.set_features(rano::term::Features::ALL);
    a.take_notification();
    a.key(Key::FocusOut);
    ask(&mut a, 1, testing::requested("r1", "rm"));
    let n = a.take_notification().expect("notified");
    assert!(n.contains("permission needed"), "{n}");

    let mut b = app();
    b.set_features(rano::term::Features::ALL);
    b.take_notification();
    b.key(Key::FocusOut);
    waiting_board(&mut b);
    b.key(Key::AltA);
    let n = b.take_notification().expect("notified");
    assert!(n.contains("a todo row asks you"), "{n}");

    let mut c = app();
    c.set_features(rano::term::Features::ALL);
    c.take_notification();
    c.key(Key::FocusOut);
    c.prefix_stale("the prompt of this session was built by another build".into());
    let n = c.take_notification().expect("the stale prompt notified");
    assert!(n.contains("the prompt is stale"), "{n}");
    assert!(n.contains("alt-r reseats"), "{n}");
    c.prefix_stale("the prompt of this session was built by another build".into());
    assert_eq!(c.take_notification(), None, "a fact said twice is one ask");
}

/// A question the daemon settles while the operator types - a deadline lapsed, or another head
/// answered it - takes the words typed under it, which were an answer and would otherwise be sent
/// by the next Enter as a prompt. The same defect the snapshot door had, at the live door.
#[test]
fn a_question_settled_live_takes_its_draft_with_it() {
    let mut a = app();
    ask(
        &mut a,
        1,
        testing::asked("q1", "how many workers?", &["4"], ""),
    );
    typed(&mut a, "16");
    ask(&mut a, 2, testing::answered("q1", "4"));
    assert!(a.open.is_empty(), "the question closed under the fingers");
    assert_eq!(
        a.input(),
        "",
        "the words were an answer to a question that is gone, not a prompt"
    );
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("was cleared (ctrl-z brings it back)"),
        "{:?}",
        a.notice
    );
    assert_eq!(a.key(Key::Enter), None, "and nothing is sent");

    // A permission settling live has no field of its own, so a line typed under it stays.
    let mut b = app();
    ask(&mut b, 1, testing::requested("r1", "rm"));
    typed(&mut b, "half a line");
    ask(&mut b, 2, testing::answered("r1", "deny"));
    assert_eq!(b.input(), "half a line", "not the card's to take");
}

/// Looking at another session is not the question closing. A switch's snapshot replaces the open
/// list with the other session's, which says nothing about this one's cards; the typed answer was
/// cleared and a sentence said its card had closed while the question was still open.
#[test]
fn a_session_switch_keeps_the_answer_being_typed() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", true)], hub.snapshot()));
    // The question in the daemon's own view too, so a snapshot of this session still holds it.
    let question = testing::asked("q1", "which branch?", &["main"], "");
    a.apply(ServerFrame::Event(env(1, question.clone())));
    hub.publish(question);
    typed(&mut a, "release-1.4");
    let other = Hub::new("s-other");
    a.apply(hello(
        "s-other",
        vec![brief("s-other", "other", true)],
        other.snapshot(),
    ));
    a.apply(hello("s", vec![brief("s", "one", true)], hub.snapshot()));
    assert_eq!(a.open[0].req_id, "q1", "the question is still open on s");
    assert_eq!(
        a.held_line.as_deref(),
        Some("release-1.4"),
        "the answer went with the card that is still open, and comes back with it"
    );
    assert!(
        !a.notice.as_deref().unwrap_or("").contains("no longer open"),
        "the switch said the question had closed: {:?}",
        a.notice
    );
}

// ────────────────────── a card in the chrome is what holds the keyboard ──────────────────────

/// No surface under a card is focused. A card is drawn in the chrome slot with the pane still on
/// the screen above it, and every key the reader presses is the card's - so a pane that answered
/// `Some(s)` put the wheel, a hover and a click on rows nothing could act on.
#[test]
fn a_card_in_the_chrome_leaves_no_surface_focused() {
    let mut a = app();
    a.push_surface(Surface::Help);
    assert_eq!(
        a.focused_surface(),
        Some(Surface::Help),
        "the premise: a pane nothing covers has the keys"
    );
    assert_eq!(a.card_up(), None);

    // A running program asks a question: the card is in the slot, and the pane under it is not.
    ask(
        &mut a,
        1,
        SessionEvent::PromptRequested {
            req_id: "r1".into(),
            job: "j1".into(),
            command: "! sudo apt install mc".into(),
            question: Some("Continue? [Y/n]".into()),
            reading: letibot_sessionlog::PromptReading::Blocked,
        },
    );
    assert_eq!(a.card_up(), Some(Card::Prompt));
    assert_eq!(
        a.focused_surface(),
        None,
        "the card holds every key, so the pane under it is not focused"
    );
    // The bar says the card, and not the pane's own chords.
    assert!(
        a.hint_bar(300).contains("type the line"),
        "{}",
        a.hint_bar(300)
    );

    // Put the card aside with Esc, which is what the bar promises: the pane has the keys again.
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.prompt_away);
    assert_eq!(a.focused_surface(), Some(Surface::Help));
}

// ───────────────── key hints belong to the head, and yield to cards ─────────────────

fn hint_cell(a: &mut App, w: usize, needle: &str) -> (u16, u16) {
    let screen = a.screen(w, 40);
    let (y, row) = screen
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not on the screen: {screen:#?}"));
    let col = crate::app::keyhints::cells(row)
        .windows(needle.chars().count())
        .position(|c| c.iter().collect::<String>() == needle)
        .expect("the hint's column");
    (col as u16, y as u16)
}

/// A hint clicked, then a card arriving before the read that applies it: the key would answer
/// the card, so it is dropped.
#[test]
fn a_hint_click_is_dropped_when_a_card_came_up_before_it_was_applied() {
    let mut a = app();
    let (x, y) = hint_cell(&mut a, 160, "ctrl-g subagents");
    a.key(Key::Click { x, y });
    assert_eq!(a.injected, vec![0x07], "the click queued nothing");
    ask(&mut a, 1, testing::requested("r1", "rm"));
    assert!(a.take_injected().is_empty(), "the key reached the new card");
}

/// With a decision in front a click is the card's, and a hint drawn on the bar is not a key.
#[test]
fn a_hint_click_under_a_card_types_nothing() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    let screen = a.screen(160, 40);
    let bar = screen.last().expect("a bar").clone();
    assert!(
        a.hint_rows.last().is_some_and(|r| !r.hints.is_empty()),
        "{bar}"
    );
    let h = a
        .hint_rows
        .last()
        .and_then(|r| r.hints.first())
        .cloned()
        .unwrap();
    a.key(Key::Click {
        x: h.x as u16,
        y: h.y as u16,
    });
    assert!(
        a.take_injected().is_empty(),
        "a click under a card typed {:?}",
        h.bytes
    );
}

/// Text in a decision's command is the other party's: it carries no button, wherever it is drawn.
#[test]
fn text_inside_a_decision_is_never_a_hint() {
    let mut a = app();
    ask(
        &mut a,
        1,
        testing::requested("r1", "echo · esc closes · /quit"),
    );
    let screen = a.screen(160, 40);
    let drawn = screen
        .iter()
        .position(|l| l.contains("echo"))
        .expect("the command is drawn");
    assert!(
        a.hint_rows.get(drawn).is_none_or(|r| r.hints.is_empty()),
        "the decision's own words became a button: {:?}",
        a.hint_rows.get(drawn).map(|r| &r.hints)
    );
}

/// A pane's body is foreign text (a plugin, a room, a tool's output); only the head's own static
/// screens keep buttons in the body.
#[test]
fn a_panes_body_has_no_hints_and_the_help_screen_keeps_its_own() {
    let mut a = app();
    let _ = a.command("help");
    let _ = a.screen(120, 40);
    let body = a.body_rows;
    assert!(
        a.hint_rows.iter().take(body).any(|r| !r.hints.is_empty()),
        "/help lost its own buttons"
    );
    a.key(Key::Esc);
    a.key(Key::CtrlT);
    let _ = a.screen(120, 40);
    let body = a.body_rows;
    assert!(
        a.hint_rows.iter().take(body).all(|r| r.hints.is_empty()),
        "a pane body became buttons"
    );
}

/// Words held in the composer do not take the arrows off a permission card: the ladder is the
/// question on screen, and the words wait for it.
#[test]
fn the_arrows_move_a_permission_ladder_with_words_in_the_composer() {
    let mut a = app();
    typed(&mut a, "half a thought");
    ask(&mut a, 1, testing::requested("r1", "rm"));
    a.key(Key::Down);
    assert_eq!(a.sel, 1, "the arrow moved the ladder");
    a.key(Key::Up);
    assert_eq!(a.sel, 0);
    assert_eq!(
        a.editor.text(),
        "half a thought",
        "the words are still there"
    );
}

/// A ctrl-v on the live tail that outlived its work leaves no window on screen, so it holds no
/// arrows: a permission card that comes up after it moves its ladder.
#[test]
fn a_stale_live_tail_selection_does_not_take_the_arrows_from_a_permission_card() {
    let mut a = app();
    a.payload_sel = Some(crate::app::runclick::LIVE_RUN.to_string());
    ask(&mut a, 1, testing::requested("r1", "rm"));
    a.key(Key::Down);
    assert_eq!(
        a.sel, 1,
        "the arrow moved the ladder, not a window nobody sees"
    );
}
