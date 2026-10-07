//! Compiles the C shim (see `csrc/shim.c` for why one exists) and links the
//! fork's `libllama`.
//!
//! The llama.cpp checkout is located by `LETIBOT_LLAMA_DIR` / `LETIBOT_LLAMA_LIB`,
//! defaulting to the fork and build tree this project targets. An rpath is
//! baked in so tests and binaries find `libggml*.so` next to `libllama.so`
//! without the caller setting `LD_LIBRARY_PATH`.

use std::path::PathBuf;

const DEFAULT_LLAMA_DIR: &str = "/home/dead/Projects/llama.cpp";
const DEFAULT_LLAMA_LIB: &str = "/home/dead/Projects/llama.cpp/build-glm/bin";

fn main() {
    println!("cargo:rerun-if-changed=csrc/shim.c");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");

    let dir = PathBuf::from(
        std::env::var("LETIBOT_LLAMA_DIR").unwrap_or_else(|_| DEFAULT_LLAMA_DIR.to_string()),
    );
    let lib = PathBuf::from(
        std::env::var("LETIBOT_LLAMA_LIB").unwrap_or_else(|_| DEFAULT_LLAMA_LIB.to_string()),
    );

    let header = dir.join("include/llama.h");
    assert!(
        header.is_file(),
        "llama.h not found at {}. Set LETIBOT_LLAMA_DIR to the llama.cpp checkout.",
        header.display()
    );
    assert!(
        ["libllama.so", "libllama.dylib", "libllama.a"]
            .iter()
            .any(|f| lib.join(f).exists()),
        "libllama not found in {}. Set LETIBOT_LLAMA_LIB to the build tree's lib dir.",
        lib.display()
    );

    cc::Build::new()
        .file("csrc/shim.c")
        .include(dir.join("include"))
        .include(dir.join("ggml/include"))
        .warnings(true)
        .compile("letibot_llama_shim");

    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=dylib=llama");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
