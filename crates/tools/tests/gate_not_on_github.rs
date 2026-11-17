//! The gate runs here and on the operator's declared hosts, never on GitHub Actions: the merge
//! queue runs the `## Merge gate` steps in a landing worktree (or on `[queue] gate_host`), and a
//! workflow repeating them is a second, weaker gate. A workflow may still RELEASE (`release.yml`).

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

/// Whether a workflow answers a pull request, in every spelling of `on:`.
fn judges_a_pull_request(text: &str) -> bool {
    letibot_tools::builtins::merge_gate::triggers(text)
        .iter()
        .any(|(event, _)| event.starts_with("pull_request"))
}

/// No workflow judges a branch. `pull_request` is the trigger whose red would be a second
/// verdict on the same commit the queue is rebasing - and the one that spends the rate limit.
#[test]
fn no_workflow_runs_on_a_pull_request() {
    for (name, text) in workflows() {
        assert!(
            !judges_a_pull_request(&text),
            "{name} runs on a pull request. Branches are gated by the merge queue, in a \
             landing worktree here or on `[queue] gate_host`'s machine; a workflow that \
             judges a push is a second gate nobody acts on, and it is the GitHub Actions \
             budget R62 exists to stop spending. See docs/gate.md."
        );
    }
}

/// And the trigger is found wherever YAML puts it. A guard that reads a line's prefix is a
/// guard a new file walks around: `on: [push, pull_request]` and `on: {pull_request: ~}` each
/// judge every PR and each pass a prefix check.
#[test]
fn a_pull_request_trigger_is_refused_in_every_spelling() {
    for text in [
        "on: pull_request\n",
        "on:\n  pull_request:\n",
        "on:\n  pull_request:\n    branches: [main]\n",
        "on: [push, pull_request]\n",
        "on: [push,\n  pull_request]\n",
        "on: {pull_request: ~}\n",
        "on: {push: ~, pull_request: ~}\n",
        "on:\n  - push\n  - pull_request\n",
        "on: pull_request_target\n",
        "on: pull_request_review\n",
    ] {
        assert!(
            judges_a_pull_request(text),
            "{text:?} judges a pull request, and a guard that reads one spelling lets it through"
        );
    }
    // And a release is still a release: a tag starts it, and no branch or PR does.
    for text in [
        "on:\n  push:\n    tags: ['v*']\n",
        "on: {push: {tags: ['v*']}}\n",
        "on: workflow_dispatch\n",
        "on: push\n",
        "on: [push, workflow_dispatch]\n",
    ] {
        assert!(
            !judges_a_pull_request(text),
            "{text:?} is not a pull request trigger, and refusing it would hold a release"
        );
    }
}

/// No workflow runs the gate's own steps. The `## Merge gate` section of AGENTS.md is the one
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

/// And the repository still has a gate to run, because the queue holds every branch of a
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

/// What `scripts/gate-steps.sh` prints for `md`, or `None` when it says there is no gate.
fn gate_steps_sh(md: &str) -> Option<Vec<String>> {
    use std::io::Write;
    let mut child = std::process::Command::new("sh")
        .arg(root().join("scripts/gate-steps.sh"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("sh runs");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(md.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    })
}

/// `gate-at.sh` and `rrun --gate` read AGENTS.md through `gate-steps.sh`; it must read the same
/// steps the queue's `parse_merge_gate` does, on this repository's file and on the edge cases.
#[test]
fn gate_steps_sh_reads_what_the_queue_reads() {
    let agents = std::fs::read_to_string(root().join("AGENTS.md")).expect("AGENTS.md");
    let cases = [
        agents.as_str(),
        "# Project\n\n```sh\nmake test\n```\n",
        "## Merge gate\n\n```sh\n# nothing\n```\n",
        "## Merge gate\n\nTBD\n\n## Other\n\n```sh\nmake test\n```\n",
        "### merge GATE\n~~~\ngo test ./...\n~~~\n",
        "## Merge gate ##\n\n  ```sh\n  a\n\n  # c\n  b  \n```\nnot\n```sh\nc\n```\n",
        "# Top\n## Merge gate\n### Sub\n```\nx\n```\n",
        "## Merge gate\n```sh\nunclosed\n",
        "#Merge gate\n```\nx\n```\n",
        "## Merge gate\r\n```sh\r\nwin\r\n```\r\n",
    ];
    for md in cases {
        assert_eq!(gate_steps_sh(md), parse_merge_gate(md), "for {md:?}");
    }
}

/// No script carries its own list of the gate's steps: two of the core steps in one file is a
/// copy that can drift from AGENTS.md, as `rrun --gate`'s did.
#[test]
fn no_script_holds_a_copy_of_the_gate() {
    const CORE: &[&str] = &[
        "check-test-attrs.sh",
        "clippy-gate.sh",
        "cargo test --workspace",
    ];
    for entry in std::fs::read_dir(root().join("scripts")).unwrap().flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let held: Vec<&&str> = CORE.iter().filter(|s| text.contains(**s)).collect();
        assert!(
            held.len() < 2,
            "scripts/{} holds {held:?} of the merge gate; read AGENTS.md through \
             scripts/gate-steps.sh instead",
            entry.file_name().to_string_lossy()
        );
    }
}

/// A fixture that turns the demand off leaves every skip in its binary green, and `set_var` with a
/// zero does that as surely as `remove_var`. The guard reads both, in every spelling a fixture uses.
#[test]
fn a_fixture_that_switches_the_apparatus_demand_off_is_refused() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-attrs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a directory");
    let script = root().join("scripts/check-test-attrs.sh");
    for body in [
        "    std::env::remove_var(\"LETIBOT_REQUIRE_APPARATUS\");\n",
        "    std::env::set_var(\"LETIBOT_REQUIRE_APPARATUS\", \"0\");\n",
        "    std::env::set_var(\"LETIBOT_REQUIRE_MODEL\", \"0\");\n",
        "    unsafe { std::env::set_var(\"LETIBOT_REQUIRE_APPARATUS\", \"0\") };\n",
        "    std::env::set_var ( \"LETIBOT_REQUIRE_APPARATUS\" , \"0\" ) ;\n",
    ] {
        let file = dir.join("case.rs");
        std::fs::write(&file, format!("#[test]\nfn a_case() {{\n{body}}}\n")).expect("the file");
        let out = std::process::Command::new("sh")
            .arg(&script)
            .arg(&file)
            .output()
            .expect("the check runs");
        assert!(
            !out.status.success(),
            "a fixture was allowed to take the demand off:\n{body}"
        );
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            said.contains("LETIBOT_REQUIRE"),
            "the refusal does not name the demand: {said}"
        );
    }
    // And the ordinary case still passes, so the guard is not one that refuses everything.
    let clean = dir.join("clean.rs");
    std::fs::write(&clean, "#[test]\nfn a_case() {}\n").expect("the file");
    let out = std::process::Command::new("sh")
        .arg(&script)
        .arg(&clean)
        .output()
        .expect("the check runs");
    assert!(out.status.success(), "a clean file was refused");
    let _ = std::fs::remove_dir_all(&dir);
}
