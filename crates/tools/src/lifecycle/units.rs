//! The per-unit artifact dirs cargo keeps under `<target>/<profile>/build/<package>/<hash>`.
//!
//! Cargo 1.100 stores a unit's whole output there - a linked test binary included - and never
//! removes a hash whose build configuration stopped being used. One directory per test target of a
//! crate, multiplied by every configuration the box has ever built in (rustflags, a profile
//! override, clippy beside test), is what takes a shared target dir to terabytes: measured here,
//! `letibot-sessionlog` alone is 19 GB in 46 dirs after one build and 91 dirs after a second
//! configuration. So the rule is per UNIT - the newest few of each, never "the newest few of the
//! package", which would throw away the test binaries the current build just made.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Artifact dirs kept per unit per profile. Two, because a box alternates between the gate's
/// configuration and a session's, so a third variant is a rebuild nobody minds paying for and a
/// second one is a build that does not repeat.
pub const KEEP: usize = 2;

/// Nothing younger than this is touched: a build that started an hour ago may still be linking, and
/// the directory it is writing into looks exactly like one nobody wants.
pub const GRACE: Duration = Duration::from_secs(60 * 60);

/// The lock a pass holds. A second pass answers "someone else is pruning" rather than racing it.
const LOCK: &str = ".letibot-units.lock";

/// A lock file nobody removed: the pass that took it died. Older than this and it is taken over,
/// because one crashed pruner must not stop every later one.
const LOCK_STALE: Duration = Duration::from_secs(30 * 60);

/// What a pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
    /// Artifact dirs left in place: everything within [`KEEP`] of its unit, plus anything too young.
    pub kept: usize,
    /// Another pass held the target's lock, so nothing here was looked at.
    pub busy: bool,
}

impl Pruned {
    /// The sentence a caller logs, or `None` when the pass removed nothing.
    pub fn line(&self) -> Option<String> {
        if self.busy {
            return None;
        }
        (!self.removed.is_empty()).then(|| {
            format!(
                "unit dirs: removed {} ({} freed), {} kept",
                self.removed.len(),
                crate::lifecycle::human_bytes(self.bytes),
                self.kept
            )
        })
    }
}

/// Remove every `build/<package>/<hash>` artifact dir past the newest `keep` of ITS unit that has
/// not been written to for `grace`.
///
/// Only `<target>/<profile>/build/<package>/<hash>` is a candidate: a name that is not sixteen hex
/// characters is another layout's (cargo's older `<package>-<hash>` suffix) and is left alone. A
/// dir whose unit cannot be read is nobody's to guess at, so it is kept too.
pub fn prune(target: &Path, keep: usize, grace: Duration, now: SystemTime) -> Pruned {
    let mut out = Pruned::default();
    if !target.is_dir() {
        return out;
    }
    let Some(_lock) = Lock::take(&target.join(LOCK), now) else {
        out.busy = true;
        return out;
    };
    let cutoff = now.checked_sub(grace).unwrap_or(SystemTime::UNIX_EPOCH);
    for (profile, build) in build_dirs(target) {
        // Cargo holds an exclusive `flock` on `<profile>/.cargo-lock` for the whole of a build, so
        // this is the one question that says "a build is in here" without guessing from mtimes.
        let Some(_cargo) = CargoLock::take(&profile) else {
            continue;
        };
        for package in packages(&build) {
            for mut dirs in by_unit(&package).into_values() {
                // Newest first, ties broken by name so two passes agree on which survive.
                dirs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
                out.kept += dirs.len().min(keep);
                for (dir, written) in dirs.into_iter().skip(keep) {
                    if written > cutoff {
                        out.kept += 1;
                        continue;
                    }
                    let bytes = bytes_of(&dir);
                    match std::fs::remove_dir_all(&dir) {
                        Ok(()) => {
                            out.bytes += bytes;
                            out.removed.push(dir);
                        }
                        // Something another process holds, or one this user may not remove: kept,
                        // and the next pass asks again.
                        Err(_) => out.kept += 1,
                    }
                }
            }
        }
    }
    out
}

/// The `<profile>` and `<profile>/build` dirs of a target: `debug`, `release`, a custom profile's.
fn build_dirs(target: &Path) -> Vec<(PathBuf, PathBuf)> {
    let Ok(profiles) = std::fs::read_dir(target) else {
        return Vec::new();
    };
    profiles
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .map(|p| {
            let build = p.join("build");
            (p, build)
        })
        .filter(|(_, b)| b.is_dir())
        .collect()
}

/// The package dirs under one `build`, sorted so a report and a second pass read the same order.
fn packages(build: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(build) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// One package's artifact dirs, grouped by the unit each one holds, with the newest time written.
fn by_unit(package: &Path) -> BTreeMap<String, Vec<(PathBuf, SystemTime)>> {
    let Ok(entries) = std::fs::read_dir(package) else {
        return BTreeMap::new();
    };
    let mut out: BTreeMap<String, Vec<(PathBuf, SystemTime)>> = BTreeMap::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !is_hash(&name) || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = e.path();
        // An unreadable unit keeps the dir out of every group, because a guess about what an
        // unknown directory belongs to is how a build loses the binary it just linked.
        let Some(key) = unit_key(&dir) else {
            out.entry(name).or_default();
            continue;
        };
        let written = newest(&dir);
        out.entry(key).or_default().push((dir, written));
    }
    out
}

/// Which unit an artifact dir holds, as the name its fingerprint files agree on -
/// `test-integration-test-sessions`, `lib-letibot_sessionlog`, `bin-harnessd` - or `None` when the
/// dir says nothing (an interrupted build) or says two things.
pub fn unit_key(dir: &Path) -> Option<String> {
    let mut names: BTreeMap<String, ()> = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("fingerprint")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == "invoked.timestamp" {
                continue;
            }
            let name = name.trim_end_matches(".json");
            let bare = name
                .strip_prefix("dep-")
                .or_else(|| name.strip_prefix("output-"))
                .unwrap_or(name);
            names.insert(bare.to_string(), ());
        }
    }
    if names.len() != 1 {
        return None;
    }
    names.into_keys().next()
}

/// Is this a cargo unit hash: sixteen hex characters, the whole name.
fn is_hash(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The newest mtime at the top of `dir` and among its children. The dir's own mtime only moves when
/// an entry is added, so a rewritten `out/` would read as old without the second level.
fn newest(dir: &Path) -> SystemTime {
    let mut out = std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                out = out.max(t);
            }
        }
    }
    out
}

/// The space `dir` occupies in the unit the rest of the box's disk accounting uses: allocated
/// blocks, so a sparse file is not counted as its length.
fn bytes_of(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut stack = vec![dir.to_path_buf()];
    let mut out = 0u64;
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            out += meta.blocks() * 512;
            if meta.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out
}

/// Cargo's own lock on one profile dir, held for as long as the pass is inside it.
///
/// Taking it is how a pass asks "is a build running here?" and how it keeps one from starting
/// under a removal; the kernel drops it if this process dies, so it cannot be left behind.
struct CargoLock(Option<std::fs::File>);

impl CargoLock {
    fn take(profile: &Path) -> Option<CargoLock> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(profile.join(".cargo-lock"))
            .ok()?;
        // SAFETY: `file` owns a live descriptor for the length of the call, and `how` is a flock flag.
        let got = unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&file),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        } == 0;
        got.then_some(CargoLock(Some(file)))
    }
}

impl Drop for CargoLock {
    fn drop(&mut self) {
        if let Some(file) = &self.0 {
            // SAFETY: as above - the descriptor is live until `self` goes.
            unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(file), libc::LOCK_UN) };
        }
    }
}

/// The target's pruning lock, removed on every way out including a panic.
struct Lock(PathBuf);

impl Lock {
    fn take(path: &Path, now: SystemTime) -> Option<Lock> {
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => return Some(Lock(path.to_path_buf())),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let died = std::fs::metadata(path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| now.duration_since(t).ok())
                        .is_some_and(|age| age > LOCK_STALE);
                    if !died {
                        return None;
                    }
                    let _ = std::fs::remove_file(path);
                }
                Err(_) => return None,
            }
        }
        None
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fx(PathBuf);

    impl Fx {
        fn new(name: &str) -> Fx {
            let d =
                std::env::temp_dir().join(format!("letibot-units-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            Fx(d)
        }

        /// One artifact dir of `unit`, `kb` KiB, last written `age` before `now`. The fingerprint
        /// files are what say which unit it is, exactly as cargo writes them.
        fn unit(
            &self,
            package: &str,
            unit: &str,
            hash: &str,
            kb: usize,
            age: Duration,
            now: SystemTime,
        ) -> PathBuf {
            let dir = self.0.join("debug/build").join(package).join(hash);
            let fp = dir.join("fingerprint");
            std::fs::create_dir_all(&fp).unwrap();
            std::fs::create_dir_all(dir.join("out")).unwrap();
            std::fs::write(fp.join(format!("test-integration-test-{unit}.json")), b"{}").unwrap();
            std::fs::write(fp.join(format!("output-test-integration-test-{unit}")), b"").unwrap();
            std::fs::write(fp.join("invoked.timestamp"), b"").unwrap();
            std::fs::write(
                dir.join("out").join(format!("{unit}-{hash}")),
                vec![b'x'; kb * 1024],
            )
            .unwrap();
            backdate(&dir, now - age);
            dir
        }

        fn hashes(&self, package: &str) -> Vec<String> {
            let mut out: Vec<String> = std::fs::read_dir(self.0.join("debug/build").join(package))
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            out.sort();
            out
        }
    }

    /// Backdate one path. Read-only is enough: `futimens` asks who owns the file, not which way it
    /// was opened, and a directory cannot be opened for writing at all.
    fn filetime(p: &Path, when: SystemTime) {
        let f = std::fs::File::open(p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    /// Backdate everything under a fixture dir, the `fingerprint/` files included: cargo rewrites
    /// them with the unit, and the newest of them is what the real pass judges.
    fn backdate(dir: &Path, when: SystemTime) {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in entries.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(e.path());
                }
                filetime(&e.path(), when);
            }
            filetime(&d, when);
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn days(n: u64) -> Duration {
        Duration::from_secs(86_400 * n)
    }

    /// Three configurations of one test target: the newest two stay, the oldest goes.
    #[test]
    fn repeated_variants_of_one_unit_leave_the_newest_two() {
        let fx = Fx::new("variants");
        let dirs: Vec<PathBuf> = (0..3)
            .map(|i| {
                fx.unit(
                    "letibot-harnessd",
                    "sessions",
                    &format!("{:016x}", i + 1),
                    64,
                    days(3 - i),
                    now(),
                )
            })
            .collect();
        let out = prune(&fx.0, KEEP, GRACE, now());
        assert_eq!(out.removed, vec![dirs[0].clone()], "the oldest of three");
        assert_eq!(out.kept, KEEP);
        assert!(
            out.bytes >= 64 * 1024,
            "what was freed is reported: {}",
            out.bytes
        );
        assert_eq!(
            fx.hashes("letibot-harnessd"),
            vec!["0000000000000002", "0000000000000003"]
        );
    }

    /// The count is per unit, not per package: a crate's hundred test targets each keep their one
    /// dir, because throwing those away would relink every test binary the build just made.
    #[test]
    fn one_dir_of_each_unit_is_never_touched() {
        let fx = Fx::new("per-unit");
        for i in 0..20 {
            fx.unit(
                "letibot-tools",
                &format!("case_{i}"),
                &format!("{:016x}", i + 1),
                4,
                days(30),
                now(),
            );
        }
        let out = prune(&fx.0, KEEP, GRACE, now());
        assert!(out.removed.is_empty(), "{:?}", out.removed);
        assert_eq!(out.kept, 20);
        assert_eq!(fx.hashes("letibot-tools").len(), 20);
    }

    /// A build that started minutes ago is young, whatever its rank: the grace is what makes a
    /// concurrent build beside this pass safe, so it is checked before the count.
    #[test]
    fn a_young_artifact_dir_is_never_removed() {
        let fx = Fx::new("young");
        for i in 0..3 {
            fx.unit(
                "letibot-bridge",
                "room",
                &format!("{:016x}", i + 1),
                8,
                Duration::from_secs(5),
                now(),
            );
        }
        let out = prune(&fx.0, 1, GRACE, now());
        assert!(out.removed.is_empty(), "every one of them is seconds old");
        assert_eq!(out.kept, 3);
    }

    /// A second pass answers "someone else is pruning" and touches nothing - two daemons and a
    /// human's `letibot gc` can arrive at one target at the same instant.
    #[test]
    fn a_held_lock_makes_the_second_pass_do_nothing() {
        let fx = Fx::new("lock");
        for i in 0..3 {
            fx.unit(
                "letibot",
                "main",
                &format!("{:016x}", i + 1),
                8,
                days(9),
                now(),
            );
        }
        let held = Lock::take(&fx.0.join(LOCK), now()).expect("the fixture takes the lock");
        let out = prune(&fx.0, 1, GRACE, now());
        assert!(out.busy);
        assert!(out.removed.is_empty());
        assert_eq!(fx.hashes("letibot").len(), 3);
        drop(held);
        // And the lock going is what lets the next pass work at all.
        assert_eq!(prune(&fx.0, 1, GRACE, now()).removed.len(), 2);
    }

    /// A lock file left by a pass that died is taken over; one a live pass holds is not.
    #[test]
    fn a_stale_lock_is_taken_over() {
        let fx = Fx::new("stale-lock");
        std::fs::create_dir_all(&fx.0).unwrap();
        std::fs::write(fx.0.join(LOCK), b"").unwrap();
        filetime(&fx.0.join(LOCK), now() - Duration::from_secs(2 * 3600));
        assert!(Lock::take(&fx.0.join(LOCK), now()).is_some());
    }

    /// Cargo's older layout names the artifact dir `<package>-<hash>` and keeps files in it: a name
    /// that is not a bare hash is another layout's, and nothing under it is removed.
    #[test]
    fn another_layout_is_left_alone() {
        let fx = Fx::new("flat");
        for i in 0..4 {
            let dir =
                fx.0.join("debug/build")
                    .join(format!("letibot-vt-{:016x}", i + 1))
                    .join("out");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("blob"), vec![b'x'; 4096]).unwrap();
            backdate(dir.parent().unwrap(), now() - days(90));
        }
        let out = prune(&fx.0, 1, GRACE, now());
        assert!(out.removed.is_empty(), "{:?}", out.removed);
        assert_eq!(
            std::fs::read_dir(fx.0.join("debug/build")).unwrap().count(),
            4
        );
    }

    /// A dir whose fingerprint says nothing is nobody's to guess at: it is kept, not removed.
    #[test]
    fn an_unreadable_unit_is_kept() {
        let fx = Fx::new("unknown");
        let dir = fx.0.join("debug/build/letibot-code/deadbeefdeadbeef");
        std::fs::create_dir_all(&dir).unwrap();
        backdate(&dir, now() - days(90));
        let out = prune(&fx.0, 1, GRACE, now());
        assert!(out.removed.is_empty());
        assert!(dir.is_dir());
    }

    /// A release profile is a separate `build` dir and is pruned by the same rule.
    #[test]
    fn every_profile_is_pruned() {
        let fx = Fx::new("profiles");
        for profile in ["debug", "release"] {
            for i in 0..3 {
                let dir =
                    fx.0.join(profile)
                        .join("build/letibot-harnessd")
                        .join(format!("{:016x}", i + 1));
                let fp = dir.join("fingerprint");
                std::fs::create_dir_all(&fp).unwrap();
                std::fs::write(fp.join("lib-letibot_harnessd.json"), b"{}").unwrap();
                backdate(&dir, now() - days(3 - i));
            }
        }
        let out = prune(&fx.0, 1, GRACE, now());
        assert_eq!(out.removed.len(), 4);
        for profile in ["debug", "release"] {
            let n = std::fs::read_dir(fx.0.join(profile).join("build/letibot-harnessd"))
                .unwrap()
                .count();
            assert_eq!(n, 1, "{profile}");
        }
    }
}
