//! The gate's build disk: where a gate's `target/` lives, the pass that gives a stale one back,
//! and the floor below which a gate does not start at all.
//!
//! A gate builds beside the tree it judges - `<landing root>/<repo>-<hex>/target` here, or
//! `<host root>/gate/<repo>-<hex>/target` on a machine `[queue] gate_host` names. On a box whose
//! root disk is small that is the wrong disk, and nothing ever removed one: 2026-11-24 left 223 GB
//! of a stale gate cache on lab2x1's root, and firecode then refused to boot a VM at 6 GiB free.
//!
//! Three answers, each a key in `[queue]`:
//!
//! - `gate_cache_root` moves every gate's cache to `<root>/<repo>-<hex>`, on the box's build disk.
//! - `gate_cache_days` (default [`GATE_CACHE_DAYS`]) is how long a cache may go unwritten before a
//!   pass removes it - [`gc_now`], run when the daemon starts and before each gate.
//! - `gate_min_free` (default [`GATE_MIN_FREE`]) is the floor: a gate whose target's filesystem has
//!   less does not start, and its row says `gate not run: <free> free on <fs>, needs <need>`.
//!
//! The scanning, the size and the staleness rule are `letibot_tools::lifecycle::gate_cache`'s; this
//! module is the policy, and the three places it is applied (the target's path, the daemon's pass,
//! a gate's guard).

use std::path::{Path, PathBuf};

use letibot_tools::lifecycle::autoarchive::Here;
use letibot_tools::lifecycle::gate_cache as tool;
use letibot_tools::remote::{Hosts, Paths};

use super::settings::GATE_CACHE_DAYS;

/// `[queue] gate_cache_root`, `gate_cache_days` and `gate_min_free`, as the gate reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Where every gate's build cache is, `<root>/<repo>-<hex>`. `None` keeps the placement's own
    /// `target/` - what every box did before this key existed.
    pub root: Option<PathBuf>,
    /// A cache nothing has written to for this many days is removed.
    pub days: u64,
    /// Free space a gate's filesystem - and the landing root's - must have, in bytes. **Zero is no
    /// floor**, which is what a fixture gets: a test's gate must not depend on how full the box it
    /// runs on is. The daemon's floor comes from `[queue] gate_min_free`.
    pub min_free: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            root: None,
            days: GATE_CACHE_DAYS,
            min_free: 0,
        }
    }
}

impl Policy {
    /// A project's settings as this policy. `gate_cache_root` is kept as written (`~` and all):
    /// this box expands it here, and a gate on a declared host expands it THERE.
    pub fn of(settings: &super::settings::QueueSettings) -> Policy {
        Policy {
            root: settings.gate_cache_root.clone(),
            days: settings.gate_cache_days,
            min_free: settings.gate_min_free,
        }
    }

    /// `gate_cache_root` as this box spells it - `None` when unset.
    pub fn root_here(&self) -> Option<PathBuf> {
        self.root.as_deref().map(home_of)
    }

    /// The same root for a gate on a declared host: `expand_root` keeps a `~` for the FAR shell,
    /// because `~` is that machine's home and not this one's.
    pub fn root_there(&self) -> Option<String> {
        self.root
            .as_deref()
            .map(|r| letibot_tools::remote::expand_root(&r.display().to_string()))
    }

    /// Where this policy puts the build cache of the tree `worktree`: `<root>/<the tree's own
    /// name>`, the name the landing directory already uses - so one repository's cache is one
    /// directory on the build disk, whatever box asked for it.
    pub fn target_for(&self, worktree: &Path) -> Option<PathBuf> {
        Some(self.root_here()?.join(worktree.file_name()?))
    }
}

/// `~/x` as this box's home, and everything else as it is.
fn home_of(p: &Path) -> PathBuf {
    let text = p.display().to_string();
    match text.strip_prefix('~') {
        Some(rest) => {
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            Paths::from_env().home().join(rest)
        }
        None => p.to_path_buf(),
    }
}

static POLICY: std::sync::Mutex<Option<Policy>> = std::sync::Mutex::new(None);

fn slot() -> std::sync::MutexGuard<'static, Option<Policy>> {
    POLICY.lock().unwrap_or_else(|e| e.into_inner())
}

/// `[queue]`'s three keys, set by the daemon once per start: every gate reads this slot rather than
/// the file again, so an edited key applies to the next gate and not to the next restart.
pub fn set(policy: Option<Policy>) {
    *slot() = policy;
}

/// The policy in force: [`Policy::default`] where nothing set one, which is every fixture.
pub fn policy() -> Policy {
    slot().clone().unwrap_or_default()
}

/// [`Policy::target_for`] for the policy in force - what a gate's step asks when it builds.
pub fn target_for(worktree: &Path) -> Option<PathBuf> {
    policy().target_for(worktree)
}

/// Every root this box's gates can have left a cache in: the configured cache root, the landing
/// root, and each declared host's placement root as this box sees it.
///
/// **Empty in a test build.** A box's roots are `~/letibot-remote/gate/*` and the real data
/// directory's landing trees, and a unit test that started a queue service would remove a real
/// gate's cache: the same rule as [`super::git::landing_root`]'s, which refuses a test the real
/// landing root for the same reason. A test that wants the pass builds its own [`tool::Roots`].
pub fn roots() -> tool::Roots {
    if cfg!(test) {
        return tool::Roots::default();
    }
    tool::Roots {
        cache: policy().root_here(),
        ..boxes()
    }
}

/// The roots a READ-ONLY reader scans - `letibot holds`, the bridge's `gc_plan`. The same rule as
/// [`roots`], with one difference a reader needs and a daemon does not: `[queue] gate_cache_root`
/// is read from the project at `repo`, because a reader process has no daemon to have read the
/// file for it. Nothing is removed here, so a reader may look at a box's roots in any build.
pub fn roots_for(repo: Option<&Path>) -> tool::Roots {
    let cache = repo
        .and_then(|r| super::settings::for_repository(r).ok())
        .and_then(|s| s.gate_cache_root)
        .map(|p| home_of(&p));
    let mut roots = boxes();
    if cache.is_some() {
        roots.cache = cache;
    }
    roots
}

/// The box's own roots, whatever a project file says: the landing root and each declared host's
/// placement root. [`roots`] adds the daemon's policy to these; [`roots_for`] a project's.
pub fn boxes() -> tool::Roots {
    tool::Roots {
        cache: None,
        landing: super::git::landing_root().ok(),
        hosts: host_roots(),
    }
}

/// Each declared host's root as this box sees it, deduped, absolute ones only.
fn host_roots() -> Vec<PathBuf> {
    let mut hosts: Vec<PathBuf> = Vec::new();
    let declared = Hosts::load();
    for name in declared.names() {
        let Some(host) = declared.get(&name) else {
            continue;
        };
        let root = home_of(Path::new(&host.root));
        // Absolute only: a `root` this box cannot place is another machine's, and its paths are
        // not directories here.
        if root.is_absolute() && !hosts.contains(&root) {
            hosts.push(root);
        }
    }
    hosts
}

/// One line per removal on the daemon's stderr, where a person reads what a start did.
pub fn log(line: &str) {
    eprintln!("  merge queue: {line}");
}

/// The pass: remove every gate cache nothing has written to for `days`, and never one a gate is
/// running in. Answers what it did, for a caller that wants to say so.
pub fn gc_now(roots: &tool::Roots, days: u64) -> tool::Gc {
    let here = Here::this_process();
    let now = super::gate::now_ms() as i64;
    let done = tool::gc(roots, &here, now, days, &mut |_| {});
    for line in &done.said {
        log(line);
    }
    done
}

/// The bounds the host's own sweep keeps per cache: measuring one, and removing one. A 223 GB
/// `target/` is minutes of work, and a gate start is not the place to spend them.
const MEASURE_SECONDS: u64 = 60;
const REMOVE_SECONDS: u64 = 600;

/// How long this box waits for a host's whole sweep before it stops listening: the removal bounds
/// above, and room for a handful of placements.
pub const SWEEP_BOUND: std::time::Duration = std::time::Duration::from_secs(900);

/// The same pass on a declared host, as one shell script: the caches are on that machine and this
/// box cannot `stat` them, so the rule is spelled where the files are.
///
/// A placement is skipped when it cannot be shown stale-and-idle: a `find` that fails (an old find
/// without `-newermt`) keeps it, a `run/*.beat` newer than a step's tick is a gate running there
/// now, and a live pid in `run/*.pid` is one that is still in its steps. Only `target/` goes. The
/// two expensive steps are bounded on that machine (`timeout`), so one enormous cache cannot hold a
/// gate start for half an hour: what a bounded run did not finish, the next pass finishes.
pub fn remote_sweep(gate_root: &str, days: u64) -> String {
    format!(
        "for d in '{gate_root}'/*/; do \
           t=\"${{d%/}}/target\"; \
           [ -d \"$t\" ] || continue; \
           beats=$(find \"${{d%/}}/run\" -maxdepth 1 -name '*.beat' -newermt '-15 minutes' \
                   -print -quit 2>/dev/null) || continue; \
           [ -z \"$beats\" ] || continue; \
           live=; \
           for p in \"${{d%/}}\"/run/*.pid; do \
             [ -f \"$p\" ] || continue; \
             kill -0 \"$(cat \"$p\" 2>/dev/null)\" 2>/dev/null && live=1; \
           done; \
           [ -z \"$live\" ] || continue; \
           fresh=$(find \"$t\" -newermt '-{days} days' -print -quit 2>/dev/null) || continue; \
           [ -z \"$fresh\" ] || continue; \
           b=$(timeout {MEASURE_SECONDS} du -sk \"$t\" 2>/dev/null | awk '{{print $1}}'); \
           if timeout {REMOVE_SECONDS} rm -rf \"$t\"; then \
             echo \"gate cache: removed $t — freed ${{b}}k\"; \
           fi; \
         done; true"
    )
}

/// What a gate must be told before it starts: `Err` is the sentence its row carries instead of a
/// verdict, and the pass that ran first freed `freed` bytes of stale cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Guarded {
    /// What the pass before the check gave back, so a row can say the GC ran and was not enough.
    pub freed: u64,
}

/// The floor check, with the pass in front of it: `Err` when the gate must not start.
///
/// Both filesystems are asked - the one the gate's target will be on, and the landing root's -
/// and the stale caches are removed before the second look. A filesystem this box cannot measure
/// is not a refusal: a gate that will not start because a `df` failed is a worse fault than a full
/// disk, and the gate's own step would say so.
pub fn guard(
    target: &Path,
    landing: Option<&Path>,
    policy: &Policy,
    roots: &tool::Roots,
) -> Result<Guarded, String> {
    let need = policy.min_free;
    if need == 0 {
        return Ok(Guarded { freed: 0 });
    }
    let Some(tight) = worst(target, landing, need) else {
        return Ok(Guarded { freed: 0 });
    };
    let done = gc_now(roots, policy.days);
    let Some(still) = tool::low(&tight, need) else {
        // Room enough once the stale caches are back; the row may still say what paid for it.
        return Ok(Guarded { freed: done.freed });
    };
    let gc = match (done.freed, done.removed.len()) {
        (0, _) => "nothing stale was there to remove".to_string(),
        (freed, n) => format!(
            "{} was freed from {} stale cache(s) and that was not enough",
            letibot_tools::lifecycle::human_bytes(freed),
            n
        ),
    };
    Err(format!(
        "gate not run: {}; {gc}. `[queue] gate_min_free` is this floor, and `gate_cache_root` \
         moves the build cache to a disk with room",
        still.said()
    ))
}

/// The tightest of the filesystems a gate cares about, as one path on it: the target's own, and
/// the landing root's, because the tree the gate copies into is written there too.
fn worst(target: &Path, landing: Option<&Path>, need: u64) -> Option<PathBuf> {
    let mut worst: Option<(PathBuf, u64)> = None;
    for path in std::iter::once(target).chain(landing) {
        let Some(space) = tool::space_for(path) else {
            continue;
        };
        if space.free >= need {
            continue;
        }
        if worst.as_ref().is_none_or(|(_, free)| space.free < *free) {
            worst = Some((path.to_path_buf(), space.free));
        }
    }
    worst.map(|(p, _)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "letibot-gate-cache-policy-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A project that says nothing gets the placement's own `target/`, three days, and no floor.
    #[test]
    fn the_default_policy_is_the_ordinary_gate() {
        let p = Policy::default();
        assert_eq!(p.root, None, "a cache root is an operator's answer");
        assert_eq!(p.days, GATE_CACHE_DAYS);
        assert_eq!(p.min_free, 0, "a fixture is under no floor");
        let tree = Path::new("/landing/letibot-0123abcdef");
        assert_eq!(p.target_for(tree), None, "no root, no move");
        let target = super::super::gate::gate_target_dir(tree);
        assert!(target.ends_with("target"));
        assert!(
            target.starts_with(tree),
            "the gate builds in its own tree: {}",
            target.display()
        );
    }

    /// A configured root moves the build cache off the tree's disk, under the tree's own name.
    #[test]
    fn a_cache_root_moves_the_target_and_gives_the_build_disk() {
        let dir = scratch("root");
        let tree = Path::new("/landing/letibot-0123abcdef");
        let p = Policy {
            root: Some(dir.join("build/letibot-gate")),
            ..Default::default()
        };
        let target = p.target_for(tree).expect("a root puts it there");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(target, dir.join("build/letibot-gate/letibot-0123abcdef"));
        assert!(
            !target.starts_with(tree),
            "the cache is off the tree's own filesystem"
        );
    }

    /// `~` is THIS box's home here and the far machine's there - one key, two machines.
    #[test]
    fn a_tilde_root_is_expanded_where_the_gate_runs() {
        let p = Policy {
            root: Some(PathBuf::from("~/build/gate")),
            ..Default::default()
        };
        let home = Paths::from_env().home().to_path_buf();
        assert_eq!(p.root_here(), Some(home.join("build/gate")));
        assert_eq!(
            p.root_there().as_deref(),
            Some("\"$HOME/build/gate\""),
            "the far shell expands its own home"
        );
    }

    /// The refusal names the filesystem and both numbers, and it is a sentence a row can carry.
    #[test]
    fn a_floor_nothing_can_meet_is_a_gate_not_run_row() {
        let dir = scratch("floor");
        let refused = guard(
            &dir,
            None,
            &Policy {
                min_free: u64::MAX / 2,
                ..Default::default()
            },
            &tool::Roots::default(),
        )
        .expect_err("no filesystem has 8 EiB free");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(refused.starts_with("gate not run: "), "{refused}");
        assert!(refused.contains("free on "), "{refused}");
        assert!(refused.contains("needs "), "{refused}");
        assert!(
            refused.contains("nothing stale was there to remove"),
            "the pass ran first and says it did: {refused}"
        );
        assert!(
            refused.contains("gate_min_free"),
            "the row names the key that set the floor: {refused}"
        );
    }

    /// No floor is no check: a fixture's gate starts whatever the box has free.
    #[test]
    fn no_floor_is_no_check() {
        let dir = scratch("nofloor");
        let ok = guard(
            &dir,
            Some(&dir),
            &Policy::default(),
            &tool::Roots::default(),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(ok.expect("zero is no floor").freed, 0);
    }

    /// The pass a gate start runs before the floor's second look: one stale cache goes, a fresh one
    /// and one a gate is running in stay, and the line says what was freed. A fake clock stamps the
    /// stale one ten days back rather than the test waiting three.
    #[test]
    fn the_pass_removes_a_stale_cache_and_keeps_a_fresh_or_running_one() {
        let root = scratch("pass");
        let placement = |name: &str, days_ago: u64| {
            let target = root.join("gate").join(name).join("target");
            std::fs::create_dir_all(target.join("debug")).unwrap();
            std::fs::write(target.join("debug").join("blob"), vec![b'x'; 2048]).unwrap();
            let when =
                std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * days_ago);
            for p in [
                target.join("debug").join("blob"),
                target.join("debug"),
                target.clone(),
            ] {
                let f = std::fs::OpenOptions::new().read(true).open(&p).unwrap();
                f.set_times(std::fs::FileTimes::new().set_modified(when))
                    .unwrap();
            }
            target
        };
        let stale = placement("letibot-aaaaaaaaaaaa", 10);
        let fresh = placement("letibot-bbbbbbbbbbbb", 0);
        let running = placement("letibot-cccccccccccc", 10);
        let run = running.parent().unwrap().join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("step-1.pid"), std::process::id().to_string()).unwrap();

        let roots = tool::Roots {
            hosts: vec![root.clone()],
            ..Default::default()
        };
        let done = gc_now(&roots, GATE_CACHE_DAYS);
        assert_eq!(done.removed.len(), 1, "{done:#?}");
        assert!(!stale.exists(), "the stale cache is gone");
        assert!(fresh.is_dir(), "a cache written today is not stale");
        assert!(
            running.is_dir(),
            "a cache with its gate's pid in `run/` is never removed"
        );
        assert_eq!(done.freed, 2048);
        assert!(
            done.said
                .iter()
                .any(|l| l.contains("gate cache: removed") && l.contains("freed 2 KiB")),
            "one line per removal, with the size freed: {:#?}",
            done.said
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
