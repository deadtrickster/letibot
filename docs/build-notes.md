# Build notes

Small, non-obvious things about building this workspace. Read before adding a crate.

## Every crate that links `libllama` needs its own `build.rs`

`cargo:rustc-link-search` and `cargo:rustc-link-arg` apply only to the targets of the
package whose build script emitted them — nothing downstream. `letibot-tokencore`'s build
script bakes the rpath, yet every other crate in the dependency chain still produces
binaries that **link fine and fail at exec**:

```
error while loading shared libraries: libllama.so: cannot open shared object file: No such file or directory
```

which reads as a missing library or a broken environment, not as a missing linker flag.

**The fix** is a small `build.rs` in every crate that links `libllama` — directly or
through its dependencies, **including dev-dependencies** (a test binary is a binary too).
It emits `cargo:rustc-link-search=native=...` plus `cargo:rustc-link-arg=-Wl,-rpath,...`
for the same directory. Copy `crates/turn/build.rs`; its doc comment explains the
reasoning, and the directory comes from `LETIBOT_LLAMA_LIB` when set, so a
differently-located llama.cpp needs no source change. Emit it unconditionally rather
than under a feature flag: an rpath to a directory whose libraries are not linked costs
nothing, and a build script that is right only under one feature combination is a trap.

Already covered: `tokencore` (the origin — also links the lib and asserts it exists),
`turn`, `sessionlog` (its dev-dependencies pull the engine in), `harnessd`. A new crate
that ends up linking `libllama` needs the same treatment.
