//! The sessions a browser may ask: the operator's own top-level conversations, live in a running
//! daemon. Never a subagent, a package, the review queue or a one-shot: the grouping rule is the
//! session list's own (`group_of`), so the picker and the TUI agree on what is "mine".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use letibot_sessionlog::{GroupRow, SessionGroup, SessionOrigin, group_of};
use serde_json::{Value, json};

use crate::daemon::{Daemon, is_up, key, socket_in};

/// One row of the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub name: String,
    /// The workspace folder's name; `workspace` is the whole path.
    pub project: String,
    pub workspace: String,
    /// `idle`, `running` or `asking`.
    pub state: &'static str,
    pub model: String,
    pub socket: PathBuf,
    pub last_activity_ms: i64,
}

impl Row {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "project": self.project,
            "workspace": self.workspace,
            "state": self.state,
            "model": self.model,
        })
    }

    /// The daemon this session lives in, as a head attaches to it.
    pub fn daemon(&self, store: &Path) -> Daemon {
        Daemon {
            workspace: PathBuf::from(&self.workspace),
            key: key(Path::new(&self.workspace)),
            socket: self.socket.clone(),
            log: PathBuf::new(),
            store: store.to_path_buf(),
            placement: None,
            started: false,
        }
    }
}

/// Every live top-level operator session, newest activity first. A store that cannot be read is
/// an error, not an empty list: the picker would otherwise say "no sessions" for a broken box.
pub fn list(store: &Path, rundir: &Path, timeout: Duration) -> Result<Vec<Row>, String> {
    let store_path = store;
    let store = crate::store::open(store, "session", None)
        .map_err(|e| format!("the session store {}: {e}", store_path.display()))?;
    let stored = store.list_sessions().map_err(|e| e.to_string())?;
    let origins: Vec<SessionOrigin> = stored
        .iter()
        .map(|s| {
            s.origin
                .as_deref()
                .and_then(SessionOrigin::parse)
                .unwrap_or_default()
        })
        .collect();
    let rows: Vec<GroupRow<'_>> = stored
        .iter()
        .zip(&origins)
        .map(|(s, origin)| GroupRow {
            id: &s.id,
            parent: s.parent_session_id.as_deref(),
            origin: *origin,
            role: s.role.as_deref(),
            title: s.title.as_deref().unwrap_or_default(),
        })
        .collect();
    let mut by_workspace: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, s) in stored.iter().enumerate() {
        if s.parent_session_id.is_none() && group_of(&rows, &s.id) == SessionGroup::Mine {
            by_workspace.entry(&s.workspace_root).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for (workspace, members) in by_workspace {
        let socket = socket_in(rundir, &key(Path::new(workspace)));
        if !is_up(&socket) {
            continue;
        }
        let Ok(briefs) = crate::fleet::briefs(&socket, &stored[members[0]].id, timeout) else {
            continue;
        };
        for i in members {
            let s = &stored[i];
            let Some(b) = briefs.iter().find(|b| b.session_id == s.id && b.live) else {
                continue;
            };
            let state = if b.waits_on_operator > 0 {
                "asking"
            } else if b.status.running {
                "running"
            } else {
                "idle"
            };
            let title = s.title.clone().unwrap_or_default();
            out.push(Row {
                id: s.id.clone(),
                name: if title.trim().is_empty() {
                    s.id.chars().take(10).collect()
                } else {
                    title
                },
                project: Path::new(workspace)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| workspace.to_string()),
                workspace: workspace.to_string(),
                state,
                model: s.model_id.clone(),
                socket: socket.clone(),
                last_activity_ms: s.last_activity_ms,
            });
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.last_activity_ms));
    Ok(out)
}
