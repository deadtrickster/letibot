//! The shadow as `automode`'s oracle on a session's own gate: stage-1 reads decided by the shadow
//! with a cite of the operator's words, everything else left to the person.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_tools::adjudicate::{AdjudicationDecision, AdjudicationRequest, Adjudicator};
use letibot_tools::authorise::{AuthorisationTrail, Utterance};
use letibot_tools::runtime::{Gate, GateDecision, ViewGrant};

use super::oracle::ShadowAdjudicator;
use super::permission::{Answering, Cell};
use super::runner::{Shadow, Timing};

struct Judge(&'static str);

impl super::judge::Judge for Judge {
    fn label(&self) -> String {
        "deepseek".into()
    }
    fn judge(&self, _system: &str, _user: &str) -> Result<String, String> {
        Ok(self.0.into())
    }
}

#[derive(Default)]
struct Rows;

impl super::record::Recorder for Rows {
    fn row(&self, _title: &str, _body: &str) {}
}

/// The person: refuses, and counts.
#[derive(Clone, Default)]
struct Person(Arc<Mutex<u32>>);

impl Adjudicator for Person {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        *self.0.lock().unwrap() += 1;
        let id = if req.options.iter().any(|o| o.id == "deny") {
            "deny"
        } else {
            "reject_once"
        };
        AdjudicationDecision::selected(req, id, "human:op", "no")
    }
    fn describe(&self) -> String {
        "asked at the head: a test".into()
    }
}

fn shadow_cell(reply: &'static str) -> Cell {
    let lines = letibot_provider::keys::ShadowLines {
        enabled: vec!["permission".into()],
        answer_reads: Some(true),
        ..Default::default()
    };
    let cfg = super::config::ShadowConfig::from_lines(lines, "local".into())
        .unwrap()
        .with_mode(false, 0);
    let judge: Arc<dyn super::judge::Judge> = Arc::new(Judge(reply));
    let shadow = Shadow::start(
        &cfg,
        "glm/x",
        None,
        Arc::new(Rows),
        &|_, _| judge.clone(),
        &|_| Timing {
            deadline: Duration::from_millis(500),
            budget: Duration::from_secs(5),
            misses: 3,
            window: 5,
            backoff: Duration::from_secs(1),
            backoff_max: Duration::from_secs(2),
        },
        &|_| Box::new(|_, _| {}),
    )
    .unwrap();
    let cell: Cell = Default::default();
    *cell.lock().unwrap() = Some((
        Arc::new(shadow),
        Arc::new(Rows) as Arc<dyn super::record::Recorder>,
    ));
    cell
}

fn trail() -> AuthorisationTrail {
    AuthorisationTrail::from_messages(
        vec![Utterance::operator(
            "look at the build tree and the hostname",
            0,
        )],
        1,
    )
}

/// A session at `automode` whose oracle is the shadow, and the person behind it.
fn automode_gate(reply: &'static str) -> (letibot_tools::AdjudicatedGate, Person) {
    let person = Person::default();
    let advisor = ShadowAdjudicator::new(
        shadow_cell(reply),
        Answering::new(true),
        "/home/op".into(),
        "/w".into(),
        true,
    );
    let g = letibot_tools::AdjudicatedGate::new(Box::new(person.clone()))
        .with_identity("s-main", "agent")
        .with_mode(letibot_tools::mode::Mode::parse("automode").unwrap())
        .with_surroundings(letibot_tools::intent::Surroundings {
            home: Some("/home/op".into()),
            workspace: Some("/w".into()),
            shell: letibot_tools::intent::ShellTrust::Pinned { how: "test".into() },
            ..Default::default()
        })
        .with_trail_source(|_| trail())
        .with_advisor(Arc::new(advisor))
        .start_supervised(true);
    (g, person)
}

const ALLOW: &str = r#"{"verdict":"allow","cites":[0],"evidence":""}"#;

/// Stage 1b: a view read at `automode` is the shadow's to grant, read-only, with no card.
#[test]
fn at_automode_the_shadow_grants_a_view_read_without_a_card() {
    let (mut g, person) = automode_gate(ALLOW);
    assert_eq!(
        g.grant_view(
            std::path::Path::new("/build/letibot-r1"),
            "bash",
            letibot_tools::ViewAccess::Read
        ),
        ViewGrant::Granted { writable: false }
    );
    assert_eq!(g.view_answered_by().as_deref(), Some("shadow:deepseek"));
    assert_eq!(*person.0.lock().unwrap(), 0, "nobody was asked");

    // A write is not a read: the person decides.
    g.grant_view(
        std::path::Path::new("/build/letibot-r2"),
        "bash",
        letibot_tools::ViewAccess::Write,
    );
    assert_eq!(*person.0.lock().unwrap(), 1);
}

/// A may-approve shell read at `automode` is admitted on the shadow's cited allow; a write is not
/// stage 1 and goes to the person; an `ask` goes to the person too.
#[test]
fn at_automode_the_shadow_decides_a_read_and_nothing_else() {
    let read = serde_json::json!({ "command": "/usr/bin/cat /etc/hostname" });
    let write = serde_json::json!({ "command": "/usr/bin/touch /etc/hostname" });

    let (mut g, person) = automode_gate(ALLOW);
    assert!(
        matches!(g.admit(&call(&read)), GateDecision::Admit),
        "a cited read is admitted"
    );
    assert_eq!(*person.0.lock().unwrap(), 0);
    assert!(!matches!(g.admit(&call(&write)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 1, "a write is the person's");

    let (mut g, person) = automode_gate(r#"{"verdict":"ask","evidence":""}"#);
    assert!(!matches!(g.admit(&call(&read)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 1, "an ask is the person's");
}

fn call(args: &serde_json::Value) -> letibot_tools::GateCall<'_> {
    letibot_tools::GateCall {
        name: "bash",
        access: letibot_tools::Access::Exec,
        args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/w",
        target_exists: None,
        scripts: &[],
        preview: &[],
    }
}
