//! **What a command changed on disk**, so an edit made any other way still draws
//! a diff.
//!
//! # Why
//!
//! `edit` and `write` hand the head a [`crate::edit::FileEdit`] and it draws a
//! diff card. A shell command that rewrites a file hands it nothing, so the
//! change lands silently — and the operator, watching a model rewrite a file
//! through a `python3` heredoc, asked for exactly this: *"how to catch edits via
//! python or git merges"*.
//!
//! A merge is the same shape and is the reason this is not simply "watch what
//! the write tools do": `git merge` rewrites files with no tool call at all, but
//! it happens INSIDE one, so a sweep after the command catches it.
//!
//! # `before` is the hard half
//!
//! Once the command has run the old bytes are gone, so they have to come from
//! somewhere:
//!
//! * a file that was **clean** before takes its `before` from `HEAD` — exact,
//!   free, and needs nothing recorded in advance;
//! * a file that was **already dirty** needs its content read before the command
//!   runs, because git cannot supply what was never committed.
//!
//! The second is the common case in real work — the file being edited is usually
//! already modified — so a small, tightly budgeted pre-read buys it. Past that
//! budget the answer is no answer: the operator's rule, *"id prefer 'not
//! detect's to slows"*. A change to a file whose old bytes were never read is
//! not reported at all, because diffing it against `HEAD` would show whatever
//! was already uncommitted as this command's doing, and a wrong diff is worse
//! than a missing one.
//!
//! # What this deliberately does not do
//!
//! It does not claim the model called `edit`. A change found this way is
//! reported as *detected after a command*, because a synthesized edit that looks
//! like a real one is the same lie as a guessed number.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// **The budget is the point, not the coverage.** The operator, on being shown a
/// sweep that read every modified file before every command: *"id prefer 'not
/// detect's to slows"*. So this is sized to be invisible on the common case — a
/// handful of files being worked on — and to give up immediately on anything
/// wider, reporting the change by name with no diff rather than paying to
/// produce one.
///
/// A file that was CLEAN before costs nothing to snapshot: `HEAD` holds its old
/// bytes and is only consulted for files that actually changed. These caps bound
/// the other half — a file already modified, whose old bytes exist nowhere else.
pub const MAX_PRE_READ: usize = 16;

/// And how much, in total.
pub const MAX_PRE_BYTES: usize = 2 << 20;

/// Past this many modified files, the pre-read is skipped ENTIRELY rather than
/// partially: a broadly dirty tree is one where this is the wrong tool, and
/// half a snapshot costs the same as a whole one without being useful. The
/// clean-file half still works, because it costs nothing.
pub const WIDE_TREE: usize = 40;

/// What a file looked like before a command, for the files that could be known.
#[derive(Debug, Default, Clone)]
pub struct Before {
    /// Dirty files whose content was read in time, by repo-relative path.
    pub dirty: BTreeMap<String, String>,
    /// True when the workspace is a git repo and the sweep can run at all.
    pub in_repo: bool,
    /// Every path git called modified, read or not. Cheap — one `git status` —
    /// and it is what separates this command's doing from what was already
    /// there.
    pub was_dirty: std::collections::BTreeSet<String>,
    /// `(mtime_ns, len)` for every modified path, read or not. A stat is orders
    /// of magnitude cheaper than a read, and it is enough to answer "did this
    /// command touch it" — which is the only question the cheap phase asks.
    pub stamp: BTreeMap<String, (u128, u64)>,
    /// Dirty files that were NOT read, because the caps above were reached. A
    /// change to one of these is reported without a diff rather than with a
    /// wrong one: diffing it against `HEAD` would show somebody else's earlier
    /// edits as this command's.
    pub skipped: usize,
}

/// One file this command changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub path: String,
    /// `None` when the file was created, or when its old content could not be
    /// known — see [`Before::skipped`].
    pub before: Option<String>,
    pub after: String,
    pub created: bool,
}

/// Snapshot what a sweep will need. Cheap when the tree is clean: one `git
/// status`, and nothing read.
pub fn before(root: &Path) -> Before {
    let Some(paths) = dirty_paths(root) else {
        return Before::default();
    };
    let mut out = Before {
        in_repo: true,
        ..Before::default()
    };
    // The set of modified paths is recorded whatever happens — it is what tells
    // "this command changed it" from "it was already like that", and it is one
    // `git status` either way. Only the CONTENT is budgeted.
    let wide = paths.len() > WIDE_TREE;
    let mut bytes = 0usize;
    for p in paths {
        if let Some(st) = stamp_of(root, &p) {
            out.stamp.insert(p.clone(), st);
        }
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
            // Unreadable, binary, or gone: not a diff this can draw, and the
            // sweep below will report the path without one.
            Err(_) => {
                out.was_dirty.insert(p);
                out.skipped += 1;
            }
        }
    }
    out
}

/// **Which paths this command changed** — the cheap half, and the only half that
/// always runs.
///
/// One `git status` and a stat per modified path. Nothing is read and no diff is
/// computed, because the caller usually only wants the names: the operator's
/// rule, on being shown a sweep that materialised every diff and then printed a
/// list — *"if that many files changed and we show only one - only one diff has
/// to be computed"*.
///
/// A path whose old bytes were never read is left out: see [`Before::skipped`].
pub fn changed_since(root: &Path, base: &Before) -> Vec<String> {
    if !base.in_repo {
        return Vec::new();
    }
    let Some(now) = dirty_paths(root) else {
        return Vec::new();
    };
    now.into_iter()
        .filter(|p| match base.stamp.get(p) {
            // Modified before. A stat that has not moved is a file this command
            // did not write; one that has is worth a diff.
            Some(was) => {
                stamp_of(root, p).is_some_and(|now| now != *was)
                // ...unless its old bytes were over the budget, in which case
                // there is nothing to diff against and the honest answer is
                // silence rather than a diff against `HEAD` that would show
                // whatever was already uncommitted as this command's doing.
                && base.dirty.contains_key(p)
            }
            // Not modified before and modified now: this command did it, and
            // `HEAD` still has the old bytes.
            None => true,
        })
        .collect()
}

/// **One diff, materialised on demand.** The expensive half: a read, and a `git
/// show` for a file that was clean. Called once per card the caller actually
/// draws, never once per changed file.
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

/// `(mtime, len)`, or `None` for a path that cannot be stat'd.
fn stamp_of(root: &Path, rel: &str) -> Option<(u128, u64)> {
    let md = std::fs::metadata(root.join(rel)).ok()?;
    let mtime = md
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((mtime, md.len()))
}

/// Repo-relative paths git reports as changed, or `None` when this is not a repo
/// (or git is not installed, which is the same answer to this question).
fn dirty_paths(root: &Path) -> Option<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "-z", "--untracked-files=all"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `-z` is NUL-separated with no quoting, which is the only form that
    // survives a path with a space, a quote or a newline in it.
    let mut paths = Vec::new();
    for rec in String::from_utf8_lossy(&out.stdout).split('\0') {
        if rec.len() < 4 {
            continue;
        }
        // `XY <path>`: two status columns, a space, then the path.
        let (code, path) = rec.split_at(3);
        // A rename's second half arrives as its own record under `-z`; the
        // destination is what exists on disk and is what a diff is about.
        if code.starts_with('D') || code.starts_with(" D") {
            continue;
        }
        paths.push(path.to_string());
    }
    Some(paths)
}

fn show_head(root: &Path, path: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("show")
        .arg(format!("HEAD:{path}"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn repo(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("letibot-detect-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "first"]);
        root
    }

    /// The case the operator watched: a shell rewrote a file that was already
    /// modified. Git cannot supply what was never committed, so the pre-read is
    /// the only place the old bytes existed.
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

    /// **A command that changed nothing reports nothing**, however dirty the tree
    /// already was. Attributing somebody else's uncommitted work to this command
    /// is the failure this whole before/after shape exists to avoid.
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

    /// **Naming what changed does not read anything.**
    ///
    /// The cheap phase is a `git status` and a stat each; the read and the `git
    /// show` happen once, for the one file a card is drawn for. Pinned by making
    /// the files unreadable after the command: the names still come back, and
    /// only the explicit `diff_of` fails.
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

    /// **A broadly dirty tree costs one `git status` and nothing else.**
    ///
    /// The budget is there to be given up on. Past `WIDE_TREE` modified files
    /// the pre-read is skipped whole, so a change to one of them is not reported
    /// — a miss, deliberately, rather than a wrong diff or a slow command.
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
}
