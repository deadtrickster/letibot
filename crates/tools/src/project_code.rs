//! **Does this command run code the agent wrote?** — R98's first half, which is the other
//! side of D16.
//!
//! D16 adjudicates `writes-allowed` this way: inside a confinement letibot has measured, a
//! per-command ruling protects nothing a freshly written *test* could not undo, so a command
//! there is admitted without a card. The same sentence read backwards is R98: **on a host with
//! no boundary, running the test file is running the agent's code.** A grant the operator
//! answered once cannot be read as consent to the next script the model writes — the file can
//! change between the answer and the run, and the grant is keyed on a program and a verb, not
//! on bytes nobody has seen yet. So a call that runs a test runner, a build that executes
//! project code, or a script in the checkout **asks every time**, whatever grant or durable
//! rule is in force, and the card says why.
//!
//! # What this is not
//!
//! It is not a classifier of danger and it changes no tier. A refusal, a boundary and an
//! always-ask entry all still happen before it is consulted — this only decides whether a
//! *grant* may settle a call, and it can therefore only make the gate ask more.
//!
//! # Why the list is programs and sub-verbs, not a regex
//!
//! `cargo test` and `cargo tree` are the same program and nothing alike: one runs the project's
//! own test binaries, the other prints a dependency graph. So each row names a program **and**
//! the first operand that makes it a runner, read from the same parse
//! ([`letibot_code::shell`]) layer A reads, so this cannot disagree with the gate about what
//! the command is.

use crate::intent::Baseline;
use letibot_code::shell::Word;

/// **Why this call runs the project's own code**, in the words a card can print — or `None`.
///
/// The sentence names the thing it found (`cargo test`, `make`, `scripts/check.sh`) rather
/// than restating the rule, because the operator reads it while deciding whether to say yes:
/// *what am I being asked about* is the question, and *it runs the project's code* is the
/// reason the answer is not remembered.
pub fn runs_project_code(baseline: &Baseline) -> Option<String> {
    find(baseline).map(|f| f.what)
}

/// **The bucket the runner falls in**, for a report that has to be read at a glance —
/// `cargo test`, `cargo build`, `pytest`, `make`, `a script in the checkout`.
///
/// The sentence [`runs_project_code`] answers is what a CARD prints, so it names the file
/// (`bash scripts/check-fmt.sh`); a table of those is a table of one row per path. This is the
/// same traversal, the same judgement and the same `None` — only the label differs — so a
/// corpus report cannot drift from what the gate does.
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
            // A glob is a pattern, not a value, and it is not a sub-verb: `cargo t*` is not
            // something to guess at. The stage then reads as *not a runner*, which is the
            // narrower reading and leaves the call's tier and its boundary where they were.
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    // `sudo -u x cargo test` is `cargo test`, and the wrapper is where a runner hides:
    // layer A unwraps these for the same reason (see `intent::unwrap_wrapper`).
    let (program, skip) = match crate::intent::unwrap_wrapper(spelled, &stage.argv) {
        Some((inner, at)) => (inner, at + 1),
        None => (spelled.to_string(), 0),
    };
    let argv = argv.get(skip.min(argv.len())..).unwrap_or(&[]);
    let name = program.rsplit('/').next().unwrap_or(&program);
    // **A program that is a path *into the checkout* is somebody's code.** `./scripts/test.sh`,
    // `target/debug/deps/letibot_tools-abc`, `scripts/run.sh` — the tree is running something
    // of its own, which is the whole of R98's subject. A bare name is a program from `PATH`
    // and is judged by the table below instead.
    //
    // **And a system binary is not**: `/bin/sed -n 1p f` is the same act as `sed -n 1p f`, and
    // reading a slash as *this project's* would narrow the ladder over half the commands in the
    // corpus. The line is the system roots — the directories `ViewSpec::SYSTEM_ROOTS` binds
    // read-only inside a boundary — and what it leaves out is a toolchain installed under
    // `~/.local/bin` (`mise`'s shims), which reads as project code and therefore asks. That is
    // the narrow direction, and it is stated here rather than left to be discovered.
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
        // A program that is its own verb (`pytest`) names itself and nothing else, on the card
        // and in the bucket alike: `cargo test` and `cargo build` are two rows of a report.
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
    // **A script handed to an interpreter** — `bash scripts/check.sh`, `python3 tool.py`.
    // The path comes from the same reader the gate uses to decide what to put in a brief
    // (`intent::script_argument`), so a `-c` body is not mistaken for a file.
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

/// **The sub-verb that makes this program run the project's code**, when one does.
///
/// The rows are the ruling's (`cargo test|run|build`, `pytest`, `go test`, `npm test`, `make`)
/// plus the rest of the cargo family that compiles and *runs* project code: a build script
/// (`build.rs`) runs on `cargo check` and `cargo clippy` exactly as it does on `cargo build`,
/// so excluding them would leave the ruling's own argument open one flag over. Everything
/// here only ever makes the gate ask more.
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
            // `cargo nextest run` — the verb is `nextest` and the runner is one word over.
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
        // project's own `package.json` names — the agent can write that file.
        "npm" | "yarn" | "pnpm" | "bun" => match words.first().copied()? {
            "test" | "t" => Some("test".to_string()),
            "run" | "run-script" => Some("run".to_string()),
            _ => None,
        },
        // **`make` runs the Makefile**, which is a file in the checkout that the agent can
        // rewrite: the ruling's own example of a build that is really an interpreter.
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

/// **Is this argument a path in the checkout rather than something else?**
///
/// A relative path is one: a session's commands run from its workspace, so `scripts/x.sh` is
/// in the tree it stands in. An absolute path is one only when it is not looking somewhere
/// else — and *somewhere else* is exactly what this module must not have to model, so the
/// answer is the narrow one (a path naming `/` is somebody's, not this project's, and the
/// caller's region test has already decided that one). A URL or a bare word is not a path.
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

    /// **The ruling's own list**, each named by what it runs.
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

    /// **A build script runs on `check` and `clippy` too**, which is why they are rows: the
    /// ruling says *a build that executes project code*, and `build.rs` is project code.
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

    /// **Everything that is not one.** A test that only asks *does the runner list fire* would
    /// pass on a predicate that answered `Some` for every command — which would make the gate
    /// ask on every build and read, and be found only by the operator.
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

    /// **A runner hiding behind a wrapper is still a runner** — `sudo`/`env`/`timeout` are
    /// where a command goes to look like something else, and layer A unwraps them for the
    /// same reason.
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

    /// **A program that is a path is somebody's code**, whatever the name looks like — and a
    /// program under a system root is not, which is what keeps the ladder over `/bin/sed`
    /// exactly as it was.
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

    /// **A stage anywhere in the line counts**, not only the first: `cd x && cargo test` is a
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

    /// **A call with no shell command at all** — every file tool — is not this question.
    #[test]
    fn a_non_command_call_is_not_a_runner() {
        let b = Baseline::of_paths(["a/b.txt"], true, false, &surroundings());
        assert_eq!(runs_project_code(&b), None);
    }
}
