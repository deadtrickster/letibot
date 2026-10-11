//! Approved shapes, seeded classes, standing grants, and who is adjudicating.

use super::*;

/// **R18's fifth point: the option label carried the literal `<tool>`.**
///
/// `grant_program` answers [`NO_PROGRAM`] for a path-shaped call — `job_kill`,
/// `write` — and that string is a **key**: it must match nothing a command produces
/// so a grant taken over `bash` cannot cover a `write`. It was also being printed
/// into the ladder, so the operator's card offered
///
///     Allow `<tool>` (this class) for the rest of the session
///
/// which is a placeholder standing where a noun belongs, on the one surface where a
/// person is asked to decide. The card was reproduced from the operator's screen on
/// 2026-09-22 with `job_kill` and no argument — an Exec-access tool whose arguments
/// contain no command line, which is exactly the case that reaches this arm.
///
/// The fix is not a guess at a name; it is the wording the other ladder already uses
/// for the same situation, because it is exactly true when there is no program: what
/// the grant covers is the class.
#[test]
fn the_session_option_names_a_program_or_says_it_is_a_class_and_never_a_placeholder() {
    // The bug, as an assertion: the sentinel must not appear in any label.
    for (program, offered) in [
        (NO_PROGRAM, exec_options(None, NO_PROGRAM)),
        ("cargo", exec_options(None, "cargo")),
        ("cargo", exec_options(Some("cargo test"), "cargo")),
    ] {
        for o in &offered {
            assert!(
                !o.label.contains(NO_PROGRAM),
                "the sentinel key {NO_PROGRAM:?} reached a label for {program:?}: {:?}",
                o.label
            );
            assert!(
                !(o.label.contains('<') && o.label.contains('>')),
                "a placeholder reached a label: {:?}",
                o.label
            );
        }
    }

    // And the two cases still say different things, which is the point of naming the
    // program when there is one.
    let named = exec_options(None, "cargo");
    let session = named
        .iter()
        .find(|o| o.id == "allow_session")
        .expect("the session rung");
    assert!(session.label.contains("cargo"), "{:?}", session.label);
    assert!(session.label.contains("this class"), "{:?}", session.label);

    let class_only = exec_options(None, NO_PROGRAM);
    let session = class_only
        .iter()
        .find(|o| o.id == "allow_session")
        .expect("the session rung");
    assert_eq!(
        session.label, "Allow this class for the rest of the session",
        "a call with no program must name the class, not a placeholder"
    );

    // **The ladder is otherwise unchanged**: same ids, same order, and the durable
    // rung still appears when — and only when — the matcher would honour it. A fix to
    // one label must not quietly reshape the ladder.
    let with_durable = exec_options(Some("cargo test"), "cargo");
    let ids: Vec<&str> = with_durable.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "allow_once",
            "allow_session",
            "allow_always",
            "deny",
            "deny_and_tell"
        ],
        "the ladder's shape moved"
    );
    let without_durable = exec_options(None, "cargo");
    let no_durable: Vec<&str> = without_durable.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(
        no_durable,
        vec!["allow_once", "allow_session", "deny", "deny_and_tell"],
        "a rung the matcher would ignore is left off rather than shown and declined"
    );
}

/// **A host the operator admitted once is not a first contact again.** The
/// rule is `network_egress_to_an_UNSEEN_host`; `saw_host` existed for the
/// second contact and nothing called it, so a session asked about the same
/// host on every call. A refused contact stays unseen.
#[test]
fn an_admitted_contact_makes_the_host_seen_and_a_refused_one_does_not() {
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            let opt = if req.target.contains("refused.example") {
                "deny"
            } else {
                "allow_once"
            };
            Some(AdjudicationDecision::selected(
                req,
                opt,
                "human:test",
                "ruled",
            ))
        },
    )))
    .with_surroundings(pinned());
    let unseen = |g: &AdjudicatedGate, cmd: &str| {
        matches!(
            g.baseline_for(&bash_at(&json!({"command": cmd}), "probe"))
                .tier,
            Tier::AlwaysAsk {
                rule: "network_egress_to_an_unseen_host",
                ..
            }
        )
    };

    assert!(
        unseen(&g, "curl -s https://api.example.org/v1/x"),
        "a first contact"
    );
    let one = json!({"command": "curl -s https://api.example.org/v1/x"});
    assert!(matches!(
        g.admit(&bash_at(&one, "s#1")),
        GateDecision::Admit
    ));
    assert!(
        !unseen(&g, "curl -s https://api.example.org/v1/y"),
        "the second contact with an admitted host is not a first one"
    );

    assert!(unseen(&g, "curl -s https://refused.example/x"));
    let bad = json!({"command": "curl -s https://refused.example/x"});
    assert!(!matches!(
        g.admit(&bash_at(&bad, "s#2")),
        GateDecision::Admit
    ));
    assert!(
        unseen(&g, "curl -s https://refused.example/y"),
        "a refused host is still unseen"
    );
}

/// **A shape the operator approved is not asked again — and the things that
/// must always ask, still do.**
///
/// *"shapes like this must be approved only once"*, and the shape is the parse
/// with its literals holed, so the second call is a different command and the
/// same question.
#[test]
fn a_shape_the_operator_approved_is_not_asked_twice() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "yes",
            ))
        },
    )))
    .with_surroundings(pinned());

    // **Exec, so that asking is what is being measured** (R21): a `grep` over the
    // workspace is a read and clause 4 settles it with nobody asked, which would
    // make this test green about the shape cache it is not exercising.
    let one = json!({"command": "sed -n 1p /w/a.rs"});
    assert!(matches!(
        g.admit(&bash_at(&one, "s#1")),
        GateDecision::Admit
    ));
    assert_eq!(asked.load(Ordering::Relaxed), 1);

    // A different line, a different file: one shape.
    let two = json!({"command": "sed -n 2p /w/b.rs"});
    assert!(matches!(
        g.admit(&bash_at(&two, "s#2")),
        GateDecision::Admit
    ));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "the same shape asked the operator a second time"
    );

    // **A flag is where a command changes kind**, so this is NOT that shape.
    let write = json!({"command": "sed -i s/a/b/ /w/a.rs"});
    let _ = g.admit(&bash_at(&write, "s#3"));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2,
        "a write was admitted by a read's shape"
    );

    // **A deletion is never settled by a shape.** A shape holes its operands,
    // and this tree judges destruction by SCOPE — so `rm -rf <arg>` approved
    // over one directory must not stand in for another. Asked both times.
    let del = json!({"command": "rm -rf /w/target"});
    let _ = g.admit(&bash_at(&del, "s#4"));
    let before = asked.load(Ordering::Relaxed);
    let del2 = json!({"command": "rm -rf /w/build"});
    let _ = g.admit(&bash_at(&del2, "s#5"));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        before + 1,
        "a second deletion of the same shape was admitted without asking"
    );
}

/// **A shape survives the restart** — the whole point of persisting it.
///
/// The operator restarts a session when one has gone wrong, which was exactly
/// when the in-memory cache threw away every approval they had given. A gate
/// seeded from the store must not ask again.
#[test]
fn a_shape_approved_in_an_earlier_session_is_not_asked_again() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "yes",
            ))
        },
    )))
    .with_surroundings(pinned());

    // What the previous session wrote down: the shape, and the class it was
    // approved at, spelled the way the row holds it.
    // **Exec** (R21): a read would be admitted by clause 4 and this would be a
    // green test about a shape cache that never ran.
    let first = json!({"command": "sed -n 1p /w/a.rs"});
    let class = {
        let mut probe = AdjudicatedGate::closed().with_surroundings(pinned());
        let _ = probe.admit(&bash_at(&first, "s#0"));
        probe.log.last().expect("a row").request.class.to_string()
    };
    let shape = crate::adjudicate::shape_of(&crate::intent::Baseline::of_command(
        "sed -n 1p /w/a.rs",
        &pinned(),
    ))
    .expect("a command has a shape");

    assert_eq!(g.seed_shapes([(shape, class)]), 1);

    // A different line and file — one shape, and nobody is asked.
    let now = json!({"command": "sed -n 2p /w/b.rs"});
    assert!(matches!(
        g.admit(&bash_at(&now, "s#1")),
        GateDecision::Admit
    ));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "a shape approved before the restart was asked about again"
    );
}

/// A seeded shape whose class does not match the call in front of it settles
/// nothing. The class is the half of the pair that says *what* was approved,
/// and equality on it is what stops a read's approval admitting a write.
#[test]
fn a_seeded_shape_does_not_admit_a_different_class() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "yes",
            ))
        },
    )))
    .with_surroundings(pinned());

    // **Exec** (R21): a read is clause 4 and would be admitted whatever the seeded
    // class said, so the call this test is about has to be one that asks.
    let cmd = "sed -n 1p /w/b.rs";
    let shape = crate::adjudicate::shape_of(&crate::intent::Baseline::of_command(cmd, &pinned()))
        .expect("a command has a shape");

    // Seeded at a class this call is not: read, off the project, irreversible.
    let wrong = ActionClass {
        access: Access::Read,
        scope: EffectScope::External,
        reversibility: Reversibility::Irreversible,
        cost: Cost::Metered,
    };
    assert_eq!(g.seed_shapes([(shape, wrong.to_string())]), 1);

    let _ = g.admit(&bash_at(&json!({ "command": cmd }), "s#1"));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "a shape seeded at another class settled this call"
    );
}

/// A class that cannot be read back is dropped, never guessed at. The next call
/// of that shape asks, which is the safe direction.
#[test]
fn an_unreadable_seeded_class_is_dropped_rather_than_guessed() {
    let mut g = AdjudicatedGate::closed();
    assert_eq!(
        g.seed_shapes([("grep -n <arg>".to_string(), "nonsense".to_string())]),
        0
    );
    assert_eq!(
        g.seed_shapes([("grep -n <arg>".to_string(), "read,host_project".to_string())]),
        0,
        "a class missing two of its four facts was accepted"
    );
}

/// Every class an approval can be recorded at reads back as itself. A class that
/// did not round-trip would silently stop matching after a restart, which reads
/// as "the cache does not work" and not as a bug.
#[test]
fn a_class_reads_back_as_what_was_written() {
    for access in [
        Access::Read,
        Access::Write,
        Access::Exec,
        Access::Network,
        Access::Session,
    ] {
        for scope in [
            EffectScope::InRun,
            EffectScope::HostProject,
            EffectScope::HostOther,
            EffectScope::External,
        ] {
            for reversibility in [Reversibility::Reversible, Reversibility::Irreversible] {
                for cost in [Cost::Free, Cost::Metered] {
                    let c = ActionClass {
                        access,
                        scope,
                        reversibility,
                        cost,
                    };
                    assert_eq!(ActionClass::parse(&c.to_string()), Some(c));
                }
            }
        }
    }
}

/// The guard's own admits never become shapes. The operator's rule about
/// drift, applied to the mechanism most able to do it: a gate that learned from
/// its own answers would widen one slightly-different call at a time.
#[test]
fn the_gates_own_admits_do_not_become_approved_shapes() {
    let mut g = AdjudicatedGate::closed().with_mode(crate::mode::Mode::WRITES_ALLOWED);
    let args = json!({"command": "grep -n x /w/a.rs"});
    // `writes-allowed` admits a read unasked; nobody was asked, so nothing is
    // learned from it.
    let _ = g.admit(&bash_at(&args, "s#1"));
    assert!(
        g.approved_shapes.is_empty(),
        "the gate learned a shape from its own answer: {:?}",
        g.approved_shapes
    );
}

/// **`/mode` moves the session it is typed in.** The gate reads its mode when
/// it decides, so the move is a field write — and the standing grants go with
/// the old point, because they were answers to questions asked under it.
#[test]
fn a_gate_moves_to_a_new_point_and_leaves_its_grants_behind() {
    use crate::mode::Mode;
    use crate::runtime::Gate;

    // A person who allows everything for the session, so a grant gets taken.
    struct Session;
    impl Adjudicator for Session {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            AdjudicationDecision::selected(
                req,
                "allow_session",
                "human:test",
                "yes, for the session",
            )
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }
    let mut g = AdjudicatedGate::new(Box::new(Session)).with_mode(Mode::WRITES_ALLOWED);
    let args = serde_json::json!({"path": "/home/dead/Projects/letibot/notes.md"});
    let call = GateCall {
        name: "web_search",
        access: Access::Network,
        args: &serde_json::json!({"query": "x"}),
        turn_id: "t1",
        call_id: "c1",
        workspace: "/home/dead/Projects/letibot",
        target_exists: None,
        scripts: &[],
    };
    assert!(matches!(g.admit(&call), GateDecision::Admit));
    assert_eq!(
        g.grants().len(),
        1,
        "a session grant was taken under writes-allowed"
    );

    // Move to always-ask: the grant does not survive the point it was taken at.
    let dropped = g.set_mode(Mode::ALWAYS_ASK).expect("the gate moves");
    assert_eq!(dropped, 1);
    assert!(g.grants().is_empty());
    assert_eq!(g.mode().name, "always-ask");

    // And the new point is the one deciding: a write that writes-allowed admits
    // unasked now reaches the adjudicator. The answer takes a grant, and since
    // the operator's revision of 2026-09-18 an explicit `allow_session` stands
    // at a `Once` point too — the durable *Always allow* was always offered
    // here, and the weaker option follows it. What the move still proves is
    // that what stands is the NEW point's grant: the old one stayed dropped.
    let write = GateCall {
        name: "write",
        access: Access::Write,
        args: &args,
        turn_id: "t1",
        call_id: "c2",
        workspace: "/home/dead/Projects/letibot",
        target_exists: Some(true),
        scripts: &[],
    };
    let _ = g.admit(&write);
    assert_eq!(
        g.grants().len(),
        1,
        "the grant on the books is the one the new point took, not the one the \
             old point did"
    );
}

/// **`automode` means the model answers.** It advised and the person was asked
/// anyway, which is `supervised` under another name — and the operator chose
/// the point precisely so the model would decide.
///
/// The always-ask list is the exemption the mode's own sentence promises, and
/// it is asserted here beside the admit so the two cannot drift apart.
#[test]
fn at_automode_the_models_admit_is_the_decision_and_always_ask_still_asks() {
    use crate::authorise::{AuthorisationTrail, OracleAnswer, Widening};
    use crate::mode::Mode;

    /// An oracle that authorises everything it is allowed to be asked about.
    struct Yes;
    impl crate::authorise::AuthorisationOracle for Yes {
        fn authorised(&self, brief: &mut crate::authorise::ModelBrief) -> OracleAnswer {
            // The witness is the proof, and taking it is how an oracle widens.
            let Some(w) = brief.adjudicable() else {
                return OracleAnswer::Unsure {
                    why: "no witness".into(),
                    kind: UnsureKind::CouldNotDecide,
                };
            };
            OracleAnswer::Authorised(Widening::new(
                w,
                brief.request_id.clone(),
                vec![0],
                "the operator asked for exactly this",
            ))
        }
        fn describe(&self) -> String {
            "test oracle".into()
        }
    }

    /// A person who would refuse. If the ladder is reached at all, the call is
    /// denied — so an admit below can only have come from the model.
    struct Refuses;
    impl Adjudicator for Refuses {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            AdjudicationDecision::selected(req, "deny", "human:test", "the person said no")
        }
        fn describe(&self) -> String {
            "a person who refuses".into()
        }
    }

    let gate = || {
        AdjudicatedGate::new(Box::new(Refuses))
            .with_mode(Mode::AUTO)
            .with_advisor(std::sync::Arc::new(crate::ModelAdjudicator::new(
                Box::new(Yes),
                |_req: &AdjudicationRequest| {
                    crate::intent::Baseline::of_command("touch /tmp/x", &Default::default())
                },
            )))
            .start_supervised(true)
            .with_trail_source(|_call: &GateCall<'_>| AuthorisationTrail {
                utterances: vec![crate::authorise::Utterance {
                    speaker: crate::authorise::Speaker::Operator,
                    text: "make that file".into(),
                    clipped: false,
                    turns_ago: 0,
                    seconds_ago: Some(5),
                }],
                provenance: crate::authorise::TrailProvenance::Scanned {
                    messages_scanned: 1,
                    operator_messages: 1,
                },
            })
    };

    let args = serde_json::json!({"path": "/home/dead/Projects/letibot/notes.md"});
    let mut g = gate();
    let d = g.admit(&GateCall {
        name: "write",
        access: Access::Write,
        args: &args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/home/dead/Projects/letibot",
        target_exists: Some(true),
        scripts: &[],
    });
    assert!(
        matches!(d, GateDecision::Admit),
        "the model authorised it and the person was asked anyway: {d:?}"
    );

    // The always-ask list still reaches the person, who refuses — so the point
    // has not become allow-all by another route.
    let secret = serde_json::json!({"path": "/home/dead/.ssh/id_rsa"});
    let mut g = gate();
    let d = g.admit(&GateCall {
        name: "write",
        access: Access::Write,
        args: &secret,
        turn_id: "t1",
        call_id: "c2",
        workspace: "/home/dead/Projects/letibot",
        target_exists: Some(true),
        scripts: &[],
    });
    assert!(
        !matches!(d, GateDecision::Admit),
        "an always-ask action was settled by the model: {d:?}"
    );
}

/// **A `bash` call is about its command**, and the prompt has to be able to say
/// so. Measured 2026-09-15: every exec permission the operator was shown read
/// *"`bash` wants exec access to `<no target argument>`"* — the placeholder for
/// a call that names nothing, on the one tool whose whole argument is the
/// thing being decided. `command` was simply not in the list.
#[test]
fn the_thing_a_call_is_about_includes_a_shell_command() {
    use serde_json::json;
    assert_eq!(
        super::target_of(&json!({"command": "cargo test -p letibot-tools"})),
        "cargo test -p letibot-tools"
    );
    // Still the most concrete argument first for the file tools.
    assert_eq!(
        super::target_of(&json!({"path": "src/main.rs"})),
        "src/main.rs"
    );
    // And a call that genuinely names nothing still says so rather than
    // borrowing a word it never had.
    assert_eq!(
        super::target_of(&json!({"limit": 5})),
        "<no target argument>"
    );
}

/// `Gate::describe` defaults to naming the absence, and an attached gate names
/// its adjudicator. The daemon's banner is computed from this, so a gate that
/// answered vaguely would put a vague sentence in front of the operator.
#[test]
fn a_gate_says_who_is_adjudicating_or_that_nobody_is() {
    use crate::runtime::{Gate, NoBoundary};
    assert!(
        NoBoundary.describe().starts_with("none"),
        "the default must name the absence: {}",
        NoBoundary.describe()
    );
    let gate = AdjudicatedGate::new(Box::new(NoAdjudicator));
    assert_eq!(gate.describe(), NoAdjudicator.describe());
}
