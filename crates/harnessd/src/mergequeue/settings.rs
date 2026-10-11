//! **The queues' per-project settings** — the `[queue]` table of the project's `leticode.toml`,
//! found by walking up from the repository the way every `leticode.toml` is found.
//!
//! ```toml
//! [queue]
//! target = "feat/memory-engine"   # decision 12: the branch this project lands on
//! merge_via_session = false       # decision 4: a review that cannot run is handed to the
//!                                 #   project's main session to gate and merge (default: no —
//!                                 #   raise only)
//! red_target = "fix"              # decision 11: when the target goes red, the reviewer of the
//!                                 #   red tip opens a fix branch through the queue ("fix",
//!                                 #   the default) or the operator is only told ("raise")
//! ```
//!
//! Three settings, each the operator's own ruling of 2026-10-11, and each per PROJECT because
//! the projects differ: pg-noop lands on `feat/memory-engine`, 91 commits ahead of its `main`,
//! and a queue that measured its branches against `main` saw every one 56–60 commits ahead.

use std::path::{Path, PathBuf};

use crate::leticode_config::LeticodeConfig;

/// What a red target sets in motion (decision 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedTarget {
    /// The reviewer of the red tip opens a fix branch, through the queue like any other, and
    /// the operator is told.
    Fix,
    /// The operator is told, and nothing is opened.
    Raise,
}

impl RedTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            RedTarget::Fix => "fix",
            RedTarget::Raise => "raise",
        }
    }
}

/// **One project's queue settings**, with where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSettings {
    /// The branch this project lands on; `None` is *the repository's default* (`main`, else
    /// `master`, else what the main checkout has out).
    pub target: Option<String>,
    /// Decision 4's option. Default `false`: raise only.
    pub merge_via_session: bool,
    /// Decision 11's default. `Fix` unless the project says `raise`.
    pub red_target: RedTarget,
    /// The file they were read from, for the pane; `None` when there was none.
    pub path: Option<PathBuf>,
}

impl Default for QueueSettings {
    fn default() -> Self {
        QueueSettings {
            target: None,
            merge_via_session: false,
            red_target: RedTarget::Fix,
            path: None,
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueTable {
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    merge_via_session: Option<bool>,
    #[serde(default)]
    red_target: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct FileWithQueue {
    #[serde(default)]
    queue: Option<toml::Value>,
}

/// **The settings for the repository at `repo`** — the nearest `leticode.toml` at or above it.
/// No file, or a file with no `[queue]`, is the defaults. A `[queue]` that does not parse, or
/// names a key this build does not know, is an `Err` naming the file — never a silent default.
pub fn for_repository(repo: &Path) -> Result<QueueSettings, String> {
    let Some(path) = LeticodeConfig::discover(repo) else {
        return Ok(QueueSettings::default());
    };
    parse(&path)
}

/// Parse one file's `[queue]` table.
pub fn parse(path: &Path) -> Result<QueueSettings, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: FileWithQueue =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let Some(table) = file.queue else {
        return Ok(QueueSettings {
            path: Some(path.to_path_buf()),
            ..Default::default()
        });
    };
    let q: QueueTable = table
        .try_into()
        .map_err(|e| format!("{}: [queue]: {e}", path.display()))?;
    let red_target = match q.red_target.as_deref().map(str::trim) {
        None | Some("fix") => RedTarget::Fix,
        Some("raise") => RedTarget::Raise,
        Some(other) => {
            return Err(format!(
                "{}: [queue] red_target = \"{other}\" — the words are \"fix\" (the reviewer of the \
                 red tip opens a fix branch through the queue) and \"raise\" (the operator is \
                 told, nothing is opened)",
                path.display()
            ));
        }
    };
    Ok(QueueSettings {
        target: q
            .target
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
        merge_via_session: q.merge_via_session.unwrap_or(false),
        red_target,
        path: Some(path.to_path_buf()),
    })
}

/// **The target for `repo`** — the project's, else the repository's default.
pub fn target_for(repo: &Path, settings: &QueueSettings) -> Option<String> {
    settings
        .target
        .clone()
        .or_else(|| super::git::default_target(repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> (PathBuf, PathBuf) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "letibot-queue-settings-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join("repo")).unwrap();
        std::fs::write(dir.join("leticode.toml"), text).unwrap();
        (dir.clone(), dir.join("repo"))
    }

    /// Decisions 4, 11 and 12 are per project, read from the project's own file, walked up to.
    #[test]
    fn a_project_names_its_target_its_red_policy_and_its_fallback() {
        let (dir, repo) = file(
            "main_model = \"local\"\n[queue]\ntarget = \"feat/memory-engine\"\n\
             merge_via_session = true\nred_target = \"raise\"\n",
        );
        let s = for_repository(&repo).unwrap();
        assert_eq!(s.target.as_deref(), Some("feat/memory-engine"));
        assert!(s.merge_via_session);
        assert_eq!(s.red_target, RedTarget::Raise);
        // The model reader still reads the same file.
        assert!(LeticodeConfig::load(&dir.join("leticode.toml")).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_table_is_the_defaults_and_an_unknown_key_is_refused_by_name() {
        let (dir, repo) = file("main_model = \"local\"\n");
        let s = for_repository(&repo).unwrap();
        assert_eq!(s.target, None);
        assert!(
            !s.merge_via_session,
            "decision 4: raise only, unless the project opts in"
        );
        assert_eq!(s.red_target, RedTarget::Fix);
        std::fs::write(
            dir.join("leticode.toml"),
            "[queue]\ntarget_branch = \"x\"\n",
        )
        .unwrap();
        let e = for_repository(&repo).unwrap_err();
        assert!(e.contains("target_branch"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
