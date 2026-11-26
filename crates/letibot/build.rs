//! Bakes the same `$ORIGIN`-first rpath `crates/harnessd`'s build script bakes.
//!
//! `letibot` links `letibot-harnessd` (for `Role::M1`), which links `letibot-tokencore`, which
//! links `libllama`. Without a runpath the binary an install puts on `PATH` runs only where
//! `LD_LIBRARY_PATH` points at a llama build.
//!
//! # Why it must be `$ORIGIN` first
//!
//! `$ORIGIN` is the directory this binary is in, expanded by ld.so at load time, so
//! a package holding `letibot` beside `libllama.so.0` and the `libggml*.so.0` set
//! starts on a machine with no llama.cpp at all. The absolute build directory is the
//! SECOND entry, so a source-tree run works with nothing copied next to the binary.
//! `scripts/make-dist.sh` refuses to ship a library whose own runpath is not `$ORIGIN`.
//!
//! # The rpath does not travel with a library
//!
//! `rustc-link-arg` applies to the targets of the package whose build script emitted
//! it and to nothing downstream. So every package whose binaries end up linking
//! llama needs its own copy of this file: `harnessd`, `letibot` and `crates/tui`.

include!("../llama/build-support.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=HOME");
    println!("cargo:rerun-if-env-changed=LETIBOT_REAL_HOME");
    println!("cargo:rerun-if-env-changed=CARGO_HOME");
    println!("cargo:rerun-if-env-changed=RUSTUP_HOME");
    let lib = llama_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib.display());
    // dyld has no `$ORIGIN`; `@loader_path` is the same idea in its spelling.
    let origin = if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        "@loader_path"
    } else {
        "$ORIGIN"
    };
    println!("cargo:rustc-link-arg=-Wl,-rpath,{origin}");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
