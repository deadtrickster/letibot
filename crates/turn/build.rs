//! Bakes the same rpath `letibot-tokencore` bakes, for this crate's own binaries.
//!
//! `cargo:rustc-link-arg` applies only to the emitting package's targets, so tokencore's
//! rpath makes its tests find `libllama.so` while this crate's test binaries link fine and
//! then fail at exec with `cannot open shared object file`. The alternative is asking every
//! developer and CI job to export `LD_LIBRARY_PATH`, a precondition that works until somebody
//! runs the tests a different way. The directory comes from `../llama/build-support.rs`.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
