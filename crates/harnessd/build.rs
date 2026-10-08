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
    // the emitting package's own targets and to nothing downstream.
    //
    // **`letibot-tui` DOES need one too, and this comment used to say it did not.**
    // It said "measured: `ldd` has no `libllama` line" — true on x86_64, false on
    // aarch64, where the same commit's binary failed at exec with
    // `libllama.so.0: cannot open shared object file` while harnessd started fine
    // (release dry-run 36840931628). `crates/tui/build.rs` now bakes the same rpath
    // and carries the account of what is and is not established about why.
    //
    // `letibot-askpass` needs none of its own: it is a binary target of THIS
    // package, so the two lines below already apply to it.
    //
    // The libraries a package must carry are measured from the transitive closure
    // of a `$ORIGIN`-only build, and there are FOUR — `libllama.so.0`,
    // `libggml.so.0`, `libggml-cpu.so.0`, `libggml-base.so.0`. An earlier version
    // of this comment named three and stopped one level short. `libggml-cuda.so` is
    // not in the set for a CPU build, because CUDA is off and it is 68 MB.
    // dyld has no `$ORIGIN`; `@loader_path` is the same idea in its spelling.
    let origin = if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        "@loader_path"
    } else {
        "$ORIGIN"
    };
    println!("cargo:rustc-link-arg=-Wl,-rpath,{origin}");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
