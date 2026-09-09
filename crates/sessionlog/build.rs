//! Bakes the same rpath `letibot-tokencore` bakes, for this crate's own binaries.
//!
//! This crate does not link `libllama` itself, but its **dev-dependencies** do
//! (the `turn` feature pulls in the engine, which pulls in the token core). T10.5:
//! `cargo:rustc-link-arg` applies only to the targets of the package that emitted
//! it, so without this the test binaries link fine and then fail at exec with
//! `cannot open shared object file` — a failure that reads as a missing library
//! rather than as a missing linker flag.
//!
//! Emitted unconditionally rather than under `CARGO_FEATURE_TURN`: an rpath to a
//! directory whose libraries are not linked costs nothing, and a build script that
//! is right only under one feature combination is a trap for the next crate.

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
