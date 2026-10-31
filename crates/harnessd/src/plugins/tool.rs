//! **A tool a layer declares, seated where the model can call it** — `docs/plugins-plan.md` §4.8,
//! and the `tool_` seam the manifest's `[[tools]]` is the declaration half of.
//!
//! # The shape, in one paragraph
//!
//! A layer's `plugin.toml` says `[[tools]] name = "claude_transcript"`, so the host seats a tool of
//! that name with that description and that JSON Schema. When the model calls it, the host asks the
//! stack for `tool_claude_transcript` with
//! `{"v":1,"name":…,"args":{…},"feeds":{…},"looks":[],"state":null}` and the layer answers a
//! result in the tree's own vocabulary — `ok`, `failed`, `abstained` or `refused`.
//!
//! **Which layers count is [`crate::plugins::declared_tools`]'s answer, and it is the three DISK
//! scopes** — the session's, the project's, the user's. A built-in's `[[tools]]` block is the text a
//! person installs (its `plugin.toml` copied into a scope, with no module, and the binary's own
//! module answers below it); it is not a tool every session is handed. `crates/harnessd/tests/
//! wired.rs` pins the default session's tool set and says why, and this rule is what keeps that set
//! where it is.
//!
//! # A hook does no I/O, so a tool asks for it
//!
//! §1 principle 1: a plugin never touches the filesystem. A pane therefore *declares a look* and
//! the host runs it (`crate::plugins::look`). A tool needs the same door and needs it with more
//! reach than a pane's: `sessions` lists a directory, `search` sweeps a 116 MB transcript, `read`
//! wants lines 4000–4040 of it, `outline` wants forty specific lines out of fifty thousand. So the
//! answer may be `{"looks":[…], "state":…}` instead of a result, and the host runs every look,
//! hands the results back in the same order, and asks again — up to [`MAX_ROUNDS`] times.
//!
//! | look | is |
//! |---|---|
//! | `{"read": path, "from": 12, "lines": 40}` | numbered lines from `from` |
//! | `{"read": path, "at": [3, 17, 42]}` | exactly those lines, for a tool that grepped first |
//! | `{"list": path}` | a directory's entries: name, kind, size, mtime |
//! | `{"grep": path, "match": "…", "context": 2}` | matches with their line index and byte offset |
//!
//! **Every look is bounded and every truncation is in the answer** — `cut` on a line, `more` on a
//! page — because a tool whose whole reason for existing is *do not put 100 MB into a context*
//! cannot answer a big file with silence about the part it left out.
//!
//! # The capability is checked HERE, and it is the manifest's line
//!
//! A look's path resolves against the session's workspace (a layer that declared `read`) or against
//! one of the trees the layer named as `read:<path>`. Both sides are canonicalised before the
//! comparison, so a symlink inside a declared tree that points out of it — `~/.claude/projects`
//! holds one, `memory` → a folder under `~/Projects` — is refused for what it resolves to. Nothing
//! else is reachable: `~/.ssh` is not under `~/.claude/projects`, and no spelling of a path makes
//! it so.
//!
//! # What a broken layer costs
//!
//! A tool call, never a session. A layer that traps, that answers something that is not a result,
//! that asks for a path it may not read, or that is not loaded at all, produces a `failed` result
//! whose payload says which of those happened and what fixes it. The stack's own rule (§1
//! principle 4) applies underneath: a layer that traps is skipped and the one below answers.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_plugins::{Capability, DeclaredTool, Scope, ToolDecl};
use letibot_sessionlog::hub::Hub;
use letibot_tools::Invocation;
use letibot_tools::runtime::{GateDecision, InvokeCtx, Tool};
use letibot_tools::schema::{Access, ToolSchema};
use serde_json::{Value, json};

use crate::plugins::PluginHost;

/// **How many declared tools one session seats.**
///
/// §8.4's ceiling counts the tools a ROLE names, and a layer's tool is not one of those: it is a
/// seat the project added, after the role was resolved, so the role's number cannot be the one that
/// binds. This is. Four is a number a person can read in a prompt beside thirty-five others without
/// the union becoming a menu, and a session that wants a fifth is a session whose fifth tool should
/// be a hook or a `bash` call.
pub const MAX_TOOLS: usize = 4;

/// **How many ask-look-ask rounds one call may take.** `outline` needs three (a grep for the two
/// row kinds, then the lines those matched); a fourth is a layer that keeps asking, and the answer
/// says so rather than looping.
pub const MAX_ROUNDS: usize = 4;

/// How many looks one answer may ask for. A `sessions` listing over eight projects and forty
/// transcripts is 48; a hundred is a layer enumerating a filesystem one call at a time.
pub const MAX_LOOKS: usize = 64;

/// Bytes of one line handed to a plugin, and the ceiling a caller may raise it to. A transcript
/// line is a JSON object and the interesting part of it is usually the first kilobyte; a line that
/// is cut says so, and `read` on the range gets the rest.
const LINE_CAP: usize = 2_000;
const LINE_CAP_MAX: usize = 32_000;

/// Lines one `read` look returns, and the ceiling for it.
const LINES_CAP: usize = 400;
const LINES_CAP_MAX: usize = 2_000;

/// Total bytes one look may carry, whatever the line count. 512 KiB is half of a plugin's answer
/// arena and the guest is the half that has to hold it.
const BYTES_CAP: usize = 512 * 1024;

/// Directory entries one `list` look returns, and matches one `grep` look returns.
const ENTRIES_CAP: usize = 256;
const MATCHES_CAP: usize = 40;

/// The contract version every `tool_` input carries, like every other hook's.
const TOOL_V: u32 = 1;

/// **The one question a plugin tool puts to a gate** — `Gate::admit_unasked`, reached through the
/// runtime's own door (`InvokeCtx::admit_unasked`).
///
/// A trait rather than the `InvokeCtx` itself because the question is the whole of what this tool
/// needs from a runtime, and a test that has to build an `InvokeCtx` (a backend, a spiller, a file
/// ledger) to ask it would be a test of the runtime. [`PluginTool::invoke`] is the only production
/// caller and it passes the context.
pub trait Decides {
    /// `None` when there is no gate to ask, which the caller reads as *nothing could judge it*.
    fn admit_unasked(&self, name: &str, access: Access, args: &Value) -> Option<GateDecision>;
}

impl Decides for InvokeCtx<'_> {
    fn admit_unasked(&self, name: &str, access: Access, args: &Value) -> Option<GateDecision> {
        InvokeCtx::admit_unasked(self, name, access, args)
    }
}

/// **One tool a layer declared, as the runtime sees it.**
pub struct PluginTool {
    /// The layer that declared it — the sentence a failure carries.
    layer: String,
    scope: Scope,
    decl: ToolDecl,
    /// What the layer may read. The tool's own reach is exactly this.
    caps: Vec<Capability>,
    session: String,
    workspace: PathBuf,
    /// The live host, filled by `Sessions::open_plugins` — the same cell the `plugin` tool holds,
    /// because a session has one stack and two would be two answers to *what is in force*.
    slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
    hub: Arc<Hub>,
}

impl PluginTool {
    pub fn new(
        declared: DeclaredTool,
        session: impl Into<String>,
        workspace: impl Into<PathBuf>,
        slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
        hub: Arc<Hub>,
    ) -> PluginTool {
        PluginTool {
            layer: declared.layer,
            scope: declared.scope,
            decl: declared.tool,
            caps: declared.capabilities,
            session: session.into(),
            workspace: workspace.into(),
            slot,
            hub,
        }
    }

    /// The access class this tool declares — **the most consequential thing the layer may do**.
    ///
    /// `access_for` can only narrow (see `letibot_tools::runtime::Tool`), so the schema has to
    /// carry the widest verb: a layer that may run a program makes its tool an `Exec` tool even if
    /// most calls read, and one that may only read is a `Read` tool and never reaches the gate.
    /// A layer that declares nothing at all is `Read`, because a tool that answers from its own
    /// arguments touches nothing.
    pub fn access(caps: &[Capability]) -> Access {
        if caps.iter().any(|c| matches!(c, Capability::Exec(_))) {
            Access::Exec
        } else if caps.iter().any(|c| matches!(c, Capability::Http(_))) {
            Access::Network
        } else {
            Access::Read
        }
    }

    /// **The facts a tool hook is handed under `feeds`** — the host's own, read once per call.
    ///
    /// Not a capability and not adjudicated: these are the four things the harness already knows
    /// and the head already shows (a home directory so `~/…` can be spelled, the workspace, the
    /// session, the clock). A tool that had to guess its own home would spell a path wrong on one
    /// machine and be refused by its own capability on that machine only.
    fn feeds(&self) -> Value {
        json!({
            "home": std::env::var("HOME").unwrap_or_default(),
            "workspace": self.workspace.display().to_string(),
            "session": self.session,
            "now_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        })
    }

    /// The sentence a result carries so a model can tell a layer's words from a tool's.
    fn answered_by(&self) -> String {
        format!("answered by plugin {} ({})", self.layer, self.scope)
    }

    fn refused(&self, why: impl std::fmt::Display, detail: impl Into<String>) -> Invocation {
        let mut inv = Invocation::not_run(why.to_string(), detail.into());
        inv.notes.push(self.answered_by());
        inv
    }
}

impl Tool for PluginTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            self.decl.name.clone(),
            self.decl.description.clone(),
            self.decl.schema.clone(),
            PluginTool::access(&self.caps),
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        // **D17's second arm, at the one door a model's call comes through.** A layer whose bytes
        // nobody pinned is adjudicated on every call, without a card; a built-in or a pinned layer
        // runs freely. `ctx` is passed because the gate lives on it — see
        // [`PluginTool::call`].
        self.call(Some(ctx), args)
    }
}

impl PluginTool {
    /// **The call, without the runtime's context** — this tool reads nothing from it, and a test
    /// that had to build an `InvokeCtx` would be a test of the runtime rather than of the seam.
    ///
    /// **A trust decision needs a gate, so a caller with no context has none**: an unpinned
    /// layer's tool is `not_run` with the sentence saying so, which is the fail-closed direction
    /// for the one caller that is not the runtime (a test standing the seam up).
    pub fn run(&self, args: &Value) -> Invocation {
        self.call(None, args)
    }

    /// **The whole call**: D17's decision first, then the layer.
    pub fn call(&self, decider: Option<&dyn Decides>, args: &Value) -> Invocation {
        let Some(host) = self.slot.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return Invocation::not_run(
                "this session has no plugin host yet",
                format!(
                    "`{}` is a tool the plugin layer `{}` declares, and the layer is answered by \
                     this session's plugin stack — which the daemon builds when it opens a session \
                     it serves. `{}` was opened by its parent's runner (a subagent), so nothing \
                     has filled the stack in. The same tool works in the session the operator is \
                     looking at.",
                    self.decl.name, self.layer, self.session
                ),
            );
        };
        if let Some(refusal) = self.admit(&host, decider, args) {
            return refusal;
        }
        self.answer(&host, args)
    }

    /// **May this call run?** — `None` when it may, and the `not_run` result that says why when it
    /// may not.
    ///
    /// Three arms, and the middle one is the whole of D17:
    ///
    /// * the declaring layer is a **built-in or pinned** → run freely, the gate is not consulted;
    /// * it is **not pinned** → `Gate::admit_unasked`, every call. No card, so a layer can never
    ///   put a question to a person, and the ladder's refusal is the answer;
    /// * the layer is not on the stack at all → refused, because a tool whose declaration this
    ///   host cannot find is a tool nobody can say is pinned.
    ///
    /// The subject of the call is the tool itself: its name, its arguments, and the class its
    /// layer's capabilities make it ([`PluginTool::access`]). That is what a `permission` rule is
    /// written against, and it is why an unpinned layer's read-only tool gets an audit row even
    /// though a read is admitted.
    fn admit(
        &self,
        host: &PluginHost,
        decider: Option<&dyn Decides>,
        args: &Value,
    ) -> Option<Invocation> {
        let Some(layer) = host.layer(&self.layer) else {
            return Some(Invocation::not_run(
                format!("the plugin layer `{}` is not on this session's stack", self.layer),
                format!(
                    "`{}` is declared by `{}` ({}) and this session's stack does not hold that \
                     layer any more — it was removed, disabled and dropped, or the declaration is \
                     in a scope this session no longer reads. Nothing was run.",
                    self.decl.name, self.layer, self.scope
                ),
            ));
        };
        if layer.trusted {
            return None;
        }
        let sentence = layer.untrusted_sentence();
        let Some(decision) = decider.and_then(|decides| {
            decides.admit_unasked(&self.decl.name, PluginTool::access(&self.caps), args)
        }) else {
            return Some(Invocation::not_run(
                format!("the plugin layer `{}` is not pinned", self.layer),
                format!(
                    "{sentence}. Nothing ran, because this call had no gate to judge it with — \
                     a layer nobody pinned runs only where the ladder can be asked."
                ),
            ));
        };
        let GateDecision::Refuse { outcome, tell } = decision else {
            return None;
        };
        let why = match &outcome {
            letibot_transcript::ToolOutcome::NotRun { why } => why.clone(),
            letibot_transcript::ToolOutcome::Denied { .. } => {
                let tell = tell.trim();
                if tell.is_empty() {
                    "the gate denied it".to_string()
                } else {
                    format!("the gate denied it — {tell}")
                }
            }
            other => format!("the gate refused it: {other:?}"),
        };
        Some(Invocation::not_run(
            format!("`{}` was not run: {why}", self.decl.name),
            format!(
                "{sentence}. The gate judged the call and would not admit it, and a layer nobody \
                 pinned is never put to a person as a card — pin the bytes with `/plugin trust \
                 {}` if this layer is yours.",
                self.layer
            ),
        ))
    }

    /// **The call, answered by the stack** — the loop of `tool_` rounds and the result it settles
    /// on.
    fn answer(&self, host: &PluginHost, args: &Value) -> Invocation {
        let hook = self.decl.hook();
        // **Round 0 has looked at nothing**, and every round replaces the results wholesale: the
        // layer asked for these looks and this call is its answer to them.
        let mut looks: Vec<Value> = Vec::new();
        let mut state = Value::Null;
        for _ in 0..MAX_ROUNDS {
            let input = json!({
                "v": TOOL_V,
                "name": self.decl.name,
                "args": args,
                "feeds": self.feeds(),
                "looks": looks,
                "state": state,
            });
            let Some(resolved) = host.ask(&self.hub, &hook, &input) else {
                return Invocation::failed(
                    format!("no plugin layer answers `{hook}`"),
                    format!(
                        "`{}` is declared by the layer `{}`, which this session's stack holds, and \
                         nothing in the stack answered `{hook}` — every layer passed it. Two ways \
                         to be here: the layer's module did not load (its `plugin.toml` names the \
                         hook and `admit` refused the module, or the build never ran), or a layer \
                         above it masked the hook. `plugin {{\"action\":\"stack\"}}` says which, \
                         and `plugin {{\"action\":\"test\"}}` calls the hook directly.",
                        self.decl.name, self.layer
                    ),
                );
            };
            let by = resolved.by.clone().unwrap_or_default();
            let Some(answer) = resolved.answer else {
                return Invocation::failed(
                    format!("`{by}` answered `{hook}` with nothing"),
                    "A hook that answers `-1` is passing, and the stack treats it as *ask the \
                     layer below* — so the call reached no layer with an answer for it. Return \
                     `{\"outcome\":\"ok\",\"payload\":\"…\"}` or say `refused` with a reason."
                        .to_string(),
                );
            };
            match answer_of(&answer) {
                Answer::Result {
                    outcome,
                    reason,
                    payload,
                } => {
                    let mut inv = match outcome.as_str() {
                        "ok" => Invocation::ok(payload),
                        "failed" => Invocation::failed(
                            reason.unwrap_or_else(|| format!("`{by}` failed")),
                            payload,
                        ),
                        "abstained" => Invocation::abstained(
                            reason.unwrap_or_else(|| format!("`{by}` abstained")),
                            payload,
                        ),
                        // **`refused` is `NotRun` and not `Failed`**: the layer did nothing, and a
                        // failure would read to the model as a tool that broke rather than as one
                        // that would not act. Its own sentence says why.
                        "refused" => Invocation::not_run(
                            reason.unwrap_or_else(|| format!("`{by}` refused")),
                            payload,
                        ),
                        _ => unreachable!("`answer_of` only answers a known outcome"),
                    };
                    inv.notes.push(format!(
                        "answered by plugin {} ({})",
                        by_or(&by, &self.layer),
                        self.scope
                    ));
                    return inv;
                }
                Answer::Looks {
                    wanted,
                    state: carry,
                } => {
                    if wanted.len() > MAX_LOOKS {
                        return Invocation::failed(
                            format!("`{by}` asked for {} looks in one answer", wanted.len()),
                            format!(
                                "One answer may ask for at most {MAX_LOOKS} looks — a tool that \
                                 enumerates a filesystem one call at a time is a tool that should \
                                 ask for a directory and then for the files it found. Narrow the \
                                 list and answer again."
                            ),
                        );
                    }
                    looks = wanted
                        .iter()
                        .map(|spec| run_look(spec, &self.caps, &self.workspace))
                        .collect();
                    state = carry;
                }
                Answer::Nonsense(why) => {
                    return Invocation::failed(
                        format!("`{by}` answered something that is not a tool result"),
                        format!(
                            "{why}. A `{hook}` answer is one of `{{\"outcome\":\"ok\",\"payload\":\
                             \"…\"}}`, the same with `failed` or `abstained` plus a `reason`, \
                             `{{\"outcome\":\"refused\",\"reason\":\"…\"}}`, or \
                             `{{\"looks\":[…],\"state\":…}}` to ask the host for data. The layer is \
                             `{}`.",
                            by_or(&by, &self.layer)
                        ),
                    );
                }
            }
        }
        self.refused(
            format!("`{hook}` kept asking for looks"),
            format!(
                "One call may ask the host for data at most {MAX_ROUNDS} times and `{}` used every \
                 one of them without answering a result. It is the layer's loop, not the host's: \
                 an answer that carries `looks` is a question, and the round after it has to be an \
                 `outcome`.",
                by_or("", &self.layer)
            ),
        )
    }
}

/// The layer that answered, or the one that declared the tool when the stack did not name it.
fn by_or(by: &str, layer: &str) -> String {
    if by.is_empty() {
        layer.to_string()
    } else {
        by.to_string()
    }
}

/// One answer, read as one of the three things it can be.
enum Answer {
    /// A tool result.
    Result {
        outcome: String,
        reason: Option<String>,
        payload: String,
    },
    /// Data the host has to fetch before the layer can answer.
    Looks { wanted: Vec<Value>, state: Value },
    /// Neither, with the sentence a model can act on.
    Nonsense(String),
}

/// **Read an answer, without trusting a byte of it.** The layer is somebody else's code; the only
/// thing this function may assume is that the bytes arrived.
fn answer_of(answer: &Value) -> Answer {
    let Some(object) = answer.as_object() else {
        return Answer::Nonsense(format!(
            "It answered {answer}, and an answer is a JSON object"
        ));
    };
    if let Some(looks) = object.get("looks") {
        let Some(list) = looks.as_array() else {
            return Answer::Nonsense(format!("`looks` is {looks}, and it is an array"));
        };
        return Answer::Looks {
            wanted: list.clone(),
            state: object.get("state").cloned().unwrap_or(Value::Null),
        };
    }
    let Some(outcome) = object.get("outcome").and_then(Value::as_str) else {
        return Answer::Nonsense(format!(
            "It answered {answer}, which has neither `outcome` nor `looks`"
        ));
    };
    let payload = object
        .get("payload")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let reason = object
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    if !matches!(outcome, "ok" | "failed" | "abstained" | "refused") {
        return Answer::Nonsense(format!(
            "It answered `outcome: {outcome}`, and the outcomes are `ok`, `failed`, `abstained` \
             and `refused`"
        ));
    }
    Answer::Result {
        outcome: outcome.to_string(),
        reason,
        payload,
    }
}

// ---------------------------------------------------------------------------------------------
// Looks
// ---------------------------------------------------------------------------------------------

/// **Run one look a tool asked for**, against the capabilities of the layer that asked.
///
/// Every failure is a `{"exit":1,"error":…}` and never a raised error: the plugin is the half that
/// knows what to say about a refusal, and a host that raised would take the call with it. Same
/// argument as `crate::plugins::look`, which is where the pane's version of this lives.
pub fn run_look(spec: &Value, caps: &[Capability], workspace: &Path) -> Value {
    let Some(object) = spec.as_object() else {
        return look_error(format!("{spec} is not a look — a look is a JSON object"));
    };
    if let Some(path) = object.get("read").and_then(Value::as_str) {
        return match resolve(path, caps, workspace) {
            Ok(real) => read_look(&real, object),
            Err(why) => look_error(why),
        };
    }
    if let Some(path) = object.get("list").and_then(Value::as_str) {
        return match resolve(path, caps, workspace) {
            Ok(real) => list_look(&real, object),
            Err(why) => look_error(why),
        };
    }
    if let Some(path) = object.get("grep").and_then(Value::as_str) {
        return match resolve(path, caps, workspace) {
            Ok(real) => grep_look(&real, object),
            Err(why) => look_error(why),
        };
    }
    look_error(format!(
        "{spec} names no look — the three are `read`, `list` and `grep`"
    ))
}

fn look_error(why: impl std::fmt::Display) -> Value {
    json!({"exit": 1, "error": why.to_string()})
}

/// **Resolve a path a layer asked for, or say why it may not have it.**
///
/// The rule is the manifest's: the workspace if the layer declared `read`, and each tree it named
/// as `read:<path>` — and nothing else, ever. Both the root and the wanted path are canonicalised,
/// so a symlink inside a declared tree that points out of it is refused for what it resolves to.
fn resolve(path: &str, caps: &[Capability], workspace: &Path) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("a look named an empty path".to_string());
    }
    let wanted = if let Some(rest) = path.strip_prefix("~/") {
        match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => return Err(format!("`{path}` needs $HOME, and this daemon has none")),
        }
    } else if path.starts_with('/') {
        PathBuf::from(path)
    } else {
        // A relative path is the workspace's, which is what `read` means.
        workspace.join(path)
    };
    let real = wanted
        .canonicalize()
        .map_err(|e| format!("{}: {e}", wanted.display()))?;
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    if caps.contains(&Capability::Read) {
        roots.push((
            "the session's workspace".to_string(),
            workspace
                .canonicalize()
                .unwrap_or_else(|_| workspace.to_path_buf()),
        ));
    }
    for cap in caps {
        let Capability::ReadPath(declared) = cap else {
            continue;
        };
        let Some(root) = expand(declared) else {
            continue;
        };
        // A root that does not exist cannot contain anything, and saying so is better than a
        // containment test against a path nobody could resolve.
        roots.push((
            format!("`read:{declared}`"),
            root.canonicalize().unwrap_or(root),
        ));
    }
    for (_what, root) in &roots {
        if real.starts_with(root) {
            return Ok(real);
        }
    }
    Err(if roots.is_empty() {
        format!(
            "`{path}` is not read for this layer: its `plugin.toml` declares no `read` capability \
             and no `read:<path>` tree, so it can look at nothing outside itself. What it wants \
             has to be written down in the manifest a person reviewed."
        )
    } else {
        let named: Vec<String> = roots.iter().map(|(what, _)| what.clone()).collect();
        format!(
            "`{path}` resolves to {}, which is outside every tree this layer may read ({}) — the \
             manifest's declaration is the approval, and nothing else is reachable",
            real.display(),
            named.join(", ")
        )
    })
}

/// `~/…` expanded against this process's home, or an absolute path as it stands.
///
/// `pub(crate)` because a pane's look (`crate::plugins::look`) resolves a `read:<path>` root the
/// same way: one expansion, in one place, so a `~/` cannot mean two things in one session.
pub(crate) fn expand(declared: &str) -> Option<PathBuf> {
    if let Some(rest) = declared.strip_prefix("~/") {
        let home = std::env::var_os("HOME")?;
        return Some(PathBuf::from(home).join(rest));
    }
    Some(PathBuf::from(declared))
}

/// A line as it is handed over: its number, its text, and whether the text was cut.
fn line_value(n: usize, bytes: &[u8], cap: usize) -> Value {
    let cut = bytes.len() > cap;
    let kept = &bytes[..bytes.len().min(cap)];
    json!({
        "n": n,
        "text": String::from_utf8_lossy(kept),
        "cut": cut,
    })
}

/// **Lines from a file, by range or by list**, streamed so a 116 MB transcript costs one line.
fn read_look(real: &Path, spec: &serde_json::Map<String, Value>) -> Value {
    let cap = spec
        .get("max_line_bytes")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(80, LINE_CAP_MAX))
        .unwrap_or(LINE_CAP);
    let want = spec
        .get("lines")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(1, LINES_CAP_MAX))
        .unwrap_or(LINES_CAP);
    let at: Option<Vec<usize>> = spec.get("at").and_then(Value::as_array).map(|list| {
        list.iter()
            .filter_map(Value::as_u64)
            .map(|n| n as usize)
            .collect()
    });
    let from = spec
        .get("from")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).max(1))
        .unwrap_or(1);
    let file = match std::fs::File::open(real) {
        Ok(file) => file,
        Err(e) => return look_error(format!("{}: {e}", real.display())),
    };
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let mut more = false;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut n = 0usize;
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return look_error(format!("{}: {e}", real.display())),
        }
        n += 1;
        let take = match &at {
            Some(list) => list.contains(&n),
            None => n >= from && out.len() < want,
        };
        if !take {
            // Past the window with a range: stop rather than read the file to its end.
            if at.is_none() && n >= from && out.len() >= want {
                more = true;
                break;
            }
            continue;
        }
        if bytes + buf.len() > BYTES_CAP {
            more = true;
            break;
        }
        bytes += buf.len();
        let mut line = &buf[..];
        while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            line = &line[..line.len() - 1];
        }
        out.push(line_value(n, line, cap));
        // A list of line numbers is answered as soon as the last of them is in hand: the rest of
        // the file is not read to find out whether it says anything.
        if let Some(list) = &at {
            if out.len() >= list.len() {
                break;
            }
            continue;
        }
        if out.len() >= want {
            more = true;
            break;
        }
    }
    json!({"exit": 0, "lines": out, "more": more})
}

/// **A directory's entries**, newest first is the plugin's business, not the host's: this answers
/// them in name order and says when the list was cut.
fn list_look(real: &Path, spec: &serde_json::Map<String, Value>) -> Value {
    let limit = spec
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(1, ENTRIES_CAP))
        .unwrap_or(ENTRIES_CAP);
    let meta = match std::fs::metadata(real) {
        Ok(meta) => meta,
        Err(e) => return look_error(format!("{}: {e}", real.display())),
    };
    if !meta.is_dir() {
        return look_error(format!(
            "{} is not a directory — `list` lists a directory, and a file is read with `read`",
            real.display()
        ));
    }
    let entries = match std::fs::read_dir(real) {
        Ok(entries) => entries,
        Err(e) => return look_error(format!("{}: {e}", real.display())),
    };
    let mut out: Vec<Value> = Vec::new();
    let mut more = false;
    for entry in entries.flatten() {
        if out.len() >= limit {
            more = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let (dir, size, mtime) = match entry.metadata() {
            Ok(m) => (
                m.is_dir(),
                m.len(),
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            ),
            // A dangling symlink — `~/.claude/projects` holds one (`memory`) — is an entry with
            // no metadata, and dropping it would hide a name the layer may be looking for.
            Err(_) => (false, 0, 0),
        };
        out.push(json!({"name": name, "dir": dir, "size": size, "mtime": mtime}));
    }
    out.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["name"].as_str().unwrap_or_default())
    });
    json!({"exit": 0, "entries": out, "more": more})
}

/// **Every line of a file that contains `match`**, with its line index and byte offset.
///
/// The byte offset is what makes a hit addressable: a model that wants to read around one can ask
/// for the line number, and a caller that has a file open can seek to the byte. The line number is
/// what `read`'s `at` takes, so the two halves of this vocabulary compose.
fn grep_look(real: &Path, spec: &serde_json::Map<String, Value>) -> Value {
    let Some(needle) = spec.get("match").and_then(Value::as_str) else {
        return look_error("a `grep` look needs `match` — the text to look for".to_string());
    };
    if needle.is_empty() {
        return look_error(
            "a `grep` look's `match` is empty, which matches every line".to_string(),
        );
    }
    let ignore_case = spec
        .get("ignore_case")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let context = spec
        .get("context")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).min(8))
        .unwrap_or(0);
    let limit = spec
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(1, MATCHES_CAP))
        .unwrap_or(20);
    let cap = spec
        .get("max_line_bytes")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(80, LINE_CAP_MAX))
        .unwrap_or(LINE_CAP);
    let file = match std::fs::File::open(real) {
        Ok(file) => file,
        Err(e) => return look_error(format!("{}: {e}", real.display())),
    };
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut offset = 0u64;
    let mut n = 0usize;
    let mut scanned = 0usize;
    let mut matches: Vec<Value> = Vec::new();
    let mut before: std::collections::VecDeque<Value> = std::collections::VecDeque::new();
    let mut more = false;
    let hay = |bytes: &[u8]| -> bool {
        let text = String::from_utf8_lossy(bytes);
        if ignore_case {
            text.to_lowercase().contains(&needle.to_lowercase())
        } else {
            text.contains(needle)
        }
    };
    loop {
        buf.clear();
        let read = match reader.read_until(b'\n', &mut buf) {
            Ok(read) => read,
            Err(e) => return look_error(format!("{}: {e}", real.display())),
        };
        if read == 0 {
            break;
        }
        n += 1;
        scanned += 1;
        let mut line = &buf[..];
        while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            line = &line[..line.len() - 1];
        }
        // **The hit's own line and offset, taken before the context is read**: `after_lines` moves
        // both, and a byte offset reconstructed from truncated text is a number that points
        // somewhere else. The line's own bytes are counted here, before its context, so every
        // offset after a hit is still the start of a line.
        let (hit_line, hit_byte) = (n, offset);
        offset += read as u64;
        if hay(line) {
            if matches.len() >= limit {
                more = true;
                break;
            }
            // **The hit's own line and offset** — see above.
            let after = after_lines(&mut reader, &mut offset, &mut n, &mut scanned, context, cap);
            matches.push(json!({
                "line": hit_line,
                "byte": hit_byte,
                "text": String::from_utf8_lossy(&line[..line.len().min(cap)]),
                "cut": line.len() > cap,
                "before": before.iter().cloned().collect::<Vec<Value>>(),
                "after": after.clone(),
            }));
            // The lines just consumed are the next hit's `before`, so two hits inside one window
            // both carry their context instead of the second losing it.
            for value in after {
                before.push_back(value);
            }
            while before.len() > context {
                before.pop_front();
            }
        } else {
            before.push_back(line_value(n, line, cap));
            while before.len() > context {
                before.pop_front();
            }
        }
    }
    json!({"exit": 0, "matches": matches, "scanned": scanned, "more": more})
}

/// The lines after a hit, consumed from the same reader so a grep stays one pass.
fn after_lines(
    reader: &mut BufReader<std::fs::File>,
    offset: &mut u64,
    n: &mut usize,
    scanned: &mut usize,
    context: usize,
    cap: usize,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for _ in 0..context {
        buf.clear();
        let read = match reader.read_until(b'\n', &mut buf) {
            Ok(read) => read,
            Err(_) => break,
        };
        if read == 0 {
            break;
        }
        *n += 1;
        *scanned += 1;
        *offset += read as u64;
        let mut line = &buf[..];
        while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            line = &line[..line.len() - 1];
        }
        out.push(line_value(*n, line, cap));
    }
    out
}
