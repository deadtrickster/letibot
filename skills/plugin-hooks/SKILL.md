---
name: plugin-hooks
description: Fix the harness's own behaviour from inside a session — pick the hook kind that owns the problem (a patch point: notice, on_tool_call, on_tool_result, condition, digest, link; or a surface: pane, command, tool), write a layer, test it against the input that misbehaved, and load it hot. Use when letibot does the wrong thing with a tool call, a tool result, a sentence it says to the model, a monitor's watch, the bytes the model has to hold or the words a screen could link, and when a session needs a screen, a `/` verb or a tool this build has not got.
---

# Patch the harness from inside this session

The operator's complaint is usually one of five shapes, and each is a hook. When it is not, it is
one of three things a layer can **add**: a pane, a `/` verb, or a tool. Eight kinds, and finding the
right one is most of the work; after that it is a small wasm module and no release.

## The patch points

| what went wrong | the kind | runs |
|---|---|---|
| the model made a call it should not have, or with the wrong arguments | **`on_tool_call`** | before the gate — it may refuse, rewrite or answer the call |
| the gate settles a call this project wants a person to see | **`adjudicate_floor`** | inside the gate, after layer A — it may only raise the tier |
| a call did something the harness should have noticed (a pull request opened and nothing watched it) | **`on_tool_result`** | on a successful result, before the model reads it |
| the harness said something misleading, or left out what the model needed | **`notice_<kind>`**, as in `notice_monitor` | on every sentence the harness injects of that kind |
| a monitor watches the wrong thing, or fires for the wrong reason | **`condition_<name>`** | on every poll of a watch |
| a session's own boundary needs words or a row — it opened, was re-seated, came back after a restart, was closed | **`lifecycle_<event>`** | at that boundary, with the facts of what changed |
| a command printed four thousand lines the model has to hold | **`digest_<name>`**, as in `digest_compilers` | on every output, before the spill policy budgets it |
| a word on the screen names a ticket, a build or a wiki page, and nothing links it | **`link_<name>`**, as in `link_gh` | once per frame, for the words the head's own references did not know |
| a turn ended when the work was unfinished, or ended when it should have gone on | **`steer_`** — `steer_turn` | at every turn's end, with the harness's own detectors as the facts |
| a compaction dropped what this project must not lose | **`compact_`** — `compact_summary` | once per compaction, before the record is written |
| the idle plan-check names the wrong row, or nags at the wrong pace | **`nag_todo`** | on every plan-check the harness is about to deliver |
| a project's review policy (what a reviewer is told, what a verdict must reach) lives in a release | **`review_brief`** and **`review_verdict`** | when a reviewer is started, and when a verdict comes in — both from the queue's stack |
| a child's brief needs a rule of this project's, or its seat a class of tools taken away | **`brief_worktree`** | on a `task_start` child, before it is opened |

The contract for some of these and for a pane is the `plugin_hooks` tool, **read from the host's
own types**: read it before writing anything, and read it again after a refusal.
`plugin_hooks {"hook":"condition"}` is one kind; no argument is all of them. `adjudicate_floor`,
`digest_`, `link_`, `nag_`, `tool_`, `command_`, `steer_` and `compact_` are written out below: the
host has no page for them yet.

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

`plugin_hooks` has no page for `tool_`, `command_`, `complete_`, `digest_`, `link_`, `nag_`, `steer_`
or `compact_`: their shapes are below, and they are the whole contract.

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
layer's capabilities, which is why a tool's schema and its `capabilities` are written together. A
look reads what this session's view holds - a path the boundary hides is refused in the boundary's
words - and an `exec` look (`{"exec": ["gh", "pr", "view", …]}`, under `exec:gh`) is put to the
gate without a card, because it runs inside the model's call and cannot park it behind a question.

### `digest_<name>` — an output, before the budget

```toml
name = "compilers"
hooks = ["digest_compilers"]
capabilities = []
```

The input is `{"v": 1, "call": {"name", "arguments"}, "payload": "…"}` and the answer is
`{"payload": "…shorter…"}`, or `{}` to leave the output alone. It runs **before the spill policy
budgets the output**, so what you hand on is what the model has to hold: a `cargo test` that printed
four hundred lines of warnings and one failure is a digest worth writing.

Two refusals, both about spending context rather than saving it: an answer **longer** than what it
was given is refused, and so is an empty answer for an output that had text in it. Either refusal
leaves the tool's own output standing and says so to the session, which is what happens to a layer
that traps - the model reads the bytes its tool produced.

### `link_<name>` — what a word on the screen links to

```toml
name = "board"
hooks = ["link_gh"]
capabilities = []
```

The input is `{"v": 1, "words": ["#212", "LET-7", …], "repo": {"slug": "owner/repo", "branches": […]}}`
- the words on THIS frame that the head's own references did not know - and the answer is
`{"urls": ["https://…", null, …]}`: **one entry per word, in the order it was asked**, `null` for a
word this layer does not link. It is asked **once per frame**, with all the words at once, and cached
by word, so a word it said nothing about is not asked again.

Only `http` and `https` targets are drawn: a head cannot ask a terminal whether a scheme is one it
should open, so anything else is refused whole - an answer of the wrong length links nothing. A trap
leaves the words the text they were.

### `steer_turn` — the turn's end

```toml
name = "after-a-record"
hooks = ["steer_turn"]
capabilities = []
```

The one export name is `steer_turn` and `Manifest::admit` refuses any other: a turn has one end, so a
second name would sit on the stack and never fire. The input is
`{"v": 1, "reply_tail": "…", "open_rows": […], "native": "…"|
null, "intent": "…"|null, "deliverable": {"uncommitted": […]}|null,
"claim": {"line", "resident", "window"}|null, "stall_rounds": 0, "after": "turn"|"compaction",
"budget": {"continues_left": 1}}` - the harness's own detectors, and `native`, the sentence the
harness itself would have appended (`null` when it was going to end the turn).

The answer is `{"continue": "…sentence…"}` or `{"stop": true}` and one of them. It **replaces**
the harness's decision: a `stop` ends a turn the harness would have nudged, and a `continue` carries
on a turn it would have ended - which is how *keep going after a compaction* becomes a project's own
rule (`"after": "compaction"`). Two things are the host's and not yours: a continuation when
`budget.continues_left` is 0, and any continuation of a turn the operator interrupted. Both are
refused, on the session's log, and the harness's own answer stands.

### `compact_summary` — what a compaction carries

```toml
name = "our-record"
hooks = ["compact_summary"]
capabilities = []
```

The one export name is `compact_summary`. The input is
`{"v": 1, "sections": ["## Objective", …], "candidates": [{"id": "agents_md", "kind":
"agents_md", "tokens": 900, "score": 1}], "budget": {"summary_tokens": 8000, "pinned_tokens":
2000}}` - the headings the harness's own record asks for, and everything this conversation could pin.
The answer is `{"sections": […], "extra_instruction": "…", "pin": ["agents_md", "row:1"],
"after": "continue"|"wait"}`.

`pin` names candidate ids, in the order the pin budget is spent, and the host reads the text and
carries it verbatim. **A pin past `pinned_tokens` is cut and the cut is said** rather than dropped in
silence, and a section or an id the host did not offer is refused: you choose among what it scored,
and never what to keep versus what to lose. `after` is whether the session goes on after the record -
`continue` is what the wall always did. When to compact and how big the record is are not yours.

### `nag_todo` — which row the plan-check names, in what words, when next

```toml
name = "every-free-head"
hooks = ["nag_todo"]
actions = []
```

The input is the board, the rows the check may speak about, the clock, and what the harness's own
check says:
`{"v": 1, "board": [{"id", "content", "status", "by", "waits", "under_review"}…], "eligible": ["…"], "clock": {"since_last_ms", "repeats", "idle_ms", "operator_added_ms", "floor_ms", "cap_ms"}, "jobs": […], "children": […], "now_ms": …, "text": "…", "name": "…"}`.
A row's `id` is its own words. The answer is
`{"name": ["<row>"], "text": "…", "next_ms": 240000}` or `{"quiet": true, "next_ms": …}`, and `{}`
leaves the harness's own check standing - which is what the built-in `b-todo` layer answers.

Three rules the host applies to whatever you answer: **a name outside `eligible` refuses the whole
answer** (half a sentence is a sentence nobody wrote) and the harness's own check is said; **quiet is
overridden** at `cap_ms` and while a queue row is open - a plan may not go unmentioned for the whole
repeat cap; and **`next_ms` is clamped** to `floor_ms`..`cap_ms`, so a layer may not shout louder or go
quieter than the operator's `[nag]` table. Quiet past the cap, and a trap, fall to the native nag: a
broken layer never loses a check.

This is the layer R127 is written as: naming every free head is an answer here (`name` may hold them
all), and the default stays the one row today's daemon names.

### `review_brief` and `review_verdict` — the reviewer's brief and the verdict's floor

```toml
name = "house-rules"
hooks = ["review_brief", "review_verdict"]
capabilities = []
```

A review is somebody else's judgement of a branch, and a project's review policy is a file rather
than a release. Two exports, one subject:

- **`review_brief`** is asked when a reviewer child is about to be started, and again for the review
  queue's OWN prompt. In: `{"v": 1, "entry": {"ask", "branch", "base", "head", "cwd"}, "brief": …}` —
  `entry` is absent for the queue's own prompt. Out: `{"brief": "…", "brief_add": "…",
  "require": ["tests touched for crates/tools", …]}`. `brief` REPLACES the host's policy body (which
  it keeps as the fallback a silent or trapping layer falls to), `brief_add` is appended after it, and
  `require` becomes rules of its own. The facts — the branch, the head, the base, the tree — are the
  host's and a layer cannot move them.
- **`review_verdict`** is asked on every verdict a reviewer hands in, from the same stack. In:
  `{"v": 1, "verdict": "accept"|"reject"|"needs_human", "reasons": […]}`. Out:
  `{"floor": "…", "why": "…"}`. The floor is merged with the verdict through the **stricter** of the
  two rungs, so `accept` over a reviewer's `reject` changes nothing and `reject` over an `accept`
  parks the branch with your `why` on the row. **It can never raise a reject to an accept**: there is
  no field that admits one, and a layer that traps or answers outside the contract has its verdict
  go to a person (`needs_human`) rather than to `accept`.

**Only a built-in, user or project layer answers either hook.** A session's own layer is refused at
`admit_for` and skipped by the stack, pinned or not: the branch under review grading itself is not a
review. That is also why the queue's stack is where these are asked — not the reviewer's, and never
the reviewed session's.

### `brief_worktree` — what a child is told, and how narrowly it is seated

```toml
name = "no-shell-child"
hooks = ["brief_worktree"]
capabilities = []
```

Asked when this session starts a `task_start` child in a worktree. In: `{"v": 1, "worktree":
{"path", "branch", "base"}, "role", "model", "brief"}` — `brief` is the brief the host would send.
Out: `{"brief_add": "…", "seat_drop": ["no-exec"]}`.

`brief_add` is appended after the host's brief, which is not replaced: *the branch is the deliverable*
is a rule a layer that took it away would land nothing under. `seat_drop` names an ACCESS CLASS, one
of `read-only`, `no-write`, `no-exec`, `no-network` — not a tool, because a name this build has no
tool for is a silent no-op and the class is what the gate reads anyway. **It can only narrow**: there
is no field that seats a tool, an answer carrying one (`{"seat_add": ["bash"]}`) is refused by name
and said once, and the drop is applied as a `Downgrade` — a union of denials, which nothing in this
tree can turn into a grant.

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

`scaffold` has a template for `pane`, `notice`, `on_tool_call`, `on_tool_result` and `condition`. For
`digest_`, `link_`, `nag_`, `steer_`, `compact_`, `command_` or `tool_`, scaffold the nearest one and
rename the export in `plugin.toml` and in `src/lib.rs` - the shape above is the whole of what you are
writing.

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
- **A digest saves the model's context, and it is what the session keeps.** It runs before the
  spill, so the digest is the bytes budgeted - spilled to the store when they do not fit - and the
  bytes a transcript row records. It cannot make an output longer and cannot hide one: both are
  refused, and then the tool's own bytes stand.
- **A link answer is one entry per word, in order, and `http`/`https` only.** A shorter or longer
  list would link the wrong word, so it is refused whole rather than zipped.
- **A nag layer chooses, words and paces - it does not widen.** The rows it may name are the
  `eligible` ones the host worked out, its quiet ends at the repeat cap, and its `next_ms` is held
  inside `[nag]`'s bounds. It cannot make the check touch a queue row's nag, which is the queue's.
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
