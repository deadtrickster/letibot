//! The `<build>/target-*` dirs every gate and package leaves behind: their size, last use and live
//! users. `letibot doctor` warns from this scan and `letibot gc --targets` removes from it, so the
//! two never disagree about which dir is in use.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use letibot_tools::lifecycle::units;

/// One target dir as the scan found it.
#[derive(Debug, Clone)]
pub struct TargetDir {
    pub path: PathBuf,
    pub bytes: u64,
    /// False when the size walk hit its deadline, so `bytes` is a lower bound.
    pub whole: bool,
    /// The newest mtime under it: the last build that wrote anything.
    pub last_use: Option<SystemTime>,
    /// Processes working in it, as `pid name`.
    pub users: Vec<String>,
}

/// Every `target-*` dir directly under `build_dir`, sorted by path, sized in parallel until `bound`.
pub fn scan(build_dir: &Path, proc_root: &Path, bound: Duration) -> Vec<TargetDir> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(build_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("target-"))
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    let deadline = Instant::now() + bound;
    let walks: Vec<Walk> = std::thread::scope(|s| {
        let hs: Vec<_> = dirs
            .iter()
            .map(|d| s.spawn(move || walk(d, deadline)))
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    dirs.into_iter()
        .zip(walks)
        .map(|(path, w)| TargetDir {
            users: users(proc_root, &path),
            path,
            bytes: w.bytes,
            whole: w.whole,
            last_use: w.newest,
        })
        .collect()
}

#[derive(Default)]
struct Walk {
    bytes: u64,
    whole: bool,
    newest: Option<SystemTime>,
}

fn walk(dir: &Path, deadline: Instant) -> Walk {
    let mut out = Walk {
        newest: std::fs::metadata(dir).and_then(|m| m.modified()).ok(),
        ..Walk::default()
    };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if Instant::now() > deadline {
            return out;
        }
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            out.bytes += m.blocks() * 512;
            if let Ok(t) = m.modified() {
                out.newest = out.newest.max(Some(t));
            }
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out.whole = true;
    out
}

/// Processes whose cwd, an open file or a command-line argument is under `dir`.
pub fn users(proc_root: &Path, dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let needle = dir.to_string_lossy().into_owned();
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let d = e.path();
        let argv = std::fs::read(d.join("cmdline")).unwrap_or_default();
        let argv: Vec<String> = argv
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        let hit = std::fs::read_link(d.join("cwd")).is_ok_and(|c| c.starts_with(dir))
            || argv.iter().any(|a| mentions(a, &needle))
            || std::fs::read_dir(d.join("fd")).is_ok_and(|fds| {
                fds.flatten()
                    .any(|f| std::fs::read_link(f.path()).is_ok_and(|l| l.starts_with(dir)))
            });
        if hit {
            let name = std::fs::read_to_string(d.join("comm"))
                .ok()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .or_else(|| {
                    argv.first()
                        .map(|a| a.rsplit('/').next().unwrap_or(a).to_string())
                })
                .unwrap_or_else(|| "?".into());
            out.push(format!("{pid} {name}"));
        }
    }
    out.sort();
    out
}

/// `arg` names `path` or something under it, so `target-a` is not matched by `target-ab`.
fn mentions(arg: &str, path: &str) -> bool {
    arg.match_indices(path).any(|(i, _)| {
        matches!(
            arg[i + path.len()..].chars().next(),
            None | Some('/') | Some(' ') | Some(':') | Some(',')
        )
    })
}

/// Which dirs go: every unused one with no cap, else unused ones oldest first until the total of
/// all dirs is at or under `cap`. A dir in use is never picked.
pub fn stale(dirs: &[TargetDir], cap: Option<u64>) -> Vec<bool> {
    let mut out = vec![false; dirs.len()];
    let mut idle: Vec<usize> = (0..dirs.len())
        .filter(|&i| dirs[i].users.is_empty())
        .collect();
    let Some(cap) = cap else {
        for i in idle {
            out[i] = true;
        }
        return out;
    };
    idle.sort_by_key(|&i| dirs[i].last_use);
    let mut total: u64 = dirs.iter().map(|d| d.bytes).sum();
    for i in idle {
        if total <= cap {
            break;
        }
        out[i] = true;
        total = total.saturating_sub(dirs[i].bytes);
    }
    out
}

fn age(t: Option<SystemTime>, now: SystemTime) -> String {
    let Some(s) = t
        .and_then(|t| now.duration_since(t).ok())
        .map(|d| d.as_secs())
    else {
        return "never".into();
    };
    match s {
        0..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

/// `letibot gc --targets [--now]`: the plan, or with `act` the removal, as the text to print.
/// Each dir is checked for users again just before it goes, since a build may have started.
pub fn gc(build_dir: &Path, proc_root: &Path, cap: Option<u64>, act: bool, json: bool) -> String {
    let human = crate::gc::bytes;
    let dirs = scan(build_dir, proc_root, Duration::from_secs(60));
    let picks = stale(&dirs, cap);
    let now = SystemTime::now();
    let total: u64 = dirs.iter().map(|d| d.bytes).sum();
    let mut rows = Vec::new();
    let mut text = String::new();
    let mut freed = 0u64;
    for (d, &pick) in dirs.iter().zip(&picks) {
        let mut state = if !d.users.is_empty() {
            format!("in use: {}", d.users.join(", "))
        } else if pick {
            "stale".to_string()
        } else {
            "kept under the cap".to_string()
        };
        if act && pick {
            let late = users(proc_root, &d.path);
            state = if !late.is_empty() {
                format!("kept, in use now: {}", late.join(", "))
            } else {
                match std::fs::remove_dir_all(&d.path) {
                    Ok(()) => {
                        freed += d.bytes;
                        "removed".to_string()
                    }
                    Err(e) => format!("not removed: {e}"),
                }
            };
        }
        let size = if d.whole {
            human(d.bytes)
        } else {
            format!("at least {}", human(d.bytes))
        };
        text += &format!(
            "{}  {size}  last use {}  {state}\n",
            d.path.display(),
            age(d.last_use, now)
        );
        rows.push(serde_json::json!({
            "path": d.path,
            "bytes": d.bytes,
            "whole": d.whole,
            "last_use": d.last_use
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
            "users": d.users,
            "stale": pick,
            "state": state,
        }));
    }
    // A dir that stays is the one that grows: cargo keeps a whole copy of each unit's output per
    // build variant under `<target>/<profile>/build/<package>/<hash>` and removes none of them.
    let mut units_freed = 0u64;
    let mut units = Vec::new();
    if act {
        for d in &dirs {
            // A target that stays is the one that grows. The sweep does not refuse a dir a build
            // is in: it takes cargo's own lock on the profile first, so a running build and a
            // removal cannot overlap.
            if !d.path.is_dir() {
                continue;
            }
            let done = units::prune(&d.path, units::KEEP, units::GRACE, SystemTime::now());
            units_freed += done.bytes;
            if let Some(line) = done.line() {
                units.push(format!("{}: {line}", d.path.display()));
            }
        }
    }
    if json {
        return serde_json::json!({
            "build_dir": build_dir,
            "total": total,
            "cap": cap,
            "freed": freed,
            "units_freed": units_freed,
            "units": units,
            "dirs": rows,
        })
        .to_string()
            + "\n";
    }
    if dirs.is_empty() {
        return format!("no {}/target-* dirs\n", build_dir.display());
    }
    let cap_line = match cap {
        Some(c) => format!("cap {} ([gc] targets_cap)", human(c)),
        None => "no [gc] targets_cap, so every unused dir is stale".into(),
    };
    text += &format!("total {}, {cap_line}\n", human(total));
    if act {
        for line in &units {
            text += &format!("  {line}\n");
        }
        text += &format!(
            "freed {}, of it {} in old unit dirs\n",
            human(freed),
            human(units_freed)
        );
    } else if picks.iter().any(|p| *p) {
        // Cheap to say: the shared sccache holds the dependencies a removed dir had compiled.
        text += "this is a plan: `letibot gc --targets --now` removes the stale ones; sccache makes \
                 a rebuild cheap\n";
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fx(PathBuf);

    impl Fx {
        fn new(name: &str) -> Fx {
            let d =
                std::env::temp_dir().join(format!("letibot-targets-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(d.join("proc")).unwrap();
            Fx(d)
        }
        /// A target dir of `kb` KiB whose every entry was last written `days` ago.
        fn target(&self, name: &str, kb: usize, days: u64) -> PathBuf {
            let t = self.0.join("build").join(name);
            std::fs::create_dir_all(t.join("debug/deps")).unwrap();
            std::fs::write(t.join("debug/deps/lib.rlib"), vec![1u8; kb * 1024]).unwrap();
            let at = SystemTime::now() - Duration::from_secs(days * 86400);
            for p in [
                t.join("debug/deps/lib.rlib"),
                t.join("debug/deps"),
                t.join("debug"),
                t.clone(),
            ] {
                std::fs::File::open(&p).unwrap().set_modified(at).unwrap();
            }
            t
        }
        fn proc(&self, pid: u32, argv: &[&str], cwd: &Path, open: Option<&Path>) {
            let d = self.0.join(format!("proc/{pid}"));
            std::fs::create_dir_all(d.join("fd")).unwrap();
            symlink(cwd, d.join("cwd")).unwrap();
            std::fs::write(d.join("cmdline"), argv.join("\0") + "\0").unwrap();
            if let Some(f) = open {
                symlink(f, d.join("fd/3")).unwrap();
            }
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Three dirs: one a cargo works in, one a rustc holds a file open in, one nobody uses.
    fn fixture(fx: &Fx) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let cwd_user = fx.target("target-cwd", 8, 1);
        let fd_user = fx.target("target-fd", 8, 2);
        let old = fx.target("target-old", 64, 9);
        let newer = fx.target("target-newer", 64, 3);
        fx.proc(
            10,
            &["/usr/bin/cargo", "build"],
            &cwd_user.join("debug"),
            None,
        );
        fx.proc(
            11,
            &["rustc", "--crate-name", "x"],
            &fx.0,
            Some(&fd_user.join("debug/deps/lib.rlib")),
        );
        // Mentions a sibling whose name only starts the same: not a user of `target-old`.
        fx.proc(12, &["ls", &format!("{}x", old.display())], &fx.0, None);
        (cwd_user, fd_user, old, newer)
    }

    #[test]
    fn the_dry_run_lists_the_unused_dirs_as_stale_and_removes_nothing() {
        let fx = Fx::new("dry");
        let (cwd_user, fd_user, old, newer) = fixture(&fx);
        let dirs = scan(
            &fx.0.join("build"),
            &fx.0.join("proc"),
            Duration::from_secs(5),
        );
        let by = |p: &Path| dirs.iter().find(|d| d.path == p).unwrap();
        assert_eq!(by(&cwd_user).users, vec!["10 cargo".to_string()]);
        assert_eq!(by(&fd_user).users, vec!["11 rustc".to_string()]);
        assert!(by(&old).users.is_empty(), "{:?}", by(&old).users);
        assert!(by(&old).bytes >= 64 * 1024 && by(&old).whole);
        assert!(by(&old).last_use < by(&newer).last_use);

        let out = gc(&fx.0.join("build"), &fx.0.join("proc"), None, false, false);
        let line = |p: &Path| {
            out.lines()
                .find(|l| l.starts_with(&format!("{}  ", p.display())))
                .unwrap_or_else(|| panic!("{out}"))
                .to_string()
        };
        assert!(line(&cwd_user).ends_with("in use: 10 cargo"), "{out}");
        assert!(line(&fd_user).ends_with("in use: 11 rustc"), "{out}");
        assert!(
            line(&old).ends_with("stale") && line(&old).contains("9d ago"),
            "{out}"
        );
        assert!(line(&newer).ends_with("stale"), "{out}");
        assert!(out.contains("this is a plan"), "{out}");
        for p in [&cwd_user, &fd_user, &old, &newer] {
            assert!(p.exists(), "a dry run removed {}", p.display());
        }
    }

    #[test]
    fn now_removes_only_the_stale_and_never_one_in_use() {
        let fx = Fx::new("now");
        let (cwd_user, fd_user, old, newer) = fixture(&fx);
        let out = gc(&fx.0.join("build"), &fx.0.join("proc"), None, true, false);
        assert!(cwd_user.exists() && fd_user.exists(), "{out}");
        assert!(!old.exists() && !newer.exists(), "{out}");
        assert!(out.contains("freed"), "{out}");
    }

    #[test]
    fn a_target_dir_that_stays_is_swept_inside() {
        let fx = Fx::new("units");
        let t = fx.target("target-units", 8, 2);
        // Three build configurations of one test target, as cargo leaves them: one dir each, and
        // the configuration that stopped being built is still there.
        let mut units = Vec::new();
        for i in 0..3u64 {
            let u = t
                .join("debug/build/letibot-harnessd")
                .join(format!("{:016x}", i + 1));
            let fp = u.join("fingerprint");
            std::fs::create_dir_all(&fp).unwrap();
            std::fs::create_dir_all(u.join("out")).unwrap();
            std::fs::write(fp.join("test-integration-test-sessions.json"), b"{}").unwrap();
            std::fs::write(u.join("out/sessions"), vec![1u8; 4096]).unwrap();
            let at = SystemTime::now() - Duration::from_secs(86400 * (3 - i));
            // The `fingerprint/` files too: cargo rewrites them with the unit, and the newest of
            // them is what the pass judges.
            for p in [
                u.join("out/sessions"),
                u.join("out"),
                fp.join("test-integration-test-sessions.json"),
                fp.clone(),
                u.clone(),
            ] {
                std::fs::File::open(&p).unwrap().set_modified(at).unwrap();
            }
            units.push(u);
        }
        // A cap nothing exceeds, so the target dir itself stays and the sweep inside it is what
        // this test is measuring.
        let out = gc(
            &fx.0.join("build"),
            &fx.0.join("proc"),
            Some(u64::MAX),
            true,
            false,
        );
        assert!(t.exists(), "the target dir stays: {out}");
        assert!(out.contains("unit dirs: removed 1"), "{out}");
        assert!(
            out.contains("of it") && out.contains("in old unit dirs"),
            "{out}"
        );
        assert!(!units[0].exists(), "the oldest configuration goes: {out}");
        assert!(units[1].exists() && units[2].exists(), "{out}");
    }

    #[test]
    fn a_cap_removes_the_oldest_unused_first_until_the_total_fits() {
        let fx = Fx::new("cap");
        let (cwd_user, fd_user, old, newer) = fixture(&fx);
        let dirs = scan(
            &fx.0.join("build"),
            &fx.0.join("proc"),
            Duration::from_secs(5),
        );
        let total: u64 = dirs.iter().map(|d| d.bytes).sum();
        let old_bytes = dirs.iter().find(|d| d.path == old).unwrap().bytes;
        // Room for everything but the oldest unused dir.
        let cap = total - old_bytes;
        let out = gc(
            &fx.0.join("build"),
            &fx.0.join("proc"),
            Some(cap),
            true,
            true,
        );
        assert!(!old.exists(), "{out}");
        assert!(
            newer.exists() && cwd_user.exists() && fd_user.exists(),
            "{out}"
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["freed"], old_bytes);

        // A cap below the dirs in use still never touches them.
        gc(
            &fx.0.join("build"),
            &fx.0.join("proc"),
            Some(0),
            true,
            false,
        );
        assert!(!newer.exists() && cwd_user.exists() && fd_user.exists());
    }

    #[test]
    fn a_path_is_mentioned_only_whole() {
        assert!(mentions(
            "-Ldependency=/b/target-a/debug/deps",
            "/b/target-a"
        ));
        assert!(mentions("/b/target-a", "/b/target-a"));
        assert!(!mentions("/b/target-ab/debug", "/b/target-a"));
    }
}
