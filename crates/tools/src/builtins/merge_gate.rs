//! `merge_gate`: what a repository's merge gate is, and what it could be. It reports the section
//! `AGENTS.md` already declares and lists what the project's own files suggest, each with a
//! reason, and writes nothing: the operator picks, the agent writes it ([`merge_gate_section`]).

use serde_json::Value;

use crate::backend::{default_skip, walk};
use crate::gatekeeper::{merge_gate_section, parse_merge_gate};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

pub struct MergeGate;

/// One thing the gate could be, and why this repository suggests it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub steps: Vec<String>,
    pub why: String,
}

fn cand(steps: &[&str], why: impl Into<String>) -> Candidate {
    Candidate {
        steps: steps.iter().map(|s| s.to_string()).collect(),
        why: why.into(),
    }
}

/// A line that starts a target or recipe called `name` in a Makefile / justfile.
fn has_target(text: &str, name: &str) -> bool {
    text.lines().any(|l| {
        l.strip_prefix(name).is_some_and(|rest| {
            rest.starts_with(':') || (rest.starts_with(' ') && rest.contains(':'))
        })
    })
}

/// The candidates this repository's own files suggest, most specific first: `make`/`just`/`task`
/// before a toolchain's defaults, and a workflow that judges a branch last, run with `act`, because
/// that needs Docker and an image prompt. Pure.
pub fn candidates(read: &dyn Fn(&str) -> Option<String>, workflows: &[String]) -> Vec<Candidate> {
    let mut out = Vec::new();
    if let Some(mk) = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .find_map(|f| read(f))
    {
        for target in ["check", "test"] {
            if has_target(&mk, target) {
                out.push(cand(
                    &[&format!("make {target}")],
                    format!("the Makefile has a `{target}` target"),
                ));
            }
        }
    }
    if let Some(just) = ["justfile", "Justfile", ".justfile"]
        .iter()
        .find_map(|f| read(f))
        && has_target(&just, "test")
    {
        out.push(cand(&["just test"], "the justfile has a `test` recipe"));
    }
    if let Some(task) = ["Taskfile.yml", "Taskfile.yaml"]
        .iter()
        .find_map(|f| read(f))
        && task
            .lines()
            .any(|l| l.trim_start() == "test:" && l.starts_with("  "))
    {
        out.push(cand(&["task test"], "the Taskfile has a `test` task"));
    }
    if read("Cargo.toml").is_some() {
        out.push(cand(
            &[
                "cargo fmt --all -- --check",
                "cargo clippy --all-targets -- -D warnings",
                "cargo test --workspace",
            ],
            "a Rust workspace (Cargo.toml): format, lints and tests",
        ));
        out.push(cand(
            &["cargo test --workspace"],
            "a Rust workspace, tests only",
        ));
    }
    if read("go.mod").is_some() {
        out.push(cand(
            &["test -z \"$(gofmt -l .)\"", "go vet ./...", "go test ./..."],
            "a Go module (go.mod): gofmt, vet and tests",
        ));
    }
    if let Some(pkg) = read("package.json") {
        let json: Value = serde_json::from_str(&pkg).unwrap_or(Value::Null);
        let script = |name: &str| {
            json.get("scripts")
                .and_then(|s| s.get(name))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let runner = if read("pnpm-lock.yaml").is_some() {
            "pnpm"
        } else if read("yarn.lock").is_some() {
            "yarn"
        } else {
            "npm"
        };
        let test = script("test").filter(|t| !t.contains("no test specified"));
        if test.is_some() {
            if script("lint").is_some() {
                out.push(cand(
                    &[&format!("{runner} run lint"), &format!("{runner} test")],
                    "package.json has `lint` and `test` scripts",
                ));
            }
            out.push(cand(
                &[&format!("{runner} test")],
                "package.json has a `test` script",
            ));
        }
    }
    if read("tox.ini").is_some() {
        out.push(cand(&["tox"], "a tox.ini"));
    } else if read("pyproject.toml").is_some()
        || read("pytest.ini").is_some()
        || read("setup.cfg").is_some()
    {
        let ruff = read("pyproject.toml").is_some_and(|p| p.contains("[tool.ruff"));
        if ruff {
            out.push(cand(
                &["ruff check .", "pytest"],
                "a Python project with ruff configured",
            ));
        }
        out.push(cand(
            &["pytest"],
            "a Python project (pyproject/pytest.ini/setup.cfg)",
        ));
    }
    if !workflows.is_empty() {
        out.push(cand(
            &["act"],
            format!(
                "the repository's own workflow, which judges a branch, run locally with `act` \
                 (needs Docker): {}",
                workflows.join(", ")
            ),
        ));
    }
    out
}

/// Whether a workflow file judges a BRANCH - a `pull_request` trigger, or a `push` that is not
/// narrowed to tags. R62 took this repository's gate off GitHub Actions, and what is left there is
/// the release, which only a tag starts and which is nobody's merge gate. Pure.
pub fn judges_a_branch(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    if lines
        .iter()
        .any(|l| l.trim_start().starts_with("pull_request"))
    {
        return true;
    }
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start() != "push:" {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let mut tags_only = false;
        for next in &lines[i + 1..] {
            let deeper = next.len() - next.trim_start().len() > indent;
            if next.trim().is_empty() || !deeper {
                break;
            }
            if next.trim_start().starts_with("tags:") {
                tags_only = true;
            }
        }
        if !tags_only {
            return true;
        }
    }
    false
}

impl Tool for MergeGate {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "merge_gate",
            "The merge queue's gate for a repository: the commands it runs on a branch before \
             landing it. Reports the gate the repository's AGENTS.md has (a `Merge gate` \
             section) and the choices its files suggest, each with why. Changes nothing. \
             Exception to the queue rule: ask the operator with `ask_user_question` (options = \
             these choices; they may type their own command), then add the section to \
             AGENTS.md and commit it to main yourself — the queue reads main's copy and cannot \
             gate the change that gives it a gate.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "repo": {"type": "string", "description": "The repository's directory, when it is not the session root."}
                }
            }),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let repo = args
            .get("repo")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .trim_end_matches('/')
            .to_string();
        let at = |f: &str| {
            if repo == "." {
                f.to_string()
            } else {
                format!("{repo}/{f}")
            }
        };
        let backend = ctx.backend;
        let read = |f: &str| -> Option<String> {
            backend
                .read(&at(f))
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
        };
        let (wf, _) = walk(ctx.backend, &at(".github/workflows"), 64, &default_skip);
        // Only a workflow that judges a BRANCH is a candidate gate: a tag's release is not one, and
        // this repository's gate runs in the queue rather than on GitHub (R62).
        let workflows: Vec<String> = wf
            .iter()
            .filter(|e| !e.is_dir && (e.name.ends_with(".yml") || e.name.ends_with(".yaml")))
            .filter(|e| {
                read(&at(&format!(".github/workflows/{}", e.name)))
                    .is_some_and(|t| judges_a_branch(&t))
            })
            .map(|e| e.name.clone())
            .collect();
        let agents = ["AGENTS.md", "agents.md", "Agents.md"]
            .iter()
            .find_map(|f| read(f).map(|t| (f.to_string(), t)));
        let current = agents.as_ref().and_then(|(_, t)| parse_merge_gate(t));
        let found = candidates(&read, &workflows);

        let mut body = format!("merge gate for `{repo}`\n\n");
        match (&agents, &current) {
            (Some((f, _)), Some(steps)) => body.push_str(&format!(
                "now: {f} has a `Merge gate` section in this working tree —\n{}\n\
                 (the queue reads main's copy; if this is not committed, main may differ)\n\n",
                steps.iter().map(|s| format!("  {s}")).collect::<Vec<_>>().join("\n")
            )),
            (Some((f, _)), None) => {
                body.push_str(&format!("now: {f} has no `Merge gate` section, so the queue holds every branch of this repository until it has one\n\n"))
            }
            (None, _) => body.push_str("now: there is no AGENTS.md, so the queue holds every branch of this repository until it has one with a `Merge gate` section\n\n"),
        }
        if found.is_empty() {
            body.push_str("choices: nothing in this repository suggests a command — ask the operator for one.\n\n");
        } else {
            body.push_str("choices:\n");
            for (i, c) in found.iter().enumerate() {
                body.push_str(&format!(
                    "  {}. {} — {}\n",
                    i + 1,
                    c.steps.join(" && "),
                    c.why
                ));
            }
            body.push_str(&format!(
                "  {}. a command the operator types\n\n",
                found.len() + 1
            ));
        }
        body.push_str(
            "Ask the operator with `ask_user_question` (options = these choices; they may \
             type their own); do not choose for them. Exception to the queue rule: once they \
             pick, add this section to the repository's AGENTS.md (creating the file if there \
             is none), with the chosen commands one per line, and commit it to main yourself — \
             the queue cannot gate the change that gives it a gate:\n\n",
        );
        body.push_str(&merge_gate_section(&[
            "<the chosen commands, one per line>".to_string(),
        ]));
        Invocation::ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn suggest(files: &[(&str, &str)], workflows: &[&str]) -> Vec<String> {
        let files: HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let wf: Vec<String> = workflows.iter().map(|s| s.to_string()).collect();
        candidates(&|f: &str| files.get(f).cloned(), &wf)
            .into_iter()
            .map(|c| c.steps.join(" && "))
            .collect()
    }

    /// **R93.F31: the AGENTS.md bootstrap is a named exception to the queue rule**, and the
    /// choice goes to the operator through `ask_user_question`.
    #[test]
    fn the_bootstrap_is_a_named_exception_to_the_queue_rule() {
        let d = MergeGate.schema().description;
        assert!(d.contains("Exception to the queue rule:"), "{d}");
        assert!(d.contains("`ask_user_question`"), "{d}");
    }

    #[test]
    fn a_projects_own_entry_points_come_first_and_ci_last() {
        let got = suggest(
            &[
                (
                    "Makefile",
                    "build:\n\tgo build\ntest: build\n\tgo test ./...\n",
                ),
                ("go.mod", "module x\n"),
            ],
            &["ci.yml"],
        );
        assert_eq!(got[0], "make test");
        assert!(
            got.iter()
                .any(|c| c.contains("go test ./...") && c.contains("gofmt")),
            "{got:?}"
        );
        assert_eq!(got.last().unwrap(), "act", "{got:?}");
    }

    /// **R62: a workflow that judges a branch is a candidate gate, and a tag's release is not.**
    /// `act` rehearses a workflow, so what it can rehearse is a branch - and this repository's own
    /// gate lives in the queue now, not in `.github/workflows`.
    #[test]
    fn only_a_workflow_that_judges_a_branch_is_a_candidate() {
        assert!(judges_a_branch("on:\n  pull_request:\n"));
        assert!(judges_a_branch("on:\n  push:\n"));
        assert!(judges_a_branch("on:\n  push:\n    branches: [main]\n"));
        // The release: a tag starts it, no branch does.
        assert!(!judges_a_branch("on:\n  push:\n    tags: ['v*']\n"));
        assert!(!judges_a_branch(
            "on:\n  workflow_dispatch:\n# pull_request: disabled\n"
        ));
    }

    #[test]
    fn each_toolchain_is_recognised_by_its_own_file() {
        assert!(
            suggest(&[("Cargo.toml", "")], &[]).contains(&"cargo test --workspace".to_string())
        );
        let node = suggest(
            &[
                (
                    "package.json",
                    r#"{"scripts":{"test":"vitest","lint":"eslint ."}}"#,
                ),
                ("pnpm-lock.yaml", ""),
            ],
            &[],
        );
        assert_eq!(node, vec!["pnpm run lint && pnpm test", "pnpm test"]);
        // npm's placeholder test script is no test.
        assert!(
            suggest(
                &[(
                    "package.json",
                    r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#
                )],
                &[]
            )
            .is_empty()
        );
        assert_eq!(suggest(&[("tox.ini", "")], &[]), vec!["tox"]);
        assert_eq!(
            suggest(&[("pyproject.toml", "[tool.ruff]\n")], &[]),
            vec!["ruff check . && pytest", "pytest"]
        );
        assert_eq!(
            suggest(&[("justfile", "test:\n  cargo test\n")], &[]),
            vec!["just test"]
        );
        assert!(suggest(&[("README.md", "")], &[]).is_empty());
    }
}
