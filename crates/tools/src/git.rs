//! Git, as the queues and the reaper use it: every command is one `git -C <dir> …` with git's
//! own words on failure.
//!
//! These are the primitives that decide whether a tree or a ref may be removed - the three-valued
//! [`Proof`] and the worktree rail under `git worktree remove` - so they live in one crate that
//! both the merge queue (`letibot-harnessd`) and the lifecycle reaper reach. Two copies of a
//! safety rail is one copy too many: a queue that removes a tree on a weaker check than the
//! reaper's is the same defect wearing a different commit.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One git command in `dir`: stdout on success, git's own words (stderr, else stdout) on failure.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git {}: {e}", words(args)))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let mut msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if msg.is_empty() {
            msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        Err(format!("git {}: {msg}", words(args)))
    }
}

/// A command's words on one line - an argument holding a space is quoted, so a credential
/// helper's `!gh auth git-credential` cannot read as three arguments in a failure message.
pub fn words(args: &[&str]) -> String {
    args.iter()
        .map(|a| {
            if a.contains(' ') {
                format!("'{a}'")
            } else {
                (*a).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The sha a revision names, or `None` when git cannot name it.
pub fn rev_parse(dir: &Path, rev: &str) -> Option<String> {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// The head of local branch `branch`, by its full ref so a tag of the same name cannot answer.
pub fn branch_head(repo: &Path, branch: &str) -> Option<String> {
    rev_parse(repo, &format!("refs/heads/{branch}"))
}

/// The commit a worktree's own `HEAD` names - what `git worktree remove --force` is
/// unrecoverable for when no ref holds it. `Err` is *it could not be read*, which the sweep keeps
/// for.
pub fn head_of(wt: &Path) -> Result<String, String> {
    let sha = git(wt, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?;
    let sha = sha.trim().to_string();
    if sha.is_empty() {
        return Err(format!(
            "`git rev-parse HEAD` in {} answered nothing",
            wt.display()
        ));
    }
    Ok(sha)
}

/// What a ref is: there, not there, or unreadable. [`rev_parse`] folds all three into one `Option`
/// and the sweep read its `None` as *gone* - a repository git could not read then took a worktree
/// down with it.
enum RefState {
    Sha(String),
    /// Git said, in so many words, that there is no such ref (exit 1 from `show-ref --verify`).
    Absent,
    Unreadable(String),
}

/// [`RefState`] for `refs/heads/<branch>` - `show-ref --verify` and not `rev-parse`, because its
/// exit status separates *no such ref* (1) from *this is not a repository* (128).
fn ref_state(repo: &Path, branch: &str) -> RefState {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => match branch_head(repo, branch) {
            Some(sha) => RefState::Sha(sha),
            None => {
                RefState::Unreadable("`git show-ref` named it and `git rev-parse` did not".into())
            }
        },
        Ok(o) if o.status.code() == Some(1) => RefState::Absent,
        Ok(o) => {
            let said = String::from_utf8_lossy(&o.stderr).trim().to_string();
            RefState::Unreadable(if said.is_empty() {
                format!("git show-ref answered {:?}", o.status.code())
            } else {
                said
            })
        }
        Err(e) => RefState::Unreadable(format!("git show-ref: {e}")),
    }
}

/// The proof a deletion needs: are the commits of a ref (or a worktree's HEAD) on the target?
/// Nothing in the queue may delete a ref or a tree whose commits are not provably there, so the
/// answer is three-valued - [`Proof::Gone`] is permission, [`Proof::Unproven`] is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proof {
    /// The tip is on the target: `git merge-base --is-ancestor` said so.
    On,
    /// The ref does not exist. It holds no commit, so nothing can be lost by removing what is left.
    Gone,
    /// The proof could not be taken, or was taken and refused: the kind is the sweep's key for
    /// saying it once, the sentence is what a person reads.
    Unproven(&'static str, String),
}

impl Proof {
    /// The reason it stays, when it stays.
    pub fn unproven(&self) -> Option<(&'static str, String)> {
        match self {
            Proof::Unproven(kind, why) => Some((kind, why.clone())),
            _ => None,
        }
    }
}

/// Whether local branch `branch`'s commits are on `target`. `what` names the thing being proved in
/// the sentence a person reads: *"the branch `agent/x`"*.
pub fn ref_proof(repo: &Path, branch: &str, target: &str, what: &str) -> Proof {
    match ref_state(repo, branch) {
        RefState::Absent => Proof::Gone,
        RefState::Unreadable(why) => Proof::Unproven(
            "unreadable",
            format!(
                "{what} is kept: whether it exists in {} could not be told ({why}), so whether its \
                 commits are on `{target}` could not be told either",
                repo.display()
            ),
        ),
        RefState::Sha(head) => commit_proof(repo, &head, target, what),
    }
}

/// The same proof about one commit - a worktree's own HEAD, which a `git worktree remove --force`
/// is unrecoverable for.
pub fn commit_proof(repo: &Path, head: &str, target: &str, what: &str) -> Proof {
    let tip = match ref_state(repo, target) {
        RefState::Sha(tip) => tip,
        RefState::Absent => {
            return Proof::Unproven(
                "target",
                format!(
                    "`{target}` cannot be named in {}, so whether {what} is on it cannot be told, \
                     and it is kept",
                    repo.display()
                ),
            );
        }
        RefState::Unreadable(why) => {
            return Proof::Unproven(
                "target",
                format!(
                    "`{target}` could not be read in {} ({why}), so whether {what} is on it cannot \
                     be told, and it is kept",
                    repo.display()
                ),
            );
        }
    };
    proof_against(repo, head, &tip, target, what)
}

/// The same proof about a commit, against a tip git is handed as a revision - the target as it was
/// read a moment ago, which is how a superseded fix's branch is judged. `target` is the label the
/// sentence uses.
pub fn commit_on(repo: &Path, head: &str, tip_rev: &str, target: &str, what: &str) -> Proof {
    match rev_parse(repo, tip_rev) {
        Some(tip) => proof_against(repo, head, &tip, target, what),
        None => Proof::Unproven(
            "target",
            format!(
                "`{tip_rev}` cannot be named in {}, so whether {what} is on it cannot be told, and \
                 it is kept",
                repo.display()
            ),
        ),
    }
}

/// `head` against a tip already resolved.
fn proof_against(repo: &Path, head: &str, tip: &str, target: &str, what: &str) -> Proof {
    match is_ancestor(repo, head, tip) {
        Ok(true) => Proof::On,
        Ok(false) => {
            let over = commits_over(repo, tip, head)
                .map(|n| format!("{n} commit(s)"))
                .unwrap_or_else(|_| "commits".into());
            Proof::Unproven(
                "carries",
                format!(
                    "{what} is kept: it carries {over} over `{target}` (at {tip}) that are not on \
                     it, so it is the only copy of that work"
                ),
            )
        }
        Err(why) => Proof::Unproven(
            "unknown",
            format!("whether {what} is on `{target}` could not be told ({why}), so it is kept"),
        ),
    }
}

/// Whether `a` is an ancestor of (or equal to) `b`.
pub fn is_ancestor(dir: &Path, a: &str, b: &str) -> Result<bool, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["merge-base", "--is-ancestor", a, b])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git merge-base: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "git merge-base --is-ancestor {a} {b}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// Commits in `head` that `base` does not have.
pub fn commits_over(dir: &Path, base: &str, head: &str) -> Result<usize, String> {
    let n = git(dir, &["rev-list", "--count", &format!("{base}..{head}")])?;
    n.trim()
        .parse()
        .map_err(|e| format!("git rev-list --count answered `{}`: {e}", n.trim()))
}

/// The repository a worktree belongs to - the main checkout, from the common git directory.
pub fn repo_of_worktree(worktree: &Path) -> Option<PathBuf> {
    let common = git(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    let common = PathBuf::from(common.trim());
    let repo = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf)?
    } else {
        common
    };
    Some(std::fs::canonicalize(&repo).unwrap_or(repo))
}

/// A file the daemon wrote into a workspace, directly in `.letibot/`: delivery log, head's notes, a monitor record. Not the child's work, so it neither keeps a tree nor nudges the child.
/// A subdirectory there (`plugins`, `notes`) is the project's and stays the child's. `path` is porcelain, relative to the tree.
pub fn daemon_leftover(path: &str) -> bool {
    let path = path.trim().trim_matches('"');
    path.strip_prefix(".letibot/")
        .is_some_and(|rest| !rest.contains('/'))
}

/// `git status --porcelain`'s lines that are the tree's own work - every untracked file named
/// (`-uall`, so a directory of the daemon's logs is not one opaque `??.letibot/`), less
/// [`daemon_leftover`]s.
pub fn uncommitted_work(wt: &Path) -> Result<Vec<String>, String> {
    let out = git(wt, &["status", "--porcelain", "--untracked-files=all"])?;
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| !daemon_leftover(l.get(3..).unwrap_or_default()))
        .map(|l| l.trim().to_string())
        .collect())
}

/// The git directory a linked worktree's `.git` names - a worktree's `.git` is a FILE of one line
/// `gitdir: <main checkout>/.git/worktrees/<name>`; the main checkout's own `.git` is a directory
/// and this answers `None` for it. That asymmetry is what [`is_worktree_of`] rests on.
pub fn linked_gitdir(wt: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(wt.join(".git")).ok()?;
    let rest = text.trim().strip_prefix("gitdir:")?.trim();
    (!rest.is_empty()).then(|| PathBuf::from(rest))
}

/// Whether `path` is a linked worktree of `repo`, and never the repository itself - the
/// rail under every removal the queues make. A row's `worktree` column is text somebody
/// wrote, and this is the only thing standing between a sweep and `rm -rf` of a checkout
pub fn is_worktree_of(path: &Path, repo: &Path) -> bool {
    let Some(linked) = linked_gitdir(path) else {
        return false;
    };
    let Some(main) = repo_of_worktree(repo) else {
        return false;
    };
    let worktrees = main.join(".git").join("worktrees");
    let resolved = std::fs::canonicalize(&worktrees).unwrap_or_else(|_| worktrees.clone());
    linked.starts_with(&worktrees)
        || linked.starts_with(&resolved)
        || std::fs::canonicalize(&linked).is_ok_and(|p| p.starts_with(&resolved))
}

/// Remove one worktree of `repo` and everything inside it - the tree, and the cache a
/// package keeps in its own tree (a private `target/`, which is what makes a review's tree
/// weigh tens of GB).
pub fn remove_worktree(repo: &Path, wt: &Path, branch: &str) -> Result<(), String> {
    if !wt.exists() {
        // The tree is gone; git's bookkeeping for it may not be.
        let _ = git(repo, &["worktree", "prune"]);
        return Ok(());
    }
    if !is_worktree_of(wt, repo) {
        return Err(format!(
            "{} is not a worktree of {} (its `.git` names no `worktrees` directory of this \
             repository), so nothing was removed from it",
            wt.display(),
            repo.display()
        ));
    }
    let dirty = uncommitted_work(wt)?;
    if !dirty.is_empty() {
        return Err(format!(
            "the worktree {} holds uncommitted changes ({}), so it and `{branch}` are kept",
            wt.display(),
            dirty.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    git(
        repo,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    )?;
    let _ = git(repo, &["worktree", "prune"]);
    Ok(())
}

/// Delete one branch of `repo`, and treat "there is no such branch" as done - a sweep runs
/// on every pass, and the second pass must not fail where the first one succeeded. The
/// caller must already have taken the proof ([`ref_proof`] or [`commit_proof`])
pub fn delete_branch(repo: &Path, branch: &str) -> Result<(), String> {
    if branch_head(repo, branch).is_none() {
        return Ok(());
    }
    git(repo, &["branch", "-D", branch]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway repository with one commit.
    pub(crate) fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "letibot-git-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(&args)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}");
        }
        std::fs::write(dir.join("one"), "one\n").unwrap();
        git(&dir, &["add", "-A"]).unwrap();
        git(&dir, &["commit", "-q", "-m", "one"]).unwrap();
        dir
    }

    #[test]
    fn a_proof_is_on_only_when_the_commit_is_reachable_from_the_target() {
        let dir = repo("proof");
        let head = head_of(&dir).unwrap();
        assert_eq!(
            commit_proof(&dir, &head, "main", "`HEAD`"),
            Proof::On,
            "a commit is on its own branch"
        );
        std::fs::write(dir.join("two"), "two\n").unwrap();
        git(&dir, &["add", "-A"]).unwrap();
        git(&dir, &["commit", "-q", "-m", "two"]).unwrap();
        let ahead = head_of(&dir).unwrap();
        // `git branch -f` refuses while that branch is checked out; the ref is what the proof reads.
        git(&dir, &["update-ref", "refs/heads/main", &head]).unwrap();
        match commit_proof(&dir, &ahead, "main", "`HEAD`") {
            Proof::Unproven(kind, why) => {
                assert_eq!(kind, "carries");
                assert!(why.contains("1 commit(s)"), "{why}");
            }
            other => panic!("one commit over the target is not a proof: {other:?}"),
        }
    }

    #[test]
    fn a_ref_git_cannot_read_is_not_a_ref_that_is_gone() {
        let dir = repo("unreadable");
        // A `.git` git refuses to read: the ref is neither there nor provably absent. The real one
        // is moved aside, because a directory is what a repository's `.git` is.
        std::fs::rename(dir.join(".git"), dir.join("git-real")).unwrap();
        std::fs::write(dir.join(".git"), "gitdir: /nonexistent/gitdir\n").unwrap();
        match ref_proof(&dir, "main", "main", "`main`") {
            Proof::Unproven(_, why) => assert!(why.contains("could not be told"), "{why}"),
            other => panic!("an unreadable repository is not permission: {other:?}"),
        }
    }

    #[test]
    fn a_main_checkout_is_never_a_worktree_of_itself() {
        let dir = repo("main");
        assert!(
            !is_worktree_of(&dir, &dir),
            "the main checkout's `.git` is a directory, so it is nobody's worktree"
        );
        let wt = dir.join("wt");
        git(&dir, &["worktree", "add", "-q", &wt.to_string_lossy()]).unwrap();
        assert!(is_worktree_of(&wt, &dir), "a linked worktree is one");
        assert!(
            !is_worktree_of(&dir.join("wt").join(".."), &dir),
            "the checkout is still not"
        );
    }

    #[test]
    fn a_dirty_worktree_is_refused_by_name_and_a_clean_one_is_removed() {
        let dir = repo("remove");
        let wt = dir.join("wt");
        git(
            &dir,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "topic",
                &wt.to_string_lossy(),
            ],
        )
        .unwrap();
        std::fs::write(wt.join("wip"), "x\n").unwrap();
        let why = remove_worktree(&dir, &wt, "topic").expect_err("uncommitted work keeps it");
        assert!(why.contains("uncommitted changes"), "{why}");
        assert!(wt.is_dir(), "the tree is still there");
        std::fs::remove_file(wt.join("wip")).unwrap();
        remove_worktree(&dir, &wt, "topic").expect("a clean worktree goes");
        assert!(!wt.exists(), "the tree is gone");
    }
}
