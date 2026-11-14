//! What a box's `leticode.toml` says about disks: where owner roots live, which build targets
//! are shared, and where firecode keeps its state (`docs/lifecycle.md` d.4, d.5, d.7).
//!
//! ```toml
//! [gc]
//! root = "/build/letibot"          # owner roots: <root>/<box>/<owner>/{target,tmp,home,...}
//! [[gc.shared]]                    # a build target several runs share, and its quota
//! path = "/build/target"
//! quota = "200G"
//! idle_days = 3                    # unused for this long and it goes too
//!
//! [firecode]
//! state = "/build/firecode/state"  # FIRECODE_STATE, handed to `firecode up`
//! runs = "/build/firecode/runs"    # FIRECODE_RUNS
//! ```
//!
//! `[gc] root` is the one that matters: on a box whose root disk is small, every owner's build
//! target and `/tmp` land here instead, which is the whole of "builds never fill the root disk".
//! A key this build does not know is refused by name, never defaulted - a typo in `quota` that
//! quietly meant "no quota" is a disk filling without a reason anybody can read.

use std::path::{Path, PathBuf};

/// One build target several runs share (`docs/lifecycle.md` d.5's "shared target"). The owner is
/// the queue: it is removed whole, and only under the lock `rrun-runner` already takes beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedTarget {
    pub path: PathBuf,
    /// Above this, a pass that finds the lock free removes it whole.
    pub quota: Option<u64>,
    /// And below the quota, this many days with nothing written in it is a removal too.
    pub idle_days: Option<u64>,
}

/// The `[gc]` and `[firecode]` tables, as one settings value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// `<gc.root>/<box>/<owner>/` - where an owner's target, tmp, home and copy-out live.
    pub root: Option<PathBuf>,
    pub shared: Vec<SharedTarget>,
    /// `FIRECODE_STATE`: where firecode keeps a project's layers.
    pub firecode_state: Option<PathBuf>,
    /// `FIRECODE_RUNS`: where firecode keeps a run's own directory.
    pub firecode_runs: Option<PathBuf>,
    /// The file this came from, so a line can say where a number was read.
    pub path: Option<PathBuf>,
}

impl Settings {
    /// This box's settings: `$LETIBOT_GC_SETTINGS` names a file, else the user's
    /// `leticode.toml`. A file that does not parse is `Err` - a box whose `[gc] root` was
    /// refused without a word builds on the root disk instead, which is the bug this exists for.
    pub fn load() -> Result<Settings, String> {
        if let Some(named) = std::env::var_os("LETIBOT_GC_SETTINGS").map(PathBuf::from) {
            return match std::fs::read_to_string(&named) {
                Ok(text) => Settings::parse(&named, &text),
                Err(e) => Err(format!("{}: {e}", named.display())),
            };
        }
        match config_file() {
            Some(path) if path.is_file() => {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                Settings::parse(&path, &text)
            }
            _ => Ok(Settings::default()),
        }
    }

    /// [`Settings::load`] with a file a test points at, for the same reason.
    pub fn load_or_default() -> Settings {
        Self::load().unwrap_or_else(|why| {
            // A box misconfigured is said once, here, rather than silently building on `/`.
            eprintln!("letibot: {why}; owner roots stay under the data directory");
            Settings::default()
        })
    }

    /// One file's two tables. No `[gc]` and no `[firecode]` is the empty settings.
    pub fn parse(path: &Path, text: &str) -> Result<Settings, String> {
        let file: File = toml::from_str(text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut out = Settings {
            path: Some(path.to_path_buf()),
            ..Settings::default()
        };
        if let Some(table) = file.gc {
            let gc: GcTable = table
                .try_into()
                .map_err(|e| format!("{}: [gc]: {e}", path.display()))?;
            out.root = gc.root.as_deref().map(expand);
            for one in gc.shared {
                let quota = match &one.quota {
                    Some(q) => Some(bytes(q).map_err(|e| {
                        format!("{}: [gc] shared quota {q:?}: {e}", path.display())
                    })?),
                    None => None,
                };
                out.shared.push(SharedTarget {
                    path: expand(&one.path),
                    quota,
                    idle_days: one.idle_days,
                });
            }
        }
        if let Some(table) = file.firecode {
            let fc: FirecodeTable = table
                .try_into()
                .map_err(|e| format!("{}: [firecode]: {e}", path.display()))?;
            out.firecode_state = fc.state.as_deref().map(expand);
            out.firecode_runs = fc.runs.as_deref().map(expand);
        }
        Ok(out)
    }
}

/// The user's `~/.config/letibot/leticode.toml`, the file every other box-wide key is read from.
pub fn config_file() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("letibot").join("leticode.toml"))
}

#[derive(Debug, Default, serde::Deserialize)]
struct File {
    #[serde(default)]
    gc: Option<toml::Value>,
    #[serde(default)]
    firecode: Option<toml::Value>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GcTable {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    shared: Vec<SharedTable>,
    /// `[gc.docker]`, read by [`crate::docker::config::Settings`] from this same file - the opt-in
    /// prune policy. Accepted here and not interpreted, so a box that writes it is not a box whose
    /// `[gc] root` this reader refuses.
    #[serde(default)]
    #[allow(dead_code)]
    docker: Option<toml::Value>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedTable {
    #[serde(default)]
    path: String,
    #[serde(default)]
    quota: Option<String>,
    #[serde(default)]
    idle_days: Option<u64>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FirecodeTable {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    runs: Option<String>,
}

/// `~` at the front is the operator's home, because a file is written by a person and a box's
/// `/build` is spelled absolutely. A relative path is refused by the caller that uses it.
fn expand(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(rest),
            None => PathBuf::from(s),
        },
        None => PathBuf::from(s),
    }
}

/// `"200G"`, `"512M"`, `"1.5T"`: a quota as a person writes one. `G` and `T` are binary like
/// every other size in this tree, so a quota and `letibot holds`' `6.1 GiB` are the same unit.
pub fn bytes(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let (digits, unit) = match t.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => (&t[..i], t[i..].to_ascii_uppercase()),
        None => (t, String::new()),
    };
    let n: f64 = digits.trim().parse().map_err(|_| {
        format!("{s:?} is not a size: write it like \"200G\", \"512M\" or \"1.5T\"")
    })?;
    if !(n.is_finite() && n >= 0.0) {
        return Err(format!("{s:?} is not a size"));
    }
    let mult = match unit.as_str() {
        "" | "B" => 1.0,
        "K" | "KB" | "KIB" => 1024.0,
        "M" | "MB" | "MIB" => 1024.0 * 1024.0,
        "G" | "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        other => {
            return Err(format!(
                "{s:?}: {other:?} is not a unit here - the sizes are K, M, G and T (binary)"
            ));
        }
    };
    Ok((n * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three keys G4 needs, read off one file, with the sizes as the plan prints them.
    #[test]
    fn a_box_says_where_its_disks_are() {
        let s = Settings::parse(
            Path::new("/b/leticode.toml"),
            r#"
[gc]
root = "/build/letibot"
[[gc.shared]]
path = "/build/target"
quota = "200G"
idle_days = 3

[firecode]
state = "/build/firecode/state"
runs = "/build/firecode/runs"
"#,
        )
        .expect("a file a person wrote");
        assert_eq!(s.root.as_deref(), Some(Path::new("/build/letibot")));
        assert_eq!(s.shared.len(), 1);
        assert_eq!(s.shared[0].path, PathBuf::from("/build/target"));
        assert_eq!(s.shared[0].quota, Some(200 * 1024 * 1024 * 1024));
        assert_eq!(s.shared[0].idle_days, Some(3));
        assert_eq!(
            s.firecode_state.as_deref(),
            Some(Path::new("/build/firecode/state"))
        );
        assert_eq!(
            s.firecode_runs.as_deref(),
            Some(Path::new("/build/firecode/runs"))
        );
    }

    /// A misspelt key is refused by name: a `qouta` read as "no quota" is a disk filling with
    /// nothing in any log to say why.
    #[test]
    fn a_key_this_build_does_not_know_is_refused_by_name() {
        let e = Settings::parse(
            Path::new("/b/leticode.toml"),
            "[gc]\nroot = \"/x\"\nqouta = \"200G\"\n",
        )
        .expect_err("a typo");
        assert!(e.contains("qouta"), "{e}");
        let e = Settings::parse(Path::new("/b/leticode.toml"), "[firecode]\nstte = \"/x\"\n")
            .expect_err("a typo");
        assert!(e.contains("stte"), "{e}");
    }

    /// A file with neither table is the empty settings, not an error: every box that has not
    /// written one keeps its owner roots under the data directory.
    #[test]
    fn no_table_is_no_settings() {
        let s = Settings::parse(Path::new("/b/leticode.toml"), "main_model = \"m\"\n").unwrap();
        assert_eq!(s.root, None);
        assert!(s.shared.is_empty());
        assert_eq!(s.firecode_state, None);
        assert_eq!(s.firecode_runs, None);
    }

    /// The units are binary, like every other size the tree prints.
    #[test]
    fn a_size_is_written_the_way_a_person_writes_one() {
        assert_eq!(bytes("200G").unwrap(), 200 * 1024 * 1024 * 1024);
        assert_eq!(bytes("512M").unwrap(), 512 * 1024 * 1024);
        assert_eq!(
            bytes("1.5T").unwrap(),
            (1.5 * 1024.0 * 1024.0 * 1024.0 * 1024.0) as u64
        );
        assert_eq!(bytes("4096").unwrap(), 4096);
        assert!(bytes("200 gigabytes").is_err());
        assert!(bytes("-1G").is_err());
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

    fn listed(path: &str) -> BTreeSet<&'static str> {
        probe::keys_of(LETICODE.table(path).unwrap())
    }

    #[test]
    fn gc_and_firecode_tables_are_listed() {
        let mut gc = listed("gc");
        gc.extend(["shared", "docker"]);
        assert_eq!(fields::<GcTable>(), gc);
        assert_eq!(fields::<SharedTable>(), listed("gc.shared[]"));
        assert_eq!(fields::<FirecodeTable>(), listed("firecode"));
    }
}
