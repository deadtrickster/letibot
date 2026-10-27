//! **The one `letibot_plugins::Loader` production uses**: a plugin is a wasm module, and rano is
//! the host that loads one.
//!
//! # Why the engine lives here and not in `crates/plugins`
//!
//! `letibot_plugins` talks to a module through a trait on purpose (`docs/plugins-slice1.md` C2):
//! precedence, the answer protocol, hot reload and the gate are testable with a table of
//! functions and no runtime at all, and that crate's tests compile nothing. The engine is one
//! implementation of [`Loader`], the daemon is the binary that runs plugins, and the head links
//! no JIT — which is the same split `TerminalDriver` makes for a pty, one seam over.
//!
//! # Capabilities
//!
//! The loader is handed the manifest's capabilities and grants **no** rano capability, because
//! rano's are not this contract's: rano's one capability is `read-dir`, an import a folder
//! viewer uses, and letibot's `read` is a *look* — the host reads the file and hands the plugin
//! the text (`docs/plugins-plan.md` §1, principle 1: plugins never do I/O themselves). So a
//! plugin's declared capabilities are enforced by [`crate::plugins::look`] when a look is run,
//! and by the wasm side they are not importable at all — the module gets the default (empty)
//! grant, so an import beyond it is a load failure naming the import.
//!
//! # What a call costs
//!
//! `Plugin::call` runs one hook in a fresh store with rano's own fuel budget, so a hook that
//! loops is a bounded cost rather than a hung daemon, and `last_fuel` is what the gate reports
//! as a pane's price.

use std::path::Path;

use letibot_plugins::{Capability, HOOK_PREFIXES, Loader, Module};

/// **A module, as the stack sees it.** The `Plugin` this wraps is `Send + Sync` (wasmtime's
/// engine and module are, and its fuel counter is an atomic), which is what lets one `Arc` be
/// shared by a session's stack, the gate's shadow and every pane render.
pub struct RanoLoader;

impl Loader for RanoLoader {
    fn load(&self, wasm: &Path, caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        // **The caps are checked here and not imported.** A capability this host does not know
        // how to grant must be a refusal a model reads, not a silent grant of nothing: `http:`
        // and `exec:` are declared in a manifest, and a module that imported something claiming
        // to be them would be a plugin doing its own I/O.
        let plugin = rano::plugin::Plugin::load_hooks(
            wasm,
            rano::plugin::Capabilities::default(),
            HOOK_PREFIXES,
        )
        .map_err(|why| format!("{}: {why}", wasm.display()))?;
        let _ = caps;
        Ok(Box::new(RanoModule { plugin }))
    }

    /// **The same engine, on bytes the binary carries** — a built-in layer (`crates/builtins`).
    ///
    /// `rano::plugin::Plugin::from_bytes_hooks` is the door rano already had for its own
    /// fixtures; a built-in is the same shape of caller. The name stands in for a path, because a
    /// module that came from memory has none to name in a refusal.
    fn load_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        caps: &[Capability],
    ) -> Result<Box<dyn Module>, String> {
        let plugin = rano::plugin::Plugin::from_bytes_hooks(
            bytes,
            std::path::PathBuf::from(format!("<built-in {name}>")),
            rano::plugin::Capabilities::default(),
            HOOK_PREFIXES,
        )
        .map_err(|why| format!("built-in `{name}`: {why}"))?;
        let _ = caps;
        Ok(Box::new(RanoModule { plugin }))
    }
}

struct RanoModule {
    plugin: rano::plugin::Plugin,
}

impl Module for RanoModule {
    fn hooks(&self) -> Vec<String> {
        self.plugin.hooks().to_vec()
    }

    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String> {
        // `Ok(None)` is rano's *unchanged* — the layer passed. Anything else is an `Err` the
        // stack records against the layer and answers from below, which is §1 principle 4.
        self.plugin.call(hook, input)
    }

    fn fuel(&self) -> u64 {
        self.plugin.last_fuel()
    }
}
