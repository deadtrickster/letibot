# Plugins, slice 1: contracts and work packages

The first vertical slice of `docs/plugins-plan.md`: the layer stack, hot reload and the shadow gate in
`crates/plugins`, pane plugins run by harnessd and drawn by the TUI, and an example plan dashboard as
the first plugin. This file fixes the contracts, so a package can be built against these interfaces
rather than against another package's code.

## C1. The wasm call (rano)

rano's `plugin::Plugin` gained, beside `open`/`save`/`provider_open`:

```rust
impl Plugin {
    /// `prefixes` are the hook prefixes the host recognises beside `on_` and `provider_`; letibot
    /// passes `["pane_", "condition_", "notice_", "digest_", "link_", "command_", "tool_"]`.
    /// An export with one of them must have the text-hook signature (i32, i32) -> i32.
    pub fn from_bytes_hooks(bytes: &[u8], path: PathBuf, caps: Capabilities, prefixes: &[&str])
        -> Result<Plugin, String>;
    pub fn load_hooks(path: &Path, caps: Capabilities, prefixes: &[&str]) -> Result<Plugin, String>;
    /// Run any text hook: `Ok(None)` = the hook is absent or answered unchanged.
    pub fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String>;
    /// Fuel the last `call` burned (for the gate's report).
    pub fn last_fuel(&self) -> u64;
}
```

The guest SDK (`sdks/rust`, crate `rano-plugin`) gained, behind a `json` feature (`serde_json`, and
`serde` with `derive`):

```rust
/// Parse the input as JSON into `I`, run `f`, serialize its answer; `None` = unchanged.
/// Malformed input answers `{"error": "..."}`.
pub fn json_hook<I: DeserializeOwned, O: Serialize>(ptr: u32, len: u32, f: impl FnOnce(I) -> Option<O>) -> u32;
```

## C2. The layer stack (`crates/plugins`, crate `letibot-plugins`)

No wasmtime in this crate's public API: it talks to modules through a trait, so a fake stands in for
the rano engine in tests.

```rust
pub trait Module: Send + Sync {
    fn hooks(&self) -> Vec<String>;                       // export names, e.g. "pane_dashboard"
    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String>;
    fn fuel(&self) -> u64;                                // last call's fuel
}
pub trait Loader: Send + Sync {                           // rano-backed in production
    fn load(&self, wasm: &Path, caps: &[Capability]) -> Result<Box<dyn Module>, String>;
}

pub enum Scope { Session, Project, User, BuiltIn }
pub enum Capability { Read, ReadPath(String), Exec(String), Http(String) }

pub struct Stack { /* layers newest-first within scope, scopes session > project > user > built-in */ }
impl Stack {
    /// Each argument is the root of a scope and may be absent, which is an empty scope.
    pub fn discover(session_dir: Option<&Path>, project: Option<&Path>, user: Option<&Path>,
                    loader: Arc<dyn Loader>) -> Stack;
    pub fn call(&self, hook: &str, input: &serde_json::Value) -> Resolved;
    pub fn poll_changes(&mut self) -> Vec<Candidate>;              // hot reload, by module file
    pub fn apply(&mut self, c: Candidate) -> Result<(), String>;   // only after the gate passed
}
```

A `Manifest` is what `plugin.toml` declares: name, purpose, module (default `plugin.wasm`), hooks,
capabilities, contract and `[contracts]` per hook kind, feeds, actions and `[[tools]]`. A `Layer` is
one scope's directory with its manifest, the sha256 of its module AND of its `plugin.toml`, a disabled
flag, and D17's `trusted`/`pin` (plan §2.5). `Resolved` is an answer, the layer id that gave it, and
the failures met on the way down; `Stack` also answers `layers`, `set_disabled` and `failures`.

`Stack::call` resolves one hook, per layer from the top: `{"pass":true}` or an unchanged answer
falls through, `{"mask":true}` is *no answer here and none below*, `{"below": <input>}` calls the
layer below and then this layer again with the original input plus `below`, and anything else is the
answer. A layer that traps, runs out of fuel or answers malformed JSON is skipped and recorded as a
failure. `below` is at the top level and not `look`: a pane's own document has a top-level `look`
meaning *run this for me*, so `{"look":{"below":…}}` could not tell an answer from a request.

Three choices C2 left open, and what was taken. The same `Candidate` goes to the gate and then to
`apply`, because a rebuilt candidate is a different value and applying one the gate never saw is
applying an unjudged layer. A manifest with no module is a declaration and not a failure: the layer
sits on the stack with an empty `sha256`, answers nothing itself, and the module below answers the
hook, which is how a project installs a built-in's tool.

## C3. The pane contract (hook `pane_<name>`)

The exact document - input, `View`, `Panel`, `Block`, `Line`, `Row`, `Finding`, `Action`, the roles,
the host's keys, the serenedash language it speaks and every refusal - is `docs/plugins-plan.md`
§4.9. What this slice fixed, and what a host must guarantee:

- A plugin describes and the host draws. A plugin never paints a cell and never reads a key; it
  returns a document in a vocabulary the head already has, rendered with the same rano widgets as the
  built-in panes. The view is also data: the same JSON is what an agent reads.
- Forward compatibility is the host's job: unknown fields are ignored and an unknown block kind
  renders as one muted line naming the kind, so a newer layer degrades instead of vanishing.
- The input carries a version (`"v"`, now 3), so a layer can tell which host is asking. The
  manifest's `contract` is a separate number and is still 1.

## C4. The wire

In `crates/sessionlog/src/protocol.rs`, beside the session frames. The view types are serde structs
in `crates/sessionlog/src/pane.rs`, shared by the daemon (which validates a plugin's answer into
them) and the head (which renders them).

```rust
// daemon -> head, a session event
SessionEvent::PluginPane { pane: String /* "plan-dashboard/dashboard" */, view: serde_json::Value /* C3 View */ }
SessionEvent::PluginPaneGone { pane: String }
// head -> daemon, a client frame (the enum is `ClientFrame`)
ClientFrame::PaneAction { pane: String, item: String, action: String }  // action = key's event, or "look"
ClientFrame::PaneOpen { pane: String, w: u32, h: u32 }   // the head opened it: the daemon runs it now
ClientFrame::PaneClose { pane: String }                  // the head closed it: stop refreshing it
```

`PaneOpen`'s `w` and `h` are the pane's drawable rectangle with chrome excluded, and they exist
because only a head knows them: a dashboard laid out for the host's guess wraps its labels for a
terminal nobody is looking at. Both are `#[serde(default)]`, `0` means the head did not say, and a
resize is `PaneOpen` again with the new rectangle. `PaneClose` is the head saying a pane is off its
screen: without it the host keeps a refresh clock per pane a head has ever opened and asks it again
every `every_s` until the session ends. It is idempotent and quiet, and a pane the session never had
is not an error.

## C5. The example: a plan dashboard

The first pane plugin is a generic example in this repository, `plugins/examples/plan-dashboard`,
reading a project's plan status file `<project>/.letibot/status.json`. Its fixture and its `status`
vocabulary are in that directory, and its only capability is `read`.

## Work packages

| id | built | where it is now |
|---|---|---|
| W1 | C1 and the SDK's `json_hook` | rano, branch `plugin-call` |
| W2 | C2, with fake `Module`/`Loader`/`Builder` and no wasmtime dependency | `crates/plugins` |
| W3 | C4's types and the TUI pane | `crates/sessionlog/src/pane.rs`, `crates/tui/src/ui/panes/plugin.rs` |
| W4 | harnessd's half: the `Loader`, the per-session stack, looks through the gate, `PluginPane`, hot reload | `crates/harnessd/src/plugins.rs`, `crates/harnessd/src/plugins/` |
| W5 | C5, with a fixture and native tests of the view | `plugins/examples/plan-dashboard` |
