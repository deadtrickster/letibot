//! Approved shapes, seeded classes, standing grants, and who is adjudicating.

use super::*;

/// [`NO_PROGRAM`] is a key and not a noun: it must match nothing a command produces, so a grant
/// taken over `bash` cannot cover a `write`. Printed into a label it becomes a placeholder standing
/// where a person is asked to decide, so a path-shaped call says what the grant covers instead.
#[test]
fn the_session_option_names_a_program_or_says_it_is_a_class_and_never_a_placeholder() {
    // The sentinel is a key and must never reach a label.
    for (program, offered) in [
        (
            NO_PROGRAM,
            options(&offered(None, NO_PROGRAM), crate::mode::GrantScope::Session),
        ),
        (
            "cargo",
            options(&offered(None, "cargo"), crate::mode::GrantScope::Session),
        ),
        (
            "cargo",
            options(
                &offered(Some("cargo test"), "cargo"),
                crate::mode::GrantScope::Session,
            ),
        ),
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

    // The two cases still say different things: naming the program is the point when there is
    // one.
    let named = options(&offered(None, "cargo"), crate::mode::GrantScope::Session);
    let session = named
        .iter()
        .find(|o| o.id == "allow_session")
        .expect("the session rung");
    assert!(session.label.contains("cargo"), "{:?}", session.label);
    assert!(session.label.contains("this class"), "{:?}", session.label);

    let class_only = options(&offered(None, NO_PROGRAM), crate::mode::GrantScope::Session);
    let session = class_only
        .iter()
        .find(|o| o.id == "allow_session")
        .expect("the session rung");
    assert_eq!(
        session.label, "Allow this class for the rest of the session",
        "a call with no program must name the class, not a placeholder"
    );

    // The ladder is otherwise unchanged: same ids, same order, and the durable rung only where
    // the matcher would honour it.
    let with_durable = options(
        &offered(Some("cargo test"), "cargo"),
        crate::mode::GrantScope::Session,
    );
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
    let without_durable = options(&offered(None, "cargo"), crate::mode::GrantScope::Session);
    let no_durable: Vec<&str> = without_durable.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(
        no_durable,
        vec!["allow_once", "allow_session", "deny", "deny_and_tell"],
        "a rung the matcher would ignore is left off rather than shown and declined"
    );
}

/// A host admitted once is not a first contact again, and a refused contact stays unseen: the
/// rule keys on what was admitted, not on what was asked.
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

/// The shape is the parse with its literals holed, so a second call is a different command asking
/// the same question. Only an answer "for the session" at a point whose grants are per session
/// records one.
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
                "allow_session",
                "human:test",
                "yes",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned());

    // Outside the workspace, so the read asks rather than being settled by clause 4.
    let one = json!({"command": "sort -u /etc/hosts"});
    assert!(matches!(
        g.admit(&bash_at(&one, "s#1")),
        GateDecision::Admit
    ));
    assert_eq!(asked.load(Ordering::Relaxed), 1);

    // A different file: one shape.
    let two = json!({"command": "sort -u /etc/hostname"});
    assert!(matches!(
        g.admit(&bash_at(&two, "s#2")),
        GateDecision::Admit
    ));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "the same shape asked the operator a second time"
    );
    assert_eq!(
        g.log.last().map(|r| r.decision.by.as_str()),
        Some("gate:shape")
    );

    // A flag is where a command changes kind, so this is not that shape.
    let write = json!({"command": "sort -u -o /w/out /etc/hosts"});
    let _ = g.admit(&bash_at(&write, "s#3"));
    assert_ne!(
        g.log.last().map(|r| r.decision.by.as_str()),
        Some("gate:shape"),
        "a write was admitted by a read's shape"
    );

    // A deletion is never settled by a shape: operands are holed, and destruction is judged by
    // scope, so an approval over one directory must not stand in for another.
    let del = json!({"command": "rm -rf /w/target"});
    let _ = g.admit(&bash_at(&del, "s#4"));
    let del2 = json!({"command": "rm -rf /w/build"});
    let _ = g.admit(&bash_at(&del2, "s#5"));
    assert_ne!(
        g.log.last().map(|r| r.decision.by.as_str()),
        Some("gate:shape"),
        "a second deletion of the same shape was admitted by its shape"
    );
}

/// The cache is persisted because a restart is exactly when an in-memory one would throw away
/// every approval the operator had given.
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
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned());

    // What the previous session wrote down: the shape and the class, spelled the way the row
    // holds it. Outside the workspace, because a read inside it is clause 4's.
    let first = json!({"command": "sort -u /etc/hosts"});
    let class = {
        let mut probe = AdjudicatedGate::closed().with_surroundings(pinned());
        let _ = probe.admit(&bash_at(&first, "s#0"));
        probe.log.last().expect("a row").request.class.to_string()
    };
    let shape = crate::adjudicate::shape_of(&crate::intent::Baseline::of_command(
        "sort -u /etc/hosts",
        &pinned(),
    ))
    .expect("a command has a shape");

    assert_eq!(g.seed_shapes([(shape, class)]), 1);

    // A different line and file: one shape, and nobody is asked.
    let now = json!({"command": "sort -u /etc/hostname"});
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

/// A standing grant is checked against the intents layer A read, so the flag that turns a read
/// into a write is another call: `sort` for the session is not `sort -o`.
#[test]
fn a_session_grant_over_a_read_does_not_cover_its_write_flag() {
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
                "yes",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned());

    // Outside the workspace, so the read asks rather than being settled by clause 4.
    let read = json!({"command": "sort -u /etc/hosts"});
    assert!(matches!(
        g.admit(&bash_at(&read, "s#1")),
        GateDecision::Admit
    ));
    assert_eq!(asked.load(Ordering::Relaxed), 1);

    // The same read of another file: the grant was taken for this.
    let again = json!({"command": "sort -u /etc/hostname"});
    assert!(matches!(
        g.admit(&bash_at(&again, "s#2")),
        GateDecision::Admit
    ));
    // `allow_session` seeds a shape as well as a grant, and a shape is checked first, so the
    // same read returns as the approved shape. Either way it is not asked again.
    assert_eq!(
        g.log.last().map(|r| r.decision.by.as_str()),
        Some("gate:shape")
    );
    assert_eq!(asked.load(Ordering::Relaxed), 1, "a read of `sort` asked");

    // `-o` writes the sorted text to a file, which is not what the grant was taken for.
    let write = json!({"command": "sort -u -o /w/out /etc/hosts"});
    let _ = g.admit(&bash_at(&write, "s#3"));
    assert_ne!(
        g.log.last().map(|r| r.decision.by.as_str()),
        Some("gate:session-grant"),
        "a standing grant over `sort`'s read admitted `sort -o`"
    );
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2,
        "the write was not put to the operator"
    );
}

/// The class is the half of the pair that says what was approved, so equality on it is what stops
/// a read's approval admitting a write.
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

    // Exec: a read is clause 4 and would be admitted whatever the seeded class said.
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

/// A class that cannot be read back is dropped rather than guessed at, so the next call of that
/// shape asks.
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

/// A class that did not round-trip would silently stop matching after a restart, which reads as
/// "the cache does not work" rather than as a bug.
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

/// A gate that learned from its own answers would widen one slightly-different call at a time, so
/// an admit nobody was consulted about records no shape.
#[test]
fn the_gates_own_admits_do_not_become_approved_shapes() {
    let mut g = AdjudicatedGate::closed().with_mode(crate::mode::Mode::WRITES_ALLOWED);
    let args = json!({"command": "grep -n x /w/a.rs"});
    // `writes-allowed` admits a read unasked, so nothing is learned from it.
    let _ = g.admit(&bash_at(&args, "s#1"));
    assert!(
        g.approved_shapes.is_empty(),
        "the gate learned a shape from its own answer: {:?}",
        g.approved_shapes
    );
}

/// The standing grants go with the old point, because they were answers to questions asked under
/// it.
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
        preview: &[],
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

    // The new point is the one deciding: a write that writes-allowed admits unasked now reaches
    // the adjudicator, and what stands afterwards is the new point's grant, not the old one.
    let write = GateCall {
        name: "write",
        access: Access::Write,
        args: &args,
        turn_id: "t1",
        call_id: "c2",
        workspace: "/home/dead/Projects/letibot",
        target_exists: Some(true),
        scripts: &[],
        preview: &[],
    };
    // A point whose grants are `Once` records nothing, so the second move is to one
    // that grants for the session: what stands afterwards is that point's answer, and not the
    // answer taken at the point the gate has left.
    let mut session = Mode::ALWAYS_ASK;
    session.grants = crate::mode::GrantScope::Session;
    g.set_mode(session).expect("the gate moves");
    let _ = g.admit(&write);
    assert_eq!(
        g.grants().len(),
        1,
        "the grant on the books is the one the new point took, not the one the \
             old point did"
    );
}

/// At `automode` the model's answer is the decision; asking the person anyway would be
/// `supervised` under another name. The always-ask exemption is asserted beside the admit so the
/// two cannot drift apart.
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

    /// A person who refuses: an admit below can only have come from the model.
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
        preview: &[],
    });
    assert!(
        matches!(d, GateDecision::Admit),
        "the model authorised it and the person was asked anyway: {d:?}"
    );

    // The always-ask list still reaches the person, so the point has not become allow-all by
    // another route.
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
        preview: &[],
    });
    assert!(
        !matches!(d, GateDecision::Admit),
        "an always-ask action was settled by the model: {d:?}"
    );
}

/// A `bash` call is about its command: without it in the list, every exec permission read as a
/// call that names nothing, on the one tool whose whole argument is the thing being decided.
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
    // A call that genuinely names nothing says so rather than borrowing a word.
    assert_eq!(
        super::target_of(&json!({"limit": 5})),
        "<no target argument>"
    );
}

/// The daemon's banner is computed from this, so a gate that answered vaguely would put a vague
/// sentence in front of the operator.
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

/// A move to `read-only` leaves the seated tools in place, so the gate refuses what the point does
/// not seat: an allow rule written for `bash` must not run it after the move.
#[test]
fn permission_allow_outlives_a_move_to_read_only() {
    use crate::runtime::Gate;
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "yes",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::AUTO)
    .with_permission(vec![crate::permission::Rule::new(
        "bash",
        "*",
        crate::permission::Action::Allow,
    )])
    .with_surroundings(pinned());
    let args = serde_json::json!({"command": "touch /w/x"});
    assert!(matches!(
        g.admit(&bash_at(&args, "s#1")),
        GateDecision::Admit
    ));
    g.set_mode(crate::mode::Mode::READ_ONLY)
        .expect("the gate moves");
    assert!(
        !matches!(g.admit(&bash_at(&args, "s#2")), GateDecision::Admit),
        "an allow rule ran bash after the move to read-only"
    );
    assert_eq!(
        g.log.last().map(|r| r.decision.by.as_str()),
        Some("gate:mode")
    );
}

fn counting(
    option: &'static str,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    AdjudicatedGate,
) {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                option,
                "human:test",
                "yes",
            ))
        },
    )))
    .with_surroundings(pinned());
    (asked, g)
}

/// The shape holes a `-c` body, so remembering it would admit any later program. "Allow this
/// one" covers the call answered, and "for the session" never stands in for a different body.
#[test]
fn an_inline_program_approval_admits_a_different_body() {
    use std::sync::atomic::Ordering;
    for (mode, option) in [
        (crate::mode::Mode::WRITES_ALLOWED, "allow_once"),
        (crate::mode::Mode::ALWAYS_ASK, "allow_once"),
        (crate::mode::Mode::WRITES_ALLOWED, "allow_session"),
    ] {
        let (asked, g) = counting(option);
        let mut g = g.with_mode(mode);
        let first = json!({"command": "python3 -c 'print(1)'"});
        let _ = g.admit(&bash_at(&first, "s#1"));
        let other = json!({"command": "python3 -c 'import os;os.system(\"rm -rf /w/src\")'"});
        let _ = g.admit(&bash_at(&other, "s#2"));
        assert_eq!(
            asked.load(Ordering::Relaxed),
            2,
            "{option} at {} admitted a different body",
            mode.name
        );
        assert_ne!(
            g.log.last().map(|r| r.decision.by.as_str()),
            Some("gate:shape")
        );
    }
}

/// A shape is a session answer at a session point, so a move to a point whose grants are once
/// forgets every shape, seeded ones included.
#[test]
fn set_mode_to_always_ask_forgets_approved_shapes() {
    use crate::runtime::Gate;
    use std::sync::atomic::Ordering;
    let (asked, g) = counting("allow_once");
    let mut g = g.with_mode(crate::mode::Mode::WRITES_ALLOWED);
    let args = json!({"command": "mkdir -p /w/build/x"});
    let seen = g.request_for(&bash_at(&args, "s#0"));
    assert_eq!(
        g.seed_shapes([(seen.shape.clone().expect("a shape"), seen.class.to_string())]),
        1
    );
    let _ = g.admit(&bash_at(&args, "s#1"));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "the seeded shape must settle first"
    );

    g.set_mode(crate::mode::Mode::ALWAYS_ASK)
        .expect("the gate moves");
    let again = json!({"command": "mkdir -p /w/build/y"});
    let _ = g.admit(&bash_at(&again, "s#2"));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "a shape outlived the move to always-ask"
    );

    // And moving back does not bring it back.
    g.set_mode(crate::mode::Mode::WRITES_ALLOWED)
        .expect("the gate moves");
    let _ = g.admit(&bash_at(&again, "s#3"));
    assert_eq!(asked.load(Ordering::Relaxed), 2);
}

/// A compound line is one card and several programs: the session rung names every one of them, and
/// the grant it writes covers a later call only when every program that call runs is among them.
#[test]
fn a_grant_over_a_compound_line_names_every_program_and_covers_only_those() {
    use std::sync::atomic::Ordering;
    let (asked, g) = counting("allow_session");
    let mut g = g.with_mode(crate::mode::Mode::WRITES_ALLOWED);
    let compound = json!({"command": "cat /etc/hosts; ip neigh; head -3 /etc/hostname"});

    // The label the card draws, read off the request the gate builds for the change.
    let req = g.request_for(&bash_at(&compound, "s#0"));
    let rung = req
        .options
        .iter()
        .find(|o| o.id == "allow_session")
        .expect("the session rung");
    assert!(
        rung.label.contains("cat, ip, head"),
        "the rung names the first program only: {:?}",
        rung.label
    );

    assert!(matches!(
        g.admit(&bash_at(&compound, "s#1")),
        GateDecision::Admit
    ));
    assert_eq!(asked.load(Ordering::Relaxed), 1);

    // One of the three, alone, is covered: it is a program this grant names.
    let one = json!({"command": "head -1 /etc/hostname"});
    assert!(matches!(
        g.admit(&bash_at(&one, "s#2")),
        GateDecision::Admit
    ));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "`head` alone was asked again"
    );
    assert_eq!(
        g.log.last().map(|r| r.decision.by.as_str()),
        Some("gate:session-grant")
    );

    // A program the card never named is not covered, however it was spelled.
    let other = json!({"command": "sort /etc/hostname"});
    let _ = g.admit(&bash_at(&other, "s#3"));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2,
        "a grant over `cat, ip, head` covered `sort`"
    );
}
