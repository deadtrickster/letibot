# Skills

A skill is a directory holding a `SKILL.md`: a name, one line saying what it is for, and a body of
instructions. The prompt never carries a body. It carries the names the operator pinned, and the
model loads a body when it decides it needs one. A skill can also offer itself: the harness notices
that what a session is doing looks like a skill's subject and says so once, in a line that costs the
prompt two lines and disappears next round.

```text
<workspace>/.letibot/skills/latex-presentation/SKILL.md
```

```markdown
---
name: latex-presentation
description: Build a slide deck with beamer, one idea per slide
when_to_use: The operator asks for slides, a deck, a talk or a presentation
keywords: beamer, latex, slides, deck, talk, presentation
allowed-tools: read, write, bash
---

Everything below the second `---` is the body. It is what the model reads when it loads the skill.
```

| frontmatter key | |
|---|---|
| `name` | required. The name every door keys by: a pin, `skill load`, an offer |
| `description` | the one line the listing shows |
| `when_to_use` | optional. The sentence a reader decides by, and the words a ranker needs, since a description is often a title |
| `keywords` | optional. Comma or space separated, either a list or one line |
| `requires` | optional. Capabilities this seat must have for the skill to apply at all. Comma or space separated, or one name a line under the key |
| `unless` | optional. Capabilities whose PRESENCE hides the skill - two skills for one job, and which fits is whichever this seat can do |
| `allowed-tools` | optional. Tool names only. It can narrow what a session may call, never grant anything |
| `origin` | optional. Where an installed skill came from, shown as its label |

A file with no `name`, or with no frontmatter at all, is skipped rather than loaded half way. Keys
this build does not know are ignored. `Bash(git:*)` is read as `bash`: the qualifier is Claude
Code's spelling of a narrower thing and this tree's tools answer to the bare name.

## Where skills are read from

| source | directory | label in a listing |
|---|---|---|
| this project | `<workspace>/.letibot/skills`, then `<workspace>/.claude/skills` | `this project` |
| the operator's | `$XDG_CONFIG_HOME/letibot/skills`, `~/.config/letibot/skills` | `the operator's` |
| installed | `~/.claude/skills` | `installed`, or `installed from <origin>` when the folder names one |
| written by a model | `<data dir>/skills` | `written by a model` |
| this build | compiled in: `plugin-pane`, `plugin-hooks` | `this build` |
| the fabric | another seat's shelf, over the node | `the fabric` |

One name found twice is held once, by the higher row of that table. A directory that cannot be read
is skipped, not fatal, and a session with no skills at all simply has no skills section.

The project half follows the session's workspace, so a child opened in another tree reads that
tree's skills. The box's half travels on the session's config.

## The prompt carries pins, not a catalogue

The prompt's skills section opens with how many skills are pinned out of how many this box holds,
then one line per pinned skill: its name, where it was found, and its description. Everything
unpinned is a count, `and N more that skill list holds`. No source is ever listed in full: a box
with two hundred installed skills would spend the prompt's first screen on names nobody asked for.

The caps are 60 rows, 4000 bytes and 800 tokens of the session's own counter. With pins only they
are rarely reached; the count line says what was left out.

The rest is reached by asking: `skill list` is every skill, `skill find` with a query loads the
nearest one and names the next two.

## `[skills] pin`

```toml
# leticode.toml, in the project, or ~/.config/letibot/leticode.toml
[skills]
pin = ["latex-presentation", "release"]
```

**Both levels apply.** The operator's names come first, then the project's new ones, each once: a
pin is a request to have a skill in hand, so a project file, which a session with `write` can edit,
must not be able to unsay what the operator pinned for every project on the box.
`--no-project-config` leaves only the operator's list.

A pin is not a load. The body still arrives only when something asks for it, so pinning a large
skill costs one line of prompt, not its text.

A pin may name a skill, or a shelf row by the id a listing leads with. A pin that answers to nothing
is the operator's typo and is said twice, so silence never reads as a pin that took effect: once as
a session warning at open, and once in the prompt's own skills section.

```text
a pinned skill nothing on this box answers to: `latex-pres`. `[skills] pin` takes the names
`skill list` reports; the rest of the pins are in force.
```

The warning's code is `skills_pin`. A key this build does not know inside `[skills]` is refused at
load, by name.

## Conditional skills

Two skills can answer one job and only one of them can be done on a given seat. A skill says so
itself:

```markdown
---
name: instrument-dashboard
description: the cluster's state as a page anyone can open
requires: flowy
---

…and the skill that fits a seat with no fabric says `unless: flowy`.
```

A capability is a **tool this session really has** (`bash`, `flowy`, `read`), or an `mcp__server` /
`mcp__server__*`, which is any tool of that server. A name whose door has nothing behind it is not
a capability: `flowy` is seated as a door on every root session so a `/flowy login` has somewhere to
land, and a seat only *has* it when the daemon gave it a fabric seat. That is read at the session's
open, so a session that logged in to the fabric afterwards keeps the answer it opened with - the
registry is the daemon's and the seat is the session's, and the listing follows the session.
Everything else is the tool name, exactly.

`requires` and `unless` are checked for **every** source, a built-in included - the build's own
skills are not exempt from a seat they cannot use. The older `mcp__`/connector half, which reads a
skill's body for a server its author guessed at, still skips built-ins.

The operator can say it for any skill, over anything a file claims:

```toml
[skills.when.instrument-dashboard]
requires = ["flowy"]

[skills.when.panel]
unless = ["flowy"]
```

A table for a name **replaces that skill's own frontmatter whole** - `requires` and `unless`
together - because an operator who has the seat knows and the author guessed. Both levels may write
tables: the project's apply, and when both name one skill the operator's is the one in force, whole.
A project file can make a skill fit this project less, and cannot unhide what the operator hid. (A
pin unions, for the other reason: a pin is a request, and a project must not unsay one.)

One rule, four doors. A skill this seat cannot use is

- **not in the `/skills` pane's listing**,
- **not offered** - and the skip is logged in `skills-offers.jsonl` with the reason, so a skill that
  never gets offered is measured rather than guessed at,
- **not what `skill find` answers**, even when it scores highest, and
- **refused by `skill load`**, in the sentence the listing shows it under: *requires `flowy`, which
  this seat does not have*, or *hidden while `flowy` is on this seat*.

`skill list` still names it, under `not for this seat`, with the reason - a model that silently
loses a skill from a listing cannot tell a rule from a bug. The operator's own `/skills NAME` still
hands the body over: that door is theirs, and the condition is an author's or their own file's
opinion rather than a permission.

## The offer

Every round, before the request is sent, the harness scores the skills this box holds against a
subject: the plan's rows (the ones the model wrote, not the queue's), the person's latest prompt,
and the model's latest text and latest reasoning, read from the newest end until 8000 characters
are used. The plan is taken first and outside that budget, because the work in hand is what a long
tool result would otherwise cut away.

The person's own newest row is also scored **on its own**, against each skill's `keywords` and
nothing else: at the tail of a session somebody writes *add it as a pane*, and against a
description of several hundred terms that line is a rounding error. One word a skill publishes as
the thing it answers to is enough on its own.

A skill is offered when either score reaches 0.18. At most two are offered per round, ranked by
the higher of the two, and a skill whose body is already in the conversation is not offered at all.
An offer nobody took may be earned once more by a **new** operator prompt that names it - asking
again is evidence, the same words on the next round are not - and an offer that was loaded is
never made again. The offer is a line at the tail of the request, never a row in the session log,
so it is composed for one round and gone from the next. It says the score, which score it was, and
the one verb that loads the body:

```text
[skill offer - the harness's own line, not the operator's] `latex-presentation` looks relevant to
what you are doing now (similarity 0.42 of 1.00, lexical over its frontmatter: description,
when-to-use and keywords). Its body is not in this prompt. If it is worth the context, load it -
`skill` action="load", name="latex-presentation". This line is composed for this round and is gone
from the next one; the skill, once loaded, is not.
```

Nothing has to be switched on. The score is lexical over the skill's whole frontmatter, so a subject
reaches a skill by its description, its `when_to_use` and its keywords, not its body - and a prompt
earned offer says it was scored over the keywords alone. With [embeddings](embeddings.md) on, the
subject's score is blended with meaning and the line says so.

What was offered and whether it was taken is logged once per offer, so the floor can be tuned from
measurement rather than by guess:

```text
<logs root>/<session id>/skills-offers.jsonl
{"at":1767225600,"session":"s-...","skill":"latex-presentation","score":0.421,"by":"subject","taken":true}
```

`taken` is true when the round that was offered the skill loaded it. `by` is `subject` or `prompt`:
the two scores are different measurements, and a ratio that added them would answer neither. The log
is also the session's memory of what it has been offered, so the rule survives a restart.

## Handing a skill over

- **`/skills`** opens the pane of everything this box holds, one row each. `enter`, or
  `/skills NAME`, hands that skill's body to the session as the operator's message;
  `/skills NAME and also fix the build` sends the body and then that line as the prompt.
- **`task { "skills": ["release"] }`** starts a child with those bodies already in its prompt, on
  top of the index every session gets. A name nothing answers to is refused before anything is
  spawned, with the names that do.

## What it says when it fails

- `skill` with no action: `skill needs an action`, and the three actions.
- A name nothing answers to: `no skill named `x``, then where it looked: `no skill named `x` on
  disk, and no shelf is attached (the daemon holds no seat, so the fabric's skills are not
  reachable from here). On disk: <the names there are>`. A shelf that is away is reported as the
  shelf's own words, never as an empty shelf.
- `find` with nothing close: `nothing in the skills index is close to `q` (the nearest is `x` at
  0.10, under 0.15)`. Below the floor the body is not loaded at all, because a weak match spends
  the model's context on a page about something else.
- A bad pin: the `skills_pin` warning above, and one line in the prompt.

See also [cookbook.md](cookbook.md) for the fabric shelf, [embeddings.md](embeddings.md) for the
second similarity tier, and [plugins-plan.md](plugins-plan.md) for the two skills this build
carries.
