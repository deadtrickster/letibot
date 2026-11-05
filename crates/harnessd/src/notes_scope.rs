//! The standing-notes scope behind the `notes` tool: `letibot-tools` cannot know where the
//! workspace, the config dir or the notes checkout are, so every answer comes from the session's
//! own [`crate::config::Config`].

use std::path::PathBuf;

use letibot_tools::builtins::notes::NotesScope;

/// The scope of one session's `notes` tool.
pub struct ConfigNotes {
    workspace: PathBuf,
    global: PathBuf,
    project: PathBuf,
    repo: Option<PathBuf>,
}

impl ConfigNotes {
    /// Over directories the harness has already settled - for a resumed session, the store's
    /// workspace rather than the daemon's start directory.
    pub fn of(cfg: &crate::config::Config) -> Self {
        ConfigNotes {
            workspace: cfg.workspace.clone(),
            global: cfg.global_notes(),
            project: cfg.project_notes(),
            repo: cfg.notes_repo(),
        }
    }
}

impl NotesScope for ConfigNotes {
    fn workspace(&self) -> PathBuf {
        self.workspace.clone()
    }

    fn global_dir(&self) -> PathBuf {
        self.global.clone()
    }

    fn project_dir(&self) -> PathBuf {
        self.project.clone()
    }

    fn repo(&self) -> Option<PathBuf> {
        self.repo.clone()
    }
}
