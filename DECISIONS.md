# Decisions

Answers to the blocking questions in `docs/workstreams.md`. One entry per decision,
with the date it was settled and what it unblocks. An entry here is the operator's
call; the reasoning under it is mine and may be wrong.

Open decisions stay in this file marked **OPEN** rather than living only in a chat
log, because the workstreams document is written against them.

---

## D1 — where the code lives — **SETTLED 2026-09-09**

`~/Projects/letibot`, on GitHub as `git@github.com:deadtrickster/letibot`, separate
from the llama.cpp fork. Visibility in D9.

Unblocks: the first commit, and every strand's file layout.

Note this is *not* a retreat from "harness and server fused to the 11" (design brief
§3). Fusion is about the seam being a private binary control channel and one release
train — not about one git repository. The server track is off M1's critical path
anyway, so a separate repo costs nothing today, and the coupling that matters is
enforced by the contract tests rather than by directory adjacency.

## D2 — which flowy reader harnessd may hold — **SETTLED 2026-09-09**

Reuse the `claude-lab2x1` seat for tests, so the operator's interactive session sees
the whole chain end to end rather than a mock.

**The constraint this runs into, which the design must respect.** flowy allows many
*writers* under one name but exactly one *reader*: `waiterlock.go` writes a claim and
checks `kill -0`, and a second waiter on the same name is refused by construction —
`inbox.go:132-137` says "IT IS ONE FLAG BECAUSE IT HAS TO BE". The seat brief states
the consequence in fleet terms: two processes listening under one name means the
roster shows that seat attached while the real one hears nothing, "a lie the whole
fleet then acts on".

| operation | shareable? |
|---|---|
| writing as `claude-lab2x1` (`flowy say`) | yes — token-based, no exclusivity |
| holding the inbox reader | **no** — one waiter, enforced |

During a test run, harnessd takes the reader and the interactive session drops its
`flowy-listen-loop` Monitor. That is the right way round for the stated goal: it is
harnessd's own delivery path being exercised, not a second listener watching it.

The two must not overlap. A test harness that starts harnessd without stopping the
Monitor gets `LISTENER REFUSED`, which is the correct outcome and should be asserted
rather than worked around.

## D3 — firecode's two asks — **ANSWERED 2026-09-09, and the answer replaces both**

The operator picked neither yes nor no. Both asks are met by mechanisms that already
exist, which is a better answer than either branch I offered.

### Ask 2 (persistent shell) — **withdrawn. Use byobu.**

Do not ask firecode for `shell --attach`. A byobu/tmux session already provides
everything the ask wanted — a cwd that sticks, exported variables that persist,
background jobs that keep running — and it is a session that attaches to a cgroup, so
its lifetime is expressible in the same terms firecode already uses.

This converges with §13 rather than adding to it: the design brief already requires
multi-head attach over byobu/tmux. The tool runtime's shell and an operator's
attached head become **one mechanism instead of two**.

### Ask 1 (parent cgroup) — **reframed: cgroup membership is use-dependent**

Not one policy. It depends on what the VM is for:

| the VM is | its cgroup |
|---|---|
| temporary — one task, dies with the work | the **session** cgroup, so ending the session reaps it |
| persistent — outlives the session that made it | **not** the session cgroup |

**In both cases it must be clearly reapable.** A persistent VM sitting outside the
session cgroup is not licence to leak it: something must still name it and be able to
kill it, and that owner has to be discoverable rather than implied.

**What this means for W10.** The execution backend takes a lifetime *argument*, not a
lifetime assumption — the caller states whether a run is session-scoped and the
harness places it accordingly. Reapability is then a property to test rather than a
consequence to hope for: for each kind, assert that ending the owner actually removes
it.

## D4 — flowy's licence — **SETTLED 2026-09-09**

MIT. flowy's delivery tests may be ported (`docs/workstreams.md` W2/W12, and the list
in the flowy delivery survey). Attribution retained.

Contrast `oh-my-openagent`, which is under a Sustainable Use Licence: **no code from
it, ported or adapted.** Its ideas may inform a design; its source may not be copied.

## D5 — Falsifier B scoring rubric — **SETTLED 2026-09-09, and the experiment is run**

Rubric: the repo's own kind of work as the task, scored **objectively** — does it
compile, do the supplied tests pass, first attempt, no retries. Tests written by us,
not by the model, so the measurement does not conflate two abilities.

It worked. 160 samples separated cleanly and the result is unambiguous: **no quality
degradation from 20k to 150k** (Fisher p = 1.000), and the only significant contrast
runs *backwards* — an empty context scored worst. See `TODO.md` T7.

The rubric's virtue is that it is re-runnable by someone who was not there, which a
hand-scored rating would not have been.

## D6 — `max_inline_bytes` — **ANSWERED 2026-09-09: it is not a number**

The operator's answer: *configurable, with the ability to plug in a prediction
model.*

So the threshold is not a constant chosen once. The spill policy takes a **decider**,
and a fixed byte count is merely its simplest implementation:

| implementation | when |
|---|---|
| fixed threshold | the default, and what M1 ships |
| per-tool threshold | a `find` and a `read` do not deserve the same budget |
| predicted | a model estimates whether the full output will be needed and spills on that |

`max_inline_bytes` keeps **no default**, so unset remains a genuine no-op rather than
a silent guess. What changes is that §8's contract must express the decision as an
*interface* rather than a comparison against a constant — otherwise the predictor has
nowhere to plug in later, and retrofitting it means touching every tool.

## D7 — the 27B preset — **ANSWERED 2026-09-09: derive it, do not choose it**

Measure rather than pick, the way Qwen Flash was sized on 2026-09-09: load it, get
KV bytes per token from two points that share `n_ctx_slot`, solve for the fixed
footprint, then choose `-c` and `--parallel` from the result. Record the numbers
beside the preset so the next person can check them rather than inherit them.

## D8 — harness licence — **SETTLED 2026-09-09**

Apache-2.0. Confirms what the workspace already declared. The patent grant is the
reason to prefer it over MIT for something that may be published.

## D9 — repository visibility — **SETTLED 2026-09-09**

**Private for now.** `git@github.com:deadtrickster/letibot.git`, ssh remote per the
standing rule against https remotes. Public later is a one-line change; the reverse
is not, which is why it starts closed.

## D10 — cloud-hosted models — **SETTLED 2026-09-09: mode 3 later, seam reserved now**

A token-metered provider API is **not** an M1 target. The seam it will need exists
from today, in `crates/backend`.

Reserving costs one small crate with no implementation behind it. Retrofitting would
mean touching the turn engine, compaction, EXPLAIN and every metric — the same shape
of mistake D6 avoids one level down.

**What the seam carries, and why each part is there:**

- `TurnRequest` holds the **transcript**, not a rendered prompt. Realisation is the
  backend's job — the local one renders, tokenizes and appends to the ledger; a
  provider one converts to that provider's message shape. Passing a rendered string
  across this seam would force every backend through the rendering stack it will
  never use, which is also why the crate depends on `letibot-transcript` and nothing
  else.
- `BackendCaps` states facts, not preferences: `renders_locally`,
  `accepts_token_ids`, `cache_reporting`, `meter`, `prefix`.
- `Meter` distinguishes wall clock from money. Per the operator's observation, our
  **own cloud compute is `WallClock`** — renting the hardware still bills by time —
  so mode 2 is architecturally mode 1 and only a metered API is a different shape.
- `TurnCost.micros_usd` is `Option`, absent under `WallClock` rather than zero,
  because zero is a number somebody will sum into a total.
- `PrefixGuarantee` is the important one. §4.3's claim is that a prefix violation is
  *inexpressible*, and that is a property of submitting token ids over memory we
  own. It does not survive a `messages` API.

**The rule, decided here:** a backend must declare what it cannot guarantee, and the
invariant suites must **skip loudly rather than pass vacuously**. `skip_reason()`
returns a message rather than a bool precisely so that the silent skip is harder to
write than the loud one, and the message says "the check did not run and this is not
a pass". A suite that quietly degrades to "the provider's cache seemed fine" while
showing the same green tick as the structural check is the exact failure this project
exists to remove.

Still open, and deferred with the milestone: which provider to build against first.

## D11 — C4's `f_keep` is `lcp / cached_entry` — **SETTLED 2026-09-09**

§18.2's C4 defined `f_keep` as `cached_tokens / prompt_tokens`, which is the server's
**`f_sim`** under `f_keep`'s name, and then claimed comparability to the 0.000 → 0.999
measurement — which was the server's actual `f_keep`, `lcp / cached_entry`. A bar
measured on one metric was applied to another. See `TODO.md` T22.

**Decision: C4 uses `lcp / cached_entry`.** The denominator is what was cached, so the
metric is *indifferent to how much the conversation grew* — which is the property that
made 0.99 meaningful in the first place.

**It needs no server change, and this is the part worth writing down.** `lcp` is not in
the OpenAI-shaped `usage`, but the denominator is a quantity we already own: the entry
we left in the cache last turn is `prompt_tokens(N) + committed_generated(N)`, straight
off the ledger. And `cached_tokens(N+1)` is the numerator the server already returns.
So:

```
f_keep(N+1) = cached_tokens(N+1) / (prompt_tokens(N) + committed_generated(N))
```

Those are **exactly C3's quantities**. C3 asserts
`cached(N+1) ≥ prompt(N) + generated(N)`; C4 is the **ratio form of the same
inequality**. One measurement, two readings — the assertion and its margin.

Note `committed`, not `predicted`: a trailing stop token is stripped before commit, so
a witness counting predicted tokens is off by one on every turn (T11).

**Consequence:** the M1 run must be re-measured before M1's exit can be judged. The
0.8771 was a correct measurement of the wrong metric.

---

## D12 — a credential is made **usable** inside the boundary, never **readable** — **SETTLED 2026-09-10**

Forced by `docs/boundary-and-adjudication.md` §3 the moment layer 1's mount view existed.
The invariant is *secret bytes may be consumed by a process inside the boundary; they may
never enter the transcript and never leave it*, and the authorised case is real:
`ssh user@host` legitimately reads `~/.ssh/id_rsa`. A project-rooted mount view leaves
that file **absent**, which is the point of the view and also breaks the authorised case.

Three mechanisms were available. Two are rejected:

| mechanism | usable | readable into context | verdict |
|---|---|---|---|
| bind `~/.ssh/id_rsa` into the view | yes | **yes** — one `cat` and it is in the transcript | rejected |
| bind `~/.ssh` read-only | yes | **yes**, and more of it | rejected |
| forward `$SSH_AUTH_SOCK` | yes | **no** — the agent signs, outside the boundary; only signatures cross | **this** |

**Decision: `Grant::AgentSocket`, and no grant that binds a private key.** The key bytes
stay in `ssh-agent`, which is in neither the view nor the namespace. What enters is a unix
socket, bound at a fixed in-sandbox path so the operator's own `/run/user/<uid>/…` path
does not travel either. `cat $SSH_AUTH_SOCK` returns nothing a transcript can use. §3's
invariant is not *enforced* here, it *holds* — which is `docs/tool-design-brief.md` §2.4:
make the mistake inexpressible rather than warn about it.

**And the case with no mechanism is a refusal, not a bind.** A key that is not loaded into
an agent cannot be used from inside the boundary. `Grant::agent_from_env` returns
`ExecError::NoCredentialMechanism` naming what to do — *load it into `ssh-agent`; letibot
forwards the agent, never the key* — because binding the key to get `ssh` working is
exactly the quiet widening that kills a boundary. It also refuses a `$SSH_AUTH_SOCK` that
is not a socket: "usable without being readable" is a property of the socket, not of the
name.

**An authorised `ssh` needs two declarations, not one.** The default network namespace has
no route out, so the second is `Egress::Host { why }` — recorded as
`NsState::SharedByDecision` so that no disclosure can mistake a declared egress for a
boundary that failed. Neither declaration makes a private key readable.

**What is NOT settled by this, and is the gap to say out loud:** an agent-less key, a
GPG/`pass` secret, and a cloud token in a file all still have no mechanism. The general
form — a credential daemon the boundary talks to over a socket — is not built. And §3's
*second* half is untouched: the mount view keeps secret bytes out of the view, but there
is still no single choke point through which every tool result passes, which is
`docs/boundary-and-adjudication.md` §5's open question and not this decision's to close.

Unblocks: seating `bash` behind a boundary at all, and the adjudicator's authorisation
trail having something to authorise that is not a path list.
