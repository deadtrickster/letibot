//! The permission card: a decision's ladder, its answers, its settled form.

use super::*;

/// **The card states why it is asking, in the words of whoever is stuck.**
///
/// `because` is the model's own line on a question — *"what you are stuck on and
/// why the answer changes what you do"* — and this head carried it on
/// `OpenDecision` from the day the field existed and never drew it. So the person
/// was given a question with no answer to *why are you asking me this*, which is
/// the only thing that decides whether the question is worth their time.
#[test]
fn the_card_states_why_it_is_asking_in_the_speakers_own_words() {
    use letibot_sessionlog::event::OptionKind;
    let a = app();

    // Nothing to say, so nothing said: §13.2b in both directions — a `because:`
    // row with an empty string is a row of nothing, and the card is not entitled
    // to it.
    let bare = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    let drawn = a.decision_lines(&bare, 100).join("\n");
    assert!(!drawn.contains("because"), "{drawn}");

    let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    d.because = "the answer changes which migration I write".into();
    let drawn = a.decision_lines(&d, 100).join("\n");
    assert!(
        drawn.contains("because: the answer changes which migration I write"),
        "{drawn}"
    );
    // Labelled with its speaker, and in the same dim register every other
    // borrowed sentence on this card uses — the oracle's verdict included,
    // because both are evidence rather than the question. Checked on a head
    // that emits colour, since the layout tests deliberately do not.
    let painted = App::new(RenderConfig {
        width: 100,
        color: true,
        ..RenderConfig::default()
    });
    let line = painted
        .decision_lines(&d, 100)
        .into_iter()
        .find(|l| l.contains("because:"))
        .expect("the row");
    assert!(line.contains(sgr::DIM), "not in the dim register: {line:?}");
    // And it sits with the rest of the evidence, above the ladder — read before
    // the choice is made, which is the only time it can inform one.
    let because_at = drawn.find("because:").unwrap();
    let ladder_at = drawn.find("allow_once").unwrap();
    assert!(because_at < ladder_at, "{drawn}");
}

/// **§11.7: the card says WHY it is asking, and only where the reason is true.**
///
/// R18's last wording item. The headline says what the tool **declares** (`wants exec
/// access`) and the line above it is layer A's reading of the **action** (`auto (a read
/// inside the boundary)`), and nothing joined them — so *"the classifier decided this
/// needed no asking"* read as being argued with by the card going up anyway. The operator,
/// asleep at the time: an operator who cannot tell that apart from a question they should
/// have been asked is the shape that cost three 300-second refusals in one night.
///
/// **Two assertions, and the second is the one that keeps the sentence honest.** An
/// `exec`-declaring call that asks carries the clause. A `read`-declaring one **must not**:
/// a read is asked about because of a rule, a path or a mode, so a sentence blaming its
/// declaration would be false on the very card carrying it — which is exactly what
/// `because: workspace: /` was, a fact named after one thing and read from another.
#[test]
fn the_card_says_the_declared_access_is_what_asks_and_only_when_it_is() {
    use letibot_sessionlog::event::OptionKind;
    let a = app();

    let mut exec = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    exec.access = "exec".into();
    let drawn = a.decision_lines(&exec, 100).join("\n");
    assert!(
        drawn.contains("the access is what asks"),
        "§11.7: an exec call that asks must say why it is asking:\n{drawn}"
    );

    // The falsification direction. A read that is asked about is asked about for some
    // other reason, and the clause would be a lie on it.
    let mut read = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    read.access = "read".into();
    let drawn = a.decision_lines(&read, 100).join("\n");
    assert!(
        !drawn.contains("the access is what asks"),
        "the clause is only true of a declaration that ASKS, and a read's does not:\n{drawn}"
    );

    // A daemon older than the field sends an empty access. No clause: the head does not
    // know, and will not guess.
    let older = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    let drawn = a.decision_lines(&older, 100).join("\n");
    assert!(!drawn.contains("the access is what asks"), "{drawn}");
}

/// **A card nobody took to an oracle says so, and a card that did does not.**
///
/// The two silences are different facts and they rendered identically: under
/// `/mode supervised` the question printed above the ladder is *do you agree with
/// the model*, and a card with no verdict on it makes *no oracle was consulted*
/// and *the oracle was asked and said nothing* the same screen. The operator hit
/// the live half of it — an oracle answered and its verdict was unreadable, and
/// the card said so — while the card nobody had asked said exactly the same
/// nothing.
#[test]
fn a_card_that_was_never_taken_to_an_oracle_says_so_and_one_that_was_does_not() {
    use letibot_sessionlog::event::OptionKind;
    let a = app();
    let asked = "no oracle was consulted for this one — the judgement is yours alone";

    // Not supervised, or supervised with no advisor installed: nothing was asked,
    // and the card says which of the two silences this is.
    let d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    let drawn = a.decision_lines(&d, 100).join("\n");
    assert!(drawn.contains(asked), "{drawn}");

    // An oracle answered, however badly: the verdict is drawn and the sentence is
    // not, so "asked" and "not asked" are two screens.
    let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    d.advice = Some(letibot_sessionlog::event::ModelAdvice {
        // The case the operator saw: the oracle was consulted and could not
        // produce a verdict, which the daemon poses as a verdict anyway.
        consulted: true,
        would: "unavailable".into(),
        by: "adjudicator".into(),
        basis: "the verdict could not be read".into(),
        cites: Vec::new(),
        latency_ms: 4_000,
        unsure: None,
    });
    let drawn = a.decision_lines(&d, 100).join("\n");
    assert!(
        !drawn.contains(asked),
        "an oracle was asked, so the sentence is false: {drawn}"
    );
    assert!(drawn.contains("model says unavailable"), "{drawn}");

    // **A question is not a gate, so it says neither.** Nothing is ever consulted
    // for one, and a line about oracles on a card offering plain-text choices is
    // noise in the register a reader is taught to skim.
    let mut q = decision_with(&[]);
    q.kind = "question".into();
    let drawn = a.decision_lines(&q, 100).join("\n");
    assert!(!drawn.contains(asked), "a question said it: {drawn}");
    assert!(
        !drawn.contains("model says"),
        "nor claimed a verdict: {drawn}"
    );
}

/// **A glob typed after the option id is the rule's coverage.**
///
/// > *"please add globbing to my answers somehow too"*
#[test]
fn an_answer_can_carry_the_operators_own_glob() {
    use letibot_sessionlog::event::OptionKind;
    let d = decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::AllowAlways,
        OptionKind::RejectOnce,
    ]);

    // The bare id still answers, and asks for no pattern.
    assert_eq!(
        match_option(&d, "allow_once"),
        OptionChoice::One {
            option_id: "allow_once".into(),
            pattern: None,
            note: None
        }
    );
    // A prefix still answers when it names exactly one option, which is how
    // people actually type.
    assert_eq!(
        match_option(&d, "d"),
        OptionChoice::One {
            option_id: "deny".into(),
            pattern: None,
            note: None
        }
    );

    // And the glob rides after it, verbatim: a pattern is a path, so it is not
    // lowercased the way the option id is.
    assert_eq!(
        match_option(&d, "allow_always crates/**/Cargo.toml"),
        OptionChoice::One {
            option_id: "allow_always".into(),
            pattern: Some("crates/**/Cargo.toml".into()),
            note: None
        }
    );

    // **A glob on anything but `allow_always` is refused, not dropped.**
    // Somebody who typed `allow_once src/**` meant the rule to cover `src/**`;
    // granting one call instead is an answer they did not give. `Unnamed` leaves
    // the line in the composer where they can see it.
    assert_eq!(match_option(&d, "allow_once src/**"), OptionChoice::Unnamed);
    assert_eq!(match_option(&d, "deny src/**"), OptionChoice::Unnamed);
}

/// **An ambiguous prefix is refused, and the candidates are named.**
///
/// Typing `allow` used to answer `allow_once` — silently, out of
/// `allow_once`/`allow_session`/`allow_always`, because `.find(starts_with)` takes the
/// first hit in list order. Which of three grants the operator gave is the whole
/// content of the answer, and list position is not something they said. This is a
/// gate: a grant invented by position is an answer nobody gave, and it writes an
/// audit row naming an option the operator can see they did not type.
///
/// Refused means **nothing is answered** — not the typed prefix's first guess, and not
/// the marked row either. The line stays in the composer to be finished, the card
/// stays up, and the sentence says both ways out.
#[test]
fn an_ambiguous_option_prefix_is_refused_with_the_candidates_named() {
    use letibot_sessionlog::event::OptionKind;
    let d = decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::AllowSession,
        OptionKind::AllowAlways,
        OptionKind::RejectOnce,
    ]);
    assert_eq!(
        match_option(&d, "allow"),
        OptionChoice::Ambiguous {
            word: "allow".into(),
            candidates: vec![
                "allow_once".into(),
                "allow_session".into(),
                "allow_always".into()
            ]
        }
    );
    // One character further and it is an answer again — and the case fold does not
    // decide which of these is ambiguous.
    assert_eq!(
        match_option(&d, "allow_a"),
        OptionChoice::One {
            option_id: "allow_always".into(),
            pattern: None,
            note: None
        }
    );
    assert_eq!(
        match_option(&d, "ALLOW_S"),
        OptionChoice::One {
            option_id: "allow_session".into(),
            pattern: None,
            note: None
        }
    );
    // An exact id is never ambiguous, however many of its siblings start the same
    // way, and neither is a line that names nothing at all.
    assert_eq!(
        match_option(&d, "allow_once"),
        OptionChoice::One {
            option_id: "allow_once".into(),
            pattern: None,
            note: None
        }
    );
    assert_eq!(match_option(&d, "nope"), OptionChoice::Unnamed);
    assert_eq!(match_option(&d, ""), OptionChoice::Unnamed);

    // The sentence names them and says what to do next.
    let said = ambiguous_option_line(
        "allow",
        &[
            "allow_once".into(),
            "allow_session".into(),
            "allow_always".into(),
        ],
    );
    for id in ["allow_once", "allow_session", "allow_always"] {
        assert!(said.contains(id), "{said}");
    }
    assert!(said.contains("`allow`"), "{said}");
    assert!(said.contains("finish typing"), "{said}");
    assert!(said.contains('↑'), "{said}");
    // A card long enough to overflow the line says how many there are instead of
    // silently dropping half the candidates.
    let many: Vec<String> = (0..9).map(|i| format!("opt_{i}")).collect();
    let long = ambiguous_option_line("opt", &many);
    assert!(long.contains("9 options"), "{long}");
    assert!(long.contains('…'), "{long}");
}

/// **`deny_and_tell` can be told something.** Its label is *"Deny, and tell
/// the model why"* and nothing carried the why: the head had no field, the
/// wire had no field, and typing it was REFUSED by `match_option` — the line
/// stayed in the composer and nothing was answered at all. The operator:
/// *"deny and tell doesnt work - there is no input for the 'tell' part"*.
#[test]
fn deny_and_tell_carries_the_operators_words() {
    use letibot_sessionlog::event::OptionKind;
    let d = decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::AllowAlways,
        OptionKind::RejectAlways,
    ]);

    // Keyed on the KIND, not the id: the daemon calls this option
    // `deny_and_tell` and this fixture calls it `deny_always`, and the rule
    // is about the option that writes a standing refusal either way.
    //
    // The words ride after the id, verbatim — a sentence for a reader, so it
    // is not lowercased the way the option id is.
    assert_eq!(
        match_option(&d, "deny_always Use the scratch dir, not /tmp."),
        OptionChoice::One {
            option_id: "deny_always".into(),
            pattern: None,
            note: Some("Use the scratch dir, not /tmp.".into())
        }
    );
    // Bare still answers, and carries nothing rather than claiming a reason.
    assert_eq!(
        match_option(&d, "deny_always"),
        OptionChoice::One {
            option_id: "deny_always".into(),
            pattern: None,
            note: None
        }
    );
    // The two trailing-word options do not borrow each other's field: a glob
    // is a rule and a reason is a sentence, and putting one where the other
    // goes would be a rule nobody wrote or a sentence nobody reads.
    assert_eq!(
        match_option(&d, "allow_always src/**"),
        OptionChoice::One {
            option_id: "allow_always".into(),
            pattern: Some("src/**".into()),
            note: None
        }
    );
    // And everything else still refuses trailing words rather than dropping
    // them.
    assert_eq!(
        match_option(&d, "allow_once because I said so"),
        OptionChoice::Unnamed
    );

    // The card says where to type it, which it never did.
    let a = app();
    assert!(
        a.decision_lines(&d, 100)
            .iter()
            .any(|l| l.contains("`deny_and_tell <why>`")),
        "{:?}",
        a.decision_lines(&d, 100)
    );
    let plain = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    assert!(
        !a.decision_lines(&plain, 100)
            .iter()
            .any(|l| l.contains("deny_and_tell")),
        "and only when the option is on offer"
    );
}

/// The hint only appears when the option it describes is on offer.
#[test]
fn the_glob_hint_is_absent_when_no_rule_can_be_written() {
    use letibot_sessionlog::event::OptionKind;
    let a = app();
    let with = decision_with(&[OptionKind::AllowOnce, OptionKind::AllowAlways]);
    let without = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    assert!(
        a.decision_lines(&with, 100)
            .iter()
            .any(|l| l.contains("allow_always <glob>")),
        "an always-allow on offer says how to scope it"
    );
    assert!(
        !a.decision_lines(&without, 100)
            .iter()
            .any(|l| l.contains("<glob>")),
        "a request with no rule to write must not advertise one"
    );
}

/// **The command is on a line of its own, and the taxonomy is not in front of
/// it.** The operator's report: *"no possible to see wtf was the command i
/// supposed to approve"* — because the summary carried the target inside a
/// sentence, layer A's reading was appended to that sentence, and a long
/// command was then wrapped into the middle of the wall.
#[test]
fn the_command_being_approved_gets_its_own_line() {
    use letibot_sessionlog::event::OptionKind;
    let a = app();
    let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    let cmd = "cargo test -p letibot-tools --test clauses -- --nocapture";
    d.summary = format!("`bash` wants exec access to `{cmd}`");
    d.target = cmd.to_string();
    d.detail = "ask — intents [execute_code] over [host_other]".into();
    let lines = a.decision_lines(&d, 100);

    // The command, alone on its line, indented — not embedded in the question.
    let own = lines
        .iter()
        .find(|l| l.contains(cmd))
        .unwrap_or_else(|| panic!("the command is not shown at all: {lines:#?}"));
    assert_eq!(own.trim(), cmd, "the command shares its line with prose");

    // The question above it names the tool and the access, and does NOT repeat
    // the command or carry layer A's vocabulary.
    assert!(
        lines[0].contains("`bash` wants exec access"),
        "{}",
        lines[0]
    );
    assert!(
        !lines[0].contains(cmd),
        "the command is back in the headline: {}",
        lines[0]
    );
    assert!(
        !lines[0].contains("intents"),
        "the taxonomy is back on top: {}",
        lines[0]
    );

    // And layer A is still there, under it, for whoever wants it.
    assert!(
        lines.iter().any(|l| l.contains("intents [execute_code]")),
        "the deterministic reading was dropped rather than demoted: {lines:#?}"
    );
}

/// **§11.7: the card says why it is asking, when the declaration is the reason.**
///
/// The card printed two statements that looked like a contradiction — the headline's
/// `wants exec access` (the tool's declared access) and the dim line under it, layer
/// A's reading of the *action* (`auto (a read inside the boundary)`) — and nothing
/// joined them, so *"the classifier decided this needed no asking"* read as being
/// argued with by the card going up anyway. That shape cost three 300-second
/// refusals in one night (`head-parity` R18); the missing clause is the access.
///
/// **And it is absent where the access is not what asks**, which is the half that
/// keeps it worth reading: a `read` call is asked about because of a rule, a path or
/// a mode, so a sentence about its declaration would be false on the very card
/// carrying it. An empty `access` is a daemon older than the field, and the head does
/// not guess.
#[test]
fn the_card_says_the_declared_access_is_what_asks() {
    use letibot_sessionlog::event::OptionKind;
    let a = app();
    let says = |access: &str| {
        let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
        d.summary = "`bash` wants exec access to `cargo test`".into();
        d.target = "cargo test".into();
        d.detail = "auto — intents [read_file] — auto (a read inside the boundary)".into();
        d.access = access.into();
        a.decision_lines(&d, 200).join("\n")
    };

    let exec = says("exec");
    assert!(exec.contains("the access is what asks"), "{exec}");
    assert!(
        exec.contains("a tool declared to `exec`"),
        "the clause names the declaration the headline already named: {exec}"
    );
    // Under the two statements it joins, because that is what it joins.
    let at_clause = exec.find("the access is what asks").unwrap();
    let at_detail = exec.find("intents [read_file]").unwrap();
    assert!(at_detail < at_clause, "{exec}");
    // …and in their register: evidence beside the question, not a second question.
    let painted = App::new(RenderConfig {
        width: 200,
        color: true,
        ..RenderConfig::default()
    });
    let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    d.access = "exec".into();
    let line = painted
        .decision_lines(&d, 200)
        .into_iter()
        .find(|l| l.contains("the access is what asks"))
        .expect("the row");
    assert!(line.contains(sgr::DIM), "not in the dim register: {line:?}");

    // **Absent where the access is not what asks** — a read call, a write call, a
    // network call, and a daemon that did not say.
    for other in ["read", "write", "network", ""] {
        let drawn = says(other);
        assert!(
            !drawn.contains("the access is what asks"),
            "an access of {other:?} is not why anything is being asked: {drawn}"
        );
        // The card is still a card.
        assert!(drawn.contains("allow_once"), "{drawn}");
    }
    // And a question never carries it: nothing is consulted for one, and its `access`
    // is whatever the ask's class happens to be rather than a declaration anyone
    // decided on.
    let mut q = decision_with(&[]);
    q.kind = "question".into();
    q.access = "exec".into();
    q.choices = vec!["a".into()];
    assert!(
        !a.decision_lines(&q, 200)
            .join("\n")
            .contains("the access is what asks"),
        "a question is not a gate and says nothing about declarations"
    );
}

/// **Five facts, five lines — leticl's measurement, as a test.**
///
/// leticl ran three real `decision_requested` frames through a scratch head and read the
/// cards: **every field a head controls was identical** across *could not decide*,
/// *unreadable* and *out of room* — `would: "ask"`, `consulted: true`, `cites: []` — and
/// the card echoed the daemon's prose and authored nothing. `would: "ask"` is *also* what
/// `NotAuthorised` sets, so five distinct facts reached the glass as one line.
///
/// The assertion is **pairwise difference**: for each of the five, the rendered line must
/// differ from every other. A test that only checked "the field is carried" would pass
/// against a head that ignored it, which is the half that was already broken.
#[test]
fn the_five_facts_behind_one_ask_are_five_lines_on_the_card() {
    use letibot_sessionlog::event::OptionKind;
    let a = app();
    let rendered = |unsure: Option<&str>, would: &str| -> String {
        let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
        d.advice = Some(letibot_sessionlog::event::ModelAdvice {
            consulted: true,
            would: would.into(),
            by: "model:test".into(),
            basis: "THE-BASIS".into(),
            cites: Vec::new(),
            unsure: unsure.map(str::to_string),
            latency_ms: 12,
        });
        a.decision_lines(&d, 100).join("\n")
    };

    // The four ways of NOT answering, and the fifth which is an answer.
    let five = [
        ("could_not_decide", "ask"),
        ("between_thresholds", "ask"),
        ("unreadable", "ask"),
        ("out_of_room", "ask"),
        // The fifth: consulted, answered, and the answer was no. `unsure: None`.
        ("", "ask"),
    ];
    let mut seen: Vec<(String, String)> = Vec::new();
    for (unsure, would) in five {
        let screen = rendered(
            if unsure.is_empty() {
                None
            } else {
                Some(unsure)
            },
            would,
        );
        assert!(
            screen.contains("THE-BASIS"),
            "the daemon's sentence must still be there ({unsure:?}):\n{screen}"
        );
        for (other_unsure, other_screen) in &seen {
            assert_ne!(
                &screen, other_screen,
                "`{unsure}` and `{other_unsure}` render identically — which is the five-\
                     facts-one-line defect this field exists to end"
            );
        }
        seen.push((unsure.to_string(), screen));
    }
    // And a kind this head does not know is shown rather than folded into one of the
    // four, so a daemon that gains a fifth is visible.
    let unknown = rendered(Some("fifth_kind"), "ask");
    assert!(unknown.contains("fifth_kind"), "{unknown}");
}

#[test]
fn a_refused_call_is_rendered_as_refused_rather_than_as_silence() {
    // The old head recorded settled decisions in a field it never drew, so a
    // denied tool call left a prompt on the screen and then nothing where the
    // answer should have been.
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::requested("r1", "rm -rf /"),
    )));
    a.apply(ServerFrame::Event(env(2, testing::answered("r1", "deny"))));
    let screen = a.screen(120, 16).join("\n");
    assert!(screen.contains("rm -rf /"), "{screen}");
    assert!(screen.contains("REFUSED"), "{screen}");
}

/// **A question the person answered is not a permission's verdict.**
///
/// The daemon's settled event for a question says `Cancelled` — `DecisionOutcome` is a
/// permission's vocabulary and a question has no ladder to select from — so a head that
/// read the outcome word alone would tell the person they had *cancelled* the answer
/// they just gave, or (as rano's `Settled::Refused` would have it) that it was
/// **REFUSED**. What the head knows and the wire does not is which kind it was, and
/// this is that: the answer, attributed, in the faint register.
#[test]
fn an_answered_question_is_drawn_as_the_answer_and_not_as_a_permission_verdict() {
    use letibot_sessionlog::event::{Decider, DecisionOutcome, SessionEvent};
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::asked(
            "q1",
            "which database should the migration target?",
            &["postgres", "sqlite"],
            "the two need different migration files",
        ),
    )));
    assert_eq!(a.open_decisions().len(), 1);
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::DecisionAnswered {
            req_id: "q1".into(),
            outcome: DecisionOutcome::Cancelled,
            by: Decider {
                kind: "human".into(),
                identity: "deadtrickster".into(),
            },
            basis: "chose option 1: sqlite with the note: only for the CUDA box".into(),
            late: false,
        },
    )));
    assert!(a.open_decisions().is_empty());
    let screen = a.screen(160, 24).join("\n");
    assert!(
        screen.contains("which database should the migration target?"),
        "{screen}"
    );
    assert!(screen.contains("sqlite"), "{screen}");
    assert!(
        screen.contains("deadtrickster"),
        "the answer is not attributed: {screen}"
    );
    for wrong in ["REFUSED", "cancelled", "allowed"] {
        assert!(
            !screen.contains(wrong),
            "a question was drawn with a permission's word {wrong:?}: {screen}"
        );
    }
}

#[test]
fn a_settled_decision_is_not_offered_for_answering() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
    assert_eq!(a.open_decisions().len(), 1);
    a.apply(ServerFrame::Event(env(2, testing::answered("r1", "deny"))));
    assert!(a.open_decisions().is_empty());
    // And typing an option id no longer answers it: it becomes a prompt.
    typed(&mut a, "allow");
    assert!(matches!(a.key(Key::Enter), Some(Action::Prompt(_))));
}

#[test]
fn a_permission_settles_on_the_call_it_gated() {
    let mut a = app();
    // A turn with one call, gated by a permission the oracle allowed.
    a.apply(ServerFrame::Event(env(0, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        1,
        testing::proposed("t1", "c1", "bash"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        testing::requested("r1", "run rm -rf"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::DecisionAnswered {
            req_id: "r1".into(),
            outcome: letibot_sessionlog::event::DecisionOutcome::Selected {
                option_id: "allow".into(),
            },
            by: letibot_sessionlog::event::Decider {
                kind: "model".into(),
                identity: "oracle".into(),
            },
            basis: "the operator asked for this".into(),
            late: false,
        },
    )));
    // The decision rides the call's card, not the note list.
    let t = a.turn.as_ref().expect("a turn");
    let c = t
        .calls
        .iter()
        .find(|c| c.call_id == "c1")
        .expect("the call");
    assert_eq!(c.decision.as_ref().expect("the decision").by.kind, "model");
    // Folded: one dim line, who decided and how. Open: what was asked and the reply.
    let folded = call_card(c, &plain_cfg(120), 0, Fold::Folded, true).join("\n");
    assert!(folded.contains("allowed, by model oracle"), "{folded}");
    assert!(
        !folded.contains("oracle:"),
        "folded shows no reply: {folded}"
    );
    let open = call_card(c, &plain_cfg(120), 0, Fold::Open, true).join("\n");
    assert!(open.contains("asked: run rm -rf"), "{open}");
    // The basis is labelled by whoever DECIDED — here a model — and never as
    // the oracle's, which it is not. No oracle advised this one, and the card
    // says so rather than leaving a blank that reads like silence.
    assert!(
        open.contains("model: the operator asked for this"),
        "{open}"
    );
    assert!(open.contains("no oracle was consulted"), "{open}");
}

/// **The bug this labelling was built for.** Under `/supervise` the oracle
/// advises and the OPERATOR answers, so the decision carries two reasons: the
/// operator's (`basis`) and the guard model's (`advice`). They were one line,
/// rendered as `oracle: {basis}` — which printed the operator's own words
/// under the oracle's name, a lie that reads exactly like the truth.
#[test]
fn an_operator_answer_over_an_oracles_advice_keeps_the_two_reasons_apart() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(0, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        1,
        testing::proposed("t1", "c1", "bash"),
    )));
    // The ask, carrying the oracle's verdict the way `/supervise` poses it.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::DecisionRequested {
            write_targets: Vec::new(),
            req_id: "r1".into(),
            kind: "permission".into(),
            call_id: Some("c1".into()),
            access: String::new(),
            summary: "run rm -rf build/".into(),
            target: String::new(),
            detail: String::new(),
            options: Vec::new(),
            choices: Vec::new(),
            because: String::new(),
            advice: Some(letibot_sessionlog::event::ModelAdvice {
                consulted: true,
                would: "admit".into(),
                by: "glm-5.3-flash".into(),
                basis: "the operator asked for a clean rebuild in this turn".into(),
                cites: vec!["rebuild it from scratch".into()],
                latency_ms: 2_100,
                unsure: None,
            }),
            subagent: None,
            deadline: None,
            on_timeout: letibot_sessionlog::event::OnTimeout::Deny,
        },
    )));
    // The operator answers it themselves.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::DecisionAnswered {
            req_id: "r1".into(),
            outcome: letibot_sessionlog::event::DecisionOutcome::Selected {
                option_id: "allow_once".into(),
            },
            by: letibot_sessionlog::event::Decider {
                kind: "human".into(),
                identity: "dead".into(),
            },
            basis: "dead chose `allow_once` at the head".into(),
            late: false,
        },
    )));

    let t = a.turn.as_ref().expect("a turn");
    let c = t
        .calls
        .iter()
        .find(|c| c.call_id == "c1")
        .expect("the call");
    let d = c.decision.as_ref().expect("the decision");
    // The advice survived the settle. It used to be dropped here.
    assert!(d.advice.is_some(), "the oracle's reply was carried across");

    let open = call_card(c, &plain_cfg(120), 0, Fold::Open, true).join("\n");
    assert!(open.contains("allowed, by human dead"), "{open}");
    // The operator's own words, under the operator's name.
    assert!(
        open.contains("human: dead chose `allow_once` at the head"),
        "{open}"
    );
    // The oracle's, under the oracle's, with what it would have done and what
    // it grounded that in.
    assert!(
        open.contains("oracle (glm-5.3-flash, 2100ms) would admit"),
        "{open}"
    );
    assert!(open.contains("clean rebuild in this turn"), "{open}");
    assert!(
        open.contains("oracle cited: rebuild it from scratch"),
        "{open}"
    );
    // And the thing that must never happen again.
    assert!(
        !open.contains("oracle (glm-5.3-flash, 2100ms) would admit: dead chose"),
        "the operator's words are never the oracle's: {open}"
    );
}

/// An oracle that grounded its verdict in nothing is a different fact from one
/// that cited four utterances, and a blank makes them look the same.
#[test]
fn an_oracle_that_cited_nothing_says_so_loudly() {
    let d = SettledDecision {
        req_id: "r1".into(),
        // A permission: the rendering this test is about is the ladder's.
        kind: "permission".into(),
        call_id: Some("c1".into()),
        summary: "run it".into(),
        outcome: letibot_sessionlog::event::DecisionOutcome::Selected {
            option_id: "allow".into(),
        },
        by: letibot_sessionlog::event::Decider {
            kind: "human".into(),
            identity: "dead".into(),
        },
        basis: "dead chose `allow` at the head".into(),
        advice: Some(letibot_sessionlog::event::ModelAdvice {
            consulted: true,
            would: "admit".into(),
            by: "glm".into(),
            basis: "it looks routine".into(),
            cites: Vec::new(),
            latency_ms: 40,
            unsure: None,
        }),
        late: false,
    };
    let lines = decision_detail(&d, 200).join("\n");
    assert!(lines.contains("oracle cited: nothing"), "{lines}");
    assert!(lines.contains("could not ground this"), "{lines}");
}

/// The approval is a fact about the call, and the call outlives the live card:
/// the moment the result row lands the transcript takes it over, and the
/// decision has to ride the row — folded to one line, open to the brief and the
/// reply — or it leaves the screen with the card.
#[test]
fn a_settled_card_keeps_the_decision_that_gated_it() {
    let mut a = app();
    // A turn with one call, gated by a permission the oracle allowed.
    a.apply(ServerFrame::Event(env(0, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        1,
        testing::proposed("t1", "c1", "bash"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        testing::requested("r1", "run rm -rf"),
    )));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::DecisionAnswered {
            req_id: "r1".into(),
            outcome: letibot_sessionlog::event::DecisionOutcome::Selected {
                option_id: "allow".into(),
            },
            by: letibot_sessionlog::event::Decider {
                kind: "model".into(),
                identity: "oracle".into(),
            },
            basis: "the operator asked for this".into(),
            late: false,
        },
    )));
    // The call runs and its result row lands in the transcript.
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: "exec".into(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "d".into(),
            inline_bytes: 12,
            full_bytes: 12,
            spill: None,
            repairs: 0,
            edit: None,
        },
    )));
    a.apply(ServerFrame::Event(env(
        6,
        testing::appended("i1", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        7,
        SessionEvent::TranscriptContent {
            item_id: "i1".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "done".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(8, testing::turn_finished("t1"))));
    // The decision rode the live card; the result row took the call over, and
    // the approval carried across with it, keyed by the row's item id.
    assert!(
        a.call_decisions.contains_key("i1"),
        "the decision carried across"
    );
    // Folded: one dim line, who decided and how.
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("allowed, by model oracle"), "{screen}");
    assert!(
        !screen.contains("oracle:"),
        "folded shows no reply: {screen}"
    );
    // Open: what was asked and what the oracle said back. The fold is a render
    // input, so the history cache has to be told the rows can render differently.
    a.tools = Fold::Open;
    a.invalidate_history();
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("asked: run rm -rf"), "{screen}");
    // Labelled by the decider — a model chose this one — and never as the
    // oracle's, which nothing here was.
    assert!(
        screen.contains("model: the operator asked for this"),
        "{screen}"
    );
    assert!(screen.contains("no oracle was consulted"), "{screen}");
}

/// **The prompt is a control, not a spelling test.**
///
/// Up and Down walk the ladder and Enter takes the highlighted one — the operator
/// never types a word they can get wrong. Reported twice as *"it wasn't a choice
/// but something I have to type (and mistype) myself"* before it was built.
#[test]
fn a_decision_is_answered_with_the_arrows_and_enter() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
    let opts: Vec<String> = a.open_decisions()[0]
        .options
        .iter()
        .map(|o| o.option_id.clone())
        .collect();
    assert!(opts.len() >= 2, "need a ladder to walk: {opts:?}");

    // Enter with nothing typed takes the FIRST option, because a fresh question
    // starts at the top of its own ladder.
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "r1".into(),
            option_id: opts[0].clone(),
            pattern: None,
            note: None
        })
    );

    // Down moves one, and the answer follows the marker rather than the order the
    // options happen to arrive in.
    let mut b = app();
    b.apply(ServerFrame::Event(env(1, testing::requested("r2", "rm"))));
    assert_eq!(b.key(Key::Down), None, "moving is not answering");
    assert_eq!(
        b.key(Key::Enter),
        Some(Action::Answer {
            req_id: "r2".into(),
            option_id: opts[1].clone(),
            pattern: None,
            note: None
        })
    );

    // Up from the top wraps to the bottom rather than sticking, so the last option
    // — which is usually the one that denies — is one keypress away.
    let mut c = app();
    c.apply(ServerFrame::Event(env(1, testing::requested("r3", "rm"))));
    assert_eq!(c.key(Key::Up), None);
    assert_eq!(
        c.key(Key::Enter),
        Some(Action::Answer {
            req_id: "r3".into(),
            option_id: opts[opts.len() - 1].clone(),
            pattern: None,
            note: None
        })
    );
}

#[test]
fn an_open_decision_is_answered_by_typing_the_option() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
    typed(&mut a, "deny");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "r1".into(),
            option_id: "deny".into(),
            pattern: None,
            note: None
        })
    );
    assert_eq!(a.input(), "", "the typed id is consumed, not held");
}

/// A digit typed into a line that has already started is part of the line,
/// not an answer — the same empty-composer guard the arrows have.
#[test]
fn a_digit_inside_a_half_typed_line_is_not_an_answer() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::requested("r1", "`bash` wants exec access"),
    )));
    typed(&mut a, "give me ");
    assert_eq!(a.key(Key::Char('2')), None, "it went into the line");
    assert_eq!(a.input(), "give me 2");
}

#[test]
fn the_ladder_survives_a_card_whose_content_is_taller_than_the_screen() {
    let mut a = tall_card(true);
    let screen = a.screen(80, 24).join("\n");

    // **The answer is on the screen**, all of it: every option, and the line that
    // says how to press one. This is the whole of the requirement.
    for opt in ["allow_once", "allow_session", "deny"] {
        assert!(screen.contains(opt), "`{opt}` was trimmed away:\n{screen}");
    }
    assert!(
        screen.contains("↑↓ to choose · Enter to answer"),
        "the hint is the fourth thing the loop used to eat:\n{screen}"
    );
    // **And the wall is where it belongs: above, with a seam.** The count is the
    // disclosure — how much is out of view, not that there is more.
    let has_seam = screen
        .lines()
        .any(|l| l.contains("line(s) out of view") && l.contains("pgdn scrolls"));
    assert!(has_seam, "no seam over a windowed card:\n{screen}");
    // The content starts at its head: the question is the first thing on the card.
    let at_headline = screen.find("? edit a file [permission]").expect("the ask");
    let at_frame = screen.find("crates/tui/src/app.rs").expect("the target");
    let at_seam = screen.find("line(s) out of view").expect("the seam");
    let at_ladder = screen.find("allow_once").expect("the ladder");
    assert!(
        at_headline < at_frame && at_frame < at_seam && at_seam < at_ladder,
        "the card is not read top to bottom:\n{screen}"
    );
    // The content was NOT paid for by the ladder's rows: what is missing is content,
    // and the seam says exactly how much.
    assert!(
        !screen.contains("line 39 of layer A"),
        "the whole wall fitted — the premise is wrong:\n{screen}"
    );
}

/// **§1.6: the card says how long there is, and what silence will do.**
///
/// Both facts were already on the wire (`DecisionRequested` carries `deadline` and
/// `on_timeout`) and neither was drawn, while this head **already drew a countdown
/// on the secret card**. The instrument existed; nothing had pointed it at the gate,
/// where the consequence of silence is a decision rather than a missing password.
///
/// Four things, and they are one requirement: the ladder on the card, the
/// consequence clause, the expired-card sentence, and **nothing at all** when the
/// deadline is null.
#[test]
fn the_gate_card_says_how_long_there_is_and_what_silence_does() {
    use letibot_sessionlog::event::{OnTimeout, OptionKind};
    let ladder = |deadline: u64, now: u64, on: OnTimeout| {
        let mut a = app();
        a.clock(now);
        let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
        d.deadline = Some(deadline);
        d.on_timeout = on;
        a.decision_lines(&d, 200).join("\n")
    };

    // **The ladder, on the card.** Whole minutes while there is time to think,
    // rounded up; seconds from where the number is acted on.
    const NOW: u64 = 1_788_984_000_000;
    assert!(
        ladder(NOW + 300_000, NOW, OnTimeout::Deny).contains("expires in 5 min"),
        "the real 300 s budget"
    );
    assert!(
        ladder(NOW + 181_000, NOW, OnTimeout::Deny).contains("expires in 4 min"),
        "rounded up, not down"
    );
    assert!(
        ladder(NOW + 119_000, NOW, OnTimeout::Deny).contains("1m59s left"),
        "seconds once the number is acted on"
    );
    assert!(
        ladder(NOW + 47_000, NOW, OnTimeout::Deny).contains("47s left"),
        "and whole seconds under a minute"
    );
    assert!(
        !ladder(NOW + 47_500, NOW, OnTimeout::Deny).contains("47.5"),
        "never a decimal on a whole second"
    );

    // **What silence does, in the daemon's three words.** `RUNS` is upper case
    // because it is the only one of the three that does something nobody asked for.
    let deny = ladder(NOW + 300_000, NOW, OnTimeout::Deny);
    assert!(deny.contains("if nobody answers, nothing runs"), "{deny}");
    let allow = ladder(NOW + 300_000, NOW, OnTimeout::Allow);
    assert!(
        allow.contains("if nobody answers, it RUNS anyway"),
        "{allow}"
    );
    let ask = ladder(NOW + 300_000, NOW, OnTimeout::Ask);
    assert!(
        ask.contains("if nobody answers, the guard model decides"),
        "{ask}"
    );

    // **Below the options**, because the order a person reads is the question, the
    // choices, then what happens if they do nothing.
    let at_clause = deny.find("if nobody answers").expect("the clause");
    let at_options = deny.find("allow_once").expect("the ladder");
    assert!(
        at_options < at_clause,
        "the consequence is read after the choices: {deny}"
    );

    // **A card past its own deadline** — the case the operator actually hit. It says
    // the clock ran out and the outcome is unreported, it **keeps** the consequence
    // (that is a rule, not an observation, and with `allow` it is the one thing worth
    // learning from an expired card), and it never counts into negative seconds.
    let expired = ladder(NOW - 14_000, NOW, OnTimeout::Allow);
    assert!(
        expired.contains("past its deadline by 14s; the daemon has not said what became of it"),
        "{expired}"
    );
    assert!(expired.contains("it RUNS anyway"), "{expired}");
    assert!(
        !expired.contains("-14s") && !expired.contains("-"),
        "a countdown below zero is a rendering fault: {expired}"
    );

    // **No deadline, nothing drawn.** `null` is §11.5's *wait forever* — a policy,
    // not missing information — so there is no silence for a clause to describe and
    // no countdown to draw. §13.2b, answered the other way round from `unreadable 0`.
    let mut a = app();
    let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    d.deadline = None;
    let bare = a.decision_lines(&d, 200).join("\n");
    assert!(!bare.contains("expires in"), "{bare}");
    assert!(!bare.contains("s left"), "{bare}");
    assert!(!bare.contains("if nobody answers"), "{bare}");
    assert!(
        bare.contains("allow_once"),
        "the card is still drawn: {bare}"
    );
}

/// **A question's rows must not be able to answer a permission, or the reverse.**
///
/// `decision_rows` is the one place the two kinds' row counts are reconciled, and
/// getting it wrong is silent: a `question` read as a `permission` shows an empty
/// ladder (which is the defect above), and a `permission` read as a `question` would
/// answer `option: None` with the option's label as free text — a grant nobody gave,
/// which is worse than not answering.
#[test]
fn the_row_count_is_the_kinds_own_field_in_both_directions() {
    use letibot_sessionlog::event::OptionKind;
    let permission = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    assert_eq!(
        decision_rows(&permission),
        2,
        "a permission counts `options`"
    );
    assert!(
        permission.choices.is_empty(),
        "the premise: it has no choices"
    );

    let mut q = decision_with(&[]);
    q.kind = "question".into();
    q.choices = vec!["a".into(), "b".into(), "c".into()];
    assert_eq!(decision_rows(&q), 3, "a question counts `choices`");
    assert!(q.options.is_empty(), "the premise: it has no options");

    // And the answers go through different frames, which is the reason the two
    // kinds are not merged into one ladder.
    let mut a = app();
    a.open.push(q.clone());
    assert!(matches!(
        a.key(Key::Enter),
        Some(Action::AnswerQuestion { .. })
    ));
    let mut b = app();
    b.open.push(permission);
    assert!(matches!(b.key(Key::Enter), Some(Action::Answer { .. })));
}

/// **leticl's finding, checked here: does the SNAPSHOT arm draw what the live arm
/// draws?**
///
/// leticl found a card arriving inside a snapshot drawing `expires in 29833973 min`
/// — fifty-six years — because its live path converted a wire deadline into the
/// head's own clock and its snapshot path did not. That is the shape this head has
/// met twice already: R16's fork (the live arm marks the pending echo, the snapshot
/// arm has to be told to) and R17's `orphan_bodies` (the live arm files a row, a
/// snapshot replaces the table). So it is checked rather than assumed, which is what
/// the operator asked for.
///
/// **This head needs no conversion**, and that is a claim with a test rather than a
/// sentence: the wire deadline is `letibot_sessionlog::event::now_ms()` — the epoch —
/// and this head's clock is the epoch too (`bin::now_ms`, `driver::now_ms`), so a
/// snapshot's deadline subtracts from `now_ms` with no constant between the two. The
/// assertion is that a card from a snapshot reads the same number a live one does, at
/// the daemon's real 300 s budget.
/// **The card names the files the action would write** — R35's severed wire, joined.
///
/// The operator: *"it cant catch those pesky python edits"*. It can — `write_targets` has
/// resolved assigned names, modes and receiver-paths since it was written, built from their own
/// card shape — and nothing showed them. A `bash` call running a script that rewrites a file drew
/// a card that did not name the file.
///
/// Three assertions, and the second and third are the ones that keep it honest:
///
///   * the paths are ON the card, in the target's own register and place;
///   * **an unresolved path is drawn differently from a resolved one** — a write whose target
///     could not be read is the case a person most needs to see, and drawing it as a path is
///     drawing it as something to skim past;
///   * **an empty list draws NOTHING.** *No write the scanner could place* is not *writes
///     nothing*, and a card claiming the second would be asserting a negative the classifier
///     cannot support — which is R35's own subject one layer over.
#[test]
fn the_card_names_the_files_the_action_would_write() {
    use letibot_sessionlog::event::{OptionKind, WriteTarget};

    let mut a = app();
    // The operator's own case: a script that rewrites one file, approved as an exec.
    let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    d.access = "exec".into();
    d.target = "python3 edit.py".into();
    d.write_targets = vec![WriteTarget {
        path: "src/syntax.rs".into(),
        unresolved: false,
    }];
    let one = a.decision_lines(&d, 200).join("\n");
    assert!(
        one.contains("src/syntax.rs"),
        "the file the script writes is not on the card: {one}"
    );
    // One path gets NO header, which is what keeps an `edit` card from growing a line.
    assert!(
        !one.contains("1 files:"),
        "a single file grew a count: {one}"
    );

    // **Several, and the count goes above the names** so a viewport cannot hide it: a window that
    // shows two of five still says five.
    d.write_targets = vec![
        WriteTarget {
            path: "a.rs".into(),
            unresolved: false,
        },
        WriteTarget {
            path: "b.rs".into(),
            unresolved: false,
        },
        WriteTarget {
            path: "c.rs".into(),
            unresolved: false,
        },
        WriteTarget {
            path: "d.rs".into(),
            unresolved: false,
        },
        WriteTarget {
            path: "e.rs".into(),
            unresolved: false,
        },
    ];
    let many = a.decision_lines(&d, 200).join("\n");
    assert!(many.contains("5 files:"), "no count for five: {many}");
    let count_at = many.find("5 files:").unwrap();
    let first_path = many.find("a.rs").unwrap();
    assert!(
        count_at < first_path,
        "the count is below the names: {many}"
    );

    // **An unresolved target is not drawn as a path.** `Path.home() / name` is the case the
    // requirement calls the one a person most needs to see.
    d.write_targets = vec![
        WriteTarget {
            path: "src/syntax.rs".into(),
            unresolved: false,
        },
        WriteTarget {
            path: "Path.home() / argv[1]".into(),
            unresolved: true,
        },
    ];
    let mixed = a.decision_lines(&d, 200).join("\n");
    assert!(
        mixed.contains("Path.home() / argv[1]"),
        "the unresolved target is not named at all: {mixed}"
    );
    assert!(
        mixed.contains("a write whose target could not be read"),
        "an unresolved target is drawn as though it were a path: {mixed}"
    );

    // **And an empty list says nothing.** This is the assertion that keeps *empty* from meaning
    // *writes nothing*.
    d.write_targets = Vec::new();
    let none = a.decision_lines(&d, 200).join("\n");
    assert!(
        !none.contains("files:") && !none.contains("could not be read"),
        "an empty list drew a claim about writing nothing: {none}"
    );
}

#[test]
fn a_deadline_arriving_in_a_snapshot_reads_the_same_as_one_arriving_live() {
    use letibot_sessionlog::event::OptionKind;
    const NOW: u64 = 1_788_984_000_000;
    let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    d.deadline = Some(NOW + 300_000);

    // The live arm's renderer, which §1.6's test covers at length.
    let mut live = app();
    live.clock(NOW);
    let from_live = live.decision_lines(&d, 200).join("\n");
    assert!(from_live.contains("expires in 5 min"), "{from_live}");

    // **The snapshot arm** — the half that goes wrong in this family: the same card,
    // arriving in a view rather than as an event.
    let mut snap = app();
    snap.clock(NOW);
    snap.apply(ServerFrame::Hello {
        protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
        session_id: "s".into(),
        head_id: "h1".into(),
        dropped: 0,
        snapshot: Some(Box::new(Snapshot {
            session_id: "s".into(),
            seq: 0,
            dropped: 0,
            items_dropped: 0,
            items: Vec::new(),
            turn: None,
            open_decisions: vec![d.clone()],
            settled_decisions: Vec::new(),
            warnings: Vec::new(),
            heads: Vec::new(),
            subagents: Vec::new(),
        })),
        resumed_from: None,
        scrubbed: Default::default(),
        wiring: wiring(),
        sessions: Vec::new(),
    });
    let from_snapshot = snap.screen(200, 30).join("\n");
    assert!(
        from_snapshot.contains("expires in 5 min"),
        "the snapshot arm drew a different countdown from the live one — and if the \
             number is astronomical that is exactly the shape leticl found:\n{from_snapshot}"
    );
    // The year-scale shape, refused by name: five figures before the unit is not a
    // countdown, it is a second epoch wearing one.
    assert!(
        !from_snapshot.contains("000 min"),
        "a second epoch in minutes:\n{from_snapshot}"
    );
}

/// **A refused call is not a decision, so nothing about deadlines is drawn on one.**
/// The clause belongs to a card that will wait for an answer, and a card that has
/// already settled cannot be left to expire again.
#[test]
fn the_deadline_line_is_only_on_a_card_that_is_actually_open() {
    use letibot_sessionlog::event::OptionKind;
    let mut a = app();
    a.clock(1_788_984_000_000);
    // The settled-decision note is a different renderer (it has no options to answer
    // and no clock of its own), and this asserts the pair stays apart: the open card
    // says it, the settled row does not repeat it.
    let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    d.deadline = Some(1_788_984_300_000);
    assert!(
        a.decision_lines(&d, 200)
            .join("\n")
            .contains("expires in 5 min")
    );
}

/// **A note about the decision does not outlive the decision.**
///
/// The gate's "asking the guard" arrives as a `ToolProgress` while the call is
/// still `Proposed`. It used to be cleared only by `ToolFinished`, so a call
/// that took a long time to RUN carried a sentence about a wait that had
/// already ended. Measured 2026-09-20: a 300 s `cargo test` sat under "asking
/// the guard" for all five minutes and was reported as an oracle hang.
#[test]
fn the_guards_note_does_not_survive_the_tool_starting() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(0, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        1,
        testing::proposed("t1", "c1", "bash"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::ToolProgress {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            note: "asking the guard".into(),
        },
    )));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("asking the guard"),
        "while deciding: {screen}"
    );

    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        !screen.contains("asking the guard"),
        "the note outlived the wait: {screen}"
    );
}
