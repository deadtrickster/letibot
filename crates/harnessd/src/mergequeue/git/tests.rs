//! The landing worktree's place, on temporary repositories.

use super::*;
use crate::mergequeue::serve::tests::{TempDir, branch, commit, repo, sh};

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

/// **The main checkout is never a worktree of itself, and never removed** — a row's `worktree`
/// column is text somebody wrote, and it is the only thing between a sweep and `rm -rf` of a
/// checkout. Another repository's tree is not this one's either.
#[test]
fn the_main_checkout_is_not_a_worktree_anybody_may_remove() {
    let dir = TempDir::new("rail");
    let repo = repo(&dir.0);
    assert!(
        !is_worktree_of(&repo, &repo),
        "the repository's own `.git` is a directory, not a link into `worktrees`"
    );
    let kept = remove_worktree(&repo, &repo, "agent/none").unwrap_err();
    assert!(kept.contains("is not a worktree of"), "{kept}");
    assert!(repo.join("a.txt").exists(), "nothing was removed");

    let other = TempDir::new("rail-other");
    let other_repo = crate::mergequeue::serve::tests::repo(&other.0);
    let (theirs, _, _) = branch(&other_repo, "theirs", "t.txt");
    assert!(
        !is_worktree_of(&theirs, &repo),
        "a tree of another repository is not this one's to remove"
    );

    let (mine, _, _) = branch(&repo, "mine", "m.txt");
    assert!(is_worktree_of(&mine, &repo));
    remove_worktree(&repo, &mine, "agent/mine").unwrap();
    assert!(!mine.exists(), "its own worktree goes");
}

/// **A tree git can no longer read is kept, not removed** — a `worktree add` whose bookkeeping was
/// pruned behind it leaves a directory git answers `fatal: not a git repository` for, and whether
/// it holds work cannot be told. Nothing is removed that cannot be checked, and the sentence says
/// which tree it was, so a person can look.
#[test]
fn a_worktree_git_no_longer_reads_is_kept_and_said() {
    let dir = TempDir::new("forgotten");
    let repo = repo(&dir.0);
    let (wt, _, _) = branch(&repo, "forgot", "f.txt");
    std::fs::create_dir_all(wt.join("target")).unwrap();
    std::fs::write(wt.join("target/cache"), "32 GB, in the story\n").unwrap();
    std::fs::remove_dir_all(repo.join(".git").join("worktrees")).unwrap();
    assert!(wt.exists(), "the directory is still there");
    let kept = remove_worktree(&repo, &wt, "agent/forgot").unwrap_err();
    assert!(kept.contains("not a git repository"), "{kept}");
    assert!(wt.exists(), "and nothing was removed from it");
    assert!(wt.join("target/cache").exists(), "the cache is still there");
}

/// **A tree with work in it is not removed, and the sentence names the file** — the branch may not
/// carry what was never committed.
#[test]
fn a_dirty_worktree_is_refused_by_name() {
    let dir = TempDir::new("refuse-dirty");
    let repo = repo(&dir.0);
    let (wt, _, _) = branch(&repo, "dirt", "d.txt");
    std::fs::write(wt.join("draft.md"), "not committed\n").unwrap();
    let kept = remove_worktree(&repo, &wt, "agent/dirt").unwrap_err();
    assert!(kept.contains("draft.md"), "{kept}");
    assert!(wt.exists());
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
    // The second call is a no-op, not a failure: a sweep runs on every pass, and the pass after a
    // release must not report one.
    retire_branch(&repo, "agent/logs", Some(&wt.to_string_lossy())).unwrap();

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

/// **A github url becomes the https one `gh`'s helper answers for**, whatever spelling it arrived
/// in — and another host is not translated. The forms below are the ones a clone, a `--push` url
/// and a forwarded agent actually write.
#[test]
fn a_github_url_becomes_the_https_one_gh_answers_for() {
    for (url, want) in [
        (
            "git@github.com:deadtrickster/letibot.git",
            Some("https://github.com/deadtrickster/letibot.git"),
        ),
        (
            "git@github.com:deadtrickster/letibot",
            Some("https://github.com/deadtrickster/letibot.git"),
        ),
        (
            "ssh://git@github.com/deadtrickster/letibot.git",
            Some("https://github.com/deadtrickster/letibot.git"),
        ),
        (
            "ssh://git@github.com:22/deadtrickster/letibot.git",
            Some("https://github.com/deadtrickster/letibot.git"),
        ),
        (
            "  git@github.com:o/r.git  ",
            Some("https://github.com/o/r.git"),
        ),
        // **An https remote is a github remote too**: it is not translated, but it is not
        // "nothing" either — which is what let an inherited helper answer it (see [`push`]).
        (
            "https://github.com/deadtrickster/letibot.git",
            Some("https://github.com/deadtrickster/letibot.git"),
        ),
        (
            "https://github.com/deadtrickster/letibot",
            Some("https://github.com/deadtrickster/letibot.git"),
        ),
        (
            "http://github.com/o/r.git",
            Some("https://github.com/o/r.git"),
        ),
        (
            "https://token@github.com/o/r.git",
            Some("https://github.com/o/r.git"),
        ),
        ("git@gitlab.com:o/r.git", None),
        ("https://gitlab.com/o/r.git", None),
        ("/Users/dead/Projects/letibot/letibot", None),
        ("git@github.com:", None),
    ] {
        assert_eq!(github_https(url).as_deref(), want, "{url}");
    }
}

// ===== A reviewer's clone: one tree per ATTEMPT =====

/// **Two clones of ONE row never share a path, and neither fails** — the class behind the operator's
/// four dead reviews of 2026-10-29 (see [`clone_branch`]). The helper used to `remove_dir_all`
/// whatever stood at `review-<row id>`, so a second attempt at a row deleted the tree the first
/// attempt's reviewer was reading: git died on its own `.git/objects/pack/tmp_pack_*` (*"No such
/// file or directory"*), and a reviewer already working there lost its workspace (*"the process did
/// not start"*, *"0 entries"*).
///
/// The property is not *who wins*: there is no contest. Both callers succeed, the two trees are
/// DIFFERENT paths, and each is complete — `rev-parse` answers the head the row names and `status`
/// is clean. Three rounds, because a race that has to be caught on the first try is a test that will
/// be green for the wrong reason.
#[test]
fn two_clones_of_one_row_never_share_a_path_and_neither_fails() {
    let dir = TempDir::new("clone-race");
    let repo = repo(&dir.0);
    let (_wt, _base, head) = branch(&repo, "the-work", "b.txt");
    let clones = dir.0.join("letibot-review-clones");
    let stem = clones.join("review-the-review");
    for round in 0..3 {
        let threads: Vec<_> = (0..2)
            .map(|n| {
                let (repo, stem, head) = (repo.clone(), stem.clone(), head.clone());
                std::thread::spawn(move || {
                    let said = clone_branch(&repo, "agent/the-work", &head, &stem);
                    (n, said)
                })
            })
            .collect();
        let mut trees = Vec::new();
        for t in threads {
            let (n, said) = t.join().expect("the clone thread");
            trees.push(said.unwrap_or_else(|e| panic!("clone {n} of round {round} failed: {e}")));
        }
        assert_ne!(
            trees[0], trees[1],
            "round {round}: two attempts never share one tree"
        );
        for tree in &trees {
            assert!(
                tree.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("review-the-review-"),
                "round {round}: the row's stem, then the attempt's own token: {}",
                tree.display()
            );
            assert_eq!(sh(tree, &["rev-parse", "HEAD"]), head, "round {round}");
            assert_eq!(
                sh(tree, &["status", "--porcelain"]),
                "",
                "round {round}: a complete clone, not a half-written one"
            );
            assert!(tree.join("b.txt").exists(), "round {round}");
        }
        // Nothing half-made is left beside them: the clone is renamed into place.
        let names: Vec<String> = std::fs::read_dir(&clones)
            .expect("the clone directory")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n.ends_with(".tmp")),
            "round {round}: no clone being made is left behind: {names:?}"
        );
    }
}

/// **A second attempt gets its OWN tree and leaves the first one alone** — a re-seat must not touch
/// what the previous reviewer is standing in, and the new reviewer must not inherit the files the
/// previous one left: both, and they are the same decision.
#[test]
fn a_second_clone_gets_its_own_tree_and_leaves_the_first_alone() {
    let dir = TempDir::new("clone-second");
    let repo = repo(&dir.0);
    let (_wt, _base, head) = branch(&repo, "the-work", "b.txt");
    let stem = dir
        .0
        .join("letibot-review-clones")
        .join("review-the-review");
    let first = clone_branch(&repo, "agent/the-work", &head, &stem).expect("the first clone");
    std::fs::write(first.join("scratch.txt"), "left by a reviewer\n").expect("a file in the tree");
    let second = clone_branch(&repo, "agent/the-work", &head, &stem).expect("the second clone");
    assert_ne!(first, second, "a second attempt is a second tree");
    assert!(
        first.join("scratch.txt").exists(),
        "the tree the first reviewer worked in is untouched: {}",
        first.display()
    );
    assert!(
        !second.join("scratch.txt").exists(),
        "and the second reviewer's tree is a fresh clone, not the first one's leftovers"
    );
    assert_eq!(sh(&second, &["rev-parse", "HEAD"]), head);
    assert_eq!(sh(&second, &["status", "--porcelain"]), "");
}

/// **A clone that cannot be made leaves NOTHING behind** — no tree, no `.tmp` for the sweep to have
/// to reason about, and nothing for the next attempt to trip over.
#[test]
fn a_clone_that_fails_leaves_no_tree_and_no_debris() {
    let dir = TempDir::new("clone-fails");
    let repo = repo(&dir.0);
    let (_wt, _base, head) = branch(&repo, "the-work", "b.txt");
    let clones = dir.0.join("letibot-review-clones");
    let stem = clones.join("review-the-review");
    let said = clone_branch(&repo, "agent/no-such-branch", &head, &stem)
        .expect_err("a branch that is not there cannot be cloned");
    assert!(
        said.contains("git clone of `agent/no-such-branch`"),
        "{said}"
    );
    let names: Vec<String> = std::fs::read_dir(&clones)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(names.is_empty(), "and no debris beside it: {names:?}");
}

// ===== The proof a deletion needs =====

/// **The proof is three-valued, and `None` from `rev_parse` is two of them.**
///
/// `branch_head` answers one `Option` for *the ref is there*, *the ref is not* and *this is not a
/// repository*, and the resource sweep read that `Option` as permission: a `None` meant *may go*,
/// and the worktree was removed with it. So the three facts are separated by `show-ref --verify`'s
/// own exit status, and only the middle one — a ref git says is not there — is permission.
#[test]
fn a_ref_git_cannot_read_is_not_a_ref_that_is_gone() {
    let dir = TempDir::new("proof-three");
    let repo = repo(&dir.0);
    let (_wt, _base, head) = branch(&repo, "prov", "p.txt");
    let what = "the branch `agent/prov`";

    // On the target: the branch was merged.
    sh(&repo, &["merge", "--ff-only", "agent/prov"]);
    assert_eq!(ref_proof(&repo, "agent/prov", "main", what), Proof::On);
    // Not on the target: one commit of its own.
    let (_wt2, _base2, head2) = branch(&repo, "prov2", "p2.txt");
    let what2 = "the branch `agent/prov2`";
    match ref_proof(&repo, "agent/prov2", "main", what2) {
        Proof::Unproven(kind, why) => {
            assert_eq!(kind, "carries");
            assert!(
                why.contains("carries 1 commit") && why.contains("agent/prov2"),
                "{why}"
            );
        }
        other => panic!("an unmerged branch is not permission: {other:?}"),
    }
    // Not there at all: permission, because it holds no commit.
    assert_eq!(
        ref_proof(&repo, "agent/never-existed", "main", what),
        Proof::Gone
    );
    // **A repository git cannot read is NOT `Gone`** — this is the one that used to take a tree with
    // it. `show-ref` exits 128 here and `rev-parse` answers `None`, and the two are different facts.
    let not_a_repo = dir.0.join("not-a-repository");
    std::fs::create_dir_all(&not_a_repo).unwrap();
    assert_eq!(
        branch_head(&not_a_repo, "agent/prov"),
        None,
        "the one-option answer"
    );
    match ref_proof(&not_a_repo, "agent/prov", "main", what) {
        Proof::Unproven(kind, why) => {
            assert_eq!(kind, "unreadable");
            assert!(why.contains("could not be told"), "{why}");
        }
        other => panic!("a repository git cannot read is not permission: {other:?}"),
    }
    // And the same proof about a worktree's own HEAD, which is what `remove_worktree` is
    // unrecoverable for: this one is on the target, the unmerged one is not.
    assert_eq!(
        commit_proof(&repo, &head, "main", "the worktree's HEAD"),
        Proof::On
    );
    assert!(matches!(
        commit_proof(&repo, &head2, "main", "the worktree's HEAD"),
        Proof::Unproven("carries", _)
    ));
    // A tree git cannot read is not a tree with no commits in it.
    assert!(head_of(&not_a_repo).is_err());
}

// ===== The replay: the branch's own commits, and nothing else =====

/// **A rewritten target must not be replayed onto itself.** `main` is rebuilt from an older commit,
/// so `merge-base(branch, main)` is that older commit — a commit on the OLD main — and a plain
/// `git rebase main` then replays the old main's two commits as well as the branch's one (and
/// conflicts in `a.txt`, which the branch never touched). The replay is `--onto <main> <the fork>`,
/// so exactly one commit is replayed and `range-diff` says the patch is unchanged.
#[test]
fn a_branch_with_one_commit_on_a_rewritten_main_replays_its_one() {
    let dir = TempDir::new("rebase-rewritten");
    let repo = repo(&dir.0);
    commit(&repo, "a.txt", "old main one\n", "old main: a.txt says one");
    let old_tip = commit(&repo, "a.txt", "old main two\n", "old main: a.txt says two");
    // The branch: cut from the tip as it was, one commit, in a file of its own.
    let (_wt, base, head) = branch(&repo, "one", "one.txt");
    assert_eq!(base, old_tip, "cut from the tip as it was");
    // The rewrite: main is rebuilt from the commit under the old main's two, and says something
    // else in `a.txt`, so neither old commit is on it any more.
    let root = sh(&repo, &["rev-parse", "HEAD~2"]);
    sh(&repo, &["reset", "--hard", &root]);
    let new_tip = commit(
        &repo,
        "a.txt",
        "rewritten\n",
        "rewritten main: a.txt says rewritten",
    );
    assert!(
        !is_ancestor(&repo, &old_tip, &new_tip).unwrap(),
        "the old tip is nobody's ancestor now"
    );
    let lwt = landing_worktree_in(&dir.0.join("landing"), &repo, &new_tip).unwrap();
    // The base the row recorded, as the review row carries it.
    let rebased = rebase_in(
        &lwt,
        &head,
        &new_tip,
        "main",
        &[String::new(), base.clone()],
    )
    .expect("the branch's own commit replays");
    assert_eq!(
        commits_over(&lwt, &new_tip, &rebased),
        Ok(1),
        "one commit, not the old main's two as well"
    );
    assert_eq!(
        sh(
            &lwt,
            &["log", "--format=%s", &format!("{new_tip}..{rebased}")]
        ),
        "work on one"
    );
    assert_eq!(
        std::fs::read_to_string(lwt.join("a.txt")).unwrap(),
        "rewritten\n",
        "the branch did not resurrect the old main's line"
    );
    assert_eq!(
        std::fs::read_to_string(lwt.join("one.txt")).unwrap(),
        "one\n"
    );
    // **The patches are the branch's, before and after.**
    let rd = sh(
        &lwt,
        &[
            "range-diff",
            "--no-color",
            &format!("{base}..{head}"),
            &format!("{new_tip}..{rebased}"),
        ],
    );
    assert!(
        !rd.is_empty() && rd.lines().all(|l| l.split_whitespace().nth(2) == Some("=")),
        "range-diff must say every patch is the same: {rd}"
    );
    assert!(
        patches_unchanged(&lwt, (&base, &head), (&new_tip, &rebased)).is_ok(),
        "the code path agrees"
    );
}

/// **The reflog alone finds the base once nothing else does** — the same branch with no recorded
/// base: `main`'s reflog still names the tip it used to point at, which is the fork.
#[test]
fn a_rewritten_main_is_asked_for_the_tip_it_used_to_point_at() {
    let dir = TempDir::new("rebase-reflog");
    let repo = repo(&dir.0);
    let old_tip = commit(&repo, "a.txt", "old main\n", "old main: a.txt says old");
    let (_wt, base, head) = branch(&repo, "log", "log.txt");
    assert_eq!(base, old_tip);
    let root = sh(&repo, &["rev-parse", "HEAD~1"]);
    sh(&repo, &["reset", "--hard", &root]);
    let new_tip = commit(&repo, "a.txt", "rewritten\n", "rewritten main");
    let lwt = landing_worktree_in(&dir.0.join("landing"), &repo, &new_tip).unwrap();
    // Nothing recorded: the row has no `onto_sha` and the review no `base_sha`.
    let rebased = rebase_in(&lwt, &head, &new_tip, "main", &[]).expect("the reflog names the base");
    assert_eq!(commits_over(&lwt, &new_tip, &rebased), Ok(1));
}

/// **A branch that merged the old main into itself replays its own three commits and no others.**
/// Its first-parent spine is `B3, merge, B2, merge, B1, fork, …`, and the commits those merges
/// brought in are *not* the branch's: `git rebase --onto <main> <fork>` would replay them too and
/// conflict in `m.txt`, so the branch's own list is replayed one commit at a time.
#[test]
fn a_branch_that_merged_the_old_main_in_replays_only_its_three_commits() {
    let dir = TempDir::new("rebase-merged");
    let repo = repo(&dir.0);
    let root = sh(&repo, &["rev-parse", "main"]);
    commit(&repo, "m.txt", "1\n", "old main 1");
    let (wt, base, _head) = branch(&repo, "three", "b1.txt");
    commit(&repo, "m.txt", "1\n2\n", "old main 2");
    sh(&wt, &["merge", "--no-edit", "main"]);
    commit(&wt, "b2.txt", "b2\n", "B2");
    commit(&repo, "m.txt", "1\n2\n3\n", "old main 3");
    sh(&wt, &["merge", "--no-edit", "main"]);
    let head = commit(&wt, "b3.txt", "b3\n", "B3");
    // The rewrite: main starts again from the base, and `m.txt` says something of its own.
    sh(&repo, &["reset", "--hard", &root]);
    let new_tip = commit(&repo, "m.txt", "rewritten\n", "rewritten main");
    let lwt = landing_worktree_in(&dir.0.join("landing"), &repo, &new_tip).unwrap();
    let rebased = rebase_in(
        &lwt,
        &head,
        &new_tip,
        "main",
        &[String::new(), base.clone()],
    )
    .expect("its own three replay");
    assert_eq!(
        commits_over(&lwt, &new_tip, &rebased),
        Ok(3),
        "three, not the old main's commits the merges brought in"
    );
    assert_eq!(
        sh(
            &lwt,
            &["log", "--format=%s", &format!("{new_tip}..{rebased}")]
        ),
        "B3\nB2\nwork on three"
    );
    // **range-diff says the branch's patches are the same.** The only patches it does not find on
    // the other side are the old main's, which came in with the merges and are not the branch's to
    // replay; every patch it does find is `=`, and none was invented.
    let rd = sh(
        &lwt,
        &[
            "range-diff",
            "--no-color",
            &format!("{base}..{head}"),
            &format!("{new_tip}..{rebased}"),
        ],
    );
    let marks: Vec<Option<&str>> = rd.lines().map(|l| l.split_whitespace().nth(2)).collect();
    assert!(
        marks.iter().all(|m| matches!(m, Some("=") | Some("<"))),
        "no patch changed and none was added: {rd}"
    );
    assert_eq!(
        marks.iter().filter(|m| **m == Some("=")).count(),
        3,
        "the branch's three patches are the same: {rd}"
    );
    assert!(patches_unchanged(&lwt, (&base, &head), (&new_tip, &rebased)).is_ok());
}

/// **A base nobody remembers is said, not guessed.** With the row recording nothing and the
/// target's reflog gone, there is no way to tell the branch's own commit from the ones it sits on —
/// so the row gets a sentence a person can act on instead of a conflict, and the landing tree is
/// left where it was rather than half-rebased.
#[test]
fn a_branch_whose_base_nobody_remembers_is_refused_in_words() {
    let dir = TempDir::new("rebase-lost-base");
    let repo = repo(&dir.0);
    let (_wt, base, head) = branch(&repo, "lost", "lost.txt");
    let root = sh(&repo, &["rev-parse", "main"]);
    sh(&repo, &["reset", "--hard", &root]);
    let new_tip = commit(&repo, "a.txt", "rewritten\n", "rewritten main");
    let lwt = landing_worktree_in(&dir.0.join("landing"), &repo, &new_tip).unwrap();
    // The reflog is the only thing that could still name the base, and this repository has none.
    std::fs::remove_dir_all(repo.join(".git/logs")).unwrap();
    let e = rebase_in(&lwt, &head, &new_tip, "main", &[]).expect_err("no base, no guess");
    let said = e.sentence("agent/lost", "main", &new_tip);
    assert!(
        said.contains("could not find the branch's own base"),
        "{said}"
    );
    assert!(said.contains("rebase it by hand"), "{said}");
    assert!(!said.contains("does not rebase"), "not a conflict: {said}");
    assert_eq!(sh(&lwt, &["rev-parse", "HEAD"]), new_tip, "nothing ran");
    assert_eq!(sh(&lwt, &["status", "--porcelain"]), "", "and no debris");
    let _ = base;
/// **A github remote is pushed on gh's helper BY NAME, in both forms** — the inherited helper list
/// cleared first, because an empty `credential.helper` is what makes the named one the one asked
/// (MEASURED: without it osxkeychain answers and gh's helper is never called). A remote that is not
/// github, or a `gh` that is not logged in, keeps git's own argv and names no helper.
#[test]
fn a_github_push_names_ghs_helper_and_clears_the_inherited_ones() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let refspec = format!("{sha}:refs/heads/main");
    for url in [
        "git@github.com:o/r.git",
        "ssh://git@github.com/o/r.git",
        "https://github.com/o/r.git",
        "https://github.com/o/r",
    ] {
        let plan = push_plan(url, "main", sha, true);
        assert_eq!(
            plan.args,
            [
                "-c",
                "credential.helper=",
                "-c",
                "credential.helper=!gh auth git-credential",
                "push",
                "https://github.com/o/r.git",
                refspec.as_str(),
            ],
            "{url}"
        );
        assert!(plan.gh, "{url}");
    }

    // The failure row a person reads keeps the helper as ONE word, not three.
    let plan = push_plan("git@github.com:o/r.git", "main", sha, true);
    let rendered: Vec<&str> = plan.args.iter().map(String::as_str).collect();
    assert!(
        words(&rendered).contains("-c 'credential.helper=!gh auth git-credential'"),
        "{}",
        words(&rendered)
    );

    for (url, gh) in [
        ("git@github.com:o/r.git", false),
        ("https://github.com/o/r.git", false),
        ("git@gitlab.com:o/r.git", true),
        ("/srv/git/o/r.git", true),
    ] {
        let plan = push_plan(url, "main", sha, gh);
        assert_eq!(
            plan.args,
            ["push", "origin", refspec.as_str()],
            "{url} gh={gh}"
        );
        assert!(!plan.gh, "{url} gh={gh}");
    }
}

/// **The row's sentence says whose credential answered** — a push on gh's credential reads
/// differently from one on git's own, and a refusal keeps git's words AND the fact that gh
/// answered, so "no access for this account" is not read as "no credential at all".
#[test]
fn the_pushes_sentence_says_whose_credential_answered() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let on_gh = push_plan("git@github.com:o/r.git", "main", sha, true);
    let on_git = push_plan("git@github.com:o/r.git", "main", sha, false);

    let gh = pushed_said(sha, "main", &on_gh);
    let own = pushed_said(sha, "main", &on_git);
    assert!(gh.contains("gh's credential"), "{gh}");
    assert!(gh.contains("!gh auth git-credential"), "{gh}");
    assert!(!own.contains("gh"), "{own}");
    assert!(own.contains("git's own config"), "{own}");
    assert_ne!(gh, own);

    let refused = failed_said("git push origin main: remote: Permission denied", &on_gh);
    assert!(
        refused.contains("remote: Permission denied"),
        "git's own words are kept: {refused}"
    );
    assert!(refused.contains("gh's"), "{refused}");
    assert!(refused.contains("not a missing credential"), "{refused}");
    assert_eq!(
        failed_said("remote: Permission denied", &on_git),
        "remote: Permission denied",
        "a plain git push's refusal is git's words, unchanged"
    );
}

/// **The old push still runs**: a remote that is not github goes to `origin` on git's own
/// credential and the sentence says so — and a repository with no `origin` says that instead.
/// A local bare repository, so nothing leaves the box.
#[test]
fn a_remote_that_is_not_github_is_pushed_to_origin() {
    let dir = TempDir::new("push-plain");
    let repo = repo(&dir.0);
    let bare = dir.0.join("origin.git");
    sh(
        &dir.0,
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    sh(&repo, &["remote", "add", "origin", &bare.to_string_lossy()]);
    let sha = sh(&repo, &["rev-parse", "main"]);
    match push(&repo, "main", &sha) {
        Pushed::Yes(s) => {
            assert!(s.contains("git's own config"), "{s}");
            assert!(!s.contains("gh"), "{s}");
            assert_eq!(
                sh(&bare, &["rev-parse", "main"]),
                sha,
                "the branch is there"
            );
        }
        other => panic!("{other:?}"),
    }

    sh(&repo, &["remote", "remove", "origin"]);
    assert_eq!(push(&repo, "main", &sha), Pushed::NoRemote);
}
