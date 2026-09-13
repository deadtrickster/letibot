//! `task` — spawn a subagent to do a subtask and return its result.
//!
//! The tool holds a [`TaskRunner`]; the harness installs the real one (a nested
//! turn with a restricted role) and the default refuses by name. Keeping the seam
//! here means the tool is registered and seatable before the subagent loop lands.

use std::sync::Arc;

use serde_json::json;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// Runs a subagent turn to completion and returns its final answer.
pub trait TaskRunner: Send + Sync {
    fn run(&self, prompt: &str, role: &str) -> Result<String, String>;
}

/// The default: no runner, and it says so rather than pretending to have run.
pub struct NoTaskRunner;

impl TaskRunner for NoTaskRunner {
    fn run(&self, _prompt: &str, _role: &str) -> Result<String, String> {
        Err("no subagent runner is installed in this session".into())
    }
}

pub struct TaskTool {
    runner: Arc<dyn TaskRunner>,
}

impl TaskTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskTool { runner }
    }
}

impl Tool for TaskTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task",
            "Run a subtask in a subagent and return its result. Give `prompt` (the \
             subtask); optionally `role` to pick the subagent's toolset (defaults to \
             coder). The subagent works to completion and its final answer is returned.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "The subtask for the subagent."},
                    "role": {"type": "string", "description": "The subagent's role. Defaults to coder."}
                },
                "required": ["prompt"]
            }),
            // `Session`, not `Exec`: `task` runs no host command itself — it delegates
            // to a child turn whose own gate governs its write/exec/network calls. As
            // `Exec` it would hit the operator's rule that exec always asks (a subagent
            // spawn is not a shell, and opencode does not gate it), and a subagent that
            // could not even be spawned without a head would never run.
            Access::Session,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(prompt) = args.get("prompt").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "task needs a prompt",
                "call `task` with `prompt` set to the subtask.",
            );
        };
        let role = args.get("role").and_then(|v| v.as_str()).unwrap_or("coder");
        match self.runner.run(prompt, role) {
            Ok(result) => Invocation::ok(result),
            Err(e) => Invocation::failed(e, "the subagent did not run."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl TaskRunner for Echo {
        fn run(&self, prompt: &str, role: &str) -> Result<String, String> {
            Ok(format!("{role}: {prompt}"))
        }
    }

    #[test]
    fn a_runner_is_delegated_to_and_no_runner_refuses() {
        assert_eq!(Echo.run("find the bug", "coder").unwrap(), "coder: find the bug");
        assert!(NoTaskRunner.run("x", "coder").unwrap_err().contains("no subagent runner"));
    }
}
