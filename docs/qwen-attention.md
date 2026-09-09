# Where a rule lives in Qwen3.8-Flash-Next, and why it stops binding

Written 2026-09-09, from the operator's question: *"the only reason memory is here and
can fade is attention right? so harness and that model must be not blind to the way llm
function."*

Mostly right, and the interesting part is the quarter where it is wrong. Companion to
`docs/closed-loop.md` (§10 commissions the falsifier this document predicts for) and
`docs/compaction.md` §5.

**Everything in §1–§3 was measured on this box. §5 is not, and says so.**

---

## 1. Two fading mechanisms, not one

This model is **hybrid**: 48 layers, of which **12 are attention and 36 are recurrent**.
They lose information in ways that are not the same and are not equally recoverable.

| | layers | what "fading" is | recoverable by placement? |
|---|---|---|---|
| attention | 12 of 48 | **dilution** — every token is in KV at full fidelity; softmax mass per token shrinks as the context grows | **yes** |
| recurrent | 36 of 48 | **overwriting** — the state is a fixed-size accumulator; old detail is compressed out and does not exist to retrieve | **no** |

The recurrent figure is the one that surprises. `llama_memory_recurrent`'s cells *are*
sequence ids, so the per-sequence state is **one cell whose payload is the whole
accumulator: 111.4 MiB, fixed and independent of length** — measured at 114.6 MiB after
a *64-token* prompt. The attention half is 24 KiB/token across its 12 layers and grows
normally.

So a fixed 111 MiB is carrying however much conversation you have. At 4k tokens that is
lavish. At 150k it is a bottleneck with a hard edge, and what falls out of it is gone in
a sense that repositioning cannot touch. **Three quarters of this network cannot hold an
old rule at fidelity no matter where you put it.**

## 2. Position is already load-bearing on this fleet, and it is measured

Two facts we already run on, neither of which is about content:

- **Effort level lives at token 3.** Changing it re-prefills everything
  (`qwen-effort-prefix-break`). A three-token-deep edit costs the whole prompt.
- **Re-entry is three fixed points, not a sliding window.** Asking a slot for an honest
  prefix of itself and counting what it re-runs: `d ≤ 3` free, `4 ≤ d ≤ 516` rewinds to a
  single checkpoint, `d > 516` costs the entire prefix from token 0. **Identical at 3.8k
  and 12.3k**, so it is structural rather than proportional.

A harness that treats the prompt as an ordered list of messages cannot see either of
these. Both are properties of *where*, not *what*.

## 3. What this predicts about the failures that prompted it

Three loaded rules failed to bind in one heavily compacted session (`docs/closed-loop.md`
§1). Every one of them was *present in context* — `MEMORY.md` is re-injected whole. The
proposed mechanism follows directly from §1:

- The rule sits near the **front**, where it was injected.
- It has not been *referenced* in tens of thousands of tokens, so nothing recent points
  at it.
- Compaction destroyed the material that used to point at it — the recent instance, the
  cost of getting it wrong — while faithfully preserving the rule itself.

**The rule survived; its salience did not.** And in the recurrent 36 layers, the salience
was not diluted but *overwritten*, which is why re-reading the file does not restore it:
the file was never gone.

This also predicts something specific and testable: **a longer rules file makes each rule
weaker.** More tokens competing for the same mass. That runs against the instinct to
write more down when something goes wrong, and it is exactly the instinct this repo has
been following all day.

## 4. What the harness can do that a prompt cannot

Placement is an actuator, and it is one the model has no access to:

1. **Put the rule next to the action, not at the front of the session.** A hazard rule
   surfaced immediately before the tool call rides recency instead of competing with
   150k tokens of history.
2. **Fire on the precondition, not on a schedule.** Re-stating every rule every turn
   costs tokens and dilutes everything around it. But the harness *sees* `pkill -f` in a
   proposed command — it does not have to guess when the rule matters. That is
   `docs/closed-loop.md`'s encoder → error signal → **placement**.
3. **Re-anchor after compaction specifically.** If §3 is right, compaction is the event
   that needs a repair pass, and the repair is positional rather than textual.
4. **Keep the map, not the prose.** For the recurrent 36 layers there is no repositioning
   out of an overwritten accumulator — the content has to be *re-presented as tokens*.
   `docs/compaction.md` §4's structural map is that re-presentation; a prose summary that
   flattens `pkill refused at turn 47` into English destroys the one artifact that could
   have been re-anchored.

**Attention is the loop gain.** A correction with no attention mass is a signal that
never reaches the actuator, and a rule everyone can point to in a file and nobody follows
is not a discipline failure — it is a gain of approximately zero.

## 5. UNVERIFIED — what is borrowed, not measured here

Stated separately because §1–§3's numbers are this box's and these are not:

1. **Attention sinks** — that the first few positions receive disproportionate mass. From
   the literature, not measured on this model. If false, "the front is privileged" is
   wrong and only recency remains.
2. **Lost-in-the-middle** — that mid-context material is retrieved worst. Same status.
   §4.1 leans on it.
3. **Dilution as the mechanism of adherence decay.** §3 is a hypothesis with a plausible
   mechanism, not a finding. `docs/closed-loop.md` §10's falsifier is what tests it, and
   its condition D (re-anchor the rule at the tail) is the direct probe: if D recovers to
   the fresh baseline, placement is the mechanism.
4. **Whether the recurrent half caps that recovery.** §1 predicts D *plateaus below*
   fresh, because re-anchoring restores the attention pathway and cannot restore
   overwritten recurrent state. This is the most interesting number in that run and
   nobody has it.
5. **The 12/36 split** is read from the layer counts in `docs/compaction.md` §5's
   measurement ("36 of 48 layers"), not from an independent architecture dump.
6. **None of this is known to transfer to GLM**, which is also hybrid but differently —
   19.25 KiB/token including its DSA indexer, and a separate per-sequence charge of
   436.7 MiB at allocation.
