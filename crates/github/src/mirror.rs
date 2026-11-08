//! One mirror per repository: a pull request of an org's second repository is reviewed from ITS OWN
//! tree, and never from the checkout the seat happens to have been started in.
//! A bare mirror is cloned once and fetched after, and every pull request's tree is a detached
//! worktree of its own repository's mirror - so a re-review lands where the last one was made.

use std::path::{Path, PathBuf};

use crate::source::Pull;

/// The trees of every repository a seat watches.
pub struct Mirrors {
    root: PathBuf,
}

impl Mirrors {
    /// The mirrors under `root`: one directory per repository, and one per pull request beside it.
    pub fn new(root: impl Into<PathBuf>) -> Mirrors {
        Mirrors { root: root.into() }
    }

    /// Where every repository's mirror and every pull request's tree go.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `pull`'s head, checked out detached in its own tree, which is cut from `repo`'s own mirror.
    /// `url` is where that mirror is cloned from the first time - the operator's `origin` when it
    /// is this repository, and GitHub's own URL otherwise.
    pub fn tree(&self, pull: &Pull, url: &str) -> Result<PathBuf, String> {
        let mirror = self.mirror(&pull.repo, url)?;
        // The head is GitHub's own ref, and it is the same ref for a fork's pull request: what is
        // fetched is the branch as GitHub built it, not the branch name somebody typed.
        let reference = format!("refs/letibot/pr/{}", pull.number);
        git(
            &mirror,
            &[
                "fetch",
                "--quiet",
                "origin",
                &format!("+refs/pull/{}/head:{reference}", pull.number),
            ],
        )?;
        let tree = self.tree_path(pull);
        if tree.is_dir() {
            // A tree an earlier review was made in: the head may have moved, and what a reviewer
            // left in it is not work anybody keeps.
            git(
                &tree,
                &["checkout", "--quiet", "--force", "--detach", &reference],
            )?;
            return Ok(tree);
        }
        std::fs::create_dir_all(&self.root).map_err(|e| format!("{}: {e}", self.root.display()))?;
        git(
            &mirror,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &tree.display().to_string(),
                &reference,
            ],
        )?;
        Ok(tree)
    }

    /// Where one pull request's tree is, whether or not it exists yet.
    pub fn tree_path(&self, pull: &Pull) -> PathBuf {
        // One path per pull request, keyed by the repository as well as the number: two
        // repositories of one org both have a #12.
        self.root
            .join(format!("{}-{}", pull.repo.replace('/', "-"), pull.number))
    }

    /// This repository's bare mirror, cloned from `url` the first time and fetched after.
    pub fn mirror(&self, repo: &str, url: &str) -> Result<PathBuf, String> {
        let dir = self.root.join(format!("{}.git", repo.replace('/', "-")));
        if dir.is_dir() {
            // `--mirror` is a fact of the clone's own config, so this is the fetch that keeps every
            // ref the remote has - the same question the clone asked.
            git(&dir, &["fetch", "--quiet", "--prune", "origin"])?;
            return Ok(dir);
        }
        std::fs::create_dir_all(&self.root).map_err(|e| format!("{}: {e}", self.root.display()))?;
        let parent = dir.parent().unwrap_or(&self.root).display().to_string();
        git(
            Path::new(&parent),
            &[
                "clone",
                "--quiet",
                "--mirror",
                url,
                &dir.display().to_string(),
            ],
        )?;
        Ok(dir)
    }
}

/// The clone URL of a repository GitHub hosts, for a mirror with no checkout of its own. `gh`'s
/// credential is what authenticates the fetch of a private repository.
pub fn github_url(repo: &str) -> String {
    format!("https://github.com/{repo}.git")
}

/// The `owner/name` a remote URL names, or `None` for a host this seat does not review. The three
/// spellings are one repository, and a checkout that cannot say is a tree the seat refuses.
pub fn slug_of_url(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, name) = rest.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// `git` in `dir`, with a refusal that carries git's own words. A mirror fetch of a repository this
/// box cannot reach is a row's reason, so the failure is a sentence and never a panic.
fn git(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("`git {}` could not be run ({e})", args.join(" ")))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "`git {}` in {} failed: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

#[cfg(test)]
mod tests;
