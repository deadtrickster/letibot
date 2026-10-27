//! **The refusal when there is no helper to apply a profile with** — on this platform.
//!
//! Its own test binary on purpose. The seam is an environment variable
//! ([`SANDBOX_ENV`](letibot_tools::exec::seatbelt::SANDBOX_ENV)), and a process's environment is
//! shared by every test thread in it: setting it in `tests/seatbelt.rs` would make its
//! concurrent probes read a helper that is not there, which is a flake dressed as a measurement.
//! One binary, one test, one variable — and the other file is left to measure what a real
//! `sandbox-exec` does.

#![cfg(target_os = "macos")]

use letibot_tools::exec::confine::{Egress, ViewSpec};
use letibot_tools::exec::seatbelt::{SANDBOX_ENV, Seatbelt};

#[test]
fn a_helper_that_is_not_there_is_a_refusal_that_says_what_to_do_instead() {
    // SAFETY: this binary is the only place that reads or writes this variable, and it holds
    // one test.
    unsafe { std::env::set_var(SANDBOX_ENV, "/nonexistent/sandbox-exec") };
    let e = Seatbelt::probe(ViewSpec::project_only(std::env::temp_dir()), Egress::Denied);
    unsafe { std::env::remove_var(SANDBOX_ENV) };

    let m = format!("{}", e.expect_err("a missing helper cannot confine"));
    assert!(m.contains("/nonexistent/sandbox-exec"), "{m}");
    // The refusal names the seam and the way back, and it says that nothing ran — the one
    // sentence a boundary refusal must never omit.
    assert!(m.contains(SANDBOX_ENV), "{m}");
    assert!(m.contains("Nothing was run"), "{m}");
    assert!(m.contains("was NOT run"), "{m}");
}
