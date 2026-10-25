//! **The status file is the project's plan as data** (`docs/plugins-slice1.md` C5): the shape the
//! agents rewrite instead of re-typing prose at whoever asks. These types are the whole of the
//! contract with that file — a field this crate does not name is a field the agents may leave out,
//! so every one of them defaults and the file is read rather than demanded.

use serde::{Deserialize, Serialize};

/// The row statuses C5 fixes. They are `&str` constants rather than an enum because a status file
/// is written by agents, and an unfamiliar status must render as an unfamiliar status — not turn
/// the whole snapshot unreadable.
pub const DONE: &str = "done";
pub const IN_PROGRESS: &str = "in progress";
pub const NOT_STARTED: &str = "not started";
pub const WAITING: &str = "waiting";
pub const BLOCKED: &str = "blocked";
pub const YOURS: &str = "yours";
/// An agent's status, out of the same vocabulary.
pub const RUNNING: &str = "running";

/// `<project>/.letibot/status.json`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Status {
    /// **Required, and the one field that is.** Every other field defaults, because a project that
    /// has not filled a section in should still draw; but a file with no date cannot be told fresh
    /// from abandoned, and the dashboard's honesty rests on that comparison — so a status file
    /// without `updated` is a malformed one, named as such.
    pub updated: String,
    #[serde(default)]
    pub release: Release,
    #[serde(default)]
    pub tiers: Vec<Tier>,
    #[serde(default)]
    pub stages: Vec<Stage>,
    #[serde(default)]
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub hosts: Vec<Host>,
    #[serde(default)]
    pub yours: Vec<Yours>,
}

impl Status {
    /// **The look's stdout as a status file, or the parser's own message.** serde_json's error
    /// carries the line and column, because "unparseable" without a place is a hunt — and this
    /// message is what the pane shows as the panel's `why`.
    pub fn parse(text: &str) -> Result<Status, String> {
        serde_json::from_str::<Status>(text).map_err(|e| e.to_string())
    }

    /// How many rows the whole plan has, which is the denominator one step up from a tier's.
    pub fn rows(&self) -> usize {
        self.tiers.iter().map(|t| t.rows.len()).sum()
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Release {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub goal: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Tier {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub rows: Vec<TierRow>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TierRow {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub area: String,
    #[serde(default)]
    pub item: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub agent: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Stage {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Agent {
    #[serde(default)]
    pub id: String,
    /// `claude` or `leticode` in the files that exist; kept open.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Host {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub jobs: u32,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Yours {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub what: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sparse_file_reads_and_a_broken_one_says_where() {
        let bare: Status = Status::parse(r#"{"updated":"2026-10-25T21:55:00Z"}"#).expect("sparse");
        assert_eq!(bare.tiers.len(), 0);
        assert_eq!(bare.rows(), 0);
        // The parser's message is what a person reads in the panel, so it must name the place.
        let why = Status::parse("{\"tiers\": [}").expect_err("not a status file");
        assert!(why.contains("line 1 column"), "{why}");
        // A file with no date parses into nothing useful, so it does not parse at all.
        let why = Status::parse("{}").expect_err("no updated");
        assert!(why.contains("updated"), "{why}");
    }

    #[test]
    fn an_unfamiliar_field_is_ignored_and_an_absent_one_defaults() {
        let status: Status = Status::parse(
            r#"{"updated":"2026-10-25T21:55:00Z","release":{"name":"v1"},
                "tiers":[{"id":"t0","rows":[{"id":"t0/a","item":"x",
                "status":"done","priority":7}]}],"scheduler":{"url":"http://x"}}"#,
        )
        .expect("a file with more in it than this plugin reads");
        assert_eq!(status.release.name, "v1");
        assert_eq!(status.release.goal, "");
        assert_eq!(status.rows(), 1);
        assert_eq!(status.tiers[0].rows[0].status, DONE);
    }
}
