//! Whether the apparatus a test needs is on THIS machine.
//!
//! A test that panics because the box has no vocabulary, no cgroup subtree or no store is
//! reporting absent apparatus as a failed assertion, which is a different thing. So a test
//! asks first, and the answer is `Some` when the thing is here and `None` after a line saying
//! what could not be shown. `LETIBOT_REQUIRE_APPARATUS=1` turns a skip into a failure, and it
//! is read per process, never set here.

use std::path::PathBuf;

/// The vocabulary this box tokenises with: under `$HOME` first, then the Linux box's literal
/// path. One home for the copies that each had to decide whether to skip.
pub fn gguf_path() -> PathBuf {
    gguf_path_from(
        std::env::var_os("LETIBOT_VOCAB_GGUF"),
        std::env::var_os("HOME"),
    )
}

/// The GLM vocabulary, found the way [`gguf_path`] finds the default one: `$HOME` first, then
/// the literal path, so four tests stop carrying it.
pub fn glm_gguf_path() -> PathBuf {
    const UNDER_HOME: &str = "models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf";
    const LITERAL: &str =
        "/home/dead/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf";
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(UNDER_HOME))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(LITERAL))
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

/// `Some(())` when the apparatus is here, `None` after announcing the skip. The shape is
/// `let Some(_) = apparatus::present(what, here) else { return };`, so a skip is one line.
pub fn present(what: &str, here: bool) -> Option<()> {
    present_given(what, here, required())
}

/// [`present`] with [`required`]'s answer handed in rather than read - the seam its test uses,
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

/// `Some(())` when what this test needs could exist on this platform at all, `None` after saying
/// that it cannot: [`present`]'s sibling for an absence no box can fix.
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

/// Announce the skip and return, for a caller that has already discovered the absence.
pub fn absent(what: &str) {
    let _ = present(what, false);
}

/// Is the operator's REAL session store something this run may open?
/// `LETIBOT_CHECK_REAL_STORE`, and yes only when it is set to something other than `0`.
pub fn real_store_allowed() -> bool {
    real_store_allowed_given(std::env::var("LETIBOT_CHECK_REAL_STORE").ok().as_deref())
}

/// [`real_store_allowed`] with the reading handed in rather than read - the seam its test uses,
/// so the test never sets a variable under a run that set it for real (R67 item 17).
fn real_store_allowed_given(flag: Option<&str>) -> bool {
    matches!(flag, Some(v) if !v.is_empty() && v != "0")
}

/// The same decision plus the skip line a reader needs, and never a panic. [`required`] is not
/// consulted here: see [`real_store_allowed`].
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Both branches of `present`, and neither sets `LETIBOT_REQUIRE_APPARATUS`: the variable is
    /// PROCESS-GLOBAL and libtest runs a binary's tests in parallel.
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

    /// `applies` is not refusable, and that is the whole difference from `present`: there is no
    /// variable to read and no seam to hand one in.
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

    /// The real store is opt-in, and the apparatus prefix cannot demand it: the two decisions side
    /// by side, because the difference is the point.
    #[test]
    fn the_real_store_is_opt_in_and_the_apparatus_prefix_cannot_demand_it() {
        assert!(real_store_allowed_given(Some("1")));
        assert!(real_store_allowed_given(Some("yes")));
        assert!(!real_store_allowed_given(None));
        assert!(!real_store_allowed_given(Some("")));
        assert!(!real_store_allowed_given(Some("0")));

        // The prefix DOES refuse a missing apparatus - that is what it is for …
        assert!(std::panic::catch_unwind(|| present_given("a store", false, true)).is_err());
        // … and the real store's decision is not that one: no flag, no panic, no reading.
        assert!(!real_store_allowed_given(None));
    }

    /// `gguf_path` honours the override and never returns an empty path - the
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
        // With no override, a path is still named - with a HOME or without one.
        assert!(!gguf_path_from(None, None).as_os_str().is_empty());
        assert!(
            !gguf_path_from(None, Some("/nonexistent-home".into()))
                .as_os_str()
                .is_empty()
        );
        assert!(!gguf_path().as_os_str().is_empty());
    }
}
