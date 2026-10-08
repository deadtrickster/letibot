//! The cards that ask for something typed: a key, a secret, a prompt, a terminal answer.

use super::*;

/// **A question from the run takes the screen back from the confirmation.**
///
/// The operator's other rule: the two questions must not be confusable, and *neither may be
/// answerable by the other's keystroke*. They are exclusive by construction everywhere else —
/// the confirmation is typed at the composer and the prompt card owns the composer while it
/// is up — but a `PromptRequested` can arrive *while* the confirmation is standing, and then
/// `y` would mean two things: this card's yes and that card's text. So the confirmation
/// yields, with a sentence, and the program it was about is still running.
#[test]
fn a_question_from_the_run_takes_the_screen_back_from_the_confirmation() {
    let mut a = app();
    a.session_id = "s".into();
    a.head_id = "h1".into();
    let _ = a.screen(80, 24);
    pane(&mut a, b"nano's screen\r\n");
    a.detach();
    typed(&mut a, "!term close");
    a.key(Key::Enter);
    assert!(a.term_ask.is_some());

    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::PromptRequested {
            req_id: "r1".into(),
            job: "j1".into(),
            command: "! sudo apt install mc".into(),
            question: Some("Continue? [Y/n]".into()),
            reading: letibot_sessionlog::PromptReading::Blocked,
        },
    )));
    assert!(a.term_ask.is_none(), "the confirmation yielded");
    assert!(a.prompt.is_some(), "and the program's question is up");
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("still running")),
        "the yield is said rather than silent: {:?}",
        a.notice
    );
    // **And `y` is the program's answer, not a kill**: the confirmation is gone, so the key
    // goes where the card that IS up says it goes.
    assert_eq!(a.key(Key::Char('y')), None);
    assert_eq!(a.prompt_buf, "y", "the letter reached the prompt card");
    assert!(a.term.is_some(), "nothing was ended");
}

/// The sentence the model writes before its first tool call is on the screen
/// once, before and after its row lands.
///
/// `TurnPane::text` accumulates every text delta of the whole turn, and prose
/// is committed to the transcript one round at a time — so once round one's
/// row had a body, its sentence was in history and still in the pane below the
/// cards. Measured at 60x34 on the operator's session.
/// **The ask is a card, and it says the thing once.**
///
/// The operator, having just answered one: *"the ask is ugly as hell"*. Three faults,
/// and this pins one assertion against each: the headline said `sudo wants a password
/// — [sudo] password for dead:` — the same fact twice, in the same weight, ending in
/// a colon — the command sat in the body register under a `for:` that named nothing,
/// and the hint ran past a hundred columns so `trim_to` cut it from the end, which is
/// exactly where the countdown lives.
/// **The daemon's key ask is the same card, worded for a key.** It arrives as a secret
/// request with no command (`Harness::obtain_key`), so the headline says a key is needed,
/// there is no `run:` row naming a command that does not exist, the field is masked like
/// a password, and a refusal is noted as the operator's — not as sudo's red line.
#[test]
fn the_key_ask_is_the_secret_card_worded_for_a_key() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::SecretRequested {
            req_id: "secret-s-1".into(),
            prompt: "deepseek needs an API key. It is saved to /h/.config/letibot/providers.toml"
                .into(),
            command: String::new(),
            deadline: 1_000_000,
        },
    )));
    let card = a.screen(100, 20).join("\n");
    assert!(card.contains("? an API key is needed"), "{card}");
    assert!(card.contains("deepseek needs an API key"), "{card}");
    assert!(
        !card.contains("sudo"),
        "a key ask must not read as sudo's: {card}"
    );
    assert!(
        !card.contains("run:"),
        "there is no command to name: {card}"
    );
    for c in "sk-abc".chars() {
        assert!(a.key(Key::Char(c)).is_none());
    }
    let typing = a.screen(100, 20).join("\n");
    assert!(
        !typing.contains("sk-abc"),
        "the key is on the screen:\n{typing}"
    );
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Secret {
            req_id: "secret-s-1".into(),
            secret: Some("sk-abc".into()),
        })
    );
    // A second ask, refused: noted as the operator's refusal of a key.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::SecretRequested {
            req_id: "secret-s-2".into(),
            prompt: "deepseek needs an API key.".into(),
            command: String::new(),
            deadline: 1_000_000,
        },
    )));
    assert_eq!(
        a.key(Key::Esc),
        Some(Action::Secret {
            req_id: "secret-s-2".into(),
            secret: None,
        })
    );
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::SecretSettled {
            req_id: "secret-s-2".into(),
            given: false,
            by: "dead".into(),
        },
    )));
    let after = a.screen(100, 20).join("\n");
    assert!(after.contains("no key given"), "{after}");
    assert!(!after.contains("no password given"), "{after}");
}

#[test]
fn the_password_card_is_a_card_and_says_it_once() {
    let mut a = app();
    a.clock(1_788_984_000_000);
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::SecretRequested {
            req_id: "secret-s-1".into(),
            prompt: "[sudo] password for dead: ".into(),
            command: "sudo apt install x".into(),
            deadline: 1_788_984_047_000,
        },
    )));
    let screen = a.screen(80, 24).join("\n");
    let headline = screen
        .lines()
        .find(|l| l.contains("sudo wants a password"))
        .expect("the card is drawn");
    assert!(headline.contains("? "), "no marker: {headline:?}");
    assert_eq!(
        screen.matches("sudo wants a password").count(),
        1,
        "the card says it twice — which was the worst of it:\n{screen}"
    );
    // sudo's own words, and the command as the thing being authorised.
    assert!(screen.contains("[sudo] password for dead:"), "{screen}");
    assert!(
        screen.contains("run: sudo apt install x"),
        "the command is not named as a command:\n{screen}"
    );
    // **The keys survive a narrow terminal along with the countdown** — they are the
    // way out, and the countdown is the only thing on the card that moves.
    for w in [44usize, 60, 80, 200] {
        let rows = a.secret_lines(a.secret.as_ref().expect("the ask"), w);
        let last = rows.last().expect("a hint row");
        assert!(line_width(last) <= w, "w={w}: {last:?}");
        assert!(
            last.contains("enter sends it") && last.contains("esc refuses"),
            "w={w}: the keys were cut: {last:?}"
        );
        assert!(
            last.contains("left"),
            "w={w}: the countdown was cut: {last:?}"
        );
    }
}

/// `sudo` wants a password: the card names the command, the keys are the
/// field's alone, the screen shows dots and never the text, Enter sends it
/// once as an `Action::Secret`, and the composer's history never had it.
#[test]
fn a_password_field_owns_the_keys_shows_dots_and_never_reaches_the_composer() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::SecretRequested {
            req_id: "secret-s-1".into(),
            prompt: "[sudo] password for dead: ".into(),
            command: "sudo apt install x".into(),
            deadline: 1_000_000,
        },
    )));
    let card = a.screen(100, 20).join("\n");
    assert!(card.contains("sudo wants a password"), "{card}");
    assert!(card.contains("sudo apt install x"), "{card}");
    for c in "hunter2".chars() {
        assert!(a.key(Key::Char(c)).is_none());
    }
    let typing = a.screen(100, 20).join("\n");
    assert!(
        !typing.contains("hunter2"),
        "the password is on the screen:\n{typing}"
    );
    assert!(
        typing.contains("\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}"),
        "{typing}"
    );
    assert!(a.input().is_empty(), "the composer must never hold it");
    assert!(a.key(Key::Backspace).is_none());
    assert!(a.key(Key::Char('2')).is_none());
    let sent = a.key(Key::Enter);
    assert_eq!(
        sent,
        Some(Action::Secret {
            req_id: "secret-s-1".into(),
            secret: Some("hunter2".into()),
        })
    );
    assert!(a.input().is_empty());
    assert!(a.editor.history().iter().all(|h| !h.contains("hunter2")));
    let after = a.screen(100, 20).join("\n");
    assert!(!after.contains("sudo wants a password"), "{after}");

    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::SecretRequested {
            req_id: "secret-s-2".into(),
            prompt: "[sudo] password: ".into(),
            command: "sudo true".into(),
            deadline: 1_000_000,
        },
    )));
    assert_eq!(
        a.key(Key::Esc),
        Some(Action::Secret {
            req_id: "secret-s-2".into(),
            secret: None,
        })
    );
}

/// **A card raised WITHOUT a reading does not claim the program asked.**
///
/// The two cards are the two facts, and the operator's measured run is why the second one
/// exists at all (2026-10-09, `! sudo apt install mc`: the password taken, `apt` at
/// `Continue? [Y/n]` as root, `/proc/<pid>/fd/0` `EACCES` for the daemon). The reading-backed
/// card's headline is *your command is asking*; a card that carried that headline on a run
/// nobody could look at would be a guess — and a wrong one for every long quiet command that
/// is asking nothing. So this asserts the distinction the row is judged on: the text says it
/// could not look, and it still offers the way in, because a line sent goes into the run's
/// input either way.
#[test]
fn an_unreadable_card_says_it_could_not_look_and_offers_the_way_in_anyway() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::PromptRequested {
            req_id: "prompt-s-9".into(),
            job: "j9".into(),
            command: "sudo apt install mc".into(),
            question: None,
            reading: letibot_sessionlog::PromptReading::Unreadable,
        },
    )));
    let card = a.screen(120, 20).join("\n");
    assert!(
        !card.contains("your command is asking"),
        "a card raised without a reading must not claim the program asked: {card}"
    );
    assert!(
        card.contains("could not read"),
        "it says which of the two facts it is: {card}"
    );
    assert!(card.contains("run: sudo apt install mc"), "{card}");
    assert!(
        card.contains("either way"),
        "and that a line sent goes into the run regardless: {card}"
    );
    // The floor under both cards: the verb that needs no reading at all, and the job a
    // person can read with `job_output`.
    assert!(card.contains("!send"), "{card}");
    assert!(card.contains("j9"), "{card}");
    // And it is a card you can still answer — that is the whole point of it being a card.
    for c in "y".chars() {
        assert!(a.key(Key::Char(c)).is_none());
    }
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::PromptAnswer {
            req_id: "prompt-s-9".into(),
            line: "y".into(),
        })
    );
}

/// **The prompt card: a command of the operator's own is asking, and the field is NOT
/// masked.**
///
/// The whole of the difference from the password card, asserted rather than argued: the
/// text is drawn as typed, the composer never holds it, Enter sends it once as an
/// [`Action::PromptAnswer`] — and it never becomes an [`Action::Secret`], which is the
/// head's half of *a secret cannot be routed through the prompt card*.
#[test]
fn a_prompt_card_owns_the_keys_shows_what_is_typed_and_never_becomes_a_secret() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::PromptRequested {
            req_id: "prompt-s-1".into(),
            job: "j7".into(),
            command: "sudo apt install mc".into(),
            question: Some("Do you want to continue? [Y/n]".into()),
            reading: letibot_sessionlog::PromptReading::Blocked,
        },
    )));
    let card = a.screen(100, 20).join("\n");
    assert!(card.contains("your command is asking"), "{card}");
    // **The command the operator typed**, which is the one thing the daemon is certain
    // of — it cannot know which process in a pipeline asked.
    assert!(card.contains("run: sudo apt install mc"), "{card}");
    // **The program's own last line, quoted.** Shown and never parsed.
    assert!(card.contains("Do you want to continue? [Y/n]"), "{card}");
    // The manual way in is ON the card, because that is the half of the feature a
    // person needs when the card does not appear at all.
    assert!(card.contains("!send"), "{card}");
    assert!(card.contains("j7"), "the job handle is named: {card}");

    // **The text is typed and it is NOT masked.** The contrast with the password field
    // is the point: dots here would be this card claiming to be a secret channel.
    for c in "Yes".chars() {
        assert!(a.key(Key::Char(c)).is_none());
    }
    let typing = a.screen(100, 20).join("\n");
    assert!(
        typing.contains("Yes"),
        "the prompt card must show what is typed, not dots:\n{typing}"
    );
    assert!(
        !typing.contains('\u{2022}'),
        "a prompt card is drawn in the open and must not mask:\n{typing}"
    );
    assert!(a.input().is_empty(), "the composer must never hold it");

    // Enter sends it once, as a `PromptAnswer` — and never as a `Secret`, which is the
    // head's half of the rule that a password cannot travel this way.
    let sent = a.key(Key::Enter);
    assert_eq!(
        sent,
        Some(Action::PromptAnswer {
            req_id: "prompt-s-1".into(),
            line: "Yes".into(),
        })
    );
    assert!(a.input().is_empty());
    assert!(
        a.editor.history().iter().all(|h| !h.contains("Yes")),
        "the answer must not reach the composer's history either"
    );
    let after = a.screen(100, 20).join("\n");
    assert!(!after.contains("your command is asking"), "{after}");

    // **An empty line is a real answer.** `Continue? [Y/n]` takes Enter as its default,
    // so a person accepting one must not have to type a letter to say so.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::PromptRequested {
            req_id: "prompt-s-2".into(),
            job: "j8".into(),
            command: "apt install mc".into(),
            question: Some("Continue? [Y/n]".into()),
            reading: letibot_sessionlog::PromptReading::Blocked,
        },
    )));
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::PromptAnswer {
            req_id: "prompt-s-2".into(),
            line: String::new(),
        })
    );

    // **Esc puts the card away and refuses nothing.** The command is still running and
    // still waiting; there is nothing to refuse. What the operator gets is the card off
    // the screen and a sentence naming the verb that still works.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::PromptRequested {
            req_id: "prompt-s-3".into(),
            job: "j9".into(),
            command: "apt install mc".into(),
            question: None,
            reading: letibot_sessionlog::PromptReading::Blocked,
        },
    )));
    let bare = a.screen(100, 20).join("\n");
    assert!(
        bare.contains("it has not written anything yet"),
        "a program blocked before its first byte says so rather than showing an \
             empty line as the question:\n{bare}"
    );
    assert_eq!(a.key(Key::Esc), None, "esc is not an answer to send");
    let gone = a.screen(100, 20).join("\n");
    assert!(!gone.contains("your command is asking"), "{gone}");
}

/// **The command ending takes the card down, and the note says which happened.**
///
/// The card outliving its command is the failure this pair exists to prevent: a person
/// typing an answer into a program that has already exited. The two sentences are
/// different because the two things are — one is *your answer went*, the other is *the
/// command ended first* — and a head that said the same words for both would be telling
/// somebody their answer was delivered when it was not.
#[test]
fn a_settled_prompt_takes_the_card_down_and_says_which_way() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::PromptRequested {
            req_id: "prompt-s-1".into(),
            job: "j7".into(),
            command: "apt install mc".into(),
            question: Some("Continue? [Y/n]".into()),
            reading: letibot_sessionlog::PromptReading::Blocked,
        },
    )));
    assert!(
        a.screen(100, 20)
            .join("\n")
            .contains("your command is asking")
    );
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::PromptSettled {
            req_id: "prompt-s-1".into(),
            sent: true,
            by: "dead".into(),
        },
    )));
    let screen = a.screen(100, 20).join("\n");
    assert!(!screen.contains("your command is asking"), "{screen}");
    assert!(screen.contains("answer sent by dead"), "{screen}");
    // And the line itself is nowhere on the screen — the settlement is the record.
    assert!(!screen.contains("Yes"), "{screen}");

    // The other ending: the command finished with the card still up.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::PromptRequested {
            req_id: "prompt-s-2".into(),
            job: "j8".into(),
            command: "apt install mc".into(),
            question: None,
            reading: letibot_sessionlog::PromptReading::Blocked,
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::PromptSettled {
            req_id: "prompt-s-2".into(),
            sent: false,
            by: "the command ended".into(),
        },
    )));
    let screen = a.screen(100, 20).join("\n");
    assert!(!screen.contains("your command is asking"), "{screen}");
    assert!(
        screen.contains("nothing sent (the command ended)"),
        "{screen}"
    );
}

/// **R13's two-clocks trap, checked in the second place it could live.**
///
/// leticl found this while building §1.6: its secret card subtracted a Unix
/// deadline from a **monotonic** clock, so the number it had been drawing was
/// in the hundreds of thousands of seconds. The question that follows is whether
/// this head does the same, and the answer is **no** — the deadline the daemon
/// puts on the wire is `event::now_ms()`, which is the epoch, and the clock this
/// head is handed is the epoch too (`bin::now_ms`, `driver::now_ms`), so the
/// subtraction is in one frame. That is a property nobody had asserted, which is
/// exactly the kind of thing that stops being true quietly, so it gets a test
/// rather than a sentence.
#[test]
fn the_password_cards_countdown_is_seconds_and_not_a_second_epoch() {
    let mut a = app();
    // The shape the daemon builds it: `event::now_ms() + PATIENCE`.
    const NOW: u64 = 1_788_984_000_000;
    a.clock(NOW);
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::SecretRequested {
            req_id: "secret-s-1".into(),
            prompt: "[sudo] password for dead: ".into(),
            command: "sudo true".into(),
            deadline: NOW + 120_000,
        },
    )));
    // **A wide frame on purpose.** The countdown is the last field on that line
    // and the card trims to the width, so a narrow terminal takes the number off
    // before the assertion can read it — which is a fact about this line worth
    // knowing rather than a detail of the test.
    //
    // **And the spelling is the gate card's ladder now** (§1.6): this countdown and
    // that one are one function, because the finding of that commit is that the
    // instrument lived here and had never been pointed at the gate. `120s` was this
    // card's old spelling, and `expires in 2 min` is what the shared ladder makes of
    // it — the number is the same, and what changed is that the two cards can no
    // longer disagree about how long there is.
    let card = a.screen(200, 20).join("\n");
    assert!(card.contains("expires in 2 min"), "{card}");
    // It counts down on the head's clock, not off the event's own timestamp —
    // the same rule the running card above had to learn: the wire deadline is
    // Unix millis and this head's clock is Unix millis, so the two are in one
    // frame and the subtraction is honest. (leticl had this wrong — a Unix
    // deadline against a monotonic counter, drawing hundreds of thousands of
    // seconds — which is why the property is asserted rather than assumed.)
    a.clock(NOW + 30_000);
    let card = a.screen(200, 20).join("\n");
    assert!(card.contains("1m30s left"), "{card}");
    // Under a minute it is whole seconds, and never a decimal. (`60s` exactly is
    // still the `1m00s` rung — the boundary is inclusive upward, which is the same
    // `>= 120` rule one rung down.)
    a.clock(NOW + 61_000);
    let card = a.screen(200, 20).join("\n");
    assert!(card.contains("59s left"), "{card}");
    a.clock(NOW + 73_000);
    let card = a.screen(200, 20).join("\n");
    assert!(card.contains("47s left"), "{card}");
    assert!(!card.contains("47.0"), "never a decimal: {card}");
    // An expired one reads `0s left` rather than counting backwards.
    a.clock(NOW + 200_000);
    let card = a.screen(200, 20).join("\n");
    assert!(card.contains("0s left"), "{card}");
    assert!(!card.contains("-80s left"), "{card}");
    // A number in the hundreds of thousands is what the trap renders as, and it
    // is a *runtime* symptom rather than a shape in the source: nothing here
    // would have caught it in a type.
    assert!(
        !card.contains("0000s left"),
        "a second epoch in seconds: {card}"
    );
}
