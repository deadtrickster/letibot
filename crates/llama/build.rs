//! Compiles the C shim (see `csrc/shim.c`) and links the fork's `libllama`.
//! Paths come from `build-support.rs`, included because five crates need it. This crate alone
//! asserts: it links the library, so a box without llama.cpp must fail here listing the paths tried.

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
    llama_watch(&lib);

    cc::Build::new()
        .file("csrc/shim.c")
        .include(dir.join("include"))
        .include(dir.join("ggml/include"))
        .warnings(true)
        .compile("letibot_llama_shim");

    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=dylib=llama");
    // The shim calls `ggml_backend_dev_by_type` to name the CPU device for a
    // vocabulary load: that symbol lives in libggml, which libllama imports at runtime.
    // Naming it here keeps the shim's one dependency explicit.
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
/// `LETIBOT_LLAMA_LIB` is an instruction rather than a hint, so a directory that was
/// named and holds nothing is a failure naming it, not a quiet fall back to one that does.
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
