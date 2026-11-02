<!-- abstract: A vocabulary GGUF that exists and cannot load has one of two causes - an architecture the linked llama.cpp does not know, or a split model whose other shards are missing. -->

# The Mac's vocabulary GGUF exists and cannot load, because this llama.cpp has no `qwen4exp`

Two causes, and they are told apart by reading the file. `llama-gguf <file> r` (or any GGUF KV
reader) gives `general.architecture` and the `split.*` keys. A file whose `n_tensors` is 0 and whose
`split.count` is 6 is shard 1 of a split model, and `llama_model_loader`'s constructor opens every
sibling shard unconditionally, before `vocab_only` is consulted - so the other five must exist however
little of the model is wanted. A file whose architecture is `qwen4exp` cannot load on a llama.cpp whose
arch list stops at `qwen35`, and `llm_load_arch()` throws even for `vocab_only`.

Which llama.cpp a build links is a box fact: `~/Projects/llama.cpp` can hold several builds that are
not the same llama.cpp (`strings <libllama> | grep -c '^qwen4exp$'` answers 0 or 1), and
`crates/llama/build-support.rs` prefers `build-glm/bin` over `build/bin`, so pinning
`LETIBOT_LLAMA_LIB` at the other one is how a box gets a vocabulary that cannot load.

`letibot_llama::apparatus` is where this is answered now: `present_file` loads the file once, caches by
path, length and mtime, and `present` says absent with a sentence rather than letting a test panic
inside `load`.
