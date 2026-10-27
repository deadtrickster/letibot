<!-- abstract: MEASURED 2026-10-27 on lab2x1: WAT cannot run in this tree's tests (rano's `wat` dev-dep is absent from Cargo.lock and from the offline index cache, and wasmtime is built `default-features = false` so its `wat` feature is off), so a real-engine plugin test needs a committed `.wasm` — 867 bytes, built by one rustc line from a no_std guest whose hooks answer constants. -->

# A plugin test through rano's engine needs a committed `.wasm`, because WAT is not reachable offline

MEASURED 2026-10-27 on lab2x1 (`wt-pg`, branch `plugin-gaps`), building
`crates/harnessd/tests/plugin_engine.rs` — the first test in this tree that runs `notice`,
`on_tool_call`, `on_tool_result` and `condition` through **rano's engine** rather than a table of
closures.

## Why not WAT, which is how rano tests its own ABI

rano's `src/plugin.rs` fixtures are WAT text through its **dev-dependency** `wat`. None of that
travels to a consumer:

* `wat` is not in this tree's `Cargo.lock` (a git dependency's dev-dependencies are not resolved),
  and the crates.io **index cache** — `$CARGO_HOME/registry/index/*/.cache` — has no `wa/t/wat`
  entry, so adding it as a dev-dependency cannot resolve with `--offline`. The `.crate` files
  (`wat-1.24[6].2`, `1.261.0`, `1.262.0`) are in the cache; the metadata that names them is not.
* rano pins `wasmtime` with `default-features = false, features = ["std", "runtime", "cranelift"]`,
  and `wat` is one of wasmtime's DEFAULT features — `cargo tree --offline -p letibot-harnessd -e
  features -i wasmtime` lists no `wat` anywhere in the graph. So even a `.wat` file named
  `plugin.wasm` would be refused by `Module::new` ("not a wasm module").

## What works: a tiny no_std guest, compiled once, committed

`crates/harnessd/tests/data/plugin-engine/`:

| file | is |
|---|---|
| `plugin.toml` | `name = "engine"`, the four hooks by name, `capabilities = ["read"]`, `contract = 1` |
| `plugin.wasm` | **867 bytes**, the compiled guest — loaded by `RanoLoader` like any layer |
| `guest/lib.rs` | the source, and the one `rustc` line that rebuilds it |

```sh
rustc --edition 2021 --crate-type cdylib --target wasm32-unknown-unknown \
      -C opt-level=s -C panic=abort -C debuginfo=0 -o plugin.wasm lib.rs
```

`wasm32-unknown-unknown` **is** installed on lab2x1 (and on the Mac, per `docs/plugins-plan.md`
§0), but the test never compiles anything: a test that shells out to a compiler fails on a box
without the target, which is the absent-apparatus shape AGENTS.md forbids. The module is committed
for the same reason `plugins/examples/plan-dashboard/plugin.wasm` is.

The guest is `#![no_std]` with a `loop {}` panic handler, `rano_abi() -> 1`,
`rano_alloc(n) -> 4096`, and one function per hook, each answering a CONSTANT: the answer is the
offset of an 8-byte block holding `(out_ptr, out_len)` as little-endian `u32`s, and the bytes are a
`&'static [u8]` that is already in linear memory — so no copy, no allocator, no import.

## Rakes

* **`git add` gave the `.wasm` mode 100755** (rustc's output is executable). Fix with
  `git update-index --chmod=-x <path>` **before** the commit, or the fixture arrives executable.
* **A table-driven test and a module answer differently about repeats**: `TableLoader`'s last
  script entry repeats, but a module's answer is its constant. A condition test that arms the same
  watch twice therefore re-runs the SECOND answer (no `look`), not the first — which is how I found
  that a script of `[look, fired]` makes the second arming ask the gate zero times.
* `PluginHost::lock()` is private; `PluginHost::recorded(hook)` is the public reader of the
  recorded inputs (made public in this package for exactly this test).
