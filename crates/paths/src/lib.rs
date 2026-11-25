//! Where this box's letibot config lives: one resolver for every reader, so the launcher, the
//! daemon, the head and the bridge cannot read different files.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

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

/// Says whether a process under test refuses the default data directory. A debug build answers yes
/// on its own, because a debug build of this tree is a test or a debugging session - the daemon
/// and the head are installed as release bins - so an agent's `cargo test` needs no ceremony.
pub const REFUSE_DEFAULT_STORE: &str = "LETIBOT_REFUSE_DEFAULT_STORE";

/// May this process not fall back to the default data directory? Unset, a debug build says yes and
/// a release build no; `0` says no to both, so a debug run that means the operator's store says so
/// in one word, and `1` guards a release test run.
pub fn refuses_default_store() -> bool {
    refuses_default_store_given(std::env::var(REFUSE_DEFAULT_STORE).ok().as_deref())
}

/// [`refuses_default_store`] with the reading handed in rather than read - the seam its test
/// uses, so the test never sets a variable the run may have set for real.
fn refuses_default_store_given(flag: Option<&str>) -> bool {
    match flag {
        Some("0") => false,
        Some(v) if !v.is_empty() => true,
        _ => cfg!(debug_assertions),
    }
}

/// End the process when `path` is this process's default data directory or anything inside it, and
/// this run may not open the store there.
///
/// It ends the process rather than panicking, because a store is opened from watcher, link and
/// client threads nobody joins: there a panic is a test waiting for an answer that never comes.
pub fn refuse_the_default_store(path: &Path) {
    let Some(why) = refusal_at(path, data_dir().as_deref(), refuses_default_store()) else {
        return;
    };
    // Printed before the abort, since an abort has no hook to say anything of its own.
    eprintln!("\nletibot: {why}");
    std::process::abort();
}

/// The refusal [`refuse_the_default_store`] acts on, with the path, the default data directory and
/// the answer handed in: `None` means the open may go ahead. A path that does not exist yet is
/// compared as its parent resolves, because a store's first open is the one that creates it.
pub fn refusal_at(path: &Path, default: Option<&Path>, refuse: bool) -> Option<String> {
    if !refuse {
        return None;
    }
    let default = resolve(default?);
    let at = resolve(path);
    if at != default && !at.starts_with(&default) {
        return None;
    }
    Some(format!(
        "`{}` is the default data directory `{}`, which a process under test must not open the \
         store in: the rows would outlive the test and read as real ones. Give the test its own \
         data directory (a tempdir in `XDG_DATA_HOME`, or `--store`), or set \
         {REFUSE_DEFAULT_STORE}=0 to mean this directory.",
        path.display(),
        default.display()
    ))
}

/// `path` with its directory resolved where it exists: `..` and a symlinked parent cannot make the
/// operator's store look like somebody else's. The file itself is not canonicalised - a store
/// being opened for the first time is not there yet.
fn resolve(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    match (std::fs::canonicalize(parent), path.file_name()) {
        (Ok(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
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

    /// The flag answers yes and no outright; with nothing said, the build profile answers - a debug
    /// build is a test or a debugging session, a release build is the installed product.
    #[test]
    fn the_flag_answers_outright_and_the_profile_answers_when_it_is_unset() {
        assert!(refuses_default_store_given(Some("1")));
        assert!(refuses_default_store_given(Some("true")));
        assert!(!refuses_default_store_given(Some("0")));
        assert_eq!(refuses_default_store_given(None), cfg!(debug_assertions));
        assert_eq!(
            refuses_default_store_given(Some("")),
            cfg!(debug_assertions)
        );
    }

    /// The store's own directory, a file of it and a project file under it are all refused; a
    /// scratch of the test's own - even one whose name starts the same - is not.
    #[test]
    fn the_default_data_directory_and_what_is_under_it_are_refused() {
        let default = Path::new("/home/op/.local/share/letibot");
        let cases: [&str; 4] = [
            "/home/op/.local/share/letibot",
            "/home/op/.local/share/letibot/sessions.db",
            "/home/op/.local/share/letibot/shared.db",
            "/home/op/.local/share/letibot/projects/abc123.db",
        ];
        for case in cases {
            let why = refusal_at(Path::new(case), Some(default), true)
                .unwrap_or_else(|| panic!("{case} is the default data directory"));
            assert!(why.contains(REFUSE_DEFAULT_STORE), "{why}");
            assert!(why.contains(case), "{why}");
        }
        for case in [
            "/home/op/.local/share/letibot-other/sessions.db",
            "/tmp/x/sessions.db",
        ] {
            assert_eq!(
                refusal_at(Path::new(case), Some(default), true),
                None,
                "{case}"
            );
        }
        // The same paths with the flag off: nothing is refused, which is how the operator's own
        // daemon opens the store it is meant to open.
        assert_eq!(refusal_at(default, Some(default), false), None);
        assert_eq!(
            refusal_at(
                Path::new("/home/op/.local/share/letibot/sessions.db"),
                Some(default),
                false
            ),
            None
        );
        // And nothing said about the default at all is nothing to refuse.
        assert_eq!(
            refusal_at(Path::new("/home/op/.local/share/letibot/x.db"), None, true),
            None
        );
    }

    /// A `..` or a symlinked parent cannot make the default directory look like someone else's.
    #[test]
    fn a_path_is_compared_with_its_directory_resolved() {
        let scratch = std::env::temp_dir().join(format!("letibot-paths-{}", std::process::id()));
        let store = scratch.join("real/letibot");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::create_dir_all(scratch.join("real/letibot-other")).unwrap();
        let through_dots = scratch.join("real/letibot/../letibot/sessions.db");
        assert!(
            refusal_at(&through_dots, Some(&store), true).is_some(),
            "a `..` spelling of the default is refused"
        );
        assert_eq!(
            refusal_at(
                &scratch.join("real/letibot-other/sessions.db"),
                Some(&store),
                true
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// The door ENDS the process: a store is opened from threads a test never joins, and a refusal
    /// there has to be a failure of the run rather than a thread somebody waits on for ever. Run as
    /// a child of this binary, because an abort's only observable is how the process went.
    #[test]
    fn the_door_ends_the_process_rather_than_unwinding() {
        const CHILD: &str = "LETIBOT_PATHS_ABORT_CHILD";
        if std::env::var_os(CHILD).is_some() {
            // The child: `HOME` is the fixture, so the default data directory is the one below.
            let dir = data_dir().expect("a data directory in this child");
            refuse_the_default_store(&dir.join("shared.db"));
            unreachable!("the door ends the process");
        }
        let root = std::env::temp_dir().join(format!("letibot-paths-abort-{}", std::process::id()));
        let store = root.join(".local/share/letibot");
        std::fs::create_dir_all(&store).unwrap();
        let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
            .args([
                "--exact",
                "tests::the_door_ends_the_process_rather_than_unwinding",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("HOME", &root)
            .env(REFUSE_DEFAULT_STORE, "1")
            .env_remove("XDG_DATA_HOME")
            .output()
            .expect("the child runs");
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !out.status.success() && out.status.code().is_none(),
            "an abort, not an exit status: {:?} {said}",
            out.status
        );
        assert!(said.contains("must not open the store in"), "{said}");
        assert!(
            said.contains(&store.join("shared.db").display().to_string()),
            "{said}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
