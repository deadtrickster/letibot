//! `scripts/quick-tests.sh` maps the paths a gate run changed to the crates they are in, and runs
//! nextest over those crates and what depends on them. The mapping is the whole of the step's
//! judgement, so it is held here: a path in a crate selects that crate, a change that can reach
//! everything selects everything, and prose selects nothing.

use std::path::{Path, PathBuf};

/// The repository this test was built in.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// What the script would run for these changed paths - as it prints it, `--print`.
fn planned(tag: &str, paths: &[&str]) -> String {
    let dir =
        std::env::temp_dir().join(format!("letibot-quick-tests-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let changed = dir.join("changed.txt");
    let mut text = String::new();
    for p in paths {
        text.push_str(p);
        text.push('\n');
    }
    std::fs::write(&changed, text).unwrap();
    let out = std::process::Command::new("sh")
        .arg(root().join("scripts/quick-tests.sh"))
        .arg("--print")
        .current_dir(root())
        .env("LETIBOT_GATE_CHANGED", &changed)
        .output()
        .expect("sh runs");
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout).trim(),
        if out.status.success() {
            String::new()
        } else {
            format!(" (exit {:?})", out.status.code())
        }
    )
}

/// A change inside a crate is that crate, and everything that depends on it, through nextest's own
/// `rdeps` - not a hard-coded list of dependents, which would drift the first time a crate was
/// added.
#[test]
fn a_path_in_a_crate_selects_that_crate_and_its_dependents() {
    assert_eq!(
        planned("one", &["crates/tools/src/builtins/grep.rs"]),
        "rdeps(letibot-tools)"
    );
    // The package name comes from the crate's own manifest, not from the directory: these two are
    // not the same word.
    assert_eq!(
        planned("harnessd", &["crates/harnessd/src/mergequeue/serve.rs"]),
        "rdeps(letibot-harnessd)"
    );
    // Two crates are two, in the order they were first named, and one crate named twice is one.
    assert_eq!(
        planned(
            "two",
            &[
                "crates/harnessd/src/lib.rs",
                "crates/tools/src/lib.rs",
                "crates/harnessd/src/gate.rs",
            ]
        ),
        "rdeps(letibot-harnessd) + rdeps(letibot-tools)"
    );
    // The shell suites are a crate's tests: `scripts/` and the root `tests/` are carried by
    // `crates/tools/tests/shell_suites.rs`.
    assert_eq!(
        planned("scripts", &["scripts/letibot"]),
        "rdeps(letibot-tools)"
    );
    assert_eq!(
        planned("tests", &["tests/launcher.sh"]),
        "rdeps(letibot-tools)"
    );
    // The built-in guests are compiled by the harnessd build script, so they are harnessd's.
    assert_eq!(
        planned("guests", &["plugins/builtin/queue/src/lib.rs"]),
        "rdeps(letibot-harnessd)"
    );
}

/// A change that can reach every crate runs every crate's tests; a change that cannot reach any
/// crate runs none, and says so rather than pretending the gate passed something.
#[test]
fn a_manifest_runs_everything_and_prose_runs_nothing() {
    assert_eq!(planned("lock", &["Cargo.lock"]), "all()");
    assert_eq!(
        planned("nextest", &[".config/nextest.toml"]),
        "all()",
        "the profile is the tests' own shape"
    );
    assert_eq!(
        planned("both", &["crates/tools/src/lib.rs", "Cargo.toml"]),
        "all()",
        "a manifest change is everything, whatever else changed with it"
    );
    assert_eq!(
        planned("prose", &["docs/gate.md", "AGENTS.md"]),
        "quick-tests: nothing in this change is Rust (docs, workflows, prose); nothing to run"
    );
}

/// A run whose changed-path list is missing or empty is refused: *nothing changed* is a fact, and
/// a step that cannot read the list has no fact at all.
#[test]
fn a_run_with_no_list_is_refused() {
    let dir = std::env::temp_dir().join(format!("letibot-quick-nolist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let empty = dir.join("empty.txt");
    std::fs::write(&empty, "").unwrap();
    for (what, path) in [("missing", dir.join("nothing-here.txt")), ("empty", empty)] {
        let out = std::process::Command::new("sh")
            .arg(root().join("scripts/quick-tests.sh"))
            .arg("--print")
            .current_dir(root())
            .env("LETIBOT_GATE_CHANGED", &path)
            .output()
            .expect("sh runs");
        assert!(
            !out.status.success(),
            "a {what} list is not a pass: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "the refusal is its own exit, not a step's: {what}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
