//! What a confined child may write in the repository it commits to. Read-write on the whole
//! common dir is what makes a commit possible, and it also let the child move `refs/heads/main`,
//! create `hooks/` where there was none, and point a checkout at a repository it controls.
//! Each test drives the write that was allowed, and the commit that must still work.

use std::path::{Path, PathBuf};
use std::time::Duration;

use letibot_tools::exec::confine::{self, Egress, HomeView, TmpView, ViewSpec};
use letibot_tools::exec::seat::{git_dir_protected, worktree_git_grants};
use letibot_tools::exec::{
    HostProcesses, JobId, JobState, ProcessHost, ScopeKind, SpawnRequest, Waited,
};

/// The skip every test in this file makes: a box with no boundary or no cgroup has nothing to
/// measure, and the skip is loud enough that `LETIBOT_REQUIRE_APPARATUS=1` can refuse it.
fn absent<T>(what: &str) -> Option<T> {
    letibot_tokencore::apparatus::absent(what);
    None
}

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A repository with `main` committed and one linked worktree on a branch of its own, which is
/// the shape a child is given: a checkout whose `.git` is a file and a common dir beside it.
struct Fixture {
    root: PathBuf,
    common: PathBuf,
    worktree: PathBuf,
    host: HostProcesses,
}

impl Fixture {
    fn new(name: &str) -> Option<Fixture> {
        let root =
            std::env::temp_dir().join(format!("letibot-gitchild-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let worktree = root.join("wt");
        std::fs::create_dir_all(&repo).expect("a fixture repository");
        git(&repo, &["init", "-q", "-b", "main", "."]);
        std::fs::write(repo.join("a.txt"), "hello\n").expect("a file");
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-q", "-m", "root"]);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "agent/child",
                worktree.to_str().unwrap(),
            ],
        );
        let common = repo.join(".git");

        let mut view = ViewSpec::project_only(&worktree)
            .with_home(HomeView::Tmpfs)
            .with_tmp(TmpView::Session(root.join("tmp")));
        for g in worktree_git_grants(&worktree) {
            view = view.granting(g);
        }
        for p in git_dir_protected(&worktree) {
            view = view.protecting(p);
        }
        let boundary = confine::probe(view, Egress::Denied);
        let confine = match boundary {
            Ok(c) => c,
            Err(e) => {
                eprintln!("git_child: no boundary on this host: {e}");
                return absent(
                    "a usable confinement boundary (bwrap and an unprivileged user namespace)",
                );
            }
        };
        let host = match HostProcesses::confined(&worktree, confine) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("git_child: no process tree on this host: {e}");
                return absent("a cgroup v2 subtree this user may write");
            }
        };
        Some(Fixture {
            root,
            common: common.canonicalize().unwrap_or(common),
            worktree,
            host,
        })
    }

    /// One confined command, answered with its output and exit code. The environment a commit
    /// needs is passed in, because the view's `$HOME` is a tmpfs and nothing sets an identity.
    fn run(&self, command: &str) -> (String, i32) {
        let id: JobId = self
            .host
            .spawn(&SpawnRequest {
                command: command.to_string(),
                slug: None,
                // Relative to the host's root, which is the worktree.
                cwd: ".".into(),
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                env: vec![
                    ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                    ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                    ("GIT_AUTHOR_NAME".into(), "child".into()),
                    ("GIT_AUTHOR_EMAIL".into(), "child@example.test".into()),
                    ("GIT_COMMITTER_NAME".into(), "child".into()),
                    ("GIT_COMMITTER_EMAIL".into(), "child@example.test".into()),
                ],
                tty: false,
                memory: None,
            })
            .expect("a confined run starts");
        let waited = self
            .host
            .wait_job(&id, Duration::from_secs(60))
            .expect("the run is waited on");
        let text = self
            .host
            .output(&id, 0, 1 << 20)
            .expect("the run's output")
            .text();
        // `-1` is not an exit status: a run a signal ended is not a run that answered, and the
        // assertions below are about what the command did, so the two must not read alike.
        let rc = match waited {
            Waited::Happened {
                state: Some(JobState::Exited { code }),
                ..
            } => code,
            _ => -1,
        };
        (text, rc)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The directory was not there, so bwrap bound nothing over it and the child made itself a hook
/// that runs in the operator's own git, outside any boundary.
#[test]
fn a_child_cannot_create_hooks_where_the_repository_has_none() {
    let Some(f) = Fixture::new("hooks") else {
        return;
    };
    // The state under test is a repository with no hooks directory: git's template and the
    // probe's mountpoint both leave one, so it is removed after both and before the run.
    let _ = std::fs::remove_dir_all(f.common.join("hooks"));
    assert!(
        !f.common.join("hooks").exists(),
        "the fixture must start with no hooks directory, or this measures nothing"
    );
    let (out, rc) = f.run(&format!(
        "mkdir {c}/hooks 2>&1 && echo MADE",
        c = f.common.display()
    ));
    assert_ne!(rc, 0, "a child created the hooks directory: {out}");
    assert!(!out.contains("MADE"), "{out}");
    assert!(
        !f.common.join("hooks").join("pre-commit").exists(),
        "and nothing of the child's is on disk where a hook would run"
    );
}

/// `refs/heads/main` is the branch the operator has out, and a child that can write the ref has
/// landed itself without a push.
#[test]
fn a_child_cannot_move_the_branch_the_repository_has_out() {
    let Some(f) = Fixture::new("refs") else {
        return;
    };
    let before = git(&f.root.join("repo"), &["rev-parse", "main"]);
    let (out, rc) = f.run(&format!(
        "git commit -q --allow-empty -m child; \
         echo moved > {c}/refs/heads/main 2>&1; \
         git update-ref refs/heads/main HEAD 2>&1; echo DONE",
        c = f.common.display()
    ));
    assert_ne!(rc, 0, "something above must have failed: {out}");
    assert!(!out.contains("DONE"), "the child ran to the end: {out}");
    let after = git(&f.root.join("repo"), &["rev-parse", "main"]);
    assert_eq!(
        before, after,
        "the branch the operator has out moved: {before} -> {after}"
    );
}

/// The other half: the commit a child is there to make still works, on its own branch, and no
/// other ref moved.
#[test]
fn a_child_still_commits_on_its_own_branch() {
    let Some(f) = Fixture::new("commit") else {
        return;
    };
    let repo = f.root.join("repo");
    let main_before = git(&repo, &["rev-parse", "main"]);
    let own_before = git(&repo, &["rev-parse", "agent/child"]);
    let (out, rc) =
        f.run("git commit -q --allow-empty -m 'from a child' && git log -1 --format=%an");
    assert_eq!(rc, 0, "the child's own commit failed: {out}");
    assert!(out.contains("child"), "the commit is the child's: {out}");
    let own_after = git(&repo, &["rev-parse", "agent/child"]);
    assert_ne!(own_before, own_after, "the child's branch did not move");
    assert_eq!(
        main_before,
        git(&repo, &["rev-parse", "main"]),
        "committing on its own branch must not move the one the operator has out"
    );
}

/// A path the protection list names but that does not exist is bound as an empty read-only
/// placeholder, so the name cannot be claimed at all: a `touch` of `packed-refs` is the write
/// that a read-only mount refuses and an absent path does not.
#[test]
fn an_absent_protected_file_cannot_be_created() {
    let Some(f) = Fixture::new("absent") else {
        return;
    };
    // The probe's mountpoint leaves an empty file here; the run must start without one.
    let _ = std::fs::remove_file(f.common.join("packed-refs"));
    assert!(
        !f.common.join("packed-refs").exists(),
        "the fixture must start with no packed-refs"
    );
    let (out, rc) = f.run(&format!(
        "echo x > {c}/packed-refs 2>&1 && echo WROTE",
        c = f.common.display()
    ));
    assert_ne!(rc, 0, "a child created packed-refs: {out}");
    assert!(!out.contains("WROTE"), "{out}");
}
