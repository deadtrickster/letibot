//! A project's `.env` files: which ones are its own, and which of them a placement carries
//! (`docs/config/dotenv.md`).
//!
//! They are untracked, so nothing brings them: not `git worktree add`, not a bundle, not a fetch.
//! [`carry`] is what puts them where a copy of the tree is — a package worktree, another machine, a
//! guest — and every rule there is a decision the operator made in `[dotenv]`.
//!
//! ```toml
//! [dotenv]
//! carry_to = ["xmg"]
//! carry_to_vm = false
//! ```

use std::path::{Path, PathBuf};

/// The two files a session's commands start with, in the order they are read: the later one wins.
pub const LOADED: [&str; 2] = [".env", ".env.local"];

/// Whether `name` is one of the files a placement carries: `.env`, `.env.local`, `.env.<name>` and
/// `.env.<name>.local`. `.envrc` is not one, and deliberately so: direnv runs it as code, which is
/// `firecode_route::guard`'s `AUTO_FILES` rule, and a file another program executes is not a
/// variable list.
pub fn is_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(".env.") else {
        return name == ".env";
    };
    let rest = rest.strip_suffix(".local").unwrap_or(rest);
    !rest.is_empty() && !rest.contains('.') && !rest.contains('/')
}

/// The carried files `dir` holds, in the order a tree is read: the two loaded ones first, then the
/// named ones by name, so a listing reads the same way twice. Something that is not a regular file
/// is here too - a symlink named `.env` is refused by [`carry::read_one`] BY NAME, which is a line a
/// person can act on, and a link in a guest's copy is removed rather than followed.
pub fn names_in(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| !e.file_type().map(|t| t.is_dir()).unwrap_or(true))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| is_name(n))
        .collect();
    out.sort_by_key(|n| {
        (
            LOADED.iter().position(|l| l == n).unwrap_or(LOADED.len()),
            n.clone(),
        )
    });
    out
}

/// `[dotenv]` from this box's `leticode.toml`: whether commands load the files, and which machines
/// a placement carries them to.
///
/// ```toml
/// [dotenv]
/// load = true
/// carry_to = ["xmg"]
/// carry_to_vm = false
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub load: bool,
    /// Hosts a remote placement carries the files to. Empty by default: these files hold
    /// credentials, so the operator names the machine. A host is never carried to on a project
    /// file's word.
    pub carry_to: Vec<String>,
    /// Whether a firecode VM is carried to. False by default, and needed on top of `carry_to` for a
    /// VM on a declared host.
    pub carry_to_vm: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            load: true,
            carry_to: Vec::new(),
            carry_to_vm: false,
        }
    }
}

impl Settings {
    /// The box's settings, from `$LETIBOT_DOTENV_SETTINGS` else the user's `leticode.toml`. A file
    /// that does not parse is said once and the defaults taken: a box whose `[dotenv]` was refused
    /// without a word is the bug this exists for.
    pub fn load() -> Settings {
        let path = match std::env::var_os("LETIBOT_DOTENV_SETTINGS").map(PathBuf::from) {
            Some(named) => Some(named),
            None => crate::lifecycle::config_file(),
        };
        let Some(path) = path.filter(|p| p.is_file()) else {
            return Settings::default();
        };
        match std::fs::read_to_string(&path)
            .map_err(|e| format!("{}: {e}", path.display()))
            .and_then(|text| Settings::parse(&path, &text))
        {
            Ok(s) => s,
            Err(why) => {
                eprintln!("letibot: {why}; .env files load and travel nowhere");
                Settings::default()
            }
        }
    }

    /// One file's `[dotenv]` table. A key this build does not know is refused by name.
    pub fn parse(path: &Path, text: &str) -> Result<Settings, String> {
        let file: File = toml::from_str(text).map_err(|e| format!("{}: {e}", path.display()))?;
        let Some(table) = file.dotenv else {
            return Ok(Settings::default());
        };
        let t: Table = table
            .try_into()
            .map_err(|e| format!("{}: [dotenv]: {e}", path.display()))?;
        let mut out = Settings {
            load: t.load.unwrap_or(true),
            carry_to: Vec::new(),
            carry_to_vm: t.carry_to_vm.unwrap_or(false),
        };
        for host in t.carry_to {
            if !host_name_ok(&host) {
                return Err(format!(
                    "{}: [dotenv] carry_to names {host:?}, which is not a host name (letters, \
                     digits, `.`, `-` and `_`)",
                    path.display()
                ));
            }
            out.carry_to.push(host);
        }
        Ok(out)
    }

    /// Whether a placement on `host` carries them.
    pub fn carries_to(&self, host: &str) -> bool {
        self.carry_to.iter().any(|h| h == host)
    }

    /// Whether a firecode VM placement carries them: the operator's second switch, on top of the
    /// host's name.
    pub fn carries_to_vm(&self, host: &str) -> bool {
        self.carry_to_vm && self.carries_to(host)
    }
}

fn host_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

#[derive(Debug, Default, serde::Deserialize)]
struct File {
    #[serde(default)]
    dotenv: Option<toml::Value>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    #[serde(default)]
    load: Option<bool>,
    #[serde(default)]
    carry_to: Vec<String>,
    #[serde(default)]
    carry_to_vm: Option<bool>,
}

pub mod carry;
