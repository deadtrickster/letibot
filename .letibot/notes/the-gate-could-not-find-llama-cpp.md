<!-- abstract: Five build scripts each carried the Linux llama.cpp path, so the gate died on one crate's assertion and then on the other four's rpath; `crates/llama/build-support.rs` is the search all share. -->

# `main` was red on the gate because five build scripts only knew the Linux llama.cpp path

`crates/llama/build.rs` panicked with `llama.h not found at /home/dead/Projects/llama.cpp/include/llama.h`
on a Mac and no lint caused it: the default was a Linux path, and the gate's steps are children of a
daemon with no `LETIBOT_LLAMA_DIR`. Teaching only `llama` to search moved the failure to step 3, where 70
targets died at exec on `Library not loaded: @rpath/libllama.0.dylib` - the link passed and the rpath
the other four baked was still the Linux one.

The fix is `crates/llama/build-support.rs`, `include!`d by every build script that needs it. It searches
`LETIBOT_LLAMA_DIR`, then `$HOME/Projects/llama.cpp`, then the box path, and inside that `build-glm/bin`
before `build/bin`. `llama_lib_dir()` never panics, so a crate that only needs an rpath stays
unconditional. `docs/build-notes.md` says include it, not copy.

The fingerprint says which env a build really used:
`target/debug/.fingerprint/letibot-llama-*/run-build-script-build-script-build.json` records
`RerunIfEnvChanged` with the last successful run's value. And the whole step output is on the row, not
just the tail `queue status` renders: `gate_steps_json` on the repository's `target_health` row.
