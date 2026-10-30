# Overriding the session list — an example that calls below

`docs/plugins-plan.md` §7 step 10: the session list is a built-in **layer**, so overriding it is one
`plugin.toml` and one small module in a project, not a fork of the head.

```sh
mkdir -p .letibot/plugins/sessions
cp plugins/examples/sessions-override/plugin.toml \
   plugins/examples/sessions-override/plugin.wasm .letibot/plugins/sessions/
```

Open the session list (`ctrl-s`). The project's `system` branch — the harness's own agents — is
drawn as **`the harness's own (0)`**, says why, and lists nothing. Everything else — the order, the
projects, the rows, the numbers, the keys — is what the built-in answered, because this layer never
re-implements the list: it asks for the layer below (`{"below": <the request>}`) and edits two
lines of the answer.

| file | is |
|---|---|
| `src/lib.rs` | the layer: ask below on the first call, edit the branch on the second |
| `plugin.toml` | what it reads (`feeds = ["sessions"]`) and what its rows may ask the host to do (`actions = ["switch", "fold"]`) |
| `plugin.wasm` | the built module, committed so the example runs without a wasm toolchain |

Rebuild the module after an edit:

```sh
cargo build --offline --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/sessions_override.wasm plugin.wasm
```

## Why this shape and not a bigger one

* **It reads the feed to pass it down, not to rebuild it.** A layer that rebuilt the request would
  be a layer that had to know the request's shape — and would break the day the host added a field.
* **It names no verb it did not declare.** The rows it keeps carry the acts they arrived with
  (`switch` on a session, `fold` on a group), and `plugin.toml` declares both; a view that reached
  a verb the manifest does not name would be refused by the host.
* **It leaves alone what it does not understand.** A row it does not recognise moves through
  unchanged, so this keeps working when the built-in grows a row kind.

The same file is the fixture `crates/tui/src/app/tests/sessions_override.rs` draws through the real
engine — the test copies this directory in as a project's layer and asserts what comes out.
