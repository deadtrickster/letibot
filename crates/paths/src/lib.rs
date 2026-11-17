//! Where this box's letibot config lives: one resolver for every reader, so the launcher, the
//! daemon, the head and the bridge cannot read different files.

use std::ffi::OsString;
use std::path::PathBuf;

/// The variables the rule reads, taken once so a test names its own instead of setting the
/// process's, which is global.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    pub xdg_config_home: Option<OsString>,
    pub xdg_data_home: Option<OsString>,
    pub home: Option<OsString>,
}

impl Env {
    pub fn of_process() -> Env {
        Env {
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME"),
            home: std::env::var_os("HOME"),
        }
    }
}

/// A variable as a base directory. Empty and relative values are unset, as the XDG spec has it:
/// a relative one would resolve against the cwd, which a workspace chooses.
pub fn absolute(value: Option<&OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_home_in(env: &Env) -> Option<PathBuf> {
    absolute(env.xdg_config_home.as_ref())
        .or_else(|| absolute(env.home.as_ref()).map(|h| h.join(".config")))
}

pub fn config_home() -> Option<PathBuf> {
    config_home_in(&Env::of_process())
}

/// `$XDG_CONFIG_HOME/letibot`, else `~/.config/letibot`.
pub fn config_dir_in(env: &Env) -> Option<PathBuf> {
    config_home_in(env).map(|d| d.join("letibot"))
}

pub fn config_dir() -> Option<PathBuf> {
    config_dir_in(&Env::of_process())
}

/// The providers file under [`config_dir_in`].
pub fn providers_file_in(env: &Env) -> Option<PathBuf> {
    config_dir_in(env).map(|d| d.join("providers.toml"))
}

/// `$XDG_DATA_HOME`, else `~/.local/share` - where the store, the room and the landing roots live.
pub fn data_home_in(env: &Env) -> Option<PathBuf> {
    absolute(env.xdg_data_home.as_ref())
        .or_else(|| absolute(env.home.as_ref()).map(|h| h.join(".local/share")))
}

pub fn data_home() -> Option<PathBuf> {
    data_home_in(&Env::of_process())
}

/// `$XDG_DATA_HOME/letibot`, else `~/.local/share/letibot` - the one data directory: the store, the
/// room's socket record, the landing worktrees and a session's plugin scope.
pub fn data_dir_in(env: &Env) -> Option<PathBuf> {
    data_home_in(env).map(|d| d.join("letibot"))
}

pub fn data_dir() -> Option<PathBuf> {
    data_dir_in(&Env::of_process())
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

    #[test]
    fn an_empty_xdg_config_home_is_unset() {
        for xdg in [Some(""), Some("relative/dir"), None] {
            assert_eq!(
                config_dir_in(&env(xdg, Some("/home/op"))),
                Some(PathBuf::from("/home/op/.config/letibot")),
                "XDG_CONFIG_HOME={xdg:?}"
            );
        }
        assert_eq!(
            providers_file_in(&env(Some("/x"), Some("/home/op"))),
            Some(PathBuf::from("/x/letibot/providers.toml"))
        );
    }

    #[test]
    fn no_home_and_no_xdg_is_no_config_dir() {
        assert_eq!(config_dir_in(&env(None, None)), None);
        assert_eq!(config_dir_in(&env(Some(""), Some(""))), None);
        assert_eq!(config_dir_in(&env(None, Some("rel"))), None);
    }
}
