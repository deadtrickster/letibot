// Where this box's llama.cpp is, shared by every build script that needs it.
//
// This file has no crate of its own on purpose. `cargo:rustc-link-search` and
// `cargo:rustc-link-arg` apply only to the targets of the package whose build script
// emitted them — nothing downstream — so five crates each need a build script that
// names the same directory (why: `docs/build-notes.md`). Until 2026-10-26 each of the
// five carried its own copy of the same two constants, and the copy was a *Linux*
// path: `main` went red on the merge gate's clippy step because `crates/llama`'s copy
// asserted its way out looking for `/home/dead/Projects/llama.cpp` on a Mac, and the
// other four, which cannot assert because their libraries only matter at exec, baked
// `/home/dead/Projects/llama.cpp/build-glm/bin` as the sole rpath of seventy test
// binaries, which then aborted before `main` ran. One definition, five `include!`s.
//
// `include!`, not a build-dependency: a crate for forty lines would be a workspace
// member to version, publish and reason about, and the two boxes' layouts are the whole
// subject. The path in each build script is `include!("../llama/build-support.rs")`,
// which `include!` resolves relative to the file doing the including.
//
// Everything here is fully qualified and there is no `use`, because the including files
// have their own and a duplicate import is a compile error in a build script — which
// fails the build with a message about a file nobody was editing.

/// The fork's checkout on the Linux box this tree is developed in.
const DEFAULT_LLAMA_DIR: &str = "/home/dead/Projects/llama.cpp";

/// Where a llama.cpp build tree keeps its libraries, in the order they are tried:
/// this project's own CUDA build (`build-glm`), then CMake's default `build/bin`.
///
/// Two layouts and not one because the two boxes disagree: the Linux box builds with
/// `-B build-glm`, and the `docs/firecode-on-macos.md` recipe builds with `-B build`.
const LLAMA_LIB_SUBDIRS: [&str; 2] = ["build-glm/bin", "build/bin"];

/// What a `libllama` is called, by platform — the first that exists names the one.
const LLAMA_LIBRARIES: [&str; 3] = ["libllama.so", "libllama.dylib", "libllama.a"];

/// The checkouts this box may have, in the order they are tried: the variable, then the
/// conventional checkout under this home, then the box's own path.
///
/// `$HOME/Projects/llama.cpp` and `/home/dead/Projects/llama.cpp` are the same directory
/// on the Linux box, so the VM's answer is unchanged by the extra candidate.
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

/// The directory holding `libllama`, if one can be found.
///
/// An explicitly set `LETIBOT_LLAMA_LIB` is honoured as given — even when it holds
/// nothing — because it is an instruction rather than a hint, and a build that quietly
/// used a different directory than the one it was told to would be the harder bug.
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
///
/// **Never panics, and that is the contract**: `crates/sessionlog` and the rest emit this
/// unconditionally, because an rpath to a directory whose libraries are not linked costs
/// nothing, and a build script that fails on a box with no llama.cpp would take the
/// cloud-only build with it. When nothing is found the Linux default is emitted, which is
/// what every one of these scripts emitted unconditionally before this file existed.
#[allow(dead_code)]
fn llama_lib_dir() -> std::path::PathBuf {
    let lib = llama_lib_found()
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_LLAMA_DIR).join(LLAMA_LIB_SUBDIRS[0]));
    llama_watch(&lib);
    lib
}

/// **Re-run when the library this build linked changes**, not only when a variable does.
///
/// The search above finds a directory, and `rerun-if-env-changed` fires on a variable
/// changing — so a `target/` was frozen to whatever its first build found. MEASURED
/// 2026-10-26: the merge queue's landing worktree kept linking llama.cpp 0.3.0 from
/// `build/bin` after the fork's 0.4.0 was built into `build-glm/bin`, and every gate run
/// failed on a vocabulary 0.3.0 cannot read. Watching the chosen library catches a rebuild
/// or a replacement in place. A NEW higher-priority directory appearing is not caught —
/// cargo cannot watch a path that does not exist without re-running on every build — and
/// that is the one case that needs `cargo clean -p letibot-llama` (`docs/build-notes.md`).
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
