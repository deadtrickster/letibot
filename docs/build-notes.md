# Build notes

Small, non-obvious things about building this workspace. Read before adding a crate.

## Every crate that links `libllama` needs its own `build.rs`

`cargo:rustc-link-search` and `cargo:rustc-link-arg` apply only to the targets of the
package whose build script emitted them — nothing downstream. `letibot-llama`'s build
script bakes the rpath, yet every other crate in the dependency chain still produces
binaries that **link fine and fail at exec**:

```
error while loading shared libraries: libllama.so: cannot open shared object file: No such file or directory
```

which reads as a missing library or a broken environment, not as a missing linker flag.

**The fix** is a small `build.rs` in every crate that links `libllama` — directly or
through its dependencies, **including dev-dependencies** (a test binary is a binary too).
It emits `cargo:rustc-link-search=native=...` plus `cargo:rustc-link-arg=-Wl,-rpath,...`
for the same directory. **Include `crates/llama/build-support.rs` rather than copy a build
script**:

```rust
include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    ...
}
```

`llama_lib_dir()` never fails — emit it unconditionally rather than under a feature flag:
an rpath to a directory whose libraries are not linked costs nothing, and a build script
that is right only under one feature combination is a trap. `crates/llama/build.rs` is the
exception that ASSERTS, because it is the only crate that links the library.

**It used to say "copy `crates/turn/build.rs`", and five copies of one constant is how
`main` went red on 2026-10-26.** The constant was `/home/dead/Projects/llama.cpp/build-glm/bin`
— one box's path. The merge gate is a child of the daemon, so it inherits whatever the
shell that started the daemon exported; on a Mac that is neither `LETIBOT_LLAMA_DIR` nor
`LETIBOT_LLAMA_LIB`, and the gate failed on `crates/llama`'s assertion with exit 101 that
looked like a clippy warning. The search in `build-support.rs` now tries the variable,
then `$HOME/Projects/llama.cpp`, then the box path, and `build-glm/bin` then `build/bin`
under it — so a differently-located llama.cpp needs no variable at all.

Already covered: `llama` (`letibot-llama`, the origin — the only crate that links the lib, and asserts it exists; it was `tokencore` until the vocabulary became a backend),
`turn`, `sessionlog` (its dev-dependencies pull the engine in), `harnessd`, `bridge` (its end-to-end test starts a real daemon). A new crate
that ends up linking `libllama` needs the same treatment.
