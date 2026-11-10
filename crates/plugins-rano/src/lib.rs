//! The production door for plugin modules: one `Loader`, and one compiled module per distinct
//! module in the process.
//!
//! Every host that runs a layer - the daemon, the head, the bridge - asks rano to compile a wasm
//! module, and a compile costs seconds of CPU. Each host had its own copy of that loader and only
//! the daemon kept a compiled module, so the head and the bridge compiled all thirteen built-ins
//! again on every host build - in the head on the UI thread. Here it is once, keyed by the SHA-256
//! of the bytes, which is the identity a pin and the gate already use: a second host build in the
//! same process compiles nothing at all.
//!
//! `letibot_plugins` stays free of a wasm engine and this crate is why it can: the engine is one
//! implementation of its `Loader` seam, and a test's table of answers is another.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use letibot_plugins::{Capability, HOOK_PREFIXES, Loader, Module};
use sha2::{Digest, Sha256};

/// How many compiled modules this process keeps. A built-in is `include_bytes!`d and cannot change,
/// and a placed layer is recompiled only when its bytes move, so the count of distinct modules a
/// run holds is small; the cap is here so a rebuild loop cannot grow it without end.
const KEEP: usize = 64;

/// The one `Loader` production uses. A unit: what it saves is the process's, so a loader per host
/// is the intended use and no two of them can hold different modules.
#[derive(Default)]
pub struct RanoLoader;

impl RanoLoader {
    /// How many modules this process has compiled that were not already there - a miss, not an ask.
    /// A second host build has nothing to compile, and this is how a test says so.
    pub fn compiles() -> usize {
        COMPILES.load(Ordering::SeqCst)
    }

    /// The module these bytes are, compiled once for the whole process. `from` is what a refusal
    /// names: a layer's path, or a built-in's name, because a module from memory has no path.
    fn one(from: &From<'_>, bytes: &[u8]) -> Result<Arc<dyn Module>, String> {
        let sha = Sha256::digest(bytes).into();
        if let Some(hit) = cached(&sha) {
            return Ok(hit);
        }
        let plugin = rano::plugin::Plugin::from_bytes_hooks(
            bytes,
            from.display(),
            // A capability named in a manifest is a request; what rano grants an import is the
            // default set, because a layer with `exec:` or `http:` reaching the host's own I/O
            // would be a plugin doing its own work behind the gate's back.
            rano::plugin::Capabilities::default(),
            HOOK_PREFIXES,
        )
        .map_err(|why| from.refusal(&why))?;
        COMPILES.fetch_add(1, Ordering::SeqCst);
        let module: Arc<dyn Module> = Arc::new(RanoModule { plugin });
        Ok(remember(sha, module))
    }
}

/// How many modules this process has compiled. A `static` beside the cache because the cache is
/// one: two hosts in one process cannot hold two answers about the same bytes.
static COMPILES: AtomicUsize = AtomicUsize::new(0);

impl Loader for RanoLoader {
    /// The bytes are read here and compiled from memory, so the hash that keys the cache is
    /// exactly the bytes that were compiled: a file rewritten between the two would otherwise
    /// answer as a module it is not.
    fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        let bytes = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
        let module = RanoLoader::one(&From::Path(wasm), &bytes)?;
        Ok(Box::new(Shared(module)))
    }

    fn load_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        _caps: &[Capability],
    ) -> Result<Box<dyn Module>, String> {
        let module = RanoLoader::one(&From::BuiltIn(name), bytes)?;
        Ok(Box::new(Shared(module)))
    }

    /// The same module, shared rather than boxed: a stack keeps an `Arc`, and one `Arc` several
    /// stacks share is what makes the second session's stack free.
    fn load_builtin(
        &self,
        name: &str,
        bytes: &[u8],
        _caps: &[Capability],
    ) -> Result<Arc<dyn Module>, String> {
        RanoLoader::one(&From::BuiltIn(name), bytes)
    }
}

/// What a module came from - read for the sentence a refusal carries, never for the compile.
enum From<'a> {
    Path(&'a Path),
    BuiltIn(&'a str),
}

impl From<'_> {
    /// The path rano keeps on the module: a built-in has none, so it gets the placeholder the
    /// daemon and the head used to spell - one spelling now, in one place.
    fn display(&self) -> PathBuf {
        match self {
            From::Path(path) => (*path).to_path_buf(),
            From::BuiltIn(name) => PathBuf::from(format!("<built-in {name}>")),
        }
    }

    fn refusal(&self, why: &str) -> String {
        match self {
            From::Path(path) => format!("{}: {why}", path.display()),
            From::BuiltIn(name) => format!("built-in `{name}`: {why}"),
        }
    }
}

/// The compiled modules this process holds, by the SHA-256 of their bytes. A rebuild leaves its old
/// module here until the cap needs the room - and dropping the entry frees nothing while a stack
/// still holds the `Arc`, which is why eviction is safe in either order.
static COMPILED: OnceLock<Mutex<BTreeMap<[u8; 32], Arc<dyn Module>>>> = OnceLock::new();

fn cache() -> &'static Mutex<BTreeMap<[u8; 32], Arc<dyn Module>>> {
    COMPILED.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn cached(sha: &[u8; 32]) -> Option<Arc<dyn Module>> {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(sha)
        .cloned()
}

/// Keep a compiled module, unless another thread compiled these bytes first - that one wins, so
/// two hosts opening at once wait for one compile rather than serialising every other module
/// behind it. A `BTreeMap` and not a queue because the keys are hashes and there is no order worth
/// keeping: the cap is over the count of distinct modules, and the eviction picks by hash.
fn remember(sha: [u8; 32], module: Arc<dyn Module>) -> Arc<dyn Module> {
    let mut held = cache().lock().unwrap_or_else(|e| e.into_inner());
    let mine = held.entry(sha).or_insert_with(|| module.clone()).clone();
    while held.len() > KEEP {
        let Some(any) = held.keys().next().copied() else {
            break;
        };
        held.remove(&any);
    }
    mine
}

/// A `Module` behind an `Arc`, handed back as the `Box` a `Loader::load` answers with.
struct Shared(Arc<dyn Module>);

impl Module for Shared {
    fn hooks(&self) -> Vec<String> {
        self.0.hooks()
    }

    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String> {
        self.0.call(hook, input)
    }

    fn fuel(&self) -> u64 {
        self.0.fuel()
    }
}

/// A module, as the stack sees it. The `Plugin` this wraps is `Send + Sync`, which is what lets one
/// `Arc` be shared by a session's stack, the gate's shadow and every pane render.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `compiles` is a MISS count, so a test cannot assume a cold process cache: it asserts what
    /// the second build adds, which is the claim, whichever test filled the cache first.
    #[test]
    fn a_second_host_build_compiles_no_builtin() {
        let mut stack_a = letibot_plugins::Stack::discover(None, None, None, Arc::new(RanoLoader));
        let failed = letibot_builtins::install(&mut stack_a);
        assert!(failed.is_empty(), "every built-in loads: {failed:?}");
        let layers = stack_a.layers().len();
        assert!(layers > 1, "the binary carries its layers: {layers}");

        // A second host, its own stack and its own loader: the same process, and so the same
        // compiled modules.
        let before = RanoLoader::compiles();
        let mut stack_b = letibot_plugins::Stack::discover(None, None, None, Arc::new(RanoLoader));
        let failed = letibot_builtins::install(&mut stack_b);
        assert!(failed.is_empty(), "every built-in loads: {failed:?}");
        assert_eq!(
            RanoLoader::compiles() - before,
            0,
            "the second host build found every module compiled"
        );
        assert_eq!(layers, stack_b.layers().len(), "the same stack, twice");
    }

    /// The bytes are the key, so a layer rewritten to bytes this process already compiled is that
    /// module - the second name is one more stack, not one more compile.
    #[test]
    fn the_bytes_are_the_key_and_not_the_name() {
        let wasm = builtin("todo");
        let one = RanoLoader.load_builtin("a", wasm, &[]).expect("compiles");
        let compiled = RanoLoader::compiles();
        let same = RanoLoader.load_builtin("b", wasm, &[]).expect("a hit");
        assert_eq!(
            RanoLoader::compiles(),
            compiled,
            "the second name compiled nothing"
        );
        assert!(
            Arc::ptr_eq(&one, &same),
            "the second name holds the same module"
        );
    }

    /// A module that is not wasm is refused by name, with the source in the sentence.
    #[test]
    fn a_refusal_names_what_the_bytes_came_from() {
        let before = RanoLoader::compiles();
        let Err(why) = RanoLoader.load_bytes("nonsense", b"not a module", &[]) else {
            panic!("a file that is not a module must be refused");
        };
        assert!(why.starts_with("built-in `nonsense`:"), "{why}");
        assert_eq!(RanoLoader::compiles(), before, "a failure is not kept");
    }

    fn builtin(name: &str) -> &'static [u8] {
        let found = letibot_builtins::table()
            .iter()
            .find(|b| b.name == name)
            .expect("the built-in the test asks for");
        found.wasm
    }
}
