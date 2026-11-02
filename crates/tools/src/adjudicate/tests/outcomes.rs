//! The four outcome classes, at the gate: shell commands, heredocs, durable rules.

use super::*;

/// A python heredoc that only writes rides the edit path, so the mode's write rules, the shape
/// cache and the grant vocabulary are reachable for it; in an ask mode it asks as a WRITE, and
/// every shape that is not a placed write keeps `exec`.
#[test]
fn a_python_heredoc_that_only_writes_is_an_edit_at_the_gate() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let gate = |mode: crate::mode::Mode| {
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        let g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "human:test",
            move |req: &AdjudicationRequest| {
                a.fetch_add(1, Ordering::Relaxed);
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_once",
                    "human:test",
                    "fine",
                ))
            },
        )))
        .with_mode(mode)
        .with_exec_follows_mode(false)
        .with_surroundings(pinned());
        (g, asked)
    };

    let write = json!({
        "command": "python3 - <<'PY'\nopen('/w/src/lib.rs', 'w').write('hi')\nPY"
    });

    // The class, and the headline a person reads: exec asks at every point, so calling this
    // `exec` would put the edit path out of reach.
    let (mut g, asked) = gate(crate::mode::Mode::AUTO_EDITS);
    let row = g.request_for(&bash(&write));
    assert_eq!(row.class.access, Access::Write, "{}", row.summary);
    assert!(
        row.summary.contains("write access"),
        "the headline says: {}",
        row.summary
    );
    // In allow-edits that is the mode's own allowed set, so nobody is consulted.
    assert_eq!(g.admit(&bash(&write)), GateDecision::Admit);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "an edit inside the workspace asked its owner"
    );
    let last = g.log.last().expect("a row");
    assert_eq!(last.decision.by, "gate:mode");
    assert!(
        last.decision.basis.contains("admits write calls"),
        "{}",
        last.decision.basis
    );

    // In ask mode it still asks, and it asks as a write: the card says so.
    let (mut g, asked) = gate(crate::mode::Mode::ALWAYS_ASK);
    let row = g.request_for(&bash(&write));
    assert_eq!(row.class.access, Access::Write, "{}", row.summary);
    assert_eq!(g.admit(&bash(&write)), GateDecision::Admit);
    assert_eq!(asked.load(Ordering::Relaxed), 1, "nobody was asked");

    // Every shape that is not a placed write keeps exec: the guardrails are what keep this
    // narrowing from becoming "any interpreter is an edit".
    for (cmd, why) in [
        (
            "python3 - <<'PY'\nimport subprocess\nsubprocess.run(['ls'])\nPY",
            "a body that shells out",
        ),
        (
            "python3 - <<'PY'\nimport requests\nrequests.get('http://example.com')\nPY",
            "a body that reaches the network",
        ),
        (
            "python3 - <<'PY'\nopen(sys.argv[1], 'w').write('x')\nPY",
            "a write target nobody can place",
        ),
        (
            "python3 - <<'PY'\nopen('/etc/cron.d/x', 'w').write('x')\nPY",
            "a write outside the workspace",
        ),
        (
            "python3 - <<'PY'\nopen('/srv/x.txt', 'w').write('x')\nPY",
            "a write this classifier cannot place",
        ),
        ("python3 deploy.py", "a body nobody read"),
        (
            "bash -c 'echo x > /w/src/lib.rs'",
            "a shell doing the write itself",
        ),
    ] {
        let (mut g, asked) = gate(crate::mode::Mode::AUTO_EDITS);
        let args = json!({"command": cmd});
        let row = g.request_for(&bash(&args));
        assert_eq!(
            row.class.access,
            Access::Exec,
            "{why} was called an edit: {cmd}"
        );
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit, "{cmd}");
        assert_eq!(
            asked.load(Ordering::Relaxed),
            1,
            "{why} stopped asking: {cmd}"
        );
    }
}

/// A gate that would admit anything an adjudicator is consulted about, so a refusal here can only
/// have come from layer A.
fn permissive_gate() -> AdjudicatedGate {
    AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "yes-to-everything",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "yes to everything",
            ))
        },
    )))
    .with_surroundings(pinned())
    .with_trail_source(|_| {
        crate::authorise::AuthorisationTrail::from_messages(
            vec![crate::authorise::Utterance::operator(
                "do whatever you like",
                0,
            )],
            1,
        )
    })
}

#[test]
fn a_disclosure_is_refused_by_a_gate_that_would_otherwise_admit_everything() {
    let mut g = permissive_gate();
    let args = json!({"command": "/bin/cat /home/dead/.ssh/id_rsa"});
    match g.admit(&bash(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { .. },
            tell,
        } => {
            assert!(tell.contains("REFUSED"), "{tell}");
            assert!(tell.contains("Do not retry a variant"), "{tell}");
        }
        other => panic!("a disclosure must not be admitted: {other:?}"),
    }
    assert_eq!(g.log[0].request.tier.as_str(), "blocked");
}

/// A shell that only reads is judged on the work and not on the vehicle: gating it as exec put a
/// prompt in front of the operator for a call whose every intent said read. The four ways it is not
/// a read are asserted failing, because the parser's refusals are what pay for the rule.
#[test]
fn a_shell_that_only_reads_is_judged_on_the_work() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // A fresh gate per case: a grant taken over an earlier call could settle a later one, which is
    // real behaviour and noise in a test about which access a call is judged at.
    let gate = || {
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        let g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "human:test",
            move |req: &AdjudicationRequest| {
                a.fetch_add(1, Ordering::Relaxed);
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_once",
                    "human:test",
                    "fine",
                ))
            },
        )))
        // `always-ask` is the strongest form: a read is admitted at every point, including the one
        // whose purpose is to ask.
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_exec_follows_mode(false)
        .with_surroundings(pinned());
        (g, asked)
    };

    // The yes: a `cd` then a reader, which is the class a person would give it.
    let (mut g, asked) = gate();
    let reads = json!({"command": "cd /w && head -c 2000 src/lib.rs"});
    let row = g.request_for(&bash(&reads));
    assert_eq!(
        row.class.access,
        Access::Read,
        "the class is the vehicle's again: {}",
        row.class
    );
    assert!(
        row.summary.contains("read access"),
        "the headline says: {}",
        row.summary
    );
    assert!(row.summary.contains("head"), "{}", row.summary);
    assert_eq!(g.admit(&bash(&reads)), GateDecision::Admit);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "nobody is asked about a read inside the boundary"
    );
    // The row records it as the mode's doing, naming the access the call was judged at.
    let last = g.log.last().expect("a row");
    assert_eq!(last.decision.by, "gate:mode");
    assert!(
        last.decision.basis.contains("admits read calls"),
        "{}",
        last.decision.basis
    );

    // The ways it is not a read: each keeps the exec class and each still asks.
    for (cmd, why) in [
        ("cd /w && frobnicate src/lib.rs", "an unknown program"),
        (
            "cd /w && cat src/lib.rs | tee /w/copy",
            "a pipeline into a writer",
        ),
        (
            "cd /w && cat src/lib.rs > /w/copy",
            "a redirection into a writer",
        ),
        ("cat /etc/passwd", "a read outside the boundary"),
        (
            "cd /w && sed -n 1p src/lib.rs",
            "a program that can execute",
        ),
        ("cd /w && rm -f src/lib.rs", "a deletion"),
        // `git symbolic-ref` reads a ref and also moves one: source order put it in the read arm,
        // where `judged_access` narrowed it to `Access::Read` and the point admitted it, so a
        // misclassification suppressed a prompt.
        (
            "git symbolic-ref HEAD refs/heads/other",
            "a ref being moved",
        ),
    ] {
        let (mut g, asked) = gate();
        let args = json!({"command": cmd});
        let row = g.request_for(&bash(&args));
        assert_eq!(
            row.class.access,
            Access::Exec,
            "{why} was judged a read: {cmd}"
        );
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit, "{cmd}");
        assert_eq!(
            asked.load(Ordering::Relaxed),
            1,
            "{why} did not reach anybody: {cmd}"
        );
    }

    // A secret read through a shell is refused, which is stronger than asking: being judged a read
    // would be worse here, so the boundary conjunct in `reads_only` keeps its exec access and the
    // flow rule refuses it before any point is read.
    let (mut g, asked) = gate();
    let secret = json!({"command": "cat ~/.ssh/id_rsa"});
    let row = g.request_for(&bash(&secret));
    assert_eq!(row.class.access, Access::Exec);
    assert!(matches!(row.tier, Tier::Blocked { .. }), "{:?}", row.tier);
    assert!(matches!(
        g.admit(&bash(&secret)),
        GateDecision::Refuse { .. }
    ));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "a secret reached a person"
    );

    // A line the parser refused is a line the point decides about, not a read: `$(ls)` has no known
    // prefix, so the scope widens and the boundary conjunct voids. The model is the one asked, and the
    // person only when the oracle is unsure.
    let (mut g, asked) = gate();
    let unresolved = json!({"command": "cd /w && cat $(ls) src/lib.rs"});
    let row = g.request_for(&bash(&unresolved));
    assert_eq!(row.class.access, Access::Exec);
    assert!(
        !row.resolved,
        "the meaning was never resolved and the row says so"
    );
    assert!(
        row.adjudicable().is_some(),
        "the model IS asked about a meaning nobody could read — the ruling of 2026-10-05"
    );
    assert_eq!(g.admit(&bash(&unresolved)), GateDecision::Admit);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "an action nobody could read is answered by the oracle, exactly once"
    );
}

/// `rm -rf /` can be allowed if it is the intent; what the gate must not do is refuse it on the
/// strength of the string.
#[test]
fn rm_rf_slash_is_not_blocked_by_the_gate_and_reaches_a_decision() {
    let mut g = permissive_gate();
    let args = json!({"command": "/bin/rm -rf /"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(g.log[0].request.tier.as_str(), "always_ask");
    assert_eq!(g.log[0].effect, "admit");
}

#[test]
fn an_always_ask_is_never_turned_into_a_standing_grant() {
    // The adjudicator asks for the widest grant it may name, and a deletion outside the project is
    // the standing authority the fixed list withholds, so the second call asks again.
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let widest = if req.option("allow_session").is_some() {
                "allow_session"
            } else {
                "allow_once"
            };
            Some(AdjudicationDecision::selected(
                req,
                widest,
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    let args = json!({"command": "/bin/rm -rf /home/dead/elsewhere"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "an always-ask is asked every time"
    );
    // It was never even offered a standing grant: the always-ask tier's ladder has no durable
    // rule, and a session grant follows the durable rule rather than standing alone.
    assert!(g.log[0].request.option("allow_session").is_none());
    assert!(g.log[0].request.option("allow_always").is_none());

    // An ordinary may-approve exec call takes the session grant its ladder offers, so the second
    // identical call is settled rather than asked again.
    let ordinary = json!({"command": "/bin/rm -rf /w/target"});
    assert_eq!(g.admit(&bash(&ordinary)), GateDecision::Admit);
    assert_eq!(g.admit(&bash(&ordinary)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "the first exec call asked; the grant took the second"
    );
    assert_eq!(
        g.grants().len(),
        1,
        "one standing permission, recorded from the exec answer"
    );
    // The grant is scoped to what the call showed, so a different program asks again. The other
    // program has to be exec: a read is admitted by clause 4 without asking anybody.
    let other = json!({"command": "/usr/bin/awk {print} /w/src/lib.rs"});
    assert_eq!(g.admit(&bash(&other)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        4,
        "a program the grant does not name asks again"
    );
}

#[test]
/// Exec asks by default, and what an answer can reach is the session grant and the durable rule
/// beside it. The command is layer A's weakest verdict, which is what clause 4 would wave through
/// if the access class let it.
fn exec_class_calls_ask_and_the_ladder_offers_both_standing_options() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let widest = if req.option("allow_session").is_some() {
                "allow_session"
            } else {
                "allow_once"
            };
            Some(AdjudicationDecision::selected(
                req,
                widest,
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    // `sed -n` and not `cat`: the command has to be an exec-class call that still asks, and a
    // shell that only reads is clause 4 (see `Baseline::reads_only`).
    let args = json!({"command": "/bin/sed -n 1p /w/src/lib.rs"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the second identical call is settled by the grant the first took"
    );
    assert_eq!(
        g.grants().len(),
        1,
        "one standing permission, recorded from the exec answer"
    );
    assert!(
        g.log
            .iter()
            .all(|r| r.request.option("allow_session").is_some()),
        "exec is offered the session grant, scoped to the program and the class"
    );
    // A durable rule is offered as well: it is the operator's own preapproval, in a file they read.
    assert!(
        g.log
            .iter()
            .all(|r| r.request.option("allow_always").is_some()),
        "exec is offered Always allow, as a rule"
    );
    // The grant is scoped to the program it was taken over, so a different exec program asks
    // again; a read would be admitted without reaching a ladder at all.
    let other = json!({"command": "/usr/bin/awk {print} /w/src/lib.rs"});
    assert_eq!(g.admit(&bash(&other)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "a program the grant does not name asks again"
    );
}

/// Without this, `next_id` restarts at 1 on every daemon and `INSERT OR IGNORE` drops every
/// colliding row, so a resumed session loses its corpus.
#[test]
fn a_resumed_gate_does_not_reissue_request_ids() {
    /// A sink that reports a history, the way a store-backed one does.
    struct Resumed(u64);
    impl crate::authorise::CorpusSink for Resumed {
        fn decided(&self, _row: &crate::authorise::CorpusRow) {}
        fn ruled(&self, _id: &str, _what: &crate::authorise::OperatorOverride) {}
        fn decisions_recorded(&self, _session_id: &str) -> u64 {
            self.0
        }
    }

    let gate = |sink: Option<std::sync::Arc<dyn crate::authorise::CorpusSink>>| {
        let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "human",
            |req: &AdjudicationRequest| {
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_once",
                    "h",
                    "fine",
                ))
            },
        )))
        .with_identity("s-42".to_string(), "dead".to_string())
        .with_mode(crate::mode::Mode::WRITES_ALLOWED)
        .with_surroundings(pinned())
        .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
        if let Some(s) = sink {
            g = g.with_corpus_sink(s);
        }
        let _ = g.admit(&bash(&json!({"command": "ls -la"})));
        g.log[0].request.id.clone()
    };

    // A fresh session starts at one.
    assert_eq!(gate(None), "adj-s-42-0001");
    // A session with 646 rows behind it does not reissue any of them.
    let id = gate(Some(std::sync::Arc::new(Resumed(646))));
    assert_eq!(id, "adj-s-42-0647", "a restart re-emitted a stored id");
}

/// The point a consenting move lands on stops asking about `bash`: the move itself is tested
/// elsewhere, and this is the half that matters afterwards.
#[test]
fn the_consented_point_admits_a_shell_command_unasked() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::ALLOW_ALL_HERE)
    .with_exec_follows_mode(true)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));

    let args = json!({"command": "cd /home/dead/Projects/leticl && grep -n \"x\" src/"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the operator was asked at a point that admits exec: {}",
        g.log[0].decision.basis
    );
    assert_eq!(g.log[0].decision.by, "gate:mode", "{:?}", g.log[0].decision);
}

/// The durable rule names the stage that needs the authority, not the first word of the line: a
/// rule over `cd` would be written to the operator's config and grant nothing.
#[test]
fn a_compound_commands_durable_rule_names_the_last_stage_not_the_cd() {
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    // A measured boundary, because `cargo test` runs the project's own code and a host with none
    // offers no standing rung; this fixture is about which stage the rule names.
    .with_boundary(Some(measured_boundary()), None)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    let _ = g.admit(&bash(&json!({
        "command": "cd /home/dead/Projects/letibot && cargo test -p letibot-tools"
    })));
    let req = &g.log[0].request;
    let always = req
        .options
        .iter()
        .find(|o| o.id == "allow_always")
        .expect("a segmentable command still offers the durable rule");
    assert!(
        always.label.contains("cargo test*"),
        "names the stage that carries the authority: {}",
        always.label
    );
    assert!(
        !always.label.contains("`cd*`"),
        "offered a permanent rule over `cd`: {}",
        always.label
    );
}

/// `bash_segments` refuses to split a command carrying a heredoc, so a rule over one is inert
/// however it is spelled: offering it invites the operator to sign something that does nothing.
#[test]
fn a_heredoc_is_offered_no_durable_rule_because_none_could_match() {
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    let cmd = "cd /home/dead/Projects/letibot && python3 - <<'PY'\nprint(1)\nPY";
    // The premise, asserted rather than assumed: no rule can answer this.
    assert!(
        !crate::permission::a_durable_rule_can_apply(cmd),
        "the premise of this test is that the matcher refuses to segment it"
    );
    let _ = g.admit(&bash(&json!({ "command": cmd })));
    let req = &g.log[0].request;
    assert!(
        req.option("allow_always").is_none(),
        "offered a durable rule that cannot match: {:?}",
        req.options.iter().map(|o| &o.label).collect::<Vec<_>>()
    );
    // The rungs that do work are still there.
    assert!(req.option("allow_once").is_some());
    assert!(req.option("allow_session").is_some());
    assert!(req.option("deny").is_some());
}

/// The label names the rule the button writes, derived by the same helper the recording site uses,
/// so what is signed and what is written cannot drift.
#[test]
fn the_always_allow_label_shows_the_rule_it_writes() {
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    // The ladder over `cargo test` where a boundary is measured, because a host with none offers a
    // runner no standing rung and this fixture is about the label.
    .with_boundary(Some(measured_boundary()), None)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    let _ = g.admit(&bash(&json!({"command": "cargo test -p letibot-tools"})));
    let req = &g.log[0].request;
    let always = req
        .options
        .iter()
        .find(|o| o.id == "allow_always")
        .expect("exec offers the durable rule");
    assert!(
        always.label.contains("cargo test*"),
        "the label names the derived pattern, program and verb: {}",
        always.label
    );
    assert!(
        always.label.contains("permission.json"),
        "the label says where the rule lands: {}",
        always.label
    );
    let session = req
        .options
        .iter()
        .find(|o| o.id == "allow_session")
        .expect("exec offers the session grant");
    assert!(
        session.label.contains("cargo"),
        "the session label names the program it would cover: {}",
        session.label
    );

    // A file tool's rule is its path, and the session grant is the class.
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let req = &g.log[1].request;
    let always = req
        .options
        .iter()
        .find(|o| o.id == "allow_always")
        .expect("a write offers the durable rule");
    assert!(
        always.label.contains("src/lib.rs"),
        "the label names the path the rule covers: {}",
        always.label
    );
}

/// The answer writes a rule over the program and its verb, the rule admits the next call before
/// the mode is consulted, and a compound hiding a second command behind the prefix still asks.
#[test]
fn always_allow_on_an_exec_call_becomes_a_prefix_rule_that_is_written_down() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let written: std::sync::Arc<std::sync::Mutex<Vec<crate::permission::Rule>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let w = written.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_always",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_permission(crate::permission::seed())
    .with_permission_sink(std::sync::Arc::new(move |r| {
        w.lock().unwrap().push(r.clone());
        Ok(())
    }))
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    // `sed -i` and not `cargo run`: a command that runs the project's own code is not settled by a
    // durable rule on a host with no boundary. `sed -n*` is on the shipped list and `sed -i` is
    // not, so the first call asks and the rule answers the second.
    let args = json!({"command": "sed -i s/x/y/ crates/tools/src/lib.rs"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
    let rules = written.lock().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].permission, "bash");
    assert_eq!(rules[0].pattern, "sed*");
    drop(rules);
    // The second, different `sed -i` is admitted by the rule, nobody asked.
    let again = json!({"command": "sed -i s/a/b/ crates/tools/src/mode.rs"});
    assert_eq!(g.admit(&bash(&again)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
    // A compound that hides a second command behind the prefix asks.
    let hidden = json!({"command": "sed -i s/x/y/ f; rm -rf /w/elsewhere"});
    assert_eq!(g.admit(&bash(&hidden)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 2);
    // And the shipped list admits a read-only git without asking at all.
    let ro = json!({"command": "git status --short"});
    assert_eq!(g.admit(&bash(&ro)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 2);
}

/// The operator's `$?` widens nothing: `echo` takes a string, not a place, so the command reads as
/// it would with the `$?` replaced by `0`. At `allow-all` the point admits it; at `always-ask` a
/// person is asked, because the widening is a scope and not a bypass.
///
/// ```text
/// Ran "cd … && make lint; echo \"exit=$?\"; …"   · not run · 17 lines
/// this command's meaning does not exist yet, so nothing can decide about it.
/// who decided: boundary:normaliser
/// basis: … parameter_expansion: `$?` … decides the argument: "\"exit=$?\""
/// ...for allow all mode
/// ```
#[test]
fn the_operators_echo_dollar_question_is_admitted_at_allow_all_and_asks_elsewhere() {
    use crate::mode::Mode;

    // `NoAdjudicator` fails closed on everything, so an `Admit` here can only have come from the
    // point: nobody could have admitted it, and it was admitted anyway.
    let cmd = json!({"command": "cd /w && make lint; echo \"exit=$?\""});

    let mut permissive = AdjudicatedGate::new(Box::new(NoAdjudicator))
        .with_mode(Mode::ALLOW_ALL_HERE)
        .with_exec_follows_mode(true)
        .with_surroundings(pinned());
    assert_eq!(
        permissive.admit(&bash(&cmd)),
        GateDecision::Admit,
        "allow-all is the point that admits this; the adjudicator refuses everything"
    );
    // The row says what was read: `$?` is a number the shell keeps, a number is not a place, and
    // the note travels with the reading so a person can check it against their own line.
    assert!(
        permissive.log[0].request.resolved,
        "the operator's own command is still unread: {}",
        permissive.log[0].request.baseline
    );
    assert!(
        !permissive.log[0]
            .request
            .baseline
            .contains("the meaning was never resolved"),
        "the row claims a resolution that did not happen: {}",
        permissive.log[0].request.baseline
    );
    let findings = &permissive.log[0]
        .request
        .reading
        .as_ref()
        .expect("a command has a reading")
        .findings;
    assert!(
        findings
            .iter()
            .any(|f| f.contains("a number is not a place")),
        "nothing said what `$?` became: {findings:?}"
    );

    // The same command where the point does not admit exec: a person decides.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut narrow = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "deny_and_tell",
                "human:test",
                "not now",
            ))
        },
    )))
    .with_mode(Mode::ALWAYS_ASK)
    .with_exec_follows_mode(false)
    .with_surroundings(pinned());
    assert!(
        matches!(narrow.admit(&bash(&cmd)), GateDecision::Refuse { .. }),
        "a narrow point must not admit it"
    );
    assert_eq!(asked.load(Ordering::Relaxed), 1, "a person was asked");
}

/// The operator's case: a heredoc whose opening line also redirects and pipes. Valid bash, but
/// layer A recovers at the `|` and its `NotRun` refused it before any mode was consulted. At
/// `allow-all` it goes on to the mode, which admits it and says it ran unread.
#[test]
fn an_unreadable_command_runs_at_allow_all_and_is_still_refused_elsewhere() {
    use crate::mode::Mode;
    let cmd = json!({"command": "cd /w; python3 - pg.html <<'PY' 2>/dev/null | head -80\nimport sys\nprint(open(sys.argv[1]).read()[:10])\nPY"});

    let mut permissive = AdjudicatedGate::new(Box::new(NoAdjudicator))
        .with_mode(Mode::ALLOW_ALL_HERE)
        .with_exec_follows_mode(true)
        .with_surroundings(pinned());
    assert_eq!(
        permissive.admit(&bash(&cmd)),
        GateDecision::Admit,
        "allow-all admits it; the adjudicator refuses everything, so only the point could"
    );
    let row = &permissive.log[0];
    assert!(
        row.decision.basis.contains("It ran unread"),
        "the row must say it was not read: {}",
        row.decision.basis
    );

    // Not at a point that admits less: layer A's refusal, before anybody is asked.
    for mode in [Mode::WRITES_ALLOWED, Mode::ALWAYS_ASK] {
        let mut narrow = AdjudicatedGate::new(Box::new(NoAdjudicator))
            .with_mode(mode)
            .with_exec_follows_mode(true)
            .with_surroundings(pinned());
        match narrow.admit(&bash(&cmd)) {
            GateDecision::Refuse { outcome, .. } => assert!(
                matches!(outcome, ToolOutcome::NotRun { ref why } if why.contains("could not read the structure")),
                "{}: {outcome:?}",
                mode.name
            ),
            other => panic!(
                "{}: an unreadable command was not refused: {other:?}",
                mode.name
            ),
        }
    }
}

/// `Grant::covers` refuses an unresolved action before it looks at coverage, so what this pins is
/// that the gate carries `resolved` on its own rather than deriving it from the verdict: a version
/// that derived it would let one `cat` cover every later `cat $ANYTHING`.
#[test]
fn a_standing_grant_does_not_cover_a_command_whose_meaning_was_never_resolved() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_session",
                "human:test",
                "yes, for the session",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::ALWAYS_ASK)
    .with_exec_follows_mode(false)
    .with_surroundings(pinned());

    // First: a resolved call, which the person allows FOR THE SESSION.
    let plain = json!({"command": "cd /w && make lint"});
    let _ = g.admit(&bash(&plain));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "the first call asked once"
    );

    // Second: the same shape with one word the grammar cannot read, so only `resolved` can keep
    // the grant off it.
    let opaque = json!({"command": "cd /w && make $TARGET"});
    let resolved_row = g.request_for(&bash(&plain));
    let opaque_row = g.request_for(&bash(&opaque));
    assert_eq!(
        resolved_row.class, opaque_row.class,
        "the pair must differ only in resolution, or this test proves nothing"
    );
    // An adjudicator may answer the unresolved twin, because a session grant covers a resolved
    // call; the pair still proves `resolved` is carried separately and the grant does not cover it.
    assert!(
        opaque_row.adjudicable().is_some(),
        "an adjudicator may answer the unresolved twin — this gate's adjudicator is a \
             person, which is the fixture's wiring and not the ruling"
    );
    let _ = g.admit(&bash(&opaque));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2,
        "the session grant must not have settled the unresolved call: it is asked again, \
             and whatever this gate wires answers it"
    );
}

/// An unresolvable command reaches the point, and the two facts that make that safe are what this
/// pins: `resolved` stays false, so no corpus row claims a resolution that never happened, and
/// `adjudicable()` is `Some`, so the oracle may answer an unread meaning.
#[test]
fn an_unresolvable_command_reaching_the_gate_is_admitted_at_a_permissive_point() {
    let mut g = permissive_gate();
    let args = json!({"command": "/bin/cat $FILE"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert!(!g.log[0].request.resolved);
    assert!(
        g.log[0].request.adjudicable().is_some(),
        "the oracle may answer an unread meaning — the ruling of 2026-10-05"
    );
}

/// The widening is a scope and not an admission: at a narrow point the unbounded scope falls
/// outside the grant for the same reason the literal does, so somebody is asked. Which adjudicator
/// answers is the gate's wiring, not the witness's.
#[test]
fn an_unresolvable_command_at_a_narrow_point_reaches_a_person_and_not_a_model() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            // The person is shown the widened scope: that is the point of widening rather than
            // refusing.
            assert!(
                req.baseline.contains("unbounded"),
                "the card must say the scope is unbounded: {}",
                req.baseline
            );
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::ALWAYS_ASK)
    .with_exec_follows_mode(false)
    .with_surroundings(pinned());

    let args = json!({"command": "/bin/cat $FILE"});
    let row = g.request_for(&bash(&args));
    assert_eq!(
        row.class.access,
        Access::Exec,
        "not a read: the path is unknown"
    );
    assert!(
        row.adjudicable().is_some(),
        "an adjudicator may answer it — here that adjudicator is the person below"
    );
    // A person is, and their yes is what admits it.
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(asked.load(Ordering::Relaxed), 1);
}

#[test]
fn a_bare_command_name_is_not_run_when_the_shell_was_not_declared_fixed() {
    // The alias defeat: `AdjudicatedGate::new` leaves `ShellTrust::Unknown`, so a name something
    // upstream could redefine is refused.
    let mut g = AdjudicatedGate::new(Box::new(NoAdjudicator));
    let args = json!({"command": "ls -la"});
    match g.admit(&bash(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => {
            assert!(why.contains("alias"), "{why}")
        }
        other => panic!("{other:?}"),
    }
}
