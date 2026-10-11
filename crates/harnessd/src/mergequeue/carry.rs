//! **What the landing tree does not have, and the project's way to give it** — `[queue]
//! gate_carry`.
//!
//! A git worktree carries what git tracks and nothing else. The firecode session found the cost,
//! 2026-10-11: *"The queue runs the gate in a landing worktree … A git worktree doesn't carry
//! gitignored files, so that tree has no `images/` and no `vendor/` — no guest rootfs, no kernel,
//! no firecracker. Every check that boots a VM fails, which is precisely the failed list."* The
//! gate peaked at 1.6 GiB of 40.9 GiB because no VM booted, and `main` was declared RED for a
//! reason that is not in the code.
//!
//! So a project names the gitignored inputs its gate needs, and each is COPIED from the
//! operator's checkout into the landing tree before every gate run ([`carry`]) — `rsync -a
//! --delete` when rsync is on the box (an unchanged file is not copied again), else a fresh
//! recursive copy. Never a symlink: a gate that writes into a carried directory writes into its
//! own copy, not the operator's checkout.
//!
//! And when a gate fails with nothing carried, the red notice names the ignored directories the
//! checkout has and the landing tree lacks ([`hint`]) — one sentence, so the next model to read
//! a red like firecode's knows where to look.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use super::git::git;

/// How the notice sentence starts — `notices::health` finds it in the evidence by this.
pub const LANDING_LACKS: &str = "The landing tree has no ";

/// What the hint never names: build outputs the gate makes for itself.
const BUILD_OUTPUTS: &[&str] = &["target"];

/// At most this many names in the hint; the rest are "…".
const HINT_NAMES: usize = 8;

/// **One `gate_carry` entry, checked by its shape** — relative, inside the repository, not the
/// repository itself and not `.git`. The refusal names the entry.
pub fn validate(entry: &str) -> Result<PathBuf, String> {
    let p = Path::new(entry.trim());
    let refuse = |why: &str| {
        Err(format!(
            "[queue] gate_carry entry \"{entry}\" is refused: {why}. An entry is a path relative \
             to the repository's root, inside it (e.g. \"images\", \"vendor\")"
        ))
    };
    if p.as_os_str().is_empty() {
        return refuse("it is empty");
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(n) => {
                if out.as_os_str().is_empty() && n == ".git" {
                    return refuse("`.git` is the repository's own, never a gate input");
                }
                out.push(n);
            }
            Component::CurDir => {}
            Component::ParentDir => return refuse("it climbs out of the repository (`..`)"),
            Component::RootDir | Component::Prefix(_) => {
                return refuse("it is absolute, and so outside the repository");
            }
        }
    }
    if out.as_os_str().is_empty() {
        return refuse("it names the repository itself");
    }
    Ok(out)
}

fn have_rsync() -> bool {
    static HAVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAVE.get_or_init(|| {
        Command::new("rsync")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// **Copy each `entries` path from the checkout `repo` into the landing tree `wt`**, replacing
/// what the landing tree had there. Refused, by name, when an entry is outside the repository
/// (by its shape, or by a symlink that resolves outside), is missing from the checkout, or is
/// TRACKED at the landing tree's commit — a tracked path carried from the checkout would gate
/// the operator's copy instead of the branch's.
pub fn carry(repo: &Path, wt: &Path, entries: &[String]) -> Result<(), String> {
    carry_with(repo, wt, entries, have_rsync())
}

/// [`carry`] with the copier chosen — the tests run both.
pub fn carry_with(repo: &Path, wt: &Path, entries: &[String], rsync: bool) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let repo_c = std::fs::canonicalize(repo).map_err(|e| format!("{}: {e}", repo.display()))?;
    let wt_c = std::fs::canonicalize(wt).map_err(|e| format!("{}: {e}", wt.display()))?;
    for entry in entries {
        let rel = validate(entry)?;
        let src = repo_c.join(&rel);
        let src_c = std::fs::canonicalize(&src).map_err(|_| {
            format!(
                "[queue] gate_carry names \"{entry}\", which is not in the checkout {}",
                repo.display()
            )
        })?;
        if !src_c.starts_with(&repo_c) || src_c == repo_c {
            return Err(format!(
                "[queue] gate_carry entry \"{entry}\" is refused: it resolves to {}, outside the \
                 repository {}",
                src_c.display(),
                repo_c.display()
            ));
        }
        let tracked = git(wt, &["ls-files", "--", &rel.to_string_lossy()]).unwrap_or_default();
        if !tracked.trim().is_empty() {
            return Err(format!(
                "[queue] gate_carry entry \"{entry}\" is refused: git tracks it, so the gate must \
                 see the branch's own copy, not the checkout's"
            ));
        }
        let dst = wt_c.join(&rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            let parent_c =
                std::fs::canonicalize(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            if !parent_c.starts_with(&wt_c) {
                return Err(format!(
                    "[queue] gate_carry entry \"{entry}\" is refused: in the landing tree it \
                     resolves outside it ({})",
                    parent_c.display()
                ));
            }
        }
        if std::fs::symlink_metadata(&dst).is_ok_and(|m| m.file_type().is_symlink()) {
            std::fs::remove_file(&dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        }
        let is_dir = src_c.is_dir();
        if rsync {
            let (from, to) = if is_dir {
                (
                    format!("{}/", src_c.display()),
                    format!("{}/", dst.display()),
                )
            } else {
                (src_c.display().to_string(), dst.display().to_string())
            };
            let out = Command::new("rsync")
                .args(["-a", "--delete", &from, &to])
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("rsync: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "carrying \"{entry}\" into the landing tree: rsync: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        } else {
            remove_any(&dst)?;
            copy_tree(&src_c, &dst)
                .map_err(|e| format!("carrying \"{entry}\" into the landing tree: {e}"))?;
        }
    }
    Ok(())
}

fn remove_any(p: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(p) {
        Err(_) => Ok(()),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
    }
    .map_err(|e| format!("{}: {e}", p.display()))
}

/// A recursive copy that keeps a symlink a symlink (as `rsync -a` does) and never follows one.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let m = std::fs::symlink_metadata(src)?;
    if m.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if m.is_dir() {
        std::fs::create_dir_all(dst)?;
        std::fs::set_permissions(dst, m.permissions())?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// **The top-level gitignored directories the checkout has and the landing tree lacks** —
/// from `git ls-files --others --ignored --exclude-standard --directory` in the checkout, less
/// the build outputs the landing tree makes for itself and anything it already has.
pub fn missing_ignored(repo: &Path, wt: &Path) -> Vec<String> {
    let Ok(list) = git(
        repo,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    ) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for line in list.lines() {
        let Some(top) = line.trim().split('/').next().filter(|t| !t.is_empty()) else {
            continue;
        };
        if BUILD_OUTPUTS.contains(&top) || out.iter().any(|o| o == top) {
            continue;
        }
        let in_checkout = repo.join(top);
        if !in_checkout.is_dir() || in_checkout.join(".git").exists() {
            // A file, or a nested repository / worktree: not a gate input.
            continue;
        }
        if wt.join(top).exists() {
            continue;
        }
        out.push(top.to_string());
    }
    out.sort();
    out
}

/// **The one sentence a failed gate's notice adds**, or `None` — only when nothing is carried
/// and the landing tree lacks an ignored directory the checkout has.
pub fn hint(repo: &Path, wt: &Path, carried: &[String]) -> Option<String> {
    if !carried.is_empty() {
        return None;
    }
    let missing = missing_ignored(repo, wt);
    if missing.is_empty() {
        return None;
    }
    let mut names: Vec<String> = missing
        .iter()
        .take(HINT_NAMES)
        .map(|n| format!("`{n}/`"))
        .collect();
    if missing.len() > HINT_NAMES {
        names.push("…".into());
    }
    Some(format!(
        "{LANDING_LACKS}{} (gitignored, present in your checkout); if the gate needs them, set \
         `[queue] gate_carry` in the project's `leticode.toml`.",
        names.join(", ")
    ))
}

/// The hint sentence a piece of evidence carries, if any.
pub fn hint_in(evidence: &str) -> Option<&str> {
    evidence
        .split("\n\n")
        .map(str::trim)
        .find(|p| p.starts_with(LANDING_LACKS))
}

#[cfg(test)]
mod tests;
