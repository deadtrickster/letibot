//! The loop: submit, stream, parse a tool call, execute it, append the result, resubmit.
//!
//! ```text
//!   user item ──append──> ledger ──submit──> stream ──parse──> items
//!        ▲                                                       │
//!        │                                              tool calls?
//!        │                                                 │        └── no ──> reply
//!   ToolResult item <──transcript_item── ToolRuntime::invoke ── yes
//! ```
//!
//! Three rules the loop cannot break. Item ids are minted by two different rules, so
//! [`CapturingSink`] reads the id off the event rather than reimplementing either. A failed turn
//! commits nothing, so the salvage notice goes in as a plain user item, the only append-only place
//! it fits. And a tool batch is executed and appended in call order, because GLM's shipped template
//! sorts tool results by call order and any other order would rewrite bytes already in the KV cache.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use letibot_dialect::StablePrefix;
use letibot_sessionlog::event::{TodoEntry, TodoStatus as WireTodoStatus};
use letibot_sessionlog::hub::{CommandKind, Hub};
use letibot_sessionlog::{LogSink, SessionEvent, ToolLogSink};
use letibot_tokencore::store::TodoItem;
use letibot_tokencore::store::{
    JobRecord, SessionRecord, StablePrefixRecord, Store, StoredSession, TodoStatus,
};
use letibot_tokencore::{Vocab, ledger::hex as hex32};
// The same hash `letibot_tokencore::store` content-addresses a stable prefix with, taken
// directly rather than through a helper there: the fingerprint is over the FROZEN text,
// and only this crate knows what the live half of the prompt is.
use letibot_tools::authorise::{
    AuthorisationTrail, BreakerState, DenialNotice, DenialSink, Reach, Speaker, Utterance,
};
use letibot_tools::builtins::intent::{self as intent_tools, IntentLedger, IntentSink};
use letibot_tools::builtins::todo::{TodoBoard, unfinished_plan, unfinished_plan_for};
use letibot_tools::exec::monitor::Monitors;
use letibot_tools::{
    AdjudicatedGate, Adjudicator, Gate, GateCall, HostBackend, NoBoundary, Registry, Role, Tool,
    ToolRuntime, roles,
};
use letibot_transcript::{SystemOrigin, ToolCall, TranscriptItem, UserPart};
use letibot_turn::{
    CompactionOutcome, Endpoint, EventSink, Harvest, OverrunPlan, Session, SteeringMessage,
    SteeringSource, TailSplit, TurnEngine, TurnEvent, TurnFailure, TurnMetrics, TurnOk,
    plan_compaction_tail, plan_cut, plan_fold, plan_overrun, plan_tail, run_compaction,
    summarise_first_half, summarise_overrun, tail_because, tail_budget, tail_split_of,
};
use sha2::{Digest, Sha256};

use crate::config::{AdjudicatorChoice, Config, GateWiring, Seat, SpillPolicy, SpillStorage};
use crate::dialect::Wiring;
use crate::jobwatch::{BackgroundKind, JobCompletion, JobWatchSink, JobWatchers};
// `is_writable` is a trait method; the backend's own answer is only reachable
// with the trait in scope.
use letibot_tools::ExecBackend as _;
// Same reason: `ProcessHost::monitors` is the trait's, and a backend's monitors
// are only reachable with it in scope.
use letibot_tools::ProcessHost as _;

/// How many of the operator's own utterances the trail carries. The bound is small because the
/// sentence that authorised an action is nearly always the last one or two, and the denominator
/// travels separately, so a short trail cannot read as a complete one.
const TRAIL_UTTERANCES: usize = 12;

/// How many relatives' boards a brief is scored against, and how many rows in all: the corpus is
/// read once a round, and a tree of twenty children is not a corpus worth the wait.
const KIN_MAX: usize = 8;
const ROWS_MAX: usize = 48;

/// How far back the walk goes, and how many utterances the trail holds: the only reader takes the
/// last `TRAIL_SCAN`, so anything older was held for the life of the session and never read.
pub(crate) const TRAIL_SCAN: usize = 60;

/// The longest an utterance travels verbatim, matching `Utterance::operator`'s own rule: a
/// truncated authorisation that reads as complete is worse than a missing one, so a clipped one
/// says it was clipped. It is also how much is stored.
pub(crate) const UTTERANCE_CHARS: usize = 600;

/// How many ledger rows a resume announces between progress ticks: a tick per row overflowed a
/// head's queue and demoted it, so the restore's own progress bar was killed by the events carrying it.
const FILLING_STRIDE: usize = 64;

/// How many transcript rows one store transaction writes, and it is the store's own chunk: one
/// chunk here is exactly one `BEGIN IMMEDIATE`, so a committed chunk, a tick of the carry's
/// `Filling` line and the `persisted` count all mean the same rows.
const PERSIST_CHUNK: usize = letibot_tokencore::store::Store::APPEND_CHUNK;

/// Which question a compaction door is answering. Two questions share `Config::at_the_wall`'s
/// arithmetic and differ in exactly one gate, which is why they are one body with a parameter rather
/// than two functions with copied thresholds: `Tidy` reads `auto_compact`, `Refused` never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WallWhy {
    Tidy,
    Refused,
}

/// What one user turn produced.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The visible text of the last generation. Empty is a legitimate answer for a
    /// turn that only called tools and then stopped, and it is reported as empty
    /// rather than as the tool output.
    pub text: String,
    /// One entry per submission - so a turn that called three tools has four.
    pub metrics: Vec<TurnMetrics>,
    pub rounds: usize,
    pub tool_calls: usize,
    /// The output was cut short but is usable. Never folded into success.
    pub truncated: bool,
}

impl Reply {
    /// The `f_keep` (`cached / what the previous turn left in the cache`) for every submission this
    /// reply cost, shorter than `metrics` when a submission had no cached entry: those are dropped
    /// rather than reported as 0.0 or 1.0.
    pub fn f_keep(&self) -> Vec<f64> {
        self.metrics.iter().filter_map(|m| m.f_keep()).collect()
    }

    /// `f_sim` - cached over *this* prompt, per submission. Falls as the
    /// conversation grows; see `TurnMetrics::f_sim`. Never compare it to a
    /// `f_keep` threshold.
    pub fn f_sim(&self) -> Vec<f64> {
        self.metrics.iter().filter_map(|m| m.f_sim()).collect()
    }
}

/// The warning code for a session whose workspace no longer exists.
const WORKSPACE_GONE: &str = "workspace_gone";

/// The warning code for a `[skills] pin` naming nothing this box holds: said at open, once, so a
/// typo does not read as a pin that took effect.
pub(crate) const SKILLS_PIN: &str = "skills_pin";

/// The warning code for a session that asked for a boundary and got a whole-host shell, said once at
/// open: the consequence is invisible from the inside, since a session with no boundary asks about
/// every `cargo test`. macOS only, because on Linux an unbuildable boundary is a refusal.
#[cfg(target_os = "macos")]
const BOUNDARY_ABSENT: &str = "boundary_absent";

/// The warning code for a session whose tools run on another machine - where its tree is,
/// the branch that comes back, and what stayed here (`docs/remote-hosts.md`).
const REMOTE_PLACEMENT: &str = "remote_placement";

/// "workspace gone", in one sentence - said when a session opens or wakes on a workspace that
/// no longer exists.
fn workspace_gone(workspace: &std::path::Path) -> String {
    format!(
        "workspace gone: `{}` no longer exists (a retired worktree, or a checkout moved or \
         removed), so nothing this session runs can work there",
        workspace.display()
    )
}

/// One session: the engine, the transcript, the tools, the log and the store.
pub struct Harness {
    cfg: Config,
    /// The right to append to this session's transcript, claimed at open and given back on drop. A field
    /// because the claim is a LIFETIME and not a permission check: it is what a second process's open is
    /// refused by. `None` with no store, and for a session whose live copy is elsewhere.
    _writer: Option<crate::writer::Claim>,
    /// The daemon's mode rows, which a `/mode` taken by a running turn writes
    /// (`apply_queued_mode`, through [`crate::modes::record`]).
    mode_store: std::sync::Arc<std::sync::RwLock<crate::modes::ModeStore>>,
    /// The worktree and branch this session's work must end up on, for a `task_start` child, and `None`
    /// for every other session. Read as a turn ends, so a dirty tree is told once and the turn goes on.
    /// Set by the spawn, because the placement is the runner's fact.
    deliverable: Option<deliverable::Deliverable>,
    /// When the prompt this turn is serving arrived, carried across its rounds: `run_turn_steered` runs
    /// INSIDE the round loop, so the `TurnStarted` a head times from fires once per round and the clock
    /// restarted. A turn is one prompt, so its start belongs to the prompt and not to a round.
    turn_began_ms: Option<u64>,
    /// Whether a PROMPT is in flight: the span the two clock functions bracket. The activity's `Turn` is
    /// deliberately the same span, because the engine's events fire per round and a turn derived from
    /// them would drop to quiet at every boundary. Set and cleared in those two functions alone.
    prompt_in_flight: Arc<AtomicBool>,
    /// Whether the shared tail is running a turn on this harness: the gate on `begin_turn_clock`, and
    /// why that is not a clock in its own right. `system_update` reaches `run_rounds` outside any turn,
    /// and a clock started there would never be cleared.
    turn_open: bool,
    /// The server thread answers the head's `Settings` from the registry, so
    /// what is running is what the pane shows.
    session_registry: Arc<letibot_sessionlog::registry::Registry>,
    /// The daemon's subagent journal, which is how a plan knows a child is still running: whether a
    /// child is running is not a fact any board holds. Shared, because it is the DAEMON's record, so a
    /// child this session started and one a peer started are one fact.
    tasks: Arc<crate::tasks::TaskJournal>,
    /// Where the mode came from, for the settings row: the project store or
    /// the daemon's flag/default. Decided at open and not kept by `Config`.
    mode_source: String,
    engine: TurnEngine,
    /// The counter this session's standing notes are measured with: a field rather than a read of
    /// `self.engine`, because the notes pane reads on the server's thread. It tracks the engine wherever
    /// the engine moves, so a session that changed weights measures notes it now speaks.
    note_vocab: Arc<Vocab>,
    /// The skills this daemon loaded, which the prompt's index is composed from and the page's
    /// rows are read off - the same `Arc` the `skill` tool and the dashboard hold, so a pane and a
    /// prompt cannot list two different corpora.
    skills: Arc<letibot_tools::builtins::skill::SkillRegistry>,
    session: Session,
    /// Shared, because the operator's run does not run on this harness's thread: a `!` line is handed to
    /// a thread of its own and invokes through THIS runtime, since one exec host per session is a fact.
    /// The gate sits behind its own mutex for exactly this reason.
    runtime: std::sync::Arc<ToolRuntime>,
    hub: Arc<Hub>,
    store: Option<Store>,
    /// The corpus sink, kept beside the gate's own copy: an operator's own call is not a decision the
    /// gate took, so nothing in the gate would write it, and it must still land in the same corpus.
    corpus: Option<std::sync::Arc<dyn letibot_tools::CorpusSink>>,
    transcript_id: String,
    /// The stable prefix this transcript was built with: on a resume the session's own, not the one
    /// this daemon would render now. Compaction forks with it for the same reason a resume keeps it,
    /// since rewriting message 0 forces a full cold re-prefill.
    prefix: StablePrefix,
    /// The store's content address for [`Harness::prefix`]. Empty when there is no
    /// store, and never read in that case - compaction is refused before the id
    /// would be used.
    prefix_id: String,
    /// How many ledger rows have reached the store.
    persisted: usize,
    system_updates: u64,
    /// The turn this harness last started. A failure has to name one.
    last_turn_id: String,
    /// Read at open time from the backend, the gate and the seated schemas, so the
    /// adjudication disclosure is a reading rather than a claim. See [`GateWiring`].
    wiring: GateWiring,
    /// True only while [`Harness::compact`] is running its summary turn. See the
    /// context-wall check in `run_rounds` for why that turn must be exempt.
    compacting: bool,
    /// Where the abandoned-call sweep stopped last time, so it reads the turn that was added rather
    /// than the whole conversation after every turn.
    swept_calls: crate::harness::turns::SweptCalls,
    /// Where the provider last counted this conversation: the anchor the wall's resident count is
    /// rolled forward from, with `cfg.ledger_scale` as the pair `measure_resident` computed. `None`
    /// with a scale set is one this session did not measure, and it keeps the old ratio reading.
    wall_anchor: Option<WallAnchor>,
    /// How this daemon RENDERS a prompt: the dialect's template and the tool-schema serialiser. One
    /// renderer, so the prefix a re-seat writes and the prefix a fresh session writes cannot drift.
    render: std::sync::Arc<Wiring>,
    /// The prerequisites this session's wiring supplies and the classes it seats, read once at open.
    /// What `/mode` checks a new point against, so a move the open would have refused is refused the
    /// same way rather than taken and then failing on its first call.
    supplies: Option<(Vec<letibot_tools::mode::Prereq>, letibot_tools::mode::Seats)>,
    /// `Some` when this harness was rebuilt from the store rather than opened fresh.
    /// The daemon prints it; a head is told through the log, by the rows themselves.
    resumed: Option<ResumeReport>,
    /// What `open` decided that the operator would otherwise learn a turn later, for a session that
    /// was NOT resumed. A one-shot started `--mode allow-all` ran at `writes allowed` because the
    /// project store said so, and the note saying so went into a report nobody printed.
    open_notes: Vec<String>,
    /// A title this harness derived and the daemon has not yet published. See
    /// [`Harness::take_new_title`].
    new_title: Option<String>,
    /// The `!` sequence: how many of the operator's own shell lines this session
    /// has run. The `call_id` of each is `bang-<n>`, minted here because - unlike the
    /// door - no head chooses one: the head sends the line and the daemon owns the run.
    bang_seq: u64,

    /// The worker's serialization state for this session's operator runs: whether one is in flight on
    /// its own thread, and the lines parked behind it. The same `Arc` the dispatch arm reads, so the
    /// round boundary's pickup and the worker's settle arm answer one question from one place.
    bang: Option<std::sync::Arc<crate::bangrun::State>>,

    /// Who said what, so the gate's adjudicator can see what authorised an action.
    /// Fed at every append, because the provenance is only knowable there.
    trail: Arc<TrailMirror>,
    /// What this session's harness HOLDS, restated at every append. Atomics, because the reader is the
    /// memory watchdog's thread and the writer is this one: a lock here would be a lock a turn holds,
    /// taken from the census.
    footprint_gauges: Arc<crate::footprint::Gauges>,
    /// The last question this session asked a person, and the answer it got: filled by the session's
    /// own `HeadQuestioner` and read back by the round loop, which is the one place a tool result is
    /// appended. The person's half is written there and not by the tool.
    asked: crate::answers::AskLog,
    /// The calls this session's gate admitted without asking anybody, waiting to be said. The gate runs
    /// inside the runtime, off this struct, so it hands the sentence over: `flush_gate_admissions`
    /// writes one row per notice at a round boundary, where no row lands between a call and its result.
    gate_admissions: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// The session's todo list, shared with the tool that writes it. The harness is the persister:
    /// `flush_todos` compares the board's version with the last one it handled and writes.
    todos: Arc<TodoBoard>,
    /// The board version this harness last persisted and announced.
    todos_version: u64,
    /// The questions the heads were last told this board waits on the operator for: each is announced
    /// once when asked and once when settled. Seeded from the board at open, so a restart does not
    /// re-announce and the settings row carries the state to a head that attaches.
    asks_said: Vec<(String, String)>,
    /// The row the last plan-check named, so the check keeps asking about THAT row rather than
    /// whatever happens to head the queue now. See `unfinished_plan_for`: the choice is sticky, and
    /// this is the one field that remembers it.
    nag_choice: Option<String>,
    /// The stale-prompt sentence the model has already been told, so it is told ONCE. The decision is
    /// the person's and it stares at them from their own head. Compared by text, so a different
    /// sentence is told again, and cleared by the fork that re-seats.
    stale_told: Option<String>,
    /// A fork whose rows are not all stored yet - sealed by [`Harness::seal_fork`]
    /// once they are.
    unsealed_fork: Option<UnsealedFork>,
    /// This turn's metrics, as it runs - begun and closed by the turn tail, fed
    /// by the round loop. See `crate::instruments`.
    pub(crate) meter: crate::instruments::TurnMeter,
    /// The encoder and error signal. Always constructed and always attached  -
    /// measuring costs nothing and `Verification::NoEncoder` is a state whose
    /// reason is ours rather than the model's.
    intent: Arc<IntentLedger>,
    /// What the next step boundary will inject: intent findings, monitor firings.
    /// Shared with the session's [`HubSteering`].
    injected: Arc<Mutex<VecDeque<String>>>,
    /// What a round handed back to its steering, shared with every `HubSteering` this harness builds,
    /// so a message handed back by a failed or interrupted round outlives the round.
    handed_back: Arc<Mutex<VecDeque<SteeringMessage>>>,
    /// Whether the turn in flight was stopped by an interrupt, shared with every `HubSteering` for
    /// `handed_back`'s reason and read once, when the turn ends: a stopped turn rings no wake.
    stopped: Arc<AtomicBool>,
    /// This session's monitors, when the backend can start processes.
    monitors: Option<Arc<Monitors>>,
    /// This session's connection to the box's room, when there is a coordinator and this session may
    /// speak in it. Two of its three duties are the harness's own moments (the beat starts and stops
    /// with the turn clock); this field is what keeps the connection alive for the session's life.
    room: Option<Arc<crate::room::link::SessionRoom>>,
    /// Shared with [`HubSteering`] so a firing reaches the model exactly once,
    /// whether it is picked up mid-turn or between turns by [`Harness::wake`].
    monitor_cursor: Arc<AtomicUsize>,
    /// The tool event sink, wrapped in the intent encoder. One for the session
    /// rather than one per round: constructing the decorator is what declares the
    /// encoder, and the call_id→name map inside it spans a turn.
    tool_sink: JobWatchSink<IntentSink<ToolLogSink>>,
    /// The session's background-job watcher, when the backend can start
    /// processes. Fed by the sink as backgrounded results pass; stopped when the
    /// backend closes.
    job_watch: Option<Arc<JobWatchers>>,
    /// The session's children, as the thing that owns their lifecycle: the same `Arc` the tool registry
    /// and `job_watch` hold, because a second runner would hand out handles its sibling cannot collect.
    /// Stopping is the session's business, so an interrupt has to reach the runner on this thread.
    subagents: Arc<dyn letibot_tools::builtins::task::TaskRunner>,
    /// The merge-queue reviews this session is hosting: who is reviewing what, and what they handed in.
    /// The child's verdict arrives HERE and not in its prose, since a child that answers without calling
    /// `review_verdict` has judged nothing and leaves the row for the queue to ask again.
    reviews: Arc<letibot_tools::builtins::verdict::ReviewDesk>,
    /// Every gatekeeper child this session has started: a settlement naming one is the review's, not
    /// news for this session's model, and is dropped from the notices. `wake` serves the reviews first,
    /// so by the time the notices are built the verdict is already written onto the row.
    gatekeepers: std::collections::HashSet<String>,
    /// What this session's model is owed about the children a restart brought back: one `[restart]`
    /// line per child that was resumed or could not be. Filled by `restore_after_restart` at open and
    /// said by the next turn the session takes.
    restart_notices: Vec<String>,
    /// Whether this harness has said its workspace is gone - once, at the first wake
    /// that found it so.
    workspace_gone_said: bool,
    /// The monitor names live at the last [`Harness::sync_monitor_records`], sorted.
    monitors_seen_live: Vec<String>,
    /// The queue entries this session's model has been told about, so each is said once per daemon. One
    /// set for both kinds, because an entry has one `evidence` string and a second telling would be
    /// about the same row.
    gate_told: std::collections::HashSet<String>,
    /// The outage the retry ladder is in, if any - one per endpoint, told ONCE when it ends.
    /// See [`Outage`] for why the row is written at the end and not per attempt.
    outage: Option<Outage>,
    /// The cloud provider the turns go to, when the session has one. `None` is
    /// the local server through the engine's own `/completion` path - unless
    /// [`Harness::key_wanted`] is set, which is a provider with no key yet.
    provider: Option<Box<dyn letibot_backend::MessagesBackend>>,
    /// A provider chosen and not yet usable, because no key for it resolves. It is not a refusal at
    /// open, which would kill the daemon before a head could attach; the first turn asks for the key on
    /// the masked secret card, saves it to providers.toml (0600) and builds the provider.
    key_wanted: Option<crate::config::ProviderConfig>,
    /// The local server's own context window, kept from before the first switch to a provider so
    /// `/models local` can put it back. `Option<Option<u64>>`: never switched against switched away from
    /// a server with no window, and only one of those should restore anything.
    local_window: Option<Option<u64>>,
    /// What `/models local` returns to: the daemon's own server, vocabulary and dialect, captured at
    /// open before any switch moved them. A return needs a record rather than a re-derivation.
    own_server: OwnServer,
    /// The same fact, shared with the subagent runner, so a child spawned `model: "local"` plans its
    /// compaction against the daemon's server window rather than the cloud model's number the parent
    /// carries. Written wherever `local_window` is written.
    local_window_cell: Arc<std::sync::Mutex<Option<Option<u64>>>>,
    /// This session's point, as it stands now, shared with the subagent runner. A cell because the
    /// runner holds a clone of this config taken at open, and `/mode` moves the point without touching
    /// that clone, so a child spawned after a move would come up at the old point.
    point_cell: Arc<std::sync::Mutex<letibot_tools::mode::Mode>>,
    /// The model this session switched away from, kept until a round proves the new one works, so a
    /// switch that fails says how to go back. Without it the operator had to remember what the session
    /// had been on and type it back by hand.
    switch_back: Option<SwitchBack>,
    /// A model switch that arrived while a prompt was in flight, waiting for that prompt to end. A
    /// field and not a hub queue, because the request is answered the moment it is taken and what is
    /// left is one act at one moment. A queue would let two switches stack.
    pending_switch: Option<PendingSwitch>,
    /// The turn the daemon owes after it moved this session off a model that was out: the notice that
    /// opens it, built where the refusal is and taken where a turn ends. Ending the turn on the refusal
    /// was half an answer, so the work goes on in a new turn. At most one, or a chain would ping-pong.
    fallback_continuation: Option<String>,
    /// This turn IS the automatic continuation a fallback earned - set while it runs, so
    /// [`Harness::fall_back_from`] moves the session to the next name without arming a second
    /// continuation. See `fallback_continuation` for why the bound exists.
    continuation_of_fallback: bool,
    /// The run of provider troubles this session is in: the kind of the last one, and how many of
    /// that kind in a row. Carried so the event says *the fourth timeout* rather than *a timeout*, and
    /// so a fallback policy can count what the wire is told.
    trouble_kind: String,
    trouble_count: u32,
    /// The session's plugin host, once the daemon has one: the same live `Arc` the `plugin` tool asks
    /// through, handed over because the tool is seated at open and the host when the daemon opens the
    /// session. `None` in a session the daemon did not open, where scaffold and build still work.
    plugin_slot: Arc<std::sync::Mutex<Option<Arc<crate::plugins::PluginHost>>>>,
    /// The mode confirmation a head asked for and nobody has answered
    /// (`PROTOCOL_VERSION` 54, B8). One per session: a second ask supersedes it.
    mode_confirm: Option<PendingModeConfirm>,
    /// The map the last compaction left: the ids the new base names and `recall` opens. One handle,
    /// shared with the runtime's tool, so the ids a model is shown are the ids it can read.
    recall: Arc<letibot_tools::Recall>,
    /// What the next fork's note carries below the record: one line per unit a compaction evicted,
    /// taken by [`Harness::fork_onto`] and written into the base it makes.
    recall_map: Vec<letibot_turn::evict::Unit>,
}

/// A confirmation of `allow-all` waiting for a person: the daemon's half of the card another head's ask
/// raised. The `asked` list is the rule and not bookkeeping - the card went to those heads and only
/// they can answer it, which makes *the bridge never consents for itself* structural.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingModeConfirm {
    /// The id the asker's own `client_request_id` was turned into; the card and the settle
    /// frame are correlated by it.
    pub req_id: String,
    /// The asker's own request name, echoed back on the settle so that head can stop waiting
    /// without having had to learn `req_id` first.
    pub client_request_id: String,
    /// The point being confirmed, as the asking head spelled it.
    pub name: String,
    /// The head ids the card was pushed to.
    pub asked: Vec<String>,
}

/// What a resume actually rebuilt, reported as numbers: the row count, the token count and the chain
/// head are the evidence that `resumed` is only the claim. The head is what makes two resumes of one
/// session comparable - if it differs, one of them is not the conversation the other was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeReport {
    pub transcript_id: String,
    pub rows: usize,
    pub tokens: usize,
    pub head: String,
    /// Where the tools are confined - this session's own root, which is not
    /// necessarily the directory the daemon was started in.
    pub workspace: String,
    /// Anything about this resume the operator would otherwise find out later: a
    /// workspace that moved, a stable prefix that no longer matches the daemon's.
    pub notes: Vec<String>,
}

/// The operator's message when they hand a skill over: `/skills NAME`, and the pane's Enter. The label
/// is the whole of it, because the body is somebody else's prose and a model that read it as the
/// operator's instruction would follow a text they merely pointed at.
pub fn skill_message(skill: &letibot_tools::builtins::skill::Skill, then: &str) -> String {
    let mut text = format!(
        "[skill `{}`, from {} — the words below are the skill's, handed to you by the \
         operator]\n{}\n[end of the skill `{}`]",
        skill.name,
        skill.source.label(),
        skill.body.trim_end(),
        skill.name
    );
    let then = then.trim();
    if !then.is_empty() {
        text.push_str("\n\n");
        text.push_str(then);
    }
    text
}

impl Harness {
    /// The turn this harness last started, or empty before the first one. Empty is a real answer - a
    /// turn that failed while being set up never got an id - and a head treats it as the pane it is
    /// looking at, the only one that can be spinning then.
    pub fn last_turn_id(&self) -> &str {
        &self.last_turn_id
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// The cell the daemon fills with this session's plugin host. `Sessions::open_plugins` is the only
    /// caller, so the `plugin` tool asks the very stack the head's panes are drawn from: two hosts would
    /// be two answers to what is in force.
    pub fn plugin_slot(&self) -> Arc<std::sync::Mutex<Option<Arc<crate::plugins::PluginHost>>>> {
        self.plugin_slot.clone()
    }

    /// This session's plugin host, when the daemon has opened it.
    pub fn plugin_host(&self) -> Option<Arc<crate::plugins::PluginHost>> {
        self.plugin_slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The project root this session's tools are confined to - the session's own,
    /// resolved from the store at open, not necessarily the directory the daemon
    /// started in. The `/mode` command keys the per-project store on it.
    pub fn workspace(&self) -> &std::path::Path {
        &self.cfg.workspace
    }

    /// Turn the guard model on or off on the running session, and read whether it gets a turn now.
    /// The gate is reached rather than a setting persisted, which is the difference from `/mode`:
    /// supervision decides only whether the model gets a turn, so nothing seated depends on it.
    pub fn supervising(&self) -> bool {
        self.gate().supervising()
    }

    /// The gate, locked: the runtime is shared with the operator's run thread and the gate is the one
    /// field that mutates in place. Reads and moves are momentary; a caller making a decision holds it.
    fn gate(&self) -> std::sync::MutexGuard<'_, Box<dyn letibot_tools::Gate>> {
        self.runtime.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    // Point this session's guard at `endpoint`, build it and attach it: the `/supervise HOST:PORT`
    // override, not the normal path, which is `[gatekeeper] endpoint` read at daemon start. Nothing is
    // written here, because config is the operator's file and a rewrite would make an override permanent.

    /// Close out tool calls no round owns any more, and read the log to say why. The round loop is
    /// synchronous, so if this is running no round owns a call: the fact that we are here is the fact
    /// that the executor is gone, which needs no timeout. `NotRun`, because nothing ran and nothing failed.
    pub fn sweep_abandoned_calls(&mut self) -> usize {
        // THE DECISION IS A FREE FUNCTION, so it can be tested without a daemon, a model or a
        // socket. What is left here is the plumbing: read the log, build the rows, append them.
        let abandoned = self.swept_calls.sweep(self.session.items.as_slice());
        if abandoned.is_empty() {
            return 0;
        }

        let tail = self.stored_tail(12);
        let mut settled = 0;
        let mut results: Vec<TranscriptItem> = Vec::new();
        for (call_id, name) in abandoned {
            let payload = format!(
                "**no result — the executor is gone.**\n\n`{name}` (call `{call_id}`)                 \n\nThe call was dispatched and nothing ever answered it. The daemon's tool thread \
                 and the call's process were both gone by the time this was noticed, and the round \
                 had already ended, so there was nothing left to interrupt.\n\n{tail}",
            );
            results.push(TranscriptItem::ToolResult {
                call_id: call_id.clone(),
                name: name.clone(),
                outcome: letibot_transcript::ToolOutcome::NotRun {
                    why: format!(
                        "the executor for `{name}` disappeared before it answered; nothing ran"
                    ),
                },
                payload,
                edit: None,
                origin: None,
                media: None,
            });
            settled += 1;
        }
        // And the head is told, including when the record could not be written: a close-out the store refused
        // must still be announced, or the spinner the sweep exists to end stays up.
        let named: Vec<String> = results
            .iter()
            .filter_map(|r| match r {
                TranscriptItem::ToolResult { call_id, name, .. } => {
                    Some(format!("`{name}` ({call_id})"))
                }
                _ => None,
            })
            .collect();
        match self.append_results(results) {
            Ok(()) => self.import_note(
                "calls_abandoned",
                format!(
                    "{settled} tool call(s) had no result and nothing left to answer them — the \
                     executor was gone and the round had ended: {}. Each is closed out as not run, \
                     with the stored transcript's tail on its row.",
                    named.join(", ")
                ),
            ),
            Err(e) => self.import_note(
                "transcript_store",
                format!(
                    "{settled} tool call(s) nothing will answer ({}) could not be closed out: {e}. \
                     They may still show as running.",
                    named.join(", ")
                ),
            ),
        }
        settled
    }

    /// Append rows the way the round loop does, so a swept call lands like any other.
    fn append_results(&mut self, results: Vec<TranscriptItem>) -> Result<(), HarnessError> {
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session
            .append_items(&self.engine, &results, &mut sink)?;
        self.reconcile(&mut sink, &results);
        Ok(())
    }

    /// Move THIS session to another point, now, rather than from the next session: the gate needs a
    /// field write. The work is the prerequisite check the open would have run, re-run against what this
    /// session has, the standing grants dropped, and a model-decided point's supervision turned on.
    pub fn set_mode(&mut self, mode: letibot_tools::mode::Mode) -> Result<String, String> {
        self.set_mode_consented(mode, false)
    }

    /// `/mode`, with the operator's answer to the unconfined-`allow-all` question. A `yes` selects
    /// `Mode::ALLOW_ALL_HERE` only when the point asked for is `allow-all`, this session really has no
    /// confinement, and the substituted point passes `check` alone. It is not persisted.
    pub fn set_mode_consented(
        &mut self,
        mode: letibot_tools::mode::Mode,
        consented: bool,
    ) -> Result<String, String> {
        let mut mode = mode;
        let mut vouched = false;
        if let Some((have, seats)) = &self.supplies {
            let unconfined = !have.contains(&letibot_tools::mode::Prereq::Confinement);
            if consented && mode.name == letibot_tools::mode::Mode::ALLOW_ALL.name && unconfined {
                mode = letibot_tools::mode::Mode::ALLOW_ALL_HERE;
                vouched = true;
            }
            mode.check(have, *seats)?;
        } else if consented && mode.name == letibot_tools::mode::Mode::ALLOW_ALL.name {
            // No `supplies` means the open never ran the check - a session built by
            // a test harness. Consent cannot be honoured against a list nobody
            // computed, so it is refused rather than granted on a guess.
            return Err("this session did not record what it can supply, so an \
                        unconfined `allow-all` cannot be confirmed against it"
                .into());
        }
        let dropped = self.gate().set_mode(mode)?;
        let mut said = format!("this session is at `{}` from the next call", mode.name);
        if vouched {
            // Said here and not only on the card: the banner, the log warning and this sentence are what
            // a person reads later. Not "the project's row still reads allow-all" - it does not, because
            // nothing was written.
            said.push_str(
                " — nothing confines this box, and this point stands on your \
                 confirmation rather than on a boundary. It lasts for this session \
                 only: nothing was written down, and a daemon restart drops it",
            );
        }
        if dropped > 0 {
            said.push_str(&format!(
                " — {dropped} standing grant(s) taken under `{}` no longer apply",
                self.cfg.mode.name
            ));
        }
        if mode.decider == letibot_tools::mode::Decider::Model && !self.gate().supervising() {
            // The check above holds an oracle to be present at this point, so an
            // error here is a real one and not the ordinary "no `--oracle`".
            self.set_supervision(true)?;
            said.push_str("; the guard model now answers");
        }
        self.cfg.mode = mode;
        // And the spawn path's copy of it, which is a different object: the runner
        // holds `base`, cloned at open, and a child built from it would inherit the point
        // this session no longer holds. See `HarnessTaskRunner::point`.
        *self.point_cell.lock().unwrap_or_else(|e| e.into_inner()) = mode;
        self.mode_source = "/mode, this session".into();
        // Bound first: the guard is held while `describe` runs, and the assignment
        // writes a field of the same struct the guard borrows through.
        let adjudicator = self.gate().describe();
        self.wiring.adjudicator = adjudicator;
        self.publish_settings();
        Ok(said)
    }

    /// Does the point asked for need a PERSON's answer here: the two facts `set_mode_consented` reads
    /// from `supplies`, asked on their own. A session that recorded no supplies is answered `true`, the
    /// fail-closed direction, because consent cannot be honoured against a list nobody computed.
    pub fn mode_needs_consent(&self, mode: letibot_tools::mode::Mode) -> bool {
        if mode.name != letibot_tools::mode::Mode::ALLOW_ALL.name {
            return false;
        }
        match &self.supplies {
            Some((have, _)) => !have.contains(&letibot_tools::mode::Prereq::Confinement),
            None => true,
        }
    }

    /// A head ASKS for the point that needs a person: the confirmation is pushed to the session's other
    /// deciding heads, and the point moves only when one says yes. A point nobody has to approve is
    /// applied here; with no other head the caller gets the refusal naming `leticode --session`.
    pub fn ask_mode_confirm(
        &mut self,
        name: &str,
        from_head: &str,
        from_identity: &str,
        client_request_id: &str,
    ) -> Result<(), String> {
        let mode = letibot_tools::mode::Mode::parse(name)?;
        let session = self.cfg.session_id.clone();
        // The id is the asker's own request name with the head in front of it: unique across
        // heads, and one the asking connection can correlate by without being told.
        let req_id = format!("{from_head}:{client_request_id}");
        if let Some(open) = self.mode_confirm.take() {
            let named = open.name.clone();
            self.settle_mode_confirm(&open, &named, false, "nobody", superseded(&open.name));
        }
        if !self.mode_needs_consent(mode) {
            // The same `mode_set` line the `/mode` path writes, because a point that moved
            // has to be legible afterwards on the session it moved in - and this one moved
            // without anybody typing it, which is precisely the case a reader would question.
            let applied = match self.set_mode_consented(mode, false) {
                Ok(said) => {
                    self.warn_mode(
                        "mode_set",
                        format!("{said}. {} asked for it", from_identity),
                    );
                    (true, said)
                }
                Err(why) => {
                    self.warn_mode(
                        "mode_unconfirmed",
                        format!(
                            "`{}` was asked for and could not be applied: {why}",
                            mode.name
                        ),
                    );
                    (false, why)
                }
            };
            self.hub
                .push_frame(letibot_sessionlog::protocol::ServerFrame::ModeSettled {
                    session_id: session,
                    req_id,
                    client_request_id: client_request_id.to_string(),
                    name: if applied.0 {
                        self.cfg.mode.name.to_string()
                    } else {
                        mode.name.to_string()
                    },
                    applied: applied.0,
                    answer: "nobody".into(),
                    said: applied.1,
                });
            return Ok(());
        }
        let asked = self.hub.deciding_heads_except(from_head);
        if asked.is_empty() {
            return Err(format!(
                "`{}` on this session needs the operator's confirmation in a head, and no \
                 head that may decide is attached to `{session}` besides the one that asked. \
                 Nothing was changed: run `leticode --session {session}`, or ask the operator \
                 to, and call this again",
                mode.name
            ));
        }
        self.hub.push_frame_where(
            letibot_sessionlog::protocol::ServerFrame::ModeConfirm {
                session_id: session,
                req_id: req_id.clone(),
                name: mode.name.to_string(),
                asked_by: from_identity.to_string(),
            },
            // Never the head that asked. A program that could answer its own request
            // would be consenting for the person the request exists to consult.
            |head| asked.iter().any(|a| a == head),
        );
        self.mode_confirm = Some(PendingModeConfirm {
            req_id,
            client_request_id: client_request_id.to_string(),
            name: mode.name.to_string(),
            asked,
        });
        Ok(())
    }

    /// A head's answer to that ask: `yes` applies the point, `no` applies nothing. Refused by name when
    /// no request is open, when the id is not the open one, and when this head was not asked, which is
    /// the structural half of *the bridge never consents for itself*.
    pub fn answer_mode_confirm(
        &mut self,
        req_id: &str,
        yes: bool,
        head: &str,
        identity: &str,
    ) -> Result<(), String> {
        let Some(open) = self.mode_confirm.as_ref() else {
            return Err(format!(
                "no mode confirmation is open on this session, so `{req_id}` settles nothing"
            ));
        };
        if open.req_id != req_id {
            return Err(format!(
                "the open confirmation is `{}`, not `{req_id}`",
                open.req_id
            ));
        }
        if !open.asked.iter().any(|a| a == head) {
            return Err(format!(
                "`{req_id}` was not put in front of this head: only the heads that were asked \
                 can answer a mode confirmation, and this one asked for it"
            ));
        }
        let open = self.mode_confirm.take().expect("just checked");
        if !yes {
            let named = open.name.clone();
            self.settle_mode_confirm(
                &open,
                &named,
                false,
                "no",
                format!(
                    "{identity} did not confirm `{}`, so nothing changed — the session stays \
                     where it was",
                    open.name
                ),
            );
            self.warn_mode(
                "mode_declined",
                format!(
                    "`{}` was asked for and the confirmation was declined by {identity}: the \
                     approval point did not move",
                    open.name
                ),
            );
            return Ok(());
        }
        let mode = letibot_tools::mode::Mode::parse(&open.name)?;
        match self.set_mode_consented(mode, true) {
            Ok(said) => {
                let in_force = self.cfg.mode.name;
                self.settle_mode_confirm(
                    &open,
                    in_force,
                    true,
                    "yes",
                    format!("{identity} confirmed: {said}"),
                );
                self.warn_mode(
                    "mode_confirmed",
                    format!("`{}` confirmed by {identity} in a head: {said}", open.name),
                );
                Ok(())
            }
            Err(why) => {
                // A yes the session cannot carry is not applied, and the person is told by
                // the settle frame as well as by their own screen: `applied: false` with the
                // prerequisite check's own words.
                let named = open.name.clone();
                self.settle_mode_confirm(&open, &named, false, "yes", why.clone());
                self.warn_mode(
                    "mode_unconfirmed",
                    format!(
                        "`{}` was confirmed by {identity} and could not be applied: {why}",
                        open.name
                    ),
                );
                Err(why)
            }
        }
    }

    /// How a confirmation ended, to every head, in one frame. `name` is the point now in force and not
    /// always the one asked for: a consented `allow-all` lands on `ALLOW_ALL_HERE`, so a caller reading
    /// the ask back would be told the wrong point.
    fn settle_mode_confirm(
        &self,
        open: &PendingModeConfirm,
        name: &str,
        applied: bool,
        answer: &str,
        said: String,
    ) {
        self.hub
            .push_frame(letibot_sessionlog::protocol::ServerFrame::ModeSettled {
                session_id: self.cfg.session_id.clone(),
                req_id: open.req_id.clone(),
                client_request_id: open.client_request_id.clone(),
                name: name.to_string(),
                applied,
                answer: answer.to_string(),
                said,
            });
    }

    fn warn_mode(&self, code: &str, detail: String) {
        self.hub.publish(SessionEvent::Warning {
            code: code.to_string(),
            detail,
            compaction: None,
        });
    }

    /// Push the settings this session runs under to the registry, where the server answers a
    /// head's `Settings` from. On open and on every runtime change, so a head draws what is
    /// running rather than the rows it read when it attached.
    fn publish_settings(&self) {
        let mut rows = self.cfg.settings(
            &self.mode_source,
            self.gate().supervising(),
            &self.door_tools(),
        );
        // Whether the board waits on the operator - `none`, or how many and the first
        // question. A row here because a head that attaches after the question was asked never
        // saw the `waiting_on_operator` row, and must still draw the loud line.
        rows.push(letibot_sessionlog::protocol::SettingRow {
            key: "asks".into(),
            value: crate::operator_ask::setting_value(&self.todos.snapshot()),
            source: "this session's todo board".into(),
            editable: "answer from the todo pane, or the waiting line's key".into(),
            choices: Vec::new(),
            tools: Vec::new(),
        });
        self.session_registry
            .set_settings(&self.cfg.session_id, rows);
    }

    /// What a head needs to offer the door without holding a schema, derived from this session's
    /// registry and the daemon's allowlist, so a schema change moves a head's behaviour with no head
    /// rebuilt. Idempotent and cheap: one pass over three schemas, on open and every republish.
    fn door_tools(&self) -> Vec<letibot_sessionlog::protocol::HeadRunTool> {
        let allow = letibot_sessionlog::HEAD_RUN_TOOLS;
        let schemas = self.runtime.registry.schemas();
        let mut out = Vec::with_capacity(allow.len());
        for (name, form) in letibot_tools::head_run::bare_forms(&allow, schemas.iter()) {
            let (field, kind, defaults, why) = match form {
                letibot_tools::head_run::BareForm::One {
                    field,
                    kind,
                    defaults,
                } => (
                    field,
                    kind.as_str().to_string(),
                    defaults.into_iter().collect(),
                    String::new(),
                ),
                letibot_tools::head_run::BareForm::None(why) => {
                    (String::new(), String::new(), Default::default(), why)
                }
            };
            out.push(letibot_sessionlog::protocol::HeadRunTool {
                name,
                field,
                kind,
                defaults,
                why_json: why,
            });
        }
        out
    }

    pub fn set_supervision(&mut self, on: bool) -> Result<String, String> {
        // Attach on demand: `/supervise` on a session opened with no `--oracle` is the ordinary case,
        // since nobody types daemon flags. If an address is known from anywhere, use it rather than
        // sending the operator to restart a daemon.
        if on && !self.gate().supervising() {
            let known = self.cfg.oracle.clone().or_else(|| {
                letibot_provider::gatekeeper(None)
                    .endpoint
                    .as_deref()
                    .and_then(|s| Endpoint::parse(s).ok())
            });
            if let Some(ep) = known {
                self.attach_oracle(ep)?;
            }
        }
        let said = self.gate().set_supervision(on)?;
        // The banner is read after this, and a disclosure that still said
        // `--adjudicator head` about a supervised session would be the constant-banner
        // defect `GateWiring` exists to prevent.
        let adjudicator = self.gate().describe();
        self.wiring.adjudicator = adjudicator;
        self.publish_settings();
        Ok(said)
    }

    /// What this session actually wired. The adjudication disclosure is computed
    /// from it; see [`GateWiring`] for why it is not a constant.
    pub fn wiring(&self) -> &GateWiring {
        &self.wiring
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Where this session's point came from: the settings row's own field, for a caller that says it
    /// somewhere else too (a child whose point was clamped says so on its parent's progress line).
    pub fn mode_source(&self) -> &str {
        &self.mode_source
    }

    /// The session's own config, to change a decision that is the session's
    /// rather than the daemon's - `auto_compact` turning itself off after a
    /// compaction that did not free enough. See `Sessions::compact_if_at_the_wall`.
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.cfg
    }

    pub fn transcript_id(&self) -> &str {
        &self.transcript_id
    }

    pub fn items(&self) -> &[TranscriptItem] {
        &self.session.items
    }

    /// What this session's harness holds, now: the same answer the hub's census gets. Read by a test
    /// comparing the census against the transcript it describes, and by nothing in a daemon.
    pub fn footprint(&self) -> letibot_sessionlog::hub::Footprint {
        crate::footprint::of(&self.footprint_gauges, &self.trail)
    }

    /// Restate the hub's footprint from what this harness holds, at open and from `reconcile`, which
    /// every append goes through. Each call walks the rows: an increment would under-report for ever
    /// after one missed site, and a transcript is a few hundred rows of a few strings each.
    fn restate_footprint(&self) {
        self.footprint_gauges
            .set_ceiling(crate::footprint::ceiling_for(self.cfg.context_window));
        self.footprint_gauges.restate(
            self.session.items.len(),
            self.session.items.iter().map(|i| i.bytes()).sum(),
            self.session.ledger.region().bytes(),
            self.session.ledger.len(),
        );
    }

    /// The session's todo list as the model last wrote it - the board's
    /// snapshot, which is what a resume restored and what `flush_todos` keeps
    /// durable.
    pub fn todo_list(&self) -> Vec<TodoItem> {
        self.todos.snapshot()
    }

    /// This session's watcher set: what queues a background job's or a subagent's settlement for THIS
    /// session, whichever transcript it is speaking by then. A turn arms it through its own sink; this
    /// is for a caller that invokes a tool outside a turn and still wants it watched.
    pub fn job_watchers(&self) -> Option<Arc<JobWatchers>> {
        self.job_watch.clone()
    }

    /// The sink for a call this session runs OUTSIDE a turn: the operator's own call and their `!` line.
    /// Null, because the transcript row is the durable record and every head draws it; watched, because
    /// a `Backgrounded` outcome is a promise the row makes in words and nothing else delivers it.
    fn unprompted_sink(&self) -> JobWatchSink<letibot_tools::events::NullToolSink> {
        JobWatchSink::new(letibot_tools::events::NullToolSink, self.job_watch.clone())
    }

    /// The board's version, which says the board moved and from whom: `flush_todos` watches it to decide
    /// whether an announcement is owed, and a parked child's loop watches it for the other writer, a
    /// parent's `todo_write target=`. It needs no new channel and cannot disagree with the flush.
    pub fn todo_board_version(&self) -> u64 {
        self.todos.version()
    }

    pub fn ledger_head(&self) -> String {
        self.session.ledger_head()
    }

    /// The ledger's whole token count - prefix and body together - which is the
    /// number a compaction is measured against.
    pub fn ledger_len(&self) -> usize {
        self.session.ledger.len()
    }

    /// Whether this session's turns go to a metered provider - the case where the
    /// ledger and the provider's count are two different numbers.
    pub fn is_metered(&self) -> bool {
        self.provider.is_some()
    }

    /// The unit a printed count is in: a count with no unit beside another in a different one is how
    /// `3191928 of 3404045` came to sit on the line above `937686 of 999999`. Three answers on a metered
    /// session, because only one of the three states is a measurement.
    pub fn token_unit(&self) -> String {
        match (&self.provider, self.cfg.ledger_scale) {
            (Some(p), Some(_)) => format!("{} tokens", p.name()),
            // An estimate names the model that made it: `estimated with deepseek's measured scale` is a
            // different claim from `deepseek tokens` and from `ledger tokens (unscaled)`, and a reader not
            // told which they have cannot tell a measurement from an inherited ratio.
            (Some(p), None) => match self.cfg.carried_note() {
                Some(model) => format!(
                    "{} tokens (estimated with {model}'s measured scale)",
                    p.name()
                ),
                None => format!(
                    "ledger tokens (unscaled: no round on {} has measured yet)",
                    p.name()
                ),
            },
            (None, _) => "tokens".into(),
        }
    }

    /// The whole submitted token vector, for a caller that wants to check the
    /// prefix relation itself (C1).
    pub fn tokens(&self) -> &[letibot_tokencore::TokenId] {
        self.session.ledger.tokens()
    }

    /// The stable prefix's tokens, which the ledger keeps apart from the item rows. C9 compares against
    /// exactly this: a system change that appended left these bytes untouched, one that rewrote the head
    /// did not.
    pub fn prefix_tokens(&self) -> &[letibot_tokencore::TokenId] {
        self.session.ledger.prefix_tokens()
    }

    /// The ledger row ids, in order: what pairs an announcement with its body, since
    /// `TranscriptAppended` carries an id and `Hub::record_item` fills the row it names. A caller
    /// checking that pairing reads the ids rather than recomputing them.
    pub fn row_ids(&self) -> Vec<&str> {
        self.session
            .ledger
            .rows()
            .iter()
            .map(|r| r.item_id.as_str())
            .collect()
    }

    /// How many tokens item `i` owns. Zero would mean an item that renders to
    /// nothing, which for an assistant turn is the `content: null` defect one layer
    /// down (C6).
    pub fn row_len(&self, i: usize) -> u32 {
        self.session
            .ledger
            .rows()
            .get(i)
            .map(|r| r.tok_len)
            .unwrap_or(0)
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.engine.endpoint
    }

    /// A warning on this session's log - the operator's own sentence about something that
    /// did not go as asked. One writer for the two doors that can refuse an attachment, and it goes
    /// through [`Hub::warn`] so the daemon and the harness cannot word one fact two ways.
    pub(crate) fn warn(&self, code: &str, detail: String) {
        self.hub.warn(code, detail);
    }

    /// The batch a turn about to open carries, taken and NOT yet called delivered: the
    /// door marks it once its row is in the transcript, or hands it back with
    /// [`Harness::the_batch_arrived`].
    pub(crate) fn batch_riding_in(&self) -> (Option<String>, Vec<JobCompletion>) {
        match self.deliveries() {
            Some(d) => d.at_turn_start_unread(),
            None => (None, Vec::new()),
        }
    }

    /// That batch reached the model, or it did not: `opened` is whether the door's row went in.
    /// A row that never went in leaves the results owed, so the next break says them.
    pub(crate) fn the_batch_arrived(&self, told: Vec<JobCompletion>, opened: bool) {
        if told.is_empty() {
            return;
        }
        let Some(d) = self.deliveries() else {
            return;
        };
        if opened {
            d.read(&told);
        } else {
            d.put_back(told);
        }
    }

    /// The delivery for this session - see [`Deliveries`]. `None` for a session with no
    /// watcher set, which has nothing to deliver.
    pub(crate) fn deliveries(&self) -> Option<Deliveries> {
        let watch = self.job_watch.clone()?;
        Some(Deliveries::new(
            watch,
            Some(self.todos.clone()),
            crate::delivery::DeliveryLog::under(&self.cfg.daemon_logs(), &self.cfg.session_id),
            self.cfg.held_results_cap,
            &self.hub.session_id(),
        ))
    }

    /// This session's monitors, or `None` on a backend that cannot start processes.
    ///
    /// The daemon needs them to arm the wake; nothing else does.
    pub fn monitors(&self) -> Option<&Arc<Monitors>> {
        self.monitors.as_ref()
    }

    /// The operator ends a monitor - the monitors pane's `x`, as `/monitor retire NAME`. The
    /// record says who (`retired by the operator`), so the model's next notice says so, and the
    /// pane's table is refilled at once.
    pub fn retire_monitor_for_operator(&self, name: &str) -> crate::slash::SlashReply {
        let Some(monitors) = self.monitors.as_ref() else {
            return crate::slash::SlashReply {
                lines: vec!["this session has no monitors to retire".into()],
                ok: false,
            };
        };
        let Some(f) = monitors.retire(name, OPERATOR_RETIRE) else {
            return crate::slash::SlashReply {
                lines: vec![format!(
                    "no monitor named `{name}` is watching — it may have fired or expired already"
                )],
                ok: false,
            };
        };
        self.publish_jobs();
        crate::slash::SlashReply {
            lines: vec![format!("monitor `{}` retired; the model is told", f.name)],
            ok: true,
        }
    }

    /// Install this session's standing-notes source, which a head's `ListNotes` is answered from. A
    /// closure rather than a mailbox, and the read happens on the ask, because the notes are the
    /// operator's own files edited in another window. Cheap: a readdir and one read per note.
    fn install_notes_source(&self) {
        let workspace = self.cfg.workspace.clone();
        let global = self.cfg.global_notes();
        // The box's instruction directories, from the config and not from `$HOME`: the pane
        // re-reads the corpus on every open and the answer is the config's, so a fixture's pane
        // names the files a fixture wrote.
        let box_dirs = self.cfg.box_dirs.clone();
        // The vocabulary the section itself is measured with: a field of its own rather than the
        // engine's, because the pane's read happens on the server's thread and must not measure with the
        // vocabulary the session OPENED on.
        let vocab = self.note_vocab.clone();
        // The session's own backend, shared: the pane reads the notes where the session's `notes`
        // tool wrote them, and this closure outlives the call that opened the session.
        let backend = self.runtime.backend.clone();
        self.session_registry.set_notes_source(
            &self.cfg.session_id,
            std::sync::Arc::new(move || {
                let files = crate::standing_notes::Reader::new(&*backend, &workspace);
                crate::standing_notes::rows(&files, &global, &box_dirs, &vocab)
            }),
        );
    }

    /// Install this session's skills source, which is what a head's `ListSkills` is answered
    /// from - the same rows the prompt's index is composed from, read on the ask.
    fn install_skills_source(&self) {
        let skills = self.skills.clone();
        self.session_registry.set_skills_source(
            &self.cfg.session_id,
            std::sync::Arc::new(move || {
                let (rows, _) = skills.index_rows();
                rows.into_iter()
                    .map(|r| letibot_sessionlog::protocol::SkillRow {
                        name: r.name,
                        source: r.source.label(),
                        description: r.description,
                        path: r.path.map(|p| p.display().to_string()),
                    })
                    .collect()
            }),
        );
    }

    /// The operator hands this session one skill's body: `/skills NAME` and the pane's Enter. The body
    /// is their message, the label says where it came from because a skill is not their words, and the
    /// rest of the line goes on as their own prompt in the same turn.
    pub fn load_skill(&mut self, name: &str, then: &str) -> Result<Reply, HarnessError> {
        let skill = self.skills.load_any(name).map_err(HarnessError::Setup)?;
        self.submit(&skill_message(&skill, then))
    }

    /// Say what this session is waiting on, if it has changed. Almost every move is caught by the hub
    /// itself, so this is for the changes with no event at all: a monitor firing on the waiter's own
    /// thread, a settlement queued by a watcher, a prompt sitting in the command queue. Not a timer.
    pub(crate) fn publish_activity(&self) -> bool {
        self.hub.refresh_activity()
    }

    /// The cursor into settled monitors, shared with this session's steering source
    /// so a firing is delivered exactly once however it is noticed.
    pub fn monitor_cursor(&self) -> &Arc<AtomicUsize> {
        &self.monitor_cursor
    }

    /// Say that this session's monitor wake is armed, so the disclosure stops calling it a poll. A
    /// setter because arming is the daemon's act, and a harness claiming a wake it was not given would
    /// be the banner defect this file has already paid for.
    pub fn declare_monitor_wake(&mut self) {
        self.wiring.monitor_wake = crate::config::MonitorWake::Armed;
    }

    /// The waiter was asked for and could not be started. The caller still publishes the `Warning` with the error; this is the banner's half.
    /// A bool would read like a session nobody armed, and *POLL ONLY* would send a reader after a useless knob.
    pub fn declare_monitor_wake_failed(&mut self) {
        self.wiring.monitor_wake = crate::config::MonitorWake::Failed;
    }

    // The metrics instruments' harness half: each call site in this file is one line, and what the
    // line needs from the harness is here in one block, so a merge that moves a site moves one line.

    fn meter_round(&mut self, m: &letibot_turn::metrics::TurnMetrics, ledger: u64) {
        let provider = self.provider.as_ref().map(|p| p.name());
        let (session, transcript) = (&self.cfg.session_id, &self.session.transcript_id);
        self.meter.round(session, transcript, m, ledger, provider);
    }

    fn meter_nag(&mut self, named: Option<&str>) {
        let status = named.and_then(|r| self.row_status(r));
        let operators = named.is_some_and(|r| self.row_is_operators(r));
        let session = self.cfg.session_id.clone();
        self.meter.nag(&session, named, status, operators);
    }

    /// `causes` in the order `wake` gathers them: queue, monitor, result, due row.
    fn meter_wake(&mut self, causes: [usize; 5], ran: bool) {
        let named = ["queue", "monitor", "result", "due", "held"];
        let causes: Vec<(&str, usize)> = named.into_iter().zip(causes).collect();
        crate::instruments::wake(&mut self.meter, &self.cfg.session_id, &causes, ran);
    }

    /// An automatic compaction: `(before, ledger before)`; the after is read here.
    fn meter_compaction(&self, tidy: bool, ok: bool, started: Instant, before: (u64, u64)) {
        let after = self.session.ledger.len() as u64;
        let door = if tidy { "pre-wall" } else { "refused-door" };
        let outcome = if ok { "ok" } else { "failed" };
        let shown = (before.0, self.cfg.shown_tokens(after));
        let unit = self.token_unit();
        let session = &self.cfg.session_id;
        crate::instruments::compaction(
            session,
            door,
            outcome,
            started,
            shown,
            (before.1, after),
            &unit,
        );
    }

    /// What this session's model is told about the queues: a repository whose target declares no gate,
    /// said once per daemon with `merge_gate` as the way forward. `watch_opened_pr` arms the watches on a
    /// pull request this session just opened, through the operator's door, and answers with the sentence.
    pub(crate) fn watch_opened_pr(&self, url: &str, n: u64) -> String {
        let mut armed = Vec::new();
        let mut refused = Vec::new();
        for (name, call) in crate::pr_watch::watch_calls(url, n) {
            if self
                .monitors
                .as_ref()
                .is_some_and(|m| m.get(&name).is_some())
            {
                continue;
            }
            let r =
                self.runtime
                    .invoke_operator("pr-watch", &call, &mut letibot_tools::NullToolSink);
            if r.outcome == letibot_transcript::ToolOutcome::Ok {
                armed.push(name);
            } else {
                let why = r.payload.lines().next().unwrap_or("").to_string();
                refused.push((name, why));
            }
        }
        if !armed.is_empty() {
            eprintln!(
                "  {} · watching {url}: {}",
                self.cfg.session_id,
                armed.join(", ")
            );
        }
        crate::pr_watch::told(url, &armed, &refused)
    }

    /// A plugin layer's look at a tool result: the text the model is about to read, and anything the
    /// layer asked to watch. It changes the payload in place, because that is the one string every later
    /// reader uses, so a rewrite after any of them would be a result nobody recorded.
    fn plugin_result(
        &self,
        call: &letibot_transcript::ToolCall,
        outcome: &str,
        payload: &mut String,
    ) {
        let Some(host) = self.plugin_host() else {
            return;
        };
        let asked = letibot_plugin::Call::new(
            call.name.clone(),
            serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null),
        );
        let Some(answered) = host.on_tool_result(&self.hub, &asked, outcome, payload) else {
            return;
        };
        let out = answered.out;
        let text = match out.said() {
            letibot_plugin::tool_result::Said::Unchanged => None,
            letibot_plugin::tool_result::Said::Text(text) => Some(text.into_owned()),
            letibot_plugin::tool_result::Said::Appended(suffix) => {
                Some(format!("{payload}{suffix}"))
            }
        };
        if let Some(text) = text {
            *payload = text;
        }
        for arm in &out.arm {
            let said = self.arm_plugin_monitor(arm);
            payload.push_str(&said);
        }
    }

    /// Arms a monitor a layer asked for, through the operator's door, because nobody asked this
    /// session's model for the watch and so no gate is owed it. The monitor tool's own rules apply
    /// unchanged, and the answer is a sentence appended to the call's result.
    fn arm_plugin_monitor(&self, arm: &letibot_plugin::tool_result::Arm) -> String {
        let name = arm.plugin.clone();
        if self
            .monitors
            .as_ref()
            .is_some_and(|m| m.get(&name).is_some())
        {
            return format!(
                "\n[harness] `{name}` is already watching, so the layer's request to arm it again \
                 was left alone"
            );
        }
        let call = crate::plugins::arm_call(arm);
        let r = self
            .runtime
            .invoke_operator("plugin-arm", &call, &mut letibot_tools::NullToolSink);
        if r.outcome == letibot_transcript::ToolOutcome::Ok {
            format!(
                "\n[harness] `{name}` is watching now (monitor `{name}`); `monitor` with action \
                 \"retire\" ends it"
            )
        } else {
            format!(
                "\n[harness] `{name}` was not armed: {}",
                r.payload.lines().next().unwrap_or("")
            )
        }
    }

    /// Whose subagent this session is, as the open settled it: resolved once by `Registry::parent_for`,
    /// so a child stays a child for the life of the process and the room's door, the settings row and
    /// the store row cannot disagree.
    pub fn parent_session(&self) -> Option<&str> {
        self.cfg.parent_session_id.as_deref()
    }

    /// Install the runner from outside this crate, which an integration test needs to stand a
    /// child in: a real one is a session and a thread, and the claim is about who is told to stop.
    #[doc(hidden)]
    pub fn set_subagents_for_test(
        &mut self,
        runner: Arc<dyn letibot_tools::builtins::task::TaskRunner>,
    ) {
        self.subagents = runner;
    }

    /// Monitors this daemon recorded that are no longer live are closed in the store, so a
    /// restart re-arms only what was really watching. Called where a firing, a retirement or an
    /// expiry has just been seen (a wake, a turn's end).
    fn sync_monitor_records(&mut self) {
        let (Some(monitors), Some(path)) = (&self.monitors, &self.cfg.store) else {
            return;
        };
        let mut live = monitors.live_names();
        live.sort();
        // Only when the live set moved - a wake is frequent and a store open is not free, and
        // the only change this records is a monitor leaving the live set.
        if live == self.monitors_seen_live {
            return;
        }
        self.monitors_seen_live = live.clone();
        let me = self.session_registry.instance();
        let Ok(store) = Store::open(path) else {
            return;
        };
        for m in store.monitors_of(&self.cfg.session_id).unwrap_or_default() {
            if m.live && m.instance == me && !live.contains(&m.name) {
                let _ = store.end_monitor(&self.cfg.session_id, &m.name);
            }
        }
    }

    /// What a resume rebuilt, or `None` for a session opened fresh.
    pub fn resumed(&self) -> Option<&ResumeReport> {
        self.resumed.as_ref()
    }

    /// The notes `open` made about a session it did not resume - see the field.
    /// Empty for a resumed session, whose notes are in [`Harness::resumed`].
    pub fn open_notes(&self) -> &[String] {
        &self.open_notes
    }

    /// The authorisation trail this session would show an adjudicator right now. Public because it is the
    /// input to a decision taken on the operator's behalf, and the trail has to be readable without going
    /// through a denial.
    pub fn trail(&self) -> AuthorisationTrail {
        self.trail.trail()
    }

    /// The intent ledger, for a head that wants to draw the board.
    pub fn intent(&self) -> &Arc<IntentLedger> {
        &self.intent
    }

    /// The operator's answer to a row's question, between turns: applied to the board, persisted and
    /// announced, with the line the model must be told handed back for the caller's turn. The mid-turn
    /// half is `HubSteering`'s arm and both go through `crate::operator_ask::settle`.
    pub fn answer_operator_ask(
        &mut self,
        content: &str,
        question: &str,
        answer: &letibot_sessionlog::question::QuestionAnswer,
    ) -> Result<String, String> {
        let line = crate::operator_ask::settle(&self.todos, content, question, answer)?;
        self.flush_todos_or_say("the operator's answer");
        Ok(line)
    }
}

/// How many bytes of a job's output one read returns: enough that a build log's tail is a single read,
/// with the next offset named when it is not. One constant, or the slash reply and the pane's window
/// page each other in circles.
pub const JOB_OUTPUT_WINDOW: usize = 16 * 1024;

// The end-to-end test of the merge queue's enqueue, declared here because it tests THIS module's private
// runner, which a crate can only reach from inside itself.
mod clear;
pub use clear::{ClearReport, clear_reply};

// A transcript whose rows do not rebuild under the prefix it names - the repair to the fork
// that a store already in that state needs, so a conversation is not refused for ever. See the
// module's header for the shape it is, and why only that shape is repaired.
mod children;
// Where each open session's own watcher set and slots are, by id - see the module's header.
pub(crate) mod trees;
// A session's estate - what it was still holding when it ended, what ended with it, and what
// it handed to the session that outlives it. See the module's header for why the substrate's own
// reaping is not the whole answer for a subagent.
mod estate;
pub use estate::{Estate, estate_said, settle_estate};
mod deliveries;
mod error;
mod hub_io;
mod job_window;
mod jobs;
mod models;
mod nags;
mod open;
mod opshell;
mod parts;
mod prefix_repair;
mod queue;
mod room;
mod trail;
mod turns;
mod wall;
#[cfg(test)]
pub(crate) use children::steer_for_turn;
#[cfg(test)]
pub(crate) use children::{
    AnswerOnce, ChildCommand, PlacedChild, SUBAGENT_TITLE_MAX, TaskSlot,
    a_child_that_ends_stops_its_own, child_command, child_leave_said, children_stopped_notice,
    finish_child_thread, inherited_spawn_model, mint_sub_id_at, name_repos, owed_answers,
    release_delivered, repos_under, stoppable_children, subagent_ask_target,
    subagent_depth_refusal, subagent_title, unsettled_children,
};
pub use children::{
    ChildExit, ChildTurns, SubagentModel, derive_title, message_from_a_session, serve_child_under,
    subagent_model, subagent_model_in,
};
pub(crate) use children::{
    HarnessTaskRunner, TaskSlots, a_child_turn_ended, child_mode_source, git_in,
    steer_for_turn_over, stop_children_first, visible_text,
};
pub(crate) use deliveries::{
    Deliveries, OPERATOR_RETIRE, graced_endings, human_secs, monitor_entries_of, monitor_facts,
    monitor_notice, notice_row,
};
pub use deliveries::{JOB_CLOSING, MONITORS_ENDED_SHOWN};
#[cfg(test)]
pub(crate) use deliveries::{
    completion_notice, completion_notices, meanwhile_notice, subagent_notice,
};
pub use error::HarnessError;
pub(crate) use error::said_failure;
pub use hub_io::{HubAdmissions, HubDenials, HubSteering};
pub use job_window::{JobFate, JobWindow};
pub(crate) use job_window::{
    VM_LOST, due_notice, live_job_output, progress_lines, vm_jobs_lost_line,
};
#[cfg(test)]
pub(crate) use job_window::{
    due_rows, fate_line, fate_of, job_window_for, next_job_offset, recorded_job_lines, tail_file,
};
#[cfg(test)]
pub(crate) use models::switch_back_said;
pub use models::{LocalSwitch, local_switch_decision, local_url_authority};
pub(crate) use models::{
    NO_MODEL_CHOSEN, OwnServer, PendingSwitch, SwitchBack, key_is_missing, key_is_replaceable,
    leftover_hint, model_name_of, parse_local_url, superseded, told_once, vocab_differs,
};
pub(crate) use open::{
    STORE_BUSY_PATIENCE, first_sentence, latest_box_stop, load_gguf, provider_notes,
    render_sampling, tool_names, tool_schemas, vocab_basename, wire_turns,
};
pub use open::{
    VerbatimFit, build_provider, engine_for, instructions_of, mode_in_vm, prefix_fingerprint,
    scratch_dir, stale_prefix_sentence, stale_prompt_for_the_model, surroundings_for, verbatim_fit,
    vm_copy_for, worktree_build_env,
};
#[cfg(test)]
pub(crate) use open::{
    base_role_for_seat, confined_grants, downgraded_ruleset, role_for, role_for_seat,
};
pub use parts::Parts;
pub use trail::TrailMirror;
pub(crate) use trail::{ASK_USER_QUESTION, asked_rows, operator_answer_text, question_row_text};
pub use turns::abandoned_calls;
#[cfg(test)]
pub(crate) use turns::reconcile_rows;
pub(crate) use turns::{CapturingSink, MAX_HTTP_RETRIES, Outage, one_line, provider_refusal};
pub use wall::{CompactReport, ForkReport, ForkTail, ReseatReport, reseated_for_the_model};
#[cfg(test)]
pub(crate) use wall::{
    Resident, grown_with_how, roll_resident, schema_change, smaller_tail, wall_stops_round,
};
pub(crate) use wall::{
    UnsealedFork, WallAnchor, compacted_line, reasoning_text, recovered_scale, round_backstop,
    sent_scale, wall_stops_this_round,
};

// The branch is the deliverable - the brief's rule checked when a worktree child's turn ends,
// and the line its parent reads when it ends dirty anyway. See the module's header.
pub mod deliverable;

#[cfg(test)]
mod queue_e2e;

#[cfg(test)]
mod tests;

/// The model half of layer B, built from the config, or a refusal naming the missing piece. One function
/// because two points need it: `--adjudicator model` puts it before the gate alone and `/mode supervised`
/// puts it before a person. `asked_by` names whichever asked, so the refusal names the right flag.
pub(crate) fn model_adjudicator(
    cfg: &Config,
    asked_by: &str,
    hub: Option<Arc<Hub>>,
) -> Result<Box<dyn Adjudicator>, HarnessError> {
    let Some(ep) = cfg.oracle.clone() else {
        return Err(HarnessError::Setup(format!(
            "{asked_by} needs `--oracle HOST:PORT`: ModelAdjudicator takes an \
             AuthorisationOracle and there is nothing to put behind it. This refuses \
             rather than falling back to a person, because a session that asked for \
             layer B and silently got always-ask is the lie `automode`'s Oracle \
             prerequisite exists to prevent."
        )));
    };
    // The guard's own model when the operator named one, else the session's. A
    // guard on another box is a different model, and naming this session's to that
    // server is both a wrong request and a wrong disclosure.
    let guard_model = cfg
        .oracle_model
        .clone()
        .unwrap_or_else(|| cfg.model.clone());
    let mut oracle = crate::oracle::HttpOracle::new(ep, guard_model, cfg.oracle_budget)
        .with_question(cfg.oracle_question);
    // The ceiling, when the operator named one. The default is the measured one;
    // what this makes possible is acting on the one reading that is a budget - a reply cut
    // off before its verdict says so, and `--oracle-max-tokens` is the wheel it names.
    if let Some(n) = cfg.oracle_max_tokens {
        oracle = oracle.with_max_tokens(n);
    }
    if let Some(scope) = &cfg.oracle_scope {
        oracle = oracle.with_scope(scope.clone());
    }
    // Layer A, re-derived per request from the command as the program will receive
    // it. Not copied from the request's own `baseline` string: that is prose for a
    // human, and the adjudicator needs the classification.
    let surroundings = letibot_tools::intent::Surroundings::default();
    Ok(Box::new(
        letibot_tools::ModelAdjudicator::new(
            Box::new(oracle),
            move |req: &letibot_tools::AdjudicationRequest| {
                let cmd = req
                    .arguments
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                letibot_tools::intent::Baseline::of_command_with(cmd, &surroundings, &req.scripts)
            },
        )
        // The wait, announced before it starts: consulting the guard costs seconds on this box, and a turn
        // that pauses with nothing on screen reads as a hang. `ToolProgress` is what a head already renders
        // as the latest note on the running call.
        .with_notice(
            move |req: &letibot_tools::AdjudicationRequest, note: &str| {
                if let Some(h) = &hub {
                    h.publish(SessionEvent::ToolProgress {
                        turn_id: req.turn_id.clone(),
                        call_id: req.call_id.clone(),
                        note: note.to_string(),
                    });
                }
            },
        ),
    ))
}

#[cfg(test)]
mod gatekeeper_reviews {
    //! The seats a review child and a notes keeper run in, and the mark a head hides them by.
    //! What a reviewer's state and its verdict do to the queue's rows is asserted with the
    //! queues themselves (`mergequeue::serve::tests`).
    use super::*;
    use letibot_tools::gatekeeper::ReviewRequest;

    fn req() -> ReviewRequest {
        ReviewRequest {
            brief: "make the reader keep the last record".into(),
            branch: "agent/reader".into(),
            base_sha: "abc123".into(),
            head_sha: "def456".into(),
            cwd: String::new(),
        }
    }

    /// A session above its repositories is told which ones it can name - `task_start`'s
    /// refusal when its root is in no repository.
    #[test]
    fn the_repositories_under_a_root_are_named() {
        let root = std::env::temp_dir().join(format!("letibot-repos-under-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in [
            "app/.git",
            "libs/core/.git",
            "libs/core/nested/.git",
            "notes",
            "target/x/.git",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let found = repos_under(&root, 3);
        let said = name_repos(&root.display().to_string(), &found);
        assert!(said.contains("`/app`") || said.contains("`app`"), "{said}");
        assert!(said.contains("libs/core"), "{said}");
        assert!(
            !said.contains("nested"),
            "a repository is not descended into: {said}"
        );
        assert!(!said.contains("target"), "{said}");
        assert_eq!(name_repos("/x", &[]), "");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_gatekeeper_is_a_seat_and_the_head_can_tell_its_child_apart() {
        assert_eq!(Seat::parse("gatekeeper").unwrap(), Seat::Gatekeeper);
        assert_eq!(Seat::Gatekeeper.as_str(), "gatekeeper");
        assert!(
            !Seat::Gatekeeper.needs_writable_backend(),
            "a reviewer writes nothing"
        );
        // The child's title is cut from the brief's first line, and the head hides a child whose
        // title begins with the mark - so the brief must begin with it, word for word.
        assert!(
            letibot_tools::gatekeeper::review_prompt(&req())
                .starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX),
            "the review brief no longer begins with the head's mark"
        );
    }

    /// The notes keeper is a seat, and the seat is what confines it: `notes` needs a writable backend, and
    /// what keeps it out of the code is having no `write`, no `edit`, no `bash` and no child. A reviewer
    /// reads the references (web search and fetch) and never the room.
    #[test]
    fn the_reviewer_seat_reads_the_web_when_the_session_has_it() {
        let mut cfg = Config::for_this_box("/tmp");
        cfg.web_fetch = true;
        cfg.web_search = Some("brave".into());
        let role = role_for_seat(Seat::Gatekeeper, &cfg);
        for wanted in ["web_fetch", "web_search"] {
            assert!(
                role.tools.iter().any(|t| t == wanted),
                "a reviewer must be seated with `{wanted}`: {:?}",
                role.tools
            );
        }
        assert!(
            !role.tools.iter().any(|t| t == "flowy"),
            "and not with the room: {:?}",
            role.tools
        );
        // Both switched off by hand: `for_this_box` reads the box's own providers file, and a
        // web-search key there would seat the tool on a config this test means to be bare.
        let mut bare_cfg = Config::for_this_box("/tmp");
        bare_cfg.web_fetch = false;
        bare_cfg.web_search = None;
        let bare = role_for_seat(Seat::Gatekeeper, &bare_cfg);
        assert!(
            !bare
                .tools
                .iter()
                .any(|t| t == "web_fetch" || t == "web_search"),
            "without them on the session, nothing is seated: {:?}",
            bare.tools
        );
    }

    #[test]
    fn the_notes_keeper_is_a_seat_that_repairs_memory_and_cannot_touch_code() {
        assert_eq!(Seat::parse("notes_keeper").unwrap(), Seat::NotesKeeper);
        assert_eq!(Seat::parse("keeper").unwrap(), Seat::NotesKeeper);
        assert_eq!(Seat::NotesKeeper.as_str(), "notes_keeper");
        assert!(
            Seat::NotesKeeper.needs_writable_backend(),
            "the `notes` tool writes, and a read-only backend refuses it"
        );
        assert!(
            !Seat::NotesKeeper.needs_exec_backend(),
            "a keeper runs nothing: the commits a note cites were resolved for it"
        );
        let seat = base_role_for_seat(Seat::NotesKeeper, &Config::for_this_box("/tmp"));
        for forbidden in ["write", "edit", "bash", "task", "task_start"] {
            assert!(
                !seat.tools.iter().any(|t| t == forbidden),
                "the notes keeper must not be seated with `{forbidden}`: {:?}",
                seat.tools
            );
        }
        assert!(
            seat.tools.iter().any(|t| t == "notes"),
            "and it must be seated with the one writer that keeps a note a note: {:?}",
            seat.tools
        );
    }

    /// The keeper's brief begins with the mark, on the gatekeeper's rule - so the piece that
    /// spawns one has nothing to change here when it lands.
    #[test]
    fn the_keepers_brief_begins_with_the_mark() {
        use letibot_tools::notes_keeper::{KeeperRequest, NoSupersession, TITLE_PREFIX};
        let root = std::env::temp_dir().join(format!("letibot-keeper-seat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".letibot/notes")).expect("mkdir");
        let note = root.join(".letibot/notes/x.md");
        std::fs::write(&note, "a note about `crates/tools/src/notes_keeper.rs`\n").expect("write");
        let req = KeeperRequest::gather(&root, &note, &NoSupersession).expect("gather");
        let prompt = letibot_tools::notes_keeper::keeper_prompt(&req);
        assert!(
            prompt.starts_with(TITLE_PREFIX),
            "the keeper's brief no longer begins with the mark"
        );
        assert!(
            prompt.contains("notes_keeper.rs"),
            "the evidence rides along"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// What an admission says on the transcript: the one line an operator reads while the work happens.
/// The row the gate writes is the record; this is the channel beside it, and the only thing that can go
/// wrong is a sentence that does not say which command ran or which rule said so.
#[cfg(test)]
mod gate_admissions {
    use super::*;

    fn notice() -> letibot_tools::adjudicate::AdmissionNotice {
        letibot_tools::adjudicate::AdmissionNotice {
            request_id: "adj-s-test-0001".into(),
            turn_id: "t1".into(),
            tool: "bash".into(),
            summary: "`bash` wants exec access to `cargo test --offline`".into(),
            basis: "admitted: inside the measured boundary (D16). …".into(),
            mode: "writes allowed".into(),
            boundary: "bubblewrap 0.11.1".into(),
        }
    }

    /// One line, naming the command and the rule - and no second `bash` in it.
    #[test]
    fn the_line_names_the_command_and_the_rule() {
        use letibot_tools::adjudicate::AdmissionSink;
        let lines: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        HubAdmissions::new(lines.clone()).admitted(&notice());
        HubAdmissions::new(lines.clone()).admitted(&notice());
        let said = lines.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(said.len(), 2, "one line per admission, in order");
        assert_eq!(
            said[0],
            "admitted without asking — `bash` wants exec access to `cargo test --offline` · \
             inside the measured boundary (bubblewrap 0.11.1, writes allowed), the row \
             and `/gate` carry the full reason"
        );
        assert_eq!(said[0].lines().count(), 1, "a notice is one line");
        // A review number is not a name the model can look up, and this line is read by it.
        assert!(!said[0].contains("D16"), "{}", said[0]);
    }
}
