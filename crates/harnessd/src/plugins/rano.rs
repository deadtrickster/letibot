//! The one `Loader` production uses: a plugin is a wasm module, and rano is the host that
//! loads one. The engine lives here and not in `crates/plugins` because that crate talks to
//! a module through a trait on purpose: precedence, the answer protocol, hot reload and the
//! gate are testable with a table of functions and no runtime, and its tests compile
//! nothing. The loader grants no rano capability, because rano's `read-dir` is not this
//! contract's `read`: a plugin never does I/O, so its declared capabilities are enforced by
//! [`crate::plugins::look`] when a look runs and are not importable at all.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use letibot_plugins::{Capability, HOOK_PREFIXES, Loader, Module};

/// A module, as the stack sees it. The `Plugin` this wraps is `Send + Sync`, which is what lets one
/// `Arc` be shared by a session's stack, the gate's shadow and every pane render.
pub struct RanoLoader;

impl Loader for RanoLoader {
    fn load(&self, wasm: &Path, caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        // The caps are checked here and not imported: a capability this host cannot grant must be
        // a refusal a model reads, not a silent grant of nothing. A module that imported something
        // claiming to be `http:` or `exec:` would be a plugin doing its own I/O.
        let plugin = rano::plugin::Plugin::load_hooks(
            wasm,
            rano::plugin::Capabilities::default(),
            HOOK_PREFIXES,
        )
        .map_err(|why| format!("{}: {why}", wasm.display()))?;
        let _ = caps;
        Ok(Box::new(RanoModule { plugin }))
    }

    /// The same engine, on bytes the binary carries - a built-in layer. The name stands in for a
    /// path, because a module that came from memory has none to name in a refusal.
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

    /// A built-in's module, compiled once for the whole process: compiling one costs seconds of CPU
    /// and every session open paid it again. What is shared is the module and not a session's state - a call runs in a fresh store. Keyed by layer name; a failure is not kept.
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
        // serialise every other built-in behind it. The second to arrive finds the first's module
        // and drops its own copy.
        let module: Arc<dyn Module> = Arc::from(self.load_bytes(name, bytes, caps)?);
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        Ok(cache
            .entry(name.to_string())
            .or_insert_with(|| module.clone())
            .clone())
    }
}

/// The compiled built-ins this process has, by layer name. A `static` and not a field because a
/// session builds a loader per host while the compile it saves is per process. Nothing is removed
/// a binary's built-ins are `include_bytes!`d into it and cannot change while it runs.
static COMPILED: OnceLock<Mutex<BTreeMap<String, Arc<dyn Module>>>> = OnceLock::new();

struct RanoModule {
    plugin: rano::plugin::Plugin,
}

impl Module for RanoModule {
    fn hooks(&self) -> Vec<String> {
        self.plugin.hooks().to_vec()
    }

    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String> {
        // `Ok(None)` is rano's *unchanged* - the layer passed. Anything else is an `Err` the stack
        // records against the layer and answers from below.
        self.plugin.call(hook, input)
    }

    fn fuel(&self) -> u64 {
        self.plugin.last_fuel()
    }
}
