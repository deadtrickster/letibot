# Plugins: a polymorphic harness — plan

Written 2026-10-25. The operator: *"why i want plugins - so i can move faster without release and
that models in session can adapt their environment to projects. I want polymorphic harness"*, and
*"yes we want to write letibot plugins in jitted wasm"*. And the immediate goal: *"if during
session i hit harness bug, limitation or other need for improvement i can do it right away with
plugins. yes it is messy but i can iterate and session discovery works pretty good so i can always
consolidate easily. which means plugins overrides are layered like firecode layers or dotenv. And
it also means we have to support hot reloading"*.

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

**The first use is a fix in the middle of a session.** The operator hits a harness bug or a
limitation, says so, and the session's model writes a layer that patches it — built, loaded and
answering within the same turn, without a release and without a restart. It is allowed to be
messy: layers pile up, and because sessions are discoverable, the pile is consolidated later —
squashed into one layer, promoted to the project, or turned into a proper core change. Three
things follow and shape the rest of this plan: overrides are **layered** (§2), loading is **hot**
(§2.3), and the hooks reach the places bugs actually are — before a tool runs, not only after
(§4).

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

## 2. Layers

### 2.1 The stack

Plugins form a **stack of layers**, like firecode's image layers or a chain of dotenv files: each
layer may override any hook, the topmost answer wins, and everything a layer does not mention falls
through to the layer below.

```
session layers   s3  "fix: CodeRabbit firing quotes an empty body"     ← newest, on top
                 s2  "watch buildkite on stroppy PRs"
                 s1  "digest: shorten ydb test logs"
project layers   p1  .letibot/plugins/…  (committed, reviewed)
user layers      u1  ~/.config/letibot/plugins/…
built-in         github-pr, coderabbit, pr-create, links             ← embedded defaults
```

- **The unit of override is a hook, not a plugin.** A layer that exports only
  `condition_coderabbit` overrides that one hook; `condition_github_pr` still comes from below. A
  layer is small because a fix is small.
- **Order within a scope is creation order**, newest on top, so the stack reads as the history of
  the fixes. Scopes stack session over project over user over built-in.
- **A layer can call down.** Asking for the layer below's answer is a *look* like any other (§1, principle 1):
  the hook returns `{"look":{"below": <input, possibly modified>}}` and is called again with the
  lower layer's output. A fix that only changes one field of the answer asks below and edits it,
  instead of re-implementing the hook — the dotenv `${VAR}` of this design.
- **A layer can mask.** `{"mask": true}` from a hook means *no answer here and none below* — the
  hook is switched off from this layer down (the built-in auto-watch, say). Masking is visible in
  the stack view, so a hook that does nothing is never a mystery.
- **A layer can be disabled without being deleted** (`plugin disable s2`): the stack is re-resolved
  as if it were not there. That is the bisect for *"which of my fixes broke this"*.

A plugin directory (any scope) holds `plugin.toml` (name, one-line purpose, the hooks it exports,
capabilities, contract versions), the source, and the built `plugin.wasm`. The source is what is
reviewed; the `.wasm` is rebuilt when stale.

### 2.2 Consolidation

Messy is fine because cleaning up is cheap:

- **`plugin squash s1..s3`** merges session layers into one, in the session, keeping the stack's
  answers the same (the model writes the merged source; the shadow gate, §2.4, replays the recorded
  hook inputs against old stack and new, and the squash is refused while they differ).
- **Promotion is a commit**: a layer moved to `.letibot/plugins/` on the session's branch goes
  through the merge queue, and the reviewers see the source and the capability list in the diff.
  Nothing else promotes.
- **Across sessions**: session layers are kept with the session, so session discovery finds them —
  *"which sessions patched the GitHub watch, and how"* — and a later session can lift one into the
  project, or into a core change when the fix belongs in Rust.

The transcript records `(layer, scope, sha256 of the .wasm)` the first time a layer answers in a
session and on every reload, so a replay or a review knows which policy was in force at each point.

### 2.3 Hot reload

A layer's change takes effect on the next hook call, with no restart:

- **Detected by the host, applied after the shadow gate**: the plugin directories are watched (the
  build writes `plugin.wasm`; `plugin load` asks explicitly). A changed module is a *candidate*: it
  goes through the dry run of §2.4, and only a candidate that passes is swapped in, atomically.
  Cranelift compiles a small module in milliseconds, so the gate, not the compile, is the wait.
- **In-flight calls finish on the version they started on.** The swap replaces the module for the
  next call; nothing is interrupted.
- **State survives the swap**: host-held state (§1, principle 5) is keyed by layer and hook, not by module, so
  a reloaded condition keeps its watch's baseline.
- **A bad candidate never replaces the good one.** One that fails the gate is refused with the
  gate's report, and the previous version keeps answering. The model is told either way.
- **Tools are the exception**: a plugin tool's schema is in the cached prompt prefix, so a change to
  a tool's *schema* waits for a reseat (which already announces tool changes); a change to its
  *behaviour* is hot like any other hook.

### 2.4 The shadow gate: a dry run on a copy of the session

The operator: *"and then mechanical thing - before applying we must check compilation etc on a
shadow session - do a dry run when a temporary session copy has changes applied"*.

Before any change to a stack takes effect — a new layer, a rebuilt one, a disable, a squash — the
daemon runs it on a **shadow**: a temporary copy of the session with the change applied, beside
the live session, which is not touched. It is the merge gate's role for layers: **mechanical, run
by the daemon, not by an agent**, and the butler's reviewer reads its report rather than redoing
it. The steps, first failure stops:

1. **Build** — `cargo build --offline --release --target wasm32-unknown-unknown` from the layer's
   source, so what is applied is what the source says (a committed or hand-copied `.wasm` that
   does not match its source is refused).
2. **Load** — compile, the ABI check (`rano_abi`), every export a known hook with a contract
   version this host speaks, the capabilities in `plugin.toml` granted and no import beyond them.
3. **Shadow** — fork the session's state into a hidden copy (its stack, its host-held hook state,
   its recorded hook inputs, its monitors as data) and apply the change there. The shadow has **no
   effects**: no model is called, no notice reaches anyone, no monitor is armed for real, and a
   look a hook declares is answered from the recorded look results where they exist and otherwise
   run only if it is read-only (`read`; `exec` against an allowlist of reading commands such as
   `gh pr view`, `git log`), else answered *skipped*.
4. **Replay** — every recorded input of every hook the change touches, through the live stack and
   the shadow stack. Traps, fuel exhausted, malformed output and contract violations fail; outputs
   are diffed.
5. **Report** — the build log, the load result, the fuel per hook, and the diff: *these inputs now
   answer differently, like this*. A change meant to alter behaviour has a diff and the diff is the
   point; a squash or a refactor must have none, and the gate refuses it otherwise.

Then the butler's reviewer reads the source with the report, and on a pass the daemon applies the
change to the live session, hot. On a box with a session VM the build and the shadow can run in
it; the shadow is discarded either way. Layers the session's model writes itself take the same
path — there is no way to apply a change that skips the gate.

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
block. Every hook is layered (§2.1): answered by the topmost layer that exports it, able to call
below, able to mask.

**The hooks go where the bugs are.** The bugs and limitations of the last weeks were nearly all at
four places: what a tool call is allowed to do and with which arguments (the monitor duplicates,
the coalescing), what happens after it (the missed PR watch), what the harness says to the model
(the empty `""` in a CodeRabbit firing, the conflict nobody was told about), and what a monitor
sees. Those four are the first hooks (§4.1–4.4), so a session can patch any of them in place.

### 4.1 `on_tool_call` — before a call runs

```jsonc
// in
{"v":1, "call":{"name":"monitor","arguments":{…}}, "session":{"mode":"allow-edits", …}}
// out — one of
{"pass":true}                                     // run it as it is (the default)
{"arguments":{…}}                                 // run it with these arguments instead
{"refuse":"already watched as pr-24; use that"}   // do not run it; the model reads why
{"answer":"…"}                                    // do not run it; this is its result
```

Runs after the model proposes a call and **before the gate**, so whatever it rewrites is still
adjudicated: a layer can narrow a call or refuse it, and a rewritten call that needs permission
still asks. This is the hook that patches *"the model armed nine monitors for three PRs"* the
same afternoon it happens.

### 4.2 `on_tool_result` — what happens after a call

```jsonc
// in
{"v":1, "call":{"name":"bash","arguments":{…}}, "outcome":"ok", "payload":"…"}
// out
{"append":"\n[harness] watching o/r#24 for checks, reviews…",   // to the call's result
 "arm":[{"plugin":"github-pr","args":{"pr":"o/r#24"}}]          // monitors to arm
}
```

Run for every successful tool result, through the stack, at the site `pr_watch::opened` hooks
today. Arming goes through the same door the operator's calls use, so coalescing and the
already-watched refusal apply.

### 4.3 `notice` — what the harness says to the model

```jsonc
{"v":1, "kind":"monitor_fired", "text":"[monitor] `pr206-coderabbit` fired: …", "facts":{…}}
→ {"text":"…"}   // or unchanged; or {"drop":true} for a notice this project never wants
```

Every message the harness injects — monitor firings, reminders, restart and reseat cards, the
merge queue's verdicts — passes through here with the structured facts it was made from, so a
layer can fix a misleading wording or add what the model was not told (*the PR is in conflict*).
A notice the operator must see cannot be dropped, only reworded; the set is the host's.

### 4.4 `condition` — what a monitor watches

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

### 4.5 `digest` — tool output before the model reads it

```jsonc
{"v":1, "call":{…}, "payload":"…"}  →  {"payload":"…shorter…"}  // or unchanged
```

For a project's test runner, build tool, log format. Runs before spill, so the digest is what is
budgeted.

### 4.6 `link` — what a word on the screen links to (TUI)

```jsonc
{"v":1, "word":"#212", "repo":{"slug":"o/r","branches":[…]}}  →  {"url":"https://…"}  // or null
```

Called per candidate word, so it is batched per frame (`{"words":[…]}` → `{"urls":[…]}`) and
cached by word. The refs keep being read by the host (`ghlinks::read_refs`).

### 4.7 `command` — slash commands

`/<name> args` → `{"say": "…to the model…"} | {"show": "…to the operator…"}`.

### 4.8 `tool` — tools the model calls (last)

A plugin tool's schema is prompt bytes in the cached prefix, so plugin tools load only at session
start and at a reseat — which already announces tool changes. Calls go through the gate as the
built-ins do; a tool that needs the world declares looks like a condition does.

### 4.9 `pane` — the UI language

*The exact contract, in serenedash's dashboard language (panels with views behind keys, one
grid, denominators, findings, legend), is C3 in `docs/plugins-slice1.md`; the sketch below is the
first draft.*

The operator: *"and we have to give plugins the ui language - so panes, keys, markdown itemized
navigation, esc, enter - all templated"*.

**Plugins describe, the host draws.** A plugin never paints cells or reads keys. It returns a
*view*: a document in a small vocabulary of the pieces the head already has. The head renders it
with the same rano widgets and the same conventions as its own panes, so a plugin pane cannot
look or behave differently from the built-in ones.

```jsonc
// pane_<name>: in
{"v":1, "state":{…}, "look":{…}, "event":null,       // or {"item":"tier1/jstests","action":"open"}
 "size":{"w":120,"h":40}}
// out
{"title":"stroppy-conformity · v0.2.0",
 "badge":{"text":"3 running","role":"attention"},     // the pane's counter in the corner
 "body":[
   {"table":{"columns":["item","status"],"rows":[
     {"id":"tier1/jstests","cells":["Mongo jstests","not started"],"role":"muted",
      "detail":"…markdown, shown on Enter…",
      "actions":[{"key":"s","label":"start","say":"start the jstests work"}]}]}},
   {"progress":{"label":"campaign A","done":41,"of":96,"role":"working"}},
   {"markdown":"…"}],
 "look":{"exec":["…"]}, "every_s":30,                 // refresh, as a condition does
 "state":{…}}
```

- **The pieces**: `markdown` (itemized: each list item and heading is a stop, ↑/↓ move between
  them as in the transcript's markers), `list`, `table`, `tree` (collapsible), `progress`, `kv`
  (label/value rows), `badge`. Each row can carry an `id`, a `role` (the palette's: muted,
  working, attention, success, failure), a `detail` and `actions`.
- **The keys are templated, not chosen**: ↑/↓/PgUp/PgDn/wheel move, **Enter** opens the row's
  `detail` (itself itemized markdown, Enter again into its items), **Esc** goes back one level and
  closes at the top — and only that, the Esc rule of every other pane, so a plugin cannot bind it
  to anything else. A plugin's own `actions` take letters, are shown in the hint bar, and are
  refused by the host if they collide with a reserved key.
- **What an action may do**: `say` (a line to the session's model, as if typed — subject to the
  ordinary queue rules), `look` (re-run the pane's look now), `arm` (a monitor), or `event` (call
  the pane again with `{"item","action"}` to change its own state — expand, filter, page). It
  cannot run anything the stack's capabilities do not allow.
- **Where panes live**: a plugin pane joins the pane list (alt-letter or the panes menu) beside
  todos, monitors and jobs, with the same scroll guard, wheel router and click arithmetic. A pane
  with a `badge` shows it in the status row, like the waits-on-you counter.
- **Hot and gated like any hook**: a pane's change goes through the shadow gate, where the view it
  returns for the recorded inputs is rendered to text and diffed — the gate's diff for a pane is
  what the pane looks like.

### Not hooks

Dialect parsers and tokencore (per-token hot path), the store and transcript, the allow side of
adjudication, the merge queue's landing, sudo and VMs.

### 4.10 The second goal: project dashboards

The operator: *"a huge tiered plan with multiple stages across 6 hosts. I want to have a dashboard
for it"* — of a project's long-running session, given as an example of the kind of thing a project
needs to see at once: a tiered plan with stages, the agents working it, the hosts it runs on. Today
such state is spread over plan files and whatever the agent re-types on request.

A **project dashboard is a project-scope `pane` layer** whose look reads the project's own sources
of truth (a status file the agents keep as data rather than prose, the session's agents and
branches, whatever scheduler the project runs). The first one is a generic example in this
repository, `plugins/examples/plan-dashboard` (C5 in `docs/plugins-slice1.md`), reading
`<project>/.letibot/status.json`; a project with a richer source writes its own.

### 4.11 Scripts that accrete, promoted to tools

The operator: *"per project/session scripts. those little things that accrete - maybe they should
be promoted to session/project local tools - maybe there must be similarity checks"*.

Every long session grows them: the `ssh … remote.sh state` typed for the tenth time, the
`for h in …; do …; done` that checks every host, the five-line `jq` over a report, the
`bin/fleet.sh` a project ends up owning. They live in the model's memory of the session, in a
`bin/` nobody indexes, or nowhere — retyped, slightly differently, each time. They are tools in all
but name, and the harness can see them, because every one passed through `bash`.

**A script tool is not wasm.** It is a descriptor around a command the harness already knows how
to run, through the same gate as `bash`:

```toml
# .letibot/scripts/host-state.toml   (project)  — or the session's scripts/ dir (session)
name = "host_state"
purpose = "the state of every campaign host's newest fleet job"
args = { host = "optional; one host instead of all" }
run = ["bin/remote.sh", "-H", "{host}", "state"]   # argv, no shell; or script = "host-state.sh"
replaces = ["ssh * remote.sh * state", "bin/remote.sh -n * state"]   # the shapes it absorbed
```

The same scopes and layering as plugins (§2): session, project, user; promotion is a commit and
the merge queue reviews it; the same name in a higher layer overrides.

**One tool for all of them, so the prompt cache never moves.** Each script is not its own tool
schema — that would re-prefill the conversation every time one is added (§4.8). The model gets one
stable tool, `script {name, args}`, and the catalog (name, purpose, args, one line each) travels as
a resident notice that is refreshed when it changes, the way standing notes are. Adding a script
costs a notice, not a prefix.

**Finding them: similarity over what `bash` ran.** The store already has every command. The
harness normalises each one into a shape (argv tokens, with paths, numbers, hashes, hostnames and
quoted strings replaced by typed holes: `ssh <host> <path>/remote.sh -n <id> state`) and clusters
shapes within the session and across the project's sessions:

- **Repetition** — the same shape run N times (default 3) in a session, or in M sessions of the
  project, is a *candidate*. The holes that varied become the script's args; the ones that never
  varied stay literal.
- **Near-duplicates** — shapes that differ by a flag or an argument order, or token-level
  similarity above a threshold (shingled tokens, Jaccard), cluster together; the candidate is the
  cluster, with its variants listed, not five scripts.
- **Against what exists** — a candidate is compared with the existing scripts' `run` and
  `replaces` shapes and with the files in the project's `bin/` and `scripts/`. A match is reported
  as *"this is `host_state` again"* (the model is pointed at the script it is reinventing), not
  offered as a new one. The same check runs when a script is *written*: a new script whose shape
  matches an existing one is refused with the name of the one it duplicates, unless it says what it
  does differently.
- **Drift** — a script whose calls keep being followed by the same hand-typed fix-up (`| jq …`, a
  retry) is a candidate for a new version, not a new script.

**Who promotes.** The butler (§6) owns it: friction it watches for includes *"the same shape
again"*. It writes the descriptor into the session's scripts, tells the model *"`host_state` now
does what you typed four times"*, and at the end of the session proposes the ones worth promoting
to the project — with the sessions that used them as the evidence. The shadow gate (§2.4) applies:
a script is built (its `run` resolves, the file exists, it is executable), loaded (the descriptor
parses, its name and args are unique, its shape does not duplicate another's), and replayed against
the recorded commands it claims to replace (each one must map onto the script with some args).

**A script that outgrows its descriptor becomes a plugin.** When it needs state, a look loop or a
pane, the butler turns it into a wasm layer; the descriptor's `replaces` shapes carry over, so the
similarity check keeps pointing at it.

## 5. What the model gets

- **`plugin` tool** with actions:
  - `scaffold` — a new session plugin from a template for a hook kind (a crate depending on the
    letibot plugin SDK, `plugin.toml`, a test).
  - `build` — `cargo build --offline --release --target wasm32-unknown-unknown` in the plugin's
    directory, through the gate like any `bash` call; errors come back verbatim.
  - `load` — submit to the shadow gate (§2.4) and apply on a pass (a rebuilt `plugin.wasm` is
    submitted by itself too, §2.3).
  - `stack` — the layers, newest first, with each hook's answering layer, masks, disabled layers,
    recent answers and recent failures.
  - `disable` / `enable` — a layer, without deleting it (the bisect).
  - `test` — call a hook with a given input through the whole stack or one layer, and show the
    output, which layer answered, fuel used and any trap; or replay the session's recorded inputs.
  - `squash` — merge session layers into one, through the gate, refused while the replay differs
    (§2.2).
  - `promote` — move a layer into `.letibot/plugins/` on the session's branch.
- **`plugin_hooks`** — the contracts of §4 with a worked example each, read from the host itself so
  it cannot drift from what the host accepts.
- **A skill** walking through the main loop end to end: the operator reports a harness problem, the
  model finds the hook it lives at, writes a layer, tests it against the input that misbehaved,
  and loads it.
- **Recorded inputs**: the host keeps the last inputs of each hook per session, so a layer is
  written and tested against the very call or firing that went wrong.

The SDK is `letibot-plugin`, in this repository: rano's `rano-plugin` ABI plus serde types for
each hook's input and output, so a plugin is `#[hook] fn condition(i: ConditionIn) -> ConditionOut`.

## 6. The harness butler

The operator: *"which means harness core should help its agents to modify itself, maybe we can have
a new role - harness butler - the thing that sits in the background of every session, created
automatically with configured (and default) model and orchestrates and reviews the changes"*.

**Why a separate agent.** The session's model is there for the project. Every turn it spends
patching the harness is a turn off the task and context spent on the harness's internals; and a
model reviewing the layer it just wrote reviews nothing. The butler owns the harness side, so the
session asks for a fix and gets back to work while the fix happens beside it.

And: *"not dissimilar to reviewers in queue"*.

**What it is: the gatekeeper's shape, for the harness.** The review queue already has the
structure (`crates/harnessd/src/sessions/gatekeeper.rs`): one hard-coded session the daemon starts
on the default or a configured model, a normal self-driving session whose **todo board is the
queue** (so nags and all), whose reviews are its own `task` children, and which the operator can
attach to and steer through its prompt. The butler is the same thing, for the harness instead of
for branches:

| | review queue | harness butler |
|---|---|---|
| the session | `s-gatekeeper`, one per daemon, a root | `s-butler`, one per daemon, a root |
| its board | one row per branch waiting for a verdict | one row per harness request, naming the session it is for |
| its children | reviewers (`Gatekeeper` role) | a **fitter** that writes the layer into the requesting session's stack, and a **reviewer** that passes or refuses it — never the same child |
| its model | default, or configured | default, or `butler.model` |
| the daemon's part | fills the queue, worktrees, gates, landing — does not review | records hook inputs, runs the shadow gate, applies layers, enforces capabilities — does not write layers |

One butler, not one per session: like the gatekeeper it is a **peer** of the working sessions,
with no tree edge, and it sits behind every session by serving all of their requests from one
board. That sees friction across sessions at once (three sessions hitting the same bug is one row
with three requesters) and costs one context, not one per session. The working session's head
shows its requests' rows, and opening one goes into the butler's child, as the queue's `o` does.

New roles beside `Gatekeeper` and the notes keeper: **`Fitter`** (`plugin`, `read` over the
session's recorded hook inputs and over letibot's own source and docs, no tool that edits the
project) and **`Butler`** for the session itself (the board, `task`, `plugin stack`/`disable`, no
authoring). Never seats a person picks; the daemon starts them.

**What wakes it.** It costs nothing while asleep. It wakes on:

- **the operator** — `/butler <what is wrong>`, or a harness complaint in the conversation that
  the session's model hands over with a `butler` call (*"the monitor pane doesn't show X"*);
- **the session's model** — asking for a change it needs (*"watch this project's Buildkite"*);
- **friction the harness sees itself** — a layer that traps or keeps failing, the same tool refused
  several times in a row, a monitor armed twice for the same thing, a notice the model answers with
  confusion. These are suggestions, not orders: the butler says what it saw and what it would do.

**What it does.**

1. **Writes the layer** (through a fitter child). Finds the hook the problem lives at, writes the
   layer, tests it against the recorded input that misbehaved (`plugin test`), and submits it to
   the shadow gate (§2.4). The session's model and the
   operator get one line saying what changed and how to undo it (`plugin disable s4`).
2. **Reviews every layer** (through a reviewer child, never the fitter that wrote it) — the
   fitter's, and any the session's model writes itself — after the gate passes it: the source
   against the claimed purpose, the capabilities against what the purpose needs, and the gate's
   diff against what the change was for. A refusal says why; the operator can override the
   review, never the gate.
3. **Keeps the stack healthy.** Disables a layer that keeps failing and says so; offers a
   `squash` when the session's layers pile up; and at the end of a session proposes what is worth
   promoting to the project.
4. **Remembers across sessions.** Through session discovery it sees that three sessions wrote the
   same fix, and proposes the project layer — or, when the fix belongs in Rust, writes up the core
   change for us, with the layer as the reproduction.

**What it may not do.** Everything §1 says of a plugin holds of the butler's layers, whoever asked
for them: tighten only, capabilities through the gate, fail down. A capability on a session layer
still asks the operator once, unless allow-all. The butler never promotes on its own — promotion
is a commit through the merge queue, reviewed by the queue's reviewers, not by the butler that
wrote it.

**The cost to watch.** On a box with one local model server, a butler on the same model competes
with the session for the GPU. That is why it sleeps until woken, why its model is a setting, and
why its work runs between the session's turns when the two share a server.

## 7. Build order

1. **Host in harnessd.** The layer stack (scopes, per-hook resolution, call-below, mask,
   disable), `plugin.toml`, capability checks, fail-down, recorded inputs, the transcript record,
   **the shadow gate** (build, load, shadow copy with no effects, replay and diff, report) and
   **hot reload** behind it (watch, gate, swap, keep state, keep the good version). Engine: `rano::plugin` (ABI v1; letibot-side hook
   kinds and JSON contracts). Tests with WAT fixtures, as rano's are, so no test needs a wasm
   toolchain.
2. **The four patch points — `on_tool_call`, `on_tool_result`, `notice`, `condition`** — the
   `letibot-plugin` SDK, and the `plugin` tool (`scaffold`, `build`, `load`, `stack`, `disable`,
   `test`) plus `plugin_hooks` and the skill. From here the operator can fix things in a session.
3. **The GitHub family as built-in plugins**: `github-pr` (checks, review, comment, closed,
   merged, conflict) and `coderabbit` as conditions, `pr-create` as `on_tool_result`. Delete
   `GithubCondition`'s decision half and `pr_watch`'s; keep the host's `gh` look runner and the
   coalescing. The existing GitHub tests move to the plugins and must pass unchanged in meaning.
4. **The butler**: the role, its creation with the session, `/butler`, the `butler` call, the
   review gate on layers it did not write, and the friction signals.
   **Scripts** (§4.11) with it: the `script` tool and its resident catalog, command shapes and the
   similarity clustering over the store's `bash` history, candidates, duplicate refusal, promotion.
5. **Consolidation**: `squash`, `promote`, and session discovery over layers.
6. **`pane`** (§4.9) and **the example plan dashboard** (§4.10).
7. **A real trial**: a stroppy session writes a project plugin for something it needs (its own CI
   or bot), with the operator watching where it gets stuck. What it trips on is the next work.
8. `digest`, then `link` in the TUI (porting `ghlinks::url_of`), then `command`.
9. `tool`, at reseat.

## 8. Open questions

- **Toolchain on the other machines**: `wasm32-unknown-unknown` on lab2x1 and lubuntu1, and inside
  firecode VMs if plugin builds run there.
- **Where `build` runs**: on the host like `bash`, or in the session's VM when it has one. A build
  runs build scripts, which is the same trust as `cargo build` by the model today.
- **Session layers across a restart**: kept with the session and re-loaded, with the model told
  which layers are active, as it is told about scoped monitors.
- **Recorded inputs and privacy**: how many inputs per hook are kept, and for how long — they hold
  tool output.
- **A child session** (`task_start`) inherits the parent's session layers, or only the project's.
- **Per daemon or per box**: the gatekeeper is per daemon; a butler per box would share one
  board across folders, but a layer is about a project, and the daemon is per folder.
- **The butler's default model** when the session runs on the local server: the same model (free,
  contends for the GPU) or a remote one (no contention, network egress, the operator's call).
- **The TUI's plugins** (`link`) load from the same scopes, read by the head from the session's
  workspace — or the daemon sends them.
