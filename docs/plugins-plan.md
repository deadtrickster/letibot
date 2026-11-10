# Plugins: a polymorphic harness

letibot is a small trusted core plus policy that is wasm. The core is what must never be wrong: the turn
loop, dialects, the store, the gate and the adjudicator, the sandbox and VMs, the merge queue, the plugin
host. The policy is what changes per project and per week: what is watched and how a firing reads, what
happens after a tool call, how output is digested, what a word on the screen links to, later the tools
themselves. The built-in policy is written as default plugins embedded in the binary, and a project or a
session overrides one by name.

The first use is a fix in the middle of a session: the operator hits a harness bug or a limitation, the
session's model writes a layer that patches it, and the layer is built, loaded and answering within the
same turn, without a release and without a restart. It is allowed to be messy, because sessions are
discoverable and the pile is consolidated later: squashed into one layer, promoted to the project, or
turned into a core change. So overrides are layered (§2), loading is hot (§2.3), and the hooks reach the
places bugs actually are, before a tool runs and not only after (§4).

## 0. What exists

| thing | where | state |
|---|---|---|
| wasm host | rano `src/plugin.rs` | wasmtime + cranelift. ABI v1: `rano_abi() -> 1`, `rano_alloc(n)`, hooks by export name, bytes in and an 8-byte result block out, `-1` = unchanged. No imports unless a capability grants one. Per call: `HOOK_FUEL` 50M, `HOOK_MEMORY` 64 MiB, `MAX_HOOK_BYTES` 16 MiB each way. |
| guest SDKs | rano `sdks/rust` (`rano-plugin`), `sdks/assemblyscript` | `rano::abi!()` + `rano::hook(ptr, len, \|text\| …)`. |
| the stack, the gate, the trust book | `crates/plugins` (`letibot-plugins`) | §2. Wasmtime is behind a `Loader` trait, so a fake stands in for the engine in tests. |
| the host's half | `crates/harnessd/src/plugins.rs` + `plugins/` | the rano `Loader`, the per-session stack, looks through the gate, `PluginPane` frames, hot reload behind the gate, the `room` and `sessions` feeds. |
| GitHub watches | `crates/tools/src/exec/github.rs` | `GithubCondition`: spawns `gh` at most once per interval, polls with `try_wait`, `parse(json, viewer) -> Seen`, baseline-then-news, `Until::{Closed, Merged, Checks, Review, Comment, CodeRabbit}`. |
| auto-watch | `crates/harnessd/src/pr_watch.rs`, hooked in `harness.rs` | `gh pr create` becomes one coalesced `pr-N` watch, and a sentence appended to the call's result. |
| repository links | `crates/tui/src/app/ghlinks.rs` | `GitRefs::url_of(word)`, fed refs read in the background. |
| monitors | `crates/tools/src/exec/monitor.rs` `Condition` | `met()`, `wait()`, `rearm()`, `describe()`, `max_ttl()`, `github()`; the poller, owners, restart rules, the monitors pane, todo waits. |
| extension traits | `tools::runtime::{Tool, Gate}`, `adjudicate::Adjudicator`, `external::web::{SearchProvider, Fetcher}`, `registry::ShellSuggester` | Rust-only seams; each is a candidate hook. |

## 1. Principles

1. Plugins never do I/O themselves. A hook is a pure function: JSON in, JSON out. When a plugin needs the
   world it *declares* the look and the host does it, asynchronously, under the gate, then calls the plugin
   again with the answer. A wasm call never blocks on the network, fuel stays a measure of work, and the
   same input gives the same output.
2. A plugin can do nothing the model could not. Every look it declares goes through the same gate and
   session mode as the model's own tool calls; capabilities (§3) say which kinds of look it may declare.
3. Tighten only. A hook on adjudication or on the merge gate can add a refusal, an ask or a failing check;
   never remove one. The allow side of the adjudicator, sudo, VMs, the merge queue's landing and the store
   stay Rust.
4. Fail down, loudly. A plugin that traps, runs out of fuel, returns malformed JSON or is refused a
   capability is skipped for that call and the next one down answers, ultimately the built-in default. The
   failure is told to the session's model and shown in the stack view.
5. State belongs to the host. A plugin is stateless between calls; state it needs is an opaque JSON blob it
   returns and the host stores, so restarts, reloads and a newer build keep it.
6. Contracts are versioned like the ABI. Each hook kind has a versioned schema (`"v"` in every input); a host
   that changes a contract keeps answering the old version or refuses the plugin by name. An added field is
   not a version.

## 2. Layers

### 2.1 The stack

Plugins form a stack of layers, like firecode's image layers or a chain of dotenv files: each layer may
override any hook, the topmost answer wins, and everything a layer does not mention falls through.

```
session layers   s3  "fix: CodeRabbit firing quotes an empty body"     <- newest, on top
                 s2  "watch buildkite on stroppy PRs"
                 s1  "digest: shorten ydb test logs"
project layers   p1  .letibot/plugins/…  (committed, reviewed)
user layers      u1  ~/.config/letibot/plugins/…
built-in         sessions, queue, room, todo, irc, claude-transcript, github   (step 3's conditions are
                 still left)
```

- The unit of override is a hook, not a plugin: a layer exporting only `condition_coderabbit` overrides that
  hook, and `condition_github_pr` still comes from below.
- Order within a scope is creation order, newest on top; scopes stack session over project over user over
  built-in.
- A layer can call down with `{"below": <input>}` and is called again with the original input plus the lower
  layer's answer under `below`. The marker is `below` and not `look`, because a pane's own document has a
  top-level `look` meaning *run this for me* (`docs/plugins-slice1.md` C3).
- A layer can mask: `{"mask": true}` means no answer here and none below, and the stack view shows it.
- A layer can be disabled without being deleted (`plugin disable s2`), which is the bisect for *which of my
  fixes broke this*.

A layer directory holds `plugin.toml` (name, purpose, hooks, capabilities, contract versions, feeds, actions,
tools), the source and the built module. The source is what is reviewed.

### 2.2 Consolidation

- `plugin squash s1..s3` merges session layers into one, and the shadow gate replays the recorded inputs
  against the old stack and the new one, refusing the squash while they differ.
- Promotion is a commit: a layer moved to `.letibot/plugins/` on the session's branch goes through the merge
  queue, and the reviewers see the source and the capability list in the diff.
- Session layers are kept with the session, so session discovery finds them and a later session can lift one
  into the project or into a core change.

The transcript records `(layer, scope, sha256 of the module)` the first time a layer answers and on every
reload, so a review knows which policy was in force.

### 2.3 Hot reload

A layer's change takes effect on the next hook call, with no restart: the directories are watched (and
`plugin load` asks explicitly), a changed module goes through §2.4, and only a candidate that passes is
swapped in atomically. In-flight calls finish on the version they started on, host-held state survives the
swap because it is keyed by layer and hook, and a bad candidate never replaces the good one. Tools are the
exception: a tool's schema is prompt bytes in the cached prefix, so a schema change waits for a reseat.

### 2.4 The shadow gate: a dry run on a copy of the session

Before any change to a stack takes effect (a new layer, a rebuilt one, a disable, a squash) the daemon runs it
on a shadow: a temporary copy of the session with the change applied, beside the live session, which is not
touched. It is the merge gate's role for layers: mechanical, run by the daemon, not by an agent. The steps,
first failure stops:

1. Build: `cargo build --offline --release --target wasm32-unknown-unknown` from the layer's source, so what
   is applied is what the source says. A module that does not match its source is refused.
2. Load: compile, the ABI check (`rano_abi`), every export a known hook with a contract version this host
   speaks, and the capabilities granted with no import beyond them.
3. Shadow: fork the session's state into a hidden copy and apply the change there. It has no effects: no
   model is called, no notice reaches anyone, no monitor is armed, and a look is answered from the recorded
   look results or run only if it is a *reading* one. Reading is not a list this step keeps: it is the gate's
   own answer to *would this run without putting a question to anybody* (`Gate::admit_unasked`, the ladder
   §4.4's `exec` look goes through), so the shadow and the live stack cannot disagree about which command is
   a read. (The gate runs no look at all today, so there is no allowlist to retire yet.)
4. Replay: every recorded input of every hook the change touches, through both stacks. Traps, exhausted fuel,
   malformed output and contract violations fail; outputs are diffed.
5. Report: the build log, the load result, the fuel per hook, and the diff. A change meant to alter behaviour
   has a diff and the diff is the point; a squash or a refactor must have none, and the gate refuses it
   otherwise.

### 2.5 Trust: a built-in runs freely, and a placed layer needs a pin

D17, the operator's rule: *only the builtins run freely; anything I place later needs a pin.*

1. A built-in runs freely, `exec` included: it is the binary's own bytes, nobody placed them at run time
   (`Stack::register_builtin_shared` marks it free).
2. A placed layer needs a pin: the SHA-256 of the layer's module AND of its `plugin.toml`, taken by a person
   (`/plugin trust NAME`). A layer whose bytes match a pin is free. Every other layer a person or a model put
   on disk is adjudicated on every call: its tool calls and its `exec` looks go through
   `Gate::admit_unasked`, which admits by the session's own rules or refuses, and never raises a card,
   because a layer cannot put a question to a person.
3. A byte change after the pin un-pins it: a layer rebuilt or edited after it was pinned is adjudicated
   again, and the sentence names the layer and both hashes, once, as the `plugin_untrusted` warning.

Provenance is recorded, not inferred: `write` and `edit` report the bytes they landed and the daemon appends
a record for anything about a layer, so *who wrote these bytes* is a fact on disk. The ladder never consults
it, so the weaker fact can never widen what runs.

| piece | where |
|---|---|
| the book (pins + write records), one JSON file in the config dir, written under a lock and merged by time | `letibot_plugins::Trust` (`crates/plugins/src/trust.rs`) |
| the decision, per layer | `Layer::trusted` / `Layer::pin`, from `Stack::set_trust`, re-decided by `Stack::refresh_trust` |
| a tool call of an unpinned layer | `crates/harnessd/src/plugins/tool.rs` (`PluginTool::admit`) |
| an `exec` look of an unpinned layer | `crates/harnessd/src/plugins/conditions.rs` (`StackOver::adjudicate`) |
| the write records | `crates/tools/src/provenance.rs` + `crates/harnessd/src/plugins.rs`'s `Provenance` |
| the operator's door | `crates/tui`'s `/plugin`, `/plugin trust NAME`, `/plugin untrust NAME` |

The book has two writers in two processes, so a save is a read-modify-write under an exclusive lock on
`<book>.lock`, merging with the file by time: a pin only stands if it is newer than the revocation of the same
layer. A revocation is a row with a time on it, not an absence, because an absence cannot be told from *never
mentioned*. The `plugin` tool has no `trust` verb, and the book lives in
`$XDG_CONFIG_HOME/letibot/plugin-trust.json` and not beside the layer it is about: a pin written where a model
can write is not a pin.

What is NOT adjudicated, and why: a `pane_`, `notice_`, `command_` or `digest_` answer. Those hooks reach
nothing; they answer JSON, the host validates it, and a look they ask for is capability-checked.

## 3. Capabilities

Declared in `plugin.toml`, enforced by the host at the moment a plugin declares a look:

- `exec:<program>`: a look may run `<program>` (e.g. `exec:gh`) with arguments the plugin chooses, in the
  workspace, through the gate. No shell, no other program.
- `read`: a look may read files under the workspace.
- `read:<path>`: a look may read one tree outside the workspace, named as a person wrote it down
  (`read:~/.claude/projects`). §4.8.1 is the whole of it.
- `http:<host>`: a look may GET from `<host>` (later; network egress is the operator's call).

Built-in and user plugins are granted by being installed. Project plugins are granted by the merge, because
the capability list is part of the reviewed diff. Session plugins: the first time one is loaded with a
capability the operator gets one ask card naming the plugin, the capability and its one-line purpose;
allow-all answers it yes.

A capability says what a layer MAY do; §2.5's ladder says whether it does it freely. A capability is enforced
either way, but an unpinned layer's `exec` look also meets the gate and a built-in's or a pinned one's does
not.

### 3.1 Two ABIs: rano's, and WASI

rano's ABI v1 stays what editor hooks and slice 1 speak. WASI is to be added beside it, not instead of it,
because the toolchains already target it (`GOOS=wasip1`, C, Rust, Python, JavaScript), as a second `Loader`
behind the `Loader`/`Module` trait (C2), chosen by `abi = "rano-1"` (the default) or `abi = "wasi-p2"`. Two
worlds, by what may block: `hook` (panes, conditions, notices, digests, links) exports
`handle: func(hook: string, input: string) -> result<option<string>, string>` and imports nothing, not even
WASI's clocks, so the gate's replay stays exact; `script` (script tools, §4.11) gets `wasi:cli` plus one
letibot import, `letibot:host/exec`, a synchronous gated exec adjudicated like `bash`. Capabilities are
WASI's own shape: `read` pre-opens the workspace read-only, `read:<path>` that one tree, `write` (scripts
only) read-write, `http:<host>` and `exec:<prog>` for that authority or program only, and none means no
pre-opens and no sockets. In the shadow gate a script sees a fixed clock, seeded random, a snapshot of the
workspace and no network. Not built: `wasmtime-wasi` is not in `Cargo.toml`.

## 4. Hooks

Each hook kind is a wasm export `<kind>_<name>` taking the JSON input and returning the JSON output through
the ABI v1 result block; every hook is layered (§2.1). `crates/plugins::HOOK_PREFIXES` is the list of prefixes
the host recognises, and an export outside it is refused at the gate, so a typo is a refusal the model can
read.

### 4.1 `on_tool_call`: before a call runs

```jsonc
// in
{"v":1, "call":{"name":"monitor","arguments":{…}}, "session":{"mode":"allow-edits", …}}
// out, one of
{"pass":true}                                     // run it as it is (the default)
{"arguments":{…}}                                 // run it with these arguments instead
{"refuse":"already watched as pr-24; use that"}   // do not run it; the model reads why
{"answer":"…"}                                    // do not run it; this is its result
```

Runs after the model proposes a call and before the gate, so a rewritten call is still adjudicated. The
tool's NAME is not a field of the answer, and no verdict says *allow*. An `answer` carries one line above its
payload, `[note] answered by plugin <name> (<scope>); the tool did not run`, because a model that cannot tell
a layer's answer from a tool's has been told something untrue. An `answer` is also the one verdict that skips
the gate, so only a built-in or a PINNED layer's is taken: from a session's own or an unpinned layer it is
refused and the call goes on to the gate as though the layer had passed.

### 4.2 `on_tool_result`: what happens after a call

```jsonc
// in
{"v":1, "call":{"name":"bash","arguments":{…}}, "outcome":"ok", "payload":"…"}
// out
{"append":"\n[harness] watching o/r#24 for checks, reviews…",   // to the call's result
 "arm":[{"plugin":"github-pr","args":{"pr":"o/r#24"}}]          // monitors to arm
}
```

Run for every successful tool result, at the site `pr_watch::opened` hooks today. Arming goes through the
operator's own door, so coalescing and the already-watched refusal apply.

### 4.3 `notice`: what the harness says to the model

```jsonc
{"v":1, "kind":"monitor_fired", "text":"[monitor] `pr206-coderabbit` fired: …", "facts":{…}}
→ {"text":"…"}   // or unchanged; or {"drop":true} for a notice this project never wants
```

The hook is resolved per notice kind (`notice_monitor`, `notice_job`, …), so a fix to one firing's wording
does not make a layer the answerer for the queue's verdicts. A notice the model is owed (a settlement, a due
row, a reseat, a restart, a queue verdict or census, the workspace's state, a salvage, the operator's
answer, a parent's message, a model fallback) may be reworded and not dropped; the harness's own six
reminders - a firing, a nudge, the plan-check, a turn's correction, the stall steer, a context claim - may
be dropped, which is `letibot_plugin::notice::MUST_SEE`'s answer and not this page's. `facts` is
typed per kind (`letibot_plugin::notice::Facts`), so a card renders from it instead of parsing the
sentence. A layer's own failure notice never passes through the hook.

### 4.4 `condition`: what a monitor watches

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

The host side is a `PluginCondition: Condition` that runs the declared look the way `GithubCondition` runs
`gh` (spawn at most once per interval, `try_wait`, never stall the poller) and fires on `fired`. The model
arms one with the existing `monitor` tool: `{"plugin":"coderabbit","args":{"pr":"o/r#24"}}`.

An `exec` look runs under the layer's `exec:<program>` capability (checked first: a program the layer did not
declare is not a call anybody adjudicates) and, for a layer nobody pinned, through `Gate::admit_unasked`
without asking, because the look runs on the session's monitor thread and a card would park that thread and
the watch with it. That entry is `admit` stopped at the card:

- admitted by a `permission` rule, a standing grant, the point the session sits at, or a measured boundary
  (D16): the look runs;
- the gate is busy with somebody else's call: the look is *deferred* to the next tick;
- a call that would have gone to a person: it does not run, the layer is told *would need your approval* with
  the card's reason, once per (layer, program, reason);
- a rule the operator wrote: the layer is told *denied*, which is a different fact from *would ask*;
- a session with no gate at all runs nothing, because *no gate* is not *every gate open*.

A look consults no guard model, writes no grant and leaves the consecutive-refusal breaker where it was, so a
watch polling a refused program cannot close the session's own gate.

### 4.5 `digest`: tool output before the model reads it

```jsonc
{"v":1, "call":{…}, "payload":"…"}  →  {"payload":"…shorter…"}  // or unchanged
```

Runs before spill, so the digest is what is budgeted.

### 4.6 `link`: what a word on the screen links to (TUI)

```jsonc
{"v":1, "word":"#212", "repo":{"slug":"o/r","branches":[…]}}  →  {"url":"https://…"}  // or null
```

Called per candidate word, so it is batched per frame (`{"words":[…]}` → `{"urls":[…]}`) and cached by word.

### 4.7 `command`: slash commands

```jsonc
// command_<verb>: out
{"act": {"name": "add_todo", "args": {"content": "a - b"}}}
```

The guest reads the line and answers an act, the host performs it. The head keeps the verbs that MOVE a row
(`done`, `rm`, `postpone`, `resume`, `when`) and offers every other line to the layer (`command_todo`); the
list is one constant (`OWN_VERBS`, `crates/tui/src/app/commands.rs`) beside the match that answers it,
because two lists had `when` missing from one of them.

| the layer | the head |
|---|---|
| `-1` (pass) | answers the line exactly as it did before there was a layer |
| an act it has heard of | performs it; `add_todo` is the only name today |
| any other answer | performs nothing, and says which act it refused |
| a trap, a fuel exhaustion | says so, and the layer below answers |

The trust boundary is the manifest pair, not the answer: `capabilities = []` is what the layer cannot DO and
`actions = ["add_todo"]` is what the HOST will do for it, which is what a reviewer reads.

### 4.8 `tool`: tools the model calls (last)

A plugin tool's schema is prompt bytes in the cached prefix, so plugin tools load only at session start and
at a reseat; calls go through the gate as the built-ins do.

```toml
# plugin.toml
hooks = ["tool_claude_transcript"]
capabilities = ["read:~/.claude/projects"]

[[tools]]
name = "claude_transcript"          # the export is `tool_claude_transcript`
description = "…the prompt's one line…"

[tools.schema]                       # JSON Schema, as a TOML table
type = "object"
```

```jsonc
// in
{"v":1, "name":"claude_transcript", "args":{…}, "feeds":{…}, "looks":[…], "state":…}
// out, a result in the tree's own vocabulary
{"outcome":"ok",       "payload":"…"}
{"outcome":"failed",   "reason":"…", "payload":"…"}
{"outcome":"abstained","reason":"…", "payload":"…"}
{"outcome":"refused",  "reason":"…", "payload":"…"}   // `not_run`: nothing ran
// or a question, when the layer needs data the host has
{"looks":[{"read":"/abs/path","from":12,"lines":40}], "state":{…}}
```

`feeds` is the host's own facts (`home`, `workspace`, `session`, `now_ms`); `state` is the host's to keep
(§1.5). The three looks stream: `read` (a line range, or a list of line numbers), `list` (a directory's
entries with size and mtime), `grep` (every line containing a substring, with its line index, byte offset and
bounded context). The host runs them in order against the layer's capabilities and asks again, up to four
rounds, and every truncation is in the answer (`cut`, `more`, `scanned`).

The prompt is composed in `Harness::open`, before the daemon has a hub, so declarations are read from
`plugin.toml`s alone (`declared_tools`) and the tool is registered after the role is resolved, because
`resolve_role` would refuse a role listing a tool the registry does not hold. A session whose slot is never
filled (a subagent) answers `not_run`. A tool is seated when a layer IN FORCE declares it, and never because
the binary carries it, which is the rule `crates/harnessd/tests/wired.rs` asserts.

Installing a built-in's tool is therefore one file:

```sh
mkdir -p .letibot/plugins/claude-transcript
cp plugins/builtin/claude-transcript/plugin.toml .letibot/plugins/claude-transcript/
```

The module is the one the binary already carries, which `PluginHost::production` puts at the bottom of the
stack for a tool the session declared and for no other reason. A manifest and no module is a DECLARATION and
discovery is silent about it: the layer is on the stack with `sha -` and the module below answers the hook. A
module that is there and will not load is a failure, and so is a manifest that does not parse. A broken layer
costs a tool call, never a session: a trap falls to the layer below, and nonsense, an undefined outcome, a
look for a path it may not read or a fifth round is a `failed` result naming the layer. The class is the
layer's most consequential capability (`exec:` -> `Exec`, `http:` -> `Network`, read-only -> `Read`, which
never reaches the gate), because `access_for` can only narrow.

The first user is `plugins/builtin/claude-transcript` (Claude Code's own store, with `sessions`, `search`,
`read`, `outline`): its layer is `claude-transcript` and its tool `claude_transcript`, because a layer is a
package and this tree spells a package with hyphens while a tool is an identifier the model calls.

### 4.8.1 `read:<path>`: one tree outside the workspace

```toml
capabilities = ["read:~/.claude/projects"]
```

`read` means *the workspace and nothing above it*. `read:<path>` names one tree outside it, as a person wrote
it down: absolute or home-relative (a relative path names a different tree on every machine), strictly deeper
than the root or a home directory (`read:/` and `read:~` are refused), and no `..` segment.

The declaration is the approval, and the check at the moment of a look is on the **resolved** path, so a
symlink inside a declared tree that points out of it (`~/.claude/projects` holds one, `memory` -> a folder
under `~/Projects`) is refused for where it points. A directory of secrets is not under a declared tree, and
no spelling of a path makes it so. One rule, one expansion of `~/`, for a pane's look and a tool's.

### 4.9 `pane`: the UI language

Plugins describe, the host draws: a plugin never paints cells or reads keys, and its document is rendered
with the same rano widgets as the head's own panes. The language is serenedash's: an overview of panels, each
a summary with a full view behind its key; every row through one grid so the numbers line up; every bar says
its denominator; findings collect what tripped and Enter reads one whole; a legend documents every label and
answers on hover; a panel that cannot get its data says why; Esc leaves what you navigated into.

Input (`letibot_plugin::pane::Input`, whose `v` is `V`):

```jsonc
{"v":3,
 "state": null | {…},                                  // what this pane returned last time
 "look":  null | {"exit":0,"stdout":"…","stderr":"…"}, // the result of the look it asked for
 "event": null | {"item":"<row id>","action":"<event>"},
 "feeds": {"sessions":{…}},                            // only the feeds the answering layer declared
 "folded": {"<row id>": true},                         // the host's fold state, by row id
 "below": null | {…},                                  // the layer below's answer, when this layer asked
 "now_ms": 1762000000000,                             // the host's clock, unix millis: a guest has none
 "utc_offset_s": 3600,                                // seconds east of UTC, for the LOCAL reading
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
 "every_s": 0 | <seconds, >= 5>,
 "state": null | {…},
 "input": null | {"key":"project/letibot","placeholder":"say something",    // the composer is the pane's
                    "multiline":true,"footer":"enter sends, alt-enter breaks the line"},
 "keys": [ <ViewKey>… ],                               // the view's own keys, live in cursor mode
 "dock": null | "screen" | "right",                    // beside the conversation, or the screen
 "width": null | 60}                                   // cells a docked column asks for
```

`Panel`:

```jsonc
{"id":"tiers", "key":"t", "title":"tiers",
 "denominator":"rows in the tier",                      // what every bar in it is a share of
 "summary":[ <Block>… ],                                // drawn on the overview (a few lines)
 "view":[ <Block>… ],                                   // drawn when its key is pressed; absent = summary
 "unavailable": null | {"why":"cannot connect to lab2x1","fix":"ssh lab2x1 true"}}
```

`Block` is one of:

```jsonc
{"lines":[ <Line>… ]}                                   // the grid
{"table":{"columns":["…"],"rows":[<Row>…]}}
{"tree":{"rows":[<Row with "children":[<Row>…]>…]}}
{"markdown":"…"}                                        // itemized: list items and headings are stops
{"kv":{"rows":[["label","value"],…]}}
{"stream":{"entries":[ <StreamEntry>… ],"empty":"nothing said yet",          // anchored at the bottom
            "highlight":"<entry id>",
            "mentions":{"@s-1793…":{"text":"@fix-room-push","hue":3}}}}
```

`Line` is the one grid, laid out by the host (label column, right-aligned value column, glyph column, tail;
a label too long is truncated with `…`, never clipped):

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

`Action`: `{"key":"s","label":"start","say":"…"}` (a line to the session's model), `{"key":"r","label":
"refresh","look":true}`, or `{"key":"x","label":"expand","event":"expand"}` (call the pane again with
`{"item","action"}`). `role` is one of `muted`, `working`, `attention`, `success`, `failure`, `plain`,
`strong`, `faint`, `pending`, or a hue - `hue:0` to `hue:7`, a colour by INDEX that the theme maps (`X4`).
A hue is refused outside that range, at the wire: `a_hue_outside_the_palette_does_not_parse`.

Keys are the host's, templated: each panel's `key` toggles its view, `f` findings, `l` legend; up/down and
the wheel move between stops; Enter opens the selected stop's `detail`; Esc leaves what you navigated into and
closes the pane at the overview, and a plugin cannot bind it to anything else. `View.keys` adds the pane's own
shortcuts beside those, and never on top of them.

What `View::validate` and `View::validate_for` refuse, each with a sentence naming the offender
(`crates/sessionlog/src/pane.rs`):

- a panel that has a view and no one-lowercase-letter key;
- a panel key that is one of the host's (`q`, `j`, `k`, `f`, `l`) or that another panel already took;
- an action key that is not one lowercase letter, is one of the host's, is a panel's, or is taken by another
  action of the same stop;
- an action that is not exactly one of `say`, `look`, `event`;
- `every_s` that is neither 0 (never) nor at least 5 seconds;
- a finding with neither an `id` nor a `what`;
- a row's `enter`, or an action's `verb`, that the layer's `plugin.toml` does not list in `actions` - and the
  same for a stream entry's `enter` and a view key's `verb`;
- a view key that is not one letter of either case or one of `ctrl-k`, `end`, `home`, that is a host key or a
  host chord (`esc`, `enter`, `tab`, `ctrl-c`), that a panel or an action already took, that is bound twice, or
  that is not exactly one of `event`, `verb`;
- an `input` with no `key` - the host keys that pane's history and drafts by it and has none to invent;
- a `width` without `dock: "right"`, or one outside 20 to 200 cells;
- a mention in a hue the palette has not got, and a stream entry that asks for a verb with no `id`.

An unknown block kind is not a refusal: it renders as one muted line naming the kind, and unknown fields are
ignored. The view is also data: the same JSON is what an agent reads.

#### Feeds: where a pane's data comes from

A pane never runs a program for its data; it declares which of the host's facts it reads and is handed
exactly those:

```toml
feeds = ["sessions"]        # sessions | queue | room | monitors | jobs | todos | git | github | chat
```

| feed | is |
|---|---|
| `sessions` | the daemon's sessions: id, title, slug, parent, origin, live/running, rows, heads, model, workspace, waits-on-you, paused nags, `project`/`project_name` and `system`, plus `here` and `home` |
| `queue` | the merge queue as the head holds it: `reviews` and `merges` with state, reason and age, `targets` with the branches' health, `leases` with heartbeat and `stale`, `recent`, `not_queued`, `archived`, plus `scope`, `view`, `cursor`, `counts` and `now_ms` |
| `room` | one host's agents' queues per tag: each item with its tag, state, claimant, the age of that claim's beat, its abandonment count and whether it is frozen; the open questions; the arbiter and who is connected; the room's `identity` |
| `monitors` | this session's monitors: armed, last look, fired, what each is watching |
| `jobs` | the session's background jobs: handle, command, state, bytes, tail |
| `todos` | the session's todo rows, with their waits and their questions |
| `git` | the head's reading of the repository: branch, detached, ahead/behind, stash, the action in progress, changed-file counts |
| `github` | the reviewer seat (`letibot attach github`): each scope's state, and its open pull requests with their review state, last action and age; the seat's identity and whether anything is posted or kept as a draft |
| `chat` | the operator's conversations, as `letibot_plugin::pane::Chat`: every channel with its label and unread count, the roster per room, the selected channel, `seen` and `opened_seen`, the failed sends, and a WINDOW of the selected channel's messages (`seq, ts, room, author, kind, to, thread, refs, attachments, text`). Built-in or pinned layers only: `Manifest::admit_for` refuses an unpinned layer that declares it, because it is every room the operator sees, DMs included |

`sessions`, `queue` and `room` are fed; the rest are named, and each is one more feed, not a format change.
Two things a feed says so a plugin need not infer them, both in `queue`: `live` per row (the in-flight states
are the daemon's `review_open`/`merge_open`) and `verbs` per row (which of `approve`/`veto`/`restart`/`rm`
would MOVE it, and why each that would not). Every age is milliseconds against the host's `now_ms`, never a
timestamp, because a guest has no `SystemTime`. A feed adjudicates nothing; declaring it is the whole of the
grant, and a feed a host cannot answer yet is absent from the map rather than empty. A pane gets no `exec`
look today (`look::EXEC_REFUSED`), so a layer that may exec from a condition may not yet exec from a pane.

#### Acts: what a row's Enter does

```toml
actions = ["switch", "fold"]          # switch to a session, fold a row
```

```jsonc
"enter": {"switch":"s-1793…"} | {"fold":true} | {"event":"expand"}
      | {"verb":"approve","id":"r-1f2…"}
```

A view carrying an undeclared act is refused (`View::validate_for`). `{"verb", "id"}` exists because a row
must say WHICH entry it means: the queue pane's keys are the head's (`a` approve, `v` veto, `r` restart, `d`
rm), and before this act a plugin row could draw one entry while the keys acted on whichever entry the host's
cursor was over. Fold state is the host's, by row id, so a fold survives the next answer. The row also
carries `facts` (the right-hand column), `under` (the faint line beneath it), `number` (what `/switch N`
types), `depth` (a flat list that has already walked its own tree) and `open` (`true`/`false`/`"fixed"`, a
row no key can fold).

#### The stream block

`{"stream":{"entries":[…],"empty":"…","highlight":"<id>","mentions":{…}}}` - the one block the host
scrolls itself. It is anchored at the bottom, sticks to the tail, keeps the reader's place when older entries
arrive above, and says `N new - End jumps` while scrolled back. An ENTRY is a stop, so the cursor moves by
message rather than by screen row:

```jsonc
{"id":"m-41", "head":[{"text":"operator","role":"hue:2"},"op"], "time":"14:20",
 "quote":"the line above", "body":"**ping** @s-1793…", "pictures":["ab12"],
 "facts":["edited"], "role":"plain", "actions":[<Action>…], "enter":{…}}
{"divider":"today" | "unread", "text":"Today"}      // a divider between two messages
```

A body is markdown and the host draws it through `rano::markdown`, with the transcript's own options. A
`pictures` entry is a blob ID the host holds or fetches (`capabilities = ["blobs:room"]`), never bytes. And
`mentions` maps a token in a body to what it reads as and which hue it takes, so a layer never edits the
markdown it is handed:

```jsonc
"mentions": {"@s-1793…": {"text":"@fix-room-push", "hue":3}, "@operator": {"me":true}}
```

#### The input surface, and the view's own keys

`View.input` makes the host's composer the pane's: the host edits the text, keeps history and drafts per
`key`, handles paste and the picture chips, and Enter calls the layer's `command_<pane>` hook with
`{line, key, reply_to, chips, feeds, now_ms, utc_offset_s}`. Text never reaches the layer per keystroke. The
layer answers acts (below) or a refusal sentence the host shows.

```jsonc
"input": {"key":"project/letibot", "placeholder":"say something",
           "multiline":true, "footer":"enter sends, alt-enter breaks the line"}
"keys":  [{"key":"R","label":"reply","event":"reply"},
          {"key":"ctrl-k","label":"copy to the end","event":"kill"},
          {"key":"end","label":"tail","event":"tail"}]
```

A view key is one letter of either case or one of `ctrl-k`, `end`, `home`. With an input, keys are live only
in CURSOR MODE - Tab on an empty input enters it, Esc leaves it - because otherwise a letter belongs to the
text. A letter compares case-blind, since the head's `Key::Char` cannot tell `R` from `r`. The event carries
the entry under the cursor as `item`; a `verb` on a view key performs one of the person's verbs on it.

#### Acts in an answer

A `command_` (and an `event`) answer may carry acts instead of a view, each one a host verb the manifest
declares:

```jsonc
{"acts":[{"verb":"room_say","args":{"room":"project/letibot","text":"hi","thread":39}}]}
{"refusal":"that channel is not mine"}                 // shown, and nothing performed
```

`Answer::validate_for` refuses an act whose verb the layer's `actions` does not name, and that is the same
list a row's `enter` is gated by - an act in an answer is a second door to the same verbs, not a lesser one.
The verbs the head performs are the ones the native room view sends today (`room_say`, `room_dm`,
`room_read`, `room_invite`, `room_leave`), plus `copy`, `open`, `pick`. A `command_todo` layer still answers
the older `{"act":{"name","args"}}`, one act performed by name.

#### Completions: the `complete_` hook

`complete_<name>` is a hook kind of its own, declared in `hooks` beside the pane's. Asked on Tab, and on `@`
or `/` at a word start, debounced - never per keystroke:

```jsonc
// in
{"v":1, "text":"@fix", "cursor":4, "key":"project/letibot", "feeds":{…},
 "now_ms":1762000000000, "utc_offset_s":3600}
// out
{"from":1, "to":4, "candidates":[{"insert":"@fix-room-push","label":"fix-room-push",
  "note":"project/letibot","hue":3}]}
```

At most 8 candidates, best first, and the span is byte offsets into the line on character boundaries
(`letibot_plugin::complete::MAX_CANDIDATES`, `Completion::validate`). The host draws the row, cycles on Tab,
accepts on Enter and closes on Esc.

#### The clock, and where the host lays the pane

Every pane, command and completion input carries `now_ms` and `utc_offset_s`, so a layer can print `HH:MM`
today and `Mon D` before that: a guest cannot read a clock, and `ts` on a message is unix millis.

`dock` is where the host puts the pane: `"screen"` (the default, the screen it has always been) or
`"right"`, a column beside the conversation whose divider and width keys the host owns. `width` is a
request in cells, 20 to 200, and only means anything with `dock: "right"`.

#### The answer's shape is versioned

A shape the wire cannot see needs a witness: when the session list became a project's tree no field changed
type, so a layer written for the groups drew a plausible list and nobody was told.

```toml
contract = 1          # the manifest's own version, unchanged
[contracts]
pane_sessions = 3     # the shape of the answer this layer draws
```

`crates/plugins::MOVED` holds the kinds that moved and the version each moved at, and `Manifest::admit`
refuses a layer that declares another version, or none at all, naming the version this host speaks.

#### The feeds a DAEMON-drawn pane is handed

The daemon did not honour feeds: a layer on disk that declared `feeds = ["sessions"]` was handed nothing and
drew an empty pane, which reads on a screen as a broken plugin. `crates/harnessd/src/plugins/feed.rs` is its
half: `PluginHost::with_sessions` takes a `SessionFeed` and `render` builds the feeds the answering layer
declared and only those. Three fields only a host can fill: `now` (the host's clock, unix millis);
`moved_ms` per session, when it last appended a row, because `created_ms` never changes and a turn running
for twenty minutes with nothing appended is stuck; and `repo` per session, the folder of the workspace's git
**common** directory, so a session in a worktree is grouped with the checkout it was cut from.

#### A row's `enter` is performed, and the pane leaves with a `switch`

`App::plugin_enter` reads a row's act first: `switch` goes to that session and the pane closes (a plugin pane
is a screen, not a column beside the conversation), `event` comes back as a `PaneAction`, and a `fold` falls
through to the detail.

#### A pane's own column: hide and resize are the LAYER's

The host hands a pane one rectangle and draws what the view says in it: there is no hide-a-pane key, no
divider and no per-pane width. A pane that wants to be a column keeps a width and a hidden flag in its own
`state` and draws a narrow column or a one-line strip inside its rectangle, which is what
`plugins/builtin/irc` does. The conversation does not reflow, because the host is not laying the column out.

#### The feeds a `command_` hook is handed

Not the same list as a pane's: a command's layer is handed what the head holds at the moment the line is
typed.

```jsonc
// command_todo: in
{"line": "title:\"a\" body:\"b\"",
 "feeds": {
   "todos":      [{"content": "…", "status": "pending", "by": "operator"}],
   "session":    {"id": "s-1793…", "workspace": "/home/dead/Projects/x"},
   "repo_todos": [{"text":"T1 git init","mark":"[x]","indent":4,"item":true}]}}
```

| feed | is |
|---|---|
| `todos` | the session's whole board, both halves, in board order, as `content`, `status` and `by` |
| `session` | where the head is standing: `id` and `workspace` |
| `repo_todos` | the workspace's `TODO.md` as the todos pane parsed it: `text`, `mark`, `indent`, `item`, and **absent, not empty, when the head has not read the file** |

Three decisions: a row's `when` and `needs` are NOT in it, because no act in `actions` can move an edge;
`repo_todos` is absent when the head has not read the file, because `[]` would tell a layer the project's list
is empty; and it is handed whole, not declared, because a pane's `feeds` vocabulary is not a command's. The
missing piece is a command-scoped feed list in `Manifest`.

### Not hooks

Dialect parsers and tokencore (per-token hot path), the store and transcript, the allow side of adjudication,
the merge queue's landing, sudo and VMs.

### 4.10 The second goal: project dashboards

A project dashboard is a project-scope `pane` layer whose look reads the project's own sources of truth. The
first one is `plugins/examples/plan-dashboard` (C5 in `docs/plugins-slice1.md`), reading
`<project>/.letibot/status.json`.

### 4.11 Scripts that accrete, promoted to tools

Every long session grows them: the `ssh … remote.sh state` typed for the tenth time, the five-line `jq` over a
report. They live in the model's memory or in a `bin/` nobody indexes, and the harness can see them, because
every one passed through `bash`.

A script tool is not wasm. It is a descriptor around a command the harness already knows how to run, through
the same gate as `bash`:

```toml
# .letibot/scripts/host-state.toml   (project) - or the session's scripts/ dir (session)
name = "host_state"
purpose = "the state of every campaign host's newest fleet job"
args = { host = "optional; one host instead of all" }
run = ["bin/remote.sh", "-H", "{host}", "state"]   # argv, no shell; or script = "host-state.sh"
replaces = ["ssh * remote.sh * state", "bin/remote.sh -n * state"]   # the shapes it absorbed
```

The same scopes and layering as plugins, and promotion is a commit the queue reviews. One tool for all of
them, so the prompt cache never moves: `script {name, args}`, with the catalog (name, purpose, args) as a
resident notice refreshed when it changes.

Finding them is similarity over what `bash` ran: the harness normalises each command into a shape (argv tokens
with paths, numbers, hashes, hostnames and quoted strings replaced by typed holes), clusters shapes that
repeat (3 times in a session, or in several sessions of a project) and near-duplicates, compares a candidate
with the existing scripts' `replaces` shapes and the project's `bin/`, and treats a call followed by the same
hand-typed fix-up as a candidate for a new version. The butler (§6) owns the promotion, and the shadow gate
applies: the `run` resolves, the descriptor parses, and the recorded commands it claims to replace replay
through it. A script module can be wasm instead, with a synchronous `exec` import gated per call, and a script
that outgrows its descriptor becomes a plugin.

## 5. What the model gets

| call | what it answers |
|---|---|
| `plugin {"action":"scaffold","name","purpose","hook"}` | a new session layer from a template for a hook kind (a crate depending on the SDK, `plugin.toml`, a test) |
| `plugin {"action":"build","name"}` | `cargo build --offline --release --target wasm32-unknown-unknown` in the layer's directory, through the gate; the compiler's words verbatim on a failure. `Access::Exec`, because a build runs build scripts |
| `plugin {"action":"load","name"}` | the shadow gate (§2.4) plus a call of every hook the layer exports on the shadow, with its recorded inputs and a synthetic one. A pass applies it hot; a refusal keeps the old version and says why |
| `plugin {"action":"test","name","input"?}` | one hook called with the input (default: a synthetic first frame of that kind), the answer, the answering layer, the fuel, any trap, and whether the host would accept the answer |
| `plugin {"action":"stack"}` | the layers newest first, the topmost layer per hook, capabilities, `sha256` prefix, disabled layers, recorded inputs per hook, discovery and apply failures |
| `plugin {"action":"disable"/"enable","name"}` | a layer off or on without deleting it; every open pane is re-resolved, gone panes published as `PluginPaneGone` |
| `plugin_hooks [{"hook":"pane"}]` | one kind's input, answer, rules and a worked example **built from the SDK's own types**, so a renamed field breaks compilation rather than a sentence |
| `skill plugin-pane`, `skill plugin-hooks` | the loop as a procedure, compiled in from `skills/` |

The SDK is `letibot-plugin` (`crates/plugin-sdk`): rano's ABI plus serde types for each hook's input and
answer, compiled into the daemon and into the guest from one crate, so a renamed field breaks a compilation on
both sides. Not built: `squash` and `promote` (a copy and a commit today), and `test`'s replay of the whole
session's recorded inputs.

A session layer lives in `<data>/sessions/<id>/plugins/<name>/` (`$XDG_DATA_HOME/letibot`, else
`~/.local/share/letibot`), because `PluginHost::for_session` discovers exactly that directory. It belongs to
the session, so no repository holds it and no other session sees it: a scaffolded plugin is not in force
until `plugin load`, and a loaded layer persists across a restart.

## 6. The harness butler

The harness core should help its agents modify it, through a separate agent: every turn the session's model
spends patching the harness is a turn off the task, and a model reviewing the layer it just wrote reviews
nothing. It is the gatekeeper's shape for the harness
(`crates/harnessd/src/sessions/gatekeeper.rs`): one session the daemon starts, a self-driving session whose
board is the queue, whose reviews are its own `task` children, and which the operator can attach to and steer.

| | review queue | harness butler |
|---|---|---|
| the session | `s-gatekeeper-<workspace key>`, one per daemon, a root | `s-butler-<workspace key>`, one per daemon, a root |
| its board | one row per branch waiting for a verdict | one row per harness request, naming the session it is for |
| its children | reviewers (`Gatekeeper` role) | a fitter that writes the layer into the requesting session's stack, and a reviewer that passes or refuses it, never the same child |
| the daemon's part | fills the queue, worktrees, gates, landing, does not review | records hook inputs, runs the shadow gate, applies layers, enforces capabilities, does not write layers |

One butler, not one per session: it is a peer of the working sessions, so three sessions hitting the same bug
is one row with three requesters. New roles beside `Gatekeeper` and the notes keeper: `Fitter` (`plugin`,
`read` over the recorded hook inputs and letibot's own source, no tool that edits the project) and `Butler`
(the board, `task`, `plugin stack`/`disable`, no authoring). It wakes on the operator's `/butler <what is
wrong>`, on the session's model asking for a change, and on friction the harness sees itself; it writes the
layer through a fitter, submits it to the gate, tells the session one line saying what changed and how to
undo it, reviews every layer through a reviewer child, and proposes what to promote. It never promotes on its
own, and it sleeps until woken, because on a box with one local model server a butler on the same model
competes with the session for the GPU.

## 7. Build order

| step | what | state |
|---|---|---|
| step 1 | the host in harnessd: layer stack, `plugin.toml`, capability checks, fail-down, recorded inputs, the transcript record, the shadow gate, hot reload behind it | built |
| step 2 | the four patch points, the SDK and the `plugin` tool | built, `pane` first |
| step 3 | the GitHub family as built-in plugins: `github-pr` and `coderabbit` as conditions, `pr-create` as `on_tool_result`, deleting `GithubCondition`'s decision half and `pr_watch`'s | left |
| step 4 | the butler and scripts (§4.11) | left |
| step 5 | consolidation: `squash`, `promote`, session discovery over layers | left |
| step 6 | `pane` (§4.9), the example dashboard (§4.10) and WASI (§3.1) | pane built; the gate's rendered-text diff, the example's promotion and WASI left |
| step 7 | a real trial: a stroppy session writes a project plugin, with the operator watching where it gets stuck | |
| step 8 | `digest`, `link` in the TUI, `command` | `command` built for `/todo`; the other two left |
| step 9 | `tool`, at reseat | built |
| step 10 | built-ins compiled in, and the session list as the first one | built |
| step 11 | script layers: a layer that is source text, with no toolchain | not built |

Step 10 decides what a built-in is: one binary is shipped, never a binary and a directory of modules.
`crates/builtins/build.rs` builds `plugins/builtin/*` for `wasm32-unknown-unknown` into `OUT_DIR` (its own
target directory, `--offline`, with the outer `CARGO_TARGET_DIR` and `RUSTFLAGS` removed) and the modules
and manifests become `include_bytes!` / `include_str!` constants; the `Loader` gained a bytes path and a
`builtin` scope sits at the bottom of the stack. The price is the wasm target on every machine that builds
letibot. The session list was the first, and the plan changed in one way: the input a pane is handed is a set
of FEEDS rather than one sessions-shaped input, with the acts declared beside them.

| piece | where |
|---|---|
| the table, the build script | `crates/builtins` (`build.rs`, `table()`, `Stack::register_builtin_bytes`) |
| the guest's ABI half | `plugins/builtin/abi` |
| the built-in layers | `plugins/builtin/*` (sessions, queue, room, chat, todo, irc, claude-transcript, github, review) |
| the bytes path | `Loader::load_bytes` / `load_builtin`, `Layer::trusted` / `Layer::pin` |
| the head's host | `crates/tui/src/app/sessions_pane.rs`: its own stack, the feed, the acts, the fold state, the way out |
| the C3 additions | `crates/plugin-sdk/src/pane.rs`, `crates/sessionlog/src/pane.rs` (`Row`'s `facts`/`under`/`number`/`depth`/`open`/`enter`, `View::validate_for`) |
| the example | `plugins/examples/sessions-override`, a project layer that calls below, renames a group and hides its rows |

Three decisions a reader should see. The session list's pane is answered by the HEAD, not the daemon, because
the list is the way out of a broken session. The way out is the head's own list (`App::session_list`),
unchanged, because the code that draws it today is the code that must keep working the day a layer breaks.
And a built-in is a layer, so a project's `sessions` may `mask` the hook as any layer may; a built-in whose
manifest declares `[[tools]]` is also installed into the daemon's stack, because `tool_` is a hook only the
daemon dispatches.

Step 11 stops on two decisions this plan does not make. Where the engine artifact comes from: no JavaScript
engine is available offline on this box (`Cargo.lock` has none and the offline registry cache holds no
`quickjs`/`rquickjs`/`boa`/`javy`), and `crates/builtins/build.rs` compiles Rust guests with cargo, not C. A
vendored blob compiled into the binary is a BUILT-IN, so §2.5 runs it freely, `exec` included, and the
model's `.js` text becomes the only untrusted thing in the path. And the JS-side contract, including how the
shadow gate's replay stays exact with a clock and `Math.random` inside it. What a script layer's module IS was
decided and built: `plugin.toml` carries `module = "<file>"` (default `plugin.wasm`), one relative path
inside the layer's own directory, refused at admit if it leaves it, so a `.js` layer pins on the script text
and an edit to it un-pins the layer like any rebuild.

## 8. Open questions

- Toolchain on the other machines: `wasm32-unknown-unknown` on lab2x1 and lubuntu1, and inside firecode VMs
  if plugin builds run there.
- Where `build` runs: on the host like `bash`, or in the session's VM when it has one. A build runs build
  scripts, which is the same trust as `cargo build` by the model today.
- Recorded inputs and privacy: how many inputs per hook are kept, and for how long. They hold tool output.
- A child session (`task_start`) inherits the parent's session layers, or only the project's.
