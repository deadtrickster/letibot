//! The standing-notes scope behind the `notes` tool - the sibling of [`crate::transcript_source`]
//! and [`crate::decision_source`], for the same reason: `letibot-tools` cannot know where the
//! workspace or the config dir are. Both answers come from the session's own [`Config`].

use std::path::PathBuf;

use letibot_tools::builtins::notes::NotesScope;

/// The scope of one session's `notes` tool.
pub struct ConfigNotes {
    workspace: PathBuf,
    global: PathBuf,
}

impl ConfigNotes {
    /// Over a workspace and a box-wide notes directory the harness has already settled -
    /// for a resumed session, the store's answer rather than the daemon's start directory.
    pub fn new(workspace: PathBuf, global: PathBuf) -> Self {
        ConfigNotes { workspace, global }
    }
}

impl NotesScope for ConfigNotes {
    fn workspace(&self) -> PathBuf {
        self.workspace.clone()
    }

    fn global_dir(&self) -> PathBuf {
        self.global.clone()
    }
}
