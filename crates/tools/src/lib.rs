//! The tool runtime and the built-ins: everything between a `ToolCall` leaving the
//! parser and a `TranscriptItem::ToolResult` returning to the transcript.
//!
//! A miss is self-correcting in the same call ([`builtins`]), malformed input is
//! salvaged ([`args`]), outcomes are closed ([`result`]), the gate sees every non-read
//! ([`schema::Access`]), and output is spilled, never truncated ([`spill`]).
//!
//! A tool schema is a stable prefix, so every tool ships even where this box cannot
//! back it: the refusal comes from the gate or the backend, and it names which.
//!
//! ```text
//!   ToolCall ──salvage──> args ──gate?──> Tool::invoke ──spill──> ToolResult
//!    (clause 2)                (clause 4)   (clause 1)  (clause 5)   (clause 3)
//!                                  │           │                        │
//!                            Adjudicator  ExecBackend            FileEdit ─> a head
//!                            (§11.2)      (the W9/W10 seam)      (both sides)
//! ```

pub mod adjudicate;
pub mod args;
pub mod attach;
pub mod authorise;
pub mod backend;
pub mod builtins;
/// Which bash an operator's own `!` line is handed to: `exec::console`'s flags, with the
/// program chosen from what this box has, because a stock Mac's `/bin/bash` is 3.2 and
/// the rc is written for 4 or later.
pub mod console_bash;
/// A development instrument, not a runtime dependency: every gate decision a store
/// recorded, replayed through today's parser and gate, so "the parser reads more than it
/// did" is a number rather than an impression.
pub mod corpus_replay;
pub mod detect;
pub mod edit;
pub mod events;
pub mod exec;
pub mod files;
pub mod firecode;
// `fanout` and `scenario`, and the engine under them: work on many hosts at once, each
// leg a job of that host's own table and a `process` record the daemon's supervisor owns.
pub mod fleet;
pub mod gatekeeper;
pub mod grant;
pub mod head_run;
pub mod heredoc;
pub mod intent;
pub mod lease;
pub mod media;
pub mod mode;
pub mod notes_keeper;
pub mod permission;
pub mod project_code;
pub mod provenance;
pub mod references;
pub mod remote;
pub mod result;
pub mod rpc;
pub mod runtime;
pub mod schema;
pub mod similarity;
pub mod spill;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use adjudicate::{
    ActionClass, Adjudicable, AdjudicatedGate, AdjudicationDecision, AdjudicationRequest,
    AdjudicationRow, Adjudicator, AskAdjudicator, ConsoleAdjudicator, Cost, DecisionOption,
    DecisionOutcome, EffectScope, FlowRule, NEVER_WRITE, NoAdjudicator, OnTimeout, OptionKind,
    RequestKind, Reversibility, Tier, always_ask_options, permission_options,
};
pub use args::{Repair, SalvageError, Salvaged, salvage};
pub use attach::NotAttached;
pub use authorise::{
    AuthorisationOracle, AuthorisationTrail, Breaker, BreakerState, Budgeted, CorpusRow,
    CorpusSink, DenialNotice, DenialSink, ModelAdjudicator, ModelBrief, OperatorOverride,
    OracleAnswer, OracleScope, RecordingCorpusSink, RecordingDenialSink, ScriptedOracle, Speaker,
    TaskDirection, TrailProvenance, UnsureKind, Utterance, Widening, refusal_text,
};
pub use backend::{BackendError, Command, DirEntry, ExecBackend, HostBackend, Output};
pub use builtins::external::{ExternalBackends, ExternalDisclosure, ExternalWiring};
pub use edit::{ChangedSpan, FileEdit, FileText, Relax};
pub use events::{NullToolSink, RecordingToolSink, ToolEvent, ToolEventSink, payload_digest};
pub use exec::{
    Cgroup2, ExecError, HostProcesses, JobId, JobState, JobView, NoScopes, ProcessHost, Reaped,
    Reaping, ScopeId, ScopeKind, ScopeTree, SpawnRequest, Waited, exec_budget, host_tree,
};
pub use files::{FileLedger, Seen};
pub use intent::{
    ALWAYS_ASK, AlwaysAskRule, Baseline, BaselineVerdict, Intent, Region, ScopedIntent, SecretFlow,
    ShellTrust, Surroundings,
};
pub use result::{Envelope, Propagation, ToolResult, propagate};
pub use runtime::{
    DEFAULT_MAX_TOOLS, Gate, GateCall, GateDecision, Invocation, InvokeCtx, Limits, NoBoundary,
    RegisterError, Registry, Role, RoleError, Tool, ToolRuntime, command_of_argv, roles,
};
pub use schema::{Access, DescriptionFinding, ToolSchema, lint_description};
pub use spill::{
    FixedBudget, InlineBudget, MemoryStore, NoBudget, PerToolBudget, PredictedBudget, SpillContext,
    SpillEntry, SpillRef, SpillStore, Spiller,
};

/// The read-only tool set, in the order the prompt lists them. Order is part of the stable prefix, so
/// reordering these re-prefills the whole conversation: it is written down once rather than assembled
/// per session.
pub fn read_only_tools(
    retrieval: std::sync::Arc<dyn builtins::retrieval::Retrieval>,
) -> Result<Registry, RegisterError> {
    use builtins::retrieval::Ask;
    let mut reg = Registry::new();
    reg.register(Box::new(builtins::read::Read))?;
    reg.register(Box::new(builtins::grep::Grep))?;
    reg.register(Box::new(builtins::glob::Glob))?;
    reg.register(Box::new(builtins::outline::OutlineTool))?;
    reg.register(Box::new(Ask::code(retrieval.clone())))?;
    reg.register(Box::new(Ask::corpus(retrieval)))?;
    reg.register(Box::new(builtins::read_spill::ReadSpill))?;
    // `hosts` reads the operator's file at every call, so a machine added mid-session is
    // visible: a list in the prompt would go stale, and the prompt is a cached prefix, so
    // refreshing it would cost a re-prefill. It needs no infrastructure, so it lives here.
    reg.register(Box::new(builtins::hosts::HostsTool::new()))?;
    Ok(reg)
}

/// Add the session tool (`todo_write`) to a registry. It is not in [`read_only_tools`] because its board
/// is the session's own, yet the read-only roles name it, so every registry they resolve against must
/// hold it.
pub fn with_session_tools(
    mut reg: Registry,
    board: std::sync::Arc<builtins::todo::TodoBoard>,
    children: Option<std::sync::Arc<dyn builtins::todo::ChildTodos>>,
    task_runner: std::sync::Arc<dyn builtins::task::TaskRunner>,
    skills: std::sync::Arc<builtins::skill::SkillRegistry>,
    lsp: std::sync::Arc<builtins::lsp::LspConfig>,
) -> Result<Registry, RegisterError> {
    let todo = match children {
        Some(children) => builtins::todo::TodoWriteTool::with_children(board, children),
        None => builtins::todo::TodoWriteTool::new(board),
    };
    reg.register(Box::new(todo))?;
    // The children read the same corpus as the tool, so a `task {skills}` name is checked
    // against what `skill list` answers: a second registry would be a second answer to
    // "is there a skill called this".
    reg.register(Box::new(builtins::skill::SkillTool::new(skills.clone())))?;
    reg.register(Box::new(builtins::lsp::LspTool::new(lsp)))?;
    reg.register(Box::new(
        builtins::task::TaskTool::new(task_runner.clone()).with_skills(skills.clone()),
    ))?;
    // `task_result` is seated with `task` and never without it: a handle that nothing can
    // collect is worse than no handle.
    reg.register(Box::new(builtins::task::TaskResultTool::new(
        task_runner.clone(),
    )))?;
    // So is `task_message`: a seat that can start a child, read it and stop it but not
    // correct it has only two remedies when the child goes wrong, wait out a wrong answer
    // or throw the work away with `job_kill`.
    reg.register(Box::new(builtins::task::TaskMessageTool::new(
        task_runner.clone(),
    )))?;
    // And so is `task_start`, with the same runner as `task` because it is the same spawn
    // with the placement arranged first: a second runner would hand out handles that
    // `task_result` cannot collect.
    reg.register(Box::new(
        builtins::task::TaskStartTool::new(task_runner).with_skills(skills),
    ))?;
    // The merge queue, read: what a repository's queue runs before landing and what its
    // files suggest. Read-only, because the agent writes the `AGENTS.md` section with `edit`
    // and that is where the operator's approval is.
    reg.register(Box::new(builtins::merge_gate::MergeGate))?;
    // The plugin loop: a session's model writes a pane, builds it, tests it and loads it hot.
    // Seated with the daemon's own desk by `Harness::open`; here so a role that names the pair
    // resolves and says by name that no plugin host is behind it, rather than being absent.
    if reg.get(builtins::plugin::TOOL).is_none() {
        let desk: std::sync::Arc<dyn builtins::plugin::PluginDesk> =
            std::sync::Arc::new(builtins::plugin::NoPlugins);
        reg.register(Box::new(builtins::plugin::PluginTool::new(desk.clone())))?;
        reg.register(Box::new(builtins::plugin::PluginHooksTool::new(desk)))?;
    }
    // The merge queue from the model's side. The daemon registers the real one first, bound to
    // its session and store; anywhere else the seat still resolves and the tool says there is
    // no queue behind it.
    if reg.get(builtins::queue::TOOL).is_none() {
        reg.register(Box::new(builtins::queue::QueueTool::new(
            std::sync::Arc::new(builtins::queue::NoQueue),
        )))?;
    }
    // The host's room, the same shape as `queue` one arm up and for the same reason: a seat
    // that names `room` resolves on every registry this function builds and the tool then says
    // there is no coordinator behind it. The daemon replaces it with the session's own link.
    if reg.get(builtins::room::TOOL).is_none() {
        reg.register(Box::new(builtins::room::RoomTool::new(
            std::sync::Arc::new(builtins::room::NoRoom),
        )))?;
    }
    // The host's own changelog, the same shape as `room` one arm up: with no config dir
    // behind it the tool says so rather than guessing one and writing into the operator's
    // real one. The daemon replaces it with the session's own scope.
    if reg.get(builtins::changelog::TOOL).is_none() {
        reg.register(Box::new(builtins::changelog::HostChangelogTool::nowhere()))?;
    }
    // This box's own settings, and the same shape one arm down: `read` and `settings` still
    // answer because they change nothing, and `set` refuses before it runs anything.
    if reg.get(builtins::host_config::TOOL).is_none() {
        reg.register(Box::new(builtins::host_config::HostConfigTool::nowhere()))?;
    }
    Ok(reg)
}

/// Add the tools that need infrastructure this box does not run. Registered, not seated: the registry is
/// a superset and the role decides what a session's prompt carries. MCP tools are not here, since their
/// schemas are the server's.
pub fn external_tools(
    mut reg: Registry,
    backends: &ExternalBackends,
) -> Result<Registry, RegisterError> {
    use builtins::external::{
        github::Github,
        web::{WebFetch, WebSearch},
    };
    reg.register(Box::new(WebSearch::new(backends.search.clone())))?;
    reg.register(Box::new(WebFetch::new(backends.fetch.clone())))?;
    reg.register(Box::new(Github::new(backends.github.clone())))?;
    Ok(reg)
}

/// The M2 tool set: the read-only ones plus `write` and `edit`. Registering them is not being able to use
/// them: the gate must admit the call, since the default boundary refuses, and the backend must have been
/// opened writable. A refusal names which of the two stopped it.
pub fn coder_tools(
    retrieval: std::sync::Arc<dyn builtins::retrieval::Retrieval>,
) -> Result<Registry, RegisterError> {
    let mut reg = read_only_tools(retrieval)?;
    reg.register(Box::new(builtins::write::Write))?;
    reg.register(Box::new(builtins::edit::Edit))?;
    Ok(reg)
}

/// The exec tool set: `bash`, the job tools and `monitor`, on top of the read-only ones. The same three
/// mechanisms one capability further out, and a backend opened executable fails rather than degrading
/// without a cgroup v2 subtree, since a process with no owner is a leak.
pub fn runner_tools(
    retrieval: std::sync::Arc<dyn builtins::retrieval::Retrieval>,
) -> Result<Registry, RegisterError> {
    let mut reg = read_only_tools(retrieval)?;
    reg.register(Box::new(builtins::bash::Bash::default()))?;
    reg.register(Box::new(builtins::jobs::JobList))?;
    reg.register(Box::new(builtins::jobs::JobOutput))?;
    reg.register(Box::new(builtins::jobs::JobWait))?;
    reg.register(Box::new(builtins::jobs::JobKill::new()))?;
    reg.register(Box::new(builtins::monitor::Monitor))?;
    // Work on many machines, registered with the exec surface and proved by nothing here: a registry that
    // names a role's tool and does not hold it is refused at resolve. The daemon binds the process book so
    // a started branch is a supervisor record.
    reg.register(Box::new(builtins::fanout::Fanout::new()))?;
    reg.register(Box::new(builtins::scenario::Scenario::new()))?;
    Ok(reg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_m1_tool_set_fits_under_the_ceiling() {
        // The seat's ceiling is a hard stop, and the set that ships must be seatable by
        // the role that ships it.
        let reg = read_only_tools(std::sync::Arc::new(builtins::retrieval::Unavailable))
            .expect("the read-only registry");
        // `todo_write` is a session tool, so it is not in `read_only_tools` - but the role
        // names it, and a role naming a tool nobody registers is the failure this test
        // exists to catch.
        let reg = with_session_tools(
            reg,
            std::sync::Arc::new(builtins::todo::TodoBoard::new(Vec::new())),
            None,
            std::sync::Arc::new(builtins::task::NoTaskRunner),
            std::sync::Arc::new(builtins::skill::SkillRegistry::default()),
            std::sync::Arc::new(builtins::lsp::LspConfig::default()),
        )
        .expect("todo_write registers");
        // The ceiling binds what a SESSION is given, not what the registry holds: this one
        // is a superset on purpose, so the number that has to fit is the resolved seat's.
        let seated = reg.resolve_role(&roles::m1_orchestrator()).unwrap();
        // `room` resolves here because `with_session_tools` registers a `NoRoom` behind it -
        // the rule this test is about: a role that names a tool must resolve on any registry
        // this crate builds, and the tool then says what is not behind it.
        assert_eq!(seated.len(), 8, "{:?}", seated.names());
        assert!(seated.len() <= DEFAULT_MAX_TOOLS, "{} tools", seated.len());
    }

    #[test]
    fn every_built_in_declares_read_access_and_a_usage_description() {
        let reg = read_only_tools(std::sync::Arc::new(builtins::retrieval::Unavailable)).unwrap();
        for s in reg.schemas() {
            assert_eq!(s.access, Access::Read, "{} is not read-only", s.name);
            assert_eq!(
                lint_description(&s.description),
                vec![],
                "{}'s description says what the data contains",
                s.name
            );
            assert!(
                !s.description.is_empty() && s.description.len() < 800,
                "{}'s description is {} bytes",
                s.name,
                s.description.len()
            );
        }
    }

    #[test]
    fn the_prompt_json_is_stable_across_builds() {
        // The stable prefix is content-addressed, so this string is a cache key.
        let reg = read_only_tools(std::sync::Arc::new(builtins::retrieval::Unavailable)).unwrap();
        let a = reg.tools_json();
        let b = reg.tools_json();
        assert_eq!(a, b);
        assert!(a[0].starts_with(r#"{"type":"function","function":{"name":"read","#));
    }
}
