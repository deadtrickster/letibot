# UNVERIFIED-16 — can stitching work on a hybrid attention + recurrent model?

Run 2026-09-09 on `lab2x1` against the live `qwen-3.8-flash-next` server
(`http://127.0.0.1:8080`, `build-mtp/bin/llama-server`, 5 slots × 262,144).
Nothing about the server was stopped, restarted or reconfigured.

**Verdict: no. Stitching is not viable on this stack, and composable KV beyond
prefix reuse is not achievable on it.** The recompute fraction is not "a few
percent"; there are exactly three points at which a hybrid state can be
re-entered at all — the last 3 tokens, one context checkpoint, and zero — and
everything between them costs a full re-fold. §19.3's recommendation against
model swapping should be made permanent.

The shape asked for in §3.10-C is a **step function**, not a convergence curve.
It is a step function for a reason that is stronger than the plan's expectation:
not "the recurrent blocks resist a partial correction", but **the recurrent
blocks have no per-token structure to correct**. There is nothing to stitch.

### The three questions, answered

1. **Does a partial recompute converge, per layer class?** No. Measured shape:
   flat at "wrong" for every *k* short of the divergence, then exactly right the
   moment the recompute starts at or before it (§6b, §6c). Recomputing 98 % of
   the prompt still breaks the continuation at token 5; recomputing 100 % is
   bit-identical. Per layer class the attribution is structural rather than
   numerical (§2, and §7 on why the numerical version could not be run): the
   attention half is 24 KiB/token of relocatable cells, the recurrent half is a
   single 111.4 MiB accumulator per sequence with no block structure in it at
   all.
2. **The recompute fraction.** 1.0 of everything after the divergence. What the
   stack can actually re-enter is three fixed points — the last 3 tokens, a
   checkpoint at tip−516, and zero (§4) — and on a chat workload the
   checkpoints sit at user-turn boundaries thinned to ~8k/16k/32k/64k/128k back.
3. **Qwen3-Next vs GLM.** One hybrid stands for the other here, because the
   question is about `llama_memory_recurrent` and both use it, with the same
   12-attention/34–36-recurrent split. GLM was not loaded (doing so means
   stopping Qwen), but the same failure was already measured on it directly
   (T3.5). See §7.

---

## 1. The served model is the hybrid the question is about

`Qwen3.8-Flash-Next` is architecture `qwen4exp` in this fork. From the GGUF:

    qwen4exp.block_count             48
    qwen4exp.full_attention_interval 4
    qwen4exp.attention.compress_ratios [0,0,0,4, 0,0,0,4, ... ]   (48 entries)
    qwen4exp.ssm.inner_size          6144
    qwen4exp.ssm.state_size          128
    qwen4exp.ssm.conv_kernel         4

Every 4th block is full (DSA-compressed) attention: **12 attention blocks, 36
recurrent blocks**. That is the same 1:3 split the plan records for GLM (12 MLA
+ 34 KDA), so the two models stand for each other on the structural question,
and this one is running.

`src/llama-model.cpp:2470-2530` builds it as a `llama_memory_hybrid`: attention
layers get a KV cache plus a QSA indexer cache, the rest get
`llama_memory_recurrent`.

## 2. The two halves of the state have incompatible algebra

This is the finding, and it is not an inference from behaviour — it is what the
memory module is.

**The attention half is per-token.** 12 layers × 2 KV heads × (256 + 256) × f16
= **24 KiB per token**, in cells that carry a position and could in principle be
relocated (that is obstacle 1, RoPE, which the plan already knows how to
attack).

**The recurrent half is one accumulator per sequence.**
`src/llama-memory-recurrent.h:69` says it outright — *"grow or shrink the number
of cells (= sequence ids)"*. Cells **are** sequence ids. A save of one sequence
(`state_write`, `:1108`) writes `cell_count` cells, and for this cache that is
1; the per-cell metadata (`state_write_meta`, `:1220`) is a single `pos`, and
the payload is the whole `r_l`/`s_l` state. 36 layers × (6144·128 + 6144·4) × f32
= **111.4 MiB, fixed, independent of length**.

Measured: a slot save after a **64-token** prompt is 120,206,104 bytes
(114.6 MiB). 111.4 of that is the recurrent accumulator.

So a block's contribution to the recurrent half is not extractable. There is no
"C's KV" in 36 of 48 layers — there is one fold over everything the sequence has
ever seen. `get_can_shift()`'s own comment (`:1053`) states the property that
makes this fatal for composition: *"the recurrent state is not a function of
position… It IS a function of content."* Position-independence is exactly what
obstacle 1 wanted — and it is worth nothing here, because content-dependence on
the **whole prefix** is obstacle 2, and it binds absolutely.

**CacheBlend's premise does assume attention-only layers, and the plan was right
to suspect it — but the failure is worse than "the fraction is large". A partial
recompute of a recurrent layer is not a poor approximation. It is not an
operation.** The only thing that produces a valid state at position *n* is
running the fold from a valid state at some *m* ≤ *n* over tokens *m…n*.

## 3. What the stack actually offers, and what it refuses

- **`--cache-reuse` (shift a matching chunk into place past an edit) — refused
  by design on this model.** `tools/server/server-context.cpp:5763` breaks out
  of the reuse loop whenever `ctx_tgt_has_recurrent_state`, with the reasoning
  written into the source: the KV could be moved, the accumulator cannot, and
  rebuilding the accumulator over those tokens regenerates their KV for free, so
  the shift is "memory churn with nothing to show for it". `:1845` logs the same
  at startup. This was earned the hard way on GLM (T3.5: `--cache-reuse 256`
  answered *"4 document parts"* where the truth was *"three"*, in 2.2 s instead
  of 61 s).
- **Partial `seq_rm` — bounded by `n_rs_seq` and nothing else.**
  `src/llama-memory-recurrent.cpp:523` allows a rollback only when it lands
  inside the per-token snapshot window; outside it the call returns `false`, and
  `llama_memory_hybrid::seq_rm` (`:242`) tries the recurrent cache **first** and
  propagates the refusal, so the attention cells the model could have kept are
  discarded with it. `n_rs_seq` is not a cache-tuning knob:
  `common/common.cpp:1725` sets it from `speculative.need_n_rs_seq()`, i.e. it
  is **0 unless speculative decoding asks for it**. It is 5 on this server only
  because of `--spec-draft-n-max 5`.
- **Slot save / restore — whole-sequence only**, and the file is a
  `llama_state_seq` blob whose recurrent section is the single accumulator.
  There is no concatenation of two of them.

**So the §3.10-C construction as literally specified — assemble a KV from a
cached A, a cached C taken from a different position, and a recompute of the
first k tokens of C — cannot be expressed by this server at all.** Not because
the server is missing a feature, but because the operation it names does not
exist for 36 of 48 layers. Building it would mean byte-splicing state files, and
for the recurrent half there is no defensible choice of bytes to splice.

## 4. The recompute fraction, measured

The cheapest possible edit: a slot holds the KV for a prompt of N tokens, and is
asked for the honest prefix `P[:N-d]`. The tokens that remain were computed in
exactly the right context at exactly the right positions. `prompt_n` is what the
server had to run again. Fresh prompt per row so nothing is served from the
prompt cache (`truncate.py`, `raw/truncate-fresh.json`, N ≈ 3,850).

    d (tokens dropped)      prefix    re-run    fraction of the prefix
    1                        3836         3     0.1 %
    2                        3855         2     0.1 %
    3                        3853         1     0.0 %
    4                        3868       512    13.2 %
    8                        3829       508    13.3 %
    32                       3808       484    12.7 %
    128                      3710       388    10.5 %
    256                      3604       260     7.2 %
    384                      3456       132     3.8 %
    512                      3319         4     0.1 %
    600                      3276      3276   100.0 %
    768                      3082      3082   100.0 %
    1024                     2812      2812   100.0 %
    2048                     1788      1788   100.0 %

Read it as re-entry points rather than as a curve. There are three:

1. **d ≤ 3** — inside the recurrent snapshot window, free.
2. **4 ≤ d ≤ 516** — rewind to the single context checkpoint that sits 516
   tokens from the tip and re-fold from there. `prompt_n = 516 − d` exactly.
3. **d > 516** — no checkpoint at or before the edit, so **the entire prefix is
   re-prefilled from token 0**.

Repeated at N ≈ 12,300 (`raw/truncate-12k.json`): identical. Still one usable
checkpoint 516 from the tip; d = 1024 already costs 11,305 tokens, i.e. 100 %.

516 is not a coincidence: `server-context.cpp:6120` creates prompt checkpoints
at fixed offsets from the end, `checkpoint_offsets[] = {4 + n_ubatch, 4}`, and
`n_ubatch` is 512 here. So a raw completion gets exactly two — tip−516 and
tip−4 — and the `checkpoint_min_step` schedule (default 8192) produced no
usable earlier one in either run. On a chat request checkpoints also land on
user-message boundaries and are then thinned exponentially by distance from the
tip (`server-context.cpp:4546`, ~5 spanning a 200k conversation at roughly
8k / 16k / 32k / 64k / 128k back), so the granularity there is "the last user
turn", not "a few tokens". Either way the re-entry points are a handful of fixed
places, not a sliding window.

**That is the answer to "the recompute fraction, if it converges at all".** It
does not converge. For an edit deeper than the last checkpoint the fraction is
1.0, and the checkpoints are thousands to tens of thousands of tokens apart.

## 5. The stitch experiment itself

Since the state cannot be assembled out of order, the strongest realisable
version was run instead, and it is strictly harder to pass:

    P     = B ++ C          the prompt to answer
    P_alt = B_alt ++ C      the same block C, at the same positions,
                            computed behind a different B

`|B| = |B_alt|` in tokens, so C's positions are identical in both and RoPE is
not a confound — the only thing wrong with C's cached KV is the context it was
computed in, which is obstacle 2 in isolation. The stitched state for recompute
width *k* is made by prefilling the first `|P| − k` tokens of `P_alt`, then
rewriting the token list inside the saved state file to the first `|P| − k`
tokens of `P` (`patch_tokens`, a same-size in-place edit of the
`server_tokens::serialize()` array inside the `llama_state_seq` file). The
server then believes it holds a prefix of `P` and prefills only the trailing *k*
tokens plus the question. Every row was checked against `timings.prompt_n`;
it equalled `k + |Q|` in every condition, so the stitch really happened and
nothing was quietly re-prefilled.

Recomputing a **suffix** rather than CacheBlend's prefix-of-the-block is
deliberate and is the stronger test: on a recurrent layer the state is a
left-to-right fold, so a recomputed prefix is immediately overwritten by the
stale later state, whereas a recomputed suffix is the only placement that can
carry corrected state to the generation point. If the suffix form fails, the
prefix form fails a fortiori.

**Validity control (it passed).** The question `Q_B` asks for a value that
appears only in `B`. Cold, the model answers it correctly. Stitched, it answers
from `B_alt` every time — seed 11: truth 23610, stitched said 57578; seed 12:
truth 47358, stitched said 77578. The doctoring takes: the state genuinely holds
the other history.

**And that is also the ceiling on what this construction can show.** In a real
composition the new text `B` would be freshly prefilled and only `C` would be
reused; here `B` was never processed at all. So the construction tests "is a
reused block's KV usable when it was computed behind different content" — the
question that matters for cross-session reuse — but it cannot test CacheBlend's
reconciliation of a *present-but-stale* neighbourhood, because this stack cannot
build that state.

## 6. The shape: a step function, with a zero noise floor

### 6a. The floor has to be measured, and it is size-dependent

Exact agreement of 200 greedy continuation tokens is only a metric if the
pipeline is deterministic when nothing is perturbed. At ~4,600 tokens it is not
(`determinism.py`): two runs with identical construction, temperature 0,
top_k 1 and a fixed seed diverged at token **43** of 200 with the server's MTP
drafting on, and at token **12** with `"speculative.n_max": 0` per request. So
the source is not MTP — it is the shared decode batch: four other slots are
serving other agents, and what else is in the batch changes the reduction order.

At ~1,470 tokens the same pair is **bit-identical, 200/200, in all 18 rows of
run 3** (`stitch3.py`, `raw/paired-near.json`), KL floor exactly 0.000000. So
the metric is usable at that size and worthless at 4.6k and above on a shared
server. Runs 1 and 2 were run at 12k and 2.4k and are noise-limited; they are
kept in `raw/` as the record of how the floor was found, and nothing is claimed
from them.

### 6b. Run 3, the answer

Three seeds, `|P|` ≈ 1,470, `|B|` ≈ 1,157, `|C|` ≈ 312, 200 forced greedy tokens
per condition, `prompt_n` verified to equal `k + |Q|` in every row. `floor` is
control-vs-repeat; `stitch` is control-vs-stitch. Both are the first divergence
out of 200.

    k          floor        div(stitch)       KL(ctrl‖stitch)
                            s21  s22  s23     s21     s22     s23
    0          200/200/200   26    5    5     0.270   0.053   0.007
    4          200/200/200   83   73    5     0.314   0.013   0.013
    16         200/200/200  172    5    5     0.087   0.148   0.006
    64         200/200/200   69   73    5     0.421   0.062   0.011
    |C| (~312) 200/200/200   12   41    5     0.510   0.012   0.005
    |C|+|B|    200/200/200  200  200  200     0.000000 (exact)

**This is the step function, and it is unambiguous.**

- The floor is 200/200 everywhere, so every divergence below 200 is signal, not
  noise.
- For every *k* short of the divergence the stitch disagrees with the control,
  and **agreement does not rise with k**. Recomputing the entire reused block
  (k = |C|, 21 % of the prompt) is no better than recomputing nothing, and on
  seed 21 it is markedly worse: divergence 12 against 26, KL 0.51 against 0.27.
  There is no convergence to find a fraction of.
- The moment *k* reaches the divergence (`k = |C| + |B|`, i.e. the recompute
  starts at or before the point where the two histories part) the stitch is
  **bit-identical** to the control: 200/200, KL exactly zero.

Wrong until you re-run from the divergence, then exact. That is precisely the
prediction the plan makes for a recurrent layer, and it is what the memory
module's structure (§2) says must happen.

### 6c. The step is at 100 %, not at 90-something

The transition was then swept finely between "recompute the reused block" and
"recompute from the divergence" (`raw/paired-transition.json`, same
construction, |P| = 1,470, divergence at token 0 because A is empty):

    k        tokens still carried    floor        div(stitch)      KL(stitch)
             from the wrong history               s21    s22      s21     s22
    ~313     ~1158                   200/200       12     41      0.510   0.012
     600      ~871                   200/200       12     23      0.314   0.005
     900      ~571                   200/200        7     73      0.039   0.014
    1100      ~371                   200/200      172     73      0.032   0.011
    1200      ~271                   200/200        5      5      0.009   0.010
    1300      ~171                   200/200       69      5      0.036   0.019
    1400       ~71                   200/200      172     41      0.036   0.014
    1440       ~31                   200/200        5     73      0.005   0.002
    |P|−1        1                   200/200      200    200      0.000   0.000

**Recomputing 98 % of the prompt buys nothing.** With ~31 tokens still carried
from the wrong history the continuation still breaks — at token 5 on one seed
and 73 on the other. Only when the carried region shrinks to a single token that
is identical in both histories does the output become bit-exact. The step is at
the divergence and nowhere before it, exactly as a fold that has to be re-entered
from a valid state implies. There is no fraction to report because there is no
fraction that works: the recompute fraction is 1.0 of everything after the
divergence.

The floor is 200/200 in all 18 rows of this sweep and all 18 of §6b — 36
consecutive bit-identical repeats. The divergences above are signal.

One qualification, because it cuts the other way and should not be buried: the
*answer* to the question about the reused block stayed correct in every
condition. A stitched state is not gibberish — the reused block's attention KV
is portable enough for a factual lookup out of that block. What it does not do
is reproduce the reference continuation, and it reads its neighbouring context
from the history it was actually computed over (the `Q_B` control, §5). For a
harness whose whole correctness argument is "the cache is an accelerator, never
a source of truth" (§3.8), "usually still answers" is not a property worth
anything.

Run 2 also aborted partway: see §8.

## 7. What could not be tested

- **GLM-5.3-Flash.** Not running, and loading it means stopping Qwen. Not done.
  The structural argument transfers exactly — 34 KDA blocks, same
  `llama_memory_recurrent`, same `seq_rm` refusal, same single accumulator — and
  the prior GLM measurement (T3.5, `--cache-reuse 256` answering "4 document
  parts" for a truth of "three") is the same failure observed directly on it.
  One hybrid does stand for the other on this question, because the question is
  about the memory module and both use the same one.
- **Depths 50k and 150k, and ≥ 20 samples per depth.** Not run. The prompt cache
  is 80 GiB of host RAM at ~120 KiB per token, and the sweep at 12k already
  OOM-killed the server once (§8). The truncation result is depth-independent in
  the only way that matters — the re-entry points are set by checkpoint
  placement, not by depth — but the greedy-agreement sweep at depth was not
  attempted after the OOM.
- **§3.10-C's per-layer-class numerics.** The server has no per-layer
  observability and a second copy of the model cannot be loaded: both GPUs are
  ~86 % full and 18 GiB of host RAM was free, against a 120 GiB model. A
  `libllama` harness is possible in principle (the `.so`s and headers are built
  under `build-glm/`) but not while Qwen holds the memory. The attribution here
  rests on the memory module's structure instead, which is not a proxy — it is
  the implementation of the thing being asked about.
- **The literal §3.10-C construction** (cached A + cached C from another
  position + recompute the first k of C). Not expressible; see §3.

## 8. The server was OOM-killed during this work

At 14:35 the kernel OOM killer killed `qwen-flash-next.service` (158.1 GiB
peak); systemd restarted it and `qwen-slots-restore` put the slots back. It was
down for about 40 seconds. **This was caused by this experiment**: every
distinct prompt prefilled becomes an entry in the server's 80 GiB host-RAM
prompt cache at ~120 KiB per token — because 111 MiB of every entry is the
recurrent accumulator, plus 145 MiB per context checkpoint — and the box was
already at 166/185 GiB before the sweep started. Roughly 50 distinct multi-
thousand-token prompt states in 15 minutes is enough.

It is a known failure mode of this box, not a new one: `server-context.cpp:4549`
records that the prompt cache "OOM-killed the server four times" and is why
checkpoints are thinned exponentially. This was the fifth.

`srv.guard()` now refuses to start a condition below 40 GiB of `MemAvailable`,
and everything after the OOM was run under it with small prompts. Anyone
sweeping prompts against this server should do the same.

## 9. What follows

- **Composable KV in the operator's sense — `[block_hash, block_hash, text,
  block_hash]`, blocks reusable across sessions and agents — is not achievable
  on this stack.** Not "expensive": unrepresentable. 36 of 48 layers hold one
  fold per sequence with no block structure in it.
- **§19.3's recommendation against model swapping becomes permanent.** Stitching
  cannot move swapping from "linear in the suffix" to "a measured fraction of
  the suffix", because the measured fraction is 1.0 past the last checkpoint.
- **The handle protocol (§3.10-B) is unaffected and is still worth building.**
  It was never selling prefill compute; it sells a checked cache hit. Nothing
  here touches it.
- **Prefix reuse plus checkpoints is the whole of what this stack can do**, and
  the checkpoint schedule is the only knob that changes the cost of an edit.
  `--checkpoint-min-step` is a real lever on the recompute cost of a mid-history
  edit, at 145 MiB of prompt-cache per checkpoint. That trade is worth costing;
  it is not stitching, but it is the same money.
- **arXiv 2608.03893's RoPE-stripping trick solves obstacle 1, which on this
  stack is the one that does not bind.** `get_can_shift()`'s comment already
  says the recurrent state is position-independent. Position was never the wall.

## Files

    srv.py            server client; `guard()` is the OOM floor
    corpus.py         deterministic record corpus with unique answerable facts
    stitch.py         run 1: k-sweep against a cold reference; `patch_tokens`
    stitch2.py        run 2: paired control, same prefill segmentation
    stitch3.py        run 3: paired control + a repeat, so the floor is measured
    truncate.py       the re-entry measurement of §4
    determinism.py    the noise floor of §6
    run2.sh           driver for run 2
    raw/              logs and json
