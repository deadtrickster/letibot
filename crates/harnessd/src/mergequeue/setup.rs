//! **A gate that cannot judge is told apart from a target that is broken** (R76 item 5).
//!
//! pg-noop's `main`, 2026-10-11: every step of `scripts/merge_gate.sh` passed — fmt, check, test,
//! clippy, build — and then the script failed *"no binary at …/target/release/letidb after a
//! successful build"*, because `main` builds `pgnoop` and the script names what another branch
//! builds. The queue called that `RED` at emergency severity and put the tip up for a reviewer to
//! find *what in the target's history broke it* — and nothing in the code had. The gate's own
//! setup failed; the code was never judged.
//!
//! So a failed gate is read once more before it is called red. It is the GATE's setup when the
//! failure came AFTER the build and the tests passed:
//!
//! * **by step** — the gate declares several steps, every one before the failing one passed, at
//!   least one of those was a build or a test, and the failing step is neither; or
//! * **by its own words** — one step (a wrapper script, as pg-noop's is) whose output shows a
//!   build or a test passing, shows no compile error, failed test, panic or format diff, and ends
//!   saying something is missing (*no binary*, *no such file*, *not found*, *not executable* …).
//!
//! Both are deliberately narrow: a regression the corpus catches after the build is still RED,
//! because its last words name no missing thing. And either way the tip stays UNJUDGED — the
//! target's health is `blocked`, so nothing lands on it (requirement: *nothing lands on an
//! unjudged target*); what changes is the word, the severity, and who is asked: the project's
//! session and the operator, not a reviewer of the code.

use letibot_tokencore::store::{HealthState, MergeGateOutcome, MergeGateStep, TargetHealth};

/// **The clause a target's evidence leads with when its gate's own setup failed** — read off the
/// row by the project's session notice and by `/queue regate`'s answer.
pub const GATE_SETUP_FAILED: &str = "the gate's own setup failed";

/// **Whether a target's evidence says its tip was never judged** — the gate's own setup failed
/// ([`GATE_SETUP_FAILED`]), or its step was killed before it judged the code
/// ([`super::gate::GATE_KILLED`]: at the gate's memory cap, or by a signal). Either way the tip
/// is `blocked`, nothing lands on it, and the project hears *cannot judge*, never *red*.
pub fn unjudged(evidence: &str) -> bool {
    evidence.starts_with(GATE_SETUP_FAILED) || evidence.starts_with(super::gate::GATE_KILLED)
}

/// **Whether this health row is a repository that never asked for a queue** — its target declares
/// no `Merge gate` section, so the row is about a branch nobody could judge and nothing else
/// (2026-10-26).
///
/// The operator's supervisor held a lease on every repository of one project folder and fourteen
/// of the fifteen were this row: repositories that had never opted in, each counted as *unjudged*
/// in every head's summary and warned about, beside the one target the queue actually served. A
/// repository is a queue target because it asked to be one — and the way it asks is the section.
/// So this row is not an alarm and is not a count; it is the pane's one muted line (*not queued:
/// no `Merge gate`*), and the store keeps it because nothing is deleted here.
pub fn not_a_queue_target(h: &TargetHealth) -> bool {
    h.state == HealthState::Blocked && h.evidence.starts_with(super::WAITING_FOR_GATE)
}

/// How many of the failing output's last lines are searched for the words that name a missing
/// thing — the end is where a script says why it stopped.
const LAST_LINES: usize = 3;

/// A command that builds or tests: what has to have PASSED before a failure is the setup's.
pub fn builds_or_tests(command: &str) -> bool {
    let c = command.to_ascii_lowercase();
    [
        "cargo build",
        "cargo test",
        "cargo nextest",
        "cargo check",
        "cargo clippy",
        "make",
        "npm test",
        "npm run build",
        "go build",
        "go test",
        "pytest",
        "mvn",
        "gradle",
    ]
    .iter()
    .any(|w| c.contains(w))
}

/// Output that shows a build or a test PASSING (cargo's own words).
fn shows_a_pass(out: &str) -> bool {
    out.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("Finished ") || t.starts_with("test result: ok")
    })
}

/// Output that shows the CODE failing — a compile error, a failed test, a panic, a format diff.
pub(crate) fn shows_code_failing(out: &str) -> bool {
    out.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("error[")
            || t.starts_with("error: could not compile")
            || t.starts_with("test result: FAILED")
            || t.contains("panicked at")
            || t.starts_with("Diff in ")
            || t.starts_with("failures:")
    })
}

/// The last words name something MISSING — a path, a binary, a command.
fn names_a_missing_thing(out: &str) -> Option<String> {
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let words = [
        "no binary",
        "no such file",
        "not found",
        "not executable",
        "does not exist",
        "is missing",
        "missing:",
        "cannot judge",
        "is not on this branch",
        "permission denied",
    ];
    lines
        .iter()
        .rev()
        .take(LAST_LINES)
        .find(|l| {
            let low = l.to_ascii_lowercase();
            words.iter().any(|w| low.contains(w))
        })
        .map(|l| l.to_string())
}

/// **Whether a failed gate is its own setup failing**, and if so the sentence that says where:
/// `"the gate's own setup failed at step N of M (`cmd`): <its last words>"`. `None` is a gate that
/// judged the code and found it red.
pub fn setup_failure(steps: &[MergeGateStep]) -> Option<String> {
    let i = steps
        .iter()
        .position(|s| s.outcome == MergeGateOutcome::Failed)?;
    let failed = &steps[i];
    let before = &steps[..i];
    let all_passed = before.iter().all(|s| s.outcome == MergeGateOutcome::Passed);
    let say = |why: &str| {
        format!(
            "{GATE_SETUP_FAILED} at step {} of {} (`{}`): {why}",
            i + 1,
            steps.len(),
            failed.command
        )
    };
    // By step: the build and the tests are their own steps and passed.
    if i > 0
        && all_passed
        && before.iter().any(|s| builds_or_tests(&s.command))
        && !builds_or_tests(&failed.command)
        && !shows_code_failing(&failed.output)
    {
        let last = failed
            .output
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("it failed without a word");
        return Some(say(&format!(
            "every build and test step before it passed — {last}"
        )));
    }
    // By its own words: one step whose output shows the build and tests passing, then a thing
    // missing.
    if all_passed && shows_a_pass(&failed.output) && !shows_code_failing(&failed.output) {
        let last = names_a_missing_thing(&failed.output)?;
        return Some(say(&format!(
            "its build and tests passed, and then: {last}"
        )));
    }
    None
}

/// **The end of the failing step's output, at most `lines` lines** — what a message puts FIRST.
pub fn failing_tail(steps: &[MergeGateStep], lines: usize) -> Option<(usize, String, String)> {
    let i = steps
        .iter()
        .position(|s| s.outcome == MergeGateOutcome::Failed)?;
    let s = &steps[i];
    let all: Vec<&str> = s.output.lines().collect();
    let from = all.len().saturating_sub(lines);
    Some((i + 1, s.command.clone(), all[from..].join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(command: &str, outcome: MergeGateOutcome, output: &str) -> MergeGateStep {
        MergeGateStep {
            command: command.into(),
            outcome,
            output: output.into(),
            started_ms: 0,
            elapsed_ms: 0,
        }
    }

    /// **pg-noop's own words, 2026-10-11** — one wrapper step whose build and tests passed and
    /// which then found no binary. FAILS before R76 item 5: the queue called it RED. (The path
    /// it printed was the old `/r/.git/letibot-landing/…`; the landing tree has lived in
    /// letibot's data directory since, so the fixture says where it is now.)
    #[test]
    fn a_wrapper_that_passed_its_build_and_then_found_no_binary_is_the_gates_setup() {
        let out = "merge gate: running: cargo test --release\n\
                   test result: ok. 412 passed; 0 failed\n\
                   merge gate: running: cargo build --release\n\
                   \x20   Finished `release` profile [optimized] target(s) in 0.31s\n\
                   merge gate: FAIL: no binary at /home/u/.local/share/letibot/landing/r-0123456789ab/target/release/letidb \
                   after a successful build\n";
        let said = setup_failure(&[step("scripts/merge_gate.sh", MergeGateOutcome::Failed, out)])
            .expect("the gate's setup, not the code");
        assert!(said.starts_with(GATE_SETUP_FAILED), "{said}");
        assert!(said.contains("step 1 of 1"), "{said}");
        assert!(said.contains("no binary at"), "{said}");
    }

    #[test]
    fn a_failed_test_a_compile_error_or_a_corpus_regression_is_still_red() {
        for out in [
            "test result: FAILED. 1 passed; 1 failed\n",
            "error[E0308]: mismatched types\nerror: could not compile `x`\n",
            "    Finished `release` profile\nthread 'x' panicked at src/a.rs:1: no such file\n",
            "    Finished `release` profile\nratchet: 3 corpus files got worse\n",
        ] {
            assert_eq!(
                setup_failure(&[step("scripts/gate.sh", MergeGateOutcome::Failed, out)]),
                None,
                "{out}"
            );
        }
    }

    #[test]
    fn a_step_after_the_passed_build_and_tests_is_the_gates_setup() {
        let steps = [
            step("cargo build --release", MergeGateOutcome::Passed, ""),
            step("cargo test", MergeGateOutcome::Passed, ""),
            step(
                "test -x target/release/letidb",
                MergeGateOutcome::Failed,
                "",
            ),
        ];
        let said = setup_failure(&steps).expect("the setup");
        assert!(said.contains("step 3 of 3"), "{said}");
        // A failing TEST step after a passed build is the code.
        let red = [
            step("cargo build", MergeGateOutcome::Passed, ""),
            step(
                "cargo test",
                MergeGateOutcome::Failed,
                "test result: FAILED",
            ),
        ];
        assert_eq!(setup_failure(&red), None);
    }
}
