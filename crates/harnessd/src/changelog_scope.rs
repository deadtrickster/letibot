//! The host changelog's scope behind the `host_changelog` tool - the sibling of
//! [`crate::notes_scope`], for the same reason: `letibot-tools` cannot know where this box keeps
//! its config, which host it is, or which session is calling.

use std::path::PathBuf;

use letibot_tools::builtins::changelog::{ChangelogScope, FILE};

/// The file a box's host changelog lives in - a function and not a field, so a test that moves
/// `XDG_CONFIG_HOME` between two calls gets the file it just moved to.
pub fn path() -> PathBuf {
    letibot_provider::keys::config_file()
        .parent()
        .map(|p| p.join(FILE))
        .unwrap_or_else(|| PathBuf::from(FILE))
}

/// The scope of one session's `host_changelog` tool.
pub struct ConfigChangelog {
    who: String,
}

impl ConfigChangelog {
    /// Over the session the harness opened - the id every line will carry.
    pub fn new(who: String) -> Self {
        ConfigChangelog { who }
    }
}

impl ChangelogScope for ConfigChangelog {
    fn path(&self) -> PathBuf {
        path()
    }

    fn host(&self) -> String {
        letibot_room::host_name()
    }

    fn who(&self) -> String {
        self.who.clone()
    }
}
