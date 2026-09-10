//! Intent: the list, the goal, plan mode, and asking a person.
//!
//! The five tools here are the ones that are about **what the turn means to do**
//! rather than about the tree. They exist together because they share one
//! mechanism, and that mechanism is the reason the group is worth building:
//!
//! > A harness that only *enforces* forces the model to be locked down. A harness
//! > that *measures* lets it be loosened, because the miss is caught. **Feedback
//! > is what buys latitude.** — `docs/closed-loop.md` §6
//!
//! # The error signal, which is the half everybody is missing
//!
//! `docs/closed-loop.md` §2 splits "guardrails" into four mechanisms and says the
//! first two are the ones that were absent. This module is those two:
//!
//! | stepper | here |
//! |---|---|
//! | **encoder** — measure the actual effect | [`ledger::IntentSink`], a decorator on the runtime's own event sink. Only `Ok` counts as an effect; an abstention is an *attempt* |
//! | **error signal** — diff intent against effect | [`ledger::IntentLedger::reconcile`], at the turn boundary |
//! | **stall fault** — fail closed, never reroute | [`queue::NoMount`], [`ask::Headless`], both `not_run` |
//! | **current limit** — the capability is not reachable | [`plan::seating`]: in plan mode the write tools are not in the tool list at all |
//!
//! T21.3 — *"when model says ill start that and by end of the turn forgets and
//! does not start anything"* — is the error signal's acceptance case, and none of
//! the five surveyed harnesses has anything that could catch it. Their todo lists
//! are notes the model writes to itself; nothing reads them back.
//!
//! # What is wired, and what is left to wire
//!
//! Everything in this module is a function of a call and a ledger, so it is all
//! testable without a model, a GPU or a network — `docs/closed-loop.md` §10 calls
//! that Class 1, *"the cheapest work in this document"*.
//!
//! The one thing that is **not** here is the caller at the turn boundary. It is
//! three lines and it needs nothing new:
//!
//! ```text
//!   let rec = ledger.reconcile(&turn.turn_id, assistant_text_of(&turn.items));
//!   if let Some(text) = rec.steering() {
//!       steering_tx.send(SteeringMessage::normal(text));   // §5.8, already built
//!   }
//! ```
//!
//! `letibot_turn::steering::SteeringSource` and `ChannelSteering` already exist and
//! already inject at a step boundary, which is exactly what T21.3 asks for
//! (*"append 'you said you would X — do it or say why not' and continue"*). What
//! is missing is only that nobody calls it yet, and the session loop that would is
//! under concurrent rewrite this session. See the crate report; this module does
//! **not** fake the comparison in the meantime — an unwired encoder is a declared
//! state ([`ledger::Verification::NoEncoder`]) rather than a silent zero.

pub mod ask;
pub mod board;
pub mod chat;
pub mod ledger;
pub mod plan;
pub mod queue;
pub mod tools;
pub mod write_plan;

pub use ask::{
    AskError, AskUserQuestion, Headless, Question, QuestionAnswer, Questioner,
};
pub use board::Todo;
pub use chat::{Chat, ChatError, Message, NoFabric, Posted, Say};
pub use ledger::{
    Board, Effect, Finding, Goal, Intent, IntentLedger, IntentSink, LedgerError, Reconciliation,
    Source, Status, Verification, commitments, is_effect,
};
pub use plan::{PLAN_MODE_TOOLS, PlanGate, PlanMode, PlanState, seating};
pub use queue::{NewRow, NoMount, Queue, QueueError, Row};
pub use tools::{EnterPlanMode, ExitPlanMode, GoalTool};
pub use write_plan::{PLAN_DIR, PLAN_EXT, WritePlan};

use crate::runtime::{RegisterError, Registry};

/// **The turn-boundary hook, in one call.**
///
/// Give it the turn that just finished and it returns the steering text, or
/// `None` when there is nothing to say — which is the common case and is meant to
/// be.
///
/// ```ignore
/// if let Some(text) = intent::close_the_turn(&ledger, &turn.turn_id, &turn.items) {
///     steering.send(SteeringMessage::normal(text));   // §5.8, already built
/// }
/// ```
///
/// `letibot_turn::steering` already injects a message at the next step boundary,
/// which is exactly the response T21.3 asks for — *"append 'you said you would X —
/// do it or say why not' and continue"*. This function is the other half; what is
/// missing is only the call site, and it is missing because the session loop that
/// owns turn boundaries is under concurrent rewrite. See this module's docs.
///
/// It reads the assistant's own text out of the turn for the prose heuristic and
/// nothing else: the tool calls are already in the ledger, put there by the
/// encoder as they ran.
pub fn close_the_turn(
    ledger: &IntentLedger,
    turn_id: &str,
    items: &[letibot_transcript::TranscriptItem],
) -> Option<String> {
    let said: String = items
        .iter()
        .filter_map(|i| match i {
            letibot_transcript::TranscriptItem::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    ledger.reconcile(turn_id, &said).steering()
}

/// Everything this module needs from the session it is registered into.
///
/// A struct rather than five arguments because two of the five are mounts whose
/// `None` is meaningful, and a positional `None` at a call site is the kind of thing
/// that gets passed in the wrong slot exactly once.
pub struct Wiring {
    pub ledger: std::sync::Arc<IntentLedger>,
    pub plan: std::sync::Arc<PlanMode>,
    pub questioner: std::sync::Arc<dyn Questioner>,
    /// The shared board. `None` is the default and not an error state: the list is
    /// this session's own, needs no node, no token and no network, and every result
    /// says so.
    pub queue: Option<std::sync::Arc<dyn Queue>>,
    /// The fabric's rooms, for `say`. `None` is likewise a configuration.
    pub chat: Option<std::sync::Arc<dyn Chat>>,
}

impl Wiring {
    /// A session with no fabric at all: no board, no rooms, no head. Everything
    /// local works; everything shared refuses with `not_run` naming what is
    /// missing.
    pub fn standalone() -> Self {
        Wiring {
            ledger: std::sync::Arc::new(IntentLedger::new()),
            plan: std::sync::Arc::new(PlanMode::new()),
            questioner: std::sync::Arc::new(Headless),
            queue: None,
            chat: None,
        }
    }

    /// The lines a startup banner wants. Same shape `harnessd`'s `Disclosure`
    /// already consumes for `retrieval`, so wiring it is a `push` per row.
    pub fn disclosures(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "board",
                self.queue
                    .as_ref()
                    .map(|q| q.describe())
                    .unwrap_or_else(|| NoMount.describe()),
            ),
            (
                "fabric",
                self.chat
                    .as_ref()
                    .map(|c| c.describe())
                    .unwrap_or_else(|| NoFabric.describe()),
            ),
            ("questions", self.questioner.describe()),
            (
                "intent-encoder",
                if self.ledger.encoder() {
                    "attached — completions are checked against what ran".into()
                } else {
                    "NOT attached — completions cannot be checked either way; wire an \
                     IntentSink"
                        .into()
                },
            ),
        ]
    }
}

/// Register the seven intent tools into a registry.
///
/// A function rather than a `Vec` so that the *order* is written down once: order
/// is part of the stable prefix and reordering it re-prefills the conversation.
///
/// `write_plan` and `say` are registered whether or not plan mode is ever entered:
/// [`plan::seating`] decides what is *seated*, and a tool that had to be registered
/// on entering plan mode would make the registry itself mode-dependent.
pub fn register_into(reg: &mut Registry, w: &Wiring) -> Result<(), RegisterError> {
    reg.register(Box::new(match &w.queue {
        Some(q) => Todo::mounted(w.ledger.clone(), q.clone()),
        None => Todo::local(w.ledger.clone()),
    }))?;
    reg.register(Box::new(GoalTool::new(w.ledger.clone())))?;
    reg.register(Box::new(EnterPlanMode::new(w.plan.clone())))?;
    reg.register(Box::new(ExitPlanMode::new(
        w.plan.clone(),
        w.ledger.clone(),
    )))?;
    reg.register(Box::new(WritePlan))?;
    reg.register(Box::new(Say::new(
        w.chat
            .clone()
            .unwrap_or_else(|| std::sync::Arc::new(NoFabric)),
    )))?;
    reg.register(Box::new(AskUserQuestion::new(w.questioner.clone())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::roles;
    use crate::schema::Access;
    use std::sync::Arc;

    fn reg(mounted: bool) -> Registry {
        let mut r =
            crate::coder_tools(Arc::new(crate::builtins::retrieval::Unavailable)).unwrap();
        let mut w = Wiring::standalone();
        if mounted {
            w.queue = Some(Arc::new(queue::FakeQueue::new("seat")));
            w.chat = Some(Arc::new(chat::FakeChat::new("seat", &["general"])));
        }
        register_into(&mut r, &w).unwrap();
        r
    }

    #[test]
    fn every_intent_tool_declares_its_access_honestly() {
        let r = reg(false);
        let by = |n: &str| r.schemas().into_iter().find(|s| s.name == n).unwrap().access;
        // Session state, not the operator's tree, and not `Read` — which is the
        // hole `docs/tool-survey.md` §1.4 found in grok-build.
        assert_eq!(by("todo"), Access::Session);
        assert_eq!(by("goal"), Access::Session);
        assert_eq!(by("enter_plan_mode"), Access::Session);
        assert_eq!(by("ask_user_question"), Access::Session);
        // Leaving plan mode restores the ability to write.
        assert_eq!(by("exit_plan_mode"), Access::Write);
        // D9's two: a scoped write is still a write, and a room is off the box.
        // Scope is what makes them cheap to approve, not a smaller class.
        assert_eq!(by("write_plan"), Access::Write);
        assert_eq!(by("say"), Access::Network);
    }

    #[test]
    fn plan_mode_keeps_the_plan_and_drops_the_work() {
        // D9: plan mode is no writes to the WORK.
        let r = reg(false);
        let base = crate::runtime::Role::new(
            "coder",
            &["read", "write", "edit", "todo", "exit_plan_mode"],
        );
        let s = seating(&r, &base, true);
        assert!(!s.tools.contains(&"write".to_string()), "{:?}", s.tools);
        assert!(!s.tools.contains(&"edit".to_string()), "{:?}", s.tools);
        // …and gains the two that make a plan a plan, even though the base role
        // never named them.
        assert!(s.tools.contains(&"write_plan".to_string()), "{:?}", s.tools);
        assert!(s.tools.contains(&"say".to_string()), "{:?}", s.tools);
        let seated = r.resolve_role(&s).expect("the plan role resolves");
        let json = seated.tools_json().join("\n");
        assert!(!json.contains("\"name\":\"write\""), "{json}");
        assert!(json.contains("\"name\":\"write_plan\""), "{json}");
    }

    #[test]
    fn a_plan_mode_tool_the_registry_lacks_is_not_invented() {
        // A session with no fabric still registers `say` (it refuses at the seam),
        // but a build that dropped the tool entirely must not get a role naming it —
        // that role would fail to resolve and a session that merely lacks chat
        // should still be able to plan.
        let mut bare =
            crate::coder_tools(Arc::new(crate::builtins::retrieval::Unavailable)).unwrap();
        bare.register(Box::new(WritePlan)).unwrap();
        let base = crate::runtime::Role::new("coder", &["read", "write"]);
        let s = seating(&bare, &base, true);
        assert!(s.tools.contains(&"write_plan".to_string()), "{:?}", s.tools);
        assert!(!s.tools.contains(&"say".to_string()), "{:?}", s.tools);
        bare.resolve_role(&s).expect("resolves without `say`");
    }

    #[test]
    fn the_disclosures_name_every_absence_as_a_configuration() {
        let w = Wiring::standalone();
        let d = w.disclosures();
        let by = |s: &str| {
            d.iter()
                .find(|(k, _)| *k == s)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert!(by("board").starts_with("not mounted"), "{}", by("board"));
        assert!(by("fabric").starts_with("not attached"), "{}", by("fabric"));
        assert!(by("questions").starts_with("none attached"), "{}", by("questions"));
        // And the one that is a defect rather than a configuration says so.
        assert!(by("intent-encoder").contains("NOT attached"), "{}", by("intent-encoder"));
        let w2 = Wiring::standalone();
        let _s = IntentSink::new(w2.ledger.clone(), crate::events::NullToolSink);
        assert!(
            w2.disclosures()
                .iter()
                .any(|(k, v)| *k == "intent-encoder" && v.starts_with("attached")),
        );
    }

    #[test]
    fn mounting_a_board_makes_todo_a_network_tool() {
        let r = reg(true);
        let a = r
            .schemas()
            .into_iter()
            .find(|s| s.name == "todo")
            .unwrap()
            .access;
        assert_eq!(
            a,
            Access::Network,
            "a mounted list writes state other seats act on, and clause 4 says a tool \
             declares the widest thing it can do"
        );
    }

    #[test]
    fn the_planner_role_seats_under_the_ceiling() {
        let r = reg(false);
        let seated = r.resolve_role(&roles::planner()).unwrap();
        assert!(seated.len() <= crate::DEFAULT_MAX_TOOLS, "{}", seated.len());
        assert!(!seated.names().contains(&"write".to_string()));
        assert!(seated.names().contains(&"exit_plan_mode".to_string()));
    }

    #[test]
    fn plan_mode_derives_the_same_answer_from_any_base_role() {
        let r = reg(true);
        let base = crate::runtime::Role::new(
            "custom",
            &["read", "write", "edit", "todo", "exit_plan_mode"],
        );
        let s = seating(&r, &base, true);
        // `todo` is Network here because a board is mounted, so plan mode drops it
        // too. That is the boundary doing its job rather than a special case:
        // filing a row on the shared queue changes the world outside the session.
        // What is left is the base's read tools plus D9's three.
        assert_eq!(
            s.tools,
            vec!["read", "exit_plan_mode", "write_plan", "say"],
            "{:?}",
            s.tools
        );
    }

    #[test]
    fn every_description_lints_clean_and_stays_under_the_budget() {
        let r = reg(false);
        for s in r.schemas() {
            assert_eq!(crate::schema::lint_description(&s.description), vec![], "{}", s.name);
            assert!(
                !s.description.is_empty() && s.description.len() < 800,
                "{} is {} bytes",
                s.name,
                s.description.len()
            );
        }
    }

    #[test]
    fn the_prompt_json_is_stable_across_builds() {
        let a = reg(false).tools_json();
        let b = reg(false).tools_json();
        assert_eq!(a, b);
    }

    #[test]
    fn the_turn_boundary_hook_is_one_call_and_sees_the_turns_own_prose() {
        use letibot_transcript::TranscriptItem;
        let l = IntentLedger::new();
        let _sink = IntentSink::new(
            std::sync::Arc::new(IntentLedger::new()),
            crate::events::NullToolSink,
        );
        // No encoder on `l` itself, so this also asserts the harness-defect
        // finding rides along rather than being reported as the model's.
        let items = vec![TranscriptItem::Assistant {
            text: "I'll restart the service now.".into(),
            tool_calls: vec![],
            truncated: false,
        }];
        let steer = close_the_turn(&l, "turn_1", &items).expect("a commitment with no calls");
        assert!(steer.contains("restart the service"), "{steer}");
        assert!(steer.contains("text heuristic"), "{steer}");
        assert!(steer.contains("no effect log is attached"), "{steer}");
    }

    #[test]
    fn a_turn_that_kept_its_word_is_told_nothing() {
        use letibot_transcript::TranscriptItem;
        let l = std::sync::Arc::new(IntentLedger::new());
        let mut sink = IntentSink::new(l.clone(), crate::events::NullToolSink);
        let id = l.declare("turn_1", "restart it", Source::Todo);
        l.start("turn_1", id).unwrap();
        crate::events::ToolEventSink::emit(
            &mut sink,
            crate::events::ToolEvent::Started {
                turn_id: "turn_1".into(),
                call_id: "c1".into(),
                name: "write".into(),
                access: Access::Write,
            },
        );
        crate::events::ToolEventSink::emit(
            &mut sink,
            crate::events::ToolEvent::Finished {
                turn_id: "turn_1".into(),
                call_id: "c1".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload_digest: "d".into(),
                inline_bytes: 1,
                full_bytes: 1,
                spill: None,
                repairs: 0,
            },
        );
        l.complete("turn_1", id).unwrap();
        let items = vec![TranscriptItem::Assistant {
            text: "I'll restart the service now.".into(),
            tool_calls: vec![],
            truncated: false,
        }];
        assert_eq!(close_the_turn(&l, "turn_1", &items), None);
    }
}
