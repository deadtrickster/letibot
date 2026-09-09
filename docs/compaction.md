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

Flatten the conversation into one user message with `system: []` and `tools: {}`, ask
for a summary, replace the history, continue. Measured on opencode:

- **144,436 tokens, cache hit 0, ~13 minutes** to the first summary token.
- The next real turn is *also* cold, because the prefix is now entirely different.
- Detail is destroyed irreversibly.
- And Falsifier B found a third cost nobody counts: a short context made this model
  reason **~4× longer** — median 6,468 generated tokens against ~1,400 — with every
  truncation-by-length in the whole run occurring at zero depth. **Compaction can cost
  tokens on the far side too.**

## 3. The conflation at the heart of it

Compaction bundles two operations that are not the same thing:

- **Reducing what is resident** — necessary, driven by the wall and by memory.
- **Destroying what is recoverable** — an implementation accident. Nobody asked for it.

Reasons 1, 4 and 5 demand only the first. Summarisation delivers both.

### The uncomfortable part, which rules out the easy fix

**Plain eviction does not help either.** Dropping old turns invalidates the prefix *by
definition*, because the prefix **is** the old turns. Summarise, prune or delete — any
reduction costs a full re-prefill. There is no cheap version, and a design that claims
one has not understood the prefix cache.

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

## 6. Open, and honestly

- **Node labelling quality.** A bad label makes a node unfindable — the jsonl failure
  again, with fewer words to guess from.
- **The map grows.** Slower than the transcript, not zero.
- **What produces nodes for prose spans.** Tool calls are structural; reasoning and
  assistant text are not, and something has to label them cheaply enough to run every
  turn.
- **Nothing here is scheduled.** §10 as written is a summarisation design; this is the
  argument for replacing it, not the replacement.
