---
name: plugin-hooks
description: Fix the harness's own behaviour from inside a session — pick one of the four patch points (notice, on_tool_call, on_tool_result, condition), write a layer, test it against the input that misbehaved, and load it hot. Use when letibot does the wrong thing with a tool call, a tool result, a sentence it says to the model, or a monitor's watch.
---

# Patch the harness from inside this session

The operator's complaint is usually one of four shapes, and each is a hook. Finding the right one
is most of the work; after that it is a small wasm module and no release.

| what went wrong | the kind | runs |
|---|---|---|
| the model made a call it should not have, or with the wrong arguments | **`on_tool_call`** | before the gate — it may refuse, rewrite or answer the call |
| a call did something the harness should have noticed (a pull request opened and nothing watched it) | **`on_tool_result`** | on a successful result, before the model reads it |
| the harness said something misleading, or left out what the model needed | **`notice`** | on every sentence the harness injects |
| a monitor watches the wrong thing, or fires for the wrong reason | **`condition`** | on every poll of a watch |

The contract for each is the `plugin_hooks` tool, **read from the host's own types**: read it
before writing anything, and read it again after a refusal. `plugin_hooks {"hook":"condition"}` is
one kind; no argument is all of them.

You are doing this **in the session, at run time**: no release, no restart, no branch.

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

## The rules that catch people

- **`on_tool_call` can never admit anything.** It runs before the gate: a rewritten call is what the
  gate then adjudicates, the tool's name is not yours to change, and there is no field that says
  *allow*. Narrow, rewrite, refuse, answer — that is the whole vocabulary.
- **A result hook only sees results that happened** (`ok`, `backgrounded`). A failed call is the
  failure's own words and the model reads them exactly.
- **A notice hook is named after the notice's kind** — `notice_monitor`, `notice_job` — because
  that is how the host asks. A notice the model is owed (a job settling, a due row, a reseat, a
  queue verdict) can be reworded and **not dropped**; a firing and a nudge can be dropped.
- **A condition never runs anything itself.** It declares a look — `{"read": "path"}` under the
  `read` capability, `{"exec": ["gh", …]}` under `exec:gh` — and the host runs it and calls the hook
  again with the answer. It has to say `every_s`, and it should say `max_ttl_s` if the thing it
  watches takes days.
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
