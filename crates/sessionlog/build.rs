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
//!
//! Which directory is `build-support.rs`, included rather than copied: five crates need
//! the same answer, and until 2026-10-26 each had its own copy of a *Linux* path — so
//! every test binary of this crate was linked against an rpath that exists on one box and
//! aborted at exec on the other, while the gate that says whether the tree is good could
//! not start them at all. Still emitted unconditionally: `llama_lib_dir` never fails.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
