<!-- abstract: MEASURED: plan-dashboard's plugin.wasm is 256,008 bytes (252K) with opt-level="s", lto, strip, panic="abort" — it is serde_json, not the plugin; and a C5 status file with every field #[serde(default)] parses `{}` into a dateless plan, so `updated` is required. -->

# The example plugin's wasm, and the status file that parsed to nothing

MEASURED 2026-10-25, `plugins/examples/plan-dashboard` (branch `plugins-dash`, fixed in `cec4839`).

## 256,008 bytes is serde_json's weight, not the dashboard's

`cargo build --offline --release --target wasm32-unknown-unknown` with `opt-level = "s"`,
`lto = true`, `strip = true`, `panic = "abort"` gives **plan_dashboard.wasm = 256,008 bytes**
(252K). Slice 1's SDK has no `json` feature (`docs/plugins-slice1.md` C1's `json_hook` belongs to
W1), so the plugin carries serde + serde_json itself; that is nearly all of the file — the
dashboard logic is a few KB of match arms. A project copying layers into `.letibot/plugins/` pays
this per plugin, so a future SDK-level `json_hook` (or a hand-rolled reader) is a size decision,
not only an ergonomics one.

## A status file with defaults everywhere parses to a plan with no date

`Status` in `src/status.rs` defaults every field so a sparse file still draws — and the consequence
was that `{}` parsed *successfully* into an empty, undated plan, which the pane then reported as
the `passed` finding "status file parsed". A missing field had become an empty dashboard that
looked like a finished one. `updated` is now the one required field (serde: "missing field
`updated` at line 1 column 2"), which is what makes the staleness comparison the pane is built on
possible at all. The lesson generalises: `#[serde(default)]` on a whole struct turns "malformed"
into "empty", and only the field whose absence is a lie needs to stay required.

## Native tests, because no wasm host exists in this tree yet

`rano-plugin`'s `hook` writes into the module's 18 MiB static memory, so a native call to the
exported `pane_dashboard` would be a wild write; the tests call the pure `view`/`view_at` instead.
`view_at(input, now)` exists only because the staleness finding compares against a clock: a test
that called `view()` over a fixture with a fixed `updated` would go red on its own after 24 hours.
