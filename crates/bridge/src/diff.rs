//! **What the worktree's branch has done**, for the reviewer who is about to read it.
//!
//! `leticode_diff` answers three things and only the last one is big: the branch, the
//! commits it put on top of its base, and the shape of what changed. The base is the
//! branch's upstream when it has one and `main`/`master` otherwise, and the comparison is
//! always against the **merge base** — `git diff main` shows the base's own new commits
//! as this branch's work undone, which is exactly the reading a review must not be given.
//!
//! **The full diff is asked for, never sent.** A branch of twenty files is a few hundred
//! kilobytes of text, and the orchestrator that wanted it knows when: a bridge that
//! returned it by default would put it in Claude's context on every status check.

use std::path::Path;
use std::process::Command;

/// The base a branch is read against: its upstream, else `main`, else `master`.
fn base(worktree: &Path) -> Option<(String, String)> {
    if let Some(upstream) = git(
        worktree,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    ) {
        return Some(("upstream".to_string(), upstream));
    }
    for name in ["main", "master"] {
        if git(worktree, &["rev-parse", "--verify", "--quiet", name]).is_some() {
            return Some(("branch".to_string(), name.to_string()));
        }
    }
    None
}

/// One git invocation, trimmed, or `None` when it failed — a branch with no upstream is an
/// ordinary state here and not an error to report.
fn git(worktree: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(worktree)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// The diff, as the tool answers it.
///
/// `error` is present instead of the fields when the folder is not a repository or has no
/// commits yet — a git failure and an empty diff must not look alike, because one means
/// "nothing to review" and the other means "nobody looked".
pub fn diff(worktree: &Path, full: bool) -> serde_json::Value {
    let Some(branch) = git(worktree, &["rev-parse", "--abbrev-ref", "HEAD"]) else {
        return serde_json::json!({
            "worktree": worktree.display().to_string(),
            "error": "not a git repository, or a repository with no commit yet",
        });
    };
    let Some((how, base)) = base(worktree) else {
        return serde_json::json!({
            "worktree": worktree.display().to_string(),
            "branch": branch,
            "error": "no base to compare against: the branch has no upstream and there is \
                      no main or master",
        });
    };
    // The merge base is what makes the answer about THIS branch: `git diff main` would
    // show every commit main gained since the fork as work this branch undid.
    let Some(merge_base) = git(worktree, &["merge-base", "HEAD", &base]) else {
        return serde_json::json!({
            "worktree": worktree.display().to_string(),
            "branch": branch,
            "base": base,
            "error": "the branch shares no history with its base",
        });
    };
    let range = format!("{merge_base}..HEAD");
    let log = git(worktree, &["log", "--oneline", &range]).unwrap_or_default();
    let stat = git(worktree, &["diff", "--stat", &merge_base, "HEAD"]).unwrap_or_default();
    let mut out = serde_json::json!({
        "worktree": worktree.display().to_string(),
        "branch": branch,
        "base": base,
        "base_from": how,
        "merge_base": merge_base,
        "log": log,
        "stat": stat,
        "commits": log.lines().filter(|l| !l.trim().is_empty()).count(),
    });
    if full {
        // `--no-color` and no pager: this is read by a program, and a diff wrapped in
        // escape codes is a diff nobody can quote.
        let diff = git(
            worktree,
            &["--no-pager", "diff", "--no-color", &merge_base, "HEAD"],
        )
        .unwrap_or_default();
        out["diff"] = serde_json::Value::String(diff);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder that is not a repository is refused with a sentence, not with an empty
    /// diff: *nothing changed* and *nobody looked* are different answers.
    #[test]
    fn a_folder_that_is_not_a_repository_is_named() {
        let dir = std::env::temp_dir().join(format!("letibot-diff-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = diff(&dir, false);
        std::fs::remove_dir_all(&dir).ok();
        assert!(out["error"].as_str().is_some(), "{out}");
        assert!(
            out.get("log").is_none(),
            "a failed read has no commits: {out}"
        );
    }

    /// A real branch: two commits on top of `main`, read against the merge base, so the
    /// log names exactly the branch's own work and the stat names its files.
    #[test]
    fn the_branch_is_read_against_the_merge_base() {
        let dir = std::env::temp_dir().join(format!("letibot-diff-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(&dir)
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q", "--initial-branch=main"]);
        run(&["config", "user.email", "bridge@example.invalid"]);
        run(&["config", "user.name", "bridge test"]);
        std::fs::write(dir.join("a.txt"), "one\n").expect("write");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "the base"]);
        // The base moves on: a commit this branch does not own must not appear as its work.
        run(&["checkout", "-q", "-b", "topic"]);
        std::fs::write(dir.join("b.txt"), "two\n").expect("write");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "the branch's own work"]);
        run(&["checkout", "-q", "main"]);
        std::fs::write(dir.join("c.txt"), "three\n").expect("write");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "somebody else's commit on main"]);
        run(&["checkout", "-q", "topic"]);

        let out = diff(&dir, false);
        let log = out["log"].as_str().unwrap_or_default();
        assert!(log.contains("the branch's own work"), "{out}");
        assert!(
            !log.contains("somebody else's commit"),
            "the base's own commits are not this branch's work: {out}"
        );
        assert!(out["stat"].as_str().unwrap().contains("b.txt"), "{out}");
        assert!(
            out.get("diff").is_none(),
            "the full diff is asked for: {out}"
        );

        // And asked for, it is there and it is the change itself.
        let full = diff(&dir, true);
        let text = full["diff"].as_str().unwrap_or_default();
        assert!(text.contains("+two"), "{full}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
