//! Emits the rpath `letibot-tokencore` emits. `cargo:rustc-link-arg` reaches only the package
//! that emitted it, so without this the test binaries link and fail at exec.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
