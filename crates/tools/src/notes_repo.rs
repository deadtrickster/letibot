//! The notes checkout: one git repository outside every project tree, one subdirectory per
//! project. Writes commit locally; only [`push`] talks to the remote, and only when asked.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::Digest;

/// The subdirectory a workspace's notes live in: `<name>-<12 hex>`. Keyed by the `origin` URL when
/// there is one, so every clone of a project on every box shares its notes; else by the main
/// checkout's path, so a worktree shares its checkout's notes.
pub fn project_key(workspace: &Path) -> String {
    if let Some(url) = git_out(workspace, &["remote", "get-url", "origin"]) {
        let norm = normalize_remote(&url);
        if !norm.is_empty() {
            let name = norm.rsplit('/').next().unwrap_or("repo").to_string();
            return keyed(&name, &norm);
        }
    }
    let checkout = git_out(
        workspace,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(PathBuf::from)
    .map(|common| match common.file_name() {
        Some(n) if n == ".git" => common.parent().map(Path::to_path_buf).unwrap_or(common),
        _ => common,
    })
    .unwrap_or_else(|| workspace.to_path_buf());
    let checkout = std::fs::canonicalize(&checkout).unwrap_or(checkout);
    let name = checkout
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "repo".into());
    keyed(&name, &checkout.to_string_lossy())
}

fn keyed(name: &str, identity: &str) -> String {
    let digest = sha2::Sha256::digest(identity.as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}-{hex}", name.trim_start_matches('.'))
}

/// `git@github.com:o/r.git`, `https://user@github.com/o/r/` and `ssh://git@github.com/o/r` are one
/// project: `github.com/o/r`.
pub fn normalize_remote(url: &str) -> String {
    let mut s = url.trim().to_string();
    if let Some((_, rest)) = s.split_once("://") {
        s = rest.to_string();
    } else if let Some((host, path)) = s.split_once(':')
        && !host.contains('/')
    {
        s = format!("{host}/{path}");
    }
    if let Some((user, rest)) = s.split_once('@')
        && !user.contains('/')
    {
        s = rest.to_string();
    }
    let s = s.trim_end_matches('/');
    s.strip_suffix(".git").unwrap_or(s).to_ascii_lowercase()
}

/// Commit everything under `repo`, making it a repository first if it is not one yet. Nothing to
/// commit is `Ok`. Callers treat a failure as a warning: the note is on disk either way.
pub fn commit(repo: &Path, message: &str) -> Result<(), String> {
    ensure(repo, None)?;
    run(repo, &["add", "-A"])?;
    if git(repo)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .is_ok_and(|s| s.success())
    {
        return Ok(());
    }
    run(repo, &["commit", "-q", "-m", message]).map(|_| ())
}

/// Make `repo` a repository on `main` if it is not one, and give it `origin` when a remote is
/// configured and it has none. An `origin` already there is left alone.
pub fn ensure(repo: &Path, remote: Option<&str>) -> Result<(), String> {
    std::fs::create_dir_all(repo).map_err(|e| format!("{}: {e}", repo.display()))?;
    if !repo.join(".git").exists() {
        run(repo, &["init", "-q", "-b", "main"])?;
    }
    if let Some(url) = remote
        && git_out(repo, &["remote", "get-url", "origin"]).is_none()
    {
        run(repo, &["remote", "add", "origin", url])?;
    }
    Ok(())
}

/// Send the checkout to `origin`: fetch, rebase onto `origin/main` when it exists, push `main`.
/// A rebase that conflicts is aborted and reported, never forced.
pub fn push(repo: &Path, remote: Option<&str>) -> Result<String, String> {
    ensure(repo, remote)?;
    if git_out(repo, &["remote", "get-url", "origin"]).is_none() {
        return Err(format!(
            "{} has no `origin`; set `[notes] remote` in ~/.config/letibot/leticode.toml",
            repo.display()
        ));
    }
    run(repo, &["fetch", "-q", "origin"])?;
    if git_out(repo, &["rev-parse", "--verify", "-q", "origin/main"]).is_some()
        && git_out(repo, &["rev-parse", "--verify", "-q", "HEAD"]).is_some()
        && let Err(e) = run(repo, &["rebase", "-q", "origin/main"])
    {
        let _ = run(repo, &["rebase", "--abort"]);
        return Err(format!(
            "rebase onto origin/main failed, nothing pushed: {e}"
        ));
    }
    run(repo, &["push", "-q", "origin", "HEAD:refs/heads/main"])?;
    Ok(format!("pushed {} to origin main", repo.display()))
}

fn git(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).env("GIT_TERMINAL_PROMPT", "0");
    // A box with no identity configured still commits its notes, as letibot.
    if !has_identity(repo) {
        cmd.env("GIT_AUTHOR_NAME", "letibot")
            .env("GIT_AUTHOR_EMAIL", "letibot@localhost")
            .env("GIT_COMMITTER_NAME", "letibot")
            .env("GIT_COMMITTER_EMAIL", "letibot@localhost");
    }
    cmd
}

fn has_identity(repo: &Path) -> bool {
    let dir = if repo.is_dir() { repo } else { Path::new("/") };
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "user.email"])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
}

fn run(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests;
