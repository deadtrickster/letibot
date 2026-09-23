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
    /// Sessions that exist and have not been opened yet. A **separate** queue, not a
    /// marker in `pending`: `pending` is arrival order across sessions and that is a
    /// promise §13.2 makes, so putting a create in it made a session's first command
    /// jump ahead of a command submitted before it. Measured by
    /// `one_worker_is_woken_by_whichever_session_was_prompted`, which is exactly what
    /// that test is for.
    opens: VecDeque<String>,
    /// Sessions something **inside the daemon** woke, with no command behind it.
    /// A third queue for the same reason `opens` is a second one: `pending` is
    /// arrival order across heads and that ordering is a promise, while a wake has
    /// no head and no `client_request_id` to be fair to.
    wakes: VecDeque<String>,
    closed: bool,
}

/// What a wake was about.
enum Ring {
    Open(String),
    Command(String),
    Woken(String),
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

    /// **Say that something inside the daemon woke this session**, with no command
    /// behind it.
    ///
    /// `TODO.md` T24's monitors are the caller: *"a monitor is a condition watched
    /// **between** turns that wakes the loop when it fires"*. Until this existed the
    /// only way into the worker was a head's command, so a fired monitor could be
    /// **polled** — `job_list` shows it, with why — and never **woke** anything.
    /// A ring with no command was swallowed by [`Registry::next_work`], which skips
    /// a session whose queue is empty, so ringing [`Bell::ring`] would have been a
    /// no-op that read like a wake.
    ///
    /// It rings no timer and starts no thread. The caller is already blocked in a
    /// `Condvar` on the thing it is watching.
    pub fn ring_wake(&self, session_id: &str) {
        {
            let mut g = self.lock();
            g.wakes.push_back(session_id.to_string());
        }
        self.cv.notify_all();
    }

    /// Say that `session_id` exists and nothing has opened it yet.
    pub fn ring_open(&self, session_id: &str) {
        {
            let mut g = self.lock();
            g.opens.push_back(session_id.to_string());
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

    /// Block until there is either a session to open or a session to serve.
    ///
    /// **Opens first.** A session that is not open yet is one whose resume has not
    /// run, and its chain check, its dialect refusal and its republished transcript
    /// all live in that open — running a command in it first would put all three
    /// inside somebody's prompt. The wait is bounded by a store read, not by a
    /// generation.
    fn next_any(&self) -> Option<Ring> {
        let mut g = self.lock();
        loop {
            if let Some(id) = g.opens.pop_front() {
                return Some(Ring::Open(id));
            }
            if let Some(id) = g.pending.pop_front() {
                return Some(Ring::Command(id));
            }
            // **Last**, and deliberately. A wake has nobody waiting on it; a head
            // that pressed enter does. Draining wakes first would let a chatty
            // monitor put itself in front of the operator.
            if let Some(id) = g.wakes.pop_front() {
                return Some(Ring::Woken(id));
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
    /// A human name. Empty until it has one; a head shows a short id then.
    ///
    /// A title is set **once** and then only on purpose — by `--title`, by `/rename`,
    /// or by the daemon naming an unnamed session from the message that opened it.
    /// What must not happen is the original objection: a row that renames itself as
    /// the conversation goes on is a row you cannot learn.
    pub title: String,
    /// Unix millis when the daemon created it.
    pub created_ms: u64,
    /// Everything the log knows, cut in one lock.
    ///
    /// All zeroes for a session that is only in the store: it has no hub, so there is
    /// no log to cut. `live` is what tells the two apart — reading "0 rows" off a
    /// stored session and concluding it is empty is exactly the mistake this pair of
    /// fields exists to prevent, which is why `stored_items` is here beside it.
    pub status: SessionStatus,
    /// What this session is talking to (`crates/ui/DESIGN.md` §4.4).
    pub wiring: SessionWiring,
    /// Whether this daemon holds a hub for it. `false` means it is on disk and has to
    /// be resumed ([`crate::protocol::ClientFrame::ResumeSession`]) before it can be
    /// switched to.
    pub live: bool,
    /// Transcript rows the **store** holds. Zero for a live session in a daemon with
    /// no store, which is a real state and not a missing measurement.
    pub stored_items: u32,
    /// The session that spawned this one as a subagent, or `None` for a top-level
    /// session. A head draws a subagent tree from this without reaching the store.
    pub parent_session_id: Option<String>,
    /// The last turn's prompt tokens, from the session's own row, or `None` before
    /// a turn has finished (or on a row that predates the column). A head that
    /// attaches after a daemon restart shows the context from this: the snapshot's
    /// turn state is ephemeral and a rebuilt view has none.
    pub context_tokens: Option<u64>,
    /// The last turn's cached tokens, for the cache %. `None` with `context_tokens`.
    pub context_cached: Option<u64>,
}

/// What a session is attached to. The daemon's own command line, which is the only
/// place these four facts exist — and, before this, the only place, full stop: a
/// head could name the model (and only during a turn) and could never name the
/// dialect, the endpoint or the workspace at all.
///
/// Empty strings for a daemon whose owner supplied none, never a plausible
/// default: a guessed endpoint on a screen is a guess somebody later quotes.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionWiring {
    pub model: String,
    pub dialect: String,
    pub endpoint: String,
    pub workspace: String,
}

/// The last eight characters of a session id, with a leading ellipsis.
///
/// Ids are minted `format!("s-{}", now_ns())`, so two made on the same afternoon
/// share thirteen leading characters and differ only at the end. Shortening from the
/// **left** is therefore the one direction that keeps them distinguishable; a
/// left-anchored truncation would render every session that day as the same string.
///
/// One function, here, because the head's header, the picker and the daemon's own
/// listing all want it and three spellings of "shorten an id" produce three different
/// strings for one session — which is the opposite of what an identifier is for.
pub fn short_id(id: &str) -> String {
    let n = id.chars().count();
    if n <= 10 {
        return id.to_string();
    }
    let tail: String = id.chars().skip(n - 8).collect();
    format!("…{tail}")
}

/// A session that exists somewhere the registry cannot see — on disk.
///
/// The registry holds hubs, and a hub is a live thing. This is the shape of a
/// session that is not live yet, so that one list can hold both and a picker does not
/// have to be told there is a second place to look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBrief {
    pub session_id: String,
    pub title: String,
    pub items: u32,
    pub last_activity_ms: u64,
    pub wiring: SessionWiring,
    /// The session that spawned this one as a subagent, or `None` for a top-level
    /// session.
    pub parent_session_id: Option<String>,
    /// The last turn's prompt tokens, from the session's row. `None` before a turn
    /// has finished (or on a row that predates the column).
    pub context_tokens: Option<u64>,
    /// The last turn's cached tokens, for the cache %. `None` with `context_tokens`.
    pub context_cached: Option<u64>,
}

/// Where a registry can find sessions it is not already holding.
///
/// A **trait and not a `Store`**: `letibot-sessionlog` does not depend on
/// `letibot-tokencore` and should not start — the log and the token ledger are two
/// strands that meet in `harnessd`, and a dependency here would make every head
/// binary link a SQLite. So the daemon passes an implementation in, and a registry
/// with no source behaves exactly as it did before, which is what every existing test
/// asserts.
pub trait SessionSource: Send + Sync {
    /// Every session on disk, newest activity first.
    fn list(&self) -> Vec<StoredBrief>;
    /// One session, or `None` if nothing anywhere has heard of it.
    fn lookup(&self, session_id: &str) -> Option<StoredBrief> {
        self.list().into_iter().find(|s| s.session_id == session_id)
    }
    /// Make the name durable. An empty title clears it.
    ///
    /// The default refuses, and refusing is the point: a registry with no source has
    /// nowhere to put a name, and a rename that only lived in memory would come back
    /// as the old name the next time the daemon started — with nothing having said
    /// so. A head is told, and can say so.
    fn set_title(&self, _session_id: &str, _title: &str) -> Result<(), String> {
        Err("this daemon has no store, so a name would not survive it".into())
    }
    /// One session's todo list, for a head's bootstrap read.
    ///
    /// The default is empty rather than an error, and that is the honest answer
    /// twice over: a source with no store has no list, and a session that never
    /// wrote one has none — from the outside these are the same state, which is
    /// exactly what the pane should show. Live changes do not come through here;
    /// they arrive as [`crate::SessionEvent::TodosUpdated`].
    fn todos(&self, _session_id: &str) -> Vec<crate::event::TodoEntry> {
        Vec::new()
    }
}

struct Entry {
    jobs: Vec<crate::protocol::JobEntry>,
    hub: Arc<Hub>,
    title: String,
    created_ms: u64,
    wiring: SessionWiring,
    /// The session that spawned this one as a subagent, or `None` for a top-level
    /// session. Live, alongside the stored copy, so a head's tree is drawn from the
    /// registry rather than from the store.
    parent_session_id: Option<String>,
    /// The settings this session runs under, as its harness last published
    /// them. Kept here because the server thread answers `Settings` and cannot
    /// reach the harness; the harness pushes on open and on every runtime
    /// change, so a head reads what is running and not what was flagged.
    settings: Vec<crate::protocol::SettingRow>,
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
/// **Where a registry can read one row's body without holding it** (R19.2b).
///
/// The other half of [`SessionSource`], and a trait for the same reason: the daemon's
/// *view* is bounded (2,000 rows, 8 MB of bodies), so an ordinal it has trimmed is a row
/// the ledger has and the view does not — and `ClientFrame::FetchRow` exists precisely to
/// reach it. Before this, a trimmed ordinal answered `null`, and a head could not tell
/// "the daemon does not hold it" from "there is no such row".
///
/// It answers a **session ordinal**, not a transcript and a seq, because that is what the
/// frame carries and because resolving a session to its *current* transcript is the
/// store's own business — after a fork, ordinal 3 is a row of the new base and not of the
/// history it summarised.
///
/// A registry with no source behaves exactly as it did before, which is what every
/// existing test asserts.
pub trait RowSource: Send + Sync {
    /// The body of `row` (a session ordinal) for `session_id`, or `None` when nothing
    /// holds it. **Not an empty string** for a row that exists and is empty — the two must
    /// not look alike, which is the rule `RowFetched`'s own doc states for the wire.
    fn row_body(&self, session_id: &str, row: usize) -> Option<String>;
}

/// **Where the oracle's exchange for one decision can be read** — R11's locator.
///
/// A second trait and not a method on [`RowSource`], for the reason that one is a trait: the
/// two read different things out of different tables and a source that can answer one is not
/// thereby able to answer the other. A registry with no source behaves exactly as it did
/// before, which is what every existing test asserts.
///
/// **`None` means "not recorded", and the caller must not render it as empty.** The store holds
/// `NULL` on every row written before R11 kept `oracle_reply`, and an oracle that never answered
/// has no reply either — see [`crate::protocol::Diagnostic`]'s own doc.
pub trait DiagnosticSource: Send + Sync {
    fn diagnostic(
        &self,
        request_id: &str,
        kind: crate::protocol::DiagnosticKind,
    ) -> Option<String>;
}

pub struct Registry {
    inner: Mutex<Inner>,
    bell: Arc<Bell>,
    /// Set once at startup by the daemon. `None` in every head and every test that
    /// predates resume, and the registry then lists only what it holds.
    source: Mutex<Option<Arc<dyn SessionSource>>>,
    /// Set once at startup by the daemon, beside [`Registry::source`]. See [`RowSource`].
    rows: Mutex<Option<Arc<dyn RowSource>>>,
    /// Set once at startup, beside [`Registry::rows`]. See [`DiagnosticSource`].
    diagnostics: Mutex<Option<Arc<dyn DiagnosticSource>>>,
}

/// What the worker was woken for.
///
/// A second variant rather than a second loop: [`Registry::next_command`] blocks, so
/// a session created by a head while the worker is asleep would sit unopened until
/// somebody prompted into it — and "unopened" is where a resume's chain check, its
/// dialect refusal and its republished transcript all live. A head that resumed a
/// session and then attached to it would find an empty screen and no error, which is
/// the shape of failure this whole strand exists to remove.
pub enum Work {
    /// Open this session before anything else happens in it.
    Open(String),
    /// Run this command against this session.
    Command(String, QueuedCommand),
    /// **Something inside the daemon woke this session** and no head asked for it.
    ///
    /// T24's monitors are the caller. It is a third variant rather than a synthetic
    /// `Command` because a command has an issuing head, an identity and a
    /// `client_request_id`, and inventing three of those for a firing would put a
    /// head's name on something no head did. The worker decides what a wake means
    /// for that session; the registry only says which session woke.
    Woken(String),
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
            source: Mutex::new(None),
            rows: Mutex::new(None),
            diagnostics: Mutex::new(None),
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
                    jobs: Vec::new(),
                    hub,
                    title: String::new(),
                    created_ms: now_ms(),
                    wiring: SessionWiring::default(),
                    parent_session_id: None,
                    settings: Vec::new(),
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
        self.create_under(session_id, title, wiring, None)
    }

    /// As [`Registry::create`], recording the session that spawned this one. `parent`
    /// is `None` for a top-level session and `Some(id)` for a subagent, so the tree
    /// is a fact of the registry and not an id convention.
    pub fn create_under(
        &self,
        session_id: impl Into<String>,
        title: impl Into<String>,
        wiring: SessionWiring,
        parent: Option<String>,
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
            id.clone(),
            Entry {
                jobs: Vec::new(),
                hub: hub.clone(),
                title: title.into(),
                created_ms: now_ms(),
                wiring,
                parent_session_id: parent,
                settings: Vec::new(),
            },
        ));
        drop(g);
        // Wake the worker so it opens this session *now* rather than on the first
        // prompt into it. On the bell's own `opens` queue, so a caller that predates
        // `next_work` — every existing test, and `Daemon::run`'s old shape — sees the
        // command stream it always did, unpolluted by creations.
        self.bell.ring_open(&id);
        Ok(hub)
    }

    /// A hub built exactly as [`Registry::create_under`] builds one — this
    /// registry's bounds, this registry's bell — and **not registered**.
    ///
    /// For a session whose owner opens it itself, off the daemon's worker: a
    /// subagent. Registering first and opening second is how a head could switch
    /// into a hub nothing would ever write to (measured 2026-09-16: an empty
    /// subagent conversation while the child was still copying its workspace),
    /// and how the worker came to build a second harness for a session the runner
    /// was already opening. Open it, then [`Registry::adopt`] it.
    pub fn new_hub(&self, session_id: impl Into<String>) -> Arc<Hub> {
        let g = self.lock();
        let hub = Hub::with_bounds(session_id.into(), g.log_bounds, g.view_bounds);
        hub.set_bell(self.bell.clone());
        hub
    }

    /// Register a hub that is **already open** — see [`Registry::new_hub`]. The
    /// same refusals as `create_under`, and no `ring_open`: the owner opened it,
    /// and a worker told to open it again would build a second harness on it.
    pub fn adopt(
        &self,
        hub: Arc<Hub>,
        title: impl Into<String>,
        wiring: SessionWiring,
        parent: Option<String>,
    ) -> Result<(), CreateError> {
        let id = hub.session_id().to_string();
        let mut g = self.lock();
        if self.bell.is_closed() {
            return Err(CreateError::Closed);
        }
        if g.entries.iter().any(|(k, _)| k == &id) {
            return Err(CreateError::Exists(id));
        }
        if g.default_id.is_empty() {
            g.default_id = id.clone();
        }
        g.entries.push((
            id,
            Entry {
                jobs: Vec::new(),
                hub,
                title: title.into(),
                created_ms: now_ms(),
                wiring,
                parent_session_id: parent,
                settings: Vec::new(),
            },
        ));
        Ok(())
    }

    /// Where to find sessions this registry is not holding. See [`SessionSource`].
    pub fn set_source(&self, source: Arc<dyn SessionSource>) {
        *self.source.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    /// **Where to find one row's body that this registry's own view no longer holds.**
    ///
    /// See [`RowSource`]. Set once at startup by the daemon, exactly as
    /// [`Registry::set_source`] is, and `None` in every head and every test that predates
    /// it — in which case a `FetchRow` for a trimmed ordinal answers `None`, as it always
    /// did.
    pub fn set_row_source(&self, rows: Arc<dyn RowSource>) {
        *self.rows.lock().unwrap_or_else(|e| e.into_inner()) = Some(rows);
    }

    /// Where the oracle's exchange can be read. See [`DiagnosticSource`].
    pub fn set_diagnostic_source(&self, diagnostics: Arc<dyn DiagnosticSource>) {
        *self
            .diagnostics
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(diagnostics);
    }

    /// One half of one decision's exchange, or `None` when there is no source or no record.
    pub fn diagnostic(
        &self,
        request_id: &str,
        kind: crate::protocol::DiagnosticKind,
    ) -> Option<String> {
        // Cloned out before the call, so the lock is not held across a SQLite read — the same
        // rule `row_body_from_store` keeps one screen up.
        let source = self
            .diagnostics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        source.diagnostic(request_id, kind)
    }

    /// **The body of a session ordinal, from the store** — or `None` when there is no
    /// source, or it does not hold that row.
    ///
    /// The second tier of `FetchRow`, and deliberately a *separate* entry point from
    /// `SessionView::row_body_at`: the view is the fast path and stays one lock and a
    /// `Vec` index, while this is the slow one and must never run for a row the view
    /// already has. The caller asks the view first, and only then here.
    pub fn row_body_from_store(&self, session_id: &str, row: usize) -> Option<String> {
        // The Arc is cloned out before the call, so the lock is not held across a SQLite
        // read: a head paging a trimmed row must not be able to block a listing.
        let source = self.rows.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        source.row_body(session_id, row)
    }

    fn source(&self) -> Option<Arc<dyn SessionSource>> {
        self.source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// A session that is on disk and not in this daemon, or `None`.
    ///
    /// `None` for a session that **is** live, deliberately: the caller asking this is
    /// asking "do I need to bring it in", and a live session is the one case where
    /// the answer is no.
    pub fn resumable(&self, session_id: &str) -> Option<StoredBrief> {
        if self.get(session_id).is_some() {
            return None;
        }
        self.source()?.lookup(session_id)
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
    /// Publish the settings a session runs under. See `Entry::settings`.
    pub fn set_settings(&self, session_id: &str, rows: Vec<crate::protocol::SettingRow>) {
        let mut g = self.lock();
        if let Some((_, e)) = g.entries.iter_mut().find(|(k, _)| k == session_id) {
            e.settings = rows;
        }
    }

    /// **The jobs a session's harness last published.**
    ///
    /// A mailbox rather than a live read, for the same reason `settings` is one:
    /// the process table belongs to the harness and the harness belongs to the
    /// daemon's own thread, while this is answered on a head's connection. The
    /// harness refills it whenever the table changes, so the snapshot a pane
    /// opens on is the daemon's answer and not the head's reconstruction.
    pub fn set_jobs(&self, session_id: &str, jobs: Vec<crate::protocol::JobEntry>) {
        let mut g = self.lock();
        if let Some((_, e)) = g.entries.iter_mut().find(|(k, _)| k == session_id) {
            e.jobs = jobs;
        }
    }

    pub fn jobs(&self, session_id: &str) -> Vec<crate::protocol::JobEntry> {
        let g = self.lock();
        g.entries
            .iter()
            .find(|(k, _)| k == session_id)
            .map(|(_, e)| e.jobs.clone())
            .unwrap_or_default()
    }

    /// What a session's harness last published. Empty for a session whose
    /// harness has not opened, which the head says rather than hides.
    pub fn settings(&self, session_id: &str) -> Vec<crate::protocol::SettingRow> {
        let g = self.lock();
        g.entries
            .iter()
            .find(|(k, _)| k == session_id)
            .map(|(_, e)| e.settings.clone())
            .unwrap_or_default()
    }

    pub fn set_title(&self, session_id: &str, title: impl Into<String>) {
        let mut g = self.lock();
        if let Some((_, e)) = g.entries.iter_mut().find(|(k, _)| k == session_id) {
            e.title = title.into();
        }
    }

    /// Name a session **and make it durable**, through the source.
    ///
    /// Both halves or neither: the in-memory title is what a head is about to be
    /// sent and the stored one is what it will see next week, and a rename that did
    /// one of them is a rename that appears to work and silently is not.
    pub fn rename(&self, session_id: &str, title: &str) -> Result<(), String> {
        let Some(src) = self.source() else {
            return Err("this daemon has no store, so a name would not survive it".into());
        };
        src.set_title(session_id, title)?;
        self.set_title(session_id, title);
        Ok(())
    }

    /// Every session, in creation order, each cut under its own hub's lock.
    ///
    /// The hubs are cloned out from under the registry lock first, so listing a
    /// session cannot be blocked by a publish into a different one — and so the
    /// registry lock is never held while a hub lock is taken, which is the whole of
    /// this file's lock ordering.
    /// One session's todo list, from the source. Empty when there is no source
    /// or no list — see [`SessionSource::todos`].
    pub fn todos(&self, session_id: &str) -> Vec<crate::event::TodoEntry> {
        self.source()
            .map(|s| s.todos(session_id))
            .unwrap_or_default()
    }

    pub fn list(&self) -> Vec<SessionBrief> {
        let rows: Vec<(String, String, u64, Arc<Hub>, SessionWiring, Option<String>)> = {
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
                        e.parent_session_id.clone(),
                    )
                })
                .collect()
        };
        let stored: Vec<StoredBrief> = self.source().map(|s| s.list()).unwrap_or_default();
        let mut out: Vec<SessionBrief> = rows
            .into_iter()
            .map(|(session_id, title, created_ms, hub, wiring, parent)| {
                let on_disk = stored.iter().find(|d| d.session_id == session_id);
                SessionBrief {
                    // A daemon started with `--title` and a store that already names
                    // the session must not disagree with itself. The store wins for a
                    // session it knows, because that is the name the operator set and
                    // the one every other list shows.
                    title: on_disk
                        .map(|d| d.title.clone())
                        .filter(|t| !t.is_empty())
                        .unwrap_or(title),
                    stored_items: on_disk.map(|d| d.items).unwrap_or(0),
                    session_id,
                    created_ms,
                    status: hub.status(),
                    wiring,
                    live: true,
                    // The registry's own record wins over the store's for a live
                    // session; the store's is what remains after a restart.
                    parent_session_id: parent
                        .or_else(|| on_disk.and_then(|d| d.parent_session_id.clone())),
                    // The store's row is the only place the number survives a
                    // restart, so it is the source for a live session too: the
                    // hub's view has the same number in its turn state while the
                    // daemon holds it, and a head that reads the brief gets the
                    // same fact either way.
                    context_tokens: on_disk.and_then(|d| d.context_tokens),
                    context_cached: on_disk.and_then(|d| d.context_cached),
                }
            })
            .collect();
        // Then everything on disk this daemon is not holding. Appended rather than
        // interleaved by time: the live ones are the ones a switch reaches in one
        // keystroke, and a picker that mixes them puts a two-step action next to a
        // one-step action with nothing to say which is which. `live` says it per row;
        // the ordering says it again.
        for d in stored {
            if out.iter().any(|b| b.session_id == d.session_id) {
                continue;
            }
            out.push(SessionBrief {
                session_id: d.session_id,
                title: d.title,
                created_ms: d.last_activity_ms,
                status: SessionStatus::default(),
                wiring: d.wiring,
                live: false,
                stored_items: d.items,
                parent_session_id: d.parent_session_id,
                context_tokens: d.context_tokens,
                context_cached: d.context_cached,
            });
        }
        out
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
    /// The next thing for the worker to do. Blocks. Supersedes
    /// [`Registry::next_command`], which is kept for the callers that have no
    /// sessions to open.
    ///
    /// An open is drained **before** the command that woke the same session, because
    /// the alternative is opening a session as a side effect of running a turn in it
    /// — and a resume that failed its chain check would then fail inside a prompt the
    /// operator is watching, instead of before it.
    pub fn next_work(&self) -> Option<Work> {
        loop {
            match self.bell.next_any()? {
                Ring::Open(id) => return Some(Work::Open(id)),
                // A wake for a session this registry does not hold is dropped, the
                // same way a command for one is: the session is gone and there is
                // nothing to wake.
                Ring::Woken(id) => {
                    if self.get(&id).is_some() {
                        return Some(Work::Woken(id));
                    }
                }
                Ring::Command(id) => {
                    let Some(hub) = self.get(&id) else { continue };
                    if let Some(cmd) = hub.try_command() {
                        self.set_default(&id);
                        return Some(Work::Command(id, cmd));
                    }
                }
            }
        }
    }

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
        let hubs: Vec<Arc<Hub>> = self
            .lock()
            .entries
            .iter()
            .map(|(_, e)| e.hub.clone())
            .collect();
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

    /// A subagent's hub is built first and registered only once its owner has
    /// opened it — so nothing can switch into it early, and the worker is never
    /// told to open a session somebody else is opening.
    #[test]
    fn a_new_hub_is_not_listed_until_adopted_and_adopting_does_not_ring_the_worker() {
        let r = reg();
        r.create("s-parent", "", SessionWiring::default()).unwrap();
        // The parent's own creation rang the worker; drain that.
        assert!(matches!(r.next_work(), Some(Work::Open(id)) if id == "s-parent"));

        let hub = r.new_hub("s-parent-sub-1");
        assert!(
            r.resolve("s-parent-sub-1").is_none(),
            "listed before it was open"
        );
        assert_eq!(r.list().len(), 1);

        r.adopt(
            hub.clone(),
            "find the bug",
            SessionWiring::default(),
            Some("s-parent".into()),
        )
        .unwrap();
        let brief = r.brief("s-parent-sub-1").expect("adopted");
        assert_eq!(brief.parent_session_id.as_deref(), Some("s-parent"));
        assert!(Arc::ptr_eq(&r.resolve("s-parent-sub-1").unwrap(), &hub));
        let again = r.adopt(hub.clone(), "", SessionWiring::default(), None);
        assert_eq!(again, Err(CreateError::Exists("s-parent-sub-1".into())));
        // The bell blocks with nothing pending, so "did not ring" is proven by
        // closing it: a drained, closed bell answers None at once, and a ring
        // for the child would come out first.
        r.bell().close();
        assert!(
            r.next_work().is_none(),
            "adopting rang the worker to open it again"
        );
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
        assert_eq!(
            c1.kind,
            CommandKind::Prompt {
                text: "for b".into()
            }
        );
        let (id2, c2) = r.next_command().unwrap();
        assert_eq!(id2, "s-a");
        assert_eq!(
            c2.kind,
            CommandKind::Prompt {
                text: "for a".into()
            }
        );
    }

    #[test]
    fn a_take_back_drops_the_issuing_heads_queued_prompts_only() {
        let r = reg();
        let a = r.create("s-a", "", SessionWiring::default()).unwrap();
        let ha = a.attach("tui", "alice", Caps::default(), 0);
        let hb = a.attach("tui", "bob", Caps::default(), 0);

        a.submit(
            &ha.head_id,
            "c1",
            0,
            CommandKind::Prompt {
                text: "alice one".into(),
            },
        );
        a.submit(
            &hb.head_id,
            "c2",
            0,
            CommandKind::Prompt { text: "bob".into() },
        );
        a.submit(
            &ha.head_id,
            "c3",
            0,
            CommandKind::Prompt {
                text: "alice two".into(),
            },
        );
        a.submit(&ha.head_id, "c4", 0, CommandKind::WithdrawPrompts);

        assert!(
            a.try_withdraw_command(),
            "the take-back was found and acted on"
        );
        // Alice's prompts are gone — both of them — and bob's is not alice's to
        // take back.
        let left: Vec<_> = (0..2).filter_map(|_| a.try_steering_command()).collect();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].kind, CommandKind::Prompt { text: "bob".into() });
        // And the take-back itself is consumed: a second one finds nothing.
        assert!(!a.try_withdraw_command());
    }

    #[test]
    fn a_ring_whose_command_was_already_drained_does_not_return_an_empty_wake() {
        // What happens for real: a turn is running, `HubSteering` drains the queue
        // at a step boundary, and the ring is still outstanding.
        let r = reg();
        let a = r.create("s-a", "", SessionWiring::default()).unwrap();
        assert!(
            matches!(r.next_work(), Some(Work::Open(_))),
            "create rings an open"
        );
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
        let a = r
            .create("s-a", "the cache question", SessionWiring::default())
            .unwrap();
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

    /// **A wake with no command behind it reaches the worker.**
    ///
    /// This is what `TODO.md` T24 left open: `Monitors::wait_for_any` is the "wakes
    /// the loop when it fires" half and nothing called it, because there was no way
    /// in. `Bell::ring` was not it — `next_work` skips a session whose command queue
    /// is empty, so ringing it for a firing would have been a no-op that read like a
    /// wake, which is worse than the poll it was replacing.
    #[test]
    fn a_wake_with_no_command_behind_it_still_reaches_the_worker() {
        let r = reg();
        r.create("s-a", "", SessionWiring::default()).unwrap();
        // `create` rings an open, and an open is drained first (a session
        // whose resume has not run must not serve a command). Take it so the
        // assertion below is about the wake and not about the create.
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
        r.bell().ring_wake("s-a");
        match r.next_work() {
            Some(Work::Woken(id)) => assert_eq!(id, "s-a"),
            _ => panic!("a wake must be work"),
        }
    }

    /// **A head that pressed enter is waiting; a monitor is not.**
    ///
    /// Wakes are drained last, so a chatty watcher cannot put itself in front of a
    /// person. Asserted rather than left to the queue order in `next_any`, because
    /// that order is a policy and a reader of the three `pop_front`s cannot tell a
    /// policy from an accident.
    #[test]
    fn a_wake_is_served_after_every_queued_command() {
        let r = reg();
        let a = r.create("s-a", "", SessionWiring::default()).unwrap();
        assert!(
            matches!(r.next_work(), Some(Work::Open(_))),
            "create rings an open"
        );
        let h = a.attach("tui", "dead", Caps::default(), 0);
        r.bell().ring_wake("s-a");
        a.submit(
            &h.head_id,
            "c1",
            0,
            CommandKind::Prompt {
                text: "hello".into(),
            },
        );

        match r.next_work() {
            Some(Work::Command(id, cmd)) => {
                assert_eq!(id, "s-a");
                assert!(matches!(cmd.kind, CommandKind::Prompt { .. }));
            }
            _ => panic!("the operator's prompt goes first even though the wake rang first"),
        }
        assert!(matches!(r.next_work(), Some(Work::Woken(_))));
    }

    /// A wake for a session this registry does not hold is dropped, the same way a
    /// command for one is. The session is gone; there is nothing to wake.
    #[test]
    fn a_wake_for_an_unknown_session_does_not_stall_the_worker() {
        let r = reg();
        r.create("s-a", "", SessionWiring::default()).unwrap();
        // `create` rings an open, and an open is drained first (a session
        // whose resume has not run must not serve a command). Take it so the
        // assertion below is about the wake and not about the create.
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
        r.bell().ring_wake("s-gone");
        r.bell().ring_wake("s-a");
        match r.next_work() {
            Some(Work::Woken(id)) => assert_eq!(id, "s-a"),
            _ => panic!("the unknown one must be skipped, not returned"),
        }
    }

    #[test]
    fn the_default_session_follows_whoever_was_last_used() {
        let r = reg();
        let a = r.create("s-a", "", SessionWiring::default()).unwrap();
        let b = r.create("s-b", "", SessionWiring::default()).unwrap();
        assert_eq!(r.default_id(), "s-a");
        let hb = b.attach("tui", "bob", Caps::default(), 0);
        b.submit(
            &hb.head_id,
            "c1",
            0,
            CommandKind::Prompt { text: "x".into() },
        );
        r.next_command().unwrap();
        assert_eq!(r.default_id(), "s-b");
        let _ = a;
    }
}
