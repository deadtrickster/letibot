# Memory and attention in GLM-5.3-Flash and the dense Qwen3.8-27B

Written 2026-09-09, the companion `docs/qwen-attention.md` owed for the other two models
on this box. Layout comes from `docs/model-topology.md` (read out of the GGUF files); this
document is what that layout means for **what a model can still see of an old turn.**

---

## 0. Why believe the arithmetic

Everything below is computed from header fields. Three of those computations land on
numbers this fleet measured independently, months apart and by other means:

| computed from the header | independently measured | source |
|---|---|---|
| Flash-Next KV: `2 kv × (256+256) × 2 B × 12 layers` = **24 KiB/token** | **24 KiB/token** | `docs/compaction.md` §5 |
| Flash-Next SSM state: `6144 × 128 × 4 B × 36` + conv = **110.6 MiB** | **111.4 MiB** | UNVERIFIED-16 |
| GLM KV: MLA `512 × 2 B × 11` + indexer `32 × 128 × 2 B` = **19 KiB/token** | **19.25 KiB/token, "incl the DSA indexer"** | `glm-kv-and-seq-costs` |

Three for three. The headers are being read correctly, so the numbers that have *not*
been measured are worth something too.

## 1. The dense Qwen3.8-27B — the clean case, and the control

**16 full-attention layers of 64, 48 linear. No sparse indexer. No expert routing.**

It is the only model here whose attention layers see the entire context. Its fading has
exactly two mechanisms rather than four:

- **dilution** in the 16 attention layers — everything retained, mass per token falls
- **overwriting** in the 48 linear layers — state `6144 × 128` per layer, so
  **≈147 MiB fixed**, independent of length

Note the direction of that second number: the dense 27B's accumulator is **larger** than
Flash-Next's (147 vs 111 MiB) because it has more linear layers, so it compresses history
*less* aggressively per token. If the linear state is what loses rules, this model should
lose them more slowly — a prediction, and a cheap one to check.

**What it costs to remember, though, is steep.** 4 kv heads against Flash-Next's 2, and
16 attention layers against 12:

| | per token | at 150k | at its 262k ceiling |
|---|---|---|---|
| dense 27B | **64 KiB** | 9.6 GiB | 16.0 GiB |
| Flash-Next | 24 KiB | 3.6 GiB | 6.0 GiB |

**2.7× the KV for the same conversation.** That is the price of dense attention, and it is
the honest reason the sparse models exist. A harness that treats "context depth" as one
number across models is wrong by that factor in its memory planning.

**Also multimodal**: `rope.dimension_sections [11,11,10,0]` is mRoPE — three positional
sections, not one — and there is an `mmproj` beside it. Position in this model is a
*triple*, and any positional reasoning that assumes a scalar index is reasoning about a
model that is not this one.

## 2. GLM-5.3-Flash — four lossy mechanisms stacked

**11 full-attention of 45, 34 linear, MLA, a `top_k 2048` indexer, and no RoPE at all.**

Every one of those is a place information goes:

| # | mechanism | where | recoverable? |
|---|---|---|---|
| 1 | **compression at write** | MLA — KV is stored as a **rank-512 latent**, never as full keys and values | no, and it is *uniform* |
| 2 | **selection** | indexer `top_k 2048`, 32 heads | yes, if the token can be made selectable |
| 3 | **dilution** | within the selected 2048 | yes |
| 4 | **overwriting** | 34 linear layers | no |

Mechanism 1 is new relative to the Qwen pair and it is different in kind: **it is lossy at
write time and it hits every position equally.** It does not create a gradient between old
and recent material — it lowers the ceiling for all of it. So MLA is not a cause of
*fading*; it is a cause of the model being less precise about everything it attends to.

### The indexer costs more than the attention it makes cheap

Decomposing the measured 19.25 KiB/token:

```
MLA latent      512 dims × 2 B × 11 layers  = 11 KiB/token   (57%)
DSA indexer     32 heads × 128 × 2 B        =  8 KiB/token   (42%)
```

MLA compresses attention KV to **1 KiB per layer per token** — against Flash-Next's 2 and
the dense 27B's 4 — and then the machinery that decides *which* 2048 tokens to attend to
costs nearly as much as all eleven attention layers combined. That is a real design
tension and it is invisible without the split.

At GLM's 1,048,576-token ceiling: **≈19.3 GiB of KV**, plus the 436.7 MiB *per sequence
id* charged at allocation that this box already pays for.

### No RoPE, and what that implies

`rope.dimension_count = 0`. GLM encodes no rotary position in its attention layers.

The causal mask still gives order a *direction*, but the fine-grained "how far back" signal
has to live somewhere, and the only sequential machinery left is the **34 linear layers** —
whose state is the fixed-size, lossily-overwritten part. So on GLM the hypothesis is
sharper than on Qwen:

> **Position information itself is stored in the lossy path.**

If true, GLM should degrade differently: not "the rule is still there but unattended", but
"the *ordering* of old material blurs first." That is a distinct, falsifiable prediction and
it is not the same failure the Qwen falsifier is looking for.

It also retires an old note. `docs/compaction.md` §5 says arXiv 2608.03893's RoPE-stripping
"solves obstacle 1". For GLM there is no obstacle 1 to solve — there is no RoPE to strip.

### One measured hazard that is GLM's alone

`cache-reuse-corrupts-recurrent`: on GLM, `--cache-reuse` **does** shift, and the restored
checkpoint holds recurrent state computed from the turn that was deleted. **Wrong answers,
fast.** That is not fading — it is memory corruption with no error, and it is the strongest
argument in the fleet for why a harness must know which model it is talking to. The same
flag is harmless elsewhere.

## 3. The three together

| | dense 27B | Flash-Next | GLM-5.3 |
|---|---|---|---|
| full-attn / linear | 16 / 48 | 12 / 36 | 11 / 34 |
| **sees whole context?** | **yes** | no — `top_k 2048` | no — `top_k 2048` |
| KV per token | **64 KiB** | 24 KiB | 19.25 KiB |
| KV compressed at write | no | no | **yes, rank 512** |
| fixed linear state | **≈147 MiB** | 111.4 MiB | not derivable (§4.2) |
| position encoding | mRoPE, 3 sections | mRoPE, 3 sections | **none** |
| fading mechanisms | 2 | 3 | 4 |

**The ranking is the useful part.** For *fidelity of old material* the dense 27B is
strictly the best of the three and pays 2.7× the memory for it. For *capacity* GLM is best
and is lossy in four places. Flash-Next sits between and is what this box serves.

## 4. What follows for the harness

1. **"How deep is this conversation" is not one number.** 150k tokens is 9.6 GiB on one
   model and 3.6 GiB on another, and means *whole context attended* on one and *1.4%
   attended* on the others. Budgeting, spill and compaction triggers that ignore the model
   are wrong by multiples.
2. **The re-anchoring fix is model-dependent.** On the dense 27B there is nothing to
   re-select — an old rule is already attended, merely diluted, so placement should help
   least there. On the two sparse models it should help most. That is a sign flip, not a
   magnitude change, and it makes the control model worth the second endpoint.
3. **On GLM, ask a different question.** Test whether *ordering* degrades, not whether a
   rule is retrievable. §2's NoPE argument says that is where its loss should show first.
4. **Per-model hazards belong in `BackendCaps`, not in a rule file.** `--cache-reuse` is
   safe on one of these and silently corrupting on another. A model-conditioned danger that
   lives in prose is the open-loop failure `docs/closed-loop.md` is about; it should be a
   capability the backend declares and the harness reads.

## 5. UNVERIFIED

1. **The MLA/indexer split of 19.25 KiB/token is arithmetic that lands on the measurement,
   not a separate measurement.** 11 + 8 = 19 against 19.25 is close enough to be persuasive
   and is not proof; `kpool 4` may change the indexer term, and the residual 0.25 is
   unexplained.
2. **GLM's linear state size is not derivable from its header** — it declares only
   `ssm.conv_kernel`, no `inner_size` or `state_size`. The 34-layer accumulator could be
   larger or smaller than Flash-Next's 111.4 MiB and nothing here says which.
3. **"No RoPE means position lives in the linear path" is an inference**, not a measurement.
   GLM may encode position some other way this header does not name.
4. **The dense 27B's ≈147 MiB accumulator is computed, never measured.** Flash-Next's
   equivalent computation matched to 0.7%, which is why it is stated at all.
5. **Whether llama.cpp applies `top_k 2048` at serving time** — carried over from
   `docs/model-topology.md` §7.1 and still open. If it is ignored, every selection argument
   in this document is about a model that is not running.
6. **11 vs 12 attention layers for GLM.** The KV arithmetic fits better at 11, which
   assumes the MTP layer at index 45 is not charged against the main sequence. Inferred
   from the fit, which is circular, and worth checking directly.
