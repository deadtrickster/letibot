# Plugins, slice 1: contracts and work packages

The first vertical slice of `docs/plugins-plan.md`: the layer stack, hot reload and the shadow gate
in a new crate, pane plugins run by harnessd and drawn by the TUI, and an example plan dashboard
as the first plugin. This file fixes the **contracts** so the packages below can be built
in parallel, each against these interfaces and not against each other's code. A package that needs
a contract changed says so in its report; it does not change it on its own.

## C1. The wasm call (rano)

rano's `plugin::Plugin` gains, beside `open`/`save`/`provider_open`:

```rust
impl Plugin {
    /// Load with extra hook prefixes the host recognises, beside `on_` and `provider_`
    /// (letibot passes `["pane_", "condition_", "notice_", "digest_", "link_", "command_", "tool_"]`).
    /// An export with one of these prefixes must have the text-hook signature (i32, i32) -> i32.
    pub fn from_bytes_hooks(bytes: &[u8], path: PathBuf, caps: Capabilities, prefixes: &[&str])
        -> Result<Plugin, String>;
    pub fn load_hooks(path: &Path, caps: Capabilities, prefixes: &[&str]) -> Result<Plugin, String>;
    /// Run any text hook: `Ok(None)` = the hook is absent or answered unchanged.
    pub fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String>;
    /// Fuel the last `call` burned (for the gate's report).
    pub fn last_fuel(&self) -> u64;
}
```

The guest SDK (`sdks/rust`, crate `rano-plugin`) gains, behind a `json` feature (dependency
`serde_json`, `serde` with `derive`):

```rust
/// Parse the input as JSON into `I`, run `f`, serialize its answer; `None` = unchanged.
/// Malformed input answers `{"error": "..."}`.
pub fn json_hook<I: DeserializeOwned, O: Serialize>(ptr: u32, len: u32, f: impl FnOnce(I) -> Option<O>) -> u32;
```

## C2. The layer stack (`crates/plugins`, crate `letibot-plugins`)

No wasmtime in this crate's public API: it talks to modules through a trait, so it is testable
with fakes and the rano engine is one implementation.

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
pub enum Capability { Exec(String), Read, Http(String) }

/// plugin.toml
/// name = "plan-dashboard"
/// purpose = "a project plan's tiers, stages, agents and hosts at a glance"
/// hooks = ["pane_dashboard"]
/// capabilities = ["read"]
/// contract = 1
pub struct Manifest { pub name: String, pub purpose: String, pub hooks: Vec<String>,
                      pub capabilities: Vec<Capability>, pub contract: u32 }

pub struct Layer { pub id: String /* "s3", "p1", "u1", "b-github-pr" */, pub scope: Scope,
                   pub dir: PathBuf, pub manifest: Manifest, pub sha256: String, pub disabled: bool }

pub struct Stack { /* layers newest-first within scope, scopes session > project > user > built-in */ }
impl Stack {
    /// Discover layers: session dir, `<project>/.letibot/plugins/*/`, `~/.config/letibot/plugins/*/`,
    /// and built-ins registered by the host. Each dir holds plugin.toml, src/, plugin.wasm.
    pub fn discover(session_dir: Option<&Path>, project: Option<&Path>, user: Option<&Path>,
                    loader: Arc<dyn Loader>) -> Stack;
    pub fn layers(&self) -> &[Layer];
    /// Resolve one hook through the stack. Answer protocol, per layer from the top:
    ///   {"pass":true} or unchanged -> next layer down
    ///   {"mask":true}              -> no answer at all
    ///   {"look":{"below": <input>}} -> call the layer below with <input>, then call this layer
    ///                                   again with the original input plus "below": <its answer>
    ///   anything else              -> the answer
    /// A layer that errors (trap, fuel, malformed JSON) is skipped and recorded as a failure.
    pub fn call(&self, hook: &str, input: &serde_json::Value) -> Resolved;
    pub fn set_disabled(&mut self, id: &str, disabled: bool) -> Result<(), String>;
    /// Hot reload: re-stat each layer's plugin.wasm; changed ones become Candidates.
    pub fn poll_changes(&mut self) -> Vec<Candidate>;
    pub fn apply(&mut self, c: Candidate) -> Result<(), String>;   // only after the gate passed
    pub fn failures(&self) -> &[Failure];
}
pub struct Resolved { pub answer: Option<serde_json::Value>, pub by: Option<String> /* layer id */,
                      pub failures: Vec<Failure> }

/// Recorded inputs: the last N (default 32) inputs per hook, for replay.
pub struct Recorder { /* ring per hook */ }

/// The shadow gate (plan §2.4), steps in order, first failure stops:
/// 1 build: `cargo build --offline --release --target wasm32-unknown-unknown` in the layer dir,
///   copy target/wasm32-unknown-unknown/release/<crate>.wasm to plugin.wasm (via a Builder trait
///   so tests do not run cargo);
/// 2 load: Loader::load, every export a known hook kind, contract == 1, manifest hooks == exports;
/// 3 shadow: a Stack clone with the candidate applied (no effects: the gate never runs looks);
/// 4 replay: every recorded input of every hook the candidate exports, live vs shadow;
/// 5 report.
pub fn gate(stack: &Stack, rec: &Recorder, c: &Candidate, b: &dyn Builder) -> GateReport;
pub struct GateReport { pub passed: bool, pub build_log: String, pub load: Result<(), String>,
                        pub fuel: Vec<(String, u64)>, pub diffs: Vec<ReplayDiff>, pub failure: Option<String> }
```

## C3. The pane contract (hook `pane_<name>`, JSON, `"v": 1`)

The dashboard language is serenedash's (`github.com/deadtrickster/serenedash`), which the operator
pointed at: **an overview of panels, every panel a summary with a full view behind its own key**;
every row through **one grid** (label, value, a bar or sparkline, a tail) so the numbers line up
down the frame; **every bar says its denominator**; **findings** collect everything that tripped in
one list, and Enter reads one whole — what was measured, the numbers behind it, the fix, and how to
check it yourself; a **legend** documents every label, and the same text answers on hover; a panel
that cannot get its data **says why and how to fix it** instead of drawing off an empty result; Esc
leaves what you navigated into.

Input:

```jsonc
{"v":1,
 "state": null | {…},                                  // what this pane returned last time
 "look":  null | {"exit":0,"stdout":"…","stderr":"…"}, // the result of the look it asked for
 "event": null | {"item":"<row id>","action":"<event>"},
 "size":  {"w":120,"h":40}}
```

Output (`View`):

```jsonc
{"title":"…",
 "badge": null | {"text":"…","role":"<role>"},        // shown in the status row
 "panels":[ <Panel>… ],                                 // the overview, in order
 "findings":[ <Finding>… ],                             // the `f` view; counted in a summary line
 "legend": {"<label>":"<what it means>", …},            // the `l` view and the hover text
 "look": null | {"exec":["prog","arg",…]} | {"read":"path"},
 "every_s": 0 | <seconds, ≥ 5>,
 "state": null | {…}}
```

`Panel`:

```jsonc
{"id":"tiers", "key":"t", "title":"tiers",
 "denominator":"rows in the tier",                      // what every bar in it is a share of
 "summary":[ <Block>… ],                                // drawn on the overview (a few lines)
 "view":[ <Block>… ],                                   // drawn when its key is pressed; absent = summary
 "unavailable": null | {"why":"cannot connect to lab2x1","fix":"ssh-add ~/.ssh/headless_server"}}
```

`Block` is one of:

```jsonc
{"lines":[ <Line>… ]}                                   // the grid
{"table":{"columns":["…"],"rows":[<Row>…]}}
{"tree":{"rows":[<Row with "children":[<Row>…]>…]}}
{"markdown":"…"}                                        // itemized: list items and headings are stops
{"kv":{"rows":[["label","value"],…]}}
```

`Line` — the one grid, laid out by the host (label column, right-aligned value column, glyph
column, tail; a label too long is truncated with `…`, never clipped):

```jsonc
{"label":"tier 1", "value":"14/22",
 "bar": null | {"frac":0.64},                           // a share of the panel's denominator
 "spark": null | [0.1, 0.4, …],                         // history, each a share of the same denominator
 "tail":"3 in progress", "role":"<role>",
 "id": null | "tier1", "detail": null | "…markdown…", "actions":[<Action>…]}
```

`Row` (tables, trees):

```jsonc
{"id":"tier1/jstests", "cells":["Mongo jstests","not started"], "role":"<role>",
 "detail":"…markdown…", "actions":[<Action>…], "children":[…]}
```

`Finding`:

```jsonc
{"id":"stale-status", "what":"status file is 3 days old", "panel":"tiers",
 "severity":"tripped" | "missing" | "passed",
 "detail":"…prose…", "evidence":{"updated":"2026-10-22T…","age_h":74},
 "fix":"…what to do…", "verify":"…a command or query to check it yourself…",
 "actions":[<Action>…]}
```

`Action`: `{"key":"s","label":"start","say":"…"}` (a line to the session's model, as an operator
line), `{"key":"r","label":"refresh","look":true}` (re-run the look now), or
`{"key":"x","label":"expand","event":"expand"}` (call the pane again with `{"item","action"}`).

`role` is one of `muted`, `working`, `attention`, `success`, `failure`, `plain`.

**Keys are the host's** (templated). On the overview: each panel's `key` toggles its view, `f`
findings, `l` legend; ↑/↓/PgUp/PgDn and the wheel move between stops (lines and rows with an `id`,
markdown items, findings); a click on a panel opens its view. **Enter** opens the selected stop's
`detail` (a finding: its detail, evidence as label/value rows, fix and verify). **Esc** leaves what
you navigated into — detail, then view — and closes the pane at the overview. Action keys are single
lowercase letters shown in the hint bar for the selected stop; the host refuses a view where an
action key collides with `q`, `j`, `k`, `f`, `l`, a panel key, or another action of the same stop.
A summary line under the title counts findings by severity, as serenedash's does.

**The view is also data.** The same JSON is what an agent reads (a later `pane read` tool), the way
serenedash's `--once --format json` is what its MCP server returns.

## C4. The wire (protocol 49)

In `crates/sessionlog/src/protocol.rs`, bump `PROTOCOL_VERSION` to 49 and add:

```rust
// daemon -> head, a session event
SessionEvent::PluginPane { pane: String /* "plan-dashboard/dashboard" */, view: serde_json::Value /* C3 View */ }
SessionEvent::PluginPaneGone { pane: String }
// head -> daemon, a client frame (the enum is `ClientFrame`)
ClientFrame::PaneAction { pane: String, item: String, action: String }  // action = key's event, or "look"
ClientFrame::PaneOpen { pane: String }   // the head opened it: the daemon runs it now
```

The view types (`View`, `Block`, `Row`, `Action`, `Role`) are serde structs in
`crates/sessionlog/src/pane.rs`, shared by the daemon (validates a plugin's answer into them) and
the head (renders them). Unknown fields are ignored; unknown block kinds render as one muted line
naming the kind.

## C5. The example: a plan dashboard

The first pane plugin is a generic example in this repository, `plugins/examples/plan-dashboard`,
reading a project's plan status file `<project>/.letibot/status.json`:

```jsonc
{"updated":"2026-10-25T21:55:00Z",
 "release":{"name":"v0.2.0","goal":"every row of tiers 0, 1 and 2 done"},
 "tiers":[{"id":"tier0","title":"Tier 0","rows":[
   {"id":"tier0/a","area":"…","item":"…","status":"done","note":"","branch":"","agent":""}]}],
 "stages":[{"id":"smoke","title":"smoke checkpoint","status":"waiting","note":"…"}],
 "agents":[{"id":"…","kind":"claude|leticode","task":"…","branch":"…","status":"running"}],
 "hosts":[{"id":"lab2x1","state":"busy","jobs":3,"note":""}],
 "yours":[{"id":"tag","what":"tag and publish"}]}
```

`status` ∈ `done`, `in progress`, `not started`, `waiting`, `blocked`, `yours`. The plugin's only
capability is `read`. A project with a richer source of truth (live hosts, a scheduler) writes its
own plugin; this one shows the language.

## Work packages

Each runs as its own leticode instance in its own worktree, commits on its own branch, never
pushes, and ends with a report: what it built, what it tested, what it could not do, and any
contract it needed changed.

| id | where | builds | depends on |
|---|---|---|---|
| W1 | rano worktree, branch `plugin-call` | C1: `from_bytes_hooks`, `load_hooks`, public `call`, `last_fuel`; SDK `json` feature with `json_hook`; WAT tests | — |
| W2 | letibot worktree, branch `plugins-stack` | C2 in `crates/plugins` with fake `Module`/`Loader`/`Builder` in tests; no wasmtime dependency yet | contracts only |
| W3 | letibot worktree, branch `plugins-pane` | C4 types in `crates/sessionlog/src/pane.rs` + protocol 49; the TUI pane that renders a `View` under the C3 keys, opened from the panes list, tested with fixture JSON | contracts only |
| W5 | letibot worktree, branch `plugins-dash` | C5: `plugins/examples/plan-dashboard`, the guest crate answering `pane_dashboard` from a status file, a fixture `status.json`, built for wasm32, with native unit tests of the view it returns | C1 SDK by path (text hook + its own serde_json) |
| W4 | letibot, after W1–W3 | harnessd: a rano `Loader`, the stack per session, looks run through the gate, `PluginPane` events, `PaneOpen`/`PaneAction`, hot reload behind the gate | W1, W2, W3 |
