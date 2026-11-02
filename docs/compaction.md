# Compaction as structural eviction

Compaction as it is normally built bundles two operations that are not the same thing, and
pays for the second one without being asked. This is the argument for keeping what it drops.
Companion: `docs/memory.md` (passive vs active memory, the suggestion mechanism).

## 1. Why does compaction happen at all?

Five reasons are usually given. They are not equally real.

| # | claimed reason | status |
|---|---|---|
| 1 | the context window is a hard wall | **forced** - the only one |
| 2 | tokens cost money | **mode-dependent** - true on a metered API, false locally (T9) |
| 3 | quality degrades with depth | **measured false** - see below |
| 4 | decode slows as KV grows | real |
| 5 | KV memory pressure across sessions | real - this is the OOM above |

Reason 3 is the one everybody assumes, and Falsifier B killed it: 160 samples scored on
compile-and-tests-pass gave 0.829 at zero depth, 0.875 at 20k, **1.000 at 60k**, 0.900 at 150k, with
Fisher p = 1.000 between 20k and 150k and the *shallowest* condition worst. Depth did not hurt; the
absence of a conversation did.

So compaction is driven by the wall and by resources, not by quality, and the right local policy is
*compact when you must, as late as possible*.

## 2. What it costs

The strategy these numbers are of: **flatten**. The conversation is collapsed into one user message with
`system: []` and `tools: {}`, a summary is asked for, the history is replaced. Measured on a harness that
does this: **144,436 tokens, cache hit 0, about 13 minutes** to the first summary token, and **the next
real turn is also cold**, because the prefix is now entirely different. Detail is destroyed irreversibly,
and a short context made this model reason **about 4x longer** - every truncation-by-length in the run
happened at zero depth.

One correction, because it changes where the cost lands: on a strategy that re-sends the previous request
**byte-identical** and appends only the instruction, the summarising call is cheap. What cannot be cheap
is the moment the history is replaced. **The reduction is what costs, and it costs the next turn.**

The tree branches on the backend for that reason, and the currency differs: a local llama.cpp miss costs
time (200-375 tok/s), a cloud miss costs money, and a cloud provider takes messages rather than tokens so
no prefix machinery applies to it at all. A local server keeps the expensive half a true prefix of what
it holds (`summarise_overrun`); a cloud provider folds and continues with no overlap (`plan_fold`).
`plan_overrun` chooses, `Harness::compact_inner` carries it out.

## 3. The conflation at the heart of it

- **Reducing what is resident** - necessary, driven by the wall and by memory.
- **Destroying what is recoverable** - an implementation accident. Nobody asked for it.

Reasons 1, 4 and 5 demand only the first. Summarisation delivers both.

### The uncomfortable part, which rules out the easy fix

**Plain eviction does not help the next turn either.** Dropping old turns invalidates the
prefix *by definition*, because the prefix **is** the old turns. Summarise, prune or delete -
any reduction costs a full re-prefill **of the turn that follows it**.

That is still enough to rule out the easy fix. And there is a reason summarisation is used
rather than deletion: a plain drop leaves a hole, and references to removed turns dangle.
**Something must stand where the evicted span was.**

So the question is not "how do we avoid the cost" but **"we are paying a re-prefill either
way - what should we get for it?"**

## 4. The answer: a zoomable map, not a summary

Replace an evicted span with a **structured, addressable index** of what it contained, rather
than prose about it.

### Why structural beats prose

Most of a conversation is already typed and we throw the schema away. Tool calls have names, arguments
and a closed `ToolOutcome` vocabulary: `read src/foo.rs:1-200`, `grep X → 3 hits in Y`, `cargo test → 3
failures` are *events*, not sentences. A prose summary flattens them into English; a map keeps them as
nodes with identities, and the model is needed only for the genuinely prose parts.

### Maintained incrementally, which is what removes the stall

Build the map turn by turn, each turn contributing its nodes as it completes. Then evicting a span
costs *rendering nodes you already have at a coarser zoom* - there is no summarisation call at
compaction time, or a tiny one. This is the leveled-compaction argument in `docs/design-brief.md` §1,
which was missing its data structure.

### The zoom levels are the ladder's rungs

We already built this ladder once, for KV, in llama.cpp: `full → thinned → skeletal → no-draft →
spill`, degrading before evicting, each rung shedding something that costs *time on restore* and never
correctness. The same shape applies here:

```
full text  →  node with children  →  node with label  →  handle only
```

**Degrade, do not drop.** A span loses fidelity by steps and stays addressable at every rung.

### The constraint that must not be violated

**Zooming in cannot rewrite the map in place.** Editing anything already in the prefix
invalidates it - the same trap as compaction itself. So:

| | where it lives | cost |
|---|---|---|
| the map | written once at eviction, in the prefix | its own tokens, permanently |
| a zoom-in | **appended** via `recall(node_id)` | a tool result |
| the detail | in the store, addressed by node id | nothing until pulled |

The map is a fixed index; recalled detail is append-only on top. That is the same mechanism as
`docs/memory.md` §4's suggestions, doing double duty.

It also gives `SegmentMark` its missing producer: the map builder is the thing that writes
segments, and its nodes are what the suggester nominates.

## 5. Zoom is recall-as-text, permanently - composable KV is dead on this stack

Zoom-as-paging - place a cached block at the tail, prefix untouched, near-free - was the
original shape of this section, and it was tested and settles negative.
`experiments/kv-stitching/RESULTS.md`.

The reason is stronger than a partial correction being hard: there is nothing to correct.
`llama_memory_recurrent`'s cells *are* sequence ids, so a per-sequence save writes **one** cell whose
payload is the entire accumulator: 36 layers, **111.4 MiB, fixed and independent of length**. The
attention half is 24 KiB/token across the other 12 layers, so **a block's contribution to three quarters
of the model is not extractable** - "recompute a fraction to reconcile it" is not a poor approximation,
it is not an operation.

Measured against a determinism floor established first: every stitch short of the divergence diverges at
token **5-172**, recomputing 98% of the prompt still breaks at token 5, and asking a slot for an honest
prefix of itself confirms **three fixed re-entry points, not a sliding window**.

### What survives, and why the design was built this way

**All of §4.** Zoom-in is `recall(node_id)` returning text, appended as a tool result. That was
chosen because appending leaves the prefix intact, not because composition was unavailable -
and it is now the permanent mechanism rather than a stopgap.

This is the payoff from decoupling active memory from composable KV (`docs/memory.md` §4). Had
the design kept them coupled, this result would have killed it. Because they were separated,
only the *optimisation* died.

## 6. What the module must keep

`crates/turn/src/compaction.rs` implements the decisions below, and it is the module that has
to keep them.

### The fixed-section summary template

Universal, on both paths: **Objective / Important Details / Work State (Completed, Active, Blocked) /
Next Move / Relevant Files**, every section kept even when empty, and *"preserve exact file paths,
symbols, commands, error strings, URLs and identifiers"*. The reason is §4's, one level down: **a section
is a question, and an absent section is a question nobody was asked.** `SUMMARY_SECTIONS` is that list,
and the wire carries seven of the eight leaves (`WIRE_SECTIONS`); the eighth is the record itself. The
instruction also says what is about to be lost - *"anything you do not carry into it is lost"*.

### A verbatim tail of recent turns, split by where the model runs

| | summary template | verbatim recent turns |
|---|---|---|
| **remote model** | yes | **yes** |
| **local model** | yes | **no** |

The reason is the reason and not the policy. A local model is bounded by the **KV cache in VRAM**,
where a tail is resident tokens competing with the very pressure that called the compaction; a remote
model is bounded by a **context limit and a bill**, where 15k of verbatim recent turns buys back
exactly what a summary is worst at. Same mechanism, different budget.

**One artefact shape, not two.** The tail is carried *beside* the summary and never folded into it, so
the local artefact is the remote one with an empty tail, and a session compacted locally and resumed
against a remote model does not meet a record its reader cannot read. `tail_budget` is a quarter of the
window clamped to 2,000-15,000 tokens.

Two constraints follow:

- **The tail is whole exchanges, or the split is disclosed.** A tail that begins mid-turn opens
  with a message answering a question that is no longer present, and §3's *something must stand
  where the evicted span was* applies inside the boundary as much as before it. One big file
  read is larger than the whole budget, so the case is real: `TailPlan::Split` carries the
  number of items left out.
- **The template's sections are a contract with the next reader**, so an empty section is kept
  and marked, never dropped.

### Not clipping what the summariser is shown

opencode truncates every tool result to 2,000 characters in the bytes handed to the summariser.
Refused here, and the reason is the cache: our summarising call is
`render(stable_prefix, items[0..k]) ++ render(instruction)`, a true prefix of the live token stream, and
clipping a payload inside it makes the prompt differ from the one the server holds. That trades a warm
prefix for a cold one - the thirteen minutes this document is about. The goal (bound the summariser's
input) is served at the other end, where `max_inline_bytes` bounds the payload as it is produced, so it
bounds *every* turn.

### `prune`: ruled, not built

`prune` walks the message list backwards, skips the last two turns, protects the most recent 40,000
tokens of tool calls and a list of protected tools, stops at the last summary, and then erases the
OUTPUT of older tool calls in place, keeping the call and its arguments. It needs no model, so a
re-prefill is its entire cost.

Adopt, adapted - it is §4's first rung, not an alternative to it:

1. **A fork, not an edit.** Here the ledger is a hash chain over rendered tokens
   (`crates/tokencore/src/ledger.rs`: `LedgerRow::h_k`, and a replay that disagrees is
   `LedgerError::ChainMismatch`), so an in-place erasure is the thing the ledger refuses. The erasure
   lands in a **new base**, exactly as a summary does, and the old transcript is kept whole. That is
   this repo's own rule: compaction never deletes, it stops carrying.
2. **A locator, not a tombstone.** The marker can name where the body went, because the spill exists
   (`crates/tools/src/spill.rs`, and `read_spill(hash, range)` is a seated tool). It must not promise
   more than it can: the spill store defaults to **memory**, which dies with the daemon and makes
   `read_spill` fail after a restart unless `--spill-dir` is given.
3. **Opt-in, and where it is worth most.** Off by default. Its strongest case is the one the overrun
   path spends two summary turns on: **a session whose prompt fills the window has no room to write a
   summary, and pruning needs no room to write.**

Sized as its own verb (`/prune`) so it can run with no model behind it, and not scheduled.

## 7. Open, and honestly

- **Node labelling quality.** A bad label makes a node unfindable - with fewer words to guess
  from than the transcript had.
- **The map grows.** Slower than the transcript, not zero.
- **What produces nodes for prose spans.** Tool calls are structural; reasoning and assistant
  text are not, and something has to label them cheaply enough to run every turn.
- **Nothing here is scheduled.** §4 is the argument for replacing the summarisation design, not
  the replacement.

## 8. Images, and the flag R54 raised

R54 part two ends with a question it deliberately did not answer: *"compaction. An image in the
transcript is re-sent on every prompt rebuild. opencode strips media on compaction and has a
recovery message for the provider refusing it. R24 and R27 are yours. Say what you conclude; do
not silently keep images forever."* Three parts, and the middle one is the one that matters.

**1. Images before the tail leave the prompt by construction - and that IS `stripMedia`.** A row
before the tail is replaced by the summary, so its `data_ref` is no longer in the transcript and never
re-sent. No code, and the property is stronger than opencode's: theirs strips media out of parts that
stay, this one replaces the parts whole.

**2. Images inside the verbatim tail are kept, and must be.** The tail's value is that it is
byte-identical to what the server already prefilled. Stripping an image would buy a smaller prompt with
a cold prefill, which is the trade this design exists to refuse - so this is a place where letibot
should not copy opencode, which strips media in the tail too.

**3. Two things are missing, and the first is a defect the moment images ship on the local
path.**

**The accounting.** `plan_tail` sums `item_tokens`, which is the tokens the ledger rendered for that
row - the TEXT. An image's payload is a sentence (about twenty tokens) while the picture costs **4151
tokens** on the wire (one vision token per 1024 pixels, capped at 4096). So a tail "planned to fit the
budget" can be two orders of magnitude over it. **Not fixed, filed, because it is a change to token
accounting**: the number is already on the row - `Media { width, height }` gives
`min(w*h/1024, 4096)` without asking any server.

**The record has to say what was read.** A picture cannot be re-derived from the summary - unlike a
path, a command or an error string, it is gone for good once the row is replaced - so the instruction
now carries a rule for it, and the test beside the other section rules holds it to that.

**And one thing this does NOT settle.** A picture in the verbatim tail stays for as long as the session
does - re-sent on every rebuild, base64 and all. opencode's answer is a recovery instruction for when a
provider refuses oversize media; this tree has no such path yet, and until it does the honest statement
is the one in `crates/transcript/src/lib.rs`: an image lives for the life of the session.
