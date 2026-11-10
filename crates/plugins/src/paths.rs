//! Where a plugin's directories are, worked out once for every host that reads them.
//! An empty `XDG_CONFIG_HOME` is UNSET, which is what the XDG spec says: a zero-length path is not
//! a path, and a host that joined `""` read `letibot/` relative to its own working directory.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The config directory's own name inside the base: one directory, beside nothing else of ours.
const NAME: &str = "letibot";

/// The trust book's file name, in the config directory.
const TRUST: &str = "plugin-trust.json";

/// The three variables a path here is derived from, so a host and a test can agree without reading
/// the process's environment - which is global, and changes under a running test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    /// `$XDG_CONFIG_HOME`: the user scope and the trust book.
    pub xdg_config_home: Option<OsString>,
    /// `$XDG_DATA_HOME`: the session scopes.
    pub xdg_data_home: Option<OsString>,
    /// `$HOME`: the base the two defaults are under.
    pub home: Option<OsString>,
}

impl Env {
    /// This process's environment, which is what a running host resolves against.
    pub fn of_process() -> Env {
        Env {
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME"),
            home: std::env::var_os("HOME"),
        }
    }

    /// A variable's value as a path, or `None` for absent AND for empty: `XDG_CONFIG_HOME=` is a
    /// person saying they have not set it, and every reader here treats it that way.
    fn path(value: &Option<OsString>) -> Option<PathBuf> {
        value
            .as_deref()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }
}

/// `$XDG_CONFIG_HOME/letibot`, else `~/.config/letibot` - where the user scope and the trust book
/// live. `None` when neither variable gives a directory.
pub fn config_dir() -> Option<PathBuf> {
    config_dir_in(&Env::of_process())
}

/// [`config_dir`] against an environment a caller names, so a test states one.
pub fn config_dir_in(env: &Env) -> Option<PathBuf> {
    let base = Env::path(&env.xdg_config_home)
        .or_else(|| Env::path(&env.home).map(|h| h.join(".config")))?;
    Some(base.join(NAME))
}

/// The user scope's layers: `$XDG_CONFIG_HOME/letibot/plugins`, else `~/.config/letibot/plugins`.
pub fn user_plugin_dir() -> Option<PathBuf> {
    user_plugin_dir_in(&Env::of_process())
}

/// [`user_plugin_dir`] against an environment a caller names.
pub fn user_plugin_dir_in(env: &Env) -> Option<PathBuf> {
    config_dir_in(env).map(|dir| dir.join("plugins"))
}

/// This box's data directory: `$XDG_DATA_HOME/letibot`, else `~/.local/share/letibot` - where the
/// store and the session scopes live. The session scope is under it, so the head and the daemon
/// have to agree about it for the same reason they agree about the config directory.
pub fn data_dir() -> Option<PathBuf> {
    data_dir_in(&Env::of_process())
}

/// [`data_dir`] against an environment a caller names.
pub fn data_dir_in(env: &Env) -> Option<PathBuf> {
    let base = Env::path(&env.xdg_data_home)
        .or_else(|| Env::path(&env.home).map(|h| h.join(".local/share")))?;
    Some(base.join(NAME))
}

/// The project scope's layers: `<root>/.letibot/plugins`. The root is the session's checkout, so
/// this is the one path here that does not depend on a variable.
pub fn project_plugin_dir(root: &Path) -> PathBuf {
    root.join(".letibot").join("plugins")
}

/// The session scope's layers: `<data>/sessions/<session>/plugins`, under the data directory the
/// daemon resolved (`letibot_data_dir`, which has its own override).
pub fn session_plugin_dir(data: &Path, session: &str) -> PathBuf {
    data.join("sessions").join(session).join("plugins")
}

/// This box's trust book, beside the layers it is about: `$XDG_CONFIG_HOME/letibot/plugin-trust.json`,
/// else `~/.config/letibot/plugin-trust.json`.
pub fn trust_book() -> Option<PathBuf> {
    trust_book_in(&Env::of_process())
}

/// [`trust_book`] against an environment a caller names.
pub fn trust_book_in(env: &Env) -> Option<PathBuf> {
    config_dir_in(env).map(|dir| dir.join(TRUST))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(xdg: Option<&str>, home: Option<&str>) -> Env {
        Env {
            xdg_config_home: xdg.map(OsString::from),
            xdg_data_home: None,
            home: home.map(OsString::from),
        }
    }

    /// The bug this module exists for: with `XDG_CONFIG_HOME=` set but empty, the head and the
    /// trust book joined an empty base and read `letibot/` from their cwd, while the daemon and the
    /// bridge read `~/.config/letibot`. One box, two stacks, and only one of them discovered.
    #[test]
    fn an_empty_xdg_config_home_resolves_the_same_in_every_host() {
        let empty = env(Some(""), Some("/home/op"));
        let unset = env(None, Some("/home/op"));
        for set in [&empty, &unset] {
            assert_eq!(
                config_dir_in(set),
                Some(PathBuf::from("/home/op/.config/letibot"))
            );
            assert_eq!(
                user_plugin_dir_in(set),
                Some(PathBuf::from("/home/op/.config/letibot/plugins"))
            );
            assert_eq!(
                trust_book_in(set),
                Some(PathBuf::from("/home/op/.config/letibot/plugin-trust.json"))
            );
        }
        assert_eq!(config_dir_in(&empty), config_dir_in(&unset));
    }

    /// And a variable that IS set wins, with no `/.config` under its home.
    #[test]
    fn a_set_xdg_config_home_is_the_base() {
        let set = env(Some("/etc/xdg-op"), Some("/home/op"));
        assert_eq!(
            config_dir_in(&set),
            Some(PathBuf::from("/etc/xdg-op/letibot"))
        );
        assert_eq!(
            trust_book_in(&set),
            Some(PathBuf::from("/etc/xdg-op/letibot/plugin-trust.json"))
        );
    }

    /// Neither variable: no directory, and no relative path invented in its place.
    #[test]
    fn nothing_set_resolves_nothing() {
        let none = env(None, None);
        assert_eq!(config_dir_in(&none), None);
        assert_eq!(user_plugin_dir_in(&none), None);
        assert_eq!(trust_book_in(&none), None);
        assert_eq!(
            config_dir_in(&env(Some(""), None)),
            None,
            "an empty XDG with no HOME is still nothing"
        );
    }

    /// The two scopes that are not the config directory: the project's is under the checkout, and
    /// the session's under the data directory - never under `~/.config`.
    #[test]
    fn the_project_and_session_scopes_are_where_the_checkout_and_the_data_dir_are() {
        assert_eq!(
            project_plugin_dir(Path::new("/work/app")),
            PathBuf::from("/work/app/.letibot/plugins")
        );
        assert_eq!(
            session_plugin_dir(Path::new("/data/letibot"), "s-1"),
            PathBuf::from("/data/letibot/sessions/s-1/plugins")
        );
    }

    /// The data directory has the same empty-is-unset rule, because the session scope hangs off it.
    #[test]
    fn an_empty_xdg_data_home_is_the_boxes_own_data_dir() {
        let mut empty = env(Some("/etc/xdg-op"), Some("/home/op"));
        empty.xdg_data_home = Some(OsString::from(""));
        assert_eq!(
            data_dir_in(&empty),
            Some(PathBuf::from("/home/op/.local/share/letibot"))
        );
        empty.xdg_data_home = Some(OsString::from("/data/op"));
        assert_eq!(data_dir_in(&empty), Some(PathBuf::from("/data/op/letibot")));
    }
}
