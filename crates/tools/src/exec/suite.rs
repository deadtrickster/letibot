//! A whole-suite run, refused on a reviewer's seat: the gate runs the suite, and a reviewer that
//! runs it again is killed by its job limits and spends a review attempt. A run that names one
//! test is allowed, because a reviewer must be able to prove or disprove its own claim.

use letibot_code::shell::{self, Context, Stage, Word};

use crate::intent::unwrap_wrapper;

/// One refusal: the runner as written, and how to target one test with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub runner: String,
    pub instead: &'static str,
}

impl Refusal {
    pub fn why(&self) -> String {
        format!(
            "`{}` runs the whole suite, and the gate runs the suite; run the one test that \
             proves your point, by name",
            self.runner
        )
    }

    pub fn body(&self) -> String {
        format!(
            "{}.\n\nThe command was NOT run. This seat reviews a diff: the merge gate runs the \
             tests, builds and benchmarks, and a full run here is killed by this seat's memory \
             and time limits and costs the review an attempt. Read the diff, the gate's recorded \
             output when the ask carries it, and the failing test's source. To prove or disprove \
             a specific claim, run that one test: {}.\n\nA project whose reviewers should run \
             whole suites sets `[queue] reviewers_run_suites = true` in its `leticode.toml`.",
            self.why(),
            self.instead
        )
    }
}

/// The first stage of `command` that runs a whole suite, or `None`. A pure function of the text:
/// a word the parser cannot resolve counts as a filter, so an unreadable command is never refused.
pub fn whole_suite(command: &str) -> Option<Refusal> {
    let normalised = shell::normalise(command);
    normalised.stages.iter().find_map(stage_is_whole_suite)
}

fn stage_is_whole_suite(stage: &Stage) -> Option<Refusal> {
    if stage
        .context
        .iter()
        .any(|c| matches!(c, Context::FunctionBody(_)))
    {
        return None;
    }
    let mut name = stage.program_name()?.to_string();
    let mut argv: &[Word] = &stage.argv;
    while let Some((inner, at)) = unwrap_wrapper(&name, argv) {
        argv = &argv[(at + 1).min(argv.len())..];
        name = inner.rsplit('/').next().unwrap_or(&inner).to_string();
    }
    // `None` stands for a word the parser could not read: treated as a filter.
    let args: Vec<Option<&str>> = argv.iter().map(|w| w.text()).collect();
    let refuse = |runner: String, instead: &'static str| Some(Refusal { runner, instead });
    match name.as_str() {
        "cargo" => cargo(&args),
        "make" | "gmake" => {
            let target = args
                .iter()
                .flatten()
                .find(|a| matches!(**a, "test" | "tests" | "check" | "bench"))?;
            refuse(
                format!("{name} {target}"),
                "the project's own runner with the test's name, e.g. `cargo test -p CRATE NAME`",
            )
        }
        "go" => {
            if args.first().copied().flatten() != Some("test") {
                return None;
            }
            let rest = &args[1..];
            let named = rest.iter().any(|a| match a {
                None => true,
                Some(a) => *a == "-run" || a.starts_with("-run="),
            });
            (!named).then(|| Refusal {
                runner: "go test".into(),
                instead: "`go test ./PKG -run TestName`",
            })
        }
        "pytest" | "py.test" => pytest(&args),
        "python" | "python3" => match (args.first().copied().flatten(), args.get(1)) {
            (Some("-m"), Some(Some("pytest"))) => pytest(&args[2..]),
            _ => None,
        },
        "npm" | "yarn" | "pnpm" => {
            let script = match args.first().copied().flatten()? {
                "test" | "t" => 1,
                "run" if args.get(1).copied().flatten() == Some("test") => 2,
                _ => return None,
            };
            // `npm test -- -t NAME` hands a filter to the test runner.
            let filtered = args[script..]
                .iter()
                .skip_while(|a| **a != Some("--"))
                .nth(1)
                .is_some();
            (!filtered).then(|| Refusal {
                runner: format!("{name} test"),
                instead: "`npx jest path/to/file.test.js -t NAME`, or `npm test -- -t NAME`",
            })
        }
        _ => None,
    }
}

/// `cargo test`, `cargo bench` and `cargo nextest run` need a filter and no `--workspace`;
/// `cargo build`, `check` and `clippy` are refused only across the whole workspace.
fn cargo(args: &[Option<&str>]) -> Option<Refusal> {
    let mut i = 0;
    while let Some(Some(a)) = args.get(i) {
        if a.starts_with('+') || matches!(*a, "--locked" | "--offline" | "--frozen" | "-q") {
            i += 1;
        } else if matches!(*a, "-Z" | "-C" | "--config" | "--color") {
            i += 2;
        } else {
            break;
        }
    }
    let sub = args.get(i).copied().flatten()?;
    let mut rest = &args[i + 1..];
    let whole = |rest: &[Option<&str>]| {
        rest.iter()
            .take_while(|a| **a != Some("--"))
            .any(|a| matches!(a, Some("--workspace" | "--all")))
    };
    let instead = "`cargo test -p CRATE TEST_NAME`, or `cargo test -p CRATE --test FILE TEST_NAME`";
    let runner = match sub {
        "test" | "t" | "bench" => format!("cargo {sub}"),
        "nextest" if rest.first().copied().flatten() == Some("run") => {
            rest = &rest[1..];
            "cargo nextest run".to_string()
        }
        "build" | "b" | "check" | "c" | "clippy" if whole(rest) => {
            return Some(Refusal {
                runner: format!("cargo {sub} --workspace"),
                instead: "`cargo check -p CRATE` for the crate the claim is about",
            });
        }
        _ => return None,
    };
    if whole(rest) {
        return Some(Refusal {
            runner: format!("{runner} --workspace"),
            instead,
        });
    }
    (!cargo_filter(rest)).then_some(Refusal { runner, instead })
}

/// Whether a test invocation names what it runs: a positional filter before or after `--`,
/// or a nextest filterset.
fn cargo_filter(rest: &[Option<&str>]) -> bool {
    const TAKES_VALUE: &[&str] = &[
        "-p",
        "--package",
        "--test",
        "--bin",
        "--example",
        "--bench",
        "--exclude",
        "--features",
        "-F",
        "--target",
        "--target-dir",
        "--manifest-path",
        "--profile",
        "-j",
        "--jobs",
        "--color",
        "--message-format",
        "--config",
        "-Z",
        "--skip",
        "--test-threads",
        "--format",
        "--partition",
        "--retries",
        "--cargo-profile",
    ];
    let mut i = 0;
    while i < rest.len() {
        let Some(a) = rest[i] else {
            return true;
        };
        if a == "-E" || a == "--filterset" || a.starts_with("--filterset=") {
            return true;
        }
        if TAKES_VALUE.contains(&a) {
            i += 2;
            continue;
        }
        if a == "--" || a.starts_with('-') {
            i += 1;
            continue;
        }
        return true;
    }
    false
}

fn pytest(args: &[Option<&str>]) -> Option<Refusal> {
    let named = args.iter().enumerate().any(|(i, a)| match a {
        None => true,
        Some(a) => {
            *a == "-k"
                || a.contains("::")
                || (a.ends_with(".py") && !matches!(args.get(i.wrapping_sub(1)), Some(Some("-c"))))
        }
    });
    (!named).then(|| Refusal {
        runner: "pytest".into(),
        instead: "`pytest path/to/test_x.py::test_y`",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(command: &str) -> bool {
        whole_suite(command).is_some()
    }

    #[test]
    fn whole_suites_are_refused() {
        for c in [
            "cargo test",
            "cargo test --workspace",
            "cargo test --workspace some_test",
            "cargo test -p letibot-tools",
            "cargo test -p x --lib",
            "cargo +nightly test --all",
            "cargo test --no-fail-fast -- --nocapture",
            "cargo nextest run",
            "cargo nextest run --workspace",
            "cargo bench",
            "cargo build --workspace",
            "cargo check --all",
            "make test",
            "make -j4 check",
            "go test ./...",
            "pytest",
            "pytest tests/",
            "python3 -m pytest",
            "npm test",
            "yarn run test",
            "timeout 600 cargo test",
            "env RUST_LOG=info cargo test --workspace",
            "cd crates && cargo test",
        ] {
            assert!(refused(c), "`{c}` is a whole-suite run and was allowed");
        }
    }

    #[test]
    fn a_named_test_is_allowed() {
        for c in [
            "cargo test -p x some_test",
            "cargo test --test file filter",
            "cargo test path::to::test -- --exact",
            "cargo test -p x -- some_test --exact",
            "cargo nextest run -E 'test(foo)'",
            "cargo nextest run foo",
            "cargo build -p x",
            "cargo check",
            "cargo test $NAME",
            "pytest path/to/test_x.py::test_y",
            "pytest -k foo",
            "pytest tests/test_x.py",
            "go test ./pkg -run TestX",
            "npm test -- -t name",
            "git diff a...b",
            "rg cargo test",
        ] {
            assert!(!refused(c), "`{c}` names a test and was refused");
        }
    }

    #[test]
    fn the_refusal_says_why_and_how_to_target_one_test() {
        let r = whole_suite("cargo test --workspace").expect("refused");
        let body = r.body();
        assert!(body.contains("the gate runs the suite"), "{body}");
        assert!(body.contains("cargo test -p CRATE TEST_NAME"), "{body}");
        assert!(body.contains("reviewers_run_suites"), "{body}");
    }
}
