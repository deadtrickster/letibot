//! `scripts/migrate-notes.sh` against the daemon's own key: the notes it copies land in the
//! directory the `notes` tool reads, committed, and the project tree is left as it was.

use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/migrate-notes.sh")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = letibot_tools::testing::git(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn migrate(project: &Path, repo: &Path) -> std::process::Output {
    Command::new("bash")
        .arg(script())
        .arg(project)
        .arg("--repo")
        .arg(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("bash")
}

#[test]
fn the_migration_copies_tracked_and_untracked_notes_under_the_daemons_key() {
    let root = letibot_tools::backend::tempdir::TempDir::new();
    let project = root.path().join("proj");
    let notes = project.join(".letibot/notes");
    std::fs::create_dir_all(&notes).unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    std::fs::write(notes.join("kept.md"), "tracked and on disk\n").unwrap();
    std::fs::write(notes.join("gone.md"), "tracked, deleted from disk\n").unwrap();
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "notes"]);
    std::fs::remove_file(notes.join("gone.md")).unwrap();
    std::fs::write(notes.join("new.md"), "untracked\n").unwrap();
    let status_before = git(&project, &["status", "--porcelain"]);

    for origin in [None, Some("git@Example.invalid:o/Proj.git")] {
        if let Some(url) = origin {
            git(&project, &["remote", "add", "origin", url]);
        }
        let repo = root.path().join(format!("notes-{}", origin.is_some()));
        let out = migrate(&project, &repo);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let dest = repo.join(letibot_tools::notes_repo::project_key(&project));
        for (name, text) in [
            ("kept.md", "tracked and on disk\n"),
            ("gone.md", "tracked, deleted from disk\n"),
            ("new.md", "untracked\n"),
        ] {
            assert_eq!(
                std::fs::read_to_string(dest.join(name)).ok().as_deref(),
                Some(text),
                "{name} under {}",
                dest.display()
            );
        }
        assert_eq!(git(&repo, &["status", "--porcelain"]), "", "all committed");
        assert_eq!(git(&repo, &["branch", "--show-current"]), "main");
        assert_eq!(
            git(&repo, &["log", "--format=%s"]),
            "migrate notes from proj"
        );
        let again = migrate(&project, &repo);
        assert!(again.status.success());
        assert_eq!(git(&repo, &["rev-list", "--count", "HEAD"]), "1");
    }
    assert_eq!(
        git(&project, &["status", "--porcelain"]),
        status_before,
        "the project tree is not touched"
    );
}

/// Run after the commit that removed `.letibot/notes` from the tree, the migration still finds
/// the notes that commit removed.
#[test]
fn the_migration_finds_notes_a_later_commit_removed() {
    let root = letibot_tools::backend::tempdir::TempDir::new();
    let project = root.path().join("proj");
    let notes = project.join(".letibot/notes");
    std::fs::create_dir_all(&notes).unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    std::fs::write(notes.join("old.md"), "committed once\n").unwrap();
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "notes"]);
    git(&project, &["rm", "-rq", ".letibot"]);
    git(&project, &["commit", "-q", "-m", "notes out of the tree"]);
    assert!(!notes.exists());

    let repo = root.path().join("notes");
    let out = migrate(&project, &repo);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dest = repo.join(letibot_tools::notes_repo::project_key(&project));
    assert_eq!(
        std::fs::read_to_string(dest.join("old.md")).ok().as_deref(),
        Some("committed once\n")
    );
}
