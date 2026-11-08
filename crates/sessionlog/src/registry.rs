//! Many sessions behind one socket: a `Hub` per session, and the management over them.
//!
//! ```text
//!            ┌─────────── Registry ───────────┐
//!   head ────┤  "s-1" → Hub ─┐                │
//!   head ────┤  "s-2" → Hub ─┼── Bell ────────┼──> a reader per session
//!   head ────┤  "s-3" → Hub ─┘                │    (harnessd's `sessions::Court`)
//!            └────────────────────────────────┘
//! ```
//!
//! `Hub` stays per-session by construction, and nothing here reaches inside one: the ledger, the
//! region and the prefix invariant are per session, and the registry only decides which hub exists,
//! which one a bare attach lands on, and which are waiting for their own reader.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::hub::{CommandKind, DAEMON_SUBMITTER, Hub, QueuedCommand, SessionStatus};
use crate::log::LogBounds;
use crate::origin::SessionOrigin;
use crate::view::ViewBounds;

/// A wake shared by every session in a registry. Not a channel: a channel needs the hub to own a
/// `Sender`, and a hub that owns one cannot be moved away.
#[derive(Debug, Default)]
pub struct Bell {
    inner: Mutex<BellInner>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct BellInner {
    pending: VecDeque<String>,
    /// Sessions that exist and have not been opened yet. A separate queue, because `pending`
    /// is arrival order across sessions and a create put in it makes a session's first command
    /// jump ahead of one submitted before it.
    opens: VecDeque<String>,
    /// Sessions something inside the daemon woke, with no command behind it. A third queue because
    /// a wake has no head and no `client_request_id` to be fair to. Each entry carries what rung it,
    /// because one kind can be taken back and the others cannot.
    wakes: VecDeque<(String, Wake)>,
    /// Sessions something asked to be closed. A fourth queue because the session is already out of
    /// the registry by then, and the worker must still hear it.
    closes: VecDeque<String>,
    /// Wakes served in a row while the caller's clock was already due. See
    /// [`WAKES_AHEAD_OF_A_DUE_CLOCK`].
    wakes_while_due: usize,
    closed: bool,
}

/// How many wakes a due clock waits behind before the worker is answered `Idle`. Not zero: a
/// clock that jumped every wake would run a session nobody asked to run.
pub const WAKES_AHEAD_OF_A_DUE_CLOCK: usize = 8;

/// What a wake was rung for, as far as the drop goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// A background job or a subagent settled. The settlement is still in its queue, so a turn
    /// stopped or failed can take this wake back: the next turn takes the settlement.
    Settlement,
    /// Anything else - a monitor's firing, a queue notice, a room question, a session put back to
    /// work. Nothing queues a second copy of those, so one taken back is a fact nobody hears.
    Other,
}

/// What a wake was about.
enum Ring {
    Open(String),
    Command(String),
    Woken(String),
    Close(String),
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

    /// Say that something inside the daemon woke this session, with no command behind it: a ring
    /// with no command is swallowed by the worker's own door. Rung as [`Wake::Other`], because that
    /// is what every caller but a settlement's ring is: a fact nothing queues twice.
    pub fn ring_wake(&self, session_id: &str) {
        self.ring_wake_for(session_id, Wake::Other);
    }

    /// A settlement rang: a job or a subagent ended, and its completion is in the session's queue.
    /// The one wake a stopped or failed turn takes back - see [`Bell::drop_wakes`].
    pub fn ring_settlement_wake(&self, session_id: &str) {
        self.ring_wake_for(session_id, Wake::Settlement);
    }

    /// One entry per session, and the second ring of a different kind upgrades it: only a
    /// settlement's can be taken back, so the entry keeps the answer the drop has to give.
    fn ring_wake_for(&self, session_id: &str, why: Wake) {
        {
            let mut g = self.lock();
            if let Some(i) = g.wakes.iter().position(|(w, _)| w == session_id) {
                if why == Wake::Other {
                    g.wakes[i].1 = Wake::Other;
                }
                return;
            }
            g.wakes.push_back((session_id.to_string(), why));
        }
        self.cv.notify_all();
    }

    /// Take back a settlement's wake this session has not been served: a turn that was stopped or
    /// failed must not be followed by one the settlement started. What it was rung for stays
    /// queued, for the next turn. Any other wake stays too - nothing else holds a copy of it.
    pub fn drop_wakes(&self, session_id: &str) {
        self.lock()
            .wakes
            .retain(|(w, why)| w != session_id || *why != Wake::Settlement);
    }

    /// Say that `session_id` exists and nothing has opened it yet.
    pub fn ring_open(&self, session_id: &str) {
        {
            let mut g = self.lock();
            g.opens.push_back(session_id.to_string());
        }
        self.cv.notify_all();
    }

    /// Say that `session_id` is to be let go of - see [`BellInner::closes`].
    pub fn ring_close(&self, session_id: &str) {
        {
            let mut g = self.lock();
            g.closes.push_back(session_id.to_string());
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

    /// Block until some session has been rung. `None` once the bell is closed and drained: a
    /// shutdown does not discard a command already accepted and announced on the log, because a
    /// head was told `Accepted` and the announcement is in the transcript.
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

    /// Block until there is either a session to open or a session to serve, giving up at
    /// `deadline`. Opens first: a session not open cannot be served.
    fn next_any_until(&self, deadline: Option<Instant>) -> RingWait {
        let mut g = self.lock();
        loop {
            // A close comes first: the session it is about no longer exists, so every other
            // queue's entry for it would be dropped and the harness stays held for ever.
            if let Some(id) = g.closes.pop_front() {
                return RingWait::Ring(Ring::Close(id));
            }
            if let Some(id) = g.opens.pop_front() {
                return RingWait::Ring(Ring::Open(id));
            }
            if let Some(id) = g.pending.pop_front() {
                return RingWait::Ring(Ring::Command(id));
            }
            // A due clock waits behind at most [`WAKES_AHEAD_OF_A_DUE_CLOCK`] wakes: looking at
            // the deadline only when every queue was empty meant a steady stream of wakes kept
            // `Idle` from ever coming. A head's command still outranks the clock.
            let due = deadline.is_some_and(|d| Instant::now() >= d);
            if due && !g.wakes.is_empty() && g.wakes_while_due >= WAKES_AHEAD_OF_A_DUE_CLOCK {
                g.wakes_while_due = 0;
                return RingWait::Idle;
            }
            // Last, and deliberately: a wake has nobody waiting on it, and draining wakes first
            // would let a chatty monitor put itself in front of the operator.
            if let Some((id, _)) = g.wakes.pop_front() {
                g.wakes_while_due = if due { g.wakes_while_due + 1 } else { 0 };
                return RingWait::Ring(Ring::Woken(id));
            }
            if g.closed {
                return RingWait::Closed;
            }
            // Every queue above is empty, so this is where the clock is allowed in.
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

/// What the worker found: work, the clock, or the bell closing. `Idle` is not an error: it is
/// the deadline passing with nothing to do.
#[allow(clippy::large_enum_variant)] // a thin wrapper over Work; the size is Work's, allowed once there
pub enum WorkOrIdle {
    Work(Work),
    Idle,
    Closed,
}

/// The bell's answer when it is allowed to give up on the clock. Three and not two: `Option`
/// cannot say nothing to do, for now, distinctly from there is nothing at all.
enum RingWait {
    Ring(Ring),
    Idle,
    Closed,
}

/// What the daemon knows about a session that the log does not: its title, when it was made, and
/// what it is talking to. A picker is drawn from this.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionBrief {
    pub session_id: String,
    /// A human name. Empty until it has one, and a head shows a short id then. Set once, and then
    /// only on purpose, by `--title` or `/rename`.
    pub title: String,
    /// Unix millis when the daemon created it.
    pub created_ms: u64,
    /// Everything the log knows, cut in one lock. All zeroes for a session only in the store, which
    /// has no hub to cut.
    pub status: SessionStatus,
    /// What this session is talking to.
    pub wiring: SessionWiring,
    /// Whether this daemon holds a hub for it. `false` means it is on disk and has to
    /// be resumed ([`crate::protocol::ClientFrame::ResumeSession`]) before it can be
    /// switched to.
    pub live: bool,
    /// Transcript rows the store holds. Zero for a live session in a daemon with no store, which
    /// is a real state and not a missing measurement.
    pub stored_items: u32,
    /// The session that spawned this one as a subagent, or `None` for a top-level
    /// session. A head draws a subagent tree from this without reaching the store.
    pub parent_session_id: Option<String>,
    /// The last turn's prompt tokens, from the session's own row, or `None` before a turn has
    /// finished. A head that attaches after a daemon restart shows the context from this: the
    /// snapshot's turn state is ephemeral and a rebuilt view has none.
    pub context_tokens: Option<u64>,
    /// The last turn's cached tokens, for the cache %. `None` with `context_tokens`.
    pub context_cached: Option<u64>,
    /// How the stored conversation's last turn ended, off its last row, or `None` for a session
    /// with no rows: the one fact a head cannot get from the live view.
    #[serde(default)]
    pub stored_end: Option<StoredEnd>,
    /// The operator has paused this session's idle plan-check, and until when. Read off the
    /// session's own row, so a stored session says it too.
    #[serde(default)]
    pub nags_paused: Option<NagPause>,
    /// How many questions this session's board is waiting on the OPERATOR for: rows with an open
    /// `TodoNeed::Operator` edge. The picker marks it.
    #[serde(default)]
    pub waits_on_operator: usize,
    /// A subagent's name, its slug, or empty for a session nobody named: every root, and a child
    /// spawned before children had slugs.
    #[serde(default)]
    pub slug: String,
    /// Who this session is for: the operator's own conversation, or one of the kinds a harness
    /// makes for itself. See [`crate::SessionOrigin`].
    #[serde(default)]
    pub origin: SessionOrigin,
    /// When this session last MOVED: its last transcript row's time, or its creation when it has
    /// none. `created_ms` never changes and answers a different question.
    #[serde(default)]
    pub moved_ms: u64,
}

/// How long the operator has paused a session's idle plan-check. Wall-clock and not a duration,
/// because it has to survive the restart it was typed to outlast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NagPause {
    /// Until `/nag resume`.
    UntilResumed,
    /// Until this unix millisecond, or `/nag resume` before it.
    Until { ms: u64 },
}

impl NagPause {
    /// The store's spelling - `0` is until resumed, anything else the unix ms it ends at. The
    /// mapping lives on this side because the column's owner (`letibot-tokencore`) is a crate
    /// this one does not depend on.
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

    /// Is the pause over as of `now_ms`? Never, for one the operator ends by hand.
    pub fn over_at(self, now_ms: u64) -> bool {
        matches!(self, NagPause::Until { ms } if ms <= now_ms)
    }
}

/// How a stored conversation's last turn ended, as its last row tells it. Two words and no
/// third, because a row can only say an answer or work that had not finished.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredEnd {
    /// The last row is an answer: its first line, as the pane's subtitle.
    Answered { first_line: String },
    /// The last row is anything else: the turn was cut, or is still in flight.
    MidTurn,
}

/// What a session is attached to: the daemon's own command line, which is the only place these
/// four facts exist. Empty strings when it was started without them.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionWiring {
    pub model: String,
    pub dialect: String,
    pub endpoint: String,
    pub workspace: String,
}

/// The `model` row's value, for the one setting that has to reach a head unprompted.
fn model_row(rows: &[crate::protocol::SettingRow]) -> Option<String> {
    rows.iter()
        .find(|r| r.key == crate::protocol::MODEL_KEY)
        .map(|r| r.value.clone())
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

/// The last eight characters of a session id, with a leading ellipsis. Ids are minted `s-<nanos>`,
/// so two made the same afternoon share a prefix.
pub fn short_id(id: &str) -> String {
    let n = id.chars().count();
    if n <= 10 {
        return id.to_string();
    }
    let tail: String = id.chars().skip(n - 8).collect();
    format!("…{tail}")
}

/// A session that exists somewhere the registry cannot see, on disk. The registry holds hubs,
/// and a hub is a live thing; this is the row.
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
    /// How the stored conversation's last turn ended, read off its last row - see
    /// [`SessionBrief::stored_end`]. The one fact about a settled child that survives the daemon.
    pub stored_end: Option<StoredEnd>,
    /// The operator's pause of the idle plan-check, off the session's row - see
    /// [`SessionBrief::nags_paused`]. A pause whose end has already passed is `None` here: it is
    /// over, whether or not a daemon has yet been there to say so.
    pub nags_paused: Option<NagPause>,
    /// Open questions the session's board waits on the operator for - see
    /// [`SessionBrief::waits_on_operator`].
    pub waits_on_operator: usize,
    /// A subagent's name, from its record - see [`SessionBrief::slug`]. Empty for a session
    /// nobody named.
    pub slug: String,
    /// Who this session is for, off its own row - see [`SessionBrief::origin`]. `Operator` for a
    /// row written before the column existed, which is what every row was then.
    pub origin: SessionOrigin,
}

/// A live entry as `Registry::assemble` needs it. A named type rather than a seven-tuple: a
/// reader that miscounted would take a title for a session id.
type LiveRow = (
    String,
    String,
    u64,
    Arc<Hub>,
    SessionWiring,
    Option<String>,
    SessionOrigin,
);

/// Where a registry can find sessions it is not already holding. A trait and not a `Store`: a
/// dependency on the store crate here would put SQL in the daemon's session list.
pub trait SessionSource: Send + Sync {
    /// Every session on disk, newest activity first.
    fn list(&self) -> Vec<StoredBrief>;
    /// One session, or `None` if nothing anywhere has heard of it.
    fn lookup(&self, session_id: &str) -> Option<StoredBrief> {
        self.list().into_iter().find(|s| s.session_id == session_id)
    }
    /// The sessions on disk whose parent is `parent_id`. The default filters [`SessionSource::list`];
    /// a source with an index overrides it so a session's kin never costs a walk of the store.
    fn children_of(&self, parent_id: &str) -> Vec<StoredBrief> {
        self.list()
            .into_iter()
            .filter(|s| s.parent_session_id.as_deref() == Some(parent_id))
            .collect()
    }
    /// Make the name durable. An empty title clears it. The default refuses, and refusing is the
    /// point: a registry with no source has nowhere to write it.
    fn set_title(&self, _session_id: &str, _title: &str) -> Result<(), String> {
        Err("this daemon has no store, so a name would not survive it".into())
    }
    /// Make it durable who this session is for. A no-op rather than a refusal, because the answer
    /// is already on the row that created it.
    fn set_origin(&self, _session_id: &str, _origin: SessionOrigin) {}
    /// One session's todo list, for a head's bootstrap read. Empty rather than an error: a source
    /// with no store has no list.
    fn todos(&self, _session_id: &str) -> Vec<crate::event::TodoEntry> {
        Vec::new()
    }
    /// The boards this daemon's store keeps for Claude Code seats (`docs/seat-todo.md`), for the
    /// same bootstrap read. Empty for a source with no store, and for one from before the table.
    fn seat_boards(&self) -> Vec<crate::event::SeatBoard> {
        Vec::new()
    }
    /// The review queue and the merge queue, whole, for a head's bootstrap read. The default SAYS
    /// it has no store rather than answering an empty queue.
    fn queues(&self) -> crate::event::QueueSnapshot {
        crate::event::QueueSnapshot {
            unreadable: Some("this daemon has no session store, so it has no queues".into()),
            ..Default::default()
        }
    }
    /// Which repository `session_id` is a session of - the daemon's own identity rule, so a head
    /// scopes its queue pane and edge to its project. `None` is a source that cannot say.
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
    /// The settings this session runs under, as its harness last published them. A mailbox
    /// because the server thread answers `Settings` and cannot reach the harness; a change is
    /// also fanned out to the attached heads, so a head reads what is running.
    settings: Vec<crate::protocol::SettingRow>,
    /// What this session's harness said about its frozen prefix, or `None` when it had nothing to
    /// say. The sentence and not a flag: only the harness can judge it.
    stale_prefix: Option<String>,
    /// The same finding in the model's own voice: the head's sentence names the session and its
    /// clock, which a model can act on neither of. Set and cleared with `stale_prefix`.
    stale_for_model: Option<String>,
    /// Who this session is for. Set by the door that knows: a head's `NewSession`, the daemon's
    /// queue, or a one-shot.
    origin: SessionOrigin,
}

struct Inner {
    /// Every session, in the order it arrived - the order `list` and `broadcast` walk. Pushed
    /// only through [`Inner::push`] and taken only through [`Inner::take`], which keep
    /// [`Inner::index`] in step.
    entries: Vec<(String, Entry)>,
    /// A session id's position in `entries`, because a linear scan here would run on every hub's
    /// publish path and every head's frame, over a table that only grows. [`Inner::take`] is the one
    /// removal, and it rewrites the positions it shifts.
    index: HashMap<String, usize>,
    /// The session a head that names none gets. The last one anybody *used*, not
    /// the last one created: a head that reattaches after a crash wants the session
    /// it was in.
    default_id: String,
    log_bounds: LogBounds,
    view_bounds: ViewBounds,
    /// Sessions this daemon has SPAWNED and is still opening - see [`Registry::announce_spawn`].
    /// Not entries: a spawning session has no hub, so nothing can attach to it and no command can
    /// reach it; the only question it answers is *is this one alive or has it gone*.
    spawning: HashSet<String>,
    /// Bumped by every change a roster is cut from: a session added, removed, renamed or given
    /// an origin. A cache keyed on it is rebuilt only when the tree can have moved.
    roster_changes: u64,
}

/// Where a registry can read one row's body without holding it. The other half of
/// [`SessionSource`], and a trait for the same reason: no store in the daemon's session path.
pub trait RowSource: Send + Sync {
    /// The body of `row` (a session ordinal) for `session_id`, or `None` when nothing holds it.
    /// Not an empty string for a row that exists and is empty - the two must not look alike.
    fn row_body(&self, session_id: &str, row: usize) -> Option<String>;

    /// Whole rows from `from`, at most `count`, each with the ordinal it was read at, stopping at
    /// the first ordinal this source cannot answer. The ordinal travels with the row because a
    /// source may skip one. Default none, so a registry with no store answers an empty page.
    fn row_items(
        &self,
        _session_id: &str,
        _from: usize,
        _count: usize,
    ) -> Vec<(usize, crate::view::SnapshotItem)> {
        Vec::new()
    }
}

/// Where the oracle's exchange for one decision can be read. A second trait and not a method on
/// [`RowSource`], because the two are read by different halves of a head.
pub trait DiagnosticSource: Send + Sync {
    fn diagnostic(&self, request_id: &str, kind: crate::protocol::DiagnosticKind)
    -> Option<String>;
}

/// Who proposes `!` completions when the history has none. A trait and not a method, for
/// [`SessionSource`]'s reason: the proposer may be a model and the registry has no key.
pub trait ShellSuggester: Send + Sync {
    /// The candidate lines for `prefix` in this session, `!` first, or none. `hub` is the log the
    /// prompt is built from, and `workspace` is where the completion would run.
    fn suggest(&self, hub: &Hub, workspace: &str, prefix: &str) -> Vec<String>;
}

/// The daemon's GitHub reviewer seat (R118), as the wire and the panes reach it. A trait for
/// [`SessionSource`]'s reason: the seat is the harness's, and this crate holds only the door.
pub trait GithubSeat: Send + Sync {
    /// One request - `{"verb": "attach", "scope": "stroppy-io"}` - answered with the snapshot the
    /// seat's pane would be drawn from, or `{"error": …}` when the verb or the scope is refused.
    fn request(&self, op: &serde_json::Value) -> serde_json::Value;

    /// The seat's snapshot as a pane's `github` feed, or `None` when nobody has attached a scope:
    /// a pane handed no key says so, which is a different fact from an empty scope.
    fn feed(&self) -> Option<serde_json::Value>;

    /// Run one poll pass if one is due, on a thread of its own: a review can take minutes and the
    /// daemon's own pass is not where that waits. `false` when nothing was started, which is what a
    /// test's seat answers.
    fn poll_due(&self, _now_ms: u64) -> bool {
        false
    }
}

/// Who answers a plugin pane's questions and lists the panes a head needs before it can open
/// one. A trait for [`SessionSource`]'s reason: the stack belongs to the harness, not here.
pub trait PaneDriver: Send + Sync {
    /// A head attached: say which panes this session has. Called after the `Hello` and the backlog,
    /// so a head that has just arrived is told.
    fn attach(&self, session_id: &str, hub: &Arc<Hub>);

    /// Run this pane now, because a head opened it: the answer is a `PluginPane` event on the
    /// session's log, never a reply to this call.
    fn open(&self, session_id: &str, hub: &Arc<Hub>, pane: &str, size: (u32, u32));

    /// Stop refreshing this pane - the head is no longer drawing it, and this is the only thing
    /// that says so. Idempotent and quiet: a pane this session never had is not an error.
    fn close(&self, session_id: &str, pane: &str);

    /// One key of one stop, back to the pane that drew it - the action word its `event` named, or
    /// `look` to re-run the look. Answered like [`Self::open`].
    fn action(&self, session_id: &str, hub: &Arc<Hub>, pane: &str, item: &str, action: &str);
}

/// Who owns a pane's pty: the program that owns the screen in `!term`. A trait for
/// [`SessionSource`]'s reason, and because a session with no pane answers every call quietly.
pub trait TerminalDriver: Send + Sync {
    /// Start the program in a pty this daemon owns. `command` is the shell line the operator typed
    /// after the verb, `cols`/`rows` the pane's size.
    fn open(
        &self,
        session_id: &str,
        hub: &Arc<Hub>,
        command: &str,
        cols: usize,
        rows: usize,
    ) -> Result<u32, String>;
    /// Give this head the pane the session already has, for a bare `!term`. The screen has to be
    /// the daemon's: a person who closes the pane and reattaches expects their program still there.
    fn attach(
        &self,
        session_id: &str,
        pane: Option<u32>,
        hub: &Arc<Hub>,
        cols: usize,
        rows: usize,
    ) -> Result<u32, String>;
    /// What this session's panes are running, by number, empty for no live pane. A read and not a
    /// notification, because a head that has attached late must be able to ask.
    fn status(&self, session_id: &str) -> Vec<crate::protocol::PaneBrief>;
    /// The operator's keys, verbatim, for one pane. Quietly ignored when there is no such pane.
    fn input(&self, session_id: &str, pane: u32, bytes: &[u8]) -> Result<(), String>;
    /// One pane's rectangle moved.
    fn resize(&self, session_id: &str, pane: u32, cols: usize, rows: usize) -> Result<(), String>;
    /// End the pane: ends its scope, which kills the program and everything it started. Quiet when
    /// there is no pane, because stop is not a request that can fail.
    fn close(&self, session_id: &str, pane: u32) -> Result<(), String>;
    /// End every pane this session holds, and answer how many programs were still running in one.
    /// A pane is that session's work, so its close and its park both come here: a program drawing
    /// into a pane nobody can open is a process nobody can end. `0` for a driver that holds none.
    fn end_session(&self, _session_id: &str) -> usize {
        0
    }
}

/// Who can write to a running command's stdin: the answer to `PromptAnswer` and to `!send`. A
/// trait on the registry for [`Terminal`]'s reason.
pub trait PromptDriver: Send + Sync {
    /// Write one line to this session's own running command. `req` is the id of a card the daemon
    /// raised, and `None` is a line a person typed into the pane.
    fn send(
        &self,
        session_id: &str,
        req: Option<&str>,
        line: &str,
    ) -> Result<Option<String>, String>;

    /// The run this session has in flight, if any: its handle and the command as the person typed
    /// it. A second witness of what the job table holds.
    fn in_flight(&self) -> Option<(String, String)> {
        None
    }
}

/// What a session must do when this daemon is asked to stop. A boxed closure rather than a trait,
/// and that is the layering: the harness supplies it and the registry only rings it.
pub type RunEnder = Arc<dyn Fn() + Send + Sync>;

/// The runs a session has in flight, NAMED AND NOT ENDED: the pure half of the ender, for the
/// announcement a stop makes before it kills anything.
pub type RunNames = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// The boundary a session's `!term` pane runs inside: given the pane's working directory and
/// environment, the argv that goes in front of the operator's line.
pub type PaneBoundary =
    Arc<dyn Fn(&std::path::Path, &[(String, String)]) -> Result<Vec<String>, String> + Send + Sync>;

/// The sentence a head is told when a session that was speaking a stale prompt is moved onto the
/// one this daemon seats. A head that draws the reseat card takes the CODE as the signal; the
/// sentence is for the one that does not.
pub const PREFIX_CURRENT: &str = "this session now speaks the prompt this daemon seats; the \
                                  stale-prompt notice no longer applies";

/// Why a stop interrupts the turns it holds - the reason recorded on the turn it ends, named once
/// because it lands in three reachable places and three copies would be three answers to *why did
/// my turn end*.
pub const STOPPING_TURN_REASON: &str = "the daemon is stopping";

/// A session's standing notes, as rows, computed where the vocabulary is. A closure rather than
/// a mailbox, because the FORM each note takes depends on the budget at the moment of asking.
pub type NotesSource = Arc<dyn Fn() -> Vec<crate::protocol::NoteEntry> + Send + Sync>;

/// A session's skills, as the pane's rows - the same shape and the same reasons as
/// [`NotesSource`]: read on the ask, and a corpus that cannot be read is no rows.
pub type SkillsSource = Arc<dyn Fn() -> Vec<crate::protocol::SkillRow> + Send + Sync>;

/// The operator's chat seat, as ONE HEAD's request reaches it. The head names whose read cursor the
/// request moves: every head speaks as `operator`, so the per-head state is the seat's to hold. See
/// [`Registry::set_room_chat`].
pub type RoomChatAsk = Arc<dyn Fn(&str, serde_json::Value) -> serde_json::Value + Send + Sync>;

/// A head's connection ended, so its per-head chat state may go. See [`RoomChatSeat`].
pub type RoomChatGone = Arc<dyn Fn(&str) + Send + Sync>;

/// One head's connection as the chat fan-out writes to it: the message and whether it names that
/// reader. Held by the connection's own chat worker, so a write cannot outlive its socket.
pub type RoomChatSink = Arc<dyn Fn(serde_json::Value, bool) + Send + Sync>;

/// The daemon's fan-out to the heads that have chatted: `None` is every one of them, `Some(head)`
/// is every one but that head - the author of a message already has its own words.
pub type RoomChatFanout = Arc<dyn Fn(Option<&str>, serde_json::Value, bool) + Send + Sync>;

/// How the daemon hands the fan-out to the seat when it installs it: pushed chat lands at the
/// seat, and the heads' connections are the daemon's, so the two are joined here.
pub type RoomChatAttach = Arc<dyn Fn(RoomChatFanout) + Send + Sync>;

/// The operator's chat seat: `ask` answers one head's request, `gone` retires one head. Both
/// halves arrive together because a head that asks is a head that can leave.
#[derive(Clone)]
pub struct RoomChatSeat {
    pub ask: RoomChatAsk,
    pub gone: RoomChatGone,
    /// Called once, by [`Registry::set_room_chat`].
    pub attach: RoomChatAttach,
}

impl RoomChatSeat {
    /// A seat for a daemon with no heads to push to: a fixture, or a test of the ask alone.
    pub fn new(ask: RoomChatAsk, gone: RoomChatGone) -> RoomChatSeat {
        RoomChatSeat {
            ask,
            gone,
            attach: Arc::new(|_| {}),
        }
    }
}

/// See [`Registry::set_broadcast_observer`].
pub type BroadcastObserver = Arc<dyn Fn(&crate::event::SessionEvent) + Send + Sync>;

pub type JobOutputSource = Arc<dyn Fn(&str, u64) -> Option<crate::SessionEvent> + Send + Sync>;

pub struct Registry {
    inner: Mutex<Inner>,
    bell: Arc<Bell>,
    /// One per session, installed by the daemon at open beside [`Registry::prompts`]. See
    /// [`RunEnder`] for why a stop has to reach the runs and why the daemon owns them.
    runs: Mutex<std::collections::HashMap<String, RunEnder>>,
    /// One per session that has a process host, installed beside [`Registry::runs`]: the same
    /// runs, named and not ended, for the announcement a stop makes before it ends them.
    run_names: Mutex<std::collections::HashMap<String, RunNames>>,
    /// Whether this stop has already said what it is about to end - see
    /// [`Registry::announce_runs_ending`]. One sentence per stop, whichever thread says it.
    announced: std::sync::atomic::AtomicBool,
    /// Set by the first attach this daemon serves, which is the one that opens its eager session.
    first_attach: std::sync::atomic::AtomicBool,
    /// One per session that has a process host, installed by its harness at open beside
    /// [`Registry::runs`]. See [`PaneBoundary`].
    panes: Mutex<std::collections::HashMap<String, PaneBoundary>>,
    /// One per session, installed by the session's own harness at open - the half that owns the
    /// token counter - and removed with the session ([`Registry::remove`]). See [`NotesSource`].
    notes: Mutex<std::collections::HashMap<String, NotesSource>>,
    /// Each session's project notes directory, for the pane to watch while it holds no note.
    notes_dirs: Mutex<std::collections::HashMap<String, String>>,
    /// The `/skills` pane's rows, one source per session - see [`SkillsSource`].
    skills: Mutex<std::collections::HashMap<String, SkillsSource>>,
    /// One per session that has a process host, installed by its harness at open beside
    /// [`Registry::notes`] and removed with the session. See [`JobOutputSource`].
    job_output: Mutex<std::collections::HashMap<String, JobOutputSource>>,
    room_chat: Mutex<Option<RoomChatSeat>>,
    /// The heads whose connection has chatted, and how to write a pushed message to each. A head
    /// that never chatted is absent, which is what keeps a passing reader from being woken.
    chatting: Mutex<std::collections::BTreeMap<String, RoomChatSink>>,
    broadcast_observer: Mutex<Option<BroadcastObserver>>,
    /// The close runs ONCE and every other caller waits for it to finish: the `Stop` frame,
    /// `catch_signals` and a head's own exit all arrive here.
    close_once: std::sync::Once,
    /// Set once at startup by the daemon. `None` in every head and every test that has no store,
    /// and the registry then lists only what it holds.
    source: Mutex<Option<Arc<dyn SessionSource>>>,
    /// Set once at startup by the daemon, beside [`Registry::source`]. See [`RowSource`].
    rows: Mutex<Option<Arc<dyn RowSource>>>,
    /// Set once at startup, beside [`Registry::rows`]. See [`DiagnosticSource`].
    diagnostics: Mutex<Option<Arc<dyn DiagnosticSource>>>,
    /// Set once at startup, beside [`Registry::diagnostics`]. See [`ShellSuggester`]. `None` means
    /// a `SuggestShell` answers with an empty list.
    suggester: Mutex<Option<Arc<dyn ShellSuggester>>>,
    /// Set once at startup by the daemon, beside [`Registry::suggester`]. See [`TerminalDriver`].
    /// `None` means a `TermOpen` answers with a pane that is over and the reason it is.
    terminal: Mutex<Option<Arc<dyn TerminalDriver>>>,
    /// The GitHub reviewer seat (R118), set once at startup by the daemon. See [`GithubSeat`].
    /// `None` means a `Github` frame is refused by name and the pane is handed no `github` feed.
    github: Mutex<Option<Arc<dyn GithubSeat>>>,
    /// One per session, installed by the session's own harness at open and removed with the
    /// session ([`Registry::remove`]). See [`PromptDriver`] for why it is keyed by session.
    prompts: Mutex<std::collections::HashMap<String, Arc<dyn PromptDriver>>>,
    /// One per session that has layers, installed by the daemon at open and removed with the
    /// session. See [`PaneDriver`] for why a plugin pane's questions are answered here.
    pane_drivers: Mutex<std::collections::HashMap<String, Arc<dyn PaneDriver>>>,
    /// Which daemon this is - minted once per registry, and a daemon has exactly one. See
    /// [`Registry::instance`].
    instance: String,
    /// When this registry, and so this daemon, came up, in ms since the epoch.
    started_ms: u64,
    /// See [`Registry::is_stopping`].
    stopping: std::sync::atomic::AtomicBool,
    /// The names of the subagents this daemon spawned, by session id - see
    /// [`Registry::set_slug`].
    slugs: Mutex<std::collections::HashMap<String, String>>,
    /// The sentence this daemon owes every head because its binary is older than the store it
    /// serves, or `None` - the ordinary case. A fact about the PROCESS, so it is set once at
    /// startup and said on every attach: a head that arrives later was not there for the once.
    store_older: Mutex<Option<String>>,
}

/// What the worker was woken for. A second variant rather than a second loop:
/// [`Registry::next_command`] blocks, so a session to OPEN must reach the same wait.
#[allow(clippy::large_enum_variant)] // one queued command per item by value; the small variants are the common case
pub enum Work {
    /// Open this session before anything else happens in it.
    Open(String),
    /// Run this command against this session.
    Command(String, QueuedCommand),
    /// Something inside the daemon woke this session and no head asked for it. A third variant
    /// rather than a synthetic command, because there is no frame to invent.
    Woken(String),
    /// Let go of this session's harness: `CloseSession`, served after the session itself is
    /// already gone, so the worker still has something to do.
    Close(String),
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
        // The metrics writer fills a row's tree root from this: one message per session.
        letibot_metrics::session_parent(&id, entry.parent_session_id.as_deref());
        // The announcement is spent here: a session with an entry is one `get` answers for.
        self.spawning.remove(&id);
        self.roster_changes += 1;
        self.index.insert(id.clone(), self.entries.len());
        self.entries.push((id, entry));
    }

    /// The one way out of `entries`, and the second thing the index has to update. `Vec::remove`
    /// and not `swap_remove`, because the ordinal a head was given must keep its place.
    fn take(&mut self, id: &str) -> Option<Entry> {
        let at = self.index.remove(id)?;
        self.roster_changes += 1;
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
                spawning: HashSet::new(),
                roster_changes: 0,
            }),
            bell: Bell::new(),
            runs: Mutex::new(std::collections::HashMap::new()),
            run_names: Mutex::new(std::collections::HashMap::new()),
            announced: std::sync::atomic::AtomicBool::new(false),
            first_attach: std::sync::atomic::AtomicBool::new(false),
            panes: Mutex::new(std::collections::HashMap::new()),
            notes: Mutex::new(std::collections::HashMap::new()),
            notes_dirs: Mutex::new(std::collections::HashMap::new()),
            skills: Mutex::new(std::collections::HashMap::new()),
            job_output: Mutex::new(std::collections::HashMap::new()),
            room_chat: Mutex::new(None),
            chatting: Mutex::new(std::collections::BTreeMap::new()),
            broadcast_observer: Mutex::new(None),
            close_once: std::sync::Once::new(),
            source: Mutex::new(None),
            rows: Mutex::new(None),
            diagnostics: Mutex::new(None),
            suggester: Mutex::new(None),
            terminal: Mutex::new(None),
            github: Mutex::new(None),
            prompts: Mutex::new(std::collections::HashMap::new()),
            pane_drivers: Mutex::new(std::collections::HashMap::new()),
            instance: mint_instance(),
            started_ms: now_ms(),
            stopping: std::sync::atomic::AtomicBool::new(false),
            slugs: Mutex::new(std::collections::HashMap::new()),
            store_older: Mutex::new(None),
        })
    }

    /// A subagent's name, recorded by the runner that spawned it: what the brief carries for a
    /// session this daemon holds.
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

    /// This daemon's instance id, distinct for every registry ever built, across processes and
    /// within one. It answers whether a stop this daemon heard was its own.
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// When this daemon came up - the end of a previous daemon's down time.
    pub fn started_ms(&self) -> u64 {
        self.started_ms
    }

    /// A registry holding one hub that already exists. The single-session case, which is what
    /// every test that predates this module takes.
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
                    stale_for_model: None,
                    origin: SessionOrigin::Operator,
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

    /// Make a session and hand back its hub. The hub is bound to the bell before it is published
    /// in `entries`, so no wake can arrive for a session nothing can serve.
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
                stale_for_model: None,
                origin: SessionOrigin::Operator,
            },
        );
        drop(g);
        // Wake the worker so it opens this session *now* rather than on the first prompt into
        // it. On the bell's own `opens` queue, so a caller that reads only the command stream
        // sees what it always did, unpolluted by creations.
        self.bell.ring_open(&id);
        Ok(hub)
    }

    /// A hub built exactly as `create_under` builds one and NOT registered, for a session whose
    /// owner opens it itself.
    pub fn new_hub(&self, session_id: impl Into<String>) -> Arc<Hub> {
        let g = self.lock();
        let hub = Hub::with_bounds(session_id.into(), g.log_bounds, g.view_bounds);
        hub.set_bell(self.bell.clone());
        hub
    }

    /// Register a hub that is already open - see [`Registry::new_hub`]. The same refusals as
    /// `create_under`, and no `ring_open`: the owner opened it, and a worker told to open it again
    /// would build a second harness on it.
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
                stale_for_model: None,
                origin: SessionOrigin::Operator,
            },
        );
        Ok(())
    }

    /// Where to find sessions this registry is not holding. See [`SessionSource`].
    pub fn set_source(&self, source: Arc<dyn SessionSource>) {
        *self.source.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    /// Where to find one row's body that this registry's own view no longer holds. See
    /// [`RowSource`]. `None` in every head and every test that predates it, in which case a
    /// `FetchRow` for a trimmed ordinal answers `None`.
    pub fn set_row_source(&self, rows: Arc<dyn RowSource>) {
        *self.rows.lock().unwrap_or_else(|e| e.into_inner()) = Some(rows);
    }

    /// Called once for every daemon-level broadcast, before it reaches the sessions' logs.
    pub fn set_broadcast_observer(&self, observer: BroadcastObserver) {
        *self
            .broadcast_observer
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(observer);
    }

    /// The operator's chat seat: answers a head's `RoomChat` request. `None` on a daemon with no
    /// host coordinator, in which case the reply says so. The seat is handed this daemon's
    /// fan-out before it is installed, because the heads it writes to are the server's.
    pub fn set_room_chat(self: &Arc<Self>, seat: RoomChatSeat) {
        let me = Arc::clone(self);
        (seat.attach)(Arc::new(move |except, message, names_you| {
            me.room_chat_pushed(except, message, names_you)
        }));
        *self.room_chat.lock().unwrap_or_else(|e| e.into_inner()) = Some(seat);
    }

    /// A head's connection began chatting: from here a pushed message reaches it.
    pub fn chat_head(&self, head: &str, sink: RoomChatSink) {
        self.chatting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(head.to_string(), sink);
    }

    /// One message the coordinator pushed to the operator's seat, to every head that has chatted.
    /// The sinks are collected under the lock and called without it: a head's socket is not the
    /// registry's to hold while the others wait.
    pub fn room_chat_pushed(
        &self,
        except: Option<&str>,
        message: serde_json::Value,
        names_you: bool,
    ) {
        let sinks: Vec<RoomChatSink> = self
            .chatting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(head, _)| except != Some(head.as_str()))
            .map(|(_, sink)| Arc::clone(sink))
            .collect();
        for sink in sinks {
            sink(message.clone(), names_you);
        }
    }

    /// Run one operator chat request for `head` through the seat, or say there is none.
    pub fn room_chat(&self, head: &str, op: serde_json::Value) -> serde_json::Value {
        let seat = self
            .room_chat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match seat {
            Some(s) => (s.ask)(head, op),
            None => serde_json::json!({"error": "this daemon has no room chat seat"}),
        }
    }

    /// A head's connection ended: the seat drops whatever it held for it. Called from the
    /// connection's own chat worker, which outlives the reader loop by exactly its last reply.
    pub fn room_chat_gone(&self, head: &str) {
        self.chatting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(head);
        if let Some(seat) = self
            .room_chat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            (seat.gone)(head);
        }
    }

    /// Where the oracle's exchange can be read. See [`DiagnosticSource`].
    pub fn set_diagnostic_source(&self, diagnostics: Arc<dyn DiagnosticSource>) {
        *self.diagnostics.lock().unwrap_or_else(|e| e.into_inner()) = Some(diagnostics);
    }

    /// Who proposes `!` completions when the history has none. See [`ShellSuggester`]. `None`
    /// means a `SuggestShell` answers with an empty list, which the head reads as no suggestion.
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

    /// Who owns a pane's pty. See [`TerminalDriver`]. `None` means a `TermOpen` answers with a
    /// pane that is over and the sentence saying there is no driver, rather than with silence.
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

    /// Who holds the GitHub reviewer seat. See [`GithubSeat`]. Installed once, by the daemon.
    pub fn set_github(&self, seat: Arc<dyn GithubSeat>) {
        *self.github.lock().unwrap_or_else(|e| e.into_inner()) = Some(seat);
    }

    /// The GitHub reviewer seat, or `None` on a daemon that was given none.
    pub fn github(&self) -> Option<Arc<dyn GithubSeat>> {
        self.github
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Install a session's stdin driver. Called by the session's own harness at open - it is the
    /// half that owns the exec host, so it is the half that has the pipe - and never removed: a
    /// session this registry holds lives for the daemon's life. See [`PromptDriver`].
    pub fn set_prompt(&self, session_id: &str, driver: Arc<dyn PromptDriver>) {
        self.prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), driver);
    }

    /// Install this session's plugin-pane driver. Called by the daemon at open - it is the half
    /// that discovers the layers - and removed with the session ([`Registry::remove`]).
    pub fn set_pane_driver(&self, session_id: &str, driver: Arc<dyn PaneDriver>) {
        self.pane_drivers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), driver);
    }

    /// This session's plugin-pane driver, or `None` for a session with no layers. A `PaneOpen`
    /// then answers nothing, which is what `/pane NAME` already knows how to say.
    pub fn pane_driver(&self, session_id: &str) -> Option<Arc<dyn PaneDriver>> {
        self.pane_drivers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }

    /// This session's stdin driver, or `None` for a session that has none. A `PromptAnswer` or a
    /// `SendLine` then answers with a sentence naming the absence rather than silence, which is
    /// `terminal`'s own rule one verb over.
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
        // Cloned out before the call, so the lock is not held across a SQLite read - the same
        // rule `row_body_from_store` keeps one screen up.
        let source = self
            .diagnostics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        source.diagnostic(request_id, kind)
    }

    /// The body of a session ordinal, from the store, or `None` when there is no source or it does
    /// not hold that row. A separate door because it reads past the hub.
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

    /// A page of whole rows from the store, in ordinal order: what a head asks for when it has
    /// scrolled above its own window. Empty when nothing can answer.
    pub fn rows_from_store(
        &self,
        session_id: &str,
        from: usize,
        count: usize,
    ) -> Vec<(usize, crate::view::SnapshotItem)> {
        // Cloned out for `row_body_from_store`'s reason: no lock is held across a SQLite read.
        let source = self.rows.lock().unwrap_or_else(|e| e.into_inner()).clone();
        source
            .map(|s| s.row_items(session_id, from, count))
            .unwrap_or_default()
    }

    fn source(&self) -> Option<Arc<dyn SessionSource>> {
        self.source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// A session that is on disk and not in this daemon, or `None`. `None` for a session that IS
    /// live, deliberately: the caller asks this to decide whether to reopen or to switch.
    pub fn resumable(&self, session_id: &str) -> Option<StoredBrief> {
        if self.get(session_id).is_some() || self.opening(session_id) {
            return None;
        }
        self.source()?.lookup(session_id)
    }

    /// Every session this registry holds a hub for, live ones only - the store's are not here.
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

    /// Say that this daemon has just SPAWNED `session_id`, whose session does not exist yet.
    /// Withdrawn by `spawn_settle` when the open finishes or fails.
    pub fn announce_spawn(&self, session_id: &str) {
        self.lock().spawning.insert(session_id.to_string());
    }

    /// The spawn is over and left no session - the child failed to open, or its thread ended.
    /// Idempotent, and a no-op for one [`Registry::adopt`] has already spent: an announcement that
    /// outlived its spawn would keep a session that does not exist looking alive for ever.
    pub fn spawn_settled(&self, session_id: &str) {
        self.lock().spawning.remove(session_id);
    }

    /// Whether `session_id` is a session this daemon is still opening - see
    /// [`Registry::announce_spawn`]. `false` for one it holds: ask [`Registry::get`] for that.
    pub fn opening(&self, session_id: &str) -> bool {
        self.lock().spawning.contains(session_id)
    }

    /// Resolve what a head asked to attach to. An empty id means whatever this daemon calls
    /// current, which is what a head with no session in mind asks for.
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
        let rows = self.live_rows(|id, _| id == session_id);
        let stored = self
            .source()
            .and_then(|s| s.lookup(session_id))
            .into_iter()
            .collect();
        self.assemble(rows, stored)
            .into_iter()
            .find(|b| b.session_id == session_id)
    }

    /// The children of `session_id` and its `parent`, as picker rows, from keyed reads: the cost
    /// follows the number of kin, never the number of sessions in the store.
    pub fn kin(&self, session_id: &str, parent: Option<&str>) -> Vec<SessionBrief> {
        let mut stored: Vec<StoredBrief> = Vec::new();
        if let Some(src) = self.source() {
            stored.extend(src.children_of(session_id));
            if let Some(p) = parent
                && let Some(row) = src.lookup(p)
            {
                stored.push(row);
            }
        }
        // A live child the store names as one counts too, so it is not listed as unheld.
        let rows = self.live_rows(|id, p| {
            p == Some(session_id) || Some(id) == parent || stored.iter().any(|d| d.session_id == id)
        });
        self.assemble(rows, stored)
    }

    /// What a session is attached to. Empty for a session nobody registered.
    pub fn wiring(&self, session_id: &str) -> SessionWiring {
        self.lock()
            .entry(session_id)
            .map(|e| e.wiring.clone())
            .unwrap_or_default()
    }

    /// The session that spawned this one, from this registry's own entry: `None` for a top-level
    /// session and for one this registry does not hold.
    pub fn parent_of(&self, session_id: &str) -> Option<String> {
        self.lock()
            .entry(session_id)
            .and_then(|e| e.parent_session_id.clone())
    }

    /// A session a session started has that session as its parent: the rule, in the one place it
    /// is decided for every door a session can arrive through.
    pub fn parent_for(
        &self,
        session_id: &str,
        said: Option<String>,
        on_disk: Option<&str>,
    ) -> Option<String> {
        said.or_else(|| self.parent_of(session_id))
            .or_else(|| on_disk.map(str::to_string))
    }

    /// The ids of the sessions this registry holds, and nothing else - no store read (the store's
    /// listing is [`Registry::list`]'s, and it costs a scan). For the merge queue, which rings the
    /// root sessions of a repository whose target went red.
    pub fn held_ids(&self) -> Vec<String> {
        self.lock()
            .entries
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Whether this registry holds the session at all - the other half of `parent_of`'s `None`, for
    /// a caller that must tell *top-level* from *not here*.
    pub fn holds(&self, session_id: &str) -> bool {
        self.lock().has(session_id)
    }

    /// Who this session is for, as this registry knows it: the entry's own answer, and
    /// [`crate::SessionOrigin::Operator`] for a session with no entry.
    pub fn origin_of(&self, session_id: &str) -> SessionOrigin {
        self.lock()
            .entry(session_id)
            .map(|e| e.origin)
            .unwrap_or_default()
    }

    /// The first attach a daemon serves lands on the session it opened eagerly, which was recorded
    /// as the operator's before anybody asked for it. Whoever opens it is who it is for.
    pub fn note_attach(&self, session_id: &str, kind: &str, identity: &str) {
        let first = !self
            .first_attach
            .swap(true, std::sync::atomic::Ordering::SeqCst);
        if first && self.default_id() == session_id {
            self.record_origin(session_id, SessionOrigin::of_opener(kind, identity));
        }
    }

    /// Write down who this session is for: the one door for the fact, in memory and on disk. Two
    /// places because the two questions - what is live and what will come back - are different.
    pub fn record_origin(&self, session_id: &str, origin: SessionOrigin) {
        {
            let mut g = self.lock();
            g.roster_changes += 1;
            if let Some(e) = g.entry_mut(session_id) {
                e.origin = origin;
            }
        }
        if let Some(src) = self.source() {
            src.set_origin(session_id, origin);
        }
    }

    /// The session's title as this registry holds it: the name it was created or adopted with, or
    /// the last one `set_title` wrote.
    pub fn title_of(&self, session_id: &str) -> Option<String> {
        self.lock().entry(session_id).map(|e| e.title.clone())
    }

    /// Forget a session: its entry, its run ender, its prompt driver and its notes source, and
    /// close its hub. The hub is closed first, so no reader sees a half-forgotten session.
    pub fn remove(&self, session_id: &str) -> Option<Arc<Hub>> {
        // The hub is CLOSED before the session stops being visible: `take` is the instant `get`
        // starts answering `None`, and everything that races it must already be closed.
        let hub = self.get(session_id)?;
        hub.close();
        let entry = {
            let mut g = self.lock();
            // A session removed before its open finished is not *still opening*: the spawn that
            // announced it is over, whatever the reason.
            g.spawning.remove(session_id);
            g.take(session_id)?
        };
        // Each map under its own lock, never under the registry's - the ordering `close` keeps.
        let ender = self
            .runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        let names = self
            .run_names
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
        self.notes_dirs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        let skills = self
            .skills
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
        drop((
            ender,
            names,
            prompt,
            notes,
            skills,
            boundary,
            job_output,
            pane_driver,
        ));
        Some(entry.hub)
    }

    /// When this daemon opened this session, or `None` for one it does not hold. An entry lookup
    /// and not `brief`, which builds a whole record to answer one number.
    pub fn created_ms(&self, session_id: &str) -> Option<u64> {
        self.lock().entry(session_id).map(|e| e.created_ms)
    }

    /// Publish the settings a session runs under. See `Entry::settings`.
    /// A changed set is pushed to attached heads. A `model` row that moved is also a warning,
    /// which is history a later head reads.
    pub fn set_settings(&self, session_id: &str, rows: Vec<crate::protocol::SettingRow>) {
        let now = model_row(&rows);
        let (hub, changed, moved) = {
            let mut g = self.lock();
            let Some(e) = g.entry_mut(session_id) else {
                return;
            };
            let was = model_row(&e.settings);
            let changed = e.settings != rows;
            if changed {
                e.settings = rows.clone();
            }
            // The first publish is what a head reads on attaching; only a move is news.
            let moved = match (was, now) {
                (Some(was), Some(now)) if was != now => Some(now),
                _ => None,
            };
            (e.hub.clone(), changed, moved)
        };
        // Only a CHANGE: a flush that republishes what a head already draws must not redraw it.
        if changed {
            hub.push_frame(crate::protocol::ServerFrame::Settings { rows });
        }
        let Some(moved) = moved else {
            return;
        };
        // Nobody to tell, and the publish is what would make the row history.
        if hub.attached_heads() == 0 {
            return;
        }
        hub.warn(crate::protocol::MODEL_SET, moved);
    }

    /// Publish what this session's harness found about its frozen prefix: the sentence an
    /// attaching head is handed, once, and never a flag. `for_model` is the same finding in the
    /// voice the model can act on - it is set and cleared here so the two cannot disagree.
    pub fn set_stale_prefix(
        &self,
        session_id: &str,
        said: Option<String>,
        for_model: Option<String>,
    ) {
        let (hub, was) = {
            let mut g = self.lock();
            match g.entry_mut(session_id) {
                Some(e) => {
                    let was = std::mem::replace(&mut e.stale_prefix, said.clone());
                    e.stale_for_model = for_model;
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
                hub.warn("prefix_stale", said);
            }
            (None, Some(_)) => {
                hub.warn("prefix_current", PREFIX_CURRENT);
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

    /// The same finding as the MODEL is told it: what it can and cannot call, and whose decision
    /// the fix is. `None` with [`Registry::stale_prefix`].
    pub fn stale_for_model(&self, session_id: &str) -> Option<String> {
        let g = self.lock();
        g.entry(session_id).and_then(|e| e.stale_for_model.clone())
    }

    /// Say that this daemon's binary is older than the store it serves: the sentence every head is
    /// told on attach, or `None` when there is nothing to say.
    pub fn set_store_older(&self, said: Option<String>) {
        *self.store_older.lock().unwrap_or_else(|e| e.into_inner()) = said;
    }

    /// What an attaching head must be told, or `None`. See [`Registry::set_store_older`].
    pub fn store_older(&self) -> Option<String> {
        self.store_older
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The jobs a session's harness last published. A mailbox rather than a live read, for the
    /// settings row's reason: the process table belongs to the worker.
    pub fn set_jobs(&self, session_id: &str, jobs: Vec<crate::protocol::JobEntry>) {
        let mut g = self.lock();
        if let Some(e) = g.entry_mut(session_id) {
            e.jobs = jobs;
        }
    }

    /// The monitors a session's harness last published - the job table's mailbox, for the same
    /// reason. Answered beside the jobs, on the same frame.
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

    /// Install a session's standing-notes source. Called by the session's own harness at open - it
    /// is the half that owns the token counter, which decides the form each note has - and removed
    /// with the session ([`Registry::remove`]). See [`NotesSource`].
    pub fn set_notes_source(&self, session_id: &str, source: NotesSource) {
        self.notes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), source);
    }

    /// The `/skills` pane's rows, read NOW from this session's source. Empty when the harness has
    /// not opened, which the pane says rather than hides.
    pub fn skills(&self, session_id: &str) -> Vec<crate::protocol::SkillRow> {
        let source = self
            .skills
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned();
        source.map(|s| s()).unwrap_or_default()
    }

    /// Install a session's skills source - the `/skills` pane's rows, read on the ask. Called by the
    /// session's own harness at open ([`Registry::set_notes_source`]'s shape and reasons).
    pub fn set_skills_source(&self, session_id: &str, source: SkillsSource) {
        self.skills
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), source);
    }

    /// Set the directory this session's notes tool writes into. See [`Registry::notes_dir`].
    pub fn set_notes_dir(&self, session_id: &str, dir: String) {
        self.notes_dirs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), dir);
    }

    /// The session's project notes directory, `None` when it has none.
    pub fn notes_dir(&self, session_id: &str) -> Option<String> {
        self.notes_dirs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }

    /// The standing notes a session's harness reads, one row each, read NOW. A read and not a
    /// mailbox, unlike `jobs`: the notes are the harness's own answer at the moment of asking.
    pub fn notes(&self, session_id: &str) -> Vec<crate::protocol::NoteEntry> {
        // The closure is cloned out before it is called, so the lock is not held across a
        // readdir, a file read and a pass of token counting - the same rule
        // `row_body_from_store` keeps one screen up.
        let source = self
            .notes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned();
        source.map(|s| s()).unwrap_or_default()
    }

    /// Install a session's live job-output read. Called by the session's own harness at open, when
    /// it has a process host. See [`JobOutputSource`].
    pub fn set_job_output_source(&self, session_id: &str, source: JobOutputSource) {
        self.job_output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), source);
    }

    /// One job's output window, read NOW, off the worker - or `None` when this session has no live
    /// source or its host does not hold the job, and the read must be queued for the worker to
    /// answer from the store. See [`JobOutputSource`].
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

    /// Name a session. Empty is allowed and means "no name"; a head then shows the id, which is
    /// the honest fallback.
    pub fn set_title(&self, session_id: &str, title: impl Into<String>) {
        let mut g = self.lock();
        // Counted even when no entry is live: a stored session's name changed too.
        g.roster_changes += 1;
        if let Some(e) = g.entry_mut(session_id) {
            e.title = title.into();
        }
    }

    /// A number that moves whenever a session is added, removed, renamed or re-originated here.
    /// Changes made to the shared store by another daemon do not move it.
    pub fn roster_version(&self) -> u64 {
        self.lock().roster_changes
    }

    /// Name a session and make it durable, through the source: both halves or neither, because
    /// the in-memory title is what a head shows and the row is what survives.
    pub fn rename(&self, session_id: &str, title: &str) -> Result<(), String> {
        let Some(src) = self.source() else {
            return Err("this daemon has no store, so a name would not survive it".into());
        };
        src.set_title(session_id, title)?;
        self.set_title(session_id, title);
        Ok(())
    }

    /// One session's todo list, from the source. Empty when there is no source or no list - see
    /// [`SessionSource::todos`].
    pub fn todos(&self, session_id: &str) -> Vec<crate::event::TodoEntry> {
        self.source()
            .map(|s| s.todos(session_id))
            .unwrap_or_default()
    }

    /// Every Claude Code seat's board the store holds, for the same read - see
    /// [`SessionSource::seat_boards`]. A registry with no source has none, which is what a head
    /// with nothing to draw says.
    pub fn seat_boards(&self) -> Vec<crate::event::SeatBoard> {
        self.source().map(|s| s.seat_boards()).unwrap_or_default()
    }

    /// The queues' snapshot, for a head's pane - see [`SessionSource::queues`]. A registry with no
    /// source says so in the snapshot rather than answering an empty queue.
    pub fn queues(&self) -> crate::event::QueueSnapshot {
        self.source()
            .map(|s| s.queues())
            .unwrap_or_else(|| crate::event::QueueSnapshot {
                unreadable: Some("this daemon has no session store, so it has no queues".into()),
                ..Default::default()
            })
    }

    /// The queues' snapshot for one asking session - [`Registry::queues`] with
    /// [`crate::event::QueueSnapshot::here`] filled from the source, so the head can scope what it
    /// draws to its own repository.
    pub fn queues_for(&self, session_id: &str) -> crate::event::QueueSnapshot {
        let mut snap = self.queues();
        snap.here = self.source().and_then(|s| s.queue_here(session_id));
        snap
    }

    /// Every session's hub, counted, plus the jobs row the registry keeps beside it - for the
    /// daemon's memory census. Collected under the registry lock and counted outside it, in
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

    /// Publish one event to EVERY session's log: the door a daemon-level fact takes. Every other
    /// publish goes to one session's hub.
    pub fn broadcast(&self, event: crate::event::SessionEvent) -> usize {
        // The hubs are collected under the registry lock and published outside it - the ordering
        // `list` and `get` follow: the registry lock is never held while a hub lock is taken, and a
        // publish that blocked a session would otherwise block the whole registry.
        let hubs: Vec<Arc<Hub>> = {
            let g = self.lock();
            g.entries.iter().map(|(_, e)| e.hub.clone()).collect()
        };
        let observer = self
            .broadcast_observer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(observe) = observer {
            observe(&event);
        }
        let mut reached = 0;
        for hub in hubs {
            hub.publish(event.clone());
            reached += 1;
        }
        reached
    }

    /// Every session, in creation order, each cut under its own hub's lock. The hubs are cloned
    /// out from under the registry lock first, so a slow session cannot hold the list.
    pub fn list(&self) -> Vec<SessionBrief> {
        let rows = self.live_rows(|_, _| true);
        let stored: Vec<StoredBrief> = self.source().map(|s| s.list()).unwrap_or_default();
        self.assemble(rows, stored)
    }

    /// The live entries `keep` accepts (called with the id and the entry's parent), cloned out
    /// under one lock.
    fn live_rows(&self, keep: impl Fn(&str, Option<&str>) -> bool) -> Vec<LiveRow> {
        let g = self.lock();
        g.entries
            .iter()
            .filter(|(id, e)| keep(id, e.parent_session_id.as_deref()))
            .map(|(id, e)| {
                (
                    id.clone(),
                    e.title.clone(),
                    e.created_ms,
                    e.hub.clone(),
                    e.wiring.clone(),
                    e.parent_session_id.clone(),
                    e.origin,
                )
            })
            .collect()
    }

    /// Join live rows with the stored rows read for them, then append the stored rows no live
    /// entry holds.
    fn assemble(&self, rows: Vec<LiveRow>, stored: Vec<StoredBrief>) -> Vec<SessionBrief> {
        // Keyed once: a scan per row is quadratic in the store's size.
        let by_id: HashMap<&str, &StoredBrief> =
            stored.iter().map(|d| (d.session_id.as_str(), d)).collect();
        let live_ids: HashSet<String> = rows.iter().map(|r| r.0.clone()).collect();
        let mut out: Vec<SessionBrief> = rows
            .into_iter()
            .map(
                |(session_id, title, created_ms, hub, wiring, parent, origin)| {
                    let on_disk = by_id.get(session_id.as_str()).copied();
                    SessionBrief {
                        // A daemon started with `--title` and a store that already names the session must
                        // not disagree with itself: the store wins for a session it already has.
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
                        // The store's row is the only place the number survives a restart, so it
                        // is the source for a live session too: the hub's view has the same number
                        // while the daemon holds it, and a head gets the same fact either way.
                        context_tokens: on_disk.and_then(|d| d.context_tokens),
                        context_cached: on_disk.and_then(|d| d.context_cached),
                        stored_end: on_disk.and_then(|d| d.stored_end.clone()),
                        // The row is the pause's one home, live or not - the harness writes it the
                        // moment the verb lands, so the store is never behind the session here.
                        nags_paused: on_disk.and_then(|d| d.nags_paused),
                        waits_on_operator: on_disk.map(|d| d.waits_on_operator).unwrap_or(0),
                        // The runner's own record for a child it is running, then the store's for
                        // one a restart brought back.
                        slug: self
                            .slug(&session_id)
                            .or_else(|| on_disk.map(|d| d.slug.clone()))
                            .unwrap_or_default(),
                        // The store wins when both have an answer, the rule `title` above follows
                        // and for the same reason: a session's own row is what survives the daemon
                        // that wrote it.
                        origin: on_disk.map(|d| d.origin).unwrap_or(origin),
                        // When it last moved, off the row - and the creation time only for a
                        // session no store knows about yet, which has moved nowhere.
                        moved_ms: on_disk.map(|d| d.last_activity_ms).unwrap_or(created_ms),
                        session_id,
                    }
                },
            )
            .collect();
        // Then everything on disk this daemon is not holding, appended rather than interleaved by
        // time: the live ones are the ones a switch should reach first.
        for d in stored {
            if live_ids.contains(&d.session_id) {
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
                origin: d.origin,
                moved_ms: d.last_activity_ms,
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

    /// The next thing for the worker to do. Blocks. An open is drained before the command that
    /// woke the same session, because the session must exist before it can be served.
    pub fn next_work(&self) -> Option<Work> {
        match self.next_work_until(None) {
            WorkOrIdle::Work(w) => Some(w),
            // Unreachable without a deadline (see `next_any`), and `Closed` is the
            // `None` this has always returned.
            WorkOrIdle::Idle | WorkOrIdle::Closed => None,
        }
    }

    /// The next thing for the worker to do, giving up at `deadline` and saying so: the worker's
    /// one blocking call that also carries the clock.
    pub fn next_work_until(&self, deadline: Option<Instant>) -> WorkOrIdle {
        loop {
            match self.bell.next_any_until(deadline) {
                RingWait::Closed => return WorkOrIdle::Closed,
                RingWait::Idle => return WorkOrIdle::Idle,
                RingWait::Ring(Ring::Open(id)) => return WorkOrIdle::Work(Work::Open(id)),
                // Not filtered on the registry holding it, unlike the two arms below: the whole
                // point of a close is that it does not any more, and dropping the ring here would
                // drop the harness the worker still owns.
                RingWait::Ring(Ring::Close(id)) => return WorkOrIdle::Work(Work::Close(id)),
                // A wake for a session this registry does not hold is dropped, the same way a command
                // for one is, and it is SAID, because a ring dropped in silence looks like a stuck session.
                RingWait::Ring(Ring::Woken(id)) => {
                    if self.get(&id).is_some() {
                        return WorkOrIdle::Work(Work::Woken(id));
                    }
                    eprintln!(
                        "  {id} · a wake was dropped: no session of that id is held here. A \
                         settlement behind it is still a row with `delivered = 0`, and the \
                         session's next open says it."
                    );
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

    /// Close this session here and let go of it: the registry's half of `CloseSession`. Two acts
    /// in this order, and the order is the whole of it.
    pub fn close_session(&self, session_id: &str) -> bool {
        let held = self.remove(session_id).is_some();
        self.bell.ring_close(session_id);
        held
    }

    /// The next command for the worker, and which session it belongs to. Blocks. A ring whose
    /// command is already drained is skipped rather than answered.
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

    /// Install what this session must end when the daemon stops. Set once per session, at open,
    /// beside the other seams.
    pub fn watch_runs(&self, session: &str, ender: RunEnder) {
        self.runs
            .lock()
            .expect("run enders")
            .insert(session.to_string(), ender);
    }

    /// Install what this session can NAME as in flight, beside its ender and for the announcement.
    /// See [`RunNames`]: the same table read without a kill.
    pub fn name_runs(&self, session: &str, names: RunNames) {
        self.run_names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string(), names);
    }

    /// Every run this daemon is about to end, in sentences - the pure read behind the announcement,
    /// from the two tables that know one: the process host's and `PromptDriver`'s. Neither is a
    /// superset of the other, which is why both are asked.
    pub fn runs_ending(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let reporters: Vec<RunNames> = self
            .run_names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for names in &reporters {
            out.extend(names());
        }
        let drivers: Vec<Arc<dyn PromptDriver>> = self
            .prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for driver in &drivers {
            if let Some((job, command)) = driver.in_flight() {
                out.push(format!("`{command}` (`{job}`)"));
            }
        }
        out
    }

    /// Say what this stop is about to end, before it ends it, and once. `false` when the sentence
    /// has already gone out - the `Stop` frame's thread says it and then `close` does not - or when
    /// nothing is running.
    pub fn announce_runs_ending(&self) -> bool {
        if self
            .announced
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return false;
        }
        let named = self.runs_ending();
        if named.is_empty() {
            return false;
        }
        let hubs: Vec<Arc<Hub>> = self
            .lock()
            .entries
            .iter()
            .map(|(_, e)| e.hub.clone())
            .collect();
        for hub in &hubs {
            for name in &named {
                hub.warn(
                    "daemon_stopping_runs",
                    format!(
                        "the daemon was asked to stop while {name} was running, so it is being \
                         ended with it"
                    ),
                );
            }
        }
        true
    }

    /// Install the boundary this session's `!term` panes run inside. See [`PaneBoundary`]. A second
    /// install replaces the first, as [`Registry::watch_runs`] does: one process host, one boundary.
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

    /// Close every session and the bell: every head wakes with `Bye`, every worker falls out of
    /// `next_command`. Every run still in flight is interrupted first, and named.
    pub fn close(&self) {
        // Said first, before any turn is interrupted: a subagent whose turn the interrupts below
        // end must be able to tell its daemon's stop from a person's (`is_stopping`).
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // The order this must keep is one closer's to keep: every run it ends is named before any
        // hub says `Bye`, and a caller that merely waits cannot break that. See the field.
        self.close_once.call_once(|| self.close_hubs_and_runs());
    }

    /// The body of [`Registry::close`], run by exactly one thread; every other caller waits for it
    /// here and then returns.
    fn close_hubs_and_runs(&self) {
        let hubs: Vec<Arc<Hub>> = self
            .lock()
            .entries
            .iter()
            .map(|(_, e)| e.hub.clone())
            .collect();
        {
            // THE TURNS FIRST, THE RUNS SECOND. A stop queued after the run enders arrives too late:
            // the enders kill the command the turn is in, and the turn then has nothing to report.
            for h in &hubs {
                if h.turn_in_flight() {
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
            // Every run this stop is about to end is NAMED FIRST, before a single kill: the enders
            // answer only once they have killed, and a kill that waits on a name has already lost it.
            self.announce_runs_ending();
            let enders: Vec<(String, RunEnder)> =
                self.runs.lock().expect("run enders").drain().collect();
            for (_session, ender) in &enders {
                // The return value is the runs it ended, which the announcement above has
                // already named: one row per run, before the row could not be sent.
                ender();
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

    /// This daemon has begun to stop: set at the very start of `close`, before the turns in flight
    /// are interrupted, so whoever asks learns the daemon is going rather than stuck.
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Unix millis now, through the tree's one clock: re-exported so this module's callers keep
/// the path they have always used.
pub use letibot_metrics::now_ms;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::{CommandKind, Delivery};
    use crate::protocol::Caps;
    use crate::testing;

    fn reg() -> Arc<Registry> {
        Registry::new()
    }

    fn stored(id: &str, parent: Option<&str>) -> StoredBrief {
        StoredBrief {
            session_id: id.into(),
            title: id.into(),
            items: 1,
            last_activity_ms: 1,
            wiring: SessionWiring::default(),
            parent_session_id: parent.map(Into::into),
            context_tokens: None,
            context_cached: None,
            stored_end: None,
            nags_paused: None,
            waits_on_operator: 0,
            slug: String::new(),
            origin: SessionOrigin::default(),
        }
    }

    /// A source that keys its answers and counts how often the whole store is walked.
    struct Keyed {
        rows: Vec<StoredBrief>,
        lists: std::sync::atomic::AtomicUsize,
    }

    impl SessionSource for Keyed {
        fn list(&self) -> Vec<StoredBrief> {
            self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.rows.clone()
        }
        fn lookup(&self, id: &str) -> Option<StoredBrief> {
            self.rows.iter().find(|r| r.session_id == id).cloned()
        }
        fn children_of(&self, parent: &str) -> Vec<StoredBrief> {
            self.rows
                .iter()
                .filter(|r| r.parent_session_id.as_deref() == Some(parent))
                .cloned()
                .collect()
        }
    }

    /// The per-round kin read and a one-session read ask the source by key and never list it.
    #[test]
    fn kin_and_brief_read_the_source_by_key_and_never_list_it() {
        let r = reg();
        let mut rows: Vec<StoredBrief> = (0..500)
            .map(|i| stored(&format!("s-other-{i}"), None))
            .collect();
        rows.push(stored("s-me", Some("s-mum")));
        rows.push(stored("s-mum", None));
        rows.push(stored("s-kid-1", Some("s-me")));
        rows.push(stored("s-kid-2", Some("s-me")));
        let src = Arc::new(Keyed {
            rows,
            lists: Default::default(),
        });
        r.set_source(src.clone());
        // One kid is live: it must come back once, as live.
        r.create_under("s-kid-1", "", SessionWiring::default(), Some("s-me".into()))
            .unwrap();
        let mut got: Vec<(String, bool)> = r
            .kin("s-me", Some("s-mum"))
            .into_iter()
            .map(|b| (b.session_id, b.live))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("s-kid-1".to_string(), true),
                ("s-kid-2".to_string(), false),
                ("s-mum".to_string(), false),
            ]
        );
        assert_eq!(
            r.brief("s-me").map(|b| b.session_id).as_deref(),
            Some("s-me")
        );
        assert!(r.brief("s-nobody").is_none());
        assert_eq!(
            src.lists.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the whole store was listed"
        );
    }

    /// A moved `model` row, said to the heads already attached. The rows answer a request, so a
    /// switch made from another head (or by the daemon itself) left this one drawing the old name
    /// until its next turn.
    #[test]
    fn a_moved_model_row_is_said_on_the_session_hub() {
        let r = reg();
        r.create("s-1", "", SessionWiring::default()).unwrap();
        let hub = r.get("s-1").expect("the session is held");
        let head = hub.attach("tui", "dead", Caps::default(), 0);
        let row = |v: &str| crate::protocol::SettingRow {
            key: crate::protocol::MODEL_KEY.into(),
            value: v.into(),
            source: String::new(),
            editable: String::new(),
            choices: Vec::new(),
            tools: Vec::new(),
        };
        // A probe of the head's own, so a read has something to answer even when nothing was said.
        // A settings publish rides the side channel ahead of it, and the events are the subject.
        let said = |hub: &Hub| -> Vec<(String, String)> {
            hub.publish(crate::SessionEvent::Warning {
                code: "prefix_current".into(),
                detail: String::new(),
                compaction: None,
            });
            let batch = loop {
                match hub.next_batch(&head.head_id, 64) {
                    Delivery::Events(batch) => break batch,
                    Delivery::Frames(_) => {}
                    other => panic!("a batch, not a {other:?}"),
                }
            };
            batch
                .events()
                .iter()
                .filter_map(|e| match &e.event {
                    crate::SessionEvent::Warning { code, detail, .. } => {
                        Some((code.clone(), detail.clone()))
                    }
                    _ => None,
                })
                .collect()
        };

        // The first publish is what a head reads when it asks for the rows: not news.
        r.set_settings("s-1", vec![row("local (qwen-3.8-27b)")]);
        assert_eq!(
            said(&hub),
            vec![("prefix_current".to_string(), String::new())]
        );

        // The switch itself. The world spelling is the row's, and the head strips the alias.
        r.set_settings("s-1", vec![row("deepseek/deepseek-flash")]);
        assert!(
            said(&hub).contains(&("model_set".into(), "deepseek/deepseek-flash".into())),
            "the move reached nobody"
        );

        // A republish of the same rows - every `/config` - is not a move and says nothing.
        r.set_settings("s-1", vec![row("deepseek/deepseek-flash")]);
        assert_eq!(
            said(&hub),
            vec![("prefix_current".to_string(), String::new())]
        );

        // With no head on the session there is nobody to tell, and the switch is not made history.
        hub.detach(&head.head_id);
        r.set_settings("s-1", vec![row("grok/grok-4")]);
        let late = hub.attach("tui", "later", Caps::default(), 0);
        let snapshot = late.snapshot.expect("a fresh head gets a snapshot");
        let moved: Vec<&str> = snapshot
            .warnings
            .iter()
            .filter(|w| w.code == crate::protocol::MODEL_SET)
            .map(|w| w.detail.as_str())
            .collect();
        assert_eq!(
            moved,
            vec!["deepseek/deepseek-flash"],
            "only the move a head was there for is a warning at all"
        );
    }

    /// An operator run in flight when the registry closes is named, even though the prompts and
    /// the host's ender are two different tables.
    #[test]
    fn an_operator_run_in_flight_is_named_when_the_registry_closes() {
        /// The operator's own run, as the `!send` door knows it: one run in flight, and nothing else
        /// this fixture needs.
        struct ARun(String, String);
        impl PromptDriver for ARun {
            fn send(
                &self,
                _s: &str,
                _r: Option<&str>,
                _line: &str,
            ) -> Result<Option<String>, String> {
                Err("nothing is running in this fixture".into())
            }
            fn in_flight(&self) -> Option<(String, String)> {
                Some((self.0.clone(), self.1.clone()))
            }
        }

        let r = reg();
        r.create("s-1", "", SessionWiring::default()).unwrap();
        let hub = r.get("s-1").expect("the session is held");
        let head = hub.attach("tui", "dead", Caps::default(), 0);
        r.set_prompt("s-1", Arc::new(ARun("j1".into(), "/bin/cat".into())));
        // The ender says nothing at all: the run is not in the table the host knows.
        r.watch_runs("s-1", Arc::new(|| {}));

        r.close();

        let codes: Vec<String> = std::iter::from_fn(|| match hub.next_batch(&head.head_id, 64) {
            Delivery::Events(batch) => Some(
                batch
                    .events()
                    .iter()
                    .filter_map(|env| match &env.event {
                        crate::event::SessionEvent::Warning { code, detail, .. } => {
                            Some(format!("{code}: {detail}"))
                        }
                        _ => None,
                    })
                    .collect::<Vec<String>>(),
            ),
            Delivery::Resync { .. } | Delivery::Frames(_) => Some(Vec::new()),
            Delivery::Closed => None,
        })
        .flatten()
        .collect();
        assert_eq!(codes.len(), 1, "one row for one run: {codes:?}");
        assert!(
            codes[0].starts_with("daemon_stopping_runs: "),
            "the code is the register's: {codes:?}"
        );
        assert!(
            codes[0].contains("/bin/cat") && codes[0].contains("j1"),
            "it names the command the person typed and the handle: {codes:?}"
        );
        assert!(
            codes[0].contains("stop") && codes[0].contains("ended"),
            "what happened and why: {codes:?}"
        );
    }

    /// A stop names the run it is about to end, before the kill and before any hub closes, even
    /// when a second caller asks for the stop that is already running.
    #[test]
    fn a_run_the_stop_ends_is_named_before_the_hubs_close() {
        let r = reg();
        r.create("s-1", "", SessionWiring::default()).unwrap();
        let hub = r.get("s-1").expect("the session is held");
        let head = hub.attach("tui", "dead", Caps::default(), 0);
        // The pure half says what is running; the ender takes a beat over it and says nothing.
        r.name_runs("s-1", Arc::new(|| vec!["`/bin/cat` (`j1`)".to_string()]));
        r.watch_runs(
            "s-1",
            Arc::new(|| std::thread::sleep(std::time::Duration::from_millis(200))),
        );

        // A head's writer thread, from the schema's own side: a loop that stops at the first
        // `Closed`, which is what the server does with a dead socket.
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let reader = {
            let (hub, head_id, seen) = (Arc::clone(&hub), head.head_id.clone(), Arc::clone(&seen));
            std::thread::spawn(move || {
                loop {
                    match hub.next_batch(&head_id, 64) {
                        Delivery::Events(batch) => {
                            for env in batch.events() {
                                if let crate::event::SessionEvent::Warning { code, .. } = &env.event
                                {
                                    seen.lock().unwrap().push(code.clone());
                                }
                            }
                        }
                        Delivery::Resync { .. } | Delivery::Frames(_) => continue,
                        Delivery::Closed => return,
                    }
                }
            })
        };

        // The winner, and a second caller that arrives while it is inside the ender.
        let loser = {
            let r = Arc::clone(&r);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                r.close();
            })
        };
        r.close();
        loser.join().expect("the second caller returns");
        reader.join().expect("the reader stops at the close");

        let codes = seen.lock().unwrap().clone();
        assert_eq!(
            codes,
            ["daemon_stopping_runs"],
            "the run the stop ended must be named, and named before the hub closes: {codes:?}"
        );
    }

    /// The rule, in the one place it is decided: a session a session started has that session as
    /// its parent, whichever of the three ways it arrived.
    #[test]
    fn a_parent_comes_from_whichever_source_knows_and_is_never_invented() {
        let r = reg();
        // (1) The caller's word, for a session with neither an entry nor a row yet.
        assert_eq!(
            r.parent_for("s-new-sub-1", Some("s-new".into()), None),
            Some("s-new".to_string())
        );
        // (2) The registry's entry - the door that created it.
        r.create_under(
            "s-entry-sub-1",
            "",
            SessionWiring::default(),
            Some("s-entry".into()),
        )
        .unwrap();
        assert_eq!(
            r.parent_for("s-entry-sub-1", None, None),
            Some("s-entry".to_string())
        );
        // (3) The session's own row, for an id this registry does not hold at all.
        assert_eq!(
            r.parent_for("s-rowed-sub-1", None, Some("s-rowed")),
            Some("s-rowed".to_string())
        );
        // The corollary: a session a person started - a head's `NewSession`, a launch, a
        // `--continue` - has no parent, whatever is current at the time.
        r.create("s-mine-sub-3", "", SessionWiring::default())
            .unwrap();
        assert_eq!(r.parent_for("s-mine-sub-3", None, None), None);
        // And the caller's word wins when a session is re-registered - a parked child reopened by
        // its parent is the case: the entry may be gone, the row may say something else.
        assert_eq!(
            r.parent_for("s-rowed-sub-1", Some("s-reopened".into()), Some("s-rowed")),
            Some("s-reopened".to_string())
        );
    }

    /// A subagent's hub is built first and registered only once its owner has opened it - so nothing
    /// can switch into it early, and the worker is never told to open a session somebody else is
    /// opening.
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

    /// A spawned session is alive BEFORE it is open, and only while it is: the child's handle
    /// exists and is returned before its own thread adopts the hub.
    #[test]
    fn a_spawn_is_alive_before_its_session_exists_and_not_after_the_spawn_ends() {
        let r = reg();
        r.create("s-parent", "", SessionWiring::default()).unwrap();
        r.announce_spawn("s-parent-sub-1");
        assert!(
            r.get("s-parent-sub-1").is_none(),
            "no hub yet: nobody may attach to it"
        );
        assert!(r.opening("s-parent-sub-1"), "but it has not gone either");

        // The open spends the announcement, and the entry is what answers from then on.
        let hub = r.new_hub("s-parent-sub-1");
        r.adopt(
            hub.clone(),
            "find the bug",
            SessionWiring::default(),
            Some("s-parent".into()),
        )
        .unwrap();
        assert!(!r.opening("s-parent-sub-1"), "an open one is not opening");
        assert!(Arc::ptr_eq(&r.get("s-parent-sub-1").unwrap(), &hub));

        // A spawn that ends without a session is not alive - withdrawing twice is not an error,
        // because `adopt` has usually done it already (the child thread's guard runs after).
        r.announce_spawn("s-parent-sub-2");
        r.spawn_settled("s-parent-sub-2");
        r.spawn_settled("s-parent-sub-2");
        assert!(!r.opening("s-parent-sub-2") && r.get("s-parent-sub-2").is_none());
        // And an ordinary session was never announced as anything.
        assert!(!r.opening("s-parent"));
        // Removing a session does not leave its announcement behind either.
        r.announce_spawn("s-parent-sub-3");
        r.remove("s-parent-sub-1").expect("the child is held");
        assert!(!r.opening("s-parent-sub-1"));
        assert!(
            r.opening("s-parent-sub-3"),
            "a live announcement survives another's removal"
        );
    }

    /// A session served by its own thread is never served by the worker. Without the owner on the
    /// hub, a head prompting a subagent would race that subagent's own thread.
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

    /// A removed session is gone from every table, its hub is closed, and the sessions after it are
    /// still found by id - the index is rewritten for every position the removal shifted, and `list`
    /// keeps arrival order.
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

    /// `parent_of` answers from the entry, and does not need a source at all - which is the point:
    /// the other lookup is `list()`, a read of every stored session.
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

    /// A stop interrupts the turns it holds, and starts none: the state this measures is the one
    /// that costs a daemon its folder's files.
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

    /// A broadcast reaches every session's log, and only the log: a head attached to any of them
    /// can open the card it announces.
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

        // Every one of them really holds it, read back through the hub's own retained log.
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

        // And no wake was rung. A broadcast that started a turn in every session would be a queue
        // move that costs a generation per session, which is the cost this door exists to bound.
        // The bell is drained and found empty.
        r.bell().close();
        assert!(
            r.bell().next().is_none(),
            "a broadcast must not wake anybody"
        );
    }

    #[test]
    fn an_unknown_session_is_none_rather_than_the_default_one() {
        // A typo that seats you in somebody else's session is worse than a typo that fails.
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

        // Arrival order across sessions: the same promise two heads on one session already get.
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

    /// A take-back dequeues the whole queued run in one go: the head put both messages back in the
    /// composer in one press, so the queue drains as a unit and never a message at a time. What it
    /// could not have taken is anything that arrives behind the withdraw - the next test.
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
        // Alice's prompts are gone - both of them - and bob's is not alice's to take back.
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

    /// A prompt typed after the take-back is not one of the prompts it names: the take-back
    /// recalls what is queued, and its point is what was there when it was asked.
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
        // Up - the recall itself
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
        // And it stops being busy when the turn ends, rather than staying busy because only
        // `TurnFinished` was ever taught to clear it.
        a.publish(crate::SessionEvent::TurnFailed {
            turn_id: "t1".into(),
            error: "the model called tools 12 times without answering".into(),
            partial_kept: false,
        });
        assert!(!r.list()[0].status.running);
    }

    /// A wake with no command behind it reaches the worker: `Bell::ring` is not it, because
    /// `next_work` skips a session whose command queue is empty.
    #[test]
    fn a_wake_with_no_command_behind_it_still_reaches_the_worker() {
        let r = reg();
        r.create("s-a", "", SessionWiring::default()).unwrap();
        // `create` rings an open, and an open is drained first (a session whose resume has not run
        // must not serve a command). Take it so the assertion below is about the wake.
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
        r.bell().ring_wake("s-a");
        match r.next_work() {
            Some(Work::Woken(id)) => assert_eq!(id, "s-a"),
            _ => panic!("a wake must be work"),
        }
    }

    /// A head that pressed enter is waiting; a monitor is not. Wakes are drained last, and this is
    /// asserted rather than left to the queue order in `next_any`, because that order is a policy
    /// and a reader of the three `pop_front`s cannot tell a policy from an accident.
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

    /// Wakes for one session coalesce while they wait - one `Harness::wake` drains everything the
    /// session owns, so a hundred rings before the worker gets there are one pass, not a hundred
    /// passes over the store. Another session's wake keeps its own place.
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

    /// The idle clock is not starved by wakes: a due deadline waits behind at most
    /// [`WAKES_AHEAD_OF_A_DUE_CLOCK`] wakes, is answered `Idle`, and the wake after it is served on
    /// the next call. A command still goes first - somebody is waiting on it.
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

    /// A wake for a session this registry does not hold is dropped, the same way a command for one
    /// is. The session is gone; there is nothing to wake.
    #[test]
    fn a_wake_for_an_unknown_session_does_not_stall_the_worker() {
        let r = reg();
        r.create("s-a", "", SessionWiring::default()).unwrap();
        // `create` rings an open, and an open is drained first (a session whose resume has not run
        // must not serve a command). Take it so the assertion below is about the wake.
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

    /// The notes source is read on the ASK, not remembered, and that is the one thing that
    /// separates it from the job table's mailbox: a note written a second ago must be offered.
    #[test]
    fn the_notes_source_is_read_on_the_ask_and_a_session_without_one_has_none() {
        let r = reg();
        r.create("s1", "", SessionWiring::default()).unwrap();
        // A session whose harness has not opened has no source, and that is no rows - the same
        // answer an empty corpus gives, which the pane says out loud.
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

    /// A wake is not one queue entry. A stopped or failed turn takes back the wake a settlement
    /// rang, because the settlement is still queued for the next turn; a monitor's firing has no
    /// queue behind it, so a drop that takes that back is a firing the session never reads.
    #[test]
    fn a_monitors_wake_survives_the_drop_a_stopped_turn_makes() {
        let r = reg();
        // A session the registry holds: a wake naming one it does not hold is refused by the
        // worker's own door, which is a different absence from the one this test is about.
        r.create("s-1", "", SessionWiring::default()).unwrap();
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
        let bell = Arc::clone(r.bell());
        bell.ring_wake("s-1");
        bell.ring_wake("s-1");
        bell.drop_wakes("s-1");
        assert!(
            matches!(
                r.next_work_until(Some(Instant::now() + std::time::Duration::from_millis(200))),
                WorkOrIdle::Work(Work::Woken(id)) if id == "s-1"
            ),
            "nothing rung this session has to hear is left: the drop took a fact no queue holds"
        );
    }
}

#[cfg(test)]
mod stored_end_wire {
    use super::*;

    /// A brief from a daemon that predates `stored_end` still decodes, as `None` - which is why the
    /// field needed no protocol bump.
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
            origin: crate::SessionOrigin::Operator,
            moved_ms: 0,
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

    /// A child's name rides the list - the runner's record for a session this daemon holds - and a
    /// brief from a daemon that predates the field decodes with no name.
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
