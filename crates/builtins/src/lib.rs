//! The plugin layers that ship inside the binary.
//!
//! A built-in is a layer like any other: a `plugin.toml` and a `plugin.wasm`, discovered in a
//! scope, overridable by name from a project or a session. What changes is where the bytes come
//! from. A shipped layer sitting as a directory beside the binary would be a second thing to
//! install, a second thing to have the wrong version of, and a file the operator's package
//! manager owns, so the modules are compiled by [`build`](build.rs) at build time and
//! `include_bytes!`d into the executable. The stack reads the `builtin` scope from [`table`] and
//! never from disk.
//!
//! The build is a build script rather than an `xtask` because the price of the wasm target is only
//! worth paying if an ordinary `cargo build` pays it: a step somebody must remember is a step that
//! will be skipped on the machine that ships.

use letibot_plugins::Stack;

/// One layer the binary carries: its name (the directory), its manifest's text, and its module.
///
/// The manifest is text rather than a parsed [`Manifest`](letibot_plugins::Manifest) because the
/// stack parses it through the same door a layer on disk goes through, so a malformed built-in
/// fails discovery with a failure naming it instead of failing at compile time.
pub struct Builtin {
    /// The directory's name, which is the layer's name: `sessions`.
    pub name: &'static str,
    /// `plugin.toml`, verbatim.
    pub manifest: &'static str,
    /// The module, compiled for `wasm32-unknown-unknown`.
    pub wasm: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/table.rs"));

/// Put every built-in at the bottom of `stack`.
///
/// Failures come back as sentences rather than being swallowed: a built-in that will not load is a
/// defect in this repository, and the caller that puts it in front of a person is the one that can
/// say so.
pub fn install(stack: &mut Stack) -> Vec<String> {
    let mut failed = Vec::new();
    for b in table() {
        if let Err(why) = stack.register_builtin_bytes(b.name, b.manifest, b.wasm) {
            failed.push(format!("{}: {why}", b.name));
        }
    }
    failed
}
