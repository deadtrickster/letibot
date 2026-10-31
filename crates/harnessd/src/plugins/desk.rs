//! **The `plugin` tool's daemon half**: one session's plugin host, behind
//! [`letibot_tools::builtins::plugin::PluginDesk`].
//!
//! # Why a slot instead of the host itself
//!
//! The tool is seated when the harness opens (`Harness::open`, which builds the tool registry) and
//! the plugin host is built when the DAEMON opens the session (`Sessions::open_plugins`, which is
//! the one place a hub and a pane driver exist). So this holds a slot the daemon fills, which is
//! the same shape [`crate::plugins::PluginHost`]'s own notices use: a live `Arc` handed over once,
//! read through a lock.
//!
//! **A session with no host is a real state and it says so**: a subagent is opened by its parent's
//! runner, not by `Sessions`, so nothing fills its slot — and its answer is a sentence naming that,
//! never an absence that reads as a capability this build lacks.
//!
//! # What needs the live host and what does not
//!
//! `scaffold` and `build` are file and cargo operations on the session's OWN plugin directory, so
//! they work in any session: a source written and compiled is useful even before anything can load
//! it. `load`, `stack`, `disable`, `enable` and `test` ask the stack the head's panes are drawn
//! from — one stack, because two would be two answers to *what is in force*.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::hub::Hub;
use letibot_tools::builtins::plugin::{PluginDesk, Refusal};
use serde_json::Value;

use crate::plugins::scaffold::{self, Scaffolded};
use crate::plugins::{PluginHost, Refused, contract};

/// **One session's plugin loop.**
pub struct SessionPlugins {
    session: String,
    dir: Option<PathBuf>,
    /// The live host, filled by `Sessions::open_plugins` when the daemon opens this session.
    host: Arc<Mutex<Option<Arc<PluginHost>>>>,
    /// The session's hub: `load` and `disable` publish, and only a hub has a head to publish to.
    hub: Arc<Hub>,
}

impl SessionPlugins {
    pub fn new(
        session: impl Into<String>,
        dir: Option<PathBuf>,
        host: Arc<Mutex<Option<Arc<PluginHost>>>>,
        hub: Arc<Hub>,
    ) -> SessionPlugins {
        SessionPlugins {
            session: session.into(),
            dir,
            host,
            hub,
        }
    }

    /// The live host, or the refusal that says why there is none.
    fn host(&self) -> Result<Arc<PluginHost>, Refusal> {
        self.host
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| {
                Refusal::new(
                    "this session has no plugin host yet",
                    format!(
                        "the daemon builds one when it opens a session it serves, and `{}` was \
                         opened by its parent's runner rather than by the daemon (a subagent); \
                         `plugin scaffold` and `plugin build` still work here, and `plugin load` \
                         needs the session the operator is looking at",
                        self.session
                    ),
                )
            })
    }
}

/// The host's own refusal shape, in the tool's — the one place the two crates meet.
fn refused(r: Refused) -> Refusal {
    Refusal::new(r.why, r.detail)
}

impl PluginDesk for SessionPlugins {
    fn scaffold(&self, name: &str, purpose: &str, hook: &str) -> Result<String, Refusal> {
        let Some(dir) = self.dir.as_deref() else {
            return Err(Refusal::new(
                "this session has no plugin directory",
                "a session layer lives under the letibot data dir (<data>/sessions/<id>/plugins), \
                 and this daemon could not resolve that directory",
            ));
        };
        let Scaffolded {
            dir,
            files,
            sdk,
            types,
            manifest,
            hook,
            kind,
            pane,
        } = scaffold::write(dir, name, purpose, hook).map_err(Refusal::only)?;
        // **What the layer is for, in the words of the kind it answers** — a pane is drawn, the
        // four patch points are not, and a sentence that said "its pane will be drawn as """ was
        // how this read before there was more than one kind.
        let drawn = if kind == "pane" {
            format!("its pane will be drawn as `{pane}`")
        } else {
            format!(
                "it answers `{hook}`, which this host asks on every {} ",
                match kind.as_str() {
                    "notice" => "notice of that kind",
                    "on_tool_call" => "tool call, before the gate",
                    "on_tool_result" => "successful tool result",
                    _ => "poll of a monitor it was armed for",
                }
            )
        };
        let mut said = format!(
            "scaffolded `{name}` at {}\n\n  {}\n\nits plugin.toml is {manifest} — {drawn}.",
            dir.display(),
            files.join("\n  ")
        );
        if !sdk.found {
            said.push_str(&format!(
                "\n\n**The rano SDK was not found at {}**, which is what the crate's Cargo.toml \
                 points at, so `plugin build` will refuse until it is there: set \
                 `LETIBOT_RANO_SDK` to a rano checkout's `sdks/rust`, or scaffold again with it \
                 set.",
                sdk.path
            ));
        }
        if !types.found {
            said.push_str(&format!(
                "\n\n**The hook contracts were not found at {}**, which is what this crate's \
                 Cargo.toml points at (the tree this daemon was built from); set \
                 `LETIBOT_PLUGIN_SDK` to that checkout's `crates/plugin-sdk` and scaffold again.",
                types.path
            ));
        }
        said.push_str(&format!(
            "\n\nnext: edit `src/lib.rs`'s `{}` (the contract is the `plugin_hooks` tool), then \
             `plugin build {name}`, `plugin test {name}`, `plugin load {name}`.",
            if kind == "pane" { "view" } else { "answer" }
        ));
        // **Provenance, recorded at the write** — D17's third arm. `scaffold` is a model's write
        // like any other, and a layer whose bytes nobody recorded is a layer whose authorship is
        // a guess from its path. The files are read back rather than the template's strings used:
        // what is on disk is what is recorded.
        let book = letibot_plugins::Trust::open_default();
        for rel in [letibot_plugins::MANIFEST, "src/lib.rs"] {
            let path = dir.join(rel);
            if let Ok(bytes) = std::fs::read(&path) {
                book.note_write(&path, &bytes, &self.session);
            }
        }
        Ok(said)
    }

    fn build(&self, name: &str) -> Result<String, Refusal> {
        self.host()?.build(name).map_err(refused)
    }

    fn load(&self, name: &str) -> Result<String, Refusal> {
        let host = self.host()?;
        host.load(&self.hub, name).map_err(refused)
    }

    fn stack(&self) -> Result<String, Refusal> {
        // A session with no host has no stack, but the question is still answerable honestly: the
        // honest answer names the one thing missing rather than an empty stack that reads as *no
        // plugins are installed*.
        Ok(self.host()?.stack_doc())
    }

    fn set_enabled(&self, what: &str, enabled: bool) -> Result<String, Refusal> {
        let host = self.host()?;
        host.set_enabled(&self.hub, what, enabled).map_err(refused)
    }

    fn test(&self, what: Option<&str>, input: Option<&Value>) -> Result<String, Refusal> {
        self.host()?.test(what, input).map_err(refused)
    }

    fn hooks(&self, hook: Option<&str>) -> Result<String, Refusal> {
        // The contracts do not depend on this session at all — they are the host's types — so a
        // session with no plugin host still gets them, which is what a subagent writing a plugin
        // for its parent needs.
        contract::hooks_doc(hook).map_err(Refusal::only)
    }
}
