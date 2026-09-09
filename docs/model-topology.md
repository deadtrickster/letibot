# What the three models on this box actually look like

Written 2026-09-09. **Every number here was read out of the GGUF files**, headers and
per-layer tensor lists, across all shards — not recalled and not taken from a model card.
`experiments/model-topology/{kv.py,split.py}` re-derive all of it in about a second and
need no dependencies, which is the point: the layout is a fact about a file on disk, and
a fact you can re-read is not a fact you have to remember.

Companion to `docs/qwen-attention.md`, **which this corrects in two places** (§6).
What the layout *means* for memory is `docs/glm-and-dense-attention.md`, which computes
the KV and state costs from these headers and reproduces three independently measured
fleet numbers in the process.

---

## 1. The measurement

`split.py` classifies a block by its tensors: a block carrying `ssm_*` is a linear /
gated-delta block, one without is full attention. This matters because **every block in
all three models carries `attn_qkv`** — the linear blocks reuse the projection and run the
recurrence over it. Classifying on the presence of `attn_` would have said "65 of 65
attention layers", which is how this nearly went wrong.

```
Qwen3.8-27B dense      blocks  65   FULL-ATTENTION  17 (26%)   LINEAR/SSM  48 (74%)
                       full attention at [3, 7, 11, ... 59, 63, 64]
Qwen3.8-Flash-Next     blocks  49   FULL-ATTENTION  13 (27%)   LINEAR/SSM  36 (73%)
                       full attention at [3, 7, 11, ... 43, 47, 48]
GLM-5.3-Flash          blocks  46   FULL-ATTENTION  12 (26%)   LINEAR/SSM  34 (74%)
                       full attention at [3, 7, 11, ... 39, 43, 45]
```

The highest index in each is the MTP/`nextn` layer (`nextn_predict_layers = 1` in all
three), so the serving stacks are 16/64, 12/48 and 11/45. **Every one of them is one full
attention layer in four, and roughly three quarters linear.** That ratio is a family
decision, not a per-model tuning.

## 2. The three, side by side

| | Qwen3.8-27B **dense** | Qwen3.8-Flash-Next | GLM-5.3-Flash |
|---|---|---|---|
| arch id | `qwen35` | `qwen4exp` | `glm5next` |
| blocks (full-attn / linear) | 64 (16/48) | 48 (12/36) | 45 (11/34) |
| `full_attention_interval` | 4 | 4 | — (every 4th, +45) |
| heads (q / kv) | 24 / 4 | 24 / 2 | 64 / 1, MLA |
| **sparse indexer** | **none** | `top_k 2048`, 4 heads | `top_k 2048`, 32 heads, kpool 4 |
| FFN | **dense**, 17408 | MoE 512, 10 used | MoE 288, 8 used + 1 shared |
| context | 262,144 | 262,144 | **1,048,576** |
| RoPE | 64 dims, base 1e7, sections `[11,11,10,0]` | same | **`rope.dimension_count = 0`** |
| extras | — | PLE n-gram table (16 heads × ~20M vocab) | hyper-connections ×4, sinkhorn 20; MLA `kv_lora_rank 512` |

## 3. The finding that matters: `indexer.top_k = 2048`

Both Flash-Next and GLM carry a **sparse attention indexer**, and its `top_k` is **2048 —
a constant.** Each query attends to at most 2048 selected keys, however long the context
is.

That is a **third fading mechanism**, and it is not dilution:

| mechanism | where | what happens to an old rule | recoverable by placement? |
|---|---|---|---|
| **dilution** | dense attention | attended, with less mass | yes |
| **selection** | sparse indexer, `top_k 2048` | **not attended at all** | yes — *if* it can be made selectable |
| **overwriting** | linear/SSM state | compressed out of a fixed state | **no** |

Selection is a hard gate, not a soft weight. And because `top_k` is fixed while context is
not, **selection pressure grows linearly with depth**:

| context | fraction of it a query may attend | 
|---|---|
| 8k | 25% |
| 32k | 6.3% |
| 150k | **1.4%** |
| 262k (max) | 0.78% |
| 1M (GLM max) | **0.195%** |

At 150k, 98.6% of the conversation is invisible to any given query in the attention
layers — and the *only* other pathway is the linear three quarters, whose state is fixed
size. This is a much better account of "signal fades at the edges" than dilution, and it
was sitting in the file header the whole time.

## 4. The dense 27B is an experimental control, and we own it

It differs from Flash-Next in exactly the two dimensions under suspicion, while matching
it on the third:

- **no sparse indexer** — its 16 attention layers see everything
- **dense FFN** — no expert routing, so every parameter sees every token
- **same ~25/75 attention/linear ratio**, and a byte-identical chat template

So running `docs/closed-loop.md` §10's falsifier on **both** separates two hypotheses that
are otherwise confounded:

- if adherence decays on Flash-Next and **not** on the dense 27B → the mechanism is
  **selection**, and re-anchoring works by making the rule selectable.
- if it decays on **both** → the mechanism is the linear state, which both share at 74%,
  and re-anchoring will plateau below the fresh baseline on both.

That is a better-designed experiment than the one I commissioned, and it costs a second
model that is already on disk.

## 5. Two smaller things worth knowing

**GLM has no RoPE.** `rope.dimension_count = 0`. Position is not encoded the way it is in
the Qwen pair, which means position-based reasoning about GLM prompts does not transfer
from measurements taken on Qwen, and the arXiv 2608.03893 RoPE-stripping result is
addressing an obstacle GLM does not have.

**Flash-Next's PLE table is confirmed here.** `ple.layers = [1]`, `ngram_size 3`,
`heads_per_ngram 8`, 16 heads of ~20M vocab each — the lazy-read n-gram table behind the
50 GB saving, and it is emphatically not speculative decoding. `nextn_predict_layers = 1`
is the separate MTP layer, present in all three.

## 6. Corrections to `docs/qwen-attention.md`

1. Its UNVERIFIED item 5 said the 12/36 split was read from another document rather than
   an architecture dump. **It is now measured**, from the tensor lists, and it is right:
   12 full attention and 36 linear over 48 serving blocks.
2. Its §1 presents Flash-Next's attention half as **dilution**. That is incomplete —
   Flash-Next has a `top_k 2048` indexer, so its attention layers *select* before they
   dilute. §3 above is the correction, and it strengthens the doc's conclusion rather than
   weakening it: a hard selection gate is a better explanation of an abruptly unbinding
   rule than a soft weight ever was.

## 7. UNVERIFIED

1. **That `top_k 2048` is applied per query at serving time** as the header declares. Read
   from metadata; not confirmed against llama.cpp's `glm5next`/`qwen4exp` implementation,
   which may clamp, disable it below some length, or ignore it.
2. **What the indexer selects on.** `kpool 4` and `indexer.key_length 128` say there is a
   learned scoring path; nothing here says whether it favours recency, or what makes a
   token selectable. §3's "make the rule selectable" has no mechanism behind it yet.
3. **Attention sinks and lost-in-the-middle** remain borrowed from the literature and
   unmeasured on these models — as `docs/qwen-attention.md` §5 already says.
4. **The 74% linear share is a layer count, not an information share.** How much of what
   the model *uses* flows through the linear path is not something a layer count settles.
5. **Blocks 64 / 48 / 45 are inferred to be the MTP layer** from `nextn_predict_layers = 1`
   plus their position, not from a tensor that names itself as such.
