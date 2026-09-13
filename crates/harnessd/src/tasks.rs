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

use serde::Serialize;

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

/// The whole state file: `tasks` plus the panels that land later (`lsp`, `skills`).
///
/// The two empty panels are here rather than absent so the dashboard's
/// `s.get("lsp", [])` reads the same whether the daemon has written them or not.
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
}

impl TaskJournal {
    /// `None` path means in-memory only: `record` still updates the list, and
    /// nothing is written. This is what a test without `$XDG_RUNTIME_DIR` gets.
    pub fn new(path: Option<PathBuf>) -> Self {
        TaskJournal {
            entries: Mutex::new(Vec::new()),
            path,
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

    fn flush(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let state = StateFile {
            tasks: self.snapshot(),
            lsp: Vec::new(),
            skills: Vec::new(),
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
