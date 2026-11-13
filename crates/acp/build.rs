//! Bakes the rpath to `libllama` into this crate's binaries, as `crates/bridge/build.rs` does.
//! The tests start a real daemon through `harnessd`, and under `cargo test --workspace` feature
//! unification links `libllama` into them even though this crate asks for no default features.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
