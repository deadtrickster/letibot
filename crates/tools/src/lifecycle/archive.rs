//! Putting an owner's work where a removal cannot lose it: `refs/letibot/archive/<owner>/…`
//! (`docs/lifecycle.md` d.8).
//!
//! Three properties of that namespace are the whole reason it is the one used: it is outside
//! `refs/heads`, so no branch list shows it and no default push or fetch moves it; its commits stay
//! reachable, so `git gc` never prunes them; and a ref costs nothing, so a copy is always kept
//! rather than judged worth keeping. A dirty tree is committed first, with a scratch index, so the
//! tree a command left uncommitted is in the archive too and the source's own index is untouched.

use std::path::Path;

/// What one `keep` preserved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kept {
    /// The refs written in the archive namespace, in the order they were read back.
    pub refs: Vec<String>,
    /// The commit that carries the tree's uncommitted work, when there was any.
    pub wip: Option<String>,
}

impl Kept {
    /// `4 refs and a wip commit`, for a journal line.
    pub fn said(&self) -> String {
        let n = self.refs.len();
        match (&self.wip, n) {
            (Some(w), 0) => format!("only its uncommitted work ({w})"),
            (Some(w), n) => format!("{n} ref(s) and its uncommitted work ({w})"),
            (None, 0) => "nothing: the tree had no commit and no change".into(),
            (None, n) => format!("{n} ref(s)"),
        }
    }
}

/// The namespace one owner's work is kept under.
pub fn namespace(owner: &str, tag: &str) -> String {
    format!("refs/letibot/archive/{owner}/{tag}")
}

/// `git` in `repo`, bounded and with the output the answer: `Ok(stdout)` on success, `Err` with
/// stderr otherwise. `envs` is for the one variable a scratch index needs.
pub fn git(repo: &Path, args: &[&str], envs: &[(&str, String)]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    Err(format!(
        "`git {}` in {}: {}",
        args.join(" "),
        repo.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// Whether `path` is a git checkout at all. `git -C` on a directory that is not one fails, which is
/// the whole answer; a `.git` file (a worktree's) counts, which is why `git` asks and `stat` does not.
pub fn is_repo(path: &Path) -> bool {
    path.is_dir() && git(path, &["rev-parse", "--git-dir"], &[]).is_ok()
}

/// Keep everything `source` holds in `repo`'s archive namespace under `owner`/`tag`, then read every
/// ref back: a fetch nobody verified is not a copy. `source` is a checkout - the booted copy, a
/// copy-out, a worktree - and it is left as it was found (`refs/letibot/wip/…` aside).
pub fn keep(repo: &Path, source: &Path, owner: &str, tag: &str) -> Result<Kept, String> {
    if !is_repo(repo) {
        return Err(format!(
            "{} is not a git repository to archive into",
            repo.display()
        ));
    }
    if !is_repo(source) {
        return Err(format!(
            "{} is not a git checkout, so there is nothing of it to fetch into {} - and a tree of \
             class work is never removed on a guess",
            source.display(),
            namespace(owner, tag)
        ));
    }
    // The uncommitted half first: a scratch index, so the source's own is not touched, and a ref of
    // its own so the fetch below carries it like any other.
    let wip_ref = format!("refs/letibot/wip/{owner}/{tag}");
    let index = std::env::temp_dir().join(format!(
        "letibot-archive-{}-{}.index",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let scratch = [("GIT_INDEX_FILE", index.display().to_string())];
    let wip = match (|| -> Result<Option<String>, String> {
        git(source, &["read-tree", "HEAD"], &scratch)?;
        // `-A` honours `.gitignore`, so a build target is not written into the object store.
        git(source, &["add", "-A", "--", "."], &scratch)?;
        let tree = git(source, &["write-tree"], &scratch)?.trim().to_string();
        let head = git(source, &["rev-parse", "HEAD"], &[])
            .ok()
            .map(|h| h.trim().to_string());
        let same = head
            .as_deref()
            .and_then(|h| git(source, &["rev-parse", &format!("{h}^{{tree}}")], &[]).ok())
            .map(|t| t.trim() == tree)
            .unwrap_or(false);
        if same {
            return Ok(None);
        }
        let message = format!("letibot: the uncommitted work of {owner} at {tag}");
        let mut args = vec!["commit-tree", tree.as_str()];
        if let Some(h) = &head {
            args.push("-p");
            args.push(h);
        }
        args.push("-m");
        args.push(message.as_str());
        let sha = git(source, &args, &[])?.trim().to_string();
        git(source, &["update-ref", &wip_ref, &sha], &[])?;
        Ok(Some(sha))
    })() {
        Ok(w) => w,
        Err(e) => {
            let _ = std::fs::remove_file(&index);
            return Err(e);
        }
    };
    let _ = std::fs::remove_file(&index);
    let mut refspecs = vec![format!("+refs/heads/*:{}/heads/*", namespace(owner, tag))];
    if wip.is_some() {
        refspecs.push(format!("+{wip_ref}:{}/wip", namespace(owner, tag)));
    }
    let mut args = vec!["fetch", "--no-tags", "--quiet"];
    let source_s = source.display().to_string();
    args.push(source_s.as_str());
    for r in &refspecs {
        args.push(r.as_str());
    }
    git(repo, &args, &[])?;
    // Read back: every ref the fetch was asked for, and the wip one, by name.
    let mut refs = Vec::new();
    for r in git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &namespace(owner, tag),
        ],
        &[],
    )?
    .lines()
    {
        let r = r.trim();
        if r.is_empty() {
            continue;
        }
        git(repo, &["rev-parse", r], &[])?;
        refs.push(r.to_string());
    }
    if let Some(w) = &wip {
        let at = format!("{}/wip", namespace(owner, tag));
        let back = git(repo, &["rev-parse", &at], &[])?.trim().to_string();
        if &back != w {
            return Err(format!(
                "the wip commit {w} went in and {back} came back from {at}: not a copy"
            ));
        }
        return Ok(Kept {
            refs,
            wip: Some(w.clone()),
        });
    }
    Ok(Kept { refs, wip: None })
}

/// Fetch a bundle a remote host wrote (`docs/lifecycle.md` d.8's fallback) into the same namespace
/// and read it back. The bundle is a `work` holding until this succeeds: a host reaper that removed
/// the tree and could not be reached from here leaves the bundle as the only copy.
pub fn fetch_bundle(repo: &Path, bundle: &Path, owner: &str, tag: &str) -> Result<Kept, String> {
    if !is_repo(repo) {
        return Err(format!(
            "{} is not a git repository to archive into",
            repo.display()
        ));
    }
    if !bundle.is_file() {
        return Err(format!("no bundle at {}", bundle.display()));
    }
    let refspec = format!("+refs/heads/*:{}/heads/*", namespace(owner, tag));
    let b = bundle.display().to_string();
    git(
        repo,
        &[
            "fetch",
            "--no-tags",
            "--quiet",
            b.as_str(),
            refspec.as_str(),
        ],
        &[],
    )?;
    let mut refs = Vec::new();
    for r in git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &namespace(owner, tag),
        ],
        &[],
    )?
    .lines()
    {
        let r = r.trim();
        if r.is_empty() {
            continue;
        }
        git(repo, &["rev-parse", r], &[])?;
        refs.push(r.to_string());
    }
    Ok(Kept { refs, wip: None })
}
