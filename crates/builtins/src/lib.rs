//! The plugin layers that ship inside the binary: a layer like any other, overridable by name.
//! [`build`](build.rs) compiles the modules and `include_bytes!` embeds them, so there is no second
//! thing to install and no step to skip.

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

// The table this build script wrote, kept under THIS tree's own directory in `OUT_DIR`: two
// worktrees share one `OUT_DIR`, and the `table.rs` one of them leaves behind names a tree that may
// since have been deleted.
include!(concat!(
    env!("OUT_DIR"),
    "/trees/",
    env!("LETIBOT_BUILTINS_TREE"),
    "/table.rs"
));

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
