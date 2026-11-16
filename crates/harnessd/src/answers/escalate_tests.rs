//! A queue session's live card, with no head of its own, reaches the main session's head, and the
//! answer reaches the session that asked.

use std::sync::atomic::{AtomicBool, Ordering};

use letibot_sessionlog::hub::CommandKind;
use letibot_sessionlog::pending_asks::PendingAsks;
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::view::OpenDecision;

use super::tests::request;
use super::*;

const MAIN: &str = "s-main";
const GATEKEEPER: &str = "s-gatekeeper-k";
const REVIEWER: &str = "s-gatekeeper-k-sub-1";

fn deciding() -> Caps {
    Caps {
        can_decide: true,
        ..Caps::default()
    }
}

fn open_of(hub: &Arc<Hub>) -> Vec<OpenDecision> {
    hub.snapshot().open_decisions
}

/// The main session with a deciding head, and the gatekeeper with none whose cards escalate.
struct World {
    main: Arc<Hub>,
    head: String,
    gatekeeper: Arc<Hub>,
    answers: Arc<Answers>,
    set: Arc<PendingAsks>,
}

fn world() -> World {
    let main = Hub::new(MAIN);
    let head = main.attach("tui", "operator", deciding(), 0).head_id;
    let gatekeeper = Hub::new(GATEKEEPER);
    let answers = Arc::new(Answers::new());
    gatekeeper.set_answer_sink(answers.clone());
    let set = PendingAsks::new();
    set.set_main(&main);
    gatekeeper.set_escalation(set.clone());
    World {
        main,
        head,
        gatekeeper,
        answers,
        set,
    }
}

/// Wait on `hub` for an open card, then run `then` on it. Stops when `stop` is set.
fn answer_when_open<T: Send + 'static>(
    hub: Arc<Hub>,
    stop: Arc<AtomicBool>,
    then: impl Fn(&OpenDecision) -> T + Send + 'static,
) -> std::thread::JoinHandle<Option<T>> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            if let Some(d) = open_of(&hub).first() {
                return Some(then(d));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    })
}

fn allow(hub: &Hub, head: &str, req_id: &str) -> letibot_sessionlog::protocol::ServerFrame {
    hub.submit(
        head,
        "c1",
        0,
        CommandKind::Answer {
            req_id: req_id.to_string(),
            reply: Reply::Permission {
                option_id: "allow_once".into(),
                pattern: None,
                note: None,
            },
        },
    )
}

#[test]
fn a_reviewers_live_permission_card_reaches_the_main_session_and_its_answer_the_reviewer() {
    let w = world();
    let reviewer = SubagentAsk {
        handle: REVIEWER.into(),
        task: "review red-master".into(),
        root: GATEKEEPER.into(),
        ..Default::default()
    };
    let adj = SubagentAdjudicator::new(
        reviewer,
        Some(
            HeadAdjudicator::new(w.gatekeeper.clone(), w.answers.clone())
                .with_budget(Duration::from_secs(10)),
        ),
    );
    let mut req = request();
    req.id = "adj-reviewer-0001".into();
    req.session_id = REVIEWER.into();

    let stop = Arc::new(AtomicBool::new(false));
    let main = w.main.clone();
    let head = w.head.clone();
    let set = w.set.clone();
    let answerer = answer_when_open(w.main.clone(), stop.clone(), move |d| {
        let pending = set.pending();
        let frame = allow(&main, &head, &d.req_id);
        (d.clone(), pending, frame)
    });
    let d = adj.decide(&req);
    stop.store(true, Ordering::SeqCst);
    let (card, pending, frame) = answerer
        .join()
        .unwrap()
        .unwrap_or_else(|| panic!("no card reached the main session: {d:?}"));

    assert!(
        matches!(
            frame,
            letibot_sessionlog::protocol::ServerFrame::Accepted { .. }
        ),
        "{frame:?}"
    );
    assert_eq!(card.req_id, "adj-reviewer-0001");
    assert_eq!(card.kind, "permission");
    let named = card.subagent.expect("the card names who asks");
    assert_eq!(named.handle, REVIEWER);
    assert_eq!(named.root, MAIN);
    assert_eq!(
        pending,
        vec![letibot_transcript::pending::Pending::new(
            letibot_transcript::pending::PendingKind::Approval,
            REVIEWER,
            "adj-reviewer-0001",
        )]
    );
    assert_eq!(
        d.outcome,
        DecisionOutcome::Selected {
            option_id: "allow_once".into()
        },
        "{d:?}"
    );
    assert!(d.by.contains("operator"), "{}", d.by);
    assert!(
        open_of(&w.main).is_empty(),
        "the main head's copy is closed"
    );
    assert!(open_of(&w.gatekeeper).is_empty());
    assert!(w.set.ids().is_empty());
}

/// The same card answered at the gatekeeper's own head, attached after the ask: the main
/// session's copy is taken down.
#[test]
fn an_answer_at_the_asking_session_retracts_the_main_sessions_card() {
    let w = world();
    let adj = HeadAdjudicator::new(w.gatekeeper.clone(), w.answers.clone())
        .with_budget(Duration::from_secs(10));
    let req = request();

    let stop = Arc::new(AtomicBool::new(false));
    let gk = w.gatekeeper.clone();
    let answerer = answer_when_open(w.main.clone(), stop.clone(), move |d| {
        let mine = gk.attach("tui", "operator-at-the-gatekeeper", deciding(), 0);
        allow(&gk, &mine.head_id, &d.req_id);
    });
    let d = adj.decide(&req);
    stop.store(true, Ordering::SeqCst);
    answerer
        .join()
        .unwrap()
        .expect("the main session got the card");

    assert!(d.by.contains("operator-at-the-gatekeeper"), "{}", d.by);
    assert!(
        open_of(&w.main).is_empty(),
        "the main head's copy is retracted"
    );
    let settled = w.main.snapshot().settled_decisions;
    assert!(
        settled.iter().any(|s| s.req_id == req.id),
        "the main head is told how it ended: {settled:?}"
    );
    assert!(w.set.ids().is_empty());
}

/// The gatekeeper's own `ask_user_question` goes the same way.
#[test]
fn a_queue_sessions_question_reaches_the_main_session() {
    let w = world();
    let q = HeadQuestioner::new(w.gatekeeper.clone(), w.answers.clone())
        .with_budget(Duration::from_secs(10));
    let stop = Arc::new(AtomicBool::new(false));
    let main = w.main.clone();
    let head = w.head.clone();
    let answerer = answer_when_open(w.main.clone(), stop.clone(), move |d| {
        main.submit(
            &head,
            "c1",
            0,
            CommandKind::Answer {
                req_id: d.req_id.clone(),
                reply: Reply::Question(letibot_sessionlog::question::QuestionAnswer {
                    option: Some(1),
                    note: None,
                    free: None,
                    abstain: false,
                }),
            },
        );
        d.subagent.clone()
    });
    let got = q.ask(&Question {
        text: "4 GiB or 8 GiB?".into(),
        options: vec!["4".into(), "8".into()],
        ..Default::default()
    });
    stop.store(true, Ordering::SeqCst);
    let named = answerer.join().unwrap().flatten().expect("named");
    assert_eq!(named.handle, GATEKEEPER);
    let (answer, by) = got.expect("answered");
    assert_eq!(answer.option, Some(1));
    assert_eq!(by, "operator");
    assert!(open_of(&w.main).is_empty());
}

/// With no main session the gate still fails closed at once.
#[test]
fn with_no_main_session_a_queue_card_still_fails_closed() {
    let gatekeeper = Hub::new(GATEKEEPER);
    let answers = Arc::new(Answers::new());
    gatekeeper.set_answer_sink(answers.clone());
    gatekeeper.set_escalation(PendingAsks::new());
    let d = HeadAdjudicator::new(gatekeeper, answers).decide(&request());
    assert_eq!(d.by, "gate:no-head", "{d:?}");
}

const CLONE: &str = "/tmp/letibot-review-clones/review-agent-p2-followups-34db3e56";

fn reviewer_ask() -> SubagentAsk {
    SubagentAsk {
        handle: REVIEWER.into(),
        task: "You are the gatekeeper. A subagent has finished work on a branch".into(),
        root: GATEKEEPER.into(),
        ..Default::default()
    }
}

fn name_reviewer(set: &PendingAsks) {
    use letibot_sessionlog::pending_asks::Asker;
    set.name_askers(
        GATEKEEPER,
        [(
            REVIEWER.to_string(),
            Asker {
                role: "reviewer of agent-p2-followups (rano review queue)".into(),
                workspace: CLONE.into(),
                workspace_short: "review clone of agent-p2-followups".into(),
            },
        )],
    );
}

/// The main session's copy of a reviewer's card names the reviewer by role and work, shows its
/// clone in short, and offers no always-allow: that rule would be box-wide. The asker's own copy
/// has no always-allow either, and an `allow_always` sent anyway runs nothing.
#[test]
fn a_relayed_card_names_the_reviewer_and_offers_no_box_wide_rule() {
    let w = world();
    name_reviewer(&w.set);
    let adj = SubagentAdjudicator::new(
        reviewer_ask(),
        Some(
            HeadAdjudicator::new(w.gatekeeper.clone(), w.answers.clone())
                .with_budget(Duration::from_secs(10)),
        ),
    );
    let mut req = request();
    req.id = "adj-reviewer-0002".into();
    req.session_id = REVIEWER.into();
    req.boundary_facts = vec![format!("workspace: {CLONE}-92726-0")];
    assert!(
        req.options
            .iter()
            .any(|o| o.kind == ToolOptionKind::AllowAlways),
        "the fixture offers the durable rung"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let (main, head, gk) = (w.main.clone(), w.head.clone(), w.gatekeeper.clone());
    let answerer = answer_when_open(w.main.clone(), stop.clone(), move |d| {
        let own = open_of(&gk);
        main.submit(
            &head,
            "c1",
            0,
            CommandKind::Answer {
                req_id: d.req_id.clone(),
                reply: Reply::Permission {
                    option_id: "allow_always".into(),
                    pattern: None,
                    note: None,
                },
            },
        );
        (d.clone(), own)
    });
    let d = adj.decide(&req);
    stop.store(true, Ordering::SeqCst);
    let (card, own) = answerer
        .join()
        .unwrap()
        .expect("the card reached the main session");

    let named = card.subagent.clone().expect("the card names who asks");
    assert_eq!(named.handle, REVIEWER);
    assert_eq!(
        named.role,
        "reviewer of agent-p2-followups (rano review queue)"
    );
    assert_eq!(
        card.because,
        "workspace: review clone of agent-p2-followups"
    );
    for c in std::iter::once(&card).chain(own.iter()) {
        assert!(
            c.options.iter().all(|o| o.kind != OptionKind::AllowAlways),
            "{:?}",
            c.options
        );
        assert!(
            c.options_note.contains(RELAYED_NO_DURABLE),
            "{}",
            c.options_note
        );
    }
    assert_eq!(d.outcome, DecisionOutcome::Unavailable, "{d:?}");
    assert!(w.answers.take_pattern(&req.id).is_none());
}

/// "Allow for the rest of the session" on a relayed card is the reviewer's grant: it stands on the
/// reviewer's gate, and the reviewer's next call of the class asks nobody.
#[test]
fn a_session_grant_from_a_relayed_ask_lands_on_the_reviewers_gate() {
    use letibot_tools::schema::Access;
    use letibot_tools::{AdjudicatedGate, Gate as _, GateCall, GateDecision};
    let w = world();
    name_reviewer(&w.set);
    let adj = SubagentAdjudicator::new(
        reviewer_ask(),
        Some(
            HeadAdjudicator::new(w.gatekeeper.clone(), w.answers.clone())
                .with_budget(Duration::from_secs(10)),
        ),
    );
    let mut reviewer_gate =
        AdjudicatedGate::new(Box::new(adj)).with_mode(letibot_tools::mode::Mode::WRITES_ALLOWED);

    let cards = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (main, head, seen, stop2) = (w.main.clone(), w.head.clone(), cards.clone(), stop.clone());
    let answerer = std::thread::spawn(move || {
        let mut answered = std::collections::HashSet::new();
        while !stop2.load(Ordering::SeqCst) {
            if let Some(d) = open_of(&main).first()
                && answered.insert(d.req_id.clone())
            {
                assert!(
                    d.options.iter().any(|o| o.option_id == "allow_session"),
                    "{:?}",
                    d.options
                );
                seen.fetch_add(1, Ordering::SeqCst);
                main.submit(
                    &head,
                    "c1",
                    0,
                    CommandKind::Answer {
                        req_id: d.req_id.clone(),
                        reply: Reply::Permission {
                            option_id: "allow_session".into(),
                            pattern: None,
                            note: None,
                        },
                    },
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });

    let args = serde_json::json!({"query": "x"});
    let call = |id: &'static str| GateCall {
        name: "web_search",
        access: Access::Network,
        args: &args,
        turn_id: "t1",
        call_id: id,
        workspace: CLONE,
        target_exists: None,
        scripts: &[],
        preview: &[],
    };
    let first = reviewer_gate.admit(&call("c1"));
    let second = reviewer_gate.admit(&call("c2"));
    stop.store(true, Ordering::SeqCst);
    answerer.join().unwrap();

    assert!(matches!(first, GateDecision::Admit), "{first:?}");
    assert!(matches!(second, GateDecision::Admit), "{second:?}");
    assert_eq!(
        reviewer_gate.grants().len(),
        1,
        "the grant is the reviewer's"
    );
    assert_eq!(
        cards.load(Ordering::SeqCst),
        1,
        "the reviewer's second call was covered by its own grant"
    );
    assert!(open_of(&w.main).is_empty());
}

// ------------------------------------------------- the main session's shadow on a relayed card

/// A judge that answers `reply` after `sleep`.
struct Slow {
    reply: &'static str,
    sleep: Duration,
}

impl crate::shadow::judge::Judge for Slow {
    fn label(&self) -> String {
        "deepseek".into()
    }
    fn judge(&self, _system: &str, _user: &str) -> Result<String, String> {
        std::thread::sleep(self.sleep);
        Ok(self.reply.into())
    }
}

#[derive(Default)]
struct Rows(std::sync::Mutex<Vec<String>>);

impl crate::shadow::record::Recorder for Rows {
    fn row(&self, title: &str, _body: &str) {
        self.0.lock().unwrap().push(title.to_string());
    }
}

/// The main session's shadow, its permission job on, answering through `judge` with a 300ms
/// deadline, installed as the set's annotator.
fn with_shadow(w: &World, judge: Slow) -> Arc<Rows> {
    use crate::shadow::runner::{Shadow, Timing};
    let rows = Arc::new(Rows::default());
    let cfg = crate::shadow::config::ShadowConfig::from_lines(
        letibot_provider::keys::ShadowLines {
            enabled: vec!["permission".into()],
            ..Default::default()
        },
        "local".into(),
    )
    .unwrap();
    let judge: Arc<dyn crate::shadow::judge::Judge> = Arc::new(judge);
    let shadow = Shadow::start(
        &cfg,
        "glm/x",
        None,
        rows.clone(),
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
    let cell: crate::shadow::permission::Cell = Default::default();
    *cell.lock().unwrap() = Some((
        Arc::new(shadow),
        rows.clone() as Arc<dyn crate::shadow::record::Recorder>,
    ));
    w.set
        .set_annotator(Some(Arc::new(crate::shadow::permission::Relay {
            cell,
            rules: vec!["bash clippy-driver *".into()],
            workspace: "/w/proj".into(),
            home: "/home/op".into(),
        })));
    rows
}

/// A reviewer's `clippy-driver --version`, asked through the gatekeeper with no head of its own.
/// The main head sees the card at once, waits `look`, and answers allow; the card it saw last is
/// returned with how long the first sight took.
fn relay_a_probe(w: &World, look: Duration) -> (OpenDecision, Duration, AdjudicationDecision) {
    let adj = SubagentAdjudicator::new(
        SubagentAsk {
            handle: REVIEWER.into(),
            task: "review".into(),
            root: GATEKEEPER.into(),
            ..Default::default()
        },
        Some(
            HeadAdjudicator::new(w.gatekeeper.clone(), w.answers.clone())
                .with_budget(Duration::from_secs(10)),
        ),
    );
    let mut req = request();
    req.id = "adj-probe-0001".into();
    req.session_id = REVIEWER.into();
    req.tool = "bash".into();
    req.arguments = serde_json::json!({"command": "clippy-driver --version"});
    let started = std::time::Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let main = w.main.clone();
    let head = w.head.clone();
    let answerer = answer_when_open(w.main.clone(), stop.clone(), move |d| {
        let first = started.elapsed();
        std::thread::sleep(look);
        let last = open_of(&main)
            .into_iter()
            .find(|o| o.req_id == d.req_id)
            .expect("still open");
        allow(&main, &head, &d.req_id);
        (last, first)
    });
    let d = adj.decide(&req);
    stop.store(true, Ordering::SeqCst);
    let (card, first) = answerer
        .join()
        .unwrap()
        .expect("a card reached the main head");
    (card, first, d)
}

#[test]
fn a_relayed_card_carries_the_shadows_line_when_it_answers_in_time() {
    let w = world();
    let rows = with_shadow(
        &w,
        Slow {
            reply: r#"{"verdict":"allow","rule":"bash clippy-driver *","evidence":""}"#,
            sleep: Duration::from_millis(20),
        },
    );
    let (card, first, d) = relay_a_probe(&w, Duration::from_millis(250));
    assert!(
        first < Duration::from_millis(200),
        "the card waited {first:?}"
    );
    assert_eq!(
        card.shadow.as_deref(),
        Some("shadow (deepseek): allow - matches rule \"bash clippy-driver *\"")
    );
    assert!(
        matches!(d.outcome, DecisionOutcome::Selected { .. }),
        "{d:?}"
    );
    let rows = rows.0.lock().unwrap();
    assert!(
        rows.iter()
            .any(|r| r == "permission · relayed adj-probe-0001 from s-gatekeeper-k-sub-1"),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.starts_with("permission · allow")),
        "the verdict is recorded: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r == "permission · operator answered admit adj-probe-0001"),
        "the operator's answer is recorded beside it: {rows:?}"
    );
}

#[test]
fn a_relayed_card_shows_no_line_and_no_wait_when_the_shadow_times_out() {
    let w = world();
    with_shadow(
        &w,
        Slow {
            reply: r#"{"verdict":"allow","rule":"bash clippy-driver *","evidence":""}"#,
            sleep: Duration::from_millis(900),
        },
    );
    let (card, first, d) = relay_a_probe(&w, Duration::from_millis(500));
    assert!(
        first < Duration::from_millis(200),
        "the card waited {first:?}"
    );
    assert_eq!(card.shadow, None, "a late verdict draws no line");
    assert!(
        matches!(d.outcome, DecisionOutcome::Selected { .. }),
        "{d:?}"
    );
}

// ------------------------------------------------- queue-scoped grants

/// A fresh store file for one test.
fn grant_store(tag: &str) -> crate::config::StoreAt {
    let dir = std::env::temp_dir().join(format!("letibot-grants-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a store dir");
    crate::config::StoreAt::File(dir.join("store.db"))
}

/// A reviewer's gate as a daemon builds it: relayed through the gatekeeper, its grants kept in
/// `at` at the queue's scope.
fn queue_reviewer(
    w: &World,
    id: &str,
    at: &crate::config::StoreAt,
) -> letibot_tools::AdjudicatedGate {
    let ask = SubagentAsk {
        handle: id.into(),
        ..reviewer_ask()
    };
    let adj = SubagentAdjudicator::new(
        ask,
        Some(
            HeadAdjudicator::new(w.gatekeeper.clone(), w.answers.clone())
                .with_budget(Duration::from_secs(10)),
        ),
    );
    letibot_tools::AdjudicatedGate::new(Box::new(adj))
        .with_identity(id, "agent")
        .with_mode(letibot_tools::mode::Mode::WRITES_ALLOWED)
        .with_grant_ledger(
            Arc::new(crate::grants::StoreLedger::new(at.clone())),
            letibot_tools::grant_ledger::queue_of(id).map(str::to_string),
        )
}

/// Answer every card the main session shows with `allow_session`, counting them, and keep the
/// last card's note.
fn allow_for_session(
    w: &World,
    stop: Arc<AtomicBool>,
) -> (
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::Mutex<Vec<String>>>,
    std::thread::JoinHandle<()>,
) {
    let cards = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let notes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (main, head, seen, said) = (w.main.clone(), w.head.clone(), cards.clone(), notes.clone());
    let t = std::thread::spawn(move || {
        let mut answered = std::collections::HashSet::new();
        while !stop.load(Ordering::SeqCst) {
            if let Some(d) = open_of(&main).first()
                && answered.insert(d.req_id.clone())
            {
                seen.fetch_add(1, Ordering::SeqCst);
                said.lock().unwrap().push(format!(
                    "{} | {}",
                    d.options_note,
                    d.options
                        .iter()
                        .map(|o| o.label.clone())
                        .collect::<Vec<_>>()
                        .join(" / ")
                ));
                main.submit(
                    &head,
                    "c1",
                    0,
                    CommandKind::Answer {
                        req_id: d.req_id.clone(),
                        reply: Reply::Permission {
                            option_id: "allow_session".into(),
                            pattern: None,
                            note: None,
                        },
                    },
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    (cards, notes, t)
}

fn search_call<'a>(args: &'a serde_json::Value, id: &'a str) -> letibot_tools::GateCall<'a> {
    letibot_tools::GateCall {
        name: "web_search",
        access: letibot_tools::schema::Access::Network,
        args,
        turn_id: "t1",
        call_id: id,
        workspace: CLONE,
        target_exists: None,
        scripts: &[],
        preview: &[],
    }
}

/// "Allow for the session" on one reviewer's relayed card covers the queue: the next reviewer's
/// same call asks nobody, before and after a restart, the card said so, the main session is not
/// widened, and a revoke takes it away again.
#[test]
fn a_session_answer_on_a_relayed_reviewer_card_covers_the_queue_until_revoked() {
    use letibot_tools::{Gate as _, GateDecision};
    let w = world();
    name_reviewer(&w.set);
    let at = grant_store("queue");
    let args = serde_json::json!({"query": "x"});

    let stop = Arc::new(AtomicBool::new(false));
    let (cards, notes, answerer) = allow_for_session(&w, stop.clone());
    let mut first = queue_reviewer(&w, REVIEWER, &at);
    assert!(matches!(
        first.admit(&search_call(&args, "c1")),
        GateDecision::Admit
    ));
    assert_eq!(cards.load(Ordering::SeqCst), 1);
    let said = notes.lock().unwrap().join("\n");
    assert!(
        said.contains(letibot_tools::grant_ledger::QUEUE_SCOPE_NOTE),
        "the card says how far the answer goes: {said}"
    );

    // The next review is a new sub-session.
    let mut next = queue_reviewer(&w, "s-gatekeeper-k-sub-2", &at);
    assert!(matches!(
        next.admit(&search_call(&args, "c2")),
        GateDecision::Admit
    ));
    assert_eq!(
        cards.load(Ordering::SeqCst),
        1,
        "the next reviewer's same call was covered by the queue's grant"
    );

    // A restart: nothing in memory, only the store.
    drop((first, next));
    let mut after = queue_reviewer(&w, "s-gatekeeper-k-sub-3", &at);
    assert!(matches!(
        after.admit(&search_call(&args, "c3")),
        GateDecision::Admit
    ));
    assert_eq!(cards.load(Ordering::SeqCst), 1, "and after a restart");

    // The main session reads its own scope only.
    let store = at.open().expect("the store");
    assert!(
        crate::grants::load(
            &store,
            &letibot_tools::grant_ledger::Scope::Session(MAIN.into())
        )
        .unwrap()
        .is_empty()
    );
    let main_gate = letibot_tools::AdjudicatedGate::new(Box::new(letibot_tools::NoAdjudicator))
        .with_identity(MAIN, "agent")
        .with_mode(letibot_tools::mode::Mode::WRITES_ALLOWED)
        .with_grant_ledger(Arc::new(crate::grants::StoreLedger::new(at.clone())), None);
    assert!(main_gate.grants().is_empty(), "{:?}", main_gate.grants());

    // Listed, and revoked.
    let listed = crate::grants::permissions(&store, REVIEWER, Some(GATEKEEPER), None);
    assert!(listed.ok, "{:?}", listed.lines);
    let row = crate::grants::load(
        &store,
        &letibot_tools::grant_ledger::Scope::Queue(GATEKEEPER.into()),
    )
    .unwrap();
    assert_eq!(row.len(), 1, "{row:?}");
    assert!(
        listed
            .lines
            .iter()
            .any(|l| l.contains(&row[0].id.to_string())),
        "{:?}",
        listed.lines
    );
    let revoked = crate::grants::permissions(&store, REVIEWER, Some(GATEKEEPER), Some(row[0].id));
    assert!(revoked.ok, "{:?}", revoked.lines);
    assert!(matches!(
        after.admit(&search_call(&args, "c4")),
        GateDecision::Admit
    ));
    stop.store(true, Ordering::SeqCst);
    answerer.join().unwrap();
    assert_eq!(
        cards.load(Ordering::SeqCst),
        2,
        "a revoked grant asks again, in a session that was already running"
    );
}
