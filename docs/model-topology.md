# What the three models on this box actually look like

Every number here was read out of the GGUF files - headers and per-layer tensor lists,
across all shards - not taken from a model card. `experiments/model-topology/split.py` (operator's private archive, `hands-off/letibot/`)
re-derives all of it in about a second, with no dependencies.

Companion to `docs/qwen-attention.md`, which this corrects in two places: the 12/36 split
is measured here rather than borrowed, and its "dilution" account of Flash-Next's
attention is incomplete (§2). Memory costs from these headers are in
`docs/glm-and-dense-attention.md`.

The dense 27B is the control model for that question: it differs from Flash-Next in
exactly the two dimensions under suspicion - no sparse indexer, dense FFN - and matches it
on the third (the same ~25/75 ratio, the same chat template). `docs/closed-loop.md` §10's
falsifier on both separates the two: decay on Flash-Next alone means **selection**, decay
on both means the **linear state**, which re-anchoring cannot recover.

## 1. The three, side by side

| | Qwen3.8-27B **dense** | Qwen3.8-Flash-Next | GLM-5.3-Flash |
|---|---|---|---|
| arch id | `qwen35` | `qwen4exp` | `glm5next` |
| blocks: full-attn / linear | 65: 17 / 48 | 49: 13 / 36 | 46: 12 / 34 |
| full attention at | `[3, 7, 11, ... 59, 63, 64]` | `[3, 7, 11, ... 43, 47, 48]` | `[3, 7, 11, ... 39, 43, 45]` |
| heads (q / kv) | 24 / 4 | 24 / 2 | 64 / 1, MLA |
| sparse indexer | none | `top_k 2048`, 4 heads | `top_k 2048`, 32 heads, kpool 4 |
| FFN | **dense**, 17408 | MoE 512, 10 used | MoE 288, 8 used + 1 shared |
| context | 262,144 | 262,144 | **1,048,576** |
| RoPE | 64 dims, base 1e7, sections `[11,11,10,0]` | same | **`rope.dimension_count = 0`** |
| extras | none | PLE n-gram table (16 heads x ~20M vocab) | hyper-connections x4, sinkhorn 20; MLA `kv_lora_rank 512` |

The highest block in each is the MTP/`nextn` layer (`nextn_predict_layers = 1` in all
three), so the serving stacks are 16/64, 12/48 and 11/45: one full attention layer in
four, three quarters linear. A family decision, not a per-model tuning. (`split.py`
classifies a block by `ssm_*` tensors: every block carries `attn_qkv`, so classifying on
`attn_` would call all 65 of the dense model's blocks attention.)

## 2. `indexer.top_k = 2048` is a third fading mechanism

Both Flash-Next and GLM carry a sparse attention indexer whose `top_k` is **2048, a
constant.** Each query attends to at most 2048 selected keys however long the context is.

| mechanism | where | what happens to an old rule | recoverable by placement? |
|---|---|---|---|
| **dilution** | dense attention | attended, with less mass | yes |
| **selection** | sparse indexer, `top_k 2048` | **not attended at all** | yes - *if* it can be made selectable |
| **overwriting** | linear/SSM state | compressed out of a fixed state | **no** |

Selection is a hard gate, not a soft weight, and `top_k` is fixed while context is not, so
selection pressure grows linearly with depth:

| context | fraction of it a query may attend |
|---|---|
| 8k | 25% |
| 32k | 6.3% |
| 150k | **1.4%** |
| 262k (max) | 0.78% |
| 1M (GLM max) | **0.195%** |

At 150k, 98.6% of the conversation is invisible to any given query in the attention
layers, and the only other pathway is the linear three quarters, whose state is a fixed
size. That explains an abruptly unbinding rule better than dilution does.

## 3. Two smaller things

**GLM has no RoPE** (`rope.dimension_count = 0`), so position-based reasoning about GLM
prompts does not transfer from measurements taken on Qwen.

**Flash-Next's PLE table is confirmed here:** `ngram_size 3`, `heads_per_ngram 8`, 16
heads of ~20M vocab each - the lazy-read table behind the 50 GB saving, not speculative
decoding.

## 4. UNVERIFIED

1. **That `top_k 2048` is applied per query at serving time** as the header declares. Read
   from metadata; not confirmed against llama.cpp's `glm5next`/`qwen4exp` implementation,
   which may clamp it, disable it below some length, or ignore it.
2. **What the indexer selects on.** `kpool 4` and `indexer.key_length 128` say there is a
   learned scoring path; nothing says whether it favours recency. §3's "make the rule
   selectable" has no mechanism behind it yet.
3. **Attention sinks and lost-in-the-middle** remain borrowed from the literature and
   unmeasured on these models, as `docs/qwen-attention.md` §5 already says.
4. **The 74% linear share is a layer count, not an information share.** How much of what
   the model uses flows through the linear path is not something a layer count settles.
5. **The top block in each is inferred to be the MTP layer** from `nextn_predict_layers =
   1` plus its position, not from a tensor that names itself as such.
