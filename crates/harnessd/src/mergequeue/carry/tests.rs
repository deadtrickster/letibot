//! `gate_carry` and the hint, on temporary repositories.

use std::path::{Path, PathBuf};

use super::*;
use crate::mergequeue::git::landing_worktree_in;
use crate::mergequeue::serve::tests::{TempDir, repo, sh};

/// A repository whose checkout has firecode's shape: `images/` and `vendor/` gitignored and
/// present, plus a `target/` the hint must never name.
fn firecode_like(tag: &str) -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new(tag);
    let repo = repo(&dir.0);
    std::fs::write(repo.join(".gitignore"), "images/\nvendor/\ntarget/\n").unwrap();
    sh(&repo, &["add", ".gitignore"]);
    sh(&repo, &["commit", "-q", "-m", "ignore the inputs"]);
    std::fs::create_dir_all(repo.join("images/rootfs")).unwrap();
    std::fs::write(repo.join("images/rootfs/kernel"), "vmlinux\n").unwrap();
    std::fs::write(repo.join("images/guest.ext4"), "ext4\n").unwrap();
    std::fs::create_dir_all(repo.join("vendor")).unwrap();
    std::fs::write(repo.join("vendor/firecracker"), "#!/bin/sh\n").unwrap();
    std::fs::create_dir_all(repo.join("target/debug")).unwrap();
    let root = dir.0.join("landing");
    let tip = sh(&repo, &["rev-parse", "main"]);
    let wt = landing_worktree_in(&root, &repo, &tip).unwrap();
    (dir, repo, wt)
}

#[test]
fn an_entry_is_a_path_inside_the_repository_and_anything_else_is_refused_by_name() {
    assert_eq!(validate("images").unwrap(), Path::new("images"));
    assert_eq!(validate("./vendor/fc/").unwrap(), Path::new("vendor/fc"));
    for (bad, why) in [
        ("/etc", "absolute"),
        ("../x", ".."),
        ("a/../../x", ".."),
        (".git", ".git"),
        (".git/hooks", ".git"),
        (".", "the repository itself"),
        ("", "empty"),
    ] {
        let e = validate(bad).unwrap_err();
        assert!(e.contains(&format!("\"{bad}\"")), "{bad}: {e}");
        assert!(e.contains(why), "{bad}: {e}");
    }
}

/// **The copy**: what the gate needs arrives in the landing tree as a copy — a gate that writes
/// there does not write into the operator's checkout — and a file gone from the checkout is gone
/// from the landing tree on the next run (`--delete`). Both copiers.
#[test]
fn gate_carry_copies_the_named_ignored_dirs_and_never_links_them() {
    for rsync in [true, false] {
        if rsync && !super::have_rsync() {
            continue;
        }
        let (_dir, repo, wt) = firecode_like(if rsync { "carry-rsync" } else { "carry-cp" });
        let entries = vec!["images".to_string(), "vendor".to_string()];
        carry_with(&repo, &wt, &entries, rsync).unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join("images/rootfs/kernel")).unwrap(),
            "vmlinux\n"
        );
        assert!(wt.join("vendor/firecracker").is_file());
        for e in ["images", "vendor"] {
            let m = std::fs::symlink_metadata(wt.join(e)).unwrap();
            assert!(m.is_dir() && !m.file_type().is_symlink(), "{e} is a copy");
        }
        // The gate scribbles in its copy; the checkout is untouched.
        std::fs::write(wt.join("images/guest.ext4"), "written by the gate\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join("images/guest.ext4")).unwrap(),
            "ext4\n"
        );
        // The next run: a removed file is removed, a changed one is the checkout's again.
        std::fs::remove_file(repo.join("vendor/firecracker")).unwrap();
        let tip = sh(&repo, &["rev-parse", "main"]);
        let wt = landing_worktree_in(&_dir.0.join("landing"), &repo, &tip).unwrap();
        assert!(
            wt.join("images/rootfs/kernel").exists(),
            "the reset keeps ignored paths (`clean -fd`), so rsync has nothing to re-copy"
        );
        carry_with(&repo, &wt, &entries, rsync).unwrap();
        assert!(!wt.join("vendor/firecracker").exists(), "rsync={rsync}");
        assert_eq!(
            std::fs::read_to_string(wt.join("images/guest.ext4")).unwrap(),
            "ext4\n",
            "rsync={rsync}"
        );
    }
}

#[test]
fn a_path_outside_missing_or_tracked_is_refused_by_name() {
    let (dir, repo, wt) = firecode_like("carry-refuse");
    // Out by its shape.
    let e = carry(&repo, &wt, &["../elsewhere".into()]).unwrap_err();
    assert!(e.contains("\"../elsewhere\""), "{e}");
    // Out by a symlink.
    let outside = dir.0.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, repo.join("escape")).unwrap();
    let e = carry(&repo, &wt, &["escape".into()]).unwrap_err();
    assert!(e.contains("\"escape\"") && e.contains("outside"), "{e}");
    // Missing.
    let e = carry(&repo, &wt, &["nothing-here".into()]).unwrap_err();
    assert!(
        e.contains("\"nothing-here\"") && e.contains("not in the checkout"),
        "{e}"
    );
    // Tracked: the branch's copy is what is gated.
    let e = carry(&repo, &wt, &["a.txt".into()]).unwrap_err();
    assert!(e.contains("\"a.txt\"") && e.contains("tracks"), "{e}");
}

/// **The notice's sentence** — the ignored dirs the checkout has and the landing tree lacks, by
/// name, never `target/`; nothing once they are carried, and nothing when there are none.
#[test]
fn the_hint_names_the_missing_ignored_dirs_only_when_nothing_is_carried() {
    let (_dir, repo, wt) = firecode_like("hint");
    let said = hint(&repo, &wt, &[]).expect("images and vendor are missing");
    assert_eq!(
        said,
        "The landing tree has no `images/`, `vendor/` (gitignored, present in your checkout); \
         if the gate needs them, set `[queue] gate_carry` in the project's `leticode.toml`."
    );
    assert!(!said.contains("target"), "{said}");
    assert_eq!(
        hint_in(&format!("gate failed\n\n{said}")),
        Some(said.as_str())
    );
    assert_eq!(
        hint(&repo, &wt, &["images".into()]),
        None,
        "carried: silent"
    );
    // Once the landing tree has them, there is nothing to say.
    carry(&repo, &wt, &["images".into(), "vendor".into()]).unwrap();
    assert_eq!(hint(&repo, &wt, &[]), None);

    let plain = TempDir::new("hint-none");
    let repo = crate::mergequeue::serve::tests::repo(&plain.0);
    let tip = sh(&repo, &["rev-parse", "main"]);
    let wt = landing_worktree_in(&plain.0.join("landing"), &repo, &tip).unwrap();
    assert_eq!(hint(&repo, &wt, &[]), None, "no ignored dirs: nothing said");
}
