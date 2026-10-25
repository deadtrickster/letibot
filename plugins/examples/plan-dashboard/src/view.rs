//! **The pane contract's types** (`docs/plugins-slice1.md` C3), in serde: the input the host calls
//! the hook with, and the `View` it reads back. They are written out here rather than imported
//! from the host because a plugin is a separate crate with its own copy of the contract — the host
//! validates an answer into *its* types, and this one has to produce JSON that survives that.
//!
//! The shape is serenedash's, which is why nothing is looser than it has to be: every bar carries
//! its panel's denominator, every stop has an id (`↑`/`↓` and the wheel move between them), and
//! every sentence a person reads in the legend is the same sentence the hover shows.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::status::Status;

/// The C3 input.
///
/// The host also sends `"v"`, `"event"` and `"size"`, and this pane acts on none of them: its
/// actions are `say` and `look`, both of which the host performs itself, so no `event` ever comes
/// back to it. serde ignores what a struct does not name, so they are documented here rather than
/// carried as fields a reader would take for state this pane keeps.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Input {
    #[serde(default)]
    pub state: Option<State>,
    #[serde(default)]
    pub look: Option<LookResult>,
}

/// What this pane returned as its `state` last time.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct State {
    #[serde(default)]
    pub status: Option<Status>,
}

/// A look's result: the exit, and what the process wrote.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LookResult {
    #[serde(default)]
    pub exit: i32,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

/// **The C3 answer.**
#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge: Option<Badge>,
    pub panels: Vec<Panel>,
    pub findings: Vec<Finding>,
    /// Labelled in the `l` view and on hover; keyed by the label itself so the same map answers
    /// both.
    pub legend: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look: Option<Look>,
    /// Seconds until the host should look again; `0` is "once, and no more".
    pub every_s: u32,
    pub state: Option<serde_json::Value>,
}

/// The status row's one word about this pane.
#[derive(Debug, Clone, Serialize)]
pub struct Badge {
    pub text: String,
    pub role: Role,
}

/// The look this pane wants next.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Look {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<String>,
}

impl Look {
    /// A path, read by the host — which is what the `read` capability buys, and all this plugin
    /// asks for.
    pub fn read(path: &str) -> Look {
        Look {
            read: Some(path.to_string()),
            ..Look::default()
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Panel {
    pub id: String,
    /// The key that toggles this panel's view. None only for a panel with nothing behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub title: String,
    /// What every bar in this panel is a share of — C3 makes this obligatory, because a bar
    /// without a denominator is a decoration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denominator: Option<String>,
    pub summary: Vec<Block>,
    /// Absent means the summary is the view.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub view: Vec<Block>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<Unavailable>,
}

/// A panel that could not get its data: why, and what to do about it — never an empty drawing.
#[derive(Debug, Clone, Serialize)]
pub struct Unavailable {
    pub why: String,
    pub fix: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Block {
    Lines { lines: Vec<Line> },
    Table { table: Table },
    Kv { kv: Kv },
    Markdown { markdown: String },
}

impl Block {
    pub fn lines(lines: Vec<Line>) -> Block {
        Block::Lines { lines }
    }

    pub fn table(columns: &[&str], rows: Vec<Row>) -> Block {
        Block::Table {
            table: Table {
                columns: columns.iter().map(|c| c.to_string()).collect(),
                rows,
            },
        }
    }

    pub fn kv(rows: Vec<(String, String)>) -> Block {
        Block::Kv { kv: Kv { rows } }
    }
}

/// One row of the grid: label, right-aligned value, a bar, a tail — laid out by the host so the
/// numbers line up down the frame.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Line {
    pub label: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bar: Option<Bar>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spark: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tail: String,
    pub role: Role,
    /// A line with an id is a stop: arrows and the wheel land on it, and its actions are offered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Bar {
    pub frac: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Row {
    pub id: String,
    pub cells: Vec<String>,
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Kv {
    pub rows: Vec<(String, String)>,
}

/// Everything that tripped, in one list, each with what was measured, the numbers behind it, the
/// fix and how to check it — one Enter reads one whole.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub id: String,
    pub what: String,
    pub panel: String,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Tripped,
    Missing,
    Passed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Action {
    pub key: String,
    pub label: String,
    /// A line to the session's model, as if the operator had typed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub say: Option<String>,
    /// Re-run the look now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look: Option<bool>,
    /// Call the pane again with `{"item","action"}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

impl Action {
    pub fn say(key: &str, label: &str, line: impl Into<String>) -> Action {
        Action {
            key: key.into(),
            label: label.into(),
            say: Some(line.into()),
            look: None,
            event: None,
        }
    }

    pub fn look(key: &str, label: &str) -> Action {
        Action {
            key: key.into(),
            label: label.into(),
            say: None,
            look: Some(true),
            event: None,
        }
    }
}

/// **The colour language**, and the one sentence each colour means — the legend and the hover show
/// the same string, so a reader is never guessing what "attention" was claiming.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Muted,
    Working,
    Attention,
    Success,
    Failure,
    #[default]
    Plain,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Muted => "muted",
            Role::Working => "working",
            Role::Attention => "attention",
            Role::Success => "success",
            Role::Failure => "failure",
            Role::Plain => "plain",
        }
    }

    pub fn meaning(self) -> &'static str {
        match self {
            Role::Muted => "nothing is happening on it, or it has not started",
            Role::Working => "an agent is on it now",
            Role::Attention => "yours, or blocked, or waiting on somebody",
            Role::Success => "done",
            Role::Failure => "the look, or the status file, failed",
            Role::Plain => "no claim either way",
        }
    }
}
