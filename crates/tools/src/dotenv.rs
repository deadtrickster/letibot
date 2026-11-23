//! A project's `.env` files: which ones a placement carries, how one is parsed, and the env every
//! command of a session starts with (`docs/config/dotenv.md`).
//!
//! ```dotenv
//! # a comment
//! export KO_DOCKER_REPO=kind.local
//! KUBECONFIG=./.kube/config
//! GREETING="hello\nworld"
//! ```
//!
//! Precedence is docker compose's: a pair letibot set, or one the call itself named, wins over the
//! file. Within a file set the later file wins, so `.env.local` overrides `.env`.
//!
//! Values are secrets. Each one of eight bytes or more joins the session's [`SecretEnv`], which is
//! what masks a tool's output, and none of them is ever written to a transcript row or the store. A
//! name is all a line about them may carry.
//!
//! [`SecretEnv`]: crate::exec::seat::SecretEnv

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::SystemTime;

/// The files a session's commands start with, in the order they are read: the later one wins.
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

/// One file's pairs and the lines it could not read, by number.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub pairs: Vec<(String, String)>,
    pub bad: Vec<usize>,
}

/// Parse one file. `base` answers a name for `${VAR}`; an earlier key in this file is asked first.
///
/// The grammar is dotenv's basics and nothing more: `KEY=value`, an optional `export `, `#` at the
/// start of a line, blank lines, `'single'` quoted values literally and `"double"` quoted with
/// `\n`, `\t`, `\r`, `\"`, `\\` and `\$`. There is no command substitution and no backtick: a file
/// is data, and one that could run a command would be a shell letibot sourced.
pub fn parse(text: &str, base: &dyn Fn(&str) -> Option<String>) -> Parsed {
    let mut out = Parsed::default();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for (i, raw) in text.split('\n').enumerate() {
        let n = i + 1;
        let line = raw.strip_suffix('\r').unwrap_or(raw).trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export ")
            .map(str::trim_start)
            .unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            out.bad.push(n);
            continue;
        };
        let key = key.trim_end();
        if !is_key(key) {
            out.bad.push(n);
            continue;
        }
        let lookup = |name: &str| seen.get(name).cloned().or_else(|| base(name));
        match value_of(value, &lookup) {
            Some(v) => {
                seen.insert(key.to_string(), v.clone());
                out.pairs.push((key.to_string(), v));
            }
            None => out.bad.push(n),
        }
    }
    out
}

/// A variable name: a letter or `_`, then letters, digits and `_`. Anything else is not a name a
/// shell could set, and a line that is not a name is reported by its number and skipped.
fn is_key(s: &str) -> bool {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A value after the `=`, or `None` for a line that does not close its quote or carries anything
/// but spaces after it.
fn value_of(raw: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('\'') {
        let end = rest.find('\'')?;
        return rest[end + 1..]
            .trim()
            .is_empty()
            .then(|| rest[..end].to_string());
    }
    if let Some(rest) = raw.strip_prefix('"') {
        return double_quoted(rest, lookup);
    }
    // An unquoted value is the rest of the line: a `#` inside it is part of it, because a password
    // with a `#` in it is ordinary and a comment is what a line STARTS with.
    Some(expand(raw, lookup))
}

fn double_quoted(rest: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::new();
    let mut i = 0;
    while i < rest.len() {
        let c = rest[i..].chars().next().expect("in bounds");
        match c {
            '"' => {
                return rest[i + c.len_utf8()..].trim().is_empty().then_some(out);
            }
            '\\' => {
                let next = rest[i + 1..].chars().next()?;
                match next {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '$' => out.push('$'),
                    // Any other escape is the character itself, backslash and all, so a value
                    // written for a Windows path is not silently rewritten.
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
                i += 1 + next.len_utf8();
            }
            '$' => match expand_at(rest, i, lookup) {
                Some((text, used)) => {
                    out.push_str(&text);
                    i += used;
                }
                None => {
                    out.push('$');
                    i += 1;
                }
            },
            other => {
                out.push(other);
                i += other.len_utf8();
            }
        }
    }
    None
}

/// `${NAME}` at `at`: what it expands to, and how many bytes it consumed.
fn expand_at(
    text: &str,
    at: usize,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<(String, usize)> {
    let after = text[at..].strip_prefix("${")?;
    let close = after.find('}')?;
    let name = &after[..close];
    if !is_key(name) {
        return None;
    }
    // An unset name is empty, as a shell's would be; nothing here can fail a line.
    Some((lookup(name).unwrap_or_default(), 2 + close + 1))
}

/// Every `${NAME}` in an unquoted value, and nothing else: a bare `$NAME` is not a reference in a
/// dotenv file.
fn expand(raw: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i..].chars().next().expect("in bounds");
        if c == '$'
            && let Some((text, used)) = expand_at(raw, i, lookup)
        {
            out.push_str(&text);
            i += used;
            continue;
        }
        out.push(c);
        i += c.len_utf8();
    }
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

/// One directory's loaded files, as last read.
struct Cached {
    signature: Option<Signature>,
    pairs: Vec<(String, String)>,
    /// The notices this version of the files already earned, so one broken line is said once.
    said: Vec<String>,
}

/// What a directory's loaded files look like without reading them: a missing file is `None`, so one
/// that appears or goes away is a change too.
type Signature = Vec<(String, Option<(SystemTime, u64)>)>;

fn signature_of(dir: &Path) -> Signature {
    LOADED
        .iter()
        .map(|name| {
            let stamp = std::fs::metadata(dir.join(name))
                .and_then(|m| m.modified().map(|t| (t, m.len())))
                .ok();
            (name.to_string(), stamp)
        })
        .collect()
}

fn watch() -> &'static Mutex<BTreeMap<PathBuf, Cached>> {
    static CELL: OnceLock<Mutex<BTreeMap<PathBuf, Cached>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The pairs `dir`'s `.env` and `.env.local` give, the later file winning, with `${VAR}` read from
/// the daemon's own environment (`HOME`, `USER`, …) and from earlier keys. Plus the notices a NEW
/// version of the files earns: one per line that is not a `KEY=value`, by number and never by
/// content, said once per version.
pub fn read(dir: &Path) -> (Vec<(String, String)>, Vec<String>) {
    let signature = signature_of(dir);
    let mut watch = watch().lock().unwrap_or_else(|e| e.into_inner());
    let cached = watch.entry(dir.to_path_buf()).or_insert_with(|| Cached {
        signature: None,
        pairs: Vec::new(),
        said: Vec::new(),
    });
    if cached.signature.as_ref() == Some(&signature) {
        return (cached.pairs.clone(), Vec::new());
    }
    let base = |name: &str| std::env::var(name).ok();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut said = Vec::new();
    for name in LOADED {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            continue;
        };
        // A file with a stray byte is still a file whose other lines count: lossy rather than a
        // whole file skipped for one malformed character.
        let text = String::from_utf8_lossy(&bytes);
        let parsed = parse(&text, &base);
        for n in parsed.bad {
            said.push(format!(
                "{name} line {n} is not a `KEY=value` line and was skipped"
            ));
        }
        for (k, v) in parsed.pairs {
            pairs.retain(|(had, _)| *had != k);
            pairs.push((k, v));
        }
    }
    cached.signature = Some(signature);
    cached.pairs = pairs.clone();
    // Only the notices this version has not said before, so a file read on every call says a
    // broken line once.
    let fresh: Vec<String> = said
        .iter()
        .filter(|s| !cached.said.contains(s))
        .cloned()
        .collect();
    cached.said = said;
    (pairs, fresh)
}

/// The env a backend's commands start with, and the notices their files earned since the last call.
/// Called by the `bash` tool before it spawns, so background jobs and foreground ones are one path;
/// a file whose mtime has not changed costs two `stat`s and nothing else.
pub fn apply(backend: &dyn crate::backend::ExecBackend) -> Vec<String> {
    let Some(dir) = backend.dotenv_dir() else {
        return Vec::new();
    };
    let (pairs, said) = read(&dir);
    backend.set_dotenv_env(pairs);
    said
}

pub mod carry;

/// The pairs `dir`'s two loaded files give, for a caller that is not a command's spawn: a
/// placement handing them to another machine, or a test.
pub fn pairs(dir: &Path) -> Vec<(String, String)> {
    read(dir).0
}

#[cfg(test)]
mod tests;
