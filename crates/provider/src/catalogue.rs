//! The model catalogue: limits and prices read from a file, never remembered.
//! Sources, first that parses wins: `LETIBOT_MODELS_JSON`, this harness's cache, opencode's cache,
//! the vendored snapshot floor. opencode is read as a courtesy, never depended on.
//!
//! The cache is [`crate::refresh`]'s to write - the daemon fetches models.dev once a day and trims
//! it to the providers the presets name - and `scripts/refresh-models-snapshot.sh` can write it by
//! hand for a box that wants a file now. Nothing here writes: a reader that could also write is a
//! reader whose answer changes depending on who asked first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One model's published limits and prices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelFacts {
    /// Total context, in tokens. The number compaction is planned against.
    pub context: u64,
    /// The most it will generate in one turn.
    pub output: u64,
    /// USD per million, as [`crate::presets::Prices`] wants them. `None` for a
    /// model the catalogue prices nowhere - some are free, some are unlisted, and
    /// those are different from zero.
    pub prices: Option<crate::presets::Prices>,
    /// What it accepts as input, from models.dev's `modalities.input`. `None` when the row
    /// says nothing, which is not the same as text only - see [`Catalogue::takes_images`].
    pub inputs: Option<Inputs>,
    /// Whether this model's output is text, from `modalities.output`: a video or image model is
    /// not one a session can run on. `None` when the row says nothing.
    pub output_is_text: Option<bool>,
    /// models.dev's `tool_call`. A model that cannot call a tool is not this harness's model.
    pub tool_call: Option<bool>,
    /// models.dev's `release_date` as `YYYYMMDD`, so it orders as a number and stays `Copy`: which
    /// model is the new one, which is the question [`Catalogue::pick`] answers rather than a
    /// constant written down here.
    pub released: Option<u32>,
}

/// `20261123` back as the `2026-11-23` the catalogue wrote.
fn date_words(v: u32) -> String {
    format!("{:04}-{:02}-{:02}", v / 10_000, (v / 100) % 100, v % 100)
}

/// `YYYY-MM-DD` as `YYYYMMDD`. A date this cannot read is an absent one, never a zero - a zero
/// would sort first and be read as the oldest model there is.
fn parsed_date(text: &str) -> Option<u32> {
    let mut parts = text.trim().split('-');
    let y: u32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    (parts.next().is_none() && (1..=12).contains(&m) && (1..=31).contains(&d))
        .then_some(y * 10_000 + m * 100 + d)
}

impl ModelFacts {
    /// The line the model card draws under a row: the window and the rate per million, the two
    /// numbers a reader compares between two models of one provider.
    pub fn card_line(&self) -> String {
        let window = format!("{} window", tokens(self.context));
        match self.prices {
            Some(p) => format!(
                "{window} · ${} in / ${} out per M",
                per_million(p.input),
                per_million(p.output)
            ),
            // Unpriced is not free: some models are, some are unlisted, and this line cannot say which.
            None => format!("{window} · price unlisted"),
        }
    }

    /// The line for a model no catalogue carries, which the card still offers because the
    /// operator named it. Said rather than left blank: an empty line reads as no window at all.
    pub fn missing_line() -> &'static str {
        "window and price not in the catalogue"
    }
}

/// A token count as a person reads it: `1M`, `393k`, `512`. Rounded down, so a window is never
/// quoted as larger than it is.
pub fn tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// One rate in USD per million tokens: `0.15`, `3`, `0.003`. `Display` and not a fixed number of
/// decimals, because a rate is published to whatever precision the provider chose.
fn per_million(rate: f64) -> String {
    format!("{rate}")
}

/// One model's input modalities as models.dev publishes them. Only the facts this harness can
/// act on are kept: nothing here can send audio or video, so a flag for them would be a switch
/// nothing reads. A document is one this harness CAN send, so `pdf` is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    pub text: bool,
    pub image: bool,
    /// The catalogue lists `pdf` among this model's input modalities.
    pub pdf: bool,
}

#[derive(Debug, Deserialize)]
struct RawProvider {
    #[serde(default)]
    models: BTreeMap<String, RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    #[serde(default)]
    limit: Option<RawLimit>,
    #[serde(default)]
    cost: Option<RawCost>,
    #[serde(default)]
    modalities: Option<RawModalities>,
    #[serde(default)]
    tool_call: Option<bool>,
    #[serde(default)]
    release_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawModalities {
    #[serde(default)]
    input: Vec<String>,
    #[serde(default)]
    output: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawLimit {
    #[serde(default)]
    context: u64,
    #[serde(default)]
    output: u64,
}

#[derive(Debug, Deserialize)]
struct RawCost {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    /// models.dev's name for the cached-read rate. Absent means the provider does
    /// not discount a cache hit, so it costs the full input rate - not zero.
    #[serde(default)]
    cache_read: Option<f64>,
}

/// The catalogue, parsed once.
#[derive(Debug, Default)]
pub struct Catalogue {
    providers: BTreeMap<String, RawProviderFacts>,
    /// Where it was read from, for the disclosure.
    source: Option<Origin>,
}

/// The catalogue a client plans against: the process's current one, re-read when the file behind
/// it changes, or one a test pinned.
///
/// A client used to keep the `Arc` its constructor loaded, so a cache refreshed mid-session
/// reached nothing until the session was rebuilt: windows, prices and the image question stayed
/// the ones from the minute the provider was built. `Current` asks [`Catalogue::load`] at each
/// request instead, which is a `stat` and a map lookup - the parse itself is cached by file.
#[derive(Debug, Clone, Default)]
pub enum Shelf {
    /// Whatever [`Catalogue::load`] answers now.
    #[default]
    Current,
    /// One catalogue, for a test whose answer must not depend on this box's files.
    Pinned(std::sync::Arc<Catalogue>),
}

impl Shelf {
    pub fn pinned(catalogue: std::sync::Arc<Catalogue>) -> Self {
        Shelf::Pinned(catalogue)
    }

    /// The catalogue in force for this request.
    pub fn get(&self) -> std::sync::Arc<Catalogue> {
        match self {
            Shelf::Current => Catalogue::load(),
            Shelf::Pinned(c) => c.clone(),
        }
    }
}

/// Which of the four places a catalogue came from - the fact the disclosure names.
#[derive(Debug)]
enum Origin {
    File(PathBuf),
    /// The vendored snapshot. Carries nothing extra because its date lives beside
    /// it as [`SNAPSHOT_FETCHED`], and the two are regenerated together.
    Snapshot,
}

#[derive(Debug, Default)]
struct RawProviderFacts {
    models: BTreeMap<String, ModelFacts>,
}

/// The operator's explicit pointer, which wins over every other source.
fn override_path() -> Option<PathBuf> {
    std::env::var("LETIBOT_MODELS_JSON").ok().map(PathBuf::from)
}

/// This harness's own cache, the first place `load` looks after the override: ours, written by
/// [`crate::refresh`] (or by `scripts/refresh-models-snapshot.sh`, or anything else that can write
/// a file), so a box with no opencode still has a catalogue somebody meant to put there.
pub fn cache_path() -> Option<PathBuf> {
    let base = match std::env::var("XDG_CACHE_HOME") {
        Ok(x) if !x.is_empty() => PathBuf::from(x),
        _ => Path::new(&std::env::var("HOME").ok()?).join(".cache"),
    };
    Some(base.join("letibot/models.json"))
}

/// Where opencode keeps its copy: read as a courtesy, never depended on, because a box
/// without opencode had no windows at all, silently.
pub fn opencode_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(Path::new(&home).join(".cache/opencode/models.json"))
}

/// The vendored snapshot's provenance, beside the data it describes. The script that
/// writes the file prints this string, so the two stay honest with each other.
pub const SNAPSHOT_FETCHED: &str = "2026-11-24";

/// The floor: every provider a preset can switch to, as models.dev carried them on
/// [`SNAPSHOT_FETCHED`]. Small because this is not a browsing catalogue, only the answer
/// to *a box with nothing else still plans against the right window*.
const SNAPSHOT: &str = include_str!("../data/models-snapshot.json");

/// One provider's default model, and the rung that chose it - [`Catalogue::pick`]'s answer, so a
/// reader can see why the name is what it is instead of trusting a rule they cannot read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub model: String,
    pub why: String,
}

/// Names a vendor ships as an experiment rather than as the model to be on. A default is what a
/// new session starts from, so it may not be a preview of something.
fn experimental(name: &str) -> bool {
    name.split('-').any(|seg| {
        matches!(
            seg.to_ascii_lowercase().as_str(),
            "exp" | "preview" | "beta" | "alpha"
        )
    })
}

/// The name without a date on the end, when the date is `YYYYMMDD` or `YYYY-MM-DD`.
fn undated_name(name: &str) -> Option<&str> {
    let (head, tail) = name.rsplit_once('-')?;
    if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit()) {
        return Some(head);
    }
    if tail.len() != 2 || !tail.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (head2, month) = head.rsplit_once('-')?;
    if month.len() != 2 || !month.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (head3, year) = head2.rsplit_once('-')?;
    (year.len() == 4 && year.chars().all(|c| c.is_ascii_digit())).then_some(head3)
}

/// A dated snapshot of a model this provider also carries undated: the snapshot is the same model
/// and is the one that gets retired, so it is not the name a new session should start on.
fn dated_snapshot(name: &str, carried: &std::collections::BTreeSet<String>) -> bool {
    undated_name(name).is_some_and(|plain| carried.contains(plain))
}

/// Where the defaults this box last resolved are kept, beside the cache: `models.json.defaults`.
/// A refresh that changes what a provider starts on is news, and news needs the previous answer.
pub fn defaults_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".defaults");
    PathBuf::from(name)
}

/// The defaults recorded last time, per catalogue id. An absent or unreadable file is an empty
/// map: a first look has nothing to compare against, which is not a change.
pub fn known_defaults(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(defaults_path(path))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Note what this catalogue resolves each provider to now and answer one line per provider whose
/// default moved: `<provider>: default is now <new> (was <old>)`. The record is written before the
/// lines come back, so a restart says each one once. A failure to write says nothing rather than
/// losing the lines.
pub fn note_defaults(path: &Path) -> Vec<String> {
    let Ok(cat) = Catalogue::read(path) else {
        return Vec::new();
    };
    let was = known_defaults(path);
    let now: BTreeMap<String, String> = crate::refresh::catalogue_ids()
        .into_iter()
        .filter_map(|id| cat.pick(id).map(|p| (id.to_string(), p.model)))
        .collect();
    let mut lines = Vec::new();
    for (id, model) in &now {
        if let Some(old) = was.get(id)
            && old != model
        {
            lines.push(format!("{id}: default is now {model} (was {old})"));
        }
    }
    let text = serde_json::to_string_pretty(&now).unwrap_or_default();
    let mut tmp = defaults_path(path).as_os_str().to_os_string();
    tmp.push(format!(".tmp-{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, defaults_path(path));
    }
    lines
}

/// The cheaper input rate first, and an unpriced model after every priced one: unpriced is not
/// free, and it is not evidence of anything either.
fn cheaper(a: &ModelFacts, b: &ModelFacts) -> std::cmp::Ordering {
    let (x, y) = (
        a.prices.map(|p| p.input).unwrap_or(f64::MAX),
        b.prices.map(|p| p.input).unwrap_or(f64::MAX),
    );
    x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
}

/// A model name's version, and how much is written after it: the two rungs that ask which model is
/// newer and which name is a plain model rather than a variant of one.
fn model_version(name: &str) -> (Vec<u64>, usize) {
    let mut version: Vec<u64> = Vec::new();
    let mut written_after = 0usize;
    let mut seen = false;
    for seg in name.split('-') {
        if !seen {
            if seg.chars().any(|c| c.is_ascii_digit()) {
                version = seg
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                seen = true;
            }
            continue;
        }
        written_after += 1;
    }
    (version, written_after)
}

/// One file's parse, kept until the file changes: the cache behind [`Catalogue::load`],
/// keyed by path, modification time and length, so the `stat` is the whole cost of a hit.
/// Only successes are kept, because the fix for a broken file is somebody writing a good one.
pub fn cached_read(path: &Path) -> Result<std::sync::Arc<Catalogue>, String> {
    use std::sync::{Arc, Mutex};
    type Key = (PathBuf, Option<std::time::SystemTime>, u64);
    static PARSED: Mutex<Vec<(Key, Arc<Catalogue>)>> = Mutex::new(Vec::new());
    let meta = std::fs::metadata(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let key: Key = (path.to_path_buf(), meta.modified().ok(), meta.len());
    if let Some((_, c)) = PARSED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(k, _)| *k == key)
    {
        return Ok(c.clone());
    }
    let mut c = Catalogue::read(path)?;
    c.fill_from(&Catalogue::floor());
    let c = Arc::new(c);
    let mut g = PARSED.lock().unwrap_or_else(|e| e.into_inner());
    // One entry per PATH: the version this replaces is stale by construction.
    g.retain(|(k, _)| k.0 != key.0);
    g.push((key, c.clone()));
    Ok(c)
}

impl Catalogue {
    /// Read the catalogue, or an empty one. Never an error: a missing catalogue is a fact about this
    /// box. Each step falls through on failure, so a broken file cannot hide a working one behind it.
    pub fn load() -> std::sync::Arc<Catalogue> {
        for p in [override_path(), cache_path(), opencode_path()]
            .into_iter()
            .flatten()
        {
            if let Ok(c) = cached_read(&p) {
                return c;
            }
        }
        Catalogue::floor()
    }

    /// The snapshot, parsed once for the process.
    fn floor() -> std::sync::Arc<Catalogue> {
        static SNAPSHOT_PARSED: std::sync::OnceLock<std::sync::Arc<Catalogue>> =
            std::sync::OnceLock::new();
        SNAPSHOT_PARSED
            .get_or_init(|| std::sync::Arc::new(Catalogue::snapshot()))
            .clone()
    }

    /// Adds what the floor has and this file lacks, keeping this file's facts where both have a
    /// model: an older cache (opencode's, say) must not hide a model the shipped snapshot knows.
    fn fill_from(&mut self, floor: &Catalogue) {
        for (id, facts) in &floor.providers {
            let mine = self.providers.entry(id.clone()).or_default();
            for (model, f) in &facts.models {
                mine.models.entry(model.clone()).or_insert(*f);
            }
        }
    }

    /// The floor on its own: what a box with no opencode, no cache and no override gets. A
    /// method rather than an arm of `load` so a test can ask for exactly the floor.
    pub fn snapshot() -> Catalogue {
        Catalogue::read_text(SNAPSHOT)
            .ok()
            .map(|c| Catalogue {
                source: Some(Origin::Snapshot),
                ..c
            })
            .unwrap_or_default()
    }

    pub fn read(path: &Path) -> Result<Catalogue, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let c = Catalogue::read_text(&text).map_err(|e| format!("{e} ({})", path.display()))?;
        // The origin is the file's; `read_text` names none because a text is not
        // a place, and the snapshot arm is the one caller with no path to name.
        Ok(Catalogue {
            source: Some(Origin::File(path.to_path_buf())),
            ..c
        })
    }

    /// Parse a catalogue's text: the file-naming half of [`Catalogue::read`], split out so
    /// the snapshot goes through the same parse and the same filters rather than a second
    /// reader, and so a refresh can refuse a body this build cannot read before it is written.
    pub(crate) fn read_text(text: &str) -> Result<Catalogue, String> {
        let raw: BTreeMap<String, RawProvider> =
            serde_json::from_str(text).map_err(|e| format!("parsing the models catalogue: {e}"))?;
        let mut providers = BTreeMap::new();
        for (id, p) in raw {
            let mut models = BTreeMap::new();
            for (mid, m) in p.models {
                // A model with no published limit is not carried: a zero would invent
                // the smallest possible window, which is the same lie as a guess.
                let Some(l) = m.limit.filter(|l| l.context > 0) else {
                    continue;
                };
                models.insert(
                    mid,
                    ModelFacts {
                        context: l.context,
                        output: l.output,
                        // Absent is not empty: a row with no `modalities` says nothing
                        // about what it takes, and a zeroed `Inputs` would say text only.
                        inputs: m.modalities.as_ref().map(|mo| Inputs {
                            text: mo.input.iter().any(|i| i == "text"),
                            image: mo.input.iter().any(|i| i == "image"),
                            pdf: mo.input.iter().any(|i| i == "pdf"),
                        }),
                        // Absent output modalities is not "no text": it is a row that did not
                        // say, and a default may not be withheld for a field nobody publishes.
                        output_is_text: m
                            .modalities
                            .as_ref()
                            .filter(|mo| !mo.output.is_empty())
                            .map(|mo| mo.output.iter().any(|o| o == "text")),
                        tool_call: m.tool_call,
                        released: m.release_date.as_deref().and_then(parsed_date),
                        prices: m.cost.map(|c| crate::presets::Prices {
                            input: c.input,
                            // No `cache_read` means no discount, which is the input
                            // rate: zero would report a metered turn as free.
                            cached: c.cache_read.unwrap_or(c.input),
                            output: c.output,
                        }),
                    },
                );
            }
            providers.insert(id, RawProviderFacts { models });
        }
        Ok(Catalogue {
            providers,
            source: None,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Which of the four places the catalogue came from, named for the disclosure: a file by
    /// its path, the snapshot by its fetch date, since a snapshot's age is the one fact
    /// about it that can go stale.
    pub fn source(&self) -> Option<String> {
        self.source.as_ref().map(|o| match o {
            Origin::File(p) => p.display().to_string(),
            Origin::Snapshot => {
                format!("the vendored snapshot (fetched {SNAPSHOT_FETCHED})")
            }
        })
    }

    /// One model's facts, by the catalogue's own provider id.
    pub fn model(&self, provider: &str, model: &str) -> Option<ModelFacts> {
        self.providers.get(provider)?.models.get(model).copied()
    }

    /// Whether this model takes pictures: `Some(false)` when the catalogue's `modalities.input`
    /// names text and not image, `None` when nothing here knows. `None` is an answer, not a reason
    /// to guess: an `image_url` sent to a text-only model refuses every later turn.
    pub fn takes_images(&self, provider: &str, model: &str) -> Option<bool> {
        self.model(provider, model)?.inputs.map(|i| i.image)
    }

    /// Whether this model takes a PDF as a document: `Some(false)` when the catalogue lists its
    /// inputs and `pdf` is not among them, `None` when nothing here knows. Absent is NOT a yes:
    /// a document part sent to a door that cannot take one refuses the turn, where the extracted
    /// text of the same file is always readable.
    pub fn takes_pdfs(&self, provider: &str, model: &str) -> Option<bool> {
        self.model(provider, model)?.inputs.map(|i| i.pdf)
    }

    /// Every model name a provider carries, sorted: for a failure message that can offer
    /// the alternatives rather than only naming the mistake.
    pub fn model_names(&self, provider: &str) -> Vec<String> {
        self.providers
            .get(provider)
            .map(|p| p.models.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// A strictly shorter name for the same window and price, when one exists: the shortest-name
    /// tiebreak exposed for the one caller that needs it as a question rather than a sort.
    pub fn shorter_equivalent(&self, provider: &str, name: &str, of: ModelFacts) -> Option<String> {
        self.providers
            .get(provider)?
            .models
            .iter()
            // Same window, same input rate, a different and shorter name: the shape of a
            // longer alias for one model.
            .filter(|(n, m)| {
                n.as_str() != name
                    && n.len() < name.len()
                    && m.context == of.context
                    && m.prices.map(|p| p.input) == of.prices.map(|p| p.input)
            })
            // The shortest of those, then the name, matching `default_model`'s order so
            // the suggestion is the spelling that rule would have used.
            .min_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(b.0)))
            .map(|(n, _)| n.clone())
    }

    /// The model to use when the operator names none, chosen by rule rather than written down,
    /// because a constant goes stale silently. See [`Catalogue::pick`].
    pub fn default_model(&self, provider: &str) -> Option<String> {
        self.pick(provider).map(|p| p.model)
    }

    /// The model a provider's sessions start on, and the rung that chose it.
    ///
    /// A vendor's own listing says which model is the new one, and a rule written down here goes
    /// stale the week a model ships - so the newest `release_date` among the provider's CHAT models
    /// wins, as Claude Code picks up a new model by picking the most recent one:
    ///
    /// 1. chat models only: `modalities.output` names text and `tool_call` is true, and a row that
    ///    says neither (an absent field is not a `no`) is kept;
    /// 2. experiments are not a default: `-exp`, `-preview`, `-beta`, `-alpha`, and a dated
    ///    snapshot where the undated name is carried too;
    /// 3. the newest `release_date`; among one date's models the plainest name, then the cheaper
    ///    input, then the shorter name;
    /// 4. a model with no `release_date` ranks after every dated one, and among themselves they
    ///    keep today's rule (largest window, cheapest input, plainest name, newest version) - a
    ///    catalogue that publishes no dates then gets the answer it always got.
    pub fn pick(&self, provider: &str) -> Option<Pick> {
        let models = &self.providers.get(provider)?.models;
        let chat = |m: &ModelFacts| m.output_is_text.unwrap_or(true) && m.tool_call.unwrap_or(true);
        let carried: std::collections::BTreeSet<String> = models.keys().cloned().collect();
        let (mut dated, mut undated): (Vec<_>, Vec<_>) = models
            .iter()
            .filter(|(n, m)| chat(m) && !experimental(n) && !dated_snapshot(n, &carried))
            .partition(|(_, m)| m.released.is_some());
        // Rung 3: newest first, then the plainest of that date, the cheaper, the shorter.
        dated.sort_by(|a, b| {
            b.1.released
                .cmp(&a.1.released)
                .then_with(|| model_version(a.0).1.cmp(&model_version(b.0).1))
                .then_with(|| cheaper(a.1, b.1))
                .then_with(|| a.0.len().cmp(&b.0.len()))
                .then_with(|| a.0.cmp(b.0))
        });
        // Rung 4, unchanged from before the dates arrived.
        undated.sort_by(|a, b| {
            b.1.context
                .cmp(&a.1.context)
                .then_with(|| cheaper(a.1, b.1))
                .then_with(|| model_version(a.0).1.cmp(&model_version(b.0).1))
                .then_with(|| model_version(b.0).0.cmp(&model_version(a.0).0))
                .then_with(|| a.0.len().cmp(&b.0.len()))
                .then_with(|| a.0.cmp(b.0))
        });
        let (name, facts) = dated.first().or(undated.first())?;
        let same_day = dated
            .iter()
            .filter(|(_, f)| f.released == facts.released)
            .count();
        Some(Pick {
            model: (*name).clone(),
            why: match facts.released {
                Some(d) if same_day > 1 => format!(
                    "newest release_date ({}); the plainest of {same_day} on that date",
                    date_words(d)
                ),
                Some(d) => format!("newest release_date ({})", date_words(d)),
                None => "no chat model here carries a release_date, so today's rule chose".into(),
            },
        })
    }

    /// Every model a provider lists that carries a context limit, largest first: for a
    /// picker, and for a miss report that names what it could have been.
    pub fn models(&self, provider: &str) -> Vec<(String, ModelFacts)> {
        let Some(p) = self.providers.get(provider) else {
            return Vec::new();
        };
        let mut out: Vec<(String, ModelFacts)> =
            p.models.iter().map(|(k, v)| (k.clone(), *v)).collect();
        out.sort_by(|a, b| b.1.context.cmp(&a.1.context).then(a.0.cmp(&b.0)));
        out
    }
}

/// A catalogue of ONE model, for a test that needs a modality fact rather than the box's own
/// catalogue. Written through the file reader because that is how a catalogue reaches a session.
#[cfg(test)]
pub(crate) fn fixture(provider: &str, model: &str, inputs: &[&str]) -> Catalogue {
    let list = inputs
        .iter()
        .map(|i| format!("\"{i}\""))
        .collect::<Vec<_>>()
        .join(",");
    let text = format!(
        r#"{{"{provider}": {{"models": {{"{model}": {{ "limit": {{"context": 1000}},
             "modalities": {{"input": [{list}], "output": ["text"]}} }}}}}}}}"#
    );
    let path = std::env::temp_dir().join(format!(
        "letibot-catalogue-{}-{}-{}.json",
        std::process::id(),
        // A counter: two tests of one provider would otherwise write and delete one file.
        {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            NEXT.fetch_add(1, Ordering::SeqCst)
        },
        provider
    ));
    std::fs::write(&path, text).expect("a catalogue file");
    let read = Catalogue::read(&path).expect("the fixture parses");
    let _ = std::fs::remove_file(&path);
    read
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "deepseek": {
        "id": "deepseek",
        "models": {
          "deepseek-chat": {"limit": {"context": 1000000, "output": 384000},
                            "cost": {"input": 0.15, "output": 0.6, "cache_read": 0.003}},
          "deepseek-legacy": {"limit": {"context": 64000, "output": 8000}},
          "an-image-model": {"limit": {"context": 0, "output": 0}}
        }
      },
      "zhipuai": {
        "id": "zhipuai",
        "models": {
          "glm-5": {"limit": {"context": 204800, "output": 131072},
                    "cost": {"input": 1.0, "output": 3.2}}
        }
      }
    }"#;

    /// A cache that predates a model still lists it from the snapshot, and keeps its own facts
    /// where both carry one: opencode's month-old copy hid `k3.1-pro` from the model card.
    #[test]
    fn an_older_cache_is_filled_from_the_snapshot() {
        let dir =
            std::env::temp_dir().join(format!("letibot-catalogue-fill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.json");
        let old = r#"{"kimi-code-plan-global": {"id": "kimi-code-plan-global", "models": {
            "k3": {"limit": {"context": 4242, "output": 1000}}}}}"#;
        std::fs::write(&path, old).unwrap();

        let c = cached_read(&path).expect("it parses");
        let models: Vec<String> = c
            .models("kimi-code-plan-global")
            .into_iter()
            .map(|(m, _)| m)
            .collect();
        assert!(models.iter().any(|m| m == "k3.1-pro"), "{models:?}");
        let k3 = c
            .models("kimi-code-plan-global")
            .into_iter()
            .find(|(m, _)| m == "k3")
            .expect("k3 is listed");
        assert_eq!(k3.1.context, 4242, "the file's own facts win");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unchanged file is parsed once and a changed one is parsed again: every session open read and
    /// parsed the whole catalogue otherwise.
    #[test]
    fn a_file_is_parsed_once_until_it_changes() {
        let dir =
            std::env::temp_dir().join(format!("letibot-catalogue-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.json");
        std::fs::write(&path, SAMPLE).unwrap();

        let first = cached_read(&path).expect("it parses");
        let again = cached_read(&path).expect("it parses");
        assert!(
            std::sync::Arc::ptr_eq(&first, &again),
            "an unchanged file was read and parsed a second time"
        );
        assert_eq!(
            first.model("deepseek", "deepseek-chat").unwrap().context,
            1_000_000
        );

        // A refresh: a different file under the same name (a different length, so the key moves
        // even on a filesystem whose clock cannot tell the two writes apart).
        std::fs::write(
            &path,
            r#"{"deepseek": {"id": "deepseek", "models": {
                 "deepseek-chat": {"limit": {"context": 128000, "output": 8000}}}}}"#,
        )
        .unwrap();
        let refreshed = cached_read(&path).expect("it parses");
        assert_eq!(
            refreshed
                .model("deepseek", "deepseek-chat")
                .unwrap()
                .context,
            128_000,
            "a rewritten file was answered from the old parse"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The newest model wins, and a plain name still beats a decorated one: a variant of the
    /// same version, or a dated snapshot whose version sorts higher, loses to the alias. The
    /// two guards stay together so neither can be fixed into the other.
    #[test]
    fn the_default_is_the_newest_model_and_not_a_dated_snapshot_of_it() {
        const FIXTURE: &str = r#"{
          "p": {"id": "p", "models": {
            "m-4.7":       {"limit": {"context": 204800},  "cost": {"input": 0}},
            "m-5.2":       {"limit": {"context": 1000000}, "cost": {"input": 0}},
            "m-5.3":       {"limit": {"context": 1000000}, "cost": {"input": 0}},
            "m-5.3-flash": {"limit": {"context": 1000000}, "cost": {"input": 0}}
          }},
          "g": {"id": "g", "models": {
            "g-4.3":                {"limit": {"context": 1000000}, "cost": {"input": 1.25}},
            "g-4.20-0309-reasoning": {"limit": {"context": 1000000}, "cost": {"input": 1.25}}
          }}
        }"#;
        let cat = read_fixture(FIXTURE);
        // Newer wins where both names are equally plain: m-5.3, not m-5.2 and not m-4.7,
        // which loses on the window first.
        assert_eq!(cat.default_model("p").as_deref(), Some("m-5.3"));
        // A dated snapshot does not win for sorting higher: g-4.20 > g-4.3 numerically,
        // and the plain alias is what survives the next catalogue refresh.
        assert_eq!(cat.default_model("g").as_deref(), Some("g-4.3"));
        // And the rungs themselves, where they are easier to read than through a pick.
        assert_eq!(model_version("glm-5.3"), (vec![5, 3], 0));
        assert_eq!(model_version("glm-5.3-flash"), (vec![5, 3], 1));
        assert_eq!(model_version("glm-5"), (vec![5], 0));
        assert_eq!(model_version("grok-4.20-0309-reasoning"), (vec![4, 20], 2));
        assert_eq!(model_version("deepseek-flash"), (vec![], 0));
        assert_eq!(model_version("deepseek-v4-flash"), (vec![4], 1));
    }

    /// The rule the operator asked for, one rung per test: the newest release date among the chat
    /// models wins, and every other clause has to hold or a session starts on a preview.
    #[test]
    fn the_newest_release_date_wins_and_only_chat_models_are_considered() {
        const FIXTURE: &str = r#"{
          "p": {"id": "p", "models": {
            "m-old":     {"limit": {"context": 900000}, "cost": {"input": 0.1},
                          "release_date": "2026-08-13", "tool_call": true,
                          "modalities": {"input": ["text"], "output": ["text"]}},
            "m-new":     {"limit": {"context": 100000}, "cost": {"input": 9.0},
                          "release_date": "2026-10-17", "tool_call": true,
                          "modalities": {"input": ["text"], "output": ["text"]}},
            "m-video":   {"limit": {"context": 900000}, "cost": {"input": 0.1},
                          "release_date": "2026-11-01", "tool_call": false,
                          "modalities": {"input": ["text"], "output": ["video"]}},
            "m-no-tool": {"limit": {"context": 900000}, "cost": {"input": 0.1},
                          "release_date": "2026-11-02", "tool_call": false,
                          "modalities": {"input": ["text"], "output": ["text"]}},
            "m-open":    {"limit": {"context": 50000}, "cost": {"input": 5.0},
                          "release_date": "2026-09-01"}
          }}
        }"#;
        let cat = read_fixture(FIXTURE);
        // The newest date wins over the widest window and the cheapest rate: a new session is
        // about which model is the new one, not which is the largest.
        let pick = cat.pick("p").expect("a chat model");
        assert_eq!(pick.model, "m-new");
        assert!(pick.why.contains("2026-10-17"), "{}", pick.why);
        // `m-video` (a video output, no tool calls) and `m-no-tool` are BOTH newer and are not
        // candidates; `m-open`, which says neither field, would win if it were dated newer - it is
        // not, and an absent field is not a `no`.
        assert_eq!(
            cat.model("p", "m-video")
                .map(|m| (m.output_is_text, m.tool_call)),
            Some((Some(false), Some(false)))
        );
        assert_eq!(
            cat.model("p", "m-open")
                .map(|m| (m.output_is_text, m.tool_call)),
            Some((None, None))
        );
    }

    #[test]
    fn a_preview_or_a_dated_snapshot_is_never_the_default() {
        const FIXTURE: &str = r#"{
          "p": {"id": "p", "models": {
            "m-5.3":              {"limit": {"context": 100000}, "cost": {"input": 1.0},
                                   "release_date": "2026-10-01"},
            "m-5.4-exp":          {"limit": {"context": 100000}, "cost": {"input": 1.0},
                                   "release_date": "2026-11-01"},
            "m-5.4-preview":      {"limit": {"context": 100000}, "cost": {"input": 1.0},
                                   "release_date": "2026-11-02"},
            "m-5.4-beta-2":       {"limit": {"context": 100000}, "cost": {"input": 1.0},
                                   "release_date": "2026-11-03"},
            "m-5.4":              {"limit": {"context": 100000}, "cost": {"input": 1.0},
                                   "release_date": "2026-09-01"}
          }},
          "q": {"id": "q", "models": {
            "q-5":        {"limit": {"context": 100000}, "cost": {"input": 1.0},
                           "release_date": "2026-10-01"},
            "q-5-20251001": {"limit": {"context": 100000}, "cost": {"input": 1.0},
                           "release_date": "2026-11-01"}
          }}
        }"#;
        let cat = read_fixture(FIXTURE);
        // Every experiment is newer and every one of them is skipped, so the answer is the newest
        // model that is not one.
        let pick = cat.pick("p").expect("a model that is not an experiment");
        assert_eq!(pick.model, "m-5.3");
        assert!(pick.why.contains("2026-10-01"), "{}", pick.why);
        // `q-5-20251001` is a dated snapshot of a model this provider also carries undated.
        assert_eq!(cat.default_model("q").as_deref(), Some("q-5"));
        // And without the undated twin the dated name is all there is, so it stands.
        assert_eq!(
            read_fixture(
                r#"{"r": {"id": "r", "models": {
                     "r-5-20251001": {"limit": {"context": 100000},
                                       "cost": {"input": 1.0},
                                       "release_date": "2026-11-01"}}}}"#
            )
            .default_model("r")
            .as_deref(),
            Some("r-5-20251001")
        );
    }

    #[test]
    fn one_day_is_ties_by_plainest_then_cheaper_then_shorter() {
        const FIXTURE: &str = r#"{
          "p": {"id": "p", "models": {
            "m-v4-flash":     {"limit": {"context": 100000}, "cost": {"input": 0.1},
                               "release_date": "2026-10-17"},
            "m-flash":        {"limit": {"context": 100000}, "cost": {"input": 9.0},
                               "release_date": "2026-10-17"},
            "m-flash-vision": {"limit": {"context": 100000}, "cost": {"input": 20.0},
                               "release_date": "2026-10-17"}
          }},
          "q": {"id": "q", "models": {
            "q-a-long": {"limit": {"context": 100000}, "cost": {"input": 0.5},
                         "release_date": "2026-10-17"},
            "q-b":      {"limit": {"context": 100000}, "cost": {"input": 0.5},
                         "release_date": "2026-10-17"}
          }}
        }"#;
        let cat = read_fixture(FIXTURE);
        // The plainest name wins the date before the price does: `m-v4-flash` is the cheapest and
        // loses to `m-flash`, whose name has nothing written after the version.
        assert_eq!(cat.default_model("p").as_deref(), Some("m-flash"));
        let pick = cat.pick("p").unwrap();
        assert!(pick.why.contains("plainest of 3"), "{}", pick.why);
        // Two equally plain names: the shorter one.
        assert_eq!(cat.default_model("q").as_deref(), Some("q-b"));
    }

    /// A refresh that moves a provider's default says so once, and a first look is not a change.
    #[test]
    fn a_default_that_moves_is_said_once_and_recorded() {
        let dir = std::env::temp_dir().join(format!(
            "letibot-defaults-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.json");
        let write = |model: &str, date: &str| {
            let mut all = serde_json::Map::new();
            for id in crate::refresh::catalogue_ids() {
                let name = if id == "deepseek" { model } else { "kept" };
                all.insert(
                    id.to_string(),
                    serde_json::json!({"models": {name: {
                        "limit": {"context": 1000},
                        "cost": {"input": 1.0},
                        "release_date": date}}}),
                );
            }
            std::fs::write(&path, serde_json::to_string(&all).unwrap()).unwrap();
        };
        write("deepseek-flash", "2026-10-17");
        assert!(
            note_defaults(&path).is_empty(),
            "a first look has nothing to compare against"
        );
        assert_eq!(
            known_defaults(&path).get("deepseek").map(String::as_str),
            Some("deepseek-flash")
        );
        // The catalogue moves: one line, naming both answers.
        write("deepseek-v4-pro", "2026-11-20");
        let lines = note_defaults(&path);
        assert_eq!(
            lines,
            vec!["deepseek: default is now deepseek-v4-pro (was deepseek-flash)".to_string()]
        );
        // Said once: the record moved with it.
        write("deepseek-v4-pro", "2026-11-20");
        assert!(note_defaults(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The rule against the vendored floor, as models.dev carried it on [`SNAPSHOT_FETCHED`]: four
    /// answers measured by hand. This fails when the snapshot is regenerated, and that is the point -
    /// the numbers are re-measured then, not trusted for ever.
    #[test]
    fn the_floor_picks_the_newest_chat_model_of_each_provider() {
        let cat = Catalogue::snapshot();
        for (id, want) in [
            // Same day as dv4-flash and its vision experiment: the plainest of the three.
            ("deepseek", "deepseek-flash"),
            // `glm-5.4` is the newest the coding plan carries.
            ("zai-coding-plan", "glm-5.4"),
            // `grok-imagine-video-1.5-lite` is newer and its output is video, so it is not a chat
            // model at all.
            ("xai", "grok-4.7"),
            ("anthropic", "claude-fable-5-2"),
        ] {
            assert_eq!(
                cat.default_model(id).as_deref(),
                Some(want),
                "{id}: the rule picked {}",
                cat.pick(id).map(|p| p.why).unwrap_or_default()
            );
        }
    }

    /// A catalogue read from a JSON string this test owns, at a path per call: these tests run
    /// as parallel threads of one process, and a name keyed on the pid had them deleting each
    /// other's fixture mid-read.
    fn read_fixture(json: &str) -> Catalogue {
        // A counter of this function's own: two helpers sharing one would still be two files
        // per call, which is what the name is for.
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "letibot-catalogue-fixture-{}-{}.json",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::write(&d, json).expect("writing");
        let c = Catalogue::read(&d).expect("parsing");
        let _ = std::fs::remove_file(&d);
        c
    }

    /// A file per call, not per process, for the reason [`read_fixture`] records.
    fn sample() -> Catalogue {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "letibot-cat-{}-{}.json",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::write(&d, SAMPLE).expect("writing");
        let c = Catalogue::read(&d).expect("parsing");
        let _ = std::fs::remove_file(&d);
        c
    }

    /// The floor is real, current, and honest about its age: it carries every provider a preset
    /// can name, the numbers are the ones models.dev published, and the source names itself with
    /// its date, so a switched session plans compaction against a number that can be checked.
    #[test]
    fn the_snapshot_floor_carries_the_preset_providers_and_names_its_age() {
        let c = Catalogue::snapshot();
        for provider in [
            "deepseek",
            "zhipuai",
            "zai-coding-plan",
            "zhipuai-coding-plan",
            "xai",
            "kimi-code-plan-cn",
            "kimi-code-plan-global",
        ] {
            assert!(
                !c.model_names(provider).is_empty(),
                "the snapshot has no models for {provider} — a preset names it, so the \
                 floor must carry it (scripts/refresh-models-snapshot.sh)"
            );
        }
        let glm = c
            .model("zai-coding-plan", "glm-5.3")
            .expect("glm-5.3 on the coding plan");
        assert_eq!(
            glm.context, 1_000_000,
            "glm-5.3's window is the number a switched session plans compaction against"
        );
        // The floor says what it is, with its date: a snapshot whose age is silent is a stale
        // table nobody can check.
        let src = c.source().expect("the floor names itself");
        assert!(
            src.contains("vendored snapshot") && src.contains(SNAPSHOT_FETCHED),
            "the source disclosure must name the snapshot AND its fetch date: {src}"
        );
    }

    /// A file read names the file and a bare text names nothing. The origin is what the
    /// disclosure draws, so stamping the snapshot's name on a file would be a lie with a path
    /// in it.
    #[test]
    fn a_file_read_names_the_file_and_a_bare_text_names_nothing() {
        let path = std::env::temp_dir().join(format!(
            "letibot-cat-origin-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(
            &path,
            r#"{"only": {"models": {"m": {"limit": {"context": 1000}}}}}"#,
        )
        .expect("write");
        let c = Catalogue::read(&path).expect("parses");
        let _ = std::fs::remove_file(&path);
        let src = c.source().expect("a file read names the file");
        assert!(src.contains(&path.display().to_string()), "named {src}");
        assert!(!src.contains("snapshot"), "a file is not the floor: {src}");
        assert_eq!(c.model("only", "m").unwrap().context, 1_000);

        // A bare text carries no origin at all: the caller that has one stamps it.
        let bare = Catalogue::read_text(SNAPSHOT).expect("the snapshot parses");
        assert!(bare.source.is_none(), "a text is not a place");
    }

    /// The fallbacks name real models against the floor itself: the snapshot's whole job,
    /// asked as the question a box with nothing else asks it.
    #[test]
    fn the_fallback_models_are_real_on_the_floor_alone() {
        let cat = Catalogue::snapshot();
        // The fallbacks as `crate::presets` spells them, read from the presets rather
        // than re-typed, so a fallback change moves this test with it.
        for (preset, provider) in [
            (crate::presets::DEEPSEEK, "deepseek"),
            (crate::presets::GLM, "zhipuai"),
            (crate::presets::GLM_CODING, "zai-coding-plan"),
            (crate::presets::GLM_CODING_CN, "zhipuai-coding-plan"),
            (crate::presets::GROK, "xai"),
            (crate::presets::KIMI_CODE, "kimi-code-plan-cn"),
            (crate::presets::KIMI_CODE_GLOBAL, "kimi-code-plan-global"),
        ] {
            assert!(
                cat.model_names(provider)
                    .contains(&preset.fallback_model.to_string()),
                "{}/{} is a preset's fallback and the floor does not carry it",
                provider,
                preset.fallback_model
            );
        }
    }

    /// What a model takes as input is read, and not knowing is its own answer: a text-only model
    /// refuses every turn of a conversation carrying pictures, so `None` is the safe end.
    #[test]
    fn the_input_modalities_are_read_and_absent_is_not_text_only() {
        const FIXTURE: &str = r#"{
          "p": {"models": {
            "sees":   {"limit": {"context": 1000},
                       "modalities": {"input": ["text", "image"], "output": ["text"]}},
            "blind":  {"limit": {"context": 1000},
                       "modalities": {"input": ["text"], "output": ["text"]}},
            "silent": {"limit": {"context": 1000}}
          }}
        }"#;
        let c = read_fixture(FIXTURE);
        assert_eq!(c.takes_images("p", "sees"), Some(true));
        assert_eq!(c.takes_images("p", "blind"), Some(false));
        assert_eq!(
            c.takes_images("p", "silent"),
            None,
            "a row with no modalities says nothing, and a zeroed Inputs would say \
             'text only' — an invented fact"
        );
        assert_eq!(
            c.takes_images("p", "not-a-model"),
            None,
            "a model the catalogue does not carry is unknown, not blind"
        );
        // The window and the modalities live on one row: adding the second must not
        // have cost the first.
        assert_eq!(c.model("p", "sees").unwrap().context, 1_000);
    }

    /// A model that takes PDFs says so, and a row that lists its inputs without `pdf` is a model
    /// that does NOT take one - the fact a document part is held back by.
    #[test]
    fn the_catalogue_says_which_models_take_a_pdf() {
        const FIXTURE: &str = r#"{
          "p": {"models": {
            "reader":  {"limit": {"context": 1000},
                        "modalities": {"input": ["text", "pdf"], "output": ["text"]}},
            "seer":    {"limit": {"context": 1000},
                        "modalities": {"input": ["text", "image", "video", "pdf"],
                                       "output": ["text"]}},
            "blind":   {"limit": {"context": 1000},
                        "modalities": {"input": ["text", "image"], "output": ["text"]}},
            "silent":  {"limit": {"context": 1000}}
          }}
        }"#;
        let c = read_fixture(FIXTURE);
        assert_eq!(c.takes_pdfs("p", "reader"), Some(true));
        assert_eq!(c.takes_pdfs("p", "seer"), Some(true));
        assert_eq!(
            c.takes_pdfs("p", "blind"),
            Some(false),
            "an input list without pdf is a model that takes no document"
        );
        assert_eq!(
            c.takes_pdfs("p", "silent"),
            None,
            "a row with no modalities says nothing, which is not a yes"
        );
        assert_eq!(c.takes_pdfs("p", "not-a-model"), None);
        // The two modality facts are independent: `blind` sees no picture and takes no pdf,
        // `reader` takes a pdf and no picture.
        assert_eq!(c.takes_images("p", "reader"), Some(false));
    }

    #[test]
    fn a_models_context_and_prices_come_from_the_catalogue() {
        let c = sample();
        let m = c.model("deepseek", "deepseek-chat").expect("the model");
        assert_eq!(m.context, 1_000_000);
        assert_eq!(m.output, 384_000);
        let p = m.prices.expect("priced");
        assert_eq!(p.input, 0.15);
        assert_eq!(p.cached, 0.003);
    }

    /// A model with no cache discount is charged at the input rate, not at zero: defaulting
    /// the missing field would report a metered turn as free.
    #[test]
    fn a_model_with_no_cache_rate_is_not_free_to_re_read() {
        let p = sample()
            .model("zhipuai", "glm-5")
            .expect("the model")
            .prices
            .expect("priced");
        assert_eq!(p.cached, p.input, "no discount is the input rate");
    }

    /// A model the catalogue does not carry, and a model whose limit is zero, are both `None`:
    /// the point is to stop inventing this number.
    #[test]
    fn an_unknown_model_is_none_rather_than_a_guess() {
        let c = sample();
        assert!(c.model("deepseek", "deepseek-reasoner").is_none());
        assert!(c.model("nobody", "nothing").is_none());
        assert!(
            c.model("deepseek", "an-image-model").is_none(),
            "a zero limit is not a limit"
        );
    }

    /// A catalogue that publishes no release dates - the shape every cache had before the rule
    /// asked for one - still answers by the rule it always had: widest window, then cheapest, and
    /// an unpriced model is skipped rather than chosen.
    #[test]
    fn a_catalogue_with_no_dates_keeps_todays_rule() {
        let c = sample();
        assert_eq!(
            c.default_model("deepseek").as_deref(),
            Some("deepseek-chat")
        );
        // `deepseek-legacy` has no price, so it ranks after every priced model rather than being
        // read as free: unpriced and free are different, and an unpriced default reports every
        // turn as costing nothing.
        assert_eq!(c.default_model("zhipuai").as_deref(), Some("glm-5"));
        assert!(c.default_model("nobody").is_none());
    }

    /// A rolling alias beats a dated snapshot of the same model: vendors publish both at
    /// identical window and price, and the snapshot is the one that gets retired.
    #[test]
    fn a_tie_goes_to_the_shorter_name() {
        let d = std::env::temp_dir().join(format!(
            "letibot-cat-tie-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(
            &d,
            r#"{"xai":{"id":"xai","models":{
                 "grok-4.20-0309-non-reasoning": {"limit":{"context":1000000,"output":30000},
                                                  "cost":{"input":1.25,"output":2.5}},
                 "grok-4.3": {"limit":{"context":1000000,"output":30000},
                              "cost":{"input":1.25,"output":2.5}}}}}"#,
        )
        .expect("writing");
        let c = Catalogue::read(&d).expect("parsing");
        let _ = std::fs::remove_file(&d);
        assert_eq!(c.default_model("xai").as_deref(), Some("grok-4.3"));
    }

    #[test]
    fn a_providers_models_come_back_largest_window_first() {
        let names: Vec<String> = sample()
            .models("deepseek")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec!["deepseek-chat", "deepseek-legacy"]);
    }

    /// The meter reads the catalogue when the operator's file prices nothing, and the file
    /// still wins where it has an entry, because that is their own number and may be a
    /// contract price.
    #[test]
    fn a_model_the_operators_file_does_not_price_is_priced_by_the_catalogue() {
        let c = sample();
        let p = c
            .model("deepseek", "deepseek-chat")
            .expect("the model")
            .prices
            .expect("the catalogue prices it");
        // 1000 prompt of which 600 cached, 100 out, at 0.15 / 0.003 / 0.6 USD per million:
        // 400*0.15 + 600*0.003 + 100*0.6 = 121.8, in micro-USD.
        assert_eq!(p.micros(1000, 600, 100), 122);
        // And a model nothing prices stays unpriced, because unpriced and free
        // are different.
        assert!(
            c.model("deepseek", "deepseek-legacy")
                .expect("the model")
                .prices
                .is_none()
        );
    }

    /// The line a model card row carries: the window and both rates per million, or the price
    /// said to be unlisted - the two mistakes being an invented window and a free-looking model.
    #[test]
    fn a_card_line_names_the_window_and_both_rates_or_says_the_price_is_unlisted() {
        let c = sample();
        assert_eq!(
            c.model("deepseek", "deepseek-chat")
                .expect("the model")
                .card_line(),
            "1M window · $0.15 in / $0.6 out per M"
        );
        assert_eq!(
            c.model("deepseek", "deepseek-legacy")
                .expect("the model")
                .card_line(),
            "64k window · price unlisted"
        );
    }

    /// A pinned shelf answers with one catalogue every time; the current one asks the process's
    /// loader at each use, which is what lets a refresh reach a provider built before it.
    #[test]
    fn a_pinned_shelf_holds_and_a_current_one_asks_every_time() {
        let pinned = Shelf::pinned(std::sync::Arc::new(Catalogue::default()));
        assert!(pinned.get().is_empty());
        assert!(std::sync::Arc::ptr_eq(&pinned.get(), &pinned.get()));
        let now = Shelf::Current;
        assert!(std::sync::Arc::ptr_eq(&now.get(), &Catalogue::load()));
    }

    /// A count as a person reads it, rounded down so a window is never quoted larger than it is.
    #[test]
    fn a_window_is_written_in_millions_and_thousands() {
        assert_eq!(tokens(1_048_576), "1M");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(393_216), "393k");
        assert_eq!(tokens(999_999), "999k");
        assert_eq!(tokens(1_000), "1k");
        assert_eq!(tokens(999), "999");
    }

    /// A box with no opencode is a box with no catalogue, and that must not be an error: every
    /// caller already handles the unknown case.
    #[test]
    fn a_missing_catalogue_is_empty_rather_than_a_failure() {
        let c = Catalogue::read(Path::new("/nonexistent/models.json"));
        assert!(c.is_err(), "read reports it");
        assert!(Catalogue::default().is_empty());
        assert!(
            Catalogue::default()
                .model("deepseek", "deepseek-chat")
                .is_none()
        );
    }
}
