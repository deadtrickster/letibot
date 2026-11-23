//! A placement's `.env` files, measured against a fake ssh: the operator names the host before a
//! credential travels, a VM on that host needs its own switch, and what arrives is 0600 and never
//! a commit.

#[path = "support/fake_remote.rs"]
mod fake_remote;

use std::path::{Path, PathBuf};
use std::time::Duration;

use letibot_tools::dotenv::Settings;
use letibot_tools::exec::Egress;
use letibot_tools::remote::{Hosts, SshBackend, SshSpec};

/// A repository with one commit, which is what a placement sends as a bundle.
fn repo_at(dir: &Path) -> PathBuf {
    let repo = dir.to_path_buf();
    std::fs::create_dir_all(&repo).expect("a repository");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    letibot_tools::testing::repo_config(&repo);
    std::fs::write(repo.join("README.md"), "the tree\n").expect("a file");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "one"]);
    repo
}

/// One placement onto the rig's host, with this test's `.env` settings.
fn place(rig: &fake_remote::Rig, repo: &Path, host: &str, dotenv: Settings) -> SshBackend {
    let text = std::fs::read_to_string(rig.hosts_file()).expect("hosts.toml");
    let hosts = Hosts::parse(&text).expect("the fixture's hosts");
    let chosen = hosts.get(host).cloned().expect("the host is declared");
    let spec = SshSpec {
        host: chosen,
        name: "s-dotenvc".into(),
        source: repo.to_path_buf(),
        writable: true,
        exec: true,
        ssh: Some(rig.ssh_program()),
        paths: Some(rig.paths()),
        bound: Some(Duration::from_secs(30)),
        place_bound: Some(Duration::from_secs(30)),
        egress: Egress::Denied,
        // The fake answers for a machine this test cannot probe, so the answer is pinned: the same
        // test would otherwise take two paths on two boxes.
        confine: Some(false),
        vm: false,
        env: Vec::new(),
        env_secrets: Vec::new(),
        grants: Vec::new(),
        dotenv,
    };
    SshBackend::place(&spec).expect("the placement is arranged")
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .expect("a file")
        .permissions()
        .mode()
        & 0o777
}

/// A host the operator named gets the files, at 0600, and git on that machine does not see them.
#[test]
fn a_named_host_gets_the_files_and_git_ignores_them() {
    let rig = fake_remote::Rig::new("dotenv-named", &["xmg"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    std::fs::write(repo.join(".env"), "KO_DOCKER_REPO=kind.local\n").expect("a .env");
    std::fs::write(repo.join(".env.local"), "EXTRA=1\n").expect("a local");
    std::fs::write(repo.join(".envrc"), "export NOPE=1\n").expect("an rc");
    let settings = Settings {
        carry_to: vec!["xmg".to_string()],
        ..Settings::default()
    };
    let backend = place(&rig, &repo, "xmg", settings);
    let wt = PathBuf::from(backend.worktree_path());
    assert_eq!(
        std::fs::read_to_string(wt.join(".env")).expect("the file arrived"),
        "KO_DOCKER_REPO=kind.local\n"
    );
    assert_eq!(mode(&wt.join(".env")), 0o600);
    assert_eq!(mode(&wt.join(".env.local")), 0o600);
    assert!(!wt.join(".envrc").exists(), ".envrc is never carried");
    // Never a commit: a clean checkout, and the exclude file is what keeps it clean.
    let status = std::process::Command::new("git")
        .args(["-C", &wt.display().to_string(), "status", "--porcelain"])
        .output()
        .expect("git");
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        "",
        "the carried file shows up as a change"
    );
    // And the name is all the notes say: no value of either file appears in them.
    let said = backend.placement_notes().join("\n");
    assert!(said.contains(".env"), "{said}");
    assert!(!said.contains("kind.local"), "{said}");
    assert!(!said.contains("EXTRA"), "{said}");
}

/// A host the operator did NOT name gets nothing, which is the default.
#[test]
fn an_unnamed_host_gets_nothing() {
    let rig = fake_remote::Rig::new("dotenv-unnamed", &["lab2x1"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    std::fs::write(repo.join(".env"), "TOKEN_VALUE=abc\n").expect("a .env");
    let backend = place(&rig, &repo, "lab2x1", Settings::default());
    let wt = PathBuf::from(backend.worktree_path());
    assert!(
        !wt.join(".env").exists(),
        "a host nobody named was carried to: {}",
        wt.display()
    );
}

/// A VM on a named host is its own switch, and it is off by default. The guest itself cannot be
/// booted by this fixture, so the decision is measured where it is made: both conditions, and no
/// pair of them that is not both. The local guest's copy has its own test (`dotenv::carry`).
#[test]
fn a_vm_is_carried_to_only_under_its_own_switch() {
    let named = Settings {
        carry_to: vec!["xmg".to_string()],
        carry_to_vm: false,
        ..Settings::default()
    };
    assert!(named.carries_to("xmg"), "the host is named");
    assert!(!named.carries_to_vm("xmg"), "a VM is off by default");

    let both = Settings {
        carry_to: vec!["xmg".to_string()],
        carry_to_vm: true,
        ..Settings::default()
    };
    assert!(both.carries_to_vm("xmg"));
    assert!(
        !both.carries_to_vm("lab2x1"),
        "the VM switch is not a host list: a host nobody named is still nobody's"
    );

    // And the file is not written when the switch is off: the placement's own copy is where the
    // decision would show, and a name the operator did not give is the same answer on a VM.
    let rig = fake_remote::Rig::new("dotenv-vm-off", &["lab2x1"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    std::fs::write(repo.join(".env"), "TOKEN_VALUE=abc\n").expect("a .env");
    let backend = place(
        &rig,
        &repo,
        "lab2x1",
        Settings {
            carry_to: vec!["xmg".to_string()],
            carry_to_vm: true,
            ..Settings::default()
        },
    );
    let wt = PathBuf::from(backend.worktree_path());
    assert!(
        !wt.join(".env").exists(),
        "a host nobody named was given the file"
    );
}
