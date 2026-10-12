//! **R90: keys, cards and popups agree** — each case a key sequence that used to send an answer,
//! a `/reseat` or a quit the operator did not choose, pinned so it cannot come back.

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

// ─────────────────────── R90.4: the highlight follows the front card ───────────────────────

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

// ───────────────── R90.3: a question over typed words does not take them ─────────────────

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

// ──────────────── R90.2 / R90.23: a row number marks a numbered row ────────────────

/// `16` typed as a free answer to "how many workers?" answered choice 1 at the `1`. A digit now
/// marks the row it names — and the numbers are drawn — and only Enter answers.
#[test]
fn a_digit_on_a_question_marks_its_numbered_row_and_enter_answers() {
    let mut a = app();
    ask(
        &mut a,
        1,
        testing::asked("q1", "how many workers?", &["2", "4", "8"], ""),
    );
    let card = a.decision_lines(&a.open[0].clone(), 80).join("\n");
    assert!(card.contains("▸ 1  2"), "{card}");
    assert!(card.contains("  3  8"), "{card}");
    assert!(card.contains("a row number marks"), "{card}");
    assert_eq!(a.key(Key::Char('3')), None, "a digit answers nothing");
    assert_eq!(a.sel, 2);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::AnswerQuestion {
            req_id: "q1".into(),
            answer: QuestionAnswer::choosing(2),
        })
    );
    // On a permission the same: the bar says so, and the rows carry their numbers.
    let mut b = app();
    ask(&mut b, 1, testing::requested("r1", "rm"));
    let card = b.decision_lines(&b.open[0].clone(), 80).join("\n");
    assert!(
        card.contains("▸ 1  Allow once") && card.contains("2  Deny"),
        "{card}"
    );
    assert!(b.hint_bar(200).contains("a row number marks"));
    assert_eq!(b.key(Key::Char('2')), None);
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
    assert!(card.contains("▸ 1  (an empty answer)"), "{card}");
}

// ───────────────────── R90.5: the prompt card has a way back ─────────────────────

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

// ─────────────── R90.6 / R90.30: the quit card and what it can be answered by ───────────────

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

// ──────────── R90.16 / R90.18 / R90.19: one order for the slot, the keys and the bar ────────────

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
    assert!(bar.contains("any other key cancels"), "{bar}");
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
    assert!(!bar.contains("ctrl-s"), "{bar}");
}

// ──────────────────── R90.8 / R90.10 / R90.11 / R90.17: the panes' bars ────────────────────

fn waiting_board(a: &mut App) {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoNeed, TodoStatus};
    a.session_id = "s".into();
    a.apply(ServerFrame::Todos {
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

/// Enter over a reading pane with a line typed is swallowed — and now said.
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

// ──────────── R90.12 / R90.13 / R90.14 / R90.27: todo cards follow the board ────────────

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

// ─────────────────────── R90.28 / R90.31: the popup and narrow widths ───────────────────────

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

// ──────────── R90.9 / R90.20 / R90.26 / R90.33: what waits, said and clickable ────────────

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
    let help = help_lines(&app().cfg, 200).join("\n");
    for words in [
        "alt-a brings",
        "alt-r reseats",
        "row number marks",
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

// ──────────── R90.29: an answered card closes at once, and comes back if refused ────────────

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
