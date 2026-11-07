---
name: plugin-hooks
description: Fix the harness's own behaviour from inside a session — pick the hook kind that owns the problem (a patch point: notice, on_tool_call, on_tool_result, condition; or a surface: pane, command, tool), write a layer, test it against the input that misbehaved, and load it hot. Use when letibot does the wrong thing with a tool call, a tool result, a sentence it says to the model or a monitor's watch, and when a session needs a screen, a `/` verb or a tool this build has not got.
---

# Patch the harness from inside this session

The operator's complaint is usually one of five shapes, and each is a hook. When it is not, it is
one of three things a layer can **add**: a pane, a `/` verb, or a tool. Eight kinds, and finding the
right one is most of the work; after that it is a small wasm module and no release.

## The five patch points

| what went wrong | the kind | runs |
|---|---|---|
| the model made a call it should not have, or with the wrong arguments | **`on_tool_call`** | before the gate — it may refuse, rewrite or answer the call |
| the gate settles a call this project wants a person to see | **`adjudicate_floor`** | inside the gate, after layer A — it may only raise the tier |
| a call did something the harness should have noticed (a pull request opened and nothing watched it) | **`on_tool_result`** | on a successful result, before the model reads it |
| the harness said something misleading, or left out what the model needed | **`notice_<kind>`**, as in `notice_monitor` | on every sentence the harness injects of that kind |
| a monitor watches the wrong thing, or fires for the wrong reason | **`condition_<name>`** | on every poll of a watch |
| a session's own boundary needs words or a row — it opened, was re-seated, came back after a restart, was closed | **`lifecycle_<event>`** | at that boundary, with the facts of what changed |

The contract for four of these and for a pane is the `plugin_hooks` tool, **read from the host's
own types**: read it before writing anything, and read it again after a refusal.
`plugin_hooks {"hook":"condition"}` is one kind; no argument is all of them. `adjudicate_floor`,
like `tool_` and `command_`, is written out below: the host has no page for it yet.

### `adjudicate_floor` — the gate's floor

```toml
name = "prod-floor"
hooks = ["adjudicate_floor"]
capabilities = []
```

The input is the action as layer A read it — `{"v": 1, "action": {"tool": "bash", "intents": ["network"],
"tier": "may_approve", "program": "ssh", "paths": [], "regions": ["remote:prod-1"], "mode":
"writes allowed", "project": "/work"}}`. The answer is the names you give it and the rung you
require: `{"intents": ["prod-host"], "floor": "ask"}`, where a floor is `auto`, `may_approve`, `ask`
(`always_ask`) or `deny` (`blocked`).

**It can only raise.** Your floor is merged through the strictest tier, so `auto` over a call layer A
already found disclosing changes nothing, and no field admits a call. A `deny` refuses it before the
point is even consulted; an `ask` is an ordinary always-ask card. A trap, a timeout or an answer the
host cannot parse is the **strictest** tier, not a skip — a floor that cannot answer has not said
the call is fine. The names you give (`prod-host`) reach the card and the audit row and decide
nothing; the floor does.

Only a **built-in, user or project** layer answers: never a session's own, which is refused at load
and never asked on the stack. This hook is *reviewed policy* — a project's production hosts, its own
secret stores — so it belongs in the project's `.letibot/plugins` or the operator's `~/.config`, not
in the layer of the session it governs.

## The three surfaces a layer can add

| what you want to add | the kind | the hook | where a head finds it |
|---|---|---|---|
| a screen in the session's pane surface | **`pane_`** | `pane_<name>`, as in `pane_nicks` | `/pane <layer>/<name>` |
| a `/` verb the head asks before it answers the line itself | **`command_`** | `command_<verb>`, as in `command_todo` | the verb, as in `/todo …` |
| a tool in the model's own tool list | **`tool_`** | `tool_<name>`, as in `tool_claude_transcript` | the `tool` list the model is sent |

`plugin_hooks` has no page for `tool_`, `command_` or `complete_`: their shapes are below, and they
are the whole contract.

### `pane_<name>` — a screen

```toml
name = "nicks"
hooks = ["pane_nicks"]
feeds = ["sessions"]        # what the host hands in `feeds`; absent, never empty, when unbuildable
capabilities = []           # a pane that reads a file declares `read` and asks with a look
actions = ["switch"]        # the host verbs its rows may reach; anything else is refused
```

The input is `{"v": 2, "state": …, "look": …, "event": …, "size": {"w","h"}, "feeds": {…}}`; the
answer is a view. Entering a pane and its first frame ask for a look, and the host calls again with
the result — `{"look": {"read": ".letibot/status.json"}}`. That path is relative to the session's
checkout, and the `read` capability is the approval. The `plugin-pane` skill is the whole of this.

### `command_<verb>` — a `/` verb

```toml
name = "todo"
hooks = ["command_todo"]
actions = ["add_todo"]
```

The input is `{"line": "<what was typed after /todo>", "feeds": {…}}` — the whole line, for the
layer to parse. The answer is `-1` to pass, which leaves the line to the head's own verbs, or an act:
`{"act": {"name": "add_todo", "args": {"content": "buy milk"}}}`. The head performs only the act
names its own code knows, so an undeclared name reaches a screen as *nothing was added* rather than
a write.

An answer may carry several acts instead - `{"acts": [{"name": "room_say", "args": {"text": "…"}}]}`
- and each is performed only if `actions` in the manifest declares it. That is also how a pane's own
input surface talks to the host: a `pane_` view with `input: {"key": "host"}` makes the head's
composer the pane's, Enter calls `command_<pane>` with the line, Tab calls `complete_<pane>`, and the
answer's acts are what was typed becoming something. An act the manifest did not declare is refused
and said.

### `complete_<name>` — the candidates under the caret

```toml
name = "chat"
hooks = ["pane_chat", "command_chat", "complete_chat"]
feeds = ["chat"]
```

The input is `{"v", "text", "cursor", "key", "feeds", "now_ms", "utc_offset_s"}`: the whole line,
the caret as a byte offset into it, and the pane's input key. The answer is
`{"from", "to", "candidates": [{"insert", "label", "note", "hue"}]}` — at most eight, best first,
where `from..to` is the span of the line an accepted candidate replaces. Tab asks it and a second Tab
cycles, Enter accepts, Esc closes. Text never reaches the layer per keystroke and the head owns the
editor: a span outside the line, one that splits a character, or more than eight candidates is
refused rather than applied.

### `lifecycle_<event>` — a boundary, not a gate

```toml
name = "what-moved"
hooks = ["lifecycle_reseat"]
actions = ["say"]           # `say`, `add_todo`, `arm`, `room_say` — acts, never a veto
```

The events are `open`, `reseat`, `restart` and `close`, one export each. The input is
`{"v": 1, "event": …, "session": …, "stale": {"prompt": …|"absent", "tools": […]}, "rows": […], "now_ms": …}`
and the answer is acts only: `{"acts": [{"verb": "say", "args": {"text": "…"}}]}`. **A layer cannot
stop the event** — there is no field for it, and the host performs the boundary whatever comes back.
This is where the wording a release used to carry (`WAKE_TAIL`, `RESTART_PROMISES`) lives, and where
*what exactly changed* is written down.

### `tool_<name>` — a tool the model calls

A tool is declared, not implied: the manifest's `[[tools]]` is the seat, and the hook is its name.

```toml
name = "claude-transcript"
hooks = ["tool_claude_transcript"]
capabilities = ["read:~/.claude/projects"]

[[tools]]
name = "claude_transcript"
description = "one line, in the tool list the model reads"
schema = {"type": "object", "properties": {"what": {"type": "string"}}, "required": ["what"]}
```

The input is `{"v": 1, "name": …, "args": …, "feeds": {…}, "looks": [ … ], "state": …}`, and the
answer is either a look to run — `{"looks": [{"list": "…"}, {"grep": "…"}]}` — or a result:
`{"outcome": "ok", "payload": "…"}` (also `failed`, `abstained`, `refused`, with a `reason`). One
call takes at most four rounds and 64 looks. A tool of a layer nobody pinned is adjudicated on every
call instead of running freely.

The guest does no I/O: every file read, grep and listing is a look the **host** runs under the
layer's capabilities, which is why a tool's schema and its `capabilities` are written together.

## The loop

```sh
plugin {"action":"scaffold","name":"one-watch","hook":"on_tool_call",
        "purpose":"refuse a second monitor for a pull request something already watches"}
```

Three files under `<data>/sessions/<this session>/plugins/one-watch/`. `src/lib.rs` holds one pure
function — `answer(&Input) -> Output` — and a native test of it, which is what makes this testable
without a wasm host. The template links `crates/plugin-sdk`, the tree's own contract crate, so the
fields you write are the fields the host parses: a renamed field is a build error rather than a
plugin that answers nothing.

```sh
edit …/one-watch/src/lib.rs            # the logic
plugin {"action":"build","name":"one-watch"}
plugin {"action":"test","name":"one-watch"}    # the answer, the layer, the fuel, any trap
plugin {"action":"load","name":"one-watch"}    # the shadow gate, then hot
```

`test` with no `input` calls the hook with a synthetic input of its kind — the plan's own example
for that hook — and says whether the host would accept the answer. Once the layer has answered real
calls, its recorded inputs are what `load` replays on the shadow, so a change is judged against the
call that went wrong.

A `tool_` layer is the one kind that also needs `plugin {"action":"load"}`, because the tool is
seated when the layer applies: a tool a session has not loaded is not in the model's tool list.

## The rules that catch people

- **`on_tool_call` can never admit anything.** It runs before the gate: a rewritten call is what the
  gate then adjudicates, the tool's name is not yours to change, and there is no field that says
  *allow*. Narrow, rewrite, refuse, answer — that is the whole vocabulary.
- **`adjudicate_floor` can never admit either, and a session cannot write one.** It tightens only,
  and a session layer that declares it is refused — reviewed policy is the project's or the
  operator's.
- **A result hook only sees results that happened** (`ok`, `backgrounded`). A failed call is the
  failure's own words and the model reads them exactly.
- **A notice hook is named after the notice's kind** — `notice_monitor`, `notice_job` — because
  that is how the host asks. A notice the model is owed (a job settling, a due row, a reseat, a
  queue verdict) can be reworded and **not dropped**; a firing and a nudge can be dropped.
- **A condition never runs anything itself.** It declares a look — `{"read": "path"}` under the
  `read` capability, `{"exec": ["gh", …]}` under `exec:gh` — and the host runs it and calls the hook
  again with the answer. It has to say `every_s`, and it should say `max_ttl_s` if the thing it
  watches takes days.
- **A pane, a command and a tool are capabilities too.** A row or an act names a host verb the
  manifest declared (`actions`), a look is checked against `capabilities`, and nothing outside those
  two lists is reachable — not by a row, not by a tool's args, not by a look.
- **No clock, no I/O.** `SystemTime::now()` traps on wasm32; a trap falls to the layer below and is
  reported. Take time from what the look read.

## When it works — and when to stop

`plugin {"action":"stack"}` lists what is in force, newest first, and each hook's answering layer;
`plugin {"action":"disable","name":"one-watch"}` is the bisect for *which of my fixes did this*, and
nothing is deleted. A layer that keeps failing should be disabled rather than piled on: two layers
fixing the same thing is a session whose behaviour nobody can predict.

**Do not use these for anything authoritative.** They can refuse and reword; they cannot admit, they
are not the gate, and the store, the merge queue and the adjudicator are Rust on purpose. When the
fix belongs in the core, say so — a layer is a reproduction of the bug, and a good one.
