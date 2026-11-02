# The host changelog - what a setting was, what it is, why, and on whose words

A host's configuration is changed by a `bash` call - a governor, a `vm.swappiness`, a unit's
`LimitNOFILE`, an `fstab` option, a power profile - often with `sudo` where there is a tty.
That is where the doing stays. What was missing is the **record**: five facts and a number,
in a file, written at the moment of the change, because a month later *"the agent changed
it"* is not an answer a person can act on.

## The file

`<config dir>/host-changelog.jsonl`, beside `providers.toml` - in
`$XDG_CONFIG_HOME/letibot` else `~/.config/letibot`.

**One JSON object per line, append-only**, for three reasons: a host's configuration
outlives every daemon, session and store row, and the config dir is already the operator's;
an append is one `write` on an `O_APPEND` descriptor, so two writers cannot lose each
other's line; and a record is evidence, so **a revert does not rewrite the change it
undoes** - it appends a line, and the reader folds the two.

```json
{"kind":"change","at_ms":1761700000000,"host":"lab2x1","what":"cpu governor","before":"powersave","after":"performance","why":"the box slept through the test window","words":"make it fast, i don't care about the fans","who":"s-7f3a"}
{"kind":"revert","at_ms":1761700600000,"change":1,"what":"cpu governor","who":"s-7f3a","words":"put it back, the fans are loud"}
```

A `change` carries the five facts (`what`, `before`, `after`, `why`, `words`) plus the
`host` the setting is on - this box by default, a declared machine's when the change was
made there - and the session (`who`) that recorded it, so a person can go and read that
conversation. `before` may be empty: *there was no such setting* is a fact, and it is the
one a revert needs. `why` and `words` may not - a record missing either is refused, naming
every field that is missing, and nothing is written.

`after` is a **measurement**, never the value somebody asked for: `host_config set` reads
the setting back, and a read that does not answer is recorded as `after: "unread"`. A
reader is then told nothing measured the change, which is true, instead of that the change
took, which nobody knows.

A `revert` carries the change's number **and its `what`**, so a file somebody truncated by
hand cannot make an old revert silently undo whatever now sits at that number. The number
is the change's ordinal among the `change` lines - 1-based, oldest first - derived on every
read rather than stored, so no counter is shared and no lock is taken.

A line the reader cannot fold - it does not parse, it reverts a change that is not there,
it names another setting - is collected and printed by `list`. A reader has to be told a
line is there and was not understood.

## The tool

`host_changelog`, three verbs, one tool - the shape `room` and `notes` have, because the
verbs act on one noun (a line in one file). The schema declares `Access::Write` for the two
writing verbs; `access_for` narrows `list` to `Read`.

| verb | takes | answers |
|---|---|---|
| `record` | `what`, `before`, `after`, `why`, `words`, optional `host` | the change's number, and `revert N` as the call that undoes it |
| `list` | optional `limit` (default 20, `0` for every change) | the recent changes, oldest first, each with its age and whether it was reverted |
| `revert` | `change` (the number), optional `words` | the recorded `before`, and the mark |

**`revert` applies nothing.** It hands back the value the setting had and appends the mark;
the seat that made the change undoes it, and the record says who asked. A second revert of
the same change is refused, naming the first: a change is undone once, and a value that has
to move again is a new change to record.

**`host_config` is the tool that fills it** - its `set` reads the current value, applies the
platform's own command, then calls this tool's `record`, so the line's `before` is a
measurement. `host_changelog` stays the record and nothing else, which is why `record` is a
verb of its own. See [host-config.md](host-config.md).

## The seat

`leticode` only - the seat a change is made on, and the one `letibot` runs by default. It is
`Access::Write`, and `Mode::requires_given` asks for a **writable backend** from any seat
holding a write tool, so seating it on `orchestrator` - the read-only seat `letibot
--read-only` and the room's coordinator open on - makes those opens refuse with *"the
`always-ask` mode needs 2 prerequisites ... missing: writable backend"*. A host setting is
changed by a person's session, so not `coder`, `runner` or `gatekeeper` either.

## What a reader gets

```
#3 · 34 d ago · lab2x1 · cpu governor: `powersave` → `performance`
    why: the box slept through the test window · words: "make it fast, i don't care about
    the fans" · by s-7f3a
```

The setting, both values, the problem it solved, whose words it rested on, which session
recorded it, how long ago - and `REVERTED 2 h ago by s-9c1b` if it was put back. None of
that depends on a conversation that was compacted, a daemon that restarted, or a store that
was rebuilt.
