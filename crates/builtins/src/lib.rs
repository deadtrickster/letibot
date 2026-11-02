//! The plugin layers that ship inside the binary.
//!
//! A built-in is a layer like any other, overridable by name from a project or a
//! session; what changes is where the bytes come from. A directory beside the
//! binary would be a second thing to install and a second thing to have the wrong
//! version of, so [`build`](build.rs) compiles the modules and `include_bytes!`
//! puts them in the executable. It is a build script rather than a step somebody
//! has to remember: a step that can be skipped is skipped on the machine that ships.

use letibot_plugins::Stack;

/// One layer the binary carries: its name (the directory), its manifest's text and
/// its module. The manifest is text so it is parsed by the same `Stack` door a layer
/// on disk goes through, not at compile time where only this crate would see it.
pub struct Builtin {
    /// The directory's name, which is the layer's name: `sessions`.
    pub name: &'static str,
    /// `plugin.toml`, verbatim.
    pub manifest: &'static str,
    /// The module, compiled for `wasm32-unknown-unknown`.
    pub wasm: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/table.rs"));

/// Put every built-in at the bottom of `stack`, the `builtin` scope. Failures come
/// back as sentences rather than being swallowed: a built-in that will not load is
/// a defect in this repository, and only the caller can say so.
pub fn install(stack: &mut Stack) -> Vec<String> {
    let mut failed = Vec::new();
    for b in table() {
        if let Err(why) = stack.register_builtin_bytes(b.name, b.manifest, b.wasm) {
            failed.push(format!("{}: {why}", b.name));
        }
    }
    failed
}
