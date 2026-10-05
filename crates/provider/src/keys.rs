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

// ---------------------------------------------------------------------------
// PER-MODEL PROFILES
// ---------------------------------------------------------------------------

/// **What `[model.<family>]` and `[model."<alias>"]` carry** — the two-level
/// profile `providers.toml` has documented since 2026-09-19 and nothing read.
///
/// The file described the design in full, down to the precedence rule and a
/// worked example, and no code in the tree parsed either block: `cfg.sampling`
/// was assigned nowhere and held `Config::default`'s greedy literal for the life
/// of every daemon. So the comment was a specification wearing the grammar of a
/// feature, which is the worse of the two failures — an absent feature is
/// discovered the first time somebody wants it, and a documented absent feature
/// is discovered after they have already trusted it.
///
/// # Why two levels
///
/// `effort` is a template concept: it is rendered into the prefix, so it belongs
/// to the DIALECT. Sampling is a property of the weights, and Flash-Next and the
/// 27B dense are different weights behind one byte-identical template, so it
/// belongs to the ALIAS. One file, two keys, and the file says which is which.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelProfile {
    /// The dialect's reasoning effort, from the family block. `None` leaves the
    /// built-in alone.
    pub effort: Option<String>,
    /// The sampling to send, already merged. **Empty means the file said nothing**,
    /// which is a different fact from the file saying `temperature = 0.0`, and the
    /// caller must not collapse them: one defers to the built-in and the other is
    /// an instruction.
    pub sampling: serde_json::Map<String, serde_json::Value>,
    /// **Keys in a `[model.*]` block that are not sampling and not `effort`.**
    ///
    /// Carried rather than dropped so a typo is loud. A silently ignored
    /// `temperatuer = 0.7` is this whole class of bug again, one line lower down:
    /// the operator writes a number, the file accepts it, nothing reads it, and
    /// the only evidence is that the model behaves as it did before.
    pub unknown: Vec<String>,
    /// **The address, for a model this fleet hosts itself.**
    ///
    /// The operator: *"the idea is that i just add urls like this - for models
    /// hosted by my fleet. they are `local` of course."* And they are: no key, no
    /// metering, an OpenAI-compatible or llama.cpp server on the LAN. A block that
    /// carries one is a model `/models` can offer; a block without one is a
    /// sampling profile for a model reached some other way, and both shapes live in
    /// the same section because the curl that describes a fleet model carries its
    /// address, its alias and its sampling together.
    pub url: Option<String>,
    /// **The alias the server answers to**, when the block is named for the BOX
    /// rather than for the weights.
    ///
    /// `[model."dense78"]` with `model = "qwen-3.8-27b"` is the shape that matters
    /// here, because `qwen-3.8-27b` is served twice on this fleet — at
    /// `127.0.0.1:8080` and at `192.168.1.78:8082` — and is also the gatekeeper's
    /// model. Keying a remote entry on the bare alias would re-sample the local one
    /// and the guard along with it. Absent, the block name IS the alias, which is
    /// what a profile for a single served model wants.
    pub model: Option<String>,
    /// **The context window, when the server will not say.**
    ///
    /// A switch reads `n_ctx` off the target's `/props`, which is the only place that
    /// number is true -- but a proxy, or an OpenAI-compatible server that is not
    /// llama.cpp, answers nothing, and the alternative is planning against no wall at
    /// all. So this is here for the operator to state what they know.
    ///
    /// Stated, never inferred from the model's name: one model served twice is served
    /// at two sizes. Measured on this fleet the day it was written -- the same 27B
    /// GGUF at `n_ctx` 262144 on `127.0.0.1:8080` and 57344 on `192.168.1.78:8082`.
    pub window: Option<u64>,
}

/// Every key a `[model.*]` block may set beside `effort`, and how to read it.
///
/// llama.cpp's own names (`top_k`, `min_p`, `thinking_budget_tokens`) are here
/// beside the OpenAI ones, because a LOCAL server takes them and this file is
/// where a local server's sampling is configured. What a METERED provider will
/// accept is a narrower list and is filtered on the way out by
/// `harnessd::provider_sampling`, which is the right place for it: the profile
/// records what the operator asked for, and each transport drops what it cannot
/// carry.
const SAMPLING_KEYS: &[(&str, Num)] = &[
    ("temperature", Num::Float),
    ("top_p", Num::Float),
    ("top_k", Num::Int),
    ("min_p", Num::Float),
    ("presence_penalty", Num::Float),
    ("frequency_penalty", Num::Float),
    ("repeat_penalty", Num::Float),
    ("seed", Num::Int),
    ("max_tokens", Num::Int),
    ("thinking_budget_tokens", Num::Int),
];

#[derive(Copy, Clone)]
enum Num {
    Float,
    Int,
}

/// Read the profile for one served model.
///
/// `family` is the dialect's own word for itself (`qwen`, `glm`); `alias` is the
/// served model (`qwen-3.8-27b`). The family block is read first and the alias
/// block overrides it **key by key**, which is what the file promises and is not
/// the same as the alias block replacing it.
///
/// A missing file, a missing block and an unparsable number are all
/// non-fatal here: this is configuration for how to sample, and refusing to start
/// a daemon over it would be worse than starting at the built-in and saying so.
/// An unparsable value lands in `unknown` with its key, so it is still said out
/// loud.
pub fn model_profile(family: &str, alias: &str, file: Option<&Path>) -> ModelProfile {
    let path = file.map(|p| p.to_path_buf()).unwrap_or_else(config_file);
    let Ok(parsed) = parse_file(&path) else {
        return ModelProfile::default();
    };
    let mut out = ModelProfile::default();
    // Family first, alias second: the second pass overwrites key by key.
    for key in [family, alias] {
        if key.is_empty() {
            continue;
        }
        let Some(kv) = model_section(&parsed, key) else {
            continue;
        };
        for (k, v) in kv {
            match k.as_str() {
                "effort" => {
                    out.effort = Some(v.clone());
                    continue;
                }
                // `endpoint` is the daemon's own word for the same thing and
                // `[gatekeeper]` already spells it that way, so both are taken
                // rather than making the operator remember which section wanted
                // which noun.
                "url" | "endpoint" => {
                    out.url = Some(v.clone());
                    continue;
                }
                "model" => {
                    out.model = Some(v.clone());
                    continue;
                }
                "window" | "context_window" => {
                    match v.trim().parse::<u64>() {
                        Ok(n) if n > 0 => out.window = Some(n),
                        _ => out
                            .unknown
                            .push(format!("window = {v} (not a positive token count)")),
                    }
                    continue;
                }
                _ => {}
            }
            match SAMPLING_KEYS.iter().find(|(name, _)| name == k) {
                Some((name, kind)) => match number(v, *kind) {
                    Some(n) => {
                        out.sampling.insert((*name).to_string(), n);
                    }
                    None => out.unknown.push(format!("{k} = {v} (not a number)")),
                },
                None => out.unknown.push(k.clone()),
            }
        }
    }
    out
}

/// The `[model.X]` section, whatever the brackets did to the quotes.
///
/// `parse_file` trims one layer of quotes off the whole bracket content, so
/// `[model."qwen-3.8-27b"]` arrives as the section `model."qwen-3.8-27b` — the
/// same asymmetry `prices."…"` already lives with a few lines up, handled the
/// same way rather than differently.
fn model_section<'a>(
    parsed: &'a ProvidersFile,
    key: &str,
) -> Option<&'a BTreeMap<String, String>> {
    parsed.sections.iter().find_map(|(sec, kv)| {
        let rest = sec.strip_prefix("model.")?;
        (rest.trim_matches('"') == key).then_some(kv)
    })
}

fn number(v: &str, kind: Num) -> Option<serde_json::Value> {
    let v = v.trim();
    match kind {
        Num::Int => v.parse::<i64>().ok().map(Into::into),
        Num::Float => v.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Into::into),
    }
}


/// **One model this fleet hosts**, as `/models` offers it.
///
/// Declared, never discovered: nothing scans the LAN. The operator writes the
/// block and the listing shows what they wrote, which is the only version of this
/// that can be trusted — a probe that finds a server says nothing about whether
/// its weights are the ones the name claims.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalModel {
    /// The name `/models NAME` takes: the block's own, after the quotes.
    pub name: String,
    /// Where it answers.
    pub url: String,
    /// The alias to send as `model`, which is the block name unless it said
    /// otherwise.
    pub model: String,
    /// Its sampling and effort, already merged with the family block.
    pub profile: ModelProfile,
}

/// Every `[model.*]` block that carries an address, in file order.
///
/// A block with no address is a sampling profile and is deliberately absent here:
/// it is not something a person can switch TO, and listing it as though it were
/// would be offering a choice that does nothing.
pub fn local_models(file: Option<&Path>) -> Vec<LocalModel> {
    let path = file.map(|p| p.to_path_buf()).unwrap_or_else(config_file);
    let Ok(parsed) = parse_file(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (sec, kv) in &parsed.sections {
        let Some(rest) = sec.strip_prefix("model.") else {
            continue;
        };
        let name = rest.trim_matches('"').to_string();
        let Some(url) = kv.get("url").or_else(|| kv.get("endpoint")) else {
            continue;
        };
        // **No family block here, and `effort` is not a fleet entry's to carry.**
        //
        // This cannot know the dialect -- it is reading a file, not talking to a
        // daemon -- and that costs nothing, because `effort` is rendered into the
        // PREFIX. Switching where turns go mid-session does not rebuild the prefix,
        // so an effort on a fleet entry would be a value that never took effect. The
        // family block is read where it can act: at daemon start, by `cli`.
        let profile = model_profile("", &name, Some(&path));
        let model = profile.model.clone().unwrap_or_else(|| name.clone());
        out.push(LocalModel {
            name,
            url: url.clone(),
            model,
            profile,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The precedence the file promises**: the family block is the floor, the
    /// alias block overrides it KEY BY KEY, and the alias does not replace it.
    #[test]
    fn the_alias_block_overrides_the_family_block_key_by_key() {
        let f = tmp_providers(
            "the_alias_block_overrides",
            "[model.qwen]\neffort = \"low\"\ntemperature = 1.0\ntop_k = 20\n\n\
             [model.\"qwen-3.8-27b\"]\ntemperature = 0.7\npresence_penalty = 1.5\n",
        );
        let p = model_profile("qwen", "qwen-3.8-27b", Some(&f));
        assert_eq!(p.effort.as_deref(), Some("low"), "from the family block");
        assert_eq!(
            p.sampling.get("temperature").unwrap().as_f64(),
            Some(0.7),
            "the alias wins on a key both set"
        );
        assert_eq!(
            p.sampling.get("top_k").unwrap().as_i64(),
            Some(20),
            "a key only the family set survives -- the alias overrides, it does not replace"
        );
        assert_eq!(
            p.sampling.get("presence_penalty").unwrap().as_f64(),
            Some(1.5),
            "a key only the alias set is there"
        );
        assert!(p.unknown.is_empty(), "{:?}", p.unknown);
    }

    /// **An empty profile and a profile saying `temperature = 0.0` are different
    /// facts.** One defers to the built-in and the other is an instruction, and a
    /// caller that cannot tell them apart cannot implement "flag beats file beats
    /// built-in" at all.
    #[test]
    fn no_block_is_empty_rather_than_a_zero() {
        let f = tmp_providers("no_block_is_empty", "[deepseek]\nkey = \"sk-x\"\n");
        let p = model_profile("qwen", "qwen-3.8-27b", Some(&f));
        assert!(p.sampling.is_empty());
        assert_eq!(p.effort, None);

        let g = tmp_providers(
            "an_explicit_zero",
            "[model.\"qwen-3.8-27b\"]\ntemperature = 0.0\n",
        );
        let q = model_profile("qwen", "qwen-3.8-27b", Some(&g));
        assert_eq!(q.sampling.get("temperature").unwrap().as_f64(), Some(0.0));
        assert!(!q.sampling.is_empty(), "a zero is something the file said");
    }

    /// **A typo is loud.** The whole defect this reader closes is a number the file
    /// accepted and nothing read, so the reader must not reproduce it one line down
    /// for a misspelling.
    #[test]
    fn a_key_that_is_not_sampling_is_carried_rather_than_dropped() {
        let f = tmp_providers(
            "a_typo_is_loud",
            "[model.\"qwen-3.8-27b\"]\ntemperatuer = 0.7\ntop_p = \"warm\"\n",
        );
        let p = model_profile("qwen", "qwen-3.8-27b", Some(&f));
        assert!(p.sampling.is_empty(), "neither line is sampling");
        assert!(
            p.unknown.iter().any(|u| u.contains("temperatuer")),
            "the misspelling is named: {:?}",
            p.unknown
        );
        assert!(
            p.unknown.iter().any(|u| u.contains("top_p") && u.contains("not a number")),
            "a known key with an unreadable value says which it was: {:?}",
            p.unknown
        );
    }

    /// **A fleet model is a block with an address**, and a block without one is a
    /// sampling profile rather than somewhere a person can switch to.
    #[test]
    fn only_a_block_with_an_address_is_offered_as_a_fleet_model() {
        let f = tmp_providers(
            "local_models",
            "[model.qwen]\neffort = \"low\"\n\n\
             [model.\"dense78\"]\nurl = \"http://192.168.1.78:8082\"\n\
             model = \"qwen-3.8-27b\"\ntemperature = 0.7\n\n\
             [model.\"qwen-3.8-flash-next\"]\npresence_penalty = 0.5\n",
        );
        let fleet = local_models(Some(&f));
        assert_eq!(fleet.len(), 1, "{fleet:?}");
        let m = &fleet[0];
        assert_eq!(m.name, "dense78");
        assert_eq!(m.url, "http://192.168.1.78:8082");
        assert_eq!(
            m.model, "qwen-3.8-27b",
            "the served alias, not the block's label"
        );
        assert_eq!(m.profile.sampling.get("temperature").unwrap().as_f64(), Some(0.7));
    }

    /// The block name is the alias when the block did not say otherwise, because
    /// that is what a profile for one served model wants and it should not have to
    /// repeat its own name.
    #[test]
    fn a_fleet_block_named_for_the_weights_needs_no_model_line() {
        let f = tmp_providers(
            "fleet_named_for_weights",
            "[model.\"glm-5.3-flash\"]\nurl = \"http://192.168.1.76:8080\"\n",
        );
        let fleet = local_models(Some(&f));
        assert_eq!(fleet.len(), 1);
        assert_eq!(fleet[0].model, "glm-5.3-flash");
    }

    fn tmp_providers(tag: &str, body: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("letibot-prof-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("providers.toml");
        std::fs::write(&f, body).unwrap();
        f
    }

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
