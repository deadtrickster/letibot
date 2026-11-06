//! What a session's confinement is threaded with, decided once for every platform: network rule,
//! read-only grants out of the operator's home, and the env that points tools at them. Only the
//! rendering (bwrap argv, SBPL) differs per OS.

use std::path::{Path, PathBuf};

use super::confine::{Egress, GRANT_CANDIDATES, GRANT_GLOBS, Grant, Protection};

/// Environment whose values are secrets: the names print, the values never do.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretEnv(pub Vec<(String, String)>);

impl std::fmt::Debug for SecretEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(k, _)| k))
            .finish()
    }
}

impl SecretEnv {
    /// `text` with every secret value masked to the same length, so offsets into it still hold.
    pub fn mask(&self, text: &[u8]) -> Vec<u8> {
        let mut out = text.to_vec();
        for (_, v) in &self.0 {
            let v = v.as_bytes();
            if v.len() < 8 {
                continue;
            }
            let mut i = 0;
            while i + v.len() <= out.len() {
                if &out[i..i + v.len()] == v {
                    out[i..i + v.len()].fill(b'*');
                    i += v.len();
                } else {
                    i += 1;
                }
            }
        }
        out
    }
}

/// `GH_TOKEN` and a git credential helper for github.com that answers with it, for a top-level
/// seat. The token is only in `GH_TOKEN`: the git config values name the variable, not its value.
pub fn github_env(token: &str) -> SecretEnv {
    let helper = "!f() { echo username=x-access-token; echo \"password=$GH_TOKEN\"; }; f";
    SecretEnv(
        [
            ("GH_TOKEN", token),
            ("GIT_CONFIG_COUNT", "2"),
            ("GIT_CONFIG_KEY_0", "credential.helper"),
            ("GIT_CONFIG_VALUE_0", ""),
            ("GIT_CONFIG_KEY_1", "credential.https://github.com.helper"),
            ("GIT_CONFIG_VALUE_1", helper),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .to_vec(),
    )
}

/// The operator's GitHub token, asked of `gh` by the daemon outside any boundary.
pub fn gh_auth_token() -> Result<String, String> {
    let gh = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "gh"]
        .into_iter()
        .find(|p| !p.starts_with('/') || Path::new(p).is_file())
        .unwrap_or("gh");
    let out = std::process::Command::new(gh)
        .args(["auth", "token"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("`gh auth token` could not run: {e}"))?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !token.is_empty() {
        Ok(token)
    } else {
        Err(format!(
            "`gh auth token` gave no token (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The grants, egress and environment a confinement is built with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatView {
    /// Values never printed; see [`SecretEnv`].
    pub secret_env: SecretEnv,
    pub grants: Vec<Grant>,
    pub egress: Egress,
    pub env: Vec<(String, String)>,
    /// The operator's own home as the view's `$HOME`, for a top-level session: the filesystem is
    /// readable and the home writable, and the deny list is the only fence. `None` is the tight
    /// view with a throwaway home.
    pub home: Option<PathBuf>,
    /// Hidden whatever else the view holds, read and write: the stores
    /// [`super::seatbelt::deny_under_home`] names, joined to `home`. A wide view holds all of them; a
    /// tight one holds the few a grant lets in, and a test holds the two renderings' lists equal.
    pub masked: Vec<PathBuf>,
}

/// How much of the machine a session's commands see. A session nobody started on the operator's
/// behalf (a child) gets the tight view, so it cannot push or land itself and its work returns
/// through its branch and the review queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Wide,
    Tight,
}

impl Level {
    pub fn of(parent_session: Option<&str>) -> Level {
        if parent_session.is_none() {
            Level::Wide
        } else {
            Level::Tight
        }
    }
}

/// The view for a session at `level`. Wide needs neither toolchain grants nor redirected `*_HOME`
/// variables, since the real home already makes them right.
pub fn seat_for_level<S: AsRef<str>>(
    level: Level,
    mode: &crate::mode::Mode,
    place: &str,
    present: &[S],
    operator_home: &Path,
    gh_token: Option<&str>,
) -> SeatView {
    match level {
        Level::Tight => seat_view(mode, place, present, operator_home),
        Level::Wide => {
            let masked = masked_under_home(true, operator_home);
            SeatView {
                grants: Vec::new(),
                egress: network_rule(mode, place),
                env: Vec::new(),
                home: Some(operator_home.to_path_buf()),
                masked,
                secret_env: gh_token.map(github_env).unwrap_or_default(),
            }
        }
    }
}

/// The session's network rule: denied unless the mode seats network calls. `Ask` counts as
/// seated, because a boundary that refused a call after the card said yes would be a second
/// decision nobody made.
pub fn network_rule(mode: &crate::mode::Mode, place: &str) -> Egress {
    if mode.seats(crate::schema::Access::Network) {
        Egress::Host {
            why: format!(
                "the `{}` mode seats network calls, so a confined command on {place} \
                 shares its network namespace rather than being handed an empty one",
                mode.name
            ),
        }
    } else {
        Egress::Denied
    }
}

/// Read-only grants for the paths that exist, in the order [`GRANT_CANDIDATES`] lists them: a
/// probe answers in glob order, which a reader of the view cannot reason about.
pub fn grants_for<S: AsRef<str>>(present: &[S]) -> Vec<Grant> {
    let rank = |p: &str| {
        GRANT_CANDIDATES
            .iter()
            .position(|c| p.ends_with(&format!("/{c}")))
            .unwrap_or(usize::MAX)
    };
    let mut ordered: Vec<&str> = present.iter().map(AsRef::as_ref).collect();
    ordered.sort_by_key(|p| rank(p));
    ordered
        .into_iter()
        .map(|path| {
            let why = if path.ends_with(".gitconfig") {
                "the identity a commit is recorded under — without it every `git commit` in this \
                 view fails, and the branch is the whole deliverable"
            } else {
                "the toolchain on that machine, so a build does not re-download what is already \
                 installed — READABLE INTO CONTEXT, and read-only: a new dependency cannot be \
                 fetched into it from inside the boundary"
            };
            Grant::ReadOnly {
                path: PathBuf::from(path),
                why: why.to_string(),
            }
        })
        .collect()
}

/// A worktree's `.git` is a file naming a directory outside the checkout, and objects and refs
/// live in that repository's common dir. Without a read-write grant on it a confined `git commit`
/// cannot write, and the branch is the whole deliverable.
pub fn worktree_git_grants(project: &Path) -> Vec<Grant> {
    // A plain checkout's `.git` is a directory the project bind already covers, and a second
    // grant for it would say nothing new.
    if project.join(".git").is_dir() {
        return Vec::new();
    }
    let Some(dirs) = git_dirs(project) else {
        return Vec::new();
    };
    vec![Grant::ReadWrite {
        path: dirs.common,
        why: "this worktree's repository: its objects and refs, so `git commit` works in the \
              checkout"
            .to_string(),
    }]
}

/// What the top-level seat reads and may not write, under the operator's home. Absent entries are
/// still named, so the file cannot be created either.
pub fn wide_protected(home: &Path) -> Vec<Protection> {
    crate::secrets::SEAT_READ_ONLY
        .iter()
        .map(|rel| Protection::file(home.join(rel)))
        .collect()
}

/// A checkout's two git directories, and the branch it has out. Read off the disk, because the
/// answer to `what may a child write in here` is different for a commit than for the tree.
struct GitDirs {
    /// The worktree's own git dir: `.git` itself, or the `worktrees/<name>` entry it names.
    worktree: PathBuf,
    /// The repository's common dir, where objects, refs, hooks and config live.
    common: PathBuf,
    /// The branch this checkout has out, as git spells it in `HEAD`: `refs/heads/agent/x`.
    own_ref: Option<String>,
}

fn git_dirs(project: &Path) -> Option<GitDirs> {
    let dot = project.join(".git");
    let (worktree, common) = if dot.is_dir() {
        (dot.clone(), dot)
    } else {
        let text = std::fs::read_to_string(&dot).ok()?;
        let dir = project.join(text.trim().strip_prefix("gitdir:")?.trim());
        let common = std::fs::read_to_string(dir.join("commondir"))
            .map(|c| dir.join(c.trim()))
            .unwrap_or_else(|_| dir.clone());
        (dir, common)
    };
    let common = common.canonicalize().unwrap_or(common);
    let worktree = worktree.canonicalize().unwrap_or(worktree);
    let own_ref = std::fs::read_to_string(worktree.join("HEAD"))
        .ok()
        .and_then(|h| h.trim().strip_prefix("ref: ").map(|r| r.trim().to_string()));
    Some(GitDirs {
        worktree,
        common,
        own_ref,
    })
}

/// What inside a granted git dir stays read-only, and of what kind when it is not there. A child
/// commits, so it writes objects and its own branch; every other ref, the hooks and config the
/// operator's git runs later, and the files that decide what a checkout reads are not its to write.
pub fn git_dir_protected(project: &Path) -> Vec<Protection> {
    let Some(dirs) = git_dirs(project) else {
        return Vec::new();
    };
    let common = &dirs.common;
    let mut out = vec![
        Protection::dir(common.join("hooks")),
        Protection::file(common.join("config")),
        Protection::file(common.join("packed-refs")),
        // Its own branch has to be writable, so `refs/heads` itself cannot be: git creates
        // `<branch>.lock` beside the ref, which needs the directory, not the ref.
        Protection::dir(common.join("refs/tags")),
        Protection::dir(common.join("refs/remotes")),
    ];
    // Every branch name the repository knows, loose or packed or only named by a worktree's
    // HEAD. A name that is merely absent as a file is one a child could create and so shadow.
    let mut names: Vec<String> = loose_refs(common, &common.join("refs/heads"))
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    let packed = packed_shas(common);
    names.extend(packed.keys().cloned());
    names.extend(worktree_head_branches(common));
    names.sort();
    names.dedup();
    let own = dirs.own_ref.as_deref();
    for name in names.iter().filter(|n| Some(n.as_str()) != own) {
        let at = common.join(name);
        // A placeholder mounted over a packed-only ref leaves an empty file in the real refs/heads
        // that shadows the packed value and breaks git. The loose copy is the same ref, and real.
        if !at.exists()
            && at.parent().is_some_and(Path::exists)
            && let Some(sha) = packed.get(name.as_str())
        {
            write_loose_ref(&at, sha);
        }
        if at.exists() || at.parent().is_some_and(Path::exists) {
            out.push(Protection::file(at));
            continue;
        }
        // The parent is absent, so a file placeholder cannot be mounted: hold the first absent
        // ancestor as an empty read-only directory, unless the child's own branch lives in it.
        let mut top = at.clone();
        while let Some(p) = top.parent().filter(|p| !p.exists()) {
            top = p.to_path_buf();
        }
        let own_in_it = own.is_some_and(|o| common.join(o).starts_with(&top));
        if !own_in_it {
            out.push(Protection::dir(top));
        }
    }
    // A sibling worktree's `HEAD` says which branch it has out, and its `commondir`/`gitdir`
    // decide which repository and which checkout git reads there at all.
    for e in std::fs::read_dir(common.join("worktrees"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let at = e.path();
        out.push(Protection::file(at.join("commondir")));
        out.push(Protection::file(at.join("gitdir")));
        out.push(Protection::file(at.join("config.worktree")));
        out.push(Protection::dir(at.join("hooks")));
        if at != dirs.worktree {
            out.push(Protection::file(at.join("HEAD")));
        }
    }
    // The checkout's `.git` file names the git dir every command in this tree reads, so writing
    // it points the operator's next `git` at a repository the child controls.
    if project.join(".git").is_file() {
        out.push(Protection::file(project.join(".git")));
    } else {
        // A plain checkout keeps HEAD in the common dir, and moving it moves the operator's tree.
        out.push(Protection::file(common.join("HEAD")));
    }
    out
}

/// Branches (`refs/heads/x`) listed in `packed-refs`, with the commit each names.
fn packed_shas(common: &Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(common.join("packed-refs"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with(['#', '^']))
        .filter_map(|l| l.split_once(' '))
        .map(|(sha, n)| (n.trim().to_string(), sha.trim().to_string()))
        .filter(|(n, _)| n.starts_with("refs/heads/"))
        .collect()
}

/// Writes `sha` as the loose ref at `at`, never over one git wrote meanwhile: a hard link fails
/// when the name exists. Best effort; a failure leaves the ref packed-only.
fn write_loose_ref(at: &Path, sha: &str) {
    let tmp = at.with_extension(format!("letibot-{}.tmp", std::process::id()));
    if std::fs::write(&tmp, format!("{sha}\n")).is_ok() {
        let _ = std::fs::hard_link(&tmp, at);
    }
    let _ = std::fs::remove_file(&tmp);
}

/// The branches other worktrees have out, including ones not yet born.
fn worktree_head_branches(common: &Path) -> Vec<String> {
    std::fs::read_dir(common.join("worktrees"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("HEAD")).ok())
        .filter_map(|h| h.trim().strip_prefix("ref: ").map(|r| r.trim().to_string()))
        .filter(|n| n.starts_with("refs/heads/"))
        .collect()
}

/// Every loose ref under `dir`, as the path and git's name for it relative to the common dir
/// (`refs/heads/agent/x`). Bounded by the tree: a ref namespace deep enough to matter is a cycle.
fn loose_refs(common: &Path, dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let at = e.path();
            if at.is_dir() {
                stack.push(at);
            } else if let Ok(rel) = at.strip_prefix(common) {
                out.push((at.clone(), rel.display().to_string()));
            }
        }
    }
    out
}

/// The variables that point a tool at a granted path, since the view's `$HOME` is fresh.
pub fn env_for_grants<'a>(grants: impl Iterator<Item = &'a Path> + Clone) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (var, leaf) in [
        ("CARGO_HOME", ".cargo"),
        ("RUSTUP_HOME", ".rustup"),
        // mise keeps its installs and its state apart, and `mise` itself reads both.
        ("MISE_DATA_DIR", ".local/share/mise"),
        ("MISE_STATE_DIR", ".local/state/mise"),
        // Without it git reads the empty `$HOME` and commits under `user@host`.
        ("GIT_CONFIG_GLOBAL", ".gitconfig"),
    ] {
        if let Some(p) = grants.clone().find(|p| p.ends_with(leaf)) {
            out.push((var.to_string(), p.display().to_string()));
        }
    }
    out
}

/// The candidates that exist under `home`, absolute: what a remote probe answers for its host,
/// answered for this one.
pub fn present_under(home: &Path) -> Vec<String> {
    let at = |c: &str| home.join(c.trim_start_matches("$HOME/"));
    let mut out: Vec<String> = GRANT_CANDIDATES
        .iter()
        .map(|c| home.join(c))
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
        .collect();
    for g in GRANT_GLOBS {
        let p = at(g);
        let (Some(parent), Some(leaf)) = (p.parent(), p.file_name().and_then(|l| l.to_str()))
        else {
            continue;
        };
        match leaf.strip_suffix('*') {
            Some(prefix) => {
                let mut hits: Vec<String> = std::fs::read_dir(parent)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .map(|p| p.display().to_string())
                    .collect();
                hits.sort();
                out.extend(hits);
            }
            None if p.is_dir() => out.push(p.display().to_string()),
            None => {}
        }
    }
    out
}

/// Every store on the seat's list, under `home`, in one order: the names are home-relative and a
/// view needs paths. A rendering masks the ones its own view reaches, so a store outside the view
/// costs nothing and a grant that covers one cannot make it readable.
pub fn masked_under_home(wide: bool, home: &Path) -> Vec<PathBuf> {
    let mut masked: Vec<PathBuf> = super::seatbelt::deny_under_home(wide)
        .into_iter()
        .map(|rel| home.join(rel))
        .collect();
    masked.sort();
    masked.dedup();
    masked
}

/// The whole view for a session on a machine where `present` exists under its home. The fence is
/// carried even though the tight view holds none of it, because a grant of the operator's home - a
/// toolchain directory today, `--grant-ro` tomorrow - is what puts a store inside it.
pub fn seat_view<S: AsRef<str>>(
    mode: &crate::mode::Mode,
    place: &str,
    present: &[S],
    operator_home: &Path,
) -> SeatView {
    let grants = grants_for(present);
    let mut env = env_for_grants(grants.iter().map(|g| g.path()));
    // A confined view has no business with the operator's credential helpers: a child commits and
    // its work returns through the review queue.
    env.extend(
        [
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "credential.helper"),
            ("GIT_CONFIG_VALUE_0", ""),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string())),
    );
    SeatView {
        grants,
        egress: network_rule(mode, place),
        env,
        home: None,
        masked: masked_under_home(false, operator_home),
        secret_env: SecretEnv::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::Mode;

    #[test]
    fn one_input_gives_one_view_on_any_platform() {
        let present = ["/h/.gitconfig", "/h/.rustup", "/h/.cargo"];
        let a = seat_view(&Mode::WRITES_ALLOWED, "x", &present, Path::new("/h"));
        let b = seat_view(&Mode::WRITES_ALLOWED, "x", &present, Path::new("/h"));
        assert_eq!(a, b);
        let paths: Vec<_> = a.grants.iter().map(|g| g.path().to_path_buf()).collect();
        assert_eq!(
            paths,
            ["/h/.cargo", "/h/.rustup", "/h/.gitconfig"].map(PathBuf::from)
        );
        assert!(a.egress.allowed());
        assert_eq!(
            a.env,
            vec![
                ("CARGO_HOME".to_string(), "/h/.cargo".to_string()),
                ("RUSTUP_HOME".to_string(), "/h/.rustup".to_string()),
                ("GIT_CONFIG_GLOBAL".to_string(), "/h/.gitconfig".to_string()),
                ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
                (
                    "GIT_CONFIG_KEY_0".to_string(),
                    "credential.helper".to_string()
                ),
                ("GIT_CONFIG_VALUE_0".to_string(), String::new()),
            ]
        );
        assert!(
            !seat_view(&Mode::READ_ONLY, "x", &present, Path::new("/h"))
                .egress
                .allowed()
        );
    }

    const FIXTURE_TOKEN: &str = "gho_fixtureFIXTUREfixture0000000000000000";

    #[test]
    fn a_top_level_session_is_wide_and_a_child_is_tight() {
        assert_eq!(Level::of(None), Level::Wide);
        assert_eq!(Level::of(Some("s-1")), Level::Tight);
        let h = Path::new("/h");
        let wide = seat_for_level(
            Level::Wide,
            &Mode::WRITES_ALLOWED,
            "x",
            &["/h/.cargo"],
            h,
            Some(FIXTURE_TOKEN),
        );
        assert!(
            wide.secret_env
                .0
                .iter()
                .any(|(k, v)| k == "GH_TOKEN" && v == FIXTURE_TOKEN)
        );
        assert!(
            !format!("{wide:?}").contains(FIXTURE_TOKEN),
            "Debug leaked the token"
        );
        assert_eq!(
            wide.secret_env
                .mask(format!("a {FIXTURE_TOKEN} b").as_bytes()),
            format!("a {} b", "*".repeat(FIXTURE_TOKEN.len())).into_bytes()
        );
        assert_eq!(wide.home.as_deref(), Some(h));
        assert!(wide.grants.is_empty() && wide.env.is_empty() && wide.egress.allowed());
        let tight = seat_for_level(
            Level::Tight,
            &Mode::WRITES_ALLOWED,
            "x",
            &["/h/.cargo"],
            h,
            Some(FIXTURE_TOKEN),
        );
        assert!(tight.home.is_none() && !tight.grants.is_empty());
        assert!(
            tight.secret_env.0.is_empty(),
            "a child never gets the token"
        );
    }

    #[test]
    fn only_what_exists_under_the_home_is_granted() {
        let home = std::env::temp_dir().join(format!("letibot-seat-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".cargo")).expect("a fixture home");
        std::fs::write(home.join(".gitconfig"), "").expect("a gitconfig");
        let got = present_under(&home);
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().any(|p| p.ends_with("/.cargo")));
        assert!(got.iter().any(|p| p.ends_with("/.gitconfig")));
    }

    /// The set a child may not write, read off a repository laid out the way git lays one out: a
    /// common dir, one linked worktree, and the branch that worktree has out. Its own branch is
    /// the one exception, because a commit writes it and nothing else here.
    #[test]
    fn a_childs_protections_are_every_ref_but_its_own_and_the_files_that_move_a_checkout() {
        let root = std::env::temp_dir().join(format!("letibot-gitdirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let common = root.join("repo/.git");
        let wt = root.join("wt");
        for d in [
            common.join("refs/heads/agent"),
            common.join("refs/tags"),
            common.join("worktrees/wt"),
            wt.clone(),
        ] {
            std::fs::create_dir_all(&d).expect("a fixture git dir");
        }
        std::fs::write(common.join("refs/heads/main"), "0".repeat(40)).unwrap();
        std::fs::write(common.join("refs/heads/agent/child"), "1".repeat(40)).unwrap();
        std::fs::write(common.join("config"), "[core]\n").unwrap();
        std::fs::write(common.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            common.join("worktrees/wt/HEAD"),
            "ref: refs/heads/agent/child\n",
        )
        .unwrap();
        std::fs::write(common.join("worktrees/wt/commondir"), "../..\n").unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", common.join("worktrees/wt").display()),
        )
        .unwrap();

        let grants = worktree_git_grants(&wt);
        assert_eq!(grants.len(), 1, "the common dir is the one grant");
        assert_eq!(grants[0].path(), &common.canonicalize().unwrap());

        let protected: Vec<PathBuf> = git_dir_protected(&wt).into_iter().map(|p| p.path).collect();
        let has = |p: PathBuf| protected.iter().any(|q| q == &p);
        assert!(has(common.join("refs/heads/main")), "{protected:?}");
        assert!(
            !has(common.join("refs/heads/agent/child")),
            "the child's own branch has to stay writable, or it cannot commit: {protected:?}"
        );
        for must in [
            common.join("hooks"),
            common.join("config"),
            common.join("packed-refs"),
            common.join("refs/tags"),
            common.join("worktrees/wt/commondir"),
            wt.join(".git"),
        ] {
            assert!(
                has(must.clone()),
                "{} is not protected: {protected:?}",
                must.display()
            );
        }
        assert!(
            !has(common.join("worktrees/wt/HEAD")),
            "a worktree's own HEAD is the one it works on"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A branch only in `packed-refs` has no file, so it is protected by name.
    #[test]
    fn a_packed_only_branch_is_protected_by_name() {
        let root = std::env::temp_dir().join(format!("letibot-packed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let common = root.join("repo/.git");
        let wt = root.join("wt");
        for d in [
            common.join("refs/heads/agent"),
            common.join("worktrees/wt"),
            wt.clone(),
        ] {
            std::fs::create_dir_all(&d).unwrap();
        }
        std::fs::write(common.join("refs/heads/agent/child"), "1".repeat(40)).unwrap();
        std::fs::write(
            common.join("packed-refs"),
            format!(
                "# pack-refs with: peeled\n{} refs/heads/main\n{} refs/heads/feat/deep\n^{}\n{} refs/tags/t\n",
                "0".repeat(40),
                "2".repeat(40),
                "3".repeat(40),
                "4".repeat(40)
            ),
        )
        .unwrap();
        std::fs::write(
            common.join("worktrees/wt/HEAD"),
            "ref: refs/heads/agent/child\n",
        )
        .unwrap();
        std::fs::write(common.join("worktrees/wt/commondir"), "../..\n").unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", common.join("worktrees/wt").display()),
        )
        .unwrap();
        let prot = git_dir_protected(&wt);
        let has = |p: PathBuf| prot.iter().any(|q| q.path == p);
        let c = common.canonicalize().unwrap();
        assert!(has(c.join("refs/heads/main")), "{prot:?}");
        assert!(
            has(c.join("refs/heads/feat")),
            "absent parent held as a dir: {prot:?}"
        );
        assert!(!has(c.join("refs/heads/agent/child")), "{prot:?}");
        assert_eq!(
            std::fs::read_to_string(common.join("refs/heads/main")).unwrap(),
            format!("{}\n", "0".repeat(40)),
            "the placeholder is the packed value, so git still reads the branch"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A plain checkout has no second grant to make, and its `HEAD` is the file that decides what
    /// the operator has out.
    #[test]
    fn a_plain_checkout_grants_nothing_and_protects_its_head() {
        let root = std::env::temp_dir().join(format!("letibot-plaingit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git/refs/heads")).expect("a fixture checkout");
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join(".git/refs/heads/main"), "2".repeat(40)).unwrap();

        assert!(
            worktree_git_grants(&root).is_empty(),
            "a plain checkout's .git is inside the project bind already"
        );
        let protected: Vec<PathBuf> = git_dir_protected(&root)
            .into_iter()
            .map(|p| p.path)
            .collect();
        assert!(
            protected.iter().any(|p| p.ends_with(".git/HEAD")),
            "{protected:?}"
        );
        assert!(
            !protected.iter().any(|p| p.ends_with("refs/heads/main")),
            "a checkout's own branch is the one it commits on: {protected:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
