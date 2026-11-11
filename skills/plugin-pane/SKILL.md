---
name: plugin-pane
description: Write a letibot plugin pane in the session and make it answer, hot — scaffold, edit the view, build, test, load, and iterate — and draw it so every number has its denominator and every word was measured. Use when the operator asks for a dashboard or panel that letibot does not have, or when a session needs to see something letibot has no pane for.
when_to_use: The operator asks for a dashboard, panel, live view or status display: in this head that is a pane, not a web page or a script serving HTML
keywords: dashboard, panel, pane, live view, status, monitor, sparkline
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

## Drawing a pane worth opening

The host lays out every row, so a pane cannot look wrong. It can still say something wrong, and
that is the failure that matters: a dashboard does not crash, it states a confident sentence the
data cannot support. The language is serenedash's (`docs/plugins-plan.md` §4.9); these are the
rules that language was learned from. A pane is operated, not read: the operator opens it to
answer *is it moving, is anything stuck, is anything wrong*, and closes it.

**Say only what was measured.** Before putting a word in a `tail` or a `role`, name the
measurement that would be false if the word were wrong. `sleeping` on a thread at 60% of a core,
`spilling` on a directory that has not grown in a day, `1 active` above `nothing running` - each
was one read standing in for a rate, or two definitions of a word one line apart. If no
measurement backs the word, the number alone was already saying it.

**One denominator per panel, and name it.** Every `bar.frac` and every `spark` point in a panel is
a share of the panel's `denominator`, and the field says what that is. Not the busiest row, not a
total that appears nowhere, not the rows that happened to fit. A value travels with its base:
`242 of 533 scored`, never `45%` alone. A spark that self-scales draws a flat 260 MB the same
height as a flat 34 GB.

**Stale, zero and absent are three facts.** A file that stopped updating must not look like a
quiet system: take its age from what the look read and say it (`badge`, or a `stale` finding with
the age in `evidence`). An entity with no data is `not started` or `no data`, never `0`. A
judgement over a window says how much window it had, and refuses below a minimum: `findings: []`
means nothing tripped only if something could have.

**Degrade by panel.** A panel whose source is gone returns `unavailable` with `why` and a `fix`
the operator can run. It never draws its shape with zeros in it: `sessions 0` from an empty result
is not a degraded panel, it is a false one. The other panels keep what they can still measure.

**Constant shape.** A panel's line count comes from a budget, not from how many rows the data has
this tick: pad short, count the overflow on the last line (`+7 more`). A frame that changes height
when a job ends has to be re-read from the top every refresh. Put the detail behind the panel's
`key`, in `view`, not in a summary that grows.

**Findings carry the fix.** A finding is what tripped, the evidence with its numbers, what to do,
and a `verify` command the operator can run to check it without trusting the pane. Sort the worst
first. A finding that nobody can clear trains the operator to ignore the list, so a condition
that is normal for some rows (an empty corpus on a target that needs none) is not a finding on
those rows.

**Every label is in the legend.** A label, a role, a column or a tail word the operator might not
know goes in `legend`, which answers on hover and in `l`. Explanations live there once, not
repeated in tails.

**A spark needs a caption.** The tail says what the line means: first and last value, the point
count, and `falling` / `climbing` / `flat` with a threshold above the noise. A trend across a
change in what is measured (the denominator moved) is not a trend: trim the window to the current
population.

**Annotate the numbers that lie at face value.** A rate that is high because the input is
misdeclared, a pass count that includes skips: put the reason beside the number in its `detail`,
dated, pinned to the row. This is the part a generic widget cannot hold.

**Do not say it twice.** The badge, the panel title and the first line are three places; a fact
goes in one. One producer per fact in your `view()`: a count computed twice disagrees with itself
eventually, always in the direction that looks like a different bug.

**Fetch what will be drawn.** A look's output crosses the host on every refresh: read the file the
pane needs, not the directory above it, and cut long text to what a row shows (keep its full length
in the row's `detail` or a count beside it, so a cut value never reads as a short one). Set
`every_s` to how fast the source changes, not as fast as allowed.

Check before `load`: run `test` with a real frame and read the view as the operator will. For each
number ask what it is a share of; for each word, what measured it; for each empty panel, whether it
is empty or blind.

## Saying what you did

The operator learns a pane exists from their panes list, not from your prose. When the loop is
done say three things: what the pane is called (`<plugin>/<hook>`, e.g.
`deploy-status/deploy_status`), what it reads, and its one-line purpose — the last is what
somebody granting it a capability later will read.
