//! A bridge seat answering a permission card, against a real hub and socket: the answer is filed as
//! the agent's and never as a person's, and a card of a session no package started is refused.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_bridge::daemon::Daemon;
use letibot_bridge::head::Head;
use letibot_harnessd::{Answers, HeadAdjudicator};
use letibot_sessionlog::hub::{CommandKind, Hub, Reply};
use letibot_tools::adjudicate::{
    ActionClass, AdjudicationDecision, AdjudicationRequest, Adjudicator, DecisionOutcome, Offer,
    OnTimeout, RequestKind, Tier,
};
use letibot_tools::schema::Access;

const PATIENCE: Duration = Duration::from_secs(20);

/// A write card that offers `allow_session`, the answer a prompt-injected seat would pick.
fn request(session: &str) -> AdjudicationRequest {
    let offer = Offer {
        derived: Some("src/main.rs".into()),
        program: letibot_tools::adjudicate::NO_PROGRAM.into(),
        session: true,
        note: None,
    };
    AdjudicationRequest {
        scripts: Vec::new(),
        preview: Vec::new(),
        id: "adj-1".into(),
        session_id: session.into(),
        turn_id: "t1".into(),
        call_id: "c1".into(),
        agent: "a".into(),
        tool: "write".into(),
        target: "src/main.rs".into(),
        class: ActionClass::host(Access::Write, true, false),
        summary: "`write` wants write access to `src/main.rs`".into(),
        arguments: serde_json::json!({"path": "src/main.rs"}),
        arguments_digest: "fnv1a:0".into(),
        boundary_facts: vec!["workspace: /tmp/p".into()],
        kind: RequestKind::Permission,
        options: letibot_tools::adjudicate::permission_options(&offer),
        options_note: None,
        on_timeout: OnTimeout::Deny,
        tier: Tier::MayApprove,
        resolved: true,
        baseline: "writes one file inside the project".into(),
        trail: Default::default(),
        prior: Vec::new(),
        examples: Vec::new(),
        brief_variant: letibot_tools::authorise::BriefVariant::AskedForIt,
        agent_claim: None,
        reading: None,
        shape: None,
        advice: None,
    }
}

/// A hub served on a socket with the daemon's own answer sink, and a bridge head on it. Whoever
/// attaches first is who the session is for, so the operator's head attaching first makes it
/// theirs and the bridge attaching first makes it a package's.
struct World {
    hub: Arc<Hub>,
    answers: Arc<Answers>,
    head: Head,
    _operator: Option<letibot_sessionlog::client::HeadClient>,
    server: letibot_sessionlog::server::ServerHandle,
}

fn world(tag: &str, started_by_a_package: bool) -> World {
    // The data directory is this binary's own: a bridge resolves its seat marks and its store from
    // it, and a test that leaves it unsaid reaches the operator's.
    letibot_testenv::isolate_data();
    let socket: PathBuf = letibot_sessionlog::testing::scratch_root()
        .join(format!("bridge-answers-{tag}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    let server = letibot_sessionlog::server::serve(hub.clone(), socket.clone()).expect("bind");
    let d = Daemon {
        workspace: socket.clone(),
        key: "k".into(),
        socket,
        log: Default::default(),
        store: Default::default(),
        placement: None,
        started: false,
    };
    let operator = (!started_by_a_package).then(|| {
        letibot_sessionlog::client::HeadClient::attach(
            &d.socket,
            "s",
            0,
            "tui",
            "dead",
            Default::default(),
        )
        .expect("the operator's head attaches")
        .0
    });
    let head = Head::attach(&d, "s").expect("the bridge attaches");
    assert_eq!(
        hub.started_by_a_package(),
        started_by_a_package,
        "the first head to attach decides who the session is for"
    );
    World {
        hub,
        answers,
        head,
        _operator: operator,
        server,
    }
}

/// The adjudicator waits on its own thread, as the gate's does.
fn decide(w: &World, budget: Duration) -> std::thread::JoinHandle<AdjudicationDecision> {
    let adj = HeadAdjudicator::new(w.hub.clone(), w.answers.clone()).with_budget(budget);
    std::thread::spawn(move || adj.decide(&request("s")))
}

/// The card's id, once the bridge head holds it.
fn the_card(head: &Head) -> String {
    let end = Instant::now() + PATIENCE;
    loop {
        if let Some(a) = head.asks().first() {
            return a.req_id.clone();
        }
        assert!(Instant::now() < end, "the card never reached the bridge");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A seat's `allow_session` on a package's own card is settled, and filed as the agent's: `human:`
/// is what the gate and corpus replay read as a person having seen the card.
#[test]
fn bridge_answer_is_not_recorded_as_human() {
    let w = world("agent", true);
    let pending = decide(&w, PATIENCE);
    let card = the_card(&w.head);
    w.head
        .answer(&card, "allow_session", None)
        .expect("the bridge sends the answer");
    let d = pending.join().expect("the adjudicator");
    assert_eq!(
        d.outcome,
        DecisionOutcome::Selected {
            option_id: "allow_session".into()
        },
        "{d:?}"
    );
    assert!(
        !d.by.starts_with("human:"),
        "a bridge seat's answer is filed as a person's: {}",
        d.by
    );
    assert!(d.by.starts_with("agent:"), "{}", d.by);
    assert!(
        d.basis.contains("not the operator"),
        "the basis says who chose: {}",
        d.basis
    );
    w.server.shutdown();
}

/// A bridge seat answers cards only for sessions it started. On the operator's own
/// session its answer is refused, the card stays open, and the operator's head still settles it.
#[test]
fn a_bridge_seat_cannot_answer_a_card_of_a_session_it_did_not_start() {
    let w = world("refused", false);
    let operator = w.hub.attach("tui", "dead", Default::default(), 0);
    let pending = decide(&w, PATIENCE);
    let card = the_card(&w.head);
    let _ = w.head.answer(&card, "allow_session", None);
    // The refusal is the daemon's: the card is still open after the seat's answer.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        w.hub
            .snapshot()
            .open_decisions
            .iter()
            .any(|o| o.req_id == card),
        "the seat's answer settled the operator's card"
    );
    w.hub.submit(
        &operator.head_id,
        "c-op",
        0,
        CommandKind::Answer {
            req_id: card,
            reply: Reply::Permission {
                option_id: "reject_once".into(),
                pattern: None,
                note: None,
            },
        },
    );
    let d = pending.join().expect("the adjudicator");
    assert_eq!(
        d.outcome,
        DecisionOutcome::Selected {
            option_id: "reject_once".into()
        },
        "the operator's answer is the one that settled it: {d:?}"
    );
    assert_eq!(d.by, "human:dead");
    w.server.shutdown();
}
