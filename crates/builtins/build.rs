//! **Build `plugins/builtin/*` for `wasm32-unknown-unknown` and hand the crate a table of them.**
//!
//! See `src/lib.rs` for why this is a build script at all. Everything here is about the two ways a
//! build script that runs `cargo` goes wrong: inheriting the outer build's environment, and
//! failing with something a person cannot act on.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));
    let repo = here
        .parent()
        .and_then(Path::parent)
        .expect("crates/builtins sits two levels under the repository root")
        .to_path_buf();
    let root = repo.join("plugins").join("builtin");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));

    // **The target, before anything is built.** A box without it can build the host and not the
    // guest, and the failure a nested cargo gives for that is a wall of errors about a missing
    // `core` that names no command. One sentence here, and the build stops before it starts.
    if !wasm_target_installed() {
        eprintln!(
            "error: building letibot needs the `wasm32-unknown-unknown` target for its built-in \
             plugins — run `rustup target add wasm32-unknown-unknown`"
        );
        std::process::exit(1);
    }

    let guests = guests(&root);
    // **The whole built-in tree is a dependency**, not only each layer's own directory: a layer
    // may share a crate with its neighbours (`plugins/builtin/abi`), and a build script that only
    // watched `<layer>/src` would keep a stale module after a change to the shared half.
    println!("cargo:rerun-if-changed={}", root.display());
    if guests.is_empty() {
        eprintln!(
            "error: {} holds no layer (a layer is a directory with `Cargo.toml` and \
             `plugin.toml`) — the built-in scope would be empty",
            root.display()
        );
        std::process::exit(1);
    }

    let mut entries = String::new();
    for (name, dir) in &guests {
        let wasm = build(name, dir, &out);
        // The manifest is read through `include_str!` from the SOURCE tree rather than copied:
        // it is what a reviewer reads, and a copy in `OUT_DIR` would be a second thing that can
        // be stale.
        let manifest = dir.join("plugin.toml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!(
            "cargo:rerun-if-changed={}",
            dir.join("Cargo.toml").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            dir.join("Cargo.lock").display()
        );
        println!("cargo:rerun-if-changed={}", dir.join("src").display());
        entries.push_str(&format!(
            "    Builtin {{\n        name: {name:?},\n        \
             manifest: include_str!({manifest:?}),\n        wasm: include_bytes!({wasm:?}),\n    }},\n",
            manifest = manifest.to_string_lossy(),
            wasm = wasm.to_string_lossy(),
        ));
    }

    let table = format!(
        "/// Every layer this binary carries, in the order they were found.\n\
         pub(crate) static BUILTIN: &[Builtin] = &[\n{entries}];\n\n\
         /// **Every layer this binary carries** — the `builtin` scope of `docs/plugins-plan.md`\n\
         /// §2.1, as a table rather than a directory.\n\
         pub fn table() -> &'static [Builtin] {{\n    BUILTIN\n}}\n"
    );
    std::fs::write(out.join("table.rs"), table).expect("write the built-in table");
    println!("cargo:rerun-if-changed=build.rs");
}

/// **Is the guest target installed on this toolchain?**
///
/// Asked of `rustc` itself — `--print sysroot` and the standard library's own directory — rather
/// than of `rustup`, because a box may have a toolchain that did not come from rustup and a build
/// script must not require one. A `rustc` that cannot answer is treated as *not installed*: the
/// build then stops with the sentence naming the command.
fn wasm_target_installed() -> bool {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let out = match Command::new(rustc).args(["--print", "sysroot"]).output() {
        Ok(out) if out.status.success() => out,
        _ => return false,
    };
    let sysroot = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    sysroot
        .join("lib")
        .join("rustlib")
        .join("wasm32-unknown-unknown")
        .is_dir()
}

/// The layer directories under `root`, by name, in the order `read_dir` gives them sorted: a
/// table whose order moved between builds would be a stack whose precedence moved with it.
fn guests(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| p.join("Cargo.toml").is_file() && p.join("plugin.toml").is_file())
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            Some((name, p))
        })
        .collect();
    found.sort();
    found
}

/// **Build one guest and answer where the module is.**
///
/// The child's environment is stripped of the four things that would make it the host's build
/// rather than the guest's — see the crate doc — and its target directory is the guest's own,
/// under `OUT_DIR`, so no build of the host can be waiting on it.
fn build(name: &str, dir: &Path, out: &Path) -> PathBuf {
    let target = out.join("target").join(name);
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(cargo)
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        // A jobserver inherited by a nested cargo is a build that waits on tokens it will never
        // be given: the outer cargo is waiting for this script, and this script is waiting for
        // the inner one.
        .env_remove("MAKEFLAGS")
        .env_remove("CARGO_MAKEFLAGS")
        .args([
            "build",
            "--offline",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .arg("--target-dir")
        .arg(&target)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("error: cannot run `cargo` to build the built-in `{name}`: {e}");
            std::process::exit(1)
        });
    if !status.success() {
        // The nested cargo has already said what it could not do; this says which layer it was
        // and stops the build rather than leaving a table pointing at nothing.
        eprintln!("error: the built-in `{name}` did not build ({status})");
        std::process::exit(1);
    }
    let module = target
        .join("wasm32-unknown-unknown")
        .join("release")
        .join(format!("{}.wasm", crate_name(dir, name).replace('-', "_")));
    if !module.is_file() {
        eprintln!(
            "error: the built-in `{name}` built without a module at {} — the crate's `crate-type` \
             must include `cdylib`",
            module.display()
        );
        std::process::exit(1);
    }
    // **Copied out of the target directory** so the path in the table is one this crate owns: a
    // `cargo clean -p` on the guest would otherwise move what the host's `include_bytes!` reads.
    let kept = out.join(name);
    std::fs::create_dir_all(&kept).expect("create the layer's directory in OUT_DIR");
    let kept = kept.join("plugin.wasm");
    std::fs::copy(&module, &kept).unwrap_or_else(|e| {
        eprintln!("error: cannot keep the built-in `{name}`'s module: {e}");
        std::process::exit(1)
    });
    kept
}

/// The package name the guest's own `Cargo.toml` declares — the module's file name is that, not
/// the directory's, because the two are allowed to differ.
fn crate_name(dir: &Path, fallback: &str) -> String {
    let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
        return fallback.to_string();
    };
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package
            && let Some(rest) = line.strip_prefix("name")
            && let Some((_, value)) = rest.split_once('=')
        {
            return value.trim().trim_matches('"').to_string();
        }
    }
    fallback.to_string()
}
