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

/// Where a spawned subagent has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    /// Still working. `note` is the child's own last word about what it is doing,
    /// when it has said one.
    Running { note: Option<String> },
    /// Finished, with its final answer.
    Done { answer: String },
    /// It did not finish, and this is why.
    Failed { why: String },
    /// No subagent by that name was ever spawned here.
    Unknown,
}

/// Starts subagents and collects them. **Starting is not running.**
///
/// The trait used to have one method, `run`, which spawned a child and blocked
/// until it answered. The daemon runs a round's calls in order on one thread, so
/// a `task` held every call behind it for as long as the child lived: measured
/// 2026-09-17, a block of `todo_write`, `task` and three `web_search` calls sat
/// unfinished for the fifteen minutes the subagent ran, and the operator — and
/// the model, in its own reasoning — read it as a hang. The operator's reading of
/// the shape: *"task by definition must be background task no? because non
/// background is what? a simple bash?"* Right. A subagent is minutes of work by
/// construction, so the call that starts one returns a handle, the same way
/// `bash --background` does, and the answer is collected by its own call.
pub trait TaskRunner: Send + Sync {
    /// Start the subtask and return the handle it can be collected by. Returns
    /// when the child is **spawned**, not when it is finished.
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String>;

    /// Where a spawned subagent has got to, waiting up to `timeout` for it to
    /// finish. A zero timeout is a poll.
    fn collect(&self, handle: &str, timeout: std::time::Duration) -> TaskStatus;

    /// The handles this session has started, oldest first — what `task_result`
    /// lists when it is called with no handle, so a model that lost one can find
    /// it without the operator.
    fn started(&self) -> Vec<String> {
        Vec::new()
    }

    /// **Stop a subagent.** `Ok` is what was done, in the words the operator gets;
    /// `Err` is why it could not be, said rather than swallowed.
    ///
    /// The operator's ruling, when `job_kill` was found to reach only the host's process
    /// table: *"expand"*. A subagent is not a process, so it cannot be reaped the way a
    /// job is — and it is still the same handle-shaped thing a model reaches for a stop
    /// with, which is the rest of the same ruling: *"in a way agent is a background job."*
    ///
    /// **The kill is an interrupt of the child's turn**, because that turn is the only
    /// thing a subagent is doing: it does not own a process, a cgroup or a scope, and the
    /// one door that reaches a turn already running is the session's own steering — the
    /// same door a head's Esc-esc uses. A runner that cannot do that says so rather than
    /// returning a kill it did not perform.
    fn kill(&self, handle: &str) -> Result<String, String> {
        Err(format!(
            "this session's runner cannot stop `{handle}`: a subagent is stopped by \
             interrupting the turn it is running, which only the runner that started it \
             can do. Nothing was stopped."
        ))
    }
}

/// The default: no runner, and it says so rather than pretending to have run.
pub struct NoTaskRunner;

impl TaskRunner for NoTaskRunner {
    fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        Err("no subagent runner is installed in this session".into())
    }

    fn collect(&self, _handle: &str, _timeout: std::time::Duration) -> TaskStatus {
        TaskStatus::Unknown
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
            "Start a subtask in a subagent. Give `prompt` (the subtask); optionally \
             `role` to pick the subagent's toolset (defaults to coder), `access` to \
             narrow it BELOW your own permissions (`read-only` for a survey that must \
             not write; or `no-write`, `no-exec`, `no-network`), and `where` (`host`, \
             the default, or `firecode` for a VM). A subagent inherits your \
             permissions and can only be given less, never more. This returns as soon \
             as the subagent has STARTED, with a handle — a subagent is minutes of \
             work and this call does not wait for it, so the rest of your calls run \
             while it does. Collect its answer with `task_result`.",
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

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
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
        let handle = match self.runner.start(prompt, &spec) {
            Ok(h) => h,
            Err(e) => return Invocation::failed(e, "the subagent did not start."),
        };
        ctx.progress(&format!("subagent {handle} started"));
        // **`Backgrounded`, not `Ok`** — the same reason `bash --background`
        // gives: the outcome names a fact about the world, *this is running and
        // has not answered yet*, and an `Ok` here would be grounding for an
        // answer nothing has produced.
        Invocation::backgrounded(
            handle.clone(),
            std::time::Duration::ZERO,
            letibot_transcript::Backgrounding::Asked,
            format!("call `task_result` with task=\"{handle}\" and a `timeout_ms`"),
            format!(
                "started subagent `{handle}` as a `{}`.\n  subtask: {}\n\nIt is working \
                 now, and this call did not wait for it — the rest of this round runs \
                 while it does. `task_result` with task=\"{handle}\" and a `timeout_ms` \
                 blocks until it answers and returns what it said; with no `timeout_ms` \
                 it reports where the subagent has got to without waiting.",
                spec.role,
                first_line(prompt),
            ),
        )
    }
}

/// The subtask's first line, for a one-line echo. The whole prompt back would be
/// the model's own bytes returned to it as a result.
fn first_line(prompt: &str) -> String {
    let l = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if l.chars().count() <= 100 {
        return l.to_string();
    }
    format!("{}…", l.chars().take(100).collect::<String>())
}

/// `task_result` — collect a subagent `task` started.
pub struct TaskResultTool {
    runner: Arc<dyn TaskRunner>,
}

impl TaskResultTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskResultTool { runner }
    }
}

impl Tool for TaskResultTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task_result",
            "Collect a subagent started by `task`. Give `task` (the handle `task` \
             returned) and optionally `timeout_ms` to block until it answers — \
             without one this reports where it has got to and returns at once. The \
             three endings are three different outcomes: it answered, it failed, or \
             it is still working. Call with no `task` to list the subagents this \
             session started.",
            json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "The handle `task` returned. Omit to list this session's subagents."},
                    "timeout_ms": {"type": "integer", "description": "Block up to this long for the subagent to answer. Omit to report its state without waiting."}
                }
            }),
            // Reading the answer of a child that has already been governed by its
            // own gate. Nothing here reaches the host.
            Access::Read,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(handle) = args.get("task").and_then(|v| v.as_str()) else {
            let started = self.runner.started();
            if started.is_empty() {
                return Invocation::ok(
                    "this session has started no subagents. `task` starts one and \
                     returns the handle to collect it by."
                        .to_string(),
                );
            }
            let mut out = format!("{} subagent(s) started here:\n", started.len());
            for h in &started {
                let state = match self.runner.collect(h, std::time::Duration::ZERO) {
                    TaskStatus::Running { note: Some(n) } => format!("running — {n}"),
                    TaskStatus::Running { note: None } => "running".into(),
                    TaskStatus::Done { .. } => "answered".into(),
                    TaskStatus::Failed { .. } => "failed".into(),
                    TaskStatus::Unknown => "unknown".into(),
                };
                out.push_str(&format!("  {h} — {state}\n"));
            }
            return Invocation::ok(out);
        };
        // A deadline always applies, and its expiry is its own outcome rather
        // than being reported as completion — `job_wait`'s rule, for the same
        // reason: a wait that ran out and a child that answered are two facts.
        let timeout = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .map(std::time::Duration::from_millis)
            .unwrap_or(std::time::Duration::ZERO);
        match self.runner.collect(handle, timeout) {
            TaskStatus::Done { answer } => Invocation::ok(answer),
            TaskStatus::Failed { why } => Invocation::failed(
                format!("subagent `{handle}` did not finish"),
                format!(
                    "{why}\n\nNothing of its work is lost: it has its own session, \
                         and its transcript is on the board under `{handle}`."
                ),
            ),
            TaskStatus::Running { note } => Invocation::abstained(
                format!("subagent `{handle}` is still working"),
                format!(
                    "the deadline passed and `{handle}` has not answered yet{}. This is \
                     the wait ending, not the subagent ending — call `task_result` again \
                     with a longer `timeout_ms`, or do something else and collect it \
                     later.",
                    note.map(|n| format!(" (last: {n})")).unwrap_or_default()
                ),
            ),
            TaskStatus::Unknown => Invocation::failed(
                format!("no subagent called `{handle}` was started here"),
                "call `task_result` with no `task` to list the ones that were.".to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runner that answers immediately: `start` records what it was asked and
    /// `collect` hands the same sentence back, so the tests are about the tool's
    /// own shape and not about threads.
    #[derive(Default)]
    struct Echo {
        answers: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl TaskRunner for Echo {
        fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
            let mut a = self.answers.lock().unwrap();
            let handle = format!("sub-{}", a.len() + 1);
            a.push((
                handle.clone(),
                format!(
                    "{} [{}] @{}: {prompt}",
                    spec.role,
                    spec.downgrade.describe(),
                    spec.placement.as_str()
                ),
            ));
            Ok(handle)
        }

        fn collect(&self, handle: &str, _timeout: std::time::Duration) -> TaskStatus {
            match self
                .answers
                .lock()
                .unwrap()
                .iter()
                .find(|(h, _)| h == handle)
            {
                Some((_, answer)) => TaskStatus::Done {
                    answer: answer.clone(),
                },
                None => TaskStatus::Unknown,
            }
        }

        fn started(&self) -> Vec<String> {
            self.answers
                .lock()
                .unwrap()
                .iter()
                .map(|(h, _)| h.clone())
                .collect()
        }
    }

    fn spec(role: &str) -> TaskSpec {
        TaskSpec {
            role: role.into(),
            ..Default::default()
        }
    }

    /// **The call returns while the child is still working.** This is the whole
    /// point of the change: the daemon runs a round's calls in order on one
    /// thread, so a `task` that waited held every call behind it for as long as
    /// the child lived. Measured 2026-09-17 in the operator's session — a block
    /// of `todo_write`, `task` and three `web_search` calls, all unfinished for
    /// the fifteen minutes the subagent ran.
    #[test]
    fn starting_a_subagent_does_not_wait_for_it() {
        use std::sync::Arc as StdArc;
        use std::sync::atomic::{AtomicBool, Ordering};

        /// A child that never finishes unless this test lets it.
        struct Slow {
            release: StdArc<AtomicBool>,
        }
        impl TaskRunner for Slow {
            fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
                Ok("sub-slow".into())
            }
            fn collect(&self, handle: &str, timeout: std::time::Duration) -> TaskStatus {
                if handle != "sub-slow" {
                    return TaskStatus::Unknown;
                }
                let deadline = std::time::Instant::now() + timeout;
                loop {
                    if self.release.load(Ordering::SeqCst) {
                        return TaskStatus::Done {
                            answer: "the child answered".into(),
                        };
                    }
                    if std::time::Instant::now() >= deadline {
                        return TaskStatus::Running {
                            note: Some("still going".into()),
                        };
                    }
                    std::thread::yield_now();
                }
            }
            fn started(&self) -> Vec<String> {
                vec!["sub-slow".into()]
            }
        }

        let release = StdArc::new(AtomicBool::new(false));
        let runner: Arc<dyn TaskRunner> = Arc::new(Slow {
            release: release.clone(),
        });
        let tool = TaskTool::new(runner.clone());
        let result = TaskResultTool::new(runner);

        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut reg = crate::runtime::Registry::new();
        reg.register(Box::new(tool)).unwrap();
        reg.register(Box::new(result)).unwrap();
        let mut rt = crate::runtime::ToolRuntime::new(reg, Box::new(backend));
        let mut sink = crate::NullToolSink;
        let call = |rt: &mut crate::runtime::ToolRuntime,
                    sink: &mut crate::NullToolSink,
                    name: &str,
                    args: &str| {
            rt.invoke(
                "t1",
                &letibot_transcript::ToolCall {
                    id: "c0".into(),
                    name: name.into(),
                    arguments: args.into(),
                },
                sink,
            )
        };

        // The child is nowhere near done, and `task` comes back anyway —
        // `Backgrounded`, not `Ok`: it is running and has not answered yet.
        let started = std::time::Instant::now();
        let r = call(&mut rt, &mut sink, "task", r#"{"prompt": "a long job"}"#);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(
            matches!(
                r.outcome,
                letibot_transcript::ToolOutcome::Backgrounded { .. }
            ),
            "{:?}",
            r.outcome
        );
        assert!(r.payload.contains("task_result"), "{}", r.payload);

        // Collecting before it is done is the WAIT ending, not the child ending:
        // an abstention, which `is_grounded` will not treat as an answer.
        let waited = call(
            &mut rt,
            &mut sink,
            "task_result",
            r#"{"task": "sub-slow", "timeout_ms": 5}"#,
        );
        assert!(
            matches!(
                waited.outcome,
                letibot_transcript::ToolOutcome::Abstained { .. }
            ),
            "{:?}",
            waited.outcome
        );
        assert!(waited.payload.contains("still going"), "{}", waited.payload);

        release.store(true, Ordering::SeqCst);
        let done = call(&mut rt, &mut sink, "task_result", r#"{"task": "sub-slow"}"#);
        assert_eq!(done.payload, "the child answered");
        assert!(
            matches!(done.outcome, letibot_transcript::ToolOutcome::Ok),
            "{:?}",
            done.outcome
        );

        // A handle nobody started is a failure that says how to find the ones
        // that were.
        let miss = call(&mut rt, &mut sink, "task_result", r#"{"task": "sub-nope"}"#);
        assert!(matches!(
            miss.outcome,
            letibot_transcript::ToolOutcome::Failed { .. }
        ));
        let listed = call(&mut rt, &mut sink, "task_result", "{}");
        assert!(listed.payload.contains("sub-slow"), "{}", listed.payload);
    }

    #[test]
    fn a_runner_is_delegated_to_and_no_runner_refuses() {
        let e = Echo::default();
        let h = e.start("find the bug", &spec("coder")).unwrap();
        assert_eq!(
            e.collect(&h, std::time::Duration::ZERO),
            TaskStatus::Done {
                answer: "coder [none] @host: find the bug".into()
            }
        );
        assert!(
            NoTaskRunner
                .start("x", &spec("coder"))
                .unwrap_err()
                .contains("no subagent runner")
        );
        assert_eq!(
            NoTaskRunner.collect("anything", std::time::Duration::ZERO),
            TaskStatus::Unknown
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
        let runner: Arc<dyn TaskRunner> = Arc::new(Echo::default());
        reg.register(Box::new(TaskTool::new(runner.clone())))
            .unwrap();
        reg.register(Box::new(TaskResultTool::new(runner))).unwrap();
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
        // `task` hands back a handle; the spec it parsed is what the subagent was
        // started with, and `task_result` is where the answer comes from.
        let started = run(
            &mut rt,
            &mut sink,
            r#"{"prompt": "survey the crates", "role": "researcher", "access": "read-only", "where": "firecode"}"#,
        );
        assert!(started.contains("started subagent `sub-1`"), "{started}");
        assert!(started.contains("survey the crates"), "{started}");
        assert!(started.contains("task_result"), "{started}");
        let collected = rt
            .invoke(
                "t1",
                &ToolCall {
                    id: "c1".into(),
                    name: "task_result".into(),
                    arguments: r#"{"task": "sub-1"}"#.into(),
                },
                &mut sink,
            )
            .payload;
        assert_eq!(
            collected, "researcher [no write, no exec, no network] @firecode: survey the crates",
            "the spec `task` parsed is the spec the subagent ran under"
        );
        let p = run(&mut rt, &mut sink, r#"{"prompt": "x", "access": "root"}"#);
        assert!(p.contains("not a downgrade"), "{p}");
        let p = run(&mut rt, &mut sink, r#"{"prompt": "x", "where": "moon"}"#);
        assert!(p.contains("not a placement"), "{p}");
    }
}
