//! The mirrors, on temporary repositories: a pull request of an org's second repository is reviewed
//! from that repository's own tree, and a re-review lands where the last one was made.

use super::*;
use crate::source::Pull;

/// A directory of this test's own, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "letibot-mirror-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        TempDir(std::fs::canonicalize(&dir).expect("a real path"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `git` in `dir` for a fixture: the box's own config is not read, and the identity is the
/// fixture's, so a commit never depends on who runs the test.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "letibot fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@letibot.invalid")
        .env("GIT_COMMITTER_NAME", "letibot fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@letibot.invalid")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One repository with one commit, and a pull request on it: GitHub's `refs/pull/<n>/head` is what
/// a seat fetches, so the fixture has that ref and no worktree of its own.
fn a_repo(under: &Path, name: &str, file: &str, text: &str, number: u64) -> PathBuf {
    let repo = under.join(name);
    std::fs::create_dir_all(&repo).expect("the repository's directory");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(file), text).expect("a file");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "the change"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &["update-ref", &format!("refs/pull/{number}/head"), &head],
    );
    repo
}

fn a_pull(repo: &str, number: u64) -> Pull {
    Pull {
        repo: repo.into(),
        number,
        head_sha: String::new(),
        base_sha: String::new(),
        branch: "topic".into(),
        from_fork: false,
        title: "a change".into(),
        description: String::new(),
        author: "someone".into(),
        updated_ms: 0,
    }
}

/// The point of the mirrors: an org scope's second repository is reviewed from a tree of ITS OWN,
/// where the first repository's files are not. The one checkout the seat was started in proves
/// nothing about a repository it does not hold.
#[test]
fn an_orgs_second_repository_is_reviewed_through_its_own_mirror() {
    let dir = TempDir::new("two-repos");
    let sources = dir.0.join("sources");
    std::fs::create_dir_all(&sources).expect("the sources' directory");
    let first = a_repo(&sources, "alpha", "alpha.txt", "alpha's own file\n", 12);
    let second = a_repo(&sources, "beta", "beta.txt", "beta's own file\n", 12);

    let mirrors = Mirrors::new(dir.0.join("github"));
    let one = mirrors
        .tree(
            &a_pull("stroppy-io/alpha", 12),
            &format!("file://{}", first.display()),
        )
        .expect("alpha's tree");
    let two = mirrors
        .tree(
            &a_pull("stroppy-io/beta", 12),
            &format!("file://{}", second.display()),
        )
        .expect("beta's tree");

    assert_ne!(one, two, "one tree per repository, and per number");
    assert_eq!(
        std::fs::read_to_string(two.join("beta.txt")).expect("beta's file"),
        "beta's own file\n"
    );
    assert!(
        !two.join("alpha.txt").exists(),
        "beta's tree is not alpha's"
    );
    assert!(!one.join("beta.txt").exists(), "alpha's tree is not beta's");
    // And one mirror per repository, not one per seat: both trees were cut from a mirror of their
    // own repository, beside each other under the root.
    let mut mirrors_on_disk: Vec<String> = std::fs::read_dir(dir.0.join("github"))
        .expect("the mirrors' root")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".git"))
        .collect();
    mirrors_on_disk.sort();
    assert_eq!(
        mirrors_on_disk,
        vec!["stroppy-io-alpha.git", "stroppy-io-beta.git"]
    );
}

/// A second review of the same pull request lands in the tree the first one was made in, at the new
/// head - what a reviewer read last time is not reseeded from scratch.
#[test]
fn a_re_review_reuses_the_tree_at_the_new_head() {
    let dir = TempDir::new("re-review");
    let sources = dir.0.join("sources");
    std::fs::create_dir_all(&sources).expect("the sources' directory");
    let repo = a_repo(&sources, "alpha", "alpha.txt", "first\n", 12);
    let mirrors = Mirrors::new(dir.0.join("github"));
    let url = format!("file://{}", repo.display());
    let pull = a_pull("stroppy-io/alpha", 12);
    let tree = mirrors.tree(&pull, &url).expect("a tree");
    assert_eq!(
        std::fs::read_to_string(tree.join("alpha.txt")).expect("the file"),
        "first\n"
    );

    // A push: the source's file moves and its `refs/pull/12/head` with it.
    std::fs::write(repo.join("alpha.txt"), "second\n").expect("the new file");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "second"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["update-ref", "refs/pull/12/head", &head]);

    let again = mirrors
        .tree(&pull, &url)
        .expect("the same tree, at the new head");
    assert_eq!(again, tree, "the tree a re-review lands in");
    assert_eq!(
        std::fs::read_to_string(tree.join("alpha.txt")).expect("the file"),
        "second\n"
    );
}

/// A mirror GitHub cannot be asked for is a refusal in words, not a panic: a row carries the reason
/// and the next pass tries again.
#[test]
fn a_repository_that_cannot_be_cloned_is_refused_in_words() {
    let dir = TempDir::new("no-repo");
    let mirrors = Mirrors::new(dir.0.join("github"));
    let why = mirrors
        .tree(
            &a_pull("stroppy-io/gone", 1),
            &format!("file://{}", dir.0.join("nowhere").display()),
        )
        .expect_err("a clone of nothing fails");
    assert!(why.contains("git"), "{why}");
    assert!(why.contains("stroppy-io-gone.git"), "{why}");
}
