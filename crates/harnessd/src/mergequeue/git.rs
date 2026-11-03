//! Git, as the queues use it: every command is one `git -C <dir> …` with git's own words on
//! failure.

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
fn words(args: &[&str]) -> String {
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

/// The change a head carries over `onto`, as one stable patch id - what tells *the branch was
/// rebased* from *the branch moved*. The diff runs from the merge base to `head`, so two heads
/// carrying one change over different bases answer the same id.
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

/// The top of the checkout `path` is in.
pub fn repo_root(path: &Path) -> Result<PathBuf, String> {
    let out = git(path, &["rev-parse", "--show-toplevel"])?;
    let top = out.trim();
    if top.is_empty() {
        return Err(format!("{} is not inside a git repository", path.display()));
    }
    Ok(PathBuf::from(top))
}

/// The repository a workspace means - itself, a same-named child (the operator's
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

/// The repositories a workspace is a session of: the one [`repo_of_workspace`] reads it as
/// or every child of a folder that is ONE PROJECT - two or more children whose `origin`
/// remotes share one host and owner.
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

/// The host and owner a remote URL names - `("github.com", "stroppy-io")` for
/// `https://github.com/stroppy-io/stroppy.git` and `git@github.com:stroppy-io/stroppy.git` alike
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

/// The queues' name for a repository - the canonical path of its main checkout, so two daemons
/// seated at two depths of one repository agree on one lease.
pub fn repository_key(any_path_in_it: &Path) -> Option<String> {
    repo_of_worktree(any_path_in_it).map(|p| p.display().to_string())
}

/// The target branch a repository lands on when the project names none: `main` when it exists
/// else `master`, else whatever the main checkout has out.
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

/// The letibot data directory - `$XDG_DATA_HOME/letibot`, else `~/.local/share/letibot`, where
/// `sessions.db` and `metrics.db` already live. `None` in a bare environment with neither.
pub fn letibot_data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|d| d.join("letibot"))
}

/// The environment variable that puts the landing root somewhere else - what a test that spawns
/// a daemon sets, so no test ever writes into the box's real `~/.local/share/letibot`.
pub const LANDING_ROOT_ENV: &str = "LETIBOT_LANDING_ROOT";

/// Where every repository's landing worktree lives: `$LETIBOT_LANDING_ROOT`, else `<data
/// dir>/landing`. Read once by the daemon; every other caller is handed the root it uses.
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

/// A test must never make anything in the box's real `~/.local/share/letibot`: under `cfg(test)`
/// a landing or a grant under the real data directory panics. Nothing in a build.
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

/// The landing directory of `repo` under `root`: `<root>/<basename>-<12 hex of sha256 of the
/// repository's canonical path>`. The basename is for a person reading the directory, the hash
/// keeps two repositories of one name apart, and any path in the repository names the same one.
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

/// The old place: `<git-common-dir>/letibot-landing`. Only ever read to migrate away.
pub const OLD_LANDING_NAME: &str = "letibot-landing";

/// The queue's own worktree for landing and for gating the target: under the daemon's
/// landing root, never the operator's checkout and never inside the repository's `.git`.
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

/// Move an old `<git-common-dir>/letibot-landing` worktree away the first time the new one is made
/// `git worktree remove --force`, then `prune`, said in the daemon's log. A remove git refuses is
/// said and left - never `rm -rf` inside `.git`. Returns what was said, for the tests.
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

/// What a replay refused: two facts the queue may not fold into one bare *conflict* - git refused a
/// patch, or the queue refused to guess which commits were the branch's at all.
#[derive(Debug)]
pub enum RebaseError {
    /// Git refused the replay: a commit of the branch's own does not apply to the new base. The
    /// rebase was aborted and the landing tree is back on the branch's head.
    Conflict(String),
    /// Nothing was replayed, and the sentence says why - no base, no commit of the branch's own, or
    /// a replay that would have changed what it says. A person has to act, so it is not dressed up
    /// as a conflict git could have resolved.
    Refused(String),
}

impl RebaseError {
    /// The row's sentence: what happened to `branch` on `target` at `tip`, in its own words.
    pub fn sentence(&self, branch: &str, target: &str, tip: &str) -> String {
        match self {
            RebaseError::Conflict(said) => {
                format!("`{branch}` does not rebase onto `{target}` at {tip}: {said}")
            }
            RebaseError::Refused(said) => {
                format!("`{branch}` was not rebased onto `{target}` at {tip}: {said}")
            }
        }
    }
}

/// Rebase the branch's own commits, and nothing else, onto `onto` - in the landing worktree
/// detached; the rebased sha, or a [`RebaseError`].
pub fn rebase_in(
    worktree: &Path,
    head: &str,
    onto: &str,
    target: &str,
    bases: &[String],
) -> Result<String, RebaseError> {
    let fork = fork_point(worktree, head, target, bases)?;
    let own = own_commits(worktree, &fork, head)?;
    if own.is_empty() {
        return Err(RebaseError::Refused(format!(
            "`{head}` carries no commit of its own over {fork}, so there is nothing to replay onto \
             {onto}; rebase it by hand"
        )));
    }
    git(worktree, &["checkout", "--detach", "--force", head]).map_err(RebaseError::Conflict)?;
    // When every non-merge commit in `fork..head` is one of the branch's own, the branch is
    // linear over its base and `--onto` replays exactly that list. A branch that merged the
    // old target into itself is not linear
    let in_range = commits_in(worktree, &fork, head, false)?;
    if in_range.len() == own.len() {
        if let Err(e) = git(worktree, &["rebase", "--onto", onto, &fork]) {
            let _ = git(worktree, &["rebase", "--abort"]);
            let _ = git(worktree, &["checkout", "--detach", "--force", head]);
            return Err(RebaseError::Conflict(e));
        }
    } else if let Err(e) = replay_each(worktree, onto, head, &own) {
        return Err(RebaseError::Conflict(e));
    }
    let Some(rebased) = rev_parse(worktree, "HEAD") else {
        return Err(RebaseError::Refused(
            "the rebased HEAD has no sha".to_string(),
        ));
    };
    // The patches are the branch's, before and after. A replay that alters what the branch says is
    // the failure all of the above exists to prevent, so it is checked rather than trusted.
    if let Err(said) = patches_unchanged(worktree, (&fork, head), (onto, &rebased)) {
        let _ = git(worktree, &["checkout", "--detach", "--force", head]);
        return Err(RebaseError::Refused(format!(
            "the replay onto {onto} would have changed what the branch says ({said}), so it is not \
             landed"
        )));
    }
    Ok(rebased)
}

/// The commit the branch was cut from - the newest base somebody remembers, on the branch's
/// own first-parent spine
fn fork_point(
    repo: &Path,
    head: &str,
    target: &str,
    bases: &[String],
) -> Result<String, RebaseError> {
    let mut known: Vec<String> = bases
        .iter()
        .map(|b| b.trim())
        .filter(|b| !b.is_empty())
        .map(str::to_string)
        .collect();
    // `rev-list -g` answers nothing (exit 0) for a ref with no reflog, which is a fact about the
    // repository and not an error: the recorded bases above are then all there is.
    if let Ok(log) = git(repo, &["rev-list", "-g", &format!("refs/heads/{target}")]) {
        known.extend(
            log.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string),
        );
    }
    let spine = git(repo, &["rev-list", "--first-parent", head]).map_err(|e| {
        RebaseError::Refused(format!("the history of `{head}` could not be read: {e}"))
    })?;
    for line in spine.lines() {
        let at = line.trim();
        if known.iter().any(|k| k == at) {
            return Ok(at.to_string());
        }
    }
    Err(RebaseError::Refused(format!(
        "could not find the branch's own base; rebase it by hand — nothing `{head}` was cut from is \
         remembered (neither the base the row recorded nor any commit `{target}` has pointed at is \
         on `{head}`'s first-parent chain), and replaying the whole chain would replay `{target}`'s \
         old history"
    )))
}

/// The branch's own commits, oldest first: first-parent, merges aside, over `fork`.
fn own_commits(repo: &Path, fork: &str, head: &str) -> Result<Vec<String>, RebaseError> {
    commits_in(repo, fork, head, true)
}

/// Every commit in `fork..head` that is not a merge - `first_parent` narrows it to the branch's own
/// spine (see [`fork_point`]); without it this is the set `git rebase --onto` would replay.
fn commits_in(
    repo: &Path,
    fork: &str,
    head: &str,
    first_parent: bool,
) -> Result<Vec<String>, RebaseError> {
    let mut args = vec!["rev-list", "--no-merges", "--reverse"];
    if first_parent {
        args.push("--first-parent");
    }
    let range = format!("{fork}..{head}");
    args.push(&range);
    let out = git(repo, &args).map_err(|e| {
        RebaseError::Refused(format!(
            "the commits of `{head}` over {fork} could not be read: {e}"
        ))
    })?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Replay exactly these commits, oldest first, on a detached `onto` - the path for a branch that
/// merged the old target into itself, where `--onto` would replay the commits those merges brought
/// in as well and conflict in files the branch never touched.
fn replay_each(worktree: &Path, onto: &str, head: &str, own: &[String]) -> Result<(), String> {
    git(worktree, &["checkout", "--detach", "--force", onto])?;
    for commit in own {
        if let Err(e) = git(worktree, &["cherry-pick", commit]) {
            if an_empty_pick(worktree) {
                // The new base already carries this change: `git rebase` drops such a commit, and
                // so does the queue.
                let _ = git(worktree, &["cherry-pick", "--skip"]);
                continue;
            }
            let _ = git(worktree, &["cherry-pick", "--abort"]);
            let _ = git(worktree, &["checkout", "--detach", "--force", head]);
            return Err(e);
        }
    }
    Ok(())
}

/// A pick that came out empty - the sequencer stopped on a commit and left a tree with nothing in
/// it: no unmerged path, an index equal to `HEAD`. That is `git rebase`'s `--empty=drop` said in
/// cherry-pick's dialect.
fn an_empty_pick(repo: &Path) -> bool {
    rev_parse(repo, "CHERRY_PICK_HEAD").is_some()
        && git(repo, &["ls-files", "-u"])
            .map(|s| s.trim().is_empty())
            .unwrap_or(false)
        // `--quiet` exits 1 when the index differs from HEAD, which `git` reports as an `Err`.
        && git(repo, &["diff", "--cached", "--quiet"]).is_ok()
}

/// The replay did not change what the branch says - `git range-diff` of the branch's own
/// commits before and after: `=` is the same patch, and `<` a commit the new base already
/// carries (git drops those).
fn patches_unchanged(repo: &Path, before: (&str, &str), after: (&str, &str)) -> Result<(), String> {
    if after.0 == after.1 {
        // Every one of the branch's commits came out empty, so there is no series to compare and
        // nothing the branch says has changed.
        return Ok(());
    }
    let out = git(
        repo,
        &[
            "range-diff",
            "--no-color",
            &format!("{}..{}", before.0, before.1),
            &format!("{}..{}", after.0, after.1),
        ],
    )?;
    for line in out.lines() {
        // `1: 7c361a6 = 1: 20e25b1 subject` - the third field compares the two patches.
        if !matches!(line.split_whitespace().nth(2), Some("=") | Some("<")) {
            return Err(line.trim().to_string());
        }
    }
    Ok(())
}

/// Move `refs/heads/<target>` from `expect` to `new`, or learn that it moved - git's own
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

/// Bring every checkout of `target` from `old` to `new` the way a fast-forward would -
/// `read-tree -m -u old new`, which refuses when a local change overlaps the landing.
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

/// What a push came to - the row keeps the sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pushed {
    /// Pushed to `origin` - the sentence says whose credential answered.
    Yes(String),
    /// The repository has no `origin`: nothing to push to, which is the local case.
    NoRemote,
    /// The push failed: git's words, and whose credential was offered when it was `gh`'s.
    Failed(String),
}

/// The helper `gh` installs for git, named on the command line rather than inherited.
const GH_HELPER: &str = "!gh auth git-credential";

/// The words git is given for one push, and whether `gh`'s helper is among them. Apart from
/// [`push`] because the argv is the whole of *whose credential answers*, and a test can
/// check it without a network.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PushPlan {
    /// After `git -C <repo>`: the `-c` words first, then `push`, the destination, the refspec.
    args: Vec<String>,
    /// True when `gh`'s helper is named - every sentence below is built from this.
    gh: bool,
}

fn push_plan(url: &str, target: &str, sha: &str, gh_logged_in: bool) -> PushPlan {
    let refspec = format!("{sha}:refs/heads/{target}");
    match github_https(url).filter(|_| gh_logged_in) {
        // The empty `helper=` first, or the named one is never asked: `credential.helper` is a list
        // and a configured helper earlier in it answers first. An empty value resets the list.
        Some(https) => PushPlan {
            args: vec![
                "-c".into(),
                "credential.helper=".into(),
                "-c".into(),
                format!("credential.helper={GH_HELPER}"),
                "push".into(),
                https,
                refspec,
            ],
            gh: true,
        },
        None => PushPlan {
            args: vec!["push".into(), "origin".into(), refspec],
            gh: false,
        },
    }
}

/// The row's sentence for a push that happened - which way it went, and whose credential answered.
fn pushed_said(sha: &str, target: &str, plan: &PushPlan) -> String {
    if plan.gh {
        format!(
            "pushed {sha} to origin/{target} over https on gh's credential (`{GH_HELPER}`, \
             inherited helpers cleared)"
        )
    } else {
        format!("pushed {sha} to origin/{target} on whatever credential git's own config holds")
    }
}

/// The row's sentence for a push that was refused: git's own words, plus - when `gh` answered -
/// that it did, so *no access for this account* is not read as *no credential at all*.
fn failed_said(e: &str, plan: &PushPlan) -> String {
    if plan.gh {
        format!(
            "{e} — the credential offered was gh's (`{GH_HELPER}`, inherited helpers cleared), so \
             this refusal is about the account gh is logged in as, not a missing credential"
        )
    } else {
        e.to_string()
    }
}

/// Push `sha` to `origin`'s `target` - a fast-forward or a refusal, never a force. Every
/// GitHub remote is pushed on `gh`'s credential by name, both spellings of the remote and
/// an https one included, whenever `gh` is logged in
pub fn push(repo: &Path, target: &str, sha: &str) -> Pushed {
    let Ok(url) = git(repo, &["remote", "get-url", "origin"]) else {
        return Pushed::NoRemote;
    };
    let plan = push_plan(url.trim(), target, sha, gh_authenticated());
    let args: Vec<&str> = plan.args.iter().map(String::as_str).collect();
    match git(repo, &args) {
        Ok(_) => Pushed::Yes(pushed_said(sha, target, &plan)),
        Err(e) => Pushed::Failed(failed_said(&e, &plan)),
    }
}

/// The https url of a GitHub remote, whatever spelling it arrived in - the form `gh`'s
/// credential helper answers for. `None` for another host, or a url naming no repository.
pub fn github_https(url: &str) -> Option<String> {
    let url = url.trim();
    // A scheme separates host from path with `/`; scp's `git@host:path` with `:`.
    let (with_scheme, rest) = match url.split_once("://") {
        Some((_, after)) => (true, after),
        None => (false, url.strip_prefix("git@")?),
    };
    let rest = rest.split_once('@').map_or(rest, |(_, after)| after);
    let (host, path) = if with_scheme {
        rest.split_once('/')?
    } else {
        rest.split_once(':')?
    };
    let host = host.split(':').next().unwrap_or(host);
    let path = path.trim_start_matches('/');
    if host != "github.com" || path.is_empty() {
        return None;
    }
    let path = if path.ends_with(".git") {
        path.to_string()
    } else {
        format!("{path}.git")
    };
    Some(format!("https://github.com/{path}"))
}

/// Is `gh` installed and authenticated? Asked on every push, not remembered: a logged-out `gh` is a
/// fact about the box that can change under a long-lived daemon, and a remembered `true` would turn
/// that into an authentication failure in the middle of a landing.
fn gh_authenticated() -> bool {
    Command::new("gh")
        .args(["auth", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A fresh clone of the branch alone at `head` - what a VM is booted on instead of the workspace
/// with history, so `git diff base...head` works inside.
///
/// One tree per attempt, named by a rename: `stem` is the row's own stem and the tree made here is
/// `stem-<pid>-<n>`, returned because the caller must know which tree this attempt got. The clone
/// is written to `.<name>.tmp` beside it and renamed on, so a reader sees no tree or a complete one.
///
/// Nothing is ever removed here. A row re-seated while its previous reviewer is still alive must
/// not have its tree cloned out from under it, which is what one path per row cost. The token is
/// `<pid>-<n>` because the reviewer's handle does not exist yet - its working directory is decided
/// before it is spawned - and the pid is the one fact about a tree another process can check.
///
/// The failure that shape fixes is the clone itself
///
/// ```text
/// git clone of `agent/a-one-line-citation-is-filled`:
///   fatal: could not open '…/letibot-review-clones/review-s-…-sub-1793298319066201000/.git/objects/pack/tmp_pack_cgbKfF'
///          for reading: No such file or directory
///   fatal: fetch-pack: invalid index-pack output
/// ```
/// - a clone whose own directory was removed under it, because a second attempt at the same row
///   removed it before cloning into it.
pub fn clone_branch(repo: &Path, branch: &str, head: &str, stem: &Path) -> Result<PathBuf, String> {
    let parent = stem.parent().unwrap_or(Path::new(""));
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("the reviewers' clone directory `{}`: {e}", parent.display()))?;
    }
    let (dir, tmp) = attempt_beside(stem)?;
    // A daemon killed mid-clone leaves one of these behind, and a reused pid would collide with it.
    let _ = std::fs::remove_dir_all(&tmp);
    if let Err(e) = clone_into(repo, branch, head, &tmp) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &dir) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "installing the reviewer's clone at `{}`: {e}",
            dir.display()
        ));
    }
    Ok(dir)
}

/// The clone itself, into a path nobody else knows about - [`clone_branch`]'s first half.
fn clone_into(repo: &Path, branch: &str, head: &str, dir: &Path) -> Result<(), String> {
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

/// The two names one attempt uses, neither of which any other attempt can be writing -
/// `<stem>-<pid>-<n>` for the tree and `.<stem>-<pid>-<n>.tmp` for the clone being made.
fn attempt_beside(stem: &Path) -> Result<(PathBuf, PathBuf), String> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let leaf = stem
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("`{}` names no directory to clone into", stem.display()))?;
    for _ in 0..8 {
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("{leaf}-{}-{n}", std::process::id());
        let dir = stem.with_file_name(&name);
        // A pid reused from a killed daemon can already have this name; the next counter is free.
        if !dir.exists() {
            return Ok((dir, stem.with_file_name(format!(".{name}.tmp"))));
        }
    }
    Err(format!("every name beside `{}` is taken", stem.display()))
}

/// A file the daemon itself wrote into a workspace - `.letibot/<name>.jsonl`, the logs older
/// builds kept there. Not the child's work, so it neither keeps a tree nor is the child nudged about
/// it. `path` is a porcelain path, relative to the tree.
pub fn daemon_leftover(path: &str) -> bool {
    let path = path.trim().trim_matches('"');
    path.strip_prefix(".letibot/")
        .is_some_and(|rest| !rest.contains('/') && rest.ends_with(".jsonl"))
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
fn linked_gitdir(wt: &Path) -> Option<PathBuf> {
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

/// Remove a branch's own worktree and the branch, after a landing - only when the tree
/// holds nothing uncommitted (the daemon's own leftover logs are not the tree's work).
/// `Err` says what was kept and why. A landing is a proof, and it is the caller's
pub fn retire_branch(repo: &Path, branch: &str, worktree: Option<&str>) -> Result<(), String> {
    if let Some(wt) = worktree {
        remove_worktree(repo, Path::new(wt), branch)?;
    }
    delete_branch(repo, branch)
}

/// Remove a superseded branch's worktree and the branch, but only when there is nothing in
/// them to lose - no commit over `base`, and (through [`retire_branch`]) nothing
/// uncommitted.
pub fn retire_if_empty(
    repo: &Path,
    base: &str,
    branch: &str,
    worktree: Option<&str>,
) -> Result<(), String> {
    if let Some(head) = branch_head(repo, branch) {
        match commit_on(repo, &head, base, base, &format!("`{branch}`")) {
            Proof::On | Proof::Gone => {}
            Proof::Unproven(_, why) => {
                return Err(format!(
                    "{why} — the work is not this tip's, but it may be worth putting somewhere else"
                ));
            }
        }
    }
    retire_branch(repo, branch, worktree)
}

/// How long a directory under a worktrees home is left alone: a child's tree is made before its
/// branch is registered, and a sweep that ran between the two would delete a live child's work.
pub const ORPHAN_GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// What a worktree sweep found: the debris it took, and what it left with a reason.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reaped {
    /// Registrations git held for a directory that is not there, now pruned.
    pub pruned: Vec<String>,
    /// Directories under a worktrees home that are no checkout's worktree any more.
    pub orphans: Vec<String>,
    /// Directories left alone, and the sentence saying why.
    pub kept: Vec<(String, String)>,
}

impl Reaped {
    /// Whether anything went - a sweep that took nothing says nothing.
    pub fn took_anything(&self) -> bool {
        !self.pruned.is_empty() || !self.orphans.is_empty()
    }

    /// One line for the daemon's log: what went, and what was kept and why.
    pub fn words(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.pruned.is_empty() {
            parts.push(format!(
                "pruned {} registration(s) for trees that are gone ({})",
                self.pruned.len(),
                self.pruned.join(", ")
            ));
        }
        if !self.orphans.is_empty() {
            parts.push(format!(
                "removed {} orphaned tree(s) ({})",
                self.orphans.len(),
                self.orphans.join(", ")
            ));
        }
        for (dir, why) in &self.kept {
            parts.push(format!("{dir} kept: {why}"));
        }
        parts.join("; ")
    }
}

/// **The debris nothing else reaps**: a registration git holds for a directory that is not there,
/// and a directory under a worktrees home whose `.git` names a gitdir that is not - a tree of a
/// checkout that is gone. Neither can hold a commit, so neither is the resource sweep's rule
/// ([`remove_worktree`] keeps what it cannot prove is released): there is nothing inside to prove.
/// A tree that is there, a directory that is a repository of its own, and another checkout's living
/// worktree under the same folder are all kept, and said.
pub fn reap_worktrees(repo: &Path, now: std::time::SystemTime) -> Result<Reaped, String> {
    let listed = git(repo, &["worktree", "list", "--porcelain"])?;
    let mut registered: Vec<(String, bool)> = Vec::new();
    for block in listed.split("\n\n") {
        let (mut path, mut locked, mut bare) = (None, false, false);
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(p.trim().to_string());
            } else if line == "locked" || line.starts_with("locked ") {
                locked = true;
            } else if line == "bare" {
                bare = true;
            }
        }
        if let Some(p) = path.filter(|_| !bare) {
            registered.push((p, locked));
        }
    }
    let main = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let mut out = Reaped::default();
    for (path, locked) in &registered {
        let dir = Path::new(path);
        let itself = dir == repo || std::fs::canonicalize(dir).is_ok_and(|c| c == main);
        if itself || dir.exists() {
            continue;
        }
        // Locked is why `git worktree prune` alone would leave it: it refuses a locked entry, and
        // an entry for a directory that is gone is a lock over nothing.
        if *locked {
            let _ = git(repo, &["worktree", "unlock", path]);
        }
        out.pruned.push(path.clone());
    }
    if !out.pruned.is_empty() {
        // `--expire now` is the point of this call: by default a missing entry is kept for months.
        git(repo, &["worktree", "prune", "--expire", "now"])?;
    }
    for home in worktree_homes(repo) {
        let Ok(entries) = std::fs::read_dir(&home) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let name = dir.display().to_string();
            if registered.iter().any(|(p, _)| Path::new(p) == dir) {
                continue;
            }
            match worktree_gitdir(&dir) {
                GitDir::OwnRepository => out.kept.push((name, "it is a repository of its own".into())),
                GitDir::OfAnotherCheckout(where_) => out.kept.push((
                    name,
                    format!("its `.git` names {where_}, which is there: it is that checkout's"),
                )),
                GitDir::Nothing => out.kept.push((name, "it has no `.git`".into())),
                GitDir::RegistrationGone(gone) => {
                    let young = std::fs::metadata(&dir)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|m| now.duration_since(m).ok())
                        .is_some_and(|age| age < ORPHAN_GRACE);
                    if young {
                        out.kept.push((name, format!("younger than {} s", ORPHAN_GRACE.as_secs())));
                    } else {
                        std::fs::remove_dir_all(&dir)
                            .map_err(|e| format!("removing {}: {e}", dir.display()))?;
                        out.orphans.push(format!("{} (its `.git` names {gone}, which is not there)", name));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// The folders a repository's agent worktrees are made in - [`worktree_home`]'s answer, and the
/// repository's own `.claude/worktrees` (the shape `task_start` uses when the layout says nothing).
fn worktree_homes(repo: &Path) -> Vec<PathBuf> {
    let mut homes = vec![repo.join(".claude").join("worktrees")];
    if let Some(home) = letibot_tools::builtins::task::worktree_home(repo) {
        homes.push(home);
    }
    homes.sort();
    homes.dedup();
    homes
}

/// What a directory's `.git` says it is, for the sweep's decision.
enum GitDir {
    /// A `.git` directory inside it: a repository of its own, never a worktree's.
    OwnRepository,
    /// Its registration exists: another checkout's worktree, alive where it is written.
    OfAnotherCheckout(String),
    /// Its registration is gone - a tree of a checkout that is not there any more.
    RegistrationGone(String),
    /// No `.git` at all, or one this build cannot read.
    Nothing,
}

fn worktree_gitdir(dir: &Path) -> GitDir {
    let dot = dir.join(".git");
    if dot.is_dir() {
        return GitDir::OwnRepository;
    }
    let Ok(text) = std::fs::read_to_string(&dot) else {
        return GitDir::Nothing;
    };
    let Some(named) = text.trim().strip_prefix("gitdir:") else {
        return GitDir::Nothing;
    };
    let named = named.trim();
    if named.is_empty() {
        return GitDir::Nothing;
    }
    let path = Path::new(named);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    };
    if path.exists() {
        GitDir::OfAnotherCheckout(path.display().to_string())
    } else {
        GitDir::RegistrationGone(path.display().to_string())
    }
}

#[cfg(test)]
mod tests;
