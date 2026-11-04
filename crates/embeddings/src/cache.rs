//! The vectors on disk: a cache keyed by `(model, sha256 of text)` in `<data dir>/embeddings.db`.
//! Separate from `sessions.db` so it carries no schema version and deleting it loses only time.
//! Rows unused for thirty days are dropped at open.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Thirty days, in the millisecond unit `last_used_ms` is.
pub const STALE_AFTER_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// The cache's own file, inside the data directory every other durable thing uses.
pub fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("embeddings.db")
}

/// Why the cache could not be opened or written. A cache that cannot be written is not
/// a reason to lose the score, so every caller may fall back to embedding again - but
/// the fault is said rather than swallowed.
#[derive(Debug)]
pub enum CacheError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::Sqlite(e) => write!(f, "embeddings.db: {e}"),
            CacheError::Io(e) => write!(f, "embeddings.db: {e}"),
        }
    }
}

impl std::error::Error for CacheError {}

impl From<rusqlite::Error> for CacheError {
    fn from(e: rusqlite::Error) -> Self {
        CacheError::Sqlite(e)
    }
}

impl From<std::io::Error> for CacheError {
    fn from(e: std::io::Error) -> Self {
        CacheError::Io(e)
    }
}

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS embedding (
    model        TEXT    NOT NULL,
    hash         TEXT    NOT NULL,
    dim          INTEGER NOT NULL,
    vector       BLOB    NOT NULL,
    last_used_ms INTEGER NOT NULL,
    PRIMARY KEY (model, hash)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS embedding_last_used ON embedding (last_used_ms);
";

/// A vector that came back, and how it got here. The distinction is what a caller logs:
/// a hit is the cache working, a miss that was embedded is the cache being filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Cached,
    Embedded,
}

pub struct Cache {
    conn: rusqlite::Connection,
    path: PathBuf,
}

impl Cache {
    /// Open the cache in a data directory, creating it and its parent. The sweep of
    /// stale rows happens here, on the clock the box is at.
    pub fn open_in(data_dir: &Path) -> Result<Cache, CacheError> {
        Cache::open_at(&db_path(data_dir), now_ms() as i64)
    }

    /// [`Cache::open_in`] at a stated instant, which is what a test that measures the
    /// thirty days needs.
    pub fn open_at(path: &Path, now_ms: i64) -> Result<Cache, CacheError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = rusqlite::Connection::open(path)?;
        // A cache is not the record: a writer's fsync per row buys nothing that
        // re-embedding does not, and the daemon is the only holder anyway.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        let cache = Cache {
            conn,
            path: path.to_path_buf(),
        };
        cache.sweep(now_ms)?;
        Ok(cache)
    }

    pub fn open_in_memory() -> Result<Cache, CacheError> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Cache {
            conn,
            path: PathBuf::from(":memory:"),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The vector for this text under this model, if this model has seen this text
    /// before. A hit moves the row's last-used instant, which is what keeps a corpus in
    /// use out of the sweep.
    pub fn get(
        &self,
        model: &str,
        text: &str,
        now_ms: i64,
    ) -> Result<Option<Vec<f32>>, CacheError> {
        let hash = key(text);
        let row: Option<(i64, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT dim, vector FROM embedding WHERE model = ?1 AND hash = ?2",
                (model, &hash),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(CacheError::Sqlite(other)),
            })?;
        let Some((dim, blob)) = row else {
            return Ok(None);
        };
        self.conn.execute(
            "UPDATE embedding SET last_used_ms = ?1 WHERE model = ?2 AND hash = ?3",
            (now_ms, model, &hash),
        )?;
        match unpack(&blob, dim) {
            Some(v) => Ok(Some(v)),
            None => {
                // A row this build cannot read is a row that must not poison every search:
                // drop it and answer *not cached*, so the caller embeds the text again instead
                // of losing the round's score to one stale row.
                self.conn.execute(
                    "DELETE FROM embedding WHERE model = ?1 AND hash = ?2",
                    (model, &hash),
                )?;
                Ok(None)
            }
        }
    }

    /// Store one vector. A row that is already there is rewritten, so a re-embed after a
    /// model rebuild replaces rather than accumulates.
    pub fn put(
        &self,
        model: &str,
        text: &str,
        vector: &[f32],
        now_ms: i64,
    ) -> Result<(), CacheError> {
        self.conn.execute(
            "INSERT INTO embedding (model, hash, dim, vector, last_used_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(model, hash) DO UPDATE SET \
                dim = excluded.dim, vector = excluded.vector, \
                last_used_ms = excluded.last_used_ms",
            (model, key(text), vector.len() as i64, pack(vector), now_ms),
        )?;
        Ok(())
    }

    /// Drop every row not used since `now_ms - STALE_AFTER_MS`, and say how many went.
    pub fn sweep(&self, now_ms: i64) -> Result<usize, CacheError> {
        Ok(self.conn.execute(
            "DELETE FROM embedding WHERE last_used_ms < ?1",
            (now_ms - STALE_AFTER_MS,),
        )?)
    }

    pub fn len(&self) -> Result<usize, CacheError> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM embedding", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn is_empty(&self) -> Result<bool, CacheError> {
        Ok(self.len()? == 0)
    }

    /// The vector for this text, embedding it only when the cache does not have it.
    pub fn embed_or_get<F>(
        &self,
        model: &str,
        text: &str,
        now_ms: i64,
        embed: F,
    ) -> Result<(Vec<f32>, Hit), EmbedOrCacheError>
    where
        F: FnOnce(&str) -> Result<Vec<f32>, String>,
    {
        match self.get(model, text, now_ms) {
            Ok(Some(v)) => return Ok((v, Hit::Cached)),
            // A cache that cannot be read is a cache this call does without; the
            // embedding is still worth its answer and a second sweep at open will retry.
            Ok(None) => {}
            Err(e) => return Err(EmbedOrCacheError::Cache(e)),
        }
        let v = embed(text).map_err(EmbedOrCacheError::Embed)?;
        self.put(model, text, &v, now_ms)
            .map_err(EmbedOrCacheError::Cache)?;
        Ok((v, Hit::Embedded))
    }
}

/// Either half of [`Cache::embed_or_get`]: the tier refused, or the cache could not be
/// written. A caller logs one and keeps the score.
#[derive(Debug)]
pub enum EmbedOrCacheError {
    Cache(CacheError),
    Embed(String),
}

impl std::fmt::Display for EmbedOrCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedOrCacheError::Cache(e) => write!(f, "{e}"),
            EmbedOrCacheError::Embed(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for EmbedOrCacheError {}

/// The content half of the key: a SHA-256 of the text, hex, so an edited note is a
/// different key and needs no invalidation.
pub fn key(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `f32`s as a blob, little-endian and explicit: a cache file that a second platform
/// reads is a cache file that must not depend on this one's byte order.
fn pack(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 4);
    for f in vector {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn unpack(blob: &[u8], dim: i64) -> Option<Vec<f32>> {
    if !blob.len().is_multiple_of(4) || blob.len() / 4 != dim as usize {
        return None;
    }
    Some(
        blob.as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
    )
}

/// The millisecond the cache stamps rows with, read from the tree's one clock.
fn now_ms() -> u64 {
    letibot_metrics::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test's own file, removed with the test. The tree has no `tempfile`: a
    /// directory under `TMPDIR` named for this process is enough and needs no crate.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "letibot-embeddings-{tag}-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).expect("scratch dir");
            Dir(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const DAY: i64 = 24 * 60 * 60 * 1000;

    /// The file is the cache's own, beside the data directory's other files.
    #[test]
    fn the_cache_is_its_own_file_in_the_data_directory() {
        let dir = Dir::new("path");
        assert_eq!(db_path(dir.path()), dir.path().join("embeddings.db"));
        let cache = Cache::open_in(dir.path()).expect("open");
        assert!(cache.path().exists());
        assert!(cache.is_empty().unwrap());
    }

    /// A put and a get are the same vector, bit for bit: a cache that returned an
    /// approximation would change a score for no reason.
    #[test]
    fn a_put_then_a_get_is_the_same_vector() {
        let dir = Dir::new("roundtrip");
        let cache = Cache::open_in(dir.path()).expect("open");
        let v = vec![0.25f32, -1.5, 3.0e-4, 0.0];
        cache.put("m", "the text", &v, 1_000).unwrap();
        assert_eq!(cache.get("m", "the text", 1_000).unwrap(), Some(v));
    }

    /// The key is the content and the model, so an edited note and a changed model are
    /// both misses - which is why nothing has to be invalidated.
    #[test]
    fn a_changed_text_or_model_is_a_miss() {
        let cache = Cache::open_in_memory().unwrap();
        cache.put("m", "one", &[1.0, 0.0], 1_000).unwrap();
        assert_eq!(cache.get("m", "one", 1_000).unwrap(), Some(vec![1.0, 0.0]));
        assert_eq!(cache.get("m", "one, edited", 1_000).unwrap(), None);
        assert_eq!(cache.get("another-model", "one", 1_000).unwrap(), None);
    }

    /// Thirty days unused and the row goes at open; twenty-nine and it stays. A corpus
    /// in use is kept alive by its own hits.
    #[test]
    fn a_row_unused_for_thirty_days_is_dropped_at_open() {
        let dir = Dir::new("sweep");
        let now = 1_000 * DAY;
        {
            let cache = Cache::open_at(&db_path(dir.path()), now).unwrap();
            cache.put("m", "fresh", &[1.0], now - 29 * DAY).unwrap();
            cache.put("m", "stale", &[2.0], now - 31 * DAY).unwrap();
            assert_eq!(cache.len().unwrap(), 2);
        }
        let cache = Cache::open_at(&db_path(dir.path()), now).unwrap();
        assert_eq!(cache.len().unwrap(), 1);
        assert_eq!(cache.get("m", "fresh", now).unwrap(), Some(vec![1.0]));
        assert_eq!(cache.get("m", "stale", now).unwrap(), None);
    }

    /// A hit is a hit: the embedder is not called, and the row's last-used instant
    /// moves so the sweep leaves it alone.
    #[test]
    fn a_hit_does_not_embed_and_a_miss_fills_the_cache() {
        let dir = Dir::new("hit");
        let cache = Cache::open_at(&db_path(dir.path()), 1_000).unwrap();
        let mut calls = 0;
        let (v, hit) = cache
            .embed_or_get("m", "text", 1_000, |_| {
                calls += 1;
                Ok(vec![0.5, 0.5])
            })
            .unwrap();
        assert_eq!((v, hit, calls), (vec![0.5, 0.5], Hit::Embedded, 1));
        let (v, hit) = cache
            .embed_or_get("m", "text", 2_000, |_| {
                calls += 1;
                Ok(vec![9.0])
            })
            .unwrap();
        assert_eq!((v, hit, calls), (vec![0.5, 0.5], Hit::Cached, 1));
    }

    /// A blob that is not a whole number of `f32`s costs one embedding, not the round's score.
    /// The row is dropped and the answer is not cached, so the caller embeds the text again.
    #[test]
    fn a_corrupt_blob_is_dropped_and_the_text_embedded_again() {
        let dir = Dir::new("corrupt");
        let cache = Cache::open_at(&db_path(dir.path()), 1_000).unwrap();
        cache.put("m", "text", &[1.0, 2.0], 1_000).unwrap();
        cache
            .conn
            .execute("UPDATE embedding SET vector = x'0102'", [])
            .unwrap();
        assert_eq!(cache.get("m", "text", 1_000).unwrap(), None);
        assert_eq!(cache.len().unwrap(), 0, "the row went with the read");

        let mut calls = 0;
        let (v, hit) = cache
            .embed_or_get("m", "text", 1_000, |_| {
                calls += 1;
                Ok(vec![0.5, 0.5])
            })
            .unwrap();
        assert_eq!((v, hit, calls), (vec![0.5, 0.5], Hit::Embedded, 1));
    }
}
