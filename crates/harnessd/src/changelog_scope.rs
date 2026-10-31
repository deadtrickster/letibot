//! The host changelog's scope behind the `host_changelog` tool.
//!
//! The sibling of [`crate::notes_scope`] and the same seam for the same reason: `letibot-tools`
//! cannot know where this box keeps its config, which host it is, or which session is calling, so
//! the tool declares a [`ChangelogScope`] and this answers it.
//!
//! Three facts, and each is read from the one place that already knows it:
//!
//! * **the file** — the config dir's `host-changelog.jsonl`, beside `providers.toml` and
//!   `prompts.toml`, by the same rule `Prompts::path` and `standing_notes::global_dir` use;
//! * **the host** — `letibot_room::host_name()`, `uname -n`'s answer, which is what a person
//!   comparing two boxes would type. A change made on a declared machine names that machine in
//!   the call; this is the default;
//! * **the writer** — the session's own id, bound at open, so every line says which session
//!   recorded it and a person can go and read that conversation.
//!
//! **The path is a function and not a field**, so a test that moves `XDG_CONFIG_HOME` between two
//! calls of one process gets the file it just moved to.

use std::path::PathBuf;

use letibot_tools::builtins::changelog::{ChangelogScope, FILE};

/// The file a box's host changelog lives in.
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
    /// Over the session the harness opened — the id every line will carry.
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
