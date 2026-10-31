<!-- abstract: MEASURED 2026-10-31 building D17 as the operator restated it ("only the builtins run freely; anything I place later needs a pin"): a pin is about the bytes the stack HOLDS, so a rebuild un-pins only when the gate applies it; `DeclaredTool::layer` is a directory name and two scopes can share one, so the tool's trust lookup needs a scope; and putting the decision in `PluginTool::run` un-pinned every existing test's fixture at once. -->

# The ladder is a hash, and a pin is about the bytes the stack HOLDS

**MEASURED 2026-10-31**, branch `agent/only-the-builtins-run-free` (D17 as the operator restated it:
*"Only the builtins run freely; anything I place later needs a pin"*). `crates/plugins/src/trust.rs`
is the book; `Layer::trusted`/`Layer::pin` are the decision; `Stack::set_trust`/`refresh_trust` are
where it is re-taken.

## A rebuild un-pins a layer only when the GATE applies it, and that is right

`Stack::refresh_trust` compares the pin against `layer.sha256` — the module the stack *holds* — not
against `plugin.wasm` on disk. So a model that rebuilds a pinned layer keeps the free arm until
`poll_changes` → the gate → `apply` swaps the new module in. That is coherent (the old module is the
one answering) and it is not obvious: the test that writes new bytes and calls `refresh_trust`
directly gets `[]` and looks like a broken rule. The path is `poll_changes` + `apply` + `refresh_trust`.

**The manifest is the other half**: it is hashed fresh on every refresh, so an *edited manifest*
un-pins a layer at once, without any module having moved — which is why `Layer::manifest_sha256`
exists as its own field.

## `DeclaredTool::layer` is a DIRECTORY NAME, and two scopes can share one

`Stack::declared_tools` fills `DeclaredTool::layer` with the layer's directory name, not its id
(`p1`) and not its path. `Stack::layer_named` therefore needed a fourth spelling (file name), and
`PluginHost::layer_in(name, scope)` a scope-aware one: a session fix and the project layer it was
promoted from can both be called `one`, and *is this layer pinned* would otherwise have two answers.

## Putting the decision in `PluginTool::run` un-pins every existing test at once

`run(&self, args)` was the door the harnessd plugin tests used, because "a test that had to build an
`InvokeCtx` would be a test of the runtime". The moment an unpinned layer's call had to be
adjudicated, every fixture in `crates/harnessd/src/plugins.rs` became *a layer nobody pinned*, so
`tool_for` now pins its layer and `tool_for_with(…, pinned: false)` is the ladder's own fixture.

The production path is `invoke(&self, ctx: &mut InvokeCtx, args)`, and the decision had to be
reachable from a test without an `InvokeCtx` — so `crate::plugins::tool::Decides` is a one-method
trait implemented for `InvokeCtx` (forwarding to `InvokeCtx::admit_unasked`) and by a stub in a test.

## `Trust::in_memory()` has no file, so `path()` is `None`

A test that wants to prove the record survives the process must `Trust::open(<tempdir>/plugin-trust.json)`,
not `in_memory()`; the latter is the right book for *nothing is pinned* and for a box with no config
dir.

## Two rakes under the fixtures

* **`room::link::tests::an_event_about_a_session_rings_its_bell_and_a_reader_is_not_rung` fails
  under a full parallel `--lib` run and passes alone** (measured: 1 of 881, then 4/4 alone). Not
  this branch's — the sentence it reads is another session's wake text, so it is a race in the room's
  wake test, not in the ladder.
* A `plugin.wasm` a test rewrites must be a **different length** to be seen on a filesystem with
  coarse mtimes (`poll_changes` compares mtime and len first) — `b"a module"` → `b"a module, rebuilt"`.
