<!-- abstract: A `Parts::load` panic says nothing about the GGUF until the linked `libllama` is known - which llama.cpp a build links is a box fact, `cargo clean -p letibot-llama` is required to change it, and `letibot_llama::apparatus::present_file` is how a test asks instead of panicking. -->

# A vocabulary that will not load is the LINKED llama.cpp, not the file

A `Parts::load(...).expect("the vocabulary must load")` panic says nothing about the GGUF until the
loaded `libllama` is known. Which one a build links is a box fact: `~/Projects/llama.cpp` can hold
several builds that are not the same llama.cpp (`strings <libllama> | grep -c '^qwen4exp$'` answers
0 or 1), and `crates/llama/build-support.rs` prefers `build-glm/bin` over `build/bin`, so pinning
`LETIBOT_LLAMA_LIB` at the other one is how a box gets a vocabulary that cannot load.

Two things the file itself can be guilty of, and neither is "it is shard 1 of 6": a loader that
mmaps a sibling shard unconditionally before `vocab_only` is consulted wants the other five to exist
however little of the model is wanted, and a file whose architecture is `qwen4exp` cannot load on a
llama.cpp whose arch list stops at `qwen35`, because `llm_load_arch()` throws even for `vocab_only`.

```sh
LETIBOT_LLAMA_DIR=$HOME/Projects/llama.cpp LETIBOT_LLAMA_LIB=$HOME/Projects/llama.cpp/build-glm/bin \
  cargo clean --offline -p letibot-llama && cargo test --offline -p letibot-harnessd --lib
```

`cargo clean -p letibot-llama` is required, not hygiene: the build script records
`RerunIfEnvChanged` with the value it last succeeded with, so a stale rlib keeps the old library
linked and the same panic comes back.

Three rules. A test file that fails identically in every test is apparatus, not code - five
unrelated assertions do not break together. A test that needs a real engine but may find none
should ask `letibot_llama::apparatus`: `present_file` loads the file once, caches by path, length
and mtime, and `present` says absent with a sentence rather than letting the test panic inside
`load`. And a test that can run without one should: the byte vocabulary plus a loopback fake
provider needs no GGUF at all.
