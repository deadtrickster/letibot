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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::hub::{CommandKind, DAEMON_SUBMITTER, Hub, QueuedCommand, SessionStatus};
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
    /// Wakes served in a row while the caller's clock was already due. See
    /// [`WAKES_AHEAD_OF_A_DUE_CLOCK`].
    wakes_while_due: usize,
    closed: bool,
}

/// **How many wakes a due clock waits behind** before the worker is answered `Idle` (R68.5).
///
/// Not zero: a clock that jumped every wake would make the worker's idle pass — a store read for
/// reviews, a sweep of every session — run between each pair of wakes a tree's settlements
/// produce together. Not unbounded: that was the starvation — under a storm of wakes the idle arm
/// never ran at all. Wakes are coalesced per session ([`Bell::ring_wake`]), so this is eight
/// SESSIONS' worth of machine traffic, after which the clock goes first once.
pub const WAKES_AHEAD_OF_A_DUE_CLOCK: usize = 8;

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
    ///
    /// **Coalesced per session** (R68.5): a wake already waiting for this session is the same
    /// wake. Its whole content is *look at what you own* — one `Harness::wake` drains every
    /// settlement queued for the session — so a second ring before the first is served would be
    /// a second pass that finds nothing, and under a storm (children settling, monitors firing,
    /// relays) the queue grew by one entry per ring and the worker served each one, each of them
    /// a pass over the store (`Harness::wake`'s reads). Arrival order across sessions is kept:
    /// the waiting wake keeps its place.
    pub fn ring_wake(&self, session_id: &str) {
        {
            let mut g = self.lock();
            if g.wakes.iter().any(|w| w == session_id) {
                return;
            }
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
    /// The same, giving up at DEADLINE.
    ///
    /// **A third answer, because `None` already means something.** The daemon's single
    /// worker blocks here until there is work, and the one thing it could not do was
    /// *nothing, for now* — it had no way to be told that time had passed. A caller that
    /// needs to act on the clock (the idle todo-nag is the first) has to be woken by the
    /// clock, and a timeout that returned `None` would be indistinguishable from the
    /// registry closing, which ends the daemon.
    fn next_any_until(&self, deadline: Option<Instant>) -> RingWait {
        let mut g = self.lock();
        loop {
            if let Some(id) = g.opens.pop_front() {
                return RingWait::Ring(Ring::Open(id));
            }
            if let Some(id) = g.pending.pop_front() {
                return RingWait::Ring(Ring::Command(id));
            }
            // **A due clock waits behind at most [`WAKES_AHEAD_OF_A_DUE_CLOCK`] wakes** (R68.5).
            // Before, the deadline was looked at only when all three queues were empty, so a
            // steady stream of wakes — children settling, monitors firing — meant `Idle` never
            // came: the sweep, the plan-check, the review pass and the park clock all waited
            // behind machine traffic nobody was waiting on. A head's command still outranks the
            // clock (a person is waiting on it); wakes keep their turn up to the bound, and the
            // one after the clock is served on the very next call.
            let due = deadline.is_some_and(|d| Instant::now() >= d);
            if due && !g.wakes.is_empty() && g.wakes_while_due >= WAKES_AHEAD_OF_A_DUE_CLOCK {
                g.wakes_while_due = 0;
                return RingWait::Idle;
            }
            // **Last**, and deliberately. A wake has nobody waiting on it; a head
            // that pressed enter does. Draining wakes first would let a chatty
            // monitor put itself in front of the operator.
            if let Some(id) = g.wakes.pop_front() {
                g.wakes_while_due = if due { g.wakes_while_due + 1 } else { 0 };
                return RingWait::Ring(Ring::Woken(id));
            }
            if g.closed {
                return RingWait::Closed;
            }
            // **The queue is empty, so this is where the clock is allowed in.** Every
            // category above is drained first: work a person is waiting on never waits
            // behind a timer.
            let timed_out = match deadline {
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return RingWait::Idle;
                    }
                    let (guard, out) = self
                        .cv
                        .wait_timeout(g, d - now)
                        .unwrap_or_else(|e| e.into_inner());
                    g = guard;
                    out.timed_out()
                }
                None => {
                    g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                    false
                }
            };
            if timed_out {
                return RingWait::Idle;
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BellInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What the worker found: work, or the clock.
///
/// `Idle` is not an error and not a shutdown — it is *the deadline passed and there was
/// nothing to do*, which is the state the daemon acts on. See
/// [`Registry::next_work_until`].
pub enum WorkOrIdle {
    Work(Work),
    Idle,
    Closed,
}

/// The bell's answer when it is allowed to give up on the clock.
///
/// Three answers and not two: `Ring` is work, `Closed` ends the daemon, and `Idle` is
/// *the deadline passed with nothing to do* — which is what a caller that acts on the
/// clock needs and what `Option` could not say.
enum RingWait {
    Ring(Ring),
    Idle,
    Closed,
}

/// What the daemon knows about a session that the log does not: its title, when it
/// was made, and what it is talking to.
///
/// Sent to a head in `Hello` and in `Sessions`, which is what a session picker is
/// drawn from. `#[serde(default)]` nowhere but `stored_end`: a field that is absent and a
/// field that is empty must not look the same, the same rule the rest of the protocol
/// keeps — and `stored_end` is the one field where they are the same fact.
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
    /// **How the stored conversation's last turn ended**, read off its last row, or `None`
    /// for a session with no rows (or none this daemon could read).
    ///
    /// The one fact about a settled child that survives the daemon: the `Subagent` event that
    /// said `done` lives on the parent's in-memory log, and after a restart a head rebuilding
    /// the subagents pane from this list had nothing to say about any child but *state
    /// unknown* — the operator's pane, 2026-10-09, was a wall of `[?]`.
    ///
    /// **The one `#[serde(default)]` on this struct, and why it may be.** The rule above is that
    /// absent and empty must not look the same; here they ARE the same fact — `None` is *the
    /// daemon has not told this head how the conversation ended*, and a daemon too old to say
    /// has not told it. So it is an added defaulted field, which the protocol's own notes say
    /// needs no version bump: a head and a daemon on either side of this change still attach.
    #[serde(default)]
    pub stored_end: Option<StoredEnd>,
    /// **The operator has paused this session's idle plan-check**, and until when — `None`
    /// when they have not. Read off the session's own row, so a stored session the daemon is
    /// not holding says it as well as a live one: a silent board on a session somebody paused
    /// must not be read, from the picker, as a session whose clock is broken.
    ///
    /// Defaulted for `stored_end`'s reason: `None` is also what a daemon too old to know the
    /// verb has to say, so the field is added without a protocol bump.
    #[serde(default)]
    pub nags_paused: Option<NagPause>,
    /// **How many questions this session's board is waiting on the OPERATOR for** — rows with an
    /// open `operator` edge (`TodoNeed::Operator`). The picker marks a session that has any, so a
    /// person who is being waited on can see it from the list and not only from inside the
    /// session. Read off the store's board, live or not: the row is the question's one home.
    ///
    /// Defaulted for `nags_paused`'s reason: `0` is also what a daemon too old to know the edge
    /// has to say.
    #[serde(default)]
    pub waits_on_operator: usize,
    /// **A subagent's name — its slug** (`fix-login`), or empty for a session nobody named: every
    /// root, and a child spawned by a daemon from before children had names.
    ///
    /// The operator: *"lets do slugs for subagents too"*. A child was drawn by its session id or
    /// by its 256-character brief, and a job by the name the agent gave it; this is the child's
    /// name, the same one `task_start` cut its worktree's branch `agent/<slug>` from, so a head
    /// can name it in the header, the picker and the subagents pane the way the jobs pane names
    /// a job. Defaulted for `waits_on_operator`'s reason: empty is also what a daemon too old to
    /// know the name has to say, so it is added without a protocol bump.
    #[serde(default)]
    pub slug: String,
}

/// **How long the operator has paused a session's idle plan-check** (`/nag pause`).
///
/// Two shapes and no third: a pause the operator ends by hand, and one that ends by itself at a
/// wall-clock time. Wall-clock (unix ms) and not a duration, because it is what survives a
/// restart and what a head draws: *paused until 14:30* is the sentence, and a duration would
/// need the moment it was measured from to mean anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NagPause {
    /// Until `/nag resume`.
    UntilResumed,
    /// Until this unix millisecond, or `/nag resume` before it.
    Until { ms: u64 },
}

impl NagPause {
    /// **The store's spelling** — `0` is until resumed, anything else the unix ms it ends at
    /// (`StoredSession::nags_paused_until` in `letibot-tokencore`, which this crate does not
    /// depend on, so the mapping lives on this side).
    pub fn from_stored(v: i64) -> NagPause {
        if v <= 0 {
            NagPause::UntilResumed
        } else {
            NagPause::Until { ms: v as u64 }
        }
    }

    pub fn to_stored(self) -> i64 {
        match self {
            NagPause::UntilResumed => 0,
            NagPause::Until { ms } => ms as i64,
        }
    }

    /// **Is the pause over as of `now_ms`?** Never, for one the operator ends by hand.
    pub fn over_at(self, now_ms: u64) -> bool {
        matches!(self, NagPause::Until { ms } if ms <= now_ms)
    }
}

/// **How a stored conversation's last turn ended**, as its last row tells it.
///
/// Two words and no third, because the row can only say these: an answer with no call after
/// it is a turn that finished, and anything else — a call with no result, a result with no
/// answer, a prompt nobody answered — is a turn that was still going when the rows stopped.
/// Whether that turn is still going NOW is not this fact's to say: a live session's
/// `status.running` is the measurement of now, and a child parked on its own job is alive
/// with a result as its last row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredEnd {
    /// The last row is an answer: its first line, as the pane's subtitle.
    Answered { first_line: String },
    /// The last row is anything else: the turn was cut, or is still in flight.
    MidTurn,
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

/// A fresh [`Registry::instance`]: pid, wall clock in nanoseconds, and a per-process counter.
fn mint_instance() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "d-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
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
    /// **How the stored conversation's last turn ended**, read off its last row, or `None`
    /// for a session with no rows (or none this daemon could read).
    ///
    /// The one fact about a settled child that survives the daemon: the `Subagent` event that
    /// said `done` lives on the parent's in-memory log, and after a restart a head rebuilding
    /// the subagents pane from this list had nothing to say about any child but *state
    /// unknown* — the operator's pane, 2026-10-09, was a wall of `[?]`.
    pub stored_end: Option<StoredEnd>,
    /// The operator's pause of the idle plan-check, off the session's row — see
    /// [`SessionBrief::nags_paused`]. A pause whose end has already passed is `None` here: it is
    /// over, whether or not a daemon has yet been there to say so.
    pub nags_paused: Option<NagPause>,
    /// Open questions the session's board waits on the operator for — see
    /// [`SessionBrief::waits_on_operator`].
    pub waits_on_operator: usize,
    /// A subagent's name, from its record — see [`SessionBrief::slug`]. Empty for a session
    /// nobody named.
    pub slug: String,
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
    /// **The review queue and the merge queue, whole, for a head's bootstrap read** (R61).
    ///
    /// The default — a source with no store — is a snapshot that SAYS it has no store, never an
    /// empty queue that looks like one nobody used: R61 defect 12 was a failed read drawn as an
    /// empty queue.
    fn queues(&self) -> crate::event::QueueSnapshot {
        crate::event::QueueSnapshot {
            unreadable: Some("this daemon has no session store, so it has no queues".into()),
            ..Default::default()
        }
    }
    /// **Which repository `session_id` is a session of** (R92 item 5) — the daemon's own
    /// identity rule, so a head scopes its queue pane and edge to its project. `None` is a source
    /// that cannot say (no store, or a session it has never heard of).
    fn queue_here(&self, _session_id: &str) -> Option<crate::event::QueueHere> {
        None
    }
}

struct Entry {
    jobs: Vec<crate::protocol::JobEntry>,
    /// The session's monitors, published beside its jobs (`Registry::set_jobs`).
    monitors: Vec<crate::protocol::MonitorEntry>,
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
    /// **What this session's harness said about its frozen prefix**, or `None` when it had
    /// nothing to say. The sentence, not a flag: the harness is the only thing that can
    /// write it, because the two facts it compares — what the session was seated with and
    /// what this daemon would compose now — are only ever in one place at once, and naming
    /// what moved (which tool, which schema) needs both in hand.
    ///
    /// **Stored and said LATER, which is the whole reason it is a field.** A warning
    /// published while the harness opens is filed by a head that attaches afterwards as
    /// [`crate::view::Placed`]`::Before` — listed by `/notes`, counted by `/status`, and
    /// **not drawn**, because it is older than the conversation on the screen. So the
    /// comparison happens where the facts are and the SENTENCE happens at ATTACH
    /// (`server::seat_in`), while the head is watching.
    ///
    /// A mailbox rather than a live read, for `settings`' own reason: this is answered on
    /// a head's connection thread and the harness belongs to the worker.
    stale_prefix: Option<String>,
}

struct Inner {
    /// Every session, in the order it arrived — the order `list` and `broadcast` walk.
    /// **Pushed only through [`Inner::push`] and taken only through [`Inner::take`]**, which
    /// keep [`Inner::index`] in step.
    entries: Vec<(String, Entry)>,
    /// **A session id's position in `entries`** — R67 item 27. Every lookup used to be
    /// `entries.iter().find(..)` under the registry lock: a linear scan on every `get`, from
    /// every hub's publish path and every head's frame, over a table that only grew (a
    /// subagent's entry stayed for the daemon's life). [`Inner::take`] — R68's
    /// `Registry::remove` — is the one removal, and it rewrites the positions it shifts.
    index: HashMap<String, usize>,
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
    fn diagnostic(&self, request_id: &str, kind: crate::protocol::DiagnosticKind)
    -> Option<String>;
}

/// **Who proposes `!` completions when the history has none** — the smart half of the
/// `!` completion the operator asked for: *"i want smart ! when a model suggest
/// completions."*
///
/// A trait and not a method on the registry, for the reason `SessionSource` is: this
/// crate holds no HTTP client and no endpoint, and the model call is the daemon's. The
/// daemon passes an implementation in (the local model, bounded), and a registry with no
/// suggester answers `SuggestShell` with an empty list — which the head reads as *no
/// suggestion*, exactly as it reads a model that had none. That is the safe direction:
/// a daemon without a local model offers nothing rather than reaching for a metered
/// provider, because a suggestion must not cost money per keystroke.
///
/// The implementation builds the prompt from the session's own rows (see
/// [`crate::suggest`]) and asks the model; the prompt and the defensive parse of the
/// reply live in this crate so they are testable without standing up inference.
pub trait ShellSuggester: Send + Sync {
    /// The candidate lines for `prefix` in this session, `!` first, or none.
    ///
    /// `hub` is the session's own log — the conversation the prompt is built from — and
    /// `workspace` is where the session runs, so a suggestion fits the tree it would be
    /// run in. **Bounded by the implementer**: a small output cap and a timeout, and a
    /// suggestion that does not arrive is nothing, never a reason to wait.
    fn suggest(&self, hub: &Hub, workspace: &str, prefix: &str) -> Vec<String>;
}

/// **Who answers a plugin pane's questions** — C4's `ClientFrame::PaneOpen` and
/// `ClientFrame::PaneAction`, and the panes list a head needs before it can open one.
///
/// # Why it is a trait on the registry, and why it is read on the connection's thread
///
/// The same reason [`TerminalDriver`] is: **a pane must answer while a turn is running**. The
/// two frames are a screen's questions and not the session's acts, so they are answered here
/// rather than queued behind a turn the worker may be inside for minutes — a dashboard that
/// waited for the model would be a dashboard that is blank exactly when somebody is watching
/// work happen.
///
/// This crate has no wasm engine and no `letibot-plugins` dependency, deliberately: the layers,
/// the looks and the shadow gate are the daemon's (W4 of `docs/plugins-slice1.md`), and a head
/// binary that answered a pane itself would be a second opinion about what a plugin may do. So
/// the daemon installs an implementation per session, and a registry with none answers both
/// frames with silence — which a head draws as the view it already holds, and which `/pane
/// NAME` reports as a pane nobody has.
///
/// # Why per session and not once
///
/// The layers are discovered per session (`docs/plugins-plan.md` §2.1): the session's own dir,
/// its project's `.letibot/plugins`, and the user's. Two sessions on one daemon therefore have
/// two stacks, and a pane id belongs to one of them.
pub trait PaneDriver: Send + Sync {
    /// **A head attached: say which panes this session has.**
    ///
    /// Called after the `Hello` and the backlog are written, so a head that has just arrived
    /// receives the panes as events on the same log as everything else, in order. This is the
    /// half that makes a panes list possible: a head learns a pane exists from a
    /// [`crate::SessionEvent::PluginPane`], and the view a call answers is also its title.
    ///
    /// **It must not block.** It is called on the connection's thread, like every other method
    /// here.
    fn attach(&self, session_id: &str, hub: &Arc<Hub>);

    /// **Run this pane now**, because a head opened it: the answer is a
    /// [`crate::SessionEvent::PluginPane`] on the session's log, never a reply frame. A pane
    /// this session does not have publishes nothing.
    fn open(&self, session_id: &str, hub: &Arc<Hub>, pane: &str);

    /// **One key of one stop, back to the pane that drew it** — the action word its `event`
    /// named, or `look` to re-run the look. Answered like [`Self::open`].
    fn action(&self, session_id: &str, hub: &Arc<Hub>, pane: &str, item: &str, action: &str);
}

/// **Who owns a pane's pty** — `!term`, the program that owns the screen.
///
/// A trait and not a method on the registry, for the reason [`ShellSuggester`] is one: this
/// crate has no `libc`, no pty and no business holding a child process, while the daemon has
/// all three. The daemon passes an implementation in (`letibot-harnessd`'s `term` module,
/// built on `letibot_tools::exec::term`), and **a registry with no driver answers `TermOpen`
/// with a pane that is over before it began** — the same safe direction the suggester takes:
/// a daemon that cannot run a pane says so rather than pretending to.
///
/// # Why the hub comes in rather than going out
///
/// A pane's bytes travel *up*, and they travel from a **reader thread** the driver owns — not
/// from the connection thread that handled the frame. So `open` is handed the hub, and the
/// driver keeps it: `Hub::push_frame` is the one door for a frame that is not the record, and
/// it is the same door every other head-visible thing goes through. A driver holding a
/// `Sender<ServerFrame>` of its own would be a second ordering on one socket.
///
/// # Why every method is keyed by the session and not by the head
///
/// **Panes are the session's — several since protocol 44, each with a number** (R60 item 2):
/// a screen program is a process in
/// the session's workspace, in the session's cgroup, and a second head attached to the same
/// session draws the same rectangle. The frames are fanned out like events (see
/// `Hub::push_frame`) and a head with no pane drops them, which is the honest reading of *a
/// pane this head did not open*.
///
/// **Nothing here may block.** `input` is called on the connection's reader thread, so a
/// driver that waited for the program to read its keystrokes would stop this head's acks and
/// its frames for as long as the program was busy — and a `nano` saving a file is a program
/// that is not reading keys.
pub trait TerminalDriver: Send + Sync {
    /// **Start the program in a pty this daemon owns.** `command` is the shell line the
    /// operator typed after the verb, `cols`/`rows` the pane's rectangle, and `hub` the
    /// session whose heads will receive the bytes.
    ///
    /// `Err` is the sentence the operator reads: it becomes [`crate::protocol::ServerFrame::TermEnded`]'s
    /// `reason`, so it must be something a person can act on.
    ///
    /// **`Ok` is the new pane's number** (protocol 44): a session may hold several, and the
    /// driver announces the number on the hub as `TermAttached { opened: true }` before the
    /// program's first byte, so every frame after it can say which pane it is about.
    fn open(
        &self,
        session_id: &str,
        hub: &Arc<Hub>,
        command: &str,
        cols: usize,
        rows: usize,
    ) -> Result<u32, String>;
    /// **Give this head the pane the session already has** — a bare `!term`.
    ///
    /// The operator's defect, and the reason the screen has to be the daemon's: a person who
    /// closes the pane, or switches session, has no way back to a program that is still
    /// running, and the code admitted it (*"a head that switches back does not find its pane
    /// again, it finds the transcript"*). So this is the other half of `open`, and it is the
    /// driver's because **the driver is what holds the screen**: `open` hands the program's
    /// bytes up as they arrive, and a driver that keeps them can replay them to whoever asks.
    ///
    /// `cols`/`rows` are **the attaching head's rectangle**, and they are not a formality: a
    /// head that switched sessions has a different one, so the pty is resized to what it is
    /// being drawn at. That also raises `SIGWINCH` for a program that redraws on a resize —
    /// which is a redraw, not the mechanism, and `letibot_harnessd`'s `term` module says so.
    ///
    /// `Err` is a sentence for a session with no pane — the same `TermEnded` a refusal uses,
    /// because a head that asked for a pane and did not get one is in the same place either
    /// way: close the rectangle and say why.
    ///
    /// `pane` is the number the operator named (`!term 2`), or `None` for *the session's most
    /// recent live pane*. `Ok` is the number attached to.
    fn attach(
        &self,
        session_id: &str,
        pane: Option<u32>,
        hub: &Arc<Hub>,
        cols: usize,
        rows: usize,
    ) -> Result<u32, String>;
    /// **What this session's pane is running, or nothing** — the answer to
    /// [`crate::protocol::ClientFrame::TermStatus`].
    ///
    /// `Some(command)` is a **live** pane: the same string [`Self::attach`] puts on
    /// [`crate::protocol::ServerFrame::TermAttached`], because it is the same fact asked for
    /// rather than volunteered. `None` is *no live pane*, which is not an error — a session
    /// nobody has run `!term` in has none, and a program that has exited stops counting the
    /// moment its reader thread reports (the slot is freed on the next `open`).
    ///
    /// # Why a read and not a notification
    ///
    /// A head that has **detached** (`ctrl-\`, which ends nothing — see
    /// `PROTOCOL_VERSION`'s 34 section) or switched session still has to know the program is
    /// there, and the operator's rule is that this is **not a transcript row**: a detach is
    /// not an event and a row for it would be a disclosure about a moment that did not
    /// happen. So the head asks, draws the fact while it is true, and stops drawing it when
    /// it stops being true — which needs no history at all.
    ///
    /// **It must not block.** Called on the connection's reader thread, like every other
    /// method here: it takes the driver's own pane table and answers.
    ///
    /// **Every live pane since 44**, by number; empty is what `None` was.
    fn status(&self, session_id: &str) -> Vec<crate::protocol::PaneBrief>;
    /// The operator's keys, verbatim, for one pane. Quietly ignored when there is no such pane.
    fn input(&self, session_id: &str, pane: u32, bytes: &[u8]) -> Result<(), String>;
    /// One pane's rectangle moved.
    fn resize(&self, session_id: &str, pane: u32, cols: usize, rows: usize) -> Result<(), String>;
    /// **End the pane.** Ends the pane's scope, which kills the program and everything it
    /// started. Quiet when there is no pane: *"stop"* is not a request that can be wrong.
    ///
    /// **This is the deliberate act, and it is no longer the way out.** `ctrl-\` used to send
    /// the frame that lands here, so leaving `nano` killed it — the operator's *"but i dont
    /// want it to exit"*. Leaving is now a detach (the head hides the rectangle and sends
    /// nothing); ending is `!term close`, after the head has asked the operator to confirm it.
    /// The confirmation is the head's, deliberately: a daemon that asked its own question
    /// would be a second card with a second set of keys.
    fn close(&self, session_id: &str, pane: u32) -> Result<(), String>;
}

/// **Who can write to a running command's stdin** — the answer to `PromptAnswer` and to
/// `!send`.
///
/// # Why this is a trait on the registry and not a field somewhere
///
/// The same reason [`TerminalDriver`] is: **the caller is the server's reader thread**, and
/// the state it needs — the handle the daemon holds on the operator's own run's input — lives in the
/// session's exec host, which the daemon worker owns. The worker is **blocked inside the very
/// command that is asking**, so it cannot be asked; the server has to reach the state
/// without it.
///
/// # Why it is per SESSION
///
/// One registry serves every session, and a pipe belongs to the run of one session. So the
/// driver is looked up by session id ([`Registry::prompt`]) rather than held once: two
/// sessions on one daemon each have their own operator run, and a `!send` in one must never
/// write into the other's command. The daemon installs one per session at open
/// ([`Registry::set_prompt`]).
///
/// # What is deliberately not here
///
/// **No secret.** This trait has no method that takes a password and no method that
/// returns one; a password travels on [`crate::protocol::ClientFrame::Secret`] to a
/// waiting `askpass` connection and nowhere else. That is not a rule this trait follows —
/// it is a shape it does not have, which is the version of the rule a later edit cannot
/// quietly break.
pub trait PromptDriver: Send + Sync {
    /// **Write one line to this session's own running command.**
    ///
    /// `req` is `Some(req_id)` when the line answers a card the daemon raised
    /// ([`crate::protocol::ClientFrame::PromptAnswer`]) and `None` for the operator's own
    /// send ([`crate::protocol::ClientFrame::SendLine`]). The difference is the whole of
    /// the stale-card rule: with a `req_id` the driver checks that **that** request is the
    /// one still open, so a card raised for `apt` cannot answer a `sleep` that started
    /// after `apt` died; with `None` the caller has said *whatever is running*, which is
    /// exactly what the manual verb means.
    ///
    /// **Returns the request this settled, if a card was up**, so the caller can publish
    /// [`crate::SessionEvent::PromptSettled`] with the request id it names — the driver
    /// does not publish, because it is the daemon's to say who answered.
    ///
    /// `Err` is a sentence a person reads: *nothing of yours is running*, *that card is
    /// not open any more*, *the command is no longer reading its stdin*. The caller turns
    /// it into a `Warning` (`prompt_late`, `nothing_to_send_to`) rather than a refusal,
    /// because a late answer must not look like a refused one.
    ///
    /// **Nothing here may block.** This is called on the connection's reader thread, so a
    /// driver that waited for the program to read would stop this head's acks and its
    /// events. The write is one line into the run's own input — its terminal on the ordinary
    /// path, a pipe on a box where no pty opens; `letibot_tools::exec::Stdin::send_line`
    /// carries what that costs and why it is bounded.
    fn send(
        &self,
        session_id: &str,
        req: Option<&str>,
        line: &str,
    ) -> Result<Option<String>, String>;
}

/// **What a session must do when this daemon is asked to stop.**
///
/// A boxed closure rather than a trait, and that is the layering: the thing being ended is
/// a **process the daemon owns** — an operator's `!` command, a model's `bash` call — and
/// this crate holds no process host, no cgroup and no exec substrate. The daemon passes one
/// in per session at open, exactly as it passes a [`PromptDriver`] or a [`ShellSuggester`].
///
/// # Why the daemon needs telling at all
///
/// `close` wakes the worker out of `next_command` — but a worker **inside a run** has not
/// reached `next_command` and will not until the run ends. The daemon has one worker, so a
/// stop that arrives mid-command waits for that command, and its own deadline is what ends
/// it: two minutes at the `bash` default. MEASURED on a live daemon, 2026-10-06: `Stop` acked,
/// `Bye` at 519 µs, and the process still in `/proc` for the whole of the run.
///
/// So a stop ends the runs. It is the same operation `job_kill` performs — the run's cgroup
/// — done by the thread that took the `Stop` rather than by the worker, and the run's own row
/// is where it is said: it settles as `Killed` carrying the reason, and the tool renders
/// that on the row the session keeps.
///
/// Returns one sentence per run it ended, for the announcement, so *it ended nothing* and
/// *it ended nothing because nothing was running* are different answers.
pub type RunEnder = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// **The boundary a session's `!term` pane runs inside** — given the pane's working directory
/// and environment, the argv that goes in front of its shell, or the sentence saying why the
/// pane cannot run (R60 item 3).
///
/// A closure for the [`RunEnder`] reason: the boundary is the session's **process host**'s —
/// its confinement, read as it stands at the moment the pane opens, grants included — and this
/// crate holds no process host. The session's harness installs one at open
/// ([`Registry::bound_panes`]) beside its run ender, and the terminal driver asks for it by
/// session id ([`Registry::pane_boundary`]) when a pane opens.
///
/// **No closure is not *no boundary* by accident**: a session with no process host — a VM
/// placement, a read-only seat — has no command boundary to give, because it runs no commands;
/// its pane runs as panes always have, and the driver says so where it opens one. An
/// unconfined session's closure answers an empty argv, which is its commands' own answer.
pub type PaneBoundary =
    Arc<dyn Fn(&std::path::Path, &[(String, String)]) -> Result<Vec<String>, String> + Send + Sync>;

/// **Why a stop interrupts the turns it holds** — the reason recorded on the turn it ends.
///
/// One string, named, because it lands in three places a reader can reach — the running
/// turn's own abort row (`steering_urgent`, `AbortCause::Steering`), the `interrupt_idle`
/// warning a stop that arrived between turns publishes, and the sentence
/// `crate::server`'s `Stop` frame prints to every head — and three copies of it would be
/// three answers to *why did my turn end*.
/// **What `prefix_current` says** — the sentence a head is told when a session that was
/// speaking a stale prompt is moved onto the one this daemon seats (see
/// [`Registry::set_stale_prefix`]). A head that draws the reseat card takes the CODE as the
/// signal and the sentence is for the one that does not.
pub const PREFIX_CURRENT: &str = "this session now speaks the prompt this daemon seats; the \
                                  stale-prompt notice no longer applies";

pub const STOPPING_TURN_REASON: &str = "the daemon is stopping";

/// **A session's standing notes, as rows — computed where the vocabulary is.**
///
/// A boxed closure rather than a mailbox, and the layering is why: the *form* each note has
/// (verbatim, or an index because it did not fit what was left of the budget) is decided with
/// the session's own token counter, and a counter is a loaded vocabulary. This crate holds
/// none and must not start — it is the log, the protocol and the wire, and every head links
/// it. So the harness installs a closure that does have one
/// ([`Registry::set_notes_source`]), exactly as it installs a [`RunEnder`] for the runs it
/// owns, and the server thread asks it.
///
/// **A read and not a mailbox, and that is the difference from `settings` and `jobs`.** Those
/// two are snapshots of something that changes for its own reasons; the notes are the
/// operator's own files, edited in another window with nothing to tell the daemon about it. A
/// mailbox would answer *what the corpus was when the section was last assembled* — which is
/// a fact about the prompt and not about the corpus — and a note written while the session sat
/// idle would be missing from a pane that claims one row per note. So the corpus is read on
/// the ask, which is the same rule `letibot_harnessd::notes_offer` keeps for the same list.
///
/// **It must not block and it must not fail.** Called on a head's connection thread, so a
/// source that waited for the harness thread would stop this head's acks; the harness's own
/// implementation is `gather` plus one pass of token counting, and a corpus that cannot be
/// read is no rows rather than an error — see `standing_notes::rows`.
pub type NotesSource = Arc<dyn Fn() -> Vec<crate::protocol::NoteEntry> + Send + Sync>;

/// **A live read of one background job's output, answered off the worker.**
///
/// The jobs pane's Enter sends `ReadJobOutput`, and that frame used to go through the command
/// queue like a prompt. The queue has one reader — the session's worker — and that worker is
/// the thread RUNNING the turn, so a read asked while a turn was in flight waited for the turn
/// to end. The operator, with a turn four minutes into `Responding` and two jobs running:
/// *"look at serendb window, try to get into a job - no output"* — the pane sat at `reading…`
/// over a job the list itself said had written 1.8 KB. The listing answered (it is read off
/// the registry, `ListJobs`); the window did not (it was queued). This is the window's half of
/// what `ListJobs` already does.
///
/// **The harness installs it, because only the harness holds the process host** — the job
/// table and its capture rings — and this crate must not learn what a process is. The closure
/// returns the EVENT to publish (`SessionEvent::JobOutput`, or a `job_output_refused`
/// `Warning`), so the shape of the answer stays the harness's own and is the same one the
/// queued path publishes.
///
/// **`None` means "not from here"**, and the server then queues the read as it always did: a
/// job the host no longer holds is answered from the store's record of how it ended, which is
/// the worker's to read. That case is a finished job, so waiting for the turn costs nothing a
/// running job's reader is owed.
///
/// **It must not block**: it runs on a head's connection thread. The host's reads are a lock
/// on the job table and a copy out of a ring — the same work `ListJobs`' refill does.
pub type JobOutputSource = Arc<dyn Fn(&str, u64) -> Option<crate::SessionEvent> + Send + Sync>;

pub struct Registry {
    inner: Mutex<Inner>,
    bell: Arc<Bell>,
    /// **One per session**, installed by the daemon at open beside [`Registry::prompts`].
    /// See [`RunEnder`] for why a stop has to reach the runs and why the daemon is the half
    /// that owns them.
    runs: Mutex<std::collections::HashMap<String, RunEnder>>,
    /// **One per session that has a process host**, installed by its harness at open beside
    /// [`Registry::runs`]. See [`PaneBoundary`].
    panes: Mutex<std::collections::HashMap<String, PaneBoundary>>,
    /// **One per session**, installed by the session's own harness at open — the half that
    /// owns the token counter — and removed with the session ([`Registry::remove`]). See
    /// [`NotesSource`].
    notes: Mutex<std::collections::HashMap<String, NotesSource>>,
    /// **One per session that has a process host**, installed by its harness at open beside
    /// [`Registry::notes`] and removed with the session. See [`JobOutputSource`].
    job_output: Mutex<std::collections::HashMap<String, JobOutputSource>>,
    /// **`close` is not idempotent where the enders are concerned.** It is called by the
    /// `Stop` frame, by `catch_signals` and again by `ServerHandle::shutdown`, and ending
    /// every run three times would be three reap records for one decision. The turns are
    /// ended under this same guard: one stop, one interrupt per session holding one.
    ended_runs: std::sync::atomic::AtomicBool,
    /// Set once at startup by the daemon. `None` in every head and every test that
    /// predates resume, and the registry then lists only what it holds.
    source: Mutex<Option<Arc<dyn SessionSource>>>,
    /// Set once at startup by the daemon, beside [`Registry::source`]. See [`RowSource`].
    rows: Mutex<Option<Arc<dyn RowSource>>>,
    /// Set once at startup, beside [`Registry::rows`]. See [`DiagnosticSource`].
    diagnostics: Mutex<Option<Arc<dyn DiagnosticSource>>>,
    /// Set once at startup, beside [`Registry::diagnostics`]. See [`ShellSuggester`].
    /// `None` in every head and every test that predates the smart `!`, and a
    /// `SuggestShell` then answers with an empty list.
    suggester: Mutex<Option<Arc<dyn ShellSuggester>>>,
    /// Set once at startup by the daemon, beside [`Registry::suggester`]. See
    /// [`TerminalDriver`]. `None` in every head and every test that predates `!term`, and a
    /// `TermOpen` then answers with a pane that is over and the reason it is.
    terminal: Mutex<Option<Arc<dyn TerminalDriver>>>,
    /// **One per session**, installed by the session's own harness at open and removed with
    /// the session ([`Registry::remove`]). See [`PromptDriver`] for why it is keyed by session
    /// and not held once.
    prompts: Mutex<std::collections::HashMap<String, Arc<dyn PromptDriver>>>,
    /// **One per session that has layers**, installed by the daemon at open and removed with
    /// the session. See [`PaneDriver`] for why a plugin pane's questions are answered here
    /// rather than queued behind a turn.
    pane_drivers: Mutex<std::collections::HashMap<String, Arc<dyn PaneDriver>>>,
    /// **Which daemon this is** — minted once per registry, and a daemon has exactly one. See
    /// [`Registry::instance`].
    instance: String,
    /// When this registry — this daemon — came up, in ms since the epoch.
    started_ms: u64,
    /// See [`Registry::is_stopping`].
    stopping: std::sync::atomic::AtomicBool,
    /// **The names of the subagents this daemon spawned**, by session id — see
    /// [`Registry::set_slug`].
    slugs: Mutex<std::collections::HashMap<String, String>>,
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

impl Inner {
    /// The entry for `id`, by the index.
    fn entry(&self, id: &str) -> Option<&Entry> {
        self.index.get(id).map(|&i| &self.entries[i].1)
    }

    fn entry_mut(&mut self, id: &str) -> Option<&mut Entry> {
        match self.index.get(id) {
            Some(&i) => Some(&mut self.entries[i].1),
            None => None,
        }
    }

    fn has(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }

    /// The one way into `entries`, so the index cannot drift from it.
    fn push(&mut self, id: String, entry: Entry) {
        // The writer fills a row's tree root from this (TODO R81): one message per session.
        letibot_metrics::session_parent(&id, entry.parent_session_id.as_deref());
        self.index.insert(id.clone(), self.entries.len());
        self.entries.push((id, entry));
    }

    /// **The one way OUT of `entries`** (R68.3), and the second thing R67 item 27's index said a
    /// removal would have to update. `Vec::remove` rather than `swap_remove`, because `entries`
    /// is arrival order and `list` and `broadcast` walk it in that order — a picker that
    /// reshuffled whenever a subagent finished would move the row under the operator's cursor.
    /// So every position after the removed one shifts down by one, and the index is rewritten
    /// for exactly those. O(sessions) per removal, which is once per session's life.
    fn take(&mut self, id: &str) -> Option<Entry> {
        let at = self.index.remove(id)?;
        let (_, entry) = self.entries.remove(at);
        for (i, (later, _)) in self.entries.iter().enumerate().skip(at) {
            self.index.insert(later.clone(), i);
        }
        if self.default_id == id {
            // The session a bare attach lands on is gone: the most recent one left is the
            // honest successor, and none is the empty id every head already handles.
            self.default_id = self
                .entries
                .last()
                .map(|(id, _)| id.clone())
                .unwrap_or_default();
        }
        Some(entry)
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
                index: HashMap::new(),
                default_id: String::new(),
                log_bounds,
                view_bounds,
            }),
            bell: Bell::new(),
            runs: Mutex::new(std::collections::HashMap::new()),
            panes: Mutex::new(std::collections::HashMap::new()),
            notes: Mutex::new(std::collections::HashMap::new()),
            job_output: Mutex::new(std::collections::HashMap::new()),
            ended_runs: std::sync::atomic::AtomicBool::new(false),
            source: Mutex::new(None),
            rows: Mutex::new(None),
            diagnostics: Mutex::new(None),
            suggester: Mutex::new(None),
            terminal: Mutex::new(None),
            prompts: Mutex::new(std::collections::HashMap::new()),
            pane_drivers: Mutex::new(std::collections::HashMap::new()),
            instance: mint_instance(),
            started_ms: now_ms(),
            stopping: std::sync::atomic::AtomicBool::new(false),
            slugs: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// **A subagent's name, recorded by the runner that spawned it** — what
    /// [`SessionBrief::slug`] carries for a session this daemon holds. Kept beside the entries
    /// rather than in one, because the runner names the child BEFORE its session is opened (a
    /// head that lists the tree while the child is still opening must already see the name),
    /// and a name for an id no entry has is simply unused. An empty slug is no name.
    pub fn set_slug(&self, session_id: &str, slug: &str) {
        let mut g = self.slugs.lock().unwrap_or_else(|e| e.into_inner());
        if slug.trim().is_empty() {
            g.remove(session_id);
        } else {
            g.insert(session_id.to_string(), slug.to_string());
        }
    }

    /// The name [`Registry::set_slug`] recorded for a session, if any.
    pub fn slug(&self, session_id: &str) -> Option<String> {
        self.slugs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }

    /// **This daemon's instance id** — distinct for every registry ever built, across processes
    /// and within one.
    ///
    /// The fact it answers is *was this written by the daemon I am, or by one before me?* — which
    /// is what a parent opening its children's records has to know: a child recorded `running` by
    /// THIS instance is live in this process (or settled here), and one recorded by another
    /// instance was lost when that daemon went down and is the one to bring back (the operator:
    /// *"we should bring subagents back … and restart automatically"*). A pid alone is not it —
    /// pids are reused, and one test process builds many registries — so the pid, the clock and
    /// a counter together.
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// **When this daemon came up** — the end of a previous daemon's down time.
    pub fn started_ms(&self) -> u64 {
        self.started_ms
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
            g.push(
                id,
                Entry {
                    jobs: Vec::new(),
                    monitors: Vec::new(),
                    hub,
                    title: String::new(),
                    created_ms: now_ms(),
                    wiring: SessionWiring::default(),
                    parent_session_id: None,
                    settings: Vec::new(),
                    stale_prefix: None,
                },
            );
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
        if g.has(&id) {
            return Err(CreateError::Exists(id));
        }
        let hub = Hub::with_bounds(id.clone(), g.log_bounds, g.view_bounds);
        hub.set_bell(self.bell.clone());
        if g.default_id.is_empty() {
            g.default_id = id.clone();
        }
        g.push(
            id.clone(),
            Entry {
                jobs: Vec::new(),
                monitors: Vec::new(),
                hub: hub.clone(),
                title: title.into(),
                created_ms: now_ms(),
                wiring,
                parent_session_id: parent,
                settings: Vec::new(),
                stale_prefix: None,
            },
        );
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
        if g.has(&id) {
            return Err(CreateError::Exists(id));
        }
        if g.default_id.is_empty() {
            g.default_id = id.clone();
        }
        g.push(
            id,
            Entry {
                jobs: Vec::new(),
                monitors: Vec::new(),
                hub,
                title: title.into(),
                created_ms: now_ms(),
                wiring,
                parent_session_id: parent,
                settings: Vec::new(),
                stale_prefix: None,
            },
        );
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
        *self.diagnostics.lock().unwrap_or_else(|e| e.into_inner()) = Some(diagnostics);
    }

    /// Who proposes `!` completions when the history has none. See [`ShellSuggester`].
    /// Set once at startup by the daemon, exactly as [`Registry::set_source`] is, and
    /// `None` in every head and every test that predates it — in which case a
    /// `SuggestShell` answers with an empty list, as the head reads *no suggestion*.
    pub fn set_suggester(&self, suggester: Arc<dyn ShellSuggester>) {
        *self.suggester.lock().unwrap_or_else(|e| e.into_inner()) = Some(suggester);
    }

    /// The suggester this registry was given, or `None` when it was not.
    pub fn suggester(&self) -> Option<Arc<dyn ShellSuggester>> {
        self.suggester
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// **Who owns a pane's pty.** See [`TerminalDriver`]. Set once at startup by the daemon,
    /// exactly as [`Registry::set_suggester`] is, and `None` in every head and every test that
    /// predates `!term` — in which case a `TermOpen` answers with a pane that is over and the
    /// sentence saying there is no driver, rather than with silence.
    pub fn set_terminal(&self, driver: Arc<dyn TerminalDriver>) {
        *self.terminal.lock().unwrap_or_else(|e| e.into_inner()) = Some(driver);
    }

    /// The pane's driver, or `None` when this registry was given none.
    pub fn terminal(&self) -> Option<Arc<dyn TerminalDriver>> {
        self.terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// **Install a session's stdin driver.** Called by the session's own harness at open —
    /// it is the half that owns the exec host, so it is the half that has the pipe — and
    /// never removed: a session this registry holds lives for the daemon's life.
    ///
    /// See [`PromptDriver`] for why the key is a session id.
    pub fn set_prompt(&self, session_id: &str, driver: Arc<dyn PromptDriver>) {
        self.prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), driver);
    }

    /// **Install this session's plugin-pane driver.** Called by the daemon at open — it is the
    /// half that discovers the layers — and removed with the session ([`Registry::remove`]).
    pub fn set_pane_driver(&self, session_id: &str, driver: Arc<dyn PaneDriver>) {
        self.pane_drivers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), driver);
    }

    /// This session's plugin-pane driver, or `None` for a session with no layers — a session no
    /// daemon has opened, a test, or a daemon built before this existed. A `PaneOpen` then
    /// answers nothing, which is what `/pane NAME` already knows how to say.
    pub fn pane_driver(&self, session_id: &str) -> Option<Arc<dyn PaneDriver>> {
        self.pane_drivers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }

    /// This session's stdin driver, or `None` for a session that has none — a daemon built
    /// before this existed, a test, or a session whose harness never opened. A `PromptAnswer`
    /// or a `SendLine` then answers with a sentence naming the absence rather than silence,
    /// which is `terminal`'s own rule one verb over.
    pub fn prompt(&self, session_id: &str) -> Option<Arc<dyn PromptDriver>> {
        self.prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
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
        let source = self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
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

    /// Every session this registry holds a hub for, live ones only — the store's are not here.
    pub fn ids(&self) -> Vec<String> {
        self.lock()
            .entries
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn get(&self, session_id: &str) -> Option<Arc<Hub>> {
        self.lock().entry(session_id).map(|e| e.hub.clone())
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
        g.entry(&want).map(|e| e.hub.clone())
    }

    /// Remember that this is the session a bare attach should land on.
    pub fn set_default(&self, session_id: &str) {
        let mut g = self.lock();
        if g.has(session_id) {
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
            .entry(session_id)
            .map(|e| e.wiring.clone())
            .unwrap_or_default()
    }

    /// **The session that spawned this one**, from this registry's own entry — `None` for a
    /// top-level session AND for one this registry does not hold (R68.6).
    ///
    /// The reader `created_ms` and `wiring` already had beside them, and the one that was
    /// missing: every open asked *is this somebody's subagent?* through [`Registry::list`],
    /// which reads the whole store (`Store::list_sessions`, a correlated scan over every session
    /// and its transcript items) under the store mutex every head's picker frame also takes — two
    /// or three full scans per session open, on the worker. The entry has the answer: a child is
    /// adopted or created WITH its parent, and a resumed one is created from its stored row,
    /// which carries the parent.
    pub fn parent_of(&self, session_id: &str) -> Option<String> {
        self.lock()
            .entry(session_id)
            .and_then(|e| e.parent_session_id.clone())
    }

    /// **The ids of the sessions this registry holds, and nothing else** — no store read (the
    /// store's listing is [`Registry::list`]'s, and it costs a scan). For the merge queue, which
    /// rings the root sessions of a repository whose target went red (R76 item 1).
    pub fn held_ids(&self) -> Vec<String> {
        self.lock()
            .entries
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// **Whether this registry holds the session at all** — the other half of `parent_of`'s
    /// `None`, for a caller that must tell *top-level* from *not here*.
    pub fn holds(&self, session_id: &str) -> bool {
        self.lock().has(session_id)
    }

    /// **The session's title as this registry holds it** — the name it was created or adopted
    /// with, or the last one [`Registry::set_title`] put there. `None` for a session it does not
    /// hold. One entry lookup, for `parent_of`'s reason: the picker's title is
    /// [`Registry::list`]'s business (where the store's name wins), and a caller that needs one
    /// session's name must not pay for every session's row to get it.
    pub fn title_of(&self, session_id: &str) -> Option<String> {
        self.lock().entry(session_id).map(|e| e.title.clone())
    }

    /// **Forget a session: its entry, its run ender, its prompt driver and its notes source —
    /// and close its hub** (R68.3).
    ///
    /// Nothing removed a session before this. `entries` only grew, so every subagent's hub — its
    /// log and its view — lived for the daemon's life; `runs` kept each session's ender, a
    /// closure holding that session's `Arc<HostProcesses>` (its whole job table, up to 8 MiB of
    /// capture a job); `prompts` and `notes` kept closures into its harness. R66 measured the
    /// sum: ~12 MB and two threads per finished subagent, never released, across a tree that
    /// started 519 of them.
    ///
    /// **The hub is closed here, not left to the caller**, because a removed hub that stayed open
    /// is the one state worse than either: a head attached to it would wait on a log nothing will
    /// ever write to, with no `Bye`. Closing wakes every head with [`crate::hub::Delivery::Closed`]
    /// and the session's own reader with `OwnWork::Closed`.
    ///
    /// **The run ender is dropped, not called.** It ends the session's runs, which is a stop's
    /// act ([`Registry::close`]); the caller that removes a session has already decided what
    /// happens to its runs — the harness that owns them closes its backend — and calling the
    /// ender here would reap a second time what that close reaped once.
    ///
    /// Returns the hub, so a caller can still read its final state; `None` when the registry did
    /// not hold the session, which is not an error — a close that raced another close.
    pub fn remove(&self, session_id: &str) -> Option<Arc<Hub>> {
        let entry = self.lock().take(session_id)?;
        // Each map under its own lock, never under the registry's — the ordering `close` keeps.
        let ender = self
            .runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        let prompt = self
            .prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        let notes = self
            .notes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        let boundary = self
            .panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        let job_output = self
            .job_output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        let pane_driver = self
            .pane_drivers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        // Dropped outside every lock: a closure's captures can run arbitrary `Drop`s (a
        // `HostProcesses` ends its deadline thread), and none of them may run under a registry
        // mutex another thread is waiting on.
        drop((ender, prompt, notes, boundary, job_output, pane_driver));
        entry.hub.close();
        Some(entry.hub)
    }

    /// **When this daemon opened this session**, or `None` for one it does not hold.
    ///
    /// One entry lookup under the lock, and a reader of its own rather than `brief` for that
    /// reason: `brief` builds a whole `SessionBrief` — which lists the session SOURCE, a store
    /// query on this box — and the caller here is the idle plan-check, which asks about one child
    /// per arming and needs one number. The name is the column's own (`SessionBrief::created_ms`),
    /// so the two readings of one fact cannot drift apart.
    pub fn created_ms(&self, session_id: &str) -> Option<u64> {
        self.lock().entry(session_id).map(|e| e.created_ms)
    }

    /// Name a session. Empty is allowed and means "no name"; a head then shows the
    /// id, which is the honest fallback.
    /// Publish the settings a session runs under. See `Entry::settings`.
    pub fn set_settings(&self, session_id: &str, rows: Vec<crate::protocol::SettingRow>) {
        let mut g = self.lock();
        if let Some(e) = g.entry_mut(session_id) {
            e.settings = rows;
        }
    }

    /// **Publish what this session's harness found about its frozen prefix** — the
    /// sentence [`Registry::stale_prefix`] hands an attaching head, or `None` when the
    /// session speaks the prompt this daemon would compose now.
    ///
    /// Written by the harness on open and on every fork (a re-seat puts the session onto
    /// the seated prompt, so there is nothing left to say). A session nobody has opened —
    /// every session on disk, and every child served by its own reader — has no entry here
    /// and says nothing, which is the honest answer: no harness has compared anything.
    ///
    /// **And a head that is ALREADY watching is told now.** The operator, after a daemon
    /// restart: *"restarted pg-noop, didnt get offer to reseat, interesting"* — and then
    /// *"ok reseat came … but later"*. The daemon registers a session and binds its socket
    /// BEFORE it opens the harness (the vocabulary load and a long resume take seconds), so
    /// a head that reconnects across a restart is attached during that window: `seat_in`
    /// found no sentence because none had been written yet, and the one written a moment
    /// later waited in this mailbox for the NEXT attach. A head attached here hears it
    /// live, so it files it at the seam it arrives at — the end of the conversation the
    /// harness has just restored — and draws it.
    ///
    /// Published outside the registry's lock (a publish takes the hub's), after the
    /// sentence is stored. A head whose `seat_in` read the mailbox between the two hears it
    /// twice, which is a repeated true sentence in a window of microseconds; the order —
    /// store, then count the heads — is what makes the other interleaving (heard never)
    /// impossible, because `seat_in` attaches before it reads.
    ///
    /// **And a head is told when it STOPS being true** — `prefix_current`, published when a
    /// sentence that was stored is cleared (a re-seat, or any fork onto the seated prompt).
    /// The operator, asking for the reseat card: *"when on abstain keep reseat select open.
    /// this way i keep reseat in mind because it stares at me yet can continue at my risk"*.
    /// A card that stares until the prompt is current needs to hear when it is; before this,
    /// nothing told a head the state had cleared, and a head could only have guessed from the
    /// `reseated` row (which a `/compact` onto the seated prompt never says). Said only on the
    /// transition from stale to current, so the ordinary open — nothing stored, nothing
    /// cleared — says nothing, and only to a session with a head on it, for the reason the
    /// stale sentence is: a head that attaches later is told by `seat_in`'s silence.
    ///
    /// **No protocol bump**: a warning's code is open vocabulary on a frame every version
    /// parses, so an older head files `prefix_current` as one more note row.
    pub fn set_stale_prefix(&self, session_id: &str, said: Option<String>) {
        let (hub, was) = {
            let mut g = self.lock();
            match g.entry_mut(session_id) {
                Some(e) => {
                    let was = std::mem::replace(&mut e.stale_prefix, said.clone());
                    (e.hub.clone(), was)
                }
                None => return,
            }
        };
        if hub.attached_heads() == 0 {
            return;
        }
        match (said, was) {
            (Some(said), _) => {
                hub.publish(crate::SessionEvent::Warning {
                    code: "prefix_stale".into(),
                    detail: said,
                    compaction: None,
                });
            }
            (None, Some(_)) => {
                hub.publish(crate::SessionEvent::Warning {
                    code: "prefix_current".into(),
                    detail: PREFIX_CURRENT.into(),
                    compaction: None,
                });
            }
            (None, None) => {}
        }
    }

    /// The sentence an attaching head should be told, or `None` for silence. See
    /// [`Registry::set_stale_prefix`].
    pub fn stale_prefix(&self, session_id: &str) -> Option<String> {
        let g = self.lock();
        g.entry(session_id).and_then(|e| e.stale_prefix.clone())
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
        if let Some(e) = g.entry_mut(session_id) {
            e.jobs = jobs;
        }
    }

    /// **The monitors a session's harness last published** — the job table's mailbox, for
    /// the same reason: the monitors are the harness's, and this is answered on a head's
    /// connection. Answered beside the jobs, on the same frame.
    pub fn set_monitors(&self, session_id: &str, monitors: Vec<crate::protocol::MonitorEntry>) {
        let mut g = self.lock();
        if let Some(e) = g.entry_mut(session_id) {
            e.monitors = monitors;
        }
    }

    pub fn monitors(&self, session_id: &str) -> Vec<crate::protocol::MonitorEntry> {
        let g = self.lock();
        g.entry(session_id)
            .map(|e| e.monitors.clone())
            .unwrap_or_default()
    }

    pub fn jobs(&self, session_id: &str) -> Vec<crate::protocol::JobEntry> {
        let g = self.lock();
        g.entry(session_id)
            .map(|e| e.jobs.clone())
            .unwrap_or_default()
    }

    /// **Install a session's standing-notes source.** Called by the session's own harness at
    /// open — it is the half that owns the token counter, which is what decides the form each
    /// note has — and removed with the session ([`Registry::remove`]). See [`NotesSource`] for
    /// why this is a closure and a live read.
    pub fn set_notes_source(&self, session_id: &str, source: NotesSource) {
        self.notes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), source);
    }

    /// **The standing notes a session's harness reads, one row each, read NOW.**
    ///
    /// The sibling of [`Registry::jobs`] one screen up and deliberately NOT its shape: the
    /// notes are the operator's own files, edited in another window with nothing to tell the
    /// daemon about it, so a mailbox would answer *what the corpus was when the section was
    /// last assembled* — a fact about the prompt and not about the corpus — and a note written
    /// while the session sat idle would be missing from a pane that claims one row per note.
    /// The source reads on the ask. See [`NotesSource`].
    ///
    /// **Empty is a real answer and it is not a silence**: a session whose harness has not
    /// opened has no source, and an empty corpus has no rows, and from out here those look
    /// alike. The pane says *the harness is reading none for this session*, which is true of
    /// both.
    pub fn notes(&self, session_id: &str) -> Vec<crate::protocol::NoteEntry> {
        // The closure is cloned out before it is called, so the lock is not held across a
        // readdir, a file read and a pass of token counting — the same rule
        // `row_body_from_store` keeps one screen up.
        let source = self
            .notes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned();
        source.map(|s| s()).unwrap_or_default()
    }

    /// **Install a session's live job-output read.** Called by the session's own harness at
    /// open, when it has a process host. See [`JobOutputSource`].
    pub fn set_job_output_source(&self, session_id: &str, source: JobOutputSource) {
        self.job_output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), source);
    }

    /// **One job's output window, read NOW, off the worker** — or `None` when this session has
    /// no live source or its host does not hold the job, and the read must be queued for the
    /// worker to answer from the store. See [`JobOutputSource`].
    pub fn job_output(
        &self,
        session_id: &str,
        job: &str,
        offset: u64,
    ) -> Option<crate::SessionEvent> {
        // Cloned out before it is called, so the lock is not held across the host's own.
        let source = self
            .job_output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()?;
        source(job, offset)
    }

    /// What a session's harness last published. Empty for a session whose
    /// harness has not opened, which the head says rather than hides.
    pub fn settings(&self, session_id: &str) -> Vec<crate::protocol::SettingRow> {
        let g = self.lock();
        g.entry(session_id)
            .map(|e| e.settings.clone())
            .unwrap_or_default()
    }

    pub fn set_title(&self, session_id: &str, title: impl Into<String>) {
        let mut g = self.lock();
        if let Some(e) = g.entry_mut(session_id) {
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

    /// **The queues' snapshot, for a head's pane** — see [`SessionSource::queues`]. A registry
    /// with no source says so in the snapshot rather than answering an empty queue.
    pub fn queues(&self) -> crate::event::QueueSnapshot {
        self.source()
            .map(|s| s.queues())
            .unwrap_or_else(|| crate::event::QueueSnapshot {
                unreadable: Some("this daemon has no session store, so it has no queues".into()),
                ..Default::default()
            })
    }

    /// **The queues' snapshot for one asking session** — [`Registry::queues`] with
    /// [`crate::event::QueueSnapshot::here`] filled from the source, so the head can scope what
    /// it draws to its own repository.
    pub fn queues_for(&self, session_id: &str) -> crate::event::QueueSnapshot {
        let mut snap = self.queues();
        snap.here = self.source().and_then(|s| s.queue_here(session_id));
        snap
    }

    /// **Publish one event to EVERY session's log** — the door a daemon-level fact takes, and
    /// the merge queue is the first thing that needed one.
    ///
    /// # Why a broadcast, and what it costs
    ///
    /// Every other `publish` in this daemon goes to ONE session's hub, because every other
    /// event is a fact about that session: a job it backgrounded, a subagent it spawned, a
    /// decision its gate made. The merge queue is not — there is one `main`, one queue and one
    /// entry per branch, and the `session_id` on an entry is its ORIGIN rather than a filter.
    ///
    /// A head attached to any session can open the queue pane (`ListMergeQueue` is answered to
    /// whoever asks), so the events that keep that pane current have to reach any session's
    /// log. The alternative — one session, the daemon's own — is cheaper and wrong in the one
    /// case the pane exists for: a head attached elsewhere, watching the queue it is about to
    /// land into.
    ///
    /// **The cost is named rather than discovered:** every session's log carries every entry's
    /// every move, so a daemon with M sessions and a queue that sees N moves records M×N
    /// events. They are log events and not transcript items — `Hub::publish` appends and fans
    /// out, it does not touch the conversation and does not ring the bell — so no prompt byte
    /// and no token changes, no turn is started, and the log's own bounds age them out. What
    /// it does cost is memory in every session's scrollback, which is why this is a door with
    /// a name rather than a `for` loop at the call site.
    ///
    /// Returns how many logs it reached, so a caller can say *nobody heard* rather than
    /// assuming somebody did.
    /// **Every session's hub, counted** — plus the jobs row the registry keeps beside it — for
    /// the daemon's memory census. Collected under the registry lock and counted outside it, in
    /// [`Registry::broadcast`]'s order.
    pub fn census(&self) -> Vec<(crate::hub::HubCensus, usize)> {
        let hubs: Vec<(Arc<Hub>, usize)> = {
            let g = self.lock();
            g.entries
                .iter()
                .map(|(_, e)| (e.hub.clone(), e.jobs.len()))
                .collect()
        };
        hubs.into_iter()
            .map(|(h, jobs)| (h.census(), jobs))
            .collect()
    }

    pub fn broadcast(&self, event: crate::event::SessionEvent) -> usize {
        // **The hubs are collected under the registry lock and published outside it.** The
        // same ordering `list` and `get` follow: the registry lock is never held while a hub
        // lock is taken, and a publish that blocked a session would otherwise block the whole
        // registry.
        let hubs: Vec<Arc<Hub>> = {
            let g = self.lock();
            g.entries.iter().map(|(_, e)| e.hub.clone()).collect()
        };
        let mut reached = 0;
        for hub in hubs {
            hub.publish(event.clone());
            reached += 1;
        }
        reached
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
                    stored_end: on_disk.and_then(|d| d.stored_end.clone()),
                    // The row is the pause's one home, live or not — the harness writes it the
                    // moment the verb lands, so the store is never behind the session here.
                    nags_paused: on_disk.and_then(|d| d.nags_paused),
                    waits_on_operator: on_disk.map(|d| d.waits_on_operator).unwrap_or(0),
                    // The runner's own record for a child it is running, then the store's for
                    // one a restart brought back.
                    slug: self
                        .slug(&session_id)
                        .or_else(|| on_disk.map(|d| d.slug.clone()))
                        .unwrap_or_default(),
                    session_id,
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
                stored_end: d.stored_end,
                nags_paused: d.nags_paused,
                waits_on_operator: d.waits_on_operator,
                slug: d.slug,
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
        match self.next_work_until(None) {
            WorkOrIdle::Work(w) => Some(w),
            // Unreachable without a deadline (see `next_any`), and `Closed` is the
            // `None` this has always returned.
            WorkOrIdle::Idle | WorkOrIdle::Closed => None,
        }
    }

    /// The same, giving up at DEADLINE and saying so.
    ///
    /// **The worker's one blocking call, with the clock as an input.** Before this the
    /// daemon could wait for work and nothing else, so anything it wanted to do *after a
    /// pause* had to be done at the end of a turn instead — which is exactly the shape the
    /// operator rejected for the todo check: *"maybe wait for a timeout actually. so send it
    /// when model is idling"*, and *"but certainly not after my message."* A turn boundary
    /// is not an idle one; this is.
    ///
    /// The command queue is drained before the deadline is looked at, so a head that pressed
    /// enter is served at once — the ordering `Bell::next_any` already keeps between work and
    /// wakes, now kept between work and the clock.
    pub fn next_work_until(&self, deadline: Option<Instant>) -> WorkOrIdle {
        loop {
            match self.bell.next_any_until(deadline) {
                RingWait::Closed => return WorkOrIdle::Closed,
                RingWait::Idle => return WorkOrIdle::Idle,
                RingWait::Ring(Ring::Open(id)) => return WorkOrIdle::Work(Work::Open(id)),
                // A wake for a session this registry does not hold is dropped, the
                // same way a command for one is: the session is gone and there is
                // nothing to wake.
                RingWait::Ring(Ring::Woken(id)) => {
                    if self.get(&id).is_some() {
                        return WorkOrIdle::Work(Work::Woken(id));
                    }
                }
                RingWait::Ring(Ring::Command(id)) => {
                    let Some(hub) = self.get(&id) else { continue };
                    if let Some(cmd) = hub.try_command() {
                        self.set_default(&id);
                        return WorkOrIdle::Work(Work::Command(id, cmd));
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

    /// **Install what this session must end when the daemon stops.** See [`RunEnder`].
    ///
    /// Set once per session, at open, beside the other per-session drivers. A second
    /// install for one session replaces the first rather than adding to it: a session has
    /// one process host, and two enders for it would be two answers to one question.
    pub fn watch_runs(&self, session: &str, ender: RunEnder) {
        self.runs
            .lock()
            .expect("run enders")
            .insert(session.to_string(), ender);
    }

    /// **Install the boundary this session's `!term` panes run inside.** See [`PaneBoundary`].
    /// A second install replaces the first, as [`Registry::watch_runs`] does: one process host,
    /// one boundary.
    pub fn bound_panes(&self, session: &str, boundary: PaneBoundary) {
        self.panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string(), boundary);
    }

    /// This session's pane boundary, or `None` for a session that installed none.
    pub fn pane_boundary(&self, session: &str) -> Option<PaneBoundary> {
        self.panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .cloned()
    }

    /// Close every session and the bell. Every head wakes with `Bye`, every worker
    /// falls out of `next_command`.
    ///
    /// **And every run still in flight is ended**, because a worker inside one has not
    /// reached `next_command` and will not until the run does. See [`RunEnder`]. The enders
    /// are taken and called **before** the hubs close, so the sentence each one returns can
    /// still be published to the heads that are watching — a stop that ended the operator's
    /// command is a fact about their session, and it belongs on the log rather than in a file
    /// nobody tails.
    ///
    /// **And every TURN in flight is interrupted** — the same problem one layer up, with a
    /// different answer because the thing being held is different. A *run* is a command's
    /// process tree and a cgroup ends it; a *turn* is a generation, and what holds it is the
    /// model's own stream, which no ender can reach. What reaches it is the interrupt a head's
    /// Esc-Esc sends, through the one door that exists for it ([`Hub::submit`]'s
    /// [`DAEMON_SUBMITTER`] arm, which admits `Interrupt` and nothing else of the sort), and
    /// that is what every session with a turn in flight is handed here, **before** the hubs
    /// close so the running turn's steering poll still finds it.
    ///
    /// **Why it is not left to finish.** That was the design and the operator has now
    /// overruled it in their own words: *"a stop must not start a turn, and must interrupt the
    /// turns it holds"*. Measured cost of the old rule on one evening: eleven
    /// `--continue` refusals in the daemon's own log, five of them back to back — the daemon
    /// asked to stop unlinked its socket and dropped its listener within a poll of the signal,
    /// went on holding the folder's lock until its last turn finished its round, and every
    /// liveness test this box has then said *no daemon here*. The launcher started one; the
    /// folder claim refused it. Ending the turn is what makes the stop take as long as the
    /// stop and not as long as the round.
    ///
    /// **And it does not start a turn.** The interrupt is a command like any other, so a
    /// session whose turn is NOT running is left alone here (`status().running` is the
    /// question, asked of the hub) — and even if one were handed over, the daemon's own arm
    /// for it between turns is `interrupt_idle`, not a prompt.
    pub fn close(&self) {
        // **Said first, before any turn is interrupted**: a subagent whose turn the interrupts
        // below end must be able to tell its daemon's stop from a person's (`is_stopping`).
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let hubs: Vec<Arc<Hub>> = self
            .lock()
            .entries
            .iter()
            .map(|(_, e)| e.hub.clone())
            .collect();
        // Once, and only once, for the reason the field gives.
        if !self
            .ended_runs
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let enders: Vec<RunEnder> = self
                .runs
                .lock()
                .expect("run enders")
                .drain()
                .map(|(_, f)| f)
                .collect();
            let said: Vec<String> = enders.iter().flat_map(|f| f()).collect();
            // To every hub this registry holds — the ones collected above, not `list()`, which
            // read every stored session from the source to find the live ones (R68.8).
            if !said.is_empty() {
                for hub in &hubs {
                    for sentence in &said {
                        hub.publish(crate::event::SessionEvent::Warning {
                            code: "daemon_stopping_runs".into(),
                            detail: sentence.clone(),
                            compaction: None,
                        });
                    }
                }
            }
            // **And the turns, once, before the hubs close.** `submit` is the only door that
            // reaches a turn in flight, and it is taken at the next token by the steering poll
            // (`Harness::HubSteering`), which is what *interrupt* means here: the partial output
            // is kept and the round is recorded as aborted, exactly as an Esc-Esc is.
            //
            // **Inside the once-guard with the run enders, and not outside it.** `close` is
            // called three times on the way out — by the `Stop` frame, by `catch_signals` and
            // again by `ServerHandle::shutdown` — and a resubmission would be a second
            // `CommandIssued` for one decision, which is exactly what `ended_runs` exists to
            // stop for the runs.
            for h in &hubs {
                if h.status().running {
                    h.submit(
                        DAEMON_SUBMITTER,
                        "daemon-stopping",
                        // No expectation: this is not a head following the stream, and a seq
                        // check here could only refuse the stop.
                        0,
                        CommandKind::Interrupt {
                            reason: STOPPING_TURN_REASON.to_string(),
                        },
                    );
                }
            }
        }
        for h in hubs {
            h.close();
        }
        self.bell.close();
    }

    pub fn is_closed(&self) -> bool {
        self.bell.is_closed()
    }

    /// **This daemon has begun to stop** — set at the very start of [`Registry::close`], before
    /// the turns in flight are interrupted, so whatever those interrupts end can tell the daemon's
    /// stop from a person's. `is_closed` becomes true only at the END of `close`, after the
    /// interrupts are out, which is too late for a turn that settles in between.
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(std::sync::atomic::Ordering::SeqCst)
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
    use crate::hub::{CommandKind, Delivery};
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

    /// **A session served by its own thread is never served by the worker** (R68.1).
    ///
    /// The defect this pins: a head prompting the subagent it is attached to rang the bell, and
    /// the worker's `try_command` popped the prompt off the child's queue — then built a second
    /// `Harness` for a session whose own thread was holding one. With the owner on the hub, the
    /// worker's pop answers `None` and the ring is skipped like any ring with nothing behind it,
    /// while the child's own reader still finds the command.
    #[test]
    fn the_worker_never_takes_a_command_off_a_hub_owned_by_its_own_thread() {
        let r = reg();
        let hub = r.new_hub("s-parent-sub-1");
        hub.owned_by_its_own_thread();
        r.adopt(
            hub.clone(),
            "a child",
            SessionWiring::default(),
            Some("s-parent".into()),
        )
        .unwrap();
        let h = hub.attach("tui", "dead", Caps::default(), 0);
        hub.submit(
            &h.head_id,
            "c1",
            0,
            CommandKind::Prompt {
                text: "what did you find?".into(),
                attachments: Vec::new(),
            },
        );
        let soon = Instant::now() + std::time::Duration::from_millis(50);
        assert!(
            matches!(r.next_work_until(Some(soon)), WorkOrIdle::Idle),
            "the worker took a prompt off a subagent's queue — the second writer"
        );
        match hub.take_own_work_until(Some(Instant::now())) {
            crate::hub::OwnWork::Command(c) => {
                assert!(matches!(c.kind, CommandKind::Prompt { .. }))
            }
            other => panic!("the child's own reader must still find its prompt: {other:?}"),
        }
    }

    /// **A removed session is gone from every table, its hub is closed, and the sessions after
    /// it are still found by id** (R68.3) — the index R67 item 27 added is rewritten for every
    /// position the removal shifted, and `list` keeps arrival order.
    #[test]
    fn a_removed_session_leaves_every_table_and_the_rest_are_still_found() {
        let r = reg();
        for id in ["s-a", "s-b", "s-c", "s-d"] {
            r.create(id, id, SessionWiring::default()).unwrap();
        }
        let b = r.get("s-b").unwrap();
        let head = b.attach("tui", "dead", Caps::default(), 0);
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ended.clone();
        r.watch_runs(
            "s-b",
            Arc::new(move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Vec::new()
            }),
        );
        r.set_notes_source("s-b", Arc::new(Vec::new));
        r.set_default("s-b");

        let gone = r.remove("s-b").expect("it was held");
        assert!(Arc::ptr_eq(&gone, &b));
        assert!(
            b.is_closed(),
            "a removed hub must not keep a head waiting on it"
        );
        assert!(matches!(b.next_batch(&head.head_id, 10), Delivery::Closed));
        assert!(r.get("s-b").is_none() && !r.holds("s-b"));
        assert!(r.remove("s-b").is_none(), "a second close is not an error");
        for id in ["s-a", "s-c", "s-d"] {
            assert_eq!(
                r.get(id).map(|h| h.session_id()),
                Some(id.to_string()),
                "the index points {id} at somebody else's entry after the removal"
            );
            assert_eq!(r.title_of(id).as_deref(), Some(id));
        }
        let order: Vec<String> = r.list().into_iter().map(|b| b.session_id).collect();
        assert_eq!(order, ["s-a", "s-c", "s-d"], "arrival order survives");
        assert_eq!(
            r.default_id(),
            "s-d",
            "the default moved to a session that exists"
        );
        // The run ender went with it: a stop ends nothing for a session that is gone.
        r.close();
        assert!(
            !ended.load(std::sync::atomic::Ordering::SeqCst),
            "the removed session's run ender was still called at stop"
        );
    }

    /// **`parent_of` answers from the entry** (R68.6) — and does not need a source at all, which
    /// is the point: the old lookup was `list()`, a read of every stored session.
    #[test]
    fn the_parent_is_read_from_the_entry_not_the_store() {
        let r = reg();
        r.create("s-root", "", SessionWiring::default()).unwrap();
        let child = r.new_hub("s-root-sub");
        r.adopt(child, "", SessionWiring::default(), Some("s-root".into()))
            .unwrap();
        assert_eq!(r.parent_of("s-root-sub").as_deref(), Some("s-root"));
        assert_eq!(r.parent_of("s-root"), None);
        assert!(r.holds("s-root") && !r.holds("s-nobody"));
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

    /// **A stop interrupts the turns it holds — and starts none.**
    ///
    /// The operator's ruling, verbatim: *"a stop must not start a turn, and must interrupt the
    /// turns it holds"*. The state this measures is the one that cost them an evening of
    /// `--continue` refusals: `close` wakes the worker out of `next_command`, and a worker
    /// **inside a turn** has not reached it — so the daemon went on holding the folder's lock
    /// after its socket file and its listener were already gone, and the next start was refused
    /// by the claim.
    ///
    /// Three facts, and the third is the one a fix gets wrong:
    ///
    ///  1. the session with a turn in flight is handed the interrupt the running turn's own
    ///     steering poll takes — asked for through `try_steering_command`, which is literally
    ///     the call `HubSteering` makes once per frame;
    ///  2. it is an `Interrupt` naming why, and not a prompt: the reason is the named constant,
    ///     so the row the turn keeps and the sentence a head is shown cannot drift;
    ///  3. a session with NOTHING running is left alone. A stop that handed every hub a command
    ///     would be a stop that could start a turn, which is half of the same rule.
    #[test]
    fn a_stop_interrupts_the_turns_it_holds_and_starts_none() {
        let r = reg();
        let running = r.create("s-running", "", SessionWiring::default()).unwrap();
        let idle = r.create("s-idle", "", SessionWiring::default()).unwrap();
        running.publish(testing::turn_started("t1"));
        assert!(running.status().running, "the premise: a turn is in flight");
        assert!(!idle.status().running, "the premise: and this one is idle");

        r.close();

        match running.try_steering_command() {
            Some(cmd) => assert_eq!(
                cmd.kind,
                CommandKind::Interrupt {
                    reason: STOPPING_TURN_REASON.to_string()
                },
                "the stop hands the turn the interrupt an Esc-Esc sends, and says why"
            ),
            None => panic!(
                "the stop left the turn in flight: nothing for its steering poll, so the \
                 daemon waits for the round it was asked to end"
            ),
        }
        assert!(
            idle.try_command().is_none(),
            "a stop queued a command for a session with nothing running"
        );
        // And it is an interrupt rather than a turn: nothing was appended to the conversation.
        assert_eq!(idle.snapshot().turn, None, "a stop must not start a turn");
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

    /// **A broadcast reaches every session's log, and only the log.**
    ///
    /// The door the merge queue's events take, and the two halves of the claim are separate:
    ///
    ///   * **Every session.** A head attached to any session can open the queue pane, so an
    ///     event that reached only the daemon's own session would leave that head drawing the
    ///     state it last saw. The count is asserted, so a door that reached one log and reported
    ///     success would fail here.
    ///   * **Only the log.** `publish` appends and fans out; it does not touch a conversation
    ///     and does not ring the bell, which is what makes broadcasting affordable — no prompt
    ///     byte changes and no turn is started. Asserted by reading back what the hub retained
    ///     rather than by trusting the doc.
    #[test]
    fn a_broadcast_reaches_every_sessions_log_and_rings_no_bell() {
        let r = reg();
        for id in ["s-a", "s-b", "s-c"] {
            r.create(id, "", SessionWiring::default()).unwrap();
        }
        let event = crate::event::SessionEvent::QueueMoved {
            queue: "merge".into(),
            id: "m-1".into(),
            to: "landed".into(),
            why: "landed at deadbeef".into(),
        };
        assert_eq!(r.broadcast(event.clone()), 3, "every session's log");

        // **Every one of them really holds it**, read back through the hub's own retained log.
        for id in ["s-a", "s-b", "s-c"] {
            let hub = r.get(id).expect("the session");
            let retained = hub.retained();
            assert!(
                retained.iter().any(|e| matches!(
                    &e.event,
                    crate::event::SessionEvent::QueueMoved { id, .. } if id == "m-1"
                )),
                "{id} did not record the move: {retained:?}"
            );
        }

        // **And no wake was rung.** A broadcast that started a turn in every session would be
        // a queue move that costs a generation per session, which is the cost this door exists
        // to bound. The bell is drained and found empty.
        r.bell().close();
        assert!(
            r.bell().next().is_none(),
            "a broadcast must not wake anybody"
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
                attachments: Vec::new(),
            },
        );
        a.submit(
            &ha.head_id,
            "c2",
            0,
            CommandKind::Prompt {
                text: "for a".into(),
                attachments: Vec::new(),
            },
        );

        // Arrival order across sessions, which is the promise §13.2 already makes
        // for two heads on one session.
        let (id1, c1) = r.next_command().unwrap();
        assert_eq!(id1, "s-b");
        assert_eq!(
            c1.kind,
            CommandKind::Prompt {
                text: "for b".into(),
                attachments: Vec::new(),
            }
        );
        let (id2, c2) = r.next_command().unwrap();
        assert_eq!(id2, "s-a");
        assert_eq!(
            c2.kind,
            CommandKind::Prompt {
                text: "for a".into(),
                attachments: Vec::new(),
            }
        );
    }

    /// **THE OPERATOR'S RULING, 2026-09-25, IS THIS TEST: *"yes whole messages queue is dequeued in
    /// one go."*** Alice queues `alice one`, then `alice two`, then recalls: **both** go, in one
    /// take-back, because the head put both back in the composer in one press — the queue drains as
    /// a unit and never a message at a time. What the `↑` could not have taken is anything that
    /// arrives *behind* the withdraw, which is the next test.
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
                attachments: Vec::new(),
            },
        );
        a.submit(
            &hb.head_id,
            "c2",
            0,
            CommandKind::Prompt {
                text: "bob".into(),
                attachments: Vec::new(),
            },
        );
        a.submit(
            &ha.head_id,
            "c3",
            0,
            CommandKind::Prompt {
                text: "alice two".into(),
                attachments: Vec::new(),
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
        assert_eq!(
            left[0].kind,
            CommandKind::Prompt {
                text: "bob".into(),
                attachments: Vec::new(),
            }
        );
        // And the take-back itself is consumed: a second one finds nothing.
        assert!(!a.try_withdraw_command());
    }

    /// **A PROMPT TYPED AFTER THE TAKE-BACK IS NOT ONE OF THE PROMPTS IT NAMES.**
    ///
    /// The take-back is a RECALL OF WHAT IS QUEUED, and its whole point is that the
    /// operator gets those words back in the composer to edit. What they send next is
    /// the edited copy — or a new message entirely — and it arrives in this queue
    /// *after* the withdraw, because the two go out on one socket in the order they
    /// were typed. Dropping it loses what the operator wrote with no frame to say so,
    /// and the head keeps drawing `queued` for a row that is never coming.
    ///
    /// **Measured, on the operator's own session.** They queued
    /// `lol, it is a bag`, pressed Up to fix the typo, and sent `lol, it is a bug`.
    /// The edited copy never became a transcript row — the two messages they sent
    /// after it became rows `#t20.2966` and `#t20.2979` — so their screen said
    /// `queued · lol, it is a bug` for the rest of the session.
    #[test]
    fn a_take_back_does_not_drop_a_prompt_sent_after_it() {
        let r = reg();
        let a = r.create("s-a", "", SessionWiring::default()).unwrap();
        let ha = a.attach("tui", "alice", Caps::default(), 0);

        // the message the operator wants back
        a.submit(
            &ha.head_id,
            "c1",
            0,
            CommandKind::Prompt {
                text: "lol, it is a bag".into(),
                attachments: Vec::new(),
            },
        );
        // Up — the recall itself
        a.submit(&ha.head_id, "c2", 0, CommandKind::WithdrawPrompts);
        // and the corrected copy, typed after the recall and sent after it
        a.submit(
            &ha.head_id,
            "c3",
            0,
            CommandKind::Prompt {
                text: "lol, it is a bug".into(),
                attachments: Vec::new(),
            },
        );

        assert!(a.try_withdraw_command(), "the take-back was acted on");
        let left: Vec<_> = (0..3).filter_map(|_| a.try_steering_command()).collect();
        assert_eq!(
            left.len(),
            1,
            "the recalled prompt is gone and the corrected copy is not: {left:?}"
        );
        assert_eq!(
            left[0].kind,
            CommandKind::Prompt {
                text: "lol, it is a bug".into(),
                attachments: Vec::new(),
            },
            "the prompt typed AFTER the take-back survives it"
        );
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
                attachments: Vec::new(),
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

    /// **Wakes for one session coalesce while they wait** (R68.5) — one `Harness::wake` drains
    /// everything the session owns, so a hundred rings before the worker gets there are one pass,
    /// not a hundred passes over the store. Another session's wake keeps its own place.
    #[test]
    fn a_storm_of_wakes_for_one_session_is_one_wake() {
        let r = reg();
        r.create("s-a", "", SessionWiring::default()).unwrap();
        r.create("s-b", "", SessionWiring::default()).unwrap();
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
        for _ in 0..100 {
            r.bell().ring_wake("s-a");
        }
        r.bell().ring_wake("s-b");
        r.bell().ring_wake("s-a");
        let soon = || Some(Instant::now() + std::time::Duration::from_millis(30));
        assert!(
            matches!(r.next_work_until(soon()), WorkOrIdle::Work(Work::Woken(id)) if id == "s-a")
        );
        assert!(
            matches!(r.next_work_until(soon()), WorkOrIdle::Work(Work::Woken(id)) if id == "s-b")
        );
        assert!(
            matches!(r.next_work_until(soon()), WorkOrIdle::Idle),
            "the storm was served more than once"
        );
    }

    /// **The idle clock is not starved by wakes** (R68.5): a due deadline waits behind at most
    /// [`WAKES_AHEAD_OF_A_DUE_CLOCK`] wakes, is answered `Idle`, and the wake after it is served on
    /// the next call. A command still goes first: somebody is waiting on it.
    #[test]
    fn a_due_clock_waits_behind_a_bounded_run_of_wakes_but_not_of_commands() {
        let r = reg();
        let n = WAKES_AHEAD_OF_A_DUE_CLOCK + 2;
        for i in 0..n {
            r.create(format!("s-{i}"), "", SessionWiring::default())
                .unwrap();
            assert!(matches!(r.next_work(), Some(Work::Open(_))));
        }
        for i in 0..n {
            r.bell().ring_wake(&format!("s-{i}"));
        }
        let past = Some(Instant::now());
        let mut answers = Vec::new();
        for _ in 0..n + 1 {
            answers.push(match r.next_work_until(past) {
                WorkOrIdle::Work(Work::Woken(_)) => "wake",
                WorkOrIdle::Idle => "idle",
                _ => "other",
            });
        }
        let mut expected = vec!["wake"; WAKES_AHEAD_OF_A_DUE_CLOCK];
        expected.push("idle");
        expected.extend(["wake", "wake"]);
        assert_eq!(
            answers, expected,
            "a due clock waited behind every wake — under a storm it would wait for ever"
        );

        let a = r.get("s-0").unwrap();
        let h = a.attach("tui", "dead", Caps::default(), 0);
        for i in 0..n {
            r.bell().ring_wake(&format!("s-{i}"));
        }
        for _ in 0..WAKES_AHEAD_OF_A_DUE_CLOCK {
            let _ = r.next_work_until(past);
        }
        a.submit(
            &h.head_id,
            "c1",
            0,
            CommandKind::Prompt {
                text: "hello".into(),
                attachments: Vec::new(),
            },
        );
        assert!(
            matches!(r.next_work_until(past), WorkOrIdle::Work(Work::Command(..))),
            "the operator's prompt waited behind a timer"
        );
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
            CommandKind::Prompt {
                text: "x".into(),
                attachments: Vec::new(),
            },
        );
        r.next_command().unwrap();
        assert_eq!(r.default_id(), "s-b");
        let _ = a;
    }

    /// **The notes source is read on the ASK, not remembered** — the one thing that
    /// separates it from the job table's mailbox.
    ///
    /// The notes are the operator's own files, edited in another window with nothing to tell
    /// the daemon, so a mailbox refilled at session open would answer *what the corpus was
    /// when the section was last assembled* — and a note written while the session sat idle
    /// would be missing from a pane that claims one row per note. The closure is called per
    /// read, and this pins that: a source whose answer moves between two reads must be seen
    /// to move, or the pane is a snapshot wearing a read's name.
    #[test]
    fn the_notes_source_is_read_on_the_ask_and_a_session_without_one_has_none() {
        let r = reg();
        r.create("s1", "", SessionWiring::default()).unwrap();
        // A session whose harness has not opened has no source, and that is no rows — the
        // same answer an empty corpus gives, which the pane says out loud.
        assert!(r.notes("s1").is_empty());
        assert!(r.notes("nobody").is_empty());

        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = reads.clone();
        r.set_notes_source(
            "s1",
            Arc::new(move || {
                let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (0..=n)
                    .map(|i| crate::protocol::NoteEntry {
                        path: format!("/tmp/note-{i}.md"),
                        abstract_line: Some(format!("note {i}")),
                        abstract_written: false,
                        form: crate::protocol::NoteForm::Verbatim,
                    })
                    .collect()
            }),
        );
        assert_eq!(r.notes("s1").len(), 1);
        assert_eq!(
            r.notes("s1").len(),
            2,
            "the source is called again, not replayed"
        );
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}

#[cfg(test)]
mod stored_end_wire {
    use super::*;

    /// **A brief from a daemon that predates `stored_end` still decodes**, as `None` — which
    /// is why the field needed no protocol bump.
    #[test]
    fn a_brief_without_stored_end_decodes_as_not_told() {
        let b = SessionBrief {
            session_id: "s".into(),
            title: String::new(),
            created_ms: 0,
            status: SessionStatus::default(),
            wiring: SessionWiring::default(),
            live: false,
            stored_items: 0,
            parent_session_id: None,
            context_tokens: None,
            context_cached: None,
            stored_end: Some(StoredEnd::Answered {
                first_line: "done".into(),
            }),
            nags_paused: None,
            waits_on_operator: 0,
            slug: String::new(),
        };
        let mut v = serde_json::to_value(&b).unwrap();
        assert_eq!(
            serde_json::from_value::<SessionBrief>(v.clone()).unwrap(),
            b,
            "round trip"
        );
        v.as_object_mut().unwrap().remove("stored_end");
        assert_eq!(
            serde_json::from_value::<SessionBrief>(v)
                .unwrap()
                .stored_end,
            None
        );
    }

    /// **A child's name rides the list** — the runner's record for a session this daemon
    /// holds — and a brief from a daemon that predates the field decodes with no name.
    #[test]
    fn a_childs_slug_is_on_its_brief_and_an_old_brief_has_none() {
        let r = Registry::new();
        let hub = r.new_hub("s-1-sub-1".to_string());
        r.adopt(
            hub,
            "s-1-sub-1".to_string(),
            SessionWiring::default(),
            Some("s-1".into()),
        )
        .unwrap();
        r.set_slug("s-1-sub-1", "fix-login");
        let b = r
            .list()
            .into_iter()
            .find(|b| b.session_id == "s-1-sub-1")
            .unwrap();
        assert_eq!(b.slug, "fix-login");
        r.set_slug("s-1-sub-1", "");
        assert_eq!(r.slug("s-1-sub-1"), None, "an empty name is no name");

        let mut v = serde_json::to_value(&b).unwrap();
        v.as_object_mut().unwrap().remove("slug");
        assert_eq!(serde_json::from_value::<SessionBrief>(v).unwrap().slug, "");
    }
}
