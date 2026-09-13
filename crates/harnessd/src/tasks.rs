//! The subagent (task) journal: live state of every subagent this daemon has
//! spawned, written to a JSON file that `leticode-dash` reads.
//!
//! # It is a file, not a socket
//!
//! The dashboard is a *view* and the daemon is the *record* (`docs/leticode.md`).
//! The record here is a small JSON file the daemon rewrites on every subagent
//! spawn and finish, which a head polling it can read without owning a socket or
//! speaking the session protocol. The alternative — a query over the socket — would
//! make the dashboard a second head with a state machine to keep in step; a file is
//! a `read(2)` and done.
//!
//! # It is rewritten atomically
//!
//! The file is written to a sibling and renamed into place, because a reader that
//! polls every few seconds must never see a half-written frame. A torn JSON blob
//! would read as a missing state, which a dashboard shows as "nothing is running"
//! — the wrong answer at the exact moment it matters.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use letibot_tools::builtins::lsp::LspConfig;
use letibot_tools::builtins::skill::SkillRegistry;
use serde::Serialize;
use serde_json::json;

/// One subagent, as the dashboard shows it.
#[derive(Debug, Clone, Serialize)]
pub struct TaskEntry {
    pub name: String,
    pub role: String,
    /// `running` | `done` | `failed`.
    pub state: String,
    /// Tokens the subagent generated so far.
    pub tokens: u64,
    /// Seconds since the subagent was spawned.
    pub elapsed: f64,
    /// The subtask it was given, first line and truncated for a row.
    pub prompt: String,
    /// The session that spawned it, so a tree can be drawn.
    pub parent: String,
}

/// The whole state file: `tasks` plus the `lsp` and `skills` panels.
#[derive(Debug, Clone, Serialize)]
struct StateFile {
    tasks: Vec<TaskEntry>,
    lsp: Vec<serde_json::Value>,
    skills: Vec<serde_json::Value>,
}

/// A shared, file-backed list of subagent states.
///
/// Held in [`crate::harness::Parts`] and cloned into the subagent runner, so every
/// spawn and finish lands here however many sessions or subagents are running. The
/// `Mutex` is the write path only; a dashboard never locks it.
#[derive(Debug)]
pub struct TaskJournal {
    entries: Mutex<Vec<TaskEntry>>,
    path: Option<PathBuf>,
    /// The language servers the session can reach, so the `lsp` panel is a reading
    /// of what is configured and installed rather than a claim.
    lsp: Arc<LspConfig>,
    /// The loaded skills, so the `skills` panel lists what the model can reach.
    skills: Arc<SkillRegistry>,
}

impl TaskJournal {
    /// `None` path means in-memory only: `record` still updates the list, and
    /// nothing is written. This is what a test without `$XDG_RUNTIME_DIR` gets.
    pub fn new(path: Option<PathBuf>, lsp: Arc<LspConfig>, skills: Arc<SkillRegistry>) -> Self {
        TaskJournal {
            entries: Mutex::new(Vec::new()),
            path,
            lsp,
            skills,
        }
    }

    /// Record a subagent's state, replacing any earlier entry with the same name so
    /// a `running` row becomes its `done` row rather than a second one.
    pub fn record(&self, entry: TaskEntry) {
        {
            let mut g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(slot) = g.iter_mut().find(|e| e.name == entry.name) {
                *slot = entry.clone();
            } else {
                g.push(entry.clone());
            }
        }
        self.flush();
    }

    /// The current entries, for a caller that wants the list rather than the file.
    pub fn snapshot(&self) -> Vec<TaskEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Write the file now. Called after every `record`, and once at startup so the
    /// `lsp` and `skills` panels are visible before any subagent has run.
    pub fn flush(&self) {
        let Some(path) = &self.path else {
            return;
        };
        // The lsp panel is a reading: which servers are configured, and whether each
        // is installed. `n_diagnostics` stays 0 until the tool reports them.
        let lsp: Vec<serde_json::Value> = self
            .lsp
            .servers
            .keys()
            .map(|lang| {
                let program = &self.lsp.servers.get(lang).map(|a| a[0].as_str()).unwrap_or("");
                json!({
                    "language": lang,
                    "installed": installed(program),
                    "n_diagnostics": 0,
                })
            })
            .collect();
        let skills: Vec<serde_json::Value> = self
            .skills
            .skills
            .iter()
            .map(|s| json!({ "name": s.name, "description": s.description }))
            .collect();
        let state = StateFile {
            tasks: self.snapshot(),
            lsp,
            skills,
        };
        let Ok(body) = serde_json::to_string_pretty(&state) else {
            return;
        };
        // Write to a sibling and rename, so a poller never reads a torn frame.
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Whether `program` is an executable on `$PATH`. A direct scan rather than a
/// `which` subprocess, so the dashboard's reading costs nothing and works where
/// `which` does not.
fn installed(program: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    for dir in path.split(':') {
        let p = std::path::Path::new(dir).join(program);
        if p.is_file() {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(md) = p.metadata() {
                return md.permissions().mode() & 0o111 != 0;
            }
        }
    }
    false
}

/// Where the state file lives: `$XDG_RUNTIME_DIR/leticode-state.json`, falling back
/// to the temp directory. `None` when neither resolves — a bare test environment,
/// where the journal still works in memory.
pub fn default_state_path() -> Option<PathBuf> {
    if let Ok(run) = std::env::var("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(run).join("leticode-state.json"));
    }
    let tmp = std::env::temp_dir();
    if tmp.as_os_str().is_empty() {
        return None;
    }
    Some(tmp.join("leticode-state.json"))
}

/// A handle a runner can clone, kept in an `Arc` so it is `Send + Sync` while the
/// journal itself stays behind a `Mutex`.
pub type SharedTaskJournal = Arc<TaskJournal>;
