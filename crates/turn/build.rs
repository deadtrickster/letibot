//! Bakes the same rpath `letibot-tokencore` bakes, for this crate's own binaries.
//!
//! `cargo:rustc-link-arg` applies to the targets of the package whose build script
//! emitted it, and to nothing downstream. So tokencore's rpath makes *its* tests
//! find `libllama.so`, while this crate's test binaries link fine and then fail at
//! exec with `cannot open shared object file` — a failure that looks like a missing
//! library rather than like a missing linker flag.
//!
//! The alternative is asking every developer and every CI job to export
//! `LD_LIBRARY_PATH`, which is the kind of environmental precondition that works
//! until somebody runs the tests a different way.
//!
//! The note every crate-adder should read first: `docs/build-notes.md`.

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
