//! Files that grow on their own: daemon logs, job captures and store journals.
//!
//! A daemon rotates its own log, because only the writer can reopen its descriptor.

use std::path::{Path, PathBuf};

use super::human_bytes;

/// `[gc.logs]`: the bounds, with grok's 32 MiB and three kept as the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogPolicy {
    /// A daemon log past this is rotated.
    pub max_bytes: u64,
    /// Rotated files kept beside the live one: `x.log.1` .. `x.log.<keep>`.
    pub keep: usize,
    /// A job's output file stops growing here.
    pub capture_max: u64,
}

impl Default for LogPolicy {
    fn default() -> Self {
        LogPolicy {
            max_bytes: 32 * 1024 * 1024,
            keep: 3,
            capture_max: 1024 * 1024 * 1024,
        }
    }
}

/// `x.log.N`.
pub fn rotated_path(path: &Path, n: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// Shift `x.log.N` up one, dropping the oldest, and move `x.log` to `x.log.1`. With `keep` 0 the
/// log is removed. The caller reopens `path`.
pub fn rotate(path: &Path, keep: usize) -> std::io::Result<()> {
    if keep == 0 {
        return remove_if_there(path);
    }
    for n in (1..keep).rev() {
        rename_if_there(&rotated_path(path, n), &rotated_path(path, n + 1))?;
    }
    std::fs::rename(path, rotated_path(path, 1))
}

/// [`rotate`] when the file is over the policy's size. `true` when it rotated.
pub fn rotate_if_over(path: &Path, policy: &LogPolicy) -> std::io::Result<bool> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > policy.max_bytes => rotate(path, policy.keep).map(|()| true),
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn rename_if_there(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn remove_if_there(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The rotated files of `path` that exist, as `(n, path)`, lowest first.
pub fn rotated_of(path: &Path) -> Vec<(usize, PathBuf)> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}.", name.to_string_lossy());
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(usize, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let rest = n.strip_prefix(&prefix)?;
            let k: usize = rest.parse().ok()?;
            Some((k, e.path()))
        })
        .collect();
    out.sort();
    out
}

/// Remove the rotated files past `keep`: what is left after `keep` was lowered, or by a writer
/// that rotated with another count. Returns what was removed.
pub fn trim_rotated(path: &Path, keep: usize) -> Vec<PathBuf> {
    rotated_of(path)
        .into_iter()
        .filter(|(n, _)| *n > keep)
        .filter_map(|(_, p)| std::fs::remove_file(&p).ok().map(|()| p))
        .collect()
}

/// The file a descriptor of this process writes to.
#[cfg(target_os = "linux")]
fn fd_path(fd: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()
}

#[cfg(target_os = "macos")]
fn fd_path(fd: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes, NUL-terminated, into `buf`.
    let rc = unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) };
    if rc == -1 {
        return None;
    }
    let len = buf.iter().position(|b| *b == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn fd_path(_fd: i32) -> Option<PathBuf> {
    None
}

/// `(device, inode, size)` of a descriptor when it is a regular file.
fn fd_file(fd: i32) -> Option<(u64, u64, u64)> {
    // SAFETY: `fstat` only writes the struct it is handed.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return None;
    }
    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return None;
    }
    Some((st.st_dev as u64, st.st_ino as u64, st.st_size as u64))
}

/// Rotate this process's stdout when it is a log file over the bound, and point stdout (and
/// stderr, when it is the same file) at a fresh file of the same name. `Ok(None)` when stdout is
/// not a file, is under the bound, or was already moved by somebody else.
pub fn rotate_stdio(policy: &LogPolicy) -> Result<Option<PathBuf>, String> {
    rotate_fds(1, Some(2), policy)
}

fn rotate_fds(out: i32, err: Option<i32>, policy: &LogPolicy) -> Result<Option<PathBuf>, String> {
    use std::os::unix::io::AsRawFd;
    let Some((dev, ino, size)) = fd_file(out) else {
        return Ok(None);
    };
    if size <= policy.max_bytes {
        return Ok(None);
    }
    let Some(path) = fd_path(out) else {
        return Ok(None);
    };
    // The name must still be our file: a second writer that rotated first owns the new one.
    match std::fs::metadata(&path) {
        Ok(m) => {
            use std::os::unix::fs::MetadataExt;
            if (m.dev(), m.ino()) != (dev, ino) {
                return Ok(None);
            }
        }
        Err(_) => return Ok(None),
    }
    let same_err = err.filter(|e| fd_file(*e).is_some_and(|(d, i, _)| (d, i) == (dev, ino)));
    rotate(&path, policy.keep).map_err(|e| format!("{}: {e}", path.display()))?;
    let fresh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    for fd in std::iter::once(out).chain(same_err) {
        // SAFETY: both descriptors are open; dup2 swaps the target atomically, so a write on
        // another thread lands in the old file or the new one, never nowhere.
        if unsafe { libc::dup2(fresh.as_raw_fd(), fd) } == -1 {
            return Err(format!(
                "{}: dup2 onto fd {fd}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(Some(path))
}

/// A thread that checks stdout every `every` and rotates it past the bound. The first line of the
/// fresh file says so.
pub fn spawn_stdio_rotation(policy: LogPolicy, every: std::time::Duration) {
    let _ = std::thread::Builder::new()
        .name("log-rotate".into())
        .spawn(move || {
            loop {
                std::thread::sleep(every);
                match rotate_stdio(&policy) {
                    Ok(Some(p)) => eprintln!(
                        "letibot: {} passed {}, rotated; {} older file(s) kept",
                        p.display(),
                        human_bytes(policy.max_bytes),
                        policy.keep
                    ),
                    Ok(None) => {}
                    Err(why) => {
                        eprintln!("letibot: log rotation stopped: {why}");
                        return;
                    }
                }
            }
        });
}

/// What the plan says about one bounded file or family of files on this box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedHolding {
    /// `log`, `wal` or `captures`.
    pub kind: &'static str,
    pub handle: String,
    pub bytes: u64,
    pub files: usize,
    /// Files `gc --now` would remove: rotated logs past the kept count.
    pub reap: Vec<PathBuf>,
    pub said: String,
}

impl BoundedHolding {
    pub fn line(&self) -> String {
        let word = if self.reap.is_empty() {
            "keep".to_string()
        } else {
            format!("reap now {} file(s)", self.reap.len())
        };
        format!(
            "{} {} {} in {} file(s) {word} — {}",
            self.kind,
            self.handle,
            human_bytes(self.bytes),
            self.files,
            self.said
        )
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.kind,
            "handle": self.handle,
            "bytes": self.bytes,
            "files": self.files,
            "reap": self.reap,
            "said": self.said,
        })
    }
}

fn is_daemon_log(name: &str) -> bool {
    name.ends_with(".log") && (name.starts_with("harnessd") || name.starts_with("room-"))
}

/// Every daemon and room log under `dir`, with its rotated files.
pub fn log_holdings(dir: &Path, policy: &LogPolicy) -> Vec<BoundedHolding> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            // A rotated file whose live log is gone still belongs to that name.
            let base = match n.rsplit_once('.') {
                Some((b, k)) if k.parse::<usize>().is_ok() => b.to_string(),
                _ => n,
            };
            is_daemon_log(&base).then_some(base)
        })
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| {
            let path = dir.join(&name);
            let live = std::fs::metadata(&path).map(|m| m.len()).ok();
            let rotated = rotated_of(&path);
            let bytes = live.unwrap_or(0)
                + rotated
                    .iter()
                    .filter_map(|(_, p)| std::fs::metadata(p).ok())
                    .map(|m| m.len())
                    .sum::<u64>();
            let reap: Vec<PathBuf> = rotated
                .iter()
                .filter(|(n, _)| *n > policy.keep)
                .map(|(_, p)| p.clone())
                .collect();
            let said = match live {
                Some(n) if n > policy.max_bytes => format!(
                    "over {}: its daemon rotates it within a minute; a daemon of an older build \
                     does not, and a restart moves it to one that does",
                    human_bytes(policy.max_bytes)
                ),
                _ => format!(
                    "rotated by its daemon past {}, {} kept (`[gc.logs]`)",
                    human_bytes(policy.max_bytes),
                    policy.keep
                ),
            };
            BoundedHolding {
                kind: "log",
                handle: path.display().to_string(),
                bytes,
                files: usize::from(live.is_some()) + rotated.len(),
                reap,
                said,
            }
        })
        .collect()
}

/// The store journals under the data directory: `*.db-wal` there and in `projects/`.
pub fn wal_holdings(data_dir: &Path) -> Vec<BoundedHolding> {
    let mut out = Vec::new();
    for dir in [data_dir.to_path_buf(), data_dir.join("projects")] {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            if !e.file_name().to_string_lossy().ends_with(".db-wal") {
                continue;
            }
            let bytes = e.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(BoundedHolding {
                kind: "wal",
                handle: e.path().display().to_string(),
                bytes,
                files: 1,
                reap: Vec::new(),
                said: "truncated by `wal_checkpoint(TRUNCATE)` at a daemon's idle exit; a reader \
                       still open keeps it"
                    .into(),
            });
        }
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.handle.cmp(&b.handle)));
    out
}

/// Job capture files in every owner root on this box (`<owner>/tmp/spill/job-*`), as one row.
pub fn capture_holding(box_dir: &Path, policy: &LogPolicy) -> Option<BoundedHolding> {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut biggest = 0u64;
    for owner in std::fs::read_dir(box_dir).ok()?.flatten() {
        let Ok(rd) = std::fs::read_dir(owner.path().join("tmp").join("spill")) else {
            continue;
        };
        for e in rd.flatten() {
            if !e.file_name().to_string_lossy().starts_with("job-") {
                continue;
            }
            let n = e.metadata().map(|m| m.len()).unwrap_or(0);
            files += 1;
            bytes += n;
            biggest = biggest.max(n);
        }
    }
    (files > 0).then(|| BoundedHolding {
        kind: "captures",
        handle: box_dir.display().to_string(),
        bytes,
        files,
        reap: Vec::new(),
        said: format!(
            "biggest {}, each stops at {} (`[gc.logs] capture_max`); removed with its owner's tmp",
            human_bytes(biggest),
            human_bytes(policy.capture_max)
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let p = std::env::temp_dir().join(format!("letibot-logs-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, n: usize) {
        std::fs::write(path, vec![b'x'; n]).unwrap();
    }

    /// Four rotations with keep 3: the live file and three rotated ones, the oldest dropped.
    #[test]
    fn a_log_rotates_at_its_size_and_keeps_n() {
        let d = Dir::new("rotate");
        let log = d.0.join("harnessd-k.log");
        let policy = LogPolicy {
            max_bytes: 100,
            keep: 3,
            capture_max: 0,
        };
        write(&log, 50);
        assert!(!rotate_if_over(&log, &policy).unwrap(), "under the bound");
        for round in 1..=4 {
            write(&log, 100 + round);
            assert!(rotate_if_over(&log, &policy).unwrap());
            assert!(!log.exists());
        }
        let kept: Vec<usize> = rotated_of(&log).into_iter().map(|(n, _)| n).collect();
        assert_eq!(kept, vec![1, 2, 3]);
        assert_eq!(std::fs::metadata(rotated_path(&log, 1)).unwrap().len(), 104);
        assert_eq!(std::fs::metadata(rotated_path(&log, 3)).unwrap().len(), 102);
    }

    /// A lowered keep leaves files the plan lists and `trim_rotated` removes.
    #[test]
    fn rotated_files_past_keep_are_listed_and_trimmed() {
        let d = Dir::new("trim");
        let log = d.0.join("room-box.log");
        write(&log, 10);
        for n in 1..=5 {
            write(&rotated_path(&log, n), 10);
        }
        write(&d.0.join("other.log"), 10);
        let policy = LogPolicy {
            max_bytes: 100,
            keep: 2,
            capture_max: 0,
        };
        let held = log_holdings(&d.0, &policy);
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!((held[0].files, held[0].bytes), (6, 60));
        assert_eq!(held[0].reap.len(), 3);
        assert!(held[0].line().contains("reap now 3"), "{}", held[0].line());
        let gone = trim_rotated(&log, 2);
        assert_eq!(gone.len(), 3);
        let kept: Vec<usize> = rotated_of(&log).into_iter().map(|(n, _)| n).collect();
        assert_eq!(kept, vec![1, 2]);
    }

    /// The descriptor half: a file opened as "stdout" past the bound is moved to `.1`, and the
    /// same descriptor then writes into a fresh file of the old name.
    #[test]
    fn a_descriptor_over_the_bound_is_reopened_on_a_fresh_file() {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;
        let d = Dir::new("fd");
        let log = d.0.join("harnessd-fd.log");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap();
        f.write_all(&[b'x'; 200]).unwrap();
        let policy = LogPolicy {
            max_bytes: 100,
            keep: 3,
            capture_max: 0,
        };
        let moved = rotate_fds(f.as_raw_fd(), None, &policy).unwrap();
        if fd_path(f.as_raw_fd()).is_none() && moved.is_none() {
            return; // no way to name a descriptor on this OS
        }
        assert_eq!(moved.as_deref(), Some(log.as_path()));
        f.write_all(b"after\n").unwrap();
        assert_eq!(std::fs::read(&log).unwrap(), b"after\n");
        assert_eq!(std::fs::metadata(rotated_path(&log, 1)).unwrap().len(), 200);
        assert_eq!(rotate_fds(f.as_raw_fd(), None, &policy).unwrap(), None);
    }

    #[test]
    fn journals_and_captures_are_listed() {
        let d = Dir::new("box");
        std::fs::create_dir_all(d.0.join("data/projects")).unwrap();
        write(&d.0.join("data/shared.db-wal"), 30);
        write(&d.0.join("data/projects/k.db-wal"), 70);
        write(&d.0.join("data/shared.db"), 5);
        let wal = wal_holdings(&d.0.join("data"));
        assert_eq!(wal.len(), 2);
        assert_eq!(wal[0].bytes, 70);
        let spill = d.0.join("gc/box/s-1/tmp/spill");
        std::fs::create_dir_all(&spill).unwrap();
        write(&spill.join("job-j1.out"), 40);
        write(&spill.join("job-j2.raw"), 2);
        write(&spill.join("abc.out"), 9);
        let cap = capture_holding(&d.0.join("gc/box"), &LogPolicy::default()).unwrap();
        assert_eq!((cap.files, cap.bytes), (2, 42));
        assert!(capture_holding(&d.0.join("nothing"), &LogPolicy::default()).is_none());
    }
}
