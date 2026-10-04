//! Where a key comes from, and where it does not.
//!
//! Order: `--api-key` (a flag, for a one-off), the provider's environment
//! variable, `~/.config/letibot/providers.toml`, then **opencode's own store**
//! (`~/.local/share/opencode/auth.json`, `type: api` entries) — the usual path
//! on a box where the operator already logged a provider in through opencode,
//! the way flowy's seat is read from where `flowy mint` left it. OAuth entries
//! there are not keys and are not used. The file also carries the price table.
//! There is no fallback to another provider's key, and a missing key is a
//! refusal that names the variable, the file and the opencode store, because
//! "unauthorized" from the provider three seconds later names none of them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::presets::{Preset, Prices};

#[derive(Debug, Clone)]
pub struct Credentials {
    pub key: String,
    /// `$DEEPSEEK_API_KEY`, `--api-key`, or the file.
    pub from: String,
    /// The operator's price table, by model id.
    pub prices: BTreeMap<String, Prices>,
    /// A base URL override from the file (a proxy, a compatible server).
    pub url: Option<String>,
}

#[derive(Debug)]
pub enum KeyError {
    Missing {
        provider: String,
        env: String,
        file: PathBuf,
    },
    Unreadable {
        file: PathBuf,
        why: String,
    },
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::Missing {
                provider,
                env,
                file,
            } => write!(
                f,
                "no key for `{provider}`: set ${env}, pass --api-key, or put `key = \"…\"` \
                 under `[{provider}]` in {}",
                file.display()
            ),
            KeyError::Unreadable { file, why } => write!(f, "{}: {why}", file.display()),
        }
    }
}

impl std::error::Error for KeyError {}

/// **The guard model's address, from the operator's config.**
///
/// > *"i dont want to put host and port bro, it is gatekeeper config and should be
/// > picked up from config"*
///
/// Right: an endpoint is a property of the box, not of a session, and a flag that
/// has to be remembered per invocation is a flag that gets left off. It lives beside
/// the provider keys because that is already where "where things are and how to
/// reach them" is written down, and a second config file is a second thing to find.
///
/// ```toml
/// [gatekeeper]
/// endpoint = "192.168.1.76:8090"
/// model = "qwen3-4b"      # optional, for the disclosure
/// budget_ms = 400         # optional
/// ```
///
/// Absent is not an error: a box with no guard runs exactly as it did, and
/// `/supervise` refuses by naming this section rather than reporting a false success.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gatekeeper {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub budget_ms: Option<u64>,
    /// **What the operator trusts their guard to answer about**, as intent names:
    /// `intents = "inspect read_file write_file execute_code destroy unknown"`.
    ///
    /// Empty means the built-in floor. See `OracleScope::declared` for why a
    /// declaration is not a calibration and is labelled as one or the other.
    pub intents: Vec<String>,
    /// How far an effect may land and still be the guard's to answer about:
    /// `in_run`, `host_project` (the default), `host_other`, `external`.
    pub max_scope: Option<String>,
    /// **Tools the guard may answer about whatever rung they land on**, by name:
    /// `tools = "web_search, web_fetch"`.
    ///
    /// The scope is a ladder, so raising `max_scope` to reach one off-box tool
    /// reaches all of them — `github`, `mcp` and the flowy verbs included, and
    /// `flowy say` posts into a room the fleet reads. This names doors instead of
    /// moving the ceiling. Empty by default.
    pub tools: Vec<String>,
}

/// Read `[gatekeeper]`. A missing file, a missing section and an unparseable file
/// are all `Gatekeeper::default()` — this is consulted at daemon start and must
/// never be a reason a daemon does not start.
pub fn gatekeeper(file: Option<&Path>) -> Gatekeeper {
    let file = file.map(Path::to_path_buf).unwrap_or_else(config_file);
    let Ok(parsed) = parse_file(&file) else {
        return Gatekeeper::default();
    };
    let Some(sec) = parsed.sections.get("gatekeeper") else {
        return Gatekeeper::default();
    };
    Gatekeeper {
        endpoint: sec.get("endpoint").or_else(|| sec.get("oracle")).cloned(),
        model: sec.get("model").cloned(),
        budget_ms: sec.get("budget_ms").and_then(|v| v.parse().ok()),
        // Whitespace- or comma-separated, because both spellings are what people
        // type and neither is worth a refusal.
        intents: sec
            .get("intents")
            .map(|v| {
                v.split([' ', ',', '\t'])
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        max_scope: sec.get("max_scope").or_else(|| sec.get("scope")).cloned(),
        // Same spelling rules as `intents`: whitespace or commas, both accepted.
        tools: sec
            .get("tools")
            .map(|v| {
                v.split([' ', ',', '\t'])
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub fn config_file() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default()
        .join("letibot")
        .join("providers.toml")
}

/// Resolve the key and prices for a preset.
pub fn resolve(
    preset: &Preset,
    flag_key: Option<&str>,
    file: Option<&Path>,
) -> Result<Credentials, KeyError> {
    let file = file.map(Path::to_path_buf).unwrap_or_else(config_file);
    let parsed = if file.is_file() {
        Some(parse_file(&file).map_err(|why| KeyError::Unreadable {
            file: file.clone(),
            why,
        })?)
    } else {
        None
    };
    let section = parsed.as_ref().and_then(|p| p.sections.get(preset.name));
    let prices = parsed
        .as_ref()
        .map(|p| p.prices.clone())
        .unwrap_or_default();
    let url = section.and_then(|s| s.get("url").cloned());
    if let Some(k) = flag_key.map(str::trim).filter(|k| !k.is_empty()) {
        return Ok(Credentials {
            key: k.into(),
            from: "--api-key".into(),
            prices,
            url,
        });
    }
    for env in std::iter::once(preset.key_env).chain(preset.alt_envs.iter().copied()) {
        if let Ok(k) = std::env::var(env) {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Ok(Credentials {
                    key: k,
                    from: format!("${env}"),
                    prices,
                    url,
                });
            }
        }
    }
    if let Some(k) = section.and_then(|s| s.get("key")).filter(|k| !k.is_empty()) {
        return Ok(Credentials {
            key: k.clone(),
            from: format!("[{}] in {}", preset.name, file.display()),
            prices,
            url,
        });
    }
    if let Some(k) = opencode_key(preset.name) {
        return Ok(Credentials {
            key: k,
            from: format!("opencode's store, {}", opencode_auth_file().display()),
            prices,
            url,
        });
    }
    Err(KeyError::Missing {
        provider: preset.name.into(),
        env: preset.key_env.into(),
        file,
    })
}

fn opencode_auth_file() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_default()
        .join("opencode")
        .join("auth.json")
}

/// opencode's `auth.json`: `{"deepseek": {"type": "api", "key": "…"}, "xai":
/// {"type": "oauth", …}}`. Only an `api` entry is a key. opencode's provider
/// ids are ours for deepseek; `xai` is our `grok`; GLM is `zhipuai`/`zai`.
fn opencode_key(provider: &str) -> Option<String> {
    let raw = std::fs::read(opencode_auth_file()).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let ids: &[&str] = match provider {
        "deepseek" => &["deepseek"],
        "grok" => &["xai", "grok"],
        "glm" => &["zhipuai", "zai", "glm", "bigmodel"],
        // **The coding plans are provider ids of their own in `auth.json`**, which is where
        // this file's keys are read from: this box's key is filed as `zai-coding-plan`, and a
        // lookup that only knew the family name would miss the key that is actually there.
        "glm-coding" => &["zai-coding-plan", "zai", "zhipuai-coding-plan", "zhipuai"],
        "glm-coding-cn" => &["zhipuai-coding-plan", "zhipuai", "zai-coding-plan", "zai"],
        _ => &[],
    };
    for id in ids {
        if let Some(e) = v.get(*id)
            && e.get("type").and_then(|t| t.as_str()) == Some("api")
            && let Some(k) = e.get("key").and_then(|k| k.as_str())
            && !k.trim().is_empty()
        {
            return Some(k.trim().to_string());
        }
    }
    None
}

/// The operator's standing choice, from `[default]` in the file: which
/// provider and model answer when a daemon is started without `--provider`.
/// `None` is the local server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultChoice {
    pub provider: String,
    pub model: Option<String>,
}

pub fn default_choice(file: Option<&Path>) -> Option<DefaultChoice> {
    let file = file.map(Path::to_path_buf).unwrap_or_else(config_file);
    let parsed = parse_file(&file).ok()?;
    let d = parsed.sections.get("default")?;
    let provider = d.get("provider")?.trim().to_string();
    if provider.is_empty() || provider == "local" {
        return None;
    }
    Some(DefaultChoice {
        provider,
        model: d
            .get("model")
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty()),
    })
}

/// Record the standing choice. `provider = "local"` records the local server.
pub fn set_default(
    file: Option<&Path>,
    provider: &str,
    model: Option<&str>,
) -> Result<PathBuf, String> {
    let file = file.map(Path::to_path_buf).unwrap_or_else(config_file);
    let mut kv = std::collections::BTreeMap::new();
    kv.insert("provider".to_string(), provider.to_string());
    if let Some(m) = model {
        kv.insert("model".to_string(), m.to_string());
    }
    write_section(&file, "default", kv)?;
    Ok(file)
}

/// Record a provider's key under `[name]`, keeping everything else in the
/// file. The file is created mode 0600.
pub fn store_key(file: Option<&Path>, provider: &str, key: &str) -> Result<PathBuf, String> {
    let file = file.map(Path::to_path_buf).unwrap_or_else(config_file);
    let mut kv = std::collections::BTreeMap::new();
    kv.insert("key".to_string(), key.trim().to_string());
    write_section(&file, provider, kv)?;
    Ok(file)
}

/// Replace (or add) one `[section]`'s given keys, re-emitting the rest of the
/// file as it was read. Comments inside a rewritten section are lost; the
/// other sections keep theirs, because the file is rewritten line by line.
fn write_section(
    file: &Path,
    section: &str,
    kv: std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let existing = if file.is_file() {
        std::fs::read_to_string(file).map_err(|e| e.to_string())?
    } else {
        String::new()
    };
    let mut out = String::new();
    let mut in_target = false;
    let mut seen_target = false;
    let mut kept: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let header = format!("[{section}]");
    for raw in existing.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            if in_target {
                emit_section(&mut out, section, &kept, &kv);
            }
            in_target = line == header;
            if in_target {
                seen_target = true;
                continue;
            }
        }
        if in_target {
            if let Some((k, v)) = line.split('#').next().unwrap_or("").split_once('=') {
                let k = k.trim().to_string();
                if !kv.contains_key(&k) {
                    kept.insert(k, v.trim().to_string());
                }
            }
            continue;
        }
        out.push_str(raw);
        out.push('\n');
    }
    if in_target || !seen_target {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        emit_section(&mut out, section, &kept, &kv);
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = file.with_extension("toml.tmp");
    std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, file).map_err(|e| e.to_string())
}

fn emit_section(
    out: &mut String,
    section: &str,
    kept: &std::collections::BTreeMap<String, String>,
    kv: &std::collections::BTreeMap<String, String>,
) {
    out.push_str(&format!("[{section}]\n"));
    for (k, v) in kept {
        out.push_str(&format!("{k} = {v}\n"));
    }
    for (k, v) in kv {
        out.push_str(&format!("{k} = \"{v}\"\n"));
    }
    out.push('\n');
}

/// The file's shape, and all of it:
///
/// ```toml
/// [deepseek]
/// key = "sk-…"
/// url = "https://api.deepseek.com/chat/completions"   # optional
///
/// [prices."deepseek-chat"]
/// input = 0.27     # USD per million tokens, uncached
/// cached = 0.07
/// output = 1.10
/// ```
///
/// Parsed by hand: sections, `key = "string"` and `key = number` lines,
/// comments. A TOML crate for eight lines of grammar would be the wrong trade.
#[derive(Debug, Default)]
struct ProvidersFile {
    sections: BTreeMap<String, BTreeMap<String, String>>,
    prices: BTreeMap<String, Prices>,
}

fn parse_file(path: &Path) -> Result<ProvidersFile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut out = ProvidersFile::default();
    let mut current: Option<String> = None;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = Some(inner.trim().trim_matches('"').to_string());
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!(
                "line {}: expected `key = value` or `[section]`",
                n + 1
            ));
        };
        let (k, v) = (k.trim().to_string(), v.trim().trim_matches('"').to_string());
        let Some(sec) = &current else {
            return Err(format!("line {}: `{k}` before any [section]", n + 1));
        };
        out.sections.entry(sec.clone()).or_default().insert(k, v);
    }
    for (sec, kv) in &out.sections {
        if let Some(model) = sec.strip_prefix("prices.") {
            let num = |k: &str| -> Result<f64, String> {
                kv.get(k)
                    .ok_or_else(|| format!("[{sec}] lacks `{k}`"))?
                    .parse::<f64>()
                    .map_err(|_| format!("[{sec}] `{k}` is not a number"))
            };
            out.prices.insert(
                model.trim_matches('"').to_string(),
                Prices {
                    input: num("input")?,
                    cached: num("cached")?,
                    output: num("output")?,
                },
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_gives_a_key_a_url_and_prices_and_the_env_wins_over_it() {
        let d = std::env::temp_dir().join(format!("letibot-keys-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("providers.toml");
        std::fs::write(
            &f,
            "# keys\n[deepseek]\nkey = \"sk-file\"\nurl = \"http://127.0.0.1:1/v1/chat/completions\"\n\n\
             [prices.\"deepseek-chat\"]\ninput = 0.27\ncached = 0.07\noutput = 1.10\n",
        )
        .unwrap();
        let c = resolve(&crate::presets::DEEPSEEK, None, Some(&f)).unwrap();
        assert_eq!(c.key, "sk-file");
        assert!(c.from.contains("[deepseek]"));
        assert_eq!(
            c.url.as_deref(),
            Some("http://127.0.0.1:1/v1/chat/completions")
        );
        assert_eq!(c.prices["deepseek-chat"].output, 1.10);
        let c = resolve(&crate::presets::DEEPSEEK, Some("flag"), Some(&f)).unwrap();
        assert_eq!(c.from, "--api-key");
        let e = resolve(&crate::presets::GROK, None, Some(&f))
            .unwrap_err()
            .to_string();
        assert!(e.contains("$XAI_API_KEY") && e.contains("[grok]"), "{e}");
    }
}
