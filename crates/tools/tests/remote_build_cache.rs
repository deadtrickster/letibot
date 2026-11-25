//! A host's `rustc_wrapper` and `sccache_dir`, measured through a fake ssh: the wrapper reaches every
//! command of the session when the host has it, nothing is set when the host does not, and a file
//! the operator configured but the machine lacks is a note rather than a broken build.

#[path = "support/fake_remote.rs"]
mod fake_remote;

use std::path::{Path, PathBuf};
use std::time::Duration;

use letibot_tools::backend::{Command, ExecBackend};
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

/// The host the rig declared, with `lines` added to its table: the placement's spec carries the
/// parsed host, so the file has to be what the operator would have written.
fn host_with(rig: &fake_remote::Rig, name: &str, lines: &str) -> letibot_tools::remote::Host {
    let text = std::fs::read_to_string(rig.hosts_file()).expect("hosts.toml");
    let text = text.replace(
        &format!("[host.{name}]"),
        &format!("[host.{name}]\n{}", lines.trim_end()),
    );
    std::fs::write(rig.hosts_file(), text).expect("hosts.toml");
    let hosts = Hosts::parse(&std::fs::read_to_string(rig.hosts_file()).expect("hosts.toml"))
        .expect("the fixture's hosts");
    hosts.get(name).cloned().expect("the host is declared")
}

/// One placement onto the rig's host, with no boundary: the probe would answer for whatever machine
/// runs the test, so the answer is pinned and the same test takes one path on two boxes.
fn place(rig: &fake_remote::Rig, repo: &Path, host: letibot_tools::remote::Host) -> SshBackend {
    let spec = SshSpec {
        host,
        name: "s-cache01".into(),
        source: repo.to_path_buf(),
        writable: true,
        exec: true,
        ssh: Some(rig.ssh_program()),
        paths: Some(rig.paths()),
        bound: Some(Duration::from_secs(30)),
        place_bound: Some(Duration::from_secs(30)),
        egress: Egress::Denied,
        askpass: None,
        confine: Some(false),
        vm: false,
        env: Vec::new(),
        env_secrets: Vec::new(),
        grants: Vec::new(),
        dotenv: letibot_tools::dotenv::Settings::default(),
    };
    SshBackend::place(&spec).expect("the placement is arranged")
}

/// An executable file, as a host's wrapper would be: the placement asks `[ -x ]` and nothing else.
fn executable(at: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(at, "#!/bin/sh\nexec \"$@\"\n").expect("the wrapper");
    std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o755)).expect("its mode");
}

/// What a command of this session sees of the two variables.
fn seen(backend: &SshBackend) -> String {
    let out = backend
        .run(&Command {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "printf 'W=[%s] C=[%s]' \"$RUSTC_WRAPPER\" \"$SCCACHE_DIR\"".into(),
            ],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("a command on the fake host");
    assert_eq!(out.exit, 0, "{}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The wrapper and the cache the operator declared reach every command, and the placement says so
/// once: this is the whole of "a package placed here does not compile the tree cold".
#[test]
fn a_declared_wrapper_and_cache_reach_the_remote_command() {
    let rig = fake_remote::Rig::new("cache-on", &["xmg"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    let wrapper = rig.bin().join("sccache-rustc");
    executable(&wrapper);
    let cache = rig
        .remote_root()
        .parent()
        .expect("the fake home")
        .join("cache")
        .join("sccache");
    std::fs::create_dir_all(&cache).expect("the cache directory");
    let host = host_with(
        &rig,
        "xmg",
        &format!(
            "rustc_wrapper = \"{}\"\nsccache_dir = \"{}\"",
            wrapper.display(),
            cache.display()
        ),
    );
    let backend = place(&rig, &repo, host);
    // `readlink -f` on a machine where the temp directory is a symlink answers the real path, so
    // the expectation is the canonical one.
    let wrapper_real = std::fs::canonicalize(&wrapper).expect("the wrapper");
    let cache_real = std::fs::canonicalize(&cache).expect("the cache");
    assert_eq!(
        seen(&backend),
        format!(
            "W=[{}] C=[{}]",
            wrapper_real.display(),
            cache_real.display()
        ),
        "the wrapper is the one the file named, resolved on the host"
    );
    let said = backend.placement_notes().join("\n");
    assert!(
        said.contains("share one compile cache")
            && said.contains(&cache_real.display().to_string()),
        "{said}"
    );
}

/// A wrapper the operator configured and the machine has not got is skipped, not fatal: cargo would
/// refuse a `RUSTC_WRAPPER` that names nothing, so the variable is left unset and the placement says
/// which key to fix. The build that follows compiles cold, which is what it did before the key.
#[test]
fn a_wrapper_that_is_not_there_is_skipped_and_named_once() {
    let rig = fake_remote::Rig::new("cache-missing", &["xmg"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    let gone = rig.bin().join("no-such-wrapper");
    let host = host_with(
        &rig,
        "xmg",
        &format!("rustc_wrapper = \"{}\"", gone.display()),
    );
    let backend = place(&rig, &repo, host);
    assert_eq!(
        seen(&backend),
        "W=[] C=[]",
        "a wrapper that is not an executable file must not be set"
    );
    let said = backend.placement_notes().join("\n");
    assert!(
        said.contains("rustc_wrapper") && said.contains("not an executable file"),
        "{said}"
    );
}

/// A host that declares neither is exactly what every host was before this key: no variable, no
/// grant, and nothing said about it.
#[test]
fn a_host_with_no_wrapper_carries_nothing() {
    let rig = fake_remote::Rig::new("cache-none", &["xmg"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    let host = host_with(&rig, "xmg", "");
    let backend = place(&rig, &repo, host);
    assert_eq!(seen(&backend), "W=[] C=[]");
    let said = backend.placement_notes().join("\n");
    assert!(
        !said.contains("rustc_wrapper") && !said.contains("sccache"),
        "a host that declares nothing must not be told a wrapper was skipped: {said}"
    );
}
