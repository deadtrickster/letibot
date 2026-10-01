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
//! **Why is now established, from the artifact the fixed run uploaded.** The arm64
//! `letibot-tui` declares two libraries its x86_64 twin does not:
//!
//! ```text
//! arm64  letibot-tui: NEEDED libllama.so.0, libsqlite3.so.0   (both tokencore's)
//! x86_64 letibot-tui: NEEDED libc, libgcc_s, ld-linux          (neither)
//! ```
//!
//! The libraries ARE offered to this link — the command line carries
//! `-L native=…/build/letibot-tokencore-<hash>/out` and the llama directory, and
//! `cargo:rustc-link-lib` propagates from a dependency where `rustc-link-arg` does
//! not. Whether an offered-but-unused library survives to `DT_NEEDED` is the
//! linker's `--as-needed` decision, and the two runners' toolchains disagree.
//! Demonstrated on this box rather than asserted:
//!
//! ```text
//! cc -o a main.c -Wl,--no-as-needed -L. -lunused  ->  NEEDED libunused.so  (kept)
//! cc -o b main.c -Wl,--as-needed    -L. -lunused  ->  (no libunused)      (dropped)
//! ```
//!
//! So the x86_64 head is *accidentally* clean: it needs no llama at run time today
//! only because its linker is dropping a dependency that is genuinely being passed
//! to it. Any symbol the head actually used from tokencore would put `libllama.so.0`
//! back into ITS `DT_NEEDED` too. That is why the answer here is not "x86_64 does
//! not need it" but "both get an rpath and both ship the libraries".
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
