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

/// Spill roots other than the default one, one path per line, so `letibot holds` finds a
/// `--spill-dir` store after the daemon that used it is gone.
pub const ROOTS: &str = "roots";

/// Past this the tombstone file is rewritten: one line per hash, the newest kept.
pub const TOMBSTONE_CAP: u64 = 1024 * 1024;

/// What an older `--spill-dir` daemon named a spill inside its per-session directory.
const LEGACY_EXT: &str = ".out";

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
    legacy: Option<PathBuf>,
    tombstone_cap: u64,
    clock: Clock,
}

impl std::fmt::Debug for DirStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirStore")
            .field("dir", &self.dir)
            .field("budget", &self.budget)
            .field("legacy", &self.legacy)
            .finish()
    }
}

impl DirStore {
    /// `budget: None` keeps everything.
    pub fn new(dir: impl Into<PathBuf>, budget: Option<u64>) -> Self {
        DirStore {
            dir: dir.into(),
            budget,
            legacy: None,
            tombstone_cap: TOMBSTONE_CAP,
            clock: Box::new(SystemTime::now),
        }
    }

    /// An older daemon's per-session directory (`<root>/<hash of session id>/<hash>.out`), read
    /// when this store misses. It is never written, evicted or counted against the budget.
    pub fn with_legacy(mut self, dir: impl Into<PathBuf>) -> Self {
        self.legacy = Some(dir.into());
        self
    }

    pub fn with_tombstone_cap(mut self, bytes: u64) -> Self {
        self.tombstone_cap = bytes;
        self
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

    fn legacy_path(&self, hash: &str) -> Option<PathBuf> {
        self.legacy
            .as_ref()
            .map(|d| d.join(format!("{hash}{LEGACY_EXT}")))
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
        spills_in(&self.dir, false)
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
            drop(f);
            self.compact_tombstones()?;
        }
        Ok(())
    }

    /// Runs under the write lock. Only the newest line per hash answers a read, and a hash that
    /// is stored again needs none; if that still leaves the file over half the cap, the oldest
    /// go, and a read of those says `NotFound`.
    fn compact_tombstones(&self) -> Result<(), SpillError> {
        let file = self.dir.join(TOMBSTONES);
        let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        if size <= self.tombstone_cap {
            return Ok(());
        }
        let mut newest: Vec<Tombstone> = Vec::new();
        for t in tombstones(&self.dir).into_iter().rev() {
            if !newest.iter().any(|n| n.hash == t.hash) && !self.path(&t.hash).exists() {
                newest.push(t);
            }
        }
        let mut lines: Vec<String> = newest
            .iter()
            .rev()
            .filter_map(|t| serde_json::to_string(t).ok())
            .collect();
        let mut total: u64 = lines.iter().map(|l| l.len() as u64 + 1).sum();
        let mut skip = 0;
        while total > self.tombstone_cap / 2 && skip < lines.len() {
            total -= lines[skip].len() as u64 + 1;
            skip += 1;
        }
        lines.drain(..skip);
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        let tmp = self.dir.join(format!(".{TOMBSTONES}.tmp"));
        std::fs::write(&tmp, text).map_err(SpillError::Io)?;
        std::fs::rename(&tmp, &file).map_err(SpillError::Io)
    }

    /// The last eviction of `hash`, if it was ever evicted.
    pub fn tombstone(&self, hash: &str) -> Option<Tombstone> {
        tombstones(&self.dir).into_iter().rfind(|t| t.hash == hash)
    }

    /// What a project directory holds, read from the files alone.
    /// An older daemon's per-session directory counts its `.out` files.
    pub fn usage(dir: &Path) -> Usage {
        let spills = spills_in(dir, true);
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
            Err(_) => {
                if let Some(bytes) = self.legacy_path(hash).and_then(|p| std::fs::read(p).ok()) {
                    return Ok(super::slice_range(&bytes, range));
                }
                Err(match self.tombstone(hash) {
                    Some(t) => SpillError::Evicted(t),
                    None => SpillError::NotFound(hash.into()),
                })
            }
        }
    }

    fn list(&self) -> Vec<SpillEntry> {
        let mut all = self.spills();
        if let Some(old) = &self.legacy {
            for s in spills_in(old, true) {
                if !all.iter().any(|a| a.0 == s.0) {
                    all.push(s);
                }
            }
        }
        all.sort_by_key(|e| std::cmp::Reverse(e.2));
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
            .or_else(|e| match self.legacy_path(hash) {
                Some(p) => std::fs::metadata(p),
                None => Err(e),
            })
            .ok()
            .map(|m| m.len() as usize)
    }
}

/// `(hash, bytes, mtime)` of every spill file in `dir`; with `legacy`, `<hash>.out` too.
fn spills_in(dir: &Path, legacy: bool) -> Vec<(String, u64, SystemTime)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let hash = match name.strip_suffix(LEGACY_EXT) {
                Some(h) if legacy => h.to_string(),
                _ => name,
            };
            if !is_hash(&hash) {
                return None;
            }
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| {
                (
                    hash,
                    meta.len(),
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                )
            })
        })
        .collect()
}

/// Notes `root` in `<spills>/roots` unless it is `spills` itself or already there.
pub fn record_root(spills: &Path, root: &Path) -> std::io::Result<()> {
    if root == spills || recorded_roots(spills).iter().any(|r| r == root) {
        return Ok(());
    }
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(spills)?;
    let mut f = std::fs::File::options()
        .create(true)
        .append(true)
        .open(spills.join(ROOTS))?;
    writeln!(f, "{}", root.display())
}

/// The roots [`record_root`] noted, absolute paths only.
pub fn recorded_roots(spills: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(spills.join(ROOTS))
        .map(|t| {
            t.lines()
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a directory under a spill root is an older daemon's per-session one: its name is a
/// bare content hash, where a project key carries the project's name.
pub fn is_legacy_dir(name: &str) -> bool {
    is_hash(name)
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
    fn an_older_daemons_per_session_spill_is_read() {
        let root = crate::backend::tempdir::TempDir::new();
        let body = b"written by a daemon before the per-project store";
        let old = root.path().join(content_hash(b"s-1"));
        crate::spill::FileStore::new(root.path(), "s-1")
            .unwrap()
            .put("bash", body)
            .unwrap();
        let hash = content_hash(body);
        assert!(old.join(format!("{hash}.out")).is_file());

        let store = DirStore::for_project(root.path(), "p", Some(1)).with_legacy(&old);
        assert_eq!(store.get(&hash, None).unwrap(), body);
        assert_eq!(store.get(&hash, Some(0..7)).unwrap(), b"written");
        assert_eq!(store.size(&hash), Some(body.len()));
        assert!(store.list().iter().any(|e| e.hash == hash));
        // Never moved, and outside the budget a new write keeps.
        store.put("bash", &[b'n'; 10]).unwrap();
        assert_eq!(store.get(&hash, None).unwrap(), body);
        let usage = DirStore::usage(&old);
        assert_eq!((usage.spills, usage.bytes), (1, body.len() as u64));
        assert!(is_legacy_dir(&old.file_name().unwrap().to_string_lossy()));
        assert!(!is_legacy_dir("letibot-0123456789ab"));
    }

    #[test]
    fn the_tombstone_file_is_compacted_past_its_cap() {
        let root = crate::backend::tempdir::TempDir::new();
        let now = Arc::new(AtomicU64::new(1_800_000_000));
        let store = clocked(root.path(), Some(100), &now).with_tombstone_cap(600);
        let file = store.dir().join(TOMBSTONES);
        let mut hashes = Vec::new();
        for i in 0..40u8 {
            now.fetch_add(10, Ordering::SeqCst);
            hashes.push(store.put("t", &[i; 100]).unwrap());
            let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            assert!(size <= 600, "tombstones at {size} bytes after {i} writes");
        }
        // Every line names a different hash, none of them stored now.
        let stones = tombstones(store.dir());
        assert!(!stones.is_empty());
        let mut seen: Vec<&str> = stones.iter().map(|t| t.hash.as_str()).collect();
        seen.dedup();
        assert_eq!(seen.len(), stones.len());
        assert!(stones.iter().all(|t| store.size(&t.hash).is_none()));
        // The newest eviction still answers; the oldest was dropped and reads as never written.
        assert!(matches!(
            store.get(&hashes[38], None),
            Err(SpillError::Evicted(_))
        ));
        assert!(matches!(
            store.get(&hashes[0], None),
            Err(SpillError::NotFound(_))
        ));
        // A hash stored again loses its tombstone at the next compaction.
        let again = store.put("t", &[38u8; 100]).unwrap();
        assert_eq!(again, hashes[38]);
        for i in 40..60u8 {
            now.fetch_add(10, Ordering::SeqCst);
            store.put("t", &[i; 100]).unwrap();
        }
        assert!(
            tombstones(store.dir())
                .iter()
                .filter(|t| t.hash == hashes[38])
                .count()
                <= 1
        );
    }

    #[test]
    fn a_spill_root_is_recorded_once_and_the_default_never() {
        let data = crate::backend::tempdir::TempDir::new();
        let spills = data.path().join(SPILLS_DIR);
        let other = data.path().join("elsewhere");
        record_root(&spills, &spills).unwrap();
        assert!(recorded_roots(&spills).is_empty());
        record_root(&spills, &other).unwrap();
        record_root(&spills, &other).unwrap();
        assert_eq!(recorded_roots(&spills), vec![other]);
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
