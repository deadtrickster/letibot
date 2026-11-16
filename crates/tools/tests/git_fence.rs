//! A child's git fence is its env, and this file measures the env rather than a boundary: the
//! tight seat hands a child `GIT_CONFIG_*` pairs that reset the credential helper list, so a
//! helper written in the operator's gitconfig, in the repository the child commits to, or in the
//! box's system config cannot answer for it. No `bwrap` is needed, because nothing here reads a
//! file the fence does not already hand over.

use std::path::{Path, PathBuf};

use letibot_tools::exec::seat::seat_view;
use letibot_tools::mode::Mode;

/// The value the fixture helper answers with. Long enough for any mask, and unmistakable in
/// output a test prints when it fails.
const PASSWORD: &str = "hunter2hunter2fixture";

/// A credential helper for one host, written the way an operator writes one.
fn helper_section() -> String {
    format!(
        "[credential \"https://git.example.test\"]\n\thelper = \"!f() {{ echo username=x; \
         echo password={PASSWORD}; }}; f\"\n"
    )
}

/// A throwaway home with the operator's own gitconfig: an identity AND a helper for one host.
/// The identity is why the seat grants the file at all, which is what makes the helper ride in.
fn operator_home(root: &Path) -> PathBuf {
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("a fixture home");
    std::fs::write(
        home.join(".gitconfig"),
        format!(
            "[user]\n\tname = Operator\n\temail = op@example.test\n{}",
            helper_section()
        ),
    )
    .expect("a gitconfig");
    home
}

/// A repository whose own `config` carries the helper: a child holds the common dir read-write,
/// so this is the file it could have written itself.
fn repository_with_a_helper(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("a fixture repository");
    git_in(&repo, &["init", "-q", "-b", "main", "."]);
    let mut config = std::fs::read_to_string(repo.join(".git/config")).expect("the repo config");
    config.push_str(&helper_section());
    std::fs::write(repo.join(".git/config"), config).expect("the repo config");
    repo
}

/// A system-wide config carrying the helper, reached through `GIT_CONFIG_SYSTEM` because this
/// test may not write `/etc`. What the env's reset does to a LOWER-precedence scope is the same
/// question wherever that scope's file lives.
fn system_config_with_a_helper(root: &Path) -> PathBuf {
    let at = root.join("gitconfig-system");
    std::fs::write(&at, helper_section()).expect("a system gitconfig");
    at
}

/// `git credential fill` is the door a push goes through to be handed a password, so this is
/// where a helper has to be visible or not.
fn credential_fill(
    repo: Option<&Path>,
    env: &[(String, String)],
    home: &Path,
    system: &Path,
) -> String {
    use std::io::Write;
    let mut cmd = std::process::Command::new("/usr/bin/git");
    if let Some(r) = repo {
        cmd.arg("-C").arg(r);
    }
    cmd.args(["credential", "fill"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        // The confined `$HOME` is a fresh directory: the seat's own env is the only way in.
        .env("HOME", home)
        .env("GIT_CONFIG_SYSTEM", system)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("git is on this box");
    child
        .stdin
        .as_mut()
        .expect("a pipe")
        .write_all(b"protocol=https\nhost=git.example.test\n\n")
        .expect("the ask");
    let out = child.wait_with_output().expect("git answers");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `git` for arranging a fixture, with the box's own config kept out of it.
fn git_in(dir: &Path, args: &[&str]) {
    let o = std::process::Command::new("/usr/bin/git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.test")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.test")
        .output()
        .expect("git");
    assert!(
        o.status.success(),
        "{args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&o.stderr)
    );
}

/// The pairs a tight seat hands a child, over a home whose gitconfig is one of its grants.
fn tight_seat_env(home: &Path) -> Vec<(String, String)> {
    let present = [home.join(".gitconfig").display().to_string()];
    seat_view(&Mode::WRITES_ALLOWED, "here", &present, home).env
}

/// The control the two tests below depend on: the fixture helper really does answer, so a test
/// that finds no password is measuring the seat's env and not a broken fixture.
#[test]
fn the_fixture_helper_answers_without_the_seats_reset() {
    let root = std::env::temp_dir().join(format!("letibot-gitfence-ctl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = operator_home(&root);
    let fresh = root.join("fresh");
    std::fs::create_dir_all(&fresh).expect("a confined home");
    let seat = tight_seat_env(&home);
    // Everything the seat says except its credential reset: what a child had before the fence.
    let without: Vec<(String, String)> = seat
        .iter()
        .filter(|(k, _)| {
            !k.starts_with("GIT_CONFIG_COUNT")
                && !k.starts_with("GIT_CONFIG_KEY_")
                && !k.starts_with("GIT_CONFIG_VALUE_")
        })
        .cloned()
        .collect();
    let said = credential_fill(None, &without, &fresh, Path::new("/dev/null"));
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        said.contains(PASSWORD),
        "the fixture's helper has to answer when nothing resets the list, or the tests beside \
         this one prove nothing: {said}"
    );
}

/// The finding's own case: the helper the seat's grant carries in has to be dead in the child.
#[test]
fn a_credential_helper_in_the_operators_gitconfig_is_not_honoured_in_a_child() {
    let root = std::env::temp_dir().join(format!("letibot-gitfence-global-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = operator_home(&root);
    let fresh = root.join("fresh");
    std::fs::create_dir_all(&fresh).expect("a confined home");
    let env = tight_seat_env(&home);
    assert!(
        env.iter().any(|(k, v)| k == "GIT_CONFIG_GLOBAL"
            && v == &home.join(".gitconfig").display().to_string()),
        "the fixture home has to be the granted gitconfig, or the test measures nothing: {env:?}"
    );
    let said = credential_fill(None, &env, &fresh, Path::new("/dev/null"));
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        !said.contains(PASSWORD),
        "a child was handed a password by the operator's own gitconfig: {said}"
    );
}

/// A helper in the repository the child commits to, which it holds read-write.
#[test]
fn a_credential_helper_in_a_granted_repository_is_not_honoured_in_a_child() {
    let root = std::env::temp_dir().join(format!("letibot-gitfence-repo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = operator_home(&root);
    let repo = repository_with_a_helper(&root);
    let fresh = root.join("fresh");
    std::fs::create_dir_all(&fresh).expect("a confined home");
    let env = tight_seat_env(&home);
    let said = credential_fill(Some(&repo), &env, &fresh, Path::new("/dev/null"));
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        !said.contains(PASSWORD),
        "a child was handed a password by the repository's own config: {said}"
    );
}

/// And the scope below both: a system config, which `/etc` being bound in the view makes
/// reachable, and which the env's reset has to cover by precedence rather than by absence.
#[test]
fn a_credential_helper_in_the_system_config_is_not_honoured_in_a_child() {
    let root = std::env::temp_dir().join(format!("letibot-gitfence-system-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = operator_home(&root);
    let system = system_config_with_a_helper(&root);
    let fresh = root.join("fresh");
    std::fs::create_dir_all(&fresh).expect("a confined home");
    let env = tight_seat_env(&home);
    let said = credential_fill(None, &env, &fresh, &system);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        !said.contains(PASSWORD),
        "a child was handed a password by the box's system config: {said}"
    );
}
