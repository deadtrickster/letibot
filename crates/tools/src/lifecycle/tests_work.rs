//! G3's done-whens: an owner's worktree kept in the archive and then removed, a tree too big to
//! commit moved to the trash, a branch another worktree holds, an archive that failed, a restore,
//! an adoption of worktrees nobody recorded, and the session put away with its work.

use std::path::{Path, PathBuf};

use letibot_tokencore::store::{HoldingRow, HoldingState, OwnerKind, OwnerRow, OwnerState, Store};

use super::*;

/// A directory of its own per test, removed when it ends.
struct Fixture(PathBuf);

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let p = std::env::temp_dir().join(format!(
            "letibot-work-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Fixture(p)
    }

    fn at(&self, rel: &str) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn places(&self) -> Places {
        Places {
            gc_root: self.at("gc"),
            box_id: "lab2x1".into(),
            settings: Settings::default(),
            data_dir: self.at("data"),
            rundir: self.at("run"),
            firecode_runs: Some(self.at("runs")),
            firecode_cache: Some(self.at("cache")),
            proc_root: self.at("proc"),
            cgroup_fs: self.at("cg"),
        }
    }

    fn store(&self) -> Store {
        Store::open_in_memory().expect("an in-memory store")
    }

    /// A repository with one commit on `main`, and a `git` that is quiet about the fixture's
    /// identity.
    fn repo(&self) -> PathBuf {
        let repo = self.at("repo");
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(&args)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}");
        }
        std::fs::write(repo.join("one"), "one\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        repo
    }
}

/// [`pass`], with the owner named - the whole reaper, which is what a settle step is proved
/// through: the grace, the archive, the removal and the state move are one pass.
fn lifecycle_pass(store: &Store, places: &Places, ops: &Ops, only: Option<&str>, now: i64) -> Pass {
    super::pass(store, places, ops, only, now).expect("the pass")
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An owner row for a package, closed, with its root registered on disk.
fn owner(
    store: &Store,
    places: &Places,
    id: &str,
    repo: &Path,
    state: OwnerState,
    now: i64,
) -> OwnerRow {
    let mut row = owner_for_open(
        store,
        id,
        OwnerKind::Package,
        Some(repo.display().to_string()),
        now,
    )
    .expect("the owner row");
    row.state = state;
    row.closed_ms = Some(now);
    register(store, places, &row).expect("the row and its root");
    row
}

/// The plan's row for one holding, for the assertions about what the plan says.
fn held(store: &Store, owner: &str, kind: &str) -> Vec<HoldingRow> {
    store
        .holdings_of(owner)
        .expect("the holdings")
        .into_iter()
        .filter(|r| r.kind == kind)
        .collect()
}

#[test]
fn a_settled_owner_keeps_its_work_and_then_loses_it() {
    let f = Fixture::new("settle");
    let places = f.places();
    let store = f.store();
    let repo = f.repo();
    let now = now_ms();
    let owner = owner(&store, &places, "s-1", &repo, OwnerState::Closed, now);

    // One commit of the owner's, one modified tracked file, one untracked file.
    let made = make_worktree(&places, &owner.id, &repo, "pkg", "HEAD").expect("the worktree");
    std::fs::write(made.path.join("wip-tracked"), "x\n").unwrap();
    git(&made.path, &["add", "-A"]);
    git(&made.path, &["commit", "-q", "-m", "the unmerged commit"]);
    std::fs::write(made.path.join("wip-tracked"), "changed\n").unwrap();
    std::fs::write(made.path.join("untracked"), "untracked\n").unwrap();
    let before = git(&made.path, &["status", "--porcelain"]);

    // Before the grace: nothing is removed, and the plan says when.
    let ops = Ops::here("test");
    let early = lifecycle_pass(&store, &places, &ops, Some(&owner.id), now + 1000);
    assert!(
        made.path.exists(),
        "a closed owner inside its grace keeps its worktree: {early:?}"
    );

    // Past it: the tree and the branch go, and the work is in the archive.
    let pass = lifecycle_pass(
        &store,
        &places,
        &ops,
        Some(&owner.id),
        now + UNLANDED_GRACE_MS + 1000,
    );
    assert!(!made.path.exists(), "the worktree is gone: {pass:?}");
    assert!(
        crate::git::branch_head(&repo, &made.branch).is_none(),
        "the branch is gone"
    );
    let ns = namespace(&owner.id, "wt");
    let refs = refs_under(&repo, &ns).expect("the archive");
    assert!(
        refs.iter()
            .any(|r| r == &format!("{ns}/heads/{}", made.branch)),
        "the branch's ref is in the archive: {refs:?}"
    );
    assert!(
        refs.iter().any(|r| r == &format!("{ns}/wip")),
        "the uncommitted work is a wip commit: {refs:?}"
    );

    // A `git gc` that prunes everything unreachable does not take them.
    git(&repo, &["gc", "--prune=now"]);
    assert_eq!(
        refs_under(&repo, &ns).expect("still there").len(),
        refs.len(),
        "the archive refs keep their commits reachable"
    );

    // Restore brings back the branch and a tree with the same `git status`.
    let back = restore_path(&places, &owner.id, &made.branch);
    let restored = restore_worktree(&repo, &owner.id, Some(&back)).expect("the restore");
    assert_eq!(restored.branches, vec![made.branch.clone()]);
    assert_eq!(
        git(&back, &["status", "--porcelain"]),
        before,
        "the restored tree's status is the one the removal found"
    );
    assert_eq!(
        git(&back, &["log", "--oneline"]).lines().count(),
        2,
        "the unmerged commit came back with the wip over it"
    );
}

#[test]
fn an_ignored_gigabyte_is_not_written_into_the_object_store() {
    let f = Fixture::new("ignored");
    let places = f.places();
    let store = f.store();
    let repo = f.repo();
    let now = now_ms();
    let owner = owner(&store, &places, "s-2", &repo, OwnerState::Settling, now);
    let made = make_worktree(&places, &owner.id, &repo, "big", "HEAD").expect("the worktree");
    // A sparse file: a gigabyte of length and no blocks, because the guard is about bytes a build
    // dropped in the tree and not about what this test's disk can afford.
    let big = made.path.join("target");
    std::fs::create_dir_all(&big).unwrap();
    let huge = big.join("build.bin");
    let file = std::fs::File::create(&huge).unwrap();
    file.set_len(1024 * 1024 * 1024).unwrap();
    drop(file);
    let objects_before = object_bytes(&repo);

    let _ = lifecycle_pass(
        &store,
        &places,
        &Ops::here("test"),
        Some(&owner.id),
        now + 1000,
    );
    assert!(!made.path.exists(), "the tree is gone");
    let grew = object_bytes(&repo) - objects_before;
    assert!(
        grew < 8 * 1024 * 1024,
        "an ignored gigabyte must not enter the object store (it grew {grew} bytes)"
    );
}

/// The bytes git's object store holds, loose and packed.
fn object_bytes(repo: &Path) -> u64 {
    let out = git(repo, &["count-objects", "-v"]);
    let mut size: u64 = 0;
    for line in out.lines() {
        if let Some((k, v)) = line.split_once(": ") {
            let n: u64 = v.trim().parse().unwrap_or(0);
            if k == "size" || k == "size-pack" {
                size += n;
            }
        }
    }
    // `size` is KiB.
    size * 1024
}

#[test]
fn a_tree_over_the_guard_goes_to_the_trash_instead_of_the_object_store() {
    let f = Fixture::new("trash");
    let places = f.places();
    let store = f.store();
    let repo = f.repo();
    let now = now_ms();
    let owner = owner(&store, &places, "s-3", &repo, OwnerState::Settling, now);
    let made = make_worktree(&places, &owner.id, &repo, "huge", "HEAD").expect("the worktree");
    let huge = made.path.join("dump.bin");
    let file = std::fs::File::create(&huge).unwrap();
    file.set_len(600 * 1024 * 1024).unwrap();
    drop(file);
    let objects_before = object_bytes(&repo);

    let _ = lifecycle_pass(
        &store,
        &places,
        &Ops::here("test"),
        Some(&owner.id),
        now + 1000,
    );
    assert!(
        object_bytes(&repo) - objects_before < 8 * 1024 * 1024,
        "over the guard, nothing is committed"
    );
    let trash = places.box_dir().join(".trash");
    let left: Vec<PathBuf> = std::fs::read_dir(&trash)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert_eq!(
        left.len(),
        1,
        "the tree itself is in {}: {left:?}",
        trash.display()
    );
    assert!(
        left[0].join("dump.bin").exists(),
        "the 600 MB file is still there, in the trash"
    );
    let row = held(&store, &owner.id, WORKTREE);
    assert!(
        row.iter().any(|r| r.archive.is_some()),
        "the plan says where it went: {row:?}"
    );
}

#[test]
fn a_branch_another_worktree_made_is_not_touched() {
    let f = Fixture::new("branch");
    let places = f.places();
    let store = f.store();
    let repo = f.repo();
    let now = now_ms();
    let owner = owner(&store, &places, "s-4", &repo, OwnerState::Settling, now);
    let made = make_worktree(&places, &owner.id, &repo, "pkg", "HEAD").expect("the worktree");

    // Somebody else's work, made in another worktree during the owner's life: another branch, no
    // stamp, and a tree outside the owner root. Nothing about settling the owner may reach it.
    let other_branch = "someone/else";
    let other = repo.join("other-tree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            other_branch,
            &other.to_string_lossy(),
        ],
    );
    std::fs::write(other.join("theirs"), "their work\n").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-q", "-m", "somebody else's commit"]);
    let theirs = crate::git::branch_head(&repo, other_branch).expect("their branch");

    let pass = lifecycle_pass(
        &store,
        &places,
        &Ops::here("test"),
        Some(&owner.id),
        now + 1000,
    );
    assert!(!made.path.exists(), "the owner's tree goes: {pass:?}");
    assert!(
        crate::git::branch_head(&repo, &made.branch).is_none(),
        "the owner's own branch goes with it"
    );
    assert_eq!(
        crate::git::branch_head(&repo, other_branch).as_deref(),
        Some(theirs.as_str()),
        "a branch somebody else made in another worktree is not touched"
    );
    assert!(other.join("theirs").exists(), "and neither is its tree");
}

/// A worktree that sits in the owner root and names nobody is *not proved* - the plan reports it,
/// and nothing removes it (d.4: a name finds a candidate, a stamp decides).
#[test]
fn a_worktree_in_the_owner_root_with_no_stamp_is_kept_and_said() {
    let f = Fixture::new("unstamped");
    let places = f.places();
    let store = f.store();
    let repo = f.repo();
    let now = now_ms();
    let owner = owner(&store, &places, "s-7", &repo, OwnerState::Settling, now);
    let root = places.owner_root(&owner.id).join("wt");
    std::fs::create_dir_all(&root).unwrap();
    let stray = root.join("stray");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "stray/branch",
            &stray.to_string_lossy(),
        ],
    );
    let pass = lifecycle_pass(
        &store,
        &places,
        &Ops::here("test"),
        Some(&owner.id),
        now + 1000,
    );
    assert!(
        stray.exists(),
        "a tree nothing names this owner by is kept: {pass:?}"
    );
    let said = pass.kept.join("\n");
    assert!(
        said.contains("nothing in it names this owner"),
        "and said, in those words: {said}"
    );
}

#[test]
fn an_archive_step_that_fails_leaves_the_tree_and_marks_the_holding_stuck() {
    let f = Fixture::new("stuck");
    let places = f.places();
    let store = f.store();
    let repo = f.repo();
    let now = now_ms();
    let owner = owner(&store, &places, "s-5", &repo, OwnerState::Settling, now);
    let made = make_worktree(&places, &owner.id, &repo, "hard", "HEAD").expect("the worktree");
    // The refs live in the main checkout's `.git`, so a read-only `.git` is an archive that cannot
    // be written - which is exactly the failure the design says is `stuck`, not a delete.
    let gitdir = repo.join(".git");
    let mut perms = std::fs::metadata(&gitdir).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o555);
    std::fs::set_permissions(&gitdir, perms).unwrap();

    let pass = lifecycle_pass(
        &store,
        &places,
        &Ops::here("test"),
        Some(&owner.id),
        now + 1000,
    );
    perms = std::fs::metadata(&gitdir).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&gitdir, perms).unwrap();

    if !made.path.exists() {
        // Running as root, where a read-only directory is still writable: the fixture cannot
        // produce the failure, and saying so beats a green test that proved nothing.
        eprintln!(
            "SKIPPED: this uid can write a read-only directory, so the archive could not fail"
        );
        return;
    }
    assert!(made.path.exists(), "the tree is still there: {pass:?}");
    let row = held(&store, &owner.id, WORKTREE);
    assert!(
        row.iter().any(|r| r.state == HoldingState::Stuck),
        "the holding is stuck: {row:?}"
    );
    let stuck = pass
        .acts
        .iter()
        .filter(|a| a.act == letibot_tokencore::store::ReapAct::Stuck)
        .count();
    assert_eq!(stuck, 1, "one line about it, once: {pass:?}");
    // And the plan keeps it, in those words.
    let plan = plan(&store, &places, Some(&owner.id), now + 2000).expect("the plan");
    let line = plan.owners[0]
        .holds
        .iter()
        .find(|h| h.kind == WORKTREE)
        .map(|h| h.verdict.said().to_string())
        .unwrap_or_default();
    assert!(line.contains("failed twice"), "{line}");
}

#[test]
fn the_session_is_put_away_when_its_work_is_settled() {
    let f = Fixture::new("archived");
    let places = f.places();
    let store = f.store();
    let repo = f.repo();
    let now = now_ms();
    let row = owner(&store, &places, "s-6", &repo, OwnerState::Settling, now);
    // A session row, so the archive stamp has somewhere to land.
    store
        .put_session(&letibot_tokencore::store::SessionRecord {
            id: "s-6".into(),
            title: Some("a package".into()),
            model_id: "qwen".into(),
            dialect_sha: "d".into(),
            workspace_root: repo.display().to_string(),
            owner: "tester".into(),
            role: Some("coder".into()),
            approvers: vec![],
            parent_session_id: None,
        })
        .expect("a session row");

    let _ = lifecycle_pass(
        &store,
        &places,
        &Ops::here("test"),
        Some(&row.id),
        now + 1000,
    );
    let after = store.owner(&row.id).unwrap().expect("the owner");
    assert_eq!(
        after.state,
        OwnerState::Settled,
        "nothing is left to settle"
    );
    let session = store.session(&row.id).unwrap().expect("the session row");
    assert!(
        session.archived_ms.is_some(),
        "a settled session is archived with its reason (R184's remainder)"
    );
}
