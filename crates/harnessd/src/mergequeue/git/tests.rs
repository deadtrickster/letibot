//! The landing worktree's place, on temporary repositories.

use super::*;
use crate::mergequeue::serve::tests::{TempDir, branch, repo, sh};

/// **Outside `.git`, and reused.** The operator, 2026-10-11: *"the merges and branches shoudlnt
/// land into .git"*. The tree is made under the landing root, keyed by the repository; the second
/// call reuses it — its ignored build cache kept — and resets it clean.
#[test]
fn the_landing_worktree_is_made_outside_git_and_reused() {
    let dir = TempDir::new("landing-place");
    let repo = repo(&dir.0);
    let root = dir.0.join("landing");
    let tip = sh(&repo, &["rev-parse", "main"]);
    let wt = landing_worktree_in(&root, &repo, &tip).unwrap();
    assert!(wt.starts_with(&root), "{}", wt.display());
    assert!(!wt.starts_with(repo.join(".git")), "{}", wt.display());
    assert!(!repo.join(".git").join(OLD_LANDING_NAME).exists());
    let name = wt.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("repo-") && name.len() == "repo-".len() + 12,
        "{name}"
    );
    assert_eq!(sh(&wt, &["rev-parse", "HEAD"]), tip);
    // A worktree of the repository: the target's ref is the repository's own.
    assert!(sh(&repo, &["worktree", "list", "--porcelain"]).contains(&wt.display().to_string()));

    std::fs::create_dir_all(wt.join("target")).unwrap();
    std::fs::write(wt.join("target/cache"), "minutes\n").unwrap();
    std::fs::write(repo.join(".git/info/exclude"), "target/\n").unwrap();
    std::fs::write(wt.join("a.txt"), "dirtied by a gate\n").unwrap();
    std::fs::write(wt.join("stray"), "left by a gate\n").unwrap();
    let again = landing_worktree_in(&root, &repo, &tip).unwrap();
    assert_eq!(again, wt, "the same tree");
    assert_eq!(
        std::fs::read_to_string(wt.join("target/cache")).unwrap(),
        "minutes\n",
        "the build cache is kept"
    );
    assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "one\n");
    assert!(!wt.join("stray").exists(), "reset clean");

    // Any path of the repository names the same landing tree; another repository of the same
    // name does not.
    let (linked, _, _) = branch(&repo, "x", "x.txt");
    assert_eq!(landing_dir_in(&root, &linked), wt);
    let other = TempDir::new("landing-place");
    let other_repo = crate::mergequeue::serve::tests::repo(&other.0);
    assert_ne!(landing_dir_in(&root, &other_repo), wt);
}

/// **The migration**: an old `.git/letibot-landing` is removed the git way (and pruned) the first
/// time the new tree is made, and nothing else in `.git` is touched.
#[test]
fn an_old_landing_worktree_in_git_is_migrated_away() {
    let dir = TempDir::new("landing-migrate");
    let repo = repo(&dir.0);
    let old = repo.join(".git").join(OLD_LANDING_NAME);
    sh(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            &old.to_string_lossy(),
            "main",
        ],
    );
    std::fs::create_dir_all(old.join("target")).unwrap();
    std::fs::write(old.join("target/big"), "x").unwrap();
    std::fs::write(repo.join(".git/keep-me"), "the repository's own\n").unwrap();
    let tip = sh(&repo, &["rev-parse", "main"]);
    let root = dir.0.join("landing");
    let wt = landing_worktree_in(&root, &repo, &tip).unwrap();
    assert!(!old.exists(), "the old tree is gone");
    let list = sh(&repo, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains(OLD_LANDING_NAME), "and pruned: {list}");
    assert!(list.contains(&wt.display().to_string()), "{list}");
    assert_eq!(
        std::fs::read_to_string(repo.join(".git/keep-me")).unwrap(),
        "the repository's own\n"
    );
    assert_eq!(migrate_old_landing(&repo), None, "once");
}
