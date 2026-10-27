//! **The plugin layers that ship inside the binary** — `docs/plugins-plan.md` §7 step 10.
//!
//! # One binary, never a binary and a directory of modules
//!
//! A built-in is a layer like any other: a `plugin.toml` and a `plugin.wasm`, discovered in a
//! scope, overridable by name from a project or a session (`§2.1`). What changes is where the
//! bytes come from. A shipped layer cannot be a directory next to the binary — that is a second
//! thing to install, a second thing to have the wrong version of, and a file the operator's
//! package manager owns — so the modules are compiled by [`build`](build.rs) at build time and
//! `include_bytes!`d into the executable. The stack then reads the `builtin` scope from
//! [`table`], and never from disk.
//!
//! # Why the build is a build script and not a `xtask`
//!
//! The plan's constraint is *the price is the wasm target on every machine that builds letibot*
//! — that price is only worth paying if the build is the ordinary `cargo build`. A step somebody
//! has to remember is a step that will be skipped on the machine that ships.
//!
//! # What the script has to get right, and does
//!
//! * **Its own target directory.** A guest is a different crate for a different target; sharing
//!   the host's target directory makes it contend for that directory's lock with every build the
//!   session runs, and putting its artifact where the *host's* `CARGO_TARGET_DIR` points is the
//!   P2 trap — a build that succeeded and a module that is not where the caller looked.
//! * **`CARGO_TARGET_DIR`, `RUSTFLAGS` and `CARGO_ENCODED_RUSTFLAGS` removed** from the child's
//!   environment for the same reason: they are the host's, and a guest compiled with the host's
//!   flags is a guest nobody tested.
//! * **`--offline`**, always: this tree builds on machines with no network and a warm cache.
//! * **One sentence when the target is absent.** A box without `wasm32-unknown-unknown` fails
//!   here, at the top, with the command that fixes it — not with a wall of linker errors from a
//!   nested cargo.
//!
//! # Adding a built-in
//!
//! A directory under `plugins/builtin/` with a `Cargo.toml` and a `plugin.toml` is one. Nothing
//! else changes: the table is generated from the directory, so a new layer appears in [`table`]
//! the moment it exists.

use letibot_plugins::Stack;

/// One layer the binary carries: its name (the directory), its manifest's text, and its module.
///
/// The manifest is TEXT rather than a parsed [`Manifest`](letibot_plugins::Manifest) on purpose:
/// it is parsed by the same [`Stack`](letibot_plugins::Stack) door a layer on disk goes through,
/// so a built-in whose `plugin.toml` is malformed fails discovery exactly as a project layer does
/// — with a failure naming it, in the stack view — rather than at compile time in a way only this
/// crate's author would ever see.
pub struct Builtin {
    /// The directory's name, which is the layer's name: `sessions`.
    pub name: &'static str,
    /// `plugin.toml`, verbatim.
    pub manifest: &'static str,
    /// The module, compiled for `wasm32-unknown-unknown`.
    pub wasm: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/table.rs"));

/// **Put every built-in at the bottom of `stack`** — the `builtin` scope of §2.1.
///
/// The failures come back as sentences rather than being swallowed: a built-in that will not load
/// is a defect in this repository, and the caller that puts it in front of a person (the daemon's
/// log, a head's pane) is the one that can say so. An empty `stack` with a failure beside it is a
/// stack whose next layer down is nothing, which is exactly what the head's plain list is for.
pub fn install(stack: &mut Stack) -> Vec<String> {
    let mut failed = Vec::new();
    for b in table() {
        if let Err(why) = stack.register_builtin_bytes(b.name, b.manifest, b.wasm) {
            failed.push(format!("{}: {why}", b.name));
        }
    }
    failed
}
