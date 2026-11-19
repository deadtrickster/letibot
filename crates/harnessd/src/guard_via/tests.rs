//! `[gatekeeper] via`: the endpoint first, the shadow when the endpoint does not answer.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use letibot_tools::adjudicate::{
    ActionClass, AdjudicationDecision, AdjudicationRequest, Adjudicator, DecisionOutcome, Offer,
    OnTimeout, RequestKind, permission_options,
};
use letibot_tools::authorise::{
    AuthorisationTrail, BriefVariant, OracleAnswer, OracleScope, ScriptedOracle, UnsureKind,
};
use letibot_tools::intent::{Baseline, Intent, ShellTrust, Surroundings};

use super::{EndpointThenShadow, Via};

fn surroundings() -> Surroundings {
    Surroundings {
        home: Some("/home/op".into()),
        workspace: Some("/w".into()),
        shell: ShellTrust::Pinned { how: "test".into() },
        ..Default::default()
    }
}

fn request() -> AdjudicationRequest {
    request_for("/usr/bin/cat /etc/hostname")
}

/// A bash request for `command`, for a test that drives an adjudicator directly.
pub(crate) fn request_for(command: &str) -> AdjudicationRequest {
    let b = Baseline::of_command(command, &surroundings());
    AdjudicationRequest {
        scripts: Vec::new(),
        preview: Vec::new(),
        id: "adj-t-0001".into(),
        session_id: "s".into(),
        turn_id: "t1".into(),
        call_id: "c1".into(),
        agent: "a".into(),
        tool: "bash".into(),
        class: ActionClass::host(letibot_tools::Access::Exec, true, false),
        summary: format!("`bash` wants exec access to `{command}`"),
        target: command.into(),
        arguments: serde_json::json!({ "command": command }),
        arguments_digest: "d".into(),
        boundary_facts: Vec::new(),
        kind: RequestKind::Permission,
        options: permission_options(&Offer {
            derived: None,
            program: "bash".into(),
            session: false,
            note: None,
        }),
        options_note: None,
        on_timeout: OnTimeout::Deny,
        resolved: b.resolved,
        tier: b.tier.clone(),
        baseline: b.summary(),
        trail: AuthorisationTrail::from_messages(
            vec![letibot_tools::authorise::Utterance::operator(
                "look at the hostname",
                0,
            )],
            2,
        ),
        prior: Vec::new(),
        examples: Vec::new(),
        brief_variant: BriefVariant::AskedForIt,
        agent_claim: None,
        reading: Some(b),
        shape: None,
        advice: None,
    }
}

/// A scope wide enough that these tests measure the router rather than the scope.
fn wide() -> OracleScope {
    let mut s = OracleScope::narrowest("test fixture, not a calibration");
    for i in [Intent::Network, Intent::ProcessControl, Intent::Unknown] {
        s.intents.insert(i);
    }
    s.max_scope = letibot_tools::adjudicate::EffectScope::External;
    s.declared = true;
    s
}

/// An endpoint over a scripted oracle that answers `answer` and counts its calls.
fn oracle(
    answer: impl Fn(&mut letibot_tools::authorise::ModelBrief) -> OracleAnswer + Send + Sync + 'static,
) -> (Arc<dyn Adjudicator>, Arc<AtomicU32>) {
    let asked = Arc::new(AtomicU32::new(0));
    let n = asked.clone();
    let adj = letibot_tools::ModelAdjudicator::new(
        Box::new(
            ScriptedOracle::new(
                "guard",
                move |b: &mut letibot_tools::authorise::ModelBrief| {
                    n.fetch_add(1, Ordering::SeqCst);
                    answer(b)
                },
            )
            .with_scope(wide()),
        ),
        |_: &AdjudicationRequest| Baseline::of_paths([], false, false, &Surroundings::default()),
    );
    (Arc::new(adj), asked)
}

/// An oracle that always answers the same thing, and counts.
fn counting(answer: OracleAnswer) -> (Arc<dyn Adjudicator>, Arc<AtomicU32>) {
    oracle(move |_| answer.clone())
}

/// The values the file may spell, and the default when it says nothing.
#[test]
fn the_spellings_and_what_the_configuration_implies() {
    for v in Via::ALL {
        assert_eq!(Via::parse(v.as_str()), Some(v));
    }
    assert_eq!(Via::parse("\"both\""), Some(Via::Both));
    assert_eq!(Via::parse("sideways"), None);
    // Both configured is the outage case, so it is the default; one alone is itself.
    assert_eq!(Via::default_for(true, true), Via::Both);
    assert_eq!(Via::default_for(true, false), Via::Endpoint);
    assert_eq!(Via::default_for(false, true), Via::Shadow);
    assert_eq!(Via::default_for(false, false), Via::Endpoint);
}

/// An endpoint that does not answer at all - a refused connection, a timeout - hands the call to
/// the shadow, which is the box whose GPU is down.
#[test]
fn an_endpoint_that_does_not_answer_hands_the_call_to_the_shadow() {
    let (first, _) = counting(OracleAnswer::Unsure {
        why: "guard did not answer".into(),
        kind: UnsureKind::Unreadable,
    });
    let (second, asked) = counting(OracleAnswer::Unsure {
        why: "the shadow says ask".into(),
        kind: UnsureKind::CouldNotDecide,
    });
    let both = EndpointThenShadow::new(first, second);
    let d = both.decide(&request());
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the shadow answered");
    // And the card carries the shadow's reason, not the endpoint's silence.
    assert!(d.basis.contains("the shadow says ask"), "{}", d.basis);
    assert!(
        both.last_advice().is_some_and(|a| a.consulted),
        "the corpus row is the arm that answered"
    );
}

/// An endpoint that answered - even with `ask` - is the answer: the shadow is not consulted over it.
#[test]
fn an_endpoint_that_answers_is_the_answer() {
    let (first, first_asked) = counting(OracleAnswer::Unsure {
        why: "nothing in the trail asks for this".into(),
        kind: UnsureKind::CouldNotDecide,
    });
    let (second, second_asked) = counting(OracleAnswer::Unsure {
        why: "the shadow".into(),
        kind: UnsureKind::CouldNotDecide,
    });
    let both = EndpointThenShadow::new(first, second);
    let d = both.decide(&request());
    assert_eq!(first_asked.load(Ordering::SeqCst), 1);
    assert_eq!(
        second_asked.load(Ordering::SeqCst),
        0,
        "the shadow was not asked"
    );
    assert!(d.basis.contains("nothing in the trail"), "{}", d.basis);
}

/// A decision the endpoint reached without consulting anybody - a blocked action, an uncollected
/// trail, a call outside the endpoint's scope - is not silence, and does not fall through.
#[test]
fn a_decision_reached_without_asking_the_endpoint_does_not_fall_through() {
    let (first, first_asked) = counting(OracleAnswer::Unsure {
        why: "unused".into(),
        kind: UnsureKind::Unreadable,
    });
    let (second, second_asked) = counting(OracleAnswer::Unsure {
        why: "the shadow".into(),
        kind: UnsureKind::CouldNotDecide,
    });
    let both = EndpointThenShadow::new(first, second);
    let mut r = request();
    r.trail = AuthorisationTrail::not_collected("no trail here");
    both.decide(&r);
    assert_eq!(
        first_asked.load(Ordering::SeqCst),
        0,
        "the endpoint was not asked"
    );
    assert_eq!(
        second_asked.load(Ordering::SeqCst),
        0,
        "the shadow was not asked"
    );
}

/// An allow from either arm travels unchanged: the composite routes, it does not judge.
#[test]
fn an_answer_is_passed_through_unchanged() {
    let (first, _) = counting(OracleAnswer::Unsure {
        why: "x".into(),
        kind: UnsureKind::Unreadable,
    });
    let shadow = Arc::new(Allowing);
    let both = EndpointThenShadow::new(first, shadow);
    let d = both.decide(&request());
    assert!(
        matches!(
            d.outcome,
            DecisionOutcome::Selected { ref option_id } if option_id == "allow_once"
        ),
        "{:?}",
        d.outcome
    );
    let said = both.describe();
    assert!(said.contains("when it does not answer"), "{said}");
}

/// A stand-in that allows without a model, for the pass-through.
struct Allowing;

impl Adjudicator for Allowing {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Selected {
                option_id: "allow_once".into(),
            },
            by: "shadow:test".into(),
            basis: "allowed by the shadow (test, exec)".into(),
            latency_ms: 0,
            grant_at: None,
        }
    }
    fn describe(&self) -> String {
        "a test stand-in".into()
    }
}
