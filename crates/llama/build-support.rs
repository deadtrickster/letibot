// Where this box's llama.cpp is, shared by every build script that needs it, because
// `cargo:rustc-link-*` applies only to the emitting package's own targets. Fully
// qualified with no `use`, since a duplicate import is a compile error in a build script.

/// The fork's checkout on the Linux box this tree is developed in.
const DEFAULT_LLAMA_DIR: &str = "/home/dead/Projects/llama.cpp";

/// Where a llama.cpp build tree keeps its libraries, in the order they are tried: this
/// project's own CUDA build (`build-glm`), then CMake's default `build/bin`. Two layouts
/// and not one because the two boxes disagree about which they build.
const LLAMA_LIB_SUBDIRS: [&str; 2] = ["build-glm/bin", "build/bin"];

/// What a `libllama` is called, by platform — the first that exists names the one.
const LLAMA_LIBRARIES: [&str; 3] = ["libllama.so", "libllama.dylib", "libllama.a"];

/// The checkouts this box may have, in the order they are tried: the variable, then the
/// conventional checkout under this home, then the box's own path. The last two are the
/// same directory on the Linux box, so its answer is unchanged by the extra candidate.
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

/// The directory holding `libllama`, if one can be found. An explicitly set
/// `LETIBOT_LLAMA_LIB` is honoured as given, even when it holds nothing, because it is an
/// instruction rather than a hint and a silent fallback would be the harder bug.
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

/// The directory to bake an rpath to, for a crate whose binaries only need it at exec.
/// Never panics, and that is the contract: an rpath to a directory whose libraries are
/// not linked costs nothing, so a build script must not fail on a box with no llama.cpp.
#[allow(dead_code)]
fn llama_lib_dir() -> std::path::PathBuf {
    let lib = llama_lib_found()
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_LLAMA_DIR).join(LLAMA_LIB_SUBDIRS[0]));
    llama_watch(&lib);
    lib
}

/// Re-run when the library this build linked changes, not only when a variable does: the
/// search finds a directory, so `rerun-if-env-changed` alone leaves `target/` frozen to
/// whatever its first build found. A new higher-priority directory is not caught.
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
