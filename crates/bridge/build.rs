//! Bakes the rpath to `libllama` into this crate's binaries, as `crates/turn/build.rs` does.
//! Tests start a real daemon through `harnessd`, so the test binary links `libllama`, and another
//! package's `cargo:rustc-link-arg` reaches none of this crate's targets.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
