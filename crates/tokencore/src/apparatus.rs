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
//!
//! **The operator's real session store is the one thing it cannot demand**, because that store is
//! the box's DATA rather than a prerequisite of the code under review: a red on somebody's
//! corrupted transcript says nothing about a diff. [`check_real_store`] is the opt-in
//! (`LETIBOT_CHECK_REAL_STORE`) that keeps that canary available to whoever runs it on purpose.
//!
//! # Two kinds of absence, and only one of them is the prefix's business
//!
//! [`present`] answers *"is this on the machine?"*, which presumes the machine could have it.
//! [`applies`] answers the question in front of that one — *"could this platform have it at
//! all?"* — and it exists because the two got collapsed and the merge gate went red on a Mac
//! for a reason no change to the tree could fix. `LETIBOT_REQUIRE_APPARATUS=1` is for a box that
//! has NOT BEEN PROVISIONED: it is a demand somebody can satisfy by installing the thing.
//! `bwrap`, unprivileged user namespaces, cgroup v2, `systemd-run` and `ss` are not that on
//! macOS — they cannot be installed there, so demanding them made *"run the tests where the
//! apparatus is"* mean *"run them on another computer"*.

use std::path::PathBuf;

/// The vocabulary this box tokenises with.
///
/// **One home for the copies that had to decide whether to skip.** It was
/// `/home/dead/models/qwen3.8-flash-next/…` in `tokencore`, `engine_decisions`,
/// `compaction`, `live_e2e`, `live_qwen`, `loop_closes`, `restore`, `todos_live`
/// and `compact_live` — the same string in nine places, which is the shape that
/// drifts, with `LETIBOT_VOCAB_GGUF` handled slightly differently in each.
///
/// **What is deliberately NOT consolidated**, so this claim is checkable rather
/// than sweeping: `crates/harnessd/src/config.rs` keeps its own copy, because that
/// one is the DAEMON'S default vocabulary rather than a test's, and a daemon
/// refusing to start without a vocabulary is the behaviour we want. `serving.rs`
/// and the remaining live-test helpers are not touched here — they are read-only
/// about the path and already guard on the server before they need it.
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
    gguf_path_from(
        std::env::var_os("LETIBOT_VOCAB_GGUF"),
        std::env::var_os("HOME"),
    )
}

/// [`gguf_path`] with `LETIBOT_VOCAB_GGUF` and `HOME` handed in rather than read, so its test
/// sets no variable (R67 item 17: a `set_var` in a test is seen by every other test running in
/// the binary at that moment).
fn gguf_path_from(over: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> PathBuf {
    const UNDER_HOME: &str =
        "models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf";
    const LITERAL: &str =
        "/home/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf";

    if let Some(p) = over {
        return PathBuf::from(p);
    }
    let under_home = home.map(|h| PathBuf::from(h).join(UNDER_HOME));
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
///
/// **A thing this platform cannot have at all is [`applies`], not a missing apparatus**: the
/// skip here is one `LETIBOT_REQUIRE_APPARATUS=1` refuses, which is right for a box that has
/// not been provisioned and wrong for a facility macOS can never be given.
pub fn present(what: &str, here: bool) -> Option<()> {
    present_given(what, here, required())
}

/// [`present`] with [`required`]'s answer handed in rather than read — the seam its test uses,
/// so the test never sets `LETIBOT_REQUIRE_APPARATUS` under a run that set it for real.
fn present_given(what: &str, here: bool, required: bool) -> Option<()> {
    if here {
        return Some(());
    }
    if required {
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

/// **`Some(())` when what this test needs could exist on this platform at all; `None` after
/// saying that it cannot.**
///
/// [`present`]'s sibling for the other absence. Two things make a test's apparatus missing and
/// they are not the same finding:
///
///   * **not provisioned** — a vocabulary GGUF, a delegated cgroup v2 subtree, a store
///     fixture. Real on this platform, absent on this box, and **somebody can go and get
///     it**, which is exactly what `LETIBOT_REQUIRE_APPARATUS=1` says. [`present`] is for
///     that one.
///   * **not applicable on this platform** — `bwrap` and unprivileged user namespaces, cgroup
///     v2, `systemd-run`, `ss`. macOS can never have any of them, however it is provisioned,
///     so a demand for one is a demand that cannot be met by provisioning. This is for that
///     one, and it is deliberately **not refusable**: there is nothing for a run to go and
///     install.
///
/// The collapse of the two is what made it matter. MEASURED 2026-10-26: the merge queue's
/// daemon runs on the operator's Mac, so `LETIBOT_REQUIRE_APPARATUS=1` — the gate's own test
/// step — turned four targets red there (`confine`, `toolchain_grant`, `memory_cap`, and the
/// launcher's `ss` cross-check) for a reason that was in no diff, on a gate whose red was
/// therefore ignored. A test that cannot apply on this OS is not missing apparatus.
///
/// `on_this_platform` is the caller's own claim, and the honest way to make it is
/// `cfg!(target_os = "linux")` — a compile-time fact, not a probe that can go stale. **Where
/// the test itself only means anything on one platform, prefer `#[cfg(target_os = …)]` on it
/// and do not call this at all.** This is for the test whose OTHER half is measured
/// everywhere — a boundary that cannot exist still has a refusal to assert — where dropping
/// the test would throw that half away.
pub fn applies(what: &str, on_this_platform: bool) -> Option<()> {
    if on_this_platform {
        return Some(());
    }
    eprintln!(
        "NOT APPLICABLE on {}: {what} — this test has nothing to test here, so nothing was \
         run and THIS IS NOT A PASS.",
        std::env::consts::OS
    );
    None
}

/// Announce the skip and return `()`, for a caller that has ALREADY discovered the
/// absence and is in an `else` branch.
///
/// `let Some(x) = probe() else { return apparatus::absent("a store"); };` reads as one
/// line, where the alternative is a `present` call whose `Some` arm is unreachable and
/// has to be written anyway. `present`'s refusal under `LETIBOT_REQUIRE_APPARATUS` still
/// fires, so this cannot become a quieter path.
pub fn absent(what: &str) {
    let _ = present(what, false);
}

/// **Is the operator's REAL session store something this run may open?**
/// `LETIBOT_CHECK_REAL_STORE`.
///
/// Yes only when the variable is set to something other than empty or `0`. With it unset,
/// [`check_real_store`] announces the skip and answers `false`.
///
/// # Why this is NOT [`present`]
///
/// `~/.local/share/letibot/sessions.db` is not apparatus in the sense the rest of this module
/// means: it is the BOX'S DATA, and the tests that read it are a canary for that data rather
/// than proof about the code a merge gate judges. MEASURED 2026-10-26: this is the test the
/// queue's `LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace` step went red on, failing on ONE
/// corrupted transcript the operator's own box had accumulated — a red no change
/// to the tree could fix, on a gate whose result nobody could trust because it was red for days.
/// So the prefix must not be able to demand this one: a run that insists on its apparatus is
/// insisting that the CODE's prerequisites be here, and it cannot insist that somebody's history
/// be intact. An opt-in flag keeps the canary available to the person who wants it.
pub fn real_store_allowed() -> bool {
    real_store_allowed_given(std::env::var("LETIBOT_CHECK_REAL_STORE").ok().as_deref())
}

/// [`real_store_allowed`] with the reading handed in rather than read — the seam its test uses,
/// so the test never sets a variable under a run that set it for real (R67 item 17).
fn real_store_allowed_given(flag: Option<&str>) -> bool {
    matches!(flag, Some(v) if !v.is_empty() && v != "0")
}

/// The same decision, plus the skip line a reader needs, and never a panic.
///
/// Nothing here consults [`required`]: see [`real_store_allowed`] for why the operator's data
/// cannot be made a gate requirement.
pub fn check_real_store() -> bool {
    if real_store_allowed() {
        return true;
    }
    eprintln!(
        "SKIPPED: this run does not read the operator's real session store (set \
         LETIBOT_CHECK_REAL_STORE=1 to check it by hand) — THIS IS NOT A PASS."
    );
    false
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

    /// **Both branches of `present`** — and neither sets `LETIBOT_REQUIRE_APPARATUS`.
    ///
    /// The variable is PROCESS-GLOBAL and libtest runs a binary's tests in PARALLEL. Written
    /// first as two tests, one setting it and one reading it, they raced: MEASURED on a full
    /// `cargo test --workspace --no-fail-fast -- --nocapture` run, both panicked with
    /// `LETIBOT_REQUIRE_APPARATUS is set` while neither had set it. Folding them into one test
    /// with a save/restore narrowed that to this binary, and still unset the variable for a
    /// moment under a gate run that had set it — the window in which any other test here
    /// would have skipped where the run demanded a failure (R67 item 17). So the switch is a
    /// parameter of [`present_given`], and this test reads no environment at all.
    #[test]
    fn present_skips_when_absent_and_fails_when_the_run_demands_the_apparatus() {
        // Present: no skip, no failure, whatever the run demands.
        assert_eq!(present_given("a test fixture", true, false), Some(()));
        assert_eq!(present_given("a test fixture", true, true), Some(()));

        // Absent, and nobody is demanding it: a skip.
        assert_eq!(
            present_given("a test fixture", false, false),
            None,
            "an absent fixture must skip rather than fail when the run does not insist"
        );

        // Absent, and the run insists: a FAILURE. This is what makes the skip
        // refusable, so a box that has the apparatus can prove full coverage.
        let blew =
            std::panic::catch_unwind(|| present_given("a test fixture", false, true)).is_err();
        assert!(
            blew,
            "with LETIBOT_REQUIRE_APPARATUS set, a missing fixture must fail"
        );
    }

    /// **`applies` is not refusable, and that is the whole difference from `present`.**
    ///
    /// There is no variable to read and no seam to hand one in, which is the point: a skip
    /// this returns cannot be turned into a failure by `LETIBOT_REQUIRE_APPARATUS` — the
    /// absence it reports is a platform's, not a box's, and no run can go and provision it.
    /// This test therefore reads no environment at all and passes under the gate's own
    /// `LETIBOT_REQUIRE_APPARATUS=1` run, where `present_given(…, false, true)` panics.
    #[test]
    fn applies_skips_without_being_refusable_and_present_still_is() {
        assert_eq!(applies("a test fixture", true), Some(()));
        assert_eq!(
            applies("a test fixture", false),
            None,
            "a thing this platform cannot have has nothing to test here"
        );
        // The distinction, side by side: the same absence through `present` is a demand.
        assert!(
            std::panic::catch_unwind(|| present_given("a test fixture", false, true)).is_err(),
            "`present` must stay refusable — only `applies` stops being one"
        );
    }

    /// **The real store is opt-in, and the apparatus prefix cannot demand it.**
    ///
    /// The two decisions side by side, because the difference is the whole point: a run with
    /// `LETIBOT_REQUIRE_APPARATUS=1` refuses a missing APPARATUS, and still skips the operator's
    /// data. If this ever went through `present`, the queue's `--workspace` step would go red on
    /// somebody's corrupted transcript again.
    #[test]
    fn the_real_store_is_opt_in_and_the_apparatus_prefix_cannot_demand_it() {
        assert!(real_store_allowed_given(Some("1")));
        assert!(real_store_allowed_given(Some("yes")));
        assert!(!real_store_allowed_given(None));
        assert!(!real_store_allowed_given(Some("")));
        assert!(!real_store_allowed_given(Some("0")));

        // The prefix DOES refuse a missing apparatus — that is what it is for …
        assert!(std::panic::catch_unwind(|| present_given("a store", false, true)).is_err());
        // … and the real store's decision is not that one: no flag, no panic, no reading.
        assert!(!real_store_allowed_given(None));
    }

    /// `gguf_path` honours the override and never returns an empty path — the
    /// message a person reads depends on it naming something. The readings are handed to
    /// [`gguf_path_from`], not set in the environment (R67 item 17).
    #[test]
    fn gguf_path_honours_the_override_and_is_never_empty() {
        assert_eq!(
            gguf_path_from(
                Some("/tmp/definitely-not-a-gguf".into()),
                Some("/home/x".into())
            ),
            PathBuf::from("/tmp/definitely-not-a-gguf")
        );
        // With no override, a path is still named — with a HOME or without one.
        assert!(!gguf_path_from(None, None).as_os_str().is_empty());
        assert!(
            !gguf_path_from(None, Some("/nonexistent-home".into()))
                .as_os_str()
                .is_empty()
        );
        assert!(!gguf_path().as_os_str().is_empty());
    }
}
