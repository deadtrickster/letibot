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

## UPDATE 2026-10-31 (the queue refused `1d53ebd1`, and it was right)

The first book merged "mine wins per entry" with a per-process `revoked` list that was never
serialised. **A merge is only half the fix** — two processes can each read the file, each merge,
and the second's `rename` discards the first's statement, so a revocation survives on luck with a
narrow window. And a revocation carried as an ABSENCE cannot be told from *never mentioned*, so a
peer's stale copy puts the pin back — with a model's ordinary `write` into the layer directory as
the trigger. What it is now:

* **A revocation is a row with a time on it** (`Book::revoked`), and every merge is decided by
  `at_ms` (`Book::merge`), so a stale copy can neither undo a revocation nor undo a re-pin. A pin
  superseded by a newer revocation is dropped from the file entirely, so the book never reads as
  the opposite of what it means.
* **`save` is a read-modify-write under an exclusive lock on `<book>.lock`** — `std::fs::File::lock`
  (stable since 1.89; this box's rustc has it), a SIDECAR because the book itself is written by
  `rename` and a lock on the old inode says nothing about the new one.
* The timestamps come from `Book::next_ms` — strictly greater than every statement in the book — so
  two acts in one millisecond are still ordered and a backwards clock cannot invert them.

**The seam that proves it has to be thread-scoped.** `#[cfg(test)] test_seam` pauses `save` between
its read and its rename so a test can land a second writer in the window. A process-wide static
fired inside *another* parallel test's save and took it down (`a_stale_book_cannot_put_back_the_pin_
that_a_byte_change_replaced` failed with the first test's message). It now stores the installing
thread's id and only that thread can trip it.

**Two more things the same review found, both real:** `Stack::set_trust` discarded the
`Vec<BrokenPin>` it returns — and `refresh_trust` marks a pin as said as it answers it, so a pin
that broke while no daemon ran was never published (now queued by `PluginHost::with_trust` and said
on the first tick; the head puts it in `problem`); and an `exec` look was judged by the topmost
layer that DECLARES the hook rather than the layer whose ANSWER asked for it, so an unpinned layer
below a pinned one ran unadjudicated (`StackOver` now keeps `Resolved::by` per hook, and the
capability check reads it too).

**And a test binary must not write the operator's book**: `LETIBOT_PLUGIN_TRUST` names the file
(empty = no book), which `crates/harnessd/tests/plugins_tool.rs` sets once per process to a scratch
path, because `plugin scaffold` records provenance through the real desk.
