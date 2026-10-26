//! Compiles the C shim (see `csrc/shim.c` for why one exists) and links the
//! fork's `libllama`.
//!
//! Where the checkout and its library are is `build-support.rs`, included rather than
//! copied — five crates need the same answer and they had five copies of it (why:
//! `docs/build-notes.md`). This is the only one of the five that ASSERTS: it is the only
//! crate that links the library, so a box with no llama.cpp must fail here, with the list
//! of paths tried, rather than link a binary that cannot start.
//!
//! **The search is not politeness — it is the merge gate.** MEASURED 2026-10-26: `main`
//! went red at `0f9ddad8` on the gate's `cargo clippy --all-targets`, exit 101, and not
//! from a lint: this script asserted its way out looking for `/home/dead/Projects/llama.cpp`
//! — the *Linux* box's path — because the gate is a child of a daemon running on a Mac
//! whose environment carries neither `LETIBOT_LLAMA_DIR` nor `LETIBOT_LLAMA_LIB`. The
//! checkout was there the whole time; only the path was, and only inside whatever shell
//! had exported it. A build script that can find its header only when a human exported a
//! variable cannot be run by a queue, and the queue is what says whether the tree is good.

include!("build-support.rs");

fn main() {
    println!("cargo:rerun-if-changed=csrc/shim.c");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    // The search reads `HOME`, so a checkout that moves with the home it is under has to
    // be able to re-run this script.
    println!("cargo:rerun-if-env-changed=HOME");

    let dir = locate_checkout();
    let lib = locate_lib(&dir);

    cc::Build::new()
        .file("csrc/shim.c")
        .include(dir.join("include"))
        .include(dir.join("ggml/include"))
        .warnings(true)
        .compile("letibot_llama_shim");

    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=dylib=llama");
    // The shim calls `ggml_backend_dev_by_type` to name the CPU device for a
    // vocabulary load (see csrc/shim.c): that symbol lives in libggml, which
    // libllama itself imports at runtime. Naming it here keeps the shim's one
    // dependency explicit instead of relying on the link order picking it up.
    println!("cargo:rustc-link-lib=dylib=ggml");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}

/// The checkout holding `include/llama.h`, or a failure that lists every path tried.
fn locate_checkout() -> std::path::PathBuf {
    match llama_checkout_found() {
        Some(dir) => dir,
        None => panic!(
            "llama.h not found. Tried:\n{}Set LETIBOT_LLAMA_DIR to the llama.cpp checkout.",
            llama_listing(
                llama_checkout_candidates()
                    .into_iter()
                    .map(|d| d.join("include/llama.h"))
            )
        ),
    }
}

/// The directory holding `libllama`, or a failure that lists every path tried.
///
/// `LETIBOT_LLAMA_LIB` is an instruction rather than a hint, so a directory that was
/// named and holds nothing is a failure naming it — not a quiet fall back to one that
/// does.
fn locate_lib(checkout: &std::path::Path) -> std::path::PathBuf {
    let candidates = llama_lib_candidates(checkout);
    let lib = match std::env::var_os("LETIBOT_LLAMA_LIB") {
        Some(lib) => std::path::PathBuf::from(lib),
        None => match candidates.iter().find(|d| llama_has_library(d)) {
            Some(lib) => lib.clone(),
            None => panic!(
                "libllama not found. Tried:\n{}Set LETIBOT_LLAMA_LIB to the build tree's lib dir.",
                llama_listing(candidates.into_iter())
            ),
        },
    };
    assert!(
        llama_has_library(&lib),
        "libllama not found in {}. Set LETIBOT_LLAMA_LIB to the build tree's lib dir.",
        lib.display()
    );
    lib
}
