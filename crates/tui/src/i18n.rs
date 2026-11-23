//! The head's own words, in the reader's language.
//!
//! English is the source and is compiled in; a second language is compiled in too, as TOML keyed by
//! a stable id. A lookup that a language does not carry falls back to English and never to the id,
//! so a half-translated release reads as English in the gaps rather than as `card.deny`.
//!
//! What is here is only the head's own interface: a status word, a card's fixed sentence, a pane
//! header. Nothing the model is sent, nothing a store row holds, no log line, no error code, no
//! command name and no config key is translated - those are a machine's or a transcript's, and a
//! translated copy of one is a different string, not a translation.
//!
//! A message is `id = "text"`, with `{name}` for a value the caller supplies. Russian needs three
//! plural forms where English needs two, so a count is one id per form - `rows.one`,
//! `rows.few`, `rows.many` - and [`count`] picks one.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// A language this build carries. `En` is the source: every id exists there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lang {
    En,
    Ru,
}

impl Lang {
    /// Every language, English first.
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Ru];

    /// The code `[ui] language` and `LANG` spell it with.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
        }
    }

    /// The language's own name, for `/language` and for a refusal: `English`, `Русский`.
    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Ru => "Русский",
        }
    }

    /// The language's catalog file, compiled in.
    fn text(self) -> &'static str {
        match self {
            Lang::En => include_str!("../messages/en.toml"),
            Lang::Ru => include_str!("../messages/ru.toml"),
        }
    }

    /// A code, or the head of a locale: `ru`, `ru_RU`, `ru-RU.UTF-8`. `None` for a word that
    /// names no language this build has - a region is dropped, since no catalog is per-region.
    pub fn parse(word: &str) -> Option<Lang> {
        let word = word.trim().split(['.', '@']).next().unwrap_or("");
        let head = word
            .split(['_', '-'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        Lang::ALL.into_iter().find(|l| l.code() == head)
    }

    /// The plural forms this language distinguishes, in the order [`count`] tries them. The tests'
    /// map of what a catalog must carry: nothing the head draws reads this.
    #[cfg(test)]
    fn forms(self) -> &'static [&'static str] {
        match self {
            Lang::En => &["one", "other"],
            Lang::Ru => &["one", "few", "many"],
        }
    }

    /// Which form a count takes here.
    fn form(self, n: usize) -> &'static str {
        match self {
            // Russian: one, few (2-4), many - with the teens the exception the language makes.
            Lang::Ru => {
                let (last, two) = (n % 10, n % 100);
                match (last, two) {
                    (1, 11) => "many",
                    (_, 11) | (_, 12) | (_, 13) | (_, 14) => "many",
                    (1, _) => "one",
                    (2..=4, _) => "few",
                    _ => "many",
                }
            }
            Lang::En => {
                if n == 1 {
                    "one"
                } else {
                    "other"
                }
            }
        }
    }

    /// This language's messages, parsed once. A malformed line and a repeated id are problems, and
    /// [`problems`] is where the tests and `/language` find them.
    fn catalog(self) -> &'static Catalog {
        static EN: OnceLock<Catalog> = OnceLock::new();
        static RU: OnceLock<Catalog> = OnceLock::new();
        let slot = match self {
            Lang::En => &EN,
            Lang::Ru => &RU,
        };
        slot.get_or_init(|| parse(self, self.text()))
    }
}

/// One language's messages, and what could not be read.
pub struct Catalog {
    pub messages: BTreeMap<String, String>,
    pub problems: Vec<String>,
}

/// The catalog's grammar: `# comment`, `id = "text"`, blank lines. Flat on purpose - a table would
/// be a second thing to get right, and a message id is already a name with dots in it.
fn parse(lang: Lang, text: &str) -> Catalog {
    let mut messages = BTreeMap::new();
    let mut problems = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let at = n + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            problems.push(format!("{}: line {at}: not `id = \"text\"`", lang.code()));
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim();
        let Some(body) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
            problems.push(format!(
                "{}: line {at}: `{key}` is not a quoted string",
                lang.code()
            ));
            continue;
        };
        if key.is_empty() {
            problems.push(format!("{}: line {at}: no id", lang.code()));
            continue;
        }
        if messages.contains_key(&key) {
            problems.push(format!("{}: line {at}: `{key}` is said twice", lang.code()));
            continue;
        }
        match unescape(body) {
            Ok(body) => {
                messages.insert(key, body);
            }
            Err(e) => problems.push(format!("{}: line {at}: `{key}`: {e}", lang.code())),
        }
    }
    Catalog { messages, problems }
}

/// `\"`, `\\`, `\n` and `\t`; anything else after a backslash is refused rather than guessed.
fn unescape(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => return Err(format!("`\\{other}` is not an escape")),
            None => return Err("a trailing backslash".into()),
        }
    }
    Ok(out)
}

/// What could not be read in any shipped catalog: empty in a released build, and a test's subject.
pub fn problems() -> Vec<String> {
    Lang::ALL
        .into_iter()
        .flat_map(|l| l.catalog().problems.clone())
        .collect()
}

thread_local! {
    /// The language in force in this thread. A thread's own, so a test that switches to Russian
    /// cannot colour a test drawing beside it - the same bargain `rano::theme` makes.
    static ACTIVE: std::cell::Cell<Lang> = const { std::cell::Cell::new(Lang::En) };
}

/// Make `lang` the language every drawing consults.
pub fn set_active(lang: Lang) {
    ACTIVE.with(|a| a.set(lang));
}

/// The language in force.
pub fn active() -> Lang {
    ACTIVE.with(|a| a.get())
}

/// The id's text in the language in force, else in English, else the id itself (which no shipped
/// catalog does: `every_id_exists_in_english` is the test that keeps that true).
pub fn t(key: &str) -> &str {
    let lang = active();
    let catalog = lang.catalog();
    if let Some(text) = catalog.messages.get(key) {
        return text;
    }
    if lang != Lang::En
        && let Some(text) = Lang::En.catalog().messages.get(key)
    {
        return text;
    }
    // A caller with a key no catalog has. The id is the only thing left to say, and the tests
    // above are what make it unreachable in a released build.
    key
}

/// [`t`] with the message's `{name}` slots filled. A slot the message does not name costs nothing;
/// a slot it names and the caller does not supply is left as it was written, since a wrong value
/// is worse than a visible placeholder.
pub fn tf(key: &str, vars: &[(&str, &str)]) -> String {
    let mut out = t(key).to_string();
    for (name, value) in vars {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// A count, in the language's own plural form: `1 строка`, `3 строки`, `12 строк`. `base` is the
/// id of the sentence with `{n}` in it; a language that carries no form falls back to `base`, and
/// then to English's.
pub fn count(base: &str, n: usize) -> String {
    let lang = active();
    let form = lang.form(n);
    let key = format!("{base}.{form}");
    let say = |l: Lang| -> Option<&'static str> {
        let c = l.catalog();
        c.messages
            .get(&key)
            .or_else(|| c.messages.get(base))
            .map(String::as_str)
    };
    let text = say(lang)
        .or_else(|| (lang != Lang::En).then(|| say(Lang::En)).flatten())
        .unwrap_or(base);
    text.replace("{n}", &n.to_string())
}

/// A language id that names nothing: the caller says so and keeps English.
pub fn unknown(word: &str) -> Option<String> {
    let word = word.trim();
    if word.is_empty() || word == "auto" || Lang::parse(word).is_some() {
        return None;
    }
    let codes: Vec<&str> = Lang::ALL.iter().map(|l| l.code()).collect();
    Some(format!(
        "`{word}` is not a language this build has — {} and `auto` (the environment's LC_ALL / \
         LC_MESSAGES / LANG) are the ones it knows",
        codes.join(", ")
    ))
}

/// The language a config setting and the environment agree on, and a sentence when the setting
/// named nothing this build has (English is used and the caller says why).
pub fn choose(configured: Option<&str>, env: Option<&str>) -> (Lang, Option<String>) {
    let configured = configured.unwrap_or("").trim();
    if configured.is_empty() || configured == "auto" {
        return (env.and_then(Lang::parse).unwrap_or(Lang::En), None);
    }
    match Lang::parse(configured) {
        Some(l) => (l, None),
        None => (Lang::En, unknown(configured)),
    }
}

/// What the environment says, in the order the C library and every shell consult it: `LC_ALL`,
/// then `LC_MESSAGES`, then `LANG`. `C` and `POSIX` name no language.
pub fn from_env() -> Option<String> {
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        match std::env::var(name) {
            Ok(v) if !v.trim().is_empty() && v.trim() != "C" && v.trim() != "POSIX" => {
                return Some(v);
            }
            _ => continue,
        }
    }
    None
}

/// The codes, for a listing or a refusal: `en, ru`.
pub fn shipped() -> String {
    Lang::ALL
        .iter()
        .map(|l| l.code())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every id this build has in the languages whose text starts with `prefix`, in the language in
/// force with English as the gap filler: what a guest pane is handed as its `words`, since a wasm
/// layer cannot call [`t`] itself.
pub fn prefixed(prefix: &str) -> BTreeMap<String, String> {
    Lang::En
        .catalog()
        .messages
        .keys()
        .filter(|k| k.starts_with(prefix))
        .map(|k| (k.clone(), t(k).to_string()))
        .collect()
}

/// Every id this build has, and every language's text for it - the shape the tests read.
pub fn all_ids() -> Vec<&'static str> {
    Lang::En
        .catalog()
        .messages
        .keys()
        .map(String::as_str)
        .collect()
}

/// Whether `key` is a plural form (`rows.few`) rather than a message id of its own.
pub fn plural_base(key: &str) -> Option<&str> {
    let (base, form) = key.rsplit_once('.')?;
    ["one", "few", "many", "other"]
        .contains(&form)
        .then_some(base)
}

#[cfg(test)]
mod tests;
