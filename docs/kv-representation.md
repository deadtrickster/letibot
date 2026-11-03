# The KV representation: what this tree holds, what it does not, and the one question T19 leaves

`TODO.md` T19 asks what would require a *rewrite* of the KV representation and whether that
rewrite pays, rather than arguing about llama.cpp's data structures. This is the answer
written from the code, with the two rows T19 already settles taken as settled
(`experiments/kv-stitching/RESULTS.md`, T14) and the third examined against what this tree
actually sends.

The short version. **This tree holds no KV at all.** It holds a *prompt* representation that
is addressable, append-only and content-addressed at its base, plus a *cost model* of a KV it
does not own. The KV itself lives in `llama-server` and is per-sequence. So "rewrite the KV
representation" is not a change this repository can make; the only questions left are (a)
which request shapes this tree sends whose prompt *shrinks*, and (b) whether anything on the
server can make a shrink cheap. There is exactly one such shape, it is a local compaction,
and nobody has measured what it costs.

## 1. What the representation is now

### 1.1 The wire: a full token array, every turn

`POST /completion` with `prompt` as a token array and `cache_prompt: true`
(`crates/turn/src/completion.rs:21`, `:54`). There is no handle, no span, no block index and
no shared memory: §3.3's `memfd` region is written and handed out read-only for a *reader*
(`crates/tokencore/src/region.rs:286`) but nothing submits through it — `readonly_fd` is reached
only through the ledger's wrapper and that wrapper has no caller outside its own tests. So
every request re-states the entire prompt, and the server works out for itself how much of it
it already holds.

### 1.2 The prompt: append-only by construction

- `TokenRegion` (`crates/tokencore/src/region.rs:1`) is a sealed `memfd`, one per transcript,
  with deliberate no-ops for `truncate`, `clear`, `pop` and insert-at-index. A conversation
  cannot be shortened in place; it can only be appended to, or a new region opened.
- `TokenLedger` (`crates/tokencore/src/ledger.rs:1`) chains `h_k = H(h_{k-1} ‖ tokens(item_k))`
  from `h_init = H(prefix tokens)`, and `PromptSpan` (`:35`) has no offset field at all —
  `offset()` is always zero, because "a request is a prefix of the region or it is not a
  request" (`:40`). The chain head is the `prefix_handle` §3.10-B specifies; it is recorded in
  every `turn_metrics` row (`crates/turn/src/metrics.rs:25`) and asserted by nothing on the
  server side, because the fallback path cannot.
- The engine submits the region's tokens plus an uncommitted generation prompt, and commits
  only what the turn produced (`crates/turn/src/engine.rs:1`).

So the representable requests are: the whole region, or the whole region extended. Nothing in
this tree can express a prompt that is a strict prefix of the region it just sent.

### 1.3 The one reuse unit: `stable_prefix`

`stable_prefix` (`crates/tokencore/src/store.rs:210`) is content-addressed over
`(dialect_sha, system, tools_json)` — plus the byte vocabulary's source when the vocabulary is
the byte fallback — and stores the *tokens* and `h_init`. A new session with the same dialect,
system text and tool set inherits a warm prefix for free; the schema's own comment records the
measurement: *"sharing one prefix took an identical request from 9,325 tokens processed to 4"*
(`:206-209`). A transcript references one (`:294`), a compaction fork can carry a different one
(`Harness::fork_onto`, `crates/harnessd/src/harness/wall.rs:1436`), and a re-seat exists to hand
one in.

That is the whole of the tree's block vocabulary: **one block, the prefix**, keyed by a hash of
the things that render it. There is no `segment` table (§19.3 claims there is; `grep segment
crates/tokencore/src/store.rs` finds only the `segment_mark` item kind), and no addressable
unit below the transcript.

### 1.4 What the tree believes about the cache

Two pieces, and the difference between them matters.

- **Measured and reported**: the post-flight prefix invariant. `PrefixWitness`/`PrefixCheck`
  (`crates/turn/src/prefix.rs`) re-hashes the previous turn's prompt-plus-generation against
  this turn's first tokens; a divergence is a defect here, and a *shortfall* — the invariant
  held but the server reused less — is reported as `cache_reuse_shortfall` and marked
  `diagnostic_only` (`:81`), so it reaches the debug channel and not the operator. Per-turn
  numbers (`prompt_tokens`, `cached_tokens`, `prompt_processed`, `id_slot`) are written to the
  store and the last pair is mirrored onto the session row as `context_tokens` /
  `context_cached` (`crates/tokencore/src/store.rs:234`).
- **Modelled and never called**: `PromptCache` (`crates/turn/src/prompt_cache.rs`). It prices
  one cache entry as `111 MiB + 24 KiB × tokens` (`:23`, `:26`, `:34`) against a budget that
  defaults to llama.cpp's own `8 GiB` (`:31`), and evicts LRU. Nothing outside its own tests
  constructs it: `grep PromptCache crates` finds no caller. It is a correct model of a budget
  the tree does not enforce, and it is where the honest numbers live —
  `entry_bytes(262_144)` is about **6.1 GiB**, so at the model's own ceiling the default
  8 GiB budget holds *one* entry.

### 1.5 What `crates/llama` links

The vocabulary, and only the vocabulary: `llama_model` loaded with `vocab_only = true`
(`crates/llama/src/lib.rs:4`) and `ffi.rs` declaring `llama_tokenize`,
`llama_token_to_piece` and the `llama_vocab_*` accessors, no memory, batch or context symbol
at all (`crates/llama/src/ffi.rs:27-63`). This crate is the only one linking llama.cpp, and it
links it for the *tokenizer*.

### 1.6 The capability the tree claims for its server

`BackendCaps::OWN_SERVER` promises `PrefixGuarantee::Structural` (`crates/backend/src/lib.rs:215`),
and `may_assert_structural_prefix` is what lets the invariant run at all. A metered API skips
loudly. Nothing in the caps says anything about *rewind*, because nothing in the tree asks for
one.

## 2. What UNVERIFIED-16 asked, and what it now answers

UNVERIFIED-16 asked whether a bounded recompute can repair a KV computed over a different
history, on a hybrid attention+recurrent model. `experiments/kv-stitching/RESULTS.md` answers
no, and the reason is structural rather than numerical:

| | measured |
|---|---|
| attention half | 12 layers × 2 KV heads × 512 × f16 = **24 KiB per token**, cells that carry a position |
| recurrent half | 36 layers × (6144·128 + 6144·4) × f32 = **111.4 MiB per sequence**, fixed, `cells` *are* sequence ids |
| a slot save after a 64-token prompt | 120,206,104 bytes — 111.4 of it the accumulator |
| recompute fractions | flat at *wrong* for every *k*, then bit-identical at 100 %: 98 % recompute still diverges at token 5 (§6c) |
| noise floor | 200/200 in 36 consecutive repeats at about 1,470 tokens of prompt, so those divergences are signal (§6a) |

and the practical form of it, from `truncate.py` at N ≈ 3,850 and again at 12,300: re-entry has
**three fixed points** — `d ≤ 3` free, `4 ≤ d ≤ 516` one checkpoint, `d > 516` the entire prefix
from token 0 (`RESULTS.md` §4).

So T19's rows 1 and 2 are settled against a rewrite: composition across a divergence is
mathematically blocked on this stack, and splitting attention from recurrent in the save format
buys nothing because after a divergence every one of the 48 layers differs anyway. §19.3's
"append, do not swap" survives unchanged, and 516 is a checkpoint-schedule fact
(`checkpoint_offsets[] = {4 + n_ubatch, 4}`, `n_ubatch` 512), not a property of the model.

## 3. What the open question actually is

Row 3 in T19 is "checkpoint density / rewind granularity", and it is written as *pure
implementation, worth doing*. Against the code the question is sharper than that, and it has
two halves.

**First: which request does this tree ever send whose prompt shrinks?** A `d > 0` rewind needs
a new prompt that is a strict prefix of one the server holds. The region cannot be truncated,
there is no rewind, backtrack or delete-turn operation anywhere in the tree, and a failed turn
leaves the region unchanged. Every ordinary turn is an *extension*. The one exception:

> a local compaction. `plan_compaction_tail` returns `TailPlan::None` when `!remote`
> (`crates/turn/src/compaction.rs:947`) — deliberately, because *"a local model gets the template
> and nothing else, because a tail here would be resident tokens competing with the KV pressure
> that called this compaction"* (`crates/harnessd/src/harness/wall.rs:1171`). So the new base is
> **stable prefix + one summary item**, and the pre-compaction entry was **stable prefix +
> the whole history + the generation**. Their longest common prefix is the stable prefix.

So the re-entry point a post-compaction prompt lands on is the *end of the stable prefix* — the
prefix's own length into a prompt that is hundreds of thousands long. Under
`RESULTS.md` §4's schedule that is not a checkpoint: a chat request's checkpoints sit at
user-turn boundaries thinned exponentially *from the tip* (about 8k/16k/32k/64k/128k back).
**The arithmetic says a local compaction's first turn re-prefills from token 0, stable prefix
included.** That is a prediction, not a measurement — and it is the thing to measure, because
it means the tree's central caching claim ("the prefix is warm, 9,325 tokens → 4") holds for a
new session seated on the same prefix and *not* for a session that has just compacted.

**Second: could the tree see it if it happened?** Almost not. `session.witness` lives on the
`Session` (`crates/turn/src/engine.rs:408`), a fork opens a *new* engine session, so the first
turn of a new base is `PrefixCheck::FirstTurn` — no warning, and `Skipped`/`FirstTurn` are
deliberately not passes (`crates/turn/src/prefix.rs:74`). The numbers are still in
`turn_metrics`, so the measurement is a query away; nothing surfaces it today.

**And the harness knows the one number the server cannot guess.** `TokenLedger::prefix_len()`
(`crates/tokencore/src/ledger.rs:250`) is the exact token offset where the stable prefix ends —
the position a post-compaction prompt re-enters at, and the position §3.4.1 ("checkpoint
placement stops being a heuristic, since the harness emitted the tokens the server recovers by
string-matching delimiters") is about. That reframes row 3: what it should buy is not *more*
checkpoints uniformly but **one checkpoint at the prefix boundary**, which is a placement
argument and not a representation one.

## 4. The evidence, both ways

**That the cliff is live and worth buying.**

- The compression above: the only shrinking prompt in the tree is a local compaction, and its
  common prefix is the stable prefix, where no checkpoint is due to exist.
- The cost of being wrong is large and measured in two places: a miss is *linear in the suffix*
  — ~11 s at the tail and ~453 s at the head of 200 k (`docs/implementation-plan.md` §19.3) —
  and a cold prompt against a warm one measured 144.6 s against 0.8 s (T14, `TODO-settled.md`).
  F1's own numbers say the same from the other side: with replay off, `f_keep` p10 0.000 and p99
  re-prefill 78,598 tokens; on, 0.999 and 5,294 (`docs/implementation-plan.md` F1).
- Compaction is not rare in a deep session: it is what keeps a 262 k session alive, and on the
  local path it is *always* a shrink.
- The tree's own budget model already says the pre-compaction entry cannot survive beside the
  new base at depth: 6.1 GiB per entry at the ceiling against an 8 GiB default
  (`crates/turn/src/prompt_cache.rs:31`). So the entry that would have been matched is evicted
  by the same act that makes the match interesting.

**That it is not worth buying, or not the thing to buy.**

- Nobody has measured a post-compaction turn. Every number above is either a server fact at
  the *tip* of a prompt or arithmetic about where checkpoints are due. The harness's own
  `cache_reuse_shortfall` has never been observed across a fork, because the fork resets the
  witness.
- The compaction turn that produces the summary is itself *warm* — it is an in-place append
  (`crates/turn/src/compaction.rs:1`), and I5 requires `f_keep ≥ 0.99` for it. So the expensive
  half of a compaction already reuses everything; what is cold is the small prompt that follows,
  whose own size is the stable prefix plus a summary plus the next user turn. That is cheap only
  if the prefix is: a seat whose brief and tool schemas run to tens of thousands of tokens pays
  that bill in full, which is why the measurement decides the row and this paragraph cannot.
- Checkpoints are not free and they are charged against the failure this tree has actually
  had: 145 MiB each in the prompt cache (`RESULTS.md` §4), and every distinct prompt prefilled
  is an entry at ~120 KiB/token (T18). Densifying checkpoints is exactly the direction that
  caused the fifth and sixth OOMs.
- The tree has already chosen the cheapest form of the same money: `--checkpoint-min-step` is
  a server knob, not code here, and §19.3 already concludes *append, do not swap*.

**What is settled regardless.** Rows 1 and 2 are not re-openable on this stack. Any "rewrite"
that survives is a rewrite of `llama_memory_recurrent` itself — a chunked or checkpointed fold,
inside llama.cpp — which is a fork of the memory module plus per-model work for three models
plus the fusion coupling §3.5 warns about, and it would buy the same thing a placement
parameter buys: cheaper re-entry. `get_can_shift()`'s own comment (`llama-memory-recurrent.h`,
quoted at `RESULTS.md` §2) says the recurrent state is position-independent and content-bound:
the position trick bought nothing because position was never the wall.

## 5. The cost of each option

| option | what it costs | what it buys |
|---|---|---|
| **do nothing** (today) | one cold prefill of *stable prefix + summary + the next user turn* per local compaction — bounded, but the prefix is not small for every seat | nothing to build; the 453 s case is never reached, because no prompt in the tree is a long-suffix trim |
| **row 3, a denser checkpoint schedule** (`--checkpoint-min-step` down) | 145 MiB per checkpoint against the same pool T18's OOM came out of; more per-entry bytes on every prompt, not just compaction's | re-entry points closer to the tip — which the tree does not need — and, only if a checkpoint happens to land at the prefix boundary, a warm prefix after each compaction |
| **row 3', a checkpoint *at the prefix boundary*** | one checkpoint per entry, 145 MiB, at a position the harness knows exactly | the post-compaction turn and every re-seat becomes a prefix hit instead of a cold start |
| **row 2, split the save format** | nothing — it is refused | nothing: after a divergence all 48 layers differ |
| **row 1, compose across a divergence** | nothing — refused, twice measured | nothing on a hybrid; "usually still answers" is not a property a §3.8 harness may use |
| **rewrite the memory module** (chunked fold inside llama.cpp) | a fork of `llama_memory_recurrent` and `llama_memory_hybrid`, three models to keep, the fusion coupling of §3.5, and a re-run of the whole §6 measurement series | cheaper re-entry — the same purchase as row 3', at perhaps a hundred times the price |
| **an append-only compaction** (never shrink: keep the history and append a fold) | the resident context never comes back down, which is what compaction exists to fix | a permanently warm prompt; it is §19.3's "append, do not swap" taken literally, and it is why §19.3 pairs that advice with compaction rather than with swapping |
| **measure it first** | one afternoon, no code | the answer to whether any row above is worth its price |

## 6. Recommendation

**Do not rewrite.** Rows 1 and 2 are measured negatives on this stack; the only rewrite left
would re-buy at great cost what a placement parameter buys at one flag.

**Do not build row 3 as written either**, until one measurement exists. Row 3 assumes the
`d > 516` cliff is something the harness falls off. The code says the harness never asks for a
long-suffix trim except at a compaction, and at a compaction the prompt that has to be
re-entered is *small*. Where the money actually goes is the prefix boundary, and that is
§3.4.1 — a checkpoint placed where the harness says the stable prefix ends.

**Carry the witness across a fork.** It is three lines and it makes the next compaction
disclose its own reuse (`cache_reuse_shortfall` today cannot fire on the first turn of a new
base). This is measurement, not KV, and it is the precondition for the experiment below
paying anything.

### The one experiment that settles it

Two arms, one depth sweep, no new code, and every number already recorded by the daemon.

1. **Setup.** A session against the local server on a scripted prompt, repeated until the
   resident ledger is at a stated depth — 40 k and 120 k, one run each. Guard the box with
   `experiments/kv-stitching/srv.py`'s `guard(floor_gib=40.0)`; the run prefills several distinct
   prompts and that guard exists because of the fifth OOM.
2. **Arm A, the server as it runs today.** Let the session compact (drive it over the configured
   window, or force it), then take the next turn. Read from the store, per turn:
   `prompt_tokens`, `cached_tokens`, `prompt_processed`, `id_slot`
   (`crates/turn/src/metrics.rs`); and from the server's own accounting the prompt cache's size —
   its `cache now N entries / X MiB` log line is the number T18 was found in, and `/slots` gives
   the per-slot view.
3. **Arm B, `--checkpoint-min-step` lowered to a stated value** (e.g. 1024), on a second server
   instance — **this needs the operator**: the model server on :8080 is protected and is never
   restarted for a measurement (`agents.md`, *How it runs on this box*). The same session,
   the same depth, the same read.
4. **The pre-registered readings.** Arm A's post-compaction turn has `cached_tokens` of
   **about the stable prefix** → the cliff is not live, T19 closes as *do not rewrite, do not
   build row 3*, and the doc's §3 prediction is wrong. Arm A's `cached_tokens` of **about
   zero** → the prediction holds, the tree loses its prefix on every local compaction, and
   row 3 becomes worth building **in the placement form** (row 3'), whose own number is then
   Arm B's: does any `--checkpoint-min-step` value put a checkpoint at the boundary, and what
   does that add to the cache's bytes?
5. **What it cannot settle**, and will not be claimed: GLM (not loadable beside Qwen), depths
   above 12 k on a shared server (§6a's noise floor is only 200/200 near 1.5 k), and §3.10-C's
   per-layer numerics, which need a `libllama` harness and both GPUs free.

One run, one query, and either T19 closes or it has a task in it that this tree can do.
