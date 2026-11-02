//! Bakes the same rpath `letibot-tokencore` bakes, for this crate's own binaries.
//! `cargo:rustc-link-arg` reaches the emitting package's targets and nothing downstream, so
//! tokencore's rpath does not help a `harnessd` that links `libllama`:

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    // `$ORIGIN` first, because it is what makes a packaged build possible: it is the binary's
    // own directory, so a tarball holding `harnessd` beside the llama.cpp libraries starts on
    // a machine with no llama.cpp.
    let origin = if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        "@loader_path"
    } else {
        "$ORIGIN"
    };
    println!("cargo:rustc-link-arg=-Wl,-rpath,{origin}");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
