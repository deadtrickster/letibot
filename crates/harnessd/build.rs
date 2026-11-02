//! Bakes the same rpath `letibot-tokencore` bakes, for this crate's own binaries.
//!
//! `cargo:rustc-link-arg` reaches the emitting package's targets and nothing downstream, so
//! tokencore's rpath does not help a `harnessd` that links `libllama`: it links fine and then
//! fails at exec with `cannot open shared object file`, which reads as a missing library. The
//! alternative, an `LD_LIBRARY_PATH` every developer and CI job must remember, fails the moment
//! the daemon is started another way.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    // `$ORIGIN` first, because it is what makes a packaged build possible: it is the binary's own
    // directory, so a tarball holding `harnessd` beside the llama.cpp libraries starts on a machine
    // with no llama.cpp. The absolute build tree stays second so a development build still works.
    //
    // It belongs here and not in `letibot-tokencore`, for the reason the module doc gives.
    // `letibot-tui` bakes its own (`crates/tui/build.rs`): it links `libllama` too on aarch64.
    // `letibot-askpass` needs none, being a target of this package.
    // dyld has no `$ORIGIN`; `@loader_path` is the same idea in its spelling.
    let origin = if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        "@loader_path"
    } else {
        "$ORIGIN"
    };
    println!("cargo:rustc-link-arg=-Wl,-rpath,{origin}");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
