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
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
