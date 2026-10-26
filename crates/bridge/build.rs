//! Bakes the rpath to `libllama` into this crate's binaries, as `crates/turn/build.rs` does.
//!
//! The bridge never calls the engine, but its tests start a real daemon through `harnessd`,
//! so the test binary links `libllama` — and `cargo:rustc-link-arg` from another package's
//! build script reaches none of this crate's targets. Without this the end-to-end test
//! passed under `cargo test` with the llama variables exported (cargo puts the search path
//! on the loader's fallback path) and aborted at exec with `no LC_RPATH's found` in the
//! merge gate's environment, which has neither.
//!
//! The note every crate-adder should read first: `docs/build-notes.md`.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
