<!-- abstract: MEASURED 2026-10-26 on the Mac: the vocabulary GGUF at ~/models/qwen3.8-flash-next/ cannot load because its architecture is `qwen4exp` and this llama.cpp (0.3.0, c1d0e7a) has no such arch — `llama_model::load_arch()` throws even for vocab_only. A shard-1-of-6 stub was the first shape; an unsplit re-pack fails the same way, and ggml-vocab-qwen35.gguf loads fine, which isolates the arch. -->

# The Mac's vocabulary GGUF exists and cannot load, because this llama.cpp has no `qwen4exp`

MEASURED 2026-10-26 on the Mac (`wt-qc`, `LETIBOT_REQUIRE_APPARATUS=1 cargo test --offline
--workspace --no-fail-fast`): 46 of 177 targets red, 248 tests red, and **222 of them are this
one file**. Everything below is measured, not inferred.

## The file, as it was at 09:34

```
-rw-r--r--  1 dead  staff  10946624 Oct 26 09:34 …/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf
```

Read with `llama-gguf <file> r` and with a 40-line python GGUF KV reader: a valid GGUF v3,
`n_kv: 67`, **`n_tensors: 0`**, `split.no`/`split.tensors.count`/`split.count = 6`, and the
tokenizer (tokens, merges, chat_template) sitting IN the KV — which is why 10.9 MB. The real
model's metadata says `general.name = "Qwen3.8 Flash Next"`,
`general.description = "A Preview of the Qwen4 Architecture"`, `general.size_label = 512x56B`,
and **`general.architecture = qwen4exp`**.

Shards 2–6 do not exist, and `llama_model_loader`'s CONSTRUCTOR opens every sibling shard
unconditionally (`src/llama-model-loader.cpp`, the `n_split > 1` loop), *before* `vocab_only` is
consulted — so `crates/llama/src/lib.rs`'s claim that "the other five shards never opened and
need not exist" is FALSE for this llama.cpp version.

## The arch is the real blocker — a second, unsplit re-pack proved it

While this was being measured another session re-provisioned the directory: at 09:42 the stub was
renamed `split-shard-1.gguf.orig` and a **`vocab-only.gguf.pending`** appeared (10 946 560 B, no
`split.*` keys at all, `n_tensors: 0`). `letibot_llama::load` on it still fails:

```
the vocabulary must load: Load { path: "/Users/dead/models/qwen3.8-flash-next/vocab-only.gguf.pending" }
```

and the same call on `~/Projects/llama.cpp/models/ggml-vocab-qwen35.gguf` **passes in 0.21 s**. So
the split is not the only cause: `llm_load_arch()` throws on `LLM_ARCH_UNKNOWN` even for
`vocab_only`, and `src/llama-arch.cpp` in this build (`c1d0e7a`, "bump version to 0.3.0") lists
qwen, qwen2, qwen2moe, qwen3, qwen3moe, qwen3next, qwen3vl, qwen3vlmoe, qwen35, qwen35moe — and
no `qwen4exp`.

**So the fix is not a re-pack of the stub**: either a llama.cpp that knows `qwen4exp` (what the
Linux box's `build-glm` must have, since these tests pass there), or a GGUF whose arch key is one
this build knows.

## What it breaks, and the shape of the red

`letibot_tokencore::apparatus::present_gguf` probes `p.is_file()`, so it says PRESENT; the test
then calls `letibot_llama::load` and panics `the vocabulary must load: Setup("loading the
vocabulary: libllama could not load a vocabulary from <path>")`. Under
`LETIBOT_REQUIRE_APPARATUS=1` that is a FAILED test, not a skip — correctly: this is a
NOT-PROVISIONED box, not a platform that cannot have the thing.

A **daemon** started with `--vocab <that path>` binds its socket, logs
`harnessd: loading the vocabulary: libllama could not load a vocabulary from …` and then goes —
so `crates/harnessd/tests/stop_ends_runs.rs` fails 90 s later with *"the run never reached the
state this test is about"*, which reads like a platform gap and is this file. Read the daemon's
`harnessd.stderr` in its `/tmp/letibot-stop-runs-*` scratch dir before believing a macOS theory.

## And the GLM fixture has its own gap

`crates/harnessd/tests/slash.rs`'s `VOCAB` is the literal
`/home/dead/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf` with no
`$HOME`/`LETIBOT_GLM_GGUF` fallback (unlike `model_switch.rs`), so on any box that is not the
Linux one it can only ever go red. `~/models/glm-5.3-flash/` does not exist on the Mac.

## The resolution (MEASURED 09:46–09:50): `build-glm` is llama.cpp 0.4.0 and it knows `qwen4exp`

`~/Projects/llama.cpp` has THREE builds, and they are not the same llama.cpp:

```
build      libllama.0.3.0.dylib   Oct  7 14:31   strings | grep -c '^qwen4exp$'  ->  0
build-cpu  libllama.0.3.0.dylib   Oct  7 15:46                                  ->  0
build-glm  libllama.0.4.0.dylib   Oct 26 09:46                                  ->  1
```

`crates/llama/build-support.rs` searches `LETIBOT_LLAMA_DIR` → `$HOME/Projects/llama.cpp` → the
box path, then **`build-glm/bin` before `build/bin`**, first hit wins — so a build with NO
`LETIBOT_LLAMA_*` set (which is exactly the merge queue's daemon, and AGENTS.md's standing note)
links 0.4.0 and can load the vocabulary. The brief for package QC pins
`LETIBOT_LLAMA_LIB=…/build/bin`, i.e. 0.3.0, which cannot.

And the split stub loads under 0.4.0 once its `split.no`/`split.tensors.count`/`split.count` keys
are gone. A 60-line python rewrite (`desplit.py`, scratch only — `~/models` untouched) dropped the
three keys and kept 64 of 67 KV, producing a 10 946 560-byte file; **byte-for-byte the same size
as the `vocab-only.gguf.pending` another session was writing**, so that was the same surgery.

```
LETIBOT_LLAMA_LIB=~/Projects/llama.cpp/build-glm/bin LETIBOT_VOCAB_GGUF=<de-split copy> \
  cargo test --offline -p letibot-llama --test gguf   ->  20 passed; 0 failed
```

with 0.3.0 the same call is `Load { path }` / `libllama could not load a vocabulary`. So the
Mac's fix is two lines of box state, not code: link `build-glm/bin`, and put a
vocabulary-only (unsplit) GGUF at `~/models/qwen3.8-flash-next/…-00001-of-00006.gguf`.
