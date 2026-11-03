//! The one key that decides where a vector comes from.
//!
//! ```toml
//! [embeddings]
//! mode = "off"                      # lexical only, and the default
//! mode = "internal"                 # candle, here, with the built-in model
//! model = "<dir>"                   # ... or a directory this build can read
//! mode = "http://host:8080"         # an OpenAI-compatible /v1/embeddings
//! model = "text-embedding-3-small"  # ... and the name it knows
//! ```
//!
//! Three values for one key, because they are three answers to one question: who turns
//! text into a vector. A key that does nothing is refused by name rather than ignored,
//! because that is the config defect this tree has already paid for once.

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

/// The whole `[embeddings]` section, and the answer to "is there a second tier".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    pub mode: Mode,
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
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Toml(e) => write!(f, "[embeddings]: {e}"),
            ConfigError::UnknownKey(k) => write!(
                f,
                "[embeddings] {k} is not a key this build knows; it reads `mode` and `model`"
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
        Ok(Settings { mode })
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

/// `serde`'s own message names the offending key; this turns it into the variant that
/// carries the name, so the refusal reads as a sentence about the file.
fn unknown_key(e: &toml::de::Error, table: &toml::Table) -> ConfigError {
    let text = e.to_string();
    for key in table.keys() {
        if key != "mode" && key != "model" {
            return ConfigError::UnknownKey(key.clone());
        }
    }
    ConfigError::Toml(text)
}

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
}
