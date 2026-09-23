# Compaction as structural eviction

Written 2026-09-09. Replaces the "ask for a summary" model in §10 with one that keeps
what it drops. The reasoning is the operator's; the measurements are this box's.

Companion: `docs/memory.md` (passive vs active memory, the suggestion mechanism).

---

## 1. Unwinding: why does compaction happen at all?

Five reasons are usually given. They are not equally real.

| # | claimed reason | status |
|---|---|---|
| 1 | the context window is a hard wall | **forced** — the only one |
| 2 | tokens cost money | **mode-dependent** — true on a metered API, false locally (T9) |
| 3 | quality degrades with depth | **measured false** — see below |
| 4 | decode slows as KV grows | real |
| 5 | KV memory pressure across sessions | real — this is the OOM we spent a day on |

**Reason 3 is the one everybody assumes, and Falsifier B killed it.** 160 samples,
scored on compile-and-tests-pass: 0.829 at zero depth, 0.875 at 20k, **1.000 at 60k**,
0.900 at 150k. Fisher exact between 20k and 150k: **p = 1.000**. The only significant
contrast was 0k against 60k and its sign runs *backwards* — the shallowest condition
was the worst. Depth did not hurt; the absence of a conversation did.

So compaction is driven by the wall and by resources. **Not by quality.** The trigger
is `n_ctx` and memory pressure, and the right local policy is *compact when you must,
as late as possible*.

## 2. What it costs today

**The strategy these numbers are of, because there is more than one and this section did
not say.** `flatten`: the conversation is collapsed into one user message with `system: []`
and `tools: {}`, a summary is asked for, the history is replaced, and the conversation
continues. Measured on opencode at `1ee74df`, on this box:

- **144,436 tokens, cache hit 0, ~13 minutes** to the first summary token.
- **The next real turn is *also* cold, because the prefix is now entirely different.**
  This half still holds, and it is the half the design turns on.
- Detail is destroyed irreversibly.
- And Falsifier B found a third cost nobody counts: a short context made this model
  reason **~4× longer** — median 6,468 generated tokens against ~1,400 — with every
  truncation-by-length in the whole run occurring at zero depth. **Compaction can cost
  tokens on the far side too.**

**For the SUMMARISING CALL the first bullet no longer holds upstream, and this section
would be read as though it did.** opencode now has a second strategy, `strategy: "cached"`
(`4efae6c`), whose own note says the conversation *"is already present as real messages
(re-sent byte-identical to the previous turn), so only the instruction and template are
appended — nothing is restated"*. Read on 2026-09-23 at `39ee69b`, which is the commit that
made the strategy *"actually fire"* — before it, `lastRequest` was captured in a per-prompt
scope and was always undefined at compaction time, so the option existed and never ran.
On that strategy the summarising call pays for the instruction, not for the history.

**Which of opencode's two `select` budgets is live** — because a number quoted from their
compaction is only meaningful with this settled, and it was not, until now:
`packages/opencode/src/session/compaction.ts` is the one the running build reaches, and its
budget is `clamp(2_000, 15_000, floor(usable × 0.25))`. `packages/core`'s
`DEFAULT_KEEP_TOKENS = 8_000` is reached only by core's own `SessionRunnerLLM`, which nothing
in `packages/opencode/src`, `packages/cli/src` or `packages/tui/src` calls. The authoritative
statement is upstream's, in the commit that wired the alternative in (`4efae6c`):

> *"1ee74df added compaction.strategy = "cached" but only to core's compactAfterOverflow,
> **which the running build never reaches. The CLI reaches compaction through
> packages/opencode/src/session/compaction.ts.**"*

The operator's own observation of a session going from about 1m to 20k tokens is consistent
with that budget and not with the other (`4,096` of summary output plus `≤15,000` of
untouched recent turns) — but it is their observation of a running session, not a figure from
this reading, and it is recorded as such.

## 3. The conflation at the heart of it

Compaction bundles two operations that are not the same thing:

- **Reducing what is resident** — necessary, driven by the wall and by memory.
- **Destroying what is recoverable** — an implementation accident. Nobody asked for it.

Reasons 1, 4 and 5 demand only the first. Summarisation delivers both.

### The uncomfortable part, which rules out the easy fix

**Plain eviction does not help the next turn either.** Dropping old turns invalidates the
prefix *by definition*, because the prefix **is** the old turns. Summarise, prune or delete —
any reduction costs a full re-prefill **of the turn that follows it**.

**The scope of that claim is the turn and not the summary, and the difference is the whole
of §2's correction.** *"There is no cheap version, and a design that claims one has not
understood the prefix cache"* was written against a measurement of the summarising call and
is an overclaim: on opencode's `cached` strategy the summarising call **is** cheap — it
re-sends the previous request byte-identical and appends the instruction — and that is a
design which understood the prefix cache better than this sentence did. What cannot be cheap
is the moment the history is actually replaced. So the sentence becomes: **the reduction is
what costs, and it costs the next turn.**

That is still enough to rule out the easy fix, and it is now stated where the cost lands
rather than as a blanket claim. The mechanism this document argues for (§4) is an answer to
the next turn's re-prefill, not to the summary's.

There is also a reason summarisation is used rather than deletion: a plain drop leaves
a hole. References to removed turns dangle. **Something must stand where the evicted
span was.**

So the question is not "how do we avoid the cost" but **"we are paying a re-prefill
either way — what should we get for it?"**

## 4. The answer: a zoomable map, not a summary

Replace an evicted span with a **structured, addressable index** of what it contained,
rather than prose about it.

### Why structural beats prose

Most of a conversation is **already typed and we throw the schema away**. Tool calls
have names, arguments and a closed `ToolOutcome` vocabulary. `read src/foo.rs:1-200`,
`grep X → 3 hits in Y`, `cargo test → 3 failures`, `decided A because B` are *events*,
not sentences. A prose summary flattens them into English; a map keeps them as nodes
with identities.

The model is then needed only for the genuinely prose parts — reasoning and assistant
text — which is a far smaller job than summarising a conversation.

### Maintained incrementally, which is what removes the stall

**"Follow and do the diff":** build the map turn by turn, each turn contributing its
nodes as it completes. Then evicting a span costs *rendering nodes you already have at
a coarser zoom* — there is no summarisation call at compaction time, or a tiny one.

This is the leveled-compaction argument from design brief §1, which said incremental
merging *"does not make the 13-minute stall faster — it removes the category"*, and
which was missing its data structure. The map is that structure.

### The zoom levels are the ladder's rungs

We already built this ladder once, for KV, in llama.cpp: `full → thinned → skeletal →
no-draft → spill`, degrading before evicting, each rung shedding something that costs
*time on restore* and never correctness. The same shape applies here:

```
full text  →  node with children  →  node with label  →  handle only
```

**Degrade, do not drop.** A span loses fidelity by steps and stays addressable at
every rung.

### The constraint that must not be violated

**Zooming in cannot rewrite the map in place.** Editing anything already in the prefix
invalidates it — the same trap as compaction itself. So:

| | where it lives | cost |
|---|---|---|
| the map | written once at eviction, in the prefix | its own tokens, permanently |
| a zoom-in | **appended** via `recall(node_id)` | a tool result |
| the detail | in the store, addressed by node id | nothing until pulled |

The map is a fixed index; recalled detail is append-only on top. That is the same
mechanism as `docs/memory.md` §4's suggestions, doing double duty.

It also gives `SegmentMark` its missing producer: **the map builder is the thing that
writes segments**, and its nodes are what the suggester nominates.

## 5. Zoom is recall-as-text, permanently — composable KV is dead on this stack

This section previously described zoom-as-paging: place a cached block at the tail,
prefix untouched, near-free. **UNVERIFIED-16 was run on 2026-09-09 and settles
negative.** `experiments/kv-stitching/RESULTS.md`.

The reason is stronger than the plan anticipated. It is not that recurrent layers
resist a partial correction — **there is nothing to correct**. `llama_memory_recurrent`'s
cells *are* sequence ids, so a per-sequence save writes **one** cell whose payload is
the entire accumulator: 36 layers, **111.4 MiB, fixed and independent of length**
(measured at 114.6 MiB after a *64-token* prompt). The attention half is 24 KiB/token
across the other 12 layers.

**A block's contribution to three quarters of the model is not extractable.** So
"recompute a fraction of C to reconcile it" is not a poor approximation. It is not an
operation.

Measured, against a determinism floor established first because the metric is
worthless without one — 36 consecutive bit-identical repeats over 200 greedy tokens:

- every stitch short of the divergence diverges at token **5–172**, with no trend
- **recomputing 98% of the prompt still breaks at token 5**
- at or before the divergence: bit-identical, KL exactly zero
- **the recompute fraction is 1.0 of everything after the divergence**

Confirmed without any doctoring by asking a slot for an honest prefix of itself and
counting what it re-runs: **three fixed re-entry points, not a sliding window** —
`d ≤ 3` free, `4 ≤ d ≤ 516` rewinds to a single checkpoint, `d > 516` costs the entire
prefix from token 0. Identical at 3.8k and 12.3k.

### What survives, and why the design was built this way

**All of §4.** Zoom-in is `recall(node_id)` returning text, appended as a tool result.
That was chosen because appending leaves the prefix intact, not because composition
was unavailable — and it is now the permanent mechanism rather than a stopgap.

This is the payoff from decoupling active memory from composable KV
(`docs/memory.md` §4). Had the design kept them coupled, this result would have killed
it. Because they were separated, only the *optimisation* died.

### What it settles elsewhere

- §19.3's recommendation against model swapping becomes **permanent**, not provisional.
- §3.10-B's handle protocol is untouched — it never sold prefill compute.
- arXiv 2608.03893's RoPE-stripping solves obstacle 1, and `get_can_shift()`'s own
  comment already says that obstacle does not bind here. **Position was never the wall.**

## 6. What was taken from opencode on 2026-09-23, and what was not

Read at `39ee69b` — *"Fix code-block selection and make cached compaction actually fire"* —
in `packages/opencode/src/session/{compaction,overflow}.ts` and
`packages/core/src/session/compaction.ts`. **A survey of source, not a measurement**: nothing
here was run against opencode. Three of its ideas were ruled on, and the rulings are recorded
here because the module they are implemented in (`crates/turn/src/compaction.rs`) is the one
that has to keep them.

### Taken: the fixed-section summary template

Universal, on both paths: **Objective / Important Details / Work State (Completed, Active,
Blocked) / Next Move / Relevant Files**, every section kept even when empty, and *"preserve
exact file paths, symbols, commands, error strings, URLs and identifiers"*. The reason is
§4's, one level down: **a section is a question, and an absent section is a question nobody
was asked.** "Nothing is blocked" and "nobody said" are different facts, and a prose ask
collapses them.

Also taken with it, and not part of the template: the model is told what it is about to lose
— *"anything you do not carry into it is lost"* — because a summariser that is not told the
prior record is discarded will reasonably assume it survives, and on a second compaction the
prior record sits in the conversation like any other item.

### Taken, conditionally: a verbatim tail of recent turns

Ruled by the operator, split by **where the model runs**:

| | summary template | verbatim recent turns |
|---|---|---|
| **remote model** | yes | **yes** |
| **local model** | yes | **no** |

The reason is the reason and not the policy. A local model is bounded by the **KV cache in
VRAM**, where a tail is resident tokens competing with the very pressure that called the
compaction. A remote model is bounded by a **context limit and a bill**, where 15k of verbatim
recent turns is affordable and buys back exactly what a summary is worst at — the literal text
of the last few exchanges. Same mechanism, different budget.

**One artefact shape, not two.** The tail is carried *beside* the summary and never folded
into it, so the local artefact is the remote one with an empty tail, and a session compacted
locally and resumed against a remote model (or the reverse) does not meet a record its reader
cannot read. The budget is a quarter of the window clamped to 2k–15k, which is opencode's own
`clamp(2_000, 15_000, usable / 4)` re-derived in our units and recorded as a shape rather than
a measurement.

Two constraints follow from this document rather than from opencode:

- **The tail is whole exchanges, or the split is disclosed.** A tail that begins mid-turn
  opens with a message answering a question that is no longer present, and §3's *something
  must stand where the evicted span was* applies inside the boundary as much as before it. One
  big file read is larger than the whole budget, so the case is real and it is stated in the
  fork's own note rather than left to be discovered.
- **The template's sections are a contract with the next reader**, so an empty section is kept
  and marked, never dropped.

### Not taken: clipping what the summariser is shown

opencode truncates every tool result to 2,000 characters **in the bytes handed to the
summariser**. Refused here, and the reason is the cache: our summarising call is
`render(stable_prefix, items[0..k]) ++ render(instruction)`, which is a true prefix of the
live token stream — §10.3's warm compaction — and clipping a payload inside it makes the
prompt differ from the one the server holds. **That trades a warm prefix for a cold one, which
is the 13 minutes this document is about.** The goal (bound the summariser's input) is served
better at the other end, where §8.3's `max_inline_bytes` bounds the payload as it is produced,
so it bounds *every* turn rather than only the summary. On a flattening summariser the clip
costs nothing because the prompt shares no prefix with anything; here it costs the prefix.

### Ruled, not built: `prune`

opencode's `prune` walks the message list backwards, skips the last two turns, protects the
most recent 40,000 tokens of tool calls and the tools in `PRUNE_PROTECTED_TOOLS`, stops at the
last summary, and then **erases the OUTPUT of older tool calls in place** — leaving
`[Old tool result content cleared]`, and keeping the call and its arguments. It needs no model
at all, so a re-prefill is its entire cost, and §2's thirteen minutes was the summarising call.

**Adopt, adapted, and it is not orthogonal to §4 — it is §4's first rung.** Three adaptations:

1. **A fork, not an edit.** opencode's transcript *is* its wire, so erasing in place is
   coherent there. Here the ledger is a hash chain over rendered tokens
   (`tokencore/src/ledger.rs`: `LedgerRow::h_k`, and a replay that disagrees is
   `LedgerError::ChainMismatch`), so an in-place erasure is not merely expensive — it is the
   thing the ledger refuses. The erasure therefore lands in a **new base**, exactly as a
   summary does, and the old transcript is kept whole. That is also this repo's own rule:
   compaction never deletes, it stops carrying.
2. **A locator, not a tombstone.** `[Old tool result content cleared]` is honest and
   unhelpful. Our marker can name where the body went, because §8.3's spill already exists
   (`crates/tools/src/spill.rs`, and `read_spill(hash, range)` is a seated tool) — so the
   erased payload is not destroyed, it is *not carried*, and the marker says how to pull it
   back. That is §4's `recall(node_id)` in miniature.
3. **Opt-in, and where it is worth most.** opencode gates it behind `cfg.compaction.prune`
   (off by default) and so should we. On this stack its strongest case is not routine use but
   the one the overrun path currently spends two summary turns on: **a session whose prompt
   fills the window has no room to write a summary, and pruning needs no room to write.**
   That is a mechanism that rescues the failing case rather than a housekeeping chore.

Sized as its own verb (`/prune`) so it can run with no model behind it, and not scheduled —
ruling and building are different acts, and this document records the ruling.

## 7. Open, and honestly

- **Node labelling quality.** A bad label makes a node unfindable — the jsonl failure
  again, with fewer words to guess from.
- **The map grows.** Slower than the transcript, not zero.
- **What produces nodes for prose spans.** Tool calls are structural; reasoning and
  assistant text are not, and something has to label them cheaply enough to run every
  turn.
- **Nothing here is scheduled.** §10 as written is a summarisation design; this is the
  argument for replacing it, not the replacement.
