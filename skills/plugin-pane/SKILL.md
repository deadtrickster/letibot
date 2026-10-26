---
name: plugin-pane
description: Write a letibot plugin pane in the session and make it answer, hot — scaffold, edit the view, build, test, load, and iterate. Use when the operator asks for a dashboard or panel that letibot does not have, or when a session needs to see something letibot has no pane for.
---

# Write a pane plugin in this session

A pane is a wasm module that answers one JSON document: panels, keys, findings, a legend, a
look and a refresh interval. The host draws it with the same widgets and the same keys as its
own panes, so a plugin cannot look or behave differently from the rest of the head. The whole
contract is the `plugin_hooks` tool, read from the host's own types — read it before writing
anything, and read it again after a refusal.

You are building this **in the session, at run time**: no release, no restart, no branch. It is
the point of the plugin plan — *"a fix in the middle of a session, built and answering within
the turn"*.

## When this is the right tool

- The operator wants to *see* something that no existing pane shows (a deploy queue, a plan, a
  fleet's state, the state of a long campaign).
- The operator wants a harness behaviour changed and a pane is the visible half.
- **Not** for anything that must be authoritative: a pane is a view over data somebody else
  writes. It never writes, never runs a program, and never decides anything.

## The loop

```sh
# 1. a source, in this session's own plugin directory
plugin {"action":"scaffold", "name":"deploy-status", "hook":"pane",
        "purpose":"the deploy queue for each environment, and what is waiting"}
```

Read the three files it wrote. `src/lib.rs` is where the work is: `view(input: &Value) -> Value`
is a pure function, and its native test is already there — keep it passing, and extend it as the
view grows. `plugin.toml` declares the hook and the capabilities (none by default).

```sh
# 2. the logic — a normal Rust edit, in the session
edit   …/sessions/<this session>/plugins/deploy-status/src/lib.rs
```

Three rules that are re-learned painfully every time:

- **No I/O.** A plugin never opens a file. It returns `"look": {"read": "path"}` in its answer
  and the host reads the file and calls the pane again with `look: {"exit":0,"stdout":…}`. That
  needs `capabilities = ["read"]` in `plugin.toml`. An `exec` look is refused by this slice.
- **No clock.** `SystemTime::now()` panics on wasm32 and a panic is a trap: the head draws an
  unavailable panel instead of your view. Take time from what the look read, or from `state`.
- **The keys are the host's.** `q`, `j`, `k`, `f`, `l`, ↑/↓, PgUp/PgDn, Enter, Esc. A panel key
  opens its `view`; an action key does the one thing it names; a collision is refused as a whole.

```sh
# 3. compile it for the host
plugin {"action":"build", "name":"deploy-status"}
```

The compiler's errors come back verbatim — fix them and build again. `--offline`, `--release`,
`wasm32-unknown-unknown`, and the artifact becomes `plugin.wasm`.

```sh
# 4. ask it straight away, before anything can see it
plugin {"action":"test", "name":"deploy-status"}
```

`test` shows the view it answered, the layer that answered, the fuel it burned and any trap. Its
input defaults to a synthetic first frame; to check a real frame, pass one:

```json
{"v":1, "state":null,
 "look":{"exit":0,"stdout":"<the file's text>","stderr":""},
 "event":null, "size":{"w":100,"h":30}}
```

If the view is wrong, you are one `edit` + `build` + `test` away from right. That is the loop.

```sh
# 5. make it answer, hot
plugin {"action":"load", "name":"deploy-status"}
```

`load` rebuilds from the source, compiles it, **runs it on a shadow copy of this session's
stack** — every input this hook has been asked, plus a synthetic first frame — checks the view
validates and that a trap or a bad fuel cost stops it, and only then swaps it in. Nothing on a
screen changes if it fails, and the refusal says what the gate saw. On a pass the pane is
announced and drawn: the operator's panes list now has it.

```sh
# 6. iterate: edit → build → load, and the pane changes under the reader
plugin {"action":"stack"}
plugin {"action":"disable", "name":"s1"}     # the bisect: which of my fixes broke this
plugin {"action":"enable",  "name":"s1"}
```

**A session plugin reaches nothing outside this session.** The layer lives under the session's
own data directory and is discovered at session scope; it is in no repository, it is not
committed, and no other session sees it. A plugin's capability is the only thing that touches
the world at all — `read` is the one this slice grants, and an `exec` look is refused with a
sentence saying so. Promoting a pane to the project is a separate act: copy the directory into
`.letibot/plugins/` on the session's branch and let the merge queue review it like any other
code.

## Saying what you did

The operator learns a pane exists from their panes list, not from your prose. When the loop is
done say three things: what the pane is called (`<plugin>/<hook>`, e.g.
`deploy-status/deploy_status`), what it reads, and its one-line purpose — the last is what
somebody granting it a capability later will read.
