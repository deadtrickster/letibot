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

/// What came back from asking whether a path may enter the session's view.
///
/// Three outcomes and not two: a refusal is somebody saying no, and `NotAsked` is
/// nobody having been asked — the same distinction `not_run` draws against
/// `denied` one layer up, and for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewGrant {
    Granted { writable: bool },
    Refused(String),
    NotAsked,
}

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
    /// **Paths this call could not see because they are outside the session's
    /// filesystem view.** Read off the boundary, not guessed from the output.
    ///
    /// Empty for every call that saw everything it named, and for every backend
    /// with no view at all. Non-empty is a dead end the runtime can do something
    /// about: it raises a grant decision, and until this existed the only thing the
    /// system could do was print a note telling the model that "whoever opened the
    /// session" would have to grant it — with nothing able to ask that person.
    pub needs_in_view: Vec<std::path::PathBuf>,
}

impl Invocation {
    pub fn ok(payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::Ok,
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
            needs_in_view: Vec::new(),
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
            needs_in_view: Vec::new(),
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
            needs_in_view: Vec::new(),
        }
    }

    /// **The deadline passed and the command was killed.** opencode's `timeout`
    /// semantics: the process is not left running in the background, it is
    /// terminated, and the payload says how to ask for more time.
    pub fn timed_out(payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::Timeout,
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
            needs_in_view: Vec::new(),
        }
    }

    /// **Nothing ran.** Not a denial, not an abstention, not a failure — see
    /// [`crate::attach`], which is where the sentence a tool puts in `why` is
    /// built so that every tool with nothing behind it produces the same shape.
    ///
    /// This was a struct literal in `retrieval` and nowhere else, which is why it
    /// is here now: the family had three constructors and a fourth outcome, and
    /// the missing constructor is what makes a tool author reach for the nearest
    /// one that exists.
    pub fn not_run(why: impl Into<String>, payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::NotRun { why: why.into() },
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
            needs_in_view: Vec::new(),
        }
    }

    /// **Still running, and here is the handle.** Not finished and not failed.
    ///
    /// The fourth constructor exists for the same reason [`Invocation::not_run`]
    /// became one: a family with three constructors and a fourth outcome is a
    /// family whose fourth outcome gets spelled as whichever of the three is
    /// nearest, and every one of the three says something false here. A promotion
    /// the model does not notice is the same defect as a denial the operator does
    /// not see — it infers the wrong thing and acts on it.
    pub fn backgrounded(
        handle: impl Into<String>,
        ran_for: std::time::Duration,
        how: letibot_transcript::Backgrounding,
        next: impl Into<String>,
        payload: impl Into<String>,
    ) -> Self {
        Invocation {
            outcome: ToolOutcome::Backgrounded {
                handle: handle.into(),
                ran_for_ms: ran_for.as_millis() as u64,
                how,
                next: next.into(),
            },
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
            needs_in_view: Vec::new(),
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
    /// The most lines `read` returns when the call named no `limit`. A full
    /// thousand-line file at twenty thousand tokens is one call occupying a
    /// session, which is this struct's job to prevent; the tool's own note
    /// hands back the offset that continues it.
    pub max_read_lines: usize,
    /// The most bytes of numbered text `read` returns in one call, whatever the
    /// line count. The line cap cannot bound a file with six enormous lines —
    /// minified javascript is one line — so the byte budget is the guarantee
    /// and the line cap is the default shape.
    pub max_read_bytes: usize,
    /// The most characters of one line `read` will show before it clips the
    /// rest. A clipped line is named in the notes, because a model that cannot
    /// see a boundary will assume there is none.
    pub max_read_line_chars: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_walk_entries: 20_000,
            max_matches: 200,
            max_file_bytes: 4 * 1024 * 1024,
            max_read_lines: 200,
            max_read_bytes: 32 * 1024,
            max_read_line_chars: 2000,
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

    /// **May this session see `path`?** Asked when a call named something the
    /// boundary hid, so the operator gets the question instead of the model
    /// getting a dead end.
    ///
    /// The default is [`ViewGrant::NotAsked`], which is the honest answer for a
    /// gate with nobody behind it: not a refusal (nobody decided) and not silence
    /// (the caller knows it was never put to anyone). A gate that CAN reach a
    /// person overrides this.
    fn grant_view(&mut self, _path: &std::path::Path, _tool: &str) -> ViewGrant {
        ViewGrant::NotAsked
    }

    /// Who is adjudicating, for the daemon's startup disclosure.
    ///
    /// Defaults to naming the absence, because that is the case an operator most
    /// needs to see and the one a hard-coded banner gets wrong: a gate that does
    /// not identify an adjudicator does not have one.
    fn describe(&self) -> String {
        "none (no adjudicator attached)".into()
    }

    /// **Turn supervision on or off while the session is running.**
    ///
    /// > *"I want to start leticode, do /supervise, and move on."*
    ///
    /// Supervision is a property of the gate and not of the mode, which is what makes
    /// that possible: the mode decides *what asks*, and is fixed when a session opens
    /// because the tools seated under it are. This decides *whether the guard model
    /// gets a turn before the answer*, and nothing about a session's shape depends on
    /// it — so it can move without rebuilding anything.
    ///
    /// Returns what to tell the operator, and whether it took. A gate with no advisor
    /// says so rather than reporting success and supervising nothing.
    ///
    /// The default refuses, because a gate that silently accepted the request and
    /// never supervised would be the exact failure the whole feature exists to catch.
    fn set_supervision(&mut self, _on: bool) -> Result<String, String> {
        Err("this gate has no adjudicator, so there is nothing to supervise".into())
    }

    /// Whether the guard model currently gets a turn on every call.
    fn supervising(&self) -> bool {
        false
    }

    /// **Move this gate to another point in mode-space, now.**
    ///
    /// The gate reads its mode at decision time — the decider, the grant scope,
    /// what admits unasked — so moving it is a field write, and everything the
    /// session has accumulated (the audit log, the breaker, the advisor) stays.
    /// What does NOT stay is the standing grants: a grant was an answer to a
    /// question asked under the old point, and a new point is a new question. A
    /// grant kept across a tightening would leak permission; kept across a
    /// loosening it is moot.
    ///
    /// Whether the point's PREREQUISITES are met is the caller's to check — the
    /// gate does not know what backend or oracle is behind it. Returns how many
    /// grants were dropped, for the sentence the operator reads.
    ///
    /// The default refuses, for the reason `set_supervision`'s does: a gate that
    /// accepted the request and kept deciding at the old point would be the mode
    /// saying one thing and the session doing another.
    fn set_mode(&mut self, _mode: crate::mode::Mode) -> Result<usize, String> {
        Err("this gate has no mode to move".into())
    }

    /// **Attach a guard model to a session that opened without one.**
    ///
    /// So that turning supervision on never means restarting anything. The endpoint
    /// is the only expensive part of supervision and it is just an address; requiring
    /// it at daemon start made `/supervise` a lie on every session that had not
    /// thought to pass it, which is every session somebody starts by typing
    /// `leticode`.
    ///
    /// Replaces whatever was there. The default refuses, for `set_supervision`'s
    /// reason.
    fn attach_advisor(&mut self, _advisor: std::sync::Arc<dyn crate::adjudicate::Adjudicator>) -> Result<(), String> {
        Err("this gate has no adjudicator, so an advisor would have nothing to advise".into())
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
/// choosing tools."* Raised 8 -> 16 (2026-09-13) for leticode, which seats the
/// opencode tool union — a dozen tools — rather than the original eight; the
/// ceiling is still a hard stop, it is just a larger one.
pub const DEFAULT_MAX_TOOLS: usize = 16;

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

    /// leticode's seat: opencode's toolset, by opencode's names, plus `skill`.
    ///
    /// Deliberately the opencode union and not the letibot extras — this is the
    /// seat an opencode-shaped agent runs under. `bash` is stripped unless the
    /// daemon was started with `--bash`, exactly as it is for `coder`; `task` and
    /// `lsp` join this list as they land.
    pub fn leticode() -> Role {
        let mut r = Role::new(
            "leticode",
            &[
                "read",
                "write",
                "edit",
                "grep",
                "glob",
                "bash",
                "todo_write",
                "skill",
                "lsp",
                "task",
                // The background-job surface, so a `bash` call that is backgrounded
                // (asked, promoted, or by the operator) can be waited, read, killed
                // and listed — and a condition can be watched across turns. Seated
                // beside `bash`, not instead of it: a background task without
                // `job_wait` is a handle the model cannot follow up on.
                "job_list",
                "job_output",
                "job_wait",
                "job_kill",
                "monitor",
                // `pkill`: find by a string, kill by pid, never itself. Seated
                // with the exec surface, because signalling a process is one.
                "pkill",
                // `ps`: the question `ps | grep -v grep` was asked 294 times to
                // answer (`PS_USE.md`), as a read-only table that never lists
                // this process. Seated with the shell, which is what it looks at.
                "ps",
                // `harness`: what this session is running inside. Read-only, and
                // it is how a model stops asking the operator to read their own
                // terminal aloud.
                "harness",
            ],
        );
        // Eighteen: the opencode union, the room (`flowy`, seated by the daemon
        // when it holds a seat), `pkill` and `ps`. The ceiling is a guard against
        // a prompt nobody counted, and this is the count, counted.
        r.max_tools = 19;
        r
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
    ///
    /// `todo_write` is seated here and in [`m3_researcher`] because those are the
    /// two roles with spare seats against §8.4's ceiling, and the todos pane needs
    /// a writer wherever a session runs by default — which is this one. It is the
    /// operator-facing list (whole-replace, persisted to the session store,
    /// announced to heads), not the intent board's `todo`: that one stays the
    /// checked working list for the roles that already seat it, and the two
    /// schemas say so. A role without a spare seat does not get this tool by
    /// taking one from something else; the pane simply stays empty there until a
    /// seat decision says otherwise.
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
                "todo_write",
            ],
        )
    }

    /// §8.4's `researcher` with the web instead of `search_corpus`, which no
    /// build has.
    ///
    /// Seven against a ceiling of eight, and the shape of the table's own
    /// `researcher`: ask the index, ask the web, then read and search the tree.
    /// Four of the seven refuse today — the two retrieval seats have no backend and
    /// the two network seats have no provider — and the role exists so that *"which
    /// tools would this agent have"* is a question with a written answer rather
    /// than one settled per session.
    ///
    /// `todo_write` takes the eighth seat for the same reason it is in
    /// [`m1_orchestrator`]: it is the pane's writer, the role had the room, and
    /// nothing was displaced to make it fit.
    pub fn m3_researcher() -> Role {
        Role::new(
            "researcher",
            &[
                "ask_corpus",
                "ask_code",
                "web_search",
                "web_fetch",
                "read",
                "grep",
                "read_spill",
                "todo_write",
            ],
        )
    }

    /// What a session in plan mode seats: nothing that can change **the work**.
    ///
    /// Plan mode is a **capability boundary**, so this is a role and not a flag —
    /// `write` and `edit` are absent from `tools_json` rather than refused at call
    /// time. [`crate::builtins::intent::plan::seating`] derives the same answer
    /// from an arbitrary base role; this is the named one.
    ///
    /// D9: it keeps `write_plan` (a write scoped to plan documents by taking a
    /// name rather than a path) and `say` (the fabric's chat verb), because a
    /// planner that can neither record nor discuss its plan has to carry the plan
    /// in the context it is about to hand over. Eight tools, which is exactly
    /// [`DEFAULT_MAX_TOOLS`] — this role has no spare seat, and `outline` is what
    /// it gave up to get one.
    pub fn planner() -> Role {
        Role::new(
            "planner",
            &[
                "read",
                "grep",
                "glob",
                "todo",
                "goal",
                "write_plan",
                "say",
                "exit_plan_mode",
            ],
        )
    }

    /// What M2 can seat: §8.4's `coder` without `bash`.
    ///
    /// `bash` is `Access::Exec` and this role deliberately does not name it, even
    /// now that the tool exists: a session seated as `coder` is a session that
    /// edits files, and giving it a shell as a side effect of somebody else's
    /// workstream is how a capability arrives without a decision. `bash` is seated
    /// by [`m2_runner`] and by nothing else. Six tools against a ceiling of eight.
    pub fn m2_coder() -> Role {
        Role::new(
            "coder",
            // `todo` is seated here and the reason is not convenience.
            //
            // It was seated only by `planner`, which has no `write` or `edit` — so no
            // role could both DO the work and record what it meant to do. That put the
            // error signal of `docs/closed-loop.md` §2 in the one role that produces no
            // effects to compare it against: `planner` recorded intentions it could not
            // carry out, `coder` carried out work it could not declare, and
            // `intent::ledger`'s intent-versus-effect diff had nothing to diff.
            //
            // What it cost, observed 2026-09-10 in a real session: asked for a todo
            // list, the model spent 13 tool calls and ~15,000 tokens of reasoning
            // reading `board.rs` and `ledger.rs` to EMULATE the tool it had not been
            // given, then wrote 1,926 tokens describing what it would have printed.
            // That is §4b — a capability that exists but is hidden manufactures the
            // workaround — at maximum price.
            //
            // It needs no gate change: `todo` is `Access::Session` (asserted in
            // `builtins::intent::mod`), so it is neither a read nor a write and nothing
            // the operator owns is touched. With no board mounted it records into this
            // session's own ledger — no node, no token, no network.
            //
            // Seven tools against a ceiling of eight. `goal` is deliberately NOT added
            // with it: a separate capability is a separate decision.
            // `bash` is listed and then STRIPPED unless `--bash` was passed — the same
            // shape `m2_runner` has. Listing it here is what makes the flag mean
            // something for this seat; without the entry the flag would be ignored and
            // the operator would be told a capability was on while it was not.
            //
            // Eight tools against a ceiling of eight when the flag is given, seven
            // without. `goal` is still not here: a separate capability is a separate
            // decision.
            &[
                "read",
                "write",
                "edit",
                "grep",
                "glob",
                "read_spill",
                "todo",
                "bash",
            ],
        )
    }

    /// The only role that can run a command.
    ///
    /// **Nine tools, one over §8.4's ceiling, and the overrun is declared rather
    /// than absorbed.** The arithmetic: the exec surface is five seats (`bash`
    /// plus the four job verbs, which cannot be fewer — starting, watching,
    /// reading and stopping are four different questions and three harnesses
    /// independently found the same shape), `monitor` is the sixth, and `read`,
    /// `grep` and `read_spill` take the rest.
    ///
    /// What that gives up is `glob`, and it is given up on purpose: a session with
    /// a shell has a worse-but-real substitute for it in `ls` and `find`, and it
    /// has **no** substitute for `read_spill`, which is what makes clause 5's
    /// "bounded, never truncated" true rather than a slogan. Dropping the honest
    /// one to keep the convenient one would be trading a correctness property for
    /// a search.
    ///
    /// # Why the ninth seat is taken rather than traded for
    ///
    /// The ceiling's evidence is *"past ~5–7 MCP servers small models get worse at
    /// choosing tools"* — it is about **confusion between similar choices**. The
    /// monitor surface was cut twice against exactly that before it was allowed to
    /// cost a seat:
    ///
    /// - declaring, renewing and retiring are **one** tool taking an `action`,
    ///   because all three act on the same named handle;
    /// - listing monitors is **not a tool at all**. It is in `job_list`, next to
    ///   the jobs, the scopes, the promotions and the reap log, because "what is
    ///   running and what is watching" is one question.
    ///
    /// What is left cannot be folded into `job_wait` without making the model's
    /// worst mistake here spellable: `job_wait` blocks **inside** the turn and a
    /// monitor watches **across** turns, and a flag that switched between them
    /// would let a model believe it had waited when it had not. T24 names them as
    /// two primitives for that reason.
    ///
    /// So `max_tools` is 9 here and [`DEFAULT_MAX_TOOLS`] everywhere else. A
    /// ceiling that is quietly raised for everybody is not a ceiling; one role
    /// declaring its own number, with the trade written down, is a decision
    /// somebody can reverse.
    pub fn m2_runner() -> Role {
        let mut r = Role::new(
            "runner",
            &[
                "read",
                "grep",
                "read_spill",
                "bash",
                "job_list",
                "job_output",
                "job_wait",
                "job_kill",
                "monitor",
            ],
        );
        r.max_tools = 9;
        r
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
    /// Drop every tool of a denied access class. A subagent's downgrade, applied
    /// after the role is resolved: the tools leave the prompt entirely, so the
    /// model is not told it has a capability the gate would refuse.
    pub fn without_access(
        mut self,
        denied: &std::collections::BTreeSet<crate::schema::Access>,
    ) -> Registry {
        if denied.is_empty() {
            return self;
        }
        self.tools.retain(|t| !denied.contains(&t.schema().access));
        self
    }

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
                let r =
                    ToolResult::new(call.id.clone(), call.name.clone(), outcome).with_payload(tell);
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

        // **A path the boundary hid is a question, not a dead end.**
        //
        // Until this existed, a command that named something outside the session's
        // view got an `ENOENT` and a note saying the path "has to be granted into
        // the view by whoever opened the session" — with nothing in the system able
        // to ask that person. So the model's only move was to hand the operator
        // shell commands to run themselves, which is the route-around this tree
        // refuses everywhere else. The operator's report was exactly that: *"i
        // asked to do the symlink and it didnt even fallback to asking me"*.
        //
        // The decision is the gate's, so it lands in the corpus beside every other
        // one, and the options are the operator's own words: read-only, writable,
        // or no. An approval re-probes the boundary — see
        // `HostBackend::grant_into_view` — so the NEXT call sees the path; this one
        // is not retried, because its output is already written and re-running a
        // command on the model's behalf is a decision nobody made.
        let mut invocation = invocation;
        if !invocation.needs_in_view.is_empty() {
            let asked: Vec<std::path::PathBuf> =
                std::mem::take(&mut invocation.needs_in_view);
            for path in asked {
                match self.gate.grant_view(&path, &schema.name) {
                    ViewGrant::Refused(why) => {
                        invocation.notes.push(format!(
                            "`{}` was NOT granted into this session's view: {why}",
                            path.display()
                        ));
                    }
                    ViewGrant::NotAsked => {}
                    ViewGrant::Granted { writable } => {
                        match self.backend.grant_into_view(
                            &path,
                            writable,
                            &format!("granted mid-session for `{}`", schema.name),
                        ) {
                            Ok(view) => invocation.notes.push(format!(
                                "`{}` is now in this session's view{}, by your answer. \
                                 The command above already ran without it — run it again \
                                 and it will see the path. The view is now: {view}",
                                path.display(),
                                if writable { ", writable" } else { ", read-only" },
                            )),
                            Err(e) => invocation.notes.push(format!(
                                "`{}` was approved but could not be bound into the view, \
                                 so nothing changed: {e}",
                                path.display()
                            )),
                        }
                    }
                }
            }
        }

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
        // Bounded where it is built: three lines of context either side of
        // the change, four hundred lines the cap, so the fan-out cost is
        // known here and not a property of whatever file the model chose.
        edit: r.edit.as_ref().map(|e| e.excerpt(3, 400)),
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
    fn without_access_drops_a_class_and_leaves_the_rest_in_role_order() {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Read,
        }))
        .unwrap();
        let mut denied = std::collections::BTreeSet::new();
        denied.insert(Access::Read);
        assert!(reg.schemas().iter().any(|s| s.name == "probe"));
        let reg = reg.without_access(&denied);
        assert!(reg.schemas().is_empty());
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Read,
        }))
        .unwrap();
        let mut other = std::collections::BTreeSet::new();
        other.insert(Access::Exec);
        assert_eq!(reg.without_access(&other).schemas().len(), 1);
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
