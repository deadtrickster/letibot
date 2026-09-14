//! Where a key comes from, and where it does not.
//!
//! Order: `--api-key` (a flag, for a one-off), the provider's environment
//! variable, then `~/.config/letibot/providers.toml`. The file also carries the
//! price table. Nothing else is read — there is no fallback to another
//! provider's key, and a missing key is a refusal that names the variable and
//! the file, because "unauthorized" from the provider three seconds later names
//! neither.

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
    Err(KeyError::Missing {
        provider: preset.name.into(),
        env: preset.key_env.into(),
        file,
    })
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
