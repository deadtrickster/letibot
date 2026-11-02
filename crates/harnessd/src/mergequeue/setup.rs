//! A gate that cannot judge is told apart from a target that is broken. A gate can pass
//! every step it declares and then fail in its own wrapper - a script naming a binary this
//! branch does not build - and nothing in the code has been judged.

use letibot_tokencore::store::{HealthState, MergeGateOutcome, MergeGateStep, TargetHealth};

/// The clause a target's evidence leads with when its gate's own setup failed; read off the row by
/// the project's session notice and by `/queue regate`'s answer.
pub const GATE_SETUP_FAILED: &str = "the gate's own setup failed";

/// Whether a target's evidence says its tip was never judged: the gate's own setup failed
/// ([`GATE_SETUP_FAILED`]), or its step was killed before it judged the code
/// ([`super::gate::GATE_KILLED`]).
pub fn unjudged(evidence: &str) -> bool {
    evidence.starts_with(GATE_SETUP_FAILED) || evidence.starts_with(super::gate::GATE_KILLED)
}

/// Whether this health row is a repository that never asked for a queue: its target
/// declares no `Merge gate` section, so the row is about a branch nobody could judge and
/// nothing else.
pub fn not_a_queue_target(h: &TargetHealth) -> bool {
    h.state == HealthState::Blocked && h.evidence.starts_with(super::WAITING_FOR_GATE)
}

/// How many of the failing output's last lines are searched for the words that name a missing
/// thing - the end is where a script says why it stopped.
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

/// The step's own words, with the gate's own notes dropped. A row's `output` ends with
/// lines the GATE wrote - `[gate memory: …]`, `[step
pub(crate) fn step_words(out: &str) -> impl Iterator<Item = &str> {
    out.lines().filter(|l| {
        let t = l.trim_start();
        !(t.starts_with("[gate memory: ")
            || t.starts_with("[gate on ")
            || t.starts_with("[step: ")
            || (t.starts_with('[') && t.ends_with("with its cgroup]")))
    })
}

/// Output that shows a build or a test PASSING (cargo's own words).
fn shows_a_pass(out: &str) -> bool {
    step_words(out).any(|l| {
        let t = l.trim_start();
        t.starts_with("Finished ") || t.starts_with("test result: ok")
    })
}

/// Output that shows the CODE failing - a compile error, a failed test, a panic, a format diff.
pub(crate) fn shows_code_failing(out: &str) -> bool {
    step_words(out).any(|l| {
        let t = l.trim_start();
        t.starts_with("error[")
            || t.starts_with("error: could not compile")
            || t.starts_with("test result: FAILED")
            || t.contains("panicked at")
            || t.starts_with("Diff in ")
            || t.starts_with("failures:")
            // Cargo's own verdict on failing test TARGETS, `error
            || (t.starts_with("error: ") && t.contains("target") && t.contains("failed"))
    })
}

/// The last words name something MISSING - a path, a binary, a command.
fn names_a_missing_thing(out: &str) -> Option<String> {
    let lines: Vec<&str> = step_words(out)
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

/// Whether a failed gate is its own setup failing, and if so the sentence that says where
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

/// The end of the failing step's output, at most `lines` lines: what a message puts FIRST.
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

    /// One wrapper step whose build and tests passed and which then found no binary. Before this
    /// rule the queue called it RED; the fixture says where the landing tree lives now, not where
    /// the old wrapper said it did.
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

    /// A RED step was reported as the gate's own setup, and nothing could land on a tip nothing
    /// was wrong with. The step is the workspace's tests under the gate - two failing targets -
    /// and the gate appends its own `[gate memory
    #[test]
    fn a_red_step_is_red_even_behind_the_gates_own_memory_note() {
        let out = "running 0 tests\n\n\
                   test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; \
                   finished in 0.00s\n\n\
                   error: 2 targets failed:\n    \
                   `-p letibot-harnessd --test session_lifecycle`\n    \
                   `-p letibot-harnessd --test stop_ends_runs`\n\n\
                   [gate memory: uncapped — the step ran in the daemon's own cgroup: there is no \
                   process-lifetime mechanism on this backend, so a process started here would \
                   have no owner and nothing would reap it: `/proc/self/cgroup` could not be read: \
                   No such file or directory (os error 2)]\n\
                   [step: exit code 101]\n";
        assert!(
            shows_code_failing(out),
            "cargo's own verdict on failing targets must count as the code failing"
        );
        assert_eq!(
            setup_failure(&[step(
                "LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --no-fail-fast -- --nocapture",
                MergeGateOutcome::Failed,
                out
            )]),
            None,
            "a step with two failing test targets was called the gate's setup"
        );
    }

    /// And the annotation alone must never be read as the step's words: an uncapped note with no
    /// failing code behind it is a setup failure only if something else names a missing thing.
    #[test]
    fn the_gates_notes_are_not_the_steps_words() {
        assert_eq!(step_words("[gate memory: uncapped — /proc/self/cgroup: No such file or directory (os error 2)]").count(), 0);
        assert_eq!(step_words("[step: exit code 101]").count(), 0);
        assert_eq!(
            step_words("[3 processes the step left running were killed with its cgroup]").count(),
            0
        );
        assert_eq!(
            names_a_missing_thing("[gate memory: uncapped — No such file or directory]"),
            None
        );
        // And the host gate's notes are the gate's too: what a machine has not got, and that a
        // step's demand was taken off it, are not the step saying anything about the code.
        assert_eq!(
            step_words("[gate on lab2x1: `LETIBOT_REQUIRE_APPARATUS=1` was TAKEN OFF 1 of 5")
                .count(),
            0
        );
        assert_eq!(
            names_a_missing_thing(
                "[gate on lab2x1: this machine cannot meet it (a cgroup v2 subtree is not on \
                 this machine)]"
            ),
            None
        );
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
