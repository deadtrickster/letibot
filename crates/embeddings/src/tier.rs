//! The tier as a caller sees it: one question in, one cosine out.
//!
//! A caller hands over its documents once, when they change, and then asks about one
//! text a round. Everything between - which engine, whether the vector is on disk, what
//! the corpus's vectors are - is this module's, so a caller's whole second tier is one
//! optional `&Tier` and one floor.

use std::path::Path;
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

    fn embed(&self, text: &str) -> Result<Vec<f32>, TierError> {
        let mut vectors = self.embed_many(&[text])?;
        vectors.pop().ok_or_else(|| TierError::Empty(text.len()))
    }

    /// One vector per text, in the order given. A remote engine asks once for the whole slice,
    /// which is what its `input` array is for; the local engines loop inside their own crate.
    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, TierError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        Ok(match self {
            Engine::Internal(m) => m.embed(texts).map_err(TierError::Model)?,
            Engine::Remote(r) => r.embed(texts).map_err(TierError::Remote)?,
            Engine::Planted(p) => texts.iter().map(|t| p.vector(t)).collect(),
        })
    }
}

/// How many texts one request carries. The endpoint's `input` is an array, and a corpus of 300
/// notes is ten round trips rather than three hundred.
const BATCH: usize = 32;

/// **A tier whose meaning is a table.** Each group is one dimension and each word in the
/// group sets it, so two texts that share a group and no word score as one - which is what a
/// caller's test needs to say *a paraphrase clears this floor*, with no model and no file.
///
/// Built by [`crate::testing::planted`] and by nothing else; it is here rather than behind a
/// feature so the arm that answers it is compiled wherever the enum is.
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

/// **The one embedder a box has.** The engine and the cache, and nothing about what is
/// being scored: the corpus's vectors belong to the caller that holds them, so four callers
/// on one box load one model between them and pay for a text once.
pub struct Embedder {
    /// One lock, because the cache is a single connection: a query is milliseconds and there
    /// is one per round per caller, which is not worth a second model.
    inner: Mutex<Inner>,
    /// The first failure this box, held until a caller says it: every caller falls back to
    /// the lexical score on a failure, and a line per round would be worse than the silence.
    fault: Mutex<Option<String>>,
    /// What the box said before it fetched the built-in model, handed out once.
    announcement: Mutex<Option<String>>,
}

struct Inner {
    engine: Engine,
    cache: Cache,
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
            inner: Mutex::new(Inner { engine, cache }),
            fault: Mutex::new(None),
            announcement: Mutex::new(announcement),
        })
    }

    /// The engine's name, which is what a disclosure and a cache row are keyed by.
    pub fn model(&self) -> String {
        self.lock().engine.name().to_string()
    }

    pub fn kind(&self) -> &'static str {
        self.lock().engine.kind()
    }

    /// **The first failure, once.** Every caller falls back to the lexical score whenever the
    /// embedder cannot answer, so a caller that said every failure would say it every round; a
    /// caller that said nothing would hide a wrong URL for a whole session. This is the
    /// sentence, handed out exactly once, and whichever caller asks first says it.
    pub fn take_fault(&self) -> Option<String> {
        self.fault.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// **What the box said before it fetched the built-in model**, handed out once: the
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

    /// Every document's vector, keyed as the caller gave them, and how much of that the box
    /// had already paid for. Only a text the cache has never seen costs an embedding, and the
    /// misses go out `BATCH` at a time: an endpoint's `input` is an array, and a 300-note corpus
    /// is ten requests rather than three hundred.
    pub fn vectors(
        &self,
        documents: impl IntoIterator<Item = (String, String)>,
        now_ms: i64,
    ) -> Result<(Vec<(String, Vec<f32>)>, Filled), TierError> {
        let docs: Vec<(String, String)> = documents.into_iter().collect();
        let lock = self.lock();
        let name = lock.engine.name().to_string();
        let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(docs.len());
        let mut misses: Vec<usize> = Vec::new();
        for (i, (_, text)) in docs.iter().enumerate() {
            match lock.cache.get(&name, text, now_ms) {
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
            let embedded = lock
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
            for (&i, vector) in chunk.iter().zip(embedded) {
                let vector = normalise(vector);
                lock.cache
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
        let inner = self.lock();
        let name = inner.engine.name().to_string();
        let embedded = |t: &str| {
            inner
                .engine
                .embed(t)
                .map(normalise)
                .map_err(|e| e.to_string())
        };
        inner
            .cache
            .embed_or_get(&name, text, now_ms, embedded)
            .map_err(|e| self.fail(e))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
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

    /// **The first failure, once**, from the box's embedder - which every caller shares, so it
    /// is said once for the box and not once for each of them.
    pub fn take_fault(&self) -> Option<String> {
        self.embedder.take_fault()
    }

    /// Embed every document that has changed and put its vector in memory. Only a text the
    /// cache has never seen costs an embedding, so a restart pays for the corpus once.
    ///
    /// A fill that cannot answer empties the tier rather than leaving the last corpus's vectors
    /// behind: a stale vector would score a text that has since been edited, and the contract a
    /// caller relies on is that a failure is the words alone.
    pub fn fill(
        &self,
        documents: impl IntoIterator<Item = (String, String)>,
        now_ms: i64,
    ) -> Result<Filled, TierError> {
        match self.embedder.vectors(documents, now_ms) {
            Ok((rows, done)) => {
                *self.write() = Index::new(rows);
                Ok(done)
            }
            Err(e) => {
                *self.write() = Index::default();
                Err(e)
            }
        }
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

    /// **A fill asks the endpoint once per batch, not once per note.** `Remote::ask` carries an
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

    /// Mode off is no tier at all, which is the whole of "lexical only".
    #[test]
    fn mode_off_opens_no_tier() {
        assert!(
            Tier::open(&Settings::default(), Path::new("."))
                .unwrap()
                .is_none()
        );
    }

    /// **A box that must not reach the network is not fetched for.** With the download off and
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

    /// **The fetch a box makes on first use, through the tier.** The model comes back whole,
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

    /// **A fetch that fails leaves the tier off, with the reason and the URL.** It never stands
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

    /// **One embedder, two callers.** Two tiers over one embedder share the vectors it makes -
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
