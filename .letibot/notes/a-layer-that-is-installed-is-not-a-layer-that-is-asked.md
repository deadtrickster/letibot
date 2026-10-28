<!-- abstract: MEASURED 2026-10-28 (plugin-command-feeds): the built-in `todo` layer was installed and never asked — the head built its plugin stack on the first DRAW of the session list, so `/todo` in a head that never opened the picker had no stack; and `when` was missing from the head's own-verb list, so a layer read `/todo when 1 j7` as a title. Plus the four answers a `command_` hook can give. -->

# A plugin layer that is installed is not a layer that is asked — two ways a seam is silently dead

MEASURED 2026-10-28, branch `agent/plugin-command-feeds`, building the `command_todo` seam (the
head's `/todo` asks a layer for the line; the layer answers an act; the head performs it).

## 1. The host was built by the picker's first DRAW, so a command had no stack

`App.sessions_host` was `None` until `sessions_list` ran (opening the session picker), so `/todo` in a
head that had never pressed that key found `None` and answered in the head's own words. The built-in
`todo` layer was in the binary, in the table, installed at `Host::build` — and never asked. Nothing
was red: every existing `/todo` test passed, because the head's own fallback produces a row too.

The fix is one door: `App::plugin_host()` builds-if-stale and both callers (the picker and the
command) go through it. **The general shape: a lazily-built resource that only one caller builds is a
feature that only works after that caller has run once.** When you add a second caller, give the
resource one door rather than a second `if None { build }`.

## 2. The head's own verbs were a SECOND list, and it was missing `when`

The first cut kept the head's verbs back from the layer with `!matches!(verb, "done" | "rm" |
"postpone" | "resume")` while the `match` below it answered five words — `when` included. With no
host built (finding 1) that was invisible; installing the layer made `/todo when 1 j7` a **row called
`when 1 j7`** instead of a condition on row 1.

Now `OWN_VERBS` is one constant used by the gate, and a test (`app/tests/todo_layer.rs`,
`the_heads_own_verbs_never_reach_a_layer`) drives every word in it at a recording layer and asserts
none is asked. **A "keep these back" predicate beside a `match` is two lists; the test is what keeps
them one.**

## The four answers, and the middle two are the design

| the layer | the head |
|---|---|
| `-1` / `Ok(None)` (pass) | answers the line exactly as it did before there was a layer |
| `{"act":{"name":"add_todo","args":{…}}}` | performs it — the one name it knows |
| any other answer | performs **nothing**, and says which act it refused |
| trap / fuel | says so, and the layer below answers |

The second and third rows are one rule: **a layer that SPOKE is the whole policy for that line**, so an
unknown act must not fall through to the head's own add (which would turn a refusal into a row). Only
`pass` falls through.

## Two smaller things

- **`Host::command` writes the failure sentence into the same `problem` slot the pane uses**, because
  a `Resolved` can be dropped before the caller with a screen reads it. The sentence had to stop
  naming one question ("so the session list is drawn by the head itself" → "so whatever it answers is
  answered by the head itself") once two callers read the slot.
- **A layer that traps and a layer below that answers both produce a notice, and `say` keeps the
  last.** Say the failure AFTER the act, or the operator reads "added to your list" and never learns
  their layer is broken.
