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
//! - **No exec tool.** `bash` needs §11.4's boundary — *the guest sees a copy of
//!   one project and nothing else of the host* — and no adjudicator on this side
//!   of that boundary can make an unsandboxed shell on the operator's box into
//!   the thing the plan describes. [`backend::ExecBackend::run`] is named and
//!   [`backend::HostBackend`] refuses it.
//! - **No §11.3 policy table and no auto mode.** One adjudicator is attached per
//!   session, not a table of them; [`adjudicate::ActionClass`] is the routing key
//!   that table will use, derived and logged from the first call.
//! - **No differ.** `crates/ui` owns one. [`edit::FileEdit`] carries both sides of
//!   a change so that it can be used; writing a second one here is how two
//!   answers to one question start disagreeing.
//! - **No firecode backend.** One `Box<dyn ExecBackend>`, by construction: no tool
//!   in this crate calls `std::fs`.
//! - **No MCP client.** [`builtins::retrieval::Retrieval`] is the seam; see that
//!   module for why a client written blind would be worse than none.
//! - **No async runtime and no HTTP.** The same argument the turn engine makes:
//!   the whole thing is a synchronous function of a call and a filesystem.

pub mod adjudicate;
pub mod args;
pub mod backend;
pub mod builtins;
pub mod edit;
pub mod events;
pub mod files;
pub mod result;
pub mod runtime;
pub mod schema;
pub mod spill;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use adjudicate::{
    ActionClass, AdjudicatedGate, Adjudicator, AdjudicationDecision, AdjudicationRequest,
    AdjudicationRow, AskAdjudicator, ConsoleAdjudicator, Cost, DecisionOption, DecisionOutcome,
    EffectScope, NEVER_WRITE, NoAdjudicator, OnTimeout, OptionKind, RequestKind, Reversibility,
    permission_options,
};
pub use args::{Repair, SalvageError, Salvaged, salvage};
pub use backend::{BackendError, Command, DirEntry, ExecBackend, HostBackend, Output};
pub use edit::{ChangedSpan, FileEdit, FileText, Relax};
pub use events::{NullToolSink, RecordingToolSink, ToolEvent, ToolEventSink, payload_digest};
pub use files::{FileLedger, Seen};
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_m1_tool_set_fits_under_the_ceiling() {
        // §8.4 is a hard stop, not a guideline, and the set that ships must be
        // seatable by the role that ships it.
        let reg = read_only_tools(std::sync::Arc::new(builtins::retrieval::Unavailable)).unwrap();
        assert!(reg.len() <= DEFAULT_MAX_TOOLS, "{} tools", reg.len());
        let seated = reg.resolve_role(&roles::m1_orchestrator()).unwrap();
        assert_eq!(seated.len(), 6);
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
