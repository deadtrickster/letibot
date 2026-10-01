//! Bakes the same `$ORIGIN`-first rpath `crates/harnessd`'s build script bakes.
//!
//! # Why this file exists now, and did not need to before
//!
//! While `letibot` was only a dispatcher over `sessionlog` and two renderer crates,
//! it linked no llama and needed no runpath — and it was checked. **Wiring `Role::M1`
//! changed that**: `m1::run` lives in `letibot-harnessd`, so this binary now links
//! `letibot-harnessd`, which links `letibot-tokencore`, which links `libllama`. The
//! first run of the wired role said so plainly:
//!
//! ```text
//! ./letibot-m1: error while loading shared libraries: libllama.so.0: cannot open
//! shared object file
//! ```
//!
//! That is the same failure the arm64 leg hit in `letibot-tui` — a dependency
//! arriving through a crate nobody thought of as linked to llama — and it is worse
//! here, because `letibot` is the binary an install puts on `PATH`. Without a
//! runpath it runs only where `LD_LIBRARY_PATH` happens to point at a llama build,
//! which is no machine but the author's.
//!
//! # Why it must be `$ORIGIN` first
//!
//! `$ORIGIN` is the directory this binary is in, expanded by ld.so at load time, so
//! a package holding `letibot` beside `libllama.so.0` and the `libggml*.so.0` set
//! starts on a machine with no llama.cpp at all. The absolute build directory is the
//! SECOND entry, so a source-tree run works with nothing copied next to the binary.
//!
//! Measured with the packager: `scripts/make-dist.sh` refuses to ship a library
//! whose own runpath is not `$ORIGIN`, and CI runs the extracted package from a bare
//! directory with `LD_LIBRARY_PATH` unset — which is the check that would have
//! caught this without anybody running it by hand.
//!
//! # The rpath does not travel with a library
//!
//! `rustc-link-arg` applies to the targets of the package whose build script emitted
//! it and to **nothing downstream** — `crates/harnessd/build.rs` says so at length
//! because it cost an hour once. So every package whose binaries end up linking
//! llama needs its own copy of this file: `harnessd`, now `letibot`, and
//! `crates/tui` (which needed one for a reason that only appears on aarch64).

use std::path::PathBuf;

const DEFAULT_LLAMA_LIB: &str = "/home/dead/Projects/llama.cpp/build-glm/bin";

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    let lib = PathBuf::from(
        std::env::var("LETIBOT_LLAMA_LIB").unwrap_or_else(|_| DEFAULT_LLAMA_LIB.to_string()),
    );
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
