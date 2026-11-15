//! The pending set reaches a head that attaches after the ask, and an ask settled anywhere comes off
//! every head.

use std::sync::Mutex;

use super::*;
use crate::event::{OnTimeout, SubagentAsk};
use crate::hub::CommandKind;
use crate::protocol::{Caps, ServerFrame};
use crate::question::QuestionAnswer;

const MAIN: &str = "s-main";
const ASKER: &str = "s-gatekeeper-k-sub-1";

fn card(req_id: &str) -> SessionEvent {
    SessionEvent::DecisionRequested {
        req_id: req_id.into(),
        subagent: Some(SubagentAsk {
            handle: ASKER.into(),
            task: "review".into(),
            root: MAIN.into(),
        }),
        kind: "question".into(),
        call_id: None,
        access: String::new(),
        summary: "may I run cargo test?".into(),
        target: String::new(),
        write_targets: Vec::new(),
        detail: String::new(),
        options: Vec::new(),
        edits: Vec::new(),
        options_note: String::new(),
        choices: vec!["yes".into(), "no".into()],
        because: String::new(),
        advice: None,
        deadline: None,
        on_timeout: OnTimeout::Ask,
    }
}

/// Records what reached the asker.
#[derive(Default)]
struct Asker(Mutex<Vec<(String, String)>>);

impl AnswerSink for Asker {
    fn answer(&self, req_id: &str, identity: &str, _reply: &Reply) -> bool {
        self.0
            .lock()
            .unwrap()
            .push((req_id.to_string(), identity.to_string()));
        true
    }
    fn cancel(&self, _why: &str) {}
    fn describe(&self) -> String {
        "the asker".into()
    }
}

fn deciding() -> Caps {
    Caps {
        can_decide: true,
        ..Caps::default()
    }
}

fn open_ids(hub: &Hub) -> Vec<String> {
    hub.snapshot()
        .open_decisions
        .into_iter()
        .map(|d| d.req_id)
        .collect()
}

#[test]
fn a_head_attaching_to_the_main_session_gets_the_pending_asks() {
    let set = PendingAsks::new();
    let first = Hub::new(MAIN);
    set.set_main(&first);
    assert!(set.post(card("ask-1"), Arc::new(Asker::default())));
    assert_eq!(
        set.pending(),
        vec![Pending::new(PendingKind::Question, ASKER, "ask-1")]
    );

    // Posted with nobody attached: the head that comes later finds it in its snapshot.
    let a = first.attach("tui", "operator", deciding(), 0);
    let snap = a.snapshot.expect("a fresh head gets a snapshot");
    assert_eq!(snap.open_decisions.len(), 1);
    assert_eq!(snap.open_decisions[0].req_id, "ask-1");

    // The main session reopened: its new hub shows nothing until a head attaches, and that head
    // is given the ask. Delivery is idempotent.
    let second = Hub::new(MAIN);
    set.lock().main = Some(Arc::downgrade(&second));
    assert!(open_ids(&second).is_empty());
    assert_eq!(set.deliver(&second), 1);
    assert_eq!(set.deliver(&second), 0);
    let b = second.attach("tui", "operator", deciding(), 0);
    assert_eq!(
        b.snapshot.unwrap().open_decisions[0].req_id,
        "ask-1",
        "delivered on attach"
    );

    // A hub that is not the main session is given nothing.
    let other = Hub::new("s-other");
    assert_eq!(set.deliver(&other), 0);
}

#[test]
fn an_ask_answered_elsewhere_is_retracted_from_every_head() {
    let set = PendingAsks::new();
    let main = Hub::new(MAIN);
    set.set_main(&main);
    let one = main.attach("tui", "operator", deciding(), 0).head_id;
    let _two = main.attach("tui", "operator-2", deciding(), 0).head_id;
    let asker = Arc::new(Asker::default());
    set.post(card("ask-1"), asker.clone());
    set.post(card("ask-2"), asker.clone());
    assert_eq!(open_ids(&main), vec!["ask-1", "ask-2"]);

    // Settled where it was asked: the copy comes down with the asker's own close.
    assert!(set.withdraw("ask-1", "answered at the asking session"));
    assert_eq!(open_ids(&main), vec!["ask-2"]);
    assert!(
        main.snapshot()
            .settled_decisions
            .iter()
            .any(|s| s.req_id == "ask-1")
    );

    // Answered from one head of the main session: it reaches the asker and leaves the set, and the
    // asker's close takes the card off the other head.
    let frame = main.submit(
        &one,
        "c1",
        0,
        CommandKind::Answer {
            req_id: "ask-2".into(),
            reply: Reply::Question(QuestionAnswer {
                option: Some(0),
                note: None,
                free: None,
                abstain: false,
            }),
        },
    );
    assert!(matches!(frame, ServerFrame::Accepted { .. }), "{frame:?}");
    assert_eq!(
        *asker.0.lock().unwrap(),
        vec![("ask-2".to_string(), "operator".to_string())]
    );
    assert!(set.ids().is_empty());
    assert!(
        main.withdraw_relayed_with(
            "ask-2",
            SessionEvent::DecisionAnswered {
                req_id: "ask-2".into(),
                outcome: DecisionOutcome::Cancelled,
                by: Decider {
                    kind: "human".into(),
                    identity: "operator".into(),
                },
                basis: "yes".into(),
                late: false,
            },
        ),
        "a card answered here, whose sink is gone, is still closed"
    );
    assert!(open_ids(&main).is_empty());
    assert!(!set.withdraw("ask-2", "again"), "nothing left to retract");
}
