//! **The gate runs here and on the operator's declared hosts, never on GitHub Actions** (R62).
//!
//! The operator's words, 2026-10-11: *"this is mostly local concern - i've been rate limited on
//! github recently with actions, so _act_ is our friend. we can use other machines like lubuntu2 to
//! run tests"*. So the jobs that judged a branch moved to the merge queue - which runs the
//! `## Merge gate` steps in a landing worktree here or on the machine `[queue] gate_host` names -
//! and a workflow that runs them again is a second, weaker gate: it judges a push rather than a
//! rebased branch, nobody reads its red, and it spends the rate limit the operator was rationing.
//!
//! What a workflow may still do is RELEASE: build the assets for a tag and publish them, which is
//! what `install.sh` downloads and nothing about the gate. `release.yml` is that file.

use std::path::{Path, PathBuf};

use letibot_tools::gatekeeper::parse_merge_gate;

/// The repository this test was built in.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every workflow file under `.github/workflows`, in name order, with its text.
fn workflows() -> Vec<(String, String)> {
    let dir = root().join(".github/workflows");
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("no workflows directory at {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let yml = name.ends_with(".yml") || name.ends_with(".yaml");
        if !path.is_file() || !yml {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a workflow file is text");
        out.push((name, text));
    }
    out.sort();
    out
}

/// A workflow's lines with the comments taken out, so a note about a step is not read as the step.
fn uncommented(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// **No workflow judges a branch.** `pull_request` is the trigger whose red would be a second
/// verdict on the same commit the queue is rebasing - and the one that spends the rate limit.
#[test]
fn no_workflow_runs_on_a_pull_request() {
    for (name, text) in workflows() {
        for line in uncommented(&text) {
            assert!(
                !line.trim_start().starts_with("pull_request"),
                "{name} runs on a pull request. Branches are gated by the merge queue, in a \
                 landing worktree here or on `[queue] gate_host`'s machine; a workflow that \
                 judges a push is a second gate nobody acts on, and it is the GitHub Actions \
                 budget R62 exists to stop spending. See docs/gate.md."
            );
        }
    }
}

/// **No workflow runs the gate's own steps.** The `## Merge gate` section of AGENTS.md is the one
/// definition, read from `main` by the queue; a workflow holding a copy of it can be deleted, can
/// drift, and can go green while the queue is red.
#[test]
fn no_workflow_runs_the_gate() {
    // The steps that can only mean "judge this code". `cargo build` is not among them: the release
    // workflow builds the binaries it packages, which is a different question asked of the tree.
    const GATE_STEPS: &[&str] = &[
        "cargo test",
        "cargo clippy",
        "clippy-gate.sh",
        "check-fmt.sh",
        "check-test-attrs.sh",
    ];
    for (name, text) in workflows() {
        for line in uncommented(&text) {
            for step in GATE_STEPS {
                assert!(
                    !line.contains(step),
                    "{name} runs `{step}`, which is one of the `## Merge gate` steps. Those run \
                     in the queue's landing worktree - locally or on `[queue] gate_host` - and \
                     nowhere else; see docs/gate.md for how to run them by hand."
                );
            }
        }
    }
}

/// **And the repository still has a gate to run**, because the queue holds every branch of a
/// repository whose `AGENTS.md` has no `## Merge gate` section. Deleting the workflow must not have
/// deleted the definition with it.
#[test]
fn the_repository_still_declares_its_gate() {
    let path = ["AGENTS.md", "agents.md", "Agents.md"]
        .iter()
        .map(|f| root().join(f))
        .find(|p| p.is_file())
        .expect("the repository has an AGENTS.md, which is where the queue reads its gate from");
    let text = std::fs::read_to_string(&path).expect("AGENTS.md is text");
    let steps = parse_merge_gate(&text).unwrap_or_else(|| {
        panic!(
            "{} has no `## Merge gate` section, so the merge queue would hold every branch of \
             this repository",
            path.display()
        )
    });
    assert!(
        steps.len() >= 5,
        "the gate is {} step(s): {steps:?}",
        steps.len()
    );
    assert!(
        steps.iter().any(|s| s.contains("cargo test")),
        "the gate does not run the tests: {steps:?}"
    );
}
