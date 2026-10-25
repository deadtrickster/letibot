# Plugins: a polymorphic harness — plan

Written 2026-10-25. The operator: *"why i want plugins - so i can move faster without release and
that models in session can adapt their environment to projects. I want polymorphic harness"*, and
*"yes we want to write letibot plugins in jitted wasm"*.

**The one-paragraph version**: letibot becomes a small trusted core plus policy that is wasm.
The core is what must never be wrong — the turn loop, dialects, the store, the gate and the
adjudicator, the sandbox and VMs, the merge queue, the plugin host. The policy is what changes
per project and per week — what is watched and how a firing reads, what happens after a tool
call, how output is digested, what a word on the screen links to, later the tools themselves.
Today's built-in policy is rewritten as **default plugins** embedded in the binary; a project or
a session overrides one by name. Plugins are written **by the model, in the session**, built and
loaded hot, and promoted to the project by committing them — where the merge queue's reviewers
review them like any other code. Moving faster is then a commit to the project, not a letibot
release.

## 0. What exists

| thing | where | state |
|---|---|---|
| wasm host | rano `src/plugin.rs` (728 lines), `plans/S10-wasm-hooks.md` | wasmtime + cranelift (JIT). ABI v1: `rano_abi() -> 1`, `rano_alloc(n)`, hooks found by export name (`on_*`, `provider_*`), bytes in → 8-byte result block out, `-1` = unchanged. No imports unless a capability grants one (only `read-dir` exists). Per call: `HOOK_FUEL` 50M, `HOOK_MEMORY` 64 MiB, `MAX_HOOK_BYTES` 16 MiB each way. |
| guest SDKs | rano `sdks/rust` (`rano-plugin`), `sdks/assemblyscript` | `rano::abi!()` + `rano::hook(ptr, len, \|text\| …)`. |
| toolchain | this Mac | `wasm32-unknown-unknown` installed 2026-10-25; rano's `examples/plugin-upper` builds offline in seconds to a 20 KB module. lab2x1 / lubuntu1 not checked yet. |
| GitHub watches | `crates/tools/src/exec/github.rs` (984 lines) | `GithubCondition`: spawns `gh` at most once per interval, polls the child with `try_wait`, `parse(json, viewer) -> Seen`, baseline-then-news, `Until::{Closed, Merged, Checks, Review, Comment, CodeRabbit}`, `is_rabbit`. |
| auto-watch | `crates/harnessd/src/pr_watch.rs`, hooked at `harness.rs` (`pr_watch::opened` on each successful tool result) | `gh pr create` → one coalesced `pr-N` watch on `WATCHED`, and a sentence appended to the call's result. |
| repository links | `crates/tui/src/app/ghlinks.rs` | `GitRefs::url_of(word)`, fed refs read in the background. |
| monitors | `crates/tools/src/exec/monitor.rs` `Condition` | `met()`, `wait()`, `rearm()`, `describe()`, `max_ttl()`, `github()`; the poller, owners, restart rules, the monitors pane, todo waits. |
| extension traits | `tools::runtime::{Tool, Gate}`, `adjudicate::Adjudicator`, `external::web::{SearchProvider, Fetcher}`, `registry::ShellSuggester` | Rust-only seams; each is a candidate hook (§4). |

## 1. Principles

1. **Plugins never do I/O themselves.** A hook is a pure function: JSON in, JSON out. When a
   plugin needs the world (run `gh`, read a file, fetch a URL), it *declares* the look and the host
   does it — asynchronously, under the gate — and calls the plugin again with the answer. A wasm
   call therefore never blocks on the network, fuel stays a measure of work, and the same input
   gives the same output (replay, tests).
2. **A plugin can do nothing the model could not.** Every look a plugin declares goes through the
   same gate and session mode as the model's own tool calls: under allow-edits a plugin's `gh` is
   adjudicated exactly as the model's `bash` running `gh` would be. Capabilities (§3) say which
   kinds of look a plugin may declare at all.
3. **Tighten only.** A hook on adjudication or on the merge gate can add a refusal, an ask or a
   failing check; never remove one. The allow side of the adjudicator, sudo, VMs, the merge queue's
   landing and the store stay Rust.
4. **Fail down, loudly.** A plugin that traps, runs out of fuel, returns malformed JSON or is
   refused a capability is skipped for that call and the next one down the precedence (§2) answers
   — ultimately the built-in default. The failure is told to the session's model (it can fix it)
   and shown in the monitors pane.
5. **State belongs to the host.** A plugin is stateless between calls; state it needs (a watch's
   baseline) is an opaque JSON blob it returns and the host stores with the monitor, so restarts,
   reloads and a newer build of the plugin keep it.
6. **Contracts are versioned like the ABI.** Plugins outlive releases. Each hook kind has a
   versioned JSON schema (`"v": 1` in every input); a host that changes a contract keeps answering
   the old version or refuses the plugin by name. Additive fields are not a version.

## 2. Scopes and precedence

| scope | lives in | written by | lifetime |
|---|---|---|---|
| session | the session's state dir, `plugins/` | the model, in that session | the session |
| project | `.letibot/plugins/<name>/` in the repository | the model, committed | the repository |
| user | `~/.config/letibot/plugins/<name>/` | the operator | until removed |
| built-in | embedded in harnessd | us | the release |

**Precedence: session → project → user → built-in, by name.** A plugin named `coderabbit` in a
project replaces the built-in `coderabbit` for sessions in that project. A plugin directory holds
`plugin.toml` (name, hooks it claims, capabilities, contract versions), the source, and the built
`plugin.wasm`; the source is what is reviewed, the `.wasm` is rebuilt when stale.

**Promotion is a commit.** A session plugin that proved itself is moved to `.letibot/plugins/` on
the session's branch and goes through the merge queue; the reviewers see the source and the
capability list in the diff. Nothing else promotes.

The transcript records `(name, scope, sha256 of the .wasm)` the first time a plugin answers in a
session, so a replay or a review knows which policy was in force.

## 3. Capabilities

Declared in `plugin.toml`, enforced by the host at the moment a plugin declares a look:

- `exec:<program>` — a look may run `<program>` (e.g. `exec:gh`, `exec:git`) with arguments the
  plugin chooses, in the workspace, through the gate. No shell, no other program.
- `read` — a look may read files under the workspace (the same confinement `read` has).
- `http:<host>` — a look may GET from `<host>` (later; network egress is the operator's call).

**Granting.** Built-in and user plugins: granted by being installed. Project plugins: granted by
the merge — the capability list is part of the reviewed diff. Session plugins: the first time a
session plugin is loaded with a capability, the operator gets one question (an ask card naming the
plugin, the capability, and the plugin's one-line purpose); allow-all answers it yes.

## 4. Hooks

Each hook kind is a wasm export `<kind>_<name>` (rano's convention: the export enumerates what a
module answers) taking the JSON input and returning the JSON output through the ABI v1 result
block. Order is the build order (§6).

### 4.1 `condition` — what a monitor watches

```jsonc
// in
{"v":1, "args":{…the monitor call's arguments…}, "state":{…or null on arming…},
 "look":{"exit":0, "stdout":"…", "stderr":"…"}  // null on the first call
}
// out
{"look":{"exec":["gh","pr","view","24","--json","state,reviews,…"]},  // what to run next
 "every_s":90, "max_ttl_s":604800,
 "fired":"CodeRabbit reviewed: 3 comments…",   // or null
 "state":{…},                                   // stored by the host
 "github":{"pr":"o/r#24","watches":["coderabbit"]}  // optional: one watch per pull request
}
```

The host side is a `PluginCondition: Condition` that runs the declared look the way
`GithubCondition` runs `gh` today (spawn at most once per interval, `try_wait`, never stall the
poller), calls the plugin with the result, and fires on `fired`. The model arms one with the
existing `monitor` tool: `{"plugin":"coderabbit","args":{"pr":"o/r#24"}}`.

### 4.2 `on_tool_result` — what happens after a call

```jsonc
// in
{"v":1, "call":{"name":"bash","arguments":{…}}, "outcome":"ok", "payload":"…"}
// out
{"append":"\n[harness] watching o/r#24 for checks, reviews…",   // to the call's result
 "arm":[{"plugin":"github-pr","args":{"pr":"o/r#24"}}]          // monitors to arm
}
```

Run for every successful tool result, in precedence order, at the site `pr_watch::opened` hooks
today. Arming goes through the same door the operator's calls use, so coalescing and the
already-watched refusal apply.

### 4.3 `digest` — tool output before the model reads it

```jsonc
{"v":1, "call":{…}, "payload":"…"}  →  {"payload":"…shorter…"}  // or unchanged
```

For a project's test runner, build tool, log format. Runs before spill, so the digest is what is
budgeted.

### 4.4 `link` — what a word on the screen links to (TUI)

```jsonc
{"v":1, "word":"#212", "repo":{"slug":"o/r","branches":[…]}}  →  {"url":"https://…"}  // or null
```

Called per candidate word, so it is batched per frame (`{"words":[…]}` → `{"urls":[…]}`) and
cached by word. The refs keep being read by the host (`ghlinks::read_refs`).

### 4.5 `command` — slash commands

`/<name> args` → `{"say": "…to the model…"} | {"show": "…to the operator…"}`.

### 4.6 `tool` — tools the model calls (last)

A plugin tool's schema is prompt bytes in the cached prefix, so plugin tools load only at session
start and at a reseat — which already announces tool changes. Calls go through the gate as the
built-ins do; a tool that needs the world declares looks like a condition does.

### Not hooks

Dialect parsers and tokencore (per-token hot path), the store and transcript, the allow side of
adjudication, the merge queue's landing, sudo and VMs.

## 5. What the model gets

- **`plugin` tool** with actions:
  - `scaffold` — a new session plugin from a template for a hook kind (a crate depending on the
    letibot plugin SDK, `plugin.toml`, a test).
  - `build` — `cargo build --offline --release --target wasm32-unknown-unknown` in the plugin's
    directory, through the gate like any `bash` call; errors come back verbatim.
  - `load` / `unload` — hot, for this session; a reload replaces the module and keeps host-held
    state.
  - `test` — call a hook with a given input and show the output, fuel used and any trap.
  - `list` — what is loaded, from which scope, which hooks answered recently, recent failures.
- **`plugin_hooks`** — the contracts of §4 with a worked example each, read from the host itself so
  it cannot drift from what the host accepts.
- **A skill** walking through writing a condition and an `on_tool_result` end to end.

The SDK is `letibot-plugin`, in this repository: rano's `rano-plugin` ABI plus serde types for
each hook's input and output, so a plugin is `#[hook] fn condition(i: ConditionIn) -> ConditionOut`.

## 6. Build order

1. **Host in harnessd.** Loading from the four scopes with precedence, `plugin.toml`, capability
   checks, fail-down, the transcript record. Engine: `rano::plugin` (ABI v1; letibot-side hook
   kinds and JSON contracts). Tests with WAT fixtures, as rano's are, so no test needs a wasm
   toolchain.
2. **`condition` + `on_tool_result`**, the `letibot-plugin` SDK, and the `plugin` tool
   (`scaffold`, `build`, `load`, `test`, `list`) plus `plugin_hooks`.
3. **The GitHub family as built-in plugins**: `github-pr` (checks, review, comment, closed,
   merged, conflict) and `coderabbit` as conditions, `pr-create` as `on_tool_result`. Delete
   `GithubCondition`'s decision half and `pr_watch`'s; keep the host's `gh` look runner and the
   coalescing. The existing GitHub tests move to the plugins and must pass unchanged in meaning.
4. **A real trial**: a stroppy session writes a project plugin for something it needs (its own CI
   or bot), with the operator watching where it gets stuck. What it trips on is the next work.
5. `digest`, then `link` in the TUI (porting `ghlinks::url_of`), then `command`.
6. `tool`, at reseat.

## 7. Open questions

- **Toolchain on the other machines**: `wasm32-unknown-unknown` on lab2x1 and lubuntu1, and inside
  firecode VMs if plugin builds run there.
- **Where `build` runs**: on the host like `bash`, or in the session's VM when it has one. A build
  runs build scripts, which is the same trust as `cargo build` by the model today.
- **Session plugins across a restart**: kept with the session (likely) and re-loaded, with the
  model told, as scoped monitors are.
- **A child session** (`task_start`) inherits the parent's session plugins, or only the project's.
- **The TUI's plugins** (`link`) load from the same scopes, read by the head from the session's
  workspace — or the daemon sends them.
