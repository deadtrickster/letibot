//! `adjudicate_`: the floor a reviewed layer sets at the gate. It can raise a tier and nothing else
//! - never admit, never lower - and a layer that cannot answer is read as the strictest tier.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use letibot_plugin::adjudicate::{Action, Floor as Rung};

use super::*;

/// The layer a test installs: one fixed answer, and every action it was shown.
struct Layer {
    answer: Floored,
    seen: Mutex<Vec<Action>>,
}

impl Layer {
    fn new(answer: Floored) -> Arc<Layer> {
        Arc::new(Layer {
            answer,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn raised(rung: Rung, names: &[&str]) -> Arc<Layer> {
        Layer::new(Floored::Raised {
            intents: names.iter().map(|n| n.to_string()).collect(),
            floor: rung,
            by: "prod-floor".into(),
        })
    }

    fn seen(&self) -> Vec<Action> {
        self.seen.lock().expect("the record").clone()
    }
}

impl FloorHook for Layer {
    fn floor(&self, action: &Action) -> Floored {
        self.seen.lock().expect("the record").push(action.clone());
        self.answer.clone()
    }
}

/// A gate over one floor layer, at a point in mode-space, with an adjudicator that answers `allow`
/// to whatever rung it is offered. The count is how many times that adjudicator was reached: a card
/// nobody raised is the difference between a call the gate settled and one it put to a person.
fn gate(
    layer: Arc<Layer>,
    mode: crate::mode::Mode,
    allow: bool,
) -> (AdjudicatedGate, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let count = asked.clone();
    let adj = AskAdjudicator::new("fixed", move |req: &AdjudicationRequest| {
        count.fetch_add(1, Ordering::Relaxed);
        req.options
            .iter()
            .find(|o| o.kind.is_allow() == allow)
            .map(|o| AdjudicationDecision::selected(req, &o.id, "fixed", "because"))
    });
    let g = AdjudicatedGate::new(Box::new(adj))
        .with_mode(mode)
        .with_surroundings(pinned())
        .with_floor_hook(layer);
    (g, asked)
}

/// One write inside the session's own workspace: layer A reads it below `always-ask`, so a point
/// that admits writes settles it and a raised tier takes it back.
fn a_write<'a>(args: &'a Value) -> GateCall<'a> {
    GateCall {
        name: "write",
        access: Access::Write,
        args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/w",
        target_exists: Some(true),
        scripts: &[],
    }
}

// -----------------------------------------------------------------------------------------
// A floor may raise a tier and nothing else
// -----------------------------------------------------------------------------------------

/// A layer may not lower what layer A found: a call on the always-ask list stays on it whatever
/// the floor says, the rule on the tier is layer A's own, and the names the layer gave the action
/// reach the card without deciding anything.
#[test]
fn a_floor_layer_cannot_lower_a_tier() {
    for rung in [Rung::Auto, Rung::MayApprove] {
        let layer = Layer::raised(rung, &["prod-host"]);
        let (mut g, _) = gate(layer.clone(), crate::mode::Mode::ALWAYS_ASK, true);
        let args = json!({"command": "sudo -n true"});
        let req = g.request_for(&bash(&args));
        match &req.tier {
            Tier::AlwaysAsk { rule, .. } => assert_ne!(
                *rule, FLOOR_RULE,
                "{rung:?}: the floor lowered an entry on the always-ask list"
            ),
            other => panic!("{rung:?}: `sudo -n true` was read as {other:?}"),
        }
        assert_eq!(
            layer.seen().len(),
            1,
            "{rung:?}: the floor is asked once per reading"
        );
        assert!(
            req.boundary_facts
                .iter()
                .any(|f| f.contains("prod-host") && f.contains("prod-floor")),
            "{rung:?}: what the layer said is on the card: {:?}",
            req.boundary_facts
        );
        // And the decision is the gate's own: a call that asks still asks.
        let (mut g, asked) = gate(
            Layer::raised(rung, &[]),
            crate::mode::Mode::ALWAYS_ASK,
            false,
        );
        assert!(
            matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }),
            "{rung:?}: the floor let a refused call run"
        );
        assert_eq!(
            asked.load(Ordering::Relaxed),
            1,
            "{rung:?}: a person was asked"
        );
    }
}

/// The converse, so the rule above means something: a floor that raises takes a call the point
/// would have admitted unasked and puts it to whoever decides.
#[test]
fn a_floor_layer_that_raises_is_honoured() {
    let args = json!({"path": "/w/notes.md"});
    // Today: the `writes allowed` point admits a write inside the workspace and asks nobody.
    let (mut plain, asked) = gate(
        // A layer that answers nothing is the same as none at all - see the identity test below.
        Layer::new(Floored::Pass),
        crate::mode::Mode::WRITES_ALLOWED,
        false,
    );
    assert_eq!(plain.admit(&a_write(&args)), GateDecision::Admit);
    assert_eq!(asked.load(Ordering::Relaxed), 0, "the point settled it");

    // And with a floor at `ask`, the same call is a question - one the adjudicator answered no.
    let layer = Layer::raised(Rung::Ask, &["prod-host"]);
    let (mut floored, asked) = gate(layer, crate::mode::Mode::WRITES_ALLOWED, false);
    assert!(
        matches!(floored.admit(&a_write(&args)), GateDecision::Refuse { .. }),
        "the raised call reached the adjudicator, which refused it"
    );
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "a point that admits a write does not admit one the floor raised"
    );
    let row = floored.log.last().expect("a row");
    assert!(
        matches!(row.request.tier, Tier::AlwaysAsk { rule, .. } if rule == FLOOR_RULE),
        "{:?}",
        row.request.tier
    );
}

/// No rung of a floor admits anything: the only call that runs is one the gate's own ladder ran.
/// `auto` and `may_approve` leave the decision to the gate, `ask` puts it to a person, and `deny`
/// stops it before anybody is asked.
#[test]
fn a_floor_layer_cannot_admit() {
    let args = json!({"path": "/w/notes.md"});
    for (rung, runs, asks) in [
        (Rung::Auto, true, 1),
        (Rung::MayApprove, true, 1),
        (Rung::Ask, true, 1),
        (Rung::Deny, false, 0),
    ] {
        let (mut g, asked) = gate(
            Layer::raised(rung, &["prod-host"]),
            crate::mode::Mode::ALWAYS_ASK,
            true,
        );
        let decision = g.admit(&a_write(&args));
        assert_eq!(
            matches!(decision, GateDecision::Admit),
            runs,
            "{rung:?}: {decision:?}"
        );
        assert_eq!(
            asked.load(Ordering::Relaxed),
            asks,
            "{rung:?}: who was asked"
        );
        // The layer's own names decide nothing on their own, at any rung.
        let row = g.log.last().expect("a row");
        assert!(
            row.request
                .boundary_facts
                .iter()
                .any(|f| f.contains("prod-host")),
            "{rung:?}: {:?}",
            row.request.boundary_facts
        );
    }
}

// -----------------------------------------------------------------------------------------
// A floor that cannot answer refuses what it guards
// -----------------------------------------------------------------------------------------

/// A trap, a timeout or an answer that is not the contract is the strictest tier, not a skip: the
/// call does not run, and it does not run even at a point that admits everything.
#[test]
fn a_trapping_floor_layer_refuses_the_call() {
    for mode in [
        crate::mode::Mode::WRITES_ALLOWED,
        crate::mode::Mode::ALLOW_ALL,
    ] {
        let layer = Layer::new(Floored::Trapped {
            by: "prod-floor".into(),
            why: "its fuel ran out".into(),
        });
        let (mut g, asked) = gate(layer, mode, true);
        let args = json!({"path": "/w/notes.md"});
        match g.admit(&a_write(&args)) {
            GateDecision::Refuse {
                outcome: ToolOutcome::Denied { .. },
                tell,
            } => {
                assert!(tell.contains("prod-floor"), "{}: {tell}", mode.name);
                assert!(tell.contains("fuel ran out"), "{}: {tell}", mode.name);
            }
            other => panic!(
                "{}: a trapped floor let the call through: {other:?}",
                mode.name
            ),
        }
        assert_eq!(
            asked.load(Ordering::Relaxed),
            0,
            "{}: nothing was offered to anybody",
            mode.name
        );
        assert_eq!(g.log.len(), 1, "{}: the refusal is a row", mode.name);
        assert!(
            matches!(g.log[0].request.tier, Tier::Blocked { .. }),
            "{}: the row says why the call is at the strictest tier",
            mode.name
        );
    }
}

// -----------------------------------------------------------------------------------------
// The door where no person may be asked
// -----------------------------------------------------------------------------------------

/// The gate's other door: `admit_unasked`, the path a look or a plugin's tool takes where a card
/// would park whoever is waiting. A read-only tool never reaches this gate at all - the runtime
/// consults it only when the access is not unattended - so this is the unattended door it can.
#[test]
fn the_unattended_path_still_runs_the_floor() {
    let args = json!({"command": "git status"});
    // The ladder alone admits a read-only shell at every point, so this call is the control.
    let (mut plain, _) = gate(
        Layer::new(Floored::Pass),
        crate::mode::Mode::ALWAYS_ASK,
        true,
    );
    assert_eq!(plain.admit_unasked(&bash(&args)), GateDecision::Admit);

    let layer = Layer::raised(Rung::Ask, &["prod-host"]);
    let (mut g, asked) = gate(layer.clone(), crate::mode::Mode::ALWAYS_ASK, true);
    match g.admit_unasked(&bash(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => assert!(why.starts_with("would need your approval:"), "{why}"),
        other => panic!("a raised unasked call is not run: {other:?}"),
    }
    assert_eq!(layer.seen().len(), 1, "the floor was asked on this door");
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "and nobody was asked about it, which is what makes this door unattended"
    );

    // A floor that denies stops it here too, and says so as a decision rather than a question.
    let (mut g, _) = gate(
        Layer::raised(Rung::Deny, &[]),
        crate::mode::Mode::ALWAYS_ASK,
        true,
    );
    assert!(matches!(
        g.admit_unasked(&bash(&args)),
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { .. },
            ..
        }
    ));
}

// -----------------------------------------------------------------------------------------
// With no layer, nothing changed
// -----------------------------------------------------------------------------------------

/// A session with no floor layer decides exactly as it did before this seam existed: the reading is
/// layer A's own object, and the request, the decision and the audit row equal a gate whose layer
/// answered "nothing to say". Those two facts are what make "no layer, no change" a property.
#[test]
fn with_no_layer_the_gate_decides_as_it_did() {
    let args = json!({"command": "sudo -n true"});
    let build = || {
        AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "fixed",
            |req: &AdjudicationRequest| {
                req.options
                    .iter()
                    .find(|o| o.kind.is_allow())
                    .map(|o| AdjudicationDecision::selected(req, &o.id, "fixed", "because"))
            },
        )))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_surroundings(pinned())
    };
    let mut bare = build();
    // The seam is a field: with no hook on it, `reading_for` is `baseline_for` and nothing else.
    assert_eq!(
        bare.reading_for(&bash(&args)),
        (bare.baseline_for(&bash(&args)), None)
    );

    let mut quiet = build().with_floor_hook(Layer::new(Floored::Pass));
    assert_eq!(
        bare.request_for(&bash(&args)),
        quiet.request_for(&bash(&args)),
        "a layer that says nothing leaves the card as it was"
    );
    let left = bare.admit(&bash(&args));
    let right = quiet.admit(&bash(&args));
    assert_eq!(left, right, "and the decision");
    assert_eq!(bare.log.len(), quiet.log.len());
    assert_eq!(
        bare.log.last().expect("a row").request,
        quiet.log.last().expect("a row").request,
        "and the row an audit reads"
    );
}

/// The action a floor is shown is layer A's own reading, in the words a guest holds: a layer is
/// asked about what the call is, not about a second opinion on how it was spelled.
#[test]
fn the_floor_is_shown_the_normalised_action() {
    let layer = Layer::new(Floored::Pass);
    let (mut g, _) = gate(layer.clone(), crate::mode::Mode::WRITES_ALLOWED, true);
    let args = json!({"command": "ssh prod-1 uptime"});
    let _ = g.request_for(&bash(&args));
    let seen = layer.seen();
    assert_eq!(seen.len(), 1);
    let action = &seen[0];
    assert_eq!(action.tool, "bash");
    assert_eq!(action.mode, "writes allowed");
    assert_eq!(action.project, "/w");
    assert_eq!(action.program.as_deref(), Some("ssh"));
    assert!(action.intents.iter().any(|i| i == "network"), "{action:?}");
    assert_eq!(action.regions, vec!["workspace".to_string()], "{action:?}");
    assert_eq!(action.tier, g.baseline_for(&bash(&args)).tier.as_str());

    // And a tool's path argument reaches the layer as the same string layer A classified.
    let layer = Layer::new(Floored::Pass);
    let (mut g, _) = gate(layer.clone(), crate::mode::Mode::WRITES_ALLOWED, true);
    let _ = g.request_for(&a_write(&json!({"path": "/w/notes.md"})));
    let seen = layer.seen();
    assert_eq!(seen[0].tool, "write");
    assert_eq!(seen[0].paths, vec!["/w/notes.md".to_string()]);
}
