<!-- abstract: The example plugin's wasm is ~250K because it carries serde_json itself, so a plugin's size is an SDK decision - and a `Status` with `#[serde(default)]` everywhere parsed `{}` into an empty plan. -->

# The example plugin's wasm, and the status file that parsed to nothing

`plugins/examples/plan-dashboard/plugin.wasm` is 255 297 bytes with `opt-level = "s"`, `lto`, `strip`
and `panic = "abort"`, and nearly all of it is serde + serde_json, which the plugin carries itself
because the slice-1 SDK has no `json` hook (`docs/plugins-slice1.md`). The dashboard logic is a few KB of
match arms, so an SDK-level reader is a size decision and not only an ergonomics one.

`Status` (`plugins/examples/plan-dashboard/src/status.rs`) defaulted every field so a sparse file still
draws, and `{}` then parsed into an empty, undated plan which the pane reported as "status file parsed".
`updated` is the one required field now. A whole struct of `#[serde(default)]` turns "malformed" into
"empty"; only the field whose absence is a lie stays required.

The tests are native and call the pure `view`/`view_at`, because `rano-plugin`'s hook writes into the
module's static memory. `view_at(input, now)` exists because the staleness finding compares against a
clock - `view()` over a fixture with a fixed `updated` goes red on its own after a day.
