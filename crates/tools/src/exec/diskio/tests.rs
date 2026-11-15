use super::*;
use crate::exec::ExecError;
use crate::exec::host::DEFAULT_CAPTURE_BYTES;
use crate::exec::jobs::{Lifetime, Stdin};
use crate::exec::scope::{Migration, Reaping, ScopeId, ScopeKind};
use std::time::SystemTime;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "letibot-diskio-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn io_stat_is_summed_over_devices_and_a_lower_device_is_skipped() {
    let text = "259:1 rbytes=100 wbytes=1000 rios=1 wios=10 dbytes=0 dios=0\n\
                252:0 rbytes=5 wbytes=24 rios=2 wios=3 dbytes=0 dios=0\n\
                259:0 rbytes=100 wbytes=999999 rios=1 wios=10 dbytes=0 dios=0\n";
    let all = parse_io_stat(text, |_| false);
    assert_eq!(
        all,
        IoStat {
            rbytes: 205,
            wbytes: 1_001_023,
            rios: 4,
            wios: 23
        }
    );
    let top = parse_io_stat(text, |d| d == "259:0");
    assert_eq!(top.wbytes, 1024);
    assert_eq!(parse_io_stat("", |_| false), IoStat::default());
}

#[test]
fn pressure_proc_io_sizes_and_floors_parse() {
    let p = "some avg10=12.50 avg60=1.00 avg300=0.00 total=1\nfull avg10=61.25 avg60=0.5 avg300=0 total=2\n";
    assert_eq!(pressure_full(p), Some(61.25));
    assert_eq!(pressure_full("some avg10=1.0\n"), None);
    let io = parse_proc_io(
        "rchar: 9\nwchar: 9\nsyscr: 3\nsyscw: 7\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 0\n",
    );
    assert_eq!(
        io,
        IoStat {
            rbytes: 4096,
            wbytes: 8192,
            rios: 3,
            wios: 7
        }
    );
    assert_eq!(parse_size("20G").unwrap(), 20 << 30);
    assert_eq!(parse_size("512MiB").unwrap(), 512 << 20);
    assert_eq!(parse_size("1048576").unwrap(), 1 << 20);
    assert!(parse_size("lots").is_err());
    assert_eq!(DiskFloor::parse("5%").unwrap(), DiskFloor::Percent(5.0));
    assert_eq!(DiskFloor::parse("20G").unwrap(), DiskFloor::Bytes(20 << 30));
    assert_eq!(DiskFloor::parse("0").unwrap(), DiskFloor::Off);
    assert!(DiskFloor::parse("150%").is_err());
    assert_eq!(DiskFloor::Percent(5.0).bytes_of(1000), Some(50));
    assert_eq!(FloorAction::parse("kill").unwrap(), FloorAction::Kill);
    assert!(FloorAction::parse("stop").is_err());
}

/// No `io.stat`: the job's processes' `/proc/<pid>/io` are summed, and the clause says sampled.
#[test]
fn without_io_stat_the_proc_fallback_is_used_and_says_so() {
    let root = temp("fallback");
    let cg = root.join("cg");
    let child = cg.join("inner");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(cg.join("cgroup.procs"), "11\n").unwrap();
    std::fs::write(child.join("cgroup.procs"), "12\n").unwrap();
    let proc = root.join("proc");
    for (pid, w) in [(11, 1 << 20), (12, 3 << 20)] {
        std::fs::create_dir_all(proc.join(pid.to_string())).unwrap();
        std::fs::write(
            proc.join(pid.to_string()).join("io"),
            format!("syscw: 5\nwrite_bytes: {w}\n"),
        )
        .unwrap();
    }
    let u = io_use_at(&cg, &proc).expect("sampled");
    assert_eq!(u.source, IoSource::Sampled);
    assert_eq!(u.stat.wbytes, 4 << 20);
    assert_eq!(u.stat.wios, 10);
    assert!(u.clause(None).contains("sampled"), "{}", u.clause(None));

    // A process that exits does not take its bytes off the total.
    let fewer = Some(IoUse {
        stat: IoStat {
            wbytes: 1 << 20,
            ..Default::default()
        },
        source: IoSource::Sampled,
    });
    assert_eq!(monotonic(Some(u), fewer).unwrap().stat.wbytes, 4 << 20);

    std::fs::write(cg.join("io.stat"), "8:0 rbytes=0 wbytes=7 rios=0 wios=1\n").unwrap();
    assert_eq!(io_use_at(&cg, &proc).unwrap().source, IoSource::Cgroup);
    let _ = std::fs::remove_dir_all(&root);
}

/// A tree whose scopes are plain directories, with io on.
struct Tree {
    ended: Mutex<Vec<String>>,
}

impl ScopeTree for Tree {
    fn describe(&self) -> String {
        "a test's directories".into()
    }
    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        _p: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError> {
        Err(ExecError::NoScopes(format!(
            "{kind:?} {name}: not opened here"
        )))
    }
    fn members(&self, _s: &ScopeId) -> Result<Vec<u32>, ExecError> {
        Ok(Vec::new())
    }
    fn end(&self, scope: &ScopeId) -> Reaping {
        self.ended.lock().unwrap().push(scope.name.clone());
        Reaping {
            scope: scope.clone(),
            at: SystemTime::now(),
            mechanism: "a test's directories",
            observed: Vec::new(),
            survivors: Vec::new(),
            waited: Duration::ZERO,
            removed: false,
            note: None,
        }
    }
    fn migrate(&self, _f: &ScopeId, _t: &ScopeId) -> Result<Migration, ExecError> {
        Err(ExecError::NoScopes("no".into()))
    }
    fn list(&self) -> Vec<ScopeId> {
        Vec::new()
    }
    fn io_controls(&self) -> Result<(), String> {
        Ok(())
    }
}

struct FakeDisks {
    free: Mutex<u64>,
    pressure: Mutex<Option<f64>>,
}

impl Disks for FakeDisks {
    fn space(&self, _path: &Path) -> Option<Space> {
        Some(Space {
            dev: 1,
            mount: PathBuf::from("/build"),
            free: *self.free.lock().unwrap(),
            total: 100 << 30,
        })
    }
    fn devices(&self, _path: &Path) -> Vec<String> {
        vec!["259:1".into()]
    }
    fn pressure_full(&self) -> Option<f64> {
        *self.pressure.lock().unwrap()
    }
}

fn job_in(root: &Path, name: &str) -> Arc<Job> {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("cgroup.procs"), "").unwrap();
    let scope = ScopeId {
        kind: ScopeKind::Session,
        name: name.into(),
        path,
    };
    Arc::new(Job::new(
        JobId::next(),
        "dd if=/dev/zero of=big".into(),
        None,
        Lifetime {
            scope: scope.clone(),
            owner: scope,
            background: None,
        },
        root.display().to_string(),
        0,
        DEFAULT_CAPTURE_BYTES,
        Stdin::none(),
    ))
}

fn wrote(job: &Job, bytes: u64, ios: u64) {
    std::fs::write(
        job.lifetime().scope.path.join("io.stat"),
        format!("259:1 rbytes=0 wbytes={bytes} rios=0 wios={ios} dbytes=0 dios=0\n"),
    )
    .unwrap();
}

fn guard(root: &Path, free: u64, pressure: Option<f64>) -> (Arc<IoGuard>, Arc<Tree>) {
    let tree = Arc::new(Tree {
        ended: Mutex::new(Vec::new()),
    });
    let g = IoGuard::new(
        tree.clone(),
        Box::new(FakeDisks {
            free: Mutex::new(free),
            pressure: Mutex::new(pressure),
        }),
        root.join("proc"),
        false,
    );
    g.set_policy(IoPolicy {
        watch: Vec::new(),
        ..IoPolicy::default()
    });
    (g, tree)
}

#[test]
fn a_configured_cap_is_written_to_the_jobs_io_max() {
    let root = temp("iomax");
    let (g, tree) = guard(&root, 50 << 30, None);
    let job = job_in(&root, "capped");
    assert_eq!(
        g.cap_new(tree.as_ref(), &job.lifetime().scope.path, &root),
        None
    );
    g.set_policy(IoPolicy {
        wbps: Some(100 << 20),
        wiops: None,
        watch: Vec::new(),
        ..IoPolicy::default()
    });
    let said = g
        .cap_new(tree.as_ref(), &job.lifetime().scope.path, &root)
        .unwrap();
    assert!(said.contains("100 MiB/s write"), "{said}");
    let max = std::fs::read_to_string(job.lifetime().scope.path.join("io.max")).unwrap();
    assert_eq!(max, format!("259:1 wbps={} wiops=max", 100u64 << 20));
    let _ = std::fs::remove_dir_all(&root);
}

/// Under the floor, the job that wrote most in the window is frozen and a row names it, the
/// bytes and the filesystem. A job that wrote little is left alone.
#[test]
fn a_filesystem_under_the_floor_freezes_the_top_writer_with_a_row() {
    let root = temp("floor");
    let (g, tree) = guard(&root, 1 << 30, None);
    let hog = job_in(&root, "hog");
    let quiet = job_in(&root, "quiet");
    g.watch(hog.clone());
    g.watch(quiet.clone());
    wrote(&hog, 0, 0);
    wrote(&quiet, 0, 0);
    let t0 = Instant::now();
    g.scan(tree.as_ref(), t0);
    wrote(&hog, 40 << 30, 1000);
    wrote(&quiet, 2 << 20, 10);
    g.scan(tree.as_ref(), t0 + IO_TICK);

    let freeze_of =
        |j: &Job| std::fs::read_to_string(j.lifetime().scope.path.join("cgroup.freeze")).ok();
    assert_eq!(freeze_of(&hog).as_deref(), Some("1"));
    assert_eq!(freeze_of(&quiet), None);
    let mark = hog.io_marks().frozen.expect("the row says frozen");
    assert!(mark.contains("/build"), "{mark}");
    assert!(mark.contains("40 GiB"), "{mark}");
    let note = g.take_note(&hog.id).expect("a row for the frozen job");
    assert!(
        note.contains(&hog.id.0) && note.contains("job_resume"),
        "{note}"
    );
    assert!(g.take_note(&hog.id).is_none(), "said once");
    assert!(g.take_note(&quiet.id).is_none());
    assert!(hog.state().is_running(), "frozen is not ended");

    // Frozen once: the next tick does not freeze it again or blame it for old bytes.
    g.scan(tree.as_ref(), t0 + IO_TICK * 2);
    assert!(g.take_note(&hog.id).is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_kill_policy_ends_the_top_writer() {
    let root = temp("floorkill");
    let (g, tree) = guard(&root, 1 << 30, None);
    g.set_policy(IoPolicy {
        action: FloorAction::Kill,
        watch: Vec::new(),
        ..IoPolicy::default()
    });
    let hog = job_in(&root, "hog");
    g.watch(hog.clone());
    wrote(&hog, 0, 0);
    let t0 = Instant::now();
    g.scan(tree.as_ref(), t0);
    wrote(&hog, 10 << 30, 10);
    g.scan(tree.as_ref(), t0 + IO_TICK);
    assert!(matches!(hog.state(), JobState::Killed { by } if by == DISK_FLOOR_KILL));
    assert_eq!(tree.ended.lock().unwrap().as_slice(), ["hog"]);
    assert!(
        g.take_note(&hog.id)
            .unwrap()
            .contains("KILLED by the disk floor")
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Sustained io pressure throttles the top writer through `io.max`, once, with a row.
#[test]
fn sustained_io_pressure_throttles_the_top_writer() {
    let root = temp("pressure");
    let (g, tree) = guard(&root, 50 << 30, Some(90.0));
    let hog = job_in(&root, "hog");
    g.watch(hog.clone());
    let t0 = Instant::now();
    for i in 0..PRESSURE_TICKS {
        wrote(&hog, (i as u64) * (500 << 20), (i as u64) * 5000);
        g.scan(tree.as_ref(), t0 + IO_TICK * i);
    }
    let max = std::fs::read_to_string(hog.lifetime().scope.path.join("io.max")).unwrap();
    assert!(max.starts_with("259:1 wbps="), "{max}");
    let note = g.take_note(&hog.id).unwrap();
    assert!(note.contains("THROTTLED"), "{note}");
    assert!(hog.io_marks().throttled.is_some());
    let _ = std::fs::remove_dir_all(&root);
}

/// The guard records the job's total as it samples: the fallback when the cgroup has no io.
#[test]
fn the_guard_samples_proc_when_the_cgroup_has_no_io() {
    let root = temp("guardfallback");
    let (g, tree) = guard(&root, 50 << 30, None);
    let job = job_in(&root, "plain");
    std::fs::write(job.lifetime().scope.path.join("cgroup.procs"), "77\n").unwrap();
    std::fs::create_dir_all(root.join("proc/77")).unwrap();
    std::fs::write(root.join("proc/77/io"), "syscw: 3\nwrite_bytes: 65536\n").unwrap();
    g.watch(job.clone());
    g.scan(tree.as_ref(), Instant::now());
    let u = job.io_used().expect("sampled");
    assert_eq!(u.source, IoSource::Sampled);
    assert_eq!(u.stat.wbytes, 65536);
    let _ = std::fs::remove_dir_all(&root);
}
