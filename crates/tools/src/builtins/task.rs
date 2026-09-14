//! `task` — spawn a subagent to do a subtask and return its result.
//!
//! The tool holds a [`TaskRunner`]; the harness installs the real one (a nested
//! turn with a restricted role) and the default refuses by name. Keeping the seam
//! here means the tool is registered and seatable before the subagent loop lands.
//!
//! # A subagent inherits, and may be downgraded — never upgraded
//!
//! A subagent runs under its parent's permission ruleset, and `access` narrows
//! it further: a survey needs no writes, a formatter needs no network. The
//! [`Downgrade`] is applied to the tools seated, the backend opened and the
//! ruleset, and it is the **union** of the parent's own downgrade and the one
//! asked for, so a downgraded session cannot spawn a wider child by naming a
//! wider role. The role picks which tools; the downgrade says which classes of
//! them are gone whatever the role.
//!
//! # Where it runs
//!
//! `where` names the substrate: `host` (the parent's own boundary, the
//! default) or `firecode` (a VM, firecracker or libvirt). Placement never widens
//! permissions either — a subagent in a VM is still the parent's ruleset, minus
//! the downgrade, inside a boundary. The firecode placement is a declared seam:
//! a build without that backend refuses by name rather than running on the host
//! and calling it a VM.

use std::sync::Arc;

use serde_json::json;

use crate::runtime::{Invocation, InvokeCtx, Tool};
pub use crate::schema::Downgrade;
use crate::schema::{Access, ToolSchema};

/// Where a subagent runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// The parent's own boundary, whatever it is.
    #[default]
    Host,
    /// A firecode VM. Which hypervisor is the backend's business.
    Firecode,
}

impl Placement {
    pub fn parse(s: &str) -> Result<Placement, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "host" | "here" => Ok(Placement::Host),
            "firecode" | "vm" | "firecracker" | "libvirt" => Ok(Placement::Firecode),
            other => Err(format!(
                "`{other}` is not a placement. There are two: `host` (the default) and \
                 `firecode` (a VM)"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Placement::Host => "host",
            Placement::Firecode => "firecode",
        }
    }
}

/// What one `task` call asked for, beyond the prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSpec {
    pub role: String,
    pub downgrade: Downgrade,
    pub placement: Placement,
}

/// Runs a subagent turn to completion and returns its final answer.
pub trait TaskRunner: Send + Sync {
    fn run(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String>;
}

/// The default: no runner, and it says so rather than pretending to have run.
pub struct NoTaskRunner;

impl TaskRunner for NoTaskRunner {
    fn run(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
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
             coder), `access` to narrow it BELOW your own permissions (`read-only` for \
             a survey that must not write; or `no-write`, `no-exec`, `no-network`), \
             and `where` (`host`, the default, or `firecode` for a VM). A subagent \
             inherits your permissions and can only be given less, never more. It \
             works to completion and its final answer is returned.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "The subtask for the subagent."},
                    "role": {"type": "string", "description": "The subagent's role. Defaults to coder."},
                    "access": {"type": "string", "description": "Narrow the subagent below your own permissions: `read-only` (no write, exec or network), or any of `no-write`, `no-exec`, `no-network`, comma-separated. Omit to inherit yours unchanged. Cannot widen."},
                    "where": {"type": "string", "description": "`host` (the default: your own boundary) or `firecode` (a VM). Placement never changes permissions."}
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
        let downgrade = match args.get("access").and_then(|v| v.as_str()) {
            None => Downgrade::none(),
            Some(a) => match Downgrade::parse(a) {
                Ok(d) => d,
                Err(e) => {
                    return Invocation::failed(
                        "`access` was not understood",
                        format!("{e}. Nothing was spawned."),
                    );
                }
            },
        };
        let placement = match args.get("where").and_then(|v| v.as_str()) {
            None => Placement::Host,
            Some(w) => match Placement::parse(w) {
                Ok(p) => p,
                Err(e) => {
                    return Invocation::failed(
                        "`where` was not understood",
                        format!("{e}. Nothing was spawned."),
                    );
                }
            },
        };
        let spec = TaskSpec {
            role: role.to_string(),
            downgrade,
            placement,
        };
        match self.runner.run(prompt, &spec) {
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
        fn run(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
            Ok(format!(
                "{} [{}] @{}: {prompt}",
                spec.role,
                spec.downgrade.describe(),
                spec.placement.as_str()
            ))
        }
    }

    fn spec(role: &str) -> TaskSpec {
        TaskSpec {
            role: role.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_runner_is_delegated_to_and_no_runner_refuses() {
        assert_eq!(
            Echo.run("find the bug", &spec("coder")).unwrap(),
            "coder [none] @host: find the bug"
        );
        assert!(
            NoTaskRunner
                .run("x", &spec("coder"))
                .unwrap_err()
                .contains("no subagent runner")
        );
    }

    #[test]
    fn a_downgrade_only_removes_and_the_union_keeps_every_denial() {
        let survey = Downgrade::parse("read-only").unwrap();
        assert!(
            survey.denies(Access::Write)
                && survey.denies(Access::Exec)
                && survey.denies(Access::Network)
        );
        assert!(!survey.denies(Access::Read) && !survey.denies(Access::Session));
        assert_eq!(survey.describe(), "no write, no exec, no network");
        let nw = Downgrade::parse("no-write, no-network").unwrap();
        assert_eq!(nw.describe(), "no write, no network");
        // A parent that cannot write cannot hand a child writes back by asking for
        // less: the union is what the child gets.
        assert_eq!(nw.and(&Downgrade::parse("no-exec").unwrap()), survey);
        assert!(
            Downgrade::parse("no-read")
                .unwrap_err()
                .contains("not a subagent")
        );
        assert!(
            Downgrade::parse("sudo")
                .unwrap_err()
                .contains("not a downgrade")
        );
        assert!(Downgrade::parse("").unwrap().is_none());
    }

    #[test]
    fn the_tool_parses_access_and_where_and_refuses_the_unknown() {
        use crate::runtime::{Registry, ToolRuntime};
        use letibot_transcript::ToolCall;
        let mut reg = Registry::new();
        reg.register(Box::new(TaskTool::new(Arc::new(Echo))))
            .unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        let mut sink = crate::NullToolSink;
        let run = |rt: &mut ToolRuntime, sink: &mut crate::NullToolSink, args: &str| {
            rt.invoke(
                "t1",
                &ToolCall {
                    id: "c0".into(),
                    name: "task".into(),
                    arguments: args.into(),
                },
                sink,
            )
            .payload
        };
        assert_eq!(
            run(
                &mut rt,
                &mut sink,
                r#"{"prompt": "survey the crates", "role": "researcher", "access": "read-only", "where": "firecode"}"#
            ),
            "researcher [no write, no exec, no network] @firecode: survey the crates"
        );
        let p = run(&mut rt, &mut sink, r#"{"prompt": "x", "access": "root"}"#);
        assert!(p.contains("not a downgrade"), "{p}");
        let p = run(&mut rt, &mut sink, r#"{"prompt": "x", "where": "moon"}"#);
        assert!(p.contains("not a placement"), "{p}");
    }
}
