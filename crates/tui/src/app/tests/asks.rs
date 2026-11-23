//! The cards that ask for something typed: a key, a secret, a prompt, a terminal answer.

use super::*;

/// A question from the run takes the screen back from the confirmation: the two questions must not
/// be confusable, and neither may be answerable by the other's keystroke. A `PromptRequested` can
/// arrive while the confirmation stands, so it yields with a sentence, and its program runs on.
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
    // And `y` is the program's answer, not a kill: the confirmation is gone, so the key
    // goes where the card that IS up says it goes.
    assert_eq!(a.key(Key::Char('y')), None);
    assert_eq!(a.prompt_buf, "y", "the letter reached the prompt card");
    assert!(a.term.is_some(), "nothing was ended");
}

/// The daemon's key ask is the same card, worded for a key: it arrives as a secret request with no
/// command, so the headline says a key is needed, no `run:` row names a command that does not
/// exist, the field is masked, and a refusal is noted as the operator's.
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
            for_what: String::new(),
            key: None,
            keep_ms: None,
            host: String::new(),
            waiting: 1,
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
            keep_ms: None,
        })
    );
    // A second ask: Esc puts the card away and sends nothing, ctrl-c is the refusal, and
    // alt-a is the way back - noted as the operator's refusal of a key either way.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::SecretRequested {
            req_id: "secret-s-2".into(),
            prompt: "deepseek needs an API key.".into(),
            command: String::new(),
            deadline: 1_000_000,
            for_what: String::new(),
            key: None,
            keep_ms: None,
            host: String::new(),
            waiting: 1,
        },
    )));
    assert_eq!(a.key(Key::Esc), None, "esc refused the ask");
    assert!(a.secret_away, "esc closed the ask");
    a.key(Key::AltA);
    assert_eq!(
        a.key(Key::CtrlC),
        Some(Action::Secret {
            req_id: "secret-s-2".into(),
            secret: None,
            keep_ms: None,
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
            for_what: String::new(),
            key: None,
            keep_ms: None,
            host: String::new(),
            waiting: 1,
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
    // The keys survive a narrow terminal along with the countdown - they are the
    // way out, and the countdown is the only thing on the card that moves.
    for w in [44usize, 60, 80, 200] {
        let rows = a.secret_lines(a.secret.as_ref().expect("the ask"), w);
        let keys = rows.last().expect("a hint row");
        assert!(line_width(keys) <= w, "w={w}: {keys:?}");
        assert!(
            keys.contains("ctrl-c refuses") && keys.contains("esc puts it away"),
            "w={w}: the keys were cut: {keys:?}"
        );
        let countdown = rows
            .iter()
            .find(|l| l.contains("enter sends it"))
            .expect("the enter row");
        assert!(
            countdown.contains("left"),
            "w={w}: the countdown was cut: {countdown:?}"
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
            for_what: String::new(),
            key: None,
            keep_ms: None,
            host: String::new(),
            waiting: 1,
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
            keep_ms: None,
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
            for_what: String::new(),
            key: None,
            keep_ms: None,
            host: String::new(),
            waiting: 1,
        },
    )));
    assert_eq!(
        a.key(Key::CtrlC),
        Some(Action::Secret {
            req_id: "secret-s-2".into(),
            secret: None,
            keep_ms: None,
        })
    );
}

/// A card raised WITHOUT a reading does not claim the program asked. The reading-backed card's
/// headline is *your command is asking*, which would be a guess on a run nobody could look at, so
/// this one says it could not look and still offers the way in: a line goes into its input anyway.
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
    // And it is a card you can still answer - that is the whole point of it being a card.
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

/// The prompt card: a command of the operator's own is asking, and the field is NOT masked. The
/// text is drawn as typed, the composer never holds it, and Enter sends it once as an
/// [`Action::PromptAnswer`] - never as an [`Action::Secret`].
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
    // The command the operator typed, which is the one thing the daemon is certain
    // of - it cannot know which process in a pipeline asked.
    assert!(card.contains("run: sudo apt install mc"), "{card}");
    // The program's own last line, quoted. Shown and never parsed.
    assert!(card.contains("Do you want to continue? [Y/n]"), "{card}");
    // The manual way in is ON the card, because that is the half of the feature a
    // person needs when the card does not appear at all.
    assert!(card.contains("!send"), "{card}");
    assert!(card.contains("j7"), "the job handle is named: {card}");

    // The text is typed and it is NOT masked. The contrast with the password field
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

    // Enter sends it once, as a `PromptAnswer` - and never as a `Secret`, which is the
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

    // An empty line is a real answer. `Continue? [Y/n]` takes Enter as its default,
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

    // Esc puts the card away and refuses nothing. The command is still running and
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

/// The command ending takes the card down, and the note says which happened. A card outliving its
/// command would have a person typing an answer into a program that has already exited, so the two
/// sentences differ: one is *your answer went*, the other *the command ended first*.
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
    // And the line itself is nowhere on the screen - the settlement is the record.
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

/// The two-clocks trap, checked in the second place it could live: a secret card that subtracted
/// a Unix deadline from a monotonic clock would draw hundreds of thousands of seconds. Here the
/// wire deadline and this head's clock are both the epoch, and that is asserted, not assumed.
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
            for_what: String::new(),
            key: None,
            keep_ms: None,
            host: String::new(),
            waiting: 1,
        },
    )));
    // A wide frame on purpose: the countdown is the last field on that line and the card trims to
    // the width, so a narrow terminal takes the number off before the assertion can read it. The
    // spelling is the gate card's ladder, so the two cards cannot disagree about how long there is.
    let card = a.screen(200, 20).join("\n");
    assert!(card.contains("expires in 2 min"), "{card}");
    // It counts down on the head's clock, not off the event's own timestamp: the wire deadline and
    // this head's clock are both Unix millis, so the two are in one frame and the subtraction is
    // honest. A Unix deadline against a monotonic counter draws hundreds of thousands of seconds.
    a.clock(NOW + 30_000);
    let card = a.screen(200, 20).join("\n");
    assert!(card.contains("1m30s left"), "{card}");
    // Under a minute it is whole seconds, and never a decimal. (`60s` exactly is
    // still the `1m00s` rung - the boundary is inclusive upward, which is the same
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

/// A key typed on the line never travels: `/models X --key K` sends nothing, opens the masked card
/// for X and leaves the composer history, so neither Up nor any row shows the key.
#[test]
fn a_key_typed_on_the_line_goes_through_the_masked_card_and_nowhere_else() {
    let mut a = app();
    a.session_id = "s".into();
    typed(&mut a, "/models deepseek/deepseek-flash --key sk-SECRET-1");
    assert_eq!(a.key(Key::Enter), None, "nothing is sent");
    let ask = a.key_ask.clone().expect("the masked card opens");
    assert_eq!(ask.choice, "deepseek/deepseek-flash");
    assert_eq!(ask.provider, "deepseek");
    let leaked = |a: &mut App| {
        a.editor.history().iter().any(|l| l.contains("SECRET"))
            || a.notice.as_deref().is_some_and(|n| n.contains("SECRET"))
            || a.screen(110, 30).join("\n").contains("SECRET")
    };
    assert!(
        !leaked(&mut a),
        "the typed key is gone from history and screen"
    );

    for c in "sk-SECRET-2".chars() {
        a.key(Key::Char(c));
    }
    assert!(!leaked(&mut a), "the card masks what is typed into it");
    match a.key(Key::Enter) {
        Some(Action::Slash { line }) => {
            assert_eq!(line, "models deepseek/deepseek-flash --key sk-SECRET-2")
        }
        other => panic!("the card's enter sends the switch, not {other:?}"),
    }
    a.refresh_prompt_history();
    assert!(!leaked(&mut a), "no recall carries the key");

    // Bare `--key` opens the card too.
    typed(&mut a, "/models glm --key");
    a.key(Key::Enter);
    assert_eq!(a.key_ask.as_ref().map(|k| k.provider.as_str()), Some("glm"));
    a.key(Key::Esc);
    assert!(!leaked(&mut a));
}

/// `/flowy login --token …` is the second secret the masked card collects. It used to be refused
/// with a `--token-file` flag, which left a person holding a token with nowhere to put it: the card
/// asks for it, on a line of its own, and what the operator typed beside the flag - the seat - rides
/// into the verb Enter sends. Nothing of the token reaches the history, the notice or the screen.
#[test]
fn a_flowy_token_is_typed_into_the_masked_card_and_sent_as_the_login_verb() {
    let mut a = app();
    a.session_id = "s".into();
    typed(&mut a, "/flowy login my-seat --token tok-SECRET-3");
    assert_eq!(a.key(Key::Enter), None, "nothing is sent");
    let ask = a.key_ask.clone().expect("the masked card opens");
    assert_eq!(ask.provider, "flowy");
    assert_eq!(
        ask.kind,
        KeyKind::Flowy {
            rest: "my-seat ".into()
        },
        "the seat the operator typed is carried, and the token is not"
    );
    let leaked = |a: &mut App| {
        a.editor.history().iter().any(|l| l.contains("SECRET"))
            || a.notice.as_deref().is_some_and(|n| n.contains("SECRET"))
            || a.screen(110, 30).join("\n").contains("SECRET")
    };
    assert!(!leaked(&mut a), "the typed token is nowhere");

    // The card says what it is for, and it is not a provider's key: rano's card is the provider's.
    let screen = a.screen(110, 30).join("\n");
    assert!(
        screen.contains("flowy seat needs a token"),
        "the card does not say what is being asked for:\n{screen}"
    );
    assert!(
        !screen.contains("needs a key this box does not hold"),
        "a token was drawn as a provider's key:\n{screen}"
    );

    for c in "tok-9".chars() {
        a.key(Key::Char(c));
    }
    assert!(!leaked(&mut a), "the card masks what is typed into it");
    match a.key(Key::Enter) {
        Some(Action::Slash { line }) => {
            assert_eq!(line, "flowy login my-seat --token tok-9");
        }
        other => panic!("the card's enter sends the login verb, not {other:?}"),
    }
    // And esc is a cancel that says so, in the card's own words.
    typed(&mut a, "/flowy login --token tok-SECRET-4");
    a.key(Key::Enter);
    assert!(a.key_ask.is_some(), "the card is up");
    a.key(Key::Esc);
    assert!(a.key_ask.is_none());
    assert_eq!(
        a.notice.as_deref(),
        Some("cancelled — no flowy token was sent")
    );
    assert!(!leaked(&mut a));
}

/// The generic card: the first line says what will use the value, the host and the count of
/// commands waiting on it are drawn, `alt-k` is the second answer, and an ask with no key offers
/// no window at all.
#[test]
fn the_generic_card_names_what_uses_and_offers_keeping_only_when_a_key_was_given() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::SecretRequested {
            req_id: "secret-s-1".into(),
            prompt: "[letibot] secret:".into(),
            command: "letibot-secret ask --for 'sudo on lubuntu1-3'".into(),
            deadline: 1_000_000,
            for_what: "sudo on lubuntu1-3".into(),
            key: Some("fleet-sudo".into()),
            keep_ms: Some(15 * 60 * 1000),
            host: "lubuntu1-3".into(),
            waiting: 2,
        },
    )));
    let card = a.screen(140, 30).join("\n");
    assert!(card.contains("for: sudo on lubuntu1-3"), "{card}");
    assert!(card.contains("on: lubuntu1-3"), "{card}");
    assert!(card.contains("2 commands are waiting"), "{card}");
    assert!(card.contains("alt-k keeps it 15 min"), "{card}");
    assert!(
        card.contains("fleet-sudo"),
        "the name it is filed under: {card}"
    );

    // alt-k answers with the window, which is the whole difference from Enter.
    for c in "hunter2".chars() {
        a.key(Key::Char(c));
    }
    match a.key(Key::AltK) {
        Some(Action::Secret {
            secret, keep_ms, ..
        }) => {
            assert_eq!(secret.as_deref(), Some("hunter2"));
            assert_eq!(keep_ms, Some(15 * 60 * 1000));
        }
        other => panic!("alt-k must answer with the window, not {other:?}"),
    }

    // An ask with no key cannot be kept, and neither the card nor the key pretends otherwise.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::SecretRequested {
            req_id: "secret-s-2".into(),
            prompt: "[sudo] password for dead: ".into(),
            command: "sudo true".into(),
            deadline: 1_000_000,
            for_what: String::new(),
            key: None,
            keep_ms: None,
            host: String::new(),
            waiting: 1,
        },
    )));
    let card = a.screen(140, 30).join("\n");
    assert!(!card.contains("alt-k"), "no key, no window: {card}");
    for c in "hunter2".chars() {
        a.key(Key::Char(c));
    }
    assert_eq!(
        a.key(Key::AltK),
        None,
        "alt-k on a keyless ask answers nothing"
    );
    assert!(a.secret.is_some(), "and the card is still up");
}

/// A second ask for the same key re-says the card with a higher count, and does not eat what the
/// person has already typed into it.
#[test]
fn a_joined_ask_keeps_the_typing_that_is_already_there() {
    let mut a = app();
    let card = |waiting: u32| SessionEvent::SecretRequested {
        req_id: "secret-s-1".into(),
        prompt: "[letibot] secret:".into(),
        command: "letibot-secret ask".into(),
        deadline: 1_000_000,
        for_what: "sudo on lubuntu1-3".into(),
        key: Some("fleet-sudo".into()),
        keep_ms: Some(15 * 60 * 1000),
        host: "lubuntu1-3".into(),
        waiting,
    };
    a.apply(ServerFrame::Event(env(1, card(1))));
    for c in "hun".chars() {
        a.key(Key::Char(c));
    }
    a.apply(ServerFrame::Event(env(2, card(2))));
    assert_eq!(a.secret_buf, "hun", "the person's typing survived");
    assert!(
        a.screen(140, 30)
            .join("\n")
            .contains("2 commands are waiting"),
        "and the count is the new one"
    );
}
