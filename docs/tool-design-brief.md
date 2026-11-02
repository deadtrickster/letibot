# Building a tool for this harness

For an agent adding to the tool set. Read `docs/tool-survey.md` for the prior art and
`crates/tools/src/lib.rs` for the runtime's own rules. Every rule here is one this repo paid for.

## 1. The frame: a closed-loop stepper

`docs/closed-loop.md` is the organising metaphor. An **open-loop stepper** commanded 200
steps *probably* takes 200, and when it stalls under load it loses them **silently**: no
error, just drift. That is a prompted model, a rule with no feedback path.

A **closed-loop** system has four parts, and they are not one thing:

| stepper | your tool | catches |
|---|---|---|
| **encoder** | measure the *actual* effect, not the intent | a count over an empty corpus |
| **error signal** | diff intent against effect, feed it back | announced-but-not-done |
| **stall fault** | fail closed, report, never reroute | routing around a guard |
| **current limit** | the capability is not reachable at all | acting outside the boundary |

**The first two are the half that is usually missing.** A tool with a perfect limit and a
dead encoder is safe and still wrong: instrument before you restrict. And closed loop exists
so the model can be given *more* room, not less.

## 2. The five rules that decide whether your tool is correct

### 2.1 A miss is self-correcting in the SAME call

Clause 1, and the acceptance case the whole tool runtime was built around. A `grep` that
finds nothing under a scoped path reports where the term does occur elsewhere; an
over-anchored pattern is relaxed to its bare identifier and says so; `glob` names the rung
that matched; `read` on a missing path returns the nearest ancestor's listing and the closest
names in it.

The model should not need a second call to recover from a near miss, but the rewrite must
be **visible in the result**: a silently improved query is a rewritten query, and the model
then reasons about an answer to a question it did not ask.

### 2.2 A count does not travel without its denominator

`0` and `0 of 0` are different facts.

```
0                    indistinguishable from "there was nothing to look at"
0 of 0 lines         self-evidently not a measurement
0 of 20,431 lines    a measurement
```

If your tool counts, searches, scans or samples anything, the size of what it examined
travels with the number. **A zero denominator is a failed scope, never an answer about
content.** `grep` returns `failed`, not `abstained`, when it opened no files: an abstention
is a claim about content, and that call examined none. Per the survey, **none of the five
does this.** It is ours.

### 2.3 The outcomes are not conflated

`ToolOutcome` (`crates/transcript/src/lib.rs`) is the vocabulary and
`crates/tools/src/result.rs` enforces it: `Ok`, `Abstained`, `Failed`, `Denied`, `Timeout`,
`NotRun`, `Backgrounded`. Two distinctions carry the weight. **`Abstained` is a claim about
the world** (it ran, looked, the thing is not there) while `Failed` is not a claim at all.
**`NotRun` is not `Denied`**: nothing was decided, or nothing was attached.

`ask_code` returns `NotRun` rather than "the corpus does not cover this", because nothing
was queried, and `propagate()` makes a subagent whose calls all abstained unable to return
`ok`.

### 2.4 Make the mistake inexpressible, do not warn about it

`TokenLedger` does not ask the model to respect the prefix; it makes a violation
*unspellable*. That is the standard. When you catch yourself writing a rule into a tool
description, ask what would make the wrong call impossible to phrase instead.

Corollary: the harness **owns the substrate**, so it knows what the model cannot - its own
pid, the parent chain, the pids it manages, which files were read this session. A check the
model could have done itself is the weak version.

### 2.5 A gate that says "allowed" because nothing is wired is worse than no gate

It reads as protection. `NoAdjudicator` answers `Unavailable`, and the gate refuses with
`NotRun` and a sentence saying *nobody decided* - not `Denied`, which would claim a decision
was made. Fail closed, name which gate stopped it, and make the refusal grantable.

## 3. Shape rules

- **Declare access honestly.** `Access::{Read, Write, Exec, Network, Session}` on the schema
  is what policy reads. `Session` is a tool that changes the session's own state and nothing
  the operator owns; one that widens capability declares `Write` even if it writes no file.
- **Bound the output and say when you bounded it.** All five surveyed harnesses cap and
  spill; this crate ships `NoBudget` by default, so a cap is something you turn on - and then
  say `stopped after N; do X to see more`, with a way to fetch the rest.
- **Progress is liveness, and never let a component's "I did not do this" be reported
  upward as success** (`docs/implementation-plan.md` §8.5 and F5).
- **The description is for the model, never for the data.** `lint_description` refuses one
  carrying an absolute path, a host or URL, a claim about the data's contents, or a year.
- **Errors carry the fix.** `edit`'s read-before-write refusal names which lines of the file
  this session was shown and the `read` call that fixes it.

## 3b. letibot must work with no flowy, and with nothing else attached either

**A no-flowy mode is kept.** Someone with no fleet, no node and no token gets a working
harness. This is an architectural constraint, not a preference, and it decides designs:

- Anything needing external infrastructure is an **optional attachment**, shaped like
  `builtins::retrieval::Retrieval` and its `Unavailable` default. Not attached is the default
  and is not a fault: the disclosure says what is off and how to turn it on.
- **A capability that needs the attachment degrades to absence, never to a substitute.**
  `NotRun`, naming what is missing. A quiet fallback to a local approximation reported as
  success is the silent-degradation failure, and afterwards nothing is left to reconcile.
- **The local path must be testable with the environment absent:** unset every relevant
  variable, unplug every backend, and the local half still passes.

## 4. Where the details are

| you need | read |
|---|---|
| the five harnesses, and what we took | `docs/tool-survey.md` |
| the gate | `crates/tools/src/adjudicate.rs`, `docs/boundary-and-adjudication.md` |

## 5. One open bet, stated so you do not compound it

The survey's sharpest criticism of us: **"a miss produces more output than a hit"** is an
invariant across this whole tool set, so every clause-1 recovery pushes *more* bytes into
context precisely when the model got something wrong. That is a bet on a warm prefix cache,
and **it has not been measured.** Keep clause 1, but cap the corrective body as you cap a
success body.

## 6. Licence boundary

No code from `omo`/`omo-slim` (oh-my-openagent, Sustainable Use Licence) or `crush`
(Functional Source Licence 1.1). Reading an *interface* and reimplementing it is what the
survey is for; copying code is not.
