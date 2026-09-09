# Falsifier B — does quality degrade with conversation depth?

Milestone **M3.5**. Answers TODO **T7**; the answer feeds **T9** and §10 of the
implementation plan.

Run 2026-09-09 on lab2x1 against the local llama.cpp server, model
`qwen-3.8-flash-next` (Q6_K, 176.9B params), 5 slots × 262,144 context.
Server config untouched.

---

## The verdict

**No measurable degradation from 0 to 150,000 tokens of preceding conversation,
in a 262,144-token window.** Pass rate at 20k and at 150k is statistically
indistinguishable (35/40 vs 36/40, Fisher exact two-sided **p = 1.000**). There
is no monotone trend and no cliff anywhere in the measured range.

The assumption on trial — *a long conversation stays usable right up to the
context window, so compaction can be lazy* — **survives** for depths up to 150k.
Nothing in this data justifies compacting on quality grounds below 150k.

The 0k control is the **worst** cell in the table, not the best. Depth did not
hurt; the absence of a conversation is what hurt.

---

## Results table

`tests_pass` = the model's first attempt compiled AND passed the rig's own
tests. One attempt, no retries, no repair, no subjective rating.

| depth (target) | depth (measured) | n scored | truncated | compiles | tests pass | pass rate | 95% CI (Wilson) | per-rep spread |
|---|---|---|---|---|---|---|---|---|
| 0k (control) | 0 | 35 | 5 | 29 | 29 | **0.829** | [0.67, 0.92] | 0.25–1.00, sd 0.240 |
| 20k | 19,190 | 40 | 0 | 36 | 35 | **0.875** | [0.74, 0.95] | 0.60–1.00, sd 0.139 |
| 60k | 60,046 | 40 | 0 | 40 | 40 | **1.000** | [0.91, 1.00] | 1.00–1.00, sd 0.000 |
| 150k | 149,713 | 40 | 0 | 37 | 36 | **0.900** | [0.77, 0.96] | 0.80–1.00, sd 0.100 |

Spread is the pass rate computed within each of the 8 repetitions (one full
sweep of all 5 tasks), then the min–max and population sd across those 8.
Temperature is the production 1.0, so this variance is real.

### By task × depth (pass / n)

| task | 0k | 20k | 60k | 150k |
|---|---|---|---|---|
| `merge_intervals` | 6/8 | 7/8 | 8/8 | 7/8 |
| `parse_size` | 6/6 | 7/8 | 8/8 | 7/8 |
| `longest_common_prefix` | 8/8 | 8/8 | 8/8 | 8/8 |
| `lru_cache` | 5/6 | 7/8 | 8/8 | 8/8 |
| `parse_range_list` | 4/7 | 6/8 | 8/8 | 6/8 |

No task falls off with depth. `parse_range_list` is the hardest task at every
depth, `longest_common_prefix` is perfect at every depth. The per-cell n is 8,
so individual cells are noisy; the depth columns are the readable unit.

### Significance

| comparison | p (Fisher exact, two-sided) |
|---|---|
| 0k vs 20k | 0.746 |
| 0k vs 60k | **0.008** |
| 0k vs 150k | 0.500 |
| **20k vs 150k** | **1.000** |
| 0k vs all depths pooled (29/35 vs 111/120) | 0.107 |

The only significant cell is 0k vs 60k, and its sign is the opposite of the
hypothesis under test: the *deeper* condition did better. Read together with the
non-monotone shape (0.83 → 0.88 → 1.00 → 0.90) this is noise around a flat
~90% line plus a genuinely worse control, not a depth effect.

---

## Run losses to `finish_reason: "length"`

5 of 160 samples (3.1%) hit the token cap and were counted separately, never as
model failures. **All 5 were at 0k. Zero at every depth ≥ 20k.**

| depth | truncated | issued | rate |
|---|---|---|---|
| 0k | 5 | 40 | 0.125 |
| 20k | 0 | 40 | 0.000 |
| 60k | 0 | 40 | 0.000 |
| 150k | 0 | 40 | 0.000 |

This is not a budget artifact. `max_tokens` was 16,000 — generous for a
20-line Rust function — and the reason it was needed is the second finding
below. An earlier abandoned run at `max_tokens` 6,000 truncated far more often;
both abandoned runs are kept under `abandoned/` and are not part of the result.

---

## Second finding: an empty context makes this model reason ~4× longer

Tokens generated per sample (reasoning + content; this model returns a separate
`reasoning_content`, and both count against `max_tokens`):

| depth | median | mean | p90 | max |
|---|---|---|---|---|
| 0k | **6,468** | 6,662 | 16,000 | 16,000 |
| 20k | 1,614 | 1,855 | 4,219 | 5,368 |
| 60k | 1,334 | 1,400 | 2,712 | 3,196 |
| 150k | 1,496 | 1,645 | 2,907 | 4,840 |

Same tasks, same temperature, same everything but the preamble. With a long
technical conversation already in context the model settles into the register of
that conversation and answers in ~1,400 tokens; with an empty context the
template's `Reasoning effort is set to xhigh` free-runs, and 12.5% of the time it
never terminates inside 16,000 tokens.

This is a cost effect, not only a quality one: on this box the 0k control was
the most expensive condition to run, per sample, of the four.

It is also the mechanism behind the 0k control's lower score. The 0k cell's
per-rep spread (0.25–1.00, sd 0.240) is by far the widest in the table, driven
by reps where several tasks ran away at once.

---

## Cache evidence

Each depth used a byte-identical filler prefix with the task appended last, and
`cache_prompt: true`. From the response `timings`:

| depth | first sample `cache_n` | first `prompt_n` | median `cache_n` after | median `prompt_n` after | median `prompt_ms` after |
|---|---|---|---|---|---|
| 0k | 42 | 295 | 42 | 391 | 357 |
| 20k | 42 | 19,445 | 19,192 | 391 | 526 |
| 60k | 56,548 | 3,795 | 60,048 | 391 | 656 |
| 150k | 42 | 149,968 | **149,715** | **391** | 805 |

At 150k the first request prefilled 149,968 tokens in **144.6 s**; every
subsequent request at that depth reused 149,715 cached tokens and prefilled only
the 391-token task, in **0.8 s**. A **~180× reduction** in prefill. The run
measured quality, not prefill contention.

(60k shows a non-zero first `cache_n` because its slot still held a prefix from
an earlier aborted run; it converged to the full 60,048 immediately after.)

This independently confirms the premise T9 rests on: with a warm prompt cache,
carrying a long history into the next turn is close to free, and it is the
*compaction* that would make the next turn a cold prefill.

---

## Method

**Model / server.** `qwen-3.8-flash-next` on `http://127.0.0.1:8080`,
5 slots × 262,144 ctx. Sampling left at the server's production defaults:
temperature 1.0, top_p 0.95, top_k 20. `max_tokens` 16,000. Nothing about the
server was restarted or reconfigured.

**Depths.** 0k control, 20k, 60k, 150k. Measured with the server's own
tokenizer, never estimated: the message list is rendered through
`/apply-template` and that rendering is tokenized via `/tokenize` with
`parse_special` passed **explicitly as `true`**. Measured depths are 0 / 19,190
/ 60,046 / 149,713 tokens. The 20k cell lands 4% low because the filler grows in
whole user+assistant pairs (~2k tokens each); the measured number is what is
reported everywhere.

**Filler.** Real technical prose in conversation shape: ten planning and
engineering documents from this box — `docs/implementation-plan.md`,
`docs/design-brief.md`, `docs/workstreams.md`, `docs/survey.md`,
`docs/chat-templates.md`, then `GLM-TODO.md`, `GLM-STATE.md`, a llama.cpp test
plan, an upstream-PR handover and an unrelated project plan — split at paragraph
boundaries into ~4,000-character chunks and laid out as alternating user /
assistant turns, both sides carrying real prose. 150k is 152 turns. No word
salad. `check_corpus.py` asserts that none of the five task identifiers appears
anywhere in the corpus or in the assembled 150k prefix; it reports CLEAN. (The
word "LRU" occurs three times in the corpus as a cache-eviction concept, never
as an implementation.)

**Tasks.** Five small, fully specified, self-contained Rust items with no
dependence on repo context: `merge_intervals`, `parse_size`,
`longest_common_prefix`, `lru_cache` (a bounded LRU with `get`/`put`/`len`),
`parse_range_list`. Each prompt gives an exact signature and a precise
behavioural spec, and demands a single Rust code block.

**Scoring.** Objective, first attempt, no retries. The last/longest fenced block
is extracted and dropped into a scratch crate. `cargo build --lib` gives
`compiles`; `cargo test --test spec` against the rig's own tests gives
`tests_pass`. Tests are in `tests_ref/`, written by the rig, never by the model
— and `cargo test --test spec` builds the lib without `cfg(test)`, so any tests
the model wrote for itself are not compiled and cannot contribute to its score.
`reference_solutions/` holds a correct implementation of each task; all five
pass, which is what makes a failed sample readable as a model failure rather
than a rig bug. All 15 observed failures were inspected: every one is a genuine
`rustc` error or a genuine behavioural test failure. None was an extraction
failure and none was a missing `pub`.

**Sampling and ordering.** 8 repetitions × 5 tasks × 4 depths = 160 samples,
40 per depth, 8 per (task × depth). Within a depth, samples are rep-major with
the task order rotated one step per repetition, so every task occupies every
position across the run. Checked afterwards: pass rate by position is
27/32, 30/30, 29/30, 28/31, 26/32 — no position effect; and by repetition
16/19 … 19/20 — no drift.

**Concurrency.** One worker per depth, each pinned to its own server slot so
each depth keeps its own warm cache. All four depths therefore ran over the same
wall-clock window, which removes drifting server state as a confound entirely
rather than merely interleaving against it. Concurrency was 4 for most of the
run. **The timing numbers in `results.json` are therefore not per-request
throughput on an idle machine** — and because the workers finished at different
times, the 0k worker ran largely alone at the end, so its decode rate (97 tok/s
vs ~32) reflects having the GPU to itself, not anything about depth. Whole run:
75.8 minutes.

---

## What this does and does not establish

**Does.** For this model, on small self-contained coding tasks, up to 150k
tokens of unrelated preceding conversation, first-attempt correctness does not
fall off. The failure mode the plan feared — a collapse at, say, 60k in a 262k
window — did not occur.

**Does not.**

1. **This is a fresh-reasoning test, not a retrieval test.** Every task is
   self-contained and the filler is deliberately irrelevant. It measures whether
   depth degrades the model's ability to reason *now*. It does **not** measure
   whether the model can still find and use something stated at turn 3 when it
   is at turn 200. That is a different failure mode and it is the one that most
   plausibly still bites; it deserves its own falsifier.
2. **Detection floor.** With n = 40 per depth at a ~90% baseline, this run can
   see a collapse (a drop to ~60% would be significant) but cannot resolve a
   5-point slide. "No measurable degradation" means exactly that, not "provably
   zero".
3. **150k is not the wall.** The window is 262,144. Nothing here says anything
   about 150k–262k, and the last stretch before a hard wall is where the KV
   pressure and eviction behaviour of T9's second justification lives.
4. **Depth content is not held constant.** The corpus is consumed in a fixed
   order, so the 150k prefix contains documents the 20k prefix does not. No
   single-corpus design avoids this without repeating text, which would be its
   own artifact. All of it is the same genre.
5. **One model.** `qwen-3.8-flash-next` only.

---

## What follows for the plan

**T9's local branch stands.** With a warm cache, carrying history is close to
free (149,715 tokens reused, 0.8 s of prefill instead of 144.6 s), and now we
know it is also
close to free *in quality* out to 150k. So locally the trigger policy is
**"compact when you must, as late as possible"** and §10's leveled design is
paying for itself only at the wall and under KV memory pressure — not for
quality. Compaction cannot bill itself to quality below 150k.

**The metered branch is untouched by this.** There, history is rent per turn and
compacting early is still correct; that trade-off is about money, not quality,
and this experiment says nothing about it.

**One thing to carry forward that the plan did not anticipate:** short contexts
are *more* expensive per turn for this model, not less — 4× the generated tokens
and a 12.5% chance of not terminating inside 16k. If compaction replaces 150k of
history with a short summary, the next turn pays a cold prefill (already known)
**and** may re-enter the free-running regime the 0k control sits in. That is an
argument against aggressive compaction that the plan does not currently make,
and it is measured rather than assumed.

---

## Files

| file | what it is |
|---|---|
| `run.py` | the runner: one worker per depth, pinned slot, cached prefix |
| `filler.py` | corpus, chunking, conversation shaping, tokenizer-measured depth |
| `tasks.py` | the five task prompts |
| `tests_ref/*.rs` | the rig's own tests — the entire rubric |
| `reference_solutions/*.rs` | correct implementations; proof the tests are satisfiable |
| `score.py` | code-block extraction, `cargo build`, `cargo test` |
| `check_corpus.py` | trap guard: no task identifier leaks into the filler |
| `analyze.py` | aggregation, Wilson intervals, Fisher exact |
| `raw/samples.jsonl` | every sample: prompt metadata, timings, verdict, model output |
| `raw/manifest.json` | server props, depths, slot map, sampling params |
| `raw/run.log` | the live run log |
| `results.json` | the aggregated numbers behind every table above |
| `abandoned/` | two earlier partial runs (6k token cap; sequential) — not results |

Reproduce with `python3 run.py && python3 analyze.py` against the same server.
