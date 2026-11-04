//! The refusal when there is no helper to apply a profile with. Own binary: the seam is the process
//! environment, which every test thread shares.

#![cfg(target_os = "macos")]

use letibot_tools::exec::confine::{Egress, ViewSpec};
use letibot_tools::exec::seatbelt::{SANDBOX_ENV, Seatbelt};

#[test]
fn a_helper_that_is_not_there_is_a_refusal_that_says_what_to_do_instead() {
    // SAFETY: this binary holds one test, so nothing else reads or writes the variable.
    unsafe { std::env::set_var(SANDBOX_ENV, "/nonexistent/sandbox-exec") };
    let e = Seatbelt::probe(ViewSpec::project_only(std::env::temp_dir()), Egress::Denied);
    unsafe { std::env::remove_var(SANDBOX_ENV) };

    let m = format!("{}", e.expect_err("a missing helper cannot confine"));
    assert!(m.contains("/nonexistent/sandbox-exec"), "{m}");
    // A boundary refusal must name the seam, the way back, and that nothing ran.
    assert!(m.contains(SANDBOX_ENV), "{m}");
    assert!(m.contains("Nothing was run"), "{m}");
    assert!(m.contains("was NOT run"), "{m}");
}
