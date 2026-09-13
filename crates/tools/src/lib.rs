//! The tool runtime and the built-ins (W9, W10): §8.
//!
//! # What this crate is
//!
//! Everything between a `ToolCall` coming out of the parser and a
//! `TranscriptItem::ToolResult` going back into the transcript, plus the tools
//! themselves: `read`, `grep`, `glob`, `ask_code`, `ask_corpus` and `read_spill`
//! (which clause 5 requires in order to be honest), and — since W10 — `write` and
//! `edit`, the first two that can change the operator's tree.
//!
//! ```text
//!   ToolCall ──salvage──> args ──gate?──> Tool::invoke ──spill──> ToolResult
//!    (clause 2)                (clause 4)   (clause 1)  (clause 5)   (clause 3)
//!                                  │           │                        │
//!                            Adjudicator  ExecBackend            FileEdit ─> a head
//!                            (§11.2)      (the W9/W10 seam)      (both sides)
//! ```
//!
//! # The six clauses, and where each is kept honest
//!
//! §8.1 says *"a tool that does not meet all six does not ship"*, so each one has a
//! home and a test that fails when it stops being true.
//!
//! 1. **A miss is self-correcting in the SAME call.** [`builtins`], per tool, and
//!    `tests/clauses.rs` holds the plan's three acceptance cases verbatim.
//! 2. **Malformed input is salvaged, not rejected.** [`args`], with the repairs
//!    carried onto the result rather than applied quietly.
//! 3. **Outcome is a closed vocabulary and abstention is not success.**
//!    [`result`] — the `NO_RESULT` envelope and [`result::propagate`].
//! 4. **Read/write declared in the schema.** [`schema::Access`], and
//!    [`runtime::ToolRuntime::invoke`] consults the gate only for what is not a
//!    read — and always for what is. [`adjudicate`] is what is behind it.
//! 5. **Bounded and spilled, never truncated.** [`spill`], with D6's decider
//!    interface rather than a constant.
//! 6. **The description says how to use the tool.** [`schema::lint_description`],
//!    enforced at registration.
//!
//! # Two gates below a write, and neither is the default
//!
//! W10 built the write tools; it did not build §11.4's boundary, which is a
//! substrate rather than a policy. What it built instead is the smallest thing
//! that genuinely decides, plus a second mechanism that does not depend on it:
//!
//! 1. **the adjudicator** ([`adjudicate`]). [`runtime::NoBoundary`] and
//!    [`adjudicate::NoAdjudicator`] are the defaults and both refuse, with
//!    `NotRun` — *nobody decided* — rather than `Denied`;
//! 2. **the backend** ([`backend::HostBackend::writable`], which is a different
//!    constructor from [`backend::HostBackend::new`]). A session that did not ask
//!    to be writable cannot write however the gate answers.
//!
//! A refusal always names which of the two stopped it.
//!
//! # What this crate deliberately does not do
//!
//! - **Layer 1 of the boundary, and not layers 2 or 3.**
//!   `docs/boundary-and-adjudication.md` §4: [`exec::scope`] is the *lifetime* half
//!   — every process in a cgroup owned by a scope — and [`exec::confine`] is the
//!   *view* half: project-scoped mount, PID, network and user namespaces, so a
//!   secret outside the project is **absent** rather than denied.
//!   [`backend::HostBackend::confined`] is the constructor that has both;
//!   [`backend::HostBackend::executable`] is the lifetime half alone and its
//!   `describe()` says `NOT CONFINED`. Neither ever *silently* has no boundary:
//!   asked-for-and-missing is [`exec::NoConfinement`], which refuses every spawn.
//!
//!   What is **not** here is §3's other half. The mount view keeps secret bytes out
//!   of the view; *never entering the transcript* needs one choke point through
//!   which every tool result passes, and each tool still builds its own body
//!   (`docs/boundary-and-adjudication.md` §5). Nor is the normalisation layer here
//!   (tree-sitter over the command) or the adjudicator (§4's layers 2 and 3) —
//!   `bash` is still seated only by [`roles::m2_runner`].
//! - **No §11.3 policy table and no auto mode.** One adjudicator is attached per
//!   session, not a table of them; [`adjudicate::ActionClass`] is the routing key
//!   that table will use, derived and logged from the first call.
//! - **No differ.** `crates/ui` owns one. [`edit::FileEdit`] carries both sides of
//!   a change so that it can be used; writing a second one here is how two
//!   answers to one question start disagreeing.
//! - **No firecode backend.** One `Box<dyn ExecBackend>`, by construction: no tool
//!   in this crate calls `std::fs`.
//! - **No MCP client, no HTTP client, no search provider and no forge credential.**
//!   [`builtins::retrieval::Retrieval`] and the four seams in
//!   [`builtins::external`] are where they attach; see those modules for why a
//!   transport written blind, against a server nobody here can reach, would be
//!   worse than none. The *tools* ship — with their real schemas, declaring
//!   [`Access::Network`], refusing with `NotRun` — because a tool schema is stable
//!   prefix and adding one later re-prefills every conversation.
//! - **No async runtime and no HTTP.** The same argument the turn engine makes:
//!   the whole thing is a synchronous function of a call and a filesystem.

pub mod adjudicate;
pub mod args;
pub mod attach;
pub mod authorise;
pub mod backend;
pub mod builtins;
pub mod edit;
pub mod events;
pub mod exec;
pub mod files;
pub mod grant;
pub mod intent;
pub mod mode;
pub mod permission;
pub mod result;
pub mod runtime;
pub mod schema;
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
    DenialNotice, DenialSink, ModelAdjudicator, ModelBrief, OperatorOverride, OracleAnswer,
    OracleScope, RecordingDenialSink, ScriptedOracle, Speaker, TaskDirection, TrailProvenance,
    Utterance, Widening, refusal_text,
};
pub use backend::{BackendError, Command, DirEntry, ExecBackend, HostBackend, Output};
pub use builtins::external::{ExternalBackends, ExternalDisclosure, ExternalWiring};
pub use edit::{ChangedSpan, FileEdit, FileText, Relax};
pub use events::{NullToolSink, RecordingToolSink, ToolEvent, ToolEventSink, payload_digest};
pub use exec::{
    Cgroup2, ExecError, HostProcesses, JobId, JobState, JobView, NoScopes, ProcessHost, Reaped,
    Reaping, ScopeId, ScopeKind, ScopeTree, SpawnRequest, Waited, exec_budget,
};
pub use files::{FileLedger, Seen};
pub use intent::{
    ALWAYS_ASK, AlwaysAskRule, Baseline, BaselineVerdict, Intent, Region, ScopedIntent, SecretFlow,
    ShellTrust, Surroundings,
};
pub use result::{Envelope, Propagation, ToolResult, propagate};
pub use runtime::{
    DEFAULT_MAX_TOOLS, Gate, GateCall, GateDecision, Invocation, InvokeCtx, Limits, NoBoundary,
    RegisterError, Registry, Role, RoleError, Tool, ToolRuntime, roles,
};
pub use schema::{Access, DescriptionFinding, ToolSchema, lint_description};
pub use spill::{
    FixedBudget, InlineBudget, MemoryStore, NoBudget, PerToolBudget, PredictedBudget, SpillContext,
    SpillEntry, SpillRef, SpillStore, Spiller,
};

/// The read-only tool set M1 ships, in the order the prompt lists them.
///
/// Order is part of the stable prefix — reordering these re-prefills the whole
/// conversation — so it is written down here once rather than assembled per
/// session.
///
/// Six tools, against §8.4's ceiling of eight. That is not an accident: the table
/// gives the `orchestrator` role six, and `read_spill` replaces `task`, which is
/// W16's.
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
    Ok(reg)
}

/// Add the session tool (`todo_write`) to a registry.
///
/// It is not in [`read_only_tools`] because its board is the session's own, restored
/// from the store: the real harness passes the restored board, and a test passes an
/// empty one. The read-only roles name `todo_write`, so any registry they are
/// resolved against has to include it — this is the one registration that makes
/// those roles resolvable, and the one the harness and the test harness share.
pub fn with_session_tools(
    mut reg: Registry,
    board: std::sync::Arc<builtins::todo::TodoBoard>,
    task_runner: std::sync::Arc<dyn builtins::task::TaskRunner>,
    skills: std::sync::Arc<builtins::skill::SkillRegistry>,
    lsp: std::sync::Arc<builtins::lsp::LspConfig>,
) -> Result<Registry, RegisterError> {
    reg.register(Box::new(builtins::todo::TodoWriteTool::new(board)))?;
    reg.register(Box::new(builtins::skill::SkillTool::new(skills)))?;
    reg.register(Box::new(builtins::lsp::LspTool::new(lsp)))?;
    reg.register(Box::new(builtins::task::TaskTool::new(task_runner)))?;
    Ok(reg)
}

/// Add the tools that need infrastructure this box does not run.
///
/// `web_search`, `web_fetch` and `github`, over whatever [`ExternalBackends`] the
/// session was given — which is [`ExternalBackends::unattached`] here, so all three
/// refuse. They are **registered, not seated**: the registry is a superset and
/// [`Registry::resolve_role`] decides what a session's prompt actually carries,
/// because §8.4's ceiling is eight and these three plus the read-only seven are
/// ten.
///
/// MCP tools are not registered here. Their schemas are the server's, so they are
/// mounted from a catalog at open time — [`builtins::external::mcp::mount`], which
/// is also where §8.4's *"refused with its tool list, not silently truncated"*
/// lives.
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

/// The M2 tool set: the read-only five plus `write` and `edit`.
///
/// Six seatable under [`roles::m2_coder`], against §8.4's ceiling of eight.
///
/// # Registering these is not the same as being able to use them
///
/// Three things must all be true before a byte reaches the operator's disk, and
/// they are three separate mechanisms on purpose:
///
/// 1. the tools are registered — this function;
/// 2. an adjudicator is attached, so [`runtime::Gate`] admits the call. The
///    default is [`NoBoundary`], which refuses. See [`adjudicate`];
/// 3. the backend was opened writable — [`HostBackend::writable`], not
///    [`HostBackend::new`].
///
/// Doing (1) alone gives a session whose `edit` refuses at the gate; doing (1)
/// and (2) gives one whose `edit` refuses at the backend, naming which. Neither
/// silently does nothing, and neither silently works.
pub fn coder_tools(
    retrieval: std::sync::Arc<dyn builtins::retrieval::Retrieval>,
) -> Result<Registry, RegisterError> {
    let mut reg = read_only_tools(retrieval)?;
    reg.register(Box::new(builtins::write::Write))?;
    reg.register(Box::new(builtins::edit::Edit))?;
    Ok(reg)
}

/// The exec tool set: `bash` and the four job-control tools, on top of the
/// read-only ones.
///
/// # Three mechanisms below a command, and none of them is the default
///
/// Same shape as [`coder_tools`], one capability further out:
///
/// 1. the tools are registered — this function;
/// 2. an adjudicator is attached, so [`runtime::Gate`] admits `bash` and
///    `job_kill`. The default is [`NoBoundary`], which refuses with `NotRun` —
///    *nobody decided*;
/// 3. the backend was opened with [`HostBackend::executable`], which needs a
///    delegated cgroup v2 subtree. **Without one it fails rather than degrading**:
///    a process with no owner is the leak `TODO.md` T24 exists to stop.
///
/// And a fourth thing that is not a mechanism but a seat: `bash` is reachable only
/// under [`roles::m2_runner`]. [`roles::m1_orchestrator`] and [`roles::m2_coder`]
/// are unchanged, so no session that exists today gains an exec path by this
/// function existing.
pub fn runner_tools(
    retrieval: std::sync::Arc<dyn builtins::retrieval::Retrieval>,
) -> Result<Registry, RegisterError> {
    let mut reg = read_only_tools(retrieval)?;
    reg.register(Box::new(builtins::bash::Bash))?;
    reg.register(Box::new(builtins::jobs::JobList))?;
    reg.register(Box::new(builtins::jobs::JobOutput))?;
    reg.register(Box::new(builtins::jobs::JobWait))?;
    reg.register(Box::new(builtins::jobs::JobKill))?;
    reg.register(Box::new(builtins::monitor::Monitor))?;
    Ok(reg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_m1_tool_set_fits_under_the_ceiling() {
        // §8.4 is a hard stop, not a guideline, and the set that ships must be
        // seatable by the role that ships it.
        let reg = read_only_tools(std::sync::Arc::new(builtins::retrieval::Unavailable))
            .expect("the read-only registry");
        // `todo_write` is a session tool, not a read-only one, so it is not in
        // `read_only_tools` — but the role names it, so the registry the role is
        // resolved against has to know it. That is exactly the failure shape the
        // ceiling test exists to catch: a role that names a tool nobody registers.
        let reg = with_session_tools(
            reg,
            std::sync::Arc::new(builtins::todo::TodoBoard::new(Vec::new())),
            std::sync::Arc::new(builtins::task::NoTaskRunner),
            std::sync::Arc::new(builtins::skill::SkillRegistry::default()),
            std::sync::Arc::new(builtins::lsp::LspConfig::default()),
        )
        .expect("todo_write registers");
        assert!(reg.len() <= DEFAULT_MAX_TOOLS, "{} tools", reg.len());
        let seated = reg.resolve_role(&roles::m1_orchestrator()).unwrap();
        assert_eq!(seated.len(), 7);
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
