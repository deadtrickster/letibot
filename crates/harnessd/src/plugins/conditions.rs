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
//! # The look, and the one thing this cannot do yet
//!
//! A `read` look runs under the answering layer's `read` capability, confined to the workspace,
//! through the same [`crate::plugins::look`] a pane's does. An `exec` look runs the declared argv
//! under that layer's `exec:<program>` capability — no shell, so nothing a plugin wrote can be
//! re-split into more commands — with its output redirected to files so the poller never waits on
//! a pipe. **The exec look does not go through the adjudicator**: the capability is the consent
//! (a project layer's is reviewed with the merge, a session layer's is the operator's), and a
//! look that raised a card would be a poller waiting for a person. `docs/plugins-plan.md` §1
//! principle 2 wants the gate here too, and that is written down as the gap it is rather than
//! quietly skipped.

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
use serde_json::Value;

use crate::plugins::{PluginHost, look};

/// **One session's conditions, over its plugin stack.** The `PluginConditions` the monitors hold.
pub struct HostConditions {
    slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
    hub: Arc<Hub>,
}

impl HostConditions {
    pub fn new(slot: Arc<Mutex<Option<Arc<PluginHost>>>>, hub: Arc<Hub>) -> Arc<HostConditions> {
        Arc::new(HostConditions { slot, hub })
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
                if !caps
                    .iter()
                    .any(|c| matches!(c, Capability::Exec(p) if p == program))
                {
                    return Look::Done(letibot_plugin::condition::LookResult::refused(format!(
                        "the layer that answered did not declare `exec:{program}`, so nothing was \
                         run — a capability is declared in plugin.toml and enforced here"
                    )));
                }
                match ChildLook::spawn(argv, &workspace) {
                    Ok(child) => Look::Running(Box::new(child)),
                    Err(why) => Look::Done(letibot_plugin::condition::LookResult::refused(why)),
                }
            }
        }
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
                    "`{}` could not be run ({e}) — the look declared it and the layer holds the \
                     capability, but the program is not on this machine",
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
