//! The permission card: a decision's ladder, its answers, its settled form.

use super::*;

/// The card states why it is asking, in the words of whoever is stuck: `because` is the model's own
/// line, and a question with no answer to *why are you asking me this* leaves the person unable to
/// decide whether it is worth their time.
#[test]
fn the_card_states_why_it_is_asking_in_the_speakers_own_words() {
    use letibot_sessionlog::event::OptionKind;
    let a = app();

    // Nothing to say, so nothing said, in both directions - a `because:`
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
    // Labelled with its speaker, in the same dim register every other borrowed sentence on this card
    // uses - the oracle's verdict included - because both are evidence rather than the question.
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
    // And it sits with the rest of the evidence, above the ladder - read before
    // the choice is made, which is the only time it can inform one.
    let because_at = drawn.find("because:").unwrap();
    let ladder_at = drawn.find("Allow once").unwrap();
    assert!(because_at < ladder_at, "{drawn}");
}

/// The card says WHY it is asking, and only where the reason is true: the headline says what the
/// tool declares and the dim line is the classifier's reading of the action, so without a clause
/// joining them the card reads as arguing with its own greening. A `read` gets no such clause.
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

/// A card nobody took to an oracle says so, and a card that did does not: *no oracle was consulted*
/// and *the oracle was asked and said nothing* are different facts, and rendering them identically
/// makes the screen say the same nothing for both.
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

    // A question is not a gate, so it says neither. Nothing is ever consulted
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

/// A typed line is the marked rung's material, not a selector: on an always-allow it is the rule's
/// coverage, on a refusal it is the reason the model is given, and on a rung with no use for words
/// nothing is answered at all.
///
/// > *"please add globbing to my answers somehow too"*
#[test]
fn a_typed_line_is_the_marked_rungs_material() {
    use crate::ui::cards::decision::{MarkedWords, Words, marked_words, words_for};
    use letibot_sessionlog::event::OptionKind;
    let d = decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::AllowAlways,
        OptionKind::RejectOnce,
        OptionKind::RejectAlways,
    ]);

    // Which rung takes what, read off the kind the daemon sent.
    assert_eq!(words_for(&d, 0), Words::None);
    assert_eq!(words_for(&d, 1), Words::Glob);
    assert_eq!(words_for(&d, 2), Words::Reason);
    assert_eq!(words_for(&d, 3), Words::Reason);

    // A glob is a path, so it rides verbatim - it is not an id, and nothing about it is case-folded.
    assert_eq!(
        marked_words(
            d.options[1].kind,
            &d.options[1].label,
            " crates/**/Cargo.toml "
        ),
        Ok(MarkedWords::Glob("crates/**/Cargo.toml".into()))
    );
    assert_eq!(
        marked_words(
            d.options[2].kind,
            &d.options[2].label,
            "use the scratch dir"
        ),
        Ok(MarkedWords::Reason("use the scratch dir".into()))
    );

    // Nothing typed carries nothing, on every rung: silence is not a reason and not a rule.
    assert_eq!(
        marked_words(d.options[1].kind, "", ""),
        Ok(MarkedWords::Bare)
    );
    assert_eq!(
        marked_words(d.options[2].kind, "", "   "),
        Ok(MarkedWords::Bare)
    );

    // A rung that takes no words refuses them rather than dropping them: somebody who typed a
    // sentence meant it, and an answer that discarded it is an answer they did not give.
    let refused = marked_words(d.options[0].kind, "Allow this one", "because I said so")
        .expect_err("a grant of one call takes no words");
    assert!(refused.contains("Allow this one"), "{refused}");
    assert!(refused.contains("ctrl-c clears"), "{refused}");
}

/// A refusal carries the operator's own words, on either refusal rung: the card says where they
/// will go, and the label is one line rather than a paragraph about the mode.
#[test]
fn a_refusal_carries_the_operators_words() {
    use letibot_sessionlog::event::OptionKind;
    let mut d = decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::AllowAlways,
        OptionKind::RejectAlways,
    ]);
    d.options[2].label = "Deny, and tell the model why".into();
    let mut a = app();
    a.open.push(d.clone());

    // The mark on the refusal, and the words typed beside it: the preview says where they go.
    typed(&mut a, "Use the scratch dir, not /tmp.");
    a.sel = 2;
    let drawn = a.decision_lines(&d, 120).join("\n");
    assert!(
        drawn.contains("tells the model: Use the scratch dir, not /tmp."),
        "the card did not say what the words will do:\n{drawn}"
    );
    assert!(
        drawn.contains("type the reason"),
        "and the hint names the choice, one spelling with the bar:\n{drawn}"
    );
    // Enter spends them on the answer, and the line is emptied: they are the reason now, not a
    // message the next Enter would send.
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "d1".into(),
            option_id: "deny_always".into(),
            pattern: None,
            note: Some("Use the scratch dir, not /tmp.".into())
        })
    );
    assert_eq!(a.input(), "", "the reason went with the answer");

    // The same words on the plain refusal rung, which is the rung a person reaches for: one rule for
    // every refusal, because the gate reads the reason off the answer and not off the rung's name.
    let mut b = app();
    let plain = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    let req = crate::ui::cards::decision::words_for(&plain, 1);
    assert_eq!(req, crate::ui::cards::decision::Words::Reason);
    b.open.push(plain);
    typed(&mut b, "not /tmp");
    b.sel = 1;
    assert_eq!(
        b.key(Key::Enter),
        Some(Action::Answer {
            req_id: "d1".into(),
            option_id: "deny".into(),
            pattern: None,
            note: Some("not /tmp".into())
        })
    );
}

/// A rung that takes no words answers nothing while the composer holds some, and says where they
/// could go: the line is not dropped, and the card is not answered with words it would discard.
#[test]
fn a_rung_that_takes_no_words_refuses_them() {
    use letibot_sessionlog::event::OptionKind;
    let mut a = app();
    a.open.push(decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::RejectOnce,
    ]));
    typed(&mut a, "a message for the model");
    assert_eq!(a.key(Key::Enter), None, "nothing is answered");
    assert_eq!(a.input(), "a message for the model", "and nothing is lost");
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("takes no words"), "{said}");
}

/// Why a rung is missing is ON the card, under the ladder it belongs to: the gate leaves a rung off
/// rather than showing a button whose effect it would decline, and a rung that vanishes in silence
/// reads as *maybe I misremembered what I was offered*.
#[test]
fn the_card_draws_the_reason_a_rung_is_missing() {
    use letibot_sessionlog::event::OptionKind;
    let a = app();

    // Nothing left off: nothing said. A complete ladder is not a card apologising.
    let complete = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    assert!(
        !a.decision_lines(&complete, 120)
            .iter()
            .any(|l| l.contains("not offered for the session")),
        "a complete ladder explained an absence it does not have"
    );

    let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    d.options_note =
        "not offered for the session: the command's meaning is not resolved — `$PWD/target`".into();
    let lines = a.decision_lines(&d, 120);
    let note = lines
        .iter()
        .find(|l| l.contains("not offered for the session"))
        .expect("the card said nothing about the rung it dropped");
    assert!(
        note.contains("`$PWD/target`"),
        "the note does not name the word a person would rewrite: {note}"
    );
    // Under the ladder, not buried in the evidence above it: it is a fact about what can
    // be answered, and the answer half is the one that is never trimmed away.
    let note_at = lines
        .iter()
        .position(|l| l.contains("not offered for the session"))
        .unwrap();
    let ladder_at = lines.iter().position(|l| l.contains("Allow once")).unwrap();
    assert!(note_at > ladder_at, "{}", lines.join("\n"));
}

/// The hint describes the rung that is marked, and only it: the glob's clause appears when the
/// always-allow is marked, the reason's when a refusal is, and neither on a grant of one call.
#[test]
fn the_hint_names_what_the_marked_rung_takes() {
    use letibot_sessionlog::event::OptionKind;
    let ladder = [
        OptionKind::AllowOnce,
        OptionKind::AllowAlways,
        OptionKind::RejectOnce,
    ];
    let hint = |at: usize| {
        let mut a = app();
        a.open.push(decision_with(&ladder));
        a.sel = at;
        a.decision_lines(&a.open[0].clone(), 100).join("\n")
    };

    // The selectors, and only them: no id is typed and nothing suggests one.
    for at in 0..3 {
        let drawn = hint(at);
        assert!(
            drawn.contains("↑↓ or a row number marks · enter answers"),
            "the ladder's own sentence:\n{drawn}"
        );
        assert!(!drawn.contains("type the id"), "no id is typed:\n{drawn}");
        assert!(
            !drawn.contains("<glob>"),
            "and no spelling of one:\n{drawn}"
        );
    }
    assert!(
        !hint(0).contains("type the"),
        "a grant of one call takes no words"
    );
    assert!(
        hint(1).contains("type the rule"),
        "an always-allow takes the glob: {}",
        hint(1)
    );
    assert!(
        hint(2).contains("type the reason"),
        "a refusal takes the reason: {}",
        hint(2)
    );
}

/// The command is on a line of its own, and the taxonomy is not in front of it: with the target
/// inside a sentence and the classifier's reading appended to it, a long command wrapped into the
/// middle of a wall and the operator could not see what they were approving.
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

    // The command, alone on its line, indented - not embedded in the question.
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

/// The card says why it is asking when the declaration is the reason: the headline's `wants exec
/// access` and the dim line's reading of the action looked like a contradiction with nothing
/// joining them. It is absent where the access is not what asks, and an empty one is not guessed.
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

    // Absent where the access is not what asks - a read call, a write call, a
    // network call, and a daemon that did not say.
    for other in ["read", "write", "network", ""] {
        let drawn = says(other);
        assert!(
            !drawn.contains("the access is what asks"),
            "an access of {other:?} is not why anything is being asked: {drawn}"
        );
        // The card is still a card.
        assert!(drawn.contains("Allow once"), "{drawn}");
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

/// Five facts, five lines: three real frames rendered identically, so five distinct facts reached
/// the glass as one line. The assertion is pairwise difference - a test that only checked the field
/// is carried would pass against a head that ignored it.
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
    let _ = a.screen(120, 16);
    // Not a row of the conversation: a decision is the operator's own act, and its record is
    // `/notes` - which is where a reader looks for what they answered.
    let listed = a.notes_lines().join("\n");
    assert!(listed.contains("rm -rf /"), "{listed}");
    assert!(listed.contains("REFUSED"), "{listed}");
}

/// A question the person answered is not a permission's verdict: the settled event says `Cancelled`
/// because `DecisionOutcome` is a permission's vocabulary, so a head reading the word alone would
/// tell the person they had refused the answer they just gave.
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
    let _ = a.screen(160, 24);
    // `/notes` holds the answer, and it is attributed and worded as the answer it is - never as
    // one of a permission's four words.
    let screen = a.notes_lines().join("\n");
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
    // The basis is labelled by whoever DECIDED - here a model - and never as
    // the oracle's, which it is not. No oracle advised this one, and the card
    // says so rather than leaving a blank that reads like silence.
    assert!(
        open.contains("model: the operator asked for this"),
        "{open}"
    );
    assert!(open.contains("no oracle was consulted"), "{open}");
}

/// The bug this labelling was built for: under `/supervise` the oracle advises and the OPERATOR
/// answers, so the decision carries two reasons. Rendered as one line they printed the operator's
/// own words under the oracle's name - a lie that reads exactly like the truth.
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
            options_note: String::new(),
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
    // The advice survives the settle.
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

/// The approval is a fact about the call, and the call outlives the live card: once the result row
/// lands the transcript takes it over, so the decision has to ride the row - folded to one line,
/// open to the brief and the reply - or it leaves the screen with the card.
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
            edits: Vec::new(),
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
                edits: Vec::new(),
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
    // Labelled by the decider - a model chose this one - and never as the
    // oracle's, which nothing here was.
    assert!(
        screen.contains("model: the operator asked for this"),
        "{screen}"
    );
    assert!(screen.contains("no oracle was consulted"), "{screen}");
}

/// The prompt is a control, not a spelling test: Up and Down walk the ladder and Enter takes the
/// highlighted one, so the operator never types a word they can get wrong.
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
    // - which is usually the one that denies - is one keypress away.
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
fn an_open_decision_is_not_answered_by_typing_an_option_id() {
    use letibot_sessionlog::event::OptionKind;
    // Digits and the arrows are the selectors. A typed line is the marked rung's material, so an id
    // typed on a rung that takes no words answers nothing at all - and the line stays where it is.
    let mut a = app();
    a.open.push(decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::RejectOnce,
    ]));
    typed(&mut a, "deny");
    assert_eq!(a.key(Key::Enter), None, "a typed id selects nothing");
    assert_eq!(a.input(), "deny", "the line is still the operator's");

    // The same answer, taken the way the card says: the digit marks the rung and Enter answers it.
    a.clear_composer();
    assert_eq!(a.key(Key::Char('2')), None, "a move, not an answer");
    assert_eq!(a.sel, 1, "the refusal is marked");
    assert_eq!(
        a.submit("deny".into()),
        Some(Action::Answer {
            req_id: "d1".into(),
            option_id: "deny".into(),
            pattern: None,
            note: Some("deny".into())
        }),
        "and the words on it are the reason the model is given"
    );
}

/// A digit typed into a line that has already started is part of the line,
/// not an answer - the same empty-composer guard the arrows have.
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

/// A rung is the answer, not its id: the card drew `Allow session  (allow_session)`, an id
/// beside a label that already says it, on every row. A digit and the arrows are the selectors, so
/// the row carries the label alone.
#[test]
fn the_ladder_rows_carry_no_option_id() {
    use letibot_sessionlog::event::OptionKind;
    let a = app();
    let d = decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::AllowSession,
        OptionKind::RejectOnce,
    ]);
    let drawn = a.decision_lines(&d, 200).join("\n");
    for id in ["allow_once", "allow_session", "(deny)"] {
        assert!(
            !drawn.contains(id),
            "`{id}` is drawn beside a row that already says the answer:\n{drawn}"
        );
    }
    // And the rung is still one: its number, the marker on the one Enter takes, its words.
    assert!(drawn.contains("▸ 1  Allow once"), "{drawn}");
    assert!(drawn.contains("2  Allow session"), "{drawn}");
    assert!(drawn.contains("3  Deny"), "{drawn}");
    // The marked rung is still the one in reverse video, on a head that emits colour.
    let painted = App::new(RenderConfig {
        width: 200,
        color: true,
        ..RenderConfig::default()
    });
    let row = painted
        .decision_lines(&d, 200)
        .into_iter()
        .find(|l| l.contains("Allow once"))
        .expect("the first rung");
    assert!(row.contains(sgr::REVERSE), "not the marked row: {row:?}");
}

#[test]
fn the_ladder_survives_a_card_whose_content_is_taller_than_the_screen() {
    let mut a = tall_card(true);
    let screen = a.screen(80, 24).join("\n");

    // The answer is on the screen, all of it: every option, and the line that
    // says how to press one. This is the whole of the requirement.
    for opt in ["Allow once", "Allow session", "Deny"] {
        assert!(screen.contains(opt), "`{opt}` was trimmed away:\n{screen}");
    }
    assert!(
        screen.contains(crate::ui::cards::decision::LADDER_HINT),
        "the hint is the fourth thing the loop used to eat:\n{screen}"
    );
    // And the wall is where it belongs: above, with a seam. The count is the
    // disclosure - how much is out of view, not that there is more.
    let has_seam = screen
        .lines()
        .any(|l| l.contains("line(s) out of view") && l.contains("pgdn scrolls"));
    assert!(has_seam, "no seam over a windowed card:\n{screen}");
    // The content starts at its head: the question is the first thing on the card.
    let at_headline = screen.find("? edit a file [permission]").expect("the ask");
    let at_frame = screen.find("crates/tui/src/app.rs").expect("the target");
    let at_seam = screen.find("line(s) out of view").expect("the seam");
    let at_ladder = screen.find("Allow once").expect("the ladder");
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

/// The card says how long there is, and what silence will do: both facts were on the wire and
/// neither was drawn, while this head already drew a countdown on the secret card. Four things -
/// the ladder, the consequence clause, the expired sentence, and nothing at all with no deadline.
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

    // The ladder, on the card. Whole minutes while there is time to think,
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

    // What silence does, in the daemon's three words. `RUNS` is upper case
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

    // Below the options, because the order a person reads is the question, the
    // choices, then what happens if they do nothing.
    let at_clause = deny.find("if nobody answers").expect("the clause");
    let at_options = deny.find("Allow once").expect("the ladder");
    assert!(
        at_options < at_clause,
        "the consequence is read after the choices: {deny}"
    );

    // A card past its own deadline says the clock ran out and the outcome is unreported; it KEEPS
    // the consequence, which is a rule and the one thing worth learning from an expired card, and
    // it never counts into negative seconds.
    let expired = ladder(NOW - 14_000, NOW, OnTimeout::Allow);
    assert!(
        expired.contains("past its deadline by 14s; the daemon has not said what became of it"),
        "{expired}"
    );
    assert!(expired.contains("it RUNS anyway"), "{expired}");
    assert!(
        !expired.contains("-14s") && !expired.contains(" -1"),
        "a countdown below zero is a rendering fault: {expired}"
    );

    // No deadline, nothing drawn. `null` is the *wait forever* - a policy,
    // not missing information - so there is no silence for a clause to describe and
    // no countdown to draw. Answered the other way round from `unreadable 0`.
    let a = app();
    let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    d.deadline = None;
    let bare = a.decision_lines(&d, 200).join("\n");
    assert!(!bare.contains("expires in"), "{bare}");
    assert!(!bare.contains("s left"), "{bare}");
    assert!(!bare.contains("if nobody answers"), "{bare}");
    assert!(
        bare.contains("Allow once"),
        "the card is still drawn: {bare}"
    );
}

/// A question's rows must not be able to answer a permission, or the reverse. `decision_rows` is
/// the one place the two counts are reconciled and getting it wrong is silent: an empty ladder one
/// way, a grant nobody gave the other.
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

/// The card names the files the action would write, because the operator's *"it cant catch those
/// pesky python edits"* was a wire that was severed, not a classifier that could not see them. An
/// unresolved path is drawn differently from a resolved one, and an empty list draws nothing.
#[test]
fn the_card_names_the_files_the_action_would_write() {
    use letibot_sessionlog::event::{OptionKind, WriteTarget};

    let a = app();
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

    // Several, and the count goes above the names so a viewport cannot hide it: a window that
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

    // An unresolved target is not drawn as a path. `Path.home() / name` is the case the
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

    // And an empty list says nothing. This is the assertion that keeps *empty* from meaning
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

    // The live arm's renderer, which the test covers at length.
    let mut live = app();
    live.clock(NOW);
    let from_live = live.decision_lines(&d, 200).join("\n");
    assert!(from_live.contains("expires in 5 min"), "{from_live}");

    // The snapshot arm - the half that goes wrong in this family: the same card,
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
            items_lead: 0,
            items: Vec::new(),
            turn: None,
            open_decisions: vec![d.clone()],
            settled_decisions: Vec::new(),
            warnings: Vec::new(),
            heads: Vec::new(),
            subagents: Vec::new(),
            activity: letibot_sessionlog::activity::Activity::Quiet,
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

/// A refused call is not a decision, so nothing about deadlines is drawn on one.
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

/// A note about the decision does not outlive the decision: the gate's "asking the guard" arrives
/// while the call is `Proposed`, and cleared only by `ToolFinished` it sat under a 300 s run for
/// all five minutes, reported as an oracle hang.
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
