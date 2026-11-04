//! The tier as a caller sees it: one question in, one cosine out.
//! A caller hands over its documents once, then asks about one text a round. Engine, disk and
//! corpus vectors are this module's, so a caller's second tier is one optional `&Tier` and one floor.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::builtin::{self, DownloadError, Pin};
use crate::cache::{Cache, CacheError, EmbedOrCacheError, Hit};
use crate::config::{Mode, Settings};
use crate::cosine::Index;
use crate::model::{BUILTIN_MODEL, Model, ModelError};
use crate::remote::{Remote, RemoteError};

use letibot_http::retry::Policy;

/// Where a vector comes from, once the mode has been read. Boxed because a loaded
/// checkpoint is megabytes beside a URL's few fields, and an embedder is shared by every
/// caller on the box.
pub enum Engine {
    Internal(Box<Model>),
    Remote(Remote),
    /// Synonyms planted by hand, for a caller's test: no model, no download, and no
    /// `[embeddings]` value reaches it.
    Planted(Planted),
}

impl Engine {
    /// The name a cache key is built from.
    pub fn name(&self) -> &str {
        match self {
            Engine::Internal(m) => m.name(),
            Engine::Remote(r) => r.name(),
            Engine::Planted(p) => p.name(),
        }
    }

    /// The word a disclosure uses.
    pub fn kind(&self) -> &'static str {
        match self {
            Engine::Internal(_) => "internal",
            Engine::Remote(_) => "url",
            Engine::Planted(_) => "planted",
        }
    }
}

/// How many texts one request carries. The endpoint's `input` is an array, and a corpus of 300
/// notes is ten round trips rather than three hundred.
const BATCH: usize = 32;

/// A tier whose meaning is a table: each group is one dimension and each word in it sets it.
/// Built by [`crate::testing::planted`] only; not feature-gated so the arm compiles wherever the enum does.
pub struct Planted {
    groups: Vec<Vec<String>>,
}

impl Planted {
    pub fn new(groups: &[&[&str]]) -> Planted {
        Planted {
            groups: groups
                .iter()
                .map(|g| g.iter().map(|w| w.to_lowercase()).collect())
                .collect(),
        }
    }

    pub fn name(&self) -> &'static str {
        "planted"
    }

    /// How wide a vector this makes, which is the number of groups.
    pub fn dim(&self) -> usize {
        self.groups.len()
    }

    /// One word, one dimension: a word outside every group means nothing to this tier, and a
    /// text of such words has no direction at all.
    fn vector(&self, text: &str) -> Vec<f32> {
        let words: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(|w| w.to_lowercase())
            .collect();
        self.groups
            .iter()
            .map(|group| {
                if group.iter().any(|w| words.contains(w)) {
                    1.0
                } else {
                    0.0
                }
            })
            .collect()
    }
}

/// Why the second tier could not answer. A caller falls back to the lexical score on all
/// of these and says so once, which is why the variants are as narrow as this.
#[derive(Debug)]
pub enum TierError {
    Model(ModelError),
    Remote(RemoteError),
    Cache(CacheError),
    /// The built-in model could not be fetched, in the words of what stopped it.
    Download(DownloadError),
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
            TierError::Download(e) => write!(f, "{e}"),
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

/// The one embedder a box has. The engine and the cache, and nothing about what is
/// being scored: the corpus's vectors belong to the caller that holds them, so four callers
/// on one box load one model between them and pay for a text once.
pub struct Embedder {
    /// The engine, behind whatever lock it needs - see [`EngineLock`].
    engine: EngineLock,
    /// The box's one cache connection. Held for a lookup or a write and never across an embed:
    /// holding it across one was a slow fill stopping every other caller's scoring.
    cache: Mutex<Cache>,
    /// The first failure this box, held until a caller says it: every caller falls back to
    /// the lexical score on a failure, and a line per round would be worse than the silence.
    fault: Mutex<Option<String>>,
    /// What the box said before it fetched the built-in model, handed out once.
    announcement: Mutex<Option<String>>,
    /// Texts handed to the engine, and cache lookups made: what a round costs.
    embedded: AtomicU64,
    lookups: AtomicU64,
    bytes: AtomicU64,
    /// The last [`QUERY_MEMO`] query vectors by text hash, so a text asked twice in a few rounds
    /// is neither embedded nor read from disk again.
    memo: Mutex<std::collections::VecDeque<(String, Vec<f32>)>>,
    /// The words and vector of the last query the engine embedded, for [`NEAR`].
    last: Mutex<Option<(std::collections::HashSet<String>, Vec<f32>)>>,
    /// When the engine last embedded a query, for [`QUERY_BUDGET`].
    spent_at: Mutex<std::collections::VecDeque<std::time::Instant>>,
}

/// A query this similar (Jaccard over words) to the last embedded one reuses its vector. Only for
/// texts of at least [`NEAR_WORDS`] words: one word moves a short text's meaning.
pub const NEAR: f64 = 0.9;
pub const NEAR_WORDS: usize = 12;

/// Queries the engine may embed in a minute. Past it callers score on words and the fault says so.
/// Corpus fills are not counted: they are paid once per change.
pub const QUERY_BUDGET: usize = 30;

/// How many query vectors the embedder keeps in memory.
const QUERY_MEMO: usize = 32;

/// What an embedder has spent since it opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Spent {
    /// Texts the engine embedded.
    pub embedded: u64,
    /// Cache lookups, hit or miss.
    pub lookups: u64,
    /// Bytes of those texts.
    pub bytes: u64,
}

/// The engine, and only the lock it actually needs. The in-process checkpoint is not thread-safe and
/// takes a mutex; a remote endpoint is a request, and holding the box's one lock across it meant one
/// slow fill blocked every caller for as long as the HTTP read could take.
enum EngineLock {
    Internal(Mutex<Box<Model>>),
    /// A request, not a model: `Sync`, and called with no lock held at all.
    Remote(Remote),
    Planted(Planted),
}

impl EngineLock {
    fn new(engine: Engine) -> EngineLock {
        match engine {
            Engine::Internal(m) => EngineLock::Internal(Mutex::new(m)),
            Engine::Remote(r) => EngineLock::Remote(r),
            Engine::Planted(p) => EngineLock::Planted(p),
        }
    }

    fn name(&self) -> String {
        match self {
            EngineLock::Internal(m) => {
                let model = m.lock().unwrap_or_else(|e| e.into_inner());
                model.name().to_string()
            }
            EngineLock::Remote(r) => r.name().to_string(),
            EngineLock::Planted(p) => p.name().to_string(),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            EngineLock::Internal(_) => "internal",
            EngineLock::Remote(_) => "url",
            EngineLock::Planted(_) => "planted",
        }
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, TierError> {
        let mut vectors = self.embed_many(&[text])?;
        vectors.pop().ok_or_else(|| TierError::Empty(text.len()))
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, TierError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        match self {
            EngineLock::Internal(m) => {
                let model = m.lock().unwrap_or_else(|e| e.into_inner());
                model.embed(texts).map_err(TierError::Model)
            }
            EngineLock::Remote(r) => r.embed(texts).map_err(TierError::Remote),
            EngineLock::Planted(p) => Ok(texts.iter().map(|t| p.vector(t)).collect()),
        }
    }
}

impl std::fmt::Debug for Embedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Embedder({})", self.model())
    }
}

impl Embedder {
    /// Build the embedder the mode asks for, or `None` when the mode is `off` - which is the
    /// whole of "lexical only".
    pub fn open(settings: &Settings, data_dir: &Path) -> Result<Option<Arc<Embedder>>, TierError> {
        Embedder::open_pinned(settings, data_dir, &Pin::of(settings), Policy::standard())
    }

    /// [`Embedder::open`] under a stated pin and retry rule. The pin is a parameter because a
    /// test serves its own bytes from its own socket, and there is no other way to exercise a
    /// fetch without reaching the network.
    pub fn open_pinned(
        settings: &Settings,
        data_dir: &Path,
        pin: &Pin,
        policy: Policy,
    ) -> Result<Option<Arc<Embedder>>, TierError> {
        let mut said: Vec<String> = Vec::new();
        let engine = match &settings.mode {
            Mode::Off => return Ok(None),
            Mode::Internal { model } => match model {
                Some(dir) => {
                    Engine::Internal(Box::new(Model::open(dir).map_err(TierError::Model)?))
                }
                // A box that may not reach the network is not fetched for: the built-in model
                // is then whatever is on disk, and the old refusal is what it gets.
                None if settings.download => {
                    let dir = builtin::ensure(pin, data_dir, policy, &mut |line| {
                        said.push(line.to_string())
                    })
                    .map_err(TierError::Download)?;
                    Engine::Internal(Box::new(
                        Model::open_named(&dir, BUILTIN_MODEL).map_err(TierError::Model)?,
                    ))
                }
                // A box that may not reach the network is not fetched for, but the pin is still
                // the pin: the directory it names is the one looked in, and the old refusal is
                // what a box with nothing there gets.
                None => Engine::Internal(Box::new(
                    Model::open_named(&pin.dir(data_dir), BUILTIN_MODEL)
                        .map_err(TierError::Model)?,
                )),
            },
            Mode::Remote { url, model } => {
                Engine::Remote(Remote::open(url, model).map_err(TierError::Remote)?)
            }
        };
        let cache = Cache::open_in(data_dir)?;
        Ok(Some(Embedder::assemble(engine, cache, said)))
    }

    /// An embedder over an engine and a cache the caller already holds, which is what a test
    /// needs.
    pub fn with(engine: Engine, cache: Cache) -> Arc<Embedder> {
        Embedder::assemble(engine, cache, Vec::new())
    }

    fn assemble(engine: Engine, cache: Cache, said: Vec<String>) -> Arc<Embedder> {
        let announcement = (!said.is_empty()).then(|| said.join("\n"));
        Arc::new(Embedder {
            engine: EngineLock::new(engine),
            cache: Mutex::new(cache),
            fault: Mutex::new(None),
            announcement: Mutex::new(announcement),
            embedded: AtomicU64::new(0),
            lookups: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            memo: Mutex::new(std::collections::VecDeque::new()),
            last: Mutex::new(None),
            spent_at: Mutex::new(std::collections::VecDeque::new()),
        })
    }

    /// What this embedder has spent so far, for a test or a log that wants a round's cost.
    pub fn spent(&self) -> Spent {
        Spent {
            embedded: self.embedded.load(Ordering::Relaxed),
            lookups: self.lookups.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
        }
    }

    /// The engine's name, which is what a disclosure and a cache row are keyed by.
    pub fn model(&self) -> String {
        self.engine.name()
    }

    pub fn kind(&self) -> &'static str {
        self.engine.kind()
    }

    /// The first failure, once. Callers fall back to the lexical score on any failure, so saying
    /// every one is noise and saying none hides a wrong URL. Whichever caller asks first says it.
    pub fn take_fault(&self) -> Option<String> {
        self.fault.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// What the box said before it fetched the built-in model, handed out once: the
    /// download happens once, so the sentence that says so is not repeated.
    pub fn take_announcement(&self) -> Option<String> {
        self.announcement
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// One vector for a text - the round's query - from the cache when it has been seen
    /// before, which for a prompt is almost never and for an unchanged note is almost always.
    pub fn vector(&self, text: &str, now_ms: i64) -> Result<Vec<f32>, TierError> {
        let (vector, _) = self.embed_or_get(text, now_ms)?;
        Ok(vector)
    }

    /// Every document's vector, keyed as the caller gave them, and how much the cache already held.
    /// Only unseen text is embedded, `BATCH` at a time, since an endpoint's `input` is an array.
    pub fn vectors(
        &self,
        documents: impl IntoIterator<Item = (String, String)>,
        now_ms: i64,
    ) -> Result<(Vec<(String, Vec<f32>)>, Filled), TierError> {
        let docs: Vec<(String, String)> = documents.into_iter().collect();
        let name = self.engine.name();
        let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(docs.len());
        let mut misses: Vec<usize> = Vec::new();
        for (i, (_, text)) in docs.iter().enumerate() {
            self.lookups.fetch_add(1, Ordering::Relaxed);
            match self.cache().get(&name, text, now_ms) {
                Ok(Some(v)) => vectors.push(Some(v)),
                Ok(None) => {
                    vectors.push(None);
                    misses.push(i);
                }
                Err(e) => return Err(self.fail(EmbedOrCacheError::Cache(e))),
            }
        }
        for chunk in misses.chunks(BATCH) {
            let texts: Vec<&str> = chunk.iter().map(|&i| docs[i].1.as_str()).collect();
            // Asked with no lock held unless the engine is the in-process model: the one cache
            // connection is taken again for the writes below, and never across this call.
            self.embedded
                .fetch_add(texts.len() as u64, Ordering::Relaxed);
            self.bytes.fetch_add(
                texts.iter().map(|t| t.len() as u64).sum(),
                Ordering::Relaxed,
            );
            let embedded = self
                .engine
                .embed_many(&texts)
                .map_err(|e| self.fail(EmbedOrCacheError::Embed(e.to_string())))?;
            if embedded.len() != texts.len() {
                return Err(self.fail(EmbedOrCacheError::Embed(format!(
                    "{} texts went out and {} vectors came back",
                    texts.len(),
                    embedded.len()
                ))));
            }
            let cache = self.cache();
            for (&i, vector) in chunk.iter().zip(embedded) {
                let vector = normalise(vector);
                cache
                    .put(&name, &docs[i].1, &vector, now_ms)
                    .map_err(|e| self.fail(EmbedOrCacheError::Cache(e)))?;
                vectors[i] = Some(vector);
            }
        }
        let done = Filled {
            embedded: misses.len(),
            cached: docs.len() - misses.len(),
        };
        let rows = docs
            .into_iter()
            .zip(vectors)
            .map(|((key, _), v)| (key, v.expect("every text was either cached or embedded")))
            .collect();
        Ok((rows, done))
    }

    fn embed_or_get(&self, text: &str, now_ms: i64) -> Result<(Vec<f32>, Hit), TierError> {
        let name = self.engine.name();
        let hash = crate::cache::key(text);
        if let Some(v) = self.remembered(&hash) {
            return Ok((v, Hit::Cached));
        }
        // Two visits to the cache with the embed between them rather than `Cache::embed_or_get`,
        // which would hold the box's one connection for the whole of a request to an endpoint.
        self.lookups.fetch_add(1, Ordering::Relaxed);
        match self.cache().get(&name, text, now_ms) {
            Ok(Some(v)) => {
                self.remember(hash, &v);
                return Ok((v, Hit::Cached));
            }
            Ok(None) => {}
            Err(e) => return Err(self.fail(EmbedOrCacheError::Cache(e))),
        }
        let words = word_set(text);
        if words.len() >= NEAR_WORDS {
            let last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((seen, v)) = last.as_ref()
                && seen.len() >= NEAR_WORDS
                && jaccard(seen, &words) >= NEAR
            {
                return Ok((v.clone(), Hit::Cached));
            }
        }
        self.spend_query_budget()?;
        self.embedded.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(text.len() as u64, Ordering::Relaxed);
        let vector = self
            .engine
            .embed(text)
            .map(normalise)
            .map_err(|e| self.fail(EmbedOrCacheError::Embed(e.to_string())))?;
        self.cache()
            .put(&name, text, &vector, now_ms)
            .map_err(|e| self.fail(EmbedOrCacheError::Cache(e)))?;
        self.remember(hash, &vector);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((words, vector.clone()));
        Ok((vector, Hit::Embedded))
    }

    /// One query embed against the minute's budget, or the refusal that names it.
    fn spend_query_budget(&self) -> Result<(), TierError> {
        let now = std::time::Instant::now();
        let mut at = self.spent_at.lock().unwrap_or_else(|e| e.into_inner());
        while at
            .front()
            .is_some_and(|t| now.duration_since(*t) >= std::time::Duration::from_secs(60))
        {
            at.pop_front();
        }
        if at.len() >= QUERY_BUDGET {
            drop(at);
            return Err(self.fail(EmbedOrCacheError::Embed(format!(
                "the embedding budget of {QUERY_BUDGET} queries per minute is spent; \
                 scoring on words alone until it refills"
            ))));
        }
        at.push_back(now);
        Ok(())
    }

    fn remembered(&self, hash: &str) -> Option<Vec<f32>> {
        let memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        memo.iter().find(|(h, _)| h == hash).map(|(_, v)| v.clone())
    }

    fn remember(&self, hash: String, vector: &[f32]) {
        let mut memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        if memo.len() >= QUERY_MEMO {
            memo.pop_front();
        }
        memo.push_back((hash, vector.to_vec()));
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Remember why this call could not answer, keeping the first reason of the box, and hand
    /// the caller the narrow variant it can act on.
    fn fail(&self, e: EmbedOrCacheError) -> TierError {
        {
            let mut fault = self.fault.lock().unwrap_or_else(|p| p.into_inner());
            if fault.is_none() {
                *fault = Some(e.to_string());
            }
        }
        match e {
            EmbedOrCacheError::Cache(c) => TierError::Cache(c),
            EmbedOrCacheError::Embed(m) => TierError::Embed(m),
        }
    }
}

/// One caller's second tier: the box's embedder, and this caller's corpus's vectors in
/// memory. Two callers share the model and never score each other's documents.
pub struct Tier {
    embedder: Arc<Embedder>,
    /// `&self` because a tool is invoked through it and a round refills a corpus it did not
    /// create; a read is a cosine over the rows already there.
    index: RwLock<Index>,
    /// Hash of the documents the index was built from: the same documents again are the same
    /// vectors, so a fill that matches it touches neither the cache nor the engine.
    filled: Mutex<Option<String>>,
    /// The round's one subject text, set by the loop that owns the round. When present every
    /// score through this tier embeds it instead of its own text, and the embedder's memo makes
    /// that one embedding for all callers.
    round: RwLock<Option<String>>,
}

impl std::fmt::Debug for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Tier({}, {} document(s))",
            self.embedder.model(),
            self.read().len()
        )
    }
}

impl Tier {
    /// Build the tier the mode asks for, or `None` when the mode is `off` - which is the
    /// whole of "lexical only".
    pub fn open(settings: &Settings, data_dir: &Path) -> Result<Option<Tier>, TierError> {
        Ok(Embedder::open(settings, data_dir)?.map(Tier::over))
    }

    /// A tier over the box's embedder, with no documents in it yet.
    pub fn over(embedder: Arc<Embedder>) -> Tier {
        Tier {
            embedder,
            index: RwLock::new(Index::default()),
            filled: Mutex::new(None),
            round: RwLock::new(None),
        }
    }

    /// A tier over an engine and a cache the caller already holds.
    pub fn with(engine: Engine, cache: Cache) -> Tier {
        Tier::over(Embedder::with(engine, cache))
    }

    /// The box's embedder, for the caller that says the first failure.
    pub fn embedder(&self) -> &Arc<Embedder> {
        &self.embedder
    }

    pub fn model(&self) -> String {
        self.embedder.model()
    }

    pub fn kind(&self) -> &'static str {
        self.embedder.kind()
    }

    /// The first failure, once, from the box's embedder - which every caller shares, so it
    /// is said once for the box and not once for each of them.
    pub fn take_fault(&self) -> Option<String> {
        self.embedder.take_fault()
    }

    /// Embed every changed document and put its vector in memory. Only unseen text costs an embedding.
    /// A fill that cannot answer empties the tier: stale vectors would score edited text, and a
    /// failure must mean the words alone.
    pub fn fill(
        &self,
        documents: impl IntoIterator<Item = (String, String)>,
        now_ms: i64,
    ) -> Result<Filled, TierError> {
        let docs: Vec<(String, String)> = documents.into_iter().collect();
        let print = fingerprint(&docs);
        let mut filled = self.filled.lock().unwrap_or_else(|e| e.into_inner());
        if filled.as_deref() == Some(print.as_str()) {
            return Ok(Filled {
                embedded: 0,
                cached: docs.len(),
            });
        }
        match self.embedder.vectors(docs, now_ms) {
            Ok((rows, done)) => {
                *self.write() = Index::new(rows);
                *filled = Some(print);
                Ok(done)
            }
            Err(e) => {
                *self.write() = Index::default();
                *filled = None;
                Err(e)
            }
        }
    }

    /// Share one subject text for the round, or clear it with `None`.
    pub fn set_round(&self, text: Option<String>) {
        *self.round.write().unwrap_or_else(|e| e.into_inner()) = text;
    }

    /// The round's shared subject text, if the loop set one.
    pub fn round(&self) -> Option<String> {
        self.round.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn query(&self, text: &str, now_ms: i64) -> Result<Vec<f32>, TierError> {
        self.embedder.vector(text, now_ms)
    }

    /// How alike a document and a query are, `None` when the document was never embedded.
    pub fn cosine(&self, key: &str, query: &[f32]) -> Option<f32> {
        self.read().cosine_to(key, query)
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
            .read()
            .nearest(&query, most)
            .into_iter()
            .map(|(k, s)| (k.to_string(), s))
            .collect())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Index> {
        self.index.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Index> {
        self.index.write().unwrap_or_else(|e| e.into_inner())
    }
}

fn word_set(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    let both = a.intersection(b).count() as f64;
    let any = a.union(b).count() as f64;
    if any == 0.0 { 0.0 } else { both / any }
}

/// A hash of keys and texts in order, with lengths so no two lists spell the same bytes.
fn fingerprint(docs: &[(String, String)]) -> String {
    let mut all = String::new();
    for (key, text) in docs {
        all.push_str(&format!("{}:{key}{}:{text}", key.len(), text.len()));
    }
    crate::cache::key(&all)
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
    use crate::testing as fixture;
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

    /// A loopback endpoint that answers one vector per text it was asked for, and remembers how
    /// many texts each request carried: the only way to see the size of `input` from outside.
    fn batching_endpoint() -> (String, Arc<Mutex<Vec<usize>>>) {
        use std::io::{BufRead, BufReader, Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!(
            "http://127.0.0.1:{}",
            listener.local_addr().expect("addr").port()
        );
        let seen: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let mut reader = BufReader::new(conn.try_clone().expect("clone"));
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(n) = lower.strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                let asked: serde_json::Value = serde_json::from_slice(&body).expect("json");
                let n = asked["input"].as_array().map(|a| a.len()).unwrap_or(0);
                log.lock().expect("log").push(n);
                let data: Vec<serde_json::Value> = (0..n)
                    .map(|i| serde_json::json!({ "index": i, "embedding": [1.0, 0.0] }))
                    .collect();
                let body = serde_json::json!({ "data": data }).to_string();
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.write_all(answer.as_bytes());
                let _ = conn.flush();
            }
        });
        (url, seen)
    }

    /// A fill asks the endpoint once per batch, not once per note. `Remote::ask` carries an
    /// `input` array and the tier sent it one element at a time, so a 300-note corpus was 300 round
    /// trips with the box's one embedder held for all of them.
    #[test]
    fn a_remote_fill_asks_once_for_a_whole_batch() {
        let (url, asked) = batching_endpoint();
        let data = Dir::new("remote-batch");
        let cache = Cache::open_in(data.path()).expect("cache");
        let engine = Engine::Remote(Remote::open(&url, "bge-m3").expect("remote"));
        let tier = Tier::with(engine, cache);

        let three: Vec<(String, String)> = (0..3)
            .map(|i| (format!("n{i}"), format!("text {i}")))
            .collect();
        let filled = tier.fill(three, now()).expect("filled");
        assert_eq!((filled.embedded, filled.cached), (3, 0));
        assert_eq!(
            asked.lock().expect("log").clone(),
            vec![3],
            "three misses are one request"
        );

        // And the batch is bounded: forty more misses are 32 and 8, in that order.
        let forty: Vec<(String, String)> = (0..40)
            .map(|i| (format!("m{i}"), format!("other text {i}")))
            .collect();
        let filled = tier.fill(forty, now()).expect("filled");
        assert_eq!(filled.embedded, 40);
        assert_eq!(
            asked.lock().expect("log").clone(),
            vec![3, 32, 8],
            "a batch is 32 texts and the rest follow"
        );
    }

    /// A loopback endpoint that holds its answer for a second and a half, and only for a text that
    /// says `slow`: a warm-up fill must not pay it.
    fn slow_endpoint() -> String {
        use std::io::{BufRead, BufReader, Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!(
            "http://127.0.0.1:{}",
            listener.local_addr().expect("addr").port()
        );
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let mut reader = BufReader::new(conn.try_clone().expect("clone"));
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(n) = lower.strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                let asked: serde_json::Value = serde_json::from_slice(&body).expect("json");
                let n = asked["input"].as_array().map(|a| a.len()).unwrap_or(0);
                let slow = body.windows(4).any(|w| w == b"slow");
                if slow {
                    std::thread::sleep(std::time::Duration::from_millis(1_500));
                }
                let data: Vec<serde_json::Value> = (0..n)
                    .map(|i| serde_json::json!({ "index": i, "embedding": [1.0, 0.0] }))
                    .collect();
                let body = serde_json::json!({ "data": data }).to_string();
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.write_all(answer.as_bytes());
                let _ = conn.flush();
            }
        });
        url
    }

    /// One caller's slow fill must not stop another caller's scoring. The box has one cache lock;
    /// held across the endpoint's request it makes a fill's wait every caller's, down to the HTTP
    /// read's 180 s.
    #[test]
    fn a_slow_fill_does_not_hold_another_callers_cached_read() {
        let url = slow_endpoint();
        let data = Dir::new("remote-slow");
        let cache = Cache::open_in(data.path()).expect("cache");
        let engine = Engine::Remote(Remote::open(&url, "bge-m3").expect("remote"));
        let embedder = Embedder::with(engine, cache);

        // Warm the cache, so the second caller needs no endpoint at all.
        Tier::over(embedder.clone())
            .fill(vec![("n0".into(), "already here".into())], now())
            .expect("the warm fill answers");

        // A fill that says `slow`, inside the endpoint for a second and a half.
        let slow = {
            let tier = Tier::over(embedder.clone());
            std::thread::spawn(move || {
                tier.fill(vec![("s0".into(), "slow text".into())], now())
                    .map(|f| f.embedded)
            })
        };
        // Long enough for that thread to be inside the request.
        std::thread::sleep(std::time::Duration::from_millis(400));

        let started = std::time::Instant::now();
        let filled = Tier::over(embedder.clone())
            .fill(vec![("n0".into(), "already here".into())], now())
            .expect("a cached read needs no endpoint");
        let took = started.elapsed();
        assert_eq!((filled.embedded, filled.cached), (0, 1));
        assert!(
            took < std::time::Duration::from_millis(800),
            "a cached read waited {took:?} behind another caller's fill"
        );
        assert_eq!(
            slow.join()
                .expect("the slow fill ended")
                .expect("one vector"),
            1
        );
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

    /// A box that must not reach the network is not fetched for. With the download off and
    /// no model on disk, what it gets is the old refusal, naming the directory to fill.
    #[test]
    fn a_download_that_is_turned_off_keeps_the_old_refusal() {
        let data = crate::fake::Dir::new("no-download");
        let settings = Settings::from_toml("[embeddings]\nmode = \"internal\"\ndownload = false\n")
            .expect("settings");
        let err = match Embedder::open(&settings, data.path()) {
            Ok(_) => panic!("a box with no model and no download has no embedder"),
            Err(e) => e,
        };
        assert!(
            matches!(err, TierError::Model(ModelError::Missing { .. })),
            "{err}"
        );
        assert!(err.to_string().contains(BUILTIN_MODEL), "{err}");
    }

    /// A box that may not reach the network still opens a model it already has: the policy is
    /// about fetching, and never about reading.
    #[test]
    fn a_model_already_on_disk_opens_with_the_download_turned_off() {
        let data = crate::fake::Dir::new("cached-no-download");
        crate::testing::tiny_bert(&crate::model::builtin_dir(data.path())).expect("fixture model");
        let settings = Settings::from_toml("[embeddings]\nmode = \"internal\"\ndownload = false\n")
            .expect("settings");
        let embedder = Embedder::open(&settings, data.path())
            .expect("no fault")
            .expect("an embedder");
        assert_eq!(embedder.kind(), "internal");
        assert_eq!(embedder.take_announcement(), None, "nothing was fetched");
    }

    /// The fetch a box makes on first use, through the tier. The model comes back whole,
    /// the sentence about it is handed out once, and scoring uses the model, not nothing.
    #[test]
    fn a_fetched_model_scores_and_says_what_it_fetched_once() {
        let files = crate::fake::Checkpoint::new();
        let fake = crate::fake::Fake::new(files.route());
        let pin = files.pin(&fake.base());
        let data = crate::fake::Dir::new("fetched-tier");
        let settings =
            Settings::from_toml("[embeddings]\nmode = \"internal\"\n").expect("settings");
        let embedder = Embedder::open_pinned(&settings, data.path(), &pin, Policy::none())
            .expect("no fault")
            .expect("an embedder");

        let said = embedder.take_announcement().expect("the sentence");
        assert!(said.contains(&fake.base()), "{said}");
        assert!(said.contains("once"), "{said}");
        assert_eq!(embedder.take_announcement(), None, "once, not every open");

        // The engine is the internal one, named for the cache by the checkpoint and not by
        // the directory the revision happens to sit in.
        assert_eq!(embedder.kind(), "internal");
        assert_eq!(embedder.model(), BUILTIN_MODEL);
        let vector = embedder
            .vector("alpha beta gamma", now())
            .expect("a vector");
        assert_eq!(vector.len(), fixture::HIDDEN);
    }

    /// A fetch that fails leaves the tier off, with the reason and the URL. It never stands
    /// in for a score - the caller falls back to its own words - which is why the one sentence
    /// has to say what went wrong and where it looked.
    #[test]
    fn a_failed_download_is_a_fault_with_the_url_in_it() {
        let files = crate::fake::Checkpoint::new();
        let mut served = files.files.clone();
        // The server's config.json is not the file the pin was taken over.
        let mut altered = served["config.json"].clone();
        altered[0] ^= 0xff;
        served.insert("config.json", altered);
        let fake = crate::fake::Fake::new(move |path| {
            match served.get(path.rsplit('/').next().unwrap_or("")) {
                Some(bytes) => crate::fake::answer(bytes),
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
            }
        });
        let pin = files.pin(&fake.base());
        let data = crate::fake::Dir::new("failed-tier");
        let settings =
            Settings::from_toml("[embeddings]\nmode = \"internal\"\n").expect("settings");
        let err = match Embedder::open_pinned(&settings, data.path(), &pin, Policy::none()) {
            Ok(_) => panic!("a hash mismatch is not an embedder"),
            Err(e) => e,
        };
        assert!(matches!(err, TierError::Download(_)), "{err}");
        let said = err.to_string();
        assert!(said.contains(&pin.url("config.json")), "{said}");
        assert!(said.contains("sha-256"), "{said}");
    }

    /// Filling a corpus embeds each document once; filling it again from a fresh tier on
    /// the same data directory costs no embedding at all.
    #[test]
    fn a_second_fill_reads_every_vector_from_the_cache() {
        let (model_dir, data, tier) = tier("fill");
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
        let again = Tier::open(&settings, data.path()).unwrap().expect("a tier");
        assert_eq!(
            again.fill(docs(), now()).unwrap(),
            Filled {
                embedded: 0,
                cached: 3
            }
        );
    }

    /// The same documents again are not looked up at all, an edited one is, and a query asked
    /// twice is read from memory.
    #[test]
    fn a_fill_of_unchanged_documents_and_a_repeated_query_cost_nothing() {
        let (_m, _d, tier) = tier("fingerprint");
        tier.fill(docs(), now()).unwrap();
        let spent = tier.embedder().spent();
        let again = tier.fill(docs(), now()).unwrap();
        assert_eq!((again.embedded, again.cached), (0, 3));
        assert_eq!(tier.embedder().spent(), spent, "no lookup, no embed");
        let mut edited = docs().into_iter().collect::<Vec<_>>();
        edited[0].1.push_str(" kappa lambda");
        let changed = tier.fill(edited, now()).unwrap();
        assert_eq!((changed.embedded, changed.cached), (1, 2));
        let before = tier.embedder().spent();
        let first = tier.query("kappa nu", now()).unwrap();
        let second = tier.query("kappa nu", now()).unwrap();
        let after = tier.embedder().spent();
        assert_eq!(first, second);
        assert_eq!(after.embedded - before.embedded, 1);
        assert_eq!(after.lookups - before.lookups, 1, "the repeat is memory");
    }

    /// Past the minute's budget a query is refused in words that name the number, and the fault
    /// is there once for the caller that says it.
    #[test]
    fn queries_past_the_budget_are_refused_naming_it() {
        let (_m, _d, tier) = tier("budget");
        for i in 0..QUERY_BUDGET {
            tier.query(&format!("alpha {i}"), now())
                .expect("within budget");
        }
        let refused = tier
            .query("alpha beta over", now())
            .unwrap_err()
            .to_string();
        assert!(refused.contains("30 queries per minute"), "{refused}");
        assert!(tier.take_fault().is_some());
    }

    /// A long text that moves by one word reuses the last vector.
    #[test]
    fn a_nearly_unchanged_subject_reuses_the_last_vector() {
        let (_m, _d, tier) = tier("near");
        let base = "alpha beta gamma delta kappa lambda mu nu one two three four five six";
        let first = tier.query(base, now()).unwrap();
        let before = tier.embedder().spent();
        let second = tier.query(&format!("{base} seven"), now()).unwrap();
        assert_eq!(first, second);
        assert_eq!(tier.embedder().spent(), before, "no embed, no lookup");
    }

    /// The ranking a caller uses: the paraphrase of a note comes first and the unrelated
    /// note last.
    #[test]
    fn ranking_puts_the_paraphrase_above_the_unrelated_note() {
        let (_m, _d, tier) = tier("rank");
        tier.fill(docs(), now()).unwrap();
        let ranked = tier.rank("alpha beta gamma", now(), 3).unwrap();
        assert_eq!(ranked[0].0, "note/paraphrase", "{ranked:?}");
        assert_eq!(ranked[2].0, "note/unrelated", "{ranked:?}");
    }

    /// A document that was never filled is `None` rather than zero, so a caller can keep
    /// "not embedded" apart from "not alike".
    #[test]
    fn a_document_outside_the_index_has_no_score() {
        let (_m, _d, tier) = tier("absent");
        tier.fill(docs(), now()).unwrap();
        let query = tier.query("alpha", now()).unwrap();
        assert!(tier.cosine("note/paraphrase", &query).is_some());
        assert_eq!(tier.cosine("note/missing", &query), None);
    }

    /// The tier falls back on every failure and is said once: the first reason is kept,
    /// handed over, and never repeated.
    #[test]
    fn a_failure_is_said_once_and_kept() {
        let data = Dir::new("fault");
        let settings =
            Settings::from_toml("[embeddings]\nmode = \"http://127.0.0.1:1\"\nmodel = \"m\"\n")
                .unwrap();
        let tier = Tier::open(&settings, data.path()).unwrap().expect("a tier");
        assert!(tier.take_fault().is_none());
        assert!(tier.query("alpha", now()).is_err());
        assert!(tier.query("beta", now()).is_err());
        let said = tier.take_fault().expect("the first failure is said");
        assert!(said.contains("127.0.0.1:1"), "{said}");
        assert_eq!(tier.take_fault(), None);
    }

    /// One embedder, two callers. Two tiers over one embedder share the vectors it makes -
    /// a text either of them asks for is embedded once - and share the corpus's absence: a
    /// document is in the index of the caller that filled it and in no other.
    #[test]
    fn one_embedder_serves_two_tiers_and_keeps_their_corpora_apart() {
        let data = Dir::new("shared");
        let groups: &[&[&str]] = &[&["alpha", "omega"], &["beta", "kappa"]];
        let cache = Cache::open_in(data.path()).unwrap();
        let embedder = Embedder::with(Engine::Planted(Planted::new(groups)), cache);
        let notes = Tier::over(embedder.clone());
        let skills = Tier::over(embedder.clone());
        notes
            .fill([("a/note".to_string(), "alpha".to_string())], now())
            .unwrap();
        let query = skills.query("omega", now()).unwrap();
        assert_eq!(notes.cosine("a/note", &query), Some(1.0));
        // The same embedder, and still another caller's corpus: a skill is not a note.
        assert_eq!(skills.cosine("a/note", &query), None);
    }

    /// The failure is said once for the BOX, not once per caller: whichever tier trips over
    /// it first takes the sentence and the other is silent.
    #[test]
    fn two_tiers_over_one_embedder_say_a_failure_once() {
        let data = Dir::new("shared-fault");
        let settings =
            Settings::from_toml("[embeddings]\nmode = \"http://127.0.0.1:1\"\nmodel = \"m\"\n")
                .unwrap();
        let embedder = Embedder::open(&settings, data.path())
            .unwrap()
            .expect("an embedder");
        let first = Tier::over(embedder.clone());
        let second = Tier::over(embedder);
        assert!(first.query("alpha", now()).is_err());
        assert!(second.query("beta", now()).is_err());
        assert!(first.take_fault().is_some());
        assert_eq!(second.take_fault(), None);
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
