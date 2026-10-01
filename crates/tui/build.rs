//! Bakes an rpath into `letibot-tui`, for the same reason `crates/harnessd` bakes
//! one into its own binaries — and for a reason that only showed up on arm64.
//!
//! # The claim this file replaces
//!
//! `crates/harnessd/build.rs` said, and it was written as a measurement:
//!
//! > `letibot-tui` needs no rpath at all, because it does not link llama
//! > (measured: `ldd` has no `libllama` line)
//!
//! That is true on **x86_64** and false on **aarch64**. MEASURED, release dry-run
//! 36840931628, the arm64 leg:
//!
//! ```text
//! harnessd 0.1.1
//! ./target/release/letibot-tui: error while loading shared libraries:
//!     libllama.so.0: cannot open shared object file        (exit 127)
//! ```
//!
//! The same commit's x86_64 leg ran both binaries, and the x86_64 artifact
//! downloaded from it has **no `libllama` in `letibot-tui`'s `DT_NEEDED` and no
//! `RUNPATH` at all**. So one of the two linkers records a dependency the other
//! drops — and `letibot-tui` has no runpath to satisfy it with, on either.
//!
//! **Why the arm64 linker does that is not established here**, and this file does
//! not claim to know: `letibot-tokencore` is not in this crate's dependency graph
//! (`cargo tree -p letibot-tui` has zero occurrences of it), and tokencore is the
//! only package that emits `-lllama`. Finding out would need an arm64 machine. What
//! IS established is that the shipped binary cannot be trusted to have the
//! dependency list its author measured, and that a package is the wrong place to
//! discover it.
//!
//! # Why an rpath fixes it either way
//!
//! In the **package**, the four llama libraries sit beside this binary, so
//! `$ORIGIN` finds them if the dependency is real and costs nothing if it is not.
//! Without `$ORIGIN` the loader consults only the system paths and fails — which is
//! how a packaged arm64 head would have shipped broken, since the step that would
//! have caught it (`The package must run on its own`) never ran: this earlier step
//! failed first.
//!
//! The absolute build directory is kept as the second entry, exactly as
//! `crates/harnessd/build.rs` keeps it, so a development build in the source tree
//! keeps working with nothing copied next to the binary.
//!
//! # This is the third time
//!
//! The same mistake in three costumes: an option NAME existing is not the target
//! being gated; one level of `DT_NEEDED` is not the closure; and `ldd` on one
//! architecture is not the dependency list. Each time, a measurement of a
//! neighbourhood was reported as the whole. The fix here is deliberately
//! *insensitive to the answer*, which is the point — it does not require knowing
//! what arm64's linker will do.

use std::path::PathBuf;

const DEFAULT_LLAMA_LIB: &str = "/home/dead/Projects/llama.cpp/build-glm/bin";

fn main() {
    println!("cargo:rerun-if-env-changed=LETIBOT_LLAMA_LIB");
    let lib = PathBuf::from(
        std::env::var("LETIBOT_LLAMA_LIB").unwrap_or_else(|_| DEFAULT_LLAMA_LIB.to_string()),
    );
    // The search path as well as the rpath: harmless with no `-lllama` on the link
    // line, and correct if one ever arrives.
    println!("cargo:rustc-link-search=native={}", lib.display());
    // `$ORIGIN` FIRST — it is the entry that makes a release work: the directory
    // this binary is in, expanded by ld.so, so a tarball holding `letibot-tui`
    // beside `libllama.so.0` and `libggml*.so.0` starts on a machine with no
    // llama.cpp at all.
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
}
