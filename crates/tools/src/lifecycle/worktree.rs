//! Owned worktrees and branches: make one row-first, prove one is an owner's, keep what it holds
//! in `refs/letibot/archive/<owner>/`, and only then remove it (`docs/lifecycle.md` d.4, d.8).
//!
//! Two properties shape every function here. A worktree is `work`: it may hold the only copy of
//! something a person wants, so a step that removes one ends by reading back the archive it wrote,
//! and a step that cannot is `stuck`, never a delete. And the proof that a worktree is an owner's
//! lives OUTSIDE the tree the agent works in - a file in the worktree's own git directory and a key
//! in the repository's config - because a name in a namespace shared with people is a candidate
//! and never an answer.

use std::path::{Path, PathBuf};

use super::{Places, archive, now_ms};
use crate::git::{self, git};

/// The file a worktree's git directory carries: the owner id, this box, and when it was stamped.
/// Beside git's own `gitdir` and `HEAD`, which is the one place an agent's tree cannot reach.
pub const OWNER_FILE: &str = "letibot-owner";

/// The repository-config key a branch letibot made carries: `branch.<name>.letibot-owner`.
pub const STAMP_KEY: &str = "letibot-owner";

/// What a dirty tree may write into the object store before the TREE is kept instead (d.8 step 4):
/// a build output dropped into a tree by mistake must not become history.
pub const WIP_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// `pk/<sid8>/<name>`: the branch a package's work is on (d.4's naming table). `<sid8>` is the same
/// eight characters a `remote/<host>/<id>` branch uses, so one owner reads the same in both.
pub fn branch_name(owner: &str, package: &str) -> String {
    format!("pk/{}/{package}", crate::remote::short_branch_id(owner))
}

/// A package name as a path segment: letters, digits, `-`, `_` and `.`, and nothing that could
/// climb out of the owner root it is about to be created in.
pub fn package_dir(package: &str) -> Result<String, String> {
    let name: String = package
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches(['-', '.']).to_string();
    if name.is_empty() {
        return Err(format!(
            "`{package}` is not a name a directory can be made of: give it letters or digits"
        ));
    }
    Ok(name)
}

/// What making or adopting one left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// The worktree's own path.
    pub path: PathBuf,
    /// The branch it is on.
    pub branch: String,
}

/// Create `<gc root>/<box>/<owner>/wt/<package>` on branch `pk/<sid8>/<package>` from `repo`'s
/// `base`, then stamp both. The caller writes the owner ROW first: a row with no resource is
/// harmless, a resource with no row is the leak this package exists to stop (d.2).
pub fn make(
    places: &Places,
    owner: &str,
    repo: &Path,
    package: &str,
    base: &str,
) -> Result<Place, String> {
    if !archive::is_repo(repo) {
        return Err(format!("{} is not a git checkout", repo.display()));
    }
    if git::repo_of_worktree(repo)
        .as_deref()
        .and_then(|r| std::fs::canonicalize(r).ok())
        != std::fs::canonicalize(repo).ok()
    {
        return Err(format!(
            "{} is itself a worktree: a package's worktree is cut from the repository a person \
             keeps, so name the main checkout",
            repo.display()
        ));
    }
    let name = package_dir(package)?;
    let branch = branch_name(owner, &name);
    if git::branch_head(repo, &branch).is_some() {
        return Err(format!(
            "the branch `{branch}` is already here: `{owner}` has a package called `{name}`, and \
             the branch a package works on is made once"
        ));
    }
    let root = places.owner_root(owner).join("wt");
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("could not make {}: {e}", root.display()))?;
    let path = root.join(&name);
    if path.exists() {
        return Err(format!(
            "{} already exists and is not a worktree this call made",
            path.display()
        ));
    }
    let base = if base.trim().is_empty() { "HEAD" } else { base };
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            &path.to_string_lossy(),
            base,
        ],
    )?;
    if let Err(why) = stamp(repo, &path, &branch, owner) {
        // A worktree with no stamp is one the next pass cannot prove: take it back rather than
        // leave a resource nobody can attribute.
        let _ = git(
            repo,
            &["worktree", "remove", "--force", &path.to_string_lossy()],
        );
        let _ = git::delete_branch(repo, &branch);
        return Err(why);
    }
    Ok(Place { path, branch })
}

/// Write the two stamps that prove this worktree and this branch are `owner`'s (d.4): the file in
/// the worktree's own git directory, and `branch.<name>.letibot-owner` in the repository's config.
/// Adopting a worktree that already exists is this call and nothing else.
pub fn stamp(repo: &Path, wt: &Path, branch: &str, owner: &str) -> Result<(), String> {
    let admin = git_admin_dir(wt)?;
    std::fs::create_dir_all(&admin)
        .map_err(|e| format!("could not make {}: {e}", admin.display()))?;
    let said = format!("{owner}\t{}\t{}", super::box_id(), now_ms());
    std::fs::write(admin.join(OWNER_FILE), said)
        .map_err(|e| format!("could not write {}: {e}", admin.join(OWNER_FILE).display()))?;
    git(
        repo,
        &["config", &format!("branch.{branch}.{STAMP_KEY}"), owner],
    )?;
    Ok(())
}

/// The owner a worktree's own stamp file names, when it is there and readable.
pub fn stamped_owner(wt: &Path) -> Option<String> {
    let admin = git_admin_dir(wt).ok()?;
    let text = std::fs::read_to_string(admin.join(OWNER_FILE)).ok()?;
    let owner = text.split_whitespace().next()?.to_string();
    (!owner.is_empty()).then_some(owner)
}

/// Why this worktree is provably `owner`'s, in one sentence - or `None`, which means *not proved*
/// and never *not ours*: the plan reports it and nothing removes it (d.4's rules).
pub fn proved(repo: &Path, wt: &Path, branch: Option<&str>, owner: &str) -> Option<String> {
    if stamped_owner(wt).as_deref() == Some(owner) {
        return Some(format!(
            "{} names its owner",
            git_admin_dir(wt).ok()?.join(OWNER_FILE).display()
        ));
    }
    if let Some(branch) = branch
        && git(
            repo,
            &["config", "--get", &format!("branch.{branch}.{STAMP_KEY}")],
        )
        .is_ok_and(|v| v.trim() == owner)
    {
        return Some(format!("`branch.{branch}.{STAMP_KEY}` names its owner"));
    }
    // d.4's last proof: the branch's creating entry is in THIS worktree's own HEAD log, written
    // after the stamp. A branch somebody else made never appears in this tree's log.
    let Some(branch) = branch else {
        return None;
    };
    let stamped = stamp_stamp_ms(wt)?;
    let log = std::fs::read_to_string(git_admin_dir(wt).ok()?.join("logs").join("HEAD")).ok()?;
    let created = log.lines().any(|l| {
        l.contains("branch: Created from") && reflog_ms(l).is_some_and(|ts| ts + 1_000 >= stamped)
    });
    created.then(|| {
        format!(
            "`{branch}` was made in this worktree's own HEAD log, after the stamp at {}",
            super::plan::stamp_of(stamped)
        )
    })
}

/// The worktrees under an owner root's `wt/`, as the discovery walk finds them.
pub fn under(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join("wt")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(".git").exists())
        .collect();
    out.sort();
    out
}

/// The branch a worktree is on - `None` for a detached HEAD, which is a fact a removal needs and
/// an inference must not guess.
pub fn branch_of(wt: &Path) -> Option<String> {
    git(wt, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
}

/// What one archive wrote, and where.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Archived {
    /// The refs written under the owner's namespace, read back by name.
    pub refs: Vec<String>,
    /// The commit carrying the tree's uncommitted work, when there was any.
    pub wip: Option<String>,
    /// The tree moved to the trash instead of being committed, when it was too big to commit.
    pub trash: Option<PathBuf>,
    /// The branch that stayed because another worktree has it checked out - said, not failed.
    pub branch_left: Option<String>,
}

impl Archived {
    /// One sentence for the journal and the plan.
    pub fn said(&self) -> String {
        let mut out = match (&self.trash, self.refs.len()) {
            (Some(t), _) => format!(
                "its tree is in {}: untracked content over {} is not written into the object \
                 store, and the tree itself is kept",
                t.display(),
                super::human_bytes(WIP_MAX_BYTES)
            ),
            (None, n) => match &self.wip {
                Some(w) => format!("{n} ref(s) and its uncommitted work ({})", short(w)),
                None => format!("{n} ref(s)"),
            },
        };
        if let Some(b) = &self.branch_left {
            out.push_str(&format!(
                "; `{b}` is checked out in another worktree, so the branch stays and its commits \
                 are in the archive"
            ));
        }
        out
    }
}

/// d.8 steps 1-5 for one worktree: the branch's ref, a detached HEAD's ref, the dirty tree as a
/// `wip` commit made with a SCRATCH index (the worktree's own index is never touched), the 512 MB
/// guard that moves the tree to `<gc root>/<box>/.trash/` instead of committing it, a read-back of
/// every ref, and only then the worktree - and the branch - removed.
pub fn archive_and_remove(
    places: &Places,
    repo: &Path,
    wt: &Path,
    owner: &str,
    branch: Option<&str>,
) -> Result<Archived, String> {
    if !git::is_worktree_of(wt, repo) {
        return Err(format!(
            "{} is not a worktree of {} (its `.git` names no `worktrees` directory of that \
             repository), so nothing was removed from it",
            wt.display(),
            repo.display()
        ));
    }
    let ns = archive::namespace(owner, "wt");
    let mut kept = Archived::default();
    let mut wrote: Vec<(String, String)> = Vec::new();
    let head = git::head_of(wt)?;
    // 1. The branch. 2. A detached HEAD, which no ref holds.
    let at = match branch {
        Some(b) => format!("{ns}/heads/{b}"),
        None => format!("{ns}/head"),
    };
    git(repo, &["update-ref", &at, &head])?;
    wrote.push((at.clone(), head.clone()));
    kept.refs.push(at);
    // 3./4. The uncommitted work - or, over the guard, the whole tree into the trash.
    if untracked_bytes(wt)? > WIP_MAX_BYTES {
        let trash = trash_dir(places).join(format!("{owner}-{}", now_ms().div_euclid(1_000)));
        move_to_trash(repo, wt, &trash)?;
        kept.trash = Some(trash);
        read_back(repo, &wrote)?;
        return Ok(kept);
    }
    if let Some(wip) = wip_commit(wt, owner)? {
        let ref_name = format!("{ns}/wip");
        git(repo, &["update-ref", &ref_name, &wip])?;
        wrote.push((ref_name.clone(), wip.clone()));
        kept.refs.push(ref_name);
        kept.wip = Some(wip);
    }
    read_back(repo, &wrote)?;
    // 5. Only now the tree, and the branch.
    remove_now(repo, wt)?;
    if let Some(b) = branch
        && git::delete_branch(repo, b).is_err()
    {
        kept.branch_left = Some(b.to_string());
    }
    Ok(kept)
}

/// Every ref the archive wrote, read back by name against the sha it was written with: a copy
/// nobody verified is not a copy (d.8 step 5).
fn read_back(repo: &Path, wrote: &[(String, String)]) -> Result<(), String> {
    for (ref_name, sha) in wrote {
        match git::rev_parse(repo, ref_name) {
            Some(back) if &back == sha => {}
            Some(back) => {
                return Err(format!(
                    "{sha} went into {ref_name} and {back} came back: not a copy"
                ));
            }
            None => {
                return Err(format!(
                    "{ref_name} was written and `git rev-parse` does not name it: not a copy"
                ));
            }
        }
    }
    Ok(())
}

/// The `wip` commit for a dirty worktree, made with a scratch index so the worktree's own index is
/// not touched - or `None` when the tree carries nothing its HEAD does not.
fn wip_commit(wt: &Path, owner: &str) -> Result<Option<String>, String> {
    if git::uncommitted_work(wt)?.is_empty() {
        return Ok(None);
    }
    let index = std::env::temp_dir().join(format!(
        "letibot-wip-{}-{}.index",
        std::process::id(),
        now_ms()
    ));
    let _ = std::fs::remove_file(&index);
    let scratch = [("GIT_INDEX_FILE", index.display().to_string())];
    let made = (|| -> Result<Option<String>, String> {
        archive::git(wt, &["read-tree", "HEAD"], &scratch)?;
        // `-A` honours `.gitignore`, so a build target is never written into the object store.
        archive::git(wt, &["add", "-A", "--", "."], &scratch)?;
        let tree = archive::git(wt, &["write-tree"], &scratch)?
            .trim()
            .to_string();
        let head = git::head_of(wt)?;
        let head_tree = git(wt, &["rev-parse", &format!("{head}^{{tree}}")])?
            .trim()
            .to_string();
        if tree == head_tree {
            return Ok(None);
        }
        let message = format!(
            "letibot: the uncommitted work of {owner} at {}",
            super::plan::stamp_of(now_ms())
        );
        let sha = git(wt, &["commit-tree", &tree, "-p", &head, "-m", &message])?
            .trim()
            .to_string();
        Ok(Some(sha))
    })();
    let _ = std::fs::remove_file(&index);
    made
}

/// The bytes of the worktree's untracked, not ignored content - what the guard is about. Ignored
/// content is not counted: `.gitignore` already says it is not the tree's work.
fn untracked_bytes(wt: &Path) -> Result<u64, String> {
    let out = git(wt, &["ls-files", "-o", "--exclude-standard", "-z"])?;
    let mut total = 0u64;
    for name in out.split('\0').filter(|n| !n.is_empty()) {
        if let Ok(meta) = std::fs::symlink_metadata(wt.join(name)) {
            total += meta.len();
        }
    }
    Ok(total)
}

/// `<gc root>/<box>/.trash`: where a tree too big to commit goes (d.8 step 4).
fn trash_dir(places: &Places) -> PathBuf {
    places.box_dir().join(".trash")
}

/// Move the worktree into the trash - one rename, because a tree of class work may be the only
/// copy there is - and prune the bookkeeping for a worktree that is no longer where it says.
fn move_to_trash(repo: &Path, wt: &Path, trash: &Path) -> Result<(), String> {
    if let Some(parent) = trash.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not make {}: {e}", parent.display()))?;
    }
    if trash.exists() {
        return Err(format!("{} is already there", trash.display()));
    }
    std::fs::rename(wt, trash).map_err(|e| {
        format!(
            "could not move {} to {} ({e}): a tree of class work is not removed on a rename \
             that did not happen",
            wt.display(),
            trash.display()
        )
    })?;
    let _ = git(repo, &["worktree", "prune"]);
    Ok(())
}

/// Remove a worktree whose content is already in the archive. The dirty check the queues' rail
/// makes is not repeated here - the work was just committed and read back - but `is_worktree_of`
/// is, so a path that is not a worktree of this repository is never removed.
fn remove_now(repo: &Path, wt: &Path) -> Result<(), String> {
    if !git::is_worktree_of(wt, repo) {
        return Err(format!(
            "{} is not a worktree of {}, so nothing was removed",
            wt.display(),
            repo.display()
        ));
    }
    git(
        repo,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    )?;
    let _ = git(repo, &["worktree", "prune"]);
    if wt.exists() {
        return Err(format!(
            "`git worktree remove` answered and {} is still there",
            wt.display()
        ));
    }
    Ok(())
}

/// What a restore brought back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restored {
    pub branches: Vec<String>,
    pub wip: Option<String>,
    pub place: Option<PathBuf>,
    /// What was left and why - a branch of that name already here.
    pub kept: Vec<String>,
}

impl Restored {
    /// One sentence for the command's answer.
    pub fn said(&self) -> String {
        let mut out = match self.branches.len() {
            0 => "no branch (the archive held none)".to_string(),
            1 => format!("the branch `{}`", self.branches[0]),
            n => format!("{n} branches ({})", self.branches.join(", ")),
        };
        if let Some(p) = &self.place {
            out.push_str(&format!(" and a worktree at {}", p.display()));
            if self.wip.is_some() {
                out.push_str(" with its uncommitted work checked out over it");
            }
        }
        if !self.kept.is_empty() {
            out.push_str(&format!("; {}", self.kept.join("; ")));
        }
        out
    }
}

/// Bring an owner's archived work back: its branches, and with `place`, a worktree at the archived
/// head whose uncommitted work is checked out over it as uncommitted again - the state the removal
/// found the tree in (d.8's restore).
pub fn restore(repo: &Path, owner: &str, place: Option<&Path>) -> Result<Restored, String> {
    let ns = archive::namespace(owner, "wt");
    let refs = refs_under(repo, &ns)?;
    if refs.is_empty() {
        return Err(format!(
            "nothing is archived under {ns}: `letibot gc` never settled {owner}, or \
             `--drop-archive` already let it go"
        ));
    }
    let mut out = Restored::default();
    let mut head = None;
    for r in &refs {
        let sha = git::rev_parse(repo, r)
            .ok_or_else(|| format!("{r} is listed and `git rev-parse` does not name it"))?;
        if let Some(name) = r.strip_prefix(&format!("{ns}/heads/")) {
            if git::branch_head(repo, name).is_none() {
                git(repo, &["branch", name, &sha])?;
                out.branches.push(name.to_string());
            } else {
                out.kept.push(format!(
                    "`{name}` is already here, so the archived {sha} stays a ref only"
                ));
            }
        } else if r.ends_with("/head") {
            head = Some(sha);
        } else if r.ends_with("/wip") {
            out.wip = Some(sha);
        }
    }
    let Some(place) = place else {
        return Ok(out);
    };
    // The commit the worktree stands on: the branch that came back, else the archived HEAD. The
    // wip commit is never the base - it is the tree's uncommitted work and is checked out over it.
    let base = out
        .branches
        .first()
        .and_then(|b| git::branch_head(repo, b))
        .or(head)
        .ok_or_else(|| format!("{ns} holds no commit to put a worktree on"))?;
    if let Some(parent) = place.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not make {}: {e}", parent.display()))?;
    }
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            &place.to_string_lossy(),
            &base,
        ],
    )?;
    if let Some(wip) = &out.wip {
        // Staged by the checkout, then unstaged: the tree the removal found, down to `git status`.
        archive::git(place, &["checkout", wip, "--", "."], &[])?;
        archive::git(place, &["reset", "-q"], &[])?;
    }
    out.place = Some(place.to_path_buf());
    Ok(out)
}

/// Where a restore puts the worktree when no path is named: the owner root's `wt/`, under the
/// package's own name.
pub fn restore_path(places: &Places, owner: &str, branch: &str) -> PathBuf {
    let name = branch.rsplit('/').next().unwrap_or("wt");
    places.owner_root(owner).join("wt").join(name)
}

/// Every ref under one archive namespace - the plan's evidence that an owner's work is kept, and
/// the list `--drop-archive` empties.
pub fn refs_under(repo: &Path, ns: &str) -> Result<Vec<String>, String> {
    if !archive::is_repo(repo) {
        return Err(format!("{} is not a git repository", repo.display()));
    }
    let out = git(repo, &["for-each-ref", "--format=%(refname)", ns])?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Every owner with an archive namespace in `repo`, with when its refs were last committed - what
/// `--drop-archive --older 90d` sorts by, because a ref has no time of its own.
pub fn archives(repo: &Path) -> Vec<(String, i64, Vec<String>)> {
    let Ok(out) = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(committerdate:unix)",
            "refs/letibot/archive",
        ],
    ) else {
        return Vec::new();
    };
    let mut by_owner: std::collections::BTreeMap<String, (i64, Vec<String>)> = Default::default();
    for line in out.lines() {
        let Some((refname, at)) = line.split_once('\0') else {
            continue;
        };
        let Some(rest) = refname.strip_prefix("refs/letibot/archive/") else {
            continue;
        };
        let owner = rest.split('/').next().unwrap_or_default().to_string();
        if owner.is_empty() {
            continue;
        }
        let at: i64 = at.trim().parse().unwrap_or(0);
        let entry = by_owner.entry(owner).or_insert((0, Vec::new()));
        entry.0 = entry.0.max(at);
        entry.1.push(refname.to_string());
    }
    by_owner
        .into_iter()
        .map(|(owner, (at, refs))| (owner, at, refs))
        .collect()
}

/// `--drop-archive`: let one owner's archive refs go. Never run by the timer (d.8) - the refs are
/// the only copy of work that was removed, so a person asks for this.
pub fn drop_archive(repo: &Path, owner: &str) -> Result<Vec<String>, String> {
    let ns = archive::namespace(owner, "wt");
    let refs = refs_under(repo, &ns)?;
    for r in &refs {
        git(repo, &["update-ref", "-d", r])?;
    }
    Ok(refs)
}

/// Keep one branch whose worktree is already gone: its ref into the owner's namespace, read back,
/// then the branch deleted. The rail is `is_worktree_of`'s cousin - the ref is read back by name
/// against the sha it was written with - so a branch nobody can see afterwards is never deleted.
pub fn archive_branch(repo: &Path, owner: &str, branch: &str) -> Result<Archived, String> {
    let Some(head) = git::branch_head(repo, branch) else {
        return Ok(Archived::default());
    };
    let ns = archive::namespace(owner, "wt");
    let at = format!("{ns}/heads/{branch}");
    git(repo, &["update-ref", &at, &head])?;
    read_back(repo, &[(at.clone(), head)])?;
    git::delete_branch(repo, branch)?;
    Ok(Archived {
        refs: vec![at],
        ..Archived::default()
    })
}

/// A short sha, as a sentence says it.
pub fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// The worktree's own git directory (`<main>/.git/worktrees/<name>`), absolute.
fn git_admin_dir(wt: &Path) -> Result<PathBuf, String> {
    let out = git(wt, &["rev-parse", "--path-format=absolute", "--git-dir"])?;
    let path = PathBuf::from(out.trim());
    if path.as_os_str().is_empty() {
        return Err(format!("{} has no git directory", wt.display()));
    }
    Ok(path)
}

/// When this worktree was stamped, from the stamp file itself, for the HEAD-log proof.
fn stamp_stamp_ms(wt: &Path) -> Option<i64> {
    let admin = git_admin_dir(wt).ok()?;
    let text = std::fs::read_to_string(admin.join(OWNER_FILE)).ok()?;
    text.split_whitespace().nth(2)?.parse().ok()
}

/// The unix-ms stamp on one reflog line: `<old> <new> <ident> <secs> <tz>\t<message>`.
fn reflog_ms(line: &str) -> Option<i64> {
    let before_tab = line.split('\t').next()?;
    let fields: Vec<&str> = before_tab.split_whitespace().collect();
    let secs: i64 = fields.get(fields.len().checked_sub(2)?)?.parse().ok()?;
    Some(secs.saturating_mul(1000))
}
