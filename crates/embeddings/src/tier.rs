//! The tier as a caller sees it: one question in, one cosine out.
//!
//! A caller hands over its documents once, when they change, and then asks about one
//! text a round. Everything between - which engine, whether the vector is on disk, what
//! the corpus's vectors are - is this module's, so a caller's whole second tier is one
//! optional `&Tier` and one floor.

use std::path::Path;

use crate::cache::{Cache, CacheError, Hit};
use crate::config::{Mode, Settings};
use crate::cosine::Index;
use crate::model::{Model, ModelError};
use crate::remote::{Remote, RemoteError};

/// Where a vector comes from, once the mode has been read.
pub enum Engine {
    Internal(Model),
    Remote(Remote),
}

impl Engine {
    /// The name a cache key is built from.
    pub fn name(&self) -> &str {
        match self {
            Engine::Internal(m) => m.name(),
            Engine::Remote(r) => r.name(),
        }
    }

    /// The word a disclosure uses.
    pub fn kind(&self) -> &'static str {
        match self {
            Engine::Internal(_) => "internal",
            Engine::Remote(_) => "url",
        }
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, TierError> {
        let mut vectors = match self {
            Engine::Internal(m) => m.embed(&[text]).map_err(TierError::Model)?,
            Engine::Remote(r) => r.embed(&[text]).map_err(TierError::Remote)?,
        };
        vectors.pop().ok_or_else(|| TierError::Empty(text.len()))
    }
}

/// Why the second tier could not answer. A caller falls back to the lexical score on all
/// of these and says so once, which is why the variants are as narrow as this.
#[derive(Debug)]
pub enum TierError {
    Model(ModelError),
    Remote(RemoteError),
    Cache(CacheError),
    /// The engine answered a request with no vector at all.
    Empty(usize),
    /// The engine refused, in its own words - the model's or the endpoint's.
    Embed(String),
}

impl std::fmt::Display for TierError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TierError::Model(e) => write!(f, "{e}"),
            TierError::Remote(e) => write!(f, "{e}"),
            TierError::Cache(e) => write!(f, "{e}"),
            TierError::Empty(len) => {
                write!(f, "the embedder answered a {len}-byte text with no vector")
            }
            TierError::Embed(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TierError {}

impl From<CacheError> for TierError {
    fn from(e: CacheError) -> Self {
        TierError::Cache(e)
    }
}

/// What a fill did, which is what a log line says: how much of the corpus this box had
/// already paid for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Filled {
    pub embedded: usize,
    pub cached: usize,
}

/// The second tier: an engine, the cache it fills, and the corpus's vectors in memory.
pub struct Tier {
    engine: Engine,
    cache: Cache,
    index: Index,
}

impl Tier {
    /// Build the tier the mode asks for, or `None` when the mode is `off` - which is the
    /// whole of "lexical only".
    pub fn open(settings: &Settings, data_dir: &Path) -> Result<Option<Tier>, TierError> {
        let engine = match &settings.mode {
            Mode::Off => return Ok(None),
            Mode::Internal { model } => match model {
                Some(dir) => Engine::Internal(Model::open(dir).map_err(TierError::Model)?),
                None => Engine::Internal(Model::open_builtin(data_dir).map_err(TierError::Model)?),
            },
            Mode::Remote { url, model } => {
                Engine::Remote(Remote::open(url, model).map_err(TierError::Remote)?)
            }
        };
        let cache = Cache::open_in(data_dir)?;
        Ok(Some(Tier {
            engine,
            cache,
            index: Index::default(),
        }))
    }

    /// A tier over an engine and a cache a caller already holds, which is what a test
    /// and a second caller on one box both need.
    pub fn with(engine: Engine, cache: Cache) -> Tier {
        Tier {
            engine,
            cache,
            index: Index::default(),
        }
    }

    /// The engine's name, which is what a disclosure and a cache row are keyed by.
    pub fn model(&self) -> &str {
        self.engine.name()
    }

    pub fn kind(&self) -> &'static str {
        self.engine.kind()
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Embed every document that has changed and put its vector in memory. Only a text
    /// the cache has never seen costs an embedding, so a restart pays for the corpus once.
    pub fn fill(
        &mut self,
        documents: impl IntoIterator<Item = (String, String)>,
        now_ms: i64,
    ) -> Result<Filled, TierError> {
        let name = self.engine.name().to_string();
        let mut done = Filled::default();
        let mut rows: Vec<(String, Vec<f32>)> = Vec::new();
        for (key, text) in documents {
            let (vector, hit) = self
                .cache
                .embed_or_get(&name, &text, now_ms, |t| {
                    self.engine
                        .embed(t)
                        .map_err(|e| e.to_string())
                        .map(|v| normalise(v))
                })
                .map_err(|e| match e {
                    crate::cache::EmbedOrCacheError::Cache(c) => TierError::Cache(c),
                    crate::cache::EmbedOrCacheError::Embed(e) => TierError::Embed(e),
                })?;
            match hit {
                Hit::Cached => done.cached += 1,
                Hit::Embedded => done.embedded += 1,
            }
            rows.push((key, vector));
        }
        self.index = Index::new(rows);
        Ok(done)
    }

    /// One vector for a text - the round's query - from the cache when it has been seen
    /// before, which for a prompt is almost never and for an unchanged note is almost
    /// always.
    pub fn query(&self, text: &str, now_ms: i64) -> Result<Vec<f32>, TierError> {
        let name = self.engine.name().to_string();
        let (vector, _) = self
            .cache
            .embed_or_get(&name, text, now_ms, |t| {
                self.engine
                    .embed(t)
                    .map(normalise)
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| match e {
                crate::cache::EmbedOrCacheError::Cache(c) => TierError::Cache(c),
                crate::cache::EmbedOrCacheError::Embed(e) => TierError::Embed(e),
            })?;
        Ok(vector)
    }

    /// How alike a document and a query are, `None` when the document was never embedded.
    pub fn cosine(&self, key: &str, query: &[f32]) -> Option<f32> {
        self.index.cosine_to(key, query)
    }

    /// The documents most like a text, best first.
    pub fn rank(
        &self,
        text: &str,
        now_ms: i64,
        most: usize,
    ) -> Result<Vec<(String, f32)>, TierError> {
        let query = self.query(text, now_ms)?;
        Ok(self
            .index
            .nearest(&query, most)
            .into_iter()
            .map(|(k, s)| (k.to_string(), s))
            .collect())
    }
}

/// Unit length, because a cosine of two unit vectors is their dot product and a
/// checkpoint that does not normalise would otherwise score long texts higher.
fn normalise(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 && norm.is_finite() {
        for x in &mut vector {
            *x /= norm;
        }
    }
    vector
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use std::path::PathBuf;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "letibot-tier-{tag}-{}-{}-{}",
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

    /// A settings file with the fixture model as its `model`, and a data directory that
    /// holds the cache.
    fn tier(tag: &str) -> (Dir, Dir, Tier) {
        let model_dir = Dir::new(&format!("model-{tag}"));
        fixture::tiny_bert(model_dir.path()).expect("fixture");
        let data = Dir::new(&format!("data-{tag}"));
        let settings = Settings::from_toml(&format!(
            "[embeddings]\nmode = \"internal\"\nmodel = \"{}\"\n",
            model_dir.path().display()
        ))
        .expect("settings");
        let tier = Tier::open(&settings, data.path())
            .expect("open")
            .expect("a tier");
        (model_dir, data, tier)
    }

    fn docs() -> Vec<(String, String)> {
        vec![
            ("note/paraphrase".into(), "alpha beta gamma delta".into()),
            ("note/unrelated".into(), "kappa lambda mu nu".into()),
            ("note/short".into(), "alpha beta".into()),
        ]
    }

    /// The box's own clock: `Cache::open_in` sweeps stale rows on it, so a row stamped
    /// with a test's 1000 is a row thirty days old the moment it is written.
    fn now() -> i64 {
        letibot_metrics::now_ms() as i64
    }

    /// Mode off is no tier at all, which is the whole of "lexical only".
    #[test]
    fn mode_off_opens_no_tier() {
        assert!(
            Tier::open(&Settings::default(), Path::new("."))
                .unwrap()
                .is_none()
        );
    }

    /// Filling a corpus embeds each document once; filling it again from a fresh tier on
    /// the same data directory costs no embedding at all.
    #[test]
    fn a_second_fill_reads_every_vector_from_the_cache() {
        let (model_dir, data, mut tier) = tier("fill");
        let first = tier.fill(docs(), now()).unwrap();
        assert_eq!(
            first,
            Filled {
                embedded: 3,
                cached: 0
            }
        );
        drop(tier);
        let settings = Settings::from_toml(&format!(
            "[embeddings]\nmode = \"internal\"\nmodel = \"{}\"\n",
            model_dir.path().display()
        ))
        .unwrap();
        let mut again = Tier::open(&settings, data.path()).unwrap().expect("a tier");
        assert_eq!(
            again.fill(docs(), now()).unwrap(),
            Filled {
                embedded: 0,
                cached: 3
            }
        );
    }

    /// The ranking a caller uses: the paraphrase of a note comes first and the unrelated
    /// note last.
    #[test]
    fn ranking_puts_the_paraphrase_above_the_unrelated_note() {
        let (_m, _d, mut tier) = tier("rank");
        tier.fill(docs(), now()).unwrap();
        let ranked = tier.rank("alpha beta gamma", now(), 3).unwrap();
        assert_eq!(ranked[0].0, "note/paraphrase", "{ranked:?}");
        assert_eq!(ranked[2].0, "note/unrelated", "{ranked:?}");
    }

    /// A document that was never filled is `None` rather than zero, so a caller can keep
    /// "not embedded" apart from "not alike".
    #[test]
    fn a_document_outside_the_index_has_no_score() {
        let (_m, _d, mut tier) = tier("absent");
        tier.fill(docs(), now()).unwrap();
        let query = tier.query("alpha", now()).unwrap();
        assert!(tier.cosine("note/paraphrase", &query).is_some());
        assert_eq!(tier.cosine("note/missing", &query), None);
    }

    /// A URL mode against a server that is not there fails, and it fails as an error the
    /// caller can fall back from rather than as a panic or a zero.
    #[test]
    fn an_unreachable_endpoint_is_an_error_the_caller_can_fall_back_from() {
        let data = Dir::new("url");
        let settings =
            Settings::from_toml("[embeddings]\nmode = \"http://127.0.0.1:1\"\nmodel = \"m\"\n")
                .unwrap();
        let tier = Tier::open(&settings, data.path()).unwrap().expect("a tier");
        assert_eq!(tier.kind(), "url");
        assert_eq!(tier.model(), "m");
        assert!(tier.query("alpha beta", now()).is_err());
    }
}
