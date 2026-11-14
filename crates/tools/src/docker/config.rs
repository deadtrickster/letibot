//! What a box's `leticode.toml` says about docker (`docs/lifecycle.md` d.7).
//!
//! ```toml
//! [docker]
//! engine = "/run/docker.sock"   # the real engine the proxy forwards to
//! memory = "4G"                 # what a create that names no limit is given
//! cgroup_parent = "letibot.slice/docker.scope"   # where a container is placed, if the driver allows
//!
//! [gc.docker]                   # OPT-IN, off unless written
//! prune_images_unused_for = "30d"
//! prune_label = ["com.docker.compose.project=bench-*"]
//! ```
//!
//! `[gc.docker]` is the one part of this the timer acts on, and it removes nothing unless the
//! operator wrote it: letibot cannot prove another project's images are its own, so the default is
//! a report (`letibot gc --docker-report`) and never a prune (`docs/lifecycle.md` d.7).

use std::path::{Path, PathBuf};

use super::ENGINE_DEFAULT;

/// The `[docker]` and `[gc.docker]` tables, as one settings value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// The real engine socket. `None` when the file names none and this box has no
    /// `$DOCKER_HOST`: a granted seat then says the box has no engine instead of guessing.
    pub engine: Option<PathBuf>,
    /// A create that asks for no memory gets this, so `docker run -d` cannot take the box.
    pub memory: Option<u64>,
    /// `HostConfig.CgroupParent`, where the engine's cgroup driver can place a container in a
    /// scope letibot made.
    pub cgroup_parent: Option<String>,
    /// `[gc.docker] prune_images_unused_for`: how long an unowned image may be unused before the
    /// opt-in pass prunes it. `None` is "never prune", which is the default.
    pub prune_images_unused_for_ms: Option<i64>,
    /// `[gc.docker] prune_label`: the compose labels the opt-in pass may prune. A pattern ending
    /// in `*` is a prefix.
    pub prune_label: Vec<String>,
    /// The file these came from, so a line can say where a number was read.
    pub path: Option<PathBuf>,
}

impl Settings {
    /// This box's settings: `$LETIBOT_GC_SETTINGS` names a file, else the user's `leticode.toml`.
    pub fn load() -> Result<Settings, String> {
        if let Some(named) = std::env::var_os("LETIBOT_GC_SETTINGS").map(PathBuf::from) {
            return match std::fs::read_to_string(&named) {
                Ok(text) => Settings::parse(&named, &text),
                Err(e) => Err(format!("{}: {e}", named.display())),
            };
        }
        match crate::lifecycle::config_file() {
            Some(path) if path.is_file() => {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                Settings::parse(&path, &text)
            }
            _ => Ok(Settings::default()),
        }
    }

    /// [`Settings::load`], with a box's bad key said once instead of failing a `docker` call.
    pub fn load_or_default() -> Settings {
        Self::load().unwrap_or_else(|why| {
            eprintln!("letibot: {why}; docker settings are the defaults");
            Settings::default()
        })
    }

    /// One file's two tables. A key this build does not know is refused by name: a typo in a
    /// prune policy that quietly meant "prune nothing" is a disk filling without a reason.
    pub fn parse(path: &Path, text: &str) -> Result<Settings, String> {
        let file: File = toml::from_str(text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut out = Settings {
            path: Some(path.to_path_buf()),
            ..Settings::default()
        };
        if let Some(raw) = file.docker {
            let table: Table = raw
                .try_into()
                .map_err(|e| format!("{}: [docker]: {e}", path.display()))?;
            out.engine = Some(match table.engine.as_deref() {
                Some(e) => expand(e),
                None => PathBuf::from(ENGINE_DEFAULT),
            });
            out.memory = match &table.memory {
                Some(m) => Some(
                    crate::lifecycle::size_bytes(m)
                        .map_err(|e| format!("{}: [docker] memory {m:?}: {e}", path.display()))?,
                ),
                None => None,
            };
            out.cgroup_parent = table.cgroup_parent;
        }
        if let Some(gc) = file.gc {
            let gc: GcFile = gc
                .try_into()
                .map_err(|e| format!("{}: [gc]: {e}", path.display()))?;
            if let Some(raw) = gc.docker {
                let d: Prune = raw
                    .try_into()
                    .map_err(|e| format!("{}: [gc.docker]: {e}", path.display()))?;
                out.prune_images_unused_for_ms = match &d.prune_images_unused_for {
                    Some(s) => Some(span_ms(s).map_err(|e| {
                        format!(
                            "{}: [gc.docker] prune_images_unused_for {s:?}: {e}",
                            path.display()
                        )
                    })?),
                    None => None,
                };
                out.prune_label = d.prune_label;
            }
        }
        Ok(out)
    }

    /// Whether the timer may prune anything at all: the design's "off by default" in one place.
    pub fn prunes(&self) -> bool {
        !self.prune_label.is_empty()
    }
}

/// `30d`, `12h`, `90m`: a span as a person writes one, in milliseconds. A bare number is refused:
/// `prune_images_unused_for = 30` is a unit nobody agrees on.
pub fn span_ms(span: &str) -> Result<i64, String> {
    let mut total: i64 = 0;
    let mut digits = String::new();
    let mut any = false;
    for c in span.trim().chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n: i64 = digits
            .parse()
            .map_err(|_| format!("`{span}` is not a span: `30d`, `12h`, `90m` or `45s`"))?;
        digits.clear();
        total += n * match c.to_ascii_lowercase() {
            's' => 1_000,
            'm' => 60 * 1_000,
            'h' => 60 * 60 * 1_000,
            'd' => 24 * 60 * 60 * 1_000,
            other => {
                return Err(format!(
                    "`{other}` is not a unit: `s`, `m`, `h` or `d` (`{span}`)"
                ));
            }
        };
        any = true;
    }
    if !digits.is_empty() || !any {
        return Err(format!(
            "`{span}` names no unit: `30d`, `12h`, `90m` or `45s`"
        ));
    }
    Ok(total)
}

fn expand(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(rest),
            None => PathBuf::from(s),
        },
        None => PathBuf::from(s),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
struct File {
    #[serde(default)]
    docker: Option<toml::Value>,
    #[serde(default)]
    gc: Option<toml::Value>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    memory: Option<String>,
    #[serde(default)]
    cgroup_parent: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GcFile {
    #[serde(default)]
    docker: Option<toml::Value>,
    /// `[gc] root` and `[[gc.shared]]` are `lifecycle::config`'s; accepted so they do not
    /// make this reader drop the docker settings.
    #[serde(default)]
    #[allow(dead_code)]
    root: Option<toml::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    shared: Option<toml::Value>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Prune {
    #[serde(default)]
    prune_images_unused_for: Option<String>,
    #[serde(default)]
    prune_label: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Settings, String> {
        Settings::parse(Path::new("/x/leticode.toml"), text)
    }

    #[test]
    fn the_two_tables_are_read() {
        let s = parse(
            "[docker]\nengine = \"/run/d.sock\"\nmemory = \"4G\"\n\
             cgroup_parent = \"letibot.slice/c.scope\"\n\
             [gc.docker]\nprune_images_unused_for = \"30d\"\n\
             prune_label = [\"com.docker.compose.project=bench-*\"]\n",
        )
        .expect("parses");
        assert_eq!(s.engine, Some(PathBuf::from("/run/d.sock")));
        assert_eq!(s.memory, Some(4 * 1024 * 1024 * 1024));
        assert_eq!(s.cgroup_parent.as_deref(), Some("letibot.slice/c.scope"));
        assert_eq!(s.prune_images_unused_for_ms, Some(30 * 24 * 60 * 60 * 1000));
        assert_eq!(s.prune_label, vec!["com.docker.compose.project=bench-*"]);
        assert!(s.prunes());
    }

    #[test]
    fn a_box_with_neither_table_prunes_nothing() {
        let s = parse("[ui]\ntheme = \"dark\"\n").expect("parses");
        assert_eq!(s.engine, None);
        assert_eq!(s.memory, None);
        assert_eq!(s.prune_images_unused_for_ms, None);
        assert!(s.prune_label.is_empty());
        assert!(!s.prunes());
    }

    #[test]
    fn a_typo_is_refused_by_name() {
        let why = parse("[docker]\nmemry = \"4G\"\n").expect_err("unknown key");
        assert!(why.contains("memry"), "{why}");
        let why = parse("[gc.docker]\nprune_images_unused_for = \"30\"\n").expect_err("no unit");
        assert!(why.contains("names no unit"), "{why}");
    }
}

#[cfg(test)]
mod reference_tests {
    use std::collections::BTreeSet;

    use letibot_config_ref::{files::LETICODE, probe};

    use super::*;

    fn fields<'de, T: serde::Deserialize<'de>>() -> BTreeSet<&'static str> {
        probe::serde_fields::<T>()
            .unwrap()
            .iter()
            .copied()
            .collect()
    }

    #[test]
    fn docker_tables_are_listed() {
        assert_eq!(
            fields::<Table>(),
            probe::keys_of(LETICODE.table("docker").unwrap())
        );
        assert_eq!(
            fields::<Prune>(),
            probe::keys_of(LETICODE.table("gc.docker").unwrap())
        );
    }

    /// `[gc] root` and `[[gc.shared]]` beside `[gc.docker]` keep the docker settings.
    #[test]
    fn gc_keys_do_not_drop_docker_settings() {
        let s = Settings::parse(
            Path::new("/x/leticode.toml"),
            "[docker]\nmemory = \"4G\"\n[gc]\nroot = \"/b\"\n[[gc.shared]]\npath = \"/t\"\n",
        );
        assert!(s.is_ok(), "{s:?}");
    }
}
