//! **Moves another daemon made, heard by this one** (R92 item 5, lifecycle C30).
//!
//! A row moves in the shared store, and the daemon that moved it says so with a `QueueMoved` on
//! ITS hubs (`serve::RepoQueue::announce_move`, the verbs' `events`). Every other daemon on the
//! box heard nothing: a head attached to a session of a daemon that does not hold the
//! repository's lease — a second daemon seated in a worktree, a daemon in another folder — never
//! re-read the queues, and its edge kept whatever it last drew, or nothing.
//!
//! So every daemon reads the store's append-only `queue_transition` history every
//! [`POLL_EVERY`], and for each new move in a repository whose lease is NOT this daemon's,
//! broadcasts the `QueueMoved` the mover's daemon broadcast on its own hubs. A move in a
//! repository this daemon serves was already announced by this daemon's worker, so it is not
//! said twice. (A `/queue` verb typed in this daemon about a repository another daemon serves is
//! announced by the verb AND by this watcher; that costs one extra re-read on the heads, which
//! coalesce a burst into one.)

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::Store;

/// How often a daemon reads the store for moves another daemon made — the serve loop's own pace.
pub const POLL_EVERY: Duration = Duration::from_secs(2);

/// How many moves one poll reads at most — a head re-reads the whole snapshot on one event, so a
/// burst larger than this loses only sentences in the log, never freshness.
const BATCH: usize = 200;

/// **Where this daemon has read the history up to.**
pub struct MoveWatch {
    seen: i64,
}

impl MoveWatch {
    /// Start at the history's current end: what moved before this daemon started is in the
    /// snapshot every head asks for when it attaches.
    pub fn start(store: &Store) -> Self {
        let seen = store
            .recent_transitions(1)
            .ok()
            .and_then(|t| t.last().map(|t| t.seq))
            .unwrap_or(0);
        MoveWatch { seen }
    }

    /// **The moves since the last poll that this daemon did not announce itself** — those in a
    /// repository whose lease `me` does not hold — as the events to broadcast.
    pub fn poll(&mut self, store: &Store, me: &str) -> Vec<SessionEvent> {
        let Ok(moves) = store.recent_transitions(BATCH) else {
            return Vec::new();
        };
        let fresh: Vec<_> = moves.into_iter().filter(|t| t.seq > self.seen).collect();
        let Some(last) = fresh.last() else {
            return Vec::new();
        };
        self.seen = last.seq;
        let mine: Vec<String> = store
            .queue_leases()
            .unwrap_or_default()
            .into_iter()
            .filter(|l| l.owner == me)
            .map(|l| l.repository)
            .collect();
        fresh
            .into_iter()
            .filter(|t| !mine.contains(&t.repository))
            .map(|t| SessionEvent::QueueMoved {
                queue: t.queue,
                id: t.entry_id,
                to: t.to_state,
                why: t.why,
            })
            .collect()
    }
}

/// **The watcher's thread** — polls until `stop`, broadcasting through `announce`.
pub fn spawn(
    store_path: PathBuf,
    me: String,
    announce: Arc<dyn Fn(SessionEvent) + Send + Sync>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("queue-moves".into())
        .spawn(move || {
            let mut watch: Option<(Store, MoveWatch)> = None;
            while !stop.load(Ordering::Relaxed) {
                if watch.is_none() {
                    watch = Store::open(&store_path).ok().map(|s| {
                        let w = MoveWatch::start(&s);
                        (s, w)
                    });
                }
                if let Some((store, w)) = watch.as_mut() {
                    for e in w.poll(store, &me) {
                        announce(e);
                    }
                }
                super::sleep_unless_stopped(POLL_EVERY, &stop);
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mergequeue::serve::tests::TempDir;
    use letibot_tokencore::store::{LeaseClaim, LeaseOutcome};

    fn claim(owner: &str) -> LeaseClaim {
        LeaseClaim {
            owner: owner.into(),
            pid: 1,
            host: "box".into(),
            build: "test".into(),
        }
    }

    fn moved(store: &Store, repo: &str, id: &str, to: &str) {
        store
            .record_transition("target", id, repo, None, to, "someone", "why", 1)
            .unwrap();
    }

    /// **A daemon that does not hold the lease hears the holder's moves** — the reason a head
    /// attached to a second daemon never refreshed its queue standing. And a move in a
    /// repository this daemon serves is not said twice: its own worker said it.
    #[test]
    fn a_daemon_hears_moves_in_repositories_another_daemon_serves_and_only_those() {
        let dir = TempDir::new("watch");
        let store = Store::open(&dir.0.join("sessions.db")).unwrap();
        let now = crate::mergequeue::gate::now_ms();
        assert!(matches!(
            store
                .acquire_queue_lease("/r/theirs", &claim("other/2/1"), now, 90_000)
                .unwrap(),
            LeaseOutcome::Held { .. }
        ));
        assert!(matches!(
            store
                .acquire_queue_lease("/r/mine", &claim("me/1/1"), now, 90_000)
                .unwrap(),
            LeaseOutcome::Held { .. }
        ));
        moved(&store, "/r/theirs", "before", "red");
        let mut w = MoveWatch::start(&store);
        assert!(w.poll(&store, "me/1/1").is_empty(), "the past is not news");

        moved(&store, "/r/theirs", "main", "red");
        moved(&store, "/r/mine", "main", "green");
        moved(&store, "/r/nobody", "main", "blocked");
        let heard = w.poll(&store, "me/1/1");
        let ids: Vec<(String, String)> = heard
            .iter()
            .map(|e| match e {
                SessionEvent::QueueMoved { id, to, .. } => (id.clone(), to.clone()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            ids,
            vec![
                ("main".to_string(), "red".to_string()),
                ("main".to_string(), "blocked".to_string())
            ],
            "the other daemon's repository and the unserved one; never this daemon's own"
        );
        assert!(
            w.poll(&store, "me/1/1").is_empty(),
            "each move is heard once"
        );
    }

    /// **A head is told which repository its session is a session of** (R92 item 5) — the
    /// daemon's own rule, carried on the snapshot it answers, so the head scopes its pane and
    /// edge without guessing. A worktree is its repository; a directory of none is `None`.
    #[test]
    fn the_snapshot_names_the_asking_sessions_repository() {
        use crate::mergequeue::serve::tests::{branch, repo};
        use letibot_sessionlog::registry::Registry;
        let dir = TempDir::new("watch-here");
        let main = repo(&dir.0);
        let (wt, _, _) = branch(&main, "h", "h.txt");
        let nowhere = dir.0.join("nowhere");
        std::fs::create_dir_all(&nowhere).unwrap();
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        for (id, ws) in [("s-wt", &wt), ("s-none", &nowhere)] {
            store
                .put_session(&letibot_tokencore::store::SessionRecord {
                    id: id.into(),
                    title: None,
                    model_id: String::new(),
                    dialect_sha: String::new(),
                    workspace_root: ws.display().to_string(),
                    owner: String::new(),
                    approvers: Vec::new(),
                    role: None,
                    parent_session_id: None,
                })
                .unwrap();
        }
        let mut cfg = crate::config::Config::for_this_box(&dir.0);
        cfg.store = Some(db);
        let reg = Registry::new();
        reg.set_source(crate::sessions::StoreSessions::open(&cfg).unwrap());

        let here = reg.queues_for("s-wt").here.expect("the daemon says");
        assert_eq!(
            here.repository.as_deref(),
            crate::mergequeue::git::repository_key(&main).as_deref(),
            "a worktree's session is a session of its repository"
        );
        let here = reg.queues_for("s-none").here.expect("the daemon says");
        assert_eq!(here.repository, None, "{here:?}");
        assert!(here.workspace.ends_with("nowhere"), "{here:?}");
    }
}
