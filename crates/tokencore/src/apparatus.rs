//! **Whether the apparatus a test needs is on THIS machine.**
//!
//! # The problem this exists for
//!
//! MEASURED on a GitHub runner, 2026-10-01, `cargo test --workspace --no-fail-fast`:
//! **2074 passed, 84 failed, 11 suites red** — and not one of the 84 is a defect.
//! They are 76 tests wanting a vocabulary GGUF at `/home/dead/models/…`, 7 wanting
//! a cgroup v2 subtree, and a few wanting a store fixture. On this box all 84 pass.
//!
//! So the count was not the finding. **The finding is that the suite could never go
//! green on a runner, which makes its red mark a constant** — and a constant carries
//! no information. A real failure appearing inside one of those 11 suites produces
//! the identical annotation as the standing 11, so nobody can see it. That is the
//! mirror of a check that can only pass: a check that can only fail is equally
//! empty, and worse, because it teaches the reader to ignore annotations.
//!
//! # Why a skip rather than an assertion
//!
//! `crates/tokencore/src/lib.rs` used to say, and it was a deliberate decision:
//!
//! > Deliberately **not** skipped when it is missing. A tokenizer test that quietly
//! > passes on a box with no model file is a test that reports the health of
//! > `std::fs::exists`.
//!
//! **That objection is about a QUIET skip, and it is right about quiet skips.** So
//! this one is not quiet: it prints what is missing and how to supply it, and it
//! says `THIS IS NOT A PASS` in the tree's established idiom. The count of skips is
//! then a measurement — *"81 skipped, no apparatus"* is a fact about the machine,
//! where `exit code 101` was a fact about nothing.
//!
//! And an assertion that knows it cannot run and panics anyway is reporting
//! **absent apparatus as a failed assertion**. Those are different findings, which
//! is the distinction this tree already drew twice: `ToolOutcome::Abstained` exists
//! because *"a tool runtime that collapses 'no answer' into 'success with empty
//! payload' makes that failure invisible"* — and this is the other half, where
//! collapsing "could not ask" into "the answer is no" makes a real no invisible.
//!
//! # What this deliberately does NOT do
//!
//! **A configured endpoint that refuses is still a failure.** [`present`] is for
//! apparatus that is either on the machine or not — a file, a kernel facility. A
//! MODEL SERVER is not that, and the two look identical at the socket: nothing
//! answers, either way. They are not the same fact, so
//! `letibot_turn::serving::skip_live_test` keeps them apart by asking whether an
//! endpoint was CONFIGURED, and refuses to skip when one was. See its docs.
//!
//! # `LETIBOT_REQUIRE_APPARATUS`
//!
//! Set it to anything but `0` and a missing apparatus PANICS instead of skipping.
//! That is how a run says "I am on the box and I mean it" — the operator's own
//! machine, or a runner somebody has provisioned. Without it a skip cannot be
//! refused, and a skip nobody can refuse is a suite that quietly stops testing.

use std::path::PathBuf;

/// The vocabulary this box tokenises with.
///
/// **One home for a path that was written out in six crates.** It was
/// `/home/dead/models/qwen3.8-flash-next/…` in `tokencore`, `engine_decisions`,
/// `compaction`, `restore`, `todos_live` and `compact_live` — six copies of one
/// string, which is the shape that drifts.
///
/// **An explicit `LETIBOT_VOCAB_GGUF` WINS, and is not a hint.** The first version
/// of this scanned its candidates for the first one that existed, which MEANT a
/// variable pointing at a real file was fine but one pointing at a missing file was
/// silently replaced by the default — so `LETIBOT_VOCAB_GGUF=/nonexistent` still
/// loaded the box's own vocabulary, and the absent case could not be tested at all.
/// Its own test caught it. An override that can be overridden is not an override.
///
/// So: `LETIBOT_VOCAB_GGUF` if set, no questions asked. Otherwise the defaults are
/// scanned, because those are guesses and a guess that finds a file is a better
/// guess than one that does not:
///
///   1. `$HOME/models/qwen3.8-flash-next/…` — the same file as (2) on the box this
///      was written on, and the RIGHT answer on any other;
///   2. the literal path, so a box with neither still gets a concrete name in the
///      message rather than an empty string.
pub fn gguf_path() -> PathBuf {
    const UNDER_HOME: &str =
        "models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf";
    const LITERAL: &str =
        "/home/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf";

    if let Ok(p) = std::env::var("LETIBOT_VOCAB_GGUF") {
        return PathBuf::from(p);
    }
    let under_home = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(UNDER_HOME));
    if let Some(p) = under_home.as_ref().filter(|p| p.is_file()) {
        return p.clone();
    }
    let literal = PathBuf::from(LITERAL);
    if literal.is_file() {
        return literal;
    }
    under_home.unwrap_or(literal)
}

/// Is a missing apparatus a failure rather than a skip? `LETIBOT_REQUIRE_APPARATUS`.
///
/// Anything but empty or `0` means yes, matching `LETIBOT_REQUIRE_MODEL`.
pub fn required() -> bool {
    std::env::var("LETIBOT_REQUIRE_APPARATUS")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// **`Some(())` when the apparatus is here; `None` after announcing the skip.**
///
/// The shape is `let Some(_) = apparatus::present(what, here) else { return };`, so
/// a call site is one line and reads the same everywhere — the same shape
/// `loop_closes.rs` uses for its endpoint (`let Some(cfg) = config() else { return };`).
///
/// * `what` names the thing in a person's words — *"a cgroup v2 tree"*, not
///   *"cgroups"*. It goes into the skip line, which is the only place a reader
///   learns why nothing ran.
/// * `here` is the caller's own probe. This function never guesses: it is told.
pub fn present(what: &str, here: bool) -> Option<()> {
    if here {
        return Some(());
    }
    if required() {
        panic!(
            "LETIBOT_REQUIRE_APPARATUS is set, and {what} is not on this machine.\n\n\
             This run does not accept a skip. Either run it where the apparatus is, or \
             unset LETIBOT_REQUIRE_APPARATUS to let it skip with a reason."
        );
    }
    eprintln!(
        "SKIPPED: {what} is not on this machine, so nothing was run and THIS IS NOT A PASS.\n\
         \x20 Run this on the box that has it, or set LETIBOT_REQUIRE_APPARATUS=1 to make\n\
         \x20 its absence a failure rather than a skip."
    );
    None
}

/// The vocabulary, or `None` with the skip already announced.
///
/// The convenience most tests want, because "does the GGUF exist" is the whole
/// probe for all of them.
pub fn present_gguf() -> Option<PathBuf> {
    let p = gguf_path();
    present(&format!("a vocabulary GGUF ({})", p.display()), p.is_file())?;
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **`present` must actually refuse when told to.** Without this the whole
    /// mechanism is a function that always returns `Some`, which is the can-only-pass
    /// shape one layer in.
    #[test]
    fn present_returns_none_and_announces_when_the_apparatus_is_absent() {
        let saved = std::env::var_os("LETIBOT_REQUIRE_APPARATUS");
        // SAFETY: single-threaded test; restored below.
        unsafe { std::env::remove_var("LETIBOT_REQUIRE_APPARATUS") };
        assert_eq!(present("a test fixture", true), Some(()));
        assert_eq!(present("a test fixture", false), None);
        unsafe {
            if let Some(v) = saved {
                std::env::set_var("LETIBOT_REQUIRE_APPARATUS", v);
            }
        }
    }

    /// And it must turn into a failure when a run says it means it — otherwise the
    /// skip cannot be refused and nobody can prove full coverage on a box that has
    /// the apparatus.
    #[test]
    fn require_apparatus_turns_the_skip_into_a_failure() {
        let saved = std::env::var_os("LETIBOT_REQUIRE_APPARATUS");
        // SAFETY: single-threaded test; restored below.
        unsafe { std::env::set_var("LETIBOT_REQUIRE_APPARATUS", "1") };
        let blew = std::panic::catch_unwind(|| present("a test fixture", false)).is_err();
        unsafe { std::env::remove_var("LETIBOT_REQUIRE_APPARATUS") };
        if let Some(v) = saved {
            unsafe { std::env::set_var("LETIBOT_REQUIRE_APPARATUS", v) };
        }
        assert!(
            blew,
            "with LETIBOT_REQUIRE_APPARATUS set, a missing fixture must fail"
        );
    }

    /// `gguf_path` honours the override and never returns an empty path — the
    /// message a person reads depends on it naming something.
    #[test]
    fn gguf_path_honours_the_override_and_is_never_empty() {
        let saved = std::env::var_os("LETIBOT_VOCAB_GGUF");
        unsafe { std::env::set_var("LETIBOT_VOCAB_GGUF", "/tmp/definitely-not-a-gguf") };
        assert_eq!(gguf_path(), PathBuf::from("/tmp/definitely-not-a-gguf"));
        unsafe {
            match saved {
                Some(v) => std::env::set_var("LETIBOT_VOCAB_GGUF", v),
                None => std::env::remove_var("LETIBOT_VOCAB_GGUF"),
            }
        }
        assert!(!gguf_path().as_os_str().is_empty());
    }
}
