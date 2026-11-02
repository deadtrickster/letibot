<!-- abstract: The pin is about the bytes the stack HOLDS, so a rebuild un-pins only when the gate applies it; `DeclaredTool::layer` is a directory name two scopes can share; and a test must not write the operator's trust book. -->

# The ladder is a hash, and a pin is about the bytes the stack HOLDS

`crates/plugins/src/trust.rs` is the book and `Layer::trusted`/`Layer::pin` the decision; the design is
`docs/plugins-plan.md` §2.5.

`Stack::refresh_trust` compares the pin against `layer.sha256` - the module the stack HOLDS - not
against `plugin.wasm` on disk, so a rebuilt layer keeps the free arm until `poll_changes` → the gate →
`apply` swaps the new module in, and a test that writes bytes and calls `refresh_trust` directly gets
`[]`. The manifest is hashed fresh on every refresh, so an edited `plugin.toml` un-pins at once.

`Stack::declared_tools` fills `DeclaredTool::layer` with the layer's DIRECTORY NAME, so a session layer
and the project layer it was promoted from can both be called `one`, and a trust lookup takes a scope -
`PluginHost::layer_in(name, scope)` (`crates/harnessd/src/plugins.rs:230`).

Under the fixtures: `Trust::in_memory()` has no file, so a test that proves the record survives the
process must use `Trust::open(<tempdir>/plugin-trust.json)`; and a test binary must never write the
operator's book - `LETIBOT_PLUGIN_TRUST` names the file (empty means no book), which
`crates/harnessd/tests/plugins_tool.rs` sets once per process.
