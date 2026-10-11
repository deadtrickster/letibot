//! Supervised points: the person and the model both consulted, and the corpus row.

use super::*;

fn supervised_gate(model_allows: bool, human_allows: bool) -> AdjudicatedGate {
    let advisor: std::sync::Arc<dyn Adjudicator> =
        std::sync::Arc::new(Fixed::new("model:test", model_allows));
    AdjudicatedGate::new(Box::new(Fixed::new("human:op", human_allows)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(advisor)
        .start_supervised(true)
}

/// **The person sees the model's verdict before answering.**
///
/// Without this the point's name is a lie: "ask me if i agree" requires that
/// there is something in front of me to agree with.
#[test]
fn the_human_is_shown_what_the_model_said() {
    let seen: std::sync::Arc<std::sync::Mutex<Option<Option<ModelAdvice>>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured = seen.clone();
    let human = AskAdjudicator::new("human:op", move |req: &AdjudicationRequest| {
        *captured.lock().unwrap() = Some(req.advice.clone());
        req.options
            .first()
            .map(|o| AdjudicationDecision::selected(req, &o.id, "human:op", "ok"))
    });
    let advisor: std::sync::Arc<dyn Adjudicator> =
        std::sync::Arc::new(Fixed::new("model:test", true));
    let mut g = AdjudicatedGate::new(Box::new(human))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(advisor)
        .start_supervised(true);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));

    let advice = seen.lock().unwrap().clone().expect("the human was asked");
    let advice = advice.expect("carrying the model's verdict");
    assert_eq!(advice.would, "admit");
    assert_eq!(advice.by, "model:test");
}

/// **The model advises and never decides.** The human's answer is the outcome,
/// whichever way the model went.
#[test]
fn the_person_overrules_the_model_in_both_directions() {
    // Model would allow, person refuses.
    let mut g = supervised_gate(true, false);
    assert!(matches!(
        g.admit(&call("edit", &json!({"path": "src/lib.rs"}))),
        GateDecision::Refuse { .. }
    ));
    assert_eq!(g.log[0].effect, "refuse");

    // Model would not allow, person admits.
    let mut g = supervised_gate(false, true);
    assert!(matches!(
        g.admit(&call("edit", &json!({"path": "src/lib.rs"}))),
        GateDecision::Admit
    ));
    assert_eq!(g.log[0].effect, "admit");
}

/// **One call, one labelled row** — the reason to sit through this point.
///
/// The model's verdict and the operator's ruling are both on the row, they are
/// different values, and the ruling did not overwrite the verdict. Nothing has
/// to be reconstructed afterwards from a list.
#[test]
fn a_supervised_call_produces_a_labelled_corpus_row_by_itself() {
    let mut g = supervised_gate(true, false);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let row = g.corpus().remove(0);

    assert!(row.asked, "a person answered this one");
    // The model's, not the person's wearing its name.
    let verdict = row.model_verdict.as_deref().expect("the model was asked");
    assert!(verdict.starts_with("admit by model:test"), "{verdict}");
    // The person's, separately.
    assert_eq!(row.verdict_by.as_deref(), Some("human:op"));
    assert_eq!(row.effect, "refuse");
    // And the label, computed from the two rather than typed later.
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("revoked"),
        "the model would have admitted and the person stopped it"
    );
    assert!(row.is_disagreement());

    // The other direction is the over-refusal, and it is `granted`.
    let mut g = supervised_gate(false, true);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let row = g.corpus().remove(0);
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("granted")
    );

    // Agreement is a label too. A corpus of corrections alone teaches that
    // every decision was wrong.
    let mut g = supervised_gate(true, true);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let row = g.corpus().remove(0);
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("upheld")
    );
    assert!(!row.is_disagreement());
}

/// **R11: the corpus keeps the bytes, and from the adjudicator that asked an
/// oracle.**
///
/// Two defects, one column each, and both were live:
///
/// * **`shown` held the person's card, not the oracle's brief.** The gate asked its
///   *decider* for `last_brief`, and at a supervised point the decider is the human
///   adjudicator, whose `last_brief` is `req.brief()` — the decision card, whose
///   first line is `decision — …`. Measured on this box 2026-09-21: `shown` was set
///   on 147 of 5785 rows and was `NULL` on every row an oracle decided, so the
///   corpus had 147 cards and **zero briefs**.
/// * **`reply` did not exist**, so the bytes a verdict was read out of were dropped
///   with the HTTP call that carried them. 1,192 rows are ones where the operator
///   and the model differ — *"the rows a fine-tune is for"* — and none of them had
///   its input.
///
/// The assertion is that both come from the ADVISOR, which the fixture stamps with
/// its own id so the two cannot be confused.
#[test]
fn the_corpus_keeps_the_oracles_brief_and_reply_not_the_persons_card() {
    let mut g = supervised_gate(true, false); // the model admits; the person stops it
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    assert!(g.corpus()[0].asked, "a person answered this one");
    let row = g.corpus().remove(0);

    let shown = row
        .shown
        .as_deref()
        .expect("an oracle was consulted, so the brief is on the row");
    assert!(
        shown.contains("model:test"),
        "not the oracle's brief: {shown}"
    );
    assert!(
        !shown.starts_with("decision —"),
        "the person's card is on a row an oracle decided: {shown}"
    );
    let reply = row
        .reply
        .as_deref()
        .expect("the oracle answered, so the reply is on the row");
    assert!(reply.contains("ALLOW 0"), "{reply}");

    // **And nothing is filed where nobody spoke.** At `always-ask` with no advisor
    // the gate consults no oracle, and both columns stay empty rather than carrying
    // the card or a previous call's reply.
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let row = g.corpus().remove(0);
    assert!(row.asked, "the person answered");
    assert_eq!(
        row.shown, None,
        "a person's answer is not an oracle's brief"
    );
    assert_eq!(row.reply, None, "and it is not a reply either");

    // **The model's own verdict admits, and that path uses to file nothing at all.**
    // It returns before the gate ever reaches its decider, so the brief and the reply
    // that admitted the call were dropped and `shown` kept whatever the previous call
    // had put there. Measured: 486 rows whose `verdict_by` names an oracle, and not
    // one of them had `shown`.
    let mut g = supervised_gate(true, true).with_mode(crate::mode::Mode::AUTO);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let row = g.corpus().remove(0);
    assert!(
        row.verdict_by.as_deref().unwrap_or("").contains("model"),
        "the model decided this one: {row:?}"
    );
    assert!(
        row.shown
            .as_deref()
            .is_some_and(|s| s.contains("model:test")),
        "the model decided and its brief was not filed: {:?}",
        row.shown
    );
    assert!(
        row.reply.as_deref().is_some_and(|r| r.contains("ALLOW 0")),
        "nor its reply: {:?}",
        row.reply
    );
}

/// **The adviser is not load-bearing.** An oracle that cannot answer leaves the
/// person answering exactly as they would at `always-ask`.
#[test]
fn a_silent_oracle_does_not_stop_the_person_being_asked() {
    struct Mute;
    impl Adjudicator for Mute {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Timeout,
                by: "model:test".into(),
                basis: "budget".into(),
                latency_ms: 400,
            }
        }
        fn describe(&self) -> String {
            "model:test".into()
        }
    }
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(std::sync::Arc::new(Mute))
        .start_supervised(true);
    assert!(matches!(
        g.admit(&call("edit", &json!({"path": "src/lib.rs"}))),
        GateDecision::Admit
    ));
    let row = g.corpus().remove(0);
    // No advice, so no label is invented: there is nothing for the person to
    // have agreed or disagreed with.
    assert!(row.operator.is_none());
    assert!(row.asked);
}

/// **A grant stops asking YOU and does not stop asking the MODEL.**
///
/// > *"it can remember my allow_session, still ask model later, still record"*
///
/// Three claims in one test because they are one mechanism: the person is asked
/// once, the oracle is asked on every covered call, and every covered call is a
/// labelled row.
#[test]
fn a_standing_grant_settles_the_person_and_still_asks_the_model() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts how often the model was consulted, and how often a person was.
    struct Counted {
        id: &'static str,
        allow: bool,
        n: std::sync::Arc<AtomicUsize>,
    }
    impl Adjudicator for Counted {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            self.n.fetch_add(1, Ordering::SeqCst);
            // The person takes the SESSION grant, which is the answer this whole
            // test is about — an `allow_once` would settle one call and prove
            // nothing about what a remembered answer does.
            let want = |o: &&DecisionOption| {
                if self.allow {
                    o.kind == OptionKind::AllowSession
                } else {
                    !o.kind.is_allow()
                }
            };
            match req.options.iter().find(want) {
                Some(o) => AdjudicationDecision::selected(req, &o.id, self.id, "yes"),
                None => AdjudicationDecision::unavailable(req, self.id, "none offered"),
            }
        }
        fn last_advice(&self) -> Option<ModelAdvice> {
            self.id.starts_with("model").then(|| ModelAdvice {
                consulted: true,
                would: if self.allow { "admit" } else { "ask" },
                by: self.id.to_string(),
                basis: "yes".into(),
                cites: Vec::new(),
                latency_ms: 1,
                unsure: None,
            })
        }
        fn describe(&self) -> String {
            self.id.into()
        }
    }

    let asks_model = std::sync::Arc::new(AtomicUsize::new(0));
    let asks_human = std::sync::Arc::new(AtomicUsize::new(0));
    let advisor: std::sync::Arc<dyn Adjudicator> = std::sync::Arc::new(Counted {
        id: "model:test",
        allow: true,
        n: asks_model.clone(),
    });
    // A point that ASKS about writes and lets one answer stand for the session —
    // the shape a grant is for. Written out rather than borrowed from `NAMED`,
    // because supervision is no longer a point and this test is about the grant,
    // not about which dot on the ladder happens to have that scope today.
    let asks_and_grants = crate::mode::Mode {
        name: "test:asks-and-grants",
        grants: crate::mode::GrantScope::Session,
        ..crate::mode::Mode::ALWAYS_ASK
    };
    let mut g = AdjudicatedGate::new(Box::new(Counted {
        id: "human:op",
        allow: true,
        n: asks_human.clone(),
    }))
    .with_mode(asks_and_grants)
    .with_advisor(advisor)
    .start_supervised(true);

    let args = json!({"path": "src/lib.rs"});
    for _ in 0..3 {
        assert!(matches!(g.admit(&call("edit", &args)), GateDecision::Admit));
    }

    assert_eq!(g.grants().len(), 1, "the first answer took a session grant");
    assert_eq!(
        asks_human.load(Ordering::SeqCst),
        1,
        "the person is asked once and the grant covers the rest"
    );
    assert_eq!(
        asks_model.load(Ordering::SeqCst),
        3,
        "the oracle is asked on EVERY call, grant or no grant — that is the label"
    );

    // Every one is a row, and every one is labelled.
    let rows = g.corpus();
    assert_eq!(rows.len(), 3);
    for r in &rows {
        assert!(r.model_verdict.is_some(), "the model answered this one");
        assert!(r.operator.is_some(), "and the operator's ruling is on it");
    }

    // The first is a judgement made with the verdict on screen; the other two
    // are a standing ruling applied. `asked` separates them and the note says
    // which grant, so a trainer can weight them differently instead of
    // discovering later that it could not tell them apart.
    assert!(rows[0].asked);
    assert!(!rows[1].asked && !rows[2].asked);
    let note = match rows[1].operator.as_ref().unwrap() {
        crate::authorise::OperatorOverride::Upheld { note }
        | crate::authorise::OperatorOverride::Granted { note }
        | crate::authorise::OperatorOverride::Revoked { note } => note.clone(),
    };
    assert!(note.contains("standing decision"), "{note}");
    assert!(note.contains("granted for this session"), "{note}");
}

/// **A command asks every time**, whatever is granted. The operator's own rule,
/// and supervised does not have a relaxed version of it.
#[test]
fn an_exec_call_is_not_covered_by_a_grant_at_a_supervised_point() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asks = std::sync::Arc::new(AtomicUsize::new(0));
    let seen = asks.clone();
    let human = AskAdjudicator::new("human:op", move |req: &AdjudicationRequest| {
        seen.fetch_add(1, Ordering::SeqCst);
        req.options
            .iter()
            .find(|o| o.kind.is_allow())
            .map(|o| AdjudicationDecision::selected(req, &o.id, "human:op", "ok"))
    });
    let advisor: std::sync::Arc<dyn Adjudicator> =
        std::sync::Arc::new(Fixed::new("model:test", true));
    let mut g = AdjudicatedGate::new(Box::new(human))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(advisor)
        .start_supervised(true)
        .with_exec_follows_mode(false)
        // An absolute path and a pinned shell, so layer A resolves the action
        // and it reaches an adjudicator at all. A bare `cargo` under
        // `ShellTrust::Unknown` is `not_run` and would pass this test for the
        // wrong reason — nobody asked because nothing was decidable.
        .with_surroundings(pinned())
        .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));

    // **A shell call that RUNS something** (R21). This was `/bin/cat …`, which is a
    // read inside the workspace and is therefore clause 4 now — the subject here is
    // exec asking, so the fixture has to be a call that is still exec. `sed` is the
    // one that stays exec on the classifier's own account: `sed -n 1p` reads, and
    // `sed` computes `execute_code` too, because GNU sed's `e` command and `s///e`
    // run a shell. See `Baseline::reads_only`.
    //
    // **Two exec calls, and two different shapes of them.** The shape cache would
    // settle the *second* identical call — only for a `MayApprove` tier, which an
    // exec call is — so what this asks of two shapes is the honest version of
    // *nobody but the person settles these*.
    for cmd in [
        "/bin/sed -n 1p /w/src/lib.rs",
        "/usr/bin/awk {print} /w/src/lib.rs",
    ] {
        assert_eq!(
            g.admit(&bash(&json!({"command": cmd}))),
            GateDecision::Admit
        );
    }
    assert_eq!(
        asks.load(Ordering::SeqCst),
        2,
        "exec asks every time; a standing permission never covers it"
    );
    assert!(
        g.grants().is_empty(),
        "and no grant is taken over an exec call"
    );
}

/// **A configured glob settles the call and the model is still asked.**
///
/// > *"we have configured globs already, they can go thru model in the
/// > supervised mode too"*
///
/// Both directions. A `deny` is worth as much as an `allow` here: a model that
/// routinely wants to admit what the operator's rules refuse is exactly the
/// miscalibration to find before automode, and it is invisible if only the
/// admissions are measured.
#[test]
fn a_permission_rule_is_still_measured_against_the_model() {
    let rules = |json: serde_json::Value| {
        crate::permission::config_to_ruleset(json.as_object().unwrap()).unwrap()
    };
    let gate = |cfg: serde_json::Value, model_allows: bool| {
        let advisor: std::sync::Arc<dyn Adjudicator> =
            std::sync::Arc::new(Fixed::new("model:test", model_allows));
        AdjudicatedGate::new(Box::new(NoAdjudicator))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(advisor)
            .start_supervised(true)
            .with_permission(rules(cfg))
    };

    // An `allow` rule admits, and the oracle's agreement is recorded.
    let mut g = gate(json!({ "edit": "allow" }), true);
    assert_eq!(
        g.admit(&call("edit", &json!({"path": "a.rs"}))),
        GateDecision::Admit
    );
    let row = g.corpus().remove(0);
    assert!(
        row.model_verdict
            .as_deref()
            .unwrap()
            .starts_with("admit by model:test")
    );
    assert!(!row.asked, "nobody was asked: a rule decided");
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("upheld")
    );

    // A `deny` rule refuses, and an oracle that would have admitted is recorded
    // disagreeing. This is the over-refusal signal and it only exists because
    // the deny path asks too.
    let mut g = gate(json!({ "edit": "deny" }), true);
    assert!(matches!(
        g.admit(&call("edit", &json!({"path": "a.rs"}))),
        GateDecision::Refuse { .. }
    ));
    let row = g.corpus().remove(0);
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("revoked"),
        "the model would have admitted and the operator's rule refused"
    );
    assert!(row.is_disagreement());

    // And nothing about the advice changed the outcome: the same rules with a
    // model that would refuse still admit and still refuse, respectively.
    let mut g = gate(json!({ "edit": "allow" }), false);
    assert_eq!(
        g.admit(&call("edit", &json!({"path": "a.rs"}))),
        GateDecision::Admit
    );
}

/// At every point but `supervised` a settled call spends no oracle round trip.
/// There is no model to ask, and paying latency for a row nobody asked for is
/// what this guards.
#[test]
fn a_rule_at_a_non_supervised_point_consults_nothing() {
    let cfg = json!({ "edit": "allow" });
    let rules = crate::permission::config_to_ruleset(cfg.as_object().unwrap()).unwrap();
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::WRITES_ALLOWED)
        .with_permission(rules);
    assert_eq!(
        g.admit(&call("edit", &json!({"path": "a.rs"}))),
        GateDecision::Admit
    );
    let row = g.corpus().remove(0);
    // The verdict column holds what decided — the rule — and no label is
    // invented, because nothing was consulted to agree or disagree with.
    assert!(
        row.model_verdict
            .as_deref()
            .unwrap()
            .contains("gate:permission")
    );
    assert!(row.operator.is_none());
}

/// **The model has its turn regardless of what settled the call.**
///
/// > *"supervised mode is essentially a normal mode but we always ask model. and
/// > model has its turn regardless of glob, deny, allow, allow_session or glob
/// > deny or allow from config"*
///
/// One case per short-circuit in `admit`, so a path added later that forgets to
/// ask fails here rather than quietly producing rows with no verdict. The
/// breaker is the one deliberate exception and is asserted as such.
#[test]
fn every_settled_path_still_gives_the_model_its_turn() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts every consultation, whatever the answer.
    struct Counting(std::sync::Arc<AtomicUsize>);
    impl Adjudicator for Counting {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            self.0.fetch_add(1, Ordering::SeqCst);
            AdjudicationDecision::unavailable(req, "model:test", "counted")
        }
        fn last_advice(&self) -> Option<ModelAdvice> {
            Some(ModelAdvice {
                consulted: true,
                would: "ask",
                by: "model:test".into(),
                basis: "counted".into(),
                cites: Vec::new(),
                latency_ms: 1,
                unsure: None,
            })
        }
        fn describe(&self) -> String {
            "model:test".into()
        }
    }

    let build = |f: &dyn Fn(AdjudicatedGate) -> AdjudicatedGate| {
        let n = std::sync::Arc::new(AtomicUsize::new(0));
        let g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(std::sync::Arc::new(Counting(n.clone())))
            .start_supervised(true);
        (f(g), n)
    };
    let rules = |j: serde_json::Value| {
        crate::permission::config_to_ruleset(j.as_object().unwrap()).unwrap()
    };

    // Each case: how to set the gate up, and the call that trips that path.
    let path = json!({"path": "src/lib.rs"});
    let secret = json!({"path": "/home/op/.ssh/id_ed25519"});

    // 1. the never-write list
    let (mut g, n) = build(&|g| g);
    let _ = g.admit(&call("edit", &secret));
    assert_eq!(n.load(Ordering::SeqCst), 1, "never-write list: model asked");
    assert_eq!(g.log[0].effect, "refuse", "and the list still refuses");

    // 1.5 a config `deny`, and a config `allow`
    for (cfg, effect) in [("deny", "refuse"), ("allow", "admit")] {
        let (mut g, n) = build(&|g| g.with_permission(rules(json!({ "edit": cfg }))));
        let _ = g.admit(&call("edit", &path));
        assert_eq!(n.load(Ordering::SeqCst), 1, "config {cfg}: model asked");
        assert_eq!(g.log[0].effect, effect, "and the rule still decides");
    }

    // 2. the mode admits unasked. `supervised` never does, so this is asserted
    //    at a point that does — the wiring is shared and the rule is the same.
    let n = std::sync::Arc::new(AtomicUsize::new(0));
    let advisor: std::sync::Arc<dyn Adjudicator> = std::sync::Arc::new(Counting(n.clone()));
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(advisor)
        .start_supervised(true);
    let _ = g.admit(&call("edit", &path));
    let before = n.load(Ordering::SeqCst);

    // 3. a standing grant, on the SAME gate: the second call is covered.
    let _ = g.admit(&call("edit", &path));
    assert!(
        n.load(Ordering::SeqCst) > before,
        "a grant-covered call still consults the model"
    );

    // 0. the breaker is the exception, and it is one on purpose: the calls
    //    reaching it are near-duplicates of rows already recorded, and paying an
    //    oracle round trip per iteration of a retry loop is what the breaker
    //    exists to prevent.
    let (mut g, n) = build(&|g| g.with_permission(rules(json!({ "edit": "deny" }))));
    for _ in 0..5 {
        let _ = g.admit(&call("edit", &path));
    }
    let asked = n.load(Ordering::SeqCst);
    assert!(
        asked < 5,
        "the breaker must stop consulting, asked {asked} of 5"
    );
    assert!(asked >= 3, "and only after it has opened, asked {asked}");
}

/// **A short-circuit is not a verdict**, and a row must not record agreement
/// with one.
///
/// `ModelAdjudicator` answers five questions without asking a model — an
/// unresolved action, an always-ask entry, an blocked one, an uncollected
/// trail, an intent outside its earned scope. From outside, those produce the
/// same `AdjudicationDecision` a real verdict does. Labelling against them would
/// manufacture signal, which is worse than none because it looks like data.
#[test]
fn an_unconsulted_oracle_produces_no_label() {
    struct Silent;
    impl Adjudicator for Silent {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            AdjudicationDecision::unavailable(req, "model:test", "always-ask short-circuit")
        }
        fn last_advice(&self) -> Option<ModelAdvice> {
            Some(ModelAdvice {
                consulted: false,
                would: "ask",
                by: "model:test".into(),
                basis: "`sudo` is on the always-ask list; no oracle was consulted".into(),
                cites: Vec::new(),
                latency_ms: 0,
                unsure: None,
            })
        }
        fn describe(&self) -> String {
            "model:test".into()
        }
    }
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(std::sync::Arc::new(Silent))
        .start_supervised(true);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));

    let row = g.corpus().remove(0);
    assert!(
        row.operator.is_none(),
        "no oracle spoke, so there is nothing the operator agreed or disagreed with"
    );
    // The row still exists and still says what happened — silence is recorded,
    // not dropped.
    assert!(row.model_verdict.is_some());
    assert!(row.asked);
    // And a head renders it as an absence rather than as an opinion.
    let advice = g.log[0].advice.as_ref().unwrap();
    assert!(
        advice.line().starts_with("no model verdict"),
        "{}",
        advice.line()
    );
}

/// **Supervision changes who is consulted and nothing else.**
///
/// It used to be a mode, which meant selecting it also pinned the dispositions
/// and the grant scope — and meant it could only change by restarting a session.
/// It is a flag on the gate now, so this asserts the property that makes that
/// safe: the same calls ask, the same answers travel, and only the verdict
/// column appears.
#[test]
fn supervision_changes_who_is_consulted_and_nothing_else() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asks = std::sync::Arc::new(AtomicUsize::new(0));
    let seen = asks.clone();
    let human = AskAdjudicator::new("human:op", move |req: &AdjudicationRequest| {
        seen.fetch_add(1, Ordering::SeqCst);
        req.options
            .iter()
            .find(|o| o.kind.is_allow())
            .map(|o| AdjudicationDecision::selected(req, &o.id, "human:op", "ok"))
    });
    let mut g = AdjudicatedGate::new(Box::new(human))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(std::sync::Arc::new(Fixed::new("model:test", true)));

    let args = json!({"path": "src/lib.rs"});

    // Off: the person is asked, and the row carries no verdict.
    assert!(!g.supervising());
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert!(g.corpus()[0].operator.is_none());

    // **On, mid-session, without rebuilding anything.**
    let said = g.set_supervision(true).expect("an advisor is installed");
    assert!(said.contains("supervision ON"), "{said}");
    assert!(g.supervising());

    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(
        asks.load(Ordering::SeqCst),
        2,
        "the same calls ask the same person; supervision adds a verdict, not a question"
    );
    let row = g.corpus().remove(1);
    assert!(
        row.model_verdict
            .as_deref()
            .unwrap()
            .starts_with("admit by model:test")
    );
    assert_eq!(row.verdict_by.as_deref(), Some("human:op"));
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("upheld")
    );

    // And off again.
    assert!(g.set_supervision(false).is_ok());
    assert!(!g.supervising());
}
