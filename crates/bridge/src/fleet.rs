//! Every package session on this box, from any daemon: the sessions come from the shared store,
//! the live state from each folder's own daemon. Nothing here starts a daemon.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::SessionBrief;
use letibot_sessionlog::{FleetRow, FleetState, SessionOrigin};

use crate::daemon::{is_up, key, rundir, socket_in};

/// Package sessions of daemons other than `own`, newest activity first. A store that cannot be
/// read is an error and not an empty fleet.
pub fn scan(store: &Path, own: &Path) -> Result<Vec<FleetRow>, String> {
    scan_in(store, own, &rundir())
}

pub fn scan_in(store: &Path, own: &Path, rundir: &Path) -> Result<Vec<FleetRow>, String> {
    let store = letibot_tokencore::store::Store::open(store).map_err(|e| e.to_string())?;
    let mut stored = store.list_sessions().map_err(|e| e.to_string())?;
    stored.retain(|s| {
        s.parent_session_id.is_none()
            && s.origin.as_deref().and_then(SessionOrigin::parse) == Some(SessionOrigin::Package)
            && Path::new(&s.workspace_root) != own
    });
    stored.sort_by_key(|s| std::cmp::Reverse(s.last_activity_ms));
    let mut by_workspace: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, s) in stored.iter().enumerate() {
        by_workspace
            .entry(s.workspace_root.clone())
            .or_default()
            .push(i);
    }
    let mut out: Vec<Option<FleetRow>> = vec![None; stored.len()];
    for (workspace, members) in &by_workspace {
        let socket = socket_in(rundir, &key(Path::new(workspace)));
        let placement = letibot_sessionlog::placement::read(&socket);
        let live = if is_up(&socket) {
            Some(briefs(&socket, &stored[members[0]].id))
        } else {
            None
        };
        for &i in members {
            let s = &stored[i];
            let state = match &live {
                None => FleetState::DaemonDown,
                Some(Err(why)) => FleetState::Unreachable(why.clone()),
                Some(Ok(all)) => match all.iter().find(|b| b.session_id == s.id) {
                    Some(b) if b.waits_on_operator > 0 => FleetState::Asking,
                    Some(b) if b.status.running => FleetState::Running,
                    _ => FleetState::Idle,
                },
            };
            out[i] = Some(FleetRow {
                id: s.id.clone(),
                title: s.title.clone().unwrap_or_default(),
                workspace: workspace.clone(),
                model: s.model_id.clone(),
                placement: placement.clone(),
                state,
            });
        }
    }
    Ok(out.into_iter().flatten().collect())
}

/// What the daemon at `socket` says it holds, from the `Hello` of one attach to `session`.
fn briefs(socket: &PathBuf, session: &str) -> Result<Vec<SessionBrief>, String> {
    let (mut client, hello, _reader) = HeadClient::attach(
        socket,
        session,
        0,
        crate::daemon::HEAD_KIND,
        "letibot-fleet",
        Caps::default(),
    )
    .map_err(|e| e.to_string())?;
    let _ = client.detach();
    match hello {
        ServerFrame::Hello { sessions, .. } => Ok(sessions),
        other => Err(format!("expected Hello, got {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::{SessionRecord, Store};

    fn put(s: &Store, id: &str, workspace: &str, origin: &str, parent: Option<&str>) {
        s.put_session(&SessionRecord {
            id: id.into(),
            title: Some(format!("title of {id}")),
            model_id: "glm-coding/glm-4.7".into(),
            dialect_sha: "d".into(),
            workspace_root: workspace.into(),
            owner: "op".into(),
            approvers: vec![],
            role: None,
            parent_session_id: parent.map(String::from),
        })
        .expect("session");
        s.set_session_origin(id, origin).expect("origin");
    }

    /// Packages of two other workspaces are listed with their folder; one whose daemon is not
    /// running says so; this daemon's own, operators' sessions and children are not listed.
    #[test]
    fn packages_of_other_workspaces_are_listed_and_a_dead_daemon_says_so() {
        let dir = std::env::temp_dir().join(format!("fleet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sessions.db");
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        {
            let s = Store::open(&db).unwrap();
            put(&s, "s-a", "/w/one", "package", None);
            put(&s, "s-b", "/w/two", "package", None);
            put(&s, "s-own", "/w/here", "package", None);
            put(&s, "s-op", "/w/one", "operator", None);
            put(&s, "s-kid", "/w/one", "package", Some("s-a"));
        }
        let rows = scan_in(&db, Path::new("/w/here"), &run).expect("scan");
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(rows.len(), 2, "{ids:?}");
        for r in &rows {
            assert_eq!(r.state, FleetState::DaemonDown, "{r:?}");
            assert_eq!(r.state.word(), "daemon not running");
        }
        let a = rows.iter().find(|r| r.id == "s-a").unwrap();
        assert_eq!(a.workspace, "/w/one");
        assert_eq!(a.attach_command(), "cd /w/one && letibot --session s-a");
        assert!(!run.read_dir().unwrap().any(|_| true), "no daemon was started");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
