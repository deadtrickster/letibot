//! Whether a folder that is not itself in a git repository may take a child repository as its
//! project. The head and the daemon both ask here, so they cannot disagree.

use std::path::{Path, PathBuf};

use crate::LeticodeConfig;

/// The home directory, from `$HOME`. `None` when unset.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// `[project] repository` of one file: `Some(false)` only when it says so, `Some(true)` when it
/// says true. An unreadable or malformed file says nothing; the config loader reports it.
fn repository_key(path: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value.get("project")?.get("repository")?.as_bool()
}

/// May `dir` adopt a repository among its children (a single one, or a folder of same-owner
/// repositories)? Never for `home`: a home directory holds dotfile checkouts, not a project.
/// Otherwise yes, unless `[project] repository = false` in the nearest `leticode.toml` at or above
/// `dir`, else in `user_file`.
pub fn adopts_children_in(dir: &Path, home: Option<&Path>, user_file: &Path) -> bool {
    if home.is_some_and(|h| same_dir(dir, h)) {
        return false;
    }
    let project = LeticodeConfig::discover(dir).and_then(|p| repository_key(&p));
    project
        .or_else(|| repository_key(user_file))
        .unwrap_or(true)
}

/// [`adopts_children_in`] with the real home and the real user file.
pub fn adopts_children(dir: &Path) -> bool {
    adopts_children_in(dir, home_dir().as_deref(), &crate::user_path())
}

/// What a header shows where a repository would be, when the folder is home.
pub fn is_home(dir: &Path) -> bool {
    home_dir().is_some_and(|h| same_dir(dir, &h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-ws-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn home_never_adopts_and_others_do() {
        let home = tmp("home");
        let other = tmp("other");
        let none = home.join("none.toml");
        assert!(!adopts_children_in(&home, Some(&home), &none));
        assert!(adopts_children_in(&other, Some(&home), &none));
    }

    #[test]
    fn project_key_turns_it_off() {
        let dir = tmp("key");
        std::fs::write(dir.join("leticode.toml"), "[project]\nrepository = false\n").unwrap();
        assert!(!adopts_children_in(&dir, None, &dir.join("none.toml")));
        let user = tmp("key-user").join("u.toml");
        std::fs::write(&user, "[project]\nrepository = false\n").unwrap();
        let plain = tmp("key-plain");
        assert!(!adopts_children_in(&plain, None, &user));
    }
}
