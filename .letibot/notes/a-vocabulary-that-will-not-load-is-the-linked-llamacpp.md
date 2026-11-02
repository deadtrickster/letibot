<!-- abstract: A `Parts::load` panic says nothing about the GGUF until the linked `libllama` is known - the two llama.cpp builds on this box are the whole difference, and `cargo clean -p letibot-llama` is required because a stale rlib keeps the old library linked. -->

# A vocabulary that will not load is the LINKED llama.cpp, not the file

A `Parts::load(...).expect("the vocabulary must load")` panic says nothing about the GGUF until the
loaded `libllama` is known. On this box `~/Projects/llama.cpp/build/bin` is llama.cpp 0.3.0 and
cannot read the qwen3.8 vocabulary; `build-glm/bin` (the 0.23.0 fork) can. The name
`...-00001-of-00006.gguf` invites the "it is a split model's first shard" reading, and that reading
was wrong.

```sh
LETIBOT_LLAMA_DIR=$HOME/Projects/llama.cpp LETIBOT_LLAMA_LIB=$HOME/Projects/llama.cpp/build-glm/bin \
  cargo clean --offline -p letibot-llama && cargo test --offline -p letibot-harnessd --lib
```

`cargo clean -p letibot-llama` is required, not hygiene: the build script records
`RerunIfEnvChanged` with the value it last succeeded with, so a stale rlib keeps the old library
linked and the same panic comes back.

Two rules. A test file that fails identically in every test is apparatus, not code - five unrelated
assertions do not break together. And a test that needs a real engine but may find none should
`let Ok(parts) = Parts::load(&cfg) else { return; }`, because absent apparatus is not a failed
assertion. A test that can run without one should: the byte vocabulary plus a loopback fake
provider needs no GGUF at all.
