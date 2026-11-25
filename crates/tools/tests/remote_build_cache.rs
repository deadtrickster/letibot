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

/// What a command of this session sees of the three variables.
fn seen(backend: &SshBackend) -> String {
    let out = backend
        .run(&Command {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "printf 'W=[%s] C=[%s] H=[%s]' \"$RUSTC_WRAPPER\" \"$SCCACHE_DIR\" \
                 \"$CARGO_HOME\""
                    .into(),
            ],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("a command on the fake host");
    assert_eq!(out.exit, 0, "{}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The one path a placement's build keys resolve to on the fake host: `readlink -f` there, because a
/// bind has to land where the writes go and this box's temp directory may be a symlink.
fn resolved(at: &Path) -> String {
    std::fs::canonicalize(at)
        .expect("the path exists")
        .display()
        .to_string()
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
            "W=[{}] C=[{}] H=[]",
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
        "W=[] C=[] H=[]",
        "a wrapper that is not an executable file must not be set"
    );
    let said = backend.placement_notes().join("\n");
    assert!(
        said.contains("rustc_wrapper") && said.contains("not an executable file"),
        "{said}"
    );
}

/// A shared cargo home reaches every command of the session, resolved on the host: this is what makes
/// two sessions' builds of the same dependency compile the same bytes at the same path, which is the
/// only way a compile cache keyed by source path can hit.
#[test]
fn a_declared_cargo_home_reaches_the_remote_command() {
    let rig = fake_remote::Rig::new("cargo-on", &["xmg"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    let home = rig
        .remote_root()
        .parent()
        .expect("the fake home")
        .join("cargo-home");
    std::fs::create_dir_all(&home).expect("the cargo home");
    let host = host_with(&rig, "xmg", &format!("cargo_home = \"{}\"", home.display()));
    let backend = place(&rig, &repo, host);
    assert_eq!(
        seen(&backend),
        format!("W=[] C=[] H=[{}]", resolved(&home)),
        "the cargo home is the one the file named, resolved on the host"
    );
    let said = backend.placement_notes().join("\n");
    assert!(
        said.contains("share one cargo home") && said.contains(&resolved(&home)),
        "{said}"
    );
}

/// A cargo home the machine has not got is skipped with one note: `CARGO_HOME` pointing at nothing is
/// a `cargo` that makes a directory it cannot make, or worse silently writes somewhere else.
#[test]
fn a_cargo_home_that_is_not_there_is_skipped_and_named_once() {
    let rig = fake_remote::Rig::new("cargo-missing", &["xmg"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    let gone = rig.remote_root().join("no-such-cargo-home");
    let host = host_with(&rig, "xmg", &format!("cargo_home = \"{}\"", gone.display()));
    let backend = place(&rig, &repo, host);
    assert_eq!(seen(&backend), "W=[] C=[] H=[]");
    let said = backend.placement_notes().join("\n");
    assert!(
        said.contains("cargo_home") && said.contains("is not a directory"),
        "{said}"
    );
}

/// A cargo home inside the host's owner homes is refused: every session's `$HOME` there is its own, so
/// nothing would be shared, and the registry would be counted and cleaned as that session's data.
#[test]
fn a_cargo_home_inside_the_owner_homes_is_refused() {
    let rig = fake_remote::Rig::new("cargo-private", &["xmg"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    // Canonical, because the placement compares the host's own `readlink -f` answer with the owner
    // homes under `$HOME`, and a temp directory behind a symlink would spell it two ways.
    let private =
        PathBuf::from(resolved(&rig.remote_root())).join(".letibot-home/s-cache01/.cargo");
    std::fs::create_dir_all(&private).expect("a session's own cargo home");
    let host = host_with(
        &rig,
        "xmg",
        &format!("cargo_home = \"{}\"", private.display()),
    );
    let backend = place(&rig, &repo, host);
    assert_eq!(
        seen(&backend),
        "W=[] C=[] H=[]",
        "a path under the owner homes is one session's, never the machine's"
    );
    let said = backend.placement_notes().join("\n");
    assert!(
        said.contains("cargo_home") && said.contains("session's own home"),
        "{said}"
    );
}

/// A cargo home the ssh user cannot write is skipped too: a fetch that cannot land there is a build
/// that fails, and this box's own writability is not the question — the host's user's is.
#[test]
fn a_cargo_home_that_the_host_cannot_write_is_skipped() {
    use std::os::unix::fs::PermissionsExt;
    let rig = fake_remote::Rig::new("cargo-ro", &["xmg"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    let home = rig
        .remote_root()
        .parent()
        .expect("the fake home")
        .join("cargo-ro");
    std::fs::create_dir_all(&home).expect("the cargo home");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).expect("its mode");
    // Root writes whatever the mode says, so the question this test asks has no answer there.
    if std::fs::write(home.join("probe"), b"").is_ok() {
        let _ = std::fs::remove_file(home.join("probe"));
        eprintln!("skipped: this test runs as a user the mode bits do not bind");
        return;
    }
    let host = host_with(&rig, "xmg", &format!("cargo_home = \"{}\"", home.display()));
    let backend = place(&rig, &repo, host);
    assert_eq!(seen(&backend), "W=[] C=[] H=[]");
    let said = backend.placement_notes().join("\n");
    assert!(
        said.contains("cargo_home") && said.contains("is not writable"),
        "{said}"
    );
}

/// A host that declares none of the three is exactly what every host was before these keys: no
/// variable, no grant, and nothing said about it.
#[test]
fn a_host_with_no_wrapper_carries_nothing() {
    let rig = fake_remote::Rig::new("cache-none", &["xmg"]);
    let repo = repo_at(&rig.remote_root().join("..").join("loc"));
    let host = host_with(&rig, "xmg", "");
    let backend = place(&rig, &repo, host);
    assert_eq!(seen(&backend), "W=[] C=[] H=[]");
    let said = backend.placement_notes().join("\n");
    assert!(
        !said.contains("rustc_wrapper") && !said.contains("sccache") && !said.contains("cargo"),
        "a host that declares nothing must not be told a wrapper was skipped: {said}"
    );
}
