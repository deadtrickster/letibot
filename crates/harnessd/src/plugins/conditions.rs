//! **The daemon's half of a plugin condition** — `condition`, `docs/plugins-plan.md` §4.4.
//!
//! `crates/tools` owns the condition's shape: the interval, the state, the poll bookkeeping, and
//! the rule that one tick is at most one plugin call. What it cannot own is the two things that
//! are the daemon's — **the stack** (which layer answers a hook, and what it declared) and **the
//! workspace** (a `read` look resolves against it) — so it asks through [`PluginStack`] and this
//! is the implementation.
//!
//! # Where the seam is installed, and why there
//!
//! [`Monitors`] belongs to the session's process host, which the harness builds when it opens; the
//! plugin stack is built later, when the DAEMON opens the session (`Sessions::open_plugins`). So
//! this holds the same slot the `plugin` tool and `on_tool_call` hold, and `Harness::open` installs
//! it once — a session nobody fills the slot for refuses a `plugin` monitor by name rather than
//! accepting one that will never be polled.
//!
//! # The look, and the gate it goes through
//!
//! A `read` look runs under the answering layer's `read` capability, confined to the workspace,
//! through the same [`crate::plugins::look`] a pane's does. An `exec` look runs the declared argv
//! under that layer's `exec:<program>` capability — no shell, so nothing a plugin wrote can be
//! re-split into more commands — with its output redirected to files so the poller never waits on
//! a pipe.
//!
//! **And it goes through the same gate the model's calls meet** (`docs/plugins-plan.md` §1,
//! principle 2): *a plugin can do nothing the model could not.* The call the gate judges is the
//! one a `bash` of that argv would be — the argv rendered as a command line whose every word is
//! quoted, so the gate reads the words the layer declared and no shell could have made them into
//! anything else. What it may not do is **ask**: a look runs on the session's monitor thread, and
//! a card would park that thread — and the watch with it — until a person answered. So
//! [`letibot_tools::Gate::admit_unasked`] is the entry, and it is the same ladder stopped at the
//! card:
//!
//! * **admitted** (a `permission` rule, a standing grant, a point that admits the class, a D16
//!   measured boundary) → the look runs, exactly as it did before there was a gate here;
//! * **the gate is busy** → nothing runs and nothing is owed: the look is *deferred*, the watch
//!   keeps it and asks again on the poller's next tick. The gate's mutex is held across the model's
//!   own decision and for as long as a person takes to answer, so waiting on it here would park the
//!   session's one monitor thread — and every watch on it — behind somebody's card;
//! * **would have been put to somebody** → it does not run, the layer is handed a look result that
//!   says *would need your approval* and why, and the session is told **once** per (layer,
//!   program, reason) — a watch polling every ninety seconds is not ninety seconds of the same
//!   warning;
//! * **denied by a rule the operator wrote** → it does not run, and the layer is told that instead,
//!   because *somebody said no* and *nobody could be asked* are different sentences.
//!
//! The capability is checked **first**, before the gate: a program the layer did not declare is
//! not a call to adjudicate at all.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use letibot_plugins::Capability;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::pane::LookSpec;
use letibot_tools::exec::monitor::Condition;
use letibot_tools::exec::plugin_condition::{
    Armed, Look, LookChild, NotArmed, PluginCondition, PluginConditions, PluginStack,
};
use letibot_tools::runtime::{GateCall, GateDecision};
use letibot_tools::{Access, ToolRuntime, command_of_argv};
use letibot_transcript::ToolOutcome;
use serde_json::Value;

use crate::plugins::{PluginHost, look};

/// **The gate, as a look asks it**: one question, it may not raise a card, and it may not wait.
///
/// A trait rather than the runtime itself so the daemon can hand over its own gate and a test can
/// answer without building one — the same seam [`PluginStack`] is, one layer down. The single
/// implementation that matters is [`ToolRuntime`]'s, because *the* gate is the session's.
pub trait Unasked: Send + Sync {
    /// What the gate would do with this call if nobody could be asked — or **`None` when the gate is
    /// busy**, which is not an answer at all. See [`letibot_tools::Gate::admit_unasked`].
    fn admit_unasked(&self, call: &GateCall<'_>) -> Option<GateDecision>;
}

impl Unasked for ToolRuntime {
    /// **`try_lock`, and the difference is the whole point.** `ToolRuntime::run` holds this mutex
    /// across the model's own decision on purpose, card and all — for as long as a person takes to
    /// answer — so a look that *waited* for it would park the session's monitor thread behind that
    /// card, and every other watch with it. A gate that is busy is *not now*: `None`, the look is
    /// deferred, and the poller asks again next tick.
    ///
    /// A poisoned lock is the same as it ever was — `into_inner`, because a panic on another thread
    /// is not a reason to stop looking.
    fn admit_unasked(&self, call: &GateCall<'_>) -> Option<GateDecision> {
        let mut gate = match self.gate.try_lock() {
            Ok(gate) => gate,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        Some(gate.admit_unasked(call))
    }
}

/// **One session's conditions, over its plugin stack.** The `PluginConditions` the monitors hold.
pub struct HostConditions {
    slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
    hub: Arc<Hub>,
    /// The session's gate, for an `exec` look. `None` is a session nobody built one for, and a look
    /// it declared for a program then runs nothing: *no gate* is not *every gate open*.
    gate: Option<Arc<dyn Unasked>>,
}

impl HostConditions {
    pub fn new(
        slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
        hub: Arc<Hub>,
        gate: Option<Arc<dyn Unasked>>,
    ) -> Arc<HostConditions> {
        Arc::new(HostConditions { slot, hub, gate })
    }
}

impl PluginConditions for HostConditions {
    fn arm(&self, plugin: &str, args: Value) -> Result<Armed, NotArmed> {
        let host = self
            .slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| NotArmed {
                why: "this session has no plugin host yet".into(),
                detail: "the daemon builds one when it opens a session it serves, and `plugin \
                         stack` would list nothing in this one. Nothing was declared."
                    .into(),
            })?;
        let stack = Arc::new(StackOver {
            host,
            hub: Arc::clone(&self.hub),
            gate: self.gate.clone(),
        });
        let condition = PluginCondition::arm(stack, plugin, args)?;
        let note = Some(format!(
            "watching through the plugin condition `{}` — the layer decides what to look at and \
             what a firing is; `monitor` with action \"retire\" ends it.",
            condition.describe()
        ));
        Ok(Armed {
            condition: condition as Arc<dyn Condition>,
            note,
        })
    }
}

/// The stack, as a condition asks it.
struct StackOver {
    host: Arc<PluginHost>,
    hub: Arc<Hub>,
    gate: Option<Arc<dyn Unasked>>,
}

impl PluginStack for StackOver {
    fn ask(&self, hook: &str, input: &Value) -> Option<Value> {
        let resolved = self.host.ask(&self.hub, hook, input)?;
        resolved.answer
    }

    fn look(&self, hook: &str, ask: &letibot_plugin::condition::LookAsk) -> Look {
        let caps = self.host.capabilities_for(hook);
        let workspace = self.host.workspace();
        match ask {
            letibot_plugin::condition::LookAsk::Read(read) => {
                // The same look a pane runs, so the confinement and the capability check are one
                // implementation rather than two that can disagree.
                let value = look::run(&LookSpec::Read { read: read.clone() }, &workspace, &caps);
                Look::Done(result_of(&value))
            }
            letibot_plugin::condition::LookAsk::Exec(argv) => {
                let Some(program) = argv.first() else {
                    return Look::Done(letibot_plugin::condition::LookResult::refused(
                        "an `exec` look needs a program to run",
                    ));
                };
                // **The capability first**, because it is not the gate's question: a program this
                // layer never declared is not a call anybody adjudicates, it is a layer reaching
                // past what its `plugin.toml` says.
                if !caps
                    .iter()
                    .any(|c| matches!(c, Capability::Exec(p) if p == program))
                {
                    return Look::Done(letibot_plugin::condition::LookResult::refused(format!(
                        "the layer that answered did not declare `exec:{program}`, so nothing was \
                         run — a capability is declared in plugin.toml and enforced here"
                    )));
                }
                if let Some(verdict) = self.adjudicate(hook, program, argv, &workspace) {
                    return verdict;
                }
                match ChildLook::spawn(argv, &workspace) {
                    Ok(child) => Look::Running(Box::new(child)),
                    Err(why) => Look::Done(letibot_plugin::condition::LookResult::refused(why)),
                }
            }
        }
    }
}

impl StackOver {
    /// **The gate's half of an `exec` look**, or `None` when it may run.
    ///
    /// The call is the one a `bash` of this argv would be: the same tool name, the same declared
    /// access, and a `command` built from the argv with every word quoted — so a `permission` rule
    /// written for the model's `gh pr view` covers the plugin's identical look, and one written
    /// against a *different* command does not. `turn_id` and `call_id` are empty because no turn
    /// and no call own a watch's look; the audit row says so rather than borrowing an id.
    ///
    /// `None` is *run it*. Anything else is what the caller returns: a refusal to hand the layer, or
    /// [`Look::Deferred`] — the gate was busy, so nothing was decided, nothing is run, and nothing is
    /// said to anybody.
    fn adjudicate(
        &self,
        hook: &str,
        program: &str,
        argv: &[String],
        workspace: &std::path::Path,
    ) -> Option<Look> {
        let Some(gate) = self.gate.as_deref() else {
            let why = format!(
                "`{program}` was not run: this session has no gate, so nothing could judge whether \
                 a program a plugin declared may run"
            );
            self.say(hook, &why);
            return Some(Look::Done(letibot_plugin::condition::LookResult::refused(
                why,
            )));
        };
        let args = serde_json::json!({ "command": command_of_argv(argv) });
        // The scripts a look would run are not read: a `read`/`exec` look is a program the layer
        // declared, and the reader for the gate's brief is the runtime's, which this is not. A
        // look that names a script is judged on the words, and the capability says which program.
        let scripts: [letibot_tools::runtime::ScriptSource; 0] = [];
        let workspace = workspace.display().to_string();
        let call = GateCall {
            name: "bash",
            access: Access::Exec,
            args: &args,
            turn_id: "",
            call_id: "",
            workspace: &workspace,
            target_exists: None,
            scripts: &scripts,
        };
        let Some(decision) = gate.admit_unasked(&call) else {
            // **The gate is busy with somebody else's call.** Its mutex is held across the model's
            // own decision (card and all), so this is *not now* rather than *no*: nothing runs,
            // nothing is said to the layer as a result, and — deliberately — no notice is
            // published, because a card that a person is looking at is not news about the plugin.
            // The watch keeps its look and the poller asks again next tick.
            return Some(Look::Deferred);
        };
        let GateDecision::Refuse { outcome, tell } = decision else {
            return None;
        };
        let why = format!("`{program}`: {}", said(&outcome, &tell));
        self.say(hook, &why);
        Some(Look::Done(letibot_plugin::condition::LookResult::refused(
            why,
        )))
    }

    /// **Said once per (layer, program, reason)** — the layer is told through its look result, and
    /// the session is told through the host's once-only channel.
    fn say(&self, hook: &str, why: &str) {
        let layer = self
            .host
            .answering(hook)
            .map(|l| format!("{} ({})", l.name, l.scope))
            .unwrap_or_else(|| "a layer".to_string());
        self.host.look_refused(
            &self.hub,
            format!(
                "the plugin layer `{layer}` declared a look the gate would not run: {why} — the \
                 look did not run, and its monitor keeps asking until the layer changes it"
            ),
        );
    }
}

/// **What the gate's refusal means, in the sentence the layer is handed.**
///
/// The two classes are two different facts and a plugin acts differently on each: `NotRun` is
/// *this needed a person and a look cannot reach one* — the program may be fine, and the operator
/// could grant it — while `Denied` is *a rule says no*, and no grant taken from here would be
/// honoured.
fn said(outcome: &ToolOutcome, tell: &str) -> String {
    match outcome {
        ToolOutcome::NotRun { why } => why.clone(),
        ToolOutcome::Denied { .. } => {
            let tell = tell.trim();
            if tell.is_empty() {
                "the gate denied it".to_string()
            } else {
                format!("the gate denied it — {tell}")
            }
        }
        // Neither class, which a gate of this tree does not produce for a look and a hand-written
        // one might: the outcome speaks for itself rather than being folded into one of the two.
        other => format!("the gate refused it: {other:?}"),
    }
}

/// A look's JSON answer, as the typed result a plugin is handed.
fn result_of(value: &Value) -> letibot_plugin::condition::LookResult {
    letibot_plugin::condition::LookResult {
        exit: value["exit"].as_i64().unwrap_or(1) as i32,
        stdout: value["stdout"].as_str().unwrap_or_default().to_string(),
        stderr: value["stderr"].as_str().unwrap_or_default().to_string(),
    }
}

/// **A look that is a running program.** Its output goes to files rather than to pipes: a child
/// that writes more than a pipe's buffer would block, and this is polled by the session's one
/// monitor thread — the whole reason it is not run synchronously.
struct ChildLook {
    child: Child,
    out: PathBuf,
    err: PathBuf,
}

/// Enough to keep two conditions' files apart in one process.
static LOOKS: AtomicU64 = AtomicU64::new(0);

impl ChildLook {
    fn spawn(argv: &[String], cwd: &std::path::Path) -> Result<ChildLook, String> {
        let tag = format!(
            "{}-{}",
            std::process::id(),
            LOOKS.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir();
        let out = dir.join(format!("letibot-look-{tag}.out"));
        let err = dir.join(format!("letibot-look-{tag}.err"));
        let open = |path: &PathBuf| {
            std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))
        };
        let child = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(open(&out)?))
            .stderr(Stdio::from(open(&err)?))
            .spawn()
            .map_err(|e| {
                format!(
                    "`{}` could not be run ({e}) — the look declared it, the layer holds the \
                     capability and the gate admitted it, but the program is not on this machine",
                    argv[0]
                )
            })?;
        Ok(ChildLook { child, out, err })
    }
}

impl LookChild for ChildLook {
    fn poll(&mut self) -> Option<letibot_plugin::condition::LookResult> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(letibot_plugin::condition::LookResult {
                exit: status.code().unwrap_or(1),
                stdout: std::fs::read_to_string(&self.out).unwrap_or_default(),
                stderr: std::fs::read_to_string(&self.err).unwrap_or_default(),
            }),
            // Still running: nothing to hand over, and the poller must not wait.
            Ok(None) => None,
            Err(why) => Some(letibot_plugin::condition::LookResult::refused(format!(
                "the look could not be polled: {why}"
            ))),
        }
    }
}

impl Drop for ChildLook {
    /// **A watch that ends takes its output with it**: `retire`, a ttl, a session closing. The
    /// child is left to the scope tree, which owns processes — killing it here would be this
    /// struct second-guessing the reaper.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.out);
        let _ = std::fs::remove_file(&self.err);
    }
}
