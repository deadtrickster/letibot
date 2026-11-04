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
/// narrowed to tags. This repository's gate is off GitHub Actions, and what is left there is
/// the release, which only a tag starts and which is nobody's merge gate. Pure.
pub fn judges_a_branch(text: &str) -> bool {
    triggers(text).iter().any(|(event, tags_only)| {
        event.starts_with("pull_request") || (event == "push" && !tags_only)
    })
}

/// What a workflow's `on:` answers, one entry per event with whether it is narrowed to tags. Every
/// spelling YAML allows is read - a scalar, a flow sequence, a flow mapping, a block mapping or a
/// block sequence - because a guard that knows one spelling is a guard a new file walks around.
pub fn triggers(text: &str) -> Vec<(String, bool)> {
    let lines: Vec<&str> = text.lines().map(uncommented).collect();
    let Some((at, rest)) = lines
        .iter()
        .enumerate()
        .find_map(|(i, line)| top_level_key(line, "on").map(|rest| (i, rest)))
    else {
        return Vec::new();
    };
    if rest.is_empty() {
        return block_events(&lines[at + 1..]);
    }
    if rest.starts_with('[') || rest.starts_with('{') {
        let flow = balanced(rest, &lines[at + 1..]);
        return flow_events(&flow);
    }
    vec![(scalar(rest).to_string(), false)]
}

/// A line with a trailing comment removed, and `#` inside quotes left alone: a workflow may name a
/// trigger in a note, and may have `#` in a branch name.
fn uncommented(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    for (at, c) in line.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, '#') if prev.is_whitespace() => return &line[..at],
            _ => {}
        }
        prev = c;
    }
    line
}

/// The value after a top-level `<key>:`, when this line is that key. A nested `on:` is a value of
/// something else and is not the workflow's trigger.
fn top_level_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    if line.len() != line.trim_start().len() {
        return None;
    }
    let rest = line.trim_start().strip_prefix(key)?.trim_start();
    Some(rest.strip_prefix(':')?.trim())
}

/// A flow collection that a line opened and later lines finish.
fn balanced(first: &str, rest: &[&str]) -> String {
    let mut text = first.to_string();
    let mut depth = depth_of(&text);
    for line in rest {
        if depth <= 0 {
            break;
        }
        text.push(' ');
        text.push_str(line.trim());
        depth = depth_of(&text);
    }
    text
}

/// How many `[`/`{` a text has opened and not closed, outside quotes.
fn depth_of(text: &str) -> i32 {
    let mut quote: Option<char> = None;
    let mut depth = 0;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, '[' | '{') => depth += 1,
            (None, ']' | '}') => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// The items of a flow collection, split on the commas that are not inside a nested one.
fn flow_items(inner: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut quote: Option<char> = None;
    let mut depth = 0;
    let mut from = 0;
    for (at, c) in inner.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, '[' | '{') => depth += 1,
            (None, ']' | '}') => depth -= 1,
            (None, ',') if depth == 0 => {
                items.push(inner[from..at].trim());
                from = at + c.len_utf8();
            }
            _ => {}
        }
    }
    items.push(inner[from..].trim());
    items.retain(|i| !i.is_empty());
    items
}

/// The events of `on: [...]` or `on: {...}`.
fn flow_events(text: &str) -> Vec<(String, bool)> {
    let text = text.trim();
    let (inner, mapping) = match (text.strip_prefix('['), text.strip_prefix('{')) {
        (Some(inner), _) => (inner, false),
        (None, Some(inner)) => (inner, true),
        (None, None) => return Vec::new(),
    };
    let inner = inner
        .strip_suffix(']')
        .or_else(|| inner.strip_suffix('}'))
        .unwrap_or(inner);
    let mut out = Vec::new();
    for item in flow_items(inner) {
        match (mapping, item.split_once(':')) {
            (true, Some((key, value))) => out.push((scalar(key).to_string(), tags_only(value))),
            _ => out.push((scalar(item).to_string(), false)),
        }
    }
    out
}

/// The events of a block collection under `on:`, one entry per line at the collection's own indent.
fn block_events(rest: &[&str]) -> Vec<(String, bool)> {
    let Some(base) = rest
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| indent(l))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let line = rest[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        let at = indent(line);
        if at < base {
            break;
        }
        if at > base {
            i += 1;
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(item) = trimmed.strip_prefix("- ") {
            out.push((scalar(item).to_string(), false));
            i += 1;
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            i += 1;
            continue;
        };
        let mut nested = Vec::new();
        let mut j = i + 1;
        while j < rest.len() && (rest[j].trim().is_empty() || indent(rest[j]) > at) {
            nested.push(rest[j]);
            j += 1;
        }
        let only_tags = if value.trim().is_empty() {
            let has = |key: &str| nested.iter().any(|l| l.trim_start().starts_with(key));
            has("tags:") || has("tags-ignore:")
        } else {
            tags_only(value)
        };
        // `branches:` beside `tags:` is the shape that runs on both, and it is not a release.
        let branches = nested.iter().any(|l| {
            l.trim_start().starts_with("branches:")
                || l.trim_start().starts_with("branches-ignore:")
        });
        out.push((scalar(key).to_string(), only_tags && !branches));
        i = j;
    }
    out
}

/// Whether a push's own lines narrow it to tags. `branches:` beside `tags:` says it runs on both.
fn tags_only(value: &str) -> bool {
    let tags = value.contains("tags");
    tags && !value.contains("branches")
}

/// A YAML scalar with its quotes taken off.
fn scalar(text: &str) -> &str {
    text.trim().trim_matches(|c| c == '\'' || c == '"').trim()
}

/// How far a line is indented, in bytes: every level of this file's mappings is spaces.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
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
        // this repository's gate runs in the queue rather than on GitHub.
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

    /// The AGENTS.md bootstrap is a named exception to the queue rule, and the
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

    /// A workflow that judges a branch is a candidate gate, and a tag's release is not.
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
        // A push that runs on a branch AND on tags is a branch judgement: `branches:` is the key
        // that says so, and the one-file shape everybody writes must not be read as a release.
        assert!(judges_a_branch(
            "on:\n  push:\n    branches: [main]\n    tags: ['v*']\n"
        ));
        assert!(!judges_a_branch("on:\n  push:\n    tags-ignore: ['v*']\n"));
        // And the trigger is the same trigger wherever YAML puts it.
        assert!(judges_a_branch("on: push\n"));
        assert!(judges_a_branch("on: push # every branch\n"));
        assert!(judges_a_branch("on: [push, pull_request]\n"));
        assert!(judges_a_branch("on:\n  - push\n  - pull_request\n"));
        assert!(judges_a_branch("on: {pull_request: ~}\n"));
        assert!(judges_a_branch(
            "on: {push: {branches: [main], tags: ['v*']}}\n"
        ));
        assert!(!judges_a_branch("on: {push: {tags: ['v*']}}\n"));
        assert!(!judges_a_branch("on: workflow_dispatch\n"));
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
