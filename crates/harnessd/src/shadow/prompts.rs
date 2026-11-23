//! The shadow's prompts: builtin files, overridable per project at `.letibot/shadow/<job>.md`.
//! The fence around the data is added here, after the prompt, so an override cannot drop it.
//!
//! The permission job asks two different questions - whether a rule covers the call, and whether
//! the operator's own words ask for it - so it has two prompts, and the CALLER says which (a
//! `Shown` with `oracle` set is an authorisation brief). A marker inside the data was the old
//! shape: one prompt doing two jobs, chosen by a sentence the data could carry.

use std::path::{Path, PathBuf};

use super::Job;

pub const TURN_END: &str = include_str!("prompts/turn_end.md");
pub const DURING: &str = include_str!("prompts/during.md");
pub const PERMISSION: &str = include_str!("prompts/permission.md");
/// The permission job's second question: did the operator ask for this call? Sent instead of
/// [`PERMISSION`] when the caller says it is asking an authorisation brief.
pub const PERMISSION_ASKING: &str = include_str!("prompts/permission_asking.md");

pub fn builtin(job: Job) -> &'static str {
    match job {
        Job::TurnEnd => TURN_END,
        Job::During => DURING,
        Job::Permission => PERMISSION,
    }
}

/// The authorisation prompt `job` has, if it has one: the permission job alone asks twice.
pub fn builtin_asking(job: Job) -> Option<&'static str> {
    (job == Job::Permission).then_some(PERMISSION_ASKING)
}

/// Where a project overrides `job`'s prompt.
pub fn override_path(workspace: &Path, job: Job) -> PathBuf {
    workspace
        .join(".letibot")
        .join("shadow")
        .join(format!("{}.md", job.as_str()))
}

/// Where a project overrides the permission job's authorisation prompt, beside its rule prompt.
pub fn asking_override_path(workspace: &Path) -> PathBuf {
    workspace
        .join(".letibot")
        .join("shadow")
        .join("permission_asking.md")
}

/// The prompt `job` is sent with, and where it came from - `builtin` or the override's path.
pub fn load(workspace: Option<&Path>, job: Job) -> (String, String) {
    if let Some(ws) = workspace {
        let p = override_path(ws, job);
        if let Some(text) = read_override(&p) {
            return (text, p.display().to_string());
        }
    }
    (builtin(job).to_string(), "builtin".to_string())
}

/// The authorisation prompt `job` asks its second question with, and where it came from. Empty
/// for the jobs that have one prompt.
pub fn load_asking(workspace: Option<&Path>, job: Job) -> (String, String) {
    let Some(text) = builtin_asking(job) else {
        return (String::new(), "none".to_string());
    };
    if let Some(ws) = workspace {
        let p = asking_override_path(ws);
        if let Some(t) = read_override(&p) {
            return (t, p.display().to_string());
        }
    }
    (text.to_string(), "builtin".to_string())
}

/// A project's own text at `path`, when there is one and it says anything. An empty override is
/// not an instruction: reading it as one would leave the job with no prompt at all.
fn read_override(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    (!text.trim().is_empty()).then_some(text)
}

/// The data, fenced and labelled. A closing tag inside the data is broken so the fence holds.
pub fn fence(data: &str) -> String {
    format!(
        "<transcript>\n{}\n</transcript>",
        data.replace("</transcript>", "</ transcript>")
    )
}
