//! What a target dir does under repeated builds, and what the prune leaves of it.
//!
//! Cargo 1.100 keeps one artifact directory per unit under `<target>/<profile>/build/<package>/
//! <hash>` and never removes a hash whose build configuration stopped being used, so one probe
//! crate built in three configurations is three directories of the SAME unit. The assertion that
//! matters is that the second and third builds ADD a directory and that [`prune`] takes them back
//! to `KEEP` - one dir per unit per kept configuration - because a pruner that keeps "the newest
//! few of the package" instead would relink every test binary on every build.
//!
//! This drives the real `cargo`: the layout is cargo's, so a fixture of directories would prove
//! nothing about it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use letibot_tools::lifecycle::units;

/// The unit the probe's lib and its test are: `lib-unitprobe`.
const KEY: &str = "lib-unitprobe";

/// A crate with no dependencies, in a directory of its own, so `cargo` can run offline.
struct Probe(PathBuf);

impl Probe {
    fn new(name: &str) -> Probe {
        let dir = std::env::temp_dir().join(format!("letibot-units-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("p/src")).unwrap();
        std::fs::write(
            dir.join("p/Cargo.toml"),
            "[package]\nname = \"unitprobe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("p/src/lib.rs"), "pub fn f() {}\n").unwrap();
        // The layout this test measures is cargo's, and it changed with the version: the tree's
        // pin is what the box builds with, so the probe is built by the same one.
        let pin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rust-toolchain.toml");
        if let Ok(text) = std::fs::read_to_string(&pin) {
            std::fs::write(dir.join("rust-toolchain.toml"), text).unwrap();
        }
        Probe(dir)
    }

    fn target(&self) -> PathBuf {
        self.0.join("target")
    }

    /// Build once with `env` on top of the inherited environment; `false` when cargo itself cannot
    /// run here (a boundary with no toolchain), which skips this test loudly.
    fn build(&self, env: &[(&str, &str)]) -> bool {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let mut cmd = Command::new(cargo);
        // `current_dir` is the probe ROOT, so the pinned toolchain beside it is the one used: the
        // layout is cargo's and 1.98 has not got it.
        cmd.current_dir(&self.0)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("RUSTUP_TOOLCHAIN")
            .args([
                "build",
                "--offline",
                "-q",
                "--manifest-path",
                "p/Cargo.toml",
                "--target-dir",
                "target",
            ]);
        for (k, v) in env {
            cmd.env(k, v);
        }
        match cmd.status() {
            Ok(s) => s.success(),
            Err(e) => {
                eprintln!("skipped: cannot run cargo here: {e}");
                false
            }
        }
    }

    /// The artifact dirs cargo wrote, grouped by the unit each holds.
    fn by_unit(&self) -> BTreeMap<String, Vec<PathBuf>> {
        let mut out: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for build in builds(&self.target()) {
            let Ok(packages) = std::fs::read_dir(&build) else {
                continue;
            };
            for package in packages.flatten() {
                let Ok(dirs) = std::fs::read_dir(package.path()) else {
                    continue;
                };
                for d in dirs.flatten() {
                    if let Some(key) = units::unit_key(&d.path()) {
                        out.entry(key).or_default().push(d.path());
                    }
                }
            }
        }
        out
    }

    fn of(&self, key: &str) -> usize {
        self.by_unit().get(key).map(Vec::len).unwrap_or(0)
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn builds(target: &Path) -> Vec<PathBuf> {
    let Ok(profiles) = std::fs::read_dir(target) else {
        return Vec::new();
    };
    profiles
        .flatten()
        .map(|p| p.path().join("build"))
        .filter(|b| b.is_dir())
        .collect()
}

/// Backdate everything under the target past the grace, so the test judges the count and not the
/// clock: a real pass refuses to touch a directory a build may still be writing into.
fn age(target: &Path) {
    let when = SystemTime::now() - Duration::from_secs(48 * 3600);
    let mut stack = vec![target.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            if let Ok(f) = std::fs::File::open(e.path()) {
                let _ = f.set_times(std::fs::FileTimes::new().set_modified(when));
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(e.path());
            }
        }
    }
}

/// Three build configurations, three dirs of one unit; one prune leaves `KEEP` and a second pass
/// has nothing left to do.
#[test]
fn three_configurations_of_one_unit_leave_keep() {
    let probe = Probe::new("variants");
    if !probe.build(&[]) {
        return;
    }
    let one = probe.of(KEY);
    assert_eq!(
        one,
        1,
        "one build, one dir of the lib: {:?}",
        probe.by_unit()
    );
    assert!(
        probe.build(&[("RUSTFLAGS", "-Dwarnings")]),
        "second variant"
    );
    assert_eq!(
        probe.of(KEY),
        2,
        "a second configuration adds a dir: {:?}",
        probe.by_unit()
    );
    assert!(
        probe.build(&[("CARGO_PROFILE_DEV_DEBUG", "0")]),
        "third variant"
    );
    let three = probe.of(KEY);
    assert_eq!(
        three,
        3,
        "and a third one adds one more: {:?}",
        probe.by_unit()
    );

    // Seconds old is what a live build looks like: nothing may be removed yet.
    let young = units::prune(
        &probe.target(),
        units::KEEP,
        units::GRACE,
        SystemTime::now(),
    );
    assert!(
        young.removed.is_empty(),
        "a dir written seconds ago: {:?}",
        young.removed
    );
    assert_eq!(probe.of(KEY), three);

    age(&probe.target());
    let done = units::prune(
        &probe.target(),
        units::KEEP,
        units::GRACE,
        SystemTime::now(),
    );
    assert_eq!(done.removed.len(), three - units::KEEP, "{done:?}");
    assert!(done.bytes > 0, "the removal reports what it freed");
    assert_eq!(probe.of(KEY), units::KEEP);

    let again = units::prune(
        &probe.target(),
        units::KEEP,
        units::GRACE,
        SystemTime::now(),
    );
    assert!(
        again.removed.is_empty(),
        "a second pass has nothing: {again:?}"
    );
    assert_eq!(probe.of(KEY), units::KEEP);
}
