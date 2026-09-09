//! Plan mode as a **capability boundary**, not a flag.
//!
//! `docs/tool-design-brief.md` §2.4: make the mistake inexpressible, do not warn
//! about it. Three of the five surveyed harnesses have a plan mode and all three
//! implement it as a *guard*: opencode denies `plan_exit` by permission, omp's
//! `plan-mode-guard.ts:143-153` hard-rejects filesystem mutation at call time,
//! grok-build swaps a policy. In every one of them the model is still handed the
//! write tools' schemas and still emits calls to them; the harness says no
//! afterwards.
//!
//! Ours does it one layer earlier. [`seating`] returns the [`Role`] for the next
//! turn, and in plan mode that role **does not contain the write tools**. They are
//! therefore not in `Registry::tools_json`, not in the stable prefix, and not in
//! the space of things the model can phrase. There is nothing to deny because
//! there is nothing to call.
//!
//! # And a second mechanism, because one is never enough here
//!
//! Un-seating happens at a turn boundary — the tool list is part of the stable
//! prefix, and rewriting it mid-turn re-prefills the conversation. So a call
//! already in flight, or a session whose head forgot to reseat, would still reach
//! a write tool. [`PlanGate`] closes that: it wraps whatever gate the session has
//! and refuses every non-read call while plan mode is active.
//!
//! That refusal carries [`ToolOutcome::Denied`], not `NotRun`, and the `req_id` is
//! **the call id of the `enter_plan_mode` that turned it on**. Somebody did decide;
//! this names who and when. Compare [`crate::runtime::NoBoundary`], which is
//! `NotRun` precisely because nobody decided there.
//!
//! # Leaving plan mode is a gated write
//!
//! Entering plan mode narrows what the session can do; leaving it *widens* that
//! again, which is the direction that needs a decision. So `exit_plan_mode`
//! declares [`crate::schema::Access::Write`] and goes to the adjudicator like any
//! other write. In a session with no adjudicator it refuses with `NotRun`, and
//! `enter_plan_mode` says so **before** you enter rather than after — clause 1
//! applied to a door you can only walk through once.

use std::sync::Mutex;

use letibot_transcript::ToolOutcome;

use crate::runtime::{Gate, GateCall, GateDecision, Registry, Role};
use crate::schema::Access;

/// Whether the session is planning, and what turned that on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanState {
    pub active: bool,
    /// The call id of the `enter_plan_mode` that turned it on. This is the
    /// `req_id` a refusal carries: a decision with nobody attached to it is the
    /// thing this crate refuses to report anywhere else, and plan mode is no
    /// exception.
    pub entered_by: Option<String>,
    pub entered_turn: Option<String>,
    /// The plan the model exited with, once it has.
    pub plan: Option<String>,
}

/// Session-scoped plan-mode state, shared by `Arc` between the two tools, the
/// gate, and whoever seats the next turn's role.
#[derive(Debug, Default)]
pub struct PlanMode {
    state: Mutex<PlanState>,
}

impl PlanMode {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PlanState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> PlanState {
        self.lock().clone()
    }

    pub fn active(&self) -> bool {
        self.lock().active
    }

    /// Enter. Idempotent: entering while already in plan mode keeps the original
    /// `entered_by`, because the refusal's `req_id` must point at the decision
    /// that is actually in force.
    pub fn enter(&self, turn_id: &str, call_id: &str) -> PlanState {
        let mut s = self.lock();
        if !s.active {
            s.active = true;
            s.entered_by = Some(call_id.to_string());
            s.entered_turn = Some(turn_id.to_string());
            s.plan = None;
        }
        s.clone()
    }

    /// Leave, with the plan that is being left with.
    pub fn exit(&self, plan: &str) -> PlanState {
        let mut s = self.lock();
        s.active = false;
        s.plan = Some(plan.to_string());
        s.clone()
    }
}

/// The role to seat for the next turn.
///
/// In plan mode the returned role contains only the tools whose declared access is
/// [`Access::Read`] or [`Access::Session`]. Everything that can change the
/// operator's tree, run a command, or reach the network is **absent from the tool
/// list the model is shown**.
///
/// `exit_plan_mode` is kept even though it declares `Write`, and that is the one
/// deliberate exception: a plan mode you cannot ask to leave is a trap rather than
/// a boundary, and its own gate still decides whether the ask succeeds.
///
/// # The cost, stated
///
/// The tool list is part of the stable prefix. Entering or leaving plan mode
/// changes `tools_json`, which **re-prefills the conversation**. That is real and
/// it is the price of the boundary being structural rather than advisory; it is
/// also why this returns a role for the *next* turn instead of mutating a
/// registry mid-turn.
pub fn seating(registry: &Registry, base: &Role, plan_active: bool) -> Role {
    if !plan_active {
        return base.clone();
    }
    let schemas = registry.schemas();
    let keep: Vec<String> = base
        .tools
        .iter()
        .filter(|name| {
            if name.as_str() == "exit_plan_mode" {
                return true;
            }
            schemas
                .iter()
                .find(|s| &s.name == *name)
                .map(|s| matches!(s.access, Access::Read | Access::Session))
                // A name the registry does not have stays in the role, so
                // `resolve_role` refuses it loudly. Silently dropping it here
                // would turn a mis-wired role into a smaller one.
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    Role {
        name: format!("{}+plan", base.name),
        tools: keep,
        max_tools: base.max_tools,
    }
}

/// The second mechanism: a gate that refuses every non-read call while plan mode
/// is on.
///
/// Wraps the session's real gate rather than replacing it, so leaving plan mode
/// restores exactly the boundary that was there before — a gate that had to be
/// rebuilt on exit is a gate that comes back subtly different.
pub struct PlanGate {
    plan: std::sync::Arc<PlanMode>,
    inner: Box<dyn Gate>,
}

impl PlanGate {
    pub fn new(plan: std::sync::Arc<PlanMode>, inner: Box<dyn Gate>) -> Self {
        PlanGate { plan, inner }
    }
}

impl Gate for PlanGate {
    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
        let s = self.plan.state();
        if s.active && call.name != "exit_plan_mode" {
            return GateDecision::refuse_and_tell(
                ToolOutcome::Denied {
                    // Somebody decided, and this is which call.
                    req_id: s
                        .entered_by
                        .clone()
                        .unwrap_or_else(|| "plan_mode".to_string()),
                },
                format!(
                    "this session is in plan mode, so `{}` ({} access) is not seated and \
                     nothing ran. Plan mode was entered by call {}{}. Finish the plan and \
                     call `exit_plan_mode` with it; that call is adjudicated, and it is \
                     the only way back to the write tools.",
                    call.name,
                    call.access.as_str(),
                    s.entered_by.as_deref().unwrap_or("(unknown)"),
                    s.entered_turn
                        .as_deref()
                        .map(|t| format!(" in turn {t}"))
                        .unwrap_or_default()
                ),
            );
        }
        self.inner.admit(call)
    }

    fn describe(&self) -> String {
        if self.plan.active() {
            format!("plan mode (active) over {}", self.inner.describe())
        } else {
            format!("plan mode (off) over {}", self.inner.describe())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{NoBoundary, roles};
    use std::sync::Arc;

    fn registry() -> Registry {
        crate::coder_tools(Arc::new(crate::builtins::retrieval::Unavailable)).unwrap()
    }

    #[test]
    fn plan_mode_removes_the_write_tools_from_the_seated_role() {
        let reg = registry();
        let base = roles::m2_coder();
        assert!(base.tools.contains(&"write".to_string()));

        let seated = seating(&reg, &base, true);
        assert!(
            !seated.tools.contains(&"write".to_string()),
            "{:?}",
            seated.tools
        );
        assert!(!seated.tools.contains(&"edit".to_string()));
        assert!(seated.tools.contains(&"read".to_string()));

        // And the property that matters: the model is never shown their schemas.
        let kept = reg.resolve_role(&seated).unwrap();
        let json = kept.tools_json().join("\n");
        assert!(!json.contains("\"name\":\"write\""), "{json}");
        assert!(!json.contains("\"name\":\"edit\""), "{json}");
    }

    #[test]
    fn leaving_plan_mode_restores_the_base_role() {
        let reg = registry();
        let base = roles::m2_coder();
        let out = seating(&reg, &base, false);
        assert_eq!(out, base);
    }

    #[test]
    fn the_gate_refuses_with_denied_and_names_the_call_that_decided() {
        let plan = Arc::new(PlanMode::new());
        plan.enter("turn_1", "call_7");
        let mut gate = PlanGate::new(plan, Box::new(NoBoundary));
        let args = serde_json::json!({"path": "a.rs"});
        let call = GateCall {
            name: "write",
            access: Access::Write,
            args: &args,
            turn_id: "turn_2",
            call_id: "call_9",
            workspace: "/w",
            target_exists: Some(false),
        };
        match gate.admit(&call) {
            GateDecision::Refuse { outcome, tell } => {
                assert_eq!(
                    outcome,
                    ToolOutcome::Denied {
                        req_id: "call_7".into()
                    },
                    "plan mode is a decision somebody made, not an absence of one"
                );
                assert!(tell.contains("call_7"), "{tell}");
            }
            other => panic!("plan mode must refuse a write, got {other:?}"),
        }
    }

    #[test]
    fn exit_plan_mode_is_the_one_call_the_gate_still_passes_through() {
        let plan = Arc::new(PlanMode::new());
        plan.enter("turn_1", "call_7");
        let mut gate = PlanGate::new(plan, Box::new(NoBoundary));
        let args = serde_json::json!({});
        let call = GateCall {
            name: "exit_plan_mode",
            access: Access::Write,
            args: &args,
            turn_id: "turn_2",
            call_id: "call_9",
            workspace: "/w",
            target_exists: None,
        };
        // It reaches the inner gate, which in this session is NoBoundary and
        // refuses with NotRun — nobody decided. That is the honest answer and it
        // is a different answer from plan mode's.
        match gate.admit(&call) {
            GateDecision::Refuse { outcome, .. } => {
                assert!(matches!(outcome, ToolOutcome::NotRun { .. }));
            }
            other => panic!("expected the inner gate's refusal, got {other:?}"),
        }
    }

    #[test]
    fn entering_twice_keeps_the_decision_that_is_in_force() {
        let plan = PlanMode::new();
        plan.enter("turn_1", "call_1");
        plan.enter("turn_2", "call_2");
        assert_eq!(plan.state().entered_by.as_deref(), Some("call_1"));
    }
}
