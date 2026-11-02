<!-- abstract: WAT cannot run in this tree's tests - `wat` is not in `Cargo.lock` and cannot resolve offline, and rano's wasmtime has the feature off - so a real-engine plugin test loads a committed 867-byte `.wasm`. -->

# A plugin test through rano's engine needs a committed `.wasm`, because WAT is not reachable offline

rano's own fixtures are WAT text through a dev-dependency, and none of that travels: `wat` is not in this tree's `Cargo.lock`, because a git dependency's dev-dependencies are not resolved, and the crates.io index cache has no entry for it, so adding it cannot resolve with `--offline`. And rano pins `wasmtime` with `default-features = false`, where `wat` is a default feature, so even a `.wat` named `plugin.wasm` would be refused by `Module::new`.

So the fixture is a committed module: `crates/harnessd/tests/data/plugin-engine/` holds `plugin.toml`, `guest/lib.rs`, and an 867-byte `plugin.wasm` that `guest/lib.rs` rebuilds with one `rustc --target wasm32-unknown-unknown` line. The test never compiles anything, because a test that shells out to a compiler fails on a box without the target.

Rakes: `git add` gives a compiled `.wasm` mode 100755, so `git update-index --chmod=-x` it before the commit. And a table-driven test and a module answer differently about repeats - `TableLoader`'s last entry repeats, a module's answer is its constant - so a condition test that arms one watch twice re-runs the second answer.
