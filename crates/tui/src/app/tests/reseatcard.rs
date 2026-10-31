//! **The reseat card: a select that stays on the screen until the prompt is current.**
//!
//! The operator, 2026-10-12: *"regarding reseat - since it is important id suggest to show me
//! "select" with one option - reseat and abstain and then when on abstain keep reseat select
//! open. this way i keep reseat in mind because it stares at me yet can continue at my risk"*.
//!
//! So a `prefix_stale` raises a card with exactly two rows — reseat now, not now — whose first
//! row is the bare `/reseat` as typing it would send it; `not now` (and Esc, and typing) pins the
//! card as a strip above the composer that stays while the operator works; alt-p opens it again,
//! alt-r reseats from anywhere; and only the daemon's word that the prompt is current takes it
//! down.

use super::*;

const SAID: &str = "session s was seated 10-08 21:14 with a prompt this daemon would not \
                    compose now: the tool schemas have changed since. This session keeps the \
                    prefix it was created with — message 0 is frozen, and rewriting it is a \
                    full cold re-prefill — so the model cannot call a tool seated since, and a \
                    schema that grew is one it has never been told about. Seated here but not \
                    in this conversation's prompt, so the model cannot call them: task_start, \
                    web_fetch. `/reseat` rebuilds the prompt from what is seated now, carrying \
                    this conversation across as it is; a new session gets the seated one from \
                    the start.";

fn warned(hub: &Hub, code: &str, detail: &str) -> ServerFrame {
    ServerFrame::Event(hub.publish(SessionEvent::Warning {
        code: code.into(),
        detail: detail.into(),
        compaction: None,
    }))
}

/// A head attached to `s`, with a turn behind it so the screen is a conversation.
fn attached() -> (App, std::sync::Arc<Hub>) {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    (a, hub)
}

fn stale() -> (App, std::sync::Arc<Hub>) {
    let (mut a, hub) = attached();
    a.apply(warned(&hub, "prefix_stale", SAID));
    (a, hub)
}

fn screen(a: &mut App) -> String {
    a.screen(120, 40).join("\n")
}

#[test]
fn a_stale_prompt_raises_a_card_with_exactly_the_two_choices() {
    let (mut a, _hub) = stale();
    assert!(a.reseat_card_up(), "the card is open in the slot");
    let s = screen(&mut a);
    assert!(s.contains("PROMPT IS STALE"), "{s}");
    assert!(s.contains(&format!("▸  1  {}", reseat::RESEAT_NOW)), "{s}");
    assert!(s.contains(&format!("2  {}", reseat::NOT_NOW)), "{s}");
    assert!(
        !s.contains("3  "),
        "two rows and no third — the card is a choice between these two: {s}"
    );
    // The short form: what changed and what was gained, not the paragraph on frozen prompts.
    // Read off the card itself — the `prefix_stale` note row in the conversation is the whole
    // sentence, as it always was.
    let card = |a: &App| {
        let (content, choices) = a.reseat_card_lines(120);
        [content, choices].concat().join("\n")
    };
    let c = card(&a);
    assert!(c.contains("the tool schemas have changed since"), "{c}");
    assert!(c.contains("task_start"), "{c}");
    assert!(
        !c.contains("full cold re-prefill"),
        "the explanation is one tab away: {c}"
    );
    assert!(c.contains("tab shows all"), "and the card says so: {c}");
    // Tab shows the whole sentence, and again hides it.
    assert_eq!(a.key(Key::Tab), None);
    assert!(card(&a).contains("full cold re-prefill"));
    assert_eq!(a.key(Key::Tab), None);
    assert!(!card(&a).contains("full cold re-prefill"));
}

#[test]
fn reseat_now_sends_the_bare_reseat_exactly_as_typing_it_would() {
    let (mut a, _hub) = stale();
    let typed_action = {
        let mut b = app();
        b.session_id = "s".into();
        typed(&mut b, "/reseat");
        b.key(Key::Enter)
    };
    assert_eq!(typed_action, Some(Action::Reseat { summarise: false }));
    assert_eq!(
        a.key(Key::Enter),
        typed_action,
        "Enter on the untouched card"
    );
    // Waiting for the daemon: pinned, saying so, and not sent twice.
    assert!(!a.reseat_card_up());
    // This fixture has a turn running, so the strip says the reseat waits for it.
    let s = screen(&mut a);
    assert!(
        s.contains("reseat queued") || s.contains("reseat asked for"),
        "{s}"
    );
    assert_eq!(
        a.key(Key::AltR),
        None,
        "a reseat already on its way is not sent again"
    );

    // **The digit is a move, as on every card** (R90.6): `1` typed as the first character of a
    // line sent `/reseat` and forked the conversation, so a row number now marks the row and
    // Enter answers it — the rule R90.2 put on the decision card, applied to this one.
    let (mut a, _hub) = stale();
    assert_eq!(a.key(Key::Char('1')), None, "the digit marks");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Reseat { summarise: false })
    );
}

#[test]
fn not_now_pins_the_card_and_the_composer_keeps_working() {
    let (mut a, _hub) = stale();
    assert_eq!(a.key(Key::Down), None);
    assert_eq!(a.key(Key::Enter), None, "not now sends nothing");
    assert!(a.reseat.is_some(), "abstaining does not dismiss it");
    assert!(!a.reseat_card_up(), "it is pinned, not open");
    let s = screen(&mut a);
    let strip = s
        .lines()
        .find(|l| l.contains("PROMPT IS STALE"))
        .unwrap_or_else(|| panic!("the strip stays on the screen: {s}"));
    assert!(strip.contains("alt-r reseats now"), "{strip}");
    assert!(strip.contains("alt-p shows the card"), "{strip}");

    // Typing goes to the composer and Enter sends a prompt, with the strip still there.
    typed(&mut a, "carry on");
    assert_eq!(a.input(), "carry on");
    assert_eq!(a.key(Key::Enter), Some(Action::Prompt("carry on".into())));
    assert!(screen(&mut a).contains("PROMPT IS STALE"), "still staring");

    // And it still offers the reseat, from the strip's key.
    assert_eq!(a.key(Key::AltR), Some(Action::Reseat { summarise: false }));
}

#[test]
fn the_pinned_card_opens_again_by_key_and_by_click() {
    let (mut a, _hub) = stale();
    assert_eq!(a.key(Key::Esc), None, "Esc is not now");
    assert!(a.reseat.is_some() && !a.reseat_card_up());
    assert_eq!(a.key(Key::AltP), None);
    assert!(a.reseat_card_up(), "alt-p opens the card again");
    assert!(screen(&mut a).contains(reseat::RESEAT_NOW));

    // The strip's `alt-p shows the card` is a button: a click types the key.
    a.key(Key::Esc);
    let frame = a.screen(120, 40);
    let (y, row) = frame
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("PROMPT IS STALE"))
        .expect("the strip");
    let x = keyhints::cells(row)
        .iter()
        .collect::<String>()
        .find("alt-p")
        .expect("the hint on the strip");
    assert_eq!(
        a.key(Key::Click {
            x: x as u16,
            y: y as u16
        }),
        None
    );
    let typed_bytes = a.take_injected();
    assert_eq!(typed_bytes, b"\x1bp".to_vec(), "the click is alt-p, typed");
    let x = keyhints::cells(row)
        .iter()
        .collect::<String>()
        .find("alt-r")
        .expect("the reseat hint on the strip");
    a.key(Key::Click {
        x: x as u16,
        y: y as u16,
    });
    assert_eq!(
        a.take_injected(),
        b"\x1br".to_vec(),
        "the click is alt-r, typed"
    );
}

#[test]
fn typing_on_the_open_card_pins_it_and_the_key_reaches_the_composer() {
    let (mut a, _hub) = stale();
    typed(&mut a, "hello");
    assert_eq!(a.input(), "hello", "not one key swallowed");
    assert!(a.reseat.is_some() && !a.reseat_card_up());
    // A stale sentence arriving while the operator has words in the composer is pinned, so it
    // cannot take their Enter.
    let (mut a, hub) = attached();
    typed(&mut a, "half a line");
    a.apply(warned(&hub, "prefix_stale", SAID));
    assert!(a.reseat.is_some() && !a.reseat_card_up());
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Prompt("half a line".into()))
    );
}

#[test]
fn the_card_waits_behind_a_question_and_is_the_strip_meanwhile() {
    let (mut a, _hub) = stale();
    a.open.push(decision_with(&[
        letibot_sessionlog::event::OptionKind::AllowOnce,
        letibot_sessionlog::event::OptionKind::RejectOnce,
    ]));
    assert!(!a.reseat_card_up(), "the decision holds the slot");
    assert!(
        screen(&mut a).contains("PROMPT IS STALE"),
        "and the strip says so"
    );
    a.open.clear();
    assert!(
        a.reseat_card_up(),
        "the card comes back when the slot is free"
    );
}

#[test]
fn the_card_comes_down_only_when_the_daemon_says_the_prompt_is_current() {
    let (mut a, hub) = stale();
    a.key(Key::Esc);
    // A second head attaching makes the daemon say it again: this head's pinned card stays
    // pinned rather than popping open.
    a.apply(warned(&hub, "prefix_stale", SAID));
    assert!(a.reseat.is_some() && !a.reseat_card_up());
    let before = a.notes.len();
    a.apply(warned(
        &hub,
        "prefix_current",
        letibot_sessionlog::registry::PREFIX_CURRENT,
    ));
    assert!(a.reseat.is_none(), "cleared");
    assert!(!screen(&mut a).contains("PROMPT IS STALE"));
    assert_eq!(
        a.notes.len(),
        before,
        "the clear is the card's signal, not a row"
    );
    assert_eq!(a.key(Key::AltR), None, "nothing to reseat any more");

    // A daemon that only says `reseated` takes it down too.
    let (mut a, hub) = stale();
    a.apply(warned(&hub, "reseated", "re-seated"));
    assert!(a.reseat.is_none());
}

#[test]
fn a_refused_reseat_lets_the_strip_send_again() {
    let (mut a, hub) = stale();
    assert!(a.key(Key::AltR).is_some());
    a.apply(warned(&hub, "reseat_refused", "nothing to re-seat"));
    assert!(a.reseat.is_some(), "still stale");
    assert_eq!(a.key(Key::AltR), Some(Action::Reseat { summarise: false }));
}

/// **A reattach after a restart**: the `Hello` forgets the card, and the daemon's `prefix_stale`
/// right after it raises it again, open — or, when the restarted daemon found the prompt
/// current, nothing does and the card is gone.
#[test]
fn an_attach_after_a_restart_shows_it_again_from_the_daemons_word() {
    let (mut a, hub) = stale();
    a.key(Key::Esc);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    assert!(a.reseat.is_none(), "the seating forgets it");
    a.apply(warned(&hub, "prefix_stale", SAID));
    assert!(a.reseat_card_up(), "and the daemon's word raises it, open");

    let (mut a, hub) = stale();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    assert!(
        a.reseat.is_none(),
        "a daemon that says nothing has a current prompt"
    );
}

#[test]
fn a_click_on_a_row_of_the_open_card_selects_it() {
    let (mut a, _hub) = stale();
    let frame = a.screen(120, 40);
    let y = frame
        .iter()
        .position(|l| l.contains(reseat::NOT_NOW))
        .expect("the second row");
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    assert_eq!(a.reseat.as_ref().map(|c| c.sel), Some(1));
    assert_eq!(a.key(Key::Enter), None, "select and confirm stay two acts");
    assert!(a.reseat.is_some() && !a.reseat_card_up());
}

#[test]
fn the_short_form_keeps_what_changed_and_drops_the_explanation() {
    let short = crate::ui::cards::reseat::short_form(SAID);
    assert!(short.starts_with("session s was seated"), "{short}");
    assert!(short.contains("task_start, web_fetch"), "{short}");
    assert!(!short.contains("frozen"), "{short}");
    assert!(
        !short.contains("`/reseat`"),
        "the card is the remedy: {short}"
    );
}

/// The daemon's refusal when the conversation no longer fits verbatim, as it says it.
const TOO_BIG: &str = "carrying this conversation verbatim would put 901221 token(s) in front of \
                       a 1000000-token window, leaving less than the 214420 a turn needs. \
                       Re-seating without summarising only works while the conversation still \
                       fits; `/reseat summarise` summarises and fits, and `/compact` does the \
                       same without changing the prompt. Nothing was changed.";

/// **A reseat that cannot fit verbatim offers the one that can** — the operator pasted the card
/// offering `alt-r reseats now` under the daemon's refusal that a verbatim reseat does not fit:
/// the choice it stared at them with could not be taken. After that refusal the card's first
/// row is a reseat with a summary, alt-r sends that, and the card opens again to say so.
#[test]
fn a_reseat_too_big_to_carry_verbatim_offers_one_with_a_summary() {
    let (mut a, hub) = stale();
    assert_eq!(a.key(Key::AltR), Some(Action::Reseat { summarise: false }));
    a.apply(warned(&hub, "reseat_refused", TOO_BIG));
    assert!(
        a.reseat_card_up(),
        "the card opens again with the choice that can work"
    );
    let s = screen(&mut a);
    assert!(
        s.contains(&format!("▸  1  {}", reseat::RESEAT_SUMMARISE)),
        "{s}"
    );
    assert!(
        !s.contains(reseat::RESEAT_NOW),
        "the one that cannot work is gone: {s}"
    );
    a.key(Key::Esc);
    let s = screen(&mut a);
    assert!(s.contains("alt-r reseats with a summary"), "{s}");
    assert_eq!(a.key(Key::AltR), Some(Action::Reseat { summarise: true }));
}

/// **R90.1: a pane's keys are the pane's.** With the card up, ctrl-t then Enter on a row, `1` in
/// the session picker, Tab and Esc in todos all acted on the card — Enter and `1` sent `/reseat`.
/// While a pane has the keyboard the card is the strip, and it is back when the pane closes.
#[test]
fn an_open_pane_takes_the_slot_and_its_keys_never_reach_the_card() {
    let (mut a, _hub) = stale();
    assert!(a.reseat_card_up());
    a.key(Key::CtrlT);
    assert_eq!(a.top_surface(), Some(Surface::Todos));
    assert!(!a.reseat_card_up(), "the pane holds the slot");
    assert!(screen(&mut a).contains("alt-p shows the card"), "the strip");
    for k in [Key::Down, Key::Enter, Key::Char('1'), Key::Tab] {
        let r = a.key(k.clone());
        assert!(
            !matches!(r, Some(Action::Reseat { .. })),
            "{k:?} in the todos pane reseated"
        );
    }
    let whole = a.reseat.as_ref().is_some_and(|c| c.whole);
    assert!(!whole, "Tab was the pane's, not the card's");
    // alt-p over the pane says why nothing opens rather than popping the card later.
    a.key(Key::AltP);
    assert!(
        a.notice.as_deref().unwrap_or("").contains("waits behind"),
        "{:?}",
        a.notice
    );
    while a.top_surface().is_some() {
        a.key(Key::Esc);
    }
    assert!(
        a.reseat.is_some(),
        "Esc closed the pane, it did not touch the card"
    );

    // The session picker: `1` is the first session, never the card's first row.
    let (mut a, _hub) = stale();
    a.key(Key::CtrlS);
    assert_eq!(a.top_surface(), Some(Surface::Picker));
    assert!(!matches!(
        a.key(Key::Char('1')),
        Some(Action::Reseat { .. })
    ));
}

/// **A digit on the reseat card moves the mark; it does not reseat** (R90.6). The digit chose at
/// once, so `1` typed as the first character of a line sent `/reseat` and forked the conversation
/// while the card's own line said *"enter chooses"*. The card's other rule — a typed line pins it
/// and goes to the composer — is what the words after that digit get.
#[test]
fn a_digit_on_the_reseat_card_marks_the_row_and_reseats_nothing() {
    let (mut a, _hub) = stale();
    assert!(a.reseat_card_up());
    assert_eq!(a.key(Key::Char('2')), None, "a move, not an answer");
    assert_eq!(
        a.reseat.as_ref().expect("the card").sel,
        1,
        "the second row is marked"
    );
    assert!(a.reseat_card_up(), "the card is still open");
    assert_eq!(a.input(), "", "and nothing was typed");
    // Enter is still the answer, and it answers the row the digit marked.
    assert_eq!(a.key(Key::Enter), None);
    assert!(!a.reseat.as_ref().expect("the card").open, "not now pins it");

    // A line that starts with a digit is the operator's: the digit marks, the rest is theirs.
    let (mut b, _hub) = stale();
    assert_eq!(b.key(Key::Char('1')), None);
    typed(&mut b, ". check the logs first");
    assert!(!b.reseat_card_up(), "the typed line pins the card");
    assert!(
        b.input().ends_with("check the logs first"),
        "{:?}",
        b.input()
    );
}

/// **R90.22: the card that says "pgdn scrolls" scrolls on PgDn** — `scroll_card` asked for an open
/// decision, so the page keys scrolled the transcript under the reseat card instead.
#[test]
fn the_page_keys_scroll_the_reseat_card_that_says_they_do() {
    let (mut a, _hub) = stale();
    a.key(Key::Tab);
    let before = a.screen(50, 14).join("\n");
    assert!(before.contains("pgdn scrolls"), "{before}");
    a.key(Key::PageDown);
    let after = a.screen(50, 14).join("\n");
    assert!(a.dec_scroll > 0, "the card's window moved");
    assert!(after.contains("pgup"), "{after}");
}

/// **R90.31: the reseat rows wrap rather than being cut** — the 79-character summarise row lost
/// its reason on an ordinary terminal; a click on its second line still chooses it.
#[test]
fn the_reseat_rows_wrap_on_a_narrow_screen() {
    let (mut a, _hub) = stale();
    a.reseat_refused("does not fit — `/reseat summarise` carries a summary instead");
    let card = a.reseat_choice_lines(50);
    assert!(card[0].len() > 1, "{card:?}");
    let joined = card[0].join(" ");
    assert!(joined.contains("verbatim"), "{joined}");
    for l in card.iter().flatten() {
        assert!(rano::width::text::width(l) <= 50, "{l}");
    }
}

/// **A reseat asked for during a turn says it waits for the turn** — the operator: *"PROMPT IS
/// STALE yellow thing doesnt go away even when i answered reseat … maybe it will go away when the
/// turn ends"*. It does: the daemon runs a reseat on its queue, between turns, never inside one.
/// The strip and the second ask now say so rather than *"waiting for the daemon"*.
#[test]
fn a_reseat_asked_for_during_a_turn_says_it_runs_when_the_turn_ends() {
    let (mut a, hub) = stale();
    a.apply(ServerFrame::Event(env_at(
        50,
        2_000,
        testing::turn_started("t2"),
    )));
    assert!(a.turn.is_some(), "the premise: a turn is running");
    assert!(a.key(Key::AltR).is_some());
    let s = screen(&mut a);
    assert!(s.contains("runs when this turn ends"), "{s}");
    a.key(Key::AltR);
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("when this turn ends"), "{said}");
    let _ = hub;
}
