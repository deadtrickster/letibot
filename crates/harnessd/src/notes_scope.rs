//! The standing-notes scope behind the `notes` tool.
//!
//! The sibling of [`crate::transcript_source`] and [`crate::decision_source`],
//! and the same seam for the same reason: `letibot-tools` cannot know where
//! the workspace or the config dir are, so the tool declares a
//! [`NotesScope`] and this implements it.
//!
//! Both answers are the same facts the reader uses, read the same way:
//!
//! * the workspace, from the session's own `Config` — which by the time a
//!   harness registers tools is the *stored* workspace for a resumed session
//!   and not merely the daemon's start directory, so the tool's notes and the
//!   reader's notes are notes about the same tree;
//! * the box-wide dir, from [`crate::standing_notes::global_dir`] itself
//!   rather than a copy of its logic, so the two can never disagree about
//!   where the operator's notes live.

use std::path::PathBuf;

use letibot_tools::builtins::notes::NotesScope;

/// The scope of one session's `notes` tool.
pub struct ConfigNotes {
    workspace: PathBuf,
}

impl ConfigNotes {
    /// Over a workspace the harness has already settled — for a resumed
    /// session, the store's answer rather than the daemon's start directory.
    pub fn new(workspace: PathBuf) -> Self {
        ConfigNotes { workspace }
    }
}

impl NotesScope for ConfigNotes {
    fn workspace(&self) -> PathBuf {
        self.workspace.clone()
    }

    fn global_dir(&self) -> PathBuf {
        crate::standing_notes::global_dir()
    }
}
