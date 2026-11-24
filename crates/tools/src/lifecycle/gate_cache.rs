//! Gate placements' build caches: what a merge gate left on this box, how big it is, how long
//! since anything wrote to it, and which of them may be given back.
//!
//! A gate builds in a `target/` beside the tree it judges - under the landing root here
//! (`<data dir>/landing/<repo>-<hex>/target`) or under a declared host's placement root
//! (`<host root>/gate/<repo>-<hex>/target`), and a `[queue] gate_cache_root` moves it to a build
//! disk instead. Nothing records those directories in a store, so this reads them off the disk:
//! the size and the newest write are what decide whether a cache is stale, and a process with its
//! cwd inside the placement (or a live pid in its `run/`) is what keeps one.
//!
//! Read-only. [`gc`] is the hand that removes, and it removes a stale `target/` and nothing else.

use std::path::{Path, PathBuf};

use super::autoarchive::Here;
use super::in_use;
use crate::exec::diskio::{Disks as _, HostDisks, Space};

/// The directories whose children are gate placements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    /// `[queue] gate_cache_root`: each gate's cache is `<cache>/<repo>-<hex>`, named the way the
    /// placement is. `None` is every gate cache sitting in its own placement.
    pub cache: Option<PathBuf>,
    /// The landing root: each child is a landing tree whose gate built in its `target/`.
    pub landing: Option<PathBuf>,
    /// Each declared host's root as this box sees it (`~/letibot-remote`): a gate placed there
    /// keeps its cache under `<root>/gate/`.
    pub hosts: Vec<PathBuf>,
}

impl Roots {
    /// Every root that exists, in the order a person reads them: the configured cache first.
    pub fn existing(&self) -> Vec<(PathBuf, Shape)> {
        let mut out = Vec::new();
        if let Some(c) = &self.cache {
            out.push((c.clone(), Shape::Cache));
        }
        if let Some(l) = &self.landing {
            out.push((l.clone(), Shape::Placement));
        }
        for h in &self.hosts {
            out.push((h.join("gate"), Shape::Placement));
        }
        out.retain(|(p, _)| p.is_dir());
        out
    }
}

/// What a root's children are: a gate's cache directly, or a whole placement with a `target/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// `<root>/<repo>-<hex>/` IS the build cache.
    Cache,
    /// `<root>/<repo>-<hex>/target/` is the build cache.
    Placement,
}

/// One gate's build cache, as the disk has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// The directory the gate worked in: the placement, or the cache itself.
    pub dir: PathBuf,
    /// The `target/` a gate built in - what [`gc`] removes.
    pub target: PathBuf,
    /// What is under it now, and whether the walk that measured it reached the end.
    pub bytes: u64,
    pub entire: bool,
    /// The newest write under `target/`, or `0` when nothing could be read.
    pub newest_ms: i64,
    /// Why it must be kept, when something is using it.
    pub busy: Option<String>,
}

impl Placement {
    /// How many milliseconds since anything wrote to it, saturating at zero for a clock that went
    /// backwards (a cache written by a process whose clock is ahead of this one).
    pub fn idle_ms(&self, now_ms: i64) -> i64 {
        now_ms.saturating_sub(self.newest_ms).max(0)
    }

    /// Nothing has written to it for `days` - the one rule that makes a cache stale.
    pub fn stale(&self, now_ms: i64, days: u64) -> bool {
        self.idle_ms(now_ms) >= days as i64 * DAY_MS
    }

    /// `gate cache <size> <path> — last written <when>`, and whether it may be given back.
    pub fn line(&self, now_ms: i64) -> String {
        let size = match self.entire {
            true => super::human_bytes(self.bytes),
            false => format!("at least {}", super::human_bytes(self.bytes)),
        };
        let when = match self.newest_ms {
            0 => "never read".to_string(),
            _ => format!("{} ago", super::human_millis(self.idle_ms(now_ms) as u64)),
        };
        let verdict = match &self.busy {
            Some(why) => format!("kept: {why}"),
            None => "no gate is running in it".to_string(),
        };
        format!(
            "gate cache {size} {} — last written {when}; {verdict}",
            self.dir.display()
        )
    }

    /// The same facts for a program.
    pub fn json(&self, now_ms: i64) -> serde_json::Value {
        serde_json::json!({
            "dir": self.dir.display().to_string(),
            "target": self.target.display().to_string(),
            "bytes": self.bytes,
            "entire": self.entire,
            "newest_ms": self.newest_ms,
            "idle_ms": self.idle_ms(now_ms),
            "busy": self.busy,
        })
    }
}

/// Every gate placement this box's roots hold, newest first.
pub fn placements(roots: &Roots, here: &Here) -> Vec<Placement> {
    let mut out = Vec::new();
    for (root, shape) in roots.existing() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let (dir, target) = match shape {
                Shape::Cache => (entry.path(), entry.path()),
                Shape::Placement => (entry.path(), entry.path().join("target")),
            };
            if !target.is_dir() {
                continue;
            }
            out.push(measure(dir, target, here));
        }
    }
    out.sort_by(|a, b| {
        b.newest_ms
            .cmp(&a.newest_ms)
            .then_with(|| a.dir.cmp(&b.dir))
    });
    out
}

fn measure(dir: PathBuf, target: PathBuf, here: &Here) -> Placement {
    let (bytes, newest_ms, entire) = walk(&target, WALK_BUDGET, COUNT_CAP);
    let busy = busy(&dir, here);
    Placement {
        dir,
        target,
        bytes,
        entire,
        newest_ms,
        busy,
    }
}

/// What a removal did: the placements given back, the ones kept - a gate is running in one, or the
/// removal itself failed - and the line each removal logged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gc {
    pub removed: Vec<Placement>,
    pub kept: Vec<Placement>,
    pub freed: u64,
    /// The sentences to log - one per removal - and one per cache that could not be removed.
    pub said: Vec<String>,
}

/// Remove every placement's `target/` that nothing has written to for `days`, and never one a
/// gate is running in. `say` is where each removal's line goes.
pub fn gc(roots: &Roots, here: &Here, now_ms: i64, days: u64, say: &mut dyn FnMut(String)) -> Gc {
    let mut out = Gc::default();
    for mut p in placements(roots, here) {
        if p.busy.is_some() {
            out.kept.push(p);
            continue;
        }
        // The cheap question first: a cache something wrote to this week is not measured at all,
        // and the walk that would measure a 200 GB target is the expensive half of this pass.
        if touched_since(&p.target, now_ms - days as i64 * DAY_MS) {
            continue;
        }
        // Stale: measure it whole, because the line has to say what was freed.
        let (bytes, newest_ms, entire) = walk(&p.target, REMOVE_WALK_BUDGET, REMOVE_COUNT_CAP);
        p.bytes = bytes;
        p.newest_ms = if newest_ms == 0 {
            p.newest_ms
        } else {
            newest_ms
        };
        p.entire = entire;
        match std::fs::remove_dir_all(&p.target) {
            Ok(()) => {
                let size = super::human_bytes(p.bytes);
                let line = match p.newest_ms {
                    0 => format!(
                        "gate cache: removed {} — freed {size}; no write under it could be read",
                        p.target.display()
                    ),
                    _ => format!(
                        "gate cache: removed {} — freed {size}, last written {} ago",
                        p.target.display(),
                        super::human_millis(p.idle_ms(now_ms) as u64)
                    ),
                };
                say(line.clone());
                out.freed += p.bytes;
                out.said.push(line);
                out.removed.push(p);
            }
            Err(e) => {
                let line = format!(
                    "gate cache: {} could NOT be removed: {e}",
                    p.target.display()
                );
                say(line.clone());
                out.said.push(line);
                out.kept.push(p);
            }
        }
    }
    out
}

/// Is anything under `dir` newer than `cutoff_ms`? Answers at the FIRST such file, so a cache a
/// gate wrote to a minute ago costs one `stat` and a stale one costs the whole walk.
pub fn touched_since(dir: &Path, cutoff_ms: i64) -> bool {
    let deadline = std::time::Instant::now() + TOUCH_BUDGET;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            if std::time::Instant::now() > deadline {
                // Not known to be stale, so kept: a walk that ran out of time is not a removal.
                return true;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if mtime_ms(&meta) > cutoff_ms {
                return true;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    false
}

/// Why `dir` must be kept, or `None` when nothing is using it: a live pid in its own `run/`
/// (what a gate placed on a host writes) or a process whose cwd is inside it.
pub fn busy(dir: &Path, here: &Here) -> Option<String> {
    if let Some(why) = live_step(dir, &here.proc_root) {
        return Some(why);
    }
    in_use::why_kept(dir, None, here)
}

/// The `step-<n>.pid` a gate on a declared host writes beside its tree, when its process is alive.
fn live_step(dir: &Path, proc_root: &Path) -> Option<String> {
    let run = dir.join("run");
    let entries = std::fs::read_dir(&run).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("step-") && name.ends_with(".pid")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Some(pid) = text.trim().parse::<u32>().ok() else {
            continue;
        };
        if crate::firecode::held::alive(proc_root, pid) {
            return Some(format!(
                "its gate is running here (pid {pid}, {})",
                entry.path().display()
            ));
        }
    }
    None
}

/// The free space of the filesystem `path` is (or will be) on: the nearest ancestor that exists,
/// because a cache root is measured before it is made.
pub fn space_for(path: &Path) -> Option<Space> {
    let mut at = Some(path);
    while let Some(p) = at {
        if p.exists() {
            return HostDisks.space(p);
        }
        at = p.parent();
    }
    None
}

/// The refusal a gate must not start over: `<free> free on <mount>, needs <need>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Low {
    pub mount: PathBuf,
    pub free: u64,
    pub need: u64,
}

impl Low {
    /// The sentence the queue's row carries.
    pub fn said(&self) -> String {
        format!(
            "{} free on {}, needs {}",
            super::human_bytes(self.free),
            self.mount.display(),
            super::human_bytes(self.need)
        )
    }
}

/// `Some` when the filesystem holding `path` has less than `need` free. A path this box cannot
/// measure is not refused: a gate that will not start because a `df` failed is a worse fault than
/// the one this guard exists for, and the gate says so in its own row.
pub fn low(path: &Path, need: u64) -> Option<Low> {
    let space = space_for(path)?;
    (space.free < need).then_some(Low {
        mount: space.mount,
        free: space.free,
        need,
    })
}

/// The span `[queue] gate_cache_days` is counted in.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How long the plan's own walk of one cache may take: a number the row can wait for.
const WALK_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
const COUNT_CAP: usize = 400_000;
/// The staleness probe's budget, and the removal's: the probe is per cache per pass, the walk is
/// once, on a cache that is about to be given back.
const TOUCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);
const REMOVE_WALK_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
const REMOVE_COUNT_CAP: usize = 4_000_000;

/// What is under `dir`: the bytes, the newest write, and whether the walk finished. A directory
/// too big to walk inside the budget answers what it reached and `false`, which every caller says
/// rather than passing off as a measurement.
fn walk(dir: &Path, budget: std::time::Duration, cap: usize) -> (u64, i64, bool) {
    let deadline = std::time::Instant::now() + budget;
    let mut bytes = 0u64;
    let mut newest = 0i64;
    let mut entries = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            entries += 1;
            if entries > cap || std::time::Instant::now() > deadline {
                return (bytes, newest, false);
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            newest = newest.max(mtime_ms(&meta));
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += meta.len();
            }
        }
    }
    (bytes, newest, true)
}

/// A file's modification time in the store's milliseconds. A file stamped before 1970 (a fixture
/// with a fake clock) is zero, which reads as *ancient* - the safe direction for a removal.
fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture directory that goes when the test does, and a clock a test can move.
    struct Fixture(PathBuf);

    impl Fixture {
        fn new(tag: &str) -> Fixture {
            let p = std::env::temp_dir().join(format!(
                "letibot-gate-cache-{tag}-{}-{}",
                std::process::id(),
                super::super::now_ms()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Fixture(p)
        }

        /// A placement with a `target/` holding a file of `bytes`, stamped `days_ago`.
        fn placement(&self, root: &str, name: &str, bytes: usize, days_ago: i64) -> PathBuf {
            let dir = self.0.join(root).join(name);
            let target = dir.join("target");
            std::fs::create_dir_all(target.join("debug")).unwrap();
            std::fs::write(target.join("debug").join("blob"), vec![b'x'; bytes]).unwrap();
            let when = std::time::SystemTime::now()
                - std::time::Duration::from_secs(86_400 * days_ago as u64);
            stamp(&target.join("debug").join("blob"), when);
            stamp(&target.join("debug"), when);
            stamp(&target, when);
            dir
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A path's mtime, moved - the fake clock a staleness rule needs without waiting three days.
    fn stamp(path: &Path, when: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().read(true).open(path).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    fn roots(f: &Fixture) -> Roots {
        Roots {
            cache: Some(f.0.join("cache")),
            landing: Some(f.0.join("landing")),
            hosts: vec![f.0.clone()],
        }
    }

    /// `Here` for a fixture: this process, and the box's own `/proc` - the in-use rule reads it.
    fn here() -> Here {
        Here::this_process()
    }

    #[test]
    fn a_stale_cache_is_removed_and_a_fresh_one_is_kept() {
        let f = Fixture::new("stale");
        let old = f.placement("cache", "letibot-abc123", 4096, 10);
        let fresh = f.placement("cache", "letibot-def456", 4096, 0);
        let now = super::super::now_ms();
        let found = placements(&roots(&f), &here());
        assert_eq!(found.len(), 2, "{found:#?}");
        for p in &found {
            assert!(p.entire, "a small fixture is measured whole");
            assert_eq!(p.bytes, 4096);
        }

        let mut said = Vec::new();
        let gc = gc(&roots(&f), &here(), now, 3, &mut |l| said.push(l));
        assert_eq!(gc.removed.len(), 1, "{gc:#?}");
        assert_eq!(gc.removed[0].dir, old);
        assert_eq!(gc.freed, 4096);
        assert!(!old.join("target").exists(), "the stale cache is gone");
        assert!(fresh.join("target").exists(), "the fresh one is not");
        assert_eq!(said.len(), 1);
        assert!(
            said[0].contains("freed 4 KiB") && said[0].contains("last written 10d0h ago"),
            "the line says what was freed and why: {}",
            said[0]
        );
        // Under a cache root the child IS the cache, so the whole of it goes; the sibling with a
        // fresh write is what proves the rule is about age and not about the root.
        assert!(!old.exists(), "a cache root's child is the cache");
        assert!(fresh.is_dir(), "the fresh one is untouched");
    }

    #[test]
    fn a_cache_a_gate_is_running_in_is_kept() {
        let f = Fixture::new("busy");
        let dir = f.placement("landing", "letibot-abc123", 1024, 30);
        // A live pid in the placement's `run/`: what a gate placed on a host writes there.
        std::fs::create_dir_all(dir.join("run")).unwrap();
        std::fs::write(
            dir.join("run").join("step-1.pid"),
            std::process::id().to_string(),
        )
        .unwrap();
        let mut said = Vec::new();
        let gc = gc(&roots(&f), &here(), super::super::now_ms(), 3, &mut |l| {
            said.push(l)
        });
        assert!(gc.removed.is_empty(), "{gc:#?}");
        assert_eq!(gc.kept.len(), 1);
        assert!(
            gc.kept[0]
                .busy
                .as_deref()
                .unwrap_or_default()
                .contains("its gate is running here"),
            "{:?}",
            gc.kept[0].busy
        );
        assert!(dir.join("target").exists(), "a running gate's cache stays");
        assert!(said.is_empty());
    }

    #[test]
    fn a_placement_with_no_target_is_not_a_cache() {
        let f = Fixture::new("notarget");
        std::fs::create_dir_all(f.0.join("landing").join("letibot-abc123").join("tree")).unwrap();
        assert!(
            placements(&roots(&f), &here()).is_empty(),
            "a placement that never built anything holds no cache"
        );
    }

    #[test]
    fn a_low_disk_is_a_refusal_that_names_the_filesystem_and_both_numbers() {
        let f = Fixture::new("low");
        // A floor nothing can meet on any filesystem: the refusal, not an absent measurement.
        let refused = low(&f.0.join("cache"), u64::MAX / 2).expect("a 8 EiB floor is nobody's");
        assert!(refused.free > 0);
        assert_eq!(refused.need, u64::MAX / 2);
        let said = refused.said();
        assert!(said.contains("needs "), "{said}");
        assert!(said.contains("free on "), "{said}");
        assert!(
            said.contains(&refused.mount.display().to_string()),
            "{said}"
        );
        // And a floor of nothing is always met.
        assert!(low(&f.0, 0).is_none());
    }

    #[test]
    fn a_cache_root_that_does_not_exist_yet_is_measured_at_its_nearest_ancestor() {
        let f = Fixture::new("unmade");
        let deep = f.0.join("build/letibot-gate");
        let space = space_for(&deep).expect("the nearest ancestor that exists is on a disk");
        let at = space_for(&f.0).expect("the fixture's own disk");
        assert_eq!(
            space.mount, at.mount,
            "the same filesystem, through a parent"
        );
        assert!(space.free > 0);
    }

    /// A placement's tree is untouched by the rule: only `target/` is a cache.
    #[test]
    fn the_gc_removes_only_the_target_directory() {
        let f = Fixture::new("onlytarget");
        let dir = f.placement("landing", "letibot-abc123", 100, 9);
        std::fs::create_dir_all(dir.join("tree").join("src")).unwrap();
        std::fs::write(dir.join("tree").join("src").join("main.rs"), "fn main() {}").unwrap();
        let gc = gc(&roots(&f), &here(), super::super::now_ms(), 3, &mut |_| {});
        assert_eq!(gc.removed.len(), 1, "{gc:#?}");
        assert!(!dir.join("target").exists());
        assert!(dir.join("tree").join("src").join("main.rs").exists());
    }
}
