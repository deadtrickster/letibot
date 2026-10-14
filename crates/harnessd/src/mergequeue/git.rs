//! **Git, as the queues use it** — every command is one `git -C <dir> …`, its own words on
//! failure, and nothing that touches the operator's checkout except the one step that has to
//! (keeping a checkout of the target coherent after the ref moved — [`follow_checkouts`]).
//!
//! The landing happens in a DEDICATED worktree ([`landing_worktree_in`]) in letibot's own data
//! directory (`~/.local/share/letibot/landing/<repo>-<hash>`, never inside the repository's
//! `.git`), and the target branch moves by a compare-and-swap on its ref
//! ([`update_ref_cas`]). The v23 queue ran `git -C <repo> merge --ff-only` in whatever the
//! operator had checked out and recorded `rev-parse main` afterwards (R61 defect 7).

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
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let mut msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if msg.is_empty() {
            msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        Err(format!("git {}: {msg}", args.join(" ")))
    }
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

/// **The change a head carries over `onto`, as one stable patch id** — what tells *the branch
/// was rebased* from *the branch moved* (decision 6: *"a rebase onto a newer main is not a
/// move"*). The diff is from the merge base of `onto` and `head` to `head`, so two heads carrying
/// the same change over two different bases answer the same id.
pub fn change_id(dir: &Path, onto: &str, head: &str) -> Option<String> {
    let base = git(dir, &["merge-base", onto, head]).ok()?;
    let diff = git(dir, &["diff", "--binary", base.trim(), head]).ok()?;
    if diff.is_empty() {
        return Some(String::new());
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take()?;
        // `git patch-id` reads a diff with a header; a bare `git diff` has one per file.
        let _ = stdin.write_all(b"commit 0000000000000000000000000000000000000000\n\n");
        let _ = stdin.write_all(diff.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
}

/// **The repository a worktree belongs to** — the main checkout, from the common git directory.
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

/// The top of the checkout `path` is in.
pub fn repo_root(path: &Path) -> Result<PathBuf, String> {
    let out = git(path, &["rev-parse", "--show-toplevel"])?;
    let top = out.trim();
    if top.is_empty() {
        return Err(format!("{} is not inside a git repository", path.display()));
    }
    Ok(PathBuf::from(top))
}

/// **The repository a workspace means** — itself, a same-named child (the operator's
/// `~/Projects/letibot/letibot`), or its only repository child. `None` when it names none or
/// several.
pub fn repo_of_workspace(workspace: &Path) -> Option<PathBuf> {
    if let Ok(top) = repo_root(workspace) {
        return Some(top);
    }
    let is_repo = |d: &Path| d.join(".git").exists();
    if let Some(name) = workspace.file_name() {
        let same_named = workspace.join(name);
        if is_repo(&same_named) {
            return Some(same_named);
        }
    }
    let children: Vec<PathBuf> = workspace
        .read_dir()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| is_repo(p))
        .collect();
    match children.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// **The repositories a workspace is a session of** — the one [`repo_of_workspace`] reads it as,
/// or, for a folder that is ONE PROJECT of several repositories, every one of them.
///
/// The operator, of `~/Projects/stroppy-io` (`stroppy`, `stroppy-contrib`, `website`, none the
/// main one): its session served no queue and heard no alarm. And the reason a folder of
/// repositories is NOT read as serving them stands — on 2026-10-12 a daemon seated at
/// `/home/dead` took letibot's lease and showed `main RED` to a session about the box's memory —
/// so what makes a folder one project is a fact, not a guess, and the operator named it: *"they
/// usually belong to the same org"*. Two or more child repositories whose `origin` remotes all
/// share one host and owner (`github.com/stroppy-io/…`) are one project; mixed owners, or a child
/// with no remote, are a folder of unrelated checkouts, and the answer is what it was.
pub fn repos_of_workspace(workspace: &Path) -> Vec<PathBuf> {
    if let Some(one) = repo_of_workspace(workspace) {
        return vec![one];
    }
    let mut children: Vec<PathBuf> = workspace
        .read_dir()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .collect();
    children.sort();
    if children.len() < 2 {
        return Vec::new();
    }
    let owners: Vec<Option<(String, String)>> = children
        .iter()
        .map(|c| {
            git(c, &["remote", "get-url", "origin"])
                .ok()
                .and_then(|u| remote_owner(u.trim()))
        })
        .collect();
    match owners.first() {
        Some(Some(first)) if owners.iter().all(|o| o.as_ref() == Some(first)) => children,
        _ => Vec::new(),
    }
}

/// **The host and owner a remote URL names** — `("github.com", "stroppy-io")` for
/// `https://github.com/stroppy-io/stroppy.git` and `git@github.com:stroppy-io/stroppy.git` alike,
/// case-folded. `None` for a path remote or anything without an owner segment.
pub fn remote_owner(url: &str) -> Option<(String, String)> {
    let rest = if let Some(r) = url.split_once("://").map(|(_, r)| r) {
        let r = r.rsplit_once('@').map_or(r, |(_, h)| h);
        r.to_string()
    } else if let Some((user_host, path)) = url.split_once(':') {
        let host = user_host.rsplit_once('@').map_or(user_host, |(_, h)| h);
        if host.contains('/') {
            return None;
        }
        format!("{host}/{path}")
    } else {
        return None;
    };
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let host = parts.next()?.split(':').next()?.to_ascii_lowercase();
    let owner = parts.next()?.to_ascii_lowercase();
    parts.next()?;
    Some((host, owner))
}

/// **The queues' name for a repository** — the canonical path of its main checkout, so two
/// daemons seated at two depths of one repository agree on one lease.
pub fn repository_key(any_path_in_it: &Path) -> Option<String> {
    repo_of_worktree(any_path_in_it).map(|p| p.display().to_string())
}

/// The target branch a repository lands on when the project names none (decision 12 makes it
/// per project): `main` when it exists, else `master`, else whatever the main checkout has out.
pub fn default_target(repo: &Path) -> Option<String> {
    for b in ["main", "master"] {
        if branch_head(repo, b).is_some() {
            return Some(b.to_string());
        }
    }
    git(repo, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// **The letibot data directory** — `$XDG_DATA_HOME/letibot`, else `~/.local/share/letibot`,
/// where `sessions.db` and `metrics.db` already live. `None` in a bare environment with neither.
pub fn letibot_data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|d| d.join("letibot"))
}

/// The environment variable that puts the landing root somewhere else — what a test that spawns
/// a daemon sets, so no test ever writes into the box's real `~/.local/share/letibot`.
pub const LANDING_ROOT_ENV: &str = "LETIBOT_LANDING_ROOT";

/// **Where every repository's landing worktree lives**: `$LETIBOT_LANDING_ROOT`, else `<data
/// dir>/landing`. Read once, by the daemon (`Config::for_the_daemon`); every other caller is
/// handed the root it uses (`Config::landing_root`, `QueueEnv::landing_root`).
///
/// A unit test never gets the real data directory from here: under `cfg(test)` this is an `Err`
/// unless the override is set, and [`guard_not_real`] fails a test that lands or grants under it.
pub fn landing_root() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(LANDING_ROOT_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    if cfg!(test) {
        return Err(format!(
            "a unit test has no real landing root; hand it a temporary one \
             (Config::landing_root, QueueEnv::landing_root or ${LANDING_ROOT_ENV})"
        ));
    }
    letibot_data_dir()
        .map(|d| d.join("landing"))
        .ok_or_else(|| {
            "no letibot data directory: neither $XDG_DATA_HOME nor $HOME is set, so there is \
             nowhere outside the repository to land in"
                .to_string()
        })
}

/// **A test must never make anything in the box's real `~/.local/share/letibot`** — under
/// `cfg(test)`, a landing or a grant under the real data directory panics. Nothing in a build.
pub fn guard_not_real(root: &Path) {
    #[cfg(test)]
    if let Some(real) = letibot_data_dir() {
        assert!(
            !root.starts_with(&real),
            "a test reached the REAL letibot data directory ({}) through the landing root {}",
            real.display(),
            root.display()
        );
    }
    let _ = root;
}

/// **The landing directory of `repo` under `root`** — `<root>/<basename>-<12 hex of sha256 of
/// the repository's canonical path>`. The basename is for a person reading the directory; the
/// hash keeps two repositories with one name apart. Any path in the repository (a linked
/// worktree, a subdirectory) names the same directory, because it is computed from the main
/// checkout [`repo_of_worktree`] finds.
pub fn landing_dir_in(root: &Path, repo: &Path) -> PathBuf {
    use sha2::Digest;
    let canonical = repo_of_worktree(repo)
        .unwrap_or_else(|| std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf()));
    let digest = sha2::Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    let base = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "repo".into());
    root.join(format!("{base}-{hex}"))
}

/// The pre-2026-10-11 place: `<git-common-dir>/letibot-landing`. Only ever read to migrate away.
pub const OLD_LANDING_NAME: &str = "letibot-landing";

/// **The queue's own worktree for landing and for gating the target** — under `root`
/// ([`landing_root`] in the daemon, so letibot's data directory; [`landing_dir_in`]), never the operator's checkout, never a branch's own tree, and
/// never inside the repository's `.git`. Created detached on first use and reused (its `target/`
/// build cache with it), reset clean to `at` every time.
///
/// It is still a `git worktree` of the repository, so a landing stays a compare-and-swap on
/// `refs/heads/<target>` in the repository itself ([`update_ref_cas`]); git's own bookkeeping for
/// it (`.git/worktrees/<name>`) is the one thing in `.git` it implies.
///
/// Why not `.git/letibot-landing` any more — the operator, 2026-10-11: *"the merges and branches
/// shoudlnt land into .git. look at firecode sission - it fights exactly this"*. The firecode
/// session's own finding: *"The queue runs the gate in a landing worktree,
/// `.git/letibot-landing` … A git worktree doesn't carry gitignored files, so that tree has no
/// `images/` and no `vendor/` — no guest rootfs, no kernel, no firecracker. Every check that boots
/// a VM fails, which is precisely the failed list."* — and *"`.git` is on the never-write list —
/// I can't inspect the landing worktree, and no grant reaches it."* The tree is now somewhere a
/// session can be granted to read (`Harness`'s confined view binds it read-only), and the
/// ignored inputs a gate needs are carried in by `[queue] gate_carry` (`mergequeue::carry`).
pub fn landing_worktree_in(root: &Path, repo: &Path, at: &str) -> Result<PathBuf, String> {
    guard_not_real(root);
    let path = landing_dir_in(root, repo);
    let usable = path.join(".git").exists() && git(&path, &["rev-parse", "HEAD"]).is_ok();
    if !usable {
        migrate_old_landing(repo);
        let _ = git(repo, &["worktree", "prune"]);
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
            let _ = git(repo, &["worktree", "prune"]);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        git(
            repo,
            &["worktree", "add", "--detach", &path.to_string_lossy(), at],
        )?;
    }
    // A rebase a killed gate left half-done, then the tree reset to `at`. `clean -fd` and not
    // `-fdx`: the ignored `target/` is the gate's build cache and is worth minutes (and the
    // carried `gate_carry` paths are ignored too, so an unchanged file is not copied again).
    let _ = git(&path, &["rebase", "--abort"]);
    git(&path, &["checkout", "--detach", "--force", at])?;
    git(&path, &["reset", "--hard", at])?;
    git(&path, &["clean", "-fd"])?;
    Ok(path)
}

/// **Move an old `<git-common-dir>/letibot-landing` worktree away**, the first time the new one
/// is made: `git worktree remove --force`, then `prune`, said in the daemon's log. Nothing else in
/// `.git` is touched, and a remove git refuses is said and left — never `rm -rf` inside `.git`.
/// Returns what was said, for the tests.
pub fn migrate_old_landing(repo: &Path) -> Option<String> {
    let common = git(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    let old = PathBuf::from(common.trim()).join(OLD_LANDING_NAME);
    if !old.exists() {
        return None;
    }
    let said = match git(
        repo,
        &["worktree", "remove", "--force", &old.to_string_lossy()],
    ) {
        Ok(_) => {
            let _ = git(repo, &["worktree", "prune"]);
            format!(
                "removed the old landing worktree {} (landing now happens outside `.git`)",
                old.display()
            )
        }
        Err(e) => format!(
            "the old landing worktree {} could NOT be removed, and is left as it is: {e}",
            old.display()
        ),
    };
    eprintln!("  merge queue: {said}");
    Some(said)
}

/// **Rebase `head` onto `onto` in the landing worktree**, detached; the rebased sha, or the
/// conflict in git's words (the rebase aborted).
pub fn rebase_in(worktree: &Path, head: &str, onto: &str) -> Result<String, String> {
    git(worktree, &["checkout", "--detach", "--force", head])?;
    if let Err(e) = git(worktree, &["rebase", onto]) {
        let _ = git(worktree, &["rebase", "--abort"]);
        return Err(e);
    }
    rev_parse(worktree, "HEAD").ok_or_else(|| "the rebased HEAD has no sha".to_string())
}

/// **Move `refs/heads/<target>` from `expect` to `new`, or learn that it moved** — git's own
/// compare-and-swap (`update-ref <ref> <new> <old>`).
pub fn update_ref_cas(repo: &Path, target: &str, new: &str, expect: &str) -> Result<(), String> {
    git(
        repo,
        &[
            "update-ref",
            "-m",
            "letibot merge queue: land",
            &format!("refs/heads/{target}"),
            new,
            expect,
        ],
    )
    .map(|_| ())
}

/// The checkouts that have `refs/heads/<target>` checked out.
pub fn checkouts_of(repo: &Path, target: &str) -> Vec<PathBuf> {
    let Ok(list) = git(repo, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut path: Option<PathBuf> = None;
    for line in list.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if line == format!("branch refs/heads/{target}")
            && let Some(p) = path.take()
        {
            out.push(p);
        }
    }
    out
}

/// **Bring every checkout of `target` from `old` to `new` the way a fast-forward would** —
/// `read-tree -m -u old new`, which refuses when a local change overlaps the landing, exactly
/// as `merge --ff-only` does. Without this the ref moves under a checkout whose index still holds
/// the old tree, and the operator's next commit would record the old tree — reverting the
/// landing. `Err` names the checkout that could not follow; the caller undoes the landing.
pub fn follow_checkouts(repo: &Path, target: &str, old: &str, new: &str) -> Result<(), String> {
    for wt in checkouts_of(repo, target) {
        git(&wt, &["read-tree", "-m", "-u", old, new]).map_err(|e| {
            format!(
                "the checkout of `{target}` at {} could not follow the landing: {e}",
                wt.display()
            )
        })?;
    }
    Ok(())
}

/// **What a push came to** — the row keeps the sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pushed {
    /// Pushed to `origin`.
    Yes(String),
    /// The repository has no `origin`: nothing to push to, which is the local case.
    NoRemote,
    /// The push failed, in git's words.
    Failed(String),
}

/// **Push `sha` to `origin`'s `target`** — a fast-forward or a refusal, never a force. The
/// credential is whatever git's own helper supplies for this one invocation.
pub fn push(repo: &Path, target: &str, sha: &str) -> Pushed {
    if git(repo, &["remote", "get-url", "origin"]).is_err() {
        return Pushed::NoRemote;
    }
    match git(
        repo,
        &["push", "origin", &format!("{sha}:refs/heads/{target}")],
    ) {
        Ok(_) => Pushed::Yes(format!("pushed {sha} to origin/{target}")),
        Err(e) => Pushed::Failed(e),
    }
}

/// **A fresh clone of the branch for a reviewer** (decision 3) — the branch alone, at `head`,
/// in `dir`: what a VM is booted on instead of the workspace (72.8 GiB, refused by the 8 GiB
/// ceiling, 8 of 21 reviews on 2026-10-11). History comes with it, so `git diff base...head`
/// works inside.
pub fn clone_branch(repo: &Path, branch: &str, head: &str, dir: &Path) -> Result<(), String> {
    if dir.exists() {
        let _ = std::fs::remove_dir_all(dir);
    }
    let out = Command::new("git")
        .args([
            "clone",
            "--quiet",
            "--no-local",
            "--single-branch",
            "--branch",
        ])
        .arg(branch)
        .arg(repo)
        .arg(dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git clone: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git clone of `{branch}`: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    git(dir, &["checkout", "--quiet", "--detach", head]).map(|_| ())
}

/// **A file the daemon itself wrote into a workspace** — `.letibot/<name>.jsonl`, the logs older
/// builds kept there (`delivery-log`, `context-claims`, `notes-offers`; R92.4 moved them to the
/// data directory). Not the child's work, so it neither keeps a tree nor is the child nudged
/// about it. `path` is a porcelain path, relative to the tree.
pub fn daemon_leftover(path: &str) -> bool {
    let path = path.trim().trim_matches('"');
    path.strip_prefix(".letibot/")
        .is_some_and(|rest| !rest.contains('/') && rest.ends_with(".jsonl"))
}

/// **`git status --porcelain`'s lines that are the tree's own work** — every untracked file named
/// (`-uall`, so a directory of the daemon's logs is not one opaque `?? .letibot/`), less
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

/// Remove a branch's own worktree and the branch, after a landing — only when the tree holds
/// nothing uncommitted (the daemon's own leftover logs are not the tree's work). `Err` says what
/// was kept and why.
pub fn retire_branch(repo: &Path, branch: &str, worktree: Option<&str>) -> Result<(), String> {
    if let Some(wt) = worktree {
        let wt = Path::new(wt);
        if wt.exists() {
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
        }
        let _ = git(repo, &["worktree", "prune"]);
    }
    git(repo, &["branch", "-D", branch]).map(|_| ())
}

#[cfg(test)]
mod tests;
