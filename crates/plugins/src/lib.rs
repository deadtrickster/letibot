//! Plugins: the layer stack, hot reload and the shadow gate.
//! A plugin is a wasm module answering text hooks; the topmost layer that answers wins and the bottom is the defaults.
//! No wasm engine here: [`Module`] and [`Loader`] are seams that tests fake.

use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};

mod admit;
mod gate;
mod manifest;
pub mod paths;
mod recorder;
mod refusals;
mod stack;
mod trust;

#[cfg(test)]
mod testing;

pub use admit::{Admission, Scopes, admit};
pub use gate::{BuildError, Builder, CargoBuilder, GateReport, ReplayDiff, gate};
pub use manifest::{Capability, Manifest, Scope, ToolDecl};
pub use recorder::Recorder;
pub use refusals::{KEEP as REFUSALS_KEEP, Refusal, Refusals};
pub use stack::{
    BrokenPin, Candidate, DeclaredTool, Failure, Layer, PreparedBuiltin, Resolved, Stack,
};
pub use trust::{Pin, Rebuilt, Standing, Trust, Written};

/// One plugin module, as the stack sees it.
/// The seam that keeps this crate out of the wasm business: rano's engine is one implementation, a
/// table in a test is another. `Ok(None)` is unchanged, and an `Err` is recorded and asked below.
pub trait Module: Send + Sync {
    /// The export names this module answers, e.g. `pane_dashboard`.
    fn hooks(&self) -> Vec<String>;

    /// Answer one hook with the JSON text of the input.
    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String>;

    /// The fuel the last `call` burned - the gate's cost per hook.
    fn fuel(&self) -> u64;
}

/// How a module is compiled and admitted, so the engine is one dependency and not a design.
/// The engine is handed the manifest's capabilities and nothing else: a plugin's imports can only
/// reach what the file it was reviewed with declared.
pub trait Loader: Send + Sync {
    fn load(&self, wasm: &Path, caps: &[Capability]) -> Result<Box<dyn Module>, String>;

    /// Load a module the host already holds in memory: a built-in, `include_bytes!`d into the
    /// binary. Same engine and capabilities, so it is a layer like any other once loaded; `name` is
    /// the layer's name because memory has no path. The default refuses - most loaders are fakes.
    fn load_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        caps: &[Capability],
    ) -> Result<Box<dyn Module>, String> {
        let _ = (bytes, caps);
        Err(format!(
            "the loader in use cannot load `{name}` from memory — a built-in needs an engine with \
             `load_bytes`, which is the daemon's rano loader"
        ))
    }

    /// A built-in's module, shared rather than owned: `load_bytes` answers a `Box` and so compiles
    /// once per stack, while a `Module` is `Send + Sync` and the stack keeps it behind an `Arc`.
    /// The default is `load_bytes`; only an engine that can keep the compiled module overrides it.
    fn load_builtin(
        &self,
        name: &str,
        bytes: &[u8],
        caps: &[Capability],
    ) -> Result<std::sync::Arc<dyn Module>, String> {
        self.load_bytes(name, bytes, caps).map(Arc::from)
    }
}

/// The contract version this host speaks for every hook kind whose shape has never changed. A
/// layer declaring another is refused by name at load and never answers, because a contract that
/// moved is not a field that can be guessed at.
pub const CONTRACT: u32 = 1;

/// One hook kind whose contract has moved, and the version it moved to.
/// Listed only when the answer changed shape in a way no wire can show - a session row and a group
/// header are both rows - so the manifest declares the version and `Manifest::admit` checks it.
pub struct Moved {
    /// The export name, as `plugin.toml`'s `hooks` spells it.
    pub hook: &'static str,
    /// The version this host speaks for it, in the pane contract's own numbering
    /// ([`letibot_plugin::pane::V`]). It moves only when this kind's shape moves.
    pub version: u32,
    /// What changed, for the refusal a layer reads.
    pub why: &'static str,
}

/// The hook kinds whose contract has moved, in no particular order.
pub const MOVED: &[Moved] = &[Moved {
    hook: "pane_sessions",
    version: 3,
    why: "the session list's answer is a project's tree now, not the five groups",
}];

/// The version this host speaks for one hook kind: its own entry in [`MOVED`], or [`CONTRACT`].
pub fn speaks(hook: &str) -> u32 {
    MOVED
        .iter()
        .find(|m| m.hook == hook)
        .map(|m| m.version)
        .unwrap_or(CONTRACT)
}

/// The export-name prefixes the host recognises as hooks. An export outside this list is refused
/// at the gate, so a typo in a hook name is a refusal the model can read instead of a hook that
/// silently never fires.
pub const HOOK_PREFIXES: &[&str] = &[
    "on_",
    "provider_",
    "pane_",
    "condition_",
    "notice_",
    "digest_",
    "link_",
    "command_",
    "tool_",
    "complete_",
    "adjudicate_",
    "lifecycle_",
    "nag_",
    "steer_",
    "compact_",
    "review_",
    "brief_",
    "push_",
    "card_",
];

/// The hook kinds this host asks: every prefix of [`HOOK_PREFIXES`] something in this tree builds a
/// hook name with. The `plugin-hooks` skill teaches one example of each, and a test fails when a
/// kind is here and the skill never names it.
pub const SERVED_KINDS: &[&str] = &[
    "on_",
    "pane_",
    "condition_",
    "notice_",
    "command_",
    "tool_",
    "adjudicate_",
    "complete_",
    "lifecycle_",
    "digest_",
    "link_",
    "nag_",
    "steer_",
    "compact_",
    "review_",
    "brief_",
    "push_",
    "card_",
];

/// The prefixes the runtime recognises and this host never asks: nothing in the tree builds a hook
/// name with them, so they are rano's own (`provider_open`). Named so the two lists are the whole of
/// [`HOOK_PREFIXES`]: a new prefix forces a decision about the skill.
pub const UNSERVED_KINDS: &[&str] = &["provider_"];

/// The module a layer directory holds when its manifest names none.
pub const WASM: &str = "plugin.wasm";

/// What a layer declares.
pub const MANIFEST: &str = "plugin.toml";

/// Whether this build has an engine for a module file, and the sentence when it has not.
/// A layer whose module is other text is still on the ladder - pinned, hashed, polled - so it is
/// refused here by name rather than pinned on an empty hash and answering nothing.
pub fn engine_for(module: &str) -> Result<(), String> {
    let wasm = Path::new(module)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wasm"));
    if wasm {
        return Ok(());
    }
    Err(format!(
        "`{module}` is not a wasm module, and no engine for it is built into this host. The \
         layer is on the ladder (pinned, hashed and polled like any other), and nothing here can \
         run it"
    ))
}

/// Whether an export name is a hook this host knows. A hook kind carries a name after the
/// prefix (`pane_dashboard`), and `is_hook_kind("pane_")` is false for that reason: an export
/// with no name names no hook.
pub fn is_hook_kind(name: &str) -> bool {
    HOOK_PREFIXES
        .iter()
        .any(|p| name.strip_prefix(p).is_some_and(|rest| !rest.is_empty()))
}

/// The layer's identity: the SHA-256 of its `plugin.wasm`, the same hash the ledger and the spill
/// locator use, so a reader of either recognises it.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for b in Sha256::digest(bytes) {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Hash a file, or say why it could not be read. Every caller wants the reason: a layer whose
/// module cannot be hashed cannot be loaded either, and the reason is what the model is told.
pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(sha256_hex(&bytes)),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}
