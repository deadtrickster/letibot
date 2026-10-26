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
//!
//! Which directory is `build-support.rs`, included rather than copied: five crates need
//! the same answer, and until 2026-10-26 each had its own copy of a *Linux* path — so
//! every test binary of this crate was linked against an rpath that exists on one box and
//! aborted at exec on the other, while the gate that says whether the tree is good could
//! not start them at all.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
