//! Every package session on this box, from any daemon: the sessions come from the shared store,
//! the live state from each folder's own daemon. Nothing here starts a daemon.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
    let store = crate::store::open_read(store, "session", Some(own)).map_err(|e| e.to_string())?;
    let mut stored = store.list_sessions().map_err(|e| e.to_string())?;
    let origin_of = |s: &letibot_tokencore::store::StoredSession| {
        s.origin
            .as_deref()
            .and_then(SessionOrigin::parse)
            .unwrap_or_default()
    };
    // The same test the picker and `--continue` use for the operator's own.
    let is_mine = |s: &letibot_tokencore::store::StoredSession| {
        letibot_sessionlog::is_operator_top_level(&letibot_sessionlog::GroupRow {
            id: &s.id,
            parent: s.parent_session_id.as_deref(),
            origin: origin_of(s),
            role: s.role.as_deref(),
            title: s.title.as_deref().unwrap_or(""),
        })
    };
    stored.retain(|s| {
        s.parent_session_id.is_none()
            && Path::new(&s.workspace_root) != own
            && (origin_of(s) == SessionOrigin::Package || is_mine(s))
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
    let mut mine = vec![false; stored.len()];
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
            mine[i] = is_mine(s);
            if live.is_none() && now.saturating_sub(s.last_activity_ms) > STOPPED_WINDOW_MS {
                if !mine[i] {
                    unlisted += 1;
                }
                continue;
            }
            let state = match &live {
                None => FleetState::DaemonDown,
                Some(Err(why)) => FleetState::Unreachable(why.clone()),
                Some(Ok(all)) => match all.iter().find(|b| b.session_id == s.id) {
                    Some(b) if b.waits_on_operator > 0 || !b.pending.is_empty() => {
                        FleetState::Asking
                    }
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
    let mut rows = Vec::new();
    let mut operators = Vec::new();
    for (i, row) in out.into_iter().enumerate() {
        if let Some(row) = row {
            if mine[i] {
                operators.push(row);
            } else {
                rows.push(row);
            }
        }
    }
    Ok(Fleet {
        rows,
        mine: operators,
        unlisted,
    })
}

/// What the daemon at `socket` says it holds, from the `Hello` of one attach to `session`.
/// The attach runs on its own thread so a daemon that accepts and never answers costs this scan
/// `timeout` and not its life. The thread is abandoned; it ends when the daemon closes the socket.
pub(crate) fn briefs(
    socket: &Path,
    session: &str,
    timeout: Duration,
) -> Result<Vec<SessionBrief>, String> {
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

/// Queue `text` into `session` through the daemon at `socket`, as `from`'s line, and wait for the
/// daemon to take it. A one-shot head: it holds nothing after, so the session stays whoever's it
/// was. Bounded like [`briefs`], for the same daemon that accepts and never answers.
pub(crate) fn prompt_once(
    socket: &Path,
    session: &str,
    text: &str,
    from: &str,
    timeout: Duration,
) -> Result<u64, String> {
    // The id is the prompt's own identity, recorded before anything is sent: a call that timed out
    // may still be in flight in its thread, so a second submit of the same words is refused rather
    // than run twice. A daemon's refusal is an answer, and forgetting the id then lets a corrected
    // prompt through.
    let id = prompt_id(session, from, text);
    remember(&id)?;
    let where_to = session.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    let (socket, session, text, from) = (
        socket.to_path_buf(),
        session.to_string(),
        text.to_string(),
        from.to_string(),
    );
    let spawned = std::thread::Builder::new()
        .name("prompt-once".into())
        .spawn(move || {
            let _ = tx.send(prompt_through(&socket, &session, &text, &from));
        });
    if let Err(e) = spawned {
        forget(&id);
        return Err(format!("no thread to prompt with: {e}"));
    }
    match rx.recv_timeout(timeout) {
        Ok(Ok(seq)) => Ok(seq),
        Ok(Err(Said::Before(why))) => {
            forget(&id);
            Err(why)
        }
        Ok(Err(Said::After(why))) => Err(why),
        Err(_) => Err(format!(
            "`{where_to}` may have taken the prompt: no answer within {}s, so it was sent and is \
             not sent again within {} minutes",
            timeout.as_secs_f32(),
            PROMPT_WINDOW.as_secs() / 60
        )),
    }
}

/// How long a prompt id is remembered. Long enough to catch a caller retrying a send it saw time
/// out, short enough that a person saying one sentence twice in a session is not refused.
const PROMPT_WINDOW: Duration = Duration::from_secs(600);

/// Where the ids this process has already sent are kept, and when.
fn prompts() -> &'static Mutex<std::collections::BTreeMap<String, Instant>> {
    static PROMPTS: std::sync::OnceLock<Mutex<std::collections::BTreeMap<String, Instant>>> =
        std::sync::OnceLock::new();
    PROMPTS.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

/// One prompt's identity: who said it, into which session, in what words. FNV-1a, because the map
/// is local and the value only has to differ between different prompts.
fn prompt_id(session: &str, from: &str, text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in session
        .bytes()
        .chain([0])
        .chain(from.bytes())
        .chain([0])
        .chain(text.bytes())
    {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Record a prompt as sent, or refuse it as one already sent inside the window.
fn remember(id: &str) -> Result<(), String> {
    let now = Instant::now();
    let mut sent = prompts().lock().unwrap_or_else(|e| e.into_inner());
    sent.retain(|_, at| now.duration_since(*at) < PROMPT_WINDOW);
    if sent.contains_key(id) {
        return Err(format!(
            "this prompt was already sent within the last {} minutes, and sending it again would \
             run its turn twice: say something with different words if the turn never started",
            PROMPT_WINDOW.as_secs() / 60
        ));
    }
    sent.insert(id.to_string(), now);
    Ok(())
}

/// Forget a prompt that certainly did not reach the daemon, so a retry after a real failure works.
fn forget(id: &str) {
    prompts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(id);
}

/// What `prompt_through` says: whether the prompt was already submitted when it failed, which is
/// what decides whether the caller may try again.
enum Said {
    Before(String),
    After(String),
}

fn prompt_through(socket: &Path, session: &str, text: &str, from: &str) -> Result<u64, Said> {
    let (mut client, hello, mut reader) = HeadClient::attach(
        socket,
        session,
        0,
        crate::daemon::HEAD_KIND,
        crate::daemon::IDENTITY,
        Caps::default(),
    )
    .map_err(|e| Said::Before(e.to_string()))?;
    // The line is sent as this seat's, and `Prompt.from` arrived at 74: below it the daemon keeps
    // the field nowhere, which records the bridge's words as the operator's.
    if let ServerFrame::Hello {
        protocol_version, ..
    } = &hello
        && *protocol_version < letibot_sessionlog::PROMPT_FROM_SINCE
    {
        return Err(Said::Before(format!(
            "`{session}`'s daemon speaks protocol {protocol_version} and a prompt that names its \
             author arrived at {}: it would store this line as the operator's own. Nothing was \
             sent; restart that daemon from the installed build.",
            letibot_sessionlog::PROMPT_FROM_SINCE
        )));
    }
    let id = client
        .prompt_from(0, text, Vec::new(), None, Some(from.to_string()))
        .map_err(|e| Said::Before(e.to_string()))?;
    let out = loop {
        match reader.read::<ServerFrame>() {
            Ok(ServerFrame::Accepted {
                client_request_id,
                seq,
                ..
            }) if client_request_id == id => break Ok(seq),
            Ok(ServerFrame::Rejected {
                client_request_id,
                reason,
                ..
            }) if client_request_id == id => {
                break Err(Said::Before(format!("the daemon refused it: {reason}")));
            }
            // Submitted and unanswered: the prompt may be in the session's queue already.
            Ok(ServerFrame::Bye { reason }) => {
                break Err(Said::After(format!("the daemon went away: {reason}")));
            }
            Ok(_) | Err(letibot_sessionlog::wire::WireError::Malformed { .. }) => {}
            Err(e) => break Err(Said::After(e.to_string())),
        }
    };
    let _ = client.detach();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::{SessionRecord, Store};

    /// A daemon that says hello, reads the prompt and never answers: the client gives up on it, and
    /// the second `prompt_once` of the same words must not put a second prompt on the wire.
    #[test]
    fn prompt_once_timeout_does_not_double_submit() {
        use std::io::{BufRead, BufReader, Write};
        let dir = std::env::temp_dir().join(format!("letibot-fleet-prompt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("d.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        // Every frame the fake daemon is handed after its hello: the prompts are the ones that name
        // `tools/call`-shaped commands, and their count is what a double submit would change.
        let seen: std::sync::Arc<Mutex<Vec<String>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
        let theirs = seen.clone();
        let held = std::thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                // One thread per connection: a client that gave up still holds its socket open, and
                // a single-threaded fake would never hear the next prompt.
                let theirs = theirs.clone();
                std::thread::spawn(move || {
                    let mut w = stream.try_clone().unwrap();
                    let mut r = BufReader::new(stream);
                    let mut line = String::new();
                    if r.read_line(&mut line).is_err() || line.is_empty() {
                        return;
                    }
                    let hello = ServerFrame::Hello {
                        protocol_version: letibot_sessionlog::PROTOCOL_VERSION,
                        session_id: "s-x".into(),
                        head_id: "h-s-x".into(),
                        dropped: 0,
                        snapshot: None,
                        resumed_from: None,
                        scrubbed: Default::default(),
                        wiring: Default::default(),
                        sessions: Vec::new(),
                    };
                    let _ = writeln!(w, "{}", serde_json::to_string(&hello).unwrap());
                    let mut frame = String::new();
                    while let Ok(n) = r.read_line(&mut frame) {
                        if n == 0 {
                            break;
                        }
                        theirs.lock().unwrap().push(frame.trim().to_string());
                        frame.clear();
                    }
                });
            }
        });
        let timeout = Duration::from_secs(1);
        let first = prompt_once(&socket, "s-x", "carry on", "cc@seat", timeout).unwrap_err();
        assert!(
            first.contains("may have taken the prompt"),
            "a timeout may not say the prompt never arrived: {first}"
        );
        let second = prompt_once(&socket, "s-x", "carry on", "cc@seat", timeout).unwrap_err();
        assert!(
            second.contains("already sent"),
            "the retry is refused rather than run twice: {second}"
        );
        // A different prompt is a different id, so it still goes.
        let _ = prompt_once(&socket, "s-x", "and again", "cc@seat", timeout);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && seen.lock().unwrap().len() < 2 {
            std::thread::sleep(Duration::from_millis(20));
        }
        let frames = seen.lock().unwrap().clone();
        assert_eq!(
            frames.len(),
            2,
            "two prompts on the wire, not three: {frames:?}"
        );
        assert!(frames[0].contains("carry on"), "{}", frames[0]);
        assert!(frames[1].contains("and again"), "{}", frames[1]);
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        assert_eq!(
            a.attach_command(),
            "cd /w/one && letibot --session 'title of s-a'",
            "a named session is attached by its name"
        );
        let mut bare = a.clone();
        bare.title.clear();
        assert_eq!(bare.attach_command(), "cd /w/one && letibot --session s-a");
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
        // The daemon's share of a scan is the attach, timed alone: the store read beside it can
        // stall for seconds on a loaded disk. The bound only has to tell a timeout from a hang.
        let t = std::time::Instant::now();
        let asked = briefs(&socket, "s-hung", Duration::from_secs(1));
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        assert_eq!(asked.err().as_deref(), Some("timed out after 1s"));
        let later = now_ms() + 3 * STOPPED_WINDOW_MS;
        let fleet = scan_in(
            &db,
            Path::new("/w/here"),
            &run,
            later,
            Duration::from_secs(1),
        )
        .expect("scan");
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

    /// The operator's own session in another folder (the `~` session, renamed) is listed beside the
    /// packages and apart from them. The queue's and a child's are not.
    #[test]
    fn a_renamed_operator_session_of_another_folder_is_listed_as_theirs() {
        let dir = std::env::temp_dir().join(format!("fleet-mine-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sessions.db");
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        {
            let s = Store::open(&db).unwrap();
            put(&s, "s-home", "/home/dead", "operator", None);
            put(&s, "s-gatekeeper-c2f63a0e6a17", "/w/one", "operator", None);
            put(&s, "s-q", "/w/one", "queue", None);
            put(&s, "s-kid", "/w/one", "operator", Some("s-home"));
            put(&s, "s-pkg", "/w/two", "package", None);
        }
        let fleet =
            scan_in(&db, Path::new("/w/here"), &run, now_ms(), ATTACH_TIMEOUT).expect("scan");
        let mine: Vec<&str> = fleet.mine.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(mine, ["s-home"]);
        let packages: Vec<&str> = fleet.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(packages, ["s-pkg"]);
    }
}
