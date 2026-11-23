//! What a box's `leticode.toml` says about disks: where owner roots live, which build targets
//! are shared, and where firecode keeps its state (`docs/lifecycle.md` d.4, d.5, d.7).
//!
//! ```toml
//! [gc]
//! root = "/build/letibot"          # owner roots: <root>/<box>/<owner>/{target,tmp,home,...}
//! disk_every = "10m"               # how often the box's one disk sweeper runs
//! [[gc.shared]]                    # a build target several runs share, and its quota
//! path = "/build/target"
//! quota = "200G"
//! idle_days = 3                    # unused for this long and it goes too
//! targets_cap = "600G"             # in [gc]: `letibot gc --targets` keeps /build/target-* under it
//!
//! [gc.logs]
//! max = "32M"                      # a daemon log past this is rotated
//! keep = 3                         # rotated files kept beside it
//! capture_max = "1G"               # a job's output file stops growing here
//! age_days = 30                    # a session's record streams go this long after it was archived
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
    /// `[gc] disk_every`: the period of the daemons' disk half, run by one daemon per box.
    pub disk_every_ms: Option<i64>,
    /// `[gc] targets_cap`: the total `letibot gc --targets` keeps the per-package target dirs under.
    pub targets_cap: Option<u64>,
    /// `FIRECODE_STATE`: where firecode keeps a project's layers.
    pub firecode_state: Option<PathBuf>,
    /// `FIRECODE_RUNS`: where firecode keeps a run's own directory.
    pub firecode_runs: Option<PathBuf>,
    /// `[firecode] budget`: how much state the box's hourly `gc --state` keeps the layers under.
    pub firecode_budget: Option<String>,
    /// `[firecode] max_age_days`: state older than this goes whatever the budget says.
    pub firecode_max_age_days: Option<u64>,
    /// `[firecode] pool_per_project`: the slots firecode may pool for one project.
    pub firecode_pool_per_project: Option<u32>,
    /// `[gc.logs]`: how big a daemon log and a job capture may grow.
    pub logs: super::logs::LogPolicy,
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
            out.disk_every_ms = match gc.disk_every.as_deref() {
                Some(span) => Some(
                    crate::docker::config::span_ms(span)
                        .ok()
                        .filter(|ms| *ms > 0)
                        .ok_or_else(|| {
                            format!(
                                "{}: [gc] disk_every {span:?} is not a span like \"10m\"",
                                path.display()
                            )
                        })?,
                ),
                None => None,
            };
            out.targets_cap = match &gc.targets_cap {
                Some(q) => Some(
                    bytes(q)
                        .map_err(|e| format!("{}: [gc] targets_cap {q:?}: {e}", path.display()))?,
                ),
                None => None,
            };
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
            if let Some(logs) = gc.logs {
                let size = |key: &str, v: &Option<String>, dflt: u64| match v {
                    Some(q) => bytes(q)
                        .map_err(|e| format!("{}: [gc.logs] {key} {q:?}: {e}", path.display())),
                    None => Ok(dflt),
                };
                let d = super::logs::LogPolicy::default();
                out.logs = super::logs::LogPolicy {
                    max_bytes: size("max", &logs.max, d.max_bytes)?,
                    keep: logs.keep.unwrap_or(d.keep),
                    capture_max: size("capture_max", &logs.capture_max, d.capture_max)?,
                    age_days: logs.age_days.unwrap_or(d.age_days),
                };
            }
        }
        if let Some(table) = file.firecode {
            let fc: FirecodeTable = table
                .try_into()
                .map_err(|e| format!("{}: [firecode]: {e}", path.display()))?;
            out.firecode_state = fc.state.as_deref().map(expand);
            out.firecode_runs = fc.runs.as_deref().map(expand);
            // The budget is handed to firecode as a person wrote it, and checked here so a typo
            // is refused at the reading and not at three in the morning by a gc that did nothing.
            out.firecode_budget = match &fc.budget {
                Some(b) => {
                    bytes(b)
                        .map_err(|e| format!("{}: [firecode] budget {b:?}: {e}", path.display()))?;
                    Some(b.clone())
                }
                None => None,
            };
            out.firecode_max_age_days = fc.max_age_days;
            out.firecode_pool_per_project = fc.pool_per_project;
        }
        Ok(out)
    }
}

/// The user's `~/.config/letibot/leticode.toml`, the file every other box-wide key is read from.
pub fn config_file() -> Option<PathBuf> {
    letibot_paths::config_dir().map(|d| d.join("leticode.toml"))
}

impl Settings {
    /// The `[firecode]` policy the reaper runs with: the file's keys where it names them, and the
    /// defaults item 5 asks for where it does not.
    pub fn firecode_policy(&self) -> FirecodePolicy {
        let d = FirecodePolicy::default();
        FirecodePolicy {
            budget: self.firecode_budget.clone().unwrap_or(d.budget),
            max_age_days: self.firecode_max_age_days.unwrap_or(d.max_age_days),
            pool_per_project: self.firecode_pool_per_project.unwrap_or(d.pool_per_project),
        }
    }
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
    disk_every: Option<String>,
    #[serde(default)]
    shared: Vec<SharedTable>,
    #[serde(default)]
    targets_cap: Option<String>,
    /// `[gc.docker]`, read by [`crate::docker::config::Settings`] from this same file - the opt-in
    /// prune policy. Accepted here and not interpreted, so a box that writes it is not a box whose
    /// `[gc] root` this reader refuses.
    #[serde(default)]
    #[allow(dead_code)]
    docker: Option<toml::Value>,
    #[serde(default)]
    logs: Option<LogsTable>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LogsTable {
    #[serde(default)]
    max: Option<String>,
    #[serde(default)]
    keep: Option<usize>,
    #[serde(default)]
    capture_max: Option<String>,
    #[serde(default)]
    age_days: Option<u64>,
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
    #[serde(default)]
    budget: Option<String>,
    #[serde(default)]
    max_age_days: Option<u64>,
    #[serde(default)]
    pool_per_project: Option<u32>,
}

/// What the box's `[firecode]` table says about the reaper, with the tree's own answers where the
/// file says nothing: 150 GiB of state, nothing older than two weeks, and two pooled VMs per
/// project. Item 5 of the wiring hands each of them to firecode verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirecodePolicy {
    pub budget: String,
    pub max_age_days: u64,
    pub pool_per_project: u32,
}

impl Default for FirecodePolicy {
    fn default() -> Self {
        FirecodePolicy {
            budget: "150G".into(),
            max_age_days: 14,
            pool_per_project: 2,
        }
    }
}

impl FirecodePolicy {
    /// `--max-age` as firecode spells a span.
    pub fn max_age(&self) -> String {
        format!("{}d", self.max_age_days)
    }
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
targets_cap = "600G"
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
        assert_eq!(s.targets_cap, Some(600 * 1024 * 1024 * 1024));
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

    /// Item 5's three keys: the budget, the age and the pool's width, with the tree's own answers
    /// where the file says nothing - and a budget that is not a size refused at the reading.
    #[test]
    fn a_box_says_what_its_firecode_state_may_cost() {
        let p = Path::new("/b/leticode.toml");
        let s = Settings::parse(
            p,
            "[firecode]\nbudget = \"150G\"\nmax_age_days = 14\npool_per_project = 2\n",
        )
        .expect("a file a person wrote");
        let policy = s.firecode_policy();
        assert_eq!(policy.budget, "150G");
        assert_eq!(policy.max_age_days, 14);
        assert_eq!(policy.max_age(), "14d");
        assert_eq!(policy.pool_per_project, 2);

        // A file that names none of them keeps the defaults item 5 asks for.
        let d = Settings::default().firecode_policy();
        assert_eq!(d, FirecodePolicy::default());
        assert_eq!(
            (d.budget.as_str(), d.max_age_days, d.pool_per_project),
            ("150G", 14, 2)
        );

        let e =
            Settings::parse(p, "[firecode]\nbudget = \"150 bananas\"\n").expect_err("not a size");
        assert!(e.contains("budget"), "{e}");
    }

    /// The disk sweep's period is a span; a bare number or a zero is refused, not defaulted.
    #[test]
    fn disk_every_is_a_span() {
        let p = Path::new("/b/leticode.toml");
        let s = Settings::parse(p, "[gc]\ndisk_every = \"15m\"\n").unwrap();
        assert_eq!(s.disk_every_ms, Some(15 * 60 * 1000));
        assert_eq!(Settings::parse(p, "[gc]\n").unwrap().disk_every_ms, None);
        for bad in ["\"15\"", "\"0m\""] {
            let e = Settings::parse(p, &format!("[gc]\ndisk_every = {bad}\n")).expect_err(bad);
            assert!(e.contains("disk_every"), "{e}");
        }
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

    /// `[gc.logs]` overrides only what it names; an absent table is the defaults.
    #[test]
    fn the_log_bounds_are_read_and_default() {
        let s = Settings::parse(
            Path::new("/b/leticode.toml"),
            "[gc.logs]\nmax = \"1M\"\nkeep = 5\nage_days = 7\n",
        )
        .unwrap();
        assert_eq!(s.logs.max_bytes, 1024 * 1024);
        assert_eq!(s.logs.keep, 5);
        assert_eq!(s.logs.age_days, 7);
        assert_eq!(
            s.logs.capture_max,
            super::super::logs::LogPolicy::default().capture_max
        );
        let d = super::super::logs::LogPolicy::default();
        assert_eq!(d.age_days, 30, "plan 1.7's 30 days");
        let none = Settings::parse(Path::new("/b/leticode.toml"), "").unwrap();
        assert_eq!(none.logs, d);
        let e = Settings::parse(Path::new("/b/leticode.toml"), "[gc.logs]\nkeeep = 2\n")
            .expect_err("a typo");
        assert!(e.contains("keeep"), "{e}");
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
        gc.extend(["shared", "docker", "logs"]);
        assert_eq!(fields::<GcTable>(), gc);
        assert_eq!(fields::<LogsTable>(), listed("gc.logs"));
        assert_eq!(fields::<SharedTable>(), listed("gc.shared[]"));
        assert_eq!(fields::<FirecodeTable>(), listed("firecode"));
    }
}
