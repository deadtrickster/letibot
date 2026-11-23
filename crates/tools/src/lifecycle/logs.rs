//! Files that grow on their own: daemon logs, a session's own record streams, job captures and
//! store journals.
//!
//! A daemon rotates its own log, because only the writer can reopen its descriptor. A session's
//! record streams are rotated and aged out by the reap pass, because their appenders open the file
//! per line (`docs/lifecycle.md` G4).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::human_bytes;

/// `[gc.logs]`: the bounds, with grok's 32 MiB and three kept as the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogPolicy {
    /// A daemon log, or one of a session's record streams, past this is rotated.
    pub max_bytes: u64,
    /// Rotated files kept beside the live one: `x.log.1` .. `x.log.<keep>`.
    pub keep: usize,
    /// A job's output file stops growing here.
    pub capture_max: u64,
    /// A session's record streams are removed once it has been archived this long (plan 1.7's 30
    /// days). The clock starts at the archive, so a session in the list is never touched.
    pub age_days: u64,
}

impl Default for LogPolicy {
    fn default() -> Self {
        LogPolicy {
            max_bytes: 32 * 1024 * 1024,
            keep: 3,
            capture_max: 1024 * 1024 * 1024,
            age_days: 30,
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

/// A thread that checks stdout every `every` and rotates it past the bound, and with it the
/// launcher's shared start log in `logs_dir`. The first line of a fresh file says so.
pub fn spawn_rotation(policy: LogPolicy, every: std::time::Duration, logs_dir: PathBuf) {
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
                // The shared start log is the launcher's, written with `>>` by a process that is
                // not this one, so it is rotated by name rather than by descriptor.
                match rotate_shared_log(&logs_dir, &policy) {
                    Ok(Some(p)) => eprintln!(
                        "letibot: {} passed {}, rotated; {} older file(s) kept",
                        p.display(),
                        human_bytes(policy.max_bytes),
                        policy.keep
                    ),
                    Ok(None) => {}
                    Err(why) => eprintln!(
                        "letibot: {} was not rotated: {why}",
                        logs_dir.join(SHARED_LOG).display()
                    ),
                }
            }
        });
}

/// `~/logs/harnessd.log`: one line per start, written by `scripts/letibot` with `>>` and never
/// rotated by it, so the daemon that a start wrote into rotates it by name.
pub const SHARED_LOG: &str = "harnessd.log";

/// Rotate the launcher's shared start log when it is over the bound. Every folder's daemon runs
/// this, so two can race: the loser's rename finds the file it moved already gone, which is the
/// rotation having happened, not a failure.
pub fn rotate_shared_log(dir: &Path, policy: &LogPolicy) -> std::io::Result<Option<PathBuf>> {
    let path = dir.join(SHARED_LOG);
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > policy.max_bytes => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    match rotate(&path, policy.keep) {
        Ok(()) => Ok(Some(path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A file in a session's logs directory that must outlive the session: the room notes it never
/// took are that session's record, and plan 1.7's clock is for debug logs.
pub const KEPT_RECORDS: &[&str] = &["room-notes.jsonl"];

/// `x.jsonl.2` as `x.jsonl`: the name a rotated file was rotated from.
fn rotated_from(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((base, n)) if n.parse::<usize>().is_ok() => base,
        _ => name,
    }
}

/// Is this one of a session's own record streams - the debug logs plan 1.7 ages out? They are the
/// `*.jsonl` files the daemon appends a line to per event (offers, deliveries, claims), and the
/// rotated files of one. A state file (`limit-hook.json`) and a kept record are not.
pub fn is_record_stream(name: &str) -> bool {
    let base = rotated_from(name);
    base.ends_with(".jsonl") && !KEPT_RECORDS.contains(&base)
}

/// What the store says about one session whose logs are on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionAge {
    /// When the session was archived, unix ms. `None` for one still in the list.
    pub archived_ms: Option<i64>,
    /// The store still holds an owner root for it open, so a daemon may be writing these logs.
    pub live: bool,
}

/// One session's logs directory, and what the size and age rules do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLogs {
    pub id: String,
    pub dir: PathBuf,
    /// Every record stream in it, rotated files included.
    pub files: Vec<PathBuf>,
    pub bytes: u64,
    /// Archived past `age_days` and not held open: every record stream goes, kept records do not.
    pub aged: bool,
    /// What a reap removes: all of them when `aged`, else the rotated files past `keep`.
    pub due: Vec<PathBuf>,
}

/// The sessions under a logs root (`<logs root>/<session id>/`), each with what the size and age
/// rules would do. A directory whose session the store does not name is left alone: there is no
/// archive clock to age it by, and a pass does not guess.
pub fn session_logs(
    root: &Path,
    policy: &LogPolicy,
    ages: &BTreeMap<String, SessionAge>,
    now_ms: i64,
) -> Vec<SessionLogs> {
    let age_ms = (policy.age_days as i64).saturating_mul(86_400_000);
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let id = e.file_name().to_string_lossy().into_owned();
        // `projects/` holds the box's own offer logs; only a session directory is one of these.
        if !id.starts_with("s-") || !e.path().is_dir() {
            continue;
        }
        let Ok(files_rd) = std::fs::read_dir(e.path()) else {
            continue;
        };
        let mut files: Vec<PathBuf> = files_rd
            .flatten()
            .filter(|f| is_record_stream(&f.file_name().to_string_lossy()))
            .map(|f| f.path())
            .collect();
        files.sort();
        if files.is_empty() {
            continue;
        }
        let bytes = files
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        let state = ages.get(&id).copied().unwrap_or(SessionAge {
            archived_ms: None,
            live: true,
        });
        let aged = !state.live
            && state
                .archived_ms
                .is_some_and(|ms| now_ms.saturating_sub(ms) >= age_ms);
        let due: Vec<PathBuf> = files
            .iter()
            .filter(|p| {
                if aged {
                    return true;
                }
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                let n = name
                    .rsplit_once('.')
                    .and_then(|(_, k)| k.parse::<usize>().ok());
                n.is_some_and(|n| n > policy.keep)
            })
            .cloned()
            .collect();
        out.push(SessionLogs {
            id,
            dir: e.path(),
            files,
            bytes,
            aged,
            due,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Rotate each live record stream past `max_bytes`: `letibot gc --now`'s session half. The
/// daemon's appenders open the file per line, so a rename is safe and the next write makes a fresh
/// one. A session about to be aged out is not rotated, and the rotated files a lowered `keep`
/// leaves behind are the plan's `reap` list, removed by the same pass.
pub fn rotate_session_logs(
    root: &Path,
    policy: &LogPolicy,
    ages: &BTreeMap<String, SessionAge>,
    now_ms: i64,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for s in session_logs(root, policy, ages, now_ms) {
        if s.aged {
            continue;
        }
        for p in &s.files {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if rotated_from(&name) != name {
                continue;
            }
            if rotate_if_over(p, policy).unwrap_or(false) {
                out.push(p.clone());
            }
        }
    }
    out
}

/// The sessions' own logs under `root`, as one bounded row: what plan 1.7 ages out, and what
/// `letibot holds` and `letibot gc` print.
pub fn session_log_holding(
    root: &Path,
    policy: &LogPolicy,
    ages: &BTreeMap<String, SessionAge>,
    now_ms: i64,
) -> Option<BoundedHolding> {
    let all = session_logs(root, policy, ages, now_ms);
    let files: usize = all.iter().map(|s| s.files.len()).sum();
    if files == 0 {
        return None;
    }
    Some(BoundedHolding {
        kind: "session-logs",
        handle: root.display().to_string(),
        bytes: all.iter().map(|s| s.bytes).sum(),
        files,
        reap: all.iter().flat_map(|s| s.due.iter().cloned()).collect(),
        said: format!(
            "per session, rotated past {} keeping {}; a session archived {} days ago is removed, \
             never one the store still holds open, its kept room notes or its state file",
            human_bytes(policy.max_bytes),
            policy.keep,
            policy.age_days
        ),
    })
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
            age_days: 30,
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
            age_days: 30,
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
            age_days: 30,
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

    const DAY: i64 = 86_400_000;

    /// A session's records: two streams, a rotated one, the state file and the room notes kept with
    /// it. The kept record and the state file are not streams, so the rules never name them.
    fn session_dir(d: &Dir, id: &str) -> PathBuf {
        let dir = d.0.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in [
            ("delivery-log.jsonl", 40),
            ("recall-offers.jsonl", 30),
            ("recall-offers.jsonl.1", 20),
            ("limit-hook.json", 5),
            ("room-notes.jsonl", 60),
        ] {
            write(&dir.join(name), bytes);
        }
        dir
    }

    fn ages(id: &str, archived_ms: Option<i64>, live: bool) -> BTreeMap<String, SessionAge> {
        let mut m = BTreeMap::new();
        m.insert(id.to_string(), SessionAge { archived_ms, live });
        m
    }

    /// Plan 1.7's clock starts at the archive: a session archived past `age_days`, whose owner root
    /// the store no longer holds open, has every record stream removed - and the room notes it
    /// never took and its limit state stay, because those are its record and not a debug log.
    #[test]
    fn an_archived_sessions_record_streams_age_out_and_its_kept_records_do_not() {
        let d = Dir::new("age");
        let dir = session_dir(&d, "s-old");
        let now = 1_000 * DAY;
        let policy = LogPolicy::default();

        let fresh = ages("s-old", Some(now - 29 * DAY), false);
        let held = session_logs(&d.0, &policy, &fresh, now);
        assert_eq!(held.len(), 1);
        assert!(!held[0].aged, "29 days is inside the bound");
        assert!(held[0].due.is_empty(), "{:?}", held[0].due);

        let old = ages("s-old", Some(now - 31 * DAY), false);
        let held = session_logs(&d.0, &policy, &old, now);
        let due: Vec<&str> = held[0]
            .due
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            due,
            vec![
                "delivery-log.jsonl",
                "recall-offers.jsonl",
                "recall-offers.jsonl.1"
            ],
            "the three streams, and only them"
        );
        let row = session_log_holding(&d.0, &policy, &old, now).unwrap();
        assert_eq!(
            (row.kind, row.files, row.reap.len()),
            ("session-logs", 3, 3)
        );
        assert_eq!(row.bytes, 90, "{} ", row.line());
        assert!(row.line().contains("30 days"), "{}", row.line());
        // The reap leaves the two files that must outlive it.
        for p in &row.reap {
            std::fs::remove_file(p).unwrap();
        }
        assert!(dir.join("room-notes.jsonl").exists());
        assert!(dir.join("limit-hook.json").exists());
        assert!(session_log_holding(&d.0, &policy, &old, now).is_none());
    }

    /// A session the store holds open, and a session still in the list, are never aged out - the
    /// clock is the archive's and an open owner root is what "live" means here.
    #[test]
    fn a_live_session_or_one_in_the_list_is_never_aged_out() {
        let d = Dir::new("live");
        session_dir(&d, "s-open");
        session_dir(&d, "s-listed");
        let now = 1_000 * DAY;
        let policy = LogPolicy::default();
        let mut facts = ages("s-open", Some(now - 400 * DAY), true);
        facts.extend(ages("s-listed", None, false));
        let held = session_logs(&d.0, &policy, &facts, now);
        assert_eq!(held.len(), 2);
        assert!(held.iter().all(|s| !s.aged && s.due.is_empty()), "{held:?}");
        // And a directory the store does not name has no clock either.
        let unnamed = ages("someone-else", Some(now - 400 * DAY), false);
        assert!(
            session_logs(&d.0, &policy, &unnamed, now)
                .iter()
                .all(|s| !s.aged)
        );
    }

    /// A record stream past the bound is rotated where it lies, and a rotated file past `keep` is
    /// what a reap removes - rotating and reaping are two steps, and neither is the other.
    #[test]
    fn a_record_stream_rotates_at_its_size_and_its_rotated_files_are_reaped() {
        let d = Dir::new("stream-rotate");
        let dir = d.0.join("s-run");
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir.join("skills-offers.jsonl"), 200);
        write(&dir.join("notes-offers.jsonl"), 5);
        write(&dir.join("notes-offers.jsonl.4"), 5);
        let policy = LogPolicy {
            max_bytes: 100,
            keep: 3,
            capture_max: 0,
            age_days: 30,
        };
        let facts = ages("s-run", None, true);
        let rotated = rotate_session_logs(&d.0, &policy, &facts, 0);
        assert_eq!(rotated.len(), 1, "{rotated:?}");
        assert!(dir.join("skills-offers.jsonl.1").exists());
        assert!(!dir.join("skills-offers.jsonl").exists());
        assert!(
            dir.join("notes-offers.jsonl.4").exists(),
            "rotating is not reaping"
        );
        let due: Vec<String> = session_logs(&d.0, &policy, &facts, 0)[0]
            .due
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(due, vec!["notes-offers.jsonl.4".to_string()]);
    }

    /// The launcher's shared start log is rotated by name, because its writer holds no descriptor;
    /// and a second daemon that finds it already rotated is not an error.
    #[test]
    fn the_shared_start_log_rotates_by_name_and_a_race_is_not_a_failure() {
        let d = Dir::new("shared");
        let policy = LogPolicy {
            max_bytes: 100,
            keep: 2,
            capture_max: 0,
            age_days: 30,
        };
        write(&d.0.join(SHARED_LOG), 50);
        assert_eq!(
            rotate_shared_log(&d.0, &policy).unwrap(),
            None,
            "under the bound"
        );
        write(&d.0.join(SHARED_LOG), 200);
        assert_eq!(
            rotate_shared_log(&d.0, &policy).unwrap(),
            Some(d.0.join(SHARED_LOG))
        );
        assert_eq!(
            std::fs::metadata(rotated_path(&d.0.join(SHARED_LOG), 1))
                .unwrap()
                .len(),
            200
        );
        assert_eq!(
            rotate_shared_log(&d.0, &policy).unwrap(),
            None,
            "gone, not failed"
        );
        assert_eq!(rotated_of(&d.0.join(SHARED_LOG)).len(), 1, "kept");
    }
}
