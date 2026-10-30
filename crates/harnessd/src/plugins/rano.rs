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

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

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

    /// **A built-in's module, compiled once for the whole process.**
    ///
    /// MEASURED 2026-10-28 in a debug host, `rano::plugin::Plugin::from_bytes_hooks`:
    /// `claude_transcript` (232,869 bytes) **1.97 s**, `sessions` (173,119 bytes) **1.39 s**. That is
    /// wasmtime compiling cranelift's output for a host built without optimisation, and every
    /// session open paid it again — two tests went red on the price alone (`no_spin`'s three-second
    /// window was entirely compile; the idle exit's 800 ms window had already passed when
    /// `Sessions::open_first` returned).
    ///
    /// The module is the same bytes for every session in the binary, so it is compiled once and
    /// the `Arc` is handed out. What is shared is the COMPILED MODULE, not a session's state: a
    /// call runs in a fresh store (`Plugin::call`), the fuel counter is an atomic, and the stack
    /// already keeps its module behind an `Arc` — so several sessions wired to these bytes answer
    /// exactly as several sessions with their own copies did, for one compile instead of N.
    ///
    /// Keyed by the layer's name, which the table makes unique within one binary (`crates/builtins`
    /// names a layer after its directory). A failure is NOT kept: it is cheap to meet again, and a
    /// refusal recorded against one stack is that stack's to show.
    fn load_builtin(
        &self,
        name: &str,
        bytes: &[u8],
        caps: &[Capability],
    ) -> Result<Arc<dyn Module>, String> {
        let cache = COMPILED.get_or_init(|| Mutex::new(BTreeMap::new()));
        if let Some(module) = cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
        {
            return Ok(module);
        }
        // Compiled outside the lock: two sessions opening at once should wait for one compile, not
        // serialise every other built-in behind it. The second one to arrive finds the first's
        // module in the map and drops its own copy.
        let module: Arc<dyn Module> = Arc::from(self.load_bytes(name, bytes, caps)?);
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        Ok(cache
            .entry(name.to_string())
            .or_insert_with(|| module.clone())
            .clone())
    }
}

/// **The compiled built-ins this process has**, by layer name — [`RanoLoader::load_builtin`].
///
/// A `static` rather than a field because `RanoLoader` is a unit struct in twenty places and a
/// session builds a loader per host: the compile it saves is per PROCESS, so the place to keep it
/// is the process. Nothing removes an entry: a binary's built-ins are `include_bytes!`d into it and
/// cannot change while it runs.
static COMPILED: OnceLock<Mutex<BTreeMap<String, Arc<dyn Module>>>> = OnceLock::new();

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
