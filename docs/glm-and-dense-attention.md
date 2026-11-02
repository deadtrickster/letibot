# Memory and attention in GLM-5.3-Flash and the dense Qwen3.8-27B

| | dense 27B | Flash-Next | GLM-5.3 |
|---|---|---|---|
| full-attn / linear | 16 / 48 | 12 / 36 | 11 / 34 |
| sees the whole context? | **yes** | no - `top_k 2048` | no - `top_k 2048` |
| KV per token | **64 KiB** | 24 KiB | 19.25 KiB |
| KV compressed at write | no | no | **yes, rank-512 MLA** |
| fixed linear state | ~147 MiB | 111.4 MiB | not derivable |

For **what a model can still see of an old turn** the layout in `docs/model-topology.md` ranks
the dense 27B first, Flash-Next between and GLM last, at 2.7x the memory for the first; for
capacity GLM is first and is lossy in four places.

## The dense 27B - the clean case, and the control

16 full-attention layers of 64, 48 linear, no sparse indexer: the only model here whose
attention layers see the entire context, so it fades in two ways - dilution in the 16,
overwriting in the 48 (~147 MiB fixed, larger than Flash-Next's 111). It pays **64 KiB per
token, 2.7x Flash-Next's 24** - 16.0 GiB against 6.0 GiB at their 262k ceiling.

## GLM-5.3-Flash - four lossy mechanisms

11 full-attention of 45, 34 linear, MLA, a `top_k 2048` indexer, and no RoPE. MLA is lossy **at
write time** - KV is a rank-512 latent - and hits every position equally, so it lowers the
ceiling for everything attended to rather than causing fading. The indexer then selects 2048
keys, dilution happens within that selection, and the 34 linear layers overwrite.

**The indexer costs more than the attention it makes cheap.** Of the measured 19.25 KiB/token,
MLA is 11 (57%) and the indexer 8 (42%): deciding *which* 2048 tokens to attend to costs nearly
as much as all eleven attention layers. At 1,048,576 tokens: ~19.3 GiB of KV, plus 436.7 MiB per
sequence id.

**No RoPE** (`rope.dimension_count = 0`): "how far back" has to live somewhere, and the only
sequential machinery left is the 34 linear layers, so **position itself is stored in the lossy
path** and GLM should lose *ordering* first.

**One hazard that is GLM's alone.** On GLM, `--cache-reuse` shifts cells and the restored
checkpoint holds recurrent state computed from the turn that was deleted: **wrong answers,
fast** - memory corruption with no error, where the same flag is harmless elsewhere.

## What follows for the harness

- **"How deep is this conversation" is not one number.** The same 262k ceiling is 16 GiB of KV
  on the dense 27B and 6 GiB on Flash-Next, and means *whole context attended* on one and 0.78%
  on the others.
- **Re-anchoring is model-dependent.** On the dense 27B an old rule is already attended, merely
  diluted, so placement helps least there and most on the two sparse models. On GLM, ask whether
  *ordering* degrades.
- **A per-model hazard belongs in the backend's declared capabilities**, not a rule file -
  `BackendCaps` has no field for `--cache-reuse` yet.

Not established: whether llama.cpp applies `top_k 2048` at serving time
(`docs/model-topology.md` §7.1), and GLM's linear state size, which its header does not declare.