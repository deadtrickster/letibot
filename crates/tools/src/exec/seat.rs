//! What a session's confinement is threaded with, decided once for every platform: network rule,
//! read-only grants out of the operator's home, and the env that points tools at them. Only the
//! rendering (bwrap argv, SBPL) differs per OS.

use std::path::{Path, PathBuf};

use super::confine::{Egress, GRANT_CANDIDATES, GRANT_GLOBS, Grant};

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
        Level::Tight => seat_view(mode, place, present),
        Level::Wide => SeatView {
            grants: Vec::new(),
            egress: network_rule(mode, place),
            env: Vec::new(),
            home: Some(operator_home.to_path_buf()),
            secret_env: gh_token.map(github_env).unwrap_or_default(),
        },
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
            .position(|c| p.ends_with(c.trim_start_matches("$HOME")))
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
    let Ok(text) = std::fs::read_to_string(project.join(".git")) else {
        return Vec::new();
    };
    let Some(dir) = text.trim().strip_prefix("gitdir:").map(str::trim) else {
        return Vec::new();
    };
    let dir = project.join(dir);
    let common = std::fs::read_to_string(dir.join("commondir"))
        .map(|c| dir.join(c.trim()))
        .unwrap_or_else(|_| dir.clone());
    let common = common.canonicalize().unwrap_or(common);
    vec![Grant::ReadWrite {
        path: common,
        why: "this worktree's repository: its objects and refs, so `git commit` works in the \
              checkout"
            .to_string(),
    }]
}

/// What inside a granted git dir stays read-only: hooks and config are run by the operator's own
/// git later, outside the boundary, so a write there is code execution after the session.
pub fn git_dir_protected(git_dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![git_dir.join("hooks"), git_dir.join("config")];
    if let Ok(rd) = std::fs::read_dir(git_dir.join("worktrees")) {
        for e in rd.flatten() {
            out.push(e.path().join("config.worktree"));
            out.push(e.path().join("hooks"));
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
        .map(|c| at(c))
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

/// The whole view for a session on a machine where `present` exists under its home.
pub fn seat_view<S: AsRef<str>>(mode: &crate::mode::Mode, place: &str, present: &[S]) -> SeatView {
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
        let a = seat_view(&Mode::WRITES_ALLOWED, "x", &present);
        let b = seat_view(&Mode::WRITES_ALLOWED, "x", &present);
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
        assert!(!seat_view(&Mode::READ_ONLY, "x", &present).egress.allowed());
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
}
