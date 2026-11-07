//! The tree guard for a routed call. The VM mounts the project read-write, so an install step can
//! write anything in it. After the call only `node_modules/` and the package manifests may differ;
//! every other tracked file, new untracked file, hook, git config and `.npmrc` is put back.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Files a package manager may change, wherever they sit in the tree.
pub const MANIFESTS: &[&str] = &[
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
];

/// Directories whose ignored contents a build may write.
const OUTPUTS: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    ".next",
    ".docusaurus",
    ".cache",
];

/// Files and directories something on the host runs by itself (direnv, mise, editors, git hooks
/// managers), so a write there is code on the host even when git ignores it.
const AUTO_DIRS: &[&str] = &[".vscode", ".idea", ".husky", ".mise"];
const AUTO_FILES: &[&str] = &[".envrc", ".tool-versions", "mise.toml", ".mise.toml"];

fn auto_executed(p: &Path) -> bool {
    p.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|c| AUTO_DIRS.contains(&c))
    }) || p
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| AUTO_FILES.contains(&n))
}

const KEEP_BYTES: u64 = 1 << 20;
const KEEP_TOTAL: u64 = 256 << 20;

#[derive(Debug)]
struct Kept {
    len: u64,
    body: Option<Vec<u8>>,
}

/// The protected tree as it was before the call.
#[derive(Debug)]
pub struct Snapshot {
    root: PathBuf,
    tracked: BTreeMap<PathBuf, Kept>,
    untracked: BTreeSet<PathBuf>,
    /// Ignored files outside the build outputs.
    ignored: BTreeMap<PathBuf, Kept>,
    /// Git's own files and `.npmrc`: `None` is absent.
    guarded: BTreeMap<PathBuf, Option<Vec<u8>>>,
}

/// What the guard did after the call.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub reverted: Vec<String>,
    pub refused: Vec<String>,
    pub unrestorable: Vec<String>,
    pub kept: Vec<String>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.reverted.is_empty() && self.refused.is_empty() && self.unrestorable.is_empty()
    }

    pub fn sentences(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.unrestorable.is_empty() {
            out.push(format!(
                "REFUSED-NOT-RESTORED: the call changed {} and the original could not be put \
                 back (over the 1 MiB snapshot limit). Restore it with git before trusting this \
                 tree",
                self.unrestorable.join(", ")
            ));
        }
        if !self.refused.is_empty() {
            out.push(format!(
                "REFUSED and put back: the call wrote {} - git hooks and info, git config, .npmrc and \
                 files the host runs by itself (.envrc, .vscode, .idea, .husky, mise) are never \
                 changed by a firecode-only call",
                self.refused.join(", ")
            ));
        }
        if !self.reverted.is_empty() {
            out.push(format!(
                "put back: the call changed {} outside `node_modules/`, the build outputs and the package \
                 manifests",
                self.reverted.join(", ")
            ));
        }
        if !self.kept.is_empty() {
            out.push(format!("kept: {}", self.kept.join(", ")));
        }
        out
    }
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn nul_paths(b: &[u8]) -> Vec<PathBuf> {
    b.split(|c| *c == 0)
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(String::from_utf8_lossy(p).into_owned()))
        .collect()
}

fn in_modules(p: &Path) -> bool {
    p.components().any(|c| c.as_os_str() == "node_modules")
}

fn is_manifest(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| MANIFESTS.contains(&n))
}

/// Untracked, not ignored: what `git status` offers to add.
fn untracked(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let raw = git(
        root,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
    )?;
    // Entries are NUL separated; an untracked one reads `? <path>`.
    Ok(raw
        .split(|c| *c == 0)
        .filter_map(|e| e.strip_prefix(b"? "))
        .map(|p| PathBuf::from(String::from_utf8_lossy(p).into_owned()))
        .collect())
}

fn guarded_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let git_path = |what: &str| -> Result<PathBuf, String> {
        let b = git(
            root,
            &["rev-parse", "--path-format=absolute", "--git-path", what],
        )?;
        Ok(PathBuf::from(String::from_utf8_lossy(&b).trim()))
    };
    let mut v = vec![git_path("config")?, root.join(".npmrc")];
    let hooks = git_path("hooks")?;
    if let Ok(rd) = std::fs::read_dir(&hooks) {
        v.extend(rd.flatten().map(|e| e.path()));
    }
    if let Ok(rd) = std::fs::read_dir(git_path("info")?) {
        v.extend(rd.flatten().map(|e| e.path()));
    }
    Ok(v)
}

/// Ignored files a build is not entitled to write: everything but `node_modules/` and the
/// usual build output directories.
fn ignored(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut args: Vec<String> = ["ls-files", "-z", "-o", "-i", "--exclude-standard", "--"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.extend(OUTPUTS.iter().map(|d| format!(":(exclude,glob)**/{d}/**")));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(nul_paths(&git(root, &refs)?).into_iter().collect())
}

/// Read the protected tree. Fails when `root` is not a git repository: the guard has nothing to
/// put a change back from, so the call is not routed.
pub fn snapshot(root: &Path) -> Result<Snapshot, String> {
    let names = nul_paths(&git(root, &["ls-files", "-z"])?);
    let mut tracked = BTreeMap::new();
    let mut total = 0u64;
    for rel in names.into_iter().filter(|p| !in_modules(p)) {
        let Ok(meta) = std::fs::symlink_metadata(root.join(&rel)) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let keep = meta.len() <= KEEP_BYTES && total + meta.len() <= KEEP_TOTAL;
        let body = keep.then(|| std::fs::read(root.join(&rel)).ok()).flatten();
        total += body.as_ref().map_or(0, |b| b.len() as u64);
        tracked.insert(
            rel,
            Kept {
                len: meta.len(),
                body,
            },
        );
    }
    let mut kept_ignored = BTreeMap::new();
    for rel in ignored(root)? {
        let Ok(meta) = std::fs::symlink_metadata(root.join(&rel)) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let keep = meta.len() <= KEEP_BYTES && total + meta.len() <= KEEP_TOTAL;
        let body = keep.then(|| std::fs::read(root.join(&rel)).ok()).flatten();
        total += body.as_ref().map_or(0, |b| b.len() as u64);
        kept_ignored.insert(
            rel,
            Kept {
                len: meta.len(),
                body,
            },
        );
    }
    let mut guarded = BTreeMap::new();
    for p in guarded_paths(root)? {
        let body = std::fs::read(&p).ok();
        guarded.insert(p, body);
    }
    Ok(Snapshot {
        root: root.to_path_buf(),
        tracked,
        untracked: untracked(root)?,
        ignored: kept_ignored,
        guarded,
    })
}

/// Compare the tree to the snapshot and put back everything that may not change.
pub fn restore(snap: &Snapshot) -> Report {
    let mut rep = Report::default();
    let root = &snap.root;
    for (rel, kept) in &snap.tracked {
        let abs = root.join(rel);
        let now = std::fs::read(&abs).ok();
        let same = match (&kept.body, &now) {
            (Some(was), Some(now)) => was == now,
            (None, Some(now)) => now.len() as u64 == kept.len,
            _ => false,
        };
        if same {
            continue;
        }
        if is_manifest(rel) {
            rep.kept.push(rel.display().to_string());
            continue;
        }
        match &kept.body {
            Some(was) => {
                if let Some(d) = abs.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                let _ = std::fs::remove_file(&abs);
                if std::fs::write(&abs, was).is_ok() {
                    if auto_executed(rel) {
                        rep.refused.push(rel.display().to_string());
                    } else {
                        rep.reverted.push(rel.display().to_string());
                    }
                } else {
                    rep.unrestorable.push(rel.display().to_string());
                }
            }
            None => rep.unrestorable.push(rel.display().to_string()),
        }
    }
    if let Ok(now) = untracked(root) {
        for rel in now.difference(&snap.untracked) {
            if in_modules(rel) || is_manifest(rel) {
                continue;
            }
            if std::fs::remove_file(root.join(rel)).is_ok() {
                if auto_executed(rel) {
                    rep.refused.push(rel.display().to_string());
                } else {
                    rep.reverted.push(rel.display().to_string());
                }
            }
        }
    }
    restore_ignored(snap, &mut rep);
    for (abs, was) in &snap.guarded {
        if std::fs::read(abs).ok() == *was {
            continue;
        }
        match was {
            Some(b) => {
                let _ = std::fs::remove_file(abs);
                let _ = std::fs::write(abs, b);
            }
            None => {
                let _ = std::fs::remove_file(abs);
            }
        }
        rep.refused.push(abs.display().to_string());
    }
    // A hook the call created is a guarded path that was absent at the snapshot.
    if let Ok(now) = guarded_paths(root) {
        for abs in now {
            if !snap.guarded.contains_key(&abs) && std::fs::remove_file(&abs).is_ok() {
                rep.refused.push(abs.display().to_string());
            }
        }
    }
    rep
}

/// Ignored files outside the build outputs: changed ones are put back, new ones removed.
fn restore_ignored(snap: &Snapshot, rep: &mut Report) {
    let root = &snap.root;
    let mut note = |rel: &Path, rep: &mut Report| {
        let name = rel.display().to_string();
        if auto_executed(rel) {
            rep.refused.push(name);
        } else {
            rep.reverted.push(name);
        }
    };
    for (rel, kept) in &snap.ignored {
        let abs = root.join(rel);
        let now = std::fs::read(&abs).ok();
        let same = match (&kept.body, &now) {
            (Some(was), Some(now)) => was == now,
            (None, Some(now)) => now.len() as u64 == kept.len,
            _ => false,
        };
        if same {
            continue;
        }
        match &kept.body {
            Some(was) => {
                if let Some(d) = abs.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                let _ = std::fs::remove_file(&abs);
                if std::fs::write(&abs, was).is_ok() {
                    note(rel, rep);
                } else {
                    rep.unrestorable.push(rel.display().to_string());
                }
            }
            None => rep.unrestorable.push(rel.display().to_string()),
        }
    }
    if let Ok(now) = ignored(root) {
        for rel in now {
            if !snap.ignored.contains_key(&rel) && std::fs::remove_file(root.join(&rel)).is_ok() {
                note(&rel, rep);
            }
        }
    }
}
