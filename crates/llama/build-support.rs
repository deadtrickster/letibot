// Where this box's llama.cpp is, shared by every build script that needs it.
//
// There is no crate for this on purpose: `cargo:rustc-link-search` and `cargo:rustc-link-arg`
// apply only to the targets of the package whose build script emitted them, nothing downstream,
// so five crates each need a build script that names the same directory. One definition, five
// `include!`s.
//
// `include!` rather than a build-dependency, because a crate for forty lines would be a workspace
// member to version, publish and reason about. The path in each build script is
// `include!("../llama/build-support.rs")`, which `include!` resolves relative to the including
// file.
//
// Everything here is fully qualified and there is no `use`, because the including files have
// their own and a duplicate import is a compile error in a build script.

/// The fork's checkout on the Linux box this tree is developed in.
const DEFAULT_LLAMA_DIR: &str = "/home/dead/Projects/llama.cpp";

/// Where a llama.cpp build tree keeps its libraries: this project's own CUDA build, then CMake's
/// default. Two layouts because the two boxes disagree about `-B`.
const LLAMA_LIB_SUBDIRS: [&str; 2] = ["build-glm/bin", "build/bin"];

/// What a `libllama` is called, by platform: the first that exists names the one.
const LLAMA_LIBRARIES: [&str; 3] = ["libllama.so", "libllama.dylib", "libllama.a"];

/// The checkouts this box may have: the variable, then the conventional checkout under this home, then
/// the box's own path, which are the same directory on the Linux box.
#[allow(dead_code)] // each includer uses a subset of this file
fn llama_checkout_candidates() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("LETIBOT_LLAMA_DIR") {
        dirs.push(std::path::PathBuf::from(dir));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(std::path::PathBuf::from(home).join("Projects/llama.cpp"));
    }
    dirs.push(std::path::PathBuf::from(DEFAULT_LLAMA_DIR));
    dirs.dedup();
    dirs
}

/// The library directories to try for a given checkout.
#[allow(dead_code)]
fn llama_lib_candidates(checkout: &std::path::Path) -> Vec<std::path::PathBuf> {
    LLAMA_LIB_SUBDIRS
        .iter()
        .map(|sub| checkout.join(sub))
        .collect()
}

#[allow(dead_code)]
fn llama_has_library(dir: &std::path::Path) -> bool {
    LLAMA_LIBRARIES.iter().any(|f| dir.join(f).exists())
}

/// The checkout holding `include/llama.h`, if this box has one.
#[allow(dead_code)]
fn llama_checkout_found() -> Option<std::path::PathBuf> {
    llama_checkout_candidates()
        .into_iter()
        .find(|d| d.join("include/llama.h").is_file())
}

/// The directory holding `libllama`, if one can be found. An explicitly set `LETIBOT_LLAMA_LIB` is
/// honoured as given, even when it holds nothing, because it is an instruction rather than a hint.
#[allow(dead_code)]
fn llama_lib_found() -> Option<std::path::PathBuf> {
    if let Some(lib) = std::env::var_os("LETIBOT_LLAMA_LIB") {
        return Some(std::path::PathBuf::from(lib));
    }
    let checkout = llama_checkout_found()?;
    llama_lib_candidates(&checkout)
        .into_iter()
        .find(|d| llama_has_library(d))
}

/// The directory to bake an rpath to, for a crate whose binaries only need it at exec. It never panics:
/// a build script that failed on a box with no llama.cpp would take the cloud-only build with it.
#[allow(dead_code)]
fn llama_lib_dir() -> std::path::PathBuf {
    let lib = llama_lib_found()
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_LLAMA_DIR).join(LLAMA_LIB_SUBDIRS[0]));
    llama_watch(&lib);
    lib
}

/// Re-run when the linked library changes, not only when a variable does: a `target/` otherwise stays
/// frozen to whatever its first build found. A new higher-priority directory appearing needs `cargo
/// clean -p letibot-llama`.
#[allow(dead_code)]
fn llama_watch(lib: &std::path::Path) {
    for name in LLAMA_LIBRARIES {
        let f = lib.join(name);
        if f.exists() {
            println!("cargo:rerun-if-changed={}", f.display());
        }
    }
}

/// The paths a failure message shows, one per line and indented.
#[allow(dead_code)]
fn llama_listing(paths: impl Iterator<Item = std::path::PathBuf>) -> String {
    paths
        .map(|p| format!("  {}\n", p.display()))
        .collect::<String>()
}
