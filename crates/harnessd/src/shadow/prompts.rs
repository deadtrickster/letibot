//! The shadow's prompts: builtin files, overridable per project at `.letibot/shadow/<job>.md`.
//! The fence around the data is added here, after the prompt, so an override cannot drop it.

use std::path::{Path, PathBuf};

use super::Job;

pub const TURN_END: &str = include_str!("prompts/turn_end.md");
pub const DURING: &str = include_str!("prompts/during.md");
pub const PERMISSION: &str = include_str!("prompts/permission.md");

pub fn builtin(job: Job) -> &'static str {
    match job {
        Job::TurnEnd => TURN_END,
        Job::During => DURING,
        Job::Permission => PERMISSION,
    }
}

/// Where a project overrides `job`'s prompt.
pub fn override_path(workspace: &Path, job: Job) -> PathBuf {
    workspace
        .join(".letibot")
        .join("shadow")
        .join(format!("{}.md", job.as_str()))
}

/// The prompt `job` is sent with, and where it came from - `builtin` or the override's path.
pub fn load(workspace: Option<&Path>, job: Job) -> (String, String) {
    if let Some(ws) = workspace {
        let p = override_path(ws, job);
        if let Ok(text) = std::fs::read_to_string(&p)
            && !text.trim().is_empty()
        {
            return (text, p.display().to_string());
        }
    }
    (builtin(job).to_string(), "builtin".to_string())
}

/// The data, fenced and labelled. A closing tag inside the data is broken so the fence holds.
pub fn fence(data: &str) -> String {
    format!(
        "<transcript>\n{}\n</transcript>",
        data.replace("</transcript>", "</ transcript>")
    )
}
