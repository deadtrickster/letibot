# Where a rule lives in Qwen3.8-Flash-Next, and why it stops binding

Why a rule present in the context, and re-injected whole, can stop being followed. Layout
numbers are `docs/model-topology.md`.

## The conclusion

Flash-Next is hybrid: 48 serving blocks, **12 full attention and 36 linear/recurrent**.
Attention **dilutes** - every token is in KV at full fidelity and mass per token falls as the
context grows - so placement can recover it. The recurrent layers **overwrite**: the state is a
fixed-size accumulator and old detail is compressed out, so nothing recovers it.

The number that decides it: the per-sequence recurrent state is **111.4 MiB, fixed and
independent of length**, against 24 KiB per token for the attention half. A fixed 111 MiB
carries any amount of conversation, so **three quarters of the network cannot hold an old rule
at fidelity wherever you put it.**

The attention half is not simply diluted either: a **sparse indexer with `top_k = 2048`** makes
it *select* first (`docs/model-topology.md` §3) - a query may see 25% of an 8k context, 6.3% of
32k, **1.4% of 150k**. An old rule is usually not attended at all.

## What follows for the harness

A rule near the front, unreferenced for tens of thousands of tokens, in a compacted session:
compaction destroyed the material that pointed at it and preserved the rule. **The rule
survived; its salience did not** - in the recurrent layers it was overwritten, not diluted,
which is why re-reading the file does not restore it.

Placement is an actuator the model has no access to. These are the constraints:

- **Put the rule next to the action**, not at the front: a hazard surfaced immediately before
  the tool call rides recency instead of competing with 150k tokens.
- **Fire on the precondition, not on a schedule** - the harness sees `pkill -f` in a proposed
  command, and restating every rule every turn only dilutes what is around it.
- **Re-anchor after compaction specifically**, and **keep the map, not the prose**: the
  recurrent layers need the content re-presented as tokens - `docs/compaction.md` §4's
  structural map.
- **A longer rules file makes each rule weaker** - more tokens for the same mass and the same
  2048 slots.
- **Effort is a session property, not a request field.** It sits near the head of the prefix,
  so changing it re-prefills everything (`ReasoningEffort`, `crates/dialect-qwen`).
- **A cache shortfall is not a divergence.** Re-entry is three fixed points (`d <= 3` free,
  `4..=516` one checkpoint, above that the whole prefix), and `crates/turn/src/prefix.rs`
  reports the shortfall as `cache_reuse_shortfall`.

Not measured here: attention sinks and lost-in-the-middle are literature, not this model.
Whether the recurrent half caps what re-anchoring buys is unmeasured - the prediction is a
plateau below a fresh session, and `docs/closed-loop.md` §10's falsifier would produce the
number. None of it is known to transfer to GLM (`docs/glm-and-dense-attention.md`).
