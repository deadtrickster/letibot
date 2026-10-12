//! **A question card moves on after an answer, and steps away on Esc.**
//!
//! The operator, 2026-10-12, with three todo rows waiting on them: they answered one and the
//! resident line simply moved to the next question — *"i answered what was asked by the todo, and
//! it didnt hide"*. Then: *"yes, and i want to be able to escape the selection card. sometimes i
//! need time to think and just let the card stay"*.
//!
//! So: after a todo answer the card opens the next row still waiting and closes when none remain,
//! with a line saying which; and Esc on any question card — a todo row's or the model's
//! `ask_user_question` — puts it away with nothing sent, keeps what was typed on it, and leaves a
//! resident line saying it waits (with the model's deadline) and that alt-a brings it back.

use super::*;
use letibot_sessionlog::event::{OnTimeout, TodoBy, TodoEntry, TodoNeed, TodoStatus};
use letibot_sessionlog::question::QuestionAnswer;

/// A board whose rows each wait on the operator with a question of their own, in the session `s`
/// (the session [`env`]'s events are stamped with).
fn board(a: &mut App, rows: &[(&str, &str)]) {
    a.session_id = "s".into();
    a.apply(ServerFrame::Todos {
        session_id: "s".into(),
        todos: rows
            .iter()
            .map(|(content, question)| TodoEntry {
                content: (*content).into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: vec![TodoNeed::Operator {
                    question: (*question).into(),
                    options: vec!["yes".into(), "no".into()],
                }],
            })
            .collect(),
    });
}

const THREE: &[(&str, &str)] = &[
    ("pick the db", "postgres or sqlite?"),
    ("name the box", "call it staging?"),
    ("ship it", "ship on friday?"),
];

/// The model's `ask_user_question`, as the daemon poses it: a `question` decision with choices, a
/// deadline on the wire's epoch clock, and `OnTimeout::Deny` (the tool returns `not_run`).
fn model_question(a: &mut App, req_id: &str, deadline: Option<u64>) {
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::DecisionRequested {
            write_targets: Vec::new(),
            req_id: req_id.into(),
            kind: "question".into(),
            call_id: None,
            access: String::new(),
            summary: "which branch should I base this on?".into(),
            target: String::new(),
            detail: String::new(),
            options: Vec::new(),
            choices: vec!["main".into(), "release".into()],
            because: String::new(),
            advice: None,
            subagent: None,
            deadline,
            on_timeout: OnTimeout::Deny,
        },
    )));
}

fn notice(a: &App) -> String {
    a.notice.clone().unwrap_or_default()
}

fn line(a: &App) -> String {
    a.waiting_line(240).join("")
}

fn todo_answer(content: &str, question: &str, answer: QuestionAnswer) -> Option<Action> {
    Some(Action::AnswerTodo {
        content: content.into(),
        question: question.into(),
        answer,
    })
}

/// **Answer → the next waiting row's card opens; the last answer → the card closes and the line
/// says nothing waits.** *"i answered what was asked by the todo, and it didnt hide"* — the answer
/// went, and nothing on the screen said so.
#[test]
fn an_answer_opens_the_next_waiting_row_and_the_last_one_closes_the_card() {
    let mut a = app();
    board(&mut a, THREE);
    a.key(Key::AltA);
    assert_eq!(a.open[0].summary, "postgres or sqlite?");

    assert_eq!(
        a.key(Key::Enter),
        todo_answer(
            "pick the db",
            "postgres or sqlite?",
            QuestionAnswer::choosing(0)
        )
    );
    assert_eq!(
        a.open[0].summary, "call it staging?",
        "the NEXT row's card is up, before the board comes back"
    );
    assert_eq!(notice(&a), "answered — 2 more waiting on you");
    let l = line(&a);
    assert!(
        l.contains("\"call it staging?\"") && l.contains("(+1 more)"),
        "the resident line moved with the answer: {l}"
    );

    // A typed answer advances the same way.
    typed(&mut a, "no");
    assert_eq!(
        a.key(Key::Enter),
        todo_answer(
            "name the box",
            "call it staging?",
            QuestionAnswer::choosing(1)
        )
    );
    assert_eq!(a.open[0].summary, "ship on friday?");
    assert_eq!(notice(&a), "answered — 1 more waiting on you");

    assert!(a.key(Key::Enter).is_some());
    assert!(a.open.is_empty(), "nothing left: the card closes");
    assert_eq!(notice(&a), "answered — nothing else is waiting on you");
    assert!(
        line(&a).is_empty(),
        "and the resident line goes: {}",
        line(&a)
    );

    // The board coming back is the truth: the daemon swapped the edges, nothing waits.
    a.apply(ServerFrame::Todos {
        session_id: "s".into(),
        todos: Vec::new(),
    });
    assert!(line(&a).is_empty());
}

/// **The stale `asks` settings row does not bring the line back after the last answer** — it was
/// written before the answer, and the line fell back to it whenever the board read empty.
#[test]
fn the_last_answer_is_not_undone_by_the_settings_row() {
    let mut a = app();
    a.session_id = "s".into();
    a.apply(ServerFrame::Settings {
        rows: vec![letibot_sessionlog::protocol::SettingRow {
            key: "asks".into(),
            value: "1 — \"ship on friday?\" on `ship it`".into(),
            source: String::new(),
            editable: String::new(),
            choices: Vec::new(),
            tools: Vec::new(),
        }],
    });
    board(&mut a, &THREE[2..]);
    a.key(Key::AltA);
    assert!(a.key(Key::Enter).is_some());
    assert!(line(&a).is_empty(), "{}", line(&a));
}

/// **A refused answer is asked again**: the board that comes back is the truth, and a row still
/// waiting on it is still waiting.
#[test]
fn a_board_still_asking_after_an_answer_asks_again() {
    let mut a = app();
    board(&mut a, &THREE[..1]);
    a.key(Key::AltA);
    assert!(a.key(Key::Enter).is_some());
    assert!(line(&a).is_empty());
    board(&mut a, &THREE[..1]);
    assert!(line(&a).contains("postgres or sqlite?"), "{}", line(&a));
}

/// **Esc on a todo card: nothing sent, the line stays, alt-a brings it back with the draft.**
/// *"sometimes i need time to think and just let the card stay"*.
#[test]
fn esc_puts_a_todo_card_away_and_alt_a_brings_back_its_draft() {
    let mut a = app();
    board(&mut a, THREE);
    a.key(Key::AltA);
    a.key(Key::Down);
    typed(&mut a, "no -- not yet");
    assert_eq!(a.key(Key::Esc), None, "Esc sends nothing");
    assert!(a.open.is_empty(), "the card is off the screen");
    assert!(
        a.editor.text().is_empty(),
        "the draft left the composer with the card — Enter now must not send it to the model"
    );
    assert!(notice(&a).contains("nothing was sent"), "{}", notice(&a));
    let l = line(&a);
    assert!(
        l.contains("WAITING ON YOU") && l.contains("postgres or sqlite?"),
        "the row still waits, and the line says so: {l}"
    );
    assert_eq!(a.key(Key::Enter), None, "an empty composer sends nothing");

    a.key(Key::AltA);
    assert_eq!(a.open[0].summary, "postgres or sqlite?");
    assert_eq!(a.editor.text(), "no -- not yet", "the draft is back");
    assert_eq!(a.sel, 1, "and so is the highlight");
    assert_eq!(
        a.key(Key::Enter),
        todo_answer(
            "pick the db",
            "postgres or sqlite?",
            QuestionAnswer::choosing(1).with_note("not yet")
        )
    );
}

/// **The model's question put away keeps waiting under its own deadline, and the line counts it
/// down** — so the operator is not surprised when it lapses. alt-a brings it back with the draft.
#[test]
fn esc_puts_the_models_question_away_and_the_line_shows_its_deadline() {
    let mut a = app();
    a.session_id = "s".into();
    a.now_ms = 1_000_000;
    model_question(&mut a, "q1", Some(1_000_000 + 300_000));
    assert_eq!(a.open[0].kind, "question");
    typed(&mut a, "release -- the fix is there");
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.open.is_empty());
    assert!(a.editor.text().is_empty());
    assert_eq!(
        a.progress(),
        rano::term::Progress::Waiting,
        "put away is still waiting on the person"
    );
    let l = line(&a);
    assert!(l.contains("QUESTION PUT AWAY"), "{l}");
    assert!(l.contains("which branch should I base this on?"), "{l}");
    assert!(l.contains("5m00s left"), "the time left on its clock: {l}");
    assert!(
        l.contains("the model goes on without your answer"),
        "and what the lapse costs: {l}"
    );
    assert!(l.contains("alt-a brings it back"), "{l}");
    a.now_ms += 61_000;
    assert!(line(&a).contains("3m59s left"), "{}", line(&a));

    a.key(Key::AltA);
    assert_eq!(a.open[0].req_id, "q1");
    assert_eq!(a.editor.text(), "release -- the fix is there");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::AnswerQuestion {
            req_id: "q1".into(),
            answer: QuestionAnswer::choosing(1).with_note("the fix is there"),
        })
    );
}

/// **The question lapsing while away takes the line down** — it is settled, nothing is left to
/// bring back.
#[test]
fn a_question_settled_while_away_is_gone() {
    let mut a = app();
    a.session_id = "s".into();
    model_question(&mut a, "q1", Some(10));
    a.key(Key::Esc);
    assert_eq!(a.away.len(), 1);
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::DecisionAnswered {
            req_id: "q1".into(),
            outcome: letibot_sessionlog::event::DecisionOutcome::TimedOut,
            by: letibot_sessionlog::event::Decider {
                kind: "gate".into(),
                identity: "timeout".into(),
            },
            basis: "nobody answered".into(),
            late: false,
        },
    )));
    assert!(a.away.is_empty() && a.open.is_empty());
    assert!(line(&a).is_empty(), "{}", line(&a));
}

/// **The Esc that puts the card away is not the first half of esc-esc.** A busy turn (the model's
/// call is waiting on the question) is interrupted by two presses of Esc; the press that stepped
/// away from the card must not count, or a second Esc "to be sure" kills the turn.
#[test]
fn the_put_away_esc_does_not_arm_the_interrupt() {
    let mut a = app();
    a.session_id = "s".into();
    a.apply(ServerFrame::Event(env(0, testing::turn_started("t1"))));
    model_question(&mut a, "q1", None);
    assert_eq!(a.key(Key::Esc), None, "puts the card away");
    assert!(
        !matches!(a.key(Key::Esc), Some(Action::Interrupt(_))),
        "the second press only arms"
    );
    assert!(
        matches!(a.key(Key::Esc), Some(Action::Interrupt(_))),
        "and esc-esc after it still interrupts — the premise: the turn is busy"
    );
    assert!(
        line(&a).contains("QUESTION PUT AWAY: \"which branch should I base this on?\" — alt-a"),
        "no deadline, no clock: {}",
        line(&a)
    );
}

/// **Abstain stays an explicit choice** — typed, and sent as one, from both kinds of card; Esc is
/// never read as it.
#[test]
fn abstain_still_sends_abstain() {
    let mut a = app();
    board(&mut a, &THREE[..1]);
    a.key(Key::AltA);
    a.key(Key::Esc);
    a.key(Key::AltA);
    typed(&mut a, "abstain");
    assert_eq!(
        a.key(Key::Enter),
        todo_answer(
            "pick the db",
            "postgres or sqlite?",
            QuestionAnswer::abstaining()
        )
    );

    let mut b = app();
    b.session_id = "s".into();
    model_question(&mut b, "q1", None);
    b.key(Key::Esc);
    b.key(Key::AltA);
    typed(&mut b, "abstain");
    assert_eq!(
        b.key(Key::Enter),
        Some(Action::AnswerQuestion {
            req_id: "q1".into(),
            answer: QuestionAnswer::abstaining(),
        })
    );
}

/// **alt-a brings back the oldest pending question of either kind** — the model's first (the one
/// with a clock and a call held on it), then a todo card that was put away (with its draft), and
/// only then the first waiting row in board order.
#[test]
fn alt_a_brings_back_the_models_question_first_then_a_put_away_row() {
    let mut a = app();
    board(&mut a, THREE);
    model_question(&mut a, "q1", None);
    a.key(Key::Esc);
    let l = line(&a);
    assert!(
        l.contains("QUESTION PUT AWAY") && l.contains("(+3 todo questions)"),
        "{l}"
    );
    // The second row's card, put away with words on it.
    a.open_operator_ask(Some("name the box"));
    typed(&mut a, "yes");
    a.key(Key::Esc);
    assert_eq!(a.away.len(), 2);

    a.key(Key::AltA);
    assert_eq!(a.open[0].req_id, "q1", "the model's question first");
    assert!(a.key(Key::Enter).is_some());
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::DecisionAnswered {
            req_id: "q1".into(),
            outcome: letibot_sessionlog::event::DecisionOutcome::Cancelled,
            by: letibot_sessionlog::event::Decider {
                kind: "human".into(),
                identity: String::new(),
            },
            basis: "main".into(),
            late: false,
        },
    )));
    assert!(a.open.is_empty());

    a.key(Key::AltA);
    assert_eq!(
        a.open[0].summary, "call it staging?",
        "the row put away comes back before the first row in board order"
    );
    assert_eq!(a.editor.text(), "yes");
}

/// **A permission card is put away by Esc too** (R90.5) — its Esc used to fall through, close a
/// pane behind it or arm the Esc-Esc interrupt — and alt-a brings it back.
#[test]
fn esc_puts_a_permission_card_away_and_alt_a_brings_it_back() {
    let mut a = app();
    a.session_id = "s".into();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(2, testing::requested("r1", "rm"))));
    a.key(Key::Down);
    a.clock(1_000);
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.open.is_empty());
    assert_eq!(a.away.len(), 1);
    assert!(line(&a).contains("PERMISSION PUT AWAY"), "{}", line(&a));
    a.clock(1_200);
    assert_eq!(a.key(Key::Esc), None);
    assert!(
        !a.take_actions()
            .iter()
            .any(|x| matches!(x, Action::Interrupt(_))),
        "the put-away Esc armed the interrupt"
    );
    a.key(Key::AltA);
    assert_eq!(a.open[0].req_id, "r1");
    assert_eq!(a.sel, 1, "with the row it was left on");
    let card = a.decision_lines(&a.open[0].clone(), 120).join("\n");
    assert!(card.contains("esc puts it away"), "{card}");
}

/// **The cards and the hint bar say how to step away** — `esc puts it away — alt-a brings it back`.
#[test]
fn the_question_card_and_its_bar_name_esc_and_alt_a() {
    let mut a = app();
    a.session_id = "s".into();
    model_question(&mut a, "q1", None);
    let card = a.decision_lines(&a.open[0].clone(), 120).join("\n");
    assert!(
        card.contains("esc puts it away — alt-a brings it back"),
        "{card}"
    );
    let screen = a.screen(160, 40).join("\n");
    assert!(
        screen.contains("esc puts it away — alt-a brings it back"),
        "{screen}"
    );
}
