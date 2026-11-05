//! Plan mode as a capability boundary, not a flag: the seated role holds no write tools,
//! so there is nothing to deny because there is nothing to call, and [`PlanGate`] catches
//! a call already in flight. [`PLAN_MODE_TOOLS`] stay seated: the plan is not the work.

use std::sync::Mutex;

use letibot_transcript::ToolOutcome;

use crate::runtime::{Gate, GateCall, GateDecision, Registry, Role};
use crate::schema::Access;

/// Whether the session is planning, and what turned that on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanState {
    pub active: bool,
    /// The call id of the `enter_plan_mode` that turned it on, which is the `req_id` a
    /// refusal carries: somebody did decide, and this names who and when.
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

/// The tools that stay seated in plan mode whatever their declared access: the plan, not
/// the work. The order is the order they are appended in, and it is part of the stable
/// prefix, so it is written down once here.
pub const PLAN_MODE_TOOLS: &[&str] = &["write_plan", "say", "exit_plan_mode"];

/// The role to seat for the next turn: the base role's read and session tools plus
/// [`PLAN_MODE_TOOLS`], with everything that can change the tree, run a command or reach the
/// network absent from the tool list. Changing that list re-prefills the conversation.
pub fn seating(registry: &Registry, base: &Role, plan_active: bool) -> Role {
    if !plan_active {
        return base.clone();
    }
    let schemas = registry.schemas();
    let mut keep: Vec<String> = base
        .tools
        .iter()
        .filter(|name| {
            if PLAN_MODE_TOOLS.contains(&name.as_str()) {
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
    for t in PLAN_MODE_TOOLS {
        if !keep.iter().any(|k| k == t) && schemas.iter().any(|s| s.name == *t) {
            keep.push((*t).to_string());
        }
    }
    Role {
        name: format!("{}+plan", base.name),
        tools: keep,
        max_tools: base.max_tools,
    }
}

/// The second mechanism: a gate that refuses every non-read call while plan mode is on. It
/// wraps the session's real gate rather than replacing it, so leaving plan mode restores
/// exactly the boundary that was there before.
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
        if s.active && !PLAN_MODE_TOOLS.contains(&call.name) {
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
                     nothing ran. Plan mode was entered by call {}{}. What IS seated: \
                     `write_plan`, to write the plan itself, and `say`, to discuss it \
                     with the fleet — plan mode is no writes to the WORK, not no writes. \
                     When the plan is done, `exit_plan_mode` with it; that call is \
                     adjudicated, and it is the only way back to the work.",
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
        // The intent tools too: `resolve_role` refuses a role naming a tool the registry does
        // not hold, and the daemon registers these separately in `Harness::open`.
        let mut r = crate::coder_tools(Arc::new(crate::builtins::retrieval::Unavailable)).unwrap();
        crate::builtins::intent::register_into(
            &mut r,
            &crate::builtins::intent::Wiring::standalone(),
        )
        .unwrap();
        // And `bash`: the daemon registers the exec tools unconditionally, and it is the ROLE
        // that decides who may call them. Without it the fixture would walk the
        // name-not-in-the-registry branch and measure the gap instead of plan mode.
        r.register(Box::new(crate::builtins::bash::Bash::default()))
            .unwrap();
        // And the delegation trio, for the same reason: `resolve_role` refuses a role naming a
        // tool the registry does not hold, so a fixture that left them out would be measuring
        // the gap rather than plan mode.
        let no_runner: Arc<dyn crate::builtins::task::TaskRunner> =
            Arc::new(crate::builtins::task::NoTaskRunner);
        r.register(Box::new(crate::builtins::task::TaskTool::new(
            no_runner.clone(),
        )))
        .unwrap();
        r.register(Box::new(crate::builtins::task::TaskResultTool::new(
            no_runner.clone(),
        )))
        .unwrap();
        r.register(Box::new(crate::builtins::task::TaskMessageTool::new(
            no_runner.clone(),
        )))
        .unwrap();
        r.register(Box::new(crate::builtins::task::TaskResumeTool::new(
            no_runner.clone(),
        )))
        .unwrap();
        // And `task_start`, for the same reason: `resolve_role` refuses a role naming a tool
        // the registry does not hold, and `m2_coder` seats it beside `task`.
        r.register(Box::new(crate::builtins::task::TaskStartTool::new(
            no_runner,
        )))
        .unwrap();
        // And `notes`, one seat later again: `m2_coder` seats it beside `write`/`edit`, and
        // leaving it out would measure the gap instead of plan mode.
        r.register(Box::new(crate::builtins::notes::NotesTool::new(
            std::sync::Arc::new(PlanNotes),
        )))
        .unwrap();
        r
    }

    /// A notes scope for the fixture: the plan-mode tests never call the tool,
    /// so it points nowhere real - it only has to let the schema exist.
    struct PlanNotes;

    impl crate::builtins::notes::NotesScope for PlanNotes {
        fn workspace(&self) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }
        fn global_dir(&self) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }
        fn project_dir(&self) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }
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
        // `bash` is `Access::Exec`, so the same rule drops it - the property worth asserting by
        // name, because a plan mode that removed `write` and `edit` and left a shell would be
        // plan mode in name only: `sh -c 'echo x > src/lib.rs'` is a write to the work.
        assert!(
            !seated.tools.contains(&"bash".to_string()),
            "plan mode that leaves a shell has not removed the write path: {:?}",
            seated.tools
        );
        // And `notes`, on the same rule: a standing note outlives the plan and every session
        // after it, which makes it the last thing a planning seat should be able to plant.
        assert!(
            !seated.tools.contains(&"notes".to_string()),
            "plan mode that leaves the notes writer has not removed the write path: {:?}",
            seated.tools
        );

        // And the property that matters: the model is never shown their schemas.
        let kept = reg.resolve_role(&seated).unwrap();
        let json = kept.tools_json().join("\n");
        assert!(!json.contains("\"name\":\"write\""), "{json}");
        assert!(!json.contains("\"name\":\"edit\""), "{json}");
        assert!(!json.contains("\"name\":\"bash\""), "{json}");
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
            scripts: &[],
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
            scripts: &[],
            name: "exit_plan_mode",
            access: Access::Write,
            args: &args,
            turn_id: "turn_2",
            call_id: "call_9",
            workspace: "/w",
            target_exists: None,
        };
        // It reaches the inner gate, which in this session is NoBoundary and
        // refuses with NotRun - nobody decided. That is the honest answer and it
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
