//! Emits the rpath `letibot-tokencore` emits: `cargo:rustc-link-arg` reaches only the targets
//! of the package that emitted it, so without it this crate's test binaries link and then fail
//! at exec. The directory is the shared `build-support.rs`; emitted unconditionally, because an
//! unused rpath costs nothing.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
