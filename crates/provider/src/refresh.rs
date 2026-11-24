//! Keeping the catalogue's cache fresh without a person running a script.
//!
//! One GET of models.dev, trimmed to the providers the presets name, written atomically where
//! [`crate::catalogue::cache_path`] reads it. A conditional GET makes a refresh of an unchanged
//! catalogue one 304 and no write at all: models.dev sends an ETag, and the value is kept in a
//! sidecar beside the cache so a restart does not fetch the body again.
//!
//! Every failure leaves the file that is there alone and answers with a sentence. Nothing here
//! decides whether to refresh or how often - that is the daemon's - and nothing here blocks.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Where the catalogue comes from. The same URL `scripts/refresh-models-snapshot.sh` fetches.
pub const URL: &str = "https://models.dev/api.json";

/// The most of it this will read. The whole catalogue is a couple of megabytes; the cap is what
/// stops a redirected or broken answer from filling a disk.
pub const MAX_BYTES: u64 = 16 << 20;

/// How long the whole request may take. A catalogue nobody could fetch in 30 s is not worth a
/// turn's wait, and there is always tomorrow.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// How old the cache may be before a refresh is worth one.
pub const AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// The providers a preset can switch to, deduplicated and in order: the trim's whole list, taken
/// from the presets so a new door needs no second hand-kept one.
pub fn catalogue_ids() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for p in crate::presets::ALL {
        if !out.contains(&p.catalogue_id) {
            out.push(p.catalogue_id);
        }
    }
    out
}

/// What a GET may be sent under: the validators of the body this box already has.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Conditional {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// One answer, as the refresh needs it: a status, the validators it came with, and the body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body: Vec<u8>,
}

/// A GET, as a function so a test needs no network and no clock.
pub type Fetcher<'a> = dyn Fn(&Conditional) -> Result<Answer, String> + 'a;

/// What a refresh did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The catalogue changed and both files were written.
    Written { providers: usize, bytes: usize },
    /// The server says the copy here is current; only its modification time moved.
    Unchanged,
}

/// Whether the cache is worth refreshing at `now`: a missing file, or one modified longer ago
/// than `every`. A timestamp in the future is this box's clock being wrong, not an old file.
pub fn stale(path: &Path, now: SystemTime, every: Duration) -> bool {
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(at) => now.duration_since(at).is_ok_and(|age| age >= every),
        Err(_) => true,
    }
}

/// The file the validators live in, beside the cache: `models.json` keeps `models.json.etag`.
pub fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".etag");
    PathBuf::from(name)
}

/// Whether a refresh is this box's to make: the operator's `[catalogue] refresh` in leticode.toml,
/// and never when they pointed the catalogue at a file of their own.
pub fn wanted(configured: bool) -> bool {
    wanted_with(
        configured,
        std::env::var("LETIBOT_MODELS_JSON").ok().as_deref(),
    )
}

/// [`wanted`] with the pointer written out, so the rule is a test rather than an environment.
fn wanted_with(configured: bool, pointed_at: Option<&str>) -> bool {
    configured && pointed_at.is_none_or(str::is_empty)
}

/// Refresh the cache this box reads, if there is one to refresh. The one caller is the daemon.
pub fn refresh() -> Result<Outcome, String> {
    let path = crate::catalogue::cache_path().ok_or("this box has no cache directory")?;
    refresh_at(&path, &fetch_live, SystemTime::now())
}

/// [`refresh`] only when the file is missing or older than [`AGE`]: `Ok(None)` is "it was fresh".
pub fn refresh_if_stale(path: &Path, now: SystemTime) -> Result<Option<Outcome>, String> {
    refresh_if_stale_with(path, now, &fetch_live)
}

/// [`refresh_if_stale`] through a fetch of the caller's, so the age rule is a test.
fn refresh_if_stale_with(
    path: &Path,
    now: SystemTime,
    fetch: &Fetcher<'_>,
) -> Result<Option<Outcome>, String> {
    if !stale(path, now, AGE) {
        return Ok(None);
    }
    refresh_at(path, fetch, now).map(Some)
}

/// One refresh of `path`, through `fetch`. A failure of any kind leaves the files that are there
/// exactly as they were, and the error is the sentence the log will carry.
pub fn refresh_at(path: &Path, fetch: &Fetcher<'_>, now: SystemTime) -> Result<Outcome, String> {
    let here = path.exists();
    // A sidecar with no body behind it describes nothing: the condition needs both files.
    let condition: Conditional = here
        .then(|| read_sidecar(path))
        .flatten()
        .unwrap_or_default();
    let answer = fetch(&condition)?;
    match answer.status {
        // The body here is the body the server has. Its age is the only thing that moved, since
        // that is what decides when the next look happens.
        304 => if_304(here, path, now),
        200 => {
            let body = String::from_utf8(answer.body)
                .map_err(|e| format!("the catalogue is not UTF-8: {e}"))?;
            let (text, providers) = trim(&body)?;
            write_atomic(path, text.as_bytes())?;
            // The body first, then its validators: a crash between them leaves a sidecar naming
            // the PREVIOUS body, which costs one full GET, where the other order would leave a
            // 304 vouching for a body the server no longer has.
            let fresh = Conditional {
                etag: answer.etag,
                last_modified: answer.last_modified,
            };
            write_atomic(&sidecar(path), &encode_sidecar(&fresh))?;
            Ok(Outcome::Written {
                providers,
                bytes: text.len(),
            })
        }
        other => Err(format!("{URL} answered HTTP {other}")),
    }
}

/// The 304 arm: the file must be there (a 304 to an unconditional GET describes nothing), and
/// only its modification time changes.
fn if_304(here: bool, path: &Path, now: SystemTime) -> Result<Outcome, String> {
    if !here {
        return Err(format!(
            "{URL} answered 304 to a GET that carried no validator - the cache was removed \
             between the two"
        ));
    }
    touch(path, now)?;
    Ok(Outcome::Unchanged)
}

/// The body trimmed to the providers the presets name, and how many that is. A provider the
/// catalogue does not carry is an error naming it, the rule the snapshot script follows: a trim
/// that silently loses a door is a cache that is fresh and useless.
fn trim(body: &str) -> Result<(String, usize), String> {
    let all: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(body).map_err(|e| format!("{URL} did not answer a catalogue: {e}"))?;
    let mut kept = serde_json::Map::new();
    for id in catalogue_ids() {
        let row = all
            .get(id)
            .ok_or_else(|| format!("the catalogue carries no provider `{id}` - is a preset's `catalogue_id` still right?"))?;
        row.get("models")
            .and_then(|m| m.as_object())
            .filter(|m| !m.is_empty())
            .ok_or_else(|| format!("the catalogue has no models for `{id}`"))?;
        kept.insert(id.to_string(), row.clone());
    }
    let text = serde_json::to_string(&kept).map_err(|e| format!("writing the catalogue: {e}"))?;
    // The trim is only worth writing if what it wrote is a catalogue: the same parse every reader
    // uses, so a shape this build cannot read is refused before it replaces a file that works.
    let parsed = crate::catalogue::Catalogue::read_text(&text)
        .map_err(|e| format!("the trimmed catalogue does not parse: {e}"))?;
    if parsed.is_empty() {
        return Err("the trimmed catalogue carries no provider this build can read".into());
    }
    Ok((text, kept.len()))
}

/// Write `bytes` at `path` through a file of its own: a reader either sees the whole old file or
/// the whole new one, never half of either.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".tmp-{}", std::process::id()));
    let temp = PathBuf::from(name);
    let written = std::fs::write(&temp, bytes)
        .map_err(|e| format!("writing {}: {e}", temp.display()))
        .and_then(|()| {
            std::fs::rename(&temp, path).map_err(|e| format!("naming {}: {e}", path.display()))
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Move a file's modification time to `now` without touching a byte of it.
fn touch(path: &Path, now: SystemTime) -> Result<(), String> {
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
    let times = std::fs::FileTimes::new().set_modified(now);
    file.set_times(times)
        .map_err(|e| format!("dating {}: {e}", path.display()))
}

/// The sidecar's text: the two validators, either of which may be absent.
fn encode_sidecar(c: &Conditional) -> Vec<u8> {
    serde_json::json!({"etag": c.etag, "last_modified": c.last_modified})
        .to_string()
        .into_bytes()
}

/// The validators the sidecar records, or `None` when there is no sidecar or it does not read:
/// a garbled sidecar is one full GET, which is the cost of being wrong the other way.
fn read_sidecar(path: &Path) -> Option<Conditional> {
    let text = std::fs::read_to_string(sidecar(path)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let field = |name: &str| {
        v.get(name)
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let c = Conditional {
        etag: field("etag"),
        last_modified: field("last_modified"),
    };
    (c.etag.is_some() || c.last_modified.is_some()).then_some(c)
}

/// The one GET this box makes: ureq, a 30 s budget, no redirect followed, and a body read
/// through a cap.
fn fetch_live(condition: &Conditional) -> Result<Answer, String> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            // Not followed on purpose: 304 is in the 3xx range, and ureq follows a redirection
            // it is allowed to. A moved catalogue then fails by name instead of by confusion.
            .max_redirects(0)
            .build(),
    );
    let mut req = agent
        .get(URL)
        .header("Accept", "application/json")
        .header("User-Agent", crate::kimi::USER_AGENT);
    if let Some(tag) = &condition.etag {
        req = req.header("If-None-Match", tag);
    }
    if let Some(at) = &condition.last_modified {
        req = req.header("If-Modified-Since", at);
    }
    let mut answer = req.call().map_err(|e| format!("GET {URL}: {e}"))?;
    let status = answer.status().as_u16();
    let header = |name: &str| {
        answer
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let (etag, last_modified) = (header("etag"), header("last-modified"));
    if status != 200 {
        return Ok(Answer {
            status,
            etag,
            last_modified,
            body: Vec::new(),
        });
    }
    let body = answer
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_vec()
        .map_err(|e| format!("reading {URL}: {e}"))?;
    Ok(Answer {
        status,
        etag,
        last_modified,
        body,
    })
}

#[cfg(test)]
mod tests;
