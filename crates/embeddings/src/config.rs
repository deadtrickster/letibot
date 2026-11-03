//! The one key that decides where a vector comes from.
//!
//! ```toml
//! [embeddings]
//! mode = "off"                      # lexical only, and the default
//! mode = "internal"                 # candle, here, with the built-in model
//! model = "<dir>"                   # ... or a directory this build can read
//! download = false                  # ... and never fetch the built-in model
//! mode = "http://host:8080"         # an OpenAI-compatible /v1/embeddings
//! model = "text-embedding-3-small"  # ... and the name it knows
//! builtin_repo = "owner/name"       # ... and fetch the built-in model from somewhere else
//! builtin_revision = "<40 hex>"     # ... at another commit of it, with
//! builtin_sha256 = { "config.json" = "<64 hex>", ... }   # ... a hash per file, all three
//! ```
//!
//! Three values for one key, because they are three answers to one question: who turns
//! text into a vector. A key that does nothing is refused by name rather than ignored,
//! because that is the config defect this tree has already paid for once.
//!
//! `download` is the fourth key and the only one that is not about the answer: it says whether
//! a box with `mode = "internal"` and no `model` may fetch the built-in one, and `false` is a
//! box that must not reach the network.
//!
//! The three `builtin_` keys are the pin, and the one rule is that a commit is never written
//! without its hashes: a new revision with the old ones can never verify.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// Where a text becomes a vector.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    /// Lexical only: nothing is embedded and every caller falls back to its own terms.
    #[default]
    Off,
    /// candle, in this process. `model` is the user's own directory, or `None` for the
    /// built-in one.
    Internal { model: Option<PathBuf> },
    /// An OpenAI-compatible endpoint, and the model name it answers to.
    Remote { url: String, model: String },
}

impl Mode {
    /// What a disclosure or a log line calls this mode.
    pub fn name(&self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Internal { .. } => "internal",
            Mode::Remote { .. } => "url",
        }
    }

    /// Is anything embedded at all? A caller asks this to skip the whole tier.
    pub fn is_off(&self) -> bool {
        matches!(self, Mode::Off)
    }
}

/// The built-in model's pin as a section may change it: the repository the files come from,
/// the commit they are pinned at, and a hash for each. Empty is the table this build ships, and
/// the parser refuses a pin written in half, so a pin built from this is shipped or whole.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PinOverride {
    /// The repository's `owner/name`, when the section named one to fetch from instead.
    pub repo: Option<String>,
    /// The commit, when the section pinned another one.
    pub revision: Option<String>,
    /// A sha-256 per file, keyed by the file's own name; empty is the shipped hashes.
    pub sha256: BTreeMap<String, String>,
}

/// The whole `[embeddings]` section, and the answer to "is there a second tier".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    /// Whether a box may fetch the built-in model. `false` is for a box that must not reach
    /// the network: the model is then whatever is already on disk, and nothing is fetched.
    pub download: bool,
    /// The built-in model's pin, as far as the section changed it.
    pub builtin: PinOverride,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            mode: Mode::Off,
            download: true,
            builtin: PinOverride::default(),
        }
    }
}

/// The model name an endpoint is asked for when the file does not name one: the name
/// every OpenAI-compatible server documents, so a local llama-server needs no line.
pub const DEFAULT_REMOTE_MODEL: &str = "text-embedding-3-small";

/// Why a `[embeddings]` section was refused. Every variant names the key or the word,
/// because a config that is wrong is a config somebody has to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The TOML itself.
    Toml(String),
    /// A key under `[embeddings]` that this version does not know.
    UnknownKey(String),
    /// `mode` that is neither `off`, `internal` nor a URL.
    UnknownMode(String),
    /// A URL whose scheme this tree's client cannot speak.
    UnspeakableUrl(String),
    /// A `model` that would do nothing under the mode it is written under.
    ModelUnderOff,
    /// `model` under a URL mode whose name would be empty.
    NamelessRemoteModel,
    /// A pin key whose value is not the shape the key takes. `key` is the key or the table
    /// entry, so the sentence names what to fix.
    PinShape { key: String, why: String },
    /// A pin written in half, which could never verify.
    HalfPin(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Toml(e) => write!(f, "[embeddings]: {e}"),
            ConfigError::UnknownKey(k) => write!(
                f,
                "[embeddings] {k} is not a key this build knows; it reads `mode`, `model`, \
                 `download`, `builtin_repo`, `builtin_revision` and `builtin_sha256`"
            ),
            ConfigError::UnknownMode(m) => write!(
                f,
                "mode = \"{m}\" is not `off`, `internal`, or a URL; a typo here would otherwise \
                 turn the second tier off in silence"
            ),
            ConfigError::UnspeakableUrl(u) => write!(
                f,
                "mode = \"{u}\" is a URL this build cannot speak: the client is HTTP/1.1 with no \
                 TLS, so an endpoint is `http://host:port`"
            ),
            ConfigError::ModelUnderOff => write!(
                f,
                "[embeddings] model is written under mode = \"off\", where nothing is embedded; \
                 set mode = \"internal\" for a directory or a URL for an endpoint"
            ),
            ConfigError::NamelessRemoteModel => write!(
                f,
                "[embeddings] model names the model an endpoint answers to, and it is empty; \
                 an endpoint that wants none must still be named"
            ),
            ConfigError::PinShape { key, why } => write!(f, "[embeddings] {key}: {why}"),
            ConfigError::HalfPin(why) => write!(f, "[embeddings] {why}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The section as it is on disk. `deny_unknown_fields` is the point: a typo'd `mod`
/// would otherwise leave the tier off and say nothing.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    mode: Option<String>,
    model: Option<String>,
    download: Option<bool>,
    /// The pin keys, carried as `Value` so a value of the wrong shape is refused by the name the
    /// operator wrote rather than by serde's message about the table.
    builtin_repo: Option<toml::Value>,
    builtin_revision: Option<toml::Value>,
    builtin_sha256: Option<toml::Value>,
}

impl Settings {
    /// Read a whole `leticode.toml` - or anything else with an `[embeddings]` table. A
    /// file with no such table is [`Mode::Off`], which is the default until the take-up
    /// log says otherwise.
    pub fn from_toml(text: &str) -> Result<Settings, ConfigError> {
        let doc: toml::Value =
            toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))?;
        match doc.get("embeddings") {
            None => Ok(Settings::default()),
            Some(value) => {
                let table = value
                    .as_table()
                    .ok_or_else(|| ConfigError::Toml("embeddings is not a table".into()))?;
                Settings::from_table(&table.clone().try_into().unwrap_or_default())
            }
        }
    }

    /// Build from the section's own table, which is what the config reader hands over.
    pub fn from_table(table: &toml::Table) -> Result<Settings, ConfigError> {
        let raw: Raw = toml::Value::Table(table.clone())
            .try_into()
            .map_err(|e: toml::de::Error| unknown_key(&e, table))?;
        Settings::assemble(raw)
    }

    fn assemble(raw: Raw) -> Result<Settings, ConfigError> {
        let mode = raw.mode.unwrap_or_else(|| "off".into());
        let written = raw.model;
        let named = written.as_deref().map(str::trim).filter(|m| !m.is_empty());
        let mode = if mode == "off" {
            if written.is_some() {
                return Err(ConfigError::ModelUnderOff);
            }
            Mode::Off
        } else if mode == "internal" {
            Mode::Internal {
                model: named.map(PathBuf::from),
            }
        } else if mode.starts_with("https://") {
            return Err(ConfigError::UnspeakableUrl(mode));
        } else if mode.starts_with("http://") {
            let model = match (named, written.is_some()) {
                (Some(name), _) => name.to_string(),
                (None, true) => return Err(ConfigError::NamelessRemoteModel),
                (None, false) => DEFAULT_REMOTE_MODEL.to_string(),
            };
            Mode::Remote { url: mode, model }
        } else {
            return Err(ConfigError::UnknownMode(mode));
        };
        Ok(Settings {
            mode,
            download: raw.download.unwrap_or(true),
            builtin: pin_of(
                raw.builtin_repo.as_ref(),
                raw.builtin_revision.as_ref(),
                raw.builtin_sha256.as_ref(),
            )?,
        })
    }

    /// Read the section out of a file. A file that is not there is [`Mode::Off`]: a box
    /// that never wrote the key has not asked for a second tier.
    pub fn load(path: &std::path::Path) -> Result<Settings, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Settings::from_toml(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(ConfigError::Toml(format!("{}: {e}", path.display()))),
        }
    }
}

/// The three pin keys, each read in the shape it was written. A revision without the three
/// hashes is refused here and not at the fetch: the old hashes can never verify under a new
/// commit, so the fetch would refuse the very bytes the section asked to trust.
fn pin_of(
    repo: Option<&toml::Value>,
    revision: Option<&toml::Value>,
    hashes: Option<&toml::Value>,
) -> Result<PinOverride, ConfigError> {
    let repo = match repo {
        None => None,
        Some(value) => {
            let text = text_of("builtin_repo", value)?;
            if !is_repo(&text) {
                return Err(ConfigError::PinShape {
                    key: "builtin_repo".to_string(),
                    why: format!(
                        "{text:?} is not a repository id; write `builtin_repo = \"owner/name\"`"
                    ),
                });
            }
            Some(text)
        }
    };
    let revision = match revision {
        None => None,
        Some(value) => {
            let text = text_of("builtin_revision", value)?;
            if !filled(&text, 40) {
                return Err(ConfigError::PinShape {
                    key: "builtin_revision".to_string(),
                    why: format!(
                        "{text:?} is not a commit; a revision is 40 hexadecimal digits, and not \
                         the all-zero filler"
                    ),
                });
            }
            Some(text)
        }
    };
    let mut sha256 = BTreeMap::new();
    if let Some(value) = hashes {
        let table = value.as_table().ok_or_else(|| ConfigError::PinShape {
            key: "builtin_sha256".to_string(),
            why: format!(
                "is {}, not a table of `\"<file>\" = \"<sha-256>\"`: {}",
                kind(value),
                files()
            ),
        })?;
        for (file, value) in table {
            if !crate::model::FILES.contains(&file.as_str()) {
                return Err(ConfigError::PinShape {
                    key: format!("builtin_sha256.{file}"),
                    why: format!("is not a file of the checkpoint; it has {}", files()),
                });
            }
            let text = text_of(&format!("builtin_sha256.{file}"), value)?;
            if !filled(&text, 64) {
                return Err(ConfigError::PinShape {
                    key: format!("builtin_sha256.{file}"),
                    why: format!(
                        "{text:?} is not a sha-256; a hash is 64 hexadecimal digits, and not the \
                         all-zero filler"
                    ),
                });
            }
            sha256.insert(file.clone(), text);
        }
        if let Some(missing) = crate::model::FILES
            .iter()
            .find(|f| !sha256.contains_key(**f))
        {
            return Err(ConfigError::PinShape {
                key: "builtin_sha256".to_string(),
                why: format!(
                    "does not name {missing}, and a pin is a sha-256 for every file the \
                     checkpoint has: {}",
                    files()
                ),
            });
        }
    }
    if revision.is_some() && sha256.is_empty() {
        return Err(ConfigError::HalfPin(format!(
            "builtin_revision is written without builtin_sha256, and a new commit with the \
             shipped hashes can never verify; write a sha-256 for each of {}",
            files()
        )));
    }
    if revision.is_none() && !sha256.is_empty() {
        return Err(ConfigError::HalfPin(
            "builtin_sha256 is written without builtin_revision, and the shipped commit's files \
             hash to the shipped hashes; write the commit these belong to"
                .to_string(),
        ));
    }
    Ok(PinOverride {
        repo,
        revision,
        sha256,
    })
}

/// The checkpoint's files in one list, for a refusal that has to say what a pin covers.
fn files() -> String {
    crate::model::FILES.join(", ")
}

/// A key whose whole value is a string, refused by name when it is anything else.
fn text_of(key: &str, value: &toml::Value) -> Result<String, ConfigError> {
    match value {
        toml::Value::String(text) => Ok(text.clone()),
        other => Err(ConfigError::PinShape {
            key: key.to_string(),
            why: format!("is {}, not a string", kind(other)),
        }),
    }
}

/// `owner/name`: one slash, neither half empty, and nothing else - the shape a repository id has.
fn is_repo(text: &str) -> bool {
    match text.split_once('/') {
        Some((owner, name)) => {
            !owner.is_empty()
                && !name.is_empty()
                && !name.contains('/')
                && !text.contains(char::is_whitespace)
        }
        None => false,
    }
}

/// A commit or a sha-256: `len` hexadecimal digits, and not the all-zero filler a table holds
/// before its values are known.
fn filled(text: &str, len: usize) -> bool {
    text.len() == len
        && text.bytes().all(|b| b.is_ascii_hexdigit())
        && text.bytes().any(|b| b != b'0')
}

/// What a TOML value is, in the words a refusal names it by.
fn kind(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "a string",
        toml::Value::Integer(_) => "a whole number",
        toml::Value::Float(_) => "a number",
        toml::Value::Boolean(_) => "a boolean",
        toml::Value::Datetime(_) => "a date",
        toml::Value::Array(_) => "a list",
        toml::Value::Table(_) => "a table",
    }
}

/// `serde`'s own message names the offending key; this turns it into the variant that
/// carries the name, so the refusal reads as a sentence about the file.
fn unknown_key(e: &toml::de::Error, table: &toml::Table) -> ConfigError {
    let text = e.to_string();
    for key in table.keys() {
        if !KNOWN.contains(&key.as_str()) {
            return ConfigError::UnknownKey(key.clone());
        }
    }
    ConfigError::Toml(text)
}

/// Every key this build reads under `[embeddings]`.
const KNOWN: [&str; 6] = [
    "mode",
    "model",
    "download",
    "builtin_repo",
    "builtin_revision",
    "builtin_sha256",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// **The default is off.** A file that never wrote the key has not asked for a second
    /// tier, and an absent table is one way of saying so.
    #[test]
    fn an_absent_section_is_off() {
        assert_eq!(Settings::from_toml("").unwrap().mode, Mode::Off);
        assert_eq!(
            Settings::from_toml("main_model = \"local\"\n")
                .unwrap()
                .mode,
            Mode::Off
        );
    }

    /// The three values the entry decided, and each one's own shape.
    #[test]
    fn the_three_modes_are_read_from_one_key() {
        assert_eq!(
            Settings::from_toml("[embeddings]\nmode = \"off\"\n")
                .unwrap()
                .mode,
            Mode::Off
        );
        assert_eq!(
            Settings::from_toml("[embeddings]\nmode = \"internal\"\n")
                .unwrap()
                .mode,
            Mode::Internal { model: None }
        );
        assert_eq!(
            Settings::from_toml("[embeddings]\nmode = \"internal\"\nmodel = \"/models/e5\"\n")
                .unwrap()
                .mode,
            Mode::Internal {
                model: Some(PathBuf::from("/models/e5"))
            }
        );
        assert_eq!(
            Settings::from_toml(
                "[embeddings]\nmode = \"http://127.0.0.1:8080\"\nmodel = \"bge-m3\"\n"
            )
            .unwrap()
            .mode,
            Mode::Remote {
                url: "http://127.0.0.1:8080".into(),
                model: "bge-m3".into()
            }
        );
    }

    /// A word this build does not know is refused BY NAME. Read as off, a typo'd
    /// `mode = "Internal"` would turn the tier off and say nothing.
    #[test]
    fn a_mode_that_is_not_a_mode_is_refused_by_name() {
        let err = Settings::from_toml("[embeddings]\nmode = \"Internal\"\n").unwrap_err();
        assert_eq!(err, ConfigError::UnknownMode("Internal".into()));
        assert!(err.to_string().contains("Internal"), "{err}");
    }

    /// A key that would do nothing is refused, not ignored.
    #[test]
    fn a_key_the_build_does_not_know_is_refused_by_name() {
        let err = Settings::from_toml("[embeddings]\nmode = \"off\"\ndim = 384\n").unwrap_err();
        assert_eq!(err, ConfigError::UnknownKey("dim".into()));
        assert!(err.to_string().contains("dim"), "{err}");
    }

    /// A `model` under `off` embeds nothing and is the silent no-op this key exists to
    /// avoid.
    #[test]
    fn a_model_under_off_is_refused() {
        let err =
            Settings::from_toml("[embeddings]\nmode = \"off\"\nmodel = \"/m\"\n").unwrap_err();
        assert_eq!(err, ConfigError::ModelUnderOff);
    }

    /// **The download is on unless a box turns it off.** A box that may not reach the network
    /// says so in the same table as the mode, and then the built-in model is a refusal and
    /// never a fetch.
    #[test]
    fn the_download_is_on_unless_the_section_turns_it_off() {
        assert!(
            Settings::from_toml("[embeddings]\nmode = \"internal\"\n")
                .unwrap()
                .download
        );
        assert!(
            !Settings::from_toml("[embeddings]\nmode = \"internal\"\ndownload = false\n")
                .unwrap()
                .download
        );
        // A box that wrote only the policy has not written the mode, and is still off.
        let off = Settings::from_toml("[embeddings]\ndownload = false\n").unwrap();
        assert_eq!(off.mode, Mode::Off);
        assert!(!off.download);
    }

    /// The tree's client speaks HTTP/1.1 and no TLS, so `https://` is refused where it is
    /// written rather than at the first round.
    #[test]
    fn an_https_url_is_refused_by_name() {
        let err = Settings::from_toml("[embeddings]\nmode = \"https://api.example.com/v1\"\n")
            .unwrap_err();
        assert_eq!(
            err,
            ConfigError::UnspeakableUrl("https://api.example.com/v1".into())
        );
        assert!(err.to_string().contains("http://"), "{err}");
    }

    /// A section that is not a table, and a file that does not parse, say the fault.
    #[test]
    fn a_malformed_file_says_what_is_wrong() {
        assert!(matches!(
            Settings::from_toml("[embeddings]\nmode = "),
            Err(ConfigError::Toml(_))
        ));
        assert!(matches!(
            Settings::from_toml("embeddings = 3\n"),
            Err(ConfigError::Toml(_))
        ));
    }

    const REV: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";
    const CONFIG: &str = "953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41";
    const TOKENIZER: &str = "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037";
    const WEIGHTS: &str = "53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db";

    /// The three pin keys in one section, which is how a box that reviews another commit or
    /// mirrors the checkpoint writes them.
    fn pinned(revision: &str, hashes: &str) -> String {
        format!(
            "[embeddings]\nmode = \"internal\"\nbuiltin_revision = \"{revision}\"\n\
             builtin_sha256 = {{ {hashes} }}\n"
        )
    }

    fn all_three() -> String {
        format!(
            "\"config.json\" = \"{CONFIG}\", \"tokenizer.json\" = \"{TOKENIZER}\", \
             \"model.safetensors\" = \"{WEIGHTS}\""
        )
    }

    /// **A section may pin its own.** The shipped table is what a section that says nothing
    /// gets, and each key it does write is kept as it wrote it.
    #[test]
    fn a_section_can_pin_its_own_revision_and_hashes() {
        let shipped = Settings::from_toml("[embeddings]\nmode = \"internal\"\n").unwrap();
        assert_eq!(shipped.builtin, PinOverride::default());

        let own = Settings::from_toml(&pinned(REV, &all_three())).unwrap();
        assert_eq!(own.builtin.revision.as_deref(), Some(REV));
        assert_eq!(own.builtin.sha256["config.json"], CONFIG);
        assert_eq!(own.builtin.sha256["tokenizer.json"], TOKENIZER);
        assert_eq!(own.builtin.sha256["model.safetensors"], WEIGHTS);
        assert!(
            own.builtin.repo.is_none(),
            "the shipped repository unless named"
        );

        let mirrored = Settings::from_toml(
            "[embeddings]\nmode = \"internal\"\nbuiltin_repo = \"me/all-MiniLM-L6-v2\"\n",
        )
        .unwrap();
        assert_eq!(
            mirrored.builtin.repo.as_deref(),
            Some("me/all-MiniLM-L6-v2")
        );
        assert!(
            mirrored.builtin.revision.is_none(),
            "and the shipped commit"
        );
    }

    /// **A revision with the old hashes can never verify.** A fetch would have to refuse the
    /// bytes it was told to trust, so the half is refused where it is written, in both
    /// directions.
    #[test]
    fn a_pin_written_half_is_refused() {
        let err = Settings::from_toml(&format!(
            "[embeddings]\nmode = \"internal\"\nbuiltin_revision = \"{REV}\"\n"
        ))
        .unwrap_err();
        assert!(matches!(err, ConfigError::HalfPin(_)), "{err}");
        assert!(err.to_string().contains("builtin_sha256"), "{err}");

        let err = Settings::from_toml(&format!(
            "[embeddings]\nmode = \"internal\"\nbuiltin_sha256 = {{ {} }}\n",
            all_three()
        ))
        .unwrap_err();
        assert!(matches!(err, ConfigError::HalfPin(_)), "{err}");
        assert!(err.to_string().contains("builtin_revision"), "{err}");
    }

    /// Every pin key is checked where it is written, and the refusal names the key: a commit is
    /// 40 hexadecimal digits, a sha-256 is 64, and a file is one the checkpoint has.
    #[test]
    fn a_pin_key_of_the_wrong_shape_is_refused_by_name() {
        let cases: Vec<(&str, String)> = vec![
            ("builtin_revision", pinned("abc", &all_three())),
            (
                "builtin_revision",
                format!(
                    "[embeddings]\nmode = \"internal\"\nbuiltin_revision = 2024\n\
                     builtin_sha256 = {{ {} }}\n",
                    all_three()
                ),
            ),
            ("builtin_revision", pinned(&"0".repeat(40), &all_three())),
            (
                "builtin_sha256",
                format!(
                    "[embeddings]\nmode = \"internal\"\nbuiltin_revision = \"{REV}\"\n\
                     builtin_sha256 = \"{CONFIG}\"\n"
                ),
            ),
            (
                "config.json",
                pinned(
                    REV,
                    &format!(
                        "\"config.json\" = 612, \"tokenizer.json\" = \"{TOKENIZER}\", \
                         \"model.safetensors\" = \"{WEIGHTS}\""
                    ),
                ),
            ),
            (
                "config.json",
                pinned(
                    REV,
                    &format!(
                        "\"config.json\" = \"abc\", \"tokenizer.json\" = \"{TOKENIZER}\", \
                         \"model.safetensors\" = \"{WEIGHTS}\""
                    ),
                ),
            ),
            (
                "model.safetensors",
                pinned(
                    REV,
                    &format!(
                        "\"config.json\" = \"{CONFIG}\", \"tokenizer.json\" = \"{TOKENIZER}\""
                    ),
                ),
            ),
            (
                "encoder.json",
                pinned(
                    REV,
                    &format!("{}, \"encoder.json\" = \"{CONFIG}\"", all_three()),
                ),
            ),
            (
                "builtin_repo",
                "[embeddings]\nmode = \"internal\"\nbuiltin_repo = \"all-MiniLM-L6-v2\"\n"
                    .to_string(),
            ),
            (
                "builtin_repo",
                "[embeddings]\nmode = \"internal\"\nbuiltin_repo = 7\n".to_string(),
            ),
        ];
        for (named, text) in cases {
            let err = Settings::from_toml(&text).unwrap_err();
            assert!(
                err.to_string().contains(named),
                "{text}\n  named {named:?}, said {err}"
            );
        }
    }
}
