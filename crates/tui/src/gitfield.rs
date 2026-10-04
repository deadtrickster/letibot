//! **What the workspace's own repository says**, for the row the operator crosses.
//!
//! The ask is theirs, 2026-10-04: *"git status in the status line — look how leticl did it."*
//! leticl's half is `src/chrome.lisp:644` and this is the same field in the same place: **beside
//! the workspace path**, because that is the thing it is a fact about — and it is ONE field: the
//! branch, a `*` when there is anything uncommitted, and a count when the branch is ahead or
//! behind.
//!
//! **A repository this cannot read is ABSENT, not empty.** The field is `None` and the row draws
//! its blank slot — leticl's words, and they are the rule this file keeps: *"a header that said
//! `main` over a directory that is not a repository would be a lie in the one row nobody
//! checks."*
//!
//! **Read from the LOOP, never from a paint.** This spawns a process, and the rule is the dash
//! collectors': a measurement never runs where the frame is drawn. `App::refresh_git` is called
//! by `driver::tick`, and [`GIT_REFRESH_MS`] is leticl's own interval — chosen to hold the process
//! rate down rather than because a frame could not afford it (`git status --porcelain=v2 --branch`
//! measured 0.00 s on this checkout). It is wrapped in coreutils' `timeout`, because a `wait` with
//! no deadline is what turns a slow mount into a frozen head.

use std::process::Command;

/// How long a reading is trusted before the loop takes another.
pub const GIT_REFRESH_MS: u64 = 2_000;

/// How long one reading may take before it is killed and treated as absent.
pub const GIT_TIMEOUT_SECS: u64 = 5;

/// **The field, out of one `git status --porcelain=v2 --branch`.**
///
/// `None` when the text names no branch at all — an empty reading, or git's own complaint, both of
/// which are *this is not a repository I can read* rather than *a repository with nothing in it*.
///
/// The v2 shape this reads, measured on this checkout:
///
/// ```text
/// # branch.oid 31f30adfa3266f3ee47186ae4a33f22a43af50ce
/// # branch.head main
/// # branch.upstream origin/main
/// # branch.ab +0 -0
/// ```
///
/// and every line that does not begin with `#` is one changed path. `branch.ab` and
/// `branch.upstream` are ABSENT on a branch with no upstream — which is a fact about the branch,
/// not about the repository, so their absence is not an error either.
pub fn git_field(porcelain: &str) -> Option<String> {
    let mut head: Option<String> = None;
    let mut oid: Option<String> = None;
    let mut ab: Option<(u64, u64)> = None;
    let mut dirty = 0usize;
    for line in porcelain.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            head = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.oid ") {
            oid = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            let mut it = rest.split_whitespace();
            let ahead = it
                .next()
                .and_then(|s| s.strip_prefix('+'))
                .and_then(|n| n.parse::<u64>().ok());
            let behind = it
                .next()
                .and_then(|s| s.strip_prefix('-'))
                .and_then(|n| n.parse::<u64>().ok());
            if let (Some(a), Some(b)) = (ahead, behind) {
                ab = Some((a, b));
            }
        } else if !line.starts_with('#') && !line.is_empty() {
            dirty += 1;
        }
    }
    let head = head?;
    // **A detached HEAD is not branch-less.** It has a name git prints as `(detached)` and a
    // commit the `oid` line still carries, so the field says which commit the tree is parked on
    // rather than nothing — the oid shortened the way every other id on this screen is.
    let mut said = if head == "(detached)" {
        match oid.as_deref() {
            Some(o) => format!("({})", short(o)),
            None => "(detached)".to_string(),
        }
    } else {
        head
    };
    if dirty > 0 {
        said.push('*');
    }
    if let Some((ahead, behind)) = ab {
        if ahead > 0 {
            said.push_str(&format!(" ↑{ahead}"));
        }
        if behind > 0 {
            said.push_str(&format!(" ↓{behind}"));
        }
    }
    Some(said)
}

/// **An action in progress, from the marker files git itself leaves.**
///
/// `git status --porcelain` does not report one at all — the long format says *rebase in progress*
/// and the porcelain says nothing — so the markers are the only source, which is what leticl's
/// `%git-action` found too. A `.git` that is a FILE is a linked worktree: its markers live beside
/// the real repository, and this returns `None` for it. An absence rather than a guess.
pub fn git_action(workspace: &str) -> Option<&'static str> {
    let git = std::path::Path::new(workspace).join(".git");
    if !git.is_dir() {
        return None;
    }
    let there = |n: &str| git.join(n).exists();
    if there("rebase-merge") || there("rebase-apply") {
        Some("rebase")
    } else if there("MERGE_HEAD") {
        Some("merge")
    } else if there("CHERRY_PICK_HEAD") {
        Some("cherry-pick")
    } else if there("REVERT_HEAD") {
        Some("revert")
    } else if there("BISECT_LOG") {
        Some("bisect")
    } else {
        None
    }
}

/// **One reading of the workspace, or `None`.** The process call, kept apart from the parse so the
/// parse is testable without a repository.
pub fn read(workspace: &str) -> Option<String> {
    if workspace.is_empty() {
        return None;
    }
    let out = Command::new("timeout")
        .arg(GIT_TIMEOUT_SECS.to_string())
        .arg("git")
        .args(["status", "--porcelain=v2", "--branch"])
        .current_dir(workspace)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut field = git_field(&text)?;
    if let Some(action) = git_action(workspace) {
        field.push_str(&format!(" ({action})"));
    }
    Some(field)
}

/// The first seven characters of an oid — the width every id on this screen is spoken at.
fn short(oid: &str) -> String {
    oid.chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_branch_is_its_name() {
        let clean = "# branch.oid 31f30adfa3266f3ee47186ae4a33f22a43af50ce\n# branch.head main\n\
                     # branch.upstream origin/main\n# branch.ab +0 -0\n";
        assert_eq!(git_field(clean).as_deref(), Some("main"));
    }

    /// **Three facts, three marks, and the marks are the vocabulary** — the operator asked for the
    /// field, and a field that showed `main` for a tree with nine changed files would be the row
    /// lying politely.
    #[test]
    fn a_dirty_branch_is_starred_and_a_diverged_one_counts_both_ways() {
        let dirty = "# branch.oid abc1234\n# branch.head main\n# branch.upstream origin/main\n\
                     # branch.ab +2 -1\n1 .M N... 100644 100644 100644 aaa bbb src/app.rs\n\
                     1 .M N... 100644 100644 100644 ccc ddd src/lib.rs\n";
        assert_eq!(git_field(dirty).as_deref(), Some("main* ↑2 ↓1"));
        // A branch with no upstream at all: no `branch.ab`, no `branch.upstream`, still a branch.
        let lonely = "# branch.oid abc1234\n# branch.head feature/thing\n";
        assert_eq!(git_field(lonely).as_deref(), Some("feature/thing"));
    }

    /// **A detached HEAD says which commit**, because `(detached)` alone is a name with no content
    /// — and the oid is on the next line.
    #[test]
    fn a_detached_head_names_the_commit_it_is_parked_on() {
        let detached = "# branch.oid 31f30adfa3266f3ee47186ae4a33f22a43af50ce\n\
                        # branch.head (detached)\n";
        assert_eq!(git_field(detached).as_deref(), Some("(31f30ad)"));
    }

    /// **Not a repository is ABSENT, not empty** — the honesty rule this file exists to keep.
    /// Git's own complaint, a partial reading, and an empty string all say nothing about a branch,
    /// and each of them must be `None` rather than a blank field that looks like `main` with no
    /// marks.
    #[test]
    fn a_reading_that_names_no_branch_is_absent() {
        for nothing in [
            "",
            "fatal: not a git repository (or any of the parent directories): .git\n",
            "# branch.oid 31f30ad\n", // an oid with no head line: a truncated or older reading
        ] {
            assert_eq!(git_field(nothing), None, "{nothing:?}");
        }
    }
}
