//! Bakes the same rpath `letibot-tokencore` bakes, for this crate's own binaries.
//!
//! W6's note, and it applies to every crate that ends up linking `libllama`:
//! `cargo:rustc-link-arg` applies to the targets of the package whose build script
//! emitted it and to **nothing downstream**. So tokencore's rpath makes tokencore's
//! own tests find `libllama.so`, while `harnessd` links fine and then fails at exec
//! with `cannot open shared object file` — a failure that reads as a missing
//! library rather than as a missing linker flag, and costs an hour to the person
//! who reads it that way.
//!
//! The alternative is asking every developer and every CI job to export
//! `LD_LIBRARY_PATH`, which works until somebody runs the daemon a different way.

use std::path::PathBuf;

const DEFAULT_LLAMA_LIB: &str = "/home/dead/Projects/llama.cpp/build-glm/bin";

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    let lib = PathBuf::from(
        std::env::var("LETIBOT_LLAMA_LIB").unwrap_or_else(|_| DEFAULT_LLAMA_LIB.to_string()),
    );
    println!("cargo:rustc-link-search=native={}", lib.display());
    // **`$ORIGIN` FIRST, and it is the entry that makes a RELEASE possible.**
    //
    // It is the directory this binary is in, expanded by ld.so at load time, so a
    // tarball holding `harnessd` beside `libllama.so.0` and `libggml*.so.0` starts
    // on a machine with no llama.cpp at all. The absolute build tree is kept as the
    // SECOND entry so a development build — libraries left in the operator's
    // checkout, nothing copied next to the binary — works exactly as it did.
    //
    // MEASURED, and it is the same defect as the absolute path three manifests had
    // for `rano`, one layer down: without this, a packaged `harnessd` names
    // `/home/dead/Projects/llama.cpp/build-glm/bin` as its only runpath and cannot
    // start anywhere else, however carefully it was packaged.
    //
    // It must be HERE and not in `letibot-tokencore`: the module docstring above
    // says why, and it is worth not learning twice — `rustc-link-arg` applies to
    // the emitting package's own targets and to nothing downstream. `letibot-tui`
    // needs no rpath at all, because it does not link llama (measured: `ldd` has no
    // `libllama` line), and neither does `letibot-askpass`.
    //
    // The libraries a package must carry are small and measured: `libllama.so.0`
    // 4.7 MB, `libggml.so.0` 0.1 MB, `libggml-base.so.0` 0.9 MB. `libggml-cuda.so`
    // is not in any `DT_NEEDED` set — ggml dlopens its backends — so a CPU-only
    // package tokenises without it.
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
