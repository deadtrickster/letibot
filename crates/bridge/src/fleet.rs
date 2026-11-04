//! Every package session on this box, from any daemon: the sessions come from the shared store,
//! the live state from each folder's own daemon. Nothing here starts a daemon.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::SessionBrief;
use letibot_sessionlog::{Fleet, FleetRow, FleetState, SessionOrigin};

use crate::daemon::{is_up, key, rundir, socket_in};

/// How long a daemon has to answer the attach, connect and `Hello` together.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

/// A package whose daemon is stopped is listed while its last activity is this recent.
pub const STOPPED_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// Package sessions of daemons other than `own`, newest activity first: every one whose daemon
/// is running, and stopped ones active within [`STOPPED_WINDOW_MS`]. The older stopped ones are
/// counted in `unlisted`. A store that cannot be read is an error and not an empty fleet.
pub fn scan(store: &Path, own: &Path) -> Result<Fleet, String> {
    scan_in(store, own, &rundir(), now_ms(), ATTACH_TIMEOUT)
}

fn now_ms() -> i64 {
    letibot_sessionlog::registry::now_ms() as i64
}

/// The reader a head holds: the folder it was started in decides which daemon is its own. A scan
/// that fails reads as no fleet.
pub fn reader() -> letibot_sessionlog::FleetReader {
    std::sync::Arc::new(|| {
        let d = match std::env::current_dir()
            .map_err(|e| e.to_string())
            .and_then(|cwd| crate::daemon::resolve(&cwd))
        {
            Ok(d) => d,
            Err(_) => return Fleet::default(),
        };
        scan(&d.store, &d.workspace).unwrap_or_default()
    })
}

pub fn scan_in(
    store: &Path,
    own: &Path,
    rundir: &Path,
    now: i64,
    timeout: Duration,
) -> Result<Fleet, String> {
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
    let mut unlisted = 0usize;
    for (workspace, members) in &by_workspace {
        let socket = socket_in(rundir, &key(Path::new(workspace)));
        let placement = letibot_sessionlog::placement::read(&socket);
        let live = if is_up(&socket) {
            Some(briefs(&socket, &stored[members[0]].id, timeout))
        } else {
            None
        };
        for &i in members {
            let s = &stored[i];
            if live.is_none() && now.saturating_sub(s.last_activity_ms) > STOPPED_WINDOW_MS {
                unlisted += 1;
                continue;
            }
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
    Ok(Fleet {
        rows: out.into_iter().flatten().collect(),
        unlisted,
    })
}

/// What the daemon at `socket` says it holds, from the `Hello` of one attach to `session`.
/// The attach runs on its own thread so a daemon that accepts and never answers costs this scan
/// `timeout` and not its life. The thread is abandoned; it ends when the daemon closes the socket.
fn briefs(socket: &Path, session: &str, timeout: Duration) -> Result<Vec<SessionBrief>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (socket, session) = (socket.to_path_buf(), session.to_string());
    let spawned = std::thread::Builder::new()
        .name("fleet-attach".into())
        .spawn(move || {
            let _ = tx.send(attach_once(&socket, &session));
        });
    if let Err(e) = spawned {
        return Err(format!("no thread to ask with: {e}"));
    }
    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| Err(format!("timed out after {}s", timeout.as_secs().max(1))))
}

fn attach_once(socket: &Path, session: &str) -> Result<Vec<SessionBrief>, String> {
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
        let rows = scan_in(&db, Path::new("/w/here"), &run, now_ms(), ATTACH_TIMEOUT)
            .expect("scan")
            .rows;
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(rows.len(), 2, "{ids:?}");
        for r in &rows {
            assert_eq!(r.state, FleetState::DaemonDown, "{r:?}");
            assert_eq!(r.state.word(), "daemon not running");
        }
        let a = rows.iter().find(|r| r.id == "s-a").unwrap();
        assert_eq!(a.workspace, "/w/one");
        assert_eq!(a.attach_command(), "cd /w/one && letibot --session s-a");
        assert!(
            !run.read_dir().unwrap().any(|_| true),
            "no daemon was started"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A daemon that takes the connection and never answers costs the scan the timeout, and its
    /// row says so. Stopped packages past the window are counted and not listed; one whose daemon
    /// is running is listed however old.
    #[test]
    fn a_hung_daemon_times_out_and_old_stopped_packages_are_counted() {
        let dir = std::env::temp_dir().join(format!("fh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("s.db");
        let run = dir.join("r");
        std::fs::create_dir_all(&run).unwrap();
        {
            let s = Store::open(&db).unwrap();
            put(&s, "s-hung", "/w/hung", "package", None);
            put(&s, "s-old", "/w/old", "package", None);
            put(&s, "s-old2", "/w/old", "package", None);
        }
        // Accepts and holds the connection open without a word.
        let socket = socket_in(&run, &key(Path::new("/w/hung")));
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        // Every connection is kept open, the liveness probe's included.
        let holder = std::thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok((c, _)) = listener.accept() {
                held.push(c);
            }
        });
        let later = now_ms() + 3 * STOPPED_WINDOW_MS;
        let t = std::time::Instant::now();
        let fleet = scan_in(
            &db,
            Path::new("/w/here"),
            &run,
            later,
            Duration::from_secs(1),
        )
        .expect("scan");
        assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
        assert_eq!(fleet.rows.len(), 1, "{:?}", fleet.rows);
        assert_eq!(fleet.rows[0].id, "s-hung");
        assert_eq!(
            fleet.rows[0].state.word(),
            "daemon not answering: timed out after 1s"
        );
        assert_eq!(fleet.unlisted, 2);
        assert_eq!(
            fleet.unlisted_line().as_deref(),
            Some("2 older stopped packages not listed - `letibot --sessions` lists them")
        );
        // Inside the window the same stopped packages are listed.
        let fresh = scan_in(
            &db,
            Path::new("/w/here"),
            &run,
            now_ms(),
            Duration::from_secs(1),
        )
        .expect("scan");
        assert_eq!((fresh.rows.len(), fresh.unlisted), (3, 0));
        drop(holder);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
