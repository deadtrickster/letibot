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

use super::*;
use serde_json::json;
use std::collections::BTreeSet;

/// **R18: a call that names no path is not described by a path it does not have.**
///
/// The facts are what the head prints as `because: …` and what the oracle is shown,
/// and one of them asserted *"the path is inside the session's workspace"* for a
/// call with **no `path` argument at all** — because `path_is_inside` answers `true`
/// when the key is absent. Measured on the operator's screen 2026-09-22: a
/// `job_kill` with no arguments, described by a path nobody had named.
///
/// The requirement's own words: *"a tool call with no path argument is judged on
/// what it DOES, not on a path it does not have — resolving no-target to `/` makes
/// every targetless call a call on the whole filesystem, which is the least true
/// reading available."*
#[test]
fn a_call_with_no_path_is_not_described_by_a_path_it_does_not_have() {
    let mut g = AdjudicatedGate::closed();

    // No `path` key at all: no containment claim, and the absence is stated.
    let no_path = g.request_for(&call("job_kill", &json!({"job": "j1"})));
    let brief = no_path.brief();
    assert!(
        !brief.contains("the path is inside") && !brief.contains("the path is OUTSIDE"),
        "a containment claim about a path nobody named:\n{brief}"
    );
    assert!(
        brief.contains("names no path"),
        "the brief must say the call has no path:\n{brief}"
    );

    // **A call that does name one keeps the fact, both ways round.** Without this
    // half the test would pass on a gate that had simply stopped saying it.
    let inside = g.request_for(&call("write", &json!({"path": "a/b.txt", "content": "x"})));
    assert!(
        inside
            .brief()
            .contains("the path is inside the session's workspace"),
        "{}",
        inside.brief()
    );
    let outside = g.request_for(&call(
        "write",
        &json!({"path": "/etc/passwd", "content": "x"}),
    ));
    assert!(
        outside
            .brief()
            .contains("the path is OUTSIDE the session's workspace"),
        "{}",
        outside.brief()
    );

    // **And the workspace fact prints the workspace it was handed.** This is the
    // fact the operator saw on four cards as `workspace: /`; the gate's half of
    // that fix is that it prints what it is given rather than substituting a root,
    // and `a_seat_rooted_at_the_host_still_reports_its_own_workspace` in
    // `backend.rs` is the other half.
    assert!(
        inside.brief().contains("workspace: /w"),
        "the brief must print the workspace it was handed:\n{}",
        inside.brief()
    );
}

fn call<'a>(name: &'a str, args: &'a Value) -> GateCall<'a> {
    GateCall {
        name,
        access: Access::Write,
        args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/w",
        target_exists: Some(true),
        scripts: &[],
    }
}

/// **At the widest point on the operator's box, the secret stores are still
/// shut.** The operator, adding `automode-edits` (2026-09-17): *"that being
/// said - .ssh and friends must stay protected"*.
///
/// This is the claim the sixth point rests on, so it is checked where it
/// actually holds — at the gate, through the real admit path, with a mode
/// whose `write` disposition is `Admit` and whose decider is a model that
/// would say yes to anything. An ordinary edit goes through with nobody
/// consulted, which is the point of the mode; a write into `.ssh` is refused
/// by `NEVER_WRITE` at step 1, before any point or adjudicator is reached,
/// and a read of a key is refused as a disclosure at step 0.
#[test]
fn the_never_write_list_outranks_the_widest_point_on_this_box() {
    let yes = |req: &AdjudicationRequest| {
        Some(AdjudicationDecision::selected(
            req,
            "allow_once",
            "model:test",
            "sure",
        ))
    };
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new("model:test", yes)))
        .with_surroundings(pinned())
        .with_mode(crate::mode::Mode::AUTO_EDITS);

    // The point of the point: an ordinary edit inside the workspace, admitted
    // with nobody asked at all.
    let ordinary = json!({"path": "/w/src/lib.rs", "content": "fn main() {}"});
    assert!(matches!(
        g.admit(&call("write", &ordinary)),
        GateDecision::Admit
    ));

    // And the stores, which the mode cannot reach.
    for path in [
        "/home/op/.ssh/authorized_keys",
        "/home/op/.gnupg/trustdb.gpg",
        "/home/op/.aws/credentials",
        "/home/op/.config/gh/hosts.yml",
    ] {
        let args = json!({"path": path, "content": "x"});
        let d = g.admit(&call("write", &args));
        assert!(
            !matches!(d, GateDecision::Admit),
            "{path} was admitted at automode-edits: {d:?}"
        );
    }
    // A read that would put a key in the transcript is a disclosure, and no
    // point moves it either.
    let read = json!({"command": "cat /home/op/.ssh/id_ed25519"});
    assert!(!matches!(g.admit(&bash(&read)), GateDecision::Admit));
}

/// opencode's `permission` config governs the gate before the mode: `deny`
/// refuses, `allow` admits, `ask` falls through.
#[test]
fn a_permission_config_denies_allows_and_asks() {
    let config = crate::permission::config_to_ruleset(
        &json!({
            // `findLast` semantics: the default `*` comes first, the specific
            // override last.
            "edit": { "*": "ask", "*.secret": "deny" },
            "write": "allow",
        })
        .as_object()
        .unwrap()
        .clone(),
    )
    .unwrap();
    let mut g = AdjudicatedGate::closed().with_permission(config);

    match g.admit(&call("edit", &json!({"path": "src/keys.secret"}))) {
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { .. },
            ..
        } => {}
        other => panic!("a denied permission must refuse, got {other:?}"),
    }

    match g.admit(&call("write", &json!({"path": "x.txt"}))) {
        GateDecision::Admit => {}
        other => panic!("an allowed permission must admit, got {other:?}"),
    }

    // `ask` falls through to the mode; a closed gate fails closed with NotRun.
    match g.admit(&call("edit", &json!({"path": "src/lib.rs"}))) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { .. },
            ..
        } => {}
        other => {
            panic!("an asked permission must fall through (closed -> not_run), got {other:?}")
        }
    }
}

// ---------------------------------------------------------------- supervised

/// An adjudicator that always picks the first option whose kind matches, and
/// records what it was shown.
struct Fixed {
    id: &'static str,
    allow: bool,
    seen: std::sync::Mutex<Option<Option<ModelAdvice>>>,
}

impl Fixed {
    fn new(id: &'static str, allow: bool) -> Self {
        Fixed {
            id,
            allow,
            seen: std::sync::Mutex::new(None),
        }
    }
}

impl Adjudicator for Fixed {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        *self.seen.lock().unwrap() = Some(req.advice.clone());
        let want = |o: &&DecisionOption| o.kind.is_allow() == self.allow;
        match req.options.iter().find(want) {
            Some(o) => AdjudicationDecision::selected(req, &o.id, self.id, "because"),
            None => AdjudicationDecision::unavailable(req, self.id, "no such option"),
        }
    }
    /// Stands in for a model adjudicator, so it reports like one. A double that
    /// answered without saying whether an oracle was consulted would exercise
    /// the `consulted: false` fallback in every test and never the real path.
    fn last_advice(&self) -> Option<ModelAdvice> {
        self.id.starts_with("model").then(|| ModelAdvice {
            consulted: true,
            would: if self.allow { "admit" } else { "ask" },
            by: self.id.to_string(),
            basis: "because".into(),
            cites: Vec::new(),
            latency_ms: 1,
            unsure: None,
        })
    }
    fn describe(&self) -> String {
        self.id.to_string()
    }

    /// **The fixture now carries both halves of an exchange** (R11), because the
    /// corpus columns have to come from the adjudicator that asked an oracle.
    fn last_brief(&self) -> Option<String> {
        Some(format!(
            "brief — {}\ntrail: 1 operator message(s) of 1 scanned",
            self.id
        ))
    }

    fn last_reply(&self) -> Option<String> {
        Some("It follows from what was asked.\nALLOW 0".to_string())
    }
}

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

/// **A gate with no guard model refuses to say it is supervising.**
///
/// Answering "ok" and supervising nothing would leave the operator believing
/// every later call was measured while none were — the same lie `Mode::check`
/// refuses to tell by downgrading silently.
#[test]
fn supervision_without_an_advisor_refuses_by_name() {
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK);
    let e = g
        .set_supervision(true)
        .expect_err("nothing to supervise with");
    assert!(e.contains("gatekeeper"), "{e}");
    assert!(!g.supervising());
}

#[test]
fn with_no_adjudicator_a_write_refuses_and_does_not_claim_a_decision() {
    let mut g = AdjudicatedGate::closed();
    let args = json!({"path": "src/lib.rs"});
    match g.admit(&call("edit", &args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => {
            assert!(why.contains("fails closed"), "{why}");
            assert!(why.contains("nobody decided"), "{why}");
        }
        other => panic!("fail closed means refuse, got {other:?}"),
    }
    assert_eq!(g.log.len(), 1);
    assert_eq!(g.log[0].effect, "refuse");
}

/// An `allow_session` is not asked twice — **at a point whose grant scope is a
/// session**.
///
/// The mode is named rather than left to the default, and that is the point of the
/// test rather than noise in it: the default is `always-ask`, whose scope is
/// `Once`, where an answer settles that call and nothing else. A test that relied
/// on the default here would have been asserting the old behaviour of a gate that
/// had no notion of where it was sitting.
///
/// The coordinate is one nobody named — write asks, and the answer is remembered —
/// which is also the demonstration that the space is reachable beyond the four.
#[test]
fn an_allow_admits_and_a_session_grant_is_not_asked_twice() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(AdjudicationDecision::selected(
            req,
            "allow_session",
            "human:test",
            "fine",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode {
        name: "ask about writes, remember the answer",
        write: crate::mode::Disposition::Ask,
        grants: crate::mode::GrantScope::Session,
        ..crate::mode::Mode::WRITES_ALLOWED
    });
    let args = json!({"path": "src/lib.rs"});
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "an allow_session must not be asked again"
    );
    assert_eq!(g.log.len(), 2, "both calls are rows, grant included");
    assert_eq!(g.grants().len(), 1, "and the grant is listable");
}

/// **A `Once` point offers the session grant its *Always allow* implies, and
/// the gate honours it.**
///
/// This test once asserted the opposite: the option was withheld at a point
/// whose grants are `Once`, because the mode's contract and the gate had to
/// agree. What unbalanced it was *Always allow* — the one option that
/// reaches past the session was on the ladder at a point that was not
/// supposed to grant past the call. The operator, 2026-09-18: *"if I'm
/// offered to permanently enable — it is stronger than allow_session — so
/// allow_session should always be offered too."* It is offered, and the
/// answer records a grant that settles the next call: a button whose effect
/// the gate would decline is the defect this ladder once shipped, in the
/// other direction.
#[test]
fn a_once_scoped_point_offers_the_session_grant_and_the_gate_honours_it() {
    use std::sync::{Arc, Mutex};
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        s.lock()
            .unwrap()
            .extend(req.options.iter().map(|o| o.id.clone()));
        a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(AdjudicationDecision::selected(
            req,
            "allow_session",
            "human:test",
            "ok",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode::ALWAYS_ASK);
    let args = json!({"path": "src/lib.rs"});
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);

    let ids = seen.lock().unwrap().clone();
    assert!(
        ids.iter().any(|i| i == "allow_session"),
        "a point whose grants are `Once` must still offer the session grant \
             while it offers the durable rule: {ids:?}"
    );
    assert!(
        ids.iter().any(|i| i == "allow_always"),
        "and the durable rule is still offered: {ids:?}"
    );
    assert!(
        ids.iter().any(|i| i == "allow_once"),
        "allowing this one call must still be offered: {ids:?}"
    );
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the second call was settled by the grant, not asked again"
    );
    assert_eq!(g.grants().len(), 1, "the answer stands for the session");
}

/// **An `allow_session` answer at `always-ask` stands for the session.**
///
/// This test once asserted the opposite, in two stages: first that the grant
/// was silently dropped, then — after the option was withdrawn from the
/// ladder — that naming it was a non-conforming answer and the gate failed
/// closed. Both stages served the mode's contract, *an answer settles that
/// call and nothing else*. The operator retired the contract on 2026-09-18:
/// *"if I'm offered to permanently enable — it is stronger than
/// allow_session — so allow_session should always be offered too."* The
/// durable rule was already on the ladder here; the weaker grant follows it,
/// and an explicit answer is the operator overriding the point's default.
///
/// What the mode still means: the SECOND call is not asked about, because the
/// grant covers it — not because the mode stopped asking. A call outside the
/// grant's class asks again, as the sibling test pins.
#[test]
fn at_always_ask_an_allow_session_answer_stands_for_the_session() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(AdjudicationDecision::selected(
            req,
            "allow_session",
            "human:test",
            "fine",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode::ALWAYS_ASK);
    let args = json!({"path": "src/lib.rs"});
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the grant the answer took settles the second call"
    );
    assert_eq!(g.grants().len(), 1, "and the grant is recorded");
}

/// **At `writes allowed` a write is not asked at all**, and the row still exists.
///
/// The second half is the one worth a test: an admission nobody was consulted
/// about is exactly the kind that stops appearing in an audit, and §4c's corpus
/// wants it — *what the operator decided* includes the mode they put this project
/// at.
#[test]
fn at_writes_allowed_a_write_goes_through_and_is_still_a_row() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(AdjudicationDecision::selected(
            req,
            "deny",
            "human:test",
            "no",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode::WRITES_ALLOWED);
    let args = json!({"path": "src/lib.rs"});
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "nothing was consulted"
    );
    assert_eq!(g.log.len(), 1, "and it is still in the audit");
    assert_eq!(g.log[0].effect, "admit");
    assert!(
        g.log[0].decision.by.contains("mode"),
        "{:?}",
        g.log[0].decision
    );
}

#[test]
fn a_deny_is_denied_and_carries_the_request_id() {
    let adj = AskAdjudicator::new("test", |req: &AdjudicationRequest| {
        Some(AdjudicationDecision::selected(
            req,
            "deny",
            "human:test",
            "not that file",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj));
    let args = json!({"path": "src/lib.rs"});
    match g.admit(&call("edit", &args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { req_id },
            ..
        } => {
            assert!(req_id.starts_with("adj-"), "{req_id}");
        }
        other => panic!("a deny is a decision, got {other:?}"),
    }
}

#[test]
fn an_answer_naming_an_option_that_was_not_offered_never_opens_the_gate() {
    let adj = AskAdjudicator::new("test", |req: &AdjudicationRequest| {
        Some(AdjudicationDecision::selected(
            req,
            "yes_obviously",
            "model:small",
            "looks fine",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj));
    let args = json!({"path": "src/lib.rs"});
    match g.admit(&call("edit", &args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => {
            assert!(
                why.contains("non-conforming") || why.contains("did not offer"),
                "{why}"
            );
        }
        other => panic!("a non-conforming answer must not admit: {other:?}"),
    }
}

#[test]
fn the_never_list_is_checked_before_the_adjudicator_and_it_cannot_be_overridden() {
    let consulted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = consulted.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(AdjudicationDecision::selected(
            req,
            "allow_always",
            "human:test",
            "yes to everything",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj));
    for path in [
        ".ssh/authorized_keys",
        "home/x/.aws/credentials",
        ".git/HEAD",
    ] {
        let args = json!({"path": path});
        assert!(
            matches!(g.admit(&call("write", &args)), GateDecision::Refuse { .. }),
            "{path} must be refused"
        );
    }
    assert_eq!(
        consulted.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the never rows are evaluated before dispatch"
    );
}

#[test]
fn nobody_answering_is_a_timeout_and_the_gate_still_refuses() {
    let adj = AskAdjudicator::new("test", |_: &AdjudicationRequest| None);
    let mut g = AdjudicatedGate::new(Box::new(adj));
    let args = json!({"path": "src/lib.rs"});
    match g.admit(&call("edit", &args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => {
            assert!(why.contains("timeout"), "{why}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_console_ask_is_a_real_ask_and_the_brief_is_what_a_human_reads() {
    let out = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
    struct Shared(std::sync::Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let adj = ConsoleAdjudicator::new(
        "deadtrickster",
        Box::new(std::io::Cursor::new(
            b"allow_once because I said so\n".to_vec(),
        )),
        Box::new(Shared(out.clone())),
    );
    let mut g = AdjudicatedGate::new(Box::new(adj));
    let args = json!({"path": "src/lib.rs", "old_string": "a", "new_string": "b"});
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);

    let shown = String::from_utf8(out.lock().unwrap().clone()).unwrap();
    assert!(
        shown.contains("write,host_project,reversible,free"),
        "{shown}"
    );
    assert!(shown.contains("allow_once"), "{shown}");
    assert!(shown.contains("old_string"), "{shown}");
    assert_eq!(g.log[0].decision.basis, "because I said so");
}

#[test]
fn a_long_argument_is_summarised_in_the_brief_rather_than_pasted() {
    let big = "x".repeat(10_000);
    let args = json!({"path": "a.txt", "content": big});
    let mut g = AdjudicatedGate::closed();
    let req = g.request_for(&call("write", &args));
    let brief = req.brief();
    assert!(brief.len() < 4_000, "{} bytes", brief.len());
    assert!(brief.contains("bytes omitted"), "{brief}");
}

#[test]
fn the_startup_disclosure_names_whichever_gate_is_shut() {
    // Three states, and only one of them is "on". A daemon that printed the
    // same line in all three would be asserting a property it does not have
    // in two of them.
    let (state, detail, active) = startup_disclosure("none attached", true, true);
    assert_eq!(state, "NONE");
    assert!(!active);
    assert!(
        detail.contains("WRITE TOOLS and no adjudicator"),
        "{detail}"
    );

    let (state, detail, active) = startup_disclosure("console ask", false, true);
    assert_eq!(state, "GATE ONLY");
    assert!(!active);
    assert!(detail.contains("READ-ONLY"), "{detail}");

    let (_, detail, active) = startup_disclosure("console ask", true, true);
    assert!(active);
    assert!(detail.contains("takes effect"), "{detail}");

    // And the M1 sentence stays true for an M1 session.
    let (state, _, active) = startup_disclosure("none attached", false, false);
    assert_eq!(state, "N/A");
    assert!(!active);
}

/// **The sentence names the classes the session actually seated.**
///
/// The defect this replaced: a caller whose gated tools were `Access::Exec` —
/// the runner role's five job verbs and `monitor`, no `write` anywhere — had to
/// pass `has_write_tools: true` to get the on/off logic right, and the banner
/// then said *"Write tools are callable"* about a session with no write tools.
/// Correct about the boundary and wrong about the session, which is the same
/// shape as the four instances `harnessd`'s `GateWiring` already paid for.
///
/// Nothing here pins a wording. What it pins is that the wording **moves** with
/// the classes, and that a class the session does not have never appears.
#[test]
fn the_disclosure_names_the_classes_seated_and_not_write_by_default() {
    let exec = startup_disclosure_for("console ask", true, &["exec"], true).1;
    assert!(exec.contains("EXEC"), "{exec}");
    assert!(
        !exec.to_lowercase().contains("write"),
        "a session with no write tools was told its write tools are callable: {exec}"
    );

    let both = startup_disclosure_for("console ask", true, &["write", "network"], true).1;
    assert!(both.contains("WRITE"), "{both}");
    assert!(both.contains("NETWORK"), "{both}");
    assert_ne!(both, exec);

    // A read-only backend stops a *write* and says so in those terms; for an
    // exec or network seat it is a different mismatch and must not be described
    // as a blocked write.
    let shut_write = startup_disclosure_for("console ask", false, &["write"], true).1;
    assert!(shut_write.contains("cannot reach the disk"), "{shut_write}");
    let shut_exec = startup_disclosure_for("console ask", false, &["exec"], true).1;
    assert_ne!(
        shut_exec, shut_write,
        "an exec seat over a read-only backend is not a blocked write"
    );

    // Empty classes is still the M1 sentence, and it is the only state that
    // may claim nothing can reach the gate.
    assert_eq!(
        startup_disclosure_for("none attached", false, &[], true).0,
        "N/A"
    );
}

// -----------------------------------------------------------------------
// The four outcome classes, at the gate.
// -----------------------------------------------------------------------

fn pinned() -> crate::intent::Surroundings {
    crate::intent::Surroundings {
        scratch: None,
        home: Some("/home/dead".into()),
        workspace: Some("/w".into()),
        shell: crate::intent::ShellTrust::Pinned {
            how: "test fixture".into(),
        },
        seen_hosts: BTreeSet::new(),
    }
}

/// **A python heredoc that only writes rides the EDIT path** — the operator's ruling of
/// 2026-10-05, asserted where *"permission mechanics"* actually live.
///
/// The ruling, in their words: *"since we try to catch python heredoc edits and render them
/// with diffs - they have to be classified as edits for permission mechanics"*, and the mode
/// they are running answers the other half: *"so we are in allow-edits mode now. there is
/// certain permission set that is allowed and doesnt need oracle. this is to be kept."* So
/// the assertions are (a) the class is `write` and the headline says so, (b) in allow-edits
/// that is the mode's own allowed set and nobody is asked, and (c) in an ask mode the same
/// call asks as a WRITE — while every shape that is not a placed write keeps `exec` and goes
/// on asking in both.
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

    // **The class, and the headline a person reads.** This call was `exec access` over
    // intents that said `write_file`, which is the same defect as the read one with the
    // consequence the other way round: exec asks at EVERY point, so the edit path — the
    // mode's write rules, the shape cache, the grant vocabulary — was unreachable.
    let (mut g, asked) = gate(crate::mode::Mode::AUTO_EDITS);
    let row = g.request_for(&bash(&write));
    assert_eq!(row.class.access, Access::Write, "{}", row.summary);
    assert!(
        row.summary.contains("write access"),
        "the headline says: {}",
        row.summary
    );
    // **And in allow-edits that is the mode's allowed set.** Nobody is consulted, which is
    // the operator's *"certain permission set that is allowed and doesnt need oracle"*.
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

    // **In ask mode it still asks, and it asks as a write.** The oracle is shown a write
    // and the card says so, which is the point of classifying it rather than of admitting
    // it.
    let (mut g, asked) = gate(crate::mode::Mode::ALWAYS_ASK);
    let row = g.request_for(&bash(&write));
    assert_eq!(row.class.access, Access::Write, "{}", row.summary);
    assert_eq!(g.admit(&bash(&write)), GateDecision::Admit);
    assert_eq!(asked.load(Ordering::Relaxed), 1, "nobody was asked");

    // **Every shape that is not a placed write keeps exec, in both modes.** Each is one of
    // `writes_only`'s guardrails, and each is asserted at the gate because the guardrail is
    // what keeps this narrowing from becoming "any interpreter is an edit".
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

fn bash<'a>(args: &'a Value) -> GateCall<'a> {
    GateCall {
        name: "bash",
        access: Access::Exec,
        args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/w",
        target_exists: None,
        scripts: &[],
    }
}

/// A gate that would admit anything an adjudicator is consulted about, so that a
/// refusal in these tests can only have come from layer A.
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

/// **R21 — a shell that only reads is judged on the work, and the four ways it is
/// not.**
///
/// `head-parity-2026-09-21.md` **R21**, raised by the operator looking at a gate on
/// `head`: *"wonder why oracle didnt allow head itself, or why head wasn't
/// categorized as Read."* Both halves had answers and neither was the defect — `head`
/// **is** a read in the classifier, and the oracle was never asked to allow it — and
/// what was left was that **the vehicle outranked the work**: a shell that only reads
/// gated as exec, and the card said *exec access* over intents that said *read*.
///
/// **Measured, 2026-09-22, over every `bash` call this box has gated** (7318 rows,
/// from the store's own `arguments_json`): 1008 rows (13.8%) are read-only by this
/// rule, and **977 of the 5153 rows that were ASKED — 19.0% — stop gating.** The two
/// numbers are the point of this test in the small and of the commit in the large.
///
/// The four guardrails are asserted failing, because the ruling says the rule is
/// *paid for* by the parser's refusals and void the moment a line carries something it
/// could not read. Each of them is a shape the corpus actually holds.
#[test]
fn a_shell_that_only_reads_is_judged_on_the_work() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // **A fresh gate per case.** A human `allow_session` on one call is a standing
    // permission whose coverage is `intents.is_subset`, so a later call in the same
    // gate can be settled by a grant taken over an earlier one — real behaviour, and
    // noise in a test about which access a call is judged at. One gate per assertion,
    // or the guardrails measure each other.
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
        // `always-ask` is the strongest form of the assertion: clause 4 admits a read
        // at **every** point, including the one whose whole purpose is to ask.
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_exec_follows_mode(false)
        .with_surroundings(pinned());
        (g, asked)
    };

    // **The yes.** The operator's own example: a `cd`, then a reader. It asked before
    // this, and the card it asked with said `exec access` over `[inspect read_file]`.
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
    // And the row records it as the mode's doing, naming the access the call was
    // judged at rather than the tool's declaration.
    let last = g.log.last().expect("a row");
    assert_eq!(last.decision.by, "gate:mode");
    assert!(
        last.decision.basis.contains("admits read calls"),
        "{}",
        last.decision.basis
    );

    // **The ways it is not a read.** Each keeps the exec class and each still asks —
    // the guardrails are the requirement, so they are asserted as loudly as the yes.
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
        // **A verb that had to be MEASURED to be here.** `git symbolic-ref` reads a ref, so it
        // sat in layer A's read arm — and it was ALSO in the write arm, where the match could
        // never reach it. Source order decided the classification: this call came back
        // `intents ["inspect"]`, `reads_only` true, so `judged_access` narrowed it to
        // `Access::Read` and `Mode::disposition` admits a read at every point. The operator
        // was never asked, and nothing on any screen said so, because what the
        // misclassification suppressed was a PROMPT.
        //
        // It is a write: `git symbolic-ref HEAD refs/heads/other` sets `.git/HEAD` to
        // `refs/heads/other` (measured against a scratch repo), and the man-page scrape in
        // `docs/evidence/man-scrape/` says so in as many words — *"This subcommand can delete
        // symbolic refs that existed before"* — while `FLAG_RULES` already carried `Destroy`
        // for its `-d` spelling. Two signals and neither reached the base arm.
        //
        // Here rather than only in `intent.rs`, because THIS is where the consequence is: the
        // class must be the vehicle's and `asked` must be 1.
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

    // **A secret read through a shell is refused, which is stronger than asking.** It
    // is the one case where being judged a read would have been *worse* than being
    // judged exec: `cat ~/.ssh/id_rsa` is a read by every intent it computes, and the
    // thing that stops it is the flow rule, which sees the secret store in the region
    // table and refuses before any point is read. So the boundary conjunct in
    // `reads_only` is what keeps this call's exec access, and the refusal is what
    // happens next.
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

    // **And a line the parser refused is now a line the POINT decides about**, which is
    // the 2026-10-02 ruling: *"in allow-all, stop refusing a command because the
    // grammar could not resolve a construct"*. It is still not a read — `$(ls)` has no
    // known prefix and `cat` takes a path, so the scope widened to `unbounded` and the
    // boundary conjunct in `reads_only` voids — so it keeps its exec access and the
    // mode is consulted exactly as it would be for the literal spelling.
    //
    // **AND THE MODEL IS THE ONE ASKED** — the operator's ruling of 2026-10-05 overrules
    // what stood here (*"a meaning nobody could read is never handed to a model to
    // approve"*): *"we have to ask oracle for this stuff"*, and *"i want to follow oracle
    // verdict … the rest is oracle."* An unresolved line is now the case a second opinion
    // exists for, so `Adjudicable` IS minted and the oracle answers it — the person is
    // asked when the oracle is unsure (or refuses nothing and has no answer), never
    // because the parser gave up.
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

/// **`rm -rf /` is not refused on the strength of the string.** This is the operator's
/// own case, and the test carries no attribute for a while — an insertion landed between
/// its `#[test]` and its `fn`, which left the attribute on the next test's doc comment and
/// this function unreachable. The compiler said so (`never used`) and nobody read it. It is
/// back, because a test that stopped running is worse than a test that fails.
#[test]
fn rm_rf_slash_is_not_blocked_by_the_gate_and_reaches_a_decision() {
    // The operator's case: `rm -rf /` can be allowed if it is the intent. What the
    // gate must NOT do is refuse it on the strength of the string.
    let mut g = permissive_gate();
    let args = json!({"command": "/bin/rm -rf /"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(g.log[0].request.tier.as_str(), "always_ask");
    assert_eq!(g.log[0].effect, "admit");
}

#[test]
fn an_always_ask_is_never_turned_into_a_standing_grant() {
    // The adjudicator asks for the widest grant it is allowed to name each time. A
    // class grant over a deletion outside the project is the standing authority the
    // fixed list withholds, so the second call must be asked about again.
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
    // And it was never even OFFERED a standing grant: the tier is `AlwaysAsk`,
    // whose ladder is `always_ask_options` — no session grant, no durable rule.
    // This half is untouched by the 2026-09-18 revision, which completed the
    // ladder at points that offer the durable rule; the always-ask TIER offers
    // no durable rule and so offers no session grant either.
    assert!(g.log[0].request.option("allow_session").is_none());
    assert!(g.log[0].request.option("allow_always").is_none());

    // An ordinary may-approve exec call, though, now takes the session grant
    // its ladder offers — the operator's revision of 2026-09-18, which retired
    // the exec exception the same way the 2026-09-14 revision had already
    // broken it with the durable rule. The second identical call is settled by
    // the grant, not asked again.
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
    // And the grant is scoped to what the call showed, not to exec at large: a
    // different program is outside it and asks again.
    // **A program the grant does not name** (R21: `cat` is a read and would be
    // admitted by clause 4 without asking anybody, so it can no longer be the
    // fixture for "a different program asks again").
    let other = json!({"command": "/usr/bin/awk {print} /w/src/lib.rs"});
    assert_eq!(g.admit(&bash(&other)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        4,
        "a program the grant does not name asks again"
    );
}

#[test]
/// **Exec asks, and the ladder it asks with is complete.**
///
/// The operator's rule of 2026-09-11 — *"i want something very simple for
/// now - exec and bash always ask me for permission"* — still holds as the
/// DEFAULT: nothing settles an exec call before somebody answers. What
/// changed twice is what an answer can reach. The revision of 2026-09-14 put
/// the durable *Always allow* rule on the ladder; the revision of
/// 2026-09-18 — *"if I'm offered to permanently enable — it is stronger than
/// allow_session — so allow_session should always be offered too"* — put the
/// session grant beside it. The command below is a read inside the workspace,
/// layer A's weakest verdict, because it is exactly the call clause 4 would
/// wave through if the access class let it.
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
    // **`sed -n` and not `cat`** (R21): the command has to be an exec-class call
    // that still ASKS, and a shell that only reads is clause 4 now. This one is the
    // classifier's own exec — see `Baseline::reads_only` — so the ladder is still
    // exercised by a call that reaches it.
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
    // The revision of 2026-09-14: a DURABLE rule is offered — it is the
    // operator's own preapproval, in a file they read — and it is not a grant.
    assert!(
        g.log
            .iter()
            .all(|r| r.request.option("allow_always").is_some()),
        "exec is offered Always allow, as a rule"
    );
    // And the grant is scoped to the program it was taken over: a different
    // program is outside it and asks again. **Exec, so that asking is what this
    // measures** (R21): `ls` is a look and a shell carrying one is clause 4, which
    // would be admitted without reaching a ladder at all.
    let other = json!({"command": "/usr/bin/awk {print} /w/src/lib.rs"});
    assert_eq!(g.admit(&bash(&other)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "a program the grant does not name asks again"
    );
}

/// **The label names the rule the button writes.**
///
/// The operator's report (2026-09-18): the offer said "add tool and verb to
/// the permissions file" without ever saying **which**, and did not let them
/// adjust it. The derived pattern is in the label now — the same helper the
/// recording site uses, so what is signed and what is written cannot drift —
/// **A gate with a durable sink carries on numbering where it left off.**
///
/// Without this, `next_id` restarts at 1 on every daemon and `INSERT OR
/// IGNORE` drops every colliding row — the whole-session corpus outage
/// measured in the operator's store on 2026-09-20.
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

/// **The consented point admits a shell command without asking.**
///
/// The operator, 2026-09-20: *"switched leticl session to allow-all, pressed
/// y, got a warning, still asked about grep"*. The move itself is tested in
/// `wired`; this is the half that matters afterwards — that the point the
/// move lands on actually stops asking about `bash`.
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

/// **The durable rule names the program that needs authority, not the first
/// word of the line.**
///
/// `always_pattern_for_command` took `command.split_whitespace().next()`,
/// which is the program only when the command is one stage. On the
/// operator's screen, 2026-09-20, `cd /home/dead/Projects/letibot/letibot &&
/// python3 - <<'PY' …` offered *Allow `python3` for the session* beside
/// *Always allow `cd*`* — two derivations of the same question, and the
/// durable one, the one written to their config, had the naive answer.
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

/// **A rule the matcher could never consult is not offered at all.**
///
/// `bash_segments` refuses to split a command carrying a heredoc, and
/// `evaluate_bash` turns that into `Ask` before any rule is read — so for
/// these commands *Always allow* is inert however it is spelled. Offering it
/// invites the operator to sign something durable that does nothing, and
/// then asks them again next time. The rule this seam already states: *"an
/// operator is never shown a button whose effect the gate would then decline
/// to honour."*
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

/// and a glob typed after `allow_always` in the composer still overrides it.
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

/// *"good old Allow Always"* (the operator, 2026-09-14): the answer writes a
/// rule over the program and its verb, the rule admits the next call before
/// the mode is consulted, `git log; rm x` under `git log*` still asks, and
/// the sink saw the rule once.
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
    // `cargo run` is not on the shipped list, so the first call asks.
    let args = json!({"command": "cargo run --bin x"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
    let rules = written.lock().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].permission, "bash");
    assert_eq!(rules[0].pattern, "cargo run*");
    drop(rules);
    // The second, different `cargo run` is admitted by the rule, nobody asked.
    let again = json!({"command": "cargo run --bin y -- --flag"});
    assert_eq!(g.admit(&bash(&again)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
    // A compound that hides a second command behind the prefix asks.
    let hidden = json!({"command": "cargo run; rm -rf /w"});
    assert_eq!(g.admit(&bash(&hidden)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 2);
    // And the shipped list admits a read-only git without asking at all.
    let ro = json!({"command": "git status --short"});
    assert_eq!(g.admit(&bash(&ro)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 2);
}

/// **The operator's own command, at `allow-all`: admitted, with nobody asked.**
///
/// This is the case that filed the ruling, verbatim from their pane:
///
/// ```text
/// Ran "cd … && make lint; echo \"exit=$?\"; …"   · not run · 17 lines
/// this command's meaning does not exist yet, so nothing can decide about it.
/// who decided: boundary:normaliser
/// basis: … parameter_expansion: `$?` … decides the argument: "\"exit=$?\""
/// ...for allow all mode
/// ```
///
/// The `$?` widens nothing — `echo` takes a string, not a place — so the command
/// reads as it would with a literal `$?` replaced by `0`, and the point admits it.
///
/// **Measured at the point that has the rule in it**: `ALLOW_ALL_HERE`, which is
/// `exec: Admit` and `OperatorConsented`, with an adjudicator that FAILS if it is
/// consulted. Nothing is asked, which is the whole of what "for allow all mode"
/// meant. The second half is the other side of the ruling: the same command at
/// `always-ask` is NOT admitted by the point, so a person is asked — the widening is
/// a scope, not a bypass.
#[test]
fn the_operators_echo_dollar_question_is_admitted_at_allow_all_and_asks_elsewhere() {
    use crate::mode::Mode;

    // **`NoAdjudicator` fails closed on everything**, so an `Admit` here can only have
    // come from the point. That is a stronger statement than "nobody was asked": it
    // is that nobody COULD have admitted it and it was admitted anyway.
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
    // And the row still says what was not read, which is the disclosure that replaces
    // the refusal.
    assert!(!permissive.log[0].request.resolved);
    assert!(
        permissive.log[0]
            .request
            .baseline
            .contains("the meaning was never resolved"),
        "the row must still say what was never read: {}",
        permissive.log[0].request.baseline
    );

    // The same command where the point does not admit exec: a person decides, and no
    // model is offered the question.
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

/// **A command the grammar could not read is admitted at `allow-all`, and says so.**
///
/// The operator's case, verbatim from the session store (2026-10-07): a heredoc whose
/// opening line also redirects and pipes. Valid bash; tree-sitter-bash recovers from it
/// at the `|`, so layer A's verdict is `NotRun` — which refused it before any mode was
/// consulted, `allow-all` included. *"im very tired of substitutions - at least for
/// allow-all"*.
///
/// At `allow-all` it now goes on to the mode, which admits it, and the decision says it
/// ran unread. At a point that does not admit everything the refusal is unchanged.
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

/// **A standing grant never settles a command whose meaning was never resolved.**
///
/// This is the property the instruction of 2026-10-02 protects — *"that field must
/// stop being derived from it and start being carried on its own"* — and it is the
/// one place the change could have gone wrong invisibly. `Grant::covers` takes
/// `resolved` as an argument and refuses an unresolved action before it looks at
/// coverage (its own unit tests cover that); what those tests cannot see is whether
/// the GATE passes the right thing. It used to derive the flag from the verdict, and
/// the verdict for an unresolvable construct is no longer `NotRun` — so a version that
/// kept deriving it would pass `true` here and an `allow_session` taken over one
/// `cat` would quietly cover every later `cat $ANYTHING`.
///
/// The pair is chosen so the grant CANNOT be refused for an incidental reason: the
/// two calls must have the same class and the same intents, which the assertion says
/// out loud rather than assuming. If that ever stops holding, this test fails on the
/// assertion rather than passing vacuously.
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

    // Second: the same shape with one word the grammar cannot read. Same program,
    // same class, same intents — so only `resolved` can keep the grant off it.
    let opaque = json!({"command": "cd /w && make $TARGET"});
    let resolved_row = g.request_for(&bash(&plain));
    let opaque_row = g.request_for(&bash(&opaque));
    assert_eq!(
        resolved_row.class, opaque_row.class,
        "the pair must differ only in resolution, or this test proves nothing"
    );
    // **THE MODEL IS ASKED NOW**, which is the 2026-10-05 ruling and the reason this
    // assertion changed: a session grant covers a RESOLVED call, and the unresolved twin
    // is answered by the oracle rather than by the point or the person. What the pair
    // still proves is that `resolved` is carried separately — the two requests differ in
    // nothing else — and that the grant itself does not silently cover it.
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

/// **An unresolvable command now reaches the POINT, and the point decides.**
///
/// This test was `an_unresolvable_command_reaching_the_gate_is_not_run_and_never_admitted`
/// and it asserted a refusal. The instruction of 2026-10-02 changed that — *"in
/// allow-all, stop refusing a command because the grammar could not resolve a
/// construct"* — so `/bin/cat $FILE` is admitted at a permissive point, and the two
/// facts that make that safe are what this pins:
///
/// * **`resolved` is still `false`.** It is carried on its own now rather than read
///   off the verdict, so no corpus row claims a resolution that never happened.
/// * **`adjudicable()` is `Some` now** — the operator's ruling of 2026-10-05 (*"the rest
///   is oracle"*) overrules the sentence that stood here (*"a meaning nobody could read
///   is never handed to a MODEL to approve"*). At a permissive point the action is
///   admitted before any gate, so nothing is asked; at a narrow one the oracle answers,
///   and only its UNSURE reaches a person.
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

/// **And the same command at a point that does not admit it asks whoever this gate
/// wires** — a person, in this fixture.
///
/// The other half of the ruling, and the half that keeps it from being a bypass: the
/// widening is not an admission, it is a *scope*, and a narrow point finds an unbounded
/// scope outside its grant for the same reason it refuses the literal. MEASURED here —
/// `always-ask` with `exec_follows_mode` off, which is the strongest form of the
/// assertion because no point admits exec there.
///
/// **What changed on 2026-10-05 is only whose adjudicator may answer.** The witness is
/// minted for an unresolved action now (the operator: *"the rest is oracle"*), so
/// `adjudicable()` is `Some` — and in THIS gate the adjudicator is `AskAdjudicator`, a
/// person. That is why the test still reads as it does: the witness says *an adjudicator
/// may answer this*, and which adjudicator is the gate's wiring, not the witness's.
#[test]
fn an_unresolvable_command_at_a_narrow_point_reaches_a_person_and_not_a_model() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            // The person is shown the widened scope, which is the point of widening
            // rather than refusing: they can see *what* is unknown.
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
    // The alias defeat: `AdjudicatedGate::new` leaves `ShellTrust::Unknown`, so a
    // gate nobody configured refuses a name that something upstream could redefine.
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

// -----------------------------------------------------------------------
// Surfacing, and the breaker.
// -----------------------------------------------------------------------

#[test]
fn every_denial_reaches_the_operator_with_the_grant_path_attached() {
    // The operator's complaint, closed: they are told when it happens, and the
    // notice carries what they can do about it.
    let sink = std::sync::Arc::new(crate::authorise::RecordingDenialSink::default());
    struct Shared(std::sync::Arc<crate::authorise::RecordingDenialSink>);
    impl crate::authorise::DenialSink for Shared {
        fn denied(&self, n: &crate::authorise::DenialNotice) {
            self.0.denied(n);
        }
    }
    let mut g = AdjudicatedGate::closed()
        .with_surroundings(pinned())
        .with_denial_sink(Box::new(Shared(sink.clone())));
    let args = json!({"command": "/bin/rm /w/x.rs"});
    assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
    assert_eq!(sink.len(), 1);
    let n = sink.last().unwrap();
    assert_eq!(n.outcome, "not_run");
    assert!(n.grant.contains("grant"), "{}", n.grant);
    assert!(n.grant.contains("Nothing was executed"), "{}", n.grant);
    assert!(n.headline().starts_with("REFUSED bash"));
}

#[test]
fn three_refusals_in_one_direction_open_the_breaker_and_stop_asking() {
    // What stops a classifier being argued down one call at a time: after three,
    // the gate does not produce another verdict at all.
    let consulted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = consulted.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "no",
        move |req: &AdjudicationRequest| {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "deny",
                "human:test",
                "no",
            ))
        },
    )))
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));

    // Three re-spellings of one intention. The counter must not be reset by the
    // re-spelling, which is the whole point of the direction key.
    // **Three re-spellings of one intention, and they have to be exec** (R21):
    // `cat`/`head`/`tail` are reads inside the workspace and clause 4 admits them
    // without asking anything, so a breaker test built on them would never open.
    // `sed -n` is the same direction and still asks (see `Baseline::reads_only`).
    for cmd in [
        "/bin/sed -n 1p /w/secret-ish.txt",
        "/usr/bin/sed -n 2p /w/other.txt",
        "/usr/bin/sed -n 3p /w/third.txt",
    ] {
        let args = json!({"command": cmd});
        assert!(
            matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }),
            "{cmd}"
        );
    }
    assert_eq!(consulted.load(std::sync::atomic::Ordering::Relaxed), 3);

    // The fourth is not adjudicated at all — and it is exec for the same reason the
    // three above are (R21): `less` is a read, so it would be clause 4 and no
    // breaker could ever reach it.
    let args = json!({"command": "/usr/bin/sed -n 4p /w/fourth.txt"});
    match g.admit(&bash(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            tell,
        } => {
            assert!(why.contains("circuit breaker is open"), "{why}");
            assert!(tell.contains("breaker is OPEN"), "{tell}");
            assert!(tell.contains("only the operator can lift it"), "{tell}");
        }
        other => panic!("the fourth must not reach the adjudicator: {other:?}"),
    }
    assert_eq!(
        consulted.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "an open breaker consults nobody"
    );
}

#[test]
fn an_admission_clears_the_direction_so_the_breaker_is_not_a_permanent_ban() {
    let answers = std::sync::Arc::new(Mutex::new(vec!["deny", "deny", "allow_once", "deny"]));
    let a = answers.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "mixed",
        move |req: &AdjudicationRequest| {
            let mut q = a.lock().unwrap();
            let pick = if q.is_empty() { "deny" } else { q.remove(0) };
            Some(AdjudicationDecision::selected(req, pick, "human:test", "…"))
        },
    )))
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    // Exec, and still asking (R21): a read would be admitted and there would be no
    // refusal to count. **The fourth is a different SHAPE and the same direction**,
    // because a human `allow_once` on a `MayApprove` exec call records an approved
    // shape — so an identical fourth call would be settled by that cache and never
    // reach the breaker this test is about. Two programs, one direction: the count
    // is keyed on the direction, and the shape is not.
    let args = json!({"command": "/bin/sed -n 1p /w/a.txt"});
    assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
    assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    // The loop closed, so the count is gone and the next refusal starts over.
    let args = json!({"command": "/usr/bin/awk {print} /w/a.txt"});
    assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
    assert!(g.breaker.open_directions().is_empty());
}

#[test]
fn a_second_attempt_at_the_same_thing_is_named_as_one_rather_than_read_as_fresh() {
    let mut g = AdjudicatedGate::closed().with_surroundings(pinned());
    let args = json!({"command": "/bin/rm /w/x.rs"});
    let _ = g.admit(&bash(&args));
    match g.admit(&bash(&args)) {
        GateDecision::Refuse { tell, .. } => {
            assert!(tell.contains("attempt 2"), "{tell}");
            assert!(tell.contains("go to a human"), "{tell}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_trail_source_is_what_puts_the_operators_words_in_front_of_an_adjudicator() {
    // The seam §2 needs. Without it the trail says NOT COLLECTED, which is a
    // different fact from the operator having said nothing.
    let mut bare = AdjudicatedGate::closed().with_surroundings(pinned());
    let args = json!({"command": "/bin/ls /w"});
    let req = bare.request_for(&bash(&args));
    assert!(!req.trail.was_collected());
    assert!(req.trail.render().contains("NOT COLLECTED"));

    let mut wired = AdjudicatedGate::closed()
        .with_surroundings(pinned())
        .with_trail_source(|_| {
            crate::authorise::AuthorisationTrail::from_messages(
                vec![crate::authorise::Utterance::operator("yeah restart it", 1).at_seconds(30)],
                18,
            )
        });
    let req = wired.request_for(&bash(&args));
    assert!(req.trail.was_collected());
    assert!(req.trail.render().contains("1 operator message(s) of 18"));
    assert!(req.trail.render().contains("yeah restart it"));
}

#[test]
fn escalation_is_refused_with_the_reason_and_not_treated_as_an_allow() {
    let adj = AskAdjudicator::new("test", |req: &AdjudicationRequest| {
        Some(AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Escalate {
                to: "human".into(),
                why: "I am not sure".into(),
            },
            by: "model:small".into(),
            basis: "unsure".into(),
            latency_ms: 1,
        })
    });
    let mut g = AdjudicatedGate::new(Box::new(adj));
    let args = json!({"path": "src/lib.rs"});
    match g.admit(&call("edit", &args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => {
            assert!(why.contains("escalated"), "{why}")
        }
        other => panic!("{other:?}"),
    }
}

// -------------------------------------------------------------------------
// R11 — the audit rows, read back at decision time
// -------------------------------------------------------------------------

/// The same call with a turn id that carries a sequence, so ages are
/// computable. `bash` names its turn "t1", which parses to nothing and is the
/// honest `None` everywhere else.
fn bash_at<'a>(args: &'a Value, turn_id: &'a str) -> GateCall<'a> {
    GateCall {
        turn_id,
        ..bash(args)
    }
}

/// §4h's loop, closed: a second call of the same shape is shown the first
/// answer, with its count and its age. The prior rows are keyed on the task
/// direction — `(tool, intents, scope, regions)` — so a re-spelled command in
/// the same direction matches and a different direction does not.
#[test]
fn a_second_call_of_the_same_shape_shows_the_first_answer_with_its_count_and_age() {
    // **A PERSON's answer, and only a person's.** This used to run against a
    // closed gate, whose fail-closed refusal is nobody's judgement, and assert
    // that it came back as history. It does not any more, and the reason is the
    // operator's: *"self recursion can do drift — 'that command was approved
    // already by someone and this is ever so slightly different'. we dont
    // include its own decisions to the brief."* A gate shown its own answers
    // ratchets; only what a human decided is evidence about what anyone wanted.
    let mut human = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "the person said yes",
            ))
        },
    )))
    .with_surroundings(pinned());
    // **Exec, and still asking** (R21). `ls` is a look and nothing else, so a shell
    // carrying it is clause 4 and no human answers — and this test is about what a
    // HUMAN's answer leaves behind in the brief. `sed -n` is the exec that still
    // reaches one; see `Baseline::reads_only`.
    let args = json!({"command": "/bin/sed -n 1p /w"});

    let req1 = human.request_for(&bash_at(&args, "s#3"));
    assert!(
        req1.prior.is_empty(),
        "nothing has been decided yet: {:?}",
        req1.prior
    );
    let _ = human.admit(&bash_at(&args, "s#3"));

    // A re-spelling of the same direction, three turns later.
    let req2 = human.request_for(&bash_at(&json!({"command": "/bin/sed -n 2p /w"}), "s#6"));
    assert_eq!(req2.prior.len(), 1, "{:?}", req2.prior);
    let p = &req2.prior[0];
    assert_eq!(p.effect, "admit");
    assert_eq!(p.count, 1);
    assert_eq!(p.latest_turns_ago, Some(3));
    assert_eq!(p.first_turns_ago, Some(3));

    // And the gate's OWN decisions stay out: a closed gate refuses every call,
    // and none of those refusals is shown back to it as history.
    let mut closed = AdjudicatedGate::closed().with_surroundings(pinned());
    let _ = closed.admit(&bash_at(&args, "s#3"));
    let after = closed.request_for(&bash_at(&args, "s#6"));
    assert!(
        after.prior.is_empty(),
        "the gate was shown its own answer: {:?}",
        after.prior
    );

    // A different direction does not inherit the answer: regions differ, and
    // a history for one shape is not a history for the next.
    let other = human.request_for(&bash_at(&json!({"command": "/bin/ls /elsewhere"}), "s#6"));
    assert!(
        other.prior.is_empty(),
        "a different region set is a different direction: {:?}",
        other.prior
    );
}

/// *"A denial is history too."* Both effects appear, denials beside approvals,
/// each with its own count and age — and the ages differ, because *"once,
/// three weeks ago"* and *"nine times this week"* are different facts.
#[test]
fn a_denial_is_history_too_and_sits_beside_the_approvals() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let n = AtomicUsize::new(0);
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        let i = n.fetch_add(1, Ordering::Relaxed);
        if i == 0 {
            // First: refused.
            Some(AdjudicationDecision::selected(
                req,
                "deny_and_tell",
                "human:test",
                "not today",
            ))
        } else {
            // Then: allowed.
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        }
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_surroundings(pinned());
    let args = json!({"command": "/bin/sed -n 1p /w"});
    let _ = g.admit(&bash_at(&args, "s#2"));
    let _ = g.admit(&bash_at(&args, "s#4"));

    let req3 = g.request_for(&bash_at(&args, "s#9"));
    let refuse = req3
        .prior
        .iter()
        .find(|p| p.effect == "refuse")
        .expect("the denial is history too");
    assert_eq!(refuse.count, 1);
    assert_eq!(refuse.latest_turns_ago, Some(7));
    let admit = req3
        .prior
        .iter()
        .find(|p| p.effect == "admit")
        .expect("so is the approval");
    assert_eq!(admit.count, 1);
    assert_eq!(admit.latest_turns_ago, Some(5));
}

/// *"History is evidence, never precedent"* — and it never lifts a tier. Two
/// approvals of the same shape do not turn an always-ask action into one the
/// gate admits unasked: the tier is layer A's, the option list still offers
/// only what the gate would honour, and a third ask still has to be answered
/// by somebody.
#[test]
fn a_history_of_approvals_cannot_move_an_always_ask_action() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let n = AtomicUsize::new(0);
    let seen: std::sync::Arc<Mutex<Vec<(String, Vec<String>)>>> =
        std::sync::Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        let i = n.fetch_add(1, Ordering::Relaxed);
        s.lock().unwrap().push((
            req.tier.as_str().to_string(),
            req.options.iter().map(|o| o.id.to_string()).collect(),
        ));
        if i < 2 {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "allowed once, by a human",
            ))
        } else {
            None
        }
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_surroundings(pinned());
    let args = json!({"command": "sudo systemctl restart foo"});
    // Two approvals in the direction — `sudo`, the privilege-escalation ask.
    assert_eq!(g.admit(&bash_at(&args, "s#1")), GateDecision::Admit);
    assert_eq!(g.admit(&bash_at(&args, "s#2")), GateDecision::Admit);

    // The third call: the history says admitted twice, and the gate asks
    // anyway — and with nobody to answer, nothing ran.
    match g.admit(&bash_at(&args, "s#3")) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { .. },
            ..
        } => {}
        other => panic!("history must not admit an always-ask action: {other:?}"),
    }
    let tiers = seen.lock().unwrap();
    for (tier, options) in tiers.iter() {
        assert_eq!(tier, "always_ask", "the tier never moved: {tier}");
        assert!(
            !options.iter().any(|o| o == "allow_session"),
            "an always-ask action is never offered a standing grant: {options:?}"
        );
    }
}
