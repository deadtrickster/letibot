//! **Plugins: the layer stack, hot reload and the shadow gate** — C2 of `docs/plugins-slice1.md`,
//! and §2 of `docs/plugins-plan.md` behind it.
//!
//! A plugin is a wasm module that answers text hooks. Policy that changes per project and per week
//! is a layer, and layers stack: the topmost layer that answers a hook wins, everything it does
//! not mention falls through to the layer below, and the bottom of the stack is the built-in
//! defaults compiled into the daemon. The point of the shape is the *fix in the middle of a
//! session*: a session's model writes a small layer, it is built and loaded hot, and the layer
//! pile is consolidated later — squashed, promoted to the project by a commit, or turned into a
//! core change.
//!
//! # What is here, in the order a hook goes through it
//!
//! | piece | is |
//! |---|---|
//! | [`Manifest`] | `plugin.toml`: the name, the one-line purpose, the hooks it exports, the capabilities it asks for, the tools it offers the model, the contract it speaks |
//! | [`Stack`] | the four scopes resolved into one order — session over project over user over built-in — and [`Stack::call`], the answer protocol |
//! | [`Recorder`] | the last 32 inputs per hook, which is what a replay can be run over |
//! | [`Trust`] | D17's ladder on disk: a built-in runs freely, a placed layer needs a pin, and a write of a layer's bytes is recorded where it happened |
//! | [`gate`] | build, load, shadow, replay, report: the dry run every change takes before it answers |
//!
//! # This crate has no wasm engine in it, on purpose
//!
//! It talks to a module through [`Module`] and to the compiler through [`Loader`], so the whole
//! of §2.1–§2.4 is testable with fakes: precedence, pass, mask, the ask-below, a trap falling to
//! the next layer, and a gate that refuses a build failure, a load failure or a trap in replay
//! are all in-crate tests that compile nothing. The rano engine is one `Loader`; a mock is
//! another. That also keeps this crate's dependency set at three crates, none of which links C++,
//! so harnessd may depend on it without pulling a JIT into the head.
//!
//! # The answer protocol, once, because it is the whole of `call`
//!
//! Each layer, from the top, is asked the hook with the same input. Its answer decides:
//!
//! | answer | what happens |
//! |---|---|
//! | `Ok(None)` (unchanged) or `{"pass":true}` | the next layer down is asked |
//! | `{"mask":true}` | nothing below is asked and there is no answer: the hook is switched off from here down |
//! | `{"below":<input>}` | the layer below is resolved with `<input>`, and this layer is asked **again** with the original input plus `"below": <that answer>` — a fix that edits one field of a lower answer instead of re-implementing the hook (§2.1) |
//! | anything else | that is the answer, and this layer is `by` |
//!
//! A layer that traps, runs out of fuel, or answers something that is not JSON is **skipped and
//! recorded**, and the next one down answers: fail down, loudly (§1, principle 4). One broken
//! layer never stops a resolution, and it never stops the layer below it either.
//!
//! The order of the three markers is fixed — `mask`, then `below`, then `pass` — because an answer
//! carrying two of them has to mean one thing, and the strongest statement is the one that gets
//! to speak.
//!
//! # Two spellings this crate added after the pane, and what each is for
//!
//! * **`tool_<name>`** — a layer that declares `[[tools]]` offers the model a tool, and the export
//!   `tool_<name>` answers it. The declaration is the prompt bytes (name, one line, JSON Schema)
//!   and [`Manifest::admit`] refuses a layer whose manifest names a tool its exports do not
//!   answer. Where the host seats it and what the hook's input and answer are is
//!   `crates/harnessd/src/plugins/tool.rs`; this crate owns only the declaration and the check.
//! * **`read:<path>`** — [`Capability::ReadPath`], one tree outside the workspace, named as a
//!   person wrote it down. `read` is unchanged and still means the workspace. The manifest line is
//!   the approval, and the host's look runner is what makes a module unable to reach past it.

use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};

mod gate;
mod manifest;
mod recorder;
mod stack;
mod trust;

#[cfg(test)]
mod testing;

pub use gate::{BuildError, Builder, CargoBuilder, GateReport, ReplayDiff, gate};
pub use manifest::{Capability, Manifest, Scope, ToolDecl};
pub use recorder::Recorder;
pub use stack::{BrokenPin, Candidate, DeclaredTool, Failure, Layer, Resolved, Stack};
pub use trust::{Pin, Standing, Trust, Written};

/// One plugin module, as the stack sees it.
///
/// The trait is the seam that keeps this crate out of the wasm business: rano's engine is one
/// implementation, a table in a test is another, and nothing above it knows the difference. A
/// module is asked for its exports once (`hooks`) and then answered through `call`, whose input
/// and output are the JSON text of the hook's contract.
///
/// `Ok(None)` means *unchanged*: the hook falls through to the layer below. An `Err` is a trap, an
/// exhausted fuel budget, or anything else that means the module could not answer — the stack
/// records it against the layer and asks the next one down (§1, principle 4).
pub trait Module: Send + Sync {
    /// The export names this module answers, e.g. `pane_dashboard`.
    fn hooks(&self) -> Vec<String>;

    /// Answer one hook with the JSON text of the input.
    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String>;

    /// The fuel the last `call` burned — the gate's cost per hook.
    fn fuel(&self) -> u64;
}

/// How a module is compiled and admitted, so the engine is one dependency and not a design.
///
/// The engine is handed the manifest's capabilities and nothing else, which is where §3's sandbox
/// starts: a plugin's imports can only reach what the file it was reviewed with declared.
pub trait Loader: Send + Sync {
    fn load(&self, wasm: &Path, caps: &[Capability]) -> Result<Box<dyn Module>, String>;

    /// **Load a module the host already holds in memory** — a built-in, `include_bytes!`d into the
    /// binary (`docs/plugins-plan.md` §7 step 10, `crates/builtins`).
    ///
    /// The same file the same engine compiles, and the same capabilities, so a built-in is a layer
    /// like any other once it is loaded; only the journey is shorter. `name` is the layer's name,
    /// for the sentence a refusal carries — a module from memory has no path to point at.
    ///
    /// The default is a refusal rather than an error type of its own, because most loaders are
    /// test fakes with no memory to read from, and a fake that filled this in would be a fake
    /// pretending to be a wasm engine.
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

    /// **A built-in's module, shared rather than owned** — the door a layer the binary carries
    /// comes through when more than one session will seat it.
    ///
    /// `load_bytes` answers a `Box`, which is one owner and so one compile per stack: MEASURED
    /// 2026-10-28 in a debug host, compiling the daemon's built-in tool layer costs **1.97 s**, and
    /// every session open paid it again. A `Module` is `Send + Sync` and answers `&self`, and the
    /// stack already keeps it behind an `Arc` (`Wired::module`) — so the same bytes may answer for
    /// several sessions, and an engine that can say so should.
    ///
    /// The default is `load_bytes` and an `Arc::from`, which is correct and pays the compile every
    /// time: only the engine that has a place to keep the compiled module overrides it. Nothing
    /// downstream can tell the two apart, which is the point — `fuel()` is the engine's own last
    /// call either way, so a shared module's price is *per module*, not per stack (ask it right
    /// after the call that interests you, as `Stack::fuel` says).
    fn load_builtin(
        &self,
        name: &str,
        bytes: &[u8],
        caps: &[Capability],
    ) -> Result<std::sync::Arc<dyn Module>, String> {
        self.load_bytes(name, bytes, caps).map(Arc::from)
    }
}

/// The contract version this host speaks. A layer declaring another is refused by name at the
/// gate (step 2) and never answers, because a contract that moved is not a field that can be
/// guessed at — `docs/plugins-plan.md` §1, principle 6.
pub const CONTRACT: u32 = 1;

/// The export-name prefixes the host recognises as hooks: rano's own two, and the kinds the
/// daemon passes as extra prefixes (`docs/plugins-slice1.md`, C1). An export outside this list is
/// refused at the gate, so a typo in a hook name is a refusal the model can read instead of a
/// hook that silently never fires.
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
];

/// The built module a layer directory holds.
pub const WASM: &str = "plugin.wasm";

/// What a layer declares.
pub const MANIFEST: &str = "plugin.toml";

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
