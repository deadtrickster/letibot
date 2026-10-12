use super::*;
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::view::OpenDecision;

pub(super) fn request() -> AdjudicationRequest {
    use letibot_tools::adjudicate::{ActionClass, Tier};
    use letibot_tools::schema::Access;
    AdjudicationRequest {
        scripts: Vec::new(),
        id: "adj-1".into(),
        session_id: "s".into(),
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
        options: letibot_tools::adjudicate::permission_options(Some("src/main.rs")),
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

fn open_of(hub: &Arc<Hub>) -> Vec<OpenDecision> {
    hub.snapshot().open_decisions
}

/// **The whole of D25 in one test**: the decision reaches a head, the head sends
/// an answer through the frame that already existed, and the tool thread — which
/// is not the thread the answer arrived on — wakes with it.
#[test]
fn a_head_answers_a_decision_the_daemons_stdin_could_not() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    let head = hub.attach("tui", "deadtrickster", Caps::default(), 0);

    // One value for the adjudicator's budget and the answerer's patience.
    let budget = Duration::from_secs(10);
    let adj = HeadAdjudicator::new(hub.clone(), answers.clone()).with_budget(budget);
    let req = request();

    // The head's side, on its own thread: wait for the question to appear, then
    // answer it the way `letibot-tui` does.
    let hub2 = hub.clone();
    let head_id = head.head_id.clone();
    // **No second deadline.** The answerer used to poll 500 × 2ms — one
    // second — while the adjudicator it races waits ten, so on a box running
    // the whole workspace's test binaries at once it gave up first and
    // panicked "the decision never reached the head" about a decision that
    // was on its way. Widening it to the budget only moved the race: with
    // both at ten seconds a loaded box lost it again the same afternoon.
    //
    // A poller racing a deadline it does not own cannot be made reliable by
    // choosing a bigger number. This one stops when the thing it is helping
    // has finished, and the FAILURE is then the assertion below — "the
    // adjudicator did not come back with the answer" — which is a statement
    // about the code rather than about the load on the box.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let answerer = std::thread::spawn(move || {
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            let open = hub2.snapshot().open_decisions;
            if let Some(d) = open.first() {
                // **And it is this session's own card**, not a subagent's: the new
                // attribution field is `None` for every ask that is not a child's, which
                // is what keeps the clause off every card that never needed it.
                assert!(
                    d.subagent.is_none(),
                    "this session's own call was labelled a subagent's: {:?}",
                    d.subagent
                );
                hub2.submit(
                    &head_id,
                    "c1",
                    0,
                    letibot_sessionlog::hub::CommandKind::Answer {
                        req_id: d.req_id.clone(),
                        reply: Reply::Permission {
                            option_id: "allow_once".into(),
                            pattern: None,
                            note: None,
                        },
                    },
                );
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // The adjudicator returned before a decision was ever open. Not a
        // panic: whatever it returned is what the assertions below read,
        // and they say more about why than this thread can.
        false
    });

    let d = adj.decide(&req);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let submitted = answerer.join().unwrap();
    assert!(submitted, "the head never got to answer: {d:?}");

    assert_eq!(
        d.outcome,
        DecisionOutcome::Selected {
            option_id: "allow_once".into()
        },
        "{d:?}"
    );
    assert!(d.by.contains("deadtrickster"), "{}", d.by);
    assert!(
        open_of(&hub).is_empty(),
        "an answered decision is off the head's screen"
    );
}

// ---- `ask_user_question`: the other vocabulary, the same rendezvous -----

fn a_question() -> Question {
    Question {
        text: "which database should the migration target?".into(),
        options: vec!["postgres".into(), "sqlite".into()],
        because: "the two need different migration files".into(),
        ..Default::default()
    }
}

/// **The whole of `ask_user_question` reaching a person**, end to end inside the
/// daemon: the question is posed to the head as a card with its choices, the head
/// answers it through the frame that already existed, and the tool thread — which
/// is not the thread the answer arrived on — wakes with the answer **and who gave
/// it**.
///
/// The attribution is the half that is not a formality: a question's result is a
/// claim about what a person said, and the identity is read from the connection the
/// answer arrived on rather than from anything the answer carries.
#[test]
fn a_head_answers_a_question_and_the_answer_carries_who_gave_it() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    let head = hub.attach("tui", "deadtrickster", Caps::default(), 0);

    let budget = Duration::from_secs(10);
    let q = a_question();
    let asker = HeadQuestioner::new(hub.clone(), answers.clone()).with_budget(budget);

    // The head's side, on its own thread: wait for the card, check it is a question
    // with the model's own choices on it, and answer it the way `letibot-tui` does.
    let hub2 = hub.clone();
    let head_id = head.head_id.clone();
    let seen = Arc::new(std::sync::Mutex::new(None));
    let seen2 = seen.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let answerer = std::thread::spawn(move || {
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            let open = hub2.snapshot().open_decisions;
            if let Some(d) = open.first() {
                *seen2.lock().unwrap() = Some(d.clone());
                hub2.submit(
                    &head_id,
                    "c1",
                    0,
                    letibot_sessionlog::hub::CommandKind::Answer {
                        req_id: d.req_id.clone(),
                        reply: Reply::Question(
                            letibot_sessionlog::question::QuestionAnswer::choosing(1)
                                .with_note("only for the CUDA box"),
                        ),
                    },
                );
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    });

    let got = asker.ask(&q);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let submitted = answerer.join().unwrap();
    assert!(submitted, "the head never got the question: {got:?}");

    let (a, by) = got.expect("the head answered");
    assert_eq!(a.option, Some(1), "the choice came back as an index");
    assert_eq!(a.note.as_deref(), Some("only for the CUDA box"));
    assert_eq!(a.free, None);
    assert!(!a.abstain);
    assert_eq!(by, "deadtrickster", "the answer is attributed to nobody");

    // **The card is the surface, and it is the card the composer answers.** The
    // posed `DecisionRequested` is a `question` whose ladder is its plain-text
    // choices — `options` is the adjudication ladder and is empty for one, which is
    // why the hub validates an answer against `choices`.
    let card = seen.lock().unwrap().clone().expect("the card was seen");
    assert_eq!(card.kind, "question");
    assert_eq!(card.summary, q.text);
    assert_eq!(card.choices, q.options);
    assert_eq!(card.because, q.because);
    assert!(card.options.is_empty(), "a question has no ladder");
    assert!(card.deadline.is_some(), "a question expires like any ask");

    // And it is off the head's screen, with the answer recorded against it.
    assert!(
        open_of(&hub).is_empty(),
        "an answered question is off the head's screen"
    );
    let settled = hub
        .snapshot()
        .settled_decisions
        .into_iter()
        .find(|d| d.req_id == card.req_id)
        .expect("the answer is on the log");
    assert_eq!(settled.by.kind, "human");
    assert_eq!(settled.by.identity, "deadtrickster");
    assert!(
        settled.basis.contains("chose option 1: sqlite")
            && settled.basis.contains("only for the CUDA box"),
        "the basis is not what the person said: {}",
        settled.basis
    );
}

/// **The exchange is recorded where the session can write it down, and only an
/// answer is.**
///
/// The two halves of this are one test because they are one rule: what a session
/// turns into the operator's own row is what a person SAID, and a question nobody
/// answered is not something they said. `Answers::ask_question`'s `Err` arms are
/// every way a question ends without an answer, and none of them may leave a record
/// behind that the transcript would then attribute to somebody.
#[test]
fn a_questioner_records_the_exchange_it_carried_and_nothing_for_a_refusal() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    let head = hub.attach("tui", "deadtrickster", Caps::default(), 0);
    let q = a_question();
    let log: AskLog = Default::default();
    let asker = HeadQuestioner::new(hub.clone(), answers.clone())
        .with_budget(Duration::from_secs(10))
        .with_ask_log(log.clone());

    let hub2 = hub.clone();
    let head_id = head.head_id.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let answerer = std::thread::spawn(move || {
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            if let Some(d) = hub2.snapshot().open_decisions.first() {
                hub2.submit(
                    &head_id,
                    "c1",
                    0,
                    letibot_sessionlog::hub::CommandKind::Answer {
                        req_id: d.req_id.clone(),
                        reply: Reply::Question(
                            letibot_sessionlog::question::QuestionAnswer::choosing(0),
                        ),
                    },
                );
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    });
    let got = asker.ask(&q);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(answerer.join().unwrap(), "the head never got the question");
    assert!(got.is_ok(), "{got:?}");

    let held = log
        .lock()
        .unwrap()
        .clone()
        .expect("the exchange is recorded");
    assert_eq!(held.question.text, q.text, "the question it answered");
    assert_eq!(held.question.options, q.options);
    assert_eq!(held.answer.option, Some(0));
    assert_eq!(
        held.by, "deadtrickster",
        "the answer is attributed to nobody"
    );

    // **A refusal records nothing.** No head is attached to this hub, so the ask
    // refuses before anybody is posted to — and the slot it left is the empty one.
    let bare = Hub::new("bare");
    let log: AskLog = Default::default();
    let asker = HeadQuestioner::new(bare, Arc::new(Answers::new()))
        .with_budget(Duration::from_secs(1))
        .with_ask_log(log.clone());
    let refused = asker.ask(&q);
    assert!(matches!(refused, Err(AskError::NoHead(_))), "{refused:?}");
    assert!(
        log.lock().unwrap().is_none(),
        "a question nobody answered left a record to attribute to somebody"
    );
}

/// **An abstention travels the whole way and is not silence.** The head sends the
/// fourth shape; the hub accepts it (it is not an empty answer and it conforms);
/// and what comes back is the abstention itself rather than an error, which is what
/// lets the tool report `Abstained` rather than `not_run`.
#[test]
fn an_abstention_is_an_answer_that_reaches_the_tool_as_one() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    let head = hub.attach("tui", "deadtrickster", Caps::default(), 0);
    let budget = Duration::from_secs(10);
    let q = a_question();
    let asker = HeadQuestioner::new(hub.clone(), answers.clone()).with_budget(budget);

    let hub2 = hub.clone();
    let head_id = head.head_id.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let answerer = std::thread::spawn(move || {
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            let open = hub2.snapshot().open_decisions;
            if let Some(d) = open.first() {
                hub2.submit(
                    &head_id,
                    "c1",
                    0,
                    letibot_sessionlog::hub::CommandKind::Answer {
                        req_id: d.req_id.clone(),
                        reply: Reply::Question(
                            letibot_sessionlog::question::QuestionAnswer::abstaining(),
                        ),
                    },
                );
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    });

    let got = asker.ask(&q);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(answerer.join().unwrap(), "the head never got the question");

    let (a, by) = got.expect("an abstention is an answer, not a failure to answer");
    assert!(a.abstain, "{a:?}");
    assert_eq!(by, "deadtrickster");
    // And the log says what they did rather than leaving it as a bare id.
    let settled = hub
        .snapshot()
        .settled_decisions
        .into_iter()
        .next()
        .expect("the abstention is on the log");
    assert!(
        settled.basis.contains("abstained"),
        "the head cannot tell this from an empty answer: {}",
        settled.basis
    );
}

/// **No head, and it refuses by name rather than waiting out a deadline.** This is
/// the behaviour `Headless` had and it is kept: the ask must not hang, and it must
/// not invent an answer. Measured rather than asserted in prose — a refusal that
/// took the budget would pass a test that only looked at the error.
#[test]
fn a_question_with_no_head_refuses_by_name_and_does_not_wait() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    // No `attach`, so nobody can decide: the hub's own answer.
    let budget = Duration::from_secs(30);
    let asker = HeadQuestioner::new(hub.clone(), answers).with_budget(budget);
    let started = std::time::Instant::now();
    let refused = asker.ask(&a_question());
    let took = started.elapsed();
    let err = refused.expect_err("nobody was there to answer");
    assert!(matches!(err, AskError::NoHead(_)), "{err:?}");
    let said = err.to_string();
    assert!(said.contains("NOBODY was asked"), "{said}");
    assert!(said.contains("still open"), "{said}");
    assert!(
        !said.to_lowercase().contains("best judg"),
        "the survey's failure, reproduced: {said}"
    );
    // **A wall-clock bound, kept on purpose, and generous on purpose** (R67 item 19): the
    // fact is "did not sit out the deadline", and the only reading of that is a clock. Half
    // the budget is 15 s of slack over a refusal measured in microseconds — a loaded box
    // does not stall a lock-and-return for 15 s — while a regression that waits trips it
    // every time, because it waits all 30.
    assert!(
        took < budget / 2,
        "a session with nobody to ask sat out the deadline: {took:?}"
    );
    // Nothing was posed, so nothing is left open on a screen nobody has.
    assert!(open_of(&hub).is_empty());
}

/// A head that is attached and cannot decide is the same fact as no head, and it is
/// found out the same way — before the wait, not after it.
#[test]
fn a_question_to_a_head_that_cannot_decide_refuses_at_once() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    // A read-only connector: attached, and not somebody who may answer.
    hub.attach(
        "acp",
        "stranger",
        Caps {
            can_decide: false,
            ..Caps::default()
        },
        0,
    );
    let budget = Duration::from_secs(30);
    let asker = HeadQuestioner::new(hub, answers).with_budget(budget);
    let started = std::time::Instant::now();
    let err = asker.ask(&a_question()).expect_err("nobody may answer");
    assert!(matches!(err, AskError::NoHead(_)), "{err:?}");
    // Half the budget: generous for the reason the sibling test above writes down.
    assert!(
        started.elapsed() < budget / 2,
        "the ask sat out a deadline for an answer nobody was allowed to give"
    );
}

/// **A question nobody answers is `not_run`, and it is not a declining.** The same
/// rule the gate keeps, for the same reason: a deadline is not a decision.
#[test]
fn a_question_nobody_answers_is_unanswered_and_the_card_comes_down() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    let _head = hub.attach("tui", "deadtrickster", Caps::default(), 0);
    // Short, because the point is the ending rather than the patience.
    let asker = HeadQuestioner::new(hub.clone(), answers).with_budget(Duration::from_millis(60));
    let err = asker.ask(&a_question()).expect_err("nobody answered");
    assert!(matches!(err, AskError::Expired(_)), "{err:?}");
    assert!(err.to_string().contains("still open"), "{err}");
    assert!(
        open_of(&hub).is_empty(),
        "an unanswered question was left on the head's screen"
    );
}

/// A read-only head is not somebody who can answer, and finding that out must
/// cost microseconds rather than a deadline. Before the `deciding_heads` check
/// this was a five-minute wait reported as a timeout — *"a real answer given for
/// a fake reason"*.
#[test]
fn a_session_with_nobody_who_can_answer_refuses_at_once() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    hub.attach(
        "flowy",
        "room:general",
        Caps {
            can_decide: false,
            ..Caps::default()
        },
        0,
    );
    let budget = Duration::from_secs(30);
    let adj = HeadAdjudicator::new(hub.clone(), answers).with_budget(budget);
    let started = Instant::now();
    let d = adj.decide(&request());
    // Half the budget, not 5 s (R67 item 19): 15 s of slack over a refusal measured in
    // microseconds, and a regression that waits still trips it, because it waits all 30.
    assert!(started.elapsed() < budget / 2, "it waited");
    assert_eq!(d.outcome, DecisionOutcome::Unavailable);
    assert!(d.basis.contains("nothing was asked"), "{}", d.basis);
    assert!(
        open_of(&hub).is_empty(),
        "nothing was posted, so nothing is open"
    );
}

/// Nobody answers: `Timeout`, which the gate routes to `on_timeout` — `Deny` for
/// a permission — and which becomes `not_run`, never `denied`. And the decision
/// is taken off the screen, or the head keeps offering a prompt for a call that
/// was refused minutes ago.
#[test]
fn a_deadline_that_passes_is_nobody_deciding_and_closes_the_row() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    hub.attach("tui", "alice", Caps::default(), 0);
    let adj = HeadAdjudicator::new(hub.clone(), answers).with_budget(Duration::from_millis(30));
    let d = adj.decide(&request());
    assert_eq!(d.outcome, DecisionOutcome::Timeout, "{d:?}");
    assert!(d.basis.contains("not a denial"), "{}", d.basis);
    assert!(open_of(&hub).is_empty(), "the row is closed either way");
}

/// **The yellow card says why it is yellow.** The ask renders in the chrome,
/// but the call is where the eye already is — and the operator's report was
/// *"some tool calls stay yellow, no idea what that means"*. So posing an ask
/// also publishes a progress note on the call itself, naming the wait and
/// the budget. The call is still `Proposed` at this point — `ToolStarted`
/// fires only after the gate admits — which is exactly why the note has to
/// travel on `ToolProgress`: it is the one event the head applies to an open
/// call of either state.
#[test]
fn an_open_ask_tells_the_card_why_it_waits() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    hub.attach("tui", "alice", Caps::default(), 0);
    let adj = HeadAdjudicator::new(hub.clone(), answers).with_budget(Duration::from_millis(30));
    let _ = adj.decide(&request());
    let note = hub
        .retained()
        .iter()
        .find_map(|e| match &e.event {
            SessionEvent::ToolProgress { note, .. } => Some(note.clone()),
            _ => None,
        })
        .expect("posing an ask published a note on the call");
    assert!(note.contains("waiting for you"), "{note}");
    assert!(note.contains("nothing runs"), "{note}");
}

// ------------------------------------------------- a subagent's card (R58's ask route)

/// The child every one of these tests is about: a real handle, a real task, and the root
/// whose head its card belongs on.
fn child_ask() -> SubagentAsk {
    SubagentAsk {
        handle: "s-sub-3".into(),
        task: "count the rows the store never reads".into(),
        root: "root".into(),
    }
}

/// **A child's ask is the ROOT's card, named, and the answer comes back to the child.**
///
/// The operator's ruling, verbatim: *"who asks subagents permissions? i think they should
/// surface to the parent head all the way to the root obviously"*. A child has no head
/// attached and cannot be driven, so its gate's ask is posted to the session that does
/// have one — and the operator has to be able to tell it from that session's own call,
/// or the card is answered for the wrong thing.
#[test]
fn a_subagents_ask_lands_on_the_roots_card_and_the_answer_comes_back() {
    let root = Hub::new("root");
    let answers = Arc::new(Answers::new());
    root.set_answer_sink(answers.clone());
    let head = root.attach("tui", "deadtrickster", Caps::default(), 0);
    // The child's own session, with NOTHING installed on it: a head could attach there,
    // and no card belongs there.
    let child = Hub::new("s-sub-3");

    let budget = Duration::from_secs(10);
    let adj = SubagentAdjudicator::new(
        child_ask(),
        Some(HeadAdjudicator::new(root.clone(), answers.clone()).with_budget(budget)),
    );
    let mut req = request();
    req.id = "adj-s-sub-3-0001".into();
    req.session_id = "s-sub-3".into();

    // **The head's side, on its own thread** — the same thread the root's own ask is
    // answered on, reading the card off the ROOT and answering through the ROOT's head.
    let root2 = root.clone();
    let child2 = child.clone();
    let head_id = head.head_id.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let answerer = std::thread::spawn(move || {
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            let open = root2.snapshot().open_decisions;
            if let Some(d) = open.first() {
                // **Nothing was posted to the child.** The card belongs to the session
                // whose head exists; a card on a session nothing drives would sit open
                // for the life of the tree.
                assert!(
                    child2.snapshot().open_decisions.is_empty(),
                    "a subagent's ask was posted to the subagent's own session"
                );
                let seen = d.subagent.clone();
                root2.submit(
                    &head_id,
                    "c1",
                    0,
                    letibot_sessionlog::hub::CommandKind::Answer {
                        req_id: d.req_id.clone(),
                        reply: Reply::Permission {
                            option_id: "allow_once".into(),
                            pattern: None,
                            note: None,
                        },
                    },
                );
                return seen;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    });

    let d = adj.decide(&req);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let seen = answerer
        .join()
        .unwrap()
        .expect("the head never got a card to answer: {d:?}");

    // **The card named the CHILD, its task and the session it belongs to** — the three
    // facts that stop it reading as the root's own call.
    assert_eq!(seen, child_ask());
    // **And the child's gate got the operator's own answer**, acting on it exactly as the
    // root would have.
    assert_eq!(
        d.outcome,
        DecisionOutcome::Selected {
            option_id: "allow_once".into()
        },
        "{d:?}"
    );
    assert!(d.by.contains("deadtrickster"), "{}", d.by);
    // Answered on the root's queue, so the row is off the root's screen either way.
    assert!(open_of(&root).is_empty());
    assert!(open_of(&child).is_empty());
}

/// **Nobody at the root to answer: the refusal is a fact, by name.**
///
/// The old sentence was a policy about subagents — *"a subagent has no operator to ask; an
/// ask is denied"* — which said nothing about where the ask went or what the child should
/// do. This one names the child, the session its card belongs to, that no head is attached
/// there, and that the child should stop and report rather than retry.
#[test]
fn a_subagents_ask_with_nobody_at_the_root_refuses_by_name() {
    let root = Hub::new("root");
    let answers = Arc::new(Answers::new());
    root.set_answer_sink(answers.clone());
    // A read-only head is not somebody who can answer — the sibling test above's premise,
    // and here it is the ROOT's state, which is what the child has to be told about.
    root.attach(
        "flowy",
        "room:general",
        Caps {
            can_decide: false,
            ..Caps::default()
        },
        0,
    );
    let child = Hub::new("s-sub-3");
    let budget = Duration::from_secs(30);
    let adj = SubagentAdjudicator::new(
        child_ask(),
        Some(HeadAdjudicator::new(root.clone(), answers).with_budget(budget)),
    );
    let started = Instant::now();
    let d = adj.decide(&request());
    // Half the budget, not 5 s — see `a_session_with_nobody_who_can_answer_refuses_at_once`.
    assert!(started.elapsed() < budget / 2, "it waited");
    assert_eq!(d.outcome, DecisionOutcome::Unavailable);
    assert!(d.by.starts_with("gate:no-head"), "{}", d.by);
    // Which child, which session the card belongs to, that nothing was asked, and what to
    // do about it.
    for want in [
        "s-sub-3",
        "root",
        "nothing was asked",
        "STOP AND REPORT",
        "rather than retry",
    ] {
        assert!(d.basis.contains(want), "{want:?} is not in {:?}", d.basis);
    }
    // **Refused before the post**, so no card is left open on a session nothing will
    // answer — on either queue.
    assert!(open_of(&root).is_empty());
    assert!(open_of(&child).is_empty());
}

/// **A tree with no head-wired adjudicator at all**, which is a different fact from a root
/// whose head is not attached right now — and it is the one the child can do nothing about.
#[test]
fn a_subagent_in_a_tree_with_no_head_adjudicator_refuses_by_name() {
    let adj = SubagentAdjudicator::new(child_ask(), None);
    // **Never `none`.** That prefix is the harness's own signal for *nobody reachable,
    // refuse to open a gated session*, and this adjudicator does have a decider: the
    // ruleset the child inherited. It simply has nobody to ask.
    assert!(!adj.describe().starts_with("none"), "{}", adj.describe());
    assert!(adj.describe().contains("s-sub-3"), "{}", adj.describe());
    assert!(adj.describe().contains("root"), "{}", adj.describe());

    let d = adj.decide(&request());
    assert_eq!(d.outcome, DecisionOutcome::Unavailable);
    assert!(d.by.starts_with("gate:no-head"), "{}", d.by);
    for want in [
        "s-sub-3",
        "root",
        "no head-wired adjudicator",
        "STOP AND REPORT",
        "rather than retry it",
    ] {
        assert!(d.basis.contains(want), "{want:?} is not in {:?}", d.basis);
    }
}

// ------------------------------------------------------------- escalation

/// A stand-in adjudicator that answers one of four ways, and counts how many
/// times it was asked — the counter is what distinguishes *decided* from
/// *consulted on the way past*. The name is what the decision's `by`
/// carries, so a test can tell which side answered.
struct Stub {
    kind: StubKind,
    name: &'static str,
    asked: std::sync::Mutex<usize>,
}

enum StubKind {
    /// The oracle's timeout: nobody decided.
    TimesOut,
    /// The oracle is down: also nobody decided.
    Unavailable,
    /// Somebody stopped the turn.
    Cancels,
    /// A verdict: the first option, allowed.
    Allows,
}

impl Stub {
    fn new(kind: StubKind, name: &'static str) -> Arc<Stub> {
        Arc::new(Stub {
            kind,
            name,
            asked: std::sync::Mutex::new(0),
        })
    }
    fn asks(&self) -> usize {
        *self.asked.lock().unwrap()
    }
}

impl Adjudicator for Stub {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        *self.asked.lock().unwrap() += 1;
        let (outcome, basis) = match self.kind {
            StubKind::TimesOut => (DecisionOutcome::Timeout, "the guard did not answer in time"),
            StubKind::Unavailable => (DecisionOutcome::Unavailable, "the guard is not reachable"),
            StubKind::Cancels => (DecisionOutcome::Cancelled, "the turn was stopped"),
            StubKind::Allows => (
                DecisionOutcome::Selected {
                    option_id: "allow_once".into(),
                },
                "the person allowed it",
            ),
        };
        AdjudicationDecision {
            request_id: req.id.clone(),
            outcome,
            by: self.name.into(),
            basis: basis.into(),
            latency_ms: 1,
        }
    }
    fn describe(&self) -> String {
        self.name.into()
    }
}

/// The operator's rule: **an oracle timeout is a hand-off, not a refusal.**
/// The model cannot answer, so the person is asked — and the record keeps
/// both facts: the person's decision, and the reason they were asked. A row
/// that showed only the first would read as a person overruling a model
/// that never spoke.
#[test]
fn an_oracle_timeout_asks_the_person_instead_of_refusing() {
    let model = Stub::new(StubKind::TimesOut, "stub:model");
    let person = Stub::new(StubKind::Allows, "stub:person");
    let esc = EscalateOnTimeout::new(model.clone(), person.clone());
    let d = esc.decide(&request());
    assert_eq!(model.asks(), 1);
    assert_eq!(person.asks(), 1, "the person was asked");
    assert_eq!(
        d.outcome,
        DecisionOutcome::Selected {
            option_id: "allow_once".into()
        },
        "{d:?}"
    );
    assert!(d.by.contains("person"), "{}", d.by);
    assert!(d.basis.contains("the person allowed it"), "{}", d.basis);
    assert!(
        d.basis.contains("the guard did not answer in time"),
        "the record keeps why the person was asked: {}",
        d.basis
    );
}

/// An oracle that is *down* is the same nobody-decided as one that is slow,
/// and escalates the same way.
#[test]
fn an_oracle_that_cannot_answer_asks_too() {
    let model = Stub::new(StubKind::Unavailable, "stub:model");
    let person = Stub::new(StubKind::Allows, "stub:person");
    let esc = EscalateOnTimeout::new(model.clone(), person.clone());
    let d = esc.decide(&request());
    assert_eq!(person.asks(), 1, "the person was asked");
    assert!(
        matches!(d.outcome, DecisionOutcome::Selected { .. }),
        "{d:?}"
    );
}

/// A model that answers decides, and nobody else is asked.
#[test]
fn a_model_that_answers_decides_and_nobody_else_is_asked() {
    let model = Stub::new(StubKind::Allows, "stub:model");
    let person = Stub::new(StubKind::Allows, "stub:person");
    let esc = EscalateOnTimeout::new(model.clone(), person.clone());
    let d = esc.decide(&request());
    assert_eq!(model.asks(), 1);
    assert_eq!(person.asks(), 0, "the person was never asked");
    assert!(d.by.contains("model"), "{}", d.by);
}

/// A cancellation is somebody stopping the turn, and asking over them would
/// be input arriving from nowhere — the same rule the sink itself keeps.
#[test]
fn a_cancelled_decision_is_not_overridden_by_an_ask() {
    let model = Stub::new(StubKind::Cancels, "stub:model");
    let person = Stub::new(StubKind::Allows, "stub:person");
    let esc = EscalateOnTimeout::new(model.clone(), person.clone());
    let d = esc.decide(&request());
    assert_eq!(person.asks(), 0, "the person was never asked");
    assert_eq!(d.outcome, DecisionOutcome::Cancelled, "{d:?}");
}

/// Esc-Esc while a decision is open. The interrupt does not reach the worker —
/// the worker is the thread waiting — so it reaches the sink, and the wait ends
/// as a cancellation rather than sitting out five minutes.
#[test]
fn an_interrupt_ends_a_wait_the_worker_could_not_have_drained() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    let head = hub.attach("tui", "alice", Caps::default(), 0);
    let budget = Duration::from_secs(60);
    let adj = HeadAdjudicator::new(hub.clone(), answers).with_budget(budget);

    let hub2 = hub.clone();
    let head_id = head.head_id.clone();
    // The same rule as the answerer above: no deadline of its own. It stops
    // when the adjudicator it is racing has returned, and the assertions
    // below are what fail if it never got its chance.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let interrupter = std::thread::spawn(move || {
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            if !hub2.snapshot().open_decisions.is_empty() {
                hub2.submit(
                    &head_id,
                    "c1",
                    0,
                    letibot_sessionlog::hub::CommandKind::Interrupt {
                        reason: "operator pressed esc twice".into(),
                    },
                );
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    });

    let started = Instant::now();
    let d = adj.decide(&request());
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let interrupted = interrupter.join().unwrap();
    assert!(interrupted, "nothing was ever open to interrupt: {d:?}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "it waited out the budget"
    );
    assert_eq!(d.outcome, DecisionOutcome::Cancelled, "{d:?}");
    assert!(d.basis.contains("esc twice"), "{}", d.basis);
}

/// An answer for a request nobody is waiting on is dropped, not held. A late
/// allow applied to a call the gate has already reported as `not_run` would be
/// the worst outcome available.
#[test]
fn an_answer_with_nothing_waiting_settles_nothing() {
    let answers = Answers::new();
    assert!(!answers.answer(
        "adj-nope",
        "alice",
        &Reply::Permission {
            option_id: "allow_once".into(),
            pattern: None,
            note: None
        }
    ));
}

/// The corpus wants the bytes a decider was **shown**, verbatim, not a later
/// reconstruction — §4c. That rule is about the model and it is not only about
/// the model: an operator's override is a labelled example of the same shape.
#[test]
fn what_the_person_was_shown_is_recorded_verbatim() {
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    hub.attach("tui", "alice", Caps::default(), 0);
    let adj = HeadAdjudicator::new(hub.clone(), answers).with_budget(Duration::from_millis(20));
    assert!(adj.last_brief().is_none(), "nothing has been shown yet");
    let req = request();
    adj.decide(&req);
    assert_eq!(adj.last_brief().as_deref(), Some(req.brief().as_str()));
}

/// **The decider is read, not guessed.** `close` used to derive the kind from
/// the OUTCOME — every `Selected` was `human` whoever chose it — and put the
/// whole `human:dead` into `identity` besides, so a head drew `by human
/// human:dead` and an oracle's own admission drew `by human` over a model id.
#[test]
fn a_decider_is_split_into_its_kind_and_its_identity() {
    for (by, kind, identity) in [
        ("human:dead", "human", "dead"),
        ("model:qwen-3.8-27b", "model", "qwen-3.8-27b"),
        ("boundary:flow", "boundary", "flow"),
        ("gate:timeout", "gate", "timeout"),
    ] {
        let d = split_decider(by);
        assert_eq!(d.kind, kind, "{by}");
        assert_eq!(d.identity, identity, "{by}");
    }
    // No colon: all kind, no identity — and the head renders the kind alone
    // rather than a name that is not there.
    let d = split_decider("subagent");
    assert_eq!(d.kind, "subagent");
    assert!(d.identity.is_empty());
}

/// An oracle that decided alone is reported as a model, not as a person. The
/// outcome is `Selected` either way, which is exactly why the outcome cannot
/// be what names the decider.
#[test]
fn an_oracle_that_selected_is_not_reported_as_a_human() {
    let req = request();
    let d = AdjudicationDecision {
        request_id: req.id.clone(),
        outcome: DecisionOutcome::Selected {
            option_id: "allow_once".into(),
        },
        by: "model:glm-5.3-flash".into(),
        basis: "the operator asked for this in the same turn".into(),
        latency_ms: 40,
    };
    match close(&req, &d) {
        SessionEvent::DecisionAnswered { by, .. } => {
            assert_eq!(by.kind, "model");
            assert_eq!(by.identity, "glm-5.3-flash");
        }
        other => panic!("{other:?}"),
    }
}

/// **An answer that arrives before the wait starts is not lost.**
///
/// `ask` creates the slot, publishes the question, and only then calls
/// `wait`. A head answering inside that window sets `Slot::Answered`, and
/// `wait` used to `insert` `Waiting` straight back over it — so the answer
/// vanished and the call sat out its whole budget before reporting that
/// nobody had decided. This is that window, made deterministic.
#[test]
fn an_answer_that_lands_before_the_wait_begins_is_still_the_answer() {
    let answers = Answers::new();
    // What `ask` does before it publishes.
    answers.lock().insert("adj-race".to_string(), Slot::Waiting);
    // The head, faster than the thread that is about to wait.
    assert!(answers.answer(
        "adj-race",
        "deadtrickster",
        &Reply::Permission {
            option_id: "allow_once".into(),
            pattern: None,
            note: None
        },
    ));
    // And now the wait begins. It must find the answer, not overwrite it.
    //
    // **No clock** (R67 item 19; this was `elapsed() < 1s`, which a loaded box trips with
    // the code right). Nobody else can answer `adj-race`, so `Answered` here can only be
    // the answer placed above, read without waiting; a wait that overwrote it would find
    // nothing to wake it and return `TimedOut`. The match is the whole claim.
    match answers.wait("adj-race", Duration::from_secs(10)) {
        Waited::Answered { reply, by } => {
            assert_eq!(by, "deadtrickster");
            assert!(matches!(
                reply,
                Reply::Permission { ref option_id, .. } if option_id == "allow_once"
            ));
        }
        Waited::TimedOut => panic!("the answer was overwritten and the wait timed out"),
        Waited::Cancelled { why } => panic!("cancelled instead of answered: {why}"),
    }
}

// ---- a question's default, taken at the deadline and written down ---------------------

/// A directory of its own per call, removed when the test ends.
struct Dir(std::path::PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(tag: &str) -> Dir {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-auto-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Dir(d)
}

fn git_in(d: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(d)
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn with_default() -> Question {
    Question {
        default: Some(1),
        timeout: Some(Duration::from_millis(60)),
        ..a_question()
    }
}

/// **In a repository the default is logged in `AUTO-DECISIONS.md`** — the operator: *"stating
/// repo commit, select options and default answer"* — and the sentence names the file too.
#[test]
fn a_default_taken_in_a_repository_is_logged_with_its_commit() {
    let d = dir("repo");
    git_in(&d.0, &["init", "-q"]);
    git_in(&d.0, &["config", "user.email", "t@t"]);
    git_in(&d.0, &["config", "user.name", "t"]);
    git_in(&d.0, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&d.0)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .unwrap();
    let commit = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let said = auto_decision(&d.0, "s-1", &with_default(), Duration::from_secs(300), 0);
    let file = std::fs::read_to_string(d.0.join("AUTO-DECISIONS.md")).expect("the log");
    for needle in [
        commit.as_str(),
        "which database should the migration target?",
        "  - postgres\n",
        "  - sqlite ← taken",
        "default taken: sqlite",
        "s-1",
    ] {
        assert!(file.contains(needle), "{needle:?} missing from:\n{file}");
    }
    assert!(said.contains("AUTO-DECISIONS.md"), "{said}");
    assert!(said.contains(&commit), "{said}");
    // Never committed: the repository's `.gitignore` names it, so git sees only that line.
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&d.0)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        "?? .gitignore\n",
        "the log is ignored"
    );

    // A second one appends, under the one heading.
    auto_decision(&d.0, "s-1", &with_default(), Duration::from_secs(300), 1);
    let file = std::fs::read_to_string(d.0.join("AUTO-DECISIONS.md")).unwrap();
    assert_eq!(file.matches("# Auto-decisions").count(), 1, "{file}");
    assert_eq!(file.matches("default taken:").count(), 2, "{file}");
    let ignore = std::fs::read_to_string(d.0.join(".gitignore")).unwrap();
    assert_eq!(ignore, "/AUTO-DECISIONS.md\n", "added once");
}

/// **Outside a repository there is no file** — the session above several repositories — and
/// the sentence is the record.
#[test]
fn a_default_taken_outside_a_repository_is_a_note_and_no_file() {
    let d = dir("norepo");
    let said = auto_decision(&d.0, "s-1", &with_default(), Duration::from_secs(300), 0);
    assert!(!d.0.join("AUTO-DECISIONS.md").exists());
    assert!(said.contains("\"sqlite\" was taken"), "{said}");
    assert!(said.contains("not a repository"), "{said}");
}

/// **End to end at the daemon**: the card shows the default, the deadline passes, the
/// questioner says it expired (not cancelled), and taking the default publishes the note.
#[test]
fn a_question_with_a_default_expires_and_its_default_is_noted() {
    let d = dir("e2e");
    let hub = Hub::new("s");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    let _head = hub.attach("tui", "deadtrickster", Caps::default(), 0);
    let asker = HeadQuestioner::new(hub.clone(), answers).with_workspace(d.0.clone());
    let q = with_default();
    let err = asker.ask(&q).expect_err("nobody answered");
    assert!(matches!(err, AskError::Expired(_)), "{err:?}");
    let said = asker
        .defaulted(&q, Duration::from_secs(60))
        .expect("a workspace to log in");
    assert!(said.contains("sqlite"), "{said}");
    let w = hub.snapshot().warnings;
    assert!(
        w.iter()
            .any(|w| w.code == "auto_decision" && w.detail == said),
        "{w:?}"
    );
}

/// The card says which choice is the default and what the deadline does.
#[test]
fn a_question_card_names_its_default() {
    let e = pose_question("q1", &with_default(), 0);
    let SessionEvent::DecisionRequested {
        choices, because, ..
    } = e
    else {
        panic!("{e:?}")
    };
    assert_eq!(
        choices,
        vec!["postgres".to_string(), "sqlite (default)".into()]
    );
    assert!(because.contains("the default is taken"), "{because}");
}
