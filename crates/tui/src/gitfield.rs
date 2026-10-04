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

/// **One directory down, when the repository is not the workspace itself** — the nested layout.
///
/// The operator's own box is the case: the session lives in `Projects/letibot` and the repository
/// in `Projects/letibot/letibot` — **the same name one level down** — so neither the git field nor
/// the todos pane found anything at the workspace, and both drew nothing (their honest absence,
/// working exactly as designed on a layout nobody had told them about).
///
/// The rule, in order, and it is a RESOLUTION rather than a guess at every step:
///
/// 1. **the workspace itself**, when it is a repository (`.git` present — a directory, or a file
///    for a linked worktree) or carries a `TODO.md` — the ordinary layout, unchanged;
/// 2. **the same-named child** (`<workspace>/<basename>`), when that is a repository — the nested
///    naming above, and the reason this function exists;
/// 3. **the one repository child**, when exactly one child is one — a parent holding two
///    repositories is a parent nobody chose between, and picking one would be the absent-field
///    rule broken by the very code that exists to keep it;
/// 4. otherwise **the workspace unchanged**, so whatever reads it fails as it always did and the
///    field stays absent rather than wrong.
///
/// A workspace that is merely *inside* a repository needs none of this: `git` walks up on its
/// own, and step 1 not matching simply leaves it to git.
///
/// Shared by the git field and the todos pane so the two cannot disagree about which directory is
/// the project — a branch from one tree and a `TODO.md` from another would each be true alone.
pub fn project_dir(ws: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(ws);
    let is_repo = |d: &std::path::Path| d.join(".git").exists();
    if dir.join("TODO.md").is_file() || is_repo(dir) || dir.read_dir().is_err() {
        return dir.to_path_buf();
    }
    // The same-named child first: nested naming is a convention, not a coincidence.
    let own_name = dir.file_name().map(std::ffi::OsStr::to_owned);
    if let Some(name) = own_name
        && is_repo(&dir.join(&name))
    {
        return dir.join(name);
    }
    // Then a single repository child — and only one. Two is a choice this function does not make.
    let repos: Vec<std::path::PathBuf> = dir
        .read_dir()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| is_repo(p))
        .collect();
    match repos.as_slice() {
        [only] => only.clone(),
        _ => dir.to_path_buf(),
    }
}

/// **One reading of the workspace, or `None`.** The process call, kept apart from the parse so the
/// parse is testable without a repository.
pub fn read(workspace: &str) -> Option<String> {
    if workspace.is_empty() {
        return None;
    }
    // **Resolved, not assumed** — the workspace may be the parent of the repository rather than
    // the repository (`project_dir`), which is the nested layout both this field and the todos
    // pane were drawing nothing over.
    let dir = project_dir(workspace);
    let out = Command::new("timeout")
        .arg(GIT_TIMEOUT_SECS.to_string())
        .arg("git")
        .args(["status", "--porcelain=v2", "--branch"])
        .current_dir(&dir)
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
    /// **The nested layout, resolved step by step** — the operator's own box is the case: the
    /// session lives in `Projects/letibot` and the repository in `Projects/letibot/letibot`, the
    /// same name one level down, so both consumers drew nothing at the workspace itself.
    #[test]
    fn the_same_named_child_is_the_project_when_the_workspace_is_not_a_repository() {
        let base = std::env::temp_dir().join(format!(
            "letibot-nested-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let ws = base.join("letibot");
        let repo = ws.join("letibot");
        std::fs::create_dir_all(repo.join(".git")).expect("scratch");
        // **The same-named child wins**, and it wins over a sibling repository too: nested naming
        // is a convention, checked before the count.
        std::fs::create_dir_all(ws.join("other").join(".git")).expect("scratch");
        assert_eq!(
            project_dir(ws.to_str().unwrap()),
            repo,
            "the same-named child is the project"
        );

        // **A single differently-named repository child** is still the project — there is only one.
        let one = base.join("one");
        std::fs::create_dir_all(one.join("only-repo").join(".git")).expect("scratch");
        assert_eq!(
            project_dir(one.to_str().unwrap()),
            one.join("only-repo"),
            "exactly one repository child is the project"
        );

        // **Two differently-named repositories is nobody's choice to make** — a parent holding two
        // is a parent nobody chose between, and guessing would be the absent-field rule broken by
        // the code that exists to keep it.
        let two = base.join("two");
        std::fs::create_dir_all(two.join("a").join(".git")).expect("scratch");
        std::fs::create_dir_all(two.join("b").join(".git")).expect("scratch");
        assert_eq!(
            project_dir(two.to_str().unwrap()),
            two,
            "two repositories and no name match: the workspace stands"
        );

        // **The workspace's own `TODO.md` outranks a nested repository** — a file that is there is
        // a stronger fact than a directory that might be the project.
        let flat = base.join("flat");
        std::fs::create_dir_all(flat.join("flat").join(".git")).expect("scratch");
        std::fs::write(flat.join("TODO.md"), "## x\n").expect("write");
        assert_eq!(
            project_dir(flat.to_str().unwrap()),
            flat,
            "the workspace's own TODO.md wins"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// **End to end, through `read`**: a real repository under the same-named child, read from
    /// its parent — the operator's exact layout, proven on a real `git` rather than on a `.git`
    /// directory that only looks like one.
    #[test]
    fn a_nested_repository_is_read_through_its_parent() {
        let base = std::env::temp_dir().join(format!(
            "letibot-nested-read-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let ws = base.join("letibot");
        let repo = ws.join("letibot");
        std::fs::create_dir_all(&repo).expect("scratch");
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .output()
                .expect("git runs")
                .status
                .success();
            assert!(ok, "git {args:?} in {}", repo.display());
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "--allow-empty", "-q", "-m", "x"]);
        assert_eq!(
            read(ws.to_str().unwrap()).as_deref(),
            Some("main"),
            "the parent's git field is the nested repository's branch"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

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
