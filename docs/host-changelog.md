# The host changelog — what a setting was, what it is, why, and on whose words

The operator, on a box whose CPU governor had been `performance` for a month:

> *"a month later nobody remembers why the governor is `performance`, and 'the agent changed it'
> is not an answer a person can act on"*

A host's configuration is changed by a `bash` call — a governor, a `vm.swappiness`, a unit's
`LimitNOFILE`, an `fstab` option, a power profile — often with `sudo` where there is a tty. That
is where the doing stays. What was missing is the **record**, and this is it: five facts and a
number, in a file, written at the moment of the change.

## The file

`<config dir>/host-changelog.jsonl` — beside `providers.toml` and `prompts.toml`, which is the
config dir the rest of the box already uses (`$XDG_CONFIG_HOME/letibot`, else
`~/.config/letibot`).

**One JSON object per line, append-only.** Three reasons, and they are the feature's:

* a host's configuration outlives every daemon, every session and every store row, and the config
  dir is the one place that is already the operator's and already survives a store being rebuilt;
* **two writers cannot lose each other's line.** An append is one `write` on a descriptor opened
  `O_APPEND`, so a second session recording a change adds a line rather than rewriting a file the
  first one is also rewriting;
* a record is evidence, and evidence is not edited. **A revert does not rewrite the change it
  undoes** — it appends a line saying so, and the reader folds the two.

### The two lines

```json
{"kind":"change","at_ms":1761700000000,"host":"lab2x1","what":"cpu governor","before":"powersave","after":"performance","why":"the box slept through the test window","words":"make it fast, i don't care about the fans","who":"s-7f3a"}
{"kind":"revert","at_ms":1761700600000,"change":1,"what":"cpu governor","who":"s-7f3a","words":"put it back, the fans are loud"}
```

A `change` carries the five facts (`what`, `before`, `after`, `why`, `words`) and two more: the
`host` the setting is on — this box's name by default, a declared machine's when the change was
made there — and the session (`who`) that recorded it, so a person can go and read that
conversation.

`before` may be empty: *there was no such setting* is a fact, and it is the one a revert needs.
`why` and `words` may not. A record missing either is **refused by name** and nothing is written,
because a record of *what* with no reason is exactly the complaint above, written down.

A `revert` carries the change's number **and its `what`**. The `what` is there so a file somebody
truncated by hand cannot make an old revert silently undo whatever now sits at that number: the
reader reports the mismatch instead of applying it.

### The number is the position

A change's number is its ordinal among the file's `change` lines — 1-based, oldest first — derived
on every read rather than stored. No counter is shared and no lock is taken, so two concurrent
records cannot be handed one number, and the order a reader sees is the order the bytes are in,
which is the only order an append-only file has.

### A line the file cannot read is reported

The reader folds every line it can and collects the rest — a line that does not parse, a revert of
a change that is not there, a revert that names another setting — into a list that `list` prints.
A record is evidence: a reader has to be told a line is there and was not understood, not shown a
tidy list quietly missing one.

## The tool

`host_changelog`, three verbs, one tool — the shape `room` and `notes` have, because the verbs act
on one noun (a line in one file).

**`host_config` is the tool that fills it.** Its `set` reads the current value, applies the
platform's own command and then calls this tool's `record` verb, so the line's `before` is a
measurement rather than a guess, and its reply carries the change's number. `host_changelog` stays
the record and nothing else: a change made by a `bash` call is still recorded here by hand, which
is why `record` is a verb of its own. See [host-config.md](host-config.md).

| verb | takes | answers |
|---|---|---|
| `record` | `what`, `before`, `after`, `why`, `words`, optional `host` | the change's number, and `revert N` as the call that undoes it |
| `list` | optional `limit` (default 20, `0` for every change) | the recent changes, oldest first, each with its age and whether it was reverted |
| `revert` | `change` (the number), optional `words` | the recorded `before`, and the mark |

**`revert` applies nothing.** It hands back the value the setting had and appends the mark; the
seat that made the change undoes it — a `bash` call, `sudo` where there is a tty — and the record
says who asked. A second revert of the same change is refused, naming the first: a change is
undone once, and a value that has to move again is a new change to record.

## The seats

`leticode` — the seat a change is actually made on, and the one `letibot` runs by default. It is
the only seat that carries `host_changelog`, and that is a measured decision rather than a taste:
the tool is `Access::Write`, and `Mode::requires_given` asks for a **writable backend** from any
seat holding a write tool. Seating it on `orchestrator` — the read-only seat, which
`letibot --read-only` and the room's own coordinator session both open on — made twelve harnessd
fixtures refuse to open with *"the `always-ask` mode needs 2 prerequisites … missing: writable
backend"*, and it would have done the same to those two commands. A seat that is by construction
the read-only one does not get a write tool.

Not `coder`, `runner` or `gatekeeper` either: a host setting is changed by a person's session, and
a subagent's words go through the session that spawned it.

`list` narrows to `Access::Read` and the two writing verbs are refused by a backend opened
read-only, naming that gate — so the second gate is real for this tool and not a formality.

## A month later

A person asks the default session *why is the governor `performance`*, or reads the file, and gets:

```
#3 · 34 d ago · lab2x1 · cpu governor: `powersave` → `performance`
    why: the box slept through the test window · words: "make it fast, i don't care about the
    fans" · by s-7f3a
```

— the setting, both values, the problem it solved, whose words it rested on, which session
recorded it, and how long ago. If it was put back, the same line says `REVERTED 2 h ago by
s-9c1b` and carries the words the revert was asked on. Nothing about that answer depends on a
conversation that was compacted, a daemon that restarted, or a store that was rebuilt.
