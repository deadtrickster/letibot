//! Two rulings, one question: may a standing answer settle a command, or must somebody be asked
//! about this one? Inside a measured boundary a per-command ruling protects nothing the mode's own
//! writes could not do, so at `writes-allowed` a command there is admitted without a card; on a
//! host with no boundary, a command that runs the project's own code asks every time.
//!
//! The fixtures are boundaries built by hand, because a real `bwrap` cannot nest in this box's.

use super::*;

use crate::exec::{Namespace, NsState, Seal, SealKind};

// ---------------------------------------------------------------------------
// The boundary fixtures
// ---------------------------------------------------------------------------

/// A boundary that encloses: every namespace entered, both seals held rather than merely not open,
/// and the egress given. `measured_boundary` in `mod.rs` is the rest of it.
fn measured(egress: crate::exec::Egress) -> crate::exec::Boundary {
    crate::exec::Boundary {
        egress,
        ..measured_boundary()
    }
}

fn measured_and_closed() -> crate::exec::Boundary {
    measured(crate::exec::Egress::Denied)
}

/// An adjudicator that must not be reached: an assertion on the audit row alone cannot tell
/// asked-and-allowed from not-asked.
struct Nobody;

impl Adjudicator for Nobody {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        panic!(
            "nobody was to be asked about `{}`, and this adjudicator was",
            req.summary
        );
    }
    fn describe(&self) -> String {
        "nobody".into()
    }
}

/// An adjudicator that records being asked and allows the call once: "it asked" is the whole of
/// what a "still asks" test can prove.
#[derive(Default)]
struct Asked(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl Asked {
    fn count(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Adjudicator for Asked {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(req.id.clone());
        AdjudicationDecision::selected(req, "allow_once", "human:test", "because")
    }
    fn describe(&self) -> String {
        "human:test".into()
    }
}

/// An adjudicator that names an option this gate does not offer, the way an older head or a
/// replayed answer would.
struct Picks(&'static str);

impl Adjudicator for Picks {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        AdjudicationDecision::selected(req, self.0, "human:old-head", "answered by a head")
    }
    fn describe(&self) -> String {
        "human:old-head".into()
    }
}

/// The admissions a gate told somebody about, in order.
#[derive(Default)]
struct Told(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl AdmissionSink for Told {
    fn admitted(&self, notice: &AdmissionNotice) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("{}: {}", notice.mode, notice.summary));
    }
}

/// A gate at `writes-allowed` on a workspace of `/w`, with the boundary it is handed.
fn gate_at(
    adjudicator: Box<dyn Adjudicator>,
    mode: crate::mode::Mode,
    boundary: Option<crate::exec::Boundary>,
) -> AdjudicatedGate {
    AdjudicatedGate::new(adjudicator)
        .with_identity("s-test", "agent")
        .with_mode(mode)
        .with_boundary(boundary, None)
        .with_surroundings(pinned())
}

fn last_basis(g: &AdjudicatedGate) -> String {
    g.log
        .last()
        .map(|r| r.decision.basis.clone())
        .unwrap_or_default()
}

fn last_by(g: &AdjudicatedGate) -> String {
    g.log
        .last()
        .map(|r| r.decision.by.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Admitted by the boundary, and what the row and the notice say
// ---------------------------------------------------------------------------

/// All three admitted with nobody asked, the audit row saying why, and a line told to the
/// operator.
#[test]
fn inside_a_measured_boundary_a_command_is_admitted_without_a_card() {
    for (command, region) in [
        ("cargo test --offline -p letibot-tools", "workspace"),
        ("bash scripts/check-fmt.sh main", "workspace"),
        ("rm -rf target", "workspace"),
    ] {
        let told = Told::default();
        let mut g = gate_at(
            Box::new(Nobody),
            crate::mode::Mode::WRITES_ALLOWED,
            Some(measured_and_closed()),
        )
        .with_admission_sink(Box::new(Told(told.0.clone())));
        let args = json!({ "command": command });
        let call = bash(&args);
        assert_eq!(
            g.admit(&call),
            GateDecision::Admit,
            "`{command}` is inside the measured boundary and must not ask"
        );
        // The command arranged the way layer A reads it, so the fixture cannot pass as a string
        // the real classifier places differently.
        let reading = g.baseline_for(&call);
        assert!(
            reading.regions.iter().any(|r| r.as_str() == region),
            "`{command}` should place a region `{region}`: {:?}",
            reading
                .regions
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(g.log.len(), 1, "one call, one row");
        assert_eq!(g.log[0].effect, "admit");
        assert_eq!(last_by(&g), "gate:boundary");
        assert!(
            last_basis(&g).starts_with("admitted: inside the measured boundary"),
            "the row has to say why in the ruling's own words: {}",
            last_basis(&g)
        );
        assert!(
            last_basis(&g).contains("bubblewrap 0.11.1 (fixture)"),
            "the row names what measured the boundary: {}",
            last_basis(&g)
        );
        let said = told.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(
            said.len(),
            1,
            "the operator is told once, not only recorded"
        );
        assert!(
            said[0].starts_with("writes allowed: "),
            "the notice says which point admitted it: {}",
            said[0]
        );
    }
}

/// The rule is not once per session: what the boundary removes is the question, and a
/// first-call-only version would leave the operator answering every build after the first.
#[test]
fn the_admit_is_not_a_once_per_session_thing() {
    let mut g = gate_at(
        Box::new(Nobody),
        crate::mode::Mode::WRITES_ALLOWED,
        Some(measured_and_closed()),
    );
    for _ in 0..3 {
        assert_eq!(
            g.admit(&bash(&json!({ "command": "cargo build --offline" }))),
            GateDecision::Admit
        );
    }
    assert_eq!(g.log.len(), 3);
    assert!(g.log.iter().all(|r| r.decision.by == "gate:boundary"));
}

// ---------------------------------------------------------------------------
// What still asks inside a measured boundary
// ---------------------------------------------------------------------------

/// Everything that crosses the boundary still asks: egress, publication and the credential tier
/// each get a card even with a measured boundary in force.
#[test]
fn a_command_that_crosses_the_boundary_still_asks() {
    for command in [
        "curl -s https://example.invalid/x",
        "cargo fetch",
        "git push origin main",
        "gh pr create --fill",
    ] {
        let asked = Asked::default();
        let mut g = gate_at(
            Box::new(Asked(asked.0.clone())),
            crate::mode::Mode::WRITES_ALLOWED,
            Some(measured_and_closed()),
        );
        let _ = g.admit(&bash(&json!({ "command": command })));
        assert_eq!(
            asked.count(),
            1,
            "`{command}` must still be put to somebody"
        );
        assert_ne!(
            last_by(&g),
            "gate:boundary",
            "`{command}` was settled by the boundary rule:\n{}",
            last_basis(&g)
        );
    }
}

/// A command that would work outside the checkout still asks: a ruling about it would be a ruling
/// about a path the boundary does not contain.
#[test]
fn a_write_outside_the_checkout_still_asks() {
    for command in [
        "echo hi > /etc/motd",
        "rm -rf /home/dead/Projects/other",
        "cp target/x ~/keep",
    ] {
        let asked = Asked::default();
        let mut g = gate_at(
            Box::new(Asked(asked.0.clone())),
            crate::mode::Mode::WRITES_ALLOWED,
            Some(measured_and_closed()),
        );
        let _ = g.admit(&bash(&json!({ "command": command })));
        assert_eq!(
            asked.count(),
            1,
            "`{command}` names a path outside the view"
        );
        assert_ne!(last_by(&g), "gate:boundary", "{command}");
    }
}

/// No boundary means nothing is admitted by this rule: the same commands that pass above are
/// asked here.
#[test]
fn without_a_measured_boundary_the_same_commands_still_ask() {
    for command in [
        "cargo test --offline -p letibot-tools",
        "rm -rf target",
        "make",
    ] {
        let asked = Asked::default();
        let mut g = gate_at(
            Box::new(Asked(asked.0.clone())),
            crate::mode::Mode::WRITES_ALLOWED,
            None,
        );
        let _ = g.admit(&bash(&json!({ "command": command })));
        assert_eq!(
            asked.count(),
            1,
            "`{command}` on an unconfined host must ask"
        );
    }
}

/// A boundary that does not hold is not a boundary. Three ways for `encloses()` to be false: a
/// required namespace missing, a seal open, and a seal merely unverified. `complete()` tolerates
/// the last; this rule does not.
#[test]
fn a_boundary_that_does_not_hold_still_asks() {
    let cases: Vec<(&str, crate::exec::Boundary)> = {
        let mut missing = measured_and_closed();
        missing.ns.insert(
            Namespace::Mount,
            NsState::NotEntered {
                why: "the fixture says the mount namespace did not take".into(),
            },
        );

        let mut open = measured_and_closed();
        open.seals.insert(
            SealKind::NestedNamespaces,
            Seal::Open {
                why: "a child unshared its way back out".into(),
            },
        );

        let mut unverified = measured_and_closed();
        unverified.seals.insert(
            SealKind::NoNewPrivs,
            Seal::Unverified {
                why: "could not be tested from inside".into(),
            },
        );
        vec![
            ("a required namespace missing", missing),
            ("a seal measured open", open),
            ("a seal that could not be tested", unverified),
        ]
    };
    for (what, boundary) in cases {
        let asked = Asked::default();
        let mut g = gate_at(
            Box::new(Asked(asked.0.clone())),
            crate::mode::Mode::WRITES_ALLOWED,
            Some(boundary),
        );
        let _ = g.admit(&bash(&json!({ "command": "cargo test" })));
        assert_eq!(asked.count(), 1, "{what} must not admit a command");
    }
}

/// A boundary that shares the host's network is not enclosed either: what crosses it is what a
/// card is still for.
#[test]
fn a_boundary_with_egress_to_the_host_still_asks() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        Some(measured(crate::exec::Egress::Host {
            why: "the session seats web_fetch".into(),
        })),
    );
    let _ = g.admit(&bash(&json!({ "command": "cargo test" })));
    assert_eq!(asked.count(), 1);
}

/// The admit is `writes-allowed` only, and both directions are asserted because either alone is a
/// test a wrong implementation passes: `always-ask` still asks, and `allow-all` admits through its
/// own `gate:mode` rule, so a reader can tell the two apart.
#[test]
fn the_rule_is_writes_allowed_only() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::ALWAYS_ASK,
        Some(measured_and_closed()),
    );
    let _ = g.admit(&bash(&json!({ "command": "cargo test" })));
    assert_eq!(asked.count(), 1, "always-ask asks about everything");

    let mut g = gate_at(
        Box::new(Nobody),
        crate::mode::Mode::ALLOW_ALL,
        Some(measured_and_closed()),
    )
    // A daemon turns this on for an unconfined seat, and `allow-all`'s promise is that exec
    // follows the point; without it the call falls through to the adjudicator.
    .with_exec_follows_mode(true);
    assert_eq!(
        g.admit(&bash(&json!({ "command": "cargo test" }))),
        GateDecision::Admit
    );
    assert_eq!(last_by(&g), "gate:mode", "allow-all admits by its own rule");
}

/// `encloses()` is necessary and not sufficient: an unreadable action is refused before it, at
/// every point that does not admit everything.
#[test]
fn an_unreadable_command_is_not_admitted_by_the_boundary() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        Some(measured_and_closed()),
    );
    // An unclosed quote: nothing decided about an unreadable command can be about a boundary.
    let decision = g.admit(&bash(&json!({ "command": "cargo test 'unclosed" })));
    assert!(
        !matches!(decision, GateDecision::Admit),
        "an unreadable command was admitted: {decision:?}"
    );
    assert_eq!(
        g.log.last().map(|r| r.decision.by.as_str()),
        Some("boundary:normaliser")
    );
    assert_eq!(
        asked.count(),
        0,
        "no adjudicator decides about an unread action"
    );
}

// ---------------------------------------------------------------------------
// On a host with no boundary, the project's own code asks every time
// ---------------------------------------------------------------------------

/// The shape cache is the durable half of a standing answer, and the file a runner executes can
/// be rewritten between the answer and the next run, so the shape is not a licence. The contrast
/// keeps this from passing on a gate that had stopped honouring its cache.
#[test]
fn a_durable_shape_does_not_settle_a_test_runner_without_a_boundary() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    );
    // Exactly what a human's earlier approval of this shape would have seeded.
    let args = json!({ "command": "cargo test --offline" });
    let seen = g.request_for(&bash(&args));
    let shape = seen.shape.clone().expect("a parsed command has a shape");
    assert_eq!(
        g.seed_shapes([(shape, seen.class.to_string())]),
        1,
        "the fixture must actually seed the shape, or this proves nothing"
    );
    let call = bash(&args);
    assert_eq!(g.admit(&call), GateDecision::Admit);
    assert_eq!(
        asked.count(),
        1,
        "an approval of this shape does not answer the next run"
    );

    // And the cache is not simply broken: `cargo fmt` is the same program at the same tier and
    // runs nothing this project wrote, so its shape still settles.
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    );
    let args = json!({ "command": "cargo fmt --all" });
    let seen = g.request_for(&bash(&args));
    g.seed_shapes([(seen.shape.clone().expect("a shape"), seen.class.to_string())]);
    let call = bash(&args);
    assert_eq!(g.admit(&call), GateDecision::Admit);
    assert_eq!(
        asked.count(),
        0,
        "`cargo fmt` is still settled by its shape"
    );
    assert_eq!(last_by(&g), "gate:shape");
}

/// The other side of the same sentence, so the rule above cannot be a rule about `cargo test` in
/// general.
#[test]
fn the_same_runner_is_admitted_inside_the_boundary() {
    let mut g = gate_at(
        Box::new(Nobody),
        crate::mode::Mode::WRITES_ALLOWED,
        Some(measured_and_closed()),
    );
    assert_eq!(
        g.admit(&bash(&json!({ "command": "cargo test --offline" }))),
        GateDecision::Admit
    );
    assert_eq!(last_by(&g), "gate:boundary");
}

/// A standing rung over a call the gate would refuse to remember is a button whose effect it
/// declines, so the ladder narrows to this one call and the note carries the reason.
#[test]
fn the_card_narrows_the_ladder_and_says_why() {
    let mut g = gate_at(
        Box::new(Asked::default()),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    );
    let request = g.request_for(&bash(&json!({ "command": "cargo test --offline" })));
    let ids: Vec<&str> = request.options.iter().map(|o| o.id.as_str()).collect();
    assert!(
        !ids.contains(&"allow_session") && !ids.contains(&"allow_always"),
        "a standing answer cannot settle this call, so it is not offered: {ids:?}"
    );
    assert!(
        ids.contains(&"allow_once"),
        "the call itself is still decidable"
    );
    let note = request.options_note.clone().unwrap_or_default();
    assert!(
        note.contains("asks every time") && note.contains("`cargo test`"),
        "the card has to say what and why: {note}"
    );
    assert!(
        note.contains("no boundary"),
        "and the fact that makes it a question is the host's, not the command's: {note}"
    );
    assert!(
        !note.contains("R98"),
        "a review number is not a name the reader can act on: {note}"
    );

    // A command that is not the project's code keeps its full ladder, or this would pass on a
    // gate that had narrowed every ladder on every host.
    let plain = g.request_for(&bash(&json!({ "command": "git status"})));
    let ids: Vec<&str> = plain.options.iter().map(|o| o.id.as_str()).collect();
    assert!(
        ids.contains(&"allow_session"),
        "`git status` may still be granted for the session: {ids:?}"
    );
    assert_eq!(plain.options_note, None, "and nothing is left off it");
}

/// The durable half of "session or durable grant": the rule is left in the config, it simply
/// does not answer this call.
#[test]
fn a_durable_allow_rule_does_not_settle_a_runner_without_a_boundary() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    )
    .with_permission(vec![crate::permission::Rule::new(
        "bash",
        "cargo test*",
        crate::permission::Action::Allow,
    )]);
    let _ = g.admit(&bash(&json!({ "command": "cargo test --offline" })));
    assert_eq!(
        asked.count(),
        1,
        "a rule written about the last run does not answer this one"
    );

    // It still governs what it can be trusted about: the same rule over `git status`, which runs
    // nothing this project wrote, is honoured without asking.
    let mut g =
        gate_at(Box::new(Nobody), crate::mode::Mode::WRITES_ALLOWED, None).with_permission(vec![
            crate::permission::Rule::new("bash", "git status", crate::permission::Action::Allow),
        ]);
    assert_eq!(
        g.admit(&bash(&json!({ "command": "git status" }))),
        GateDecision::Admit
    );
    assert_eq!(last_by(&g), "gate:permission");
}

/// The guard that actually bites: the shape cache is warmed from any human admit of a call shape,
/// so without this the first `cargo test` the operator allowed would settle every later one.
#[test]
fn an_allow_once_answer_does_not_seed_a_shape_for_a_runner() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    );
    let args = json!({ "command": "cargo test --offline" });
    let call = bash(&args);
    assert_eq!(g.admit(&call), GateDecision::Admit);
    assert_eq!(asked.count(), 1);
    let call = bash(&args);
    assert_eq!(g.admit(&call), GateDecision::Admit);
    assert_eq!(
        asked.count(),
        2,
        "the shape of the first answer must not settle the second run"
    );
    assert_eq!(last_by(&g), "human:test", "a person answered again");
}

// ---------------------------------------------------------------------------
// The fact, and where it comes from
// ---------------------------------------------------------------------------

/// The gate is handed the one platform fact there is - a boundary it can be shown, or none - and
/// nothing else switches the rule on. `None` is what `confine::probe` answers on a Mac host shell,
/// so a Linux test stands in for a Mac by handing the gate exactly that.
#[test]
fn a_host_with_no_measured_boundary_asks_with_nothing_switched_on() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    );
    // A shape a previous approval would have seeded, and the ladder that would record one.
    let args = json!({ "command": "cargo test --offline" });
    let seen = g.request_for(&bash(&args));
    assert_eq!(
        g.seed_shapes([(seen.shape.clone().expect("a shape"), seen.class.to_string())]),
        1
    );
    let ids: Vec<&str> = seen.options.iter().map(|o| o.id.as_str()).collect();
    assert!(
        !ids.contains(&"allow_session") && !ids.contains(&"allow_always"),
        "a standing answer cannot settle this call: {ids:?}"
    );
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(
        asked.count(),
        1,
        "the only answer that runs a test on a host with no boundary is a person's"
    );
    assert_eq!(last_by(&g), "human:test");
    assert!(g.grants().is_empty(), "nothing was remembered for later");

    // The other side of the same injected fact: the same command, the same seeded shape, and a
    // boundary that was measured, where D16 admits it with nobody asked.
    let mut g = gate_at(
        Box::new(Nobody),
        crate::mode::Mode::WRITES_ALLOWED,
        Some(measured_and_closed()),
    );
    let seen = g.request_for(&bash(&args));
    g.seed_shapes([(seen.shape.clone().expect("a shape"), seen.class.to_string())]);
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(last_by(&g), "gate:boundary");
}

/// `sudo -n true` and `shutdown -h now` carry no network intent and name no path, so a rule argued
/// about egress and the view would admit both: the boundary's admit holds only where the tier is
/// one a point may move at all.
#[test]
fn the_always_ask_tier_still_asks_inside_the_boundary() {
    for command in ["sudo -n true", "shutdown -h now", "crontab /w/cron.txt"] {
        let asked = Asked::default();
        let mut g = gate_at(
            Box::new(Asked(asked.0.clone())),
            crate::mode::Mode::WRITES_ALLOWED,
            Some(measured_and_closed()),
        );
        let _ = g.admit(&bash(&json!({ "command": command })));
        assert_eq!(
            asked.count(),
            1,
            "`{command}` is on the always-ask list and must still be put to somebody"
        );
        assert_ne!(last_by(&g), "gate:boundary", "{command}");
    }
}

/// The ladder is the first mechanism and `admit`'s option lookup is the second, so an older head
/// or a replayed answer naming a rung anyway gets nothing run and no grant left on the books for
/// the gate to decline later.
#[test]
fn a_rung_the_ladder_did_not_offer_is_refused_for_a_runner() {
    for rung in ["allow_session", "allow_always"] {
        let mut g = gate_at(
            Box::new(Picks(rung)),
            crate::mode::Mode::WRITES_ALLOWED,
            None,
        );
        let decision = g.admit(&bash(&json!({ "command": "cargo test" })));
        assert!(
            !matches!(decision, GateDecision::Admit),
            "`{rung}` was honoured although the ladder did not offer it"
        );
        assert!(g.grants().is_empty(), "a grant was recorded for `{rung}`");
        assert!(g.permission().is_empty(), "a rule was written for `{rung}`");
    }

    // A command that is not the project's code still offers both rungs, on the side where they
    // are supposed to work.
    let mut g = gate_at(
        Box::new(Picks("allow_session")),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    );
    assert_eq!(
        g.admit(&bash(&json!({ "command": "cargo fmt --all" }))),
        GateDecision::Admit
    );
    assert_eq!(g.grants().len(), 1, "`cargo fmt` may still be granted");
}

/// The admit is the boundary's, and `admit_unasked`'s one caller (a condition layer's `exec`
/// look) is spawned by the daemon on its own host: the same command is a question there.
#[test]
fn an_unasked_call_is_not_admitted_by_the_boundary() {
    let told = Told::default();
    let mut g = gate_at(
        Box::new(Nobody),
        crate::mode::Mode::WRITES_ALLOWED,
        Some(measured_and_closed()),
    )
    .with_admission_sink(Box::new(Told(told.0.clone())));
    let args = json!({ "command": "cargo test --offline -p letibot-tools" });
    match g.admit_unasked(&bash(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => assert!(
            why.starts_with("would need your approval:"),
            "the look is refused as a question nobody was put: {why}"
        ),
        other => panic!("a look outside the boundary was admitted by it: {other:?}"),
    }
    assert!(
        told.0.lock().unwrap().is_empty(),
        "nothing was admitted, so nothing is told as admitted"
    );
    // The model's own call, which does run inside, is unchanged.
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
}
