//! The runtime: what happens between a `ToolCall` coming out of the parser and a
//! `TranscriptItem::ToolResult` going back in.
//!
//! Every clause that is not a property of one tool lives here.
//!
//! | clause | here |
//! |---|---|
//! | 2 — malformed input is salvaged | [`ToolRuntime::invoke`] calls [`crate::args::salvage`] and carries the repairs onto the result |
//! | 3 — abstention is not success | the outcome comes from the tool and is never widened; [`crate::result::propagate`] is the caller's half |
//! | 4 — read/write declared in the schema | [`ToolRuntime::invoke`] consults the [`Gate`] **only** for a non-`Read` tool |
//! | 5 — bounded and spilled | [`crate::spill::Spiller`] runs on every payload |
//! | 6 — the description is usage | [`Registry::register`] refuses a description that fails the lint |
//! | §8.4 — tool budget | [`Role`] and [`Registry::resolve_role`], a refusal and not a guideline |
//!
//! Clause 1 is the tools' own, because it is a different sentence for every tool.
//! What the runtime does about it is smaller and easy to miss: an **unknown tool
//! name** is itself a miss, and it comes back with the tool list and the nearest
//! match rather than with "unknown tool".

use letibot_transcript::{ToolCall, ToolOutcome, TranscriptItem};
use serde_json::Value;

use crate::args::{Repair, salvage};
use crate::backend::ExecBackend;
use crate::events::{ToolEvent, ToolEventSink, payload_digest};
use crate::result::ToolResult;
use crate::schema::{Access, ToolSchema, lint_description};
use crate::spill::{SpillContext, Spiller};

/// What a tool hands back. The outcome is the tool's decision and the runtime
/// never widens it.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub outcome: ToolOutcome,
    pub payload: String,
    /// Clause 1's voice: what the tool did about a miss.
    pub notes: Vec<String>,
    /// Both sides of a file this call changed, for a head to draw.
    ///
    /// `None` for every tool that changed nothing, **including a write tool whose
    /// call was a no-op** — a head asked to render a diff of no change would draw
    /// an empty card, and an empty card is indistinguishable from a bug. See
    /// [`crate::edit::FileEdit`] for what the head does with it and for why it is
    /// here rather than in a [`crate::events::ToolEvent`].
    pub edit: Option<crate::edit::FileEdit>,
}

impl Invocation {
    pub fn ok(payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::Ok,
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
        }
    }

    /// No result, and the body is whatever helps the caller correct itself.
    pub fn abstained(reason: impl Into<String>, payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::Abstained {
                reason: reason.into(),
            },
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
        }
    }

    pub fn failed(reason: impl Into<String>, payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::Failed {
                reason: reason.into(),
            },
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

/// Bounds a tool must respect so that one call cannot occupy a session.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The most filesystem entries a walk may visit.
    pub max_walk_entries: usize,
    /// The most matches a search reports before it says it stopped counting.
    pub max_matches: usize,
    /// The most bytes a single file read returns before the spill policy sees it.
    pub max_file_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_walk_entries: 20_000,
            max_matches: 200,
            max_file_bytes: 4 * 1024 * 1024,
        }
    }
}

/// What a tool is given for one call.
pub struct InvokeCtx<'a> {
    pub backend: &'a dyn ExecBackend,
    pub spiller: &'a Spiller,
    /// What this session has shown the model, and therefore what it may change.
    /// See [`crate::files`]: the read-only tools write to it, the write tools
    /// read it, and it is on the context rather than on each tool so that two
    /// tools cannot end up with two ledgers.
    pub files: &'a crate::files::FileLedger,
    pub limits: Limits,
    turn_id: &'a str,
    call_id: &'a str,
    sink: &'a mut dyn ToolEventSink,
}

impl InvokeCtx<'_> {
    /// Say that the call is still working. §8.5 requires this to count as liveness,
    /// and a tool that walks a large tree must produce it.
    pub fn progress(&mut self, note: impl Into<String>) {
        self.sink.emit(ToolEvent::Progress {
            turn_id: self.turn_id.to_string(),
            call_id: self.call_id.to_string(),
            note: note.into(),
        });
    }

    pub fn call_id(&self) -> &str {
        self.call_id
    }

    /// The turn this call belongs to.
    ///
    /// Needed by any tool whose result is a fact *about the turn* rather than
    /// about the tree — the intent ledger keys on it, because "declared this turn
    /// and nothing ran this turn" is the whole of T21.3.
    pub fn turn_id(&self) -> &str {
        self.turn_id
    }
}

/// One tool.
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;

    /// `args` has already been salvaged against the schema, so a tool sees an
    /// object with the right key names and the right value types or nothing at
    /// all. What it must still handle is a *missing* argument, because supplying
    /// one would be inventing meaning.
    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation;
}

/// One gated call, as the gate sees it.
///
/// W9's `admit(name, access, args)` was the smallest thing the runtime needed and
/// `TODO.md` T16.3 said W11 should absorb it rather than build a parallel seam.
/// W10 is the first caller that needs the gate to *decide* something, and three
/// of §11.2's request fields could not be filled from the old signature: the turn
/// and call this belongs to (so an audit row can be correlated), and whether the
/// target exists (so [`crate::adjudicate::ActionClass`] can say whether the action
/// is reversible). They are here rather than smuggled through `args`.
#[derive(Debug, Clone, Copy)]
pub struct GateCall<'a> {
    /// The tool as **declared**, not as the model spelled it.
    pub name: &'a str,
    pub access: Access,
    /// Already salvaged (clause 2), so the gate reads the same values the tool will.
    pub args: &'a Value,
    pub turn_id: &'a str,
    pub call_id: &'a str,
    /// What the backend calls itself. `EXPLAIN` and the adjudication brief both
    /// want the operator to see where a write would land.
    pub workspace: &'a str,
    /// Whether the `path` argument names something that exists, when there is one.
    /// `None` means the call has no path argument to stat, which is a different
    /// fact from "the path is not there".
    pub target_exists: Option<bool>,
}

impl GateCall<'_> {
    /// Whether the `path` argument stays inside the workspace, decided
    /// **lexically** and before the tool runs.
    ///
    /// Lexical on purpose: §11.4 says an action *"whose class says `in_run` but
    /// whose arguments would leave the run"* escalates rather than hard-failing,
    /// and that is a property of the argument, not of what the filesystem happens
    /// to hold. [`crate::backend::HostBackend::resolve`] does the second, stricter
    /// check (it canonicalises, so it catches a symlink out of the tree); this one
    /// exists so the *class* is known before anything is opened.
    pub fn path_is_inside(&self) -> bool {
        let Some(path) = self.args.get("path").and_then(|v| v.as_str()) else {
            return true;
        };
        if path.starts_with('/') {
            return path.starts_with(self.workspace);
        }
        let mut depth: i32 = 0;
        for seg in path.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                _ => depth += 1,
            }
        }
        true
    }
}

/// §11's seam, from the tool side.
///
/// Consulted only for a call whose declared access is not [`Access::Read`], which
/// is clause 4 as a control-flow fact: there is no code path from a read-only tool
/// to a question.
///
/// `Send + Sync` closes `TODO.md` T20.4 — *"`ToolRuntime` is not `Send` — `Gate`
/// lacks `Send + Sync`, alone among the runtime's traits. Blocks §13.2's
/// multi-head worker."* It was left for W11 to absorb; absorbing it means writing
/// the bound down, and every implementation in the tree already satisfies it.
pub trait Gate: Send + Sync {
    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision;

    /// Who is adjudicating, for the daemon's startup disclosure.
    ///
    /// Defaults to naming the absence, because that is the case an operator most
    /// needs to see and the one a hard-coded banner gets wrong: a gate that does
    /// not identify an adjudicator does not have one.
    fn describe(&self) -> String {
        "none (no adjudicator attached)".into()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    Admit,
    /// The call does not happen, and this is the outcome it carries. `Denied` when
    /// somebody decided; `NotRun` when there was nobody to ask.
    Refuse {
        outcome: ToolOutcome,
        /// What the **model** is told, which is not the same as what the audit
        /// records. §11.5: *"the audit is log-only and never enters the model
        /// transcript … the model sees the derived tool outcome, not the
        /// deliberation."* An adjudicator that chose `deny_and_tell` puts its
        /// reason here; a plain `deny` leaves it empty and the reason stays in the
        /// row.
        tell: String,
    },
}

impl GateDecision {
    pub fn refuse(outcome: ToolOutcome) -> Self {
        GateDecision::Refuse {
            outcome,
            tell: String::new(),
        }
    }

    pub fn refuse_and_tell(outcome: ToolOutcome, tell: impl Into<String>) -> Self {
        GateDecision::Refuse {
            outcome,
            tell: tell.into(),
        }
    }
}

/// The gate for a session with **nothing attached**, and it refuses.
///
/// This is deliberately not a `DenyAll` that says "denied": `Denied` means a
/// decision was made, and no decision was made here. §8.2's discipline applied to
/// the outcome vocabulary itself.
///
/// [`crate::adjudicate::AdjudicatedGate::closed`] is the same fail-closed
/// behaviour reached through the full §11.2 shape, and it is what a session that
/// *could* have an adjudicator should use, because it produces an audit row. This
/// one produces none, which is right for a session that has no adjudication at
/// all.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoBoundary;

impl Gate for NoBoundary {
    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
        GateDecision::refuse(ToolOutcome::NotRun {
            why: format!(
                "`{}` declares {} access and no adjudicator is attached to this session, \
                 so there is nobody to decide whether it may run. The gate fails closed: \
                 nothing was executed and nothing on disk changed. This is not a denial — \
                 nobody decided. Attach an adjudicator (§11) to make this callable.",
                call.name,
                call.access.as_str()
            ),
        })
    }
}

/// A role's tool set, and §8.4's ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    pub tools: Vec<String>,
    pub max_tools: usize,
}

/// §8.4's default ceiling. *"Past ~5–7 MCP servers small models get worse at
/// choosing tools."*
pub const DEFAULT_MAX_TOOLS: usize = 8;

impl Role {
    pub fn new(name: &str, tools: &[&str]) -> Self {
        Role {
            name: name.to_string(),
            tools: tools.iter().map(|t| t.to_string()).collect(),
            max_tools: DEFAULT_MAX_TOOLS,
        }
    }
}

/// §8.4's table, verbatim. Roles naming tools that do not exist yet fail to
/// resolve, loudly, which is how M2's arrival is noticed rather than assumed.
pub mod roles {
    use super::Role;

    pub fn orchestrator() -> Role {
        Role::new(
            "orchestrator",
            &["task", "read", "grep", "glob", "ask_code", "ask_corpus"],
        )
    }

    pub fn coder() -> Role {
        Role::new(
            "coder",
            &[
                "read",
                "write",
                "edit",
                "grep",
                "glob",
                "bash",
                "read_spill",
            ],
        )
    }

    pub fn researcher() -> Role {
        Role::new(
            "researcher",
            &[
                "ask_corpus",
                "search_corpus",
                "ask_code",
                "read",
                "grep",
                "read_spill",
            ],
        )
    }

    pub fn reviewer() -> Role {
        Role::new("reviewer", &["read", "grep", "glob", "git", "read_spill"])
    }

    /// What M1 can actually seat: §8.4's `orchestrator` without `task`, which is
    /// W16's, plus `read_spill`, which is clause 5's own tool.
    pub fn m1_orchestrator() -> Role {
        Role::new(
            "orchestrator",
            &[
                "read",
                "grep",
                "glob",
                "ask_code",
                "ask_corpus",
                "read_spill",
            ],
        )
    }

    /// What a session in plan mode seats: nothing that can change the tree.
    ///
    /// Plan mode is a **capability boundary**, so this is a role and not a flag —
    /// the write tools are absent from `tools_json` rather than refused at call
    /// time. [`crate::builtins::intent::plan::seating`] derives the same answer
    /// from an arbitrary base role; this is the named one.
    pub fn planner() -> Role {
        Role::new(
            "planner",
            &[
                "read",
                "grep",
                "glob",
                "outline",
                "todo",
                "goal",
                "exit_plan_mode",
            ],
        )
    }

    /// What M2 can seat: §8.4's `coder` without `bash`.
    ///
    /// `bash` is `Access::Exec`, and `HostBackend`s `run` still
    /// refuses — an unadjudicated exec path on the host is the thing §11.4's
    /// boundary exists to prevent, and a boundary is not what W10 built. Six
    /// tools against a ceiling of eight; the two spare seats are `bash` and
    /// `task`, in that order, and neither is a stub.
    pub fn m2_coder() -> Role {
        Role::new(
            "coder",
            &["read", "write", "edit", "grep", "glob", "read_spill"],
        )
    }
}

#[derive(Debug)]
pub enum RegisterError {
    Duplicate(String),
    /// Clause 6, refused at registration rather than in review.
    Description {
        tool: String,
        findings: Vec<crate::schema::DescriptionFinding>,
    },
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterError::Duplicate(n) => write!(f, "a tool called `{n}` is already registered"),
            RegisterError::Description { tool, findings } => write!(
                f,
                "`{tool}`'s description says what the data contains, which clause 6 forbids: {}",
                findings
                    .iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        }
    }
}

impl std::error::Error for RegisterError {}

#[derive(Debug)]
pub enum RoleError {
    /// The resolved tool count exceeds the ceiling, and the overflow is named.
    OverBudget {
        role: String,
        count: usize,
        max: usize,
        overflow: Vec<String>,
    },
    /// A role naming a tool this build does not have. Refused rather than silently
    /// seated with a smaller set, for the same reason an MCP server over budget is
    /// refused with its tool list rather than truncated.
    Unknown { role: String, names: Vec<String> },
}

impl std::fmt::Display for RoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoleError::OverBudget {
                role,
                count,
                max,
                overflow,
            } => write!(
                f,
                "role `{role}` resolves to {count} tools and the ceiling is {max}; \
                 over by {}: {}",
                overflow.len(),
                overflow.join(", ")
            ),
            RoleError::Unknown { role, names } => write!(
                f,
                "role `{role}` names {} tool(s) this build does not have: {}",
                names.len(),
                names.join(", ")
            ),
        }
    }
}

impl std::error::Error for RoleError {}

/// The tools a session may call.
#[derive(Default)]
pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
    /// Descriptions that failed the lint but were registered anyway, from
    /// [`Registry::register_foreign`]. Surfaced rather than swallowed.
    pub foreign_findings: Vec<(String, Vec<crate::schema::DescriptionFinding>)>,
}

impl std::fmt::Debug for Registry {
    /// By name. A `Box<dyn Tool>` has nothing else to show, and a registry that
    /// cannot be printed makes every `Result` around it awkward.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("tools", &self.names())
            .field("foreign_findings", &self.foreign_findings)
            .finish()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one of ours. Clause 6 is enforced here.
    pub fn register(&mut self, tool: Box<dyn Tool>) -> Result<(), RegisterError> {
        let schema = tool.schema();
        if self.get(&schema.name).is_some() {
            return Err(RegisterError::Duplicate(schema.name));
        }
        let findings = lint_description(&schema.description);
        if !findings.is_empty() {
            return Err(RegisterError::Description {
                tool: schema.name,
                findings,
            });
        }
        self.tools.push(tool);
        Ok(())
    }

    /// Register a tool whose description we do not control — an MCP server's, in
    /// M3. The lint still runs; its findings are recorded rather than fatal,
    /// because refusing somebody else's server over its prose would be a harness
    /// making a policy nobody asked for.
    pub fn register_foreign(&mut self, tool: Box<dyn Tool>) -> Result<(), RegisterError> {
        let schema = tool.schema();
        if self.get(&schema.name).is_some() {
            return Err(RegisterError::Duplicate(schema.name));
        }
        let findings = lint_description(&schema.description);
        if !findings.is_empty() {
            self.foreign_findings.push((schema.name.clone(), findings));
        }
        self.tools.push(tool);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.schema().name == name)
            .map(|t| t.as_ref())
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.schema().name).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The schemas, in registration order. Order is part of the stable prefix, so
    /// this is the order the prompt is built in and it must not depend on a hash
    /// map's iteration.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.iter().map(|t| t.schema()).collect()
    }

    /// `StablePrefix::tools_json`, ready for the dialect.
    pub fn tools_json(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|t| t.schema().prompt_json())
            .collect()
    }

    /// §8.4, enforced: **refuse to seat a role** whose resolved tool count exceeds
    /// its ceiling, naming the overflow, and refuse one that names a tool this
    /// build does not have.
    pub fn resolve_role(self, role: &Role) -> Result<Registry, RoleError> {
        let have = self.names();
        let missing: Vec<String> = role
            .tools
            .iter()
            .filter(|t| !have.contains(t))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(RoleError::Unknown {
                role: role.name.clone(),
                names: missing,
            });
        }
        if role.tools.len() > role.max_tools {
            return Err(RoleError::OverBudget {
                role: role.name.clone(),
                count: role.tools.len(),
                max: role.max_tools,
                overflow: role.tools[role.max_tools..].to_vec(),
            });
        }
        let mut kept = Registry::new();
        kept.foreign_findings = self.foreign_findings;
        // Role order, not registration order: the role is what the prompt is built
        // from, and it is written down in one place.
        let mut tools = self.tools;
        for name in &role.tools {
            if let Some(i) = tools.iter().position(|t| &t.schema().name == name) {
                kept.tools.push(tools.remove(i));
            }
        }
        Ok(kept)
    }
}

/// The runtime proper.
pub struct ToolRuntime {
    pub registry: Registry,
    pub backend: Box<dyn ExecBackend>,
    pub spiller: Spiller,
    pub gate: Box<dyn Gate>,
    pub limits: Limits,
    /// Session-scoped, and on the runtime rather than on a tool because
    /// read-before-write is a fact about the *session*, not about `edit`.
    pub files: crate::files::FileLedger,
}

impl ToolRuntime {
    pub fn new(registry: Registry, backend: Box<dyn ExecBackend>) -> Self {
        ToolRuntime {
            registry,
            backend,
            spiller: Spiller::unset(),
            // Fail closed by default: a runtime nobody configured refuses every
            // non-read call rather than allowing it.
            gate: Box::new(NoBoundary),
            limits: Limits::default(),
            files: crate::files::FileLedger::new(),
        }
    }

    pub fn with_spiller(mut self, spiller: Spiller) -> Self {
        self.spiller = spiller;
        self
    }

    pub fn with_gate(mut self, gate: Box<dyn Gate>) -> Self {
        self.gate = gate;
        self
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Run one call the model proposed.
    pub fn invoke(
        &mut self,
        turn_id: &str,
        call: &ToolCall,
        sink: &mut dyn ToolEventSink,
    ) -> ToolResult {
        let Some(schema) = self.registry.get(&call.name).map(|t| t.schema()) else {
            // An unknown tool name is a miss, and clause 1 applies to it: the model
            // gets the list it can choose from and the nearest thing to what it
            // asked for, in the same call.
            let names = self.registry.names();
            let near = nearest(&call.name, &names);
            let mut payload = format!("this session has these tools: {}", names.join(", "));
            if let Some(n) = near {
                payload.push_str(&format!("\nthe nearest to `{}` is `{n}`", call.name));
            }
            let r = ToolResult::new(
                call.id.clone(),
                call.name.clone(),
                ToolOutcome::Failed {
                    reason: format!("no tool called `{}` is available here", call.name),
                },
            )
            .with_payload(payload);
            sink.emit(finished_event(turn_id, &r));
            return r;
        };

        // Clause 2, before anything else looks at the arguments.
        let (args, repairs) = match salvage(&call.arguments, &schema) {
            Ok(s) => (s.value, s.repairs),
            Err(e) => {
                let r = ToolResult::new(
                    call.id.clone(),
                    call.name.clone(),
                    ToolOutcome::Failed { reason: e.reason },
                )
                .with_payload(e.guidance);
                sink.emit(finished_event(turn_id, &r));
                return r;
            }
        };

        // Clause 4. The gate is consulted **only** when the declared access is not
        // read: a read-only tool has no code path to a question.
        if !schema.access.is_unattended() {
            let workspace = self
                .backend
                .root_path()
                .unwrap_or_else(|| self.backend.describe());
            let target_exists = args
                .get("path")
                .and_then(|v| v.as_str())
                .map(|p| self.backend.stat(p).is_some());
            let gate_call = GateCall {
                name: &schema.name,
                access: schema.access,
                args: &args,
                turn_id,
                call_id: &call.id,
                workspace: &workspace,
                target_exists,
            };
            if let GateDecision::Refuse { outcome, tell } = self.gate.admit(&gate_call) {
                let r = ToolResult::new(call.id.clone(), call.name.clone(), outcome)
                    .with_payload(tell);
                sink.emit(finished_event(turn_id, &r));
                return r;
            }
        }

        sink.emit(ToolEvent::Started {
            turn_id: turn_id.to_string(),
            call_id: call.id.clone(),
            name: schema.name.clone(),
            access: schema.access,
        });

        let invocation = {
            let tool = self
                .registry
                .get(&call.name)
                .expect("the schema was found a moment ago");
            let mut ctx = InvokeCtx {
                backend: self.backend.as_ref(),
                spiller: &self.spiller,
                files: &self.files,
                limits: self.limits,
                turn_id,
                call_id: &call.id,
                sink,
            };
            tool.invoke(&mut ctx, &args)
        };

        // Clause 5, on every payload and not only the ones somebody remembered.
        //
        // The budget bounds the **tool's output**. The envelope and the `[note]`
        // lines are the harness's own bytes, are bounded by construction, and are
        // added after: a policy that counted them would make the model's share of
        // its own budget depend on how many repairs its arguments needed.
        let produced = invocation.payload.len();
        let (payload, spill) = self.spiller.apply(
            invocation.payload,
            &SpillContext {
                tool: &schema.name,
                args: &args,
                bytes: produced,
            },
        );

        let result = ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            outcome: invocation.outcome,
            payload,
            repairs,
            notes: invocation.notes,
            spill,
            // Never spilled and never truncated: this is the head's copy, not the
            // model's, and clause 5 bounds what goes into the prompt. A head that
            // was handed half a file could not draw a diff at all.
            edit: invocation.edit,
        };
        sink.emit(finished_event(turn_id, &result));
        result
    }

    /// The transcript row for a finished call. The payload is the **rendered**
    /// result, envelope included, because that is the byte sequence the next
    /// prompt replays.
    pub fn transcript_item(result: &ToolResult) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: result.call_id.clone(),
            name: result.name.clone(),
            outcome: result.outcome.clone(),
            payload: result.render(),
        }
    }
}

fn finished_event(turn_id: &str, r: &ToolResult) -> ToolEvent {
    let rendered = r.render();
    ToolEvent::Finished {
        turn_id: turn_id.to_string(),
        call_id: r.call_id.clone(),
        outcome: r.outcome.clone(),
        payload_digest: payload_digest(&rendered),
        inline_bytes: rendered.len() as u64,
        full_bytes: r
            .spill
            .as_ref()
            .map(|s| s.full_bytes as u64)
            .unwrap_or(rendered.len() as u64),
        spill: r.spill.as_ref().map(|s| s.hash.clone()),
        repairs: r.repairs.len() as u32,
    }
}

/// The nearest known name, by a cheap edit distance. Used only to *suggest*.
fn nearest(want: &str, have: &[String]) -> Option<String> {
    have.iter()
        .map(|h| (distance(want, h), h))
        .filter(|(d, h)| *d <= h.len().max(want.len()) / 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, h)| h.clone())
}

fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The repairs a result carries, as one line for a head.
pub fn repair_summary(repairs: &[Repair]) -> String {
    repairs
        .iter()
        .map(|r| r.code)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::RecordingToolSink;

    struct Probe {
        access: Access,
    }

    impl Tool for Probe {
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                "probe",
                "Do a thing. Takes a path.",
                serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                }),
                self.access,
            )
        }
        fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
            Invocation::ok(format!("probed {}", args["path"]))
        }
    }

    struct ExplodingGate;

    impl Gate for ExplodingGate {
        fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
            panic!("a read-only tool must never reach the gate: {}", call.name);
        }
    }

    fn runtime(access: Access) -> ToolRuntime {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe { access })).unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        // The temp dir outlives the backend only within one test; leak it there
        // rather than complicate every caller.
        std::mem::forget(d);
        ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(ExplodingGate))
    }

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "c0".into(),
            name: name.into(),
            arguments: args.into(),
        }
    }

    #[test]
    fn a_read_only_tool_never_reaches_the_gate() {
        // Clause 4, as a control-flow fact rather than a promise: the gate panics
        // if it is consulted, and this call must not consult it.
        let mut rt = runtime(Access::Read);
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke("t1", &call("probe", r#"{"path":"a"}"#), &mut sink);
        assert_eq!(r.outcome, ToolOutcome::Ok);
    }

    #[test]
    fn a_write_tool_does_and_m1_has_nobody_to_ask() {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Write,
        }))
        .unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke("t1", &call("probe", r#"{"path":"a"}"#), &mut sink);
        match r.outcome {
            // `NotRun`, not `Denied`: nobody decided anything.
            ToolOutcome::NotRun { why } => assert!(why.contains("fails closed"), "{why}"),
            other => panic!("a write tool must not run unattended: {other:?}"),
        }
        assert!(
            !sink.kinds().contains(&"ToolStarted"),
            "a refused call never started"
        );
    }

    #[test]
    fn an_unknown_tool_comes_back_with_the_list_and_the_nearest_name() {
        let mut rt = runtime(Access::Read);
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke("t1", &call("probes", "{}"), &mut sink);
        assert!(r.payload.contains("probe"), "{}", r.payload);
        assert!(r.payload.contains("nearest"), "{}", r.payload);
    }

    #[test]
    fn a_description_that_names_data_is_refused_at_registration() {
        struct Stale;
        impl Tool for Stale {
            fn schema(&self) -> ToolSchema {
                ToolSchema::new(
                    "stale",
                    "Search the corpus. The corpus contains the Rust book.",
                    serde_json::json!({"type": "object"}),
                    Access::Read,
                )
            }
            fn invoke(&self, _c: &mut InvokeCtx<'_>, _a: &Value) -> Invocation {
                Invocation::ok("")
            }
        }
        let mut reg = Registry::new();
        let e = reg.register(Box::new(Stale)).unwrap_err();
        assert!(format!("{e}").contains("clause 6"), "{e}");
        // A foreign tool is seated, and its findings are visible.
        assert!(reg.register_foreign(Box::new(Stale)).is_ok());
        assert_eq!(reg.foreign_findings.len(), 1);
    }

    #[test]
    fn a_role_over_the_ceiling_is_refused_with_the_overflow_named() {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Read,
        }))
        .unwrap();
        let mut role = Role::new("greedy", &["probe"]);
        role.max_tools = 0;
        let e = reg.resolve_role(&role).unwrap_err();
        assert!(format!("{e}").contains("probe"), "{e}");
    }

    #[test]
    fn a_role_naming_a_tool_this_build_lacks_is_refused_not_shrunk() {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Read,
        }))
        .unwrap();
        let e = reg.resolve_role(&roles::coder()).unwrap_err();
        let msg = format!("{e}");
        assert!(msg.contains("write") && msg.contains("bash"), "{msg}");
    }

    #[test]
    fn the_events_bracket_the_call() {
        let mut rt = runtime(Access::Read);
        let mut sink = RecordingToolSink::new();
        rt.invoke("t1", &call("probe", r#"{"path":"a"}"#), &mut sink);
        assert_eq!(sink.kinds(), vec!["ToolStarted", "ToolFinished"]);
    }
}
