//! Bakes the same rpath `letibot-tokencore` bakes. `cargo:rustc-link-arg` applies only to the
//! emitting package, so without it this crate's test binaries link and fail at exec.
//! The directory comes from `../llama/build-support.rs`.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
