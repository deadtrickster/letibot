//! What a command changed on disk, so a change made by a shell command or a `git merge` still
//! draws a diff. The old bytes of an already-dirty file must be read before the command runs;
//! one whose bytes were never read is not reported, because a wrong diff is worse than none.
//!
//! The tree is found from the directory the command runs in, not from the session's workspace: a
//! `cd <other-checkout> && …` writes into a repository the workspace may not be the root of.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The budget is the point, not the coverage: sized to be invisible on the common case, a handful
/// of files, and to give up on anything wider. A file that was clean before costs nothing, since
/// `HEAD` holds its old bytes; these caps bound the other half, a file already modified.
pub const MAX_PRE_READ: usize = 16;

/// And how much, in total.
pub const MAX_PRE_BYTES: usize = 2 << 20;

/// Past this many modified files the pre-read is skipped entirely rather than partially: a
/// broadly dirty tree is one where this is the wrong tool, and half a snapshot costs what a
/// whole one does without being useful. The clean-file half still works, because it costs nothing.
pub const WIDE_TREE: usize = 40;

/// What a file looked like before a command, for the files that could be known.
#[derive(Debug, Default, Clone)]
pub struct Before {
    /// Dirty files whose content was read in time, by repo-relative path.
    pub dirty: BTreeMap<String, String>,
    /// True when the directory was in a git work tree and the sweep can run at all.
    pub in_repo: bool,
    /// Every path git called modified, read or not. One `git status`, and it is what
    /// separates this command's doing from what was already there.
    pub was_dirty: std::collections::BTreeSet<String>,
    /// Dirty files that were NOT read, because the caps above were reached. A change to one of
    /// these is not reported at all: diffing it against `HEAD` would show somebody else's earlier
    /// edits as this command's.
    pub skipped: usize,
}

/// The git work tree `dir` is in, walked up from `dir` itself, or `None` when it is in none.
/// A shell command's `cd` is not visible to anybody but the shell, so the tree is read from the
/// directory the call starts in and from the one a leading `cd` names.
pub fn tree(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--show-toplevel"])?;
    let top = String::from_utf8_lossy(&out).trim().to_string();
    (!top.is_empty()).then(|| PathBuf::from(top))
}

/// One file this command changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub path: String,
    /// `None` when the file was created, or when its old content could not be
    /// known - see [`Before::skipped`].
    pub before: Option<String>,
    pub after: String,
    pub created: bool,
}

/// Snapshot what a sweep will need. Cheap when the tree is clean: one `git status`, and nothing
/// read. `root` is a work tree root, as [`tree`] gives it, so the paths git names are relative to
/// what they are joined onto.
pub fn before(root: &Path) -> Before {
    let Some(paths) = dirty_paths(root) else {
        return Before::default();
    };
    let mut out = Before {
        in_repo: true,
        ..Before::default()
    };
    // The set of modified paths is recorded whatever happens: one `git status`, and it is what
    // tells "this command changed it" from "already like that". The content read in time IS the
    // hash, so a rewrite that left the same bytes is not a change.
    let wide = paths.len() > WIDE_TREE;
    let mut bytes = 0usize;
    for p in paths {
        if wide || out.dirty.len() >= MAX_PRE_READ || bytes >= MAX_PRE_BYTES {
            out.was_dirty.insert(p);
            out.skipped += 1;
            continue;
        }
        match std::fs::read_to_string(root.join(&p)) {
            Ok(s) => {
                bytes += s.len();
                out.was_dirty.insert(p.clone());
                out.dirty.insert(p, s);
            }
            // Unreadable, binary, or gone: not a diff this can draw, so the path is
            // left out of the sweep rather than reported without one.
            Err(_) => {
                out.was_dirty.insert(p);
                out.skipped += 1;
            }
        }
    }
    out
}

/// Which paths this command changed: one `git status` and a read of the files whose old bytes are
/// already held, no diff, because the caller usually only wants the names. A path whose old bytes
/// were never read is left out: [`Before::skipped`].
pub fn changed_since(root: &Path, base: &Before) -> Vec<String> {
    if !base.in_repo {
        return Vec::new();
    }
    let Some(now) = dirty_paths(root) else {
        return Vec::new();
    };
    now.into_iter()
        .filter(|p| match base.dirty.get(p) {
            // Already dirty, and its old bytes are in hand, so the answer is exactly whether the
            // content moved: a rewrite that left the same bytes is not this command's change.
            Some(was) => std::fs::read_to_string(root.join(p)).is_ok_and(|now| &now != was),
            // Already dirty and over the budget: there is nothing to diff against, and the honest
            // answer is silence rather than a diff against `HEAD` that would blame earlier edits.
            None => !base.was_dirty.contains(p),
        })
        .collect()
}

/// One diff, materialised on demand. The expensive half: a read, and a `git show`
/// for a file that was clean. Called once per card the caller actually draws, never
/// once per changed file.
pub fn diff_of(root: &Path, base: &Before, path: &str) -> Option<Changed> {
    let after = std::fs::read_to_string(root.join(path)).ok()?;
    match base.dirty.get(path) {
        Some(was) => Some(Changed {
            path: path.to_string(),
            before: Some(was.clone()),
            after,
            created: false,
        }),
        None => {
            let head = show_head(root, path);
            Some(Changed {
                path: path.to_string(),
                created: head.is_none(),
                before: head,
                after,
            })
        }
    }
}

/// Repo-relative paths git reports as changed, or `None` when this is not a repo
/// (or git is not installed, which is the same answer to this question).
fn dirty_paths(root: &Path) -> Option<Vec<String>> {
    // `--porcelain=v2` is the stable format; `-z` is NUL-separated with no quoting, which is the
    // only form that survives a path with a space, a quote or a newline in it. Ignored paths are
    // not listed at all, which is what keeps a generated file out of the sweep.
    let out = git(
        root,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
    )?;
    let text = String::from_utf8_lossy(&out).into_owned();
    let fields: Vec<&str> = text.split('\0').collect();
    let mut paths = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let rec = fields[i];
        i += 1;
        let Some((kind, rest)) = rec.split_once(' ') else {
            continue;
        };
        // Every record but `?`/`!` starts with its two status columns, then a fixed number of
        // tokens, then the path - which may itself hold spaces, hence `splitn`'s last piece.
        let path = match kind {
            // `? <path>`: an untracked file, so the whole remainder is the path.
            "?" => rest,
            // `1 XY <sub> <mH> <mI> <mW> <hH> <hI> <path>`
            "1" => rest.splitn(8, ' ').nth(7).unwrap_or(""),
            // `u XY <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`
            "u" => rest.splitn(10, ' ').nth(9).unwrap_or(""),
            // `2 XY <sub> <mH> <mI> <mW> <hH> <hI> <score> <path>`, and the field AFTER the record
            // is the original path: the destination is what exists and what a diff is about.
            "2" => {
                i += 1;
                rest.splitn(9, ' ').nth(8).unwrap_or("")
            }
            _ => continue,
        };
        // A file git calls deleted is not on disk, so there is nothing to read and nothing to draw.
        let xy = rest.split(' ').next().unwrap_or("");
        if path.is_empty() || (kind != "?" && xy.contains('D')) {
            continue;
        }
        paths.push(path.to_string());
    }
    Some(paths)
}

/// `git -C <dir> <args>`, or `None` when git refused, is not installed, or `<dir>` is not in a repo.
fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

fn show_head(root: &Path, path: &str) -> Option<String> {
    let out = git(root, &["show", &format!("HEAD:{path}")])?;
    Some(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let out = crate::testing::git(root).args(args).output().expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn repo(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("letibot-detect-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        git(&root, &["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "first"]);
        root
    }

    /// A shell rewrote a file that was already modified. Git cannot supply what was
    /// never committed, so the pre-read is the only place the old bytes existed.
    #[test]
    fn a_change_to_an_already_dirty_file_still_has_both_sides() {
        let root = repo("dirty");
        std::fs::write(root.join("a.txt"), "one\nTWO\n").expect("write");
        let base = before(&root);
        assert!(base.in_repo);

        std::fs::write(root.join("a.txt"), "one\nTHREE\n").expect("write");
        let got = changed_since(&root, &base);
        assert_eq!(got, vec!["a.txt".to_string()], "the cheap phase names it");
        // The diff is computed only for the file a card is drawn for.
        let d = diff_of(&root, &base, "a.txt").expect("a diff");
        assert_eq!(d.before.as_deref(), Some("one\nTWO\n"));
        assert_eq!(d.after, "one\nTHREE\n");
        assert!(!d.created);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A clean file needs nothing recorded in advance: `HEAD` has its old bytes.
    #[test]
    fn a_change_to_a_clean_file_takes_its_before_from_head() {
        let root = repo("clean");
        let base = before(&root);
        assert!(base.dirty.is_empty(), "nothing to pre-read in a clean tree");

        std::fs::write(root.join("a.txt"), "one\nchanged\n").expect("write");
        let got = changed_since(&root, &base);
        assert_eq!(got, vec!["a.txt".to_string()]);
        let d = diff_of(&root, &base, "a.txt").expect("a diff");
        assert_eq!(d.before.as_deref(), Some("one\ntwo\n"), "from HEAD");
        assert!(!d.created);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A new file is a creation, not a diff against nothing.
    #[test]
    fn a_created_file_says_so() {
        let root = repo("created");
        let base = before(&root);
        std::fs::write(root.join("b.txt"), "new\n").expect("write");
        let got = changed_since(&root, &base);
        assert_eq!(got, vec!["b.txt".to_string()]);
        let d = diff_of(&root, &base, "b.txt").expect("a diff");
        assert!(d.created);
        assert_eq!(d.before, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A command that changed nothing reports nothing, however dirty the tree already was.
    /// Attributing somebody else's uncommitted work to this command is the failure this whole
    /// before/after shape exists to avoid.
    #[test]
    fn a_tree_that_was_already_dirty_is_not_this_commands_doing() {
        let root = repo("untouched");
        std::fs::write(root.join("a.txt"), "one\nedited by hand\n").expect("write");
        std::fs::write(root.join("c.txt"), "also mine\n").expect("write");
        let base = before(&root);

        // The command ran and touched nothing.
        assert!(changed_since(&root, &base).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Naming what changed does not read anything. Pinned by making the files unreadable
    /// after the command: the names still come back, and only the explicit `diff_of` fails.
    #[test]
    fn the_cheap_phase_computes_no_diffs() {
        let root = repo("lazy");
        let base = before(&root);
        for n in ["x.txt", "y.txt", "z.txt"] {
            std::fs::write(root.join(n), "new\n").expect("write");
        }
        let got = changed_since(&root, &base);
        assert_eq!(got.len(), 3, "{got:?}");

        // Now make every one of them unreadable. The names were already known
        // without reading, so they are unaffected; a diff is not.
        for n in ["x.txt", "y.txt", "z.txt"] {
            std::fs::remove_file(root.join(n)).expect("rm");
        }
        assert_eq!(
            changed_since(&root, &base).len(),
            0,
            "gone files are not changes"
        );
        assert!(
            diff_of(&root, &base, "x.txt").is_none(),
            "a diff needs the file"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A broadly dirty tree costs one `git status` and nothing else: past `WIDE_TREE` the
    /// pre-read is skipped whole, so a change to one of them is a miss rather than a wrong diff.
    #[test]
    fn a_wide_dirty_tree_is_given_up_on_rather_than_read() {
        let root = repo("wide");
        for i in 0..(WIDE_TREE + 5) {
            std::fs::write(root.join(format!("f{i}.txt")), "before\n").expect("write");
        }
        let base = before(&root);
        assert!(base.in_repo);
        assert!(
            base.dirty.is_empty(),
            "nothing was read: {}",
            base.dirty.len()
        );
        assert!(
            base.skipped > WIDE_TREE,
            "and it says how many it gave up on"
        );

        // A change to one of them is a miss, not a wrong diff.
        std::fs::write(root.join("f3.txt"), "after\n").expect("write");
        let got = changed_since(&root, &base);
        assert!(
            !got.iter().any(|p| p == "f3.txt"),
            "reported a file whose old bytes were never read: {got:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Outside a repo the sweep does not run, and says so by finding nothing
    /// rather than by guessing at mtimes.
    #[test]
    fn outside_a_repo_there_is_no_sweep() {
        let root = std::env::temp_dir().join(format!("letibot-detect-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        let base = before(&root);
        assert!(!base.in_repo);
        std::fs::write(root.join("x.txt"), "hello").expect("write");
        assert!(changed_since(&root, &base).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A command's `cd` is invisible from outside it, so the tree is found by walking up from the
    /// directory the call names: a session working in a subdirectory is still in its repository.
    #[test]
    fn a_directory_inside_a_repository_finds_its_tree() {
        let root = repo("subdir");
        std::fs::create_dir_all(root.join("fuzz/deep")).expect("mkdir");
        assert_eq!(
            tree(&root.join("fuzz/deep")).as_deref(),
            Some(root.as_path())
        );
        assert_eq!(tree(&root).as_deref(), Some(root.as_path()));
        // Outside one there is no tree to sweep, which is what switches the sweep off.
        let outside =
            std::env::temp_dir().join(format!("letibot-detect-not-a-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).expect("mkdir");
        assert!(
            tree(&outside).is_none(),
            "found a tree for a directory in none"
        );
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The status reader is `--porcelain=v2 -z`: a path may hold a space, a rename reports its
    /// destination and never its original, and a file git calls deleted is not a change.
    #[test]
    fn the_status_reader_names_the_paths_a_diff_can_be_drawn_for() {
        let root = repo("porcelain");
        // An untracked path with a space in it: one path, not a status column and a fragment.
        std::fs::write(root.join("with a space.txt"), "new\n").expect("write");
        let got = dirty_paths(&root).expect("a repo");
        assert!(
            got.iter().any(|p| p == "with a space.txt"),
            "an untracked path with a space in it is one path: {got:?}"
        );

        // A rename: the destination is what exists on disk, and it is what a diff is about.
        git(&root, &["mv", "a.txt", "renamed.txt"]);
        let got = dirty_paths(&root).expect("a repo");
        assert!(
            got.iter().any(|p| p == "renamed.txt"),
            "a rename's destination is a path: {got:?}"
        );
        assert!(
            !got.iter().any(|p| p == "a.txt"),
            "a rename's ORIGINAL path is not what exists: {got:?}"
        );

        // A deleted file has nothing to read and nothing to draw.
        std::fs::remove_file(root.join("renamed.txt")).expect("rm");
        let got = dirty_paths(&root).expect("a repo");
        assert!(
            !got.iter().any(|p| p == "renamed.txt" || p == "a.txt"),
            "a file git calls deleted is not a change: {got:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A rewrite that leaves the same bytes is not this command's change, and it is decided on the
    /// content: a stat would call any same-length rewrite inside one clock tick a change.
    #[test]
    fn a_rewrite_that_leaves_the_same_bytes_is_not_a_change() {
        let root = repo("same");
        std::fs::write(root.join("a.txt"), "one\nTWO\n").expect("write");
        let base = before(&root);
        // The same bytes, written again: exactly what a `python3 -c` round trip does.
        std::fs::write(root.join("a.txt"), "one\nTWO\n").expect("write");
        assert!(
            changed_since(&root, &base).is_empty(),
            "a no-op rewrite was reported as a change"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
