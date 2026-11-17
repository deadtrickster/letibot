//! Whether a provider's account can serve at all, asked once at a start so a plan with no balance
//! does not spend the first turn learning it. One capped request, cached per account. A failed
//! probe is [`Account::Unknown`], which refuses nothing.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use letibot_provider::keys::Credentials;
use letibot_provider::presets::Preset;

/// How long a verdict stands. A wallet is not a window: five minutes is short enough that a top-up
/// is seen and long enough that a fleet of starts asks once.
pub const FRESH: Duration = Duration::from_secs(300);

/// What one probe said about an account.
#[derive(Debug, Clone, PartialEq)]
pub enum Account {
    /// The door answered, and nothing on it says the account cannot serve.
    Ready,
    /// The door named a reason it cannot: the sentence a start is refused with.
    CannotServe(String),
    /// The probe did not answer, or this preset has no door to ask. Never a refusal.
    Unknown(String),
}

/// The words a plan uses for *there is no money on this account*, besides the code below: they are
/// the rest of the trade's prose, so a provider that says it in words is caught too.
const NO_MONEY: &[&str] = &[
    "insufficient balance",
    "no balance",
    "not enough balance",
    "余额不足",
];

/// z.ai's code for a plan with no balance on it.
const NO_MONEY_CODE: i64 = 1113;

/// The verdicts this process has, by account: `provider/key-prefix`.
fn cache() -> &'static Mutex<BTreeMap<String, (Instant, Account)>> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, (Instant, Account)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The kept verdict for `at`, when it is younger than [`FRESH`].
fn kept(at: &str) -> Option<Account> {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(at)
        .filter(|(when, _)| when.elapsed() < FRESH)
        .map(|(_, said)| said.clone())
}

/// Keep one verdict for `at`.
fn keep(at: &str, said: Account) {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(at.to_string(), (Instant::now(), said));
}

/// Why this account cannot serve, when it cannot: `None` is *serve it, or say nothing*. The answer
/// is cached, so a fleet of starts in one process asks the plan once.
pub fn refusal(name: &str, model: Option<&str>, key: Option<&str>) -> Option<String> {
    match check(name, model, key) {
        Account::CannotServe(said) => Some(said),
        _ => None,
    }
}

/// The probe, from the cache when it is fresh: resolve the preset's door and ask it whether this
/// account can serve.
pub fn check(name: &str, model: Option<&str>, key: Option<&str>) -> Account {
    let file = None;
    let Ok(preset) = letibot_provider::registry::lookup(name, file) else {
        return Account::Unknown(format!("`{name}` is not a provider this build knows"));
    };
    let Ok(creds) = letibot_provider::keys::resolve(preset, key, file) else {
        return Account::Unknown(format!("`{name}` has no key to ask with"));
    };
    let Some(url) = letibot_provider::usage::url(preset, &creds)
        .or_else(|| plan_door(creds.url.as_deref().unwrap_or(preset.url)))
    else {
        // Nothing to ask. A provider with no plan door is not an account this can judge, and a
        // guess about one is worse than the first call's own refusal.
        return Account::Unknown(format!("`{name}` declares no plan endpoint"));
    };
    let at = format!("{}/{}", name, creds.key.chars().take(8).collect::<String>());
    if let Some(said) = kept(&at) {
        return said;
    }
    let said = ask(&url, preset, &creds, model);
    keep(&at, said.clone());
    said
}

/// The door a preset that declares no `usage_url` is asked on, when its host is one whose plan
/// publishes a quota report: z.ai's answers on `api.z.ai` and `open.bigmodel.cn`, on the same key
/// as the chat door - and a `glm` account with nothing on it is the 1113 this exists for.
fn plan_door(chat: &str) -> Option<String> {
    let (scheme, rest) = chat.split_once("://")?;
    let host = rest.split('/').next().filter(|h| !h.is_empty())?;
    matches!(host, "api.z.ai" | "open.bigmodel.cn")
        .then(|| format!("{scheme}://{host}{}", letibot_provider::usage::ZAI_QUOTA))
}

/// One request to the door, judged. A door that does not answer, or answers with something this
/// cannot read, is no fact about the account.
fn ask(url: &str, preset: &Preset, creds: &Credentials, model: Option<&str>) -> Account {
    match letibot_provider::usage_poll::body(url, creds) {
        Some(body) => judge(preset.name, model, &body),
        None => Account::Unknown(format!("`{}`'s plan endpoint did not answer", preset.name)),
    }
}

/// What one answer of the door says about the account. Public for the test that reads a body
/// without a network, and because this is the whole rule.
pub fn judge(name: &str, model: Option<&str>, body: &serde_json::Value) -> Account {
    let flat = body.to_string().to_lowercase();
    let coded = [&body["error"]["code"], &body["code"]]
        .iter()
        .any(|c| c.as_i64() == Some(NO_MONEY_CODE) || c.as_str() == Some("1113"));
    let worded = NO_MONEY.iter().any(|w| flat.contains(w));
    if coded || worded {
        let which = match model {
            Some(m) => format!("{name}/{m}"),
            None => name.to_string(),
        };
        let said = body["error"]["message"]
            .as_str()
            .or_else(|| body["msg"].as_str())
            .or_else(|| body["error"].as_str())
            .unwrap_or("the plan says the account cannot serve");
        return Account::CannotServe(format!(
            "`{which}` cannot serve this account: {said}. Nothing was started and no turn was \
             spent; top the account up or start on another model."
        ));
    }
    Account::Ready
}

#[cfg(test)]
mod tests;
