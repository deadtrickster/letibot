//! `goal`, `enter_plan_mode` and `exit_plan_mode`.
//!
//! The three tools that are about the *shape* of a turn rather than its list.
//!
//! # `goal`
//!
//! Three of the five surveyed harnesses track a goal (`goal`, `update_goal`,
//! `create_/get_/update_goal`). All three take a sentence and store it. Ours takes
//! a sentence **and the criterion that would settle it**, and refuses without one,
//! because a goal nobody can check is a wish and `docs/closed-loop.md` §8 is
//! explicit that the harness *"can force the form — demand a baseline, demand the
//! unverified list, refuse a conclusion citing no measurement — but not supply the
//! answer."* Demanding the form is exactly what this does.
//!
//! Marking it met goes through the same check a todo item does: at least one tool
//! call must have succeeded since the goal was set, or it is recorded as claimed
//! and the call says so.
//!
//! # Plan mode
//!
//! See [`super::plan`] for why it is a seating decision rather than a flag. The two
//! tools here are the doors; the boundary is in that module.

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::ledger::{IntentLedger, Source, Verification};
use super::plan::PlanMode;

// ---------------------------------------------------------------------------
// goal
// ---------------------------------------------------------------------------

pub struct GoalTool {
    pub ledger: std::sync::Arc<IntentLedger>,
}

impl GoalTool {
    pub fn new(ledger: std::sync::Arc<IntentLedger>) -> Self {
        GoalTool { ledger }
    }
}

impl Tool for GoalTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "goal",
            "Record what this session is for, and what would settle it. `op` is set, \
             get or met. `set` needs `goal` and `acceptance` — the observable that \
             would show the goal was reached; it is refused without one, because a \
             goal nobody can check cannot be finished, only abandoned. `met` is \
             checked the same way a todo item is: with no successful tool call \
             behind it, it is recorded as claimed rather than met.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "op": {"type": "string", "enum": ["set", "get", "met"]},
                    "goal": {"type": "string", "description": "For set: what this session is for, in one line."},
                    "acceptance": {"type": "string", "description": "For set: what would show it was reached — a command that passes, a file that exists, a number that moves. Not a feeling."}
                },
                "required": ["op"]
            }),
            Access::Session,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let turn = ctx.turn_id().to_string();
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("get");
        let board = format!("\n{}", self.ledger.board());

        match op {
            "set" => {
                let goal = args.get("goal").and_then(|v| v.as_str()).unwrap_or("");
                let acceptance = args
                    .get("acceptance")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if goal.trim().is_empty() || acceptance.trim().is_empty() {
                    let missing = if goal.trim().is_empty() {
                        "goal"
                    } else {
                        "acceptance"
                    };
                    return Invocation::failed(
                        format!("goal set needs `{missing}`"),
                        format!(
                            "call `goal` again with `op: \"set\"`, `goal` set to what this \
                             session is for, and `acceptance` set to the observable that \
                             would show it was reached — a command that passes, a file \
                             that exists, a number that moves. A goal with no acceptance \
                             criterion cannot be finished, only abandoned.{board}"
                        ),
                    );
                }
                let g = self.ledger.set_goal(&turn, goal, acceptance);
                Invocation::ok(format!(
                    "goal: {}\nsettled when: {}{board}",
                    g.text, g.acceptance
                ))
            }
            "get" => match self.ledger.goal() {
                Some(g) => {
                    let mut body = format!("goal: {}\nsettled when: {}\n", g.text, g.acceptance);
                    match &g.met {
                        Some(v) => body.push_str(&format!("status: {}\n", v.as_str())),
                        None => body.push_str("status: not yet met\n"),
                    }
                    Invocation::ok(format!("{body}{board}"))
                }
                None => Invocation::abstained(
                    "no goal has been set in this session",
                    format!(
                        "nothing has been recorded as this session's goal. Set one with \
                         `op: \"set\"`, `goal` and `acceptance`.{board}"
                    ),
                ),
            },
            "met" => match self.ledger.meet_goal(&turn) {
                Err(e) => Invocation::failed("there is no goal to mark met", format!("{e}{board}")),
                Ok(g) => {
                    let v = g.met.clone().unwrap_or(Verification::NoEffect);
                    let mut inv = Invocation::ok(format!(
                        "goal: {}\nsettled when: {}\nstatus: {}{board}",
                        g.text,
                        g.acceptance,
                        v.as_str()
                    ));
                    if !v.is_complete() {
                        inv = inv.with_note(match v {
                            Verification::NoEncoder => {
                                "the goal is NOT counted as met: this session has no effect \
                                 log attached, so nothing could check it either way. That \
                                 is a harness defect, not yours."
                                    .to_string()
                            }
                            _ => format!(
                                "the goal is NOT counted as met: no tool call succeeded \
                                 since it was set, so nothing shows `{}` was reached. Run \
                                 the check and say what it returned.",
                                g.acceptance
                            ),
                        });
                    }
                    inv
                }
            },
            other => Invocation::failed(
                format!("`{other}` is not a goal op"),
                format!("`goal` takes one of: set, get, met.{board}"),
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// plan mode
// ---------------------------------------------------------------------------

pub struct EnterPlanMode {
    pub plan: std::sync::Arc<PlanMode>,
}

impl EnterPlanMode {
    pub fn new(plan: std::sync::Arc<PlanMode>) -> Self {
        EnterPlanMode { plan }
    }
}

impl Tool for EnterPlanMode {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "enter_plan_mode",
            "Stop being able to change anything, and work out what to do first. From \
             the next turn the write tools are not seated: they are absent from your \
             tool list, not merely refused, so there is nothing to call. Reading, \
             searching and this list still work. Leaving is `exit_plan_mode`, which \
             takes the plan and is adjudicated — say so in your plan, because a \
             session with nobody to adjudicate cannot leave.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "why": {"type": "string", "description": "One line: what you want to work out before changing anything."}
                }
            }),
            // Narrowing what the session can do needs no permission. Widening it
            // again does, which is `exit_plan_mode`.
            Access::Session,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let was = self.plan.active();
        let s = self.plan.enter(ctx.turn_id(), ctx.call_id());
        let why = args.get("why").and_then(|v| v.as_str()).unwrap_or("");
        let mut body = String::from("plan mode is on.\n");
        if !why.is_empty() {
            body.push_str(&format!("to work out: {why}\n"));
        }
        body.push_str(
            "From the next turn the write tools are not in your tool list. Read, search \
             and the list still are.\n",
        );
        let mut inv = Invocation::ok(body);
        if was {
            inv = inv.with_note(format!(
                "plan mode was already on, entered by call {} — this call changed nothing",
                s.entered_by.as_deref().unwrap_or("(unknown)")
            ));
        }
        // Clause 1 applied to a door you can only walk through once: say what
        // leaving will cost BEFORE it is the only way out.
        inv.with_note(
            "leaving is a gated call: `exit_plan_mode` declares write access, so it goes \
             to whoever adjudicates this session. If nobody is attached it refuses with \
             not_run and the operator has to reseat the session — plan the work assuming \
             you may have to hand the plan over rather than run it."
                .to_string(),
        )
    }
}

pub struct ExitPlanMode {
    pub plan: std::sync::Arc<PlanMode>,
    pub ledger: std::sync::Arc<IntentLedger>,
}

impl ExitPlanMode {
    pub fn new(plan: std::sync::Arc<PlanMode>, ledger: std::sync::Arc<IntentLedger>) -> Self {
        ExitPlanMode { plan, ledger }
    }
}

/// The most plan steps that become intents. A plan longer than this is not a plan.
const MAX_STEPS: usize = 20;

impl Tool for ExitPlanMode {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "exit_plan_mode",
            "Leave plan mode with the plan you are leaving with. `plan` is required, \
             and `steps` — one line each, in order — is what you are committing to do. \
             Each step becomes an item on the list, so the same check that applies to \
             anything else applies to the plan: a step marked done with nothing \
             measured behind it is recorded as claimed. This call is adjudicated; a \
             session with nobody to decide will refuse it and say so.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": {"type": "string", "description": "The plan, in prose. What you worked out and why."},
                    "steps": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "The steps you are committing to, one line each, in order."
                    }
                },
                "required": ["plan"]
            }),
            // Leaving plan mode restores the ability to write, and widening a
            // capability is the direction that needs a decision.
            Access::Write,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        if !self.plan.active() {
            return Invocation::failed(
                "this session is not in plan mode",
                "there is nothing to leave. `enter_plan_mode` is how you go in.",
            );
        }
        let Some(plan) = args
            .get("plan")
            .and_then(|v| v.as_str())
            .filter(|p| !p.trim().is_empty())
        else {
            return Invocation::failed(
                "exit_plan_mode needs the plan",
                "call `exit_plan_mode` again with `plan` set to what you worked out, and \
                 `steps` set to what you are committing to do. Leaving plan mode with no \
                 plan is leaving with nothing, and the whole point of going in was to \
                 come out with one.",
            );
        };
        let mut steps: Vec<String> = args
            .get("steps")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let over = steps.len().saturating_sub(MAX_STEPS);
        steps.truncate(MAX_STEPS);

        let turn = ctx.turn_id().to_string();
        // **This is where plan mode joins the error signal.** The steps a plan
        // exits with are declared intents, so the turn-boundary diff can say
        // "you committed to six things and did none of them" — which is T21.3
        // over a plan rather than over a sentence.
        let mut ids = Vec::new();
        for s in &steps {
            ids.push(self.ledger.declare_row(&turn, s, Source::Plan, None));
        }
        self.plan.exit(plan);

        let mut body = String::from(
            "plan mode is off; the write tools are seated again \
                                     from the next turn.\n",
        );
        if ids.is_empty() {
            body.push_str(
                "no steps were given, so nothing was put on the list and nothing will be \
                 checked against what runs.\n",
            );
        } else {
            body.push_str(&format!("{} step(s) are now on the list:\n", ids.len()));
            for (id, s) in ids.iter().zip(&steps) {
                body.push_str(&format!("  #{id} {s}\n"));
            }
        }
        body.push_str(&format!("{}\n", self.ledger.board()));

        let mut inv = Invocation::ok(body);
        if over > 0 {
            inv = inv.with_note(format!(
                "{over} step(s) past the first {MAX_STEPS} were not put on the list; a \
                 plan with more steps than that is two plans"
            ));
        }
        if ids.is_empty() {
            inv = inv.with_note(
                "a plan with no `steps` cannot be checked against what runs. Add them \
                 with `todo` if you want the intent check to cover this plan."
                    .to_string(),
            );
        }
        inv
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::intent::ledger::IntentSink;
    use crate::events::NullToolSink;
    use crate::result::ToolResult;
    use letibot_transcript::ToolOutcome;
    use std::sync::Arc;

    fn goal_call(args: &str) -> (ToolResult, Arc<IntentLedger>) {
        let l = Arc::new(IntentLedger::new());
        let _sink = IntentSink::new(l.clone(), NullToolSink);
        let mut h = crate::testing::harness();
        h.rt.registry
            .register(Box::new(GoalTool::new(l.clone())))
            .unwrap();
        let r = h.call("goal", args);
        (r, l)
    }

    #[test]
    fn a_goal_with_no_acceptance_criterion_is_refused_with_the_fix() {
        let (r, _) = goal_call(r#"{"op":"set","goal":"make the tests pass"}"#);
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
        let out = r.render();
        assert!(out.contains("`acceptance`"), "{out}");
        assert!(out.contains("only abandoned"), "{out}");
    }

    #[test]
    fn a_goal_met_with_nothing_measured_is_not_met() {
        let l = Arc::new(IntentLedger::new());
        let _sink = IntentSink::new(l.clone(), NullToolSink);
        let mut h = crate::testing::harness();
        h.rt.registry
            .register(Box::new(GoalTool::new(l.clone())))
            .unwrap();
        h.call(
            "goal",
            r#"{"op":"set","goal":"make the tests pass","acceptance":"cargo test --workspace is green"}"#,
        );
        let out = h.call("goal", r#"{"op":"met"}"#).render();
        assert!(out.contains("NOT counted as met"), "{out}");
        assert!(out.contains("cargo test --workspace is green"), "{out}");
    }

    #[test]
    fn asking_for_a_goal_nobody_set_abstains_rather_than_inventing_one() {
        let (r, _) = goal_call(r#"{"op":"get"}"#);
        assert!(matches!(r.outcome, ToolOutcome::Abstained { .. }), "{r:?}");
        assert_eq!(
            crate::result::Envelope::classify(&r.render()),
            Some("NO_RESULT")
        );
    }

    fn plan_harness() -> (crate::testing::Harness, Arc<PlanMode>, Arc<IntentLedger>) {
        let plan = Arc::new(PlanMode::new());
        let ledger = Arc::new(IntentLedger::new());
        let mut h = crate::testing::writable_harness();
        h.rt.registry
            .register(Box::new(EnterPlanMode::new(plan.clone())))
            .unwrap();
        h.rt.registry
            .register(Box::new(ExitPlanMode::new(plan.clone(), ledger.clone())))
            .unwrap();
        (h, plan, ledger)
    }

    #[test]
    fn entering_says_what_leaving_will_cost_before_you_are_in() {
        let (mut h, plan, _) = plan_harness();
        let out = h
            .call("enter_plan_mode", r#"{"why":"work out the shape"}"#)
            .render();
        assert!(plan.active());
        assert!(out.contains("adjudicate"), "{out}");
        assert!(out.contains("not_run"), "{out}");
    }

    #[test]
    fn leaving_without_a_plan_is_refused() {
        let (mut h, _plan, _) = plan_harness();
        h.call("enter_plan_mode", "{}");
        let r = h.call("exit_plan_mode", "{}");
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }), "{r:?}");
        assert!(
            r.render().contains("leaving with nothing"),
            "{}",
            r.render()
        );
    }

    #[test]
    fn the_steps_a_plan_exits_with_become_checkable_intents() {
        let (mut h, _plan, ledger) = plan_harness();
        h.call("enter_plan_mode", "{}");
        let r = h.call(
            "exit_plan_mode",
            r#"{"plan":"rewrite it","steps":["read the parser","write the new one","run the tests"]}"#,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok, "{r:?}");
        assert_eq!(ledger.items().len(), 3);
        assert_eq!(ledger.board().total, 3);
        assert!(r.render().contains("0 of 3 complete"), "{}", r.render());
        // And they are diffable: a turn that committed to three things and ran
        // nothing is exactly T21.3.
        let rec = ledger.reconcile("turn_1", "");
        assert_eq!(rec.declared, 3);
        assert!(rec.steering().unwrap().contains("read the parser"));
    }

    #[test]
    fn a_plan_with_no_steps_says_that_nothing_will_be_checked() {
        let (mut h, _plan, ledger) = plan_harness();
        h.call("enter_plan_mode", "{}");
        let r = h.call("exit_plan_mode", r#"{"plan":"just look at it"}"#);
        assert_eq!(r.outcome, ToolOutcome::Ok);
        assert!(r.render().contains("cannot be checked"), "{}", r.render());
        assert_eq!(ledger.items().len(), 0);
    }

    #[test]
    fn leaving_when_not_in_plan_mode_is_refused_rather_than_a_no_op() {
        let (mut h, _plan, _) = plan_harness();
        let r = h.call("exit_plan_mode", r#"{"plan":"x"}"#);
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
    }

    #[test]
    fn every_description_here_lints_clean() {
        let plan = Arc::new(PlanMode::new());
        let ledger = Arc::new(IntentLedger::new());
        for s in [
            GoalTool::new(ledger.clone()).schema(),
            EnterPlanMode::new(plan.clone()).schema(),
            ExitPlanMode::new(plan, ledger).schema(),
        ] {
            assert_eq!(
                crate::schema::lint_description(&s.description),
                vec![],
                "{}",
                s.name
            );
            assert!(
                s.description.len() < 800,
                "{} is {} bytes",
                s.name,
                s.description.len()
            );
        }
    }
}
