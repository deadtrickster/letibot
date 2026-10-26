<!-- abstract: MEASURED 2026-10-26, then corrected the same day: with `LETIBOT_LLAMA_LIB=$HOME/Projects/llama.cpp/build/bin` (llama.cpp 0.3.0) `Parts::load` cannot read this box's vocabulary and 30 harnessd lib tests + all 5 in tests/delivery.rs fail on the `expect("the vocabulary must load")` line; with the fork at `build-glm/bin` (0.23.0) all 661 pass. The lib is the whole difference. -->

# A vocabulary that will not load is the LINKED llama.cpp, not the file

MEASURED 2026-10-26 in `~/Projects/letibot/wt-d1` (package D1), macOS, and corrected the same day —
the first conclusion below was wrong about the cause and the correction is the useful half.

## The symptom

`cargo test --offline -p letibot-harnessd --lib` with
`LETIBOT_LLAMA_LIB=$HOME/Projects/llama.cpp/build/bin` gave **631 passed, 30 failed**, every failure
panicking on a `Parts::load(...).expect("the vocabulary must load")` line:

```
loading the vocabulary: libllama could not load a vocabulary from
/Users/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf
```

`tests/delivery.rs` failed 5/5 the same way. The file is 10,946,560 bytes and its name says
`00001-of-00006`, so *"it is a split model's first shard"* was a plausible reading — and wrong.

## The actual cause: two builds of llama.cpp on one box

`~/Projects/llama.cpp/build/bin` holds llama.cpp **0.3.0** (`libggml-base.0.22.0.dylib`), which
cannot read this GGUF. `~/Projects/llama.cpp/build-glm/bin` holds the **0.23.0** fork
(`libggml-base.0.23.0.dylib`), which reads it. Linking the fork is the whole difference:

```
LETIBOT_LLAMA_DIR=$HOME/Projects/llama.cpp LETIBOT_LLAMA_LIB=$HOME/Projects/llama.cpp/build-glm/bin
cargo clean --offline -p letibot-llama      # the rlib is the thing that was linked
cargo test --offline -p letibot-harnessd --lib
→ 661 passed, 0 failed, 1 ignored
```

`tests/delivery.rs` → 5 passed. So there are **no pre-existing harnessd failures** on this box; the
30 were the wrong library, and the AGENTS.md line that says `LETIBOT_LLAMA_LIB=$HOME/Projects/llama.cpp/build/bin`
is the macOS setting is stale for this vocabulary.

## The mistakes to keep

* **A panic on `Parts::load` says nothing about the file** until the loaded `libllama` is known.
  The `.dylib` version beside it (`build/bin` vs `build-glm/bin`) is the first thing to check, and
  `cargo clean -p letibot-llama` is required — the build script records `RerunIfEnvChanged` with
  the value it last succeeded with, so a stale rlib keeps the old library linked.
* **A test file that fails identically in every test** (`delivery.rs` 5/5) is apparatus, not code:
  five unrelated assertions do not break together.
* A test that needs a real engine but may find none should `let Ok(parts) = Parts::load(&cfg) else
  { return; }` — absent apparatus is not a failed assertion (`AGENTS.md`) — and a test that can run
  without one should: the byte vocabulary plus a loopback fake provider needs no GGUF at all
  (`crates/harnessd/tests/activity_turn.rs`, `idle_clock.rs`, `no_vocab.rs`).
