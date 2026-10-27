//! **The daemon's half of a session's leases** (RH9) — where a block of a shared host's ports is
//! taken, and the two moments it goes back.
//!
//! `crates/tools/src/lease.rs` is the engine and the seam; this is the implementation of that seam
//! over a real store, in the same shape `supervise::StoreBook` gives the process book: a path, a
//! session, one lazily-opened connection.
//!
//! # The two moments a lease goes back
//!
//! * **A session closes** ([`release_session`], called by `Sessions::close` and
//!   `close_backends`). That is an act, and it is the ordinary one.
//! * **The owner is found GONE** ([`reap`], called by the supervisor's tick). A daemon that was
//!   killed hard never ran the first one, and the row it left says the block is still held by a
//!   session nobody serves. The measurement that asked for this is RH7's: a session's processes are
//!   records so a restart finds them, and a lease is a record for the same reason.
//!
//! **A restart is not an owner being gone**, and the difference is [`GRACE_MS`]. A daemon that has
//! just come up holds no session at all for the first second of its life, so a reaper that asked
//! *is this session in the registry* would release every block on the box at each restart — which
//! is the opposite of what a lease is for. So a session must be seen gone for the grace before its
//! blocks go back, and a block taken in the last grace is never reaped at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use letibot_tokencore::store::{Ask, Lease, Store};
use letibot_tools::lease::{Blocks, Leases};

/// **How long the owner must be gone before its blocks go back.** Sixty seconds: long enough for a
/// daemon that has just started to reopen the sessions it is going to reopen (each open is a store
/// read and an eager first session at boot), short enough that a block left by a hard-killed daemon
/// is available to the next session within one turn.
pub const GRACE_MS: i64 = 60_000;

/// **A session's leases, over the store** — the implementation of
/// [`letibot_tools::lease::Leases`] the daemon registers.
pub struct StoreLeases {
    path: PathBuf,
    session: String,
    /// Opened on first use and kept, `StoreBook`'s shape for its reason: a session that never takes
    /// a block pays nothing, and one that takes two pays one open.
    store: Mutex<Option<Store>>,
}

impl StoreLeases {
    pub fn new(path: impl Into<PathBuf>, session: impl Into<String>) -> StoreLeases {
        StoreLeases {
            path: path.into(),
            session: session.into(),
            store: Mutex::new(None),
        }
    }

    /// Run `f` against the store, opening it once.
    fn with<T>(&self, f: impl FnOnce(&Store) -> Result<T, String>) -> Result<T, String> {
        let mut g = self
            .store
            .lock()
            .map_err(|_| "the lease connection is poisoned".to_string())?;
        if g.is_none() {
            *g =
                Some(Store::open(&self.path).map_err(|e| {
                    format!("the store `{}` did not open: {e}", self.path.display())
                })?);
        }
        match g.as_ref() {
            Some(store) => f(store),
            None => Err("the store did not open".to_string()),
        }
    }
}

impl Leases for StoreLeases {
    fn take(&self, ask: &Ask<'_>) -> Result<Lease, String> {
        self.with(|store| store.allocate_lease(ask).map_err(|e| e.to_string()))
    }

    fn release(&self, id: i64, by: &str) -> Result<(), String> {
        self.with(|store| {
            store
                .release_lease(id, by, Store::lease_now_ms())
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }
}

impl std::fmt::Debug for StoreLeases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreLeases")
            .field("session", &self.session)
            .finish()
    }
}

/// **The engine a session's tools and a close both use**: the operator's hosts, this box's ssh
/// material, and a [`StoreLeases`] behind it.
///
/// One function rather than two spellings, because a teardown that removed containers and a
/// teardown that did not would be two answers to *what does closing a session do*.
pub fn blocks(path: &Path, session: &str) -> Blocks {
    Blocks::new(session).with_leases(std::sync::Arc::new(StoreLeases::new(path, session)))
}

/// **Give back every block a session still holds, and remove exactly what it created** — a
/// session's close, the daemon's stop, and the reaper's act, in one place.
///
/// Returns one line per lease, so the caller can log what happened: the block, what was removed
/// from it, and any refusal the operator's never-touch list made.
pub fn release_session(path: &Path, session: &str, by: &str) -> Vec<String> {
    let store = match Store::open(path) {
        Ok(s) => s,
        Err(e) => {
            return vec![format!(
                "the store `{}` did not open, so {session}'s leases were left as they are: {e}",
                path.display()
            )];
        }
    };
    let held: Vec<Lease> = match store.leases_of_session(session) {
        Ok(v) => v.into_iter().filter(Lease::held).collect(),
        Err(e) => {
            return vec![format!(
                "{session}'s leases could not be read, so they were left as they are: {e}"
            )];
        }
    };
    if held.is_empty() {
        return Vec::new();
    }
    let engine = blocks(path, session);
    let mut said = Vec::new();
    for lease in &held {
        said.extend(engine.release(lease, by));
    }
    said
}

/// **Every block whose owner is gone goes back.**
///
/// `alive` is the daemon's own answer — the registry's — `seen` is the reaper's memory of *how long*
/// each session has been gone, and `release` is the act that does it. Three arguments rather than
/// one method call each because the three are tested apart: the grace is arithmetic, the act is a
/// teardown over ssh, and a test of the first must not reach the second.
///
/// `seen` lives with the supervisor (one per process) rather than here, so this takes it as an
/// argument and edits it: the fact is *the supervisor has been watching*, and a function that kept
/// it privately would be a second clock.
pub fn reap(
    store: &Store,
    alive: &dyn Fn(&str) -> bool,
    seen: &mut BTreeMap<String, i64>,
    at: i64,
    release: &dyn Fn(&str) -> Vec<String>,
) -> Vec<String> {
    let held = match store.held_leases() {
        Ok(v) => v,
        Err(e) => return vec![format!("the leases could not be read: {e}")],
    };
    let mut said = Vec::new();
    for session in due(&held, alive, seen, at) {
        seen.remove(&session);
        let lines = release(&session);
        if lines.is_empty() {
            said.push(format!("{session}'s leases went back: its owner was gone"));
        } else {
            said.extend(lines);
        }
    }
    said
}

/// **Which sessions' leases are due to go back** — the grace, and nothing else.
///
/// A session seen alive is stamped with `at` and kept. A session not seen for [`GRACE_MS`] is due.
/// **A session nobody has seen yet is not a session known to be gone**: the first tick that finds it
/// gone stamps it, and the grace runs from there — which is what makes a restart a restart and not
/// a wholesale release of every block on the box.
pub fn due(
    held: &[Lease],
    alive: &dyn Fn(&str) -> bool,
    seen: &mut BTreeMap<String, i64>,
    at: i64,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for session in held.iter().map(|l| l.session.as_str()) {
        if alive(session) {
            seen.insert(session.to_string(), at);
            continue;
        }
        let since = *seen.entry(session.to_string()).or_insert(at);
        if at - since >= GRACE_MS && !out.iter().any(|s| s == session) {
            out.push(session.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::Store;

    fn store_at(tag: &str) -> (PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("letibot-leases-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("sessions.db");
        let store = Store::open(&path).expect("a store");
        (path, store)
    }

    fn take(store: &Store, session: &str, ports: u16) -> Lease {
        let busy = std::collections::BTreeSet::new();
        store
            .allocate_lease(&Ask {
                session,
                host: Some("lab2x1"),
                prefix: "letibot-x-1",
                ports,
                ephemeral: (32768, 60999),
                busy: &busy,
                now_ms: 1,
            })
            .expect("a block")
    }

    /// **A restart is not an owner being gone.** The first tick stamps the session; the release
    /// happens only after the grace, and a session that comes back inside it keeps its block.
    #[test]
    fn a_lease_survives_the_grace_a_restart_needs() {
        let (_path, store) = store_at("grace");
        take(&store, "s-a", 2);
        let mut seen = BTreeMap::new();
        let held = store.held_leases().expect("held");
        // The daemon has just come up and holds nothing: not one block goes back.
        assert!(due(&held, &|_| false, &mut seen, 1_000).is_empty());
        // Half the grace later, still nothing.
        assert!(due(&held, &|_| false, &mut seen, 1_000 + GRACE_MS / 2).is_empty());
        // And the session is reopened before the grace is out — its block stays, for ever.
        assert!(due(&held, &|s| s == "s-a", &mut seen, 1_000 + GRACE_MS - 1).is_empty());
        // Even after the grace, a session that came back is not due.
        assert!(due(&held, &|s| s == "s-a", &mut seen, 1_000 + GRACE_MS * 3).is_empty());
        assert_eq!(store.held_leases().expect("held").len(), 1);
    }

    /// Past the grace, an owner nobody serves is gone and its block goes back — once, and the
    /// release hears its own name.
    #[test]
    fn a_lease_whose_owner_is_gone_is_released() {
        let (_path, store) = store_at("gone");
        let lease = take(&store, "s-b", 2);
        let mut seen = BTreeMap::new();
        let asked = Mutex::new(Vec::new());
        let release = |session: &str| {
            asked
                .lock()
                .expect("the recorder")
                .push(session.to_string());
            // What `release_session` would do, minus the ssh: the row goes back.
            store
                .release_session_leases(session, "its owner was gone", 1)
                .map(|_| vec![format!("the block of {session} went back")])
                .unwrap_or_default()
        };
        // The first look at it stamps the session; it is not gone YET.
        assert!(
            reap(&store, &|_| false, &mut seen, 0, &release).is_empty(),
            "a session nobody has seen is not known to be gone"
        );
        assert!(asked.lock().expect("the recorder").is_empty());
        // And once the grace is past, it is.
        let said = reap(&store, &|_| false, &mut seen, GRACE_MS + 1, &release);
        assert!(!said.is_empty(), "{said:?}");
        assert_eq!(
            *asked.lock().expect("the recorder"),
            vec!["s-b".to_string()]
        );
        assert!(store.held_leases().expect("held").is_empty());
        // The row is still there, released, with the reason on it.
        let after = store.lease(lease.id).expect("a query").expect("a row");
        assert_eq!(after.released_by.as_deref(), Some("its owner was gone"));
    }

    /// **A session closing releases its own blocks and nobody else's** — the difference between a
    /// teardown and a sweep.
    #[test]
    fn a_close_releases_one_sessions_blocks() {
        let (path, store) = store_at("close");
        let yours = take(&store, "s-c", 2);
        let theirs = take(&store, "s-d", 2);
        let said = release_session(&path, "s-c", "the session closing");
        assert!(!said.is_empty(), "{said:?}");
        let held = store.held_leases().expect("held");
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].session, "s-d");
        // And releasing again is a no-op rather than an error: three callers can reach one lease.
        assert!(release_session(&path, "s-c", "the session closing").is_empty());
        let _ = (yours, theirs);
    }
}
