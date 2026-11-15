//! Per-job disk IO: what a job's cgroup wrote, the `io.max` throttle on it, and the disk floor.
//!
//! The kernel's `io.stat` is the primary source. Without a delegated io controller the job's
//! processes' `/proc/<pid>/io` is summed instead, and every reading says it was sampled.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::jobs::{Job, JobId, JobState};
use super::scope::{ScopeTree, human_bytes};

/// The one-time root setup that delegates the io controller to the user manager.
pub const IO_SETUP: &str = "sudo mkdir -p /etc/systemd/system/user@.service.d && printf \
     '[Service]\\nDelegate=cpu memory pids io\\n' | sudo tee \
     /etc/systemd/system/user@.service.d/delegate-io.conf && sudo systemctl daemon-reload";

/// Who ends a job at the disk floor, as `JobState::Killed` says it.
pub const DISK_FLOOR_KILL: &str = "the disk floor";

/// Bytes and operations, summed over devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IoStat {
    pub rbytes: u64,
    pub wbytes: u64,
    pub rios: u64,
    pub wios: u64,
}

/// Where a reading came from. `Sampled` misses processes that left the cgroup between samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoSource {
    Cgroup,
    Sampled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoUse {
    pub stat: IoStat,
    pub source: IoSource,
}

/// Write rate between two readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoRate {
    pub wbps: u64,
    pub wiops: u64,
}

impl IoRate {
    pub fn between(was: &IoStat, is: &IoStat, over: Duration) -> Option<IoRate> {
        let s = over.as_secs_f64();
        if s <= 0.0 {
            return None;
        }
        Some(IoRate {
            wbps: (is.wbytes.saturating_sub(was.wbytes) as f64 / s) as u64,
            wiops: (is.wios.saturating_sub(was.wios) as f64 / s) as u64,
        })
    }
}

impl IoUse {
    /// `64 MiB written, 1.2k writes, 3 MiB read; now 12 MiB/s, 300 writes/s`.
    pub fn clause(&self, rate: Option<IoRate>) -> String {
        let s = &self.stat;
        let mut out = format!("{} written, {} writes", human_bytes(s.wbytes), s.wios);
        if s.rbytes > 0 {
            out.push_str(&format!(", {} read", human_bytes(s.rbytes)));
        }
        if let Some(r) = rate {
            out.push_str(&format!(
                "; now {}/s, {} writes/s",
                human_bytes(r.wbps),
                r.wiops
            ));
        }
        if self.source == IoSource::Sampled {
            out.push_str(
                " (sampled from /proc: io is not delegated, so children that exited can be missed)",
            );
        }
        out
    }
}

/// Sum `io.stat` over its devices. `skip` names a device to leave out: one under another
/// device (a disk under dm-crypt) carries the same bytes again.
pub fn parse_io_stat(text: &str, skip: impl Fn(&str) -> bool) -> IoStat {
    let mut sum = IoStat::default();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let Some(dev) = words.next() else { continue };
        if skip(dev) {
            continue;
        }
        for kv in words {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            let v: u64 = v.parse().unwrap_or(0);
            match k {
                "rbytes" => sum.rbytes += v,
                "wbytes" => sum.wbytes += v,
                "rios" => sum.rios += v,
                "wios" => sum.wios += v,
                _ => {}
            }
        }
    }
    sum
}

/// `full avg10=` of an `io.pressure` or `/proc/pressure/io`, in percent.
pub fn pressure_full(text: &str) -> Option<f64> {
    text.lines()
        .find_map(|l| l.strip_prefix("full "))?
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("avg10="))?
        .parse()
        .ok()
}

/// One `/proc/<pid>/io`: write_bytes, syscw, read_bytes, syscr.
pub fn parse_proc_io(text: &str) -> IoStat {
    let mut s = IoStat::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v: u64 = v.trim().parse().unwrap_or(0);
        match k.trim() {
            "write_bytes" => s.wbytes = v,
            "syscw" => s.wios = v,
            "read_bytes" => s.rbytes = v,
            "syscr" => s.rios = v,
            _ => {}
        }
    }
    s
}

/// A device under another one, by sysfs `holders`: the disk or a partition of it has one.
fn is_lower(dev: &str) -> bool {
    let at = Path::new("/sys/dev/block").join(dev);
    let held =
        |p: &Path| std::fs::read_dir(p.join("holders")).is_ok_and(|mut d| d.next().is_some());
    if held(&at) {
        return true;
    }
    std::fs::read_dir(&at).is_ok_and(|rd| {
        rd.flatten()
            .any(|e| e.path().join("partition").exists() && held(&e.path()))
    })
}

/// What a cgroup directory has done to disks, or `None` when nothing can say.
pub fn io_use_at(dir: &Path, proc_root: &Path) -> Option<IoUse> {
    if let Ok(text) = std::fs::read_to_string(dir.join("io.stat")) {
        return Some(IoUse {
            stat: parse_io_stat(&text, is_lower),
            source: IoSource::Cgroup,
        });
    }
    sampled_at(dir, proc_root)
}

/// The fallback: `/proc/<pid>/io` summed over the cgroup's processes.
pub fn sampled_at(dir: &Path, proc_root: &Path) -> Option<IoUse> {
    let mut pids = Vec::new();
    super::scope::members_at(dir, &mut pids);
    pids.sort_unstable();
    pids.dedup();
    let mut sum = IoStat::default();
    let mut any = false;
    for pid in pids {
        if let Ok(t) = std::fs::read_to_string(proc_root.join(pid.to_string()).join("io")) {
            let s = parse_proc_io(&t);
            sum.rbytes += s.rbytes;
            sum.wbytes += s.wbytes;
            sum.rios += s.rios;
            sum.wios += s.wios;
            any = true;
        }
    }
    any.then_some(IoUse {
        stat: sum,
        source: IoSource::Sampled,
    })
}

/// A sampled sum can drop when a process exits; the total a job has written cannot.
pub fn monotonic(prev: Option<IoUse>, now: Option<IoUse>) -> Option<IoUse> {
    match (prev, now) {
        (Some(p), Some(n)) if n.source == IoSource::Sampled => Some(IoUse {
            stat: IoStat {
                rbytes: n.stat.rbytes.max(p.stat.rbytes),
                wbytes: n.stat.wbytes.max(p.stat.wbytes),
                rios: n.stat.rios.max(p.stat.rios),
                wios: n.stat.wios.max(p.stat.wios),
            },
            source: n.source,
        }),
        (p, None) => p,
        (_, n) => n,
    }
}

/// Write `io.max` for each device. `Ok` when at least one device took it.
pub fn write_io_max(
    dir: &Path,
    devices: &[String],
    wbps: Option<u64>,
    wiops: Option<u64>,
) -> Result<(), String> {
    if devices.is_empty() {
        return Err("no block device backs this job's filesystems".into());
    }
    let limit = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_else(|| "max".into());
    let mut errors = Vec::new();
    let mut took = 0;
    for dev in devices {
        let line = format!("{dev} wbps={} wiops={}", limit(wbps), limit(wiops));
        match std::fs::write(dir.join("io.max"), &line) {
            Ok(()) => took += 1,
            Err(e) => errors.push(format!("{dev}: {e}")),
        }
    }
    if took == 0 {
        return Err(format!(
            "writing `{}` failed: {}",
            dir.join("io.max").display(),
            errors.join("; ")
        ));
    }
    Ok(())
}

/// `cgroup.freeze`: 1 stops every process in the subtree, 0 lets it go on.
pub fn freeze(dir: &Path, frozen: bool) -> Result<(), String> {
    let at = dir.join("cgroup.freeze");
    std::fs::write(&at, if frozen { "1" } else { "0" })
        .map_err(|e| format!("writing `{}` failed: {e}", at.display()))
}

/// Whether the io controller is enabled for the daemon's tool trees, or the sentence that says
/// why not and what the operator runs once.
pub fn io_delegation() -> Result<(), String> {
    let own = match super::scope::delegate_memory() {
        Ok(own) => own.clone(),
        Err(_) => super::scope::own_cgroup().map_err(|e| e.to_string())?,
    };
    let has = |p: &Path, file: &str| {
        std::fs::read_to_string(p.join(file))
            .unwrap_or_default()
            .split_whitespace()
            .any(|c| c == "io")
    };
    if has(&own, "cgroup.controllers") {
        return Ok(());
    }
    let uid = unsafe { libc::getuid() };
    let manager = PathBuf::from(format!(
        "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service"
    ));
    if has(&manager, "cgroup.controllers") {
        return Err(format!(
            "io is delegated to user@{uid}.service, but its manager started before that and does \
             not pass io down; it takes effect at the next login or `systemctl restart \
             user@{uid}`, then a daemon restart. Disk IO is sampled from /proc until then"
        ));
    }
    Err(format!(
        "the io controller is not delegated to `{}`, so disk IO is sampled from /proc and \
         `io.max` cannot throttle. One-time setup: {IO_SETUP}",
        own.display()
    ))
}

// ---------------------------------------------------------------- policy

/// `[exec] disk_floor`: free space below which the top writers are stopped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiskFloor {
    Off,
    Percent(f64),
    Bytes(u64),
}

impl DiskFloor {
    pub const DEFAULT: DiskFloor = DiskFloor::Percent(5.0);

    /// `5%`, `20G`, `512M`, `0` (off).
    pub fn parse(s: &str) -> Result<DiskFloor, String> {
        let t = s.trim();
        if let Some(p) = t.strip_suffix('%') {
            let v: f64 = p
                .trim()
                .parse()
                .map_err(|_| format!("`{s}` is not a percentage"))?;
            if !(0.0..100.0).contains(&v) {
                return Err(format!("`{s}` must be from 0% to below 100%"));
            }
            return Ok(if v == 0.0 {
                DiskFloor::Off
            } else {
                DiskFloor::Percent(v)
            });
        }
        let b = parse_size(t)?;
        Ok(if b == 0 {
            DiskFloor::Off
        } else {
            DiskFloor::Bytes(b)
        })
    }

    pub fn bytes_of(&self, total: u64) -> Option<u64> {
        match self {
            DiskFloor::Off => None,
            DiskFloor::Percent(p) => Some((total as f64 * p / 100.0) as u64),
            DiskFloor::Bytes(b) => Some(*b),
        }
    }

    pub fn label(&self) -> String {
        match self {
            DiskFloor::Off => "off".into(),
            DiskFloor::Percent(p) => format!("{p}%"),
            DiskFloor::Bytes(b) => human_bytes(*b),
        }
    }
}

/// `20G`, `512M`, `64k`, `1048576`. Binary units.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let t = t
        .strip_suffix("iB")
        .or_else(|| t.strip_suffix('B'))
        .unwrap_or(t);
    let (num, mul) = match t.chars().last() {
        Some('k' | 'K') => (&t[..t.len() - 1], 1u64 << 10),
        Some('m' | 'M') => (&t[..t.len() - 1], 1 << 20),
        Some('g' | 'G') => (&t[..t.len() - 1], 1 << 30),
        Some('t' | 'T') => (&t[..t.len() - 1], 1 << 40),
        _ => (t, 1),
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("`{s}` is not a size (`20G`, `512M` or bytes)"))?;
    if v < 0.0 {
        return Err(format!("`{s}` is negative"));
    }
    Ok((v * mul as f64) as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorAction {
    Freeze,
    Kill,
}

impl FloorAction {
    pub fn parse(s: &str) -> Result<FloorAction, String> {
        match s.trim() {
            "freeze" => Ok(FloorAction::Freeze),
            "kill" => Ok(FloorAction::Kill),
            other => Err(format!("`{other}` is not `freeze` or `kill`")),
        }
    }
}

/// The `[exec]` io keys, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct IoPolicy {
    /// `io.max` written on every job at its spawn. `None` is no cap.
    pub wbps: Option<u64>,
    pub wiops: Option<u64>,
    pub floor: DiskFloor,
    pub action: FloorAction,
    /// System `io` pressure "full" avg10, percent, past which the top writer is throttled.
    pub pressure_full: Option<f64>,
    /// Filesystems watched besides each job's cwd: target dirs, /build.
    pub watch: Vec<PathBuf>,
}

impl Default for IoPolicy {
    fn default() -> Self {
        IoPolicy {
            wbps: None,
            wiops: None,
            floor: DiskFloor::DEFAULT,
            action: FloorAction::Freeze,
            pressure_full: Some(DEFAULT_PRESSURE_FULL),
            watch: default_watch(),
        }
    }
}

pub const DEFAULT_PRESSURE_FULL: f64 = 60.0;

/// `/build` and `$CARGO_TARGET_DIR`, those that exist.
pub fn default_watch() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/build")];
    if let Some(t) = std::env::var_os("CARGO_TARGET_DIR") {
        v.push(PathBuf::from(t));
    }
    v.retain(|p| p.exists());
    v
}

// ---------------------------------------------------------------- disks

/// One filesystem's free space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Space {
    pub dev: u64,
    pub mount: PathBuf,
    pub free: u64,
    pub total: u64,
}

/// What the guard asks of the host's disks. A trait so a test can put a filesystem under its floor.
pub trait Disks: Send + Sync {
    fn space(&self, path: &Path) -> Option<Space>;
    /// `MAJ:MIN` of the block devices backing the filesystem of `path`.
    fn devices(&self, path: &Path) -> Vec<String>;
    /// System-wide `full avg10` of `/proc/pressure/io`.
    fn pressure_full(&self) -> Option<f64>;
}

pub struct HostDisks;

fn split_dev(dev: u64) -> (u64, u64) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major, minor)
}

/// The whole disk of a block device: a partition's parent directory in sysfs.
fn whole_disk(dev: &str) -> Option<String> {
    let at = std::fs::canonicalize(Path::new("/sys/dev/block").join(dev)).ok()?;
    let disk = if at.join("partition").exists() {
        at.parent()?.to_path_buf()
    } else {
        at
    };
    Some(
        std::fs::read_to_string(disk.join("dev"))
            .ok()?
            .trim()
            .to_string(),
    )
}

/// The mount a path is on, and its source device, from `/proc/self/mountinfo`.
fn mount_of(path: &Path) -> Option<(PathBuf, String)> {
    let path = std::fs::canonicalize(path).ok()?;
    let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let mut best: Option<(PathBuf, String)> = None;
    for line in info.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(mp), Some(dash)) = (f.get(4), f.iter().position(|w| *w == "-")) else {
            continue;
        };
        let mp = PathBuf::from(mp.replace("\\040", " "));
        if path.starts_with(&mp)
            && best
                .as_ref()
                .is_none_or(|(b, _)| mp.as_os_str().len() >= b.as_os_str().len())
        {
            best = Some((mp, f.get(dash + 2).unwrap_or(&"").to_string()));
        }
    }
    best
}

impl Disks for HostDisks {
    fn space(&self, path: &Path) -> Option<Space> {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::MetadataExt as _;
        let dev = std::fs::metadata(path).ok()?.dev();
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a valid NUL-terminated path and `st` a writable statvfs.
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        let frsize = st.f_frsize as u64;
        Some(Space {
            dev,
            mount: mount_of(path)
                .map(|(m, _)| m)
                .unwrap_or_else(|| path.to_path_buf()),
            free: st.f_bavail as u64 * frsize,
            total: st.f_blocks as u64 * frsize,
        })
    }

    fn devices(&self, path: &Path) -> Vec<String> {
        use std::os::unix::fs::MetadataExt as _;
        let Ok(meta) = std::fs::metadata(path) else {
            return Vec::new();
        };
        let (ma, mi) = split_dev(meta.dev());
        let mut first = format!("{ma}:{mi}");
        // An anonymous device (btrfs, overlay) names no disk: the mount's source does.
        if ma == 0 {
            let Some(rdev) = mount_of(path)
                .and_then(|(_, src)| std::fs::metadata(src).ok())
                .map(|m| m.rdev())
                .filter(|r| *r != 0)
            else {
                return Vec::new();
            };
            let (ma, mi) = split_dev(rdev);
            first = format!("{ma}:{mi}");
        }
        let mut out = Vec::new();
        let Some(top) = whole_disk(&first) else {
            return out;
        };
        out.push(top.clone());
        // A stacked device (dm-crypt, LVM) also throttles at the disks under it.
        if let Ok(rd) = std::fs::read_dir(Path::new("/sys/dev/block").join(&top).join("slaves")) {
            for e in rd.flatten() {
                if let Some(d) = std::fs::read_to_string(e.path().join("dev"))
                    .ok()
                    .and_then(|d| whole_disk(d.trim()))
                    && !out.contains(&d)
                {
                    out.push(d);
                }
            }
        }
        out
    }

    fn pressure_full(&self) -> Option<f64> {
        pressure_full(&std::fs::read_to_string("/proc/pressure/io").ok()?)
    }
}

// ---------------------------------------------------------------- the guard

/// How often the guard looks, and how far back it attributes growth.
pub const IO_TICK: Duration = Duration::from_secs(5);
pub const IO_WINDOW: Duration = Duration::from_secs(60);
/// Consecutive ticks over the pressure threshold before the top writer is throttled.
pub const PRESSURE_TICKS: u32 = 4;
/// Growth below this in the window is not a disk hog.
pub const MIN_BLAME: u64 = 1 << 20;

struct Watched {
    job: Arc<Job>,
    samples: VecDeque<(Instant, IoStat)>,
}

impl Watched {
    /// Bytes and writes over the window, and the span they cover.
    fn window(&self) -> (u64, u64, Duration) {
        match (self.samples.front(), self.samples.back()) {
            (Some((t0, a)), Some((t1, b))) => (
                b.wbytes.saturating_sub(a.wbytes),
                b.wios.saturating_sub(a.wios),
                t1.saturating_duration_since(*t0),
            ),
            _ => (0, 0, Duration::ZERO),
        }
    }
}

/// A row the guard owes a job, taken once by whoever says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoNote {
    pub job: JobId,
    pub text: String,
}

/// The io accounting and the disk floor, on a thread of its own.
pub struct IoGuard {
    policy: Mutex<IoPolicy>,
    watching: Mutex<Vec<Watched>>,
    notes: Mutex<Vec<IoNote>>,
    hot: Mutex<u32>,
    disks: Box<dyn Disks>,
    proc_root: PathBuf,
    wake: Condvar,
    closed: AtomicBool,
}

impl IoGuard {
    /// The guard, ticking on its own thread when `tick` is set.
    pub fn new(
        tree: Arc<dyn ScopeTree>,
        disks: Box<dyn Disks>,
        proc_root: PathBuf,
        tick: bool,
    ) -> Arc<IoGuard> {
        let g = Arc::new(IoGuard {
            policy: Mutex::new(IoPolicy::default()),
            watching: Mutex::new(Vec::new()),
            notes: Mutex::new(Vec::new()),
            hot: Mutex::new(0),
            disks,
            proc_root,
            wake: Condvar::new(),
            closed: AtomicBool::new(false),
        });
        if tick {
            let watch = Arc::clone(&g);
            if let Err(e) = std::thread::Builder::new()
                .name("letibot-io".into())
                .spawn(move || watch.run(tree))
            {
                eprintln!(
                    "letibot: the disk floor did not start ({e}); nothing watches free space"
                );
            }
        }
        g
    }

    pub fn policy(&self) -> IoPolicy {
        self.policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_policy(&self, p: IoPolicy) {
        *self.policy.lock().unwrap_or_else(|e| e.into_inner()) = p;
    }

    pub fn watch(&self, job: Arc<Job>) {
        let mut g = self.watching.lock().unwrap_or_else(|e| e.into_inner());
        if !g.iter().any(|w| w.job.id == job.id) {
            g.push(Watched {
                job,
                samples: VecDeque::new(),
            });
        }
    }

    pub fn take_note(&self, job: &JobId) -> Option<String> {
        let mut g = self.notes.lock().unwrap_or_else(|e| e.into_inner());
        let at = g.iter().position(|n| &n.job == job)?;
        Some(g.remove(at).text)
    }

    pub fn notes(&self) -> Vec<IoNote> {
        self.notes.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn note(&self, job: &JobId, text: String) {
        let mut g = self.notes.lock().unwrap_or_else(|e| e.into_inner());
        g.push(IoNote {
            job: job.clone(),
            text,
        });
        let over = g.len().saturating_sub(super::host::RECORD_LIMIT);
        g.drain(..over);
    }

    pub fn close(&self) {
        let g = self.watching.lock().unwrap_or_else(|e| e.into_inner());
        self.closed.store(true, Ordering::SeqCst);
        drop(g);
        self.wake.notify_all();
    }

    fn run(&self, tree: Arc<dyn ScopeTree>) {
        loop {
            {
                let g = self.watching.lock().unwrap_or_else(|e| e.into_inner());
                if self.closed.load(Ordering::SeqCst) {
                    return;
                }
                let _ = self
                    .wake
                    .wait_timeout(g, IO_TICK)
                    .unwrap_or_else(|e| e.into_inner());
            }
            if self.closed.load(Ordering::SeqCst) {
                return;
            }
            self.scan(tree.as_ref(), Instant::now());
        }
    }

    /// `io.max` for a job at its spawn, from the configured caps. `Some` is the row's sentence.
    pub fn cap_new(&self, tree: &dyn ScopeTree, dir: &Path, cwd: &Path) -> Option<String> {
        let p = self.policy();
        if p.wbps.is_none() && p.wiops.is_none() {
            return None;
        }
        let caps = cap_words(p.wbps, p.wiops);
        if let Err(why) = tree.io_controls() {
            return Some(format!("NOT throttled ({caps} was configured: {why})"));
        }
        let devices = self.devices_for(cwd, &p.watch);
        Some(match write_io_max(dir, &devices, p.wbps, p.wiops) {
            Ok(()) => format!("io.max {caps} (configured)"),
            Err(why) => format!("NOT throttled ({caps} was configured: {why})"),
        })
    }

    fn devices_for(&self, cwd: &Path, watch: &[PathBuf]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in std::iter::once(cwd).chain(watch.iter().map(|p| p.as_path())) {
            for d in self.disks.devices(p) {
                if !out.contains(&d) {
                    out.push(d);
                }
            }
        }
        out
    }

    /// One look: sample every running job, then the floor, then the pressure.
    pub fn scan(&self, tree: &dyn ScopeTree, now: Instant) {
        let policy = self.policy();
        let mut g = self.watching.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|w| w.job.state().is_running());
        for w in g.iter_mut() {
            let dir = w.job.lifetime().scope.path;
            let read = monotonic(w.job.io_used(), io_use_at(&dir, &self.proc_root));
            if let Some(u) = read {
                w.job.record_io(u);
                w.samples.push_back((now, u.stat));
            }
            // One sample older than the window stays, as the window's baseline.
            while w.samples.len() > 2
                && w.samples
                    .get(1)
                    .is_some_and(|(t, _)| now.saturating_duration_since(*t) >= IO_WINDOW)
            {
                w.samples.pop_front();
            }
        }
        let mut killed = Vec::new();
        self.floor(&policy, &g, tree, &mut killed);
        self.pressure(&policy, &g, tree);
        g.retain(|w| !killed.contains(&w.job.id));
    }

    fn floor(
        &self,
        policy: &IoPolicy,
        g: &[Watched],
        tree: &dyn ScopeTree,
        killed: &mut Vec<JobId>,
    ) {
        if policy.floor == DiskFloor::Off || g.is_empty() {
            return;
        }
        let mut seen: Vec<u64> = Vec::new();
        let paths: Vec<PathBuf> = g
            .iter()
            .map(|w| PathBuf::from(&w.job.cwd))
            .chain(policy.watch.iter().cloned())
            .collect();
        for path in paths {
            let Some(space) = self.disks.space(&path) else {
                continue;
            };
            if seen.contains(&space.dev) {
                continue;
            }
            seen.push(space.dev);
            let Some(floor) = policy.floor.bytes_of(space.total) else {
                continue;
            };
            if space.free >= floor {
                continue;
            }
            let mut blame: Vec<(&Watched, u64)> = g
                .iter()
                .filter(|w| w.job.io_marks().frozen.is_none() && !killed.contains(&w.job.id))
                .map(|w| (w, w.window().0))
                .filter(|(_, b)| *b >= MIN_BLAME)
                .collect();
            blame.sort_by_key(|b| std::cmp::Reverse(b.1));
            let Some(top) = blame.first().map(|b| b.1) else {
                super::scope::said_once(
                    &format!("disk-floor-{}", space.mount.display()),
                    &format!(
                        "{} is below its disk floor ({} free of {}, floor {}) and no running job \
                         wrote to disk in the last {}s",
                        space.mount.display(),
                        human_bytes(space.free),
                        human_bytes(space.total),
                        policy.floor.label(),
                        IO_WINDOW.as_secs()
                    ),
                );
                continue;
            };
            // The top writer, and any writing at least half as much.
            for (w, bytes) in blame.into_iter().take(3).filter(|(_, b)| *b * 2 >= top) {
                let why = format!(
                    "{} is at {} free of {} (floor {}); this job wrote {} in the last {}s",
                    space.mount.display(),
                    human_bytes(space.free),
                    human_bytes(space.total),
                    policy.floor.label(),
                    human_bytes(bytes),
                    w.window().2.as_secs().max(1)
                );
                let dir = w.job.lifetime().scope.path;
                match policy.action {
                    FloorAction::Freeze => match freeze(&dir, true) {
                        Ok(()) => {
                            w.job
                                .set_frozen(Some(format!("FROZEN by the disk floor: {why}")));
                            self.note(
                                &w.job.id,
                                format!(
                                    "`{}` was FROZEN by the disk floor: {why}. Free space, then \
                                     `job_resume` lets it go on; `job_kill` ends it.",
                                    w.job.id.0
                                ),
                            );
                        }
                        Err(e) => self.note(
                            &w.job.id,
                            format!(
                                "`{}` is the top writer at the disk floor ({why}) and could not \
                                 be frozen: {e}",
                                w.job.id.0
                            ),
                        ),
                    },
                    FloorAction::Kill => {
                        w.job.settle(JobState::Killed {
                            by: DISK_FLOOR_KILL.to_string(),
                        });
                        let _ = tree.end(&w.job.lifetime().scope);
                        killed.push(w.job.id.clone());
                        self.note(
                            &w.job.id,
                            format!("`{}` was KILLED by the disk floor: {why}.", w.job.id.0),
                        );
                    }
                }
            }
        }
    }

    fn pressure(&self, policy: &IoPolicy, g: &[Watched], tree: &dyn ScopeTree) {
        let Some(limit) = policy.pressure_full else {
            return;
        };
        let full = self.disks.pressure_full();
        let mut hot = self.hot.lock().unwrap_or_else(|e| e.into_inner());
        if !full.is_some_and(|f| f > limit) {
            *hot = 0;
            return;
        }
        *hot += 1;
        if *hot < PRESSURE_TICKS {
            return;
        }
        *hot = 0;
        let Some((w, (bytes, ios, over))) = g
            .iter()
            .filter(|w| w.job.io_marks().frozen.is_none() && w.job.io_marks().throttled.is_none())
            .map(|w| (w, w.window()))
            .filter(|(_, (b, i, _))| *b >= MIN_BLAME || *i > 0)
            .max_by_key(|(_, (b, i, _))| (*b, *i))
        else {
            return;
        };
        let secs = over.as_secs_f64().max(1.0);
        let wbps = ((bytes as f64 / secs) as u64 / 2).max(1 << 20);
        let wiops = ((ios as f64 / secs) as u64 / 2).max(50);
        let wbps = policy.wbps.map_or(wbps, |c| c.min(wbps));
        let wiops = policy.wiops.map_or(wiops, |c| c.min(wiops));
        let caps = cap_words(Some(wbps), Some(wiops));
        let why = format!(
            "io pressure full {:.0}% over {:.0}% for {}s; it wrote {} in the last {}s",
            full.unwrap_or(0.0),
            limit,
            PRESSURE_TICKS as u64 * IO_TICK.as_secs(),
            human_bytes(bytes),
            secs as u64
        );
        let result = tree.io_controls().and_then(|()| {
            let devices = self.devices_for(Path::new(&w.job.cwd), &policy.watch);
            write_io_max(
                &w.job.lifetime().scope.path,
                &devices,
                Some(wbps),
                Some(wiops),
            )
        });
        match result {
            Ok(()) => {
                w.job
                    .set_throttled(Some(format!("THROTTLED to {caps}: {why}")));
                self.note(
                    &w.job.id,
                    format!("`{}` was THROTTLED to {caps}: {why}.", w.job.id.0),
                );
            }
            Err(e) => {
                w.job.set_throttled(Some(format!(
                    "top writer under io pressure, NOT throttled: {e}"
                )));
                self.note(
                    &w.job.id,
                    format!(
                        "`{}` is the top writer under io pressure ({why}) and could not be \
                         throttled: {e}",
                        w.job.id.0
                    ),
                );
            }
        }
    }
}

fn cap_words(wbps: Option<u64>, wiops: Option<u64>) -> String {
    let mut parts = Vec::new();
    if let Some(b) = wbps {
        parts.push(format!("{}/s write", human_bytes(b)));
    }
    if let Some(i) = wiops {
        parts.push(format!("{i} writes/s"));
    }
    parts.join(", ")
}

/// A job's io state as its row says it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IoMarks {
    pub frozen: Option<String>,
    pub throttled: Option<String>,
}

#[cfg(test)]
mod tests;
