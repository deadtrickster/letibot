//! The daemon's spill store: one directory per project, a file per spill named by its content
//! hash, a size budget kept on every write, and a tombstone line for every eviction. A spill is
//! part of the conversation, so it outlives the daemon that wrote it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{SpillEntry, SpillError, SpillStore, content_hash};

/// The store's directory under the data dir: `<data dir>/spills/<project key>/`.
pub const SPILLS_DIR: &str = "spills";

/// A project's store holds at most this much before the least recently read spill goes.
pub const DEFAULT_SPILL_BUDGET: u64 = 2 * 1024 * 1024 * 1024;

/// The tombstone index: one JSON line per eviction, so a read of a gone hash can say when.
pub const TOMBSTONES: &str = "evicted.jsonl";

const LOCK: &str = ".lock";

/// What an eviction left behind.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Tombstone {
    pub hash: String,
    pub bytes: u64,
    pub at_ms: i64,
}

impl Tombstone {
    pub fn said(&self) -> String {
        format!(
            "spill {} was evicted on {}, {} bytes: its project's spill store went over its budget \
             and this was the least recently read",
            self.hash,
            crate::lifecycle::stamp_of(self.at_ms),
            self.bytes
        )
    }
}

/// How much one project's store holds, as `letibot holds` lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub spills: usize,
    pub bytes: u64,
    pub evicted: usize,
}

type Clock = Box<dyn Fn() -> SystemTime + Send + Sync>;

/// `<root>/<project key>/<hash>`. The directory is made on the first write, so a session that
/// never spills leaves nothing on disk.
pub struct DirStore {
    dir: PathBuf,
    budget: Option<u64>,
    clock: Clock,
}

impl std::fmt::Debug for DirStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirStore")
            .field("dir", &self.dir)
            .field("budget", &self.budget)
            .finish()
    }
}

impl DirStore {
    /// `budget: None` keeps everything.
    pub fn new(dir: impl Into<PathBuf>, budget: Option<u64>) -> Self {
        DirStore {
            dir: dir.into(),
            budget,
            clock: Box::new(SystemTime::now),
        }
    }

    pub fn for_project(root: &Path, project_key: &str, budget: Option<u64>) -> Self {
        Self::new(root.join(project_key), budget)
    }

    /// Recency is the file's mtime; a test sets the clock so the order is not left to timer
    /// resolution.
    pub fn with_clock(mut self, clock: impl Fn() -> SystemTime + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(hash)
    }

    fn touch(&self, path: &Path) {
        if let Ok(f) = std::fs::File::options().append(true).open(path) {
            let _ = f.set_modified((self.clock)());
        }
    }

    /// Writers of one project share the directory across sessions and daemons; the lock keeps the
    /// budget arithmetic and the tombstone file whole.
    fn lock(&self) -> Result<std::fs::File, SpillError> {
        use std::os::fd::AsRawFd;
        let f = std::fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(LOCK))
            .map_err(SpillError::Io)?;
        // SAFETY: flock on an fd this function owns; it is released when the file drops.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(SpillError::Io(std::io::Error::last_os_error()));
        }
        Ok(f)
    }

    fn make_dir(&self) -> Result<(), SpillError> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&self.dir)
            .map_err(SpillError::Io)
    }

    fn spills(&self) -> Vec<(String, u64, SystemTime)> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        rd.flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if !is_hash(&name) {
                    return None;
                }
                let meta = e.metadata().ok()?;
                Some((
                    name,
                    meta.len(),
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                ))
            })
            .collect()
    }

    /// Oldest read first until the rest fits. The spill just written is never the one evicted:
    /// the seam naming it is about to reach the model.
    fn evict_over_budget(&self, keep: &str) -> Result<(), SpillError> {
        let Some(budget) = self.budget else {
            return Ok(());
        };
        let mut all = self.spills();
        let mut total: u64 = all.iter().map(|(_, b, _)| b).sum();
        if total <= budget {
            return Ok(());
        }
        all.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
        let at_ms = millis((self.clock)());
        let mut stones = String::new();
        for (hash, bytes, _) in all {
            if total <= budget {
                break;
            }
            if hash == keep {
                continue;
            }
            if std::fs::remove_file(self.path(&hash)).is_ok() {
                total -= bytes;
                let stone = Tombstone { hash, bytes, at_ms };
                stones.push_str(&serde_json::to_string(&stone).unwrap_or_default());
                stones.push('\n');
            }
        }
        if !stones.is_empty() {
            let mut f = std::fs::File::options()
                .create(true)
                .append(true)
                .open(self.dir.join(TOMBSTONES))
                .map_err(SpillError::Io)?;
            f.write_all(stones.as_bytes()).map_err(SpillError::Io)?;
        }
        Ok(())
    }

    /// The last eviction of `hash`, if it was ever evicted.
    pub fn tombstone(&self, hash: &str) -> Option<Tombstone> {
        tombstones(&self.dir).into_iter().rfind(|t| t.hash == hash)
    }

    /// What a project directory holds, read from the files alone.
    pub fn usage(dir: &Path) -> Usage {
        let store = DirStore::new(dir, None);
        let spills = store.spills();
        Usage {
            spills: spills.len(),
            bytes: spills.iter().map(|(_, b, _)| b).sum(),
            evicted: tombstones(dir).len(),
        }
    }
}

impl SpillStore for DirStore {
    fn put(&self, _tool: &str, bytes: &[u8]) -> Result<String, SpillError> {
        let hash = content_hash(bytes);
        self.make_dir()?;
        let _lock = self.lock()?;
        let path = self.path(&hash);
        if path.exists() {
            self.touch(&path);
            return Ok(hash);
        }
        let tmp = self.dir.join(format!(".{hash}.tmp"));
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(SpillError::Io)?;
            f.write_all(bytes).map_err(SpillError::Io)?;
            f.set_modified((self.clock)()).map_err(SpillError::Io)?;
        }
        std::fs::rename(&tmp, &path).map_err(SpillError::Io)?;
        self.evict_over_budget(&hash)?;
        Ok(hash)
    }

    fn get(
        &self,
        hash: &str,
        range: Option<std::ops::Range<usize>>,
    ) -> Result<Vec<u8>, SpillError> {
        if !is_hash(hash) {
            return Err(SpillError::NotFound(hash.into()));
        }
        let path = self.path(hash);
        match std::fs::read(&path) {
            Ok(bytes) => {
                self.touch(&path);
                Ok(super::slice_range(&bytes, range))
            }
            Err(_) => Err(match self.tombstone(hash) {
                Some(t) => SpillError::Evicted(t),
                None => SpillError::NotFound(hash.into()),
            }),
        }
    }

    fn list(&self) -> Vec<SpillEntry> {
        let mut all = self.spills();
        all.sort_by_key(|a| std::cmp::Reverse(a.2));
        all.into_iter()
            .map(|(hash, bytes, _)| SpillEntry {
                hash,
                tool: String::new(),
                bytes: bytes as usize,
            })
            .collect()
    }

    fn size(&self, hash: &str) -> Option<usize> {
        if !is_hash(hash) {
            return None;
        }
        std::fs::metadata(self.path(hash))
            .ok()
            .map(|m| m.len() as usize)
    }
}

fn tombstones(dir: &Path) -> Vec<Tombstone> {
    std::fs::read_to_string(dir.join(TOMBSTONES))
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn is_hash(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

fn millis(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    fn clocked(dir: &Path, budget: Option<u64>, now: &Arc<AtomicU64>) -> DirStore {
        let now = now.clone();
        DirStore::for_project(dir, "proj-0123456789ab", budget).with_clock(move || {
            SystemTime::UNIX_EPOCH + Duration::from_secs(now.load(Ordering::SeqCst))
        })
    }

    #[test]
    fn a_spill_survives_a_reopen() {
        let root = crate::backend::tempdir::TempDir::new();
        let body = "kept across a restart\n".repeat(100);
        let hash = DirStore::for_project(root.path(), "p", None)
            .put("bash", body.as_bytes())
            .unwrap();
        let again = DirStore::for_project(root.path(), "p", None);
        assert_eq!(again.get(&hash, None).unwrap(), body.as_bytes());
        assert_eq!(again.get(&hash, Some(0..4)).unwrap(), b"kept");
        assert!(root.path().join("p").join(&hash).is_file());
    }

    #[test]
    fn over_budget_the_least_recently_read_is_evicted_and_said() {
        let root = crate::backend::tempdir::TempDir::new();
        let now = Arc::new(AtomicU64::new(1_800_000_000));
        let store = clocked(root.path(), Some(250), &now);
        let tick = || now.fetch_add(10, Ordering::SeqCst);
        let a = store.put("t", &[b'a'; 100]).unwrap();
        tick();
        let b = store.put("t", &[b'b'; 100]).unwrap();
        tick();
        // `a` was written first and read last, so `b` is the least recently read.
        store.get(&a, None).unwrap();
        tick();
        let c = store.put("t", &[b'c'; 100]).unwrap();

        assert!(store.get(&a, None).is_ok());
        assert!(store.get(&c, None).is_ok());
        match store.get(&b, None) {
            Err(SpillError::Evicted(t)) => {
                assert_eq!(t.bytes, 100);
                assert_eq!(t.hash, b);
                assert_eq!(t.at_ms, 1_800_000_030_000);
            }
            other => panic!("expected an eviction, got {other:?}"),
        }
        // The tombstone outlives the store that wrote it.
        let reopened = DirStore::for_project(root.path(), "proj-0123456789ab", None);
        let said = reopened.get(&b, None).unwrap_err().to_string();
        assert!(said.contains("evicted on 2027-01-15"), "{said}");
        assert!(said.contains("100 bytes"), "{said}");
        let usage = DirStore::usage(reopened.dir());
        assert_eq!((usage.spills, usage.bytes, usage.evicted), (2, 200, 1));
    }

    #[test]
    fn a_hash_never_written_is_not_found_not_evicted() {
        let root = crate::backend::tempdir::TempDir::new();
        let store = DirStore::for_project(root.path(), "p", Some(10));
        assert!(matches!(
            store.get("0123456789abcdef", None),
            Err(SpillError::NotFound(_))
        ));
        assert!(matches!(
            store.get("../../etc/passwd", None),
            Err(SpillError::NotFound(_))
        ));
    }
}
