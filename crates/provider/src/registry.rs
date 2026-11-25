//! The doors this box knows: the built-ins, changed or added to by `[provider.<name>]` entries in
//! providers.toml, plus one door per base-URL variable (`ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL`).
//!
//! ```toml
//! [provider.myproxy]
//! protocol = "chat-completions"   # anthropic-messages | responses | genai
//! url = "http://127.0.0.1:4000/v1/chat/completions"
//! key_env = "MYPROXY_API_KEY"     # or key = "…"
//! model = "qwen-coder"
//! window = 131072                 # optional, when no catalogue carries it
//! fixed = ["temperature"]         # sampling keys the endpoint refuses
//! aliases = ["proxy"]
//! effort_field = "reasoning_effort"   # with effort_levels = ["off", "low", "high", "max"]
//! max_tokens_field = "max_completion_tokens"
//! reasoning_echo = "always"       # tools | always | never
//! reasoning_required = true       # tool-call messages carry reasoning_content, empty if none
//! tool_schema = "typed-enum"      # plain | typed-enum
//! json_object = true
//! cache_key = true
//! client_gate = true              # send our User-Agent; a 403 is explained as the client gate
//! thinking_locked = ["3.1"]       # models that refuse `thinking: disabled`, by substring
//! ```
//!
//! A door is a `&'static Preset`, as the clients take it. One built from config is interned: the
//! same entry read twice is the same preset, so re-reading the file leaks nothing new.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use crate::effort::Effort;
use crate::presets::{
    ALL, Compat, EffortField, Origin, Preset, Protocol, ReasoningEcho, ToolSchema,
};

/// Every key a `[provider.<name>]` entry may set.
const KEYS: &[&str] = &[
    "protocol",
    "url",
    "key_env",
    "key",
    "model",
    "catalogue",
    "thinking",
    "fixed",
    "aliases",
    "window",
    "usage",
    "effort_field",
    "effort_levels",
    "max_tokens_field",
    "reasoning_echo",
    "reasoning_required",
    "tool_schema",
    "json_object",
    "cache_key",
    "client_gate",
    "thinking_locked",
];

/// Sampling keys `fixed` may name: the ones a cloud request carries.
const SAMPLING: &[&str] = &[
    "temperature",
    "top_p",
    "n",
    "presence_penalty",
    "frequency_penalty",
    "seed",
    "max_tokens",
];

/// The protocols by their config spelling.
const PROTOCOLS: &[(&str, Protocol)] = &[
    ("chat-completions", Protocol::ChatCompletions),
    ("anthropic-messages", Protocol::AnthropicMessages),
    ("responses", Protocol::Responses),
    ("genai", Protocol::GenAi),
];

/// One door, or the sentence refusing its entry, with the extra names it answers to.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    door: Result<&'static Preset, String>,
    aliases: Vec<String>,
}

/// The doors as read once: built-ins first (each changed by its entry, if any), then env doors,
/// then the file's own.
#[derive(Debug, Clone)]
pub struct Registry {
    entries: Vec<Entry>,
}

impl Registry {
    /// Read the file (`None` is `~/.config/letibot/providers.toml`) and the process environment.
    pub fn read(file: Option<&Path>) -> Registry {
        Registry::read_with(file, &|v| std::env::var(v).ok())
    }

    /// [`Registry::read`] with the environment as a function, so a test need not set variables.
    pub fn read_with(file: Option<&Path>, env: &dyn Fn(&str) -> Option<String>) -> Registry {
        // An unreadable file is the built-ins: the readers of its other sections report it.
        let (path, sections) = crate::keys::provider_sections(file).unwrap_or_default();
        let mut entries = Vec::new();
        let mut used = vec![false; sections.len()];
        let mut entry_for = |name: &str| -> Option<&BTreeMap<String, String>> {
            let i = sections
                .iter()
                .position(|(n, _)| n == name || Preset::parse(n).is_ok_and(|p| p.name == name))?;
            used[i] = true;
            Some(&sections[i].1)
        };
        for base in ALL {
            let door = match entry_for(base.name) {
                Some(kv) => build(&path, base.name, Some(Spec::of(base)), kv, Origin::Changed),
                None => Ok(*base),
            };
            entries.push(Entry {
                name: base.name.to_string(),
                door,
                aliases: Vec::new(),
            });
        }
        for (name, spec) in env_doors(env) {
            let door = match (spec, entry_for(&name)) {
                (Ok(spec), Some(kv)) => build(&path, &name, Some(spec), kv, Origin::Changed),
                (Ok(spec), None) => Ok(intern(spec)),
                (Err(why), _) => Err(why),
            };
            entries.push(Entry {
                name,
                door,
                aliases: Vec::new(),
            });
        }
        for (i, (name, kv)) in sections.iter().enumerate() {
            if used[i] {
                continue;
            }
            entries.push(Entry {
                name: name.clone(),
                door: build(&path, name, None, kv, Origin::Config),
                aliases: kv
                    .get("aliases")
                    .map(|a| crate::keys::word_list(a))
                    .unwrap_or_default(),
            });
        }
        Registry { entries }
    }

    /// The door a name or alias calls: an entry first, then a built-in's own aliases. A refused
    /// entry is refused here by name; an unknown name lists every door there is.
    pub fn find(&self, name: &str) -> Result<&'static Preset, String> {
        let want = name.trim().to_ascii_lowercase();
        let hit = self
            .entries
            .iter()
            .find(|e| {
                e.name.eq_ignore_ascii_case(&want)
                    || e.aliases.iter().any(|a| a.eq_ignore_ascii_case(&want))
            })
            .or_else(|| {
                let builtin = Preset::parse(&want).ok()?;
                self.entries.iter().find(|e| e.name == builtin.name)
            });
        match hit {
            Some(e) => e.door.clone(),
            None => Err(format!(
                "`{want}` is not a provider this box knows; there are {}: {}",
                self.doors().len(),
                self.doors()
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Every door that can be used, in listing order.
    pub fn doors(&self) -> Vec<&'static Preset> {
        self.entries
            .iter()
            .filter_map(|e| e.door.as_ref().ok().copied())
            .collect()
    }

    /// Every refused entry, each naming the file and the key.
    pub fn faults(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|e| e.door.clone().err())
            .collect()
    }
}

/// The door `name` calls, with `file`'s entries over the built-ins.
pub fn lookup(name: &str, file: Option<&Path>) -> Result<&'static Preset, String> {
    Registry::read(file).find(name)
}

/// Every usable door, with `file`'s entries over the built-ins.
pub fn all(file: Option<&Path>) -> Vec<&'static Preset> {
    Registry::read(file).doors()
}

/// A preset's facts in owned form: what an entry changes, and what interning compares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Spec {
    name: String,
    url: String,
    key_env: String,
    protocol: Protocol,
    alt_envs: Vec<String>,
    fallback_model: String,
    thinking_field: Option<String>,
    catalogue_id: String,
    vendor_pick: bool,
    fixed_sampling: Vec<String>,
    /// The effort field and levels, owned: a config door's are leaked when interned.
    effort: Option<(Option<String>, Vec<Effort>)>,
    origin: Origin,
    window: Option<u64>,
    usage_url: Option<String>,
    compat: Compat,
    max_tokens_field: String,
    thinking_locked: Vec<String>,
}

impl Spec {
    fn of(p: &Preset) -> Spec {
        Spec {
            name: p.name.into(),
            url: p.url.into(),
            key_env: p.key_env.into(),
            protocol: p.protocol,
            alt_envs: p.alt_envs.iter().map(|s| s.to_string()).collect(),
            fallback_model: p.fallback_model.into(),
            thinking_field: p.thinking_field.map(str::to_string),
            catalogue_id: p.catalogue_id.into(),
            vendor_pick: p.vendor_pick,
            fixed_sampling: p.fixed_sampling.iter().map(|s| s.to_string()).collect(),
            effort: p
                .effort
                .map(|e| (e.field.map(str::to_string), e.levels.to_vec())),
            origin: p.origin,
            window: p.window,
            usage_url: p.usage_url.map(str::to_string),
            compat: p.compat,
            max_tokens_field: p.compat.max_tokens_field.into(),
            thinking_locked: p
                .compat
                .thinking_locked
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// One `[provider.<name>]` entry over `base` (a built-in or env door), or a door of its own.
fn build(
    file: &Path,
    name: &str,
    base: Option<Spec>,
    kv: &BTreeMap<String, String>,
    origin: Origin,
) -> Result<&'static Preset, String> {
    let at = |key: &str, why: String| format!("{}: [provider.{name}] {key}: {why}", file.display());
    if let Some(k) = kv.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(at(
            k,
            format!("not a key this reads; it takes {}", KEYS.join(", ")),
        ));
    }
    let own = base.is_none();
    let mut s = base.unwrap_or_else(|| Spec {
        name: name.to_string(),
        url: String::new(),
        key_env: format!(
            "{}_API_KEY",
            name.to_ascii_uppercase().replace(['-', '.'], "_")
        ),
        protocol: Protocol::ChatCompletions,
        alt_envs: Vec::new(),
        fallback_model: String::new(),
        thinking_field: None,
        catalogue_id: String::new(),
        vendor_pick: false,
        fixed_sampling: Vec::new(),
        effort: None,
        origin,
        window: None,
        usage_url: None,
        compat: Compat::PLAIN,
        max_tokens_field: Compat::PLAIN.max_tokens_field.into(),
        thinking_locked: Vec::new(),
    });
    // An env door stays marked by its variable, whatever the file adds.
    if !matches!(s.origin, Origin::Env(_)) {
        s.origin = origin;
    }
    match kv.get("protocol") {
        Some(v) => match PROTOCOLS.iter().find(|(w, _)| w == v) {
            Some((_, p)) => s.protocol = *p,
            None => {
                return Err(at(
                    "protocol",
                    format!(
                        "`{v}` is not one of {}",
                        PROTOCOLS
                            .iter()
                            .map(|(w, _)| *w)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
        },
        None if own => return Err(at("protocol", "missing; a new door names its wire".into())),
        None => {}
    }
    match kv.get("url") {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => s.url = u.clone(),
        Some(u) => return Err(at("url", format!("`{u}` is not an http(s) URL"))),
        None if own => return Err(at("url", "missing".into())),
        None => {}
    }
    match kv.get("model") {
        Some(m) if !m.trim().is_empty() => s.fallback_model = m.trim().to_string(),
        Some(_) => return Err(at("model", "empty".into())),
        None if own => return Err(at("model", "missing; the default model to send".into())),
        None => {}
    }
    if let Some(e) = kv.get("key_env") {
        s.key_env = e.trim().to_string();
        s.alt_envs.clear();
    }
    if let Some(c) = kv.get("catalogue") {
        s.catalogue_id = c.trim().to_string();
    }
    if let Some(t) = kv.get("thinking") {
        s.thinking_field = Some(t.trim().to_string()).filter(|t| !t.is_empty());
    }
    if let Some(f) = kv.get("fixed") {
        let keys = crate::keys::word_list(f);
        if let Some(bad) = keys.iter().find(|k| !SAMPLING.contains(&k.as_str())) {
            return Err(at(
                "fixed",
                format!(
                    "`{bad}` is not a sampling key; they are {}",
                    SAMPLING.join(", ")
                ),
            ));
        }
        s.fixed_sampling = keys;
    }
    if let Some(w) = kv.get("window") {
        match w.trim().parse::<u64>() {
            Ok(n) if n > 0 => s.window = Some(n),
            _ => return Err(at("window", format!("`{w}` is not a positive token count"))),
        }
    }
    match kv.get("usage").map(|u| u.trim()) {
        Some("") => s.usage_url = None,
        Some(u) if u.starts_with('/') || u.starts_with("http://") || u.starts_with("https://") => {
            s.usage_url = Some(u.to_string())
        }
        Some(u) => {
            return Err(at(
                "usage",
                format!("`{u}` is not a path or an http(s) URL"),
            ));
        }
        None => {}
    }
    quirks(&mut s, kv, &at)?;
    Ok(intern(s))
}

/// The effort and compat keys of one entry, over what `s` already holds.
fn quirks(
    s: &mut Spec,
    kv: &BTreeMap<String, String>,
    at: &dyn Fn(&str, String) -> String,
) -> Result<(), String> {
    let flag = |key: &str| -> Result<Option<bool>, String> {
        match kv.get(key).map(|v| v.trim()) {
            None => Ok(None),
            Some("true") => Ok(Some(true)),
            Some("false") => Ok(Some(false)),
            Some(v) => Err(at(key, format!("`{v}` is not true or false"))),
        }
    };
    let field = kv
        .get("effort_field")
        .map(|f| f.trim().to_string())
        .map(|f| (!f.is_empty()).then_some(f));
    let levels = match kv.get("effort_levels") {
        Some(l) => Some(
            crate::keys::word_list(l)
                .iter()
                .map(|w| Effort::parse(w).map_err(|why| at("effort_levels", why)))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        None => None,
    };
    if field.is_some() || levels.is_some() {
        let (old_field, old_levels) = s.effort.take().unwrap_or((None, Effort::ALL.to_vec()));
        let mut levels = levels.unwrap_or(old_levels);
        levels.sort();
        levels.dedup();
        s.effort = Some((field.unwrap_or(old_field), levels));
    }
    if let Some(f) = kv.get("max_tokens_field") {
        match f.trim() {
            "" => return Err(at("max_tokens_field", "empty".into())),
            f => s.max_tokens_field = f.to_string(),
        }
    }
    if let Some(e) = kv.get("reasoning_echo") {
        s.compat.reasoning_echo = match e.trim() {
            "tools" => ReasoningEcho::Tools,
            "always" => ReasoningEcho::Always,
            "never" => ReasoningEcho::Never,
            v => {
                return Err(at(
                    "reasoning_echo",
                    format!("`{v}` is not tools, always or never"),
                ));
            }
        };
    }
    if let Some(t) = kv.get("tool_schema") {
        s.compat.tool_schema = match t.trim() {
            "plain" => ToolSchema::Plain,
            "typed-enum" => ToolSchema::TypedEnum,
            v => {
                return Err(at(
                    "tool_schema",
                    format!("`{v}` is not plain or typed-enum"),
                ));
            }
        };
    }
    if let Some(b) = flag("reasoning_required")? {
        s.compat.reasoning_required = b;
    }
    if let Some(b) = flag("json_object")? {
        s.compat.json_object = b;
    }
    if let Some(b) = flag("cache_key")? {
        s.compat.cache_key = b;
    }
    if let Some(b) = flag("client_gate")? {
        s.compat.client_gate = b;
    }
    if let Some(l) = kv.get("thinking_locked") {
        s.thinking_locked = crate::keys::word_list(l);
    }
    Ok(())
}

/// The base-URL doors, Claude Code style. Without a base URL these variables keep meaning the
/// built-ins' keys, and no door is defined.
fn env_doors(env: &dyn Fn(&str) -> Option<String>) -> Vec<(String, Result<Spec, String>)> {
    let set = |v: &str| {
        env(v)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let mut out = Vec::new();
    if let Some(base) = set("ANTHROPIC_BASE_URL") {
        out.push((
            "env-anthropic".to_string(),
            Ok(Spec {
                name: "env-anthropic".into(),
                url: endpoint(&base, "/v1/messages", "/messages"),
                key_env: "ANTHROPIC_API_KEY".into(),
                protocol: Protocol::AnthropicMessages,
                alt_envs: vec!["ANTHROPIC_AUTH_TOKEN".into()],
                fallback_model: set("ANTHROPIC_MODEL")
                    .unwrap_or_else(|| crate::presets::ANTHROPIC.fallback_model.into()),
                thinking_field: None,
                catalogue_id: String::new(),
                vendor_pick: false,
                fixed_sampling: Vec::new(),
                effort: None,
                origin: Origin::Env("ANTHROPIC_BASE_URL"),
                window: None,
                usage_url: None,
                compat: Compat::PLAIN,
                max_tokens_field: Compat::PLAIN.max_tokens_field.into(),
                thinking_locked: Vec::new(),
            }),
        ));
    }
    if let Some(base) = set("OPENAI_BASE_URL") {
        let protocol = match set("LETIBOT_OPENAI_PROTOCOL").as_deref() {
            None | Some("chat-completions") => Ok(Protocol::ChatCompletions),
            Some("responses") => Ok(Protocol::Responses),
            Some(other) => Err(format!(
                "$LETIBOT_OPENAI_PROTOCOL: `{other}` is not chat-completions or responses"
            )),
        };
        let spec = protocol.map(|protocol| Spec {
            name: "env-openai".into(),
            url: match protocol {
                Protocol::Responses => endpoint(&base, "/responses", "/responses"),
                _ => endpoint(&base, "/chat/completions", "/chat/completions"),
            },
            key_env: "OPENAI_API_KEY".into(),
            protocol,
            alt_envs: Vec::new(),
            fallback_model: set("OPENAI_MODEL")
                .unwrap_or_else(|| crate::presets::OPENAI.fallback_model.into()),
            thinking_field: None,
            catalogue_id: String::new(),
            vendor_pick: false,
            fixed_sampling: Vec::new(),
            effort: None,
            origin: Origin::Env("OPENAI_BASE_URL"),
            window: None,
            usage_url: None,
            compat: Compat::PLAIN,
            max_tokens_field: Compat::PLAIN.max_tokens_field.into(),
            thinking_locked: Vec::new(),
        });
        out.push(("env-openai".to_string(), spec));
    }
    out
}

/// A base URL as the endpoint the client posts to: the clients post the URL they are given.
fn endpoint(base: &str, append: &str, already: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with(already) {
        base.to_string()
    } else {
        format!("{base}{append}")
    }
}

/// The one `&'static Preset` for these facts: a built-in's own when nothing changed it, else
/// the one made the first time these facts were seen.
fn intern(spec: Spec) -> &'static Preset {
    if let Some(b) = ALL.iter().find(|p| Spec::of(p) == spec) {
        return b;
    }
    static MADE: Mutex<Vec<(Spec, &'static Preset)>> = Mutex::new(Vec::new());
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, p)) = made.iter().find(|(s, _)| *s == spec) {
        return p;
    }
    let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
    let list = |v: &[String]| -> &'static [&'static str] {
        Box::leak(
            v.iter()
                .map(|s| leak(s))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    };
    let p: &'static Preset = Box::leak(Box::new(Preset {
        name: leak(&spec.name),
        url: leak(&spec.url),
        key_env: leak(&spec.key_env),
        protocol: spec.protocol,
        alt_envs: list(&spec.alt_envs),
        fallback_model: leak(&spec.fallback_model),
        thinking_field: spec.thinking_field.as_deref().map(leak),
        catalogue_id: leak(&spec.catalogue_id),
        vendor_pick: spec.vendor_pick,
        fixed_sampling: list(&spec.fixed_sampling),
        effort: spec.effort.as_ref().map(|(field, levels)| EffortField {
            field: field.as_deref().map(leak),
            levels: Box::leak(levels.clone().into_boxed_slice()),
        }),
        origin: spec.origin,
        window: spec.window,
        usage_url: spec.usage_url.as_deref().map(leak),
        // A `[provider.NAME]` entry in providers.toml defines a door and never a picture cap: the
        // cap is leticode.toml's `[provider] max_images`, handed to the client per session.
        budget: crate::budget::Budget::NONE,
        compat: Compat {
            max_tokens_field: leak(&spec.max_tokens_field),
            thinking_locked: list(&spec.thinking_locked),
            ..spec.compat
        },
    }));
    made.push((spec, p));
    p
}

/// The models a door's endpoint lists, from its own `GET …/models`: `None` while the first
/// fetch is in flight. Fetched once per daemon in the background, so a listing never waits.
pub fn listed_models(
    preset: &'static Preset,
    creds: &crate::keys::Credentials,
) -> Option<Vec<String>> {
    enum State {
        Pending,
        Done(Vec<String>),
    }
    static CACHE: Mutex<BTreeMap<String, State>> = Mutex::new(BTreeMap::new());
    let url = creds.url.clone().unwrap_or_else(|| preset.url.to_string());
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    match cache.get(&url) {
        Some(State::Done(m)) => return Some(m.clone()),
        Some(State::Pending) => return None,
        None => {}
    }
    cache.insert(url.clone(), State::Pending);
    let creds = creds.clone();
    std::thread::spawn(move || {
        // A door that cannot list is an empty list, asked once: the card then shows its default.
        let models = fetch_models(preset, &creds).unwrap_or_default();
        CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url, State::Done(models));
    });
    None
}

/// One blocking `GET …/models` on the door's own listing, sorted.
pub fn fetch_models(
    preset: &Preset,
    creds: &crate::keys::Credentials,
) -> Result<Vec<String>, String> {
    let url = creds.url.clone().unwrap_or_else(|| preset.url.to_string());
    let base = match preset.protocol {
        Protocol::ChatCompletions => url.trim_end_matches("/chat/completions"),
        Protocol::Responses => url.trim_end_matches("/responses"),
        Protocol::AnthropicMessages => url.trim_end_matches("/messages"),
        Protocol::GenAi => url.trim_end_matches('/'),
    };
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build(),
    );
    let req = agent.get(format!("{base}/models"));
    let req = match preset.protocol {
        Protocol::AnthropicMessages => req
            .header("x-api-key", &creds.key)
            .header("anthropic-version", "2023-06-01"),
        Protocol::GenAi => req.header("x-goog-api-key", &creds.key),
        _ => req.header("Authorization", &format!("Bearer {}", creds.key)),
    };
    let body: serde_json::Value = req
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    let mut out: Vec<String> = match preset.protocol {
        Protocol::GenAi => body["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["name"].as_str())
            .map(|n| n.trim_start_matches("models/").to_string())
            .collect(),
        _ => body["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["id"].as_str())
            .map(str::to_string)
            .collect(),
    };
    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests;
