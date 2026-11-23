//! `scripts/gate-at.sh`, the lead's gate for a stack, run on a scratch repository whose
//! `## Merge gate` is a handful of stand-in steps and whose `cargo` is a script on `PATH`. The
//! verdict is the steps' exit status and the scratch's leak check, never a count of `ok` lines.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gate-at-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_exe(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        // The box's own config is held out of it: a global `commit.gpgsign` needs a key no
        // seat has, and these fixtures commit.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

struct Run {
    code: Option<i32>,
    out: String,
    dir: PathBuf,
}

/// A repository whose AGENTS.md declares `steps`, with `cargo` on `PATH` running `cargo_body`, and
/// `gate-at.sh` run on it against `the-base`.
fn gate(tag: &str, steps: &[&str], cargo_body: &str) -> Run {
    let dir = scratch(tag);
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    for s in ["gate-env.sh", "gate-steps.sh"] {
        let from = root().join("scripts").join(s);
        if from.is_file() {
            std::fs::copy(&from, repo.join("scripts").join(s)).unwrap();
        }
    }
    write_exe(
        &repo.join("scripts/check-fmt.sh"),
        &format!(
            "#!/bin/sh\necho \"$@\" > {}\n",
            dir.join("fmt-args").display()
        ),
    );
    std::fs::write(
        repo.join("AGENTS.md"),
        format!("## Merge gate\n\n```sh\n{}\n```\n", steps.join("\n")),
    )
    .unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "the-base"]);
    let bin = dir.join("bin");
    write_exe(&bin.join("cargo"), &format!("#!/bin/sh\n{cargo_body}\n"));
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new("sh")
        .arg(root().join("scripts/gate-at.sh"))
        .arg(&repo)
        .arg(dir.join("target"))
        .arg("the-base")
        .env_clear()
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("HOME", &home)
        .env("TMPDIR", &dir)
        .env("LETIBOT_GATE_LOCK", dir.join("gate.lock"))
        .env("LETIBOT_GATE_LOGS", dir.join("logs"))
        .output()
        .unwrap();
    Run {
        code: out.status.code(),
        out: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        dir,
    }
}

const SUITES_THEN_KILLED: &str = "i=0\nwhile [ $i -lt 150 ]; do echo 'test result: ok. 1 passed; 0 failed'; i=$((i+1)); done\nkill -KILL $$";

/// 150 suites said ok, then the run was killed: the exit status is the verdict.
#[test]
fn a_killed_run_after_140_suites_is_not_green() {
    let r = gate(
        "killed",
        &[
            "sh scripts/check-fmt.sh main",
            "cargo test --workspace --no-fail-fast",
        ],
        SUITES_THEN_KILLED,
    );
    assert!(!r.out.contains("GREEN"), "{}", r.out);
    assert_eq!(r.code, Some(3), "{}", r.out);
    assert!(
        r.out.contains("exited 137"),
        "the step and its status are named: {}",
        r.out
    );
}

/// A store under the gate's scratch is a test that wrote where the operator's store lives.
#[test]
fn a_store_left_under_the_scratch_is_red() {
    let r = gate(
        "leak",
        &[
            "sh scripts/check-fmt.sh main",
            "cargo test --workspace --no-fail-fast",
        ],
        "mkdir -p \"$XDG_DATA_HOME/letibot\" && : > \"$XDG_DATA_HOME/letibot/sessions.db\"\n\
         i=0\nwhile [ $i -lt 150 ]; do echo 'test result: ok. 1 passed; 0 failed'; i=$((i+1)); done",
    );
    assert!(!r.out.contains("GREEN"), "{}", r.out);
    assert_eq!(r.code, Some(3), "{}", r.out);
    assert!(r.out.contains("LEAKED: "), "{}", r.out);
}

/// Every declared step runs, in order, but the install, and `check-fmt.sh` is handed the base.
#[test]
fn the_declared_steps_run_but_the_install() {
    let r = gate(
        "steps",
        &[
            "sh scripts/check-fmt.sh main",
            "echo one >> \"$TMPDIR/ran\"",
            "cargo test --workspace",
            "echo install >> \"$TMPDIR/ran\" # install-gate-build",
        ],
        "echo cargo >> \"$TMPDIR/ran\"",
    );
    assert_eq!(r.code, Some(0), "{}", r.out);
    assert!(r.out.contains("GREEN"), "{}", r.out);
    let fmt = std::fs::read_to_string(r.dir.join("fmt-args")).unwrap();
    assert_eq!(fmt.trim(), "the-base", "check-fmt compares against BASE");
    let ran = std::fs::read_to_string(r.dir.join("ran")).unwrap_or_default();
    assert_eq!(ran, "one\ncargo\n", "{}", r.out);
}
