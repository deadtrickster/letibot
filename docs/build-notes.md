# Build notes

Small, non-obvious things about building this workspace. Read before adding a crate.

## Every crate that links `libllama` needs its own `build.rs`

`cargo:rustc-link-search` and `cargo:rustc-link-arg` apply only to the targets of the package
whose build script emitted them - nothing downstream. So every crate whose binaries link
`libllama`, directly or through a dependency (a dev-dependency counts, because a test binary
is a binary), needs a small `build.rs` of its own. Without one the binary links fine and
fails at exec:

```
error while loading shared libraries: libllama.so: cannot open shared object file: No such file or directory
```

which reads as a missing library or a broken environment, not as a missing linker flag.

Include the shared support file rather than copy a script:

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

`llama_lib_dir()` never fails, and that is its contract: emit it unconditionally, not under a
feature flag. An rpath to a directory whose libraries are not linked costs nothing, and a
build script that is right only under one feature combination is a trap. `crates/llama/build.rs`
is the exception that asserts, because it is the only crate that links the library - a box with
no llama.cpp must fail there, with the list of paths tried, rather than link a binary that
cannot start.

Where the library is found, in order: `LETIBOT_LLAMA_LIB` (honoured as given even when it holds
nothing, because it is an instruction and not a hint), then the checkout named by
`LETIBOT_LLAMA_DIR`, then `$HOME/Projects/llama.cpp`, then the box's own
`/home/dead/Projects/llama.cpp`. Under a checkout, `build-glm/bin` then `build/bin`. A
differently located llama.cpp therefore needs no variable at all, which is why no crate carries
the path as its own constant.

A target dir follows the library it linked, not a new one. Each script watches the `libllama` it
chose (`llama_watch`), so rebuilding llama.cpp in place relinks. A new higher-priority directory
appearing (a `build-glm/` built after `build/`) is invisible to cargo; run
`cargo clean -p letibot-llama` in that target dir once, or set `LETIBOT_LLAMA_LIB`.

Crates that carry this: `llama` (`letibot-llama`, the origin), `turn`, `sessionlog` (its
dev-dependencies pull the engine in), `harnessd`, `bridge` (its end-to-end test starts a real
daemon), and `letibot` - whose rpath leads with `$ORIGIN`, because it is the binary an install
puts on `PATH` and it must run from a directory that holds the libraries beside it.
