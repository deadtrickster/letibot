//! Many sessions behind one socket: the thing `harnessd` disclosed it did not have.
//!
//! > *"**One session per process.** `Hub` is per-session by construction and
//! > §13.2's multi-session daemon is M5's problem, not a thing to half-build
//! > here."* — `letibot_harnessd`'s own startup disclosure.
//!
//! `Hub` stays per-session by construction, and that is the load-bearing part: a
//! session's log, view, heads and command queue are one object and nothing here
//! reaches inside one. What was missing was **management** — a way to name a second
//! session, to find it, to list what exists and to serve a worker that has more
//! than one.
//!
//! ```text
//!            ┌─────────── Registry ───────────┐
//!   head ────┤  "s-1" → Hub ─┐                │
//!   head ────┤  "s-2" → Hub ─┼── Bell ────────┼──> one worker
//!   head ────┤  "s-3" → Hub ─┘                │    (harnessd's Sessions)
//!            └────────────────────────────────┘
//! ```
//!
//! # The three things that had to stay true, and how
//!
//! **The ledger, the memfd region and the prefix invariant are per session.** They
//! are not touched here at all, which is the strongest form of that guarantee:
//! `TokenLedger::new` creates its own `TokenRegion` over its own memfd, the region
//! is not `Clone`, and appending needs `&mut`. Two sessions cannot share a token
//! region because there is no expression that would make them. What this module
//! adds is a `Hub` per session, and `harnessd` opens a `Harness` — and therefore a
//! `Session`, a ledger and a region — per `Hub`.
//!
//! **A head attaching mid-turn still works, per session.** Also untouched:
//! `Hub::attach` cuts the snapshot and registers the subscriber under one lock, and
//! this module hands out the `Arc<Hub>` and gets out of the way. §13.2b's mechanics
//! are all inside the hub, so "which session" is resolved *before* any of them
//! start.
//!
//! **No timer and no poll loop** (§18.1-I12). A worker serving N sessions cannot
//! block on N condvars, and the wrong fix is to wake it every 50 ms and look. So a
//! [`Bell`] is a single condition every hub rings when it queues a command; the
//! worker blocks on that one, and is told *which* session to drain. Three blocking
//! reads and no clock is still three blocking reads and no clock.
//!
//! # Why the bell carries session ids rather than being a bare wake
//!
//! A bare "something happened" wake would make the worker scan every session's
//! queue on every command, which is O(sessions) per command and, worse, gives a
//! session with a fast head a structural advantage over one further down the Vec.
//! The queue of ids preserves **arrival order across sessions**, so two heads
//! prompting two different sessions are served in the order they pressed enter —
//! which is the same promise §13.2 already makes for two heads on one session.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use crate::hub::{Hub, QueuedCommand, SessionStatus};
use crate::log::LogBounds;
use crate::view::ViewBounds;

/// A wake shared by every session in a registry.
///
/// Not a channel: a channel would need the hub to own a `Sender`, and a hub that
/// owns a `Sender` cannot be created before the registry that receives from it.
/// This is a `Mutex<VecDeque<String>>` and a `Condvar`, which is the same shape as
/// the hub's own command queue one level up.
#[derive(Debug, Default)]
pub struct Bell {
    inner: Mutex<BellInner>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct BellInner {
    pending: VecDeque<String>,
    closed: bool,
}

impl Bell {
    pub fn new() -> Arc<Bell> {
        Arc::new(Bell::default())
    }

    /// Say that `session_id` has something for the worker.
    pub fn ring(&self, session_id: &str) {
        {
            let mut g = self.lock();
            g.pending.push_back(session_id.to_string());
        }
        self.cv.notify_all();
    }

    /// Wake every waiter, for good. `next` then returns `None`.
    pub fn close(&self) {
        self.lock().closed = true;
        self.cv.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Block until some session has been rung. `None` once the bell is closed
    /// **and drained** — a shutdown does not discard a command that was already
    /// accepted and announced on the log, because a head was told `Accepted` and
    /// the announcement is in the transcript.
    pub fn next(&self) -> Option<String> {
        let mut g = self.lock();
        loop {
            if let Some(id) = g.pending.pop_front() {
                return Some(id);
            }
            if g.closed {
                return None;
            }
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BellInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What the daemon knows about a session that the log does not: its title, when it
/// was made, and what it is talking to.
///
/// Sent to a head in `Hello` and in `Sessions`, which is what a session picker is
/// drawn from. `#[serde(default)]` nowhere: a field that is absent and a field that
/// is empty must not look the same, the same rule the rest of the protocol keeps.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionBrief {
    pub session_id: String,
    /// A human name. Empty until somebody sets one; a head shows the id then,
    /// rather than inventing a title from the first prompt — a title guessed from
    /// content is a title that changes under you.
    pub title: String,
    /// Unix millis when the daemon created it.
    pub created_ms: u64,
    /// Everything the log knows, cut in one lock.
    pub status: SessionStatus,
    /// What this session is talking to (`crates/ui/DESIGN.md` §4.4).
    pub wiring: SessionWiring,
}

/// What a session is attached to. The daemon's own command line, which is the only
/// place these four facts exist — and, before this, the only place, full stop: a
/// head could name the model (and only during a turn) and could never name the
/// dialect, the endpoint or the workspace at all.
///
/// Empty strings for a daemon whose owner supplied none, never a plausible
/// default: a guessed endpoint on a screen is a guess somebody later quotes.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize,
)]
pub struct SessionWiring {
    pub model: String,
    pub dialect: String,
    pub endpoint: String,
    pub workspace: String,
}

impl SessionWiring {
    /// `model · dialect · endpoint`, skipping whichever of them is unknown.
    ///
    /// One function because the composer's legend, the session picker and the
    /// header all want it and three spellings of it drift.
    pub fn summary(&self) -> String {
        [&self.model, &self.dialect, &self.endpoint]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

struct Entry {
    hub: Arc<Hub>,
    title: String,
    created_ms: u64,
    wiring: SessionWiring,
}

struct Inner {
    entries: Vec<(String, Entry)>,
    /// The session a head that names none gets. The last one anybody *used*, not
    /// the last one created: a head that reattaches after a crash wants the session
    /// it was in.
    default_id: String,
    log_bounds: LogBounds,
    view_bounds: ViewBounds,
}

/// Every session this daemon is serving.
pub struct Registry {
    inner: Mutex<Inner>,
    bell: Arc<Bell>,
}

/// Why a session could not be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateError {
    /// A session with that id is already here. Not silently returned instead: a
    /// caller who asked to *create* and got somebody else's session would write
    /// into it.
    Exists(String),
    /// The registry is shutting down.
    Closed,
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateError::Exists(id) => write!(f, "session {id} already exists"),
            CreateError::Closed => write!(f, "the daemon is shutting down"),
        }
    }
}

impl Registry {
    pub fn new() -> Arc<Registry> {
        Registry::with_bounds(LogBounds::default(), ViewBounds::default())
    }

    pub fn with_bounds(log_bounds: LogBounds, view_bounds: ViewBounds) -> Arc<Registry> {
        Arc::new(Registry {
            inner: Mutex::new(Inner {
                entries: Vec::new(),
                default_id: String::new(),
                log_bounds,
                view_bounds,
            }),
            bell: Bell::new(),
        })
    }

    /// A registry holding one hub that already exists. The single-session case,
    /// which is what `serve(hub, path)` and every test that predates this module
    /// take.
    pub fn of(hub: Arc<Hub>) -> Arc<Registry> {
        let r = Registry::new();
        let id = hub.session_id();
        hub.set_bell(r.bell.clone());
        {
            let mut g = r.lock();
            g.default_id = id.clone();
            g.entries.push((
                id,
                Entry {
                    hub,
                    title: String::new(),
                    created_ms: now_ms(),
                    wiring: SessionWiring::default(),
                },
            ));
        }
        r
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn bell(&self) -> &Arc<Bell> {
        &self.bell
    }

    /// Make a session and hand back its hub.
    ///
    /// The hub is bound to the bell **before** it is published in `entries`, so
    /// there is no window in which a head could reach it, submit a command and have
    /// nothing wake the worker.
    pub fn create(
        &self,
        session_id: impl Into<String>,
        title: impl Into<String>,
        wiring: SessionWiring,
    ) -> Result<Arc<Hub>, CreateError> {
        let id = session_id.into();
        let mut g = self.lock();
        if self.bell.is_closed() {
            return Err(CreateError::Closed);
        }
        if g.entries.iter().any(|(k, _)| k == &id) {
            return Err(CreateError::Exists(id));
        }
        let hub = Hub::with_bounds(id.clone(), g.log_bounds, g.view_bounds);
        hub.set_bell(self.bell.clone());
        if g.default_id.is_empty() {
            g.default_id = id.clone();
        }
        g.entries.push((
            id,
            Entry {
                hub: hub.clone(),
                title: title.into(),
                created_ms: now_ms(),
                wiring,
            },
        ));
        Ok(hub)
    }

    pub fn get(&self, session_id: &str) -> Option<Arc<Hub>> {
        self.lock()
            .entries
            .iter()
            .find(|(k, _)| k == session_id)
            .map(|(_, e)| e.hub.clone())
    }

    /// Resolve what a head asked to attach to.
    ///
    /// An empty id means *"whatever this daemon calls current"*, which is what
    /// `letibot-tui` with no `--session` has always sent and what the single-session
    /// daemon answered with its only hub. An id nobody has is `None`, and the caller
    /// refuses it by name — **not** by silently seating the head somewhere else,
    /// which would make a typo look like a working attach to an empty session.
    pub fn resolve(&self, session_id: &str) -> Option<Arc<Hub>> {
        let g = self.lock();
        let want = if session_id.is_empty() {
            g.default_id.clone()
        } else {
            session_id.to_string()
        };
        g.entries
            .iter()
            .find(|(k, _)| k == &want)
            .map(|(_, e)| e.hub.clone())
    }

    /// Remember that this is the session a bare attach should land on.
    pub fn set_default(&self, session_id: &str) {
        let mut g = self.lock();
        if g.entries.iter().any(|(k, _)| k == session_id) {
            g.default_id = session_id.to_string();
        }
    }

    pub fn default_id(&self) -> String {
        self.lock().default_id.clone()
    }

    /// One session, as a picker row. `None` if it is not here.
    pub fn brief(&self, session_id: &str) -> Option<SessionBrief> {
        self.list().into_iter().find(|b| b.session_id == session_id)
    }

    /// What a session is attached to. Empty for a session nobody registered.
    pub fn wiring(&self, session_id: &str) -> SessionWiring {
        self.lock()
            .entries
            .iter()
            .find(|(k, _)| k == session_id)
            .map(|(_, e)| e.wiring.clone())
            .unwrap_or_default()
    }

    /// Name a session. Empty is allowed and means "no name"; a head then shows the
    /// id, which is the honest fallback.
    pub fn set_title(&self, session_id: &str, title: impl Into<String>) {
        let mut g = self.lock();
        if let Some((_, e)) = g.entries.iter_mut().find(|(k, _)| k == session_id) {
            e.title = title.into();
        }
    }

    /// Every session, in creation order, each cut under its own hub's lock.
    ///
    /// The hubs are cloned out from under the registry lock first, so listing a
    /// session cannot be blocked by a publish into a different one — and so the
    /// registry lock is never held while a hub lock is taken, which is the whole of
    /// this file's lock ordering.
    pub fn list(&self) -> Vec<SessionBrief> {
        let rows: Vec<(String, String, u64, Arc<Hub>, SessionWiring)> = {
            let g = self.lock();
            g.entries
                .iter()
                .map(|(id, e)| {
                    (
                        id.clone(),
                        e.title.clone(),
                        e.created_ms,
                        e.hub.clone(),
                        e.wiring.clone(),
                    )
                })
                .collect()
        };
        rows.into_iter()
            .map(|(session_id, title, created_ms, hub, wiring)| SessionBrief {
                session_id,
                title,
                created_ms,
                status: hub.status(),
                wiring,
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The next command for the worker, and which session it belongs to. Blocks.
    ///
    /// A ring whose command has already been drained — by a session's own steering
    /// source, during a turn — is skipped rather than returned as an empty wake.
    /// That loop is bounded by the number of rings, not by time: nothing here
    /// spins.
    pub fn next_command(&self) -> Option<(String, QueuedCommand)> {
        loop {
            let id = self.bell.next()?;
            let Some(hub) = self.get(&id) else { continue };
            if let Some(cmd) = hub.try_command() {
                self.set_default(&id);
                return Some((id, cmd));
            }
        }
    }

    /// Close every session and the bell. Every head wakes with `Bye`, every worker
    /// falls out of `next_command`.
    pub fn close(&self) {
        let hubs: Vec<Arc<Hub>> = self.lock().entries.iter().map(|(_, e)| e.hub.clone()).collect();
        for h in hubs {
            h.close();
        }
        self.bell.close();
    }

    pub fn is_closed(&self) -> bool {
        self.bell.is_closed()
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::CommandKind;
    use crate::protocol::Caps;
    use crate::testing;

    fn reg() -> Arc<Registry> {
        Registry::new()
    }

    #[test]
    fn two_sessions_do_not_share_a_log_a_view_or_a_queue() {
        // The property that makes the ledger and the memfd region safe one layer
        // down: nothing in this file can hand two sessions the same `Hub`, and a
        // `Harness` is opened per `Hub`.
        let r = reg();
        let a = r.create("s-a", "", SessionWiring::default()).unwrap();
        let b = r.create("s-b", "", SessionWiring::default()).unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        a.publish(testing::turn_started("t1"));
        a.publish(testing::delta("t1", "only in a"));
        assert_eq!(b.head_seq(), 0, "b saw a's events");
        assert_eq!(b.snapshot().turn, None);
        assert_eq!(a.snapshot().turn.unwrap().text, "only in a");
    }

    #[test]
    fn creating_the_same_id_twice_is_refused_rather_than_returning_the_first() {
        let r = reg();
        r.create("s-a", "", SessionWiring::default()).unwrap();
        let again = r.create("s-a", "", SessionWiring::default());
        assert!(
            matches!(&again, Err(CreateError::Exists(id)) if id == "s-a"),
            "a second create must not hand back the first session"
        );
    }

    #[test]
    fn an_unknown_session_is_none_rather_than_the_default_one() {
        // A typo that seats you in somebody else's session is worse than a typo
        // that fails.
        let r = reg();
        r.create("s-a", "", SessionWiring::default()).unwrap();
        assert!(r.resolve("s-typo").is_none());
        assert!(r.resolve("").is_some(), "an empty id is still the default");
    }

    #[test]
    fn one_worker_is_woken_by_whichever_session_was_prompted() {
        let r = reg();
        let a = r.create("s-a", "", SessionWiring::default()).unwrap();
        let b = r.create("s-b", "", SessionWiring::default()).unwrap();
        let ha = a.attach("tui", "alice", Caps::default(), 0);
        let hb = b.attach("tui", "bob", Caps::default(), 0);

        b.submit(
            &hb.head_id,
            "c1",
            0,
            CommandKind::Prompt {
                text: "for b".into(),
            },
        );
        a.submit(
            &ha.head_id,
            "c2",
            0,
            CommandKind::Prompt {
                text: "for a".into(),
            },
        );

        // Arrival order across sessions, which is the promise §13.2 already makes
        // for two heads on one session.
        let (id1, c1) = r.next_command().unwrap();
        assert_eq!(id1, "s-b");
        assert_eq!(c1.kind, CommandKind::Prompt { text: "for b".into() });
        let (id2, c2) = r.next_command().unwrap();
        assert_eq!(id2, "s-a");
        assert_eq!(c2.kind, CommandKind::Prompt { text: "for a".into() });
    }

    #[test]
    fn a_ring_whose_command_was_already_drained_does_not_return_an_empty_wake() {
        // What happens for real: a turn is running, `HubSteering` drains the queue
        // at a step boundary, and the ring is still outstanding.
        let r = reg();
        let a = r.create("s-a", "", SessionWiring::default()).unwrap();
        let h = a.attach("tui", "alice", Caps::default(), 0);
        a.submit(
            &h.head_id,
            "c1",
            0,
            CommandKind::Interrupt {
                reason: "stop".into(),
            },
        );
        assert!(a.try_command().is_some(), "the steering source took it");
        r.close();
        // Closed and drained: `None`, and not a `(session, command)` invented to
        // satisfy the outstanding ring.
        assert!(r.next_command().is_none());
    }

    #[test]
    fn closing_wakes_a_worker_that_is_blocked_on_the_bell() {
        let r = reg();
        r.create("s-a", "", SessionWiring::default()).unwrap();
        let r2 = r.clone();
        let t = std::thread::spawn(move || r2.next_command());
        std::thread::sleep(std::time::Duration::from_millis(30));
        r.close();
        assert!(t.join().unwrap().is_none());
    }

    #[test]
    fn a_list_says_which_session_is_busy_and_which_is_idle() {
        let r = reg();
        let a = r.create("s-a", "the cache question", SessionWiring::default()).unwrap();
        r.create("s-b", "", SessionWiring::default()).unwrap();
        a.publish(testing::turn_started("t1"));
        let rows = r.list();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "the cache question");
        assert!(rows[0].status.running);
        assert!(!rows[1].status.running);
        // …and it stops being busy when the turn ends, rather than staying busy
        // because only `TurnFinished` was ever taught to clear it.
        a.publish(crate::SessionEvent::TurnFailed {
            turn_id: "t1".into(),
            error: "the model called tools 12 times without answering".into(),
            partial_kept: false,
        });
        assert!(!r.list()[0].status.running);
    }

    #[test]
    fn the_default_session_follows_whoever_was_last_used() {
        let r = reg();
        let a = r.create("s-a", "", SessionWiring::default()).unwrap();
        let b = r.create("s-b", "", SessionWiring::default()).unwrap();
        assert_eq!(r.default_id(), "s-a");
        let hb = b.attach("tui", "bob", Caps::default(), 0);
        b.submit(&hb.head_id, "c1", 0, CommandKind::Prompt { text: "x".into() });
        r.next_command().unwrap();
        assert_eq!(r.default_id(), "s-b");
        let _ = a;
    }
}
