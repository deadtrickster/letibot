# The boundary, and who decides inside it

Written 2026-09-10, from the operator's directions in one sitting. Companion to
`docs/closed-loop.md` (the frame), `docs/tool-survey.md` §"Sandboxing and permissions"
(the prior art), and `docs/tool-design-brief.md` (the rules every tool follows).

The exec substrate exists (`crates/tools/src/exec/`) and says plainly that it **is not a
sandbox**: a spawned command reads the operator's entire filesystem. This document is what
goes around it.

---

## 1. The measured case for a model in the loop

Not a preference. As of 2026-08-14 Claude Code's auto mode is default on paid plans, and
the numbers that decide it are:

| | catches |
|---|---|
| the model classifier | **~83%** of overeager behaviours |
| **a human in the approval loop** | **~67%** — humans miss about a third of dangerous requests |

So the classifier is not a risky substitute for a careful human. It is **better than the
fatigued human clicking yes**, and approval fatigue is the mechanism that makes the human
number so low. The question is not "can a model be trusted with this" but "is it better
than what it replaces", and that is measured.

The other number is the one that keeps it honest: **~17% get through.** Anthropic's own
framing — *auto mode is "one layer of defence-in-depth inside a sandbox, not a substitute
for one."* 2026 produced prompt-injection chains that bypassed restrictive modes. So:
**boundary first, classifier inside it.** A classifier without a boundary is an
open-loop stepper with a good prior.

## 2. The constraint that reshapes everything: authority is contextual

> *"it is ok to use ssh key if i said ssh to that host and it is absolutely out of
> question to read private key or copy it"*

A stateless command classifier **cannot be correct**, because the same command is
authorised or not depending on what the operator just said. `systemctl restart X` after
*"yeah restart"* is the requested action; the identical string unprompted is a decision
nobody made. So the adjudicator's input is **not a command**. It is a command *plus what
authorised it*.

That is why the seam takes a model rather than a table, and it is already the design's
position: `Adjudicator`'s doc says §11.1's three are *"boundary, human, model — three
implementations of this, and the design's job is to make them interchangeable rather than
to rank them"*, and the operator recorded *"it is ok if I plug a model for auto mode"*
(`docs/design-brief.md:159`).

**What follows for the request object.** §11.2's `AdjudicationRequest` carries the action
and its class. It must also carry the **authorisation trail**: the operator's own words
that bear on this action, and their distance in the conversation. An adjudicator that
cannot see *"yeah restart"* will refuse the thing that was asked for, and a seat that
refuses what was asked for gets switched off.

## 3. But some things are not adjudicable, and the rule is about FLOW, not access

The tempting rule is a forbidden-path list. We have one — `NEVER_WRITE` — and it is the
wrong shape twice over.

**First, it is a string check, so it is wrong in both directions.** A `web_search` query
that merely *mentions* `.password-store` is denied (a real false positive, T25/D20), while
a path that reaches the same file by another spelling is not.

**Second, and more important: `ssh` reads the private key.** So "never read `~/.ssh/id_rsa`"
cannot be the rule — it would forbid the authorised case. What separates them is not the
operation on the file, it is **where the bytes end up**:

| | bytes go | verdict |
|---|---|---|
| `ssh user@host` | into `ssh`, consumed inside the boundary, never surfaced | **adjudicable** — allowed with authorisation |
| `cat ~/.ssh/id_rsa` | into a tool result, therefore into the transcript, therefore into the model's context and the store | **inexpressible** |
| `cp ~/.ssh/id_rsa /tmp/k` | to a location with weaker protection, and then anywhere | **inexpressible** |
| `scp ~/.ssh/id_rsa remote:` | off the machine | **inexpressible** |

So the invariant, stated once:

> **Secret bytes may be consumed by a process inside the boundary. They may never enter
> the transcript, and they may never leave the boundary.**

That is a data-flow rule, and it has three properties a path list does not:

1. It permits the authorised case **without an exception**, so nobody has to widen the
   list to get work done — which is how path lists die.
2. It is enforceable at a real edge (what a tool returns, what crosses the network
   namespace) rather than at a spelling.
3. It makes the prohibition **structural**. §2's two tiers are therefore not "high risk"
   and "low risk" but **adjudicable** and **inexpressible** — and no context, no
   classifier verdict and no operator instruction promotes the second tier into the first.
   `docs/tool-design-brief.md` §2.4: make the mistake unspellable.

## 4. The three layers, in build order

**Layer 1 — the boundary. Project-scoped namespaces plus cgroups.**

Half exists: `crates/tools/src/exec/scope.rs` has the cgroup v2 tree, three scopes
(`turn`/`session`/`explicit`), and a child scope's cgroup is a *directory inside* its
parent's so a parent reaps its children by construction. A process joins before `exec`, in
a wrapper that `exit 125`s rather than running unscoped.

Missing: the namespace half. Mount (a project-rooted filesystem view, so the secret is not
merely unreadable but **absent**), PID (so the process cannot see or signal the operator's
daemon or the model server), network (so egress is a decision rather than a default), and
user. Absence beats denial: a file that is not in the mount namespace needs no rule and no
classifier.

**Layer 2 — normalisation. Tree-sitter, and we already have the grammar.**

`crates/code/Cargo.toml` already lists `tree-sitter-bash 0.25`, added for `outline`. No
new dependency.

This matters for a specific reason. Every surveyed harness that gates on a shell command
decides on a **string the shell will reinterpret**, and G1's own predicate checker concedes
it: *"not a shell parser… `eval`, here-docs, aliases, `$(...)` nesting past one level are
unhandled"* — it calls itself a diagnosis, not the mechanism. A decision made on a string
whose meaning does not exist yet is theatre.

Parsing through a **grammar** means the gate and the classifier see the same structure the
shell will execute: which binaries, which arguments, which redirections, which
substitutions, where the data flows. And the honest limit stays honest — a construct the
grammar cannot resolve (a command built at runtime) is **not classified as safe**; it is
reported as unresolvable, which is `not_run`, not `ok`.

**Layer 3 — the adjudicator, with the conversation.**

A small model, resident, seeing the normalised action plus the authorisation trail. Two
mechanisms to take from the prior art:

- **Milliseconds, not seconds.** It sits in the tool path.
- **The consecutive-denial circuit breaker.** Auto mode pauses after **3 consecutive
  denials for the same task direction** and falls back to asking. That is a closed loop
  where the denial rate is the error signal, and it is what stops a classifier from being
  argued down one call at a time.

And the failure mode that must be designed first, because it is the one that converts a
guard into a rubber stamp: **an unavailable or timed-out classifier fails CLOSED.** It
returns `NotRun` — *nobody decided* — never `Denied` (which claims a decision) and never
`Admit`. `NoAdjudicator` already behaves exactly this way and is the pattern.

### Which model

| candidate | weights | co-resident with Flash-Next? |
|---|---|---|
| dense Qwen3.8-27B | 23.6 GiB | **no** — ~22.3 GiB free |
| **Qwen3-Coder-Next Q4_K_M** | **18.5 GiB** | **yes**, tight once KV is counted |

§11.7 recommends the dense 27B, and it is also the falsifier's control model
(`docs/glm-and-dense-attention.md` §4.2). But an adjudicator has to be **always on**, so
the dense model means swapping Flash-Next out and Coder-Next is the one that co-resides.
Different jobs, different answers: the control run wants a swap, the adjudicator wants the
smaller MoE.

## 5. Open

- **Whether the classifier sees the raw conversation or a summary of the authorisation.**
  Raw is more faithful and puts operator text inside a security decision — which is a
  prompt-injection surface pointed at the guard itself.
- **What counts as "the same task direction"** for the circuit breaker.
- **Where the transcript edge is enforced.** §3's invariant needs a single choke point
  through which every tool result passes, and today each tool builds its own body.
- **Whether `NEVER_WRITE` survives at all** once §3 is structural, or becomes a
  belt-and-braces check that no longer decides anything.
- **Nothing here is scheduled.** This is the argument for the shape, not the build.
