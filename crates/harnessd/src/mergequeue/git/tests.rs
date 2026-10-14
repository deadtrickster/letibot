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

/// **No test makes anything in the box's real `~/.local/share/letibot`.** A fixture's config has
/// no landing root, a unit test cannot resolve the real one, and landing or granting under it
/// fails the test.
#[test]
fn a_test_never_reaches_the_real_landing_root() {
    assert_eq!(
        crate::config::Config::for_this_box("/tmp").landing_root,
        None,
        "a fixture lands nowhere real"
    );
    if std::env::var_os(LANDING_ROOT_ENV).is_none() {
        assert!(
            landing_root().is_err(),
            "a unit test has no real landing root"
        );
    }
    let Some(real) = letibot_data_dir() else {
        return;
    };
    let caught = std::panic::catch_unwind(|| guard_not_real(&real.join("landing")));
    assert!(caught.is_err(), "the guard fails a test that reaches it");
    guard_not_real(std::path::Path::new("/tmp/elsewhere"));
}

/// **The daemon's own leftover logs do not keep a landed branch's tree** (R92.4): `.letibot/*.jsonl`
/// that older builds wrote into the workspace are not the child's work; a note it wrote is.
#[test]
fn a_tree_holding_only_the_daemons_logs_is_retired_and_one_with_work_is_kept() {
    let dir = TempDir::new("retire-leftovers");
    let repo = repo(&dir.0);
    let (wt, _, _) = branch(&repo, "logs", "l.txt");
    std::fs::create_dir_all(wt.join(".letibot")).unwrap();
    for f in [
        "delivery-log.jsonl",
        "context-claims.jsonl",
        "notes-offers.jsonl",
    ] {
        std::fs::write(wt.join(".letibot").join(f), "{}\n").unwrap();
    }
    assert!(uncommitted_work(&wt).unwrap().is_empty());
    retire_branch(&repo, "agent/logs", Some(&wt.to_string_lossy())).unwrap();
    assert!(!wt.exists());

    let (wt, _, _) = branch(&repo, "notes", "n.txt");
    std::fs::create_dir_all(wt.join(".letibot/notes")).unwrap();
    std::fs::write(wt.join(".letibot/notes/a.md"), "mine\n").unwrap();
    std::fs::write(wt.join(".letibot/delivery-log.jsonl"), "{}\n").unwrap();
    let kept = retire_branch(&repo, "agent/notes", Some(&wt.to_string_lossy())).unwrap_err();
    assert!(kept.contains(".letibot/notes/a.md"), "{kept}");
    assert!(!kept.contains("delivery-log"), "{kept}");
    assert!(wt.exists());
}

#[test]
fn only_jsonl_directly_under_letibot_is_a_daemon_leftover() {
    assert!(daemon_leftover(".letibot/delivery-log.jsonl"));
    assert!(daemon_leftover("\".letibot/x.jsonl\""));
    assert!(!daemon_leftover(".letibot/notes/a.jsonl"));
    assert!(!daemon_leftover(".letibot/notes/a.md"));
    assert!(!daemon_leftover("src/x.jsonl"));
}

/// **One host, one owner, whatever the URL's spelling.**
#[test]
fn a_remote_names_its_host_and_owner() {
    let want = Some(("github.com".to_string(), "stroppy-io".to_string()));
    assert_eq!(
        remote_owner("https://github.com/stroppy-io/stroppy.git"),
        want
    );
    assert_eq!(remote_owner("git@github.com:stroppy-io/stroppy.git"), want);
    assert_eq!(
        remote_owner("ssh://git@github.com/Stroppy-IO/website"),
        want
    );
    assert_eq!(remote_owner("/srv/git/stroppy.git"), None);
    assert_eq!(
        remote_owner("https://github.com/stroppy-io"),
        None,
        "no repository segment"
    );
}

/// **A folder of one org's repositories is one project; a folder of unrelated ones is not.**
#[test]
fn a_folder_is_one_project_when_its_repositories_share_an_owner() {
    let root = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-repos-of-{}-{}",
        std::process::id(),
        letibot_sessionlog::registry::now_ms()
    ));
    let repo = |dir: &Path, url: Option<&str>| {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]).unwrap();
        if let Some(u) = url {
            git(dir, &["remote", "add", "origin", u]).unwrap();
        }
    };
    let org = root.join("stroppy-io");
    repo(
        &org.join("stroppy"),
        Some("git@github.com:stroppy-io/stroppy.git"),
    );
    repo(
        &org.join("website"),
        Some("https://github.com/stroppy-io/stroppy-io.github.io"),
    );
    let got: Vec<String> = repos_of_workspace(&org)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(got, ["stroppy", "website"]);

    let mixed = root.join("Projects");
    repo(
        &mixed.join("peerdb"),
        Some("https://github.com/peerdb-io/peerdb.git"),
    );
    repo(
        &mixed.join("letim"),
        Some("https://github.com/deadtrickster/letim.git"),
    );
    assert!(
        repos_of_workspace(&mixed).is_empty(),
        "mixed owners are no project"
    );

    let bare = root.join("scratch");
    repo(&bare.join("a"), None);
    repo(&bare.join("b"), None);
    assert!(
        repos_of_workspace(&bare).is_empty(),
        "no remote, no owner, no project"
    );
    let _ = std::fs::remove_dir_all(&root);
}
