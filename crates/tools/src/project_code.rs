//! Does this command run code the agent wrote?
//!
//! On a host with no boundary, running the project's test file is running the agent's code, and a grant
//! answered once is not consent to the next script, since the file can change between the answer and the
//! run. So a runner, a build that executes project code, or a script in the checkout asks every time.
//!
//! It changes no tier, so this can only make the gate ask more, and the rows are programs and sub-verbs
//! rather than a regex, because `cargo test` and `cargo tree` are nothing alike.

use crate::intent::Baseline;
use letibot_code::shell::Word;

/// Why this call runs the project's own code, in the words a card can print, or `None`. The sentence
/// names the thing it found rather than restating the rule, because the operator reads it while deciding.
pub fn runs_project_code(baseline: &Baseline) -> Option<String> {
    find(baseline).map(|f| f.what)
}

/// The bucket the runner falls in, for a report read at a glance. The card's sentence names the file and
/// a table of those is one row per path; this is the same traversal and the same judgement, so a report
/// cannot drift from what the gate does.
pub fn runs_project_code_kind(baseline: &Baseline) -> Option<String> {
    find(baseline).map(|f| f.kind)
}

/// What one stage was found to run: the sentence a card prints, and the bucket a report
/// counts.
struct Found {
    what: String,
    kind: String,
}

fn find(baseline: &Baseline) -> Option<Found> {
    let command = baseline.command.as_ref()?;
    command.stages.iter().find_map(stage_runs_project_code)
}

/// One simple command, read the way the gate reads it: unwrap the wrapper, then ask what the
/// real program is.
fn stage_runs_project_code(stage: &letibot_code::shell::Stage) -> Option<Found> {
    let spelled = stage.program.literal()?;
    let argv: Vec<String> = stage
        .argv
        .iter()
        .map(|w| match w {
            Word::Literal(s) => Some(s.clone()),
            // A glob is a pattern, not a value, and it is not a sub-verb: `cargo t*` is
            // not something to guess at. The stage then reads as not a runner, which is
            // the narrower reading and leaves the call's tier and boundary where they were.
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    // `sudo -u x cargo test` is `cargo test`, and the wrapper is where a runner hides.
    let (program, skip) = match crate::intent::unwrap_wrapper(spelled, &stage.argv) {
        Some((inner, at)) => (inner, at + 1),
        None => (spelled.to_string(), 0),
    };
    let argv = argv.get(skip.min(argv.len())..).unwrap_or(&[]);
    let name = program.rsplit('/').next().unwrap_or(&program);
    // A program that is a path into the checkout is somebody's code. A bare name comes from `PATH` and
    // is judged by the table below instead. A system binary is not: reading a slash as this project's
    // would narrow the ladder over half the corpus, so the line is the system roots.
    let system = program.starts_with('/')
        && [
            "/bin/", "/sbin/", "/usr/", "/lib", "/opt/", "/snap/", "/nix/", "/etc/",
        ]
        .iter()
        .any(|root| program.starts_with(root));
    if program.contains('/') && !system {
        return Some(Found {
            what: format!("`{program}` — a program in this checkout"),
            kind: "a program in the checkout".into(),
        });
    }
    if let Some(verb) = subcommand_that_runs_code(name, argv) {
        // A program that is its own verb (`pytest`) names itself and nothing else, on the
        // card and in the bucket alike: `cargo test` and `cargo build` are two report rows.
        return Some(match verb.is_empty() {
            true => Found {
                what: format!("`{name}`"),
                kind: name.to_string(),
            },
            false => Found {
                what: format!("`{name} {verb}`"),
                kind: format!("{name} {verb}"),
            },
        });
    }
    // A script handed to an interpreter - `bash scripts/check.sh`, `python3 tool.py`. The
    // path comes from the same reader the gate uses for a brief (`intent::script_argument`),
    // so a `-c` body is not mistaken for a file.
    if let Some(path) = crate::intent::script_argument(name, argv)
        && is_relative_or_inside(&path)
    {
        return Some(Found {
            what: format!("`{name} {path}` — a script in this checkout"),
            kind: "a script in the checkout".into(),
        });
    }
    None
}

/// The sub-verb that makes this program run the project's code, when one does. A build script runs on
/// `cargo check` and `cargo clippy` exactly as on `cargo build`, so excluding them would leave the
/// argument open one flag over.
fn subcommand_that_runs_code(program: &str, argv: &[String]) -> Option<String> {
    let words: Vec<&str> = argv
        .iter()
        .map(String::as_str)
        .filter(|w| !w.starts_with('-'))
        .collect();
    match program {
        "cargo" => {
            let verb = words.first().copied()?;
            // `+nightly` and `-Zflag` are consumed above; a toolchain override is a flag.
            let runs = [
                "test", "run", "build", "bench", "check", "clippy", "nextest", "miri", "doc-test",
            ];
            // `cargo nextest run` - the verb is `nextest` and the runner is one word over.
            if verb == "nextest" {
                return Some("nextest run".to_string());
            }
            runs.contains(&verb).then(|| verb.to_string())
        }
        // `pytest` is a runner whatever it is handed; `python -m pytest` is the same act
        // and is caught by the `-m` arm below.
        "pytest" | "py.test" | "tox" | "nox" => Some(String::new()),
        "go" => match words.first().copied()? {
            "test" => Some("test".to_string()),
            _ => None,
        },
        // `npm test`, `npm run <script>` and `yarn`/`pnpm`'s equivalents run a script the
        // project's own `package.json` names - the agent can write that file.
        "npm" | "yarn" | "pnpm" | "bun" => match words.first().copied()? {
            "test" | "t" => Some("test".to_string()),
            "run" | "run-script" => Some("run".to_string()),
            _ => None,
        },
        // `make` runs the Makefile, a file in the checkout the agent can rewrite: a build
        // that is really an interpreter.
        "make" | "gmake" | "just" | "task" => Some(
            words
                .first()
                .copied()
                .unwrap_or("the default target")
                .to_string(),
        ),
        "python" | "python2" | "python3" => {
            // `python3 -m pytest` (and `-m unittest`, `-m tox`): a runner spelled as a module.
            let at = argv.iter().position(|w| w == "-m")?;
            let module = argv.get(at + 1)?;
            matches!(
                module.as_str(),
                "pytest" | "unittest" | "tox" | "nose" | "nose2" | "doctest"
            )
            .then(|| format!("-m {module}"))
        }
        _ => None,
    }
}

/// Is this argument a path in the checkout rather than something else? A relative path is one, since
/// commands run from the session's workspace; an absolute path is not, because somewhere else is what
/// this module must not have to model.
fn is_relative_or_inside(arg: &str) -> bool {
    if arg.starts_with('/') {
        return false;
    }
    if arg.starts_with('-') || arg.is_empty() {
        return false;
    }
    // `host:path` and `scheme://` are not files.
    !arg.contains("://") && !arg.contains(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline_of(cmd: &str) -> Baseline {
        Baseline::of_command_with(cmd, &surroundings(), &[])
    }

    fn surroundings() -> crate::intent::Surroundings {
        crate::intent::Surroundings {
            home: Some("/home/dead".into()),
            workspace: Some("/w".into()),
            scratch: None,
            shell: crate::intent::ShellTrust::Pinned {
                how: "test fixture".into(),
            },
            seen_hosts: Default::default(),
            ..Default::default()
        }
    }

    /// Each of these is a runner, named by what it runs.
    #[test]
    fn the_ruling_s_own_examples_are_runners() {
        for (cmd, want) in [
            ("cargo test --offline -p letibot-tools", "`cargo test`"),
            ("cargo run --release", "`cargo run`"),
            ("cargo build", "`cargo build`"),
            ("pytest -x", "`pytest`"),
            ("python3 -m pytest tests/", "`python3 -m pytest`"),
            ("go test ./...", "`go test`"),
            ("npm test", "`npm test`"),
            ("make", "`make the default target`"),
            (
                "bash scripts/check-fmt.sh main",
                "`bash scripts/check-fmt.sh` — a script in this checkout",
            ),
        ] {
            let got = runs_project_code(&baseline_of(cmd));
            assert_eq!(got.as_deref(), Some(want), "{cmd}");
        }
    }

    /// A build script runs on `check` and `clippy` too: the rule is a build that
    /// executes project code, and `build.rs` is project code.
    #[test]
    fn a_cargo_verb_that_compiles_is_a_runner() {
        for cmd in [
            "cargo check -p x",
            "cargo clippy --all-targets",
            "cargo bench",
            "cargo nextest run",
        ] {
            assert!(
                runs_project_code(&baseline_of(cmd)).is_some(),
                "{cmd} compiles the project and runs its build scripts"
            );
        }
    }

    /// Everything that is not one. A test that only asked does the runner list fire
    /// would pass on a predicate answering `Some` for every command - which would make
    /// the gate ask on every build and read, and be found only by the operator.
    #[test]
    fn what_is_not_a_runner() {
        for cmd in [
            "cargo tree",
            "cargo fmt --check",
            "ls -la",
            "git status",
            "grep -rn todo crates/",
            "python3 -c 'print(1)'",
            "go build ./...",
            "npm ci",
            "cargo test --help", // still `cargo test`: the flag does not unmake the verb
        ] {
            let got = runs_project_code(&baseline_of(cmd));
            if cmd == "cargo test --help" {
                assert_eq!(got.as_deref(), Some("`cargo test`"), "{cmd}");
            } else {
                assert_eq!(got, None, "{cmd}");
            }
        }
    }

    /// A runner hiding behind a wrapper is still a runner - `sudo`/`env`/`timeout` are
    /// where a command goes to look like something else.
    #[test]
    fn a_wrapper_does_not_hide_the_runner() {
        for cmd in [
            "env CARGO_TARGET_DIR=/tmp/t cargo test",
            "timeout 600 cargo test -p x",
            "sudo -u dead make",
        ] {
            assert!(runs_project_code(&baseline_of(cmd)).is_some(), "{cmd}");
        }
    }

    /// A program that is a path is somebody's code, whatever the name looks like; a
    /// program under a system root is not, which keeps the ladder over `/bin/sed` as it was.
    #[test]
    fn a_program_in_the_checkout_is_project_code() {
        for cmd in [
            "./scripts/test.sh",
            "scripts/run.sh --all",
            "target/debug/deps/letibot_tools-3ce46a0e --exact",
        ] {
            let got = runs_project_code(&baseline_of(cmd));
            assert!(
                got.as_deref().is_some_and(|w| w.contains("this checkout")),
                "{cmd}: {got:?}"
            );
        }
        // The false positive this rule would otherwise be: `/bin/sed` is not the project's.
        assert_eq!(
            runs_project_code(&baseline_of("/bin/sed -n 1p /w/a.rs")),
            None
        );
    }

    /// A stage anywhere in the line counts, not only the first: `cd x && cargo test` is a
    /// runner, and that is how every command in this repository starts.
    #[test]
    fn a_later_stage_counts_too() {
        assert_eq!(
            runs_project_code(&baseline_of("cd /w && cargo test --offline")).as_deref(),
            Some("`cargo test`")
        );
        assert_eq!(
            runs_project_code(&baseline_of("cargo build 2>&1 | tee /tmp/x.log")).as_deref(),
            Some("`cargo build`")
        );
    }

    /// A call with no shell command at all - every file tool - is not this question.
    #[test]
    fn a_non_command_call_is_not_a_runner() {
        let b = Baseline::of_paths(["a/b.txt"], true, false, &surroundings());
        assert_eq!(runs_project_code(&b), None);
    }
}
