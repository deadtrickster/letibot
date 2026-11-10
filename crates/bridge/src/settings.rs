//! The one setting the bridge owns in the user's `~/.config/letibot/leticode.toml`: `[bridge]`.
//! The rest of that file belongs to the daemon's reader (`leticode-config`), which accepts this
//! table and does not interpret it - the shape `[embeddings]` already has.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The file's name, spelled as `leticode-config`'s `FILE_NAME` spells it.
const FILE_NAME: &str = "leticode.toml";

/// `$XDG_CONFIG_HOME/letibot/leticode.toml`, else `~/.config/letibot/leticode.toml` - the same
/// file `leticode_config::user_path` names, through the one module every host takes the directory
/// from, so an empty `XDG_CONFIG_HOME` is unset here as it is in the daemon.
pub fn path() -> PathBuf {
    letibot_plugins::paths::config_dir()
        .unwrap_or_default()
        .join(FILE_NAME)
}

/// What `[bridge]` says. Off is the absent table, the empty table and `false` alike: the relay
/// approves tool calls in somebody's session, so it is turned on and never assumed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeSettings {
    /// `permission_relay = true`: declare `claude/channel/permission` and relay the session's
    /// prompts to the operator.
    pub permission_relay: bool,
}

/// The table as written. `deny_unknown_fields` makes a typo a refusal that names the file rather
/// than a key that loads and does nothing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    permission_relay: Option<bool>,
}

impl BridgeSettings {
    /// Read the `[bridge]` table out of a whole `leticode.toml`. A file with no such table is the
    /// default, which is off.
    pub fn from_toml(text: &str) -> Result<BridgeSettings, String> {
        let doc: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        match doc.get("bridge") {
            None => Ok(BridgeSettings::default()),
            Some(value) => {
                let raw: Raw = value
                    .clone()
                    .try_into()
                    .map_err(|e: toml::de::Error| e.to_string())?;
                Ok(BridgeSettings {
                    permission_relay: raw.permission_relay.unwrap_or(false),
                })
            }
        }
    }

    /// Read the user level. A file that is not there is off: a box that never wrote the key has
    /// not asked for a relay.
    pub fn load(path: &Path) -> Result<BridgeSettings, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => BridgeSettings::from_toml(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BridgeSettings::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The user level at its fixed path, with a bad file reported as a sentence and read as off.
    pub fn load_user() -> BridgeSettings {
        let file = path();
        BridgeSettings::load(&file).unwrap_or_else(|why| {
            eprintln!(
                "letibot-mcp: {}: {why} - the permission relay stays off",
                file.display()
            );
            BridgeSettings::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_table_is_off() {
        assert!(!BridgeSettings::from_toml("").unwrap().permission_relay);
        assert!(
            !BridgeSettings::from_toml("[bridge]\n")
                .unwrap()
                .permission_relay
        );
        assert!(
            !BridgeSettings::from_toml("[bridge]\npermission_relay = false\n")
                .unwrap()
                .permission_relay
        );
    }

    #[test]
    fn the_key_turns_it_on_beside_every_other_table() {
        let text =
            "main_model = \"glm\"\n[bridge]\npermission_relay = true\n[queue]\nconcurrency = 2\n";
        assert!(BridgeSettings::from_toml(text).unwrap().permission_relay);
    }

    /// A key this build does not know is refused by name, not loaded and ignored: a relay nobody
    /// can tell is on is a relay nobody reads the transcripts of.
    #[test]
    fn a_misspelt_key_is_refused() {
        let why = BridgeSettings::from_toml("[bridge]\npermission_rely = true\n").unwrap_err();
        assert!(why.contains("permission_rely"), "{why}");
    }

    #[test]
    fn a_file_that_is_not_there_is_off() {
        let dir = std::env::temp_dir().join(format!("bridge-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            !BridgeSettings::load(&dir.join("none.toml"))
                .unwrap()
                .permission_relay
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
