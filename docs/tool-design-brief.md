# Building a tool for this harness

Written 2026-09-10, for the agents building out the tool set from
`docs/tool-survey.md`. Read this first, then §11 of `docs/design-brief.md` and
`crates/tools/src/lib.rs`. Everything here is a rule this repo has already paid for.

---

## 1. The frame: a closed-loop stepper

`docs/closed-loop.md` is the organising metaphor and it is worth reading in full. The
short form:

An **open-loop stepper** commanded 200 steps *probably* takes 200 steps, and when it
stalls under load it loses them **silently** — no error, just accumulated drift. That is
a prompted model: a rule with no feedback path, compliance sampled rather than
guaranteed. Measured here in one session, with every rule loaded in context: a process
check self-matched **five times**, a `grep -c` counted an empty journal and the `0`
travelled to four machines as a measurement, a banner asserted "read-only tools" about a
session that had write tools.

A **closed-loop** system has four parts, and they are not one thing:

| stepper | your tool | catches |
|---|---|---|
| **encoder** | measure the *actual* effect, not the intent | a count over an empty corpus |
| **error signal** | diff intent against effect, feed it back | announced-but-not-done |
| **stall fault** | fail closed, report, never reroute | routing around a guard |
| **current limit** | the capability is not reachable at all | acting outside the boundary |

**The first two are the half that is usually missing.** A tool with a perfect limit and
a dead encoder is safe and still wrong. Instrument before you restrict.

And the payoff runs opposite to how this is usually sold: closed loop exists so the
model can be given *more* room, not less. Feedback is what buys latitude.

## 2. The five rules that decide whether your tool is correct

### 2.1 A miss is self-correcting in the SAME call

Clause 1, and the acceptance case the whole tool runtime was built around. A `grep` that
finds nothing under a scoped path reports **where the term does occur**. An over-anchored
pattern is relaxed to its bare identifier **and the relaxation is reported**. A wrong
tool name comes back with the list and the nearest match.

The model should not need a second call to recover from a near miss — but the rewrite
must be **visible in the result**. Silently improving a query is a rewritten query, and
the model then reasons about an answer to a question it did not ask.

### 2.2 A count does not travel without its denominator

`0` and `0 of 0` are different facts and were reported identically. This cost a night.

```
0                    indistinguishable from "there was nothing to look at"
0 of 0 lines         self-evidently not a measurement
0 of 20,431 lines    a measurement
```

If your tool counts, searches, scans or samples anything, the size of what it examined
travels with the number. **A zero denominator is a failed scope, never an answer about
content.** `grep` now returns `failed` — not `abstained` — when it opened no files,
because an abstention is a claim about content and that call examined none.

Per the survey: **none of the five surveyed harnesses does this.** It is ours.

### 2.3 Three outcomes, never conflated

`ToolOutcome` distinguishes them and `crates/tools/src/result.rs` enforces it:

- **`ok`** — it ran and this is the answer.
- **`abstained`** — it ran, looked, and the thing is not there. A *claim about the world*.
- **`not_run`** — nothing was decided or nothing was attached. **Not a denial.**

`ask_code` returns `not_run` rather than "the corpus does not cover this", because
nothing was queried and saying otherwise would be a claim about a corpus nobody searched.
A subagent whose calls all abstained cannot return `ok`.

### 2.4 Make the mistake inexpressible, do not warn about it

`TokenLedger` does not ask the model to respect the prefix; it makes a violation
*unspellable*. That is the standard. When you catch yourself writing a rule into a tool
description, ask what would make the wrong call impossible to phrase instead.

Corollary: the harness **owns the substrate**, so it knows things the model cannot — its
own pid, the parent chain, the pids of the servers it manages, which files were read this
session. Use that. A check the model could have done itself is the weak version.

### 2.5 A gate that says "allowed" because nothing is wired is worse than no gate

It reads as protection. `Gate::admit` returns `Refuse { outcome: NotRun }` with a
sentence saying *nobody decided* — not `Denied`, which would claim a decision was made.
Fail closed, name which gate stopped it, and make the refusal **grantable and logged**
rather than absent: a refusal that can only be routed around teaches people to route
around refusals.

## 3. Shape rules

- **Declare access honestly.** `Access::{Read, Write, Exec, Network}` on the schema is
  what policy reads. A tool that under-declares is a hole.
- **Bound the output and say when you bounded it.** All five surveyed harnesses cap and
  spill; we are the outlier with `NoBudget` by default. Say `stopped after N; do X to see
  more` and give a way to fetch the rest.
- **Progress is liveness** (§8.5). A tool that can run long emits progress, and progress
  reports *work done*, not that the tool is alive.
- **Never let a component's "I did not do this" be reported upward as success** (F5).
- **The description is for the model, never for the data.** `lint_description` enforces it.
- **Errors carry the fix.** The refusing path supplies what was missing so the retry
  succeeds: `edit`'s read-before-write refusal *hands over the file's contents*, which is
  clause 1 applied to a guard.

## 4. Where the details are

| you need | read |
|---|---|
| the frame, and why | `docs/closed-loop.md` |
| what the other five harnesses do, with file:line | `docs/tool-survey.md` |
| the tool runtime's own rules | `crates/tools/src/lib.rs` module docs |
| outcomes and envelopes | `crates/tools/src/result.rs` |
| the gate and adjudication | `crates/tools/src/adjudicate.rs`, design-brief §11 |
| an exemplary tool | `crates/tools/src/builtins/grep.rs` (the ladder, the denominator) |
| a tool that refuses well | `crates/tools/src/builtins/edit.rs` (read-before-write) |
| the structural layer | `crates/code`, `crates/tools/src/builtins/outline.rs` |
| process lifetime, cgroups, monitors | `TODO.md` T24 |
| memory, passive vs active | `docs/memory.md` |
| model layout and KV costs | `docs/model-topology.md`, `docs/glm-and-dense-attention.md` |

## 5. One open bet, stated so you do not compound it

The survey's sharpest criticism of us: **"a miss produces more output than a hit"** is an
invariant across this whole tool set. Every clause-1 recovery pushes *more* bytes into
context precisely when the model got something wrong. That is a bet on a warm prefix
cache, and **it has not been measured.**

Do not abandon clause 1 — it is why the tools work. But do not let a miss produce an
unbounded reply either: cap the corrective body the way you cap a success body, and if
your tool's miss path can be large, say so in your report.

## 6. Licence boundary

No code from `omo`/`omo-slim` (oh-my-openagent, Sustainable Use Licence) or `crush`
(Functional Source Licence 1.1). Reading an *interface* and reimplementing it is fine and
is what the survey is for; copying an implementation is not.
