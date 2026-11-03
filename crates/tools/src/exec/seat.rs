//! What a session's confinement is threaded with, decided once for every platform: the network
//! rule, the read-only grants out of the operator's home, and the environment that points tools at
//! them. Only the rendering (bwrap argv, SBPL) differs per OS, so a placement, a local confined
//! role and the Mac default seat all take their view from here.

use std::path::{Path, PathBuf};

use super::confine::{Egress, GRANT_CANDIDATES, GRANT_GLOBS, Grant};

/// The grants, egress and environment a confinement is built with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatView {
    pub grants: Vec<Grant>,
    pub egress: Egress,
    pub env: Vec<(String, String)>,
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
    let env = env_for_grants(grants.iter().map(|g| g.path()));
    SeatView {
        grants,
        egress: network_rule(mode, place),
        env,
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
            ]
        );
        assert!(!seat_view(&Mode::READ_ONLY, "x", &present).egress.allowed());
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
