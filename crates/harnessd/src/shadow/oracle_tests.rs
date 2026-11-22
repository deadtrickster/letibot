//! The shadow as `automode`'s oracle on a session's own gate: the stages the operator promoted
//! decided by the shadow with a cite of the operator's words, everything else left to the person.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_tools::adjudicate::{AdjudicationDecision, AdjudicationRequest, Adjudicator};
use letibot_tools::authorise::{AuthorisationTrail, Utterance};
use letibot_tools::runtime::{Gate, GateDecision, ViewGrant};

use super::config::Stage;
use super::oracle::ShadowAdjudicator;
use super::permission::{Answering, Cell};
use super::runner::{Shadow, Timing};
use letibot_tools::view::GrantedRoots;

/// A judge that answers `reply`, counts the questions, and can be told to be slow.
struct Judge {
    reply: &'static str,
    asked: Arc<AtomicU32>,
    slow: Option<Duration>,
}

impl super::judge::Judge for Judge {
    fn label(&self) -> String {
        "deepseek".into()
    }
    fn judge(&self, _system: &str, _user: &str) -> Result<String, String> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if let Some(d) = self.slow {
            std::thread::sleep(d);
        }
        Ok(self.reply.into())
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

/// The shadow the oracle reaches: `stages` promoted, a judge answering `reply`.
fn shadow_cell(
    stages: &[Stage],
    reply: &'static str,
    slow: Option<Duration>,
) -> (Cell, Arc<AtomicU32>) {
    let mut lines = letibot_provider::keys::ShadowLines {
        enabled: vec!["permission".into()],
        ..Default::default()
    };
    lines.answers = Some(stages.iter().map(|s| s.as_str().to_string()).collect());
    let cfg = super::config::ShadowConfig::from_lines(lines, "local".into())
        .unwrap()
        .with_mode(false, 0);
    let asked = Arc::new(AtomicU32::new(0));
    let judge: Arc<dyn super::judge::Judge> = Arc::new(Judge {
        reply,
        asked: asked.clone(),
        slow,
    });
    let shadow = Shadow::start(
        &cfg,
        "glm/x",
        None,
        Arc::new(Rows),
        &|_, _| judge.clone(),
        &|_| Timing {
            deadline: Duration::from_millis(300),
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
    (cell, asked)
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
fn automode_gate(
    stages: &[Stage],
    reply: &'static str,
) -> (
    letibot_tools::AdjudicatedGate,
    Person,
    Arc<Answering>,
    Arc<AtomicU32>,
) {
    automode_gate_slow(stages, reply, None)
}

fn automode_gate_slow(
    stages: &[Stage],
    reply: &'static str,
    slow: Option<Duration>,
) -> (
    letibot_tools::AdjudicatedGate,
    Person,
    Arc<Answering>,
    Arc<AtomicU32>,
) {
    let person = Person::default();
    let (cell, asked) = shadow_cell(stages, reply, slow);
    let set: BTreeSet<Stage> = stages.iter().copied().collect();
    let answering = Answering::new(true);
    let advisor = ShadowAdjudicator::new(
        cell,
        answering.clone(),
        set,
        "/home/op".into(),
        "/w".into(),
        true,
        GrantedRoots::default(),
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
    (g, person, answering, asked)
}

const ALLOW: &str = r#"{"verdict":"allow","cites":[0],"evidence":""}"#;

/// The shadow's adjudicator alone, for a test that wants its decision rather than the gate's.
fn adjudicator(stages: &[Stage]) -> ShadowAdjudicator {
    let (cell, _) = shadow_cell(stages, ALLOW, None);
    ShadowAdjudicator::new(
        cell,
        Answering::new(true),
        stages.iter().copied().collect(),
        "/home/op".into(),
        "/w".into(),
        true,
        GrantedRoots::default(),
    )
}

fn all_stages() -> Vec<Stage> {
    Stage::ALL.to_vec()
}

/// Stage 1b: a view read at `automode` is the shadow's to grant, read-only, with no card.
#[test]
fn at_automode_the_shadow_grants_a_view_read_without_a_card() {
    let (mut g, person, ..) = automode_gate(&[Stage::Reads], ALLOW);
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

    // A write is not a read: at stage 1 the person decides.
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

    let (mut g, person, ..) = automode_gate(&[Stage::Reads], ALLOW);
    assert!(
        matches!(g.admit(&call(&read)), GateDecision::Admit),
        "a cited read is admitted"
    );
    assert_eq!(*person.0.lock().unwrap(), 0);
    assert!(!matches!(g.admit(&call(&write)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 1, "a write is the person's");

    let (mut g, person, ..) = automode_gate(&[Stage::Reads], r#"{"verdict":"ask","evidence":""}"#);
    assert!(!matches!(g.admit(&call(&read)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 1, "an ask is the person's");
}

/// A stage the operator did not promote is the person's, and the model is not even asked.
#[test]
fn a_stage_the_shadow_does_not_answer_goes_to_the_person_unasked() {
    let read = serde_json::json!({ "command": "/usr/bin/cat /etc/hostname" });
    let (mut g, person, _, asked) = automode_gate(&[Stage::WorkspaceWrites], ALLOW);
    assert!(!matches!(g.admit(&call(&read)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 1);
    assert_eq!(asked.load(Ordering::SeqCst), 0, "the shadow was consulted");
}

/// Stage 2: a write inside the workspace is the shadow's; one that lands outside it is not.
#[test]
fn at_stage_two_a_write_inside_the_workspace_runs_and_one_outside_asks() {
    let inside = serde_json::json!({ "command": "/usr/bin/touch /w/notes.txt" });
    let outside = serde_json::json!({ "command": "/usr/bin/touch /etc/hostname" });
    let (mut g, person, ..) = automode_gate(&[Stage::Reads, Stage::WorkspaceWrites], ALLOW);
    assert!(
        matches!(g.admit(&call(&inside)), GateDecision::Admit),
        "a workspace write is admitted on a cited allow"
    );
    assert_eq!(*person.0.lock().unwrap(), 0);
    assert!(!matches!(g.admit(&call(&outside)), GateDecision::Admit));
    assert_eq!(
        *person.0.lock().unwrap(),
        1,
        "outside the workspace is the person's"
    );
}

/// Stage 3: a command whose reach is the workspace is the shadow's, and the card says so; one that
/// raises privilege or writes outside it is not.
#[test]
fn at_stage_three_a_command_inside_the_workspace_runs_and_the_card_names_the_stage() {
    let inside = serde_json::json!({ "command": "/usr/bin/python3 /w/x.py" });
    let privileged = serde_json::json!({ "command": "/usr/bin/sudo /usr/bin/id" });
    let (mut g, person, ..) = automode_gate(&all_stages(), ALLOW);
    assert!(
        matches!(g.admit(&call(&inside)), GateDecision::Admit),
        "exec inside the workspace is admitted on a cited allow"
    );
    assert_eq!(*person.0.lock().unwrap(), 0);
    assert!(!matches!(g.admit(&call(&privileged)), GateDecision::Admit));
    assert_eq!(
        *person.0.lock().unwrap(),
        1,
        "raising privilege is the person's"
    );
}

/// The always-ask list is the operator's whatever stages are on, and the shadow is not asked.
#[test]
fn the_always_ask_list_is_never_the_shadows_with_every_stage_on() {
    let destroy = serde_json::json!({ "command": "/usr/bin/rm -rf /usr" });
    let (mut g, person, _, asked) = automode_gate(&all_stages(), ALLOW);
    assert!(!matches!(g.admit(&call(&destroy)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 1, "the person decides");
    assert_eq!(asked.load(Ordering::SeqCst), 0, "the shadow was not asked");
}

/// A shadow that does not answer inside its deadline is a card, never an allow.
#[test]
fn a_shadow_that_does_not_answer_inside_its_deadline_asks() {
    let read = serde_json::json!({ "command": "/usr/bin/cat /etc/hostname" });
    let (mut g, person, ..) =
        automode_gate_slow(&all_stages(), ALLOW, Some(Duration::from_millis(600)));
    assert!(!matches!(g.admit(&call(&read)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 1, "a timeout is a card");
}

/// The breaker: while it is tripped nothing is answered, at any stage.
#[test]
fn a_tripped_breaker_asks_at_every_stage() {
    let read = serde_json::json!({ "command": "/usr/bin/cat /etc/hostname" });
    let mut person_asks = 0;
    for stages in [&[Stage::Reads][..], &all_stages()[..]] {
        let person = Person::default();
        let (cell, asked) = shadow_cell(stages, ALLOW, None);
        let advisor = ShadowAdjudicator::new(
            cell,
            Answering::new(false),
            stages.iter().copied().collect(),
            "/home/op".into(),
            "/w".into(),
            true,
            GrantedRoots::default(),
        );
        let mut g = letibot_tools::AdjudicatedGate::new(Box::new(person.clone()))
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
        assert!(!matches!(g.admit(&call(&read)), GateDecision::Admit));
        assert_eq!(
            asked.load(Ordering::SeqCst),
            0,
            "a tripped breaker asks nobody"
        );
        person_asks += *person.0.lock().unwrap();
    }
    assert_eq!(person_asks, 2, "the person answered at both stage sets");
}

/// An allow is machinery, not news: no row arrives for it, and the gate's audit row - what `/gate`
/// reads and what the corpus keeps - still says the shadow took the decision and admitted.
#[test]
fn an_answer_is_no_row_and_the_audit_names_the_shadow() {
    let read = serde_json::json!({ "command": "/usr/bin/cat /etc/hostname" });
    let (mut g, _, answering, _) = automode_gate(&[Stage::Reads], ALLOW);
    assert!(matches!(g.admit(&call(&read)), GateDecision::Admit));
    assert!(
        answering.take_rows().is_empty(),
        "the burst of `shadow allowed …` rows the operator complained about"
    );
    let row = g.log.last().expect("the decision is a row");
    assert_eq!(
        row.decision.by, "shadow:deepseek",
        "never filed as a person's answer"
    );
    assert!(
        row.decision.basis.contains("deepseek"),
        "{}",
        row.decision.basis
    );
    assert_eq!(row.effect, "admit");
    assert_eq!(stage_of_the_audit(&g, &[]), Stage::Reads);
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

/// Why a call fell through, as the card says it: the stage, the tier or the workspace.
#[test]
fn why_a_call_fell_through() {
    let adj = adjudicator(&[Stage::WorkspaceWrites]);
    for (cmd, want) in [
        (
            "/usr/bin/cat /etc/hostname",
            "`reads` is not a stage the shadow answers",
        ),
        (
            "/usr/bin/rm -rf /usr",
            "destruction_outside_the_project` is on the always-ask list",
        ),
        (
            "/usr/bin/sudo /usr/bin/id",
            "privilege_escalation` is on the always-ask list",
        ),
        (
            "/usr/bin/cat /home/op/.ssh/id_rsa",
            "the tier is blocked, which the operator decides",
        ),
        (
            "/usr/bin/python3 /w/x.py",
            "`exec` is not a stage the shadow answers",
        ),
        (
            "/usr/bin/touch /etc/hostname",
            "it writes /etc/hostname, outside the workspace",
        ),
    ] {
        let d = adj.decide(&crate::guard_via::tests::request_for(cmd));
        assert!(
            matches!(
                d.outcome,
                letibot_tools::adjudicate::DecisionOutcome::Escalate { .. }
            ),
            "{cmd} was answered: {:?}",
            d.outcome
        );
        assert!(d.basis.contains(want), "{cmd}: {}", d.basis);
    }
}

/// A call whose reach is past the stage's is refused by the reach, not by the model.
#[test]
fn a_reach_past_the_stage_is_refused_by_the_stage() {
    let adj = adjudicator(&[Stage::Reads, Stage::WorkspaceWrites]);
    let mut r = crate::guard_via::tests::request_for("/usr/bin/touch /w/notes.txt");
    // What the gate computes for a file tool that names a path outside its workspace.
    r.class =
        letibot_tools::adjudicate::ActionClass::host(letibot_tools::Access::Write, false, false);
    let d = adj.decide(&r);
    assert!(d.basis.contains("lands on `host_other`"), "{}", d.basis);
}

/// A root the operator granted writable outside the workspace, as one list a test can change.
type Roots = Arc<Mutex<Vec<(std::path::PathBuf, bool)>>>;

fn granted(roots: &[(&str, bool)]) -> (GrantedRoots, Roots) {
    let list: Roots = Arc::new(Mutex::new(
        roots
            .iter()
            .map(|(p, write)| (std::path::PathBuf::from(*p), *write))
            .collect(),
    ));
    let read = list.clone();
    (
        GrantedRoots::live(move || read.lock().unwrap_or_else(|e| e.into_inner()).clone()),
        list,
    )
}

/// A session at `automode` whose shadow answers `stages`, with the operator's granted roots read
/// by BOTH halves that judge a call: the gate's class and the shadow's own stage.
fn gate_with_grants(
    stages: &[Stage],
    roots: &[(&str, bool)],
) -> (
    letibot_tools::AdjudicatedGate,
    Person,
    Arc<Answering>,
    Arc<AtomicU32>,
    Roots,
) {
    let person = Person::default();
    let (cell, asked) = shadow_cell(stages, ALLOW, None);
    let set: BTreeSet<Stage> = stages.iter().copied().collect();
    let answering = Answering::new(true);
    let (for_oracle, list) = granted(roots);
    let (for_gate, _) = granted(roots);
    let advisor = ShadowAdjudicator::new(
        cell,
        answering.clone(),
        set,
        "/home/op".into(),
        "/w".into(),
        true,
        for_oracle,
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
        .with_granted_roots(for_gate)
        .with_advisor(Arc::new(advisor))
        .start_supervised(true);
    (g, person, answering, asked, list)
}

/// A file tool naming `args`, for the grant tests: the class comes from its `path`.
fn write_call(args: &serde_json::Value) -> letibot_tools::GateCall<'_> {
    letibot_tools::GateCall {
        name: "write",
        access: letibot_tools::Access::Write,
        args,
        turn_id: "t1",
        call_id: "c2",
        workspace: "/w",
        target_exists: Some(true),
        scripts: &[],
        preview: &[],
    }
}

/// The stage a decision was answered at, read from the request the gate recorded: `stage_of` IS the
/// derivation the decider used, so the test and the shadow cannot disagree about which stage took
/// the call.
fn stage_of_the_audit(g: &letibot_tools::AdjudicatedGate, granted: &[&str]) -> Stage {
    let req = &g
        .log
        .last()
        .expect("the decision is the gate's own row")
        .request;
    let roots: Vec<std::path::PathBuf> = granted.iter().map(std::path::PathBuf::from).collect();
    super::oracle::stage_of(
        req,
        std::path::Path::new("/home/op"),
        std::path::Path::new("/w"),
        &roots,
    )
    .expect("a stage answers it")
}

/// A folder the operator granted writable is a workspace for a write: the class is the project's
/// own and the stage that answers it is `workspace_writes`, with nobody asked.
#[test]
fn a_write_into_a_granted_folder_is_answered_at_workspace_writes() {
    let path = "/data/scratch/notes.txt";
    let args = serde_json::json!({ "path": path });
    let (mut g, person, answering, _, _) = gate_with_grants(
        &[Stage::Reads, Stage::WorkspaceWrites],
        &[("/data/scratch", true)],
    );
    assert!(
        matches!(g.admit(&write_call(&args)), GateDecision::Admit),
        "a write into a granted folder is the shadow's"
    );
    assert_eq!(*person.0.lock().unwrap(), 0, "nobody was asked");
    assert_eq!(
        stage_of_the_audit(&g, &["/data/scratch"]),
        Stage::WorkspaceWrites
    );
    assert!(
        answering.take_rows().is_empty(),
        "an allow is machinery, not a card for the person watching"
    );
}

/// The same write with no grant is outside the workspace, and the person decides.
#[test]
fn the_same_write_without_a_grant_asks() {
    let args = serde_json::json!({ "path": "/data/scratch/notes.txt" });
    let (mut g, person, ..) = gate_with_grants(
        &[Stage::Reads, Stage::WorkspaceWrites],
        &[("/data/scratch", false)],
    );
    assert!(!matches!(g.admit(&write_call(&args)), GateDecision::Admit));
    assert_eq!(
        *person.0.lock().unwrap(),
        1,
        "a read-only grant is no write"
    );
}

/// Destruction in a folder the operator granted writable is ordinary work, as it is in the
/// workspace: exec answers it and the always-ask row for a delete outside the project does not
/// fire on where it lands.
#[test]
fn a_delete_inside_a_granted_folder_is_answered_at_exec() {
    let rm = serde_json::json!({ "command": "/usr/bin/rm -rf /data/scratch/tree" });
    let (mut g, person, answering, _, _) =
        gate_with_grants(&all_stages(), &[("/data/scratch", true)]);
    assert!(
        matches!(g.admit(&call(&rm)), GateDecision::Admit),
        "a delete inside the grant is the shadow's"
    );
    assert_eq!(*person.0.lock().unwrap(), 0, "nobody was asked");
    assert_eq!(stage_of_the_audit(&g, &["/data/scratch"]), Stage::Exec);
    assert!(answering.take_rows().is_empty(), "an allow is not a card");
}

/// A grant revoked between two calls stops counting at once: the same write asks again.
#[test]
fn a_revoked_grant_stops_counting_at_once() {
    let args = serde_json::json!({ "path": "/data/scratch/notes.txt" });
    let (mut g, person, _, _, roots) = gate_with_grants(
        &[Stage::Reads, Stage::WorkspaceWrites],
        &[("/data/scratch", true)],
    );
    assert!(matches!(g.admit(&write_call(&args)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 0);
    roots.lock().unwrap().clear();
    assert!(!matches!(g.admit(&write_call(&args)), GateDecision::Admit));
    assert_eq!(
        *person.0.lock().unwrap(),
        1,
        "a grant taken back is a workspace no longer"
    );
}

/// The always-ask list is the operator's whatever was granted: a grant says where the session may
/// work, never that privilege may be raised there, and the shadow is not asked.
#[test]
fn the_always_ask_list_wins_over_a_grant() {
    let sudo = serde_json::json!({ "command": "/usr/bin/sudo /usr/bin/rm -rf /data/scratch/tree" });
    let (mut g, person, _, asked, _) = gate_with_grants(&all_stages(), &[("/data/scratch", true)]);
    assert!(!matches!(g.admit(&call(&sudo)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 1, "the person decides");
    assert_eq!(asked.load(Ordering::SeqCst), 0, "the shadow was not asked");
}

/// The card's own command: three programs this build never heard of, one `/sys` counter and a
/// `head`. Unknown is the exec stage's, so the model judges it instead of the person being asked.
const DIAGNOSTIC: &str = "cat /sys/class/net/eno1np0/carrier_changes; echo ---; ip neigh 2>&1; \
                          echo ---; (networkctl status eno1np0 2>&1 | head -20) ; echo ---; \
                          nmcli -t dev status 2>&1 | head";

#[test]
fn a_read_only_diagnostic_of_the_host_is_the_shadows() {
    let (mut g, person, _, asked) = automode_gate(&all_stages(), ALLOW);
    assert!(
        matches!(
            g.admit(&call(&serde_json::json!({ "command": DIAGNOSTIC }))),
            GateDecision::Admit
        ),
        "the shadow's cited allow did not admit the diagnostic"
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the shadow was consulted");
    assert_eq!(*person.0.lock().unwrap(), 0, "and the person was not");
}

/// `Unknown` is not the model's at any cost: a call carrying it that writes outside the workspace
/// is the person's, and the lexical screen refuses it before the model is asked.
#[test]
fn an_unknown_program_with_a_write_outside_the_workspace_asks() {
    let write =
        serde_json::json!({ "command": "nmcli -t dev status; /usr/bin/touch /etc/hostname" });
    let (mut g, person, _, asked) = automode_gate(&all_stages(), ALLOW);
    assert!(!matches!(g.admit(&call(&write)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 1, "the person decides");
    assert_eq!(asked.load(Ordering::SeqCst), 0, "the shadow was not asked");
}

/// And with `exec` not promoted, an unknown program is the person's too: how far the shadow reaches
/// is what the operator promoted, not what the model is willing to judge.
#[test]
fn an_unknown_program_without_the_exec_stage_asks() {
    let unknown = serde_json::json!({ "command": "nmcli -t dev status" });
    let (mut g, person, _, asked) = automode_gate(&[Stage::Reads, Stage::WorkspaceWrites], ALLOW);
    assert!(!matches!(g.admit(&call(&unknown)), GateDecision::Admit));
    assert_eq!(*person.0.lock().unwrap(), 1, "the person decides");
    assert_eq!(asked.load(Ordering::SeqCst), 0, "the shadow was not asked");
}
