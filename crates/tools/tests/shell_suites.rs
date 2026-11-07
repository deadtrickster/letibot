//! **The shell half of the tree is judged by shell, so it is run from here.** `scripts/letibot`
//! decides where a run's turns go and `scripts/rrun` decides what a host is sent, each before any
//! Rust executes; `tests/launcher.sh` and `tests/rrun.sh` are what judges them. Running them inside
//! `cargo test` puts them in the step every gate already has - four launcher cases drifted unseen
//! for weeks because nothing ran the file (R152).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The suites, by their path from the repository root, which is where their headers say to run
/// them from; each builds its own throwaway home and stubs every binary it reaches.
const SUITES: &[&str] = &["tests/launcher.sh", "tests/rrun.sh"];

/// What a suite is written in. Without both it cannot run at all, so an absence is said out loud
/// through `apparatus` rather than read as a pass.
const NEEDED: &[&str] = &["bash", "python3"];

/// The repository this test was built in - in a landing worktree too, which is where the gate
/// runs, so a suite is judged against the branch being landed and not against another tree.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The first `name` on `PATH` that is an executable file: what `command -v` answers, no shell. A
/// file that cannot be executed is not there, and running it would fail as a file error instead.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Run one suite from the repository root and answer whether it passed. The output goes to stderr,
/// which the gate keeps: a red's diagnosis is the case that failed and the lines it printed.
fn run(suite: &str, bash: &Path) -> bool {
    let out = Command::new(bash)
        .arg(suite)
        .current_dir(root())
        .output()
        .unwrap_or_else(|e| panic!("{suite} could not be started: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        // The suite's own last line is the one that says it walked every case, not that it parsed.
        eprintln!("{suite}: {}", text.lines().last().unwrap_or("(no output)"));
        return true;
    }
    eprintln!(
        "{suite} exited {:?}, its output follows:\n{text}",
        out.status
    );
    false
}

/// Both suites pass. They run in this one test and one after the other, so a red names the suite
/// that failed and the second is still judged; `--test-threads` would otherwise run them beside
/// every other test in this crate and beside each other.
#[test]
fn the_shell_suites_pass() {
    let missing: Vec<&str> = NEEDED
        .iter()
        .copied()
        .filter(|p| on_path(p).is_none())
        .collect();
    let Some(_) = letibot_tokencore::apparatus::present(
        &format!(
            "{} — a shell suite is written in {}",
            missing.join(" and "),
            NEEDED.join(" and ")
        ),
        missing.is_empty(),
    ) else {
        return;
    };
    let bash = on_path("bash").expect("checked above");
    let failed: Vec<&str> = SUITES
        .iter()
        .copied()
        .filter(|suite| !run(suite, &bash))
        .collect();
    assert!(
        failed.is_empty(),
        "{} did not pass, and its failing cases are above",
        failed.join(" and ")
    );
}
