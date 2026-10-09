//! `merge_gate` — **what a repository's merge gate is, and what it could be.**
//!
//! The operator, 2026-10-09: *"make the gate configurable per repo, and let main project agent
//! manage it … it is not hard to guess project type and ask for confirmation and user can provide
//! a command (make test if exists can be used, or even act if .github present) - all heuristics
//! can be presented as choices"*, and *"then it becomes a section in agents.md"*.
//!
//! So this is the agent's half: it reads the repository (the session's root, or a directory
//! under it — an agent may run in the repo or a level above it), reports the gate its `AGENTS.md`
//! already has, and lists what the project's own files suggest, each with the reason. It changes
//! nothing. The operator picks one or types their own; the agent writes the section
//! ([`crate::gatekeeper::merge_gate_section`]) with `edit`/`write`, which is where the operator's
//! approval is, and commits it to `main` — the queue reads `main`'s copy.

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

/// **The candidates this repository's own files suggest**, most specific first: a project's
/// own entry points (`make`, `just`, `task`) before a toolchain's defaults, and the CI the
/// repository already runs (`act`) last, because it needs Docker.
///
/// `read` answers a path relative to the repository; `workflows` are the files under
/// `.github/workflows`. Pure, so every heuristic is asserted without a filesystem.
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
                "the repository's own CI, run locally with `act` (needs Docker): {}",
                workflows.join(", ")
            ),
        ));
    }
    out
}

impl Tool for MergeGate {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "merge_gate",
            "The merge queue's gate for a repository: the commands it runs on a branch before \
             landing it. Reports the gate the repository's AGENTS.md has (a `Merge gate` \
             section) and the choices its files suggest, each with why. Changes nothing: put \
             the choices to the operator, who may also type their own command; then write the \
             section into AGENTS.md and commit it to main, which is the copy the queue reads.",
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
        let backend = &*ctx.backend;
        let read = |f: &str| -> Option<String> {
            backend
                .read(&at(f))
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
        };
        let (wf, _) = walk(ctx.backend, &at(".github/workflows"), 64, &default_skip);
        let workflows: Vec<String> = wf
            .iter()
            .filter(|e| !e.is_dir && (e.name.ends_with(".yml") || e.name.ends_with(".yaml")))
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
            "Put these to the operator as choices and let them pick or type their own; do not \
             choose for them. Then add this section to the repository's AGENTS.md (creating the \
             file if there is none), with the chosen commands one per line, and commit it to \
             main:\n\n",
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
