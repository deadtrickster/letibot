//! **Brave Search behind `web_search`.**
//!
//! `letibot-tools` ships `web_search` as a tool that refuses: the schema is fixed
//! (it is prompt bytes in the stable prefix, so the argument names had to be right
//! before anything cached against them) and [`SearchProvider`] is the seam an
//! implementation attaches through. This crate is one implementation.
//!
//! It lives in its own crate rather than in `letibot-tools` because that crate is
//! deliberately *a function of a call and a filesystem* — no sockets, no TLS. The
//! trait is the seam that keeps it that way; putting rustls behind the seam rather
//! than in front of it is the whole point of the seam existing.
//!
//! # What the operator has to do
//!
//! ```text
//! harnessd --web-search brave            # and a key, resolved in this order:
//!   --brave-key KEY                      #   1. the flag
//!   $BRAVE_API_KEY                       #   2. the environment
//!   [brave] key = "…" in providers.toml  #   3. ~/.config/letibot/providers.toml
//! ```
//!
//! A missing key **refuses at attach**, naming all three, rather than three
//! seconds into the first search as a 401 that names none of them — the same rule
//! `letibot-provider` follows for a model key, and for the same reason.
//!
//! # What this does not do
//!
//! It does not fetch pages: `web_fetch` is a different seam with a different
//! backend, and a search provider that quietly also fetched would be a second
//! egress path nobody declared. It does not cache: a cached search result that
//! looks live is §8.2's failure wearing a timestamp. And it never invents a hit —
//! Brave's `web.results` is the only thing read, so a query that matched nothing
//! comes back as nothing rather than as the "did you mean" Brave also returns.

use std::time::Duration;

use letibot_tools::builtins::external::web::{
    SearchError, SearchHit, SearchProvider, SearchQuery, SearchResults,
};

/// Brave's web-search endpoint.
pub const ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

/// How long one search may take before it is a transport failure. A search the
/// model is waiting on is a turn nobody can interrupt, so this is short.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// Where a key was found, for the disclosure. Never the key itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    Flag,
    Env(&'static str),
    File(std::path::PathBuf),
}

impl std::fmt::Display for KeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeySource::Flag => write!(f, "--brave-key"),
            KeySource::Env(v) => write!(f, "${v}"),
            KeySource::File(p) => write!(f, "{}", p.display()),
        }
    }
}

/// `~/.config/letibot/providers.toml` — the file the model keys already live in.
fn providers_file() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("letibot").join("providers.toml"))
}

/// `[brave] key = "…"`, read without a TOML parser: the file is two-deep by
/// construction and `letibot-provider` reads it the same way.
fn key_from_file(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[brave]";
            continue;
        }
        if !in_section {
            continue;
        }
        // A commented-out key is not a key. The section ships with
        // `# key = "BSA-..."` as a placeholder, and reading that would attach a
        // garbage credential and turn a clean "no key" refusal into a 401.
        if line.starts_with('#') {
            continue;
        }
        if let Some(v) = line.strip_prefix("key") {
            let v = v.trim_start().strip_prefix('=')?.trim();
            return Some(v.trim_matches(['"', '\'']).to_string());
        }
    }
    None
}

/// A key the operator gave on the command line, held here rather than in
/// `Config` — which derives `Debug`, and a secret in a struct that can be
/// `{:?}`-printed is a secret one `eprintln!` away from a log.
static FLAG_KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Record `--brave-key`. Called once, during flag parsing.
pub fn set_flag_key(key: impl Into<String>) {
    let _ = FLAG_KEY.set(key.into());
}

/// The key, and where it came from. Flag beats environment beats file.
pub fn resolve_key(flag: Option<&str>) -> Result<(String, KeySource), String> {
    if let Some(k) = flag
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .or_else(|| FLAG_KEY.get().cloned())
        .filter(|k| !k.trim().is_empty())
    {
        return Ok((k.trim().to_string(), KeySource::Flag));
    }
    for var in ["BRAVE_API_KEY", "BRAVE_SEARCH_API_KEY"] {
        if let Ok(k) = std::env::var(var)
            && !k.trim().is_empty()
        {
            return Ok((k.trim().to_string(), KeySource::Env(var)));
        }
    }
    if let Some(p) = providers_file()
        && let Some(k) = key_from_file(&p).filter(|k| !k.trim().is_empty())
    {
        return Ok((k.trim().to_string(), KeySource::File(p)));
    }
    Err(format!(
        "no Brave key: pass --brave-key, set $BRAVE_API_KEY, or put `key = \"…\"` under \
         `[brave]` in {}. Refusing at attach rather than at the first search, where it \
         would be a 401 that names none of these",
        providers_file()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.config/letibot/providers.toml".into())
    ))
}

/// Brave, attached.
pub struct Brave {
    key: String,
    from: KeySource,
    endpoint: String,
    agent: ureq::Agent,
}

impl Brave {
    /// Attach, resolving the key. The error is the operator's to read.
    pub fn attach(flag_key: Option<&str>) -> Result<Brave, String> {
        let (key, from) = resolve_key(flag_key)?;
        Ok(Brave::with_key(key, from, ENDPOINT))
    }

    /// The pieces given outright — for a test against a local stand-in.
    pub fn with_key(key: String, from: KeySource, endpoint: &str) -> Brave {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .build()
            .into();
        Brave {
            key,
            from,
            endpoint: endpoint.to_string(),
            agent,
        }
    }
}

impl SearchProvider for Brave {
    fn search(&self, query: &SearchQuery) -> Result<SearchResults, SearchError> {
        // `site:` is Brave's own operator, and passing it through the query is what
        // the API documents. The tool has already clamped `max_results`.
        let q = match &query.site {
            Some(site) if !site.trim().is_empty() => {
                format!("{} site:{}", query.query, site.trim())
            }
            _ => query.query.clone(),
        };
        let count = query.max_results.clamp(1, 20).to_string();
        let resp = self
            .agent
            .get(&self.endpoint)
            .query("q", &q)
            .query("count", &count)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &self.key)
            .call();

        let mut resp = match resp {
            Ok(r) => r,
            // A status Brave chose is a refusal and carries its own sentence; a
            // connection that never happened is transport. Kept apart because the
            // first is a fact about the request and the second is not.
            Err(ureq::Error::StatusCode(code)) => {
                return Err(SearchError::Refused(match code {
                    401 | 403 => format!(
                        "Brave refused the key from {} ({code}). Check it at \
                         https://api-dashboard.search.brave.com/",
                        self.from
                    ),
                    429 => "Brave rate-limited this key (429). The free tier is one \
                            query per second; wait and ask again"
                        .to_string(),
                    other => format!("Brave answered {other}"),
                }));
            }
            Err(e) => return Err(SearchError::Transport(e.to_string())),
        };

        let body: serde_json::Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| SearchError::Transport(format!("Brave's answer did not parse: {e}")))?;

        // **Only `web.results`.** Brave also returns `query.altered`, discussions,
        // FAQ and infobox blocks; folding those in would report as a search hit a
        // thing that was never a search hit.
        let results = body
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let considered = results.len();
        let hits: Vec<SearchHit> = results
            .iter()
            .filter_map(|r| {
                let url = r.get("url").and_then(|v| v.as_str())?.to_string();
                // A hit with no URL is not reportable — `SearchHit::url` is not
                // optional, and `web.rs` says why.
                if url.is_empty() {
                    return None;
                }
                Some(SearchHit {
                    title: text_of(r.get("title")),
                    url,
                    snippet: text_of(r.get("description")),
                })
            })
            .take(query.max_results)
            .collect();

        // Brave says when it searched for something else. The tool renders it; the
        // point is that a silently rewritten query is a rewritten query.
        let rewritten = body
            .get("query")
            .and_then(|q| q.get("altered"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty() && s != &q);

        Ok(SearchResults {
            hits,
            considered,
            provider: "Brave Search".into(),
            rewritten_query: rewritten,
        })
    }

    fn describe(&self) -> String {
        format!("Brave Search, key from {}", self.from)
    }
}

/// Brave's snippets carry `<strong>` around the matched terms. They are prompt
/// bytes in a tool result, not markup anybody renders, so they come out.
fn text_of(v: Option<&serde_json::Value>) -> String {
    let raw = v.and_then(|v| v.as_str()).unwrap_or("");
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_refused_by_naming_all_three_places() {
        // SAFETY: single-threaded test binary at this point.
        unsafe {
            std::env::remove_var("BRAVE_API_KEY");
            std::env::remove_var("BRAVE_SEARCH_API_KEY");
            std::env::set_var(
                "XDG_CONFIG_HOME",
                std::env::temp_dir().join("letibot-no-brave"),
            );
        }
        let e = resolve_key(None).expect_err("no key anywhere");
        assert!(e.contains("--brave-key"), "{e}");
        assert!(e.contains("BRAVE_API_KEY"), "{e}");
        assert!(e.contains("providers.toml"), "{e}");
        // The flag wins, and says so.
        let (k, from) = resolve_key(Some("from-the-flag")).unwrap();
        assert_eq!(k, "from-the-flag");
        assert_eq!(from, KeySource::Flag);
    }

    #[test]
    fn the_file_is_read_only_under_its_own_section() {
        let dir = std::env::temp_dir().join(format!("letibot-brave-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("letibot")).unwrap();
        let f = dir.join("letibot").join("providers.toml");
        std::fs::write(
            &f,
            "[deepseek]\nkey = \"not-this-one\"\n\n[brave]\nkey = \"BSA-xyz\"\n",
        )
        .unwrap();
        assert_eq!(key_from_file(&f).as_deref(), Some("BSA-xyz"));
        // No `[brave]` section at all is a miss, not the first key in the file.
        std::fs::write(&f, "[deepseek]\nkey = \"nope\"\n").unwrap();
        assert_eq!(key_from_file(&f), None);
        // The section as it ships: a header and a commented placeholder. Reading
        // that would attach a garbage credential, so the answer is still None and
        // the operator still gets the refusal that names all three places.
        std::fs::write(&f, "[brave]\n# key = \"BSA-...\"\n").unwrap();
        assert_eq!(key_from_file(&f), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn brave_markup_never_reaches_the_model() {
        let v = serde_json::json!("the <strong>rust</strong> book");
        assert_eq!(text_of(Some(&v)), "the rust book");
        assert_eq!(text_of(None), "");
    }
}
