use super::*;
use crate::backend::tempdir::TempDir;

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = crate::testing::git(dir).args(args).output().expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn every_spelling_of_one_remote_is_one_project() {
    let want = "github.com/letiflow/letibot";
    for url in [
        "git@github.com:letiflow/letibot.git",
        "https://github.com/letiflow/letibot.git",
        "https://user@github.com/letiflow/letibot/",
        "ssh://git@github.com/letiflow/letibot",
        "https://GitHub.com/letiflow/letibot",
    ] {
        assert_eq!(normalize_remote(url), want, "{url}");
    }
}

/// A worktree's notes are its checkout's, and a clone with the same `origin` elsewhere on disk
/// shares them too: the key follows the project, not the path.
#[test]
fn a_worktree_and_a_second_clone_share_the_checkouts_key() {
    let root = TempDir::new();
    let repo = root.path().join("proj");
    std::fs::create_dir_all(&repo).unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    git_ok(&repo, &["commit", "-q", "--allow-empty", "-m", "root"]);
    let wt = root.path().join("wt-x");
    git_ok(&repo, &["worktree", "add", "-q", &wt.to_string_lossy()]);
    let plain = project_key(&repo);
    assert!(plain.starts_with("proj-"), "{plain}");
    assert_eq!(project_key(&wt), plain);

    git_ok(
        &repo,
        &["remote", "add", "origin", "git@example.invalid:o/proj.git"],
    );
    let other = root.path().join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    git_ok(&other, &["init", "-q"]);
    git_ok(
        &other,
        &["remote", "add", "origin", "https://example.invalid/o/proj"],
    );
    let keyed = project_key(&repo);
    assert!(keyed.starts_with("proj-") && keyed != plain, "{keyed}");
    assert_eq!(project_key(&wt), keyed);
    assert_eq!(project_key(&other), keyed);
}

/// The first commit makes the repository; a second with nothing new is not an error and makes no
/// commit; a configured remote becomes `origin` and an existing one is kept.
#[test]
fn commit_makes_the_repository_and_ensure_adds_origin_once() {
    let root = TempDir::new();
    let repo = root.path().join("notes");
    std::fs::create_dir_all(repo.join("proj-1")).unwrap();
    std::fs::write(repo.join("proj-1/a.md"), "a\n").unwrap();
    commit(&repo, "add a").expect("first commit");
    commit(&repo, "nothing").expect("an empty commit is not an error");
    assert_eq!(git_ok(&repo, &["log", "--format=%s"]), "add a");
    assert_eq!(git_ok(&repo, &["branch", "--show-current"]), "main");

    ensure(&repo, Some("https://example.invalid/first.git")).unwrap();
    ensure(&repo, Some("https://example.invalid/second.git")).unwrap();
    assert_eq!(
        git_ok(&repo, &["remote", "get-url", "origin"]),
        "https://example.invalid/first.git"
    );
}

/// `push` sends `main` to `origin`, and rebases over what another box pushed first.
#[test]
fn push_rebases_over_the_remote_and_sends_main() {
    let root = TempDir::new();
    let bare = root.path().join("remote.git");
    std::fs::create_dir_all(&bare).unwrap();
    git_ok(&bare, &["init", "-q", "--bare", "-b", "main"]);
    let url = bare.to_string_lossy().into_owned();

    let a = root.path().join("a");
    std::fs::create_dir_all(a.join("p")).unwrap();
    std::fs::write(a.join("p/one.md"), "one\n").unwrap();
    commit(&a, "add one").unwrap();
    assert!(push(&a, None).unwrap_err().contains("no `origin`"));
    push(&a, Some(&url)).expect("first push");

    let b = root.path().join("b");
    std::fs::create_dir_all(b.join("p")).unwrap();
    std::fs::write(b.join("p/two.md"), "two\n").unwrap();
    commit(&b, "add two").unwrap();
    push(&b, Some(&url)).expect("a push over another box's history");

    let log = git_ok(&bare, &["log", "--format=%s", "main"]);
    assert_eq!(log, "add two\nadd one");
}

/// A box that signs every commit still commits its notes: they are letibot's, unsigned, and a key
/// that cannot sign is never asked.
#[test]
fn a_signing_config_does_not_stop_a_note_and_the_commit_is_letibots() {
    let root = TempDir::new();
    let repo = root.path().join("notes");
    std::fs::create_dir_all(&repo).unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    git_ok(&repo, &["config", "commit.gpgsign", "true"]);
    git_ok(&repo, &["config", "gpg.program", "false"]);
    std::fs::write(repo.join("a.md"), "a\n").unwrap();
    commit(&repo, "add a").expect("a signing config is not consulted");
    assert_eq!(
        git_ok(&repo, &["log", "--format=%an <%ae> | %cn | %G?"]),
        "letibot <letibot@localhost> | letibot | N"
    );
}
